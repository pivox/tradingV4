//! Sous-commande `tv4 backtest` (S3) : backtest minimal sur une fixture CSV Kraken 1m.
//!
//! Aucune logique métier ici (docs/03-architecture.md §2) : lecture des fichiers, appel du registre,
//! de `engine::run_cell` et de `report`.

use std::fs;
use std::path::PathBuf;

use clap::Args;

use super::CliError;
use crate::engine::{self, CellConfig};
use crate::marketdata::kraken;
use crate::model::hash;
use crate::model::{ExchangeId, FeeSchedule, Slippage, Timeframe};
use crate::report::{self, DataInfo, EngineInfo, Metrics};
use crate::sim::SimRules;
use crate::strategy::registry;

/// Symbole canonique de la fixture (XBTUSD Kraken, docs/03 §4).
const FIXTURE_SYMBOL: &str = "BTC-USD";

/// Arguments de `tv4 backtest`.
#[derive(Debug, Args)]
pub struct BacktestArgs {
    /// CSV Kraken OHLCVT en 1m, sans en-tête (par exemple fixtures/kraken_xbtusd_1m.csv).
    #[arg(long, value_name = "CSV")]
    pub fixture: PathBuf,
    /// Identifiant du registre (sma_cross).
    #[arg(long, value_name = "ID")]
    pub strategy: String,
    /// Fichier TOML des paramètres de la stratégie.
    #[arg(long, value_name = "TOML")]
    pub params: PathBuf,
    /// Grille de frais (section de --fees-file).
    #[arg(long, value_name = "EXCHANGE", default_value = "okx")]
    pub fees: ExchangeId,
    /// Fichier des grilles de frais, relatif au dossier courant.
    #[arg(long, value_name = "TOML", default_value = "config/fees.toml")]
    pub fees_file: PathBuf,
    /// Slippage en points de base (nombre fini dans [0, 10 000[).
    #[arg(
        long,
        value_name = "BPS",
        default_value = "5",
        allow_negative_numbers = true
    )]
    pub slippage_bps: Slippage,
    /// Capital initial, dans la monnaie de cotation des données.
    #[arg(
        long,
        value_name = "MONTANT",
        default_value_t = 300.0,
        allow_negative_numbers = true
    )]
    pub capital: f64,
    /// Dossier de sortie (créé au besoin) ; metrics.json y est écrit.
    #[arg(long, value_name = "DOSSIER")]
    pub out: PathBuf,
}

/// `tv4 backtest` : lit la fixture et les paramètres, rejoue la stratégie, écrit `metrics.json`.
pub(super) fn run(args: &BacktestArgs) -> Result<(), CliError> {
    let bytes = fs::read(&args.fixture).map_err(|source| CliError::Read {
        path: args.fixture.display().to_string(),
        source,
    })?;
    let fixture_blake3 = hash::blake3_hex(&bytes);
    let candles = kraken::read_all(bytes.as_slice(), Timeframe::M1)?;

    let fees = FeeSchedule::load(args.fees, &args.fees_file)?;

    let params_text = fs::read_to_string(&args.params).map_err(|source| CliError::Read {
        path: args.params.display().to_string(),
        source,
    })?;
    let params =
        toml::from_str::<toml::Value>(&params_text).map_err(|source| CliError::ParamsToml {
            path: args.params.display().to_string(),
            source,
        })?;
    let strategy = registry::build(&args.strategy, &params)?;

    let config = CellConfig {
        symbol: FIXTURE_SYMBOL,
        fees,
        slippage: args.slippage_bps,
        initial_cash: args.capital,
        sim_rules: SimRules::default(),
    };
    let result = engine::run_cell(&candles, strategy, &config)?;

    let data = DataInfo::fixture(fixture_blake3, FIXTURE_SYMBOL, &candles)?;
    let engine_info = EngineInfo {
        tv4_version: env!("CARGO_PKG_VERSION"),
        git_sha: crate::GIT_SHA,
    };
    let metrics = Metrics::new(&result, data, engine_info);
    let path = report::write_json_atomic(&args.out, "metrics.json", &metrics)?;
    println!(
        "metrics.json écrit dans {} : {} trades, PnL net {:.2}",
        path.display(),
        result.trades.len(),
        result.pnl_net
    );
    Ok(())
}
