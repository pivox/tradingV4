//! `tv4 backtest` sur un dataset de recherche (S11, docs/specs/s11.md §8, §9 et §11) : un run par ligne
//! sur un bloc, avec `--symbols` (univers fixe) ou `--universe` (univers trimestriel).
//!
//! Un dataset est de recherche si son manifeste porte `block` ([`store::research_meta`]). Il ne se joue
//! qu'en mode portefeuille ; ce module n'est appelé que dans ce cas ([`dispatch`]).
//!
//! # Disposition de la sortie
//!
//! ```text
//! sans --okx-subset      OUT/{metrics.json, trades.csv, equity.csv, manifest.json}
//!                        (ou avec --costs : OUT/{baseline,adverse,maker}/ puis OUT/profiles.json)
//! avec --okx-subset      OUT/full/       (un run, ou un dossier de profils avec --costs)
//!                        OUT/okx_usdc/   (idem)
//!                        OUT/lines.json  (écrit en dernier, après les deux lignes)
//! ```
//!
//! Les deux lignes sont **deux runs** sur le même dataset (§9.1) : `full` joue toutes les jambes,
//! `okx_usdc` seulement celles dont la base est dans le sous-ensemble figé. Elles ont le même
//! `config_hash` et deux `run_id` (identité de recherche, §8.6). Rien n'est écrit tant que les deux lignes
//! ne sont pas jouées et rendues en mémoire : un échec de la seconde ne laisse ni `lines.json` ni fichier
//! à moitié neuf, et un `lines.json` préexistant est retiré avant la première écriture.
//!
//! # Refus d'usage (code 2), tous avant la lecture des séries
//!
//! * dataset de bloc sans `--symbols` ni `--universe` ; `--universe` ou `--okx-subset`
//!   sur un dataset sans bloc ;
//! * fiche absente ou non commitée, gel incohérent, claims ou quotas manquants (admission S12) ;
//! * VALIDATION ou HOLDOUT sans `--okx-subset` (deux lignes obligatoires, §9.4) ;
//! * `--universe` dont la fenêtre ne donne pas le `window_hash` du manifeste, ou sur un dataset sans
//!   univers (§8.1) ;
//! * `--universe` avec `--instruments-file` ou `--instruments-okx` (clap, §4.7).
//!
//! # Choix isolés (points de §16 non tranchés)
//!
//! * l'identité de recherche (`ResearchIdentity`) est calculée ici et hachée à part du `config_hash`
//!   (point n° 2) ; supprimer ce choix revient à refaire `identity` et `run_id_s11`, pas la CLI ;
//! * l’admission S12 est commune aux autres chemins CLI ([`Session`]) ;
//! * le sous-ensemble obligatoire pour VALIDATION et HOLDOUT est une seule fonction
//!   ([`check_subset_required`], point n° 4).
//!
//! # Mémoire
//!
//! Les variantes `*_owned` du moteur, qui consomment les colonnes, ne sont pas utilisées : le calcul de
//! G0 relit les séries après le run (médianes de volume et de close, écrites dans `metrics.json`), et les
//! deux lignes jouent le même dataset. Le dataset reste donc en mémoire avec la copie du moteur (§12.2,
//! à mesurer sur le Mac).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cli::research::{admission::Session, digest};
use crate::cli::{CliError, now_ms};
use crate::engine::{
    self, CostSetup, Line, NoopObserver, PortfolioInputs, ProfilesInputs, ResearchIdentity,
    ResearchSpec, UniverseSpec,
};
use crate::model::blocks::BLOCKS;
use crate::model::costs::{CostMeasures, CostsFile};
use crate::model::universe::PairSubset;
use crate::model::{Block, ExchangeId, InstrumentRules, Timeframe, Ts, UniverseSchedule};
use crate::report::manifest::{ManifestS9Inputs, Timings};
use crate::report::metrics_s9::{CostsInputs, DatasetInfo, InstrumentsInfo, MetricsS9Inputs};
use crate::report::profiles::{
    ProfileInput, ProfilesInput, ProfilesReport, RenderedProfile, render_profile_research,
};
use crate::report::research::{
    COST_PROFILES, Guard, LineInput, LineRun, LinesReport, ResearchInputs, SINGLE_PROFILE,
    SubsetInfo, render_research_run,
};
use crate::report::{self, EngineInfo, ManifestS9, ManifestS10Inputs, profiles_id};
use crate::research::registry::{Metrics as TrialMetrics, TrialResult};
use crate::sim::SimRules;
use crate::store::time::parse_rfc3339_utc;
use crate::store::universe::{LoadedUniverse, read_pair_subset};
use crate::store::{self, Columns, DatasetManifest, ResearchInfo};
use crate::strategy::{Strategy, registry};

use super::portfolio::{
    Instruments, OKX_QUOTE_USDC, PortfolioArgs, load_params, market_of, numeric_options,
    okx_instruments, option_error, toml_instruments,
};
use super::{BacktestArgs, MANIFEST_FILE, REPORT_FILE, binary_hash, elapsed_ms, remove_stale};

/// Graine par défaut des tirages de coûts (comme `profiles.rs`).
const DEFAULT_COST_SEED: u64 = 1;

