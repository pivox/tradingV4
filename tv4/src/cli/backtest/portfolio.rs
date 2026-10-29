//! `tv4 backtest --symbols ...` : mode portefeuille de S9 (docs S9 §15.1).
//!
//! Même enveloppe que le chemin de S8 (quatre fichiers écrits de façon atomique, le manifeste en
//! dernier), mais le moteur est [`engine::run_portfolio`] et les schémas sont `tv4-metrics-s9` et
//! `tv4-manifest-s9`. Aucune logique métier ici : lecture du dataset multi-séries et des règles
//! d'instrument, appel du registre (une stratégie fraîche par paire), du moteur et de `report`.
//!
//! Règles d'usage :
//!
//! * `--symbols` exige `--dataset` (pas `--fixture`) : la fixture est mono-paire ;
//! * `--max-positions`, `--sizing`, `--instruments-okx`, `--okx-quote`, `--regime-symbol`,
//!   `--quote-gap-days` et `--min-order-bump-risk` sans `--symbols` sont refusés, pour qu'un run de S8
//!   ne croie pas les appliquer ;
//! * `--min-order-bump-risk` n'est accepté qu'avec `--sizing compounding` ;
//! * la paire OKX d'une jambe `BASE-QUOTE` est `BASE-<okx-quote>` (`BTC-USD` donne `BTC-USDC`, et
//!   `BTC-USDC` reste `BTC-USDC`) ;
//! * `--capital` est lu au démarrage (défaut 300), jamais pris d'une constante.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Args;

use crate::cli::{CliError, now_ms};
use crate::engine::{
    self, DEFAULT_MAX_POSITIONS, DEFAULT_QUOTE_GAP_MS, MAX_POSITIONS_LIMIT, PortfolioConfig,
    PortfolioInputs,
};
use crate::marketdata::okx::instruments::{InstrumentsError, InstrumentsFile};
use crate::model::{ExchangeId, FeeSchedule, InstrumentRules, Slippage, Timeframe, Ts, hash};
use crate::report::executability::{LegMarket, OkxPair};
use crate::report::manifest::{ManifestS9Inputs, OutputFiles, Timings};
use crate::report::metrics_s9::{
    DatasetInfo, InstrumentsInfo, MetricsS9Inputs, SeriesInfo, UpcomingChangeInfo,
};
use crate::report::{self, ManifestS9, MetricsS9};
use crate::sim::{SimRules, SizingMode};
use crate::store::{self, Columns};
use crate::strategy::{Strategy, registry};

use super::{
    BacktestArgs, MANIFEST_FILE, REPORT_FILE, RunInputArgs, binary_hash, elapsed_ms, remove_stale,
    resolve_dataset_path,
};

/// Paire de régime par défaut (docs S9 §6.3).
const DEFAULT_REGIME_SYMBOL: &str = "BTC-USD";
/// Monnaie de cotation OKX des jambes (seule valeur de S9).
pub(super) const OKX_QUOTE_USDC: &str = "USDC";
/// Millisecondes dans un jour.
const DAY_MS: Ts = 86_400_000;

