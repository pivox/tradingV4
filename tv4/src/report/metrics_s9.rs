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
    EngineFunnel, LegResult, PortfolioResult, PortfolioRunConfig, ProfileRun, SignalRejections,
};
use crate::model::blocks::Block;
use crate::model::costs::{CostMeasures, CostProfileName, CostsFile, LegCostRecord, ValueSource};
use crate::model::{ExchangeId, FeeGridId, Timeframe, Ts};
use crate::sim::{CostCounters, ExitReason, SimCounters, SimDetail, SizingMode, Trade};
use crate::store::time::format_rfc3339_utc;

use super::ReportError;
use super::approx::{self, Approx};
use super::cost_arithmetic::{self, CostArithmetic, CostArithmeticInput};
use super::distribution::{self, Distribution};
use super::drawdown::max_drawdown;
use super::equity::sample_indices;
use super::executability::{
    self, Executability, ExecutabilityInput, G1Counts, G1Executable, LegMarket, OkxPair, PairInput,
};
use super::manifest::{run_id_s9, run_id_s11};
use super::metrics::{
    EntryNotFilled, Exposure, MaxDrawdowns, RunVerdict, SIM_DEVIATIONS, mean, sum_trades,
};
use super::research::{
    ResearchBlock, ResearchInputs, TradeSplit, UNIVERSE_QUALITY_DEVIATION, split_trades,
};
use super::writers::{exit_type, holding_ms, r_multiple};

/// Identifiant du schéma.
pub const SCHEMA: &str = "tv4-metrics-s9";
/// Identifiant du schéma des runs à profils de coûts (docs/specs/s10.md §12.1).
pub const SCHEMA_S10: &str = "tv4-metrics-s10";
/// Identifiant du schéma des runs de recherche (docs/specs/s11.md §9.2) : s9 ou s10 plus le bloc
/// `research`, avec des métriques de trades sur les trades conservés.
pub const SCHEMA_S11: &str = "tv4-metrics-s11";
/// Écart de S9 que la loi de stop de s10 remplace.
const STOP_SLIPPAGE_FIXED: &str = "stop_slippage_fixed";
/// Écart de S9 que le profil maker remplace par un tirage de fill.
const NO_MAKER_FILL_PROBABILITY: &str = "no_maker_fill_probability";
/// Écart s10 : la probabilité de stress est la valeur provisoire, pas une mesure.
const STOP_STRESS_UNCALIBRATED: &str = "stop_stress_uncalibrated";
/// Écart s10 : une jambe au moins garde un défaut de classe (aucune mesure de carnet).
const COST_DEFAULTS_UNMEASURED: &str = "cost_defaults_unmeasured";
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
    /// Bloc de recherche du dataset (s11) ; absent en s9 et s10.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<Block>,
    /// Fenêtre de décision du bloc (s11) ; absente en s9 et s10.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_window: Option<WindowS9>,
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
    /// Profil de coûts du run (s10) ; absent en s9.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub costs: Option<CostsBlock>,
    /// Arithmétique de docs/04 §3.3 (s10) ; absent en s9.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_arithmetic: Option<CostArithmetic>,
    /// Compteurs de coûts sommés sur les jambes, avec le détail par paire (s10) ; absent en s9.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_counters: Option<CostCountersReport>,
    /// Bloc de recherche (s11) ; absent en s9 et s10.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research: Option<ResearchBlock>,
}

/// Bloc `costs` de `metrics.json` s10 (docs/specs/s10.md §12.1).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CostsBlock {
    pub profile: CostProfileName,
    /// `true` pour baseline et adverse, `false` pour maker (informatif, docs/04 §3).
    pub gates_profile: bool,
    pub rules_version: &'static str,
    pub fee_grid: FeeGridId,
    pub seed: u64,
    /// Étiquette du fichier de coûts, telle que la CLI la donne : jamais un chemin absolu propre à une
    /// machine, puisque `metrics.json` est comparé à l'octet.
    pub costs_file: String,
    pub costs_file_hash: String,
    pub cost_measures_file: Option<String>,
    pub cost_measures_hash: Option<String>,
    pub stress_calibrated: bool,
    pub stress_calibration_ref: Option<String>,
    /// Valeurs résolues et sources de chaque jambe, dans l'ordre de l'univers.
    pub legs: Vec<LegCostRecord>,
}

