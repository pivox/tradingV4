//! `metrics.json` du mode portefeuille, schéma S9 (`tv4-metrics-s9`, docs S9 §13.1).
//!
//! Mêmes règles générales que S8 (voir [`metrics`](super::metrics)) : rien ne dépend de la machine, de
//! l'heure, du chemin ni du commit ; toute clé est toujours présente, une valeur indéfinie vaut `null` ;
//! sommes et moyennes sont des boucles `for` dans l'ordre des trades ; la libm n'est appelée que dans
//! [`approx`](super::approx).
//!
//! Les champs communs à S8 gardent leur nom et leur définition, calculés sur le portefeuille : le
//! drawdown porte sur l'equity du portefeuille, l'exposition est la fraction de minutes de la grille
//! avec au moins une position, l'entonnoir est la somme des jambes. Les blocs ajoutés par S9 sont
//! `funnel_by_pair`, `sim_by_pair`, `portfolio`, `priority`, `regime`, `instruments`, `executability`
//! et `g1_executable`, plus `distribution.by_pair`. Toutes les listes par paire suivent l'ordre de
//! l'univers de la configuration, jamais l'alphabet.
//!
//! # Écarts d'implémentation
//!
//! 1. Le bloc `data` porte la fenêtre jouée (`data.window`) : `metrics.json` de S9 n'a ni bloc `window`
//!    ni `data.bars` (les séries ont leurs propres bornes dans `data.series[]`). Il porte aussi `source`
//!    (toujours `dataset`) et `dataset` (le nom du dataset).
//! 2. `signals.rejected` vaut `emitted − accepted` : il compte les refus de `max_positions`, qui ne sont
//!    pas dans `rejected_by_reason` (17 motifs épinglés par S8). Ils ont leur compteur,
//!    `signals.rejected_max_positions`, et `funnel.entry_rejected_max_positions`.
//! 3. `exits_by_type` compte huit types (`gap_forced_exit` en plus des sept du jalon 1).
//! 4. `distribution.by_pair[].exposure` est la somme des `position_time_share` des trades de la paire
//!    (temps détenu / durée de la fenêtre) : le moteur ne compte pas les minutes en position par paire.
//!    Elle peut dépasser la fraction de `exposure` du portefeuille si plusieurs paires sont détenues en
//!    même temps.
//! 5. `instruments.file` est le nom du fichier (sans dossier), pour que deux runs identiques lus depuis
//!    deux dossiers aient le même `metrics.json`. Le chemin tel que donné est dans le manifeste.
//! 6. `sim_deviations` : avec un fichier d'instruments OKX, `instrument_minimums_unvalidated` est
//!    remplacé par `instrument_minimums_current_not_historical` (les minimums du compte sont ceux
//!    d'aujourd'hui, appliqués à tout l'historique, docs/04 §3.1 point 6).
//! 7. `funnel_by_pair[].rejected` : `min_notional` et `lot_step` viennent de `SimDetail` (`below_min_*`),
//!    `cash` somme le refus à la soumission et au fill, `other` est le complément qui rend la somme
//!    exacte (autres refus de soumission, risque invalide au fill, entrée sur une jambe occupée).

use serde::Serialize;

use crate::engine::{
    EngineFunnel, LegResult, PortfolioResult, PortfolioRunConfig, SignalRejections,
};
use crate::model::{ExchangeId, Timeframe, Ts};
use crate::sim::{SimCounters, SimDetail, SizingMode, Trade};
use crate::store::time::format_rfc3339_utc;

use super::ReportError;
use super::approx::{self, Approx};
use super::distribution::{self, Distribution};
use super::drawdown::max_drawdown;
use super::equity::sample_indices;
use super::executability::{
    self, Executability, ExecutabilityInput, G1Counts, G1Executable, LegMarket, OkxPair, PairInput,
};
use super::manifest::run_id_s9;
use super::metrics::{
    EntryNotFilled, Exposure, MaxDrawdowns, RunVerdict, SIM_DEVIATIONS, mean, sum_trades,
};
use super::writers::{exit_type, holding_ms, r_multiple};