fn usage(option: &str, detail: impl Into<String>) -> CliError {
    CliError::Usage {
        option: option.to_string(),
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------------------------------
// Aiguillage
// ---------------------------------------------------------------------------------------------

/// Blocs protégés que la fenêtre `[start_ms, end_ms)` recoupe (avertissement de `backtest` sur un
/// dataset sans bloc, docs/specs/s11.md §3.4).
pub(super) fn protected_blocks_overlapping(start_ms: Ts, end_ms: Ts) -> Vec<&'static str> {
    BLOCKS
        .iter()
        .filter(|(block, span)| {
            block.is_protected()
                && span.start_ms < end_ms
                && span.end_ms.is_none_or(|end| end > start_ms)
        })
        .map(|(block, _)| block.as_str())
        .collect()
}

/// Dit si le run est un run de recherche, et refuse les usages incohérents avec le dataset.
///
/// Rend `true` si le manifeste de `--dataset` porte `block` et que le mode portefeuille est demandé (le
/// run est alors joué par [`run`]). Rend `false` pour tout autre cas, que les chemins de S8 à S10
/// traitent sans changement : un manifeste illisible, ou un dataset ordinaire sans option de recherche.
/// Un dataset ordinaire dont la fenêtre recoupe VALIDATION ou HOLDOUT ne produit qu'un avertissement sur
/// stderr (§3.4, §9.5).
///
/// # Erreurs
///
/// Code 2 : dataset de bloc sans `--symbols` ni `--universe`, option de recherche sur un dataset sans
/// bloc. Un manifeste illisible n'est une erreur que si une option de recherche est donnée.
pub(super) fn dispatch(args: &BacktestArgs) -> Result<bool, CliError> {
    let wants_research = args.portfolio.has_research_option();
    let Some(dataset) = args.inputs.dataset.as_deref() else {
        return Ok(false);
    };
    let read = super::resolve_dataset_path(&args.inputs.root, dataset)
        .and_then(|path| Ok(store::read_dataset_manifest(&path)?));
    let manifest = match read {
        Ok(manifest) => manifest,
        // Le chemin ordinaire redonne la même erreur, avec ses propres refus d'usage devant elle.
        Err(_) if !wants_research => return Ok(false),
        Err(error) => return Err(error),
    };
    match manifest.block {
        Some(block) => {
            if !args.portfolio.is_portfolio() {
                return Err(usage(
                    "--dataset",
                    format!(
                        "le dataset de recherche `{}` (bloc {block}) ne se joue qu'en mode portefeuille : \
                         donner --symbols ou --universe",
                        manifest.name
                    ),
                ));
            }
            Ok(true)
        }
        None => {
            if wants_research {
                let option = if args.portfolio.universe.is_some() {
                    "--universe"
                } else {
                    "--okx-subset"
                };
                return Err(usage(
                    option,
                    format!(
                        "le dataset `{}` n'est pas un dataset de recherche (pas de bloc) : \
                         le construire avec `tv4 dataset build --block`",
                        manifest.name
                    ),
                ));
            }
            warn_if_protected_window(&manifest);
            Ok(false)
        }
    }
}

/// Avertissement de stderr quand un dataset sans bloc recoupe VALIDATION ou HOLDOUT : la garde de S11 ne
/// couvre que les datasets marqués (§9.5). stderr n'est ni haché ni comparé.
fn warn_if_protected_window(manifest: &DatasetManifest) {
    let (Ok(start), Ok(end)) = (
        parse_rfc3339_utc(&manifest.window[0]),
        parse_rfc3339_utc(&manifest.window[1]),
    ) else {
        return;
    };
    let blocks = protected_blocks_overlapping(start, end);
    if !blocks.is_empty() {
        eprintln!(
            "avertissement : la fenêtre du dataset `{}` [{}, {}) recoupe {} sans passer par un dataset \
             de bloc : aucune garde ne s'applique (docs/specs/s11.md §9.5)",
            manifest.name,
            manifest.window[0],
            manifest.window[1],
            blocks.join(" et ")
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Refus propres à la recherche
// ---------------------------------------------------------------------------------------------

/// Deux lignes obligatoires pour VALIDATION et HOLDOUT (§9.4, point n° 4) : `--okx-subset` exigé.
fn check_subset_required(block: Block, portfolio: &PortfolioArgs) -> Result<(), CliError> {
    if block.is_protected() && portfolio.okx_subset.is_none() {
        return Err(usage(
            "--okx-subset",
            format!("obligatoire pour {block} : le rapport donne les lignes full et okx_usdc"),
        ));
    }
    Ok(())
}

/// Le `--universe` donné est celui qui a choisi les symboles du dataset (§8.1) : mêmes règles, mêmes
/// trimestres lus sur la fenêtre de décision.
fn check_universe_matches(
    dir: &Path,
    loaded: &LoadedUniverse,
    manifest: &DatasetManifest,
    info: &ResearchInfo,
) -> Result<String, CliError> {
    let mismatch = |detail: String| CliError::UniverseMismatch {
        path: dir.display().to_string(),
        dataset: manifest.name.clone(),
        detail,
    };
    let Some(recorded) = &info.universe else {
        return Err(mismatch(
            "le dataset a été construit sans univers (symboles fixes)".to_string(),
        ));
    };
    let window_hash = loaded
        .window_hash(info.decision_start_ms, info.decision_end_ms)
        .map_err(|error| mismatch(error.to_string()))?;
    if loaded.rules_hash() != recorded.rules_hash {
        return Err(mismatch(format!(
            "rules_hash {} contre {} dans le manifeste du dataset",
            loaded.rules_hash(),
            recorded.rules_hash
        )));
    }
    if window_hash != recorded.window_hash {
        return Err(mismatch(format!(
            "window_hash {window_hash} contre {} dans le manifeste du dataset",
            recorded.window_hash
        )));
    }
    Ok(window_hash)
}

/// Base d'un symbole canonique : `BTC` pour `BTC-USD`.
fn base_of(symbol: &str) -> &str {
    symbol.split_once('-').map_or(symbol, |(base, _)| base)
}

// ---------------------------------------------------------------------------------------------
// Préparation
// ---------------------------------------------------------------------------------------------

/// Réglages communs aux lignes (tout ce que `PortfolioConfig` prend hors jambes et règles).
struct Settings {
    fees: crate::model::FeeSchedule,
    slippage: crate::model::Slippage,
    capital: f64,
    min_trades: u32,
    regime_symbol: String,
    max_positions: u16,
    sizing: crate::sim::SizingMode,
    min_order_bump: Option<f64>,
    quote_gap_ms: Ts,
    strategy_id: String,
    params: toml::Value,
}

/// Une ligne à jouer.
struct LineSpec {
    line: Line,
    /// Jambes jouées, dans l'ordre de l'univers.
    legs: Vec<String>,
    /// Jambes que le sous-ensemble retire (ligne `okx_usdc`).
    excluded: Vec<String>,
}

/// Métadonnées et paramètres résolus avant toute lecture de bougies.
struct Plan {
    dataset_path: PathBuf,
    manifest: DatasetManifest,
    info: ResearchInfo,
    guard: Guard,
    effective_block: Block,
    warmup_start_ms: Ts,
    exchange: ExchangeId,
    universe: Option<LoadedUniverse>,
    universe_spec: UniverseSpec,
    window_hash: Option<String>,
    subset: Option<PairSubset>,
    settings: Settings,
    lines: Vec<LineSpec>,
    /// Chemin de `--universe` tel que donné.
    universe_as_given: Option<String>,
    /// Chemin de `--okx-subset` tel que donné.
    subset_as_given: Option<String>,
}

/// Jambes d'un run : membres de l'union des trimestres de la fenêtre de décision (`--universe`), ou
/// symboles de `--symbols`.
fn legs_of(
    portfolio: &PortfolioArgs,
    universe: Option<&LoadedUniverse>,
    info: &ResearchInfo,
) -> Result<Vec<String>, CliError> {
    match universe {
        Some(loaded) => Ok(loaded.union_symbols(info.decision_start_ms, info.decision_end_ms)),
        None => {
            let legs: Vec<String> = portfolio
                .symbols
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|symbol| symbol.trim().to_string())
                .collect();
            if legs.is_empty() || legs.iter().any(String::is_empty) {
                return Err(option_error(
                    "--symbols",
                    "au moins un symbole, sans élément vide",
                ));
            }
            Ok(legs)
        }
    }
}

/// Résout les entrées et refuse les usages incohérents sans charger de série.
fn prepare_metadata(args: &BacktestArgs) -> Result<Plan, CliError> {
    let portfolio = &args.portfolio;
    let inputs = &args.inputs;
    if inputs.fixture.is_some() {
        return Err(CliError::PortfolioNeedsDataset);
    }
    let Some(dataset_arg) = inputs.dataset.as_deref() else {
        return Err(CliError::DataSource);
    };
    let numeric = numeric_options(inputs, portfolio)?;

    let shown = dataset_arg.display().to_string();
    let path = super::resolve_dataset_path(&inputs.root, dataset_arg)?;
    let manifest = store::read_dataset_manifest(&path)?;
    let Some(info) = store::research_meta(&manifest)? else {
        return Err(usage(
            "--dataset",
            format!(
                "le dataset `{}` n'est pas un dataset de recherche",
                manifest.name
            ),
        ));
    };
    if manifest.tfs.as_slice() != [Timeframe::M1] {
        return Err(CliError::DatasetTimeframe {
            path: shown,
            tf: manifest.tfs.first().copied().unwrap_or(Timeframe::M1),
        });
    }

    // Les deux lignes sont exigées avant les autres métadonnées ; Session vérifie le bloc effectif.
    let guard = Guard::None;
    check_subset_required(info.block, portfolio)?;

    let (universe, window_hash, universe_spec) = match portfolio.universe.as_deref() {
        Some(dir) => {
            let loaded = store::universe::load(dir)?;
            let window_hash = check_universe_matches(dir, &loaded, &manifest, &info)?;
            let spec = UniverseSpec::Quarterly {
                rules_hash: loaded.rules_hash().to_string(),
            };
            (Some(loaded), Some(window_hash), spec)
        }
        None => {
            let legs = legs_of(portfolio, None, &info)?;
            (None, None, UniverseSpec::Fixed { symbols: legs })
        }
    };
    let legs = legs_of(portfolio, universe.as_ref(), &info)?;
    if legs.is_empty() {
        return Err(usage(
            "--universe",
            "aucune jambe : l'univers n'a aucun membre sur la fenêtre de décision",
        ));
    }
    if let Some(missing) = legs
        .iter()
        .find(|leg| !manifest.symbols.iter().any(|s| s.as_str() == leg.as_str()))
    {
        return Err(CliError::LegNotInDataset {
            symbol: missing.clone(),
            dataset: manifest.name.clone(),
        });
    }

    let subset = match portfolio.okx_subset.as_deref() {
        Some(file) => Some(read_pair_subset(file)?),
        None => None,
    };
    let mut lines = vec![LineSpec {
        line: Line::Full,
        legs: legs.clone(),
        excluded: Vec::new(),
    }];
    if let (Some(subset), Some(file)) = (&subset, portfolio.okx_subset.as_deref()) {
        let (kept, excluded): (Vec<String>, Vec<String>) = legs
            .iter()
            .cloned()
            .partition(|leg| subset.contains_base(base_of(leg)));
        if kept.is_empty() {
            return Err(CliError::LinesSubsetEmpty {
                path: file.display().to_string(),
            });
        }
        lines.push(LineSpec {
            line: Line::OkxUsdc,
            legs: kept,
            excluded,
        });
    }

    let fees = crate::model::FeeSchedule::load_grid(inputs.fees, &inputs.fees_file)?;
    let params = load_params(inputs)?;
    let warmup_start_ms = parse_rfc3339_utc(&manifest.window[0])?;
    let exchange = manifest.exchange;
    let settings = Settings {
        fees,
        slippage: inputs.legacy_slippage()?,
        capital: inputs.capital,
        min_trades: inputs.min_trades,
        regime_symbol: portfolio.regime_symbol_or_default(),
        max_positions: portfolio.max_positions_or_default(),
        sizing: numeric.sizing,
        min_order_bump: numeric.min_order_bump,
        quote_gap_ms: numeric.quote_gap_ms,
        strategy_id: inputs.strategy.clone(),
        params,
    };
    Ok(Plan {
        dataset_path: path,
        manifest,
        effective_block: info.block,
        info,
        guard,
        warmup_start_ms,
        exchange,
        universe,
        universe_spec,
        window_hash,
        subset,
        settings,
        lines,
        universe_as_given: portfolio.universe.as_ref().map(|p| p.display().to_string()),
        subset_as_given: portfolio
            .okx_subset
            .as_ref()
            .map(|p| p.display().to_string()),
    })
}

impl Plan {
    /// Règles d'instrument des jambes d'une ligne : l'`instruments_kraken.toml` de l'univers avec
    /// `--universe` (mêmes règles pour les jambes communes aux deux lignes, §4.7), sinon
    /// `--instruments-okx` ou `--instruments-file` comme en S9.
    fn instruments(&self, args: &BacktestArgs, legs: &[String]) -> Result<Instruments, CliError> {
        if let Some(universe) = &self.universe {
            let mut rules = Vec::with_capacity(legs.len());
            for leg in legs {
                rules.push(InstrumentRules::from_toml_str(
                    self.exchange,
                    leg,
                    universe.instruments_text(),
                )?);
            }
            return Ok(Instruments {
                rules,
                info: InstrumentsInfo::toml(),
                okx_pairs: None,
                hash: None,
                file_as_given: None,
            });
        }
        match args.portfolio.instruments_okx.as_deref() {
            Some(file) => okx_instruments(
                file,
                legs,
                args.portfolio
                    .okx_quote
                    .as_deref()
                    .unwrap_or(OKX_QUOTE_USDC),
            ),
            None => toml_instruments(&args.inputs, self.exchange, legs),
        }
    }

    /// Identité de recherche d'une ligne ; `legs_hash` est recalculé par le moteur (écart n° 2 de
    /// `engine::research`).
    fn identity(&self, line: Line) -> Result<ResearchIdentity, CliError> {
        Ok(ResearchIdentity {
            block: self.info.block,
            decision_start_ms: self.info.decision_start_ms,
            decision_end_ms: self.info.decision_end_ms,
            universe_window_hash: self.window_hash.clone(),
            quality_hash: self.info.quality.hash()?,
            line,
            subset_hash: match line {
                Line::Full => None,
                Line::OkxUsdc => self.subset.as_ref().map(|s| s.subset_hash.clone()),
            },
            legs_hash: String::new(),
        })
    }

    fn schedule(&self) -> Option<&UniverseSchedule> {
        self.universe.as_ref().map(LoadedUniverse::schedule)
    }

    /// Informations de dataset d'une ligne : les jambes jouées et la paire de régime.
    fn dataset_info(&self, legs: &[String], columns: &[(String, Columns)]) -> DatasetInfo {
        let series: Vec<_> = columns
            .iter()
            .filter(|(symbol, _)| *symbol == self.settings.regime_symbol || legs.contains(symbol))
            .map(|(symbol, columns)| crate::report::metrics_s9::SeriesInfo {
                symbol: symbol.clone(),
                rows: columns.len() as u64,
                first_ms: columns.open_time_ms().first().copied().unwrap_or(0),
                last_ms: columns.open_time_ms().last().copied().unwrap_or(0),
            })
            .collect();
        DatasetInfo {
            name: self.manifest.name.clone(),
            exchange: self.exchange,
            tf: Timeframe::M1,
            gaps_policy: self.manifest.gaps_policy.as_str().to_string(),
            hash_format: self.manifest.hash_format.clone(),
            dataset_hash: self.manifest.dataset_hash.clone(),
            series,
        }
    }

    /// Entrées de rapport d'une ligne.
    fn research_inputs<'a>(&'a self, spec: &'a LineSpec) -> ResearchInputs<'a> {
        ResearchInputs {
            guard: self.guard,
            warmup_start_ms: self.warmup_start_ms,
            universe: self.schedule(),
            quality: &self.info.quality,
            subset: match (spec.line, &self.subset) {
                (Line::OkxUsdc, Some(subset)) => Some(SubsetInfo {
                    subset,
                    legs_excluded: &spec.excluded,
                }),
                _ => None,
            },
        }
    }

    /// Chemins d'entrée de recherche recopiés dans le manifeste (jamais dans `metrics.json`).
    fn manifest_research_inputs(&self, line: Line) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        if let Some(universe) = &self.universe_as_given {
            map.insert("universe".to_string(), universe.clone());
        }
        if line == Line::OkxUsdc
            && let Some(subset) = &self.subset_as_given
        {
            map.insert("okx_subset".to_string(), subset.clone());
        }
        map
    }
}

// ---------------------------------------------------------------------------------------------
// Jeu d'une ligne
// ---------------------------------------------------------------------------------------------

/// Le rendu d'une ligne : un run, ou les trois profils d'un dossier de profils.
struct Rendered {
    spec_index: usize,
    /// Un rendu pour un run seul, trois (baseline, adverse, maker) avec `--costs`.
    files: Vec<RenderedProfile>,
    instruments_file: Option<String>,
    run_ms: u64,
}

fn strategies_for(settings: &Settings, legs: usize) -> Result<Vec<Box<dyn Strategy>>, CliError> {
    let mut strategies: Vec<Box<dyn Strategy>> = Vec::with_capacity(legs);
    for _ in 0..legs {
        strategies.push(registry::build(&settings.strategy_id, &settings.params)?);
    }
    Ok(strategies)
}

fn config_for_line<'a>(
    plan: &'a Plan,
    spec: &'a LineSpec,
    instruments: &'a Instruments,
    with_costs: bool,
) -> crate::engine::PortfolioConfig<'a> {
    let settings = &plan.settings;
    let slippage = if with_costs {
        // Le slippage vient du profil de coûts (S10).
        crate::model::Slippage::ZERO
    } else {
        settings.slippage
    };
    crate::engine::PortfolioConfig {
        universe: &spec.legs,
        regime_symbol: &settings.regime_symbol,
        fees: settings.fees,
        slippage,
        initial_cash: settings.capital,
        sim_rules: SimRules::default(),
        instruments: &instruments.rules,
        instruments_hash: instruments.hash.as_deref(),
        max_positions: settings.max_positions,
        sizing: settings.sizing,
        min_order_bump_max_risk: settings.min_order_bump,
        quote_gap_ms: settings.quote_gap_ms,
        min_trades: settings.min_trades,
    }
}