/// Part des entrées converties remplies en maker : `filled_maker / (filled_maker + taken_at_arrival)`,
/// `None` si le dénominateur est nul (docs/specs/s10.md §6.5). Calculée ici et non par le simulateur.
pub fn maker_entry_maker_share(counters: &CostCounters) -> Option<f64> {
    let filled = counters.maker_entries_filled_maker;
    let denominator = filled + counters.maker_entries_taken_at_arrival;
    (denominator > 0).then(|| filled as f64 / denominator as f64)
}

/// Compteurs de coûts d'une paire et part maker.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CostCountersPair {
    pub symbol: String,
    #[serde(flatten)]
    pub counters: CostCounters,
    pub maker_entry_maker_share: Option<f64>,
}

/// Bloc `cost_counters` : la somme des compteurs des jambes (dans l'ordre de l'univers), la part maker
/// dérivée et le détail par paire.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CostCountersReport {
    #[serde(flatten)]
    pub total: CostCounters,
    pub maker_entry_maker_share: Option<f64>,
    pub by_pair: Vec<CostCountersPair>,
}

impl CostCountersReport {
    /// Somme les compteurs des jambes ; `symbols` et `by_leg` ont la même longueur.
    pub fn new(symbols: &[String], by_leg: &[CostCounters]) -> CostCountersReport {
        let mut total = CostCounters::default();
        let mut by_pair = Vec::with_capacity(by_leg.len());
        for (symbol, c) in symbols.iter().zip(by_leg) {
            total.stop_fills_drawn += c.stop_fills_drawn;
            total.stop_stress_fills += c.stop_stress_fills;
            total.maker_entries_converted += c.maker_entries_converted;
            total.maker_entries_taken_at_arrival += c.maker_entries_taken_at_arrival;
            total.maker_fill_draws += c.maker_fill_draws;
            total.maker_fills += c.maker_fills;
            total.maker_fill_missed += c.maker_fill_missed;
            total.maker_entries_filled_maker += c.maker_entries_filled_maker;
            total.spread_cost_total += c.spread_cost_total;
            total.adverse_selection_total += c.adverse_selection_total;
            by_pair.push(CostCountersPair {
                symbol: symbol.clone(),
                counters: *c,
                maker_entry_maker_share: maker_entry_maker_share(c),
            });
        }
        CostCountersReport {
            maker_entry_maker_share: maker_entry_maker_share(&total),
            total,
            by_pair,
        }
    }
}

/// Fichiers de coûts d'un run s10, tels que la CLI les a lus.
#[derive(Clone, Copy, Debug)]
pub struct CostsInputs<'a> {
    pub file: &'a CostsFile,
    /// Étiquette du fichier de coûts écrite dans `metrics.costs.costs_file` (voir [`CostsBlock`]).
    pub file_label: &'a str,
    pub measures: Option<&'a CostMeasures>,
    /// Étiquette du fichier de mesures ; `None` si et seulement si `measures` l'est.
    pub measures_label: Option<&'a str>,
}

fn inconsistent(detail: String) -> ReportError {
    ReportError::FunnelInconsistent { detail }
}