/// Identifiant du schéma.
pub const SCHEMA: &str = "tv4-metrics-s9";
/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = 60_000;
/// Écart de S8 remplacé quand les minimums viennent d'un fichier d'instruments OKX.
const MINIMUMS_UNVALIDATED: &str = "instrument_minimums_unvalidated";
/// Son remplaçant (écart 6).
const MINIMUMS_CURRENT: &str = "instrument_minimums_current_not_historical";

/// `config` de `metrics.json` et du manifeste : l'objet haché (sans capital) plus le capital initial.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConfigS9 {
    #[serde(flatten)]
    pub hashed: PortfolioRunConfig,
    /// Hors du `config_hash` (décision du 28/10, 13:52 UTC) ; `tv4 verify` exige des capitaux égaux.
    pub initial_cash: f64,
}

/// Bornes d'un segment du dataset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SeriesInfo {
    pub symbol: String,
    pub rows: u64,
    pub first_ms: Ts,
    pub last_ms: Ts,
}

/// Fenêtre jouée : `end` est exclue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowS9 {
    pub start: String,
    pub end: String,
    pub start_ms: Ts,
    pub end_ms: Ts,
}

/// Bloc `data` de `metrics.json` et du manifeste.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DataS9 {
    pub source: &'static str,
    pub dataset: String,
    /// `blake3:<hex>` (`dsh-v1`), tous segments compris, celui du régime aussi.
    pub dataset_hash: String,
    pub hash_format: String,
    pub exchange: ExchangeId,
    pub tf: Timeframe,
    pub gaps_policy: String,
    /// L'univers, dans l'ordre de la configuration.
    pub symbols: Vec<String>,
    /// La paire de régime si une stratégie le lit, sinon `null`.
    pub regime_symbol: Option<String>,
    /// Les segments du dataset : l'univers dans son ordre, puis la paire de régime si elle n'y est pas.
    pub series: Vec<SeriesInfo>,
    pub window: WindowS9,
}

/// Le dataset tel que la CLI le décrit.
#[derive(Clone, Debug)]
pub struct DatasetInfo {
    pub name: String,
    pub exchange: ExchangeId,
    pub tf: Timeframe,
    pub gaps_policy: String,
    pub hash_format: String,
    pub dataset_hash: String,
    /// Un segment par symbole, dans n'importe quel ordre (le rapport les range).
    pub series: Vec<SeriesInfo>,
}

/// Changement annoncé d'un instrument (`upcChg`), recopié sans être appliqué.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpcomingChangeInfo {
    pub inst_id: String,
    pub param: String,
    pub new_value: String,
    pub eff_time: String,
}

/// Bloc `instruments` de `metrics.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstrumentsInfo {
    /// `okx` (fichier d'instruments du compte) ou `toml` (`config/instruments.toml`).
    pub source: &'static str,
    /// Nom du fichier OKX (écart 5) ; `null` pour `toml`.
    pub file: Option<String>,
    pub instruments_hash: Option<String>,
    pub fetched_at: Option<String>,
    /// Instruments de l'univers dont l'état n'est pas `live`.
    pub not_live: Vec<String>,
    /// `upcChg` des instruments de l'univers.
    pub upcoming_changes: Vec<UpcomingChangeInfo>,
}

impl InstrumentsInfo {
    /// Règles lues dans le TOML de S6.
    pub fn toml() -> InstrumentsInfo {
        InstrumentsInfo {
            source: "toml",
            file: None,
            instruments_hash: None,
            fetched_at: None,
            not_live: Vec::new(),
            upcoming_changes: Vec::new(),
        }
    }

    /// Règles lues dans un fichier d'instruments OKX. `pairs` sont les instruments de l'univers.
    pub fn okx(
        file: String,
        instruments_hash: String,
        fetched_at: String,
        pairs: &[OkxPair],
        upcoming_changes: Vec<UpcomingChangeInfo>,
    ) -> InstrumentsInfo {
        InstrumentsInfo {
            source: "okx",
            file: Some(file),
            instruments_hash: Some(instruments_hash),
            fetched_at: Some(fetched_at),
            not_live: pairs
                .iter()
                .filter(|pair| pair.state != "live")
                .map(|pair| pair.inst_id.clone())
                .collect(),
            upcoming_changes,
        }
    }
}

/// Bloc `signals` : émis, acceptés, refusés (dont `max_positions`) et le détail par motif.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SignalsS9 {
    pub emitted: u64,
    pub accepted: u64,
    /// `emitted − accepted`.
    pub rejected: u64,
    pub rejected_by_reason: SignalRejections,
    pub rejected_max_positions: u64,
}

