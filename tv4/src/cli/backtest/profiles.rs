//! `tv4 backtest --symbols ... --costs ...` : trois profils de coûts de S10 (docs/specs/s10.md §14.1).
//!
//! La CLI charge le dataset une fois, construit trois jeux de stratégies frais, appelle
//! [`engine::run_portfolio_profiles`] (une seule préparation partagée) puis écrit, sous `--out` :
//!
//! ```text
//! DIR/baseline/   metrics.json  trades.csv  equity.csv  manifest.json
//! DIR/adverse/    (idem)
//! DIR/maker/      (idem)
//! DIR/profiles.json
//! ```
//!
//! Chaque sous-dossier est un run complet : fichiers atomiques, manifeste en dernier, avec suppression
//! préalable d'un ancien manifeste (même discipline que S8 et S9). `profiles.json` est écrit tout à la fin,
//! et un `profiles.json` préexistant est supprimé **avant** le premier profil : un dossier interrompu n'en a
//! pas, et `tv4 verify` le refuse. Rien n'est écrit tant que les trois profils ne sont pas rendus en
//! mémoire, donc une erreur de rendu ne laisse aucun fichier à moitié neuf.
//!
//! Règles d'usage ([`CostArgs::check`], toutes refusées avant la lecture du moindre fichier) :
//!
//! * `--costs` exige `--symbols` ([`CliError::CostsWithoutSymbols`]) ;
//! * `--slippage-bps` est refusé avec `--costs` ([`CliError::CostsWithScalarSlippage`]) : le slippage vient
//!   du profil ;
//! * `--cost-measures` et `--cost-seed` exigent `--costs` (clap) ; la graine est dans `[0, 2^53[` (clap).
//!
//! Le code de retour est 0 si les trois runs et `profiles.json` sont écrits, quel que soit le verdict.
//! Aucune logique métier ici : les étiquettes et les portes viennent de `report::profiles`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Args;

use crate::cli::{CliError, now_ms};
use crate::engine::{self, CostSetup, NoopObserver, ProfileRun, ProfilesInputs};
use crate::model::Slippage;
use crate::model::costs::{CostMeasures, CostsFile, MAX_COST_SEED};
use crate::report::manifest::{ManifestS9Inputs, Timings};
use crate::report::metrics_s9::{CostsInputs, MetricsS9Inputs};
use crate::report::profiles::{
    ProfileInput, ProfilesInput, ProfilesReport, RenderedProfile, render_profile,
};
use crate::report::{self, EngineInfo, ManifestS9, ManifestS10Inputs, profiles_id};

use super::portfolio::{PortfolioArgs, PreparedPortfolio, market_of, prepare};
use super::{
    BacktestArgs, MANIFEST_FILE, REPORT_FILE, RunInputArgs, binary_hash, elapsed_ms, remove_stale,
};

/// Graine par défaut des tirages (docs/specs/s10.md §8.4).
const DEFAULT_COST_SEED: u64 = 1;

/// Options des profils de coûts, aplaties dans `BacktestArgs`. Aucune n'a de défaut clap : le défaut est
/// appliqué ici, pour qu'une option donnée sans `--costs` soit refusée.
#[derive(Debug, Default, Args)]
pub struct CostArgs {
    /// Fichier des profils de coûts (`config/costs.toml`). Active le mode s10 : trois runs (baseline,
    /// adverse, maker) et `profiles.json`. Exige --symbols ; refuse --slippage-bps.
    #[arg(long, value_name = "TOML")]
    pub costs: Option<PathBuf>,
    /// Fichier de mesures de carnet (`tv4-cost-measures-v1`) : remplace les défauts paire par paire.
    /// Exige --costs.
    #[arg(long, value_name = "JSON", requires = "costs")]
    pub cost_measures: Option<PathBuf>,
    /// Graine des tirages de coûts, dans [0, 2^53[ (défaut 1). Exige --costs.
    #[arg(
        long,
        value_name = "N",
        requires = "costs",
        value_parser = clap::value_parser!(u64).range(..MAX_COST_SEED)
    )]
    pub cost_seed: Option<u64>,
}

impl CostArgs {
    /// Vrai si `--costs` est donné.
    pub fn is_costs(&self) -> bool {
        self.costs.is_some()
    }

