//! `metrics.json`, schéma S3 (`tv4-metrics-s3`).
//!
//! Rien ne dépend de la machine ni de l'heure : ni chemin de fichier, ni horodatage d'exécution, ni
//! durée. Le bloc `engine` et `fixture_blake3` sont provisoires : ils passeront dans `manifest.json`
//! et le `dataset_hash` (`dsh-v1`) en S8.

use serde::Serialize;

use crate::engine::{CellResult, RunConfig, SignalRejections};
use crate::model::{Candle, Timeframe, Ts};

use super::ReportError;

/// Identifiant du schéma.
pub const SCHEMA: &str = "tv4-metrics-s3";

/// Écarts temporaires de S3 (docs/03 §7.2, détail dans la doc de `sim` et de `engine`).
pub const S3_DEVIATIONS: [&str; 7] = [
    "stops_inactive",
    "risk_sizing_on_signal_close",
    "market_buy_cash_margin",
    "no_lot_tick_min_size",
    "activation_delay_0",
    "no_volume_rule",
    "single_tf_1m",
];

/// Version du moteur qui a produit le fichier.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EngineInfo {
    pub tv4_version: &'static str,
    pub git_sha: &'static str,
}

/// Données rejouées. `fixture_blake3` est le blake3 des octets du CSV, pas un `dataset_hash`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DataInfo {
    pub source: &'static str,
    pub fixture_blake3: String,
    pub symbol: String,
    pub tf: Timeframe,
    pub bars: usize,
    pub first_open_time_ms: Ts,
    pub last_open_time_ms: Ts,
}

impl DataInfo {
    /// Description d'une fixture CSV : hash de ses octets, symbole et bornes des bougies lues.
    pub fn fixture(
        fixture_blake3: String,
        symbol: &str,
        candles: &[Candle],
    ) -> Result<DataInfo, ReportError> {
        let (Some(first), Some(last)) = (candles.first(), candles.last()) else {
            return Err(ReportError::NoCandles);
        };
        Ok(DataInfo {
            source: "fixture",
            fixture_blake3,
            symbol: symbol.to_string(),
            tf: Timeframe::M1,
            bars: candles.len(),
            first_open_time_ms: first.open_time_ms,
            last_open_time_ms: last.open_time_ms,
        })
    }
}

/// Fenêtre rejouée, après le warmup.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WindowInfo {
    pub start_index: usize,
    pub bars: usize,
    pub first_open_time_ms: Ts,
    pub last_open_time_ms: Ts,
}

/// Bloc des signaux : émis, acceptés, rejetés, et le détail par motif.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SignalsInfo {
    pub emitted: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub rejected_by_reason: SignalRejections,
}

/// Contenu de `metrics.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Metrics {
    pub schema: &'static str,
    pub config: RunConfig,
    pub config_hash: String,
    pub engine: EngineInfo,
    pub data: DataInfo,
    pub window: WindowInfo,
    pub n_trades: u64,
    pub n_trades_forced: u64,
    pub forced_close: bool,
    pub pnl_net: f64,
    pub pnl_realized: f64,
    pub pnl_marked: f64,
    pub fees_total: f64,
    pub slippage_total: f64,
    pub equity_final: f64,
    pub cash_final: f64,
    pub entries_capped_by_cash: u64,
    pub stop_breaches_ignored: u64,
    pub fills: u64,
    pub fills_rejected_insufficient_cash: u64,
    pub orders_unfilled_at_end: u64,
    pub signals: SignalsInfo,
    pub s3_deviations: [&'static str; 7],
}

impl Metrics {
    pub fn new(result: &CellResult, data: DataInfo, engine: EngineInfo) -> Metrics {
        Metrics {
            schema: SCHEMA,
            config: result.config.clone(),
            config_hash: result.config_hash.clone(),
            engine,
            data,
            window: WindowInfo {
                start_index: result.start_index,
                bars: result.bars_in_window,
                first_open_time_ms: result.window_first_open_ms,
                last_open_time_ms: result.window_last_open_ms,
            },
            n_trades: result.trades.len() as u64,
            n_trades_forced: result.n_trades_forced,
            forced_close: result.forced_close,
            pnl_net: result.pnl_net,
            pnl_realized: result.pnl_realized,
            pnl_marked: result.pnl_marked,
            fees_total: result.fees_total,
            slippage_total: result.slippage_total,
            equity_final: result.equity_final,
            cash_final: result.cash_final,
            entries_capped_by_cash: result.sim.entries_capped_by_cash,
            stop_breaches_ignored: result.sim.stop_breaches_ignored,
            fills: result.sim.fills,
            fills_rejected_insufficient_cash: result.sim.fills_rejected_insufficient_cash,
            orders_unfilled_at_end: result.sim.orders_unfilled_at_end,
            signals: SignalsInfo {
                emitted: result.signals_emitted,
                accepted: result.signals_accepted,
                rejected: result.rejections.total(),
                rejected_by_reason: result.rejections,
            },
            s3_deviations: S3_DEVIATIONS,
        }
    }
}