/// Trades par type de sortie, les huit valeurs de S9 toujours présentes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ExitsByTypeS9 {
    pub end_of_window: u64,
    pub gap_forced_exit: u64,
    pub limit_sell: u64,
    pub stop_loss: u64,
    pub strategy_exit: u64,
    pub take_profit: u64,
    pub time_stop: u64,
    pub trailing_stop: u64,
}

impl ExitsByTypeS9 {
    fn count(&mut self, trade: &Trade) {
        let slot = match exit_type(trade) {
            "end_of_window" => &mut self.end_of_window,
            "gap_forced_exit" => &mut self.gap_forced_exit,
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

/// Entonnoir agrégé du portefeuille : la forme de S8 plus `entry_rejected_max_positions`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FunnelS9 {
    pub evaluations: u64,
    pub raw_entry_signals: Option<u64>,
    pub entry_filtered_by_reason: serde_json::Map<String, serde_json::Value>,
    pub entry_signals: u64,
    pub entry_rejected_by_reason: SignalRejections,
    pub entry_rejected_max_positions: u64,
    pub entry_orders: u64,
    pub entry_not_filled_by_reason: EntryNotFilled,
    pub entry_fills: u64,
    pub trades: u64,
    pub exit_signals: u64,
    pub exit_orders: u64,
    pub other_signals: u64,
    pub other_accepted: u64,
    pub exits_by_type: ExitsByTypeS9,
}

/// Refus d'entrée d'une paire, par motif (docs S9 §9.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PairRejected {
    pub min_notional: u64,
    pub lot_step: u64,
    pub cash: u64,
    pub max_positions: u64,
    pub volume: u64,
    pub other: u64,
}

/// Une ligne de `funnel_by_pair`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PairFunnel {
    pub symbol: String,
    pub evaluations: u64,
    pub entry_signals: u64,
    pub entry_orders: u64,
    pub entry_fills: u64,
    pub trades: u64,
    pub rejected: PairRejected,
}

/// Une ligne de `sim_by_pair`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PairSim {
    pub symbol: String,
    pub counters: SimCounters,
    pub detail: SimDetail,
}

/// Une ligne de `distribution.by_pair`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PairStats {
    pub symbol: String,
    pub trades: u64,
    pub win_rate: Option<f64>,
    pub expectancy_r: Option<f64>,
    pub net_pnl: f64,
    pub exposure: f64,
}

/// `distribution` de S8 plus `by_pair`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DistributionS9 {
    #[serde(flatten)]
    pub base: Distribution,
    pub by_pair: Vec<PairStats>,
}

/// Bloc `portfolio`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PortfolioInfo {
    pub legs: u64,
    pub grid_minutes: u64,
    pub max_open_positions: u64,
    pub max_cost_to_initial: f64,
    /// Entrées refusées par `max_positions`, toutes paires confondues.
    pub positions_cap_hits: u64,
    pub quote_gap_exits: u64,
    pub sizing: SizingMode,
    pub initial_cash: f64,
}

/// Bloc `priority`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PriorityInfo {
    pub rule: &'static str,
    pub liquidity_window_days: u32,
    /// Le rang vient des données du run (docs S9 §5.3).
    pub rank_source: &'static str,
    pub rank_exchange: ExchangeId,
    pub rank_days: u64,
    pub partial_days: u64,
}

/// Bloc `regime` ; absent (`null`) si aucune stratégie ne lit le régime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RegimeInfo {
    pub symbol: String,
    pub tf: Timeframe,
    pub sma_period: u64,
    pub first_ready_ms: Ts,
    pub days_bull: u64,
    pub days_bear: u64,
}

/// Ce que la CLI fournit en plus du résultat du moteur.
#[derive(Clone, Debug)]
pub struct MetricsS9Inputs<'a> {
    pub dataset: DatasetInfo,
    pub instruments: InstrumentsInfo,
    /// Minimums et état OKX de chaque paire de l'univers, dans l'ordre ; `None` sans fichier OKX.
    pub okx_pairs: Option<Vec<OkxPair>>,
    /// Colonnes 1m de chaque paire de l'univers, dans l'ordre (calcul de G0).
    pub markets: Vec<LegMarket<'a>>,
}