/// Les trois profils sont identifiés avant accès au store, par les constructeurs du moteur.
fn preflight_line(
    plan: &Plan,
    index: usize,
    instruments: &Instruments,
    costs: Option<&CostsPlan>,
) -> Result<Vec<String>, CliError> {
    let spec = &plan.lines[index];
    let strategies = strategies_for(&plan.settings, spec.legs.len())?;
    let config = config_for_line(plan, spec, instruments, costs.is_some());
    let profiles = match costs {
        Some(costs) => crate::model::costs::CostProfileName::ALL
            .into_iter()
            .map(|profile| {
                engine::preflight::portfolio(
                    &strategies,
                    &config,
                    Some((
                        CostSetup {
                            file: &costs.file,
                            measures: costs.measures.as_ref(),
                            seed: costs.seed,
                        },
                        profile,
                    )),
                    Some(&plan.universe_spec),
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![engine::preflight::portfolio(
            &strategies,
            &config,
            None,
            Some(&plan.universe_spec),
        )?],
    };
    profiles
        .iter()
        .map(|profile| profile.hash().map_err(CliError::from))
        .collect()
}

/// Joue et rend en mémoire une ligne.
fn play_line(
    plan: &Plan,
    series: &[(String, Columns)],
    args: &BacktestArgs,
    spec_index: usize,
    costs: Option<&CostsPlan>,
    instruments: &Instruments,
) -> Result<Rendered, CliError> {
    let spec = &plan.lines[spec_index];
    let settings = &plan.settings;
    let identity = plan.identity(spec.line)?;
    let research = ResearchSpec {
        block: plan.info.block,
        decision_start_ms: plan.info.decision_start_ms,
        decision_end_ms: plan.info.decision_end_ms,
        universe: plan.schedule(),
        universe_spec: &plan.universe_spec,
        quality: &plan.info.quality,
        identity: &identity,
    };
    let config = config_for_line(plan, spec, instruments, costs.is_some());
    let research_inputs = plan.research_inputs(spec);
    let t_run = Instant::now();
    let files = match costs {
        None => {
            let mut result = engine::run_portfolio_research(
                PortfolioInputs {
                    series,
                    strategies: strategies_for(settings, spec.legs.len())?,
                    config,
                },
                &research,
            )?;
            relabel(&mut result, plan.effective_block)?;
            let run_ms = elapsed_ms(t_run);
            let markets = result
                .universe
                .iter()
                .map(|symbol| market_of(series, symbol))
                .collect();
            let rendered = render_research_run(
                &result,
                MetricsS9Inputs {
                    dataset: plan.dataset_info(&spec.legs, series),
                    instruments: instruments.info.clone(),
                    okx_pairs: instruments.okx_pairs.clone(),
                    markets,
                },
                &research_inputs,
                args.equity_step,
            )?;
            return Ok(Rendered {
                spec_index,
                files: vec![rendered],
                instruments_file: instruments.file_as_given.clone(),
                run_ms,
            });
        }
        Some(costs) => {
            let strategies = [
                strategies_for(settings, spec.legs.len())?,
                strategies_for(settings, spec.legs.len())?,
                strategies_for(settings, spec.legs.len())?,
            ];
            let mut result = engine::run_portfolio_profiles_research(
                ProfilesInputs {
                    series,
                    strategies,
                    config,
                    costs: CostSetup {
                        file: &costs.file,
                        measures: costs.measures.as_ref(),
                        seed: costs.seed,
                    },
                },
                &research,
                &mut NoopObserver,
            )?;
            for run in &mut result.runs {
                relabel(&mut run.result, plan.effective_block)?;
            }
            let run_ms = elapsed_ms(t_run);
            let inputs = CostsInputs {
                file: &costs.file,
                file_label: &costs.file_label,
                measures: costs.measures.as_ref(),
                measures_label: costs.measures_label.as_deref(),
            };
            let mut rendered = Vec::with_capacity(3);
            for run in &result.runs {
                let markets = run
                    .result
                    .universe
                    .iter()
                    .map(|symbol| market_of(series, symbol))
                    .collect();
                rendered.push(render_profile_research(
                    run,
                    MetricsS9Inputs {
                        dataset: plan.dataset_info(&spec.legs, series),
                        instruments: instruments.info.clone(),
                        okx_pairs: instruments.okx_pairs.clone(),
                        markets,
                    },
                    inputs,
                    &research_inputs,
                    args.equity_step,
                )?);
            }
            (rendered, run_ms)
        }
    };
    Ok(Rendered {
        spec_index,
        files: files.0,
        instruments_file: instruments.file_as_given.clone(),
        run_ms: files.1,
    })
}

/// La simulation valide la fenêtre contre le bloc source ; seul le rapport expose le bloc effectif.
fn relabel(result: &mut engine::PortfolioResult, block: Block) -> Result<(), CliError> {
    if let Some(research) = &mut result.research {
        research.block = block;
        research.identity.block = block;
        research.identity_hash = research.identity.hash()?;
    }
    Ok(())
}

/// Fichiers de coûts de `--costs` (mode profils).
struct CostsPlan {
    file: CostsFile,
    file_label: String,
    path: PathBuf,
    measures: Option<CostMeasures>,
    measures_label: Option<String>,
    measures_path: Option<PathBuf>,
    seed: u64,
}

/// Nom de fichier d'un chemin, pour que `metrics.json` ne porte pas de chemin propre à la machine.
fn label_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn costs_plan(args: &BacktestArgs) -> Result<Option<CostsPlan>, CliError> {
    let Some(path) = args.costs.costs.as_deref() else {
        return Ok(None);
    };
    let file = CostsFile::load(path)?;
    let measures = match args.costs.cost_measures.as_deref() {
        Some(measures_path) => Some(CostMeasures::load(measures_path)?),
        None => None,
    };
    Ok(Some(CostsPlan {
        file,
        file_label: label_of(path),
        path: path.to_path_buf(),
        measures,
        measures_label: args.costs.cost_measures.as_deref().map(label_of),
        measures_path: args.costs.cost_measures.clone(),
        seed: args.costs.cost_seed.unwrap_or(DEFAULT_COST_SEED),
    }))
}

// ---------------------------------------------------------------------------------------------
// Écriture
// ---------------------------------------------------------------------------------------------

/// Ce que l'écriture d'un dossier de ligne (ou du dossier de sortie d'un run seul) partage.
struct WriteContext<'a> {
    plan: &'a Plan,
    session: &'a Session,
    args: &'a BacktestArgs,
    costs: Option<&'a CostsPlan>,
    engine: EngineInfo,
    started_at_ms: Ts,
    load_ms: u64,
    t0: Instant,
}

impl WriteContext<'_> {
    fn base_inputs(
        &self,
        rendered: &RenderedProfile,
        instruments_file: &Option<String>,
        run_ms: u64,
    ) -> ManifestS9Inputs {
        ManifestS9Inputs {
            instruments_file: instruments_file.clone(),
            engine: self.engine.clone(),
            equity_step: self.args.equity_step,
            files: rendered.files(),
            started_at_ms: self.started_at_ms,
            timings: Timings {
                load_ms: self.load_ms,
                run_ms,
                cli_cold_ms: elapsed_ms(self.t0),
            },
        }
    }
}

/// Écrit un run seul dans `dir` : anciens manifeste, rapport HTML, `profiles.json` et `lines.json`
/// retirés, trois fichiers, manifeste en dernier.
fn write_single(
    dir: &Path,
    rendered: &Rendered,
    line: Line,
    context: &WriteContext<'_>,
) -> Result<String, CliError> {
    let Some(run) = rendered.files.first() else {
        return Err(usage("--out", "aucun run à écrire (bug)"));
    };
    for stale in [
        MANIFEST_FILE,
        REPORT_FILE,
        report::profiles::PROFILES_FILE,
        report::research::LINES_FILE,
    ] {
        remove_stale(dir, stale)?;
    }
    run.write_files(dir)?;
    let mut manifest = ManifestS9::new_s11(
        &run.metrics,
        context.base_inputs(run, &rendered.instruments_file, rendered.run_ms),
        context.plan.manifest_research_inputs(line),
    )?;
    manifest.governance = Some(context.session.context(&run.metrics.config_hash)?);
    report::write_json_atomic(dir, MANIFEST_FILE, &manifest)?;
    Ok(run.metrics.run_id())
}

/// Écrit un dossier de profils s11 dans `dir` : trois sous-dossiers avec manifeste, puis `profiles.json`.
/// Rend le `profiles_id`.
fn write_profiles(
    dir: &Path,
    rendered: &Rendered,
    line: Line,
    context: &WriteContext<'_>,
) -> Result<String, CliError> {
    let Some(costs) = context.costs else {
        return Err(usage("--costs", "fichier de coûts absent (bug)"));
    };
    let [baseline, adverse, maker] = rendered.files.as_slice() else {
        return Err(usage("--costs", "trois profils attendus (bug)"));
    };
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
        min_maker_entry_share: costs.file.profiles.maker.min_maker_entry_share,
    })?;
    debug_assert_eq!(profiles.profiles_id, id);

    // `profiles.json` (et `lines.json`) d'une exécution précédente sont retirés avant le premier profil.
    for stale in [
        report::profiles::PROFILES_FILE,
        REPORT_FILE,
        report::research::LINES_FILE,
    ] {
        remove_stale(dir, stale)?;
    }
    for (profile, name) in [baseline, adverse, maker].into_iter().zip(COST_PROFILES) {
        let sub = dir.join(name);
        for stale in [MANIFEST_FILE, REPORT_FILE] {
            remove_stale(&sub, stale)?;
        }
        profile.write_files(&sub)?;
        let mut manifest = ManifestS9::new_s11_profile(
            &profile.metrics,
            ManifestS10Inputs {
                base: context.base_inputs(profile, &rendered.instruments_file, rendered.run_ms),
                costs_file: costs.path.display().to_string(),
                cost_measures_file: costs
                    .measures_path
                    .as_ref()
                    .map(|p| p.display().to_string()),
                profiles_id: id.clone(),
            },
            context.plan.manifest_research_inputs(line),
        )?;
        manifest.governance = Some(context.session.context(&profile.metrics.config_hash)?);
        report::write_json_atomic(&sub, MANIFEST_FILE, &manifest)?;
    }
    profiles.write(dir)?;
    Ok(id)
}

