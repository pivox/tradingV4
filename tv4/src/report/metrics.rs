//! `metrics.json`, schéma S8 (`tv4-metrics-s8`).
//!
//! Rien ne dépend de la machine, de l'heure, du chemin ni du commit : le bloc `engine` et les champs de
//! provenance de `data` (`fixture_blake3`, `hash_format`) vivent dans `manifest.json`. Toute clé est
//! toujours présente ; une valeur indéfinie vaut `null`. Sommes et moyennes : boucles `for` dans
//! l'ordre des trades du simulateur (docs/03 §7.4), aucune libm hors de [`approx`](super::approx).

use serde::Serialize;

use crate::engine::{CellResult, RunConfig, SignalRejections};
use crate::model::{Candle, ExchangeId, Timeframe, Ts};
use crate::sim::{SimCounters, Trade};

use super::ReportError;
use super::approx::{self, Approx};
use super::distribution::{self, Distribution};
use super::drawdown::{Dd, max_drawdown};
use super::equity::sample_indices;
use super::writers::{exit_type, holding_ms, r_multiple};

/// Identifiant du schéma.
pub const SCHEMA: &str = "tv4-metrics-s8";

/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = 60_000;

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
pub const SIM_DEVIATIONS: [&str; 5] = [
    "market_buy_cash_margin",
    "stop_slippage_fixed",
    "instrument_minimums_unvalidated",
    "no_maker_fill_probability",
    "exit_volume_rule_counted_not_rejected",
];

/// Verdict d'un run (précédence : trop peu de trades d'abord, une espérance sur moins de `min_trades`
/// trades ne se lit pas).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunVerdict {
    /// Au moins `min_trades` trades et une espérance nette strictement positive.
    Ok,
    /// Moins de `min_trades` trades (zéro compris).
    RejectedTooFewTrades,
    /// Assez de trades mais une espérance nette ≤ 0.
    RejectedNegativeExpectancy,
}

impl RunVerdict {
    /// Verdict pour `n_trades` trades, le seuil `min_trades` et l'espérance nette.
    pub fn decide(n_trades: u64, min_trades: u32, expectancy_net: Option<f64>) -> RunVerdict {
        if n_trades < u64::from(min_trades) {
            RunVerdict::RejectedTooFewTrades
        } else if expectancy_net.is_none_or(|e| e <= 0.0) {
            RunVerdict::RejectedNegativeExpectancy
        } else {
            RunVerdict::Ok
        }
    }

    /// Valeur écrite dans `metrics.json` et `manifest.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            RunVerdict::Ok => "ok",
            RunVerdict::RejectedTooFewTrades => "rejected_too_few_trades",
            RunVerdict::RejectedNegativeExpectancy => "rejected_negative_expectancy",
        }
    }
}

/// Données rejouées, telles que `metrics.json` les décrit.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DataInfo {
    /// `fixture` ou `dataset`.
    pub source: &'static str,
    /// Nom du dataset (`datasets/<nom>.json`) ; `null` pour une fixture.
    pub dataset: Option<String>,
    pub exchange: ExchangeId,
    pub symbol: String,
    pub tf: Timeframe,
    pub gaps_policy: String,
    /// Bougies synthétiques passées au moteur ; 0 pour une fixture.
    pub synthetic_bars: u64,
    /// Bougies passées au moteur, synthétiques comprises.
    pub bars: u64,
    /// 1m absentes entre la première et la dernière bougie passées au moteur.
    pub minutes_missing: u64,
    pub first_open_time_ms: Ts,
    pub last_open_time_ms: Ts,
}

/// Référence d'un dataset pour le rapport (évite une fonction à 8 arguments).
#[derive(Clone, Debug, PartialEq)]
pub struct DatasetRef {
    pub name: String,
    pub gaps_policy: String,
    pub synthetic_bars: usize,
}

impl DataInfo {
    /// Description d'une fixture CSV : exchange, symbole et bornes des bougies lues.
    pub fn fixture(
        exchange: ExchangeId,
        symbol: &str,
        candles: &[Candle],
    ) -> Result<DataInfo, ReportError> {
        DataInfo::build(
            "fixture",
            None,
            exchange,
            symbol,
            Timeframe::M1,
            "skip",
            0,
            candles,
        )
    }