/// Contenu de `metrics.json` du mode portefeuille.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetricsS9 {
    pub schema: &'static str,
    pub config: ConfigS9,
    pub config_hash: String,
    /// `blake3:<hex>` (`dsh-v1`).
    pub dataset_hash: String,
    pub data: DataS9,
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
    /// Compteurs du simulateur sommés sur les paires.
    pub sim: SimCounters,
    pub signals: SignalsS9,
    pub signals_rejected: u64,
    pub sim_deviations: Vec<&'static str>,
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
    pub funnel: FunnelS9,
    pub distribution: DistributionS9,
    pub approx: Approx,
    pub funnel_by_pair: Vec<PairFunnel>,
    pub sim_by_pair: Vec<PairSim>,
    pub portfolio: PortfolioInfo,
    pub priority: PriorityInfo,
    pub regime: Option<RegimeInfo>,
    pub instruments: InstrumentsInfo,
    pub executability: Executability,
    pub g1_executable: G1Executable,
}

fn inconsistent(detail: String) -> ReportError {
    ReportError::FunnelInconsistent { detail }
}

/// Lots ouverts distincts de chaque paire, d'après les trades (un lot peut sortir en plusieurs trades).
fn entry_fills_by_leg(result: &PortfolioResult) -> Vec<u64> {
    let mut lots: Vec<Vec<u64>> = vec![Vec::new(); result.legs.len()];
    for (trade, &leg) in result.trades.iter().zip(&result.trade_legs) {
        if let Some(list) = lots.get_mut(usize::from(leg)) {
            list.push(trade.lot_id);
        }
    }
    lots.into_iter()
        .map(|mut list| {
            list.sort_unstable();
            list.dedup();
            list.len() as u64
        })
        .collect()
}

/// Refus au fill d'une paire : cash, minimums, volume, risque invalide, entrée sur une jambe occupée.
fn fill_rejections(counters: &SimCounters) -> u64 {
    counters.buys_rejected_insufficient_cash
        + counters.buys_rejected_below_minimum
        + counters.buys_rejected_volume_share
        + counters.entries_invalid_risk_at_fill
        + counters.entries_dropped_while_long
}

/// Entonnoir et refus d'une paire, avec les identités de docs S9 §9.1 : `entry_signals = entry_orders +
/// refus à la soumission` est contrôlée à l'égalité, et `entry_orders = entry_fills + refus au fill +
/// expirations + annulations` en inégalité (`entry_orders ≥ entry_fills + refus au fill`), car
/// `limits_expired` et `orders_cancelled` mélangent entrées et sorties et ne sont pas publiés par
/// paire ; l'égalité n'est contrôlée qu'en agrégat (`other` de `build_funnel`, et `tv4 verify`).
fn pair_funnel(leg: &LegResult, entry_fills: u64) -> Result<PairFunnel, ReportError> {
    let f: &EngineFunnel = &leg.funnel;
    let submit_rejections = f.entry_rejections.total() + leg.max_positions_rejected;
    if f.entry_signals != f.entry_orders + submit_rejections {
        return Err(inconsistent(format!(
            "{} : {} signaux d'entrée pour {} ordres et {} refus à la soumission",
            leg.symbol, f.entry_signals, f.entry_orders, submit_rejections
        )));
    }
    let at_fill = fill_rejections(&leg.counters);
    if f.entry_orders < entry_fills + at_fill {
        return Err(inconsistent(format!(
            "{} : {} ordres d'entrée pour {} lots ouverts et {} refus au fill",
            leg.symbol, f.entry_orders, entry_fills, at_fill
        )));
    }
    let d = &leg.detail;
    let min_notional = d.below_min_min_qty + d.below_min_notional;
    let lot_step = d.below_min_zero_lot;
    let cash = f.entry_rejections.insufficient_quote + leg.counters.buys_rejected_insufficient_cash;
    let volume = leg.counters.buys_rejected_volume_share;
    let named = min_notional + lot_step + cash + volume + leg.max_positions_rejected;
    let all = submit_rejections + at_fill;
    let other = all.checked_sub(named).ok_or_else(|| {
        inconsistent(format!(
            "{} : {} refus nommés pour {} refus d'entrée",
            leg.symbol, named, all
        ))
    })?;
    Ok(PairFunnel {
        symbol: leg.symbol.clone(),
        evaluations: f.evaluations,
        entry_signals: f.entry_signals,
        entry_orders: f.entry_orders,
        entry_fills,
        trades: leg.trades,
        rejected: PairRejected {
            min_notional,
            lot_step,
            cash,
            max_positions: leg.max_positions_rejected,
            volume,
            other,
        },
    })
}