/// Écrit une ligne dans `dir` : un run seul, ou un dossier de profils.
fn write_line(
    dir: &Path,
    rendered: &Rendered,
    line: Line,
    context: &WriteContext<'_>,
) -> Result<String, CliError> {
    if context.costs.is_some() {
        write_profiles(dir, rendered, line, context)
    } else {
        write_single(dir, rendered, line, context)
    }
}

/// La ligne pour `lines.json` : un run, ou les trois profils.
fn line_input(line: Line, rendered: &Rendered) -> LineInput<'_> {
    let runs = if rendered.files.len() == 1 {
        rendered
            .files
            .iter()
            .map(|r| LineRun {
                profile: SINGLE_PROFILE,
                metrics: &r.metrics,
            })
            .collect()
    } else {
        rendered
            .files
            .iter()
            .zip(COST_PROFILES)
            .map(|(r, profile)| LineRun {
                profile,
                metrics: &r.metrics,
            })
            .collect()
    };
    LineInput { line, runs }
}

// ---------------------------------------------------------------------------------------------
// tv4 backtest (recherche)
// ---------------------------------------------------------------------------------------------

/// `tv4 backtest` sur un dataset de bloc : une ligne, ou deux avec `--okx-subset`.
pub(super) fn run(args: &BacktestArgs) -> Result<(), CliError> {
    let t0 = Instant::now();
    let started_at_ms = now_ms()?;
    let mut plan = prepare_metadata(args)?;
    let costs = costs_plan(args)?;
    let mut instruments = plan
        .lines
        .iter()
        .map(|spec| plan.instruments(args, &spec.legs))
        .collect::<Result<Vec<_>, _>>()?;
    let expected_hashes = instruments
        .iter()
        .enumerate()
        .map(|(index, instruments)| preflight_line(&plan, index, instruments, costs.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    if expected_hashes.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(usage(
            "--okx-subset",
            "les lignes n'ont pas les mêmes configurations canoniques",
        ));
    }
    let dataset_hash = plan.manifest.dataset_hash.clone();
    let source_block = plan.info.block;
    let source_window = (plan.info.decision_start_ms, plan.info.decision_end_ms);
    let subset_hash = plan.subset.as_ref().map(|s| s.subset_hash.clone());
    let mut session = Session::begin_with_prepare(
        &args.inputs.research,
        &expected_hashes[0],
        &dataset_hash,
        Some(source_block),
        Some(source_window),
        subset_hash.as_deref(),
        |effective_block, decision_window| {
            plan.effective_block = effective_block.unwrap_or(source_block);
            check_subset_required(plan.effective_block, &args.portfolio)?;
            if let Some((start, end)) = decision_window
                && (start, end) != (plan.info.decision_start_ms, plan.info.decision_end_ms)
            {
                plan.info.decision_start_ms = start;
                plan.info.decision_end_ms = end;
                if let Some(universe) = &plan.universe {
                    let previous_legs = plan
                        .lines
                        .iter()
                        .map(|line| line.legs.clone())
                        .collect::<Vec<_>>();
                    plan.window_hash = Some(universe.window_hash(start, end)?);
                    let legs = universe.union_symbols(start, end);
                    if legs.is_empty() {
                        return Err(usage("--universe", "aucune jambe sur la fenêtre admise"));
                    }
                    plan.lines[0].legs = legs.clone();
                    if let Some(subset) = &plan.subset {
                        let (kept, excluded) = legs
                            .into_iter()
                            .partition(|s| subset.contains_base(base_of(s)));
                        plan.lines[1].legs = kept;
                        plan.lines[1].excluded = excluded;
                        if plan.lines[1].legs.is_empty() {
                            return Err(usage(
                                "--okx-subset",
                                "sous-ensemble vide sur la fenêtre admise",
                            ));
                        }
                    }
                    // Conserver les règles déjà résolues avant admission ; la fenêtre réduit seulement
                    // les unions. Aucun fichier d'instruments mutable n'est relu.
                    for (index, spec) in plan.lines.iter().enumerate() {
                        instruments[index].rules = spec
                            .legs
                            .iter()
                            .map(|leg| {
                                let at = previous_legs[index]
                                    .iter()
                                    .position(|old| old == leg)
                                    .ok_or_else(|| {
                                        usage(
                                            "--universe",
                                            "jambe admise absente de l’union source",
                                        )
                                    })?;
                                Ok(instruments[index].rules[at])
                            })
                            .collect::<Result<Vec<_>, CliError>>()?;
                    }
                }
            }
            for (index, instruments) in instruments.iter().enumerate() {
                if preflight_line(&plan, index, instruments, costs.as_ref())?
                    != expected_hashes[index]
                {
                    return Err(usage(
                        "--universe",
                        "configuration canonique différente après admission de la fenêtre",
                    ));
                }
            }
            Ok(())
        },
    )?;
    debug_assert_eq!(
        session.decision_window(),
        Some((plan.info.decision_start_ms, plan.info.decision_end_ms))
    );
    plan.guard = if session.effective_block().is_some_and(Block::is_protected) {
        Guard::Guarded
    } else {
        Guard::None
    };
    let series = store::load_dataset_multi_manifest(
        &args.inputs.root,
        &plan.dataset_path,
        plan.manifest.clone(),
    )?
    .series;
    let load_ms = elapsed_ms(t0);

    let mut rendered: Vec<Rendered> = Vec::with_capacity(plan.lines.len());
    for index in 0..plan.lines.len() {
        let line = play_line(
            &plan,
            &series,
            args,
            index,
            costs.as_ref(),
            &instruments[index],
        )?;
        let actual: Vec<_> = line
            .files
            .iter()
            .map(|run| run.metrics.config_hash.clone())
            .collect();
        if actual != expected_hashes[index] {
            return Err(usage(
                "--dataset",
                "configuration exécutée différente de la configuration admise",
            ));
        }
        rendered.push(line);
    }

    // Identifiant du dossier de sortie par défaut, et lines.json avant toute écriture.
    let two_lines = rendered.len() == 2;
    let lines_report = match (rendered.as_slice(), &plan.subset) {
        ([full, okx], Some(subset)) => Some(LinesReport::build(
            &line_input(Line::Full, full),
            &line_input(Line::OkxUsdc, okx),
            subset,
        )?),
        _ => None,
    };
    let default_id = match (&lines_report, rendered.first()) {
        (Some(lines), _) => lines.lines_id.clone(),
        (None, Some(single)) if costs.is_some() => {
            let [b, a, m] = single.files.as_slice() else {
                return Err(usage("--costs", "trois profils attendus (bug)"));
            };
            profiles_id(
                &b.metrics.run_id(),
                &a.metrics.run_id(),
                &m.metrics.run_id(),
            )
        }
        (None, Some(single)) => single
            .files
            .first()
            .map(|r| r.metrics.run_id())
            .unwrap_or_default(),
        (None, None) => String::new(),
    };
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| args.inputs.root.join("runs").join(&default_id));

    let results = rendered[0]
        .files
        .iter()
        .map(|r| {
            Ok(TrialResult {
                config_hash: digest(&r.metrics.config_hash)?,
                failed: false,
                metrics: TrialMetrics {
                    mu_r: r.metrics.expectancy_r,
                    n: Some(r.metrics.n_trades),
                    pf: r.metrics.profit_factor,
                },
            })
        })
        .collect::<Result<Vec<_>, crate::cli::research::ResearchError>>()?;
    session.finish(&results)?;
    session.print_context();
    let context = WriteContext {
        plan: &plan,
        session: &session,
        args,
        costs: costs.as_ref(),
        engine: EngineInfo::current(binary_hash()?),
        started_at_ms,
        load_ms,
        t0,
    };
    if two_lines {
        // `lines.json` d'une exécution précédente est retiré avant la première ligne : un dossier
        // interrompu n'en a pas, et `tv4 verify` le refuse.
        for stale in [report::research::LINES_FILE, REPORT_FILE] {
            remove_stale(&out, stale)?;
        }
    }
    for item in &rendered {
        let Some(spec) = plan.lines.get(item.spec_index) else {
            continue;
        };
        let dir = if two_lines {
            out.join(spec.line.as_str())
        } else {
            out.clone()
        };
        let id = write_line(&dir, item, spec.line, &context)?;
        print_line(&dir, spec, item, &id, costs.is_some());
    }
    if let Some(lines) = &lines_report {
        lines.write(&out)?;
        println!(
            "lignes {} écrites dans {} ({} et {})",
            lines.lines_id,
            out.display(),
            Line::Full.as_str(),
            Line::OkxUsdc.as_str()
        );
        print_residual(lines);
    }
    Ok(())
}