    /// Description d'un dataset figé : sa référence, l'exchange, le symbole, le timeframe et les
    /// bornes des bougies passées au moteur (synthétiques comprises).
    pub fn dataset(
        dataset: DatasetRef,
        exchange: ExchangeId,
        symbol: &str,
        tf: Timeframe,
        candles: &[Candle],
    ) -> Result<DataInfo, ReportError> {
        DataInfo::build(
            "dataset",
            Some(dataset.name),
            exchange,
            symbol,
            tf,
            &dataset.gaps_policy,
            dataset.synthetic_bars as u64,
            candles,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        source: &'static str,
        dataset: Option<String>,
        exchange: ExchangeId,
        symbol: &str,
        tf: Timeframe,
        gaps_policy: &str,
        synthetic_bars: u64,
        candles: &[Candle],
    ) -> Result<DataInfo, ReportError> {
        let (first, last) = bounds(candles)?;
        let bars = candles.len() as u64;
        let span_minutes = last
            .checked_sub(first)
            .map(|d| d / MINUTE_MS + 1)
            .and_then(|m| u64::try_from(m).ok())
            .ok_or(ReportError::Overflow {
                context: "minutes_missing",
            })?;
        Ok(DataInfo {
            source,
            dataset,
            exchange,
            symbol: symbol.to_string(),
            tf,
            gaps_policy: gaps_policy.to_string(),
            synthetic_bars,
            bars,
            minutes_missing: span_minutes.saturating_sub(bars),
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

impl WindowInfo {
    /// Durée couverte par la fenêtre : `dernière ouverture + 60 000 − première ouverture`.
    pub fn span_ms(&self) -> Ts {
        self.last_open_time_ms + MINUTE_MS - self.first_open_time_ms
    }
}

/// Bloc des signaux : émis, acceptés, rejetés, et le détail par motif.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SignalsInfo {
    pub emitted: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub rejected_by_reason: SignalRejections,
}

/// Drawdowns maximaux sur la série 1m complète et sur la série échantillonnée aux clôtures 1h.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct MaxDrawdowns {
    pub m1: Dd,
    pub h1: Dd,
}

/// Part de la fenêtre passée avec une position ouverte.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Exposure {
    pub bars_in_position: u64,
    pub bars_in_window: u64,
    pub fraction: f64,
}

/// Trades par type de sortie, les sept valeurs du jalon 1 toujours présentes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ExitsByType {
    pub end_of_window: u64,
    pub limit_sell: u64,
    pub stop_loss: u64,
    pub strategy_exit: u64,
    pub take_profit: u64,
    pub time_stop: u64,
    pub trailing_stop: u64,
}

impl ExitsByType {
    fn count(&mut self, kind: &'static str) {
        let slot = match kind {
            "end_of_window" => &mut self.end_of_window,
            "limit_sell" => &mut self.limit_sell,
            "stop_loss" => &mut self.stop_loss,
            "strategy_exit" => &mut self.strategy_exit,
            "take_profit" => &mut self.take_profit,
            "time_stop" => &mut self.time_stop,
            _ => &mut self.trailing_stop,
        };
        *slot += 1;
    }
}

/// Ordres d'entrée acceptés qui n'ont pas produit de lot, par cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EntryNotFilled {
    pub insufficient_cash: u64,
    pub below_minimum: u64,
    pub volume_share: u64,
    pub invalid_risk_at_fill: u64,
    pub dropped_while_long: u64,
    /// `entry_orders − entry_fills − Σ des cinq autres` : ordres d'achat expirés, annulés ou encore en
    /// attente à la fin (les compteurs du simulateur ne distinguent pas achat et vente).
    pub other: u64,
}

/// Entonnoir des signaux (docs/04 §9 point 4, docs/03 §15.1 ligne 4).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Funnel {
    pub evaluations: u64,
    /// Déclaré par la stratégie ; `null` si elle ne déclare rien.
    pub raw_entry_signals: Option<u64>,
    /// Motifs de filtrage déclarés par la stratégie ; `{}` si rien.
    pub entry_filtered_by_reason: serde_json::Map<String, serde_json::Value>,
    pub entry_signals: u64,
    pub entry_rejected_by_reason: SignalRejections,
    pub entry_orders: u64,
    pub entry_not_filled_by_reason: EntryNotFilled,
    pub entry_fills: u64,
    pub trades: u64,
    pub exit_signals: u64,
    pub exit_orders: u64,
    pub other_signals: u64,
    pub other_accepted: u64,
    pub exits_by_type: ExitsByType,
}