/// Options du mode portefeuille, aplaties dans `BacktestArgs`. Aucune n'a de défaut clap : le défaut
/// est appliqué ici, ce qui permet de refuser une option donnée sans `--symbols`.
#[derive(Debug, Default, Args)]
pub struct PortfolioArgs {
    /// Univers du portefeuille, dans l'ordre donné (départage des rangs égaux) : `BTC-USD,ETH-USD`.
    /// Active le mode portefeuille ; exige --dataset.
    #[arg(long, value_name = "A,B,...", value_delimiter = ',')]
    pub symbols: Option<Vec<String>>,
    /// Positions ouvertes plus entrées en attente au plus (1 à 64, défaut 8).
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u16).range(1..=i64::from(MAX_POSITIONS_LIMIT))
    )]
    pub max_positions: Option<u16>,
    /// Sizing : `research` (base = capital initial, plafond capital / N, défaut) ou `compounding`
    /// (base = equity du portefeuille).
    #[arg(long, value_name = "MODE", value_parser = ["research", "compounding"])]
    pub sizing: Option<String>,
    /// Fichier d'instruments OKX (`tv4 instruments fetch`) : minimums du compte. Sans lui, les règles
    /// viennent de --instruments-file et G0 et G1 sont `not_applicable`.
    #[arg(long, value_name = "JSON")]
    pub instruments_okx: Option<PathBuf>,
    /// Monnaie de cotation OKX des jambes (USDC seulement).
    #[arg(long, value_name = "QUOTE", value_parser = [OKX_QUOTE_USDC])]
    pub okx_quote: Option<String>,
    /// Paire de régime BTC 1d / SMA200 (défaut BTC-USD), lue seulement si la stratégie lit le régime.
    #[arg(long, value_name = "SYMBOLE")]
    pub regime_symbol: Option<String>,
    /// Jours sans minute active au-delà desquels une jambe en position sort de force (défaut 7).
    #[arg(
        long,
        value_name = "D",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub quote_gap_days: Option<u32>,
    /// Palier 20 % : porte une entrée sous le minimum d'ordre à ce minimum si son risque effectif reste
    /// sous cette part de l'equity (par exemple 0.015). Réservé à `--sizing compounding`.
    #[arg(long, value_name = "P", allow_negative_numbers = true)]
    pub min_order_bump_risk: Option<f64>,
    /// Dossier d'univers trimestriel (`tv4 universe build`, S11) : les jambes sont les membres des
    /// trimestres de la fenêtre de décision d'un dataset de bloc, et les règles d'instrument viennent de
    /// son `instruments_kraken.toml`. Active le mode portefeuille ; exclusif avec --symbols,
    /// --instruments-file et --instruments-okx.
    #[arg(
        long,
        value_name = "DOSSIER",
        conflicts_with_all = ["symbols", "instruments_file", "instruments_okx"]
    )]
    pub universe: Option<PathBuf>,
    /// Sous-ensemble figé de paires (`tv4 universe okx-subset`, S11) : ajoute la ligne `okx_usdc` au run
    /// de recherche (`full/`, `okx_usdc/`, `lines.json`). Obligatoire pour VALIDATION et HOLDOUT.
    #[arg(long, value_name = "JSON")]
    pub okx_subset: Option<PathBuf>,
    /// Lit un bloc protégé (VALIDATION, HOLDOUT) malgré l'absence des garde-fous de S12 ; tracé dans
    /// chaque rapport (`research.guard = unguarded`) et sur stderr (docs/specs/s11.md §9.5).
    #[arg(long, hide = true)]
    pub unguarded_protected_read: bool,
}

impl PortfolioArgs {
    /// Vrai si `--symbols` ou `--universe` est donné.
    pub fn is_portfolio(&self) -> bool {
        self.symbols.is_some() || self.universe.is_some()
    }

    /// Vrai si une option propre à la recherche de S11 est donnée (elle exige un dataset de bloc).
    pub fn has_research_option(&self) -> bool {
        self.universe.is_some() || self.okx_subset.is_some() || self.unguarded_protected_read
    }

    /// Paire de régime : `--regime-symbol`, sinon [`DEFAULT_REGIME_SYMBOL`].
    pub fn regime_symbol_or_default(&self) -> String {
        self.regime_symbol
            .clone()
            .unwrap_or_else(|| DEFAULT_REGIME_SYMBOL.to_string())
    }

    /// Plafond de positions : `--max-positions`, sinon [`DEFAULT_MAX_POSITIONS`].
    pub fn max_positions_or_default(&self) -> u16 {
        self.max_positions.unwrap_or(DEFAULT_MAX_POSITIONS)
    }

    /// Refuse toute option du portefeuille donnée sans `--symbols`.
    pub fn reject_without_symbols(&self) -> Result<(), CliError> {
        let given = [
            ("--max-positions", self.max_positions.is_some()),
            ("--sizing", self.sizing.is_some()),
            ("--instruments-okx", self.instruments_okx.is_some()),
            ("--okx-quote", self.okx_quote.is_some()),
            ("--regime-symbol", self.regime_symbol.is_some()),
            ("--quote-gap-days", self.quote_gap_days.is_some()),
            ("--min-order-bump-risk", self.min_order_bump_risk.is_some()),
            ("--okx-subset", self.okx_subset.is_some()),
            ("--unguarded-protected-read", self.unguarded_protected_read),
        ];
        match given.iter().find(|(_, present)| *present) {
            Some((option, _)) => Err(CliError::PortfolioOptionWithoutSymbols {
                option: (*option).to_string(),
            }),
            None => Ok(()),
        }
    }
}

