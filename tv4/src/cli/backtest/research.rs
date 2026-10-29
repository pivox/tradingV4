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
//! * dataset de bloc sans `--symbols` ni `--universe` ; `--universe`, `--okx-subset` ou
//!   `--unguarded-protected-read` sur un dataset sans bloc ;
//! * VALIDATION ou HOLDOUT sans `--unguarded-protected-read` : « lecture protégée : garde-fous de S12 non
//!   livrés » (§9.5) ; avec l'option, `research.guard = unguarded` dans chaque rapport et un avertissement
//!   sur stderr ;
//! * VALIDATION ou HOLDOUT sans `--okx-subset` (deux lignes obligatoires, §9.4) ;
//! * `--universe` dont la fenêtre ne donne pas le `window_hash` du manifeste, ou sur un dataset sans
//!   univers (§8.1) ;
//! * `--universe` avec `--instruments-file` ou `--instruments-okx` (clap, §4.7).
//!
//! # Choix isolés (points de §16 non tranchés)
//!
//! * l'identité de recherche (`ResearchIdentity`) est calculée ici et hachée à part du `config_hash`
//!   (point n° 2) ; supprimer ce choix revient à refaire `identity` et `run_id_s11`, pas la CLI ;
//! * la garde provisoire est une seule fonction ([`check_guard`], point n° 13) ;
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
                } else if args.portfolio.okx_subset.is_some() {
                    "--okx-subset"
                } else {
                    "--unguarded-protected-read"
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

/// Garde provisoire avant S12 (§9.5, point n° 13) : un bloc protégé n'est lu qu'avec
/// `--unguarded-protected-read`, tracé dans les rapports et sur stderr.
fn check_guard(block: Block, dataset: &str, portfolio: &PortfolioArgs) -> Result<Guard, CliError> {
    if !block.is_protected() {
        return Ok(Guard::None);
    }
    if !portfolio.unguarded_protected_read {
        return Err(CliError::ProtectedRead {
            block: block.to_string(),
            dataset: dataset.to_string(),
        });
    }
    eprintln!(
        "avertissement : lecture du bloc protégé {block} (dataset `{dataset}`) sans les garde-fous de S12 : \
         research.guard = unguarded dans chaque rapport"
    );
    Ok(Guard::Unguarded)
}

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

/// Ce que le run a lu avant de jouer quoi que ce soit.
struct Plan {
    manifest: DatasetManifest,
    info: ResearchInfo,
    guard: Guard,
    warmup_start_ms: Ts,
    exchange: ExchangeId,
    universe: Option<LoadedUniverse>,
    universe_spec: UniverseSpec,
    window_hash: Option<String>,
    subset: Option<PairSubset>,
    series: Vec<(String, Columns)>,
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

/// Lit tout ce qu'il faut, refuse les usages incohérents, puis charge les séries.
fn prepare(args: &BacktestArgs) -> Result<Plan, CliError> {
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

    // Garde provisoire, puis deux lignes obligatoires : avant toute lecture de série.
    let guard = check_guard(info.block, &manifest.name, portfolio)?;
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
    let loaded = store::load_dataset_multi(&inputs.root, &path)?;
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
        manifest,
        info,
        guard,
        warmup_start_ms,
        exchange,
        universe,
        universe_spec,
        window_hash,
        subset,
        series: loaded.series,
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
    fn dataset_info(&self, legs: &[String]) -> DatasetInfo {
        let series: Vec<_> = self
            .series
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

/// Joue et rend en mémoire une ligne.
fn play_line(
    plan: &Plan,
    args: &BacktestArgs,
    spec_index: usize,
    costs: Option<&CostsPlan>,
) -> Result<Rendered, CliError> {
    let spec = &plan.lines[spec_index];
    let settings = &plan.settings;
    let instruments = plan.instruments(args, &spec.legs)?;
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
    let slippage = if costs.is_some() {
        // Le slippage vient du profil de coûts (S10).
        crate::model::Slippage::ZERO
    } else {
        settings.slippage
    };
    let config = crate::engine::PortfolioConfig {
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
    };
    let research_inputs = plan.research_inputs(spec);
    let t_run = Instant::now();
    let files = match costs {
        None => {
            let result = engine::run_portfolio_research(
                PortfolioInputs {
                    series: &plan.series,
                    strategies: strategies_for(settings, spec.legs.len())?,
                    config,
                },
                &research,
            )?;
            let run_ms = elapsed_ms(t_run);
            let markets = result
                .universe
                .iter()
                .map(|symbol| market_of(&plan.series, symbol))
                .collect();
            let rendered = render_research_run(
                &result,
                MetricsS9Inputs {
                    dataset: plan.dataset_info(&spec.legs),
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
                instruments_file: instruments.file_as_given,
                run_ms,
            });
        }
        Some(costs) => {
            let strategies = [
                strategies_for(settings, spec.legs.len())?,
                strategies_for(settings, spec.legs.len())?,
                strategies_for(settings, spec.legs.len())?,
            ];
            let result = engine::run_portfolio_profiles_research(
                ProfilesInputs {
                    series: &plan.series,
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
                    .map(|symbol| market_of(&plan.series, symbol))
                    .collect();
                rendered.push(render_profile_research(
                    run,
                    MetricsS9Inputs {
                        dataset: plan.dataset_info(&spec.legs),
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
        instruments_file: instruments.file_as_given,
        run_ms: files.1,
    })
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
    let manifest = ManifestS9::new_s11(
        &run.metrics,
        context.base_inputs(run, &rendered.instruments_file, rendered.run_ms),
        context.plan.manifest_research_inputs(line),
    )?;
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
        let manifest = ManifestS9::new_s11_profile(
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
    let plan = prepare(args)?;
    let costs = costs_plan(args)?;
    let load_ms = elapsed_ms(t0);

    let mut rendered: Vec<Rendered> = Vec::with_capacity(plan.lines.len());
    for index in 0..plan.lines.len() {
        rendered.push(play_line(&plan, args, index, costs.as_ref())?);
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

    let context = WriteContext {
        plan: &plan,
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