fn print_line(dir: &Path, spec: &LineSpec, rendered: &Rendered, id: &str, profiles: bool) {
    let what = if profiles { "profils" } else { "run" };
    let metrics = rendered.files.first().map(|r| &r.metrics);
    let summary = metrics.map_or_else(String::new, |m| {
        let purged = m.research.as_ref().map_or(0, |r| r.purge.trades_purged);
        format!(
            "{} trades conservés, {purged} purgés, PnL net {:.2}, verdict {}",
            m.n_trades, m.pnl_net, m.verdict
        )
    });
    println!(
        "ligne {} : {what} {id} écrit dans {} : {} jambes ({}) ; {summary}",
        spec.line.as_str(),
        dir.display(),
        spec.legs.len(),
        if spec.excluded.is_empty() {
            "toutes".to_string()
        } else {
            format!("{} retirées par le sous-ensemble", spec.excluded.len())
        }
    );
}

fn print_residual(lines: &LinesReport) {
    for (profile, residual) in &lines.residual_survivorship {
        let shown =
            |value: Option<f64>| value.map_or_else(|| "n/a".to_string(), |v| format!("{v:.4}"));
        println!(
            "survivance résiduelle ({profile}) : expectancy nette {}, rendement {}, écart de trades {}",
            shown(residual.expectancy_net),
            shown(residual.return_total),
            residual.n_trades
        );
    }
}