pub(super) fn option_error(option: &str, detail: impl Into<String>) -> CliError {
    CliError::PortfolioOption {
        option: option.to_string(),
        detail: detail.into(),
    }
}

/// Instrument OKX d'une jambe : `BASE-QUOTE` devient `BASE-<quote OKX>`.
pub(super) fn okx_inst_id(symbol: &str, quote: &str) -> String {
    match symbol.rsplit_once('-') {
        Some((base, _)) => format!("{base}-{quote}"),
        None => symbol.to_string(),
    }
}

/// Règles d'instrument des jambes et ce que le rapport en dit.
pub(super) struct Instruments {
    pub rules: Vec<InstrumentRules>,
    pub info: InstrumentsInfo,
    /// Une entrée par jambe si les règles viennent d'un fichier OKX.
    pub okx_pairs: Option<Vec<OkxPair>>,
    pub hash: Option<String>,
    /// Chemin du fichier tel que donné.
    pub file_as_given: Option<String>,
}

/// Run préparé : dataset chargé et vérifié, règles, frais, paramètres.
pub(super) struct PreparedPortfolio {
    pub dataset: DatasetInfo,
    pub series: Vec<(String, Columns)>,
    pub universe: Vec<String>,
    pub fees: FeeSchedule,
    /// Slippage scalaire du chemin hérité ; le mode `--costs` ne l'utilise pas (il est refusé).
    pub slippage: Slippage,
    pub capital: f64,
    pub min_trades: u32,
    pub instruments: Instruments,
    pub regime_symbol: String,
    pub max_positions: u16,
    pub sizing: SizingMode,
    pub min_order_bump: Option<f64>,
    pub quote_gap_ms: Ts,
    pub strategy_id: String,
    pub params: toml::Value,
}

impl PreparedPortfolio {
    /// Configuration du moteur, avec le slippage scalaire donné (celui du chemin hérité, ou
    /// [`Slippage::ZERO`] pour les profils de coûts de S10).
    pub fn portfolio_config(&self, slippage: Slippage) -> PortfolioConfig<'_> {
        PortfolioConfig {
            universe: &self.universe,
            regime_symbol: &self.regime_symbol,
            fees: self.fees,
            slippage,
            initial_cash: self.capital,
            sim_rules: SimRules::default(),
            instruments: &self.instruments.rules,
            instruments_hash: self.instruments.hash.as_deref(),
            max_positions: self.max_positions,
            sizing: self.sizing,
            min_order_bump_max_risk: self.min_order_bump,
            quote_gap_ms: self.quote_gap_ms,
            min_trades: self.min_trades,
        }
    }

    /// Une stratégie fraîche par jambe, dans l'ordre de l'univers.
    pub fn build_strategies(&self) -> Result<Vec<Box<dyn Strategy>>, CliError> {
        let mut strategies: Vec<Box<dyn Strategy>> = Vec::with_capacity(self.universe.len());
        for _ in &self.universe {
            strategies.push(registry::build(&self.strategy_id, &self.params)?);
        }
        Ok(strategies)
    }
}

/// Lit le fichier d'instruments OKX et les règles de chaque jambe.
pub(super) fn okx_instruments(
    path: &Path,
    universe: &[String],
    quote: &str,
) -> Result<Instruments, CliError> {
    let file = InstrumentsFile::load(path).map_err(|error| match error {
        InstrumentsError::HashMismatch { .. } => CliError::InstrumentsHashMismatch {
            path: path.display().to_string(),
        },
        other => CliError::Instruments(other),
    })?;
    let mut rules = Vec::with_capacity(universe.len());
    let mut pairs = Vec::with_capacity(universe.len());
    for symbol in universe {
        let inst_id = okx_inst_id(symbol, quote);
        let leg_rules = file.to_rules(&inst_id).map_err(|error| match error {
            InstrumentsError::NotListed { inst_id } => CliError::InstrumentNotListed { inst_id },
            other => CliError::Instruments(other),
        })?;
        let state = file
            .get(&inst_id)
            .map(|instrument| instrument.state.clone())
            .unwrap_or_default();
        pairs.push(OkxPair {
            inst_id,
            min_sz: leg_rules.min_qty,
            state,
        });
        rules.push(leg_rules);
    }
    let changes: Vec<UpcomingChangeInfo> = file
        .upcoming_changes()
        .into_iter()
        .filter(|change| pairs.iter().any(|pair| pair.inst_id == change.inst_id))
        .map(|change| UpcomingChangeInfo {
            inst_id: change.inst_id,
            param: change.param,
            new_value: change.new_value,
            eff_time: change.eff_time,
        })
        .collect();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let info = InstrumentsInfo::okx(
        name,
        file.instruments_hash.clone(),
        file.fetched_at.clone(),
        &pairs,
        changes,
    );
    Ok(Instruments {
        rules,
        info,
        okx_pairs: Some(pairs),
        hash: Some(file.instruments_hash),
        file_as_given: Some(path.display().to_string()),
    })
}