    /// Refuse les combinaisons interdites avec `--costs`, sans rien lire.
    pub fn check(&self, portfolio: &PortfolioArgs, inputs: &RunInputArgs) -> Result<(), CliError> {
        if !self.is_costs() {
            return Ok(());
        }
        if !portfolio.is_portfolio() {
            return Err(CliError::CostsWithoutSymbols);
        }
        if inputs.slippage_bps.is_some() {
            return Err(CliError::CostsWithScalarSlippage);
        }
        Ok(())
    }
}

/// Étiquette d'un fichier dans `metrics.json` : son nom sans dossier, pour que `metrics.json` (comparé à
/// l'octet par `tv4 verify`) ne porte pas de chemin propre à la machine. Le chemin tel que donné va dans le
/// manifeste.
fn label_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Rend un profil joué en fichiers.
fn render(
    run: &ProfileRun,
    prepared: &PreparedPortfolio,
    costs: CostsInputs<'_>,
    args: &BacktestArgs,
) -> Result<RenderedProfile, CliError> {
    let markets = run
        .result
        .universe
        .iter()
        .map(|symbol| market_of(&prepared.series, symbol))
        .collect();
    let inputs = MetricsS9Inputs {
        dataset: prepared.dataset.clone(),
        instruments: prepared.instruments.info.clone(),
        okx_pairs: prepared.instruments.okx_pairs.clone(),
        markets,
    };
    Ok(render_profile(run, inputs, costs, args.equity_step)?)
}

/// Ce qu'il faut pour écrire le manifeste de chaque profil.
struct ManifestContext<'a> {
    prepared: &'a PreparedPortfolio,
    costs_path: &'a Path,
    measures_path: Option<&'a Path>,
    profiles_id: &'a str,
    equity_step: crate::model::Timeframe,
    started_at_ms: crate::model::Ts,
    load_ms: u64,
    run_ms: u64,
}

/// Écrit un profil : ancien manifeste et ancien rapport HTML retirés, trois fichiers, manifeste en dernier.
///
/// `engine` est lu une fois (hash du binaire) à la première écriture, après l'arrêt du chronomètre du
/// premier profil, comme en S8.
fn write_profile(
    out: &Path,
    name: &str,
    rendered: &RenderedProfile,
    context: &ManifestContext<'_>,
    t0: Instant,
    engine: &mut Option<EngineInfo>,
) -> Result<(), CliError> {
    let dir = out.join(name);
    for stale in [MANIFEST_FILE, REPORT_FILE] {
        remove_stale(&dir, stale)?;
    }
    rendered.write_files(&dir)?;
    let cli_cold_ms = elapsed_ms(t0);
    let engine_info = match engine {
        Some(info) => info.clone(),
        None => {
            let info = EngineInfo::current(binary_hash()?);
            *engine = Some(info.clone());
            info
        }
    };
    let manifest = ManifestS9::new_s10(
        &rendered.metrics,
        ManifestS10Inputs {
            base: ManifestS9Inputs {
                instruments_file: context.prepared.instruments.file_as_given.clone(),
                engine: engine_info,
                equity_step: context.equity_step,
                files: rendered.files(),
                started_at_ms: context.started_at_ms,
                timings: Timings {
                    load_ms: context.load_ms,
                    run_ms: context.run_ms,
                    cli_cold_ms,
                },
            },
            costs_file: context.costs_path.display().to_string(),
            cost_measures_file: context.measures_path.map(|p| p.display().to_string()),
            profiles_id: context.profiles_id.to_string(),
        },
    )?;
    report::write_json_atomic(&dir, MANIFEST_FILE, &manifest)?;
    Ok(())
}

/// Valeur optionnelle pour la sortie standard.
fn shown(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.4}"),
        None => "n/a".to_string(),
    }
}