/// Contenu de `metrics.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Metrics {
    pub schema: &'static str,
    pub config: RunConfig,
    pub config_hash: String,
    /// `blake3:<hex>` (`dsh-v1`).
    pub dataset_hash: String,
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
    pub signals_rejected: u64,
    pub sim_deviations: [&'static str; 5],
    pub min_trades: u32,
    pub verdict: &'static str,
    pub expectancy_net: Option<f64>,
    pub n_wins: u64,
    pub n_losses: u64,
    pub win_rate: Option<f64>,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub profit_factor: Option<f64>,
    pub avg_holding_ms: Option<f64>,
    pub mfe_mean: Option<f64>,
    pub mae_mean: Option<f64>,
    pub expectancy_r: Option<f64>,
    pub trades_with_r: u64,
    pub return_total: f64,
    pub max_drawdown: MaxDrawdowns,
    pub exposure: Exposure,
    pub funnel: Funnel,
    pub distribution: Distribution,
    pub approx: Approx,
}

/// Agrégats parcourus dans l'ordre des trades.
struct TradeSums {
    pnl: f64,
    n_wins: u64,
    n_losses: u64,
    gross_profit: f64,
    gross_loss: f64,
    holding: i64,
    mfe: f64,
    mae: f64,
    r: f64,
    n_r: u64,
    exits: ExitsByType,
}

fn sum_trades(trades: &[Trade]) -> Result<TradeSums, ReportError> {
    let mut s = TradeSums {
        pnl: 0.0,
        n_wins: 0,
        n_losses: 0,
        gross_profit: 0.0,
        gross_loss: 0.0,
        holding: 0,
        mfe: 0.0,
        mae: 0.0,
        r: 0.0,
        n_r: 0,
        exits: ExitsByType::default(),
    };
    for t in trades {
        s.pnl += t.pnl_net;
        if t.pnl_net > 0.0 {
            s.n_wins += 1;
            s.gross_profit += t.pnl_net;
        } else if t.pnl_net < 0.0 {
            s.n_losses += 1;
            s.gross_loss += t.pnl_net;
        }
        s.holding = s
            .holding
            .checked_add(holding_ms(t)?)
            .ok_or(ReportError::Overflow {
                context: "avg_holding_ms",
            })?;
        s.mfe += t.mfe;
        s.mae += t.mae;
        if let Some(r) = r_multiple(t) {
            s.r += r;
            s.n_r += 1;
        }
        s.exits.count(exit_type(t));
    }
    Ok(s)
}

/// `somme / n`, ou `None` si `n = 0`.
fn mean(sum: f64, n: u64) -> Option<f64> {
    (n > 0).then(|| sum / n as f64)
}