/// Règles de S6 (`config/instruments.toml`), par exchange et symbole du dataset.
pub(super) fn toml_instruments(
    args: &RunInputArgs,
    exchange: ExchangeId,
    universe: &[String],
) -> Result<Instruments, CliError> {
    let mut rules = Vec::with_capacity(universe.len());
    for symbol in universe {
        rules.push(InstrumentRules::load(
            exchange,
            symbol,
            &args.instruments_file,
        )?);
    }
    Ok(Instruments {
        rules,
        info: InstrumentsInfo::toml(),
        okx_pairs: None,
        hash: None,
        file_as_given: None,
    })
}

/// Segments du dataset pour le rapport : symbole, lignes et bornes.
pub(super) fn series_info(series: &[(String, Columns)]) -> Vec<SeriesInfo> {
    series
        .iter()
        .map(|(symbol, columns)| SeriesInfo {
            symbol: symbol.clone(),
            rows: columns.len() as u64,
            first_ms: columns.open_time_ms().first().copied().unwrap_or(0),
            last_ms: columns.open_time_ms().last().copied().unwrap_or(0),
        })
        .collect()
}

/// Options numériques du mode portefeuille, validées avant toute lecture de dataset.
pub(super) struct Numeric {
    pub sizing: SizingMode,
    pub min_order_bump: Option<f64>,
    pub quote_gap_ms: Ts,
}

/// Valide `--capital`, `--sizing`, `--min-order-bump-risk` et `--quote-gap-days`. Partagé avec le mode
/// recherche de S11 (`research.rs`) : mêmes refus, même ordre.
pub(super) fn numeric_options(
    args: &RunInputArgs,
    portfolio: &PortfolioArgs,
) -> Result<Numeric, CliError> {
    if !(args.capital.is_finite() && args.capital > 0.0) {
        return Err(option_error(
            "--capital",
            format!(
                "{} : un nombre strictement positif est attendu",
                args.capital
            ),
        ));
    }
    let sizing = match portfolio.sizing.as_deref() {
        None | Some("research") => SizingMode::Research,
        Some(_) => SizingMode::Compounding,
    };
    let min_order_bump = match portfolio.min_order_bump_risk {
        None => None,
        Some(risk) => {
            if sizing != SizingMode::Compounding {
                return Err(option_error(
                    "--min-order-bump-risk",
                    "réservé à --sizing compounding",
                ));
            }
            if !(risk.is_finite() && risk > 0.0 && risk <= 1.0) {
                return Err(option_error(
                    "--min-order-bump-risk",
                    format!("{risk} : un risque dans ]0, 1] est attendu"),
                ));
            }
            Some(risk)
        }
    };
    let quote_gap_ms = match portfolio.quote_gap_days {
        None => DEFAULT_QUOTE_GAP_MS,
        Some(days) => Ts::from(days).checked_mul(DAY_MS).ok_or_else(|| {
            option_error("--quote-gap-days", format!("{days} jours : trop grand"))
        })?,
    };
    Ok(Numeric {
        sizing,
        min_order_bump,
        quote_gap_ms,
    })
}