/// Lots ouverts distincts de chaque paire, d'après les trades (un lot peut sortir en plusieurs trades).
fn entry_fills_by_leg(legs: usize, trades: &[Trade], trade_legs: &[u16]) -> Vec<u64> {
    let mut lots: Vec<Vec<u64>> = vec![Vec::new(); legs];
    for (trade, &leg) in trades.iter().zip(trade_legs) {
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
///
/// En recherche (s11), `entry_signals = entry_orders + refus à la soumission + research_refused`, les
/// refus de recherche restant hors de `rejected` ; `entry_fills` et `trades` portent sur les trades
/// conservés, donc l'entrée d'un trade purgé (soldé à la fin de la fenêtre) tombe dans `other` de
/// l'entonnoir agrégé, avec les expirations et annulations.
fn pair_funnel(
    leg: &LegResult,
    entry_fills: u64,
    research_refused: u64,
    trades: u64,
) -> Result<PairFunnel, ReportError> {
    let f: &EngineFunnel = &leg.funnel;
    let submit_rejections = f.entry_rejections.total() + leg.max_positions_rejected;
    if f.entry_signals != f.entry_orders + submit_rejections + research_refused {
        return Err(inconsistent(format!(
            "{} : {} signaux d'entrée pour {} ordres, {} refus à la soumission et {} refus de recherche",
            leg.symbol, f.entry_signals, f.entry_orders, submit_rejections, research_refused
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
        trades,
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
    n_trades: u64,
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
        trades: n_trades,
        exit_signals: f.exit_signals,
        exit_orders: f.exit_orders,
        other_signals: f.other_signals,
        other_accepted: f.other_accepted,
        exits_by_type: exits,
    })
}

/// Statistiques de chaque paire d'après ses trades, dans l'ordre de l'univers.
fn pair_stats(
    result: &PortfolioResult,
    trades: &[Trade],
    trade_legs: &[u16],
    span_ms: Ts,
) -> Result<Vec<PairStats>, ReportError> {
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
    for (trade, &leg) in trades.iter().zip(trade_legs) {
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

/// Métriques construites et, pour un run de recherche, la classification de ses trades.
struct Built {
    metrics: MetricsS9,
    split: Option<TradeSplit>,
}

impl MetricsS9 {
    /// Construit `metrics.json` d'un run portefeuille.
    ///
    /// Refuse un résultat de recherche ([`PortfolioResult::research`] présent) : ses métriques de trades
    /// se calculent sur les trades conservés, par [`MetricsS9::new_research`].
    pub fn new(
        result: &PortfolioResult,
        inputs: MetricsS9Inputs<'_>,
    ) -> Result<MetricsS9, ReportError> {
        if result.research.is_some() {
            return Err(ReportError::Series {
                detail:
                    "résultat de recherche : utiliser `new_research` (schéma s11, trades purgés)"
                        .to_string(),
            });
        }
        Ok(Self::build(result, inputs, None)?.metrics)
    }

    /// Construit `metrics.json` (`tv4-metrics-s11`) d'un run de recherche (docs/specs/s11.md §9.2) : le
    /// corps de s9, un bloc `research`, des métriques de trades sur les trades conservés et des métriques
    /// d'equity sur la fenêtre de décision.
    pub fn new_research(
        result: &PortfolioResult,
        inputs: MetricsS9Inputs<'_>,
        research: &ResearchInputs<'_>,
    ) -> Result<MetricsS9, ReportError> {
        Ok(Self::build(result, inputs, Some(research))?.metrics)
    }

    fn build(
        result: &PortfolioResult,
        inputs: MetricsS9Inputs<'_>,
        research: Option<&ResearchInputs<'_>>,
    ) -> Result<Built, ReportError> {
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

        // Recherche : les métriques de trades portent sur les trades conservés (docs S11 §3.4), les
        // métriques d'equity sur la fenêtre de décision, purgés compris.
        let split = match (research, result.research.as_ref()) {
            (Some(_), Some(engine)) => Some(split_trades(result, engine.decision_end_ms)?),
            (None, None) => None,
            _ => {
                return Err(ReportError::Series {
                    detail: "entrées de recherche et résultat de recherche vont ensemble"
                        .to_string(),
                });
            }
        };
        let (trades, trade_legs): (&[Trade], &[u16]) = match &split {
            Some(split) => (&split.kept, &split.kept_legs),
            None => (&result.trades, &result.trade_legs),
        };
        let research_refused = |leg: usize| {
            result
                .research
                .as_ref()
                .and_then(|r| r.legs.get(leg))
                .map_or(0, |l| l.counters.refused())
        };
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
        let fills_by_leg = entry_fills_by_leg(legs, trades, trade_legs);
        let entry_fills: u64 = fills_by_leg.iter().sum();
        let funnel = build_funnel(result, entry_fills, exits, n)?;
        let mut funnel_by_pair = Vec::with_capacity(legs);
        for (k, (leg, &fills)) in result.legs.iter().zip(&fills_by_leg).enumerate() {
            // Hors recherche, tous les trades sont conservés : `leg.trades` fait foi. En recherche,
            // `funnel_by_pair` porte sur les trades conservés, comme `funnel` (docs S11 §9.2).
            let kept = if split.is_some() {
                trade_legs.iter().filter(|&&l| usize::from(l) == k).count() as u64
            } else {
                leg.trades
            };
            funnel_by_pair.push(pair_funnel(leg, fills, research_refused(k), kept)?);
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
            by_pair: pair_stats(result, trades, trade_legs, span_ms)?,
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
                block: result.research.as_ref().map(|r| r.block),
                decision_window: result
                    .research
                    .as_ref()
                    .map(|r| -> Result<WindowS9, ReportError> {
                        Ok(WindowS9 {
                            start: timestamp(r.decision_start_ms)?,
                            end: timestamp(r.decision_end_ms)?,
                            start_ms: r.decision_start_ms,
                            end_ms: r.decision_end_ms,
                        })
                    })
                    .transpose()?,
            }
        };

        // Les refus d'entrée de la recherche sont émis puis refusés avant le carnet : ni acceptés, ni dans
        // les motifs de refus du simulateur (écart 3 de `report::research`).
        let rejected_total = result.rejections.total() + result.max_positions_rejected;
        let refused_total = result.research.as_ref().map_or(0, |r| r.totals.refused());
        if result.signals_emitted.checked_sub(result.signals_accepted)
            != Some(rejected_total + refused_total)
        {
            return Err(inconsistent(format!(
                "{} signaux émis, {} acceptés, {} refus comptés et {} refus de recherche",
                result.signals_emitted, result.signals_accepted, rejected_total, refused_total
            )));
        }

        let mut sim_deviations: Vec<&'static str> = SIM_DEVIATIONS
            .iter()
            .map(|&d| {
                if okx_rules && d == MINIMUMS_UNVALIDATED {
                    MINIMUMS_CURRENT
                } else {
                    d
                }
            })
            .collect();
        let research_block = match (research, result.research.as_ref(), split.as_ref()) {
            (Some(inputs), Some(engine), Some(split)) => {
                let block = ResearchBlock::build(engine, &result.universe, inputs, split)?;
                if block.universe.mode == "quarterly" {
                    sim_deviations.push(UNIVERSE_QUALITY_DEVIATION);
                }
                Some(block)
            }
            _ => None,
        };
        let n_trades_forced = if split.is_some() {
            trades
                .iter()
                .filter(|t| t.exit_reason == ExitReason::ForcedClose)
                .count() as u64
        } else {
            result.n_trades_forced
        };

        let metrics = MetricsS9 {
            schema: if research_block.is_some() {
                SCHEMA_S11
            } else {
                SCHEMA
            },
            config: ConfigS9 {
                hashed: result.config.clone(),
                initial_cash,
            },
            config_hash: result.config_hash.clone(),
            dataset_hash: inputs.dataset.dataset_hash.clone(),
            data,
            n_trades: n,
            n_trades_forced,
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
            costs: None,
            cost_arithmetic: None,
            cost_counters: None,
            research: research_block,
        };
        Ok(Built { metrics, split })
    }

    /// Construit `metrics.json` d'un profil de coûts s10 (docs/specs/s10.md §12.1).
    ///
    /// Le corps est celui de [`MetricsS9::new`] sur `run.result` (même entonnoir, mêmes identités), puis
    /// le schéma devient `tv4-metrics-s10`, `sim_deviations` suit §12.1 et les trois blocs `costs`,
    /// `cost_arithmetic` et `cost_counters` sont remplis. Refuse un run qui n'a pas de bloc `costs`
    /// haché, un profil qui ne correspond pas à celui du hash, ou des jambes dont les valeurs résolues ne
    /// suivent pas l'univers.
    pub fn new_s10(
        run: &ProfileRun,
        inputs: MetricsS9Inputs<'_>,
        costs: CostsInputs<'_>,
    ) -> Result<MetricsS9, ReportError> {
        if run.result.research.is_some() {
            return Err(ReportError::Series {
                detail: "résultat de recherche : utiliser `new_s11_profile` (schéma s11)"
                    .to_string(),
            });
        }
        Self::profile(run, inputs, costs, None)
    }

    /// Construit `metrics.json` (`tv4-metrics-s11`) d'un profil de coûts joué en recherche : le corps de
    /// [`MetricsS9::new_research`], plus les trois blocs de coûts de s10. L'arithmétique de coûts porte sur
    /// les trades conservés.
    pub fn new_s11_profile(
        run: &ProfileRun,
        inputs: MetricsS9Inputs<'_>,
        costs: CostsInputs<'_>,
        research: &ResearchInputs<'_>,
    ) -> Result<MetricsS9, ReportError> {
        Self::profile(run, inputs, costs, Some(research))
    }

    fn profile(
        run: &ProfileRun,
        inputs: MetricsS9Inputs<'_>,
        costs: CostsInputs<'_>,
        research: Option<&ResearchInputs<'_>>,
    ) -> Result<MetricsS9, ReportError> {
        let result = &run.result;
        let Some(hashed) = result.config.costs.as_ref() else {
            return Err(ReportError::Series {
                detail:
                    "le run n'a pas de bloc `costs` dans son objet haché : ce n'est pas un run s10"
                        .to_string(),
            });
        };
        if hashed.profile != run.profile {
            return Err(ReportError::Series {
                detail: format!(
                    "profil `{}` joué pour un objet haché de profil `{}`",
                    run.profile, hashed.profile
                ),
            });
        }
        let legs = result.legs.len();
        let symbols_match = run.legs.len() == legs
            && run.cost_by_leg.len() == legs
            && run
                .legs
                .iter()
                .zip(&result.universe)
                .all(|(record, symbol)| &record.symbol == symbol);
        if !symbols_match {
            return Err(ReportError::Series {
                detail: format!(
                    "{} jambes résolues et {} compteurs de coûts pour {legs} paires de l'univers",
                    run.legs.len(),
                    run.cost_by_leg.len()
                ),
            });
        }
        if costs.measures.is_some() != costs.measures_label.is_some() {
            return Err(ReportError::Series {
                detail: "le fichier de mesures et son étiquette vont ensemble".to_string(),
            });
        }
        let decision_tf = run
            .legs
            .first()
            .map(|record| record.decision_tf)
            .ok_or_else(|| ReportError::Series {
                detail: "aucune jambe résolue".to_string(),
            })?;

        let Built { mut metrics, split } = Self::build(result, inputs, research)?;
        // L'arithmétique de coûts suit les trades des métriques : conservés en recherche.
        let trades: &[Trade] = match &split {
            Some(split) => &split.kept,
            None => &result.trades,
        };
        let arithmetic = cost_arithmetic::compute(&CostArithmeticInput {
            trades,
            profile: run.profile,
            decision_tf,
            span_ms: metrics.span_ms(),
            grid_open_ms: &result.grid_open_ms,
            equity: &result.equity,
        })?;
        let defaults_unmeasured = run.legs.iter().any(|record| {
            record.spread_source == ValueSource::Default
                || record.slippage_source == ValueSource::Default
        });
        let stress_calibrated = costs.file.stress_calibrated;
        metrics.sim_deviations = metrics
            .sim_deviations
            .iter()
            .copied()
            .filter(|&d| {
                d != STOP_SLIPPAGE_FIXED
                    && !(run.profile == CostProfileName::Maker && d == NO_MAKER_FILL_PROBABILITY)
            })
            .chain((!stress_calibrated).then_some(STOP_STRESS_UNCALIBRATED))
            .chain(defaults_unmeasured.then_some(COST_DEFAULTS_UNMEASURED))
            .collect();
        if research.is_none() {
            metrics.schema = SCHEMA_S10;
        }
        metrics.costs = Some(CostsBlock {
            profile: run.profile,
            gates_profile: run.profile.gates(),
            rules_version: hashed.rules_version,
            fee_grid: hashed.fee_grid.grid,
            seed: hashed.seed,
            costs_file: costs.file_label.to_string(),
            costs_file_hash: costs.file.file_hash().to_string(),
            cost_measures_file: costs.measures_label.map(str::to_string),
            cost_measures_hash: costs.measures.map(|m| m.file_hash().to_string()),
            stress_calibrated,
            stress_calibration_ref: costs.file.calibration_ref().map(str::to_string),
            legs: run.legs.clone(),
        });
        metrics.cost_arithmetic = Some(arithmetic);
        metrics.cost_counters = Some(CostCountersReport::new(&result.universe, &run.cost_by_leg));
        Ok(metrics)
    }

    /// Durée de la fenêtre jouée : `dernière ouverture + 60 000 − première ouverture`.
    pub fn span_ms(&self) -> Ts {
        self.data.window.end_ms - self.data.window.start_ms
    }

    /// Identifiant du run : le capital y entre (il n'est pas dans `config_hash`) et, en recherche,
    /// l'identité de recherche hachée (docs S11 §8.6).
    pub fn run_id(&self) -> String {
        if let Some(research) = &self.research {
            return run_id_s11(
                &self.config_hash,
                &self.dataset_hash,
                self.data.window.start_ms,
                self.data.window.end_ms,
                self.config.initial_cash,
                &research.identity_hash,
            );
        }
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