fn build_funnel(result: &CellResult, exits: ExitsByType) -> Result<Funnel, ReportError> {
    let f = &result.funnel;
    let inconsistent = |detail: String| ReportError::FunnelInconsistent { detail };
    if f.entry_signals + f.exit_signals + f.other_signals != result.signals_emitted {
        return Err(inconsistent(format!(
            "{} + {} + {} signaux par classe pour {} émis",
            f.entry_signals, f.exit_signals, f.other_signals, result.signals_emitted
        )));
    }
    if f.entry_orders + f.exit_orders + f.other_accepted != result.signals_accepted {
        return Err(inconsistent(format!(
            "{} + {} + {} acceptés par classe pour {} acceptés",
            f.entry_orders, f.exit_orders, f.other_accepted, result.signals_accepted
        )));
    }
    let mut lot_ids: Vec<u64> = result.trades.iter().map(|t| t.lot_id).collect();
    lot_ids.sort_unstable();
    lot_ids.dedup();
    let entry_fills = lot_ids.len() as u64;
    let sim = &result.sim;
    let known = sim.buys_rejected_insufficient_cash
        + sim.buys_rejected_below_minimum
        + sim.buys_rejected_volume_share
        + sim.entries_invalid_risk_at_fill
        + sim.entries_dropped_while_long;
    let other = f
        .entry_orders
        .checked_sub(entry_fills)
        .and_then(|rest| rest.checked_sub(known))
        .ok_or_else(|| {
            inconsistent(format!(
                "{} ordres d'entrée pour {} lots ouverts et {} abandons comptés",
                f.entry_orders, entry_fills, known
            ))
        })?;
    let (raw_entry_signals, filtered) = match &f.strategy {
        Some(s) => {
            let mut map = serde_json::Map::new();
            for (reason, count) in &s.filtered {
                map.insert((*reason).to_string(), serde_json::Value::from(*count));
            }
            (Some(s.raw_entry_signals), map)
        }
        None => (None, serde_json::Map::new()),
    };
    Ok(Funnel {
        evaluations: f.evaluations,
        raw_entry_signals,
        entry_filtered_by_reason: filtered,
        entry_signals: f.entry_signals,
        entry_rejected_by_reason: f.entry_rejections,
        entry_orders: f.entry_orders,
        entry_not_filled_by_reason: EntryNotFilled {
            insufficient_cash: sim.buys_rejected_insufficient_cash,
            below_minimum: sim.buys_rejected_below_minimum,
            volume_share: sim.buys_rejected_volume_share,
            invalid_risk_at_fill: sim.entries_invalid_risk_at_fill,
            dropped_while_long: sim.entries_dropped_while_long,
            other,
        },
        entry_fills,
        trades: result.trades.len() as u64,
        exit_signals: f.exit_signals,
        exit_orders: f.exit_orders,
        other_signals: f.other_signals,
        other_accepted: f.other_accepted,
        exits_by_type: exits,
    })
}

impl Metrics {
    /// Construit `metrics.json` d'un résultat. `window_open_times` sont les `open_time_ms` des bougies
    /// de la fenêtre jouée (`candles[start_index..]`), une par valeur de `result.equity`.
    pub fn new(
        result: &CellResult,
        window_open_times: &[Ts],
        data: DataInfo,
        dataset_hash: String,
    ) -> Result<Metrics, ReportError> {
        if window_open_times.len() != result.equity.len() {
            return Err(ReportError::Series {
                detail: format!(
                    "{} instants de fenêtre pour {} valeurs d'equity",
                    window_open_times.len(),
                    result.equity.len()
                ),
            });
        }
        let trades = &result.trades;
        let n = trades.len() as u64;
        let sums = sum_trades(trades)?;
        let initial_cash = result.config.initial_cash;
        let min_trades = result.config.min_trades;

        let expectancy_net = mean(sums.pnl, n);
        let expectancy_r = mean(sums.r, sums.n_r);
        let verdict = RunVerdict::decide(n, min_trades, expectancy_net);
        let profit_factor =
            (sums.gross_loss != 0.0).then(|| sums.gross_profit / (0.0 - sums.gross_loss));

        let window = WindowInfo {
            start_index: result.start_index,
            bars: result.bars_in_window,
            first_open_time_ms: result.window_first_open_ms,
            last_open_time_ms: result.window_last_open_ms,
        };
        let span_ms = window.span_ms();

        // Drawdowns : instants de clôture des 1m (`open + 60 000`).
        let closes: Vec<Ts> = window_open_times.iter().map(|t| t + MINUTE_MS).collect();
        let m1 = max_drawdown(initial_cash, &closes, &result.equity);
        let sampled = sample_indices(window_open_times, Timeframe::H1);
        let h1_times: Vec<Ts> = sampled
            .iter()
            .filter_map(|&j| closes.get(j).copied())
            .collect();
        let h1_equity: Vec<f64> = sampled
            .iter()
            .filter_map(|&j| result.equity.get(j).copied())
            .collect();
        let h1 = max_drawdown(initial_cash, &h1_times, &h1_equity);

        let bars_in_window = result.bars_in_window as u64;
        let exposure = Exposure {
            bars_in_position: result.bars_in_position,
            bars_in_window,
            fraction: result.bars_in_position as f64 / bars_in_window as f64,
        };
        let funnel = build_funnel(result, sums.exits)?;
        let distribution = distribution::compute(trades, expectancy_net, expectancy_r)?;

        Ok(Metrics {
            schema: SCHEMA,
            config: result.config.clone(),
            config_hash: result.config_hash.clone(),
            dataset_hash,
            data,
            window,
            n_trades: n,
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
            signals_rejected: result.rejections.total(),
            sim_deviations: SIM_DEVIATIONS,
            min_trades,
            verdict: verdict.as_str(),
            expectancy_net,
            n_wins: sums.n_wins,
            n_losses: sums.n_losses,
            win_rate: mean(sums.n_wins as f64, n),
            gross_profit: sums.gross_profit,
            gross_loss: sums.gross_loss,
            profit_factor,
            avg_holding_ms: mean(sums.holding as f64, n),
            mfe_mean: mean(sums.mfe, n),
            mae_mean: mean(sums.mae, n),
            expectancy_r,
            trades_with_r: sums.n_r,
            return_total: result.equity_final / initial_cash - 1.0,
            max_drawdown: MaxDrawdowns { m1, h1 },
            exposure,
            funnel,
            distribution,
            approx: approx::compute(initial_cash, result.equity_final, span_ms),
        })
    }