/// Lit le fichier de paramètres de la stratégie et en fait une construction d'essai.
pub(super) fn load_params(args: &RunInputArgs) -> Result<toml::Value, CliError> {
    let params_text = std::fs::read_to_string(&args.params).map_err(|source| CliError::Read {
        path: args.params.display().to_string(),
        source,
    })?;
    let params =
        toml::from_str::<toml::Value>(&params_text).map_err(|source| CliError::ParamsToml {
            path: args.params.display().to_string(),
            source,
        })?;
    // Construction d'essai : une stratégie inconnue ou des paramètres invalides échouent avant tout calcul.
    registry::build(&args.strategy, &params)?;
    Ok(params)
}

pub(super) fn prepare(
    args: &RunInputArgs,
    portfolio: &PortfolioArgs,
) -> Result<PreparedPortfolio, CliError> {
    if args.fixture.is_some() {
        return Err(CliError::PortfolioNeedsDataset);
    }
    let Some(dataset_arg) = args.dataset.as_deref() else {
        return Err(CliError::DataSource);
    };
    let universe: Vec<String> = portfolio
        .symbols
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|symbol| symbol.trim().to_string())
        .collect();
    if universe.is_empty() || universe.iter().any(String::is_empty) {
        return Err(option_error(
            "--symbols",
            "au moins un symbole, sans élément vide",
        ));
    }
    let numeric = numeric_options(args, portfolio)?;

    let shown = dataset_arg.display().to_string();
    let path = resolve_dataset_path(&args.root, dataset_arg)?;
    let loaded = store::load_dataset_multi(&args.root, &path)?;
    if loaded.manifest.tfs.as_slice() != [Timeframe::M1] {
        return Err(CliError::DatasetTimeframe {
            path: shown,
            tf: loaded
                .manifest
                .tfs
                .first()
                .copied()
                .unwrap_or(Timeframe::M1),
        });
    }
    let exchange = loaded.manifest.exchange;

    let instruments = match portfolio.instruments_okx.as_deref() {
        Some(file) => okx_instruments(
            file,
            &universe,
            portfolio.okx_quote.as_deref().unwrap_or(OKX_QUOTE_USDC),
        )?,
        None => toml_instruments(args, exchange, &universe)?,
    };
    let fees = FeeSchedule::load_grid(args.fees, &args.fees_file)?;

    let params = load_params(args)?;

    let dataset = DatasetInfo {
        name: loaded.manifest.name.clone(),
        exchange,
        tf: Timeframe::M1,
        gaps_policy: loaded.manifest.gaps_policy.as_str().to_string(),
        hash_format: loaded.manifest.hash_format.clone(),
        dataset_hash: loaded.manifest.dataset_hash.clone(),
        series: series_info(&loaded.series),
    };
    Ok(PreparedPortfolio {
        dataset,
        series: loaded.series,
        universe,
        fees,
        slippage: args.legacy_slippage()?,
        capital: args.capital,
        min_trades: args.min_trades,
        instruments,
        regime_symbol: portfolio.regime_symbol_or_default(),
        max_positions: portfolio.max_positions_or_default(),
        sizing: numeric.sizing,
        min_order_bump: numeric.min_order_bump,
        quote_gap_ms: numeric.quote_gap_ms,
        strategy_id: args.strategy.clone(),
        params,
    })
}

/// Colonnes d'une paire pour le calcul de G0 ; une série absente donne des colonnes vides (le moteur a
/// déjà refusé un symbole absent du dataset).
pub(super) fn market_of<'a>(series: &'a [(String, Columns)], symbol: &str) -> LegMarket<'a> {
    match series.iter().find(|(name, _)| name == symbol) {
        Some((_, columns)) => LegMarket {
            open_time_ms: columns.open_time_ms(),
            low: columns.low(),
            close: columns.close(),
            volume: columns.volume(),
        },
        None => LegMarket {
            open_time_ms: &[],
            low: &[],
            close: &[],
            volume: &[],
        },
    }
}

