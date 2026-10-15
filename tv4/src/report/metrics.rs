//! `metrics.json`, schéma S6 (`tv4-metrics-s6`).
//!
//! Rien ne dépend de la machine ni de l'heure : ni chemin de fichier, ni horodatage d'exécution, ni
//! durée. Le bloc `engine`, `fixture_blake3` et le bloc dataset (`dataset_hash`, ajouté en S4 pour
//! `tv4 backtest --dataset`) sont provisoires : le `dataset_hash` (`dsh-v1`) passera dans
//! `manifest.json` en S8.

use serde::Serialize;

use crate::engine::{CellResult, RunConfig, SignalRejections};
use crate::model::{Candle, Timeframe, Ts};
use crate::sim::SimCounters;

use super::ReportError;

/// Identifiant du schéma.
pub const SCHEMA: &str = "tv4-metrics-s6";

/// Écarts du simulateur encore ouverts (docs/03 §7.2, détail dans la doc de `sim`).
///
/// - `market_buy_cash_margin` : un achat market est plafonné à `cash × (1 − marge) / (px × (1 + taker))`.
/// - `stop_slippage_fixed` : le slippage de stop est un scalaire plus `stop_stress_bps` à 0 jusqu'à S10
///   (issue #12), pas une distribution.
/// - `instrument_minimums_unvalidated` : les minimums d'instrument sont à 0 (S9, issue #11).
/// - `no_maker_fill_probability` : une limit traversée d'un tick est remplie en entier, sans modèle de
///   file d'attente.
/// - `exit_volume_rule_counted_not_rejected` : la règle des 0,5 % du volume quote ne refuse que les
///   achats ; une sortie au-delà est exécutée au prix plein et comptée (`exits_over_volume_share`).
///   Précision de docs/04 §3.1 point 4 à valider par le propriétaire.
/// - `limit_gap_fill_optimistic` : une limit déjà au repos qu'un gap traverse est remplie à
///   `min(L, open)` (`max(L, open)` en vente), et une limit déjà marketable à son activation est
///   facturée maker ; un carnet continu la prendrait à `L`, ou en taker. Règles de l'issue S6, à arbitrer
///   par le propriétaire.
pub const SIM_DEVIATIONS: [&str; 6] = [
    "market_buy_cash_margin",
    "stop_slippage_fixed",
    "instrument_minimums_unvalidated",
    "no_maker_fill_probability",
    "exit_volume_rule_counted_not_rejected",
    "limit_gap_fill_optimistic",
];

/// Version du moteur qui a produit le fichier.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EngineInfo {
    pub tv4_version: &'static str,
    pub git_sha: &'static str,
}

/// Données rejouées. `fixture_blake3` est le blake3 des octets du CSV, pas un `dataset_hash`.
///
/// Les clés `fixture_blake3` (sortie `--fixture`) et `dataset`, `dataset_hash`, `hash_format`,
/// `gaps_policy`, `synthetic_bars` (sortie `--dataset`, S4) sont exclusives : la sortie `--fixture`
/// garde exactement ses sept clés de S3, à l'octet près.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DataInfo {
    /// `fixture` ou `dataset`.
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture_blake3: Option<String>,
    /// Nom du dataset (`datasets/<nom>.json`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    /// `blake3:<hex>`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_hash: Option<String>,
    /// `dsh-v1`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_bars: Option<usize>,
    pub symbol: String,
    pub tf: Timeframe,
    /// Bougies passées au moteur, synthétiques comprises.
    pub bars: usize,
    pub first_open_time_ms: Ts,
    pub last_open_time_ms: Ts,
}

/// Référence d'un dataset pour le rapport (évite une fonction à 8 arguments et une dépendance de
/// `report` vers `store`).
#[derive(Clone, Debug, PartialEq)]
pub struct DatasetRef {
    pub name: String,
    pub dataset_hash: String,
    pub hash_format: String,
    pub gaps_policy: String,
    pub synthetic_bars: usize,
}

impl DataInfo {
    /// Description d'une fixture CSV : hash de ses octets, symbole et bornes des bougies lues.
    pub fn fixture(
        fixture_blake3: String,
        symbol: &str,
        candles: &[Candle],
    ) -> Result<DataInfo, ReportError> {
        let (first, last) = bounds(candles)?;
        Ok(DataInfo {
            source: "fixture",
            fixture_blake3: Some(fixture_blake3),
            dataset: None,
            dataset_hash: None,
            hash_format: None,
            gaps_policy: None,
            synthetic_bars: None,
            symbol: symbol.to_string(),
            tf: Timeframe::M1,
            bars: candles.len(),
            first_open_time_ms: first,
            last_open_time_ms: last,
        })
    }

    /// Description d'un dataset figé : sa référence, le symbole, le timeframe et les bornes des
    /// bougies passées au moteur (synthétiques comprises).
    pub fn dataset(
        dataset: DatasetRef,
        symbol: &str,
        tf: Timeframe,
        candles: &[Candle],
    ) -> Result<DataInfo, ReportError> {
        let (first, last) = bounds(candles)?;
        Ok(DataInfo {
            source: "dataset",
            fixture_blake3: None,
            dataset: Some(dataset.name),
            dataset_hash: Some(dataset.dataset_hash),
            hash_format: Some(dataset.hash_format),
            gaps_policy: Some(dataset.gaps_policy),
            synthetic_bars: Some(dataset.synthetic_bars),
            symbol: symbol.to_string(),
            tf,
            bars: candles.len(),
            first_open_time_ms: first,
            last_open_time_ms: last,
        })
    }
}

/// Premier et dernier `open_time` des bougies, ou [`ReportError::NoCandles`].
fn bounds(candles: &[Candle]) -> Result<(Ts, Ts), ReportError> {
    match (candles.first(), candles.last()) {
        (Some(first), Some(last)) => Ok((first.open_time_ms, last.open_time_ms)),
        _ => Err(ReportError::NoCandles),
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
    pub fills: u64,
    /// Compteurs du simulateur, tous les `u64` nommés de [`SimCounters`].
    pub sim: SimCounters,
    pub signals: SignalsInfo,
    pub sim_deviations: [&'static str; 6],
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
            fills: result.sim.fills,
            sim: result.sim,
            signals: SignalsInfo {
                emitted: result.signals_emitted,
                accepted: result.signals_accepted,
                rejected: result.rejections.total(),
                rejected_by_reason: result.rejections,
            },
            sim_deviations: SIM_DEVIATIONS,
        }
    }
}