#[cfg(test)]
mod preflight_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn les_options_de_recherche_sont_partagees_par_backtest_et_bench() {
        for command in ["backtest", "bench"] {
            let cli = crate::cli::Cli::try_parse_from([
                "tv4",
                command,
                "--fixture",
                "exemple.csv",
                "--strategy",
                "sma_cross",
                "--params",
                "exemple.toml",
                "--hypothesis",
                "H-001",
                "--research-root",
                "/tmp/recherche",
            ])
            .expect("options de recherche communes");
            let research = match &cli.command {
                crate::cli::Command::Backtest(args) => &args.inputs.research,
                crate::cli::Command::Bench(args) => &args.inputs.research,
                _ => panic!("commande attendue"),
            };
            assert_eq!(research.hypothesis.as_deref(), Some("H-001"));
            assert_eq!(
                research.research_root.as_deref(),
                Some(Path::new("/tmp/recherche"))
            );
        }
    }

    #[test]
    fn la_preparation_des_metadonnees_n_ouvre_pas_les_partitions() {
        let root = tempfile::tempdir().unwrap();
        let dataset = root.path().join("admis.json");
        let hash = format!("blake3:{}", "0".repeat(64));
        let manifest = serde_json::json!({
            "name": "admis", "exchange": "kraken", "symbols": ["BTC-USD"],
            "tfs": ["1m"], "window": ["2026-07-01T00:00:00Z", "2026-07-02T00:00:00Z"],
            "gaps_policy": "zero_volume_marked", "row_count": 1440,
            "dataset_hash": hash, "hash_format": "dsh-v1",
            "built_at": "2026-11-01T00:00:00Z", "tv4_version": "0.1.0",
            "block": "FORWARD", "decision_window": ["2026-07-01T00:00:00Z", "2026-07-02T00:00:00Z"],
            "quality_exclusions": {},
            "files": [{"path": "data/candles/kraken/BTC-USD/1m/absent.parquet",
                "symbol": "BTC-USD", "tf": "1m", "period": "2026-07",
                "content_hash": hash, "row_count": 1440, "snapshot": false}]
        });
        std::fs::write(&dataset, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let args = vec![
            "tv4".to_string(),
            "backtest".into(),
            "--root".into(),
            root.path().display().to_string(),
            "--dataset".into(),
            dataset.display().to_string(),
            "--symbols".into(),
            "BTC-USD".into(),
            "--strategy".into(),
            "sma_cross".into(),
            "--params".into(),
            repo.join("config/strategies/sma_cross.toml")
                .display()
                .to_string(),
            "--fees-file".into(),
            repo.join("config/fees.toml").display().to_string(),
        ];
        let crate::cli::Command::Backtest(args) =
            crate::cli::Cli::try_parse_from(args).unwrap().command
        else {
            panic!("backtest");
        };
        let plan =
            prepare_metadata(&args).expect("le fichier Parquet absent ne doit pas etre ouvert");
        assert_eq!(plan.info.block, Block::Forward);
        assert_eq!(plan.lines[0].legs, ["BTC-USD"]);
    }
}