/// `tv4 backtest --symbols ...` : prépare le run, rejoue le portefeuille, écrit `metrics.json`,
/// `trades.csv`, `equity.csv` puis `manifest.json`.
pub(super) fn run(args: &BacktestArgs) -> Result<(), CliError> {
    let t0 = Instant::now();
    let started_at_ms = now_ms()?;

    let prepared = prepare(&args.inputs, &args.portfolio)?;
    let strategies = prepared.build_strategies()?;
    let config = prepared.portfolio_config(prepared.slippage);
    let load_ms = elapsed_ms(t0);

    let t_run = Instant::now();
    let result = engine::run_portfolio(PortfolioInputs {
        series: &prepared.series,
        strategies,
        config,
    })?;
    let run_ms = elapsed_ms(t_run);

    let markets: Vec<LegMarket<'_>> = result
        .universe
        .iter()
        .map(|symbol| market_of(&prepared.series, symbol))
        .collect();
    let metrics = MetricsS9::new(
        &result,
        MetricsS9Inputs {
            dataset: prepared.dataset.clone(),
            instruments: prepared.instruments.info.clone(),
            okx_pairs: prepared.instruments.okx_pairs.clone(),
            markets,
        },
    )?;
    let symbols: Vec<&str> = result.universe.iter().map(String::as_str).collect();
    let metrics_bytes = report::json_bytes(&metrics)?;
    let trades_bytes = report::writers::trades_csv_legs(
        &result.trades,
        &result.trade_legs,
        &symbols,
        metrics.span_ms(),
    )?;
    let equity_bytes = report::equity::equity_csv(
        &result.grid_open_ms,
        &result.equity,
        result.initial_cash,
        args.equity_step,
    )?;
    let files = OutputFiles {
        equity_csv: hash::blake3_prefixed(&equity_bytes),
        metrics_json: hash::blake3_prefixed(&metrics_bytes),
        trades_csv: hash::blake3_prefixed(&trades_bytes),
    };

    let run_id = metrics.run_id();
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| args.inputs.root.join("runs").join(&run_id));

    // Même discipline que S8 : le manifeste, qui marque un run complet, et le rapport HTML d'un rendu
    // précédent sont retirés avant de réécrire les autres fichiers, et le manifeste est écrit en dernier.
    // Le `profiles.json` d'un run `--costs` précédent et le `lines.json` d'un run de recherche à deux
    // lignes le sont aussi : verify et le control plane classent un dossier sur sa seule présence.
    for stale in [
        MANIFEST_FILE,
        REPORT_FILE,
        report::profiles::PROFILES_FILE,
        report::research::LINES_FILE,
    ] {
        remove_stale(&out, stale)?;
    }
    report::write_bytes_atomic(&out, "metrics.json", &metrics_bytes)?;
    report::write_bytes_atomic(&out, report::writers::TRADES_FILE, &trades_bytes)?;
    report::write_bytes_atomic(&out, report::equity::EQUITY_FILE, &equity_bytes)?;
    let cli_cold_ms = elapsed_ms(t0);

    let engine_info = report::EngineInfo::current(binary_hash()?);
    let manifest = ManifestS9::new(
        &metrics,
        ManifestS9Inputs {
            instruments_file: prepared.instruments.file_as_given.clone(),
            engine: engine_info,
            equity_step: args.equity_step,
            files,
            started_at_ms,
            timings: Timings {
                load_ms,
                run_ms,
                cli_cold_ms,
            },
        },
    )?;
    debug_assert_eq!(manifest.run_id, run_id);
    report::write_json_atomic(&out, MANIFEST_FILE, &manifest)?;
    println!(
        "run {run_id} écrit dans {} : {} trades sur {} paires, PnL net {:.2}, verdict {}",
        out.display(),
        result.trades.len(),
        result.universe.len(),
        result.pnl_net,
        metrics.verdict
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_paire_okx_remplace_la_monnaie_de_cotation() {
        assert_eq!(okx_inst_id("BTC-USD", "USDC"), "BTC-USDC");
        assert_eq!(okx_inst_id("BTC-USDC", "USDC"), "BTC-USDC");
        assert_eq!(okx_inst_id("ETH-USDT", "USDC"), "ETH-USDC");
        assert_eq!(okx_inst_id("SANSTIRET", "USDC"), "SANSTIRET");
    }

    #[test]
    fn une_option_sans_symbols_est_refusee_en_la_nommant() {
        let args = PortfolioArgs {
            quote_gap_days: Some(3),
            ..PortfolioArgs::default()
        };
        assert!(!args.is_portfolio());
        let erreur = args.reject_without_symbols().unwrap_err();
        assert!(
            matches!(&erreur, CliError::PortfolioOptionWithoutSymbols { option } if option == "--quote-gap-days"),
            "{erreur}"
        );
        assert!(PortfolioArgs::default().reject_without_symbols().is_ok());
    }
}