/// `tv4 backtest --symbols ... --costs ...`.
pub(super) fn run(args: &BacktestArgs) -> Result<(), CliError> {
    let t0 = Instant::now();
    let started_at_ms = now_ms()?;
    let Some(costs_path) = args.costs.costs.as_deref() else {
        return Err(CliError::PortfolioOption {
            option: "--costs".to_string(),
            detail: "fichier de coûts absent".to_string(),
        });
    };

    let prepared = prepare(&args.inputs, &args.portfolio)?;
    let costs_file = CostsFile::load(costs_path)?;
    let measures = match args.costs.cost_measures.as_deref() {
        Some(path) => Some(CostMeasures::load(path)?),
        None => None,
    };
    let seed = args.costs.cost_seed.unwrap_or(DEFAULT_COST_SEED);
    // Trois jeux frais : les stratégies ont un état, rien ne passe d'un profil à l'autre.
    let strategies = [
        prepared.build_strategies()?,
        prepared.build_strategies()?,
        prepared.build_strategies()?,
    ];
    let load_ms = elapsed_ms(t0);

    let t_run = Instant::now();
    let result = engine::run_portfolio_profiles(
        ProfilesInputs {
            series: &prepared.series,
            strategies,
            // Le slippage scalaire est nul : le slippage vient du profil de coûts.
            config: prepared.portfolio_config(Slippage::ZERO),
            costs: CostSetup {
                file: &costs_file,
                measures: measures.as_ref(),
                seed,
            },
        },
        &mut NoopObserver,
    )?;
    let run_ms = elapsed_ms(t_run);

    let costs_label = label_of(costs_path);
    let measures_label = args.costs.cost_measures.as_deref().map(label_of);
    let costs_inputs = CostsInputs {
        file: &costs_file,
        file_label: &costs_label,
        measures: measures.as_ref(),
        measures_label: measures_label.as_deref(),
    };
    let [run_baseline, run_adverse, run_maker] = &result.runs;
    let baseline = render(run_baseline, &prepared, costs_inputs, args)?;
    let adverse = render(run_adverse, &prepared, costs_inputs, args)?;
    let maker = render(run_maker, &prepared, costs_inputs, args)?;

    let id = profiles_id(
        &baseline.metrics.run_id(),
        &adverse.metrics.run_id(),
        &maker.metrics.run_id(),
    );
    let (hash_baseline, hash_adverse, hash_maker) = (
        baseline.result_hash(),
        adverse.result_hash(),
        maker.result_hash(),
    );
    let profiles = ProfilesReport::build(&ProfilesInput {
        baseline: ProfileInput {
            metrics: &baseline.metrics,
            result_hash: &hash_baseline,
        },
        adverse: ProfileInput {
            metrics: &adverse.metrics,
            result_hash: &hash_adverse,
        },
        maker: ProfileInput {
            metrics: &maker.metrics,
            result_hash: &hash_maker,
        },
        min_maker_entry_share: costs_file.profiles.maker.min_maker_entry_share,
    })?;
    debug_assert_eq!(profiles.profiles_id, id);

    let out = args
        .out
        .clone()
        .unwrap_or_else(|| args.inputs.root.join("runs").join(&id));

    // `profiles.json` d'une exécution précédente est retiré avant le premier profil : si l'écriture d'un
    // profil échoue, le dossier n'a pas de `profiles.json`, et `tv4 verify` le refuse. Le rapport HTML
    // d'un rendu précédent est périmé lui aussi.
    for stale in [report::profiles::PROFILES_FILE, REPORT_FILE] {
        remove_stale(&out, stale)?;
    }
    let context = ManifestContext {
        prepared: &prepared,
        costs_path,
        measures_path: args.costs.cost_measures.as_deref(),
        profiles_id: &id,
        equity_step: args.equity_step,
        started_at_ms,
        load_ms,
        run_ms,
    };
    let mut engine_info: Option<EngineInfo> = None;
    for (run, rendered) in [
        (run_baseline, &baseline),
        (run_adverse, &adverse),
        (run_maker, &maker),
    ] {
        write_profile(
            &out,
            run.profile.as_str(),
            rendered,
            &context,
            t0,
            &mut engine_info,
        )?;
    }
    profiles.write(&out)?;

    println!(
        "profils {id} écrits dans {} : {} paires",
        out.display(),
        run_baseline.result.universe.len()
    );
    for summary in &profiles.profiles {
        println!(
            "profil {} : config_hash {}, verdict {}, {} trades, expectancy nette {}",
            summary.name.as_str(),
            summary.config_hash,
            summary.verdict,
            summary.n_trades,
            shown(summary.expectancy_net)
        );
    }
    println!(
        "maker : {} trades, part des entrées remplies en maker {}",
        profiles.maker.n_trades,
        shown(profiles.maker.maker_entry_maker_share)
    );
    println!(
        "étiquette : {} ({})",
        profiles.label.fr(),
        profiles.label.as_str()
    );
    Ok(())
}