/// Entonnoir agrégé : même construction que `build_funnel` de S8, sur la somme des jambes.
fn build_funnel(
    result: &PortfolioResult,
    entry_fills: u64,
    exits: ExitsByTypeS9,
) -> Result<FunnelS9, ReportError> {
    let f = &result.funnel;
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
    let known = fill_rejections(&result.sim);
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
    let sim = &result.sim;
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
    Ok(FunnelS9 {
        evaluations: f.evaluations,
        raw_entry_signals,
        entry_filtered_by_reason: filtered,
        entry_signals: f.entry_signals,
        entry_rejected_by_reason: f.entry_rejections,
        entry_rejected_max_positions: result.max_positions_rejected,
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

/// Statistiques de chaque paire d'après ses trades, dans l'ordre de l'univers.
fn pair_stats(result: &PortfolioResult, span_ms: Ts) -> Result<Vec<PairStats>, ReportError> {
    struct Acc {
        trades: u64,
        wins: u64,
        pnl: f64,
        r: f64,
        n_r: u64,
        share: f64,
    }
    let mut accs: Vec<Acc> = result
        .legs
        .iter()
        .map(|_| Acc {
            trades: 0,
            wins: 0,
            pnl: 0.0,
            r: 0.0,
            n_r: 0,
            share: 0.0,
        })
        .collect();
    for (trade, &leg) in result.trades.iter().zip(&result.trade_legs) {
        let Some(acc) = accs.get_mut(usize::from(leg)) else {
            return Err(ReportError::Series {
                detail: format!("un trade porte la jambe {leg}, absente de l'univers"),
            });
        };
        acc.trades += 1;
        if trade.pnl_net > 0.0 {
            acc.wins += 1;
        }
        acc.pnl += trade.pnl_net;
        if let Some(r) = r_multiple(trade) {
            acc.r += r;
            acc.n_r += 1;
        }
        acc.share += holding_ms(trade)? as f64 / span_ms as f64;
    }
    Ok(result
        .legs
        .iter()
        .zip(accs)
        .map(|(leg, acc)| PairStats {
            symbol: leg.symbol.clone(),
            trades: acc.trades,
            win_rate: mean(acc.wins as f64, acc.trades),
            expectancy_r: mean(acc.r, acc.n_r),
            net_pnl: acc.pnl,
            exposure: acc.share,
        })
        .collect())
}

fn timestamp(ts_ms: Ts) -> Result<String, ReportError> {
    format_rfc3339_utc(ts_ms).map_err(|_| ReportError::Timestamp { ts_ms })
}

impl MetricsS9 {
    /// Construit `metrics.json` d'un run portefeuille.
    pub fn new(
        result: &PortfolioResult,
        inputs: MetricsS9Inputs<'_>,
    ) -> Result<MetricsS9, ReportError> {
        let legs = result.legs.len();
        if result.grid_open_ms.len() != result.equity.len() {
            return Err(ReportError::Series {
                detail: format!(
                    "{} minutes de grille pour {} valeurs d'equity",
                    result.grid_open_ms.len(),
                    result.equity.len()
                ),
            });
        }
        if inputs.markets.len() != legs
            || inputs
                .okx_pairs
                .as_ref()
                .is_some_and(|pairs| pairs.len() != legs)
        {
            return Err(ReportError::Series {
                detail: format!("les entrées de G0 ne couvrent pas les {legs} paires de l'univers"),
            });
        }
        if result.grid_open_ms.is_empty() {
            return Err(ReportError::NoCandles);
        }

        let trades = &result.trades;
        let n = trades.len() as u64;
        let sums = sum_trades(trades)?;
        let initial_cash = result.initial_cash;
        let min_trades = result.config.min_trades;

        let expectancy_net = mean(sums.pnl, n);
        let expectancy_r = mean(sums.r, sums.n_r);
        let verdict = RunVerdict::decide(n, min_trades, expectancy_net);
        let profit_factor =
            (sums.gross_loss != 0.0).then(|| sums.gross_profit / (0.0 - sums.gross_loss));

        let start_ms = result.window_first_open_ms;
        let end_ms = result.window_last_open_ms + MINUTE_MS;
        let span_ms = end_ms - start_ms;

        // Drawdowns : instants de clôture des minutes de la grille.
        let closes: Vec<Ts> = result.grid_open_ms.iter().map(|t| t + MINUTE_MS).collect();
        let m1 = max_drawdown(initial_cash, &closes, &result.equity);
        let sampled = sample_indices(&result.grid_open_ms, Timeframe::H1);
        let h1_times: Vec<Ts> = sampled
            .iter()
            .filter_map(|&j| closes.get(j).copied())
            .collect();
        let h1_equity: Vec<f64> = sampled
            .iter()
            .filter_map(|&j| result.equity.get(j).copied())
            .collect();
        let h1 = max_drawdown(initial_cash, &h1_times, &h1_equity);

        let bars_in_window = result.grid_minutes as u64;
        let exposure = Exposure {
            bars_in_position: result.bars_in_position,
            bars_in_window,
            fraction: result.bars_in_position as f64 / bars_in_window as f64,
        };

        let mut exits = ExitsByTypeS9::default();
        for trade in trades {
            exits.count(trade);
        }
        let fills_by_leg = entry_fills_by_leg(result);
        let entry_fills: u64 = fills_by_leg.iter().sum();
        let funnel = build_funnel(result, entry_fills, exits)?;
        let mut funnel_by_pair = Vec::with_capacity(legs);
        for (leg, &fills) in result.legs.iter().zip(&fills_by_leg) {
            funnel_by_pair.push(pair_funnel(leg, fills)?);
        }
        let sim_by_pair: Vec<PairSim> = result
            .legs
            .iter()
            .map(|leg| PairSim {
                symbol: leg.symbol.clone(),
                counters: leg.counters,
                detail: leg.detail,
            })
            .collect();

        let mut g1_counts = G1Counts::default();
        for pair in &funnel_by_pair {
            g1_counts.fills += pair.entry_fills;
            g1_counts.rejected_volume += pair.rejected.volume;
            g1_counts.rejected_min_notional += pair.rejected.min_notional;
            g1_counts.rejected_lot_step += pair.rejected.lot_step;
            g1_counts.rejected_cash += pair.rejected.cash;
            g1_counts.rejected_max_positions += pair.rejected.max_positions;
        }
        let okx_rules = inputs.okx_pairs.is_some();
        // Les carnets mesurés sont ceux du lieu d'exécution seulement si le dataset est OKX et si
        // chaque jambe est son propre instrument USDC d'exécution (docs S9 §10.1) : un dataset OKX en
        // USDT, ou sans fichier d'instruments, ne l'est pas.
        let venue_matches_execution = inputs.dataset.exchange == ExchangeId::Okx
            && inputs.okx_pairs.as_ref().is_some_and(|pairs| {
                pairs.len() == result.legs.len()
                    && pairs
                        .iter()
                        .zip(&result.legs)
                        .all(|(pair, leg)| pair.inst_id == leg.symbol)
            });
        let g1_executable = executability::g1(g1_counts, initial_cash, okx_rules);
        let mut okx_iter = inputs.okx_pairs.clone().map(Vec::into_iter);
        let pair_inputs: Vec<PairInput<'_>> = result
            .legs
            .iter()
            .zip(&inputs.markets)
            .map(|(leg, market)| PairInput {
                symbol: &leg.symbol,
                okx: okx_iter.as_mut().and_then(Iterator::next),
                entry_orders: leg.funnel.entry_orders,
                entry_fractions: &leg.entry_fractions,
                market: *market,
            })
            .collect();
        let executability = executability::compute(&ExecutabilityInput {
            pairs: pair_inputs,
            window_first_open_ms: result.window_first_open_ms,
            window_last_open_ms: result.window_last_open_ms,
            okx_rules,
            venue_matches_execution,
        });

        let regime = match (&result.regime, &result.config.portfolio.regime) {
            (Some(stats), Some(record)) => Some(RegimeInfo {
                symbol: stats.symbol.clone(),
                tf: record.tf,
                sma_period: record.sma_period as u64,
                first_ready_ms: stats.first_ready_ms,
                days_bull: stats.days_bull,
                days_bear: stats.days_bear,
            }),
            _ => None,
        };

        let distribution = DistributionS9 {
            base: distribution::compute(trades, expectancy_net, expectancy_r)?,
            by_pair: pair_stats(result, span_ms)?,
        };

        let data = {
            let mut series: Vec<SeriesInfo> = Vec::with_capacity(inputs.dataset.series.len());
            for symbol in &result.universe {
                if let Some(info) = inputs.dataset.series.iter().find(|s| &s.symbol == symbol) {
                    series.push(info.clone());
                }
            }
            for info in &inputs.dataset.series {
                if !series.iter().any(|s| s.symbol == info.symbol) {
                    series.push(info.clone());
                }
            }
            DataS9 {
                source: "dataset",
                dataset: inputs.dataset.name.clone(),
                dataset_hash: inputs.dataset.dataset_hash.clone(),
                hash_format: inputs.dataset.hash_format.clone(),
                exchange: inputs.dataset.exchange,
                tf: inputs.dataset.tf,
                gaps_policy: inputs.dataset.gaps_policy.clone(),
                symbols: result.universe.clone(),
                regime_symbol: result.regime.as_ref().map(|r| r.symbol.clone()),
                series,
                window: WindowS9 {
                    start: timestamp(start_ms)?,
                    end: timestamp(end_ms)?,
                    start_ms,
                    end_ms,
                },
            }
        };

        let rejected_total = result.rejections.total() + result.max_positions_rejected;
        if result.signals_emitted.checked_sub(result.signals_accepted) != Some(rejected_total) {
            return Err(inconsistent(format!(
                "{} signaux émis, {} acceptés et {} refus comptés",
                result.signals_emitted, result.signals_accepted, rejected_total
            )));
        }

        let sim_deviations = SIM_DEVIATIONS
            .iter()
            .map(|&d| {
                if okx_rules && d == MINIMUMS_UNVALIDATED {
                    MINIMUMS_CURRENT
                } else {
                    d
                }
            })
            .collect();

        Ok(MetricsS9 {
            schema: SCHEMA,
            config: ConfigS9 {
                hashed: result.config.clone(),
                initial_cash,
            },
            config_hash: result.config_hash.clone(),
            dataset_hash: inputs.dataset.dataset_hash.clone(),
            data,
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
            signals: SignalsS9 {
                emitted: result.signals_emitted,
                accepted: result.signals_accepted,
                rejected: rejected_total,
                rejected_by_reason: result.rejections,
                rejected_max_positions: result.max_positions_rejected,
            },
            signals_rejected: rejected_total,
            sim_deviations,
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
            funnel_by_pair,
            sim_by_pair,
            portfolio: PortfolioInfo {
                legs: legs as u64,
                grid_minutes: bars_in_window,
                max_open_positions: u64::from(result.max_open_positions),
                max_cost_to_initial: result.max_cost_to_initial,
                positions_cap_hits: result.max_positions_rejected,
                quote_gap_exits: result.quote_gap_exits,
                sizing: result.sizing,
                initial_cash,
            },
            priority: PriorityInfo {
                rule: result.priority.rule,
                liquidity_window_days: result.priority.liquidity_window_days,
                rank_source: "dataset",
                rank_exchange: inputs.dataset.exchange,
                rank_days: result.priority.rank_days as u64,
                partial_days: result.priority.partial_days,
            },
            regime,
            instruments: inputs.instruments,
            executability,
            g1_executable,
        })
    }

    /// Durée de la fenêtre jouée : `dernière ouverture + 60 000 − première ouverture`.
    pub fn span_ms(&self) -> Ts {
        self.data.window.end_ms - self.data.window.start_ms
    }

    /// Identifiant du run : le capital y entre (il n'est pas dans `config_hash`).
    pub fn run_id(&self) -> String {
        run_id_s9(
            &self.config_hash,
            &self.dataset_hash,
            self.data.window.start_ms,
            self.data.window.end_ms,
            self.config.initial_cash,
        )
    }

    /// Verdict sous forme de valeur.
    pub fn run_verdict(&self) -> RunVerdict {
        RunVerdict::decide(self.n_trades, self.min_trades, self.expectancy_net)
    }
}