    /// Verdict sous forme de valeur.
    pub fn run_verdict(&self) -> RunVerdict {
        RunVerdict::decide(self.n_trades, self.min_trades, self.expectancy_net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{ExitReason, Liquidity};

    /// Trade écrit à la main : PnL, durée de détention, MFE, MAE et risque (stop sous l'entrée).
    fn trade(pnl: f64, holding: i64, mfe: f64, mae: f64, risk: Option<f64>) -> Trade {
        Trade {
            lot_id: 0,
            entry_signal_ms: 0,
            entry_order_ms: 0,
            entry_ms: 1_000,
            entry_ref_price: 100.0,
            entry_price: 100.0,
            entry_fee: 0.0,
            entry_slippage: 0.0,
            entry_liquidity: Liquidity::Taker,
            exit_signal_ms: 0,
            exit_order_ms: 0,
            exit_ms: 1_000 + holding,
            exit_ref_price: 100.0,
            exit_price: 100.0,
            exit_fee: 0.0,
            exit_slippage: 0.0,
            exit_liquidity: Liquidity::Taker,
            qty: 1.0,
            pnl_net: pnl,
            mfe,
            mae,
            stop_at_exit: None,
            stop_at_entry: risk.map(|r| 100.0 - r),
            stop_moved: false,
            capped_by_cash: false,
            exit_reason: ExitReason::Signal,
            forced_close: false,
            entry_tag: "t",
            exit_tag: "t",
        }
    }

    #[test]
    fn verdict_trois_cas_et_precedence() {
        use RunVerdict::*;
        // Trop peu de trades d'abord, même avec une espérance négative ou nulle (`n = 0` compris).
        assert_eq!(RunVerdict::decide(0, 30, None), RejectedTooFewTrades);
        assert_eq!(RunVerdict::decide(29, 30, Some(-1.0)), RejectedTooFewTrades);
        assert_eq!(RunVerdict::decide(29, 30, Some(5.0)), RejectedTooFewTrades);
        // Assez de trades : le signe de l'espérance décide, zéro compris.
        assert_eq!(
            RunVerdict::decide(30, 30, Some(0.0)),
            RejectedNegativeExpectancy
        );
        assert_eq!(
            RunVerdict::decide(30, 30, Some(-0.1)),
            RejectedNegativeExpectancy
        );
        assert_eq!(RunVerdict::decide(30, 30, Some(0.1)), Ok);
        assert_eq!(Ok.as_str(), "ok");
        assert_eq!(RejectedTooFewTrades.as_str(), "rejected_too_few_trades");
        assert_eq!(
            RejectedNegativeExpectancy.as_str(),
            "rejected_negative_expectancy"
        );
    }

    #[test]
    fn agregats_sur_des_trades_ecrits_a_la_main() {
        let trades = [
            trade(3.0, 10, 4.0, -1.0, Some(1.0)),  // gagnant, R = 3
            trade(-1.0, 20, 0.5, -1.5, Some(2.0)), // perdant, R = -0,5
            trade(0.0, 30, 0.0, 0.0, None),        // nul : ni gagnant ni perdant, pas de R
            trade(-2.0, 40, 1.0, -2.5, Some(1.0)), // perdant, R = -2
        ];
        let s = sum_trades(&trades).unwrap();
        assert_eq!((s.pnl, s.n_wins, s.n_losses), (0.0, 1, 2));
        assert_eq!((s.gross_profit, s.gross_loss), (3.0, -3.0));
        assert_eq!(s.holding, 100);
        assert_eq!((s.mfe, s.mae), (5.5, -5.0));
        assert_eq!((s.r, s.n_r), (0.5, 3));
        assert_eq!(mean(s.pnl, 4), Some(0.0));
        assert_eq!(mean(s.mfe, 4), Some(1.375));
        assert_eq!(mean(s.r, s.n_r), Some(0.5 / 3.0));
        assert_eq!(s.exits.strategy_exit, 4);
    }

    #[test]
    fn aucun_trade_donne_des_moyennes_indefinies() {
        let s = sum_trades(&[]).unwrap();
        assert_eq!((s.n_wins, s.n_losses, s.n_r), (0, 0, 0));
        assert_eq!((s.gross_profit, s.gross_loss), (0.0, 0.0));
        assert_eq!(mean(s.pnl, 0), None);
        assert_eq!(mean(s.r, s.n_r), None);
    }

    #[test]
    fn aucune_perte_laisse_gross_loss_nul() {
        let trades = [trade(1.0, 5, 1.0, 0.0, None), trade(2.0, 5, 2.0, 0.0, None)];
        let s = sum_trades(&trades).unwrap();
        assert_eq!(s.gross_loss.to_bits(), 0.0f64.to_bits());
        assert_eq!(s.n_losses, 0);
    }

    #[test]
    fn la_somme_des_durees_est_verifiee() {
        let trades = [trade(1.0, i64::MAX - 1_000, 0.0, 0.0, None); 2];
        assert!(matches!(
            sum_trades(&trades),
            Err(ReportError::Overflow { .. })
        ));
    }

    #[test]
    fn exits_by_type_compte_les_sept_valeurs() {
        let mut e = ExitsByType::default();
        for kind in [
            "end_of_window",
            "limit_sell",
            "stop_loss",
            "strategy_exit",
            "take_profit",
            "time_stop",
            "trailing_stop",
            "stop_loss",
        ] {
            e.count(kind);
        }
        assert_eq!((e.stop_loss, e.time_stop, e.trailing_stop), (2, 1, 1));
        let json = serde_json::to_value(e).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 7);
    }

    fn bougie(open_time_ms: Ts) -> Candle {
        Candle {
            open_time_ms,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            trades: None,
        }
    }

    #[test]
    fn minutes_manquantes_et_bornes_des_donnees() {
        let t0 = 1_704_067_200_000;
        // Minutes 0, 1, 4 : deux minutes absentes.
        let candles = [bougie(t0), bougie(t0 + 60_000), bougie(t0 + 240_000)];
        let d = DataInfo::fixture(ExchangeId::Kraken, "BTC-USD", &candles).unwrap();
        assert_eq!((d.bars, d.minutes_missing), (3, 2));
        assert_eq!(
            (d.first_open_time_ms, d.last_open_time_ms),
            (t0, t0 + 240_000)
        );
        assert_eq!(
            (d.source, d.synthetic_bars, d.gaps_policy.as_str()),
            ("fixture", 0, "skip")
        );
        assert!(DataInfo::fixture(ExchangeId::Kraken, "BTC-USD", &[]).is_err());
        let contigu = [bougie(t0), bougie(t0 + 60_000)];
        let d = DataInfo::fixture(ExchangeId::Kraken, "BTC-USD", &contigu).unwrap();
        assert_eq!(d.minutes_missing, 0);
    }

    #[test]
    fn span_de_la_fenetre() {
        let w = WindowInfo {
            start_index: 0,
            bars: 3,
            first_open_time_ms: 120_000,
            last_open_time_ms: 240_000,
        };
        assert_eq!(w.span_ms(), 180_000);
    }
}
