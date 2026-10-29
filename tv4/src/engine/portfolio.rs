//! Moteur portefeuille multi-paires (docs S9 §2.3, §4, §5, §6, §7 et §11).
//!
//! [`run_portfolio`] est une fonction pure, comme [`run_cell`](super::run_cell) : une stratégie fraîche
//! par paire (construite par l'appelant), des colonnes déjà chargées par paire, un [`Book`] qui porte le
//! cash et le journal de trades communs, et aucune écriture (c'est le rôle de `report`).
//!
//! # Découpage en étages (docs S10 §9.3)
//!
//! `run_portfolio_observed` enchaîne quatre étages, sans changement observable (le golden `portfolio_2legs`
//! le prouve) : `check_config` (contrôles), `load` (bougies possédées), `share` (tout ce qui emprunte les
//! bougies et ne change pas d'un profil à l'autre : `Mtf`, colonnes d'indicateurs, série de régime,
//! départ de fenêtre, rang de liquidité) et `play_profile` (vues par jambe, curseur de régime neuf, boucle,
//! synthèse). `run_portfolio_profiles` appelle les trois premiers une fois et le dernier trois fois.
//!
//! # Déroulé d'une minute `t` de la grille (docs S9 §4.2)
//!
//! La grille est l'union triée des `open_time` des 1m de toutes les jambes de l'univers dans la fenêtre,
//! fusionnée par `k` pointeurs. Une jambe n'avance que sur ses propres bougies (« un trou = la paire
//! n'avance pas »). Soit `J(t)` les jambes qui ont une bougie en `t`, dans l'ordre du rang de
//! liquidité 90 j du jour UTC de `now_ms = t + 60 000` ([`RankTable`]).
//!
//! 1. **Trous de cotation** : toute jambe en position dont l'échéance `g0 + T` est atteinte sort de
//!    force ([`Book::force_exit_quote_gap`]), avant son `on_bar`.
//! 2. Pour chaque `k ∈ J(t)` : `Book::on_bar` (annulations, fills, stops).
//! 3. Equity du portefeuille, poussée une fois par minute de la grille.
//! 4. Pour chaque `k ∈ J(t)` dont un timeframe déclaré vient de clôturer : vues, `BarState`, appel de
//!    la stratégie de la jambe. **Toutes** les stratégies de la minute voient le même état : aucune
//!    soumission n'a lieu avant la fin de cette étape.
//! 5. Soumission des signaux, jambe par jambe dans l'ordre du rang, puis signal par signal dans l'ordre
//!    d'émission.
//!
//! # Départ de la fenêtre (docs S9 §6.3, point 10)
//!
//! Toutes les jambes démarrent à la même minute : la plus tardive des premières minutes jouées de
//! chaque jambe (convention de warmup de S5, par jambe) et, si une stratégie lit le régime, de la
//! première minute où la SMA200 de la paire de régime existe. L'activation d'une paire en cours de
//! fenêtre n'existe qu'en mode recherche (docs S11 §8.2, module [`research`](super::research)) : chaque
//! jambe y démarre à sa propre minute, et la grille n'est enregistrée qu'à partir de `decision_start`.
//!
//! # Sizing (docs S9 §7)
//!
//! * mode recherche : la base de `Size::RiskPct` est `initial_cash` et chaque entrée est plafonnée à
//!   `initial_cash / N` de notional ;
//! * mode capitalisé : la base est l'equity du portefeuille à la minute du signal, sans plafond.
//!
//! La base passe par `SubmitContext.equity`, le plafond par le 4e argument de [`Book::submit`].
//!
//! # Équivalence avec S8 (docs S9 §2.3)
//!
//! Un portefeuille à une jambe, en mode capitalisé, avec `max_positions = 1` et aucun régime, suit la
//! même séquence de calcul que `run_loop` : même grille (les minutes de la jambe), même equity, même
//! base de sizing. `tests/portfolio_equivalence.rs` le prouve à l'octet sur `trades.csv` et
//! `equity.csv`.
//!
//! # Écarts d'implémentation
//!
//! 1. Les entrées refusées par `max_positions` ne sont pas dans [`SignalRejections`] (dont les 17
//!    champs sont épinglés par S8) : elles ont leur compteur, [`LegResult::max_positions_rejected`].
//!    L'identité de l'entonnoir d'une jambe est donc `entry_signals = entry_orders +
//!    entry_rejections.total() + max_positions_rejected`.
//! 2. Le tampon des fractions de capital demandées par les entrées (`entry_fractions`, docs S9 §10.1
//!    étape 1) enregistre une entrée quand son sizing est valide et que l'ordre est accepté ou refusé
//!    pour `max_positions`, `InsufficientQuote` ou `BelowMinimum` : un refus pour `EntryWhileLong`,
//!    créneau ou prix invalide n'est pas une demande de capital.
//! 3. La paire de régime est lue même si elle n'est pas dans l'univers ; elle est alors absente de la
//!    grille, du rang et des rapports par paire.

use std::collections::BTreeMap;

use crate::model::costs::LegCostHashed;
use crate::model::{Candle, FeeSchedule, Side, Timeframe, Ts};
use crate::mtf::Mtf;
use crate::sim::{
    Book, BookConfig, BookRejection, CostCounters, SimCounters, SimDetail, SimError, SizingMode,
    SubmitContext, Trade, sizing,
};
use crate::store::Columns;
use crate::strategy::{
    BarContext, BarState, OrderKind, PendingOrder, Signal, Strategy, StrategyFunnel, TfView,
    validate_declaration,
};

use super::config::StrategyRecord;
use super::portfolio_config::{
    LIQUIDITY_WINDOW_DAYS, MAX_POSITIONS_LIMIT, PRIORITY_RULE, PortfolioConfig, PortfolioRunConfig,
    ResearchHashed,
};
use super::priority::RankTable;
use super::regime::{RegimeFeed, RegimeSeries};
use super::research::{
    LegOutcome, ResearchLegCounters, ResearchResult, ResearchSpec, ResearchState, build_result,
    check_research, decision_tf_of,
};
use super::run::{
    MINUTE_MS, SIGNALS_FLOOR, TRADES_INITIAL_CAPACITY, check_candles, indicator_column,
    to_pending_order, to_position,
};
use super::signals::{
    EngineFunnel, SignalClass, SignalRejections, StrategyFunnelCounts, classify, instruction,
    size_spec,
};
use super::{EngineError, LoopObserver, NoopObserver};

/// Entrées de [`run_portfolio`].
pub struct PortfolioInputs<'a> {
    /// Séries 1m du dataset, une par symbole : l'univers, et la paire de régime si elle n'y est pas.
    /// Typiquement `MultiDataset::series`.
    pub series: &'a [(String, Columns)],
    /// Une stratégie fraîche par symbole de l'univers, dans l'ordre de l'univers. Elles doivent avoir
    /// le même identifiant, la même version et les mêmes paramètres : le `config_hash` n'en décrit
    /// qu'une.
    pub strategies: Vec<Box<dyn Strategy>>,
    pub config: PortfolioConfig<'a>,
}

/// Résultat d'une jambe (une paire de l'univers).
#[derive(Clone, Debug)]
pub struct LegResult {
    pub symbol: String,
    /// Index de la première bougie jouée dans la série de la jambe.
    pub start_index: usize,
    /// Bougies de la jambe dans la fenêtre jouée.
    pub rows: usize,
    pub first_ms: Ts,
    pub last_ms: Ts,
    /// Entonnoir des signaux de la jambe.
    pub funnel: EngineFunnel,
    pub counters: SimCounters,
    pub detail: SimDetail,
    /// Entrées refusées par `max_positions` sur cette jambe.
    pub max_positions_rejected: u64,
    /// Fractions de capital demandées par les entrées (notional demandé / base), avant plafonds, dans
    /// l'ordre des signaux (docs S9 §10.1 étape 1).
    pub entry_fractions: Vec<f64>,
    /// Trades de la jambe.
    pub trades: u64,
}

/// Statistiques de la règle de priorité (bloc `priority` de `metrics.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriorityStats {
    pub rule: &'static str,
    pub liquidity_window_days: u32,
    /// Jours UTC de la table de rang.
    pub rank_days: usize,
    /// Jours de la fenêtre qui ont moins de 90 jours de données avant eux.
    pub partial_days: u64,
}

/// Statistiques du régime sur la fenêtre jouée (bloc `regime` de `metrics.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegimeStats {
    pub symbol: String,
    /// Instant (ms) où la SMA200 devient disponible : fin de la première 1m de la paire de régime à
    /// laquelle 200 bougies 1d sont closes.
    pub first_ready_ms: Ts,
    /// Jours de la fenêtre où la dernière 1d close du jour d'avant est au-dessus de sa SMA200 (`>`
    /// strict).
    pub days_bull: u64,
    /// Jours de la fenêtre où elle ne l'est pas.
    pub days_bear: u64,
}

/// Résultat d'un run portefeuille.
#[derive(Clone, Debug)]
pub struct PortfolioResult {
    /// Objet haché du `config_hash` (sans capital).
    pub config: PortfolioRunConfig,
    /// Hash blake3 hexadécimal de `config`.
    pub config_hash: String,
    /// Capital initial du run : hors du `config_hash`, à écrire à part.
    pub initial_cash: f64,
    pub sizing: SizingMode,
    pub max_positions: u16,
    pub quote_gap_ms: Ts,
    /// Univers, dans l'ordre de la configuration.
    pub universe: Vec<String>,
    pub legs: Vec<LegResult>,
    pub warmup_bars: usize,
    /// Première minute jouée (ms) : la plus tardive des premières minutes de chaque jambe.
    pub window_start_ms: Ts,
    /// Minutes de la grille.
    pub grid_minutes: usize,
    pub window_first_open_ms: Ts,
    pub window_last_open_ms: Ts,
    /// `open_time` de chaque minute de la grille, aligné sur `equity`.
    pub grid_open_ms: Vec<Ts>,
    /// Equity du portefeuille à la clôture de chaque minute de la grille, avant la clôture forcée.
    pub equity: Vec<f64>,
    /// Journal de trades de tout le portefeuille, dans l'ordre de génération.
    pub trades: Vec<Trade>,
    /// Indice (dans `universe`) de la jambe de chaque trade, aligné sur `trades`.
    pub trade_legs: Vec<u16>,
    pub cash_final: f64,
    /// Après clôture forcée : égale à `cash_final`.
    pub equity_final: f64,
    pub forced_close: bool,
    pub n_trades_forced: u64,
    pub pnl_net: f64,
    pub pnl_realized: f64,
    pub pnl_marked: f64,
    pub fees_total: f64,
    pub slippage_total: f64,
    pub signals_emitted: u64,
    pub signals_accepted: u64,
    /// Somme champ à champ des rejets de toutes les classes et de toutes les jambes (hors
    /// `max_positions`).
    pub rejections: SignalRejections,
    /// Entrées refusées par `max_positions`, toutes jambes confondues.
    pub max_positions_rejected: u64,
    /// Somme des compteurs du simulateur de toutes les jambes.
    pub sim: SimCounters,
    /// Somme des compteurs de détail de toutes les jambes.
    pub detail: SimDetail,
    /// Somme des entonnoirs de toutes les jambes.
    pub funnel: EngineFunnel,
    /// Minutes de la grille où au moins une jambe est en position.
    pub bars_in_position: u64,
    /// Plus grand nombre de jambes en position observé.
    pub max_open_positions: u16,
    /// Plus grand rapport « coût des positions ouvertes / capital initial » observé.
    pub max_cost_to_initial: f64,
    pub quote_gap_exits: u64,
    pub priority: PriorityStats,
    /// `None` si aucune stratégie ne lit le régime.
    pub regime: Option<RegimeStats>,
    /// Résultat de recherche (docs S11 §8.4), `None` hors mode recherche.
    pub research: Option<ResearchResult>,
}

/// Rejoue le portefeuille (voir [`run_portfolio_observed`]).
pub fn run_portfolio(inputs: PortfolioInputs<'_>) -> Result<PortfolioResult, EngineError> {
    run_portfolio_observed(inputs, &mut NoopObserver)
}

// ---------------------------------------------------------------------------------------------
// Préparation d'une jambe (hors boucle)
// ---------------------------------------------------------------------------------------------

/// Un indicateur calculé hors boucle : sa position dans les tableaux par timeframe déclaré.
struct Slot {
    name: &'static str,
    tf_index: usize,
    column_index: usize,
}

/// Colonnes d'indicateurs et déclaration d'une jambe, sans emprunt (les vues viennent après).
struct LegPrep {
    declared: Vec<Timeframe>,
    declared_mask: u8,
    names: Vec<Vec<&'static str>>,
    columns: Vec<Vec<Vec<f64>>>,
    slots: Vec<Slot>,
    warmup_bars: usize,
    /// Index de la première 1m jouée si la jambe était seule (convention de warmup de S5).
    start: usize,
}

/// Calcule les colonnes de la jambe et son index de départ, comme `run_prepared` de S8.
///
/// `tolerate_short` (mode recherche) : une jambe dont le warmup n'est jamais satisfait n'est pas une
/// erreur, son index de départ vaut alors `mtf.len()` (elle n'est jamais jouée).
fn prepare_leg(
    mtf: &Mtf<'_>,
    strategy: &dyn Strategy,
    tolerate_short: bool,
) -> Result<LegPrep, EngineError> {
    let n = mtf.len();
    let declared: Vec<Timeframe> = strategy.timeframes().to_vec();
    let mut declared_mask = 0u8;
    let mut tf_max = Timeframe::M1;
    let mut tf_candles: Vec<&[Candle]> = Vec::with_capacity(declared.len());
    let mut closed: Vec<&[usize]> = Vec::with_capacity(declared.len());
    for &tf in &declared {
        let (Some(series), Some(counts)) = (mtf.candles(tf), mtf.closed_counts(tf)) else {
            return Err(EngineError::MissingTimeframe { tf });
        };
        declared_mask |= tf.bit();
        tf_max = tf_max.max(tf);
        tf_candles.push(series);
        closed.push(counts);
    }
    let specs = strategy.indicators();
    let mut names: Vec<Vec<&'static str>> = vec![Vec::new(); declared.len()];
    let mut columns: Vec<Vec<Vec<f64>>> = vec![Vec::new(); declared.len()];
    let mut slots: Vec<Slot> = Vec::with_capacity(specs.len());
    for spec in &specs {
        let tf_index = declared
            .iter()
            .position(|tf| *tf == spec.tf)
            .ok_or(EngineError::MissingTimeframe { tf: spec.tf })?;
        let (Some(series), Some(tf_names), Some(tf_columns)) = (
            tf_candles.get(tf_index),
            names.get_mut(tf_index),
            columns.get_mut(tf_index),
        ) else {
            return Err(EngineError::MissingTimeframe { tf: spec.tf });
        };
        slots.push(Slot {
            name: spec.name,
            tf_index,
            column_index: tf_columns.len(),
        });
        tf_names.push(spec.name);
        tf_columns.push(indicator_column(spec.kind, series)?);
    }
    let warmup_bars = strategy.warmup_bars();
    let counts_max = declared
        .iter()
        .position(|tf| *tf == tf_max)
        .and_then(|k| closed.get(k).copied())
        .ok_or(EngineError::MissingTimeframe { tf: tf_max })?;
    let found = (0..n).find(|&i| {
        let before = if i == 0 {
            0
        } else {
            counts_max.get(i - 1).copied().unwrap_or(0)
        };
        before >= warmup_bars
    });
    let start = match found {
        Some(start) => start,
        None if tolerate_short => n,
        None => {
            return Err(EngineError::NotEnoughBars {
                warmup_bars,
                bars: n,
            });
        }
    };
    Ok(LegPrep {
        declared,
        declared_mask,
        names,
        columns,
        slots,
        warmup_bars,
        start,
    })
}

/// Refus de démarrer (S3 point 2 généralisé) à l'index `first` : pour chaque indicateur, la dernière
/// valeur visible à la première 1m jouée doit être finie.
fn check_start(mtf: &Mtf<'_>, prep: &LegPrep, first: usize) -> Result<(), EngineError> {
    for slot in &prep.slots {
        let visible = prep
            .declared
            .get(slot.tf_index)
            .and_then(|tf| mtf.closed_counts(*tf))
            .and_then(|counts| counts.get(first))
            .copied()
            .unwrap_or(0);
        let index = visible.saturating_sub(1);
        let value = prep
            .columns
            .get(slot.tf_index)
            .and_then(|tf_columns| tf_columns.get(slot.column_index))
            .filter(|_| visible > 0)
            .and_then(|column| column.get(index));
        if value.is_none_or(|v| !v.is_finite()) {
            return Err(EngineError::WarmupTooShort {
                name: slot.name,
                index,
            });
        }
    }
    Ok(())
}

/// Jambe en cours de rejeu : données empruntées, stratégie et compteurs.
struct LegRun<'a> {
    candles: &'a [Candle],
    mtf: &'a Mtf<'a>,
    strategy: Box<dyn Strategy>,
    uses_regime: bool,
    declared_mask: u8,
    /// Vues à `closed = 0`, dans l'ordre de `Strategy::timeframes`.
    base: Vec<TfView<'a>>,
    /// Vues de la bougie courante, réécrites sur place.
    current: Vec<TfView<'a>>,
    /// `closed_count` de chaque timeframe déclaré, même ordre que `base`.
    closed: Vec<&'a [usize]>,
    /// Index de la prochaine bougie à jouer.
    idx: usize,
    /// Vrai pendant la minute courante si la jambe a une bougie.
    present: bool,
    /// Vrai entre les étapes 4 et 5 si la stratégie a été appelée.
    evaluated: bool,
    signals: Vec<Signal>,
    funnel: EngineFunnel,
    entry_fractions: Vec<f64>,
    max_positions_rejected: u64,
    /// `g0 + T` : instant à partir duquel la jambe, si elle est en position, sort de force. `Ts::MAX`
    /// tant qu'elle n'a pas eu de minute active depuis sa dernière sortie.
    deadline: Ts,
    start_index: usize,
    rows: usize,
    /// Compteurs de recherche (docs S11 §8.3), nuls hors recherche.
    research: ResearchLegCounters,
}

/// Fusionne les pointeurs de lecture : plus petite `open_time` non jouée, `None` en fin de données.
fn next_open(runs: &[LegRun<'_>]) -> Option<Ts> {
    let mut next: Option<Ts> = None;
    for run in runs {
        if let Some(candle) = run.candles.get(run.idx) {
            next = Some(next.map_or(candle.open_time_ms, |t| t.min(candle.open_time_ms)));
        }
    }
    next
}

/// Nombre de minutes de la grille **enregistrée** (`open_time ≥ record_from`), par une fusion à blanc (les
/// tampons sont alloués à la bonne taille). `Ts::MIN` hors recherche : toute la grille.
fn count_grid(runs: &[LegRun<'_>], record_from: Ts) -> usize {
    let mut cursors: Vec<usize> = runs.iter().map(|run| run.idx).collect();
    let mut count = 0usize;
    loop {
        let mut next: Option<Ts> = None;
        for (run, &cursor) in runs.iter().zip(&cursors) {
            if let Some(candle) = run.candles.get(cursor) {
                next = Some(next.map_or(candle.open_time_ms, |t| t.min(candle.open_time_ms)));
            }
        }
        let Some(t) = next else {
            return count;
        };
        for (run, cursor) in runs.iter().zip(cursors.iter_mut()) {
            if run
                .candles
                .get(*cursor)
                .is_some_and(|candle| candle.open_time_ms == t)
            {
                *cursor += 1;
            }
        }
        if t >= record_from {
            count += 1;
        }
    }
}

/// Fraction de capital demandée par un signal d'entrée : `notional demandé / base`, où le notional est
/// la quantité de `size_qty` au prix de référence, avant tout plafond. `None` pour un signal qui n'est
/// pas une entrée ou dont le sizing est invalide.
fn entry_fraction(signal: &Signal, base: f64, ref_price: f64) -> Option<f64> {
    let (size, price, stop) = match *signal {
        Signal::EnterLong {
            size, kind, stop, ..
        } => {
            let price = match kind {
                OrderKind::Market => ref_price,
                OrderKind::Limit { price, .. } => price,
            };
            (size, price, stop)
        }
        Signal::PlaceLimit {
            side: Side::Buy,
            price,
            size,
            ..
        } => (size, price, None),
        _ => return None,
    };
    let qty = sizing::size_qty(size_spec(size), base, price, stop)?;
    let fraction = qty * price / base;
    (fraction.is_finite() && fraction > 0.0).then_some(fraction)
}

// ---------------------------------------------------------------------------------------------
// Sommes
// ---------------------------------------------------------------------------------------------

/// Somme champ à champ de compteurs. La déstructuration exhaustive empêche d'oublier un champ ajouté.
fn add_counters(total: &mut SimCounters, other: &SimCounters) {
    let SimCounters {
        fills,
        fills_market,
        fills_limit,
        stops_triggered,
        take_profits_triggered,
        entries_capped_by_cash,
        buys_rejected_insufficient_cash,
        buys_rejected_below_minimum,
        buys_rejected_volume_share,
        entries_invalid_risk_at_fill,
        entries_dropped_while_long,
        exits_dropped_flat,
        exits_over_volume_share,
        limits_marketable_at_arrival,
        limits_expired,
        orders_cancelled,
        sells_cancelled_by_exit,
        take_profits_ignored_fill_bar,
        stop_tp_same_bar,
        move_stops_applied,
        move_stops_dropped_flat,
        bars_zero_volume_blocked,
        orders_unfilled_at_end,
    } = *other;
    total.fills += fills;
    total.fills_market += fills_market;
    total.fills_limit += fills_limit;
    total.stops_triggered += stops_triggered;
    total.take_profits_triggered += take_profits_triggered;
    total.entries_capped_by_cash += entries_capped_by_cash;
    total.buys_rejected_insufficient_cash += buys_rejected_insufficient_cash;
    total.buys_rejected_below_minimum += buys_rejected_below_minimum;
    total.buys_rejected_volume_share += buys_rejected_volume_share;
    total.entries_invalid_risk_at_fill += entries_invalid_risk_at_fill;
    total.entries_dropped_while_long += entries_dropped_while_long;
    total.exits_dropped_flat += exits_dropped_flat;
    total.exits_over_volume_share += exits_over_volume_share;
    total.limits_marketable_at_arrival += limits_marketable_at_arrival;
    total.limits_expired += limits_expired;
    total.orders_cancelled += orders_cancelled;
    total.sells_cancelled_by_exit += sells_cancelled_by_exit;
    total.take_profits_ignored_fill_bar += take_profits_ignored_fill_bar;
    total.stop_tp_same_bar += stop_tp_same_bar;
    total.move_stops_applied += move_stops_applied;
    total.move_stops_dropped_flat += move_stops_dropped_flat;
    total.bars_zero_volume_blocked += bars_zero_volume_blocked;
    total.orders_unfilled_at_end += orders_unfilled_at_end;
}

/// Somme champ à champ de compteurs de détail (déstructuration exhaustive).
fn add_detail(total: &mut SimDetail, other: &SimDetail) {
    let SimDetail {
        below_min_zero_lot,
        below_min_min_qty,
        below_min_notional,
        below_min_exits,
        entries_capped_by_notional,
        entries_bumped_to_min,
        quote_gap_exits,
    } = *other;
    total.below_min_zero_lot += below_min_zero_lot;
    total.below_min_min_qty += below_min_min_qty;
    total.below_min_notional += below_min_notional;
    total.below_min_exits += below_min_exits;
    total.entries_capped_by_notional += entries_capped_by_notional;
    total.entries_bumped_to_min += entries_bumped_to_min;
    total.quote_gap_exits += quote_gap_exits;
}

/// Somme des entonnoirs des jambes. L'entonnoir de stratégie n'existe que si toutes les jambes en ont
/// un ; ses motifs sont sommés par nom, dans l'ordre de première apparition.
fn sum_funnels(legs: &[LegResult]) -> EngineFunnel {
    let mut total = EngineFunnel::default();
    let mut strategy: Option<StrategyFunnelCounts> = None;
    let mut all_have = !legs.is_empty();
    for leg in legs {
        let f = &leg.funnel;
        total.evaluations += f.evaluations;
        total.entry_signals += f.entry_signals;
        total.entry_orders += f.entry_orders;
        total.exit_signals += f.exit_signals;
        total.exit_orders += f.exit_orders;
        total.other_signals += f.other_signals;
        total.other_accepted += f.other_accepted;
        total.entry_rejections.add(&f.entry_rejections);
        total.other_rejections.add(&f.other_rejections);
        match &f.strategy {
            Some(counts) => {
                let sum = strategy.get_or_insert_with(StrategyFunnelCounts::default);
                sum.raw_entry_signals += counts.raw_entry_signals;
                for &(name, value) in &counts.filtered {
                    match sum.filtered.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, total_value)) => *total_value += value,
                        None => sum.filtered.push((name, value)),
                    }
                }
            }
            None => all_have = false,
        }
    }
    total.strategy = if all_have { strategy } else { None };
    total
}

// ---------------------------------------------------------------------------------------------
// Entrée principale
// ---------------------------------------------------------------------------------------------

/// Compteurs de la préparation partagée (docs S10 §9.4). Ils sont incrémentés par **l'appelant** de
/// `load` et de `share` ([`run_portfolio_counted`], [`run_portfolio_profiles`](super::run_portfolio_profiles))
/// juste après chaque appel, jamais par une fonction que chaque profil pourrait rappeler : `play_profile`
/// n'a accès à aucun de ces compteurs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrepStats {
    /// Séries de bougies converties depuis les colonnes (jambes, plus la paire de régime hors univers).
    pub candles_converted: usize,
    /// `Mtf` des jambes construits (celui de la série de régime est compté à part).
    pub mtf_builds: usize,
    /// Séries de régime (1d et SMA200) construites.
    pub regime_series_built: usize,
    /// Colonnes d'indicateurs calculées, tous timeframes et toutes jambes confondus.
    pub indicator_columns_computed: usize,
    /// Tables de rang de liquidité construites.
    pub rank_tables_built: usize,
}

impl PrepStats {
    /// Ajoute ce que viennent de produire un appel de [`load`] et un appel de [`share`].
    pub(super) fn record(&mut self, loaded: &Loaded, shared: &Shared<'_>) {
        self.candles_converted +=
            loaded.candle_store.len() + usize::from(!loaded.regime_store.is_empty());
        self.mtf_builds += shared.mtfs.len();
        self.regime_series_built += usize::from(shared.regime.is_some());
        self.indicator_columns_computed += shared
            .preps
            .iter()
            .flat_map(|prep| prep.columns.iter())
            .map(Vec::len)
            .sum::<usize>();
        self.rank_tables_built += 1;
    }
}

/// Ce que valident les contrôles de configuration et dont la suite a besoin.
pub(super) struct Checked {
    /// Au moins une stratégie lit le régime.
    pub(super) uses_regime: bool,
    pub(super) warmup_bars: usize,
}

/// Contrôles de configuration, hors boucle : univers, nombre de stratégies et d'instruments, bornes,
/// règles du simulateur, déclarations et identité des stratégies.
///
/// # Erreurs
///
/// Univers vide ou en double, nombre de stratégies ou d'instruments différent de celui de l'univers,
/// `max_positions` hors de 1 à 64, trou de cotation non positif, `min_trades` nul, règles invalides,
/// stratégies différentes.
pub(super) fn check_config(
    strategies: &[Box<dyn Strategy>],
    config: &PortfolioConfig<'_>,
) -> Result<Checked, EngineError> {
    let legs = config.universe.len();
    if legs == 0 {
        return Err(EngineError::EmptyUniverse);
    }
    if legs > usize::from(u16::MAX) {
        return Err(EngineError::TooManyLegs { legs });
    }
    for (i, symbol) in config.universe.iter().enumerate() {
        if config
            .universe
            .get(..i)
            .is_some_and(|before| before.contains(symbol))
        {
            return Err(EngineError::DuplicateSymbol {
                symbol: symbol.clone(),
            });
        }
    }
    if config.instruments.len() != legs {
        return Err(EngineError::InstrumentCountMismatch {
            instruments: config.instruments.len(),
            legs,
        });
    }
    if strategies.len() != legs {
        return Err(EngineError::StrategyCountMismatch {
            strategies: strategies.len(),
            legs,
        });
    }
    if !(1..=MAX_POSITIONS_LIMIT).contains(&config.max_positions) {
        return Err(EngineError::InvalidMaxPositions {
            max_positions: config.max_positions,
        });
    }
    if config.quote_gap_ms <= 0 {
        return Err(EngineError::InvalidQuoteGap {
            quote_gap_ms: config.quote_gap_ms,
        });
    }
    if config.min_trades == 0 {
        return Err(EngineError::InvalidMinTrades);
    }
    config.sim_rules.validate()?;
    for rules in config.instruments {
        rules.validate()?;
    }
    for strategy in strategies {
        validate_declaration(strategy.as_ref())?;
    }
    let record = strategies
        .first()
        .map(|s| StrategyRecord::of(s.as_ref()))
        .ok_or(EngineError::EmptyUniverse)?;
    if strategies
        .iter()
        .any(|s| StrategyRecord::of(s.as_ref()) != record)
    {
        return Err(EngineError::StrategiesDiffer);
    }
    let uses_regime = strategies.iter().any(|s| s.uses_regime());
    let warmup_bars = strategies
        .iter()
        .map(|s| s.warmup_bars())
        .max()
        .unwrap_or(0);
    Ok(Checked {
        uses_regime,
        warmup_bars,
    })
}

/// Bougies possédées : une série par jambe, plus la paire de régime si elle n'est pas dans l'univers.
/// Aucun emprunt : les `Mtf` de [`share`] les empruntent, dans l'étage suivant.
pub(super) struct Loaded {
    candle_store: Vec<Vec<Candle>>,
    /// Vide si aucune stratégie ne lit le régime ou si la paire de régime est dans l'univers.
    regime_store: Vec<Candle>,
    /// Indice de la paire de régime dans l'univers, si elle y est.
    regime_in_universe: Option<usize>,
}

impl Loaded {
    /// Plus grand nombre de bougies d'une jambe (capacité du journal de trades).
    pub(super) fn window_max(&self) -> usize {
        self.candle_store.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// D'où viennent les colonnes du chargement : empruntées (chemin hérité, copie des colonnes) ou possédées
/// (mode recherche, docs S11 §12.2 parade 1 : chaque série est libérée dès qu'elle est convertie, ce qui
/// retire une copie de la mémoire du run).
pub(super) enum SeriesSource<'a> {
    Borrowed(&'a [(String, Columns)]),
    Owned(Vec<(String, Columns)>),
}

/// Résultat de la recherche d'une série par symbole.
enum Fetched {
    Missing,
    Empty,
    Candles(Vec<Candle>),
}

impl SeriesSource<'_> {
    /// Convertit la série de `symbol` en bougies. Une source possédée la remplace par une série vide
    /// (le nom reste, la mémoire est rendue).
    fn fetch(&mut self, symbol: &str) -> Fetched {
        match self {
            SeriesSource::Borrowed(series) => {
                match series.iter().find(|(name, _)| name == symbol) {
                    None => Fetched::Missing,
                    Some((_, columns)) if columns.is_empty() => Fetched::Empty,
                    Some((_, columns)) => Fetched::Candles(columns.to_candles()),
                }
            }
            SeriesSource::Owned(series) => {
                match series.iter_mut().find(|(name, _)| name == symbol) {
                    None => Fetched::Missing,
                    Some((_, slot)) => {
                        let columns = std::mem::replace(slot, Columns::with_capacity(0));
                        if columns.is_empty() {
                            Fetched::Empty
                        } else {
                            Fetched::Candles(columns.to_candles())
                        }
                    }
                }
            }
        }
    }

    fn contains(&self, symbol: &str) -> bool {
        match self {
            SeriesSource::Borrowed(series) => series.iter().any(|(name, _)| name == symbol),
            SeriesSource::Owned(series) => series.iter().any(|(name, _)| name == symbol),
        }
    }
}

/// Convertit les colonnes du dataset en bougies et les contrôle (alignement, ordre).
///
/// # Erreurs
///
/// Symbole absent du dataset, série vide ou mal formée, série de régime absente.
pub(super) fn load(
    series: &[(String, Columns)],
    config: &PortfolioConfig<'_>,
    uses_regime: bool,
) -> Result<Loaded, EngineError> {
    load_from(SeriesSource::Borrowed(series), config, uses_regime)
}

/// Comme [`load`], depuis une source empruntée ou possédée.
pub(super) fn load_from(
    mut source: SeriesSource<'_>,
    config: &PortfolioConfig<'_>,
    uses_regime: bool,
) -> Result<Loaded, EngineError> {
    let legs = config.universe.len();
    let mut candle_store: Vec<Vec<Candle>> = Vec::with_capacity(legs);
    for symbol in config.universe {
        let candles = match source.fetch(symbol) {
            Fetched::Missing => {
                return Err(EngineError::SymbolNotInDataset {
                    symbol: symbol.clone(),
                });
            }
            Fetched::Empty => {
                return Err(EngineError::LegWithoutBars {
                    symbol: symbol.clone(),
                });
            }
            Fetched::Candles(candles) => candles,
        };
        check_candles(&candles)?;
        candle_store.push(candles);
    }
    let regime_in_universe = config
        .universe
        .iter()
        .position(|symbol| symbol == config.regime_symbol);
    let mut regime_store: Vec<Candle> = Vec::new();
    if uses_regime && regime_in_universe.is_none() {
        regime_store = match source.fetch(config.regime_symbol) {
            Fetched::Missing => {
                return Err(EngineError::RegimeSeriesMissing {
                    symbol: config.regime_symbol.to_string(),
                });
            }
            Fetched::Empty => Vec::new(),
            Fetched::Candles(candles) => candles,
        };
        check_candles(&regime_store)?;
    } else if uses_regime && !source.contains(config.regime_symbol) {
        return Err(EngineError::RegimeSeriesMissing {
            symbol: config.regime_symbol.to_string(),
        });
    }
    Ok(Loaded {
        candle_store,
        regime_store,
        regime_in_universe,
    })
}

/// Tout ce qui emprunte les bougies et ne change pas d'un profil à l'autre : `Mtf` de chaque jambe,
/// colonnes d'indicateurs, série de régime (immuable), départ commun de la fenêtre et table de rang.
/// Construit **une fois** par [`share`] ; chaque profil le lit par `&` ([`play_profile`]) et ne peut ni
/// le reconstruire ni le modifier.
pub(super) struct Shared<'a> {
    candle_store: &'a [Vec<Candle>],
    mtfs: Vec<Mtf<'a>>,
    preps: Vec<LegPrep>,
    regime: Option<RegimeSeries<'a>>,
    /// Première minute jouée : commune à toutes les jambes hors recherche, la plus précoce des jambes
    /// vivantes en recherche.
    window_start_ms: Ts,
    /// Première minute jouée de chaque jambe : `window_start_ms` pour toutes hors recherche ; en
    /// recherche `max(fin du warmup de la jambe, SMA200 du régime prête)`, `Ts::MAX` si la jambe ne
    /// devient jamais vivante (docs S11 §8.2).
    live_ms: Vec<Ts>,
    /// Fin de la première 1m de la paire de régime à laquelle la SMA200 existe.
    regime_ready_ms: Option<Ts>,
    ranks: RankTable,
    warmup_bars: usize,
}

/// Multi-timeframe, colonnes d'indicateurs et départ de chaque jambe, série de régime, départ commun de
/// la fenêtre et table de rang. Les colonnes sont calculées sur `strategies` ; les autres jeux de
/// stratégies d'un même appel (profils) doivent déclarer les mêmes indicateurs et timeframes.
///
/// # Erreurs
///
/// Timeframe ou indicateur invalide, warmup trop long, série de régime absente ou trop courte pour la
/// SMA200.
pub(super) fn share<'a>(
    loaded: &'a Loaded,
    strategies: &[Box<dyn Strategy>],
    config: &PortfolioConfig<'_>,
    checked: &Checked,
) -> Result<Shared<'a>, EngineError> {
    share_impl(loaded, strategies, config, checked, false)
}

/// Comme [`share`] pour le mode recherche (docs S11 §8.2) : chaque jambe a sa propre minute de départ.
///
/// # Erreurs
///
/// Celles de [`share`], sauf qu'une jambe au warmup jamais satisfait n'en est pas une ; en revanche
/// [`EngineError::ResearchNoLiveLeg`] si aucune jambe ne devient vivante.
pub(super) fn share_research<'a>(
    loaded: &'a Loaded,
    strategies: &[Box<dyn Strategy>],
    config: &PortfolioConfig<'_>,
    checked: &Checked,
) -> Result<Shared<'a>, EngineError> {
    share_impl(loaded, strategies, config, checked, true)
}

fn share_impl<'a>(
    loaded: &'a Loaded,
    strategies: &[Box<dyn Strategy>],
    config: &PortfolioConfig<'_>,
    checked: &Checked,
    research: bool,
) -> Result<Shared<'a>, EngineError> {
    let legs = config.universe.len();
    let uses_regime = checked.uses_regime;
    let mut mtfs: Vec<Mtf<'a>> = Vec::with_capacity(legs);
    for (candles, strategy) in loaded.candle_store.iter().zip(strategies) {
        mtfs.push(Mtf::build(candles, strategy.timeframes())?);
    }
    let mut preps: Vec<LegPrep> = Vec::with_capacity(legs);
    for (mtf, strategy) in mtfs.iter().zip(strategies) {
        preps.push(prepare_leg(mtf, strategy.as_ref(), research)?);
    }
    let regime_m1: &'a [Candle] = match loaded.regime_in_universe {
        Some(k) if uses_regime => loaded.candle_store.get(k).map_or(&[], Vec::as_slice),
        _ => &loaded.regime_store,
    };
    let regime: Option<RegimeSeries<'a>> = if uses_regime {
        Some(RegimeSeries::new(regime_m1)?)
    } else {
        None
    };
    let mut live_ms: Vec<Ts> = Vec::with_capacity(legs);
    let mut window_start_ms = Ts::MIN;
    let mut regime_ready_ms: Option<Ts> = None;
    if research {
        // Activation par jambe : `live_k = max(fin du warmup de k, prêt du régime)`, la grille commence à
        // la plus précoce.
        let mut ready_open = Ts::MIN;
        if let Some(series) = regime.as_ref() {
            let ready =
                series
                    .first_ready_open_ms()
                    .ok_or_else(|| EngineError::RegimeNotReady {
                        symbol: config.regime_symbol.to_string(),
                    })?;
            regime_ready_ms = Some(ready.saturating_add(MINUTE_MS));
            ready_open = ready;
        }
        for (candles, prep) in loaded.candle_store.iter().zip(&preps) {
            live_ms.push(
                candles
                    .get(prep.start)
                    .map_or(Ts::MAX, |first| first.open_time_ms.max(ready_open)),
            );
        }
        window_start_ms = live_ms
            .iter()
            .copied()
            .filter(|&live| live != Ts::MAX)
            .min()
            .ok_or(EngineError::ResearchNoLiveLeg)?;
    } else {
        // Départ commun : la plus tardive des premières minutes de chaque jambe et de la SMA200 du
        // régime.
        for ((symbol, candles), prep) in
            config.universe.iter().zip(&loaded.candle_store).zip(&preps)
        {
            let first = candles
                .get(prep.start)
                .ok_or_else(|| EngineError::LegNotEnoughBars {
                    symbol: symbol.clone(),
                    warmup_bars: prep.warmup_bars,
                    bars: candles.len(),
                })?;
            window_start_ms = window_start_ms.max(first.open_time_ms);
        }
        if let Some(series) = regime.as_ref() {
            let ready =
                series
                    .first_ready_open_ms()
                    .ok_or_else(|| EngineError::RegimeNotReady {
                        symbol: config.regime_symbol.to_string(),
                    })?;
            regime_ready_ms = Some(ready.saturating_add(MINUTE_MS));
            window_start_ms = window_start_ms.max(ready);
        }
        live_ms.resize(legs, window_start_ms);
    }
    let rank_inputs: Vec<&[Candle]> = loaded.candle_store.iter().map(Vec::as_slice).collect();
    let ranks = RankTable::build(&rank_inputs);
    Ok(Shared {
        candle_store: &loaded.candle_store,
        mtfs,
        preps,
        regime,
        window_start_ms,
        live_ms,
        regime_ready_ms,
        ranks,
        warmup_bars: checked.warmup_bars,
    })
}

/// Comme [`run_portfolio`], avec un observateur appelé juste avant et juste après la boucle de la grille.
///
/// # Erreurs
///
/// Configuration invalide (univers vide ou en double, nombre de stratégies ou d'instruments différent
/// de celui de l'univers, `max_positions` hors de 1 à 64, trou de cotation non positif, stratégies
/// différentes), symbole absent du dataset, série vide ou mal formée, warmup trop long, série de régime
/// absente ou trop courte pour la SMA200, et toute erreur de `sim` ou de `strategy`.
pub fn run_portfolio_observed(
    inputs: PortfolioInputs<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<PortfolioResult, EngineError> {
    run_portfolio_counted(inputs, observer).map(|(result, _)| result)
}

/// Comme [`run_portfolio_observed`], avec les compteurs de la préparation ([`PrepStats`]). Sert de
/// référence au test de la préparation partagée : un run isolé compte ce que `n` profils ne doivent pas
/// recompter.
///
/// # Erreurs
///
/// Celles de [`run_portfolio_observed`].
pub fn run_portfolio_counted(
    inputs: PortfolioInputs<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<(PortfolioResult, PrepStats), EngineError> {
    let PortfolioInputs {
        series,
        strategies,
        config,
    } = inputs;
    let checked = check_config(&strategies, &config)?;
    let loaded = load(series, &config, checked.uses_regime)?;
    let shared = share(&loaded, &strategies, &config, &checked)?;
    let mut prep = PrepStats::default();
    prep.record(&loaded, &shared);

    // --- Config hachée (sans capital) et simulateur partagé ---
    let run_config = PortfolioRunConfig::new(
        strategies
            .first()
            .map(|s| s.as_ref())
            .ok_or(EngineError::EmptyUniverse)?,
        &config,
        checked.warmup_bars,
        checked.uses_regime,
    );
    let config_hash = run_config.hash()?;
    let book = Book::new(
        book_config(&config, loaded.window_max()),
        config.instruments,
    )?;
    let (result, _) = play_profile(
        &shared,
        strategies,
        book,
        &config,
        run_config,
        config_hash,
        None,
        observer,
    )?;
    Ok((result, prep))
}

/// Rejoue le portefeuille en **mode recherche** (docs S11 §8) : activation par jambe, grille enregistrée
/// à partir de `decision_start`, filtre des entrées, annulation des entrées d'une jambe qui sort de
/// l'univers. Le `config_hash` est celui de la configuration gelée (bloc `research` haché, tables par
/// jambe vidées) ; l'identité de recherche est dans `PortfolioResult::research`.
///
/// # Erreurs
///
/// Celles de [`run_portfolio`] (hors départ commun : une jambe au warmup jamais satisfait n'est pas une
/// erreur), plus [`EngineError::ResearchNeedsZeroVolumeRule`], [`EngineError::ResearchWindow`],
/// [`EngineError::UniverseQuarterMissing`], [`EngineError::ResearchNoLiveLeg`] et
/// [`EngineError::DecisionTfNotDeclared`].
pub fn run_portfolio_research(
    inputs: PortfolioInputs<'_>,
    research: &ResearchSpec<'_>,
) -> Result<PortfolioResult, EngineError> {
    run_portfolio_research_counted(inputs, research, &mut NoopObserver).map(|(result, _)| result)
}

/// Comme [`run_portfolio_research`], avec un observateur appelé juste avant et juste après la boucle et
/// les compteurs de la préparation. Sert au test d'allocation du mode recherche.
///
/// # Erreurs
///
/// Celles de [`run_portfolio_research`].
pub fn run_portfolio_research_counted(
    inputs: PortfolioInputs<'_>,
    research: &ResearchSpec<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<(PortfolioResult, PrepStats), EngineError> {
    let PortfolioInputs {
        series,
        strategies,
        config,
    } = inputs;
    research_core(
        SeriesSource::Borrowed(series),
        strategies,
        config,
        research,
        observer,
    )
}

/// Entrées de [`run_portfolio_research_owned`] : comme [`PortfolioInputs`], mais les séries sont
/// **possédées**.
pub struct OwnedPortfolioInputs<'a> {
    pub series: Vec<(String, Columns)>,
    pub strategies: Vec<Box<dyn Strategy>>,
    pub config: PortfolioConfig<'a>,
}

/// Comme [`run_portfolio_research`], en consommant les colonnes (docs S11 §12.2, parade 1) : chaque série
/// est convertie en bougies puis libérée, si bien que le dataset et les bougies du moteur ne vivent plus
/// ensemble. Le résultat est identique au bit à celui de [`run_portfolio_research`].
///
/// # Erreurs
///
/// Celles de [`run_portfolio_research`].
pub fn run_portfolio_research_owned(
    inputs: OwnedPortfolioInputs<'_>,
    research: &ResearchSpec<'_>,
) -> Result<PortfolioResult, EngineError> {
    let OwnedPortfolioInputs {
        series,
        strategies,
        config,
    } = inputs;
    research_core(
        SeriesSource::Owned(series),
        strategies,
        config,
        research,
        &mut NoopObserver,
    )
    .map(|(result, _)| result)
}

fn research_core(
    source: SeriesSource<'_>,
    strategies: Vec<Box<dyn Strategy>>,
    config: PortfolioConfig<'_>,
    research: &ResearchSpec<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<(PortfolioResult, PrepStats), EngineError> {
    let checked = check_config(&strategies, &config)?;
    check_research(research, &config)?;
    let decision_tf = decision_tf_of(&strategies)?;
    let loaded = load_from(source, &config, checked.uses_regime)?;
    let shared = share_research(&loaded, &strategies, &config, &checked)?;
    let mut prep = PrepStats::default();
    prep.record(&loaded, &shared);

    let run_config = PortfolioRunConfig::new(
        strategies
            .first()
            .map(|s| s.as_ref())
            .ok_or(EngineError::EmptyUniverse)?,
        &config,
        checked.warmup_bars,
        checked.uses_regime,
    )
    .into_research(ResearchHashed::new(
        research.universe_spec.clone(),
        decision_tf,
    ));
    let config_hash = run_config.hash()?;
    let book = Book::new(
        book_config(&config, loaded.window_max()),
        config.instruments,
    )?;
    let run = ResearchRun {
        spec: research,
        decision_tf,
        costs_legs: BTreeMap::new(),
    };
    let (result, _) = play_profile(
        &shared,
        strategies,
        book,
        &config,
        run_config,
        config_hash,
        Some(&run),
        observer,
    )?;
    Ok((result, prep))
}

/// Ce que `play_profile` lit d'un run de recherche : la spécification, le TF de décision et les valeurs de
/// coûts résolues par jambe (vides hors profils), que l'identité de recherche hache.
pub(super) struct ResearchRun<'a> {
    pub(super) spec: &'a ResearchSpec<'a>,
    pub(super) decision_tf: Timeframe,
    pub(super) costs_legs: BTreeMap<String, LegCostHashed>,
}

/// Paramètres du [`Book`] d'un profil, communs aux chemins hérité et s10.
pub(super) fn book_config(config: &PortfolioConfig<'_>, window_max: usize) -> BookConfig {
    BookConfig {
        fees: config.fees,
        slippage: config.slippage,
        rules: config.sim_rules,
        initial_cash: config.initial_cash,
        max_positions: config.max_positions,
        mode: config.sizing,
        min_order_bump_max_risk: config.min_order_bump_max_risk,
        trades_capacity: (window_max / 2 + 1).min(TRADES_INITIAL_CAPACITY),
    }
}

/// Joue un profil sur la préparation partagée : vues par jambe (coût O(jambes × TF), sans agrégation),
/// curseur de régime **neuf**, tampons, boucle de la grille, puis synthèse. `shared` est prêté en lecture
/// seule : ni les bougies possédées ni de quoi appeler [`share`] ne sont ici, donc un `Mtf` ne peut pas
/// être reconstruit par profil (docs S10 §9.4).
///
/// Rend le résultat et les compteurs de coûts de chaque jambe (nuls pour un coût hérité).
///
/// `research` : `None` hors recherche (chemin hérité, inchangé au bit), `Some` en mode recherche (docs
/// S11 §8).
#[allow(clippy::too_many_arguments)]
pub(super) fn play_profile(
    shared: &Shared<'_>,
    strategies: Vec<Box<dyn Strategy>>,
    mut book: Book,
    config: &PortfolioConfig<'_>,
    run_config: PortfolioRunConfig,
    config_hash: String,
    research: Option<&ResearchRun<'_>>,
    observer: &mut dyn LoopObserver,
) -> Result<(PortfolioResult, Vec<CostCounters>), EngineError> {
    let legs = config.universe.len();
    let candle_store = shared.candle_store;
    let ranks = &shared.ranks;
    let window_start_ms = shared.window_start_ms;
    let mut feed: Option<RegimeFeed<'_>> = shared.regime.as_ref().map(RegimeFeed::new);

    // --- Vues par jambe (emprunts de `shared`) ---
    let column_refs: Vec<Vec<Vec<&[f64]>>> = shared
        .preps
        .iter()
        .map(|prep| {
            prep.columns
                .iter()
                .map(|tf_columns| tf_columns.iter().map(Vec::as_slice).collect())
                .collect()
        })
        .collect();
    let max_pending = config.sim_rules.max_pending;
    let mut runs: Vec<LegRun<'_>> = Vec::with_capacity(legs);
    for (k, strategy) in strategies.into_iter().enumerate() {
        let (Some(candles), Some(mtf), Some(prep), Some(refs), Some(symbol)) = (
            candle_store.get(k),
            shared.mtfs.get(k),
            shared.preps.get(k),
            column_refs.get(k),
            config.universe.get(k),
        ) else {
            return Err(EngineError::EmptyUniverse);
        };
        let live_ms = shared.live_ms.get(k).copied().unwrap_or(window_start_ms);
        let first = candles.partition_point(|c| c.open_time_ms < live_ms);
        if first >= candles.len() {
            // En recherche, une jambe qui ne devient jamais vivante n'est jamais jouée (docs S11 §8.2).
            if research.is_none() {
                return Err(EngineError::LegNotEnoughBars {
                    symbol: symbol.clone(),
                    warmup_bars: prep.warmup_bars,
                    bars: candles.len(),
                });
            }
        } else {
            check_start(mtf, prep, first)?;
        }
        let mut base: Vec<TfView<'_>> = Vec::with_capacity(prep.declared.len());
        let mut closed: Vec<&[usize]> = Vec::with_capacity(prep.declared.len());
        for (j, &tf) in prep.declared.iter().enumerate() {
            let (Some(tf_candles), Some(counts), Some(tf_names), Some(tf_refs)) = (
                mtf.candles(tf),
                mtf.closed_counts(tf),
                prep.names.get(j),
                refs.get(j),
            ) else {
                return Err(EngineError::MissingTimeframe { tf });
            };
            base.push(TfView::new(tf, tf_candles, tf_names, tf_refs)?);
            closed.push(counts);
        }
        let rows = candles.len() - first;
        runs.push(LegRun {
            candles,
            mtf,
            uses_regime: strategy.uses_regime(),
            strategy,
            declared_mask: prep.declared_mask,
            current: base.clone(),
            base,
            closed,
            idx: first,
            present: false,
            evaluated: false,
            signals: Vec::with_capacity(2 * SIGNALS_FLOOR.max(max_pending)),
            funnel: EngineFunnel::default(),
            entry_fractions: Vec::with_capacity((rows / 2 + 1).min(TRADES_INITIAL_CAPACITY)),
            max_positions_rejected: 0,
            deadline: Ts::MAX,
            start_index: first,
            rows,
            research: ResearchLegCounters::default(),
        });
    }

    // --- État de recherche : éligibilité de chaque jambe, construite avant la boucle ---
    let mut state: Option<ResearchState> =
        research.map(|r| ResearchState::new(r.spec, config.universe, r.decision_tf));
    let record_from = state
        .as_ref()
        .map_or(Ts::MIN, ResearchState::decision_start_ms);

    // --- Tampons de la boucle, tous préalloués ---
    let grid_minutes = count_grid(&runs, record_from);
    let mut buffers = PlayBuffers {
        grid: Vec::with_capacity(grid_minutes),
        equity: Vec::with_capacity(grid_minutes),
        pending: Vec::with_capacity(max_pending),
        emitted: 0,
        accepted: 0,
        bars_in_position: 0,
        last_open_ms: None,
    };
    let max_notional = match config.sizing {
        SizingMode::Research => Some(config.initial_cash / f64::from(config.max_positions)),
        SizingMode::Compounding => None,
    };

    // --- Boucle : 0 allocation attendue (tolérance documentée : 1 pour 10 000 minutes) ---
    observer.loop_started();
    let played = play(
        &mut book,
        &mut runs,
        ranks,
        feed.as_mut(),
        config,
        max_notional,
        &mut buffers,
        state.as_mut(),
    );
    observer.loop_finished();
    played?;

    // --- Fin de run, hors boucle ---
    let (Some(&window_first_open_ms), Some(&window_last_open_ms)) =
        (buffers.grid.first(), buffers.grid.last())
    else {
        return Err(EngineError::EmptyData);
    };
    let last_per_leg: Vec<Option<Candle>> =
        runs.iter().map(|r| r.candles.last().copied()).collect();
    book.close_all_at_end(&last_per_leg)?;
    let outcome = book.into_outcome();

    let mut totals = Totals::default();
    for trade in &outcome.trades {
        totals.fees += trade.entry_fee;
        totals.fees += trade.exit_fee;
        totals.slippage += trade.entry_slippage;
        totals.slippage += trade.exit_slippage;
        if trade.forced_close {
            totals.marked += trade.pnl_net;
            totals.forced += 1;
        } else {
            totals.realized += trade.pnl_net;
        }
    }
    let mut trades_by_leg = vec![0u64; legs];
    for &leg in &outcome.trade_legs {
        if let Some(count) = trades_by_leg.get_mut(usize::from(leg)) {
            *count += 1;
        }
    }
    let mut leg_results: Vec<LegResult> = Vec::with_capacity(legs);
    let mut research_legs: Vec<LegOutcome<'_>> = Vec::with_capacity(legs);
    for (k, mut run) in runs.into_iter().enumerate() {
        run.funnel.strategy = run
            .strategy
            .funnel()
            .map(|f: StrategyFunnel| StrategyFunnelCounts {
                raw_entry_signals: f.raw_entry_signals,
                filtered: f.filtered,
            });
        let (Some(counters), Some(detail), Some(symbol)) = (
            outcome.counters.get(k),
            outcome.details.get(k),
            config.universe.get(k),
        ) else {
            return Err(EngineError::EmptyUniverse);
        };
        let last_ms = run
            .candles
            .last()
            .map_or(window_last_open_ms, |c| c.open_time_ms);
        let first_ms = run
            .candles
            .get(run.start_index)
            .map_or(window_first_open_ms, |c| c.open_time_ms);
        if research.is_some() {
            research_legs.push(LegOutcome {
                symbol,
                live_from_ms: run.candles.get(run.start_index).map(|c| c.open_time_ms),
                counters: run.research,
            });
        }
        leg_results.push(LegResult {
            symbol: symbol.clone(),
            start_index: run.start_index,
            rows: run.rows,
            first_ms,
            last_ms,
            funnel: run.funnel,
            counters: *counters,
            detail: *detail,
            max_positions_rejected: run.max_positions_rejected,
            entry_fractions: run.entry_fractions,
            trades: trades_by_leg.get(k).copied().unwrap_or(0),
        });
    }
    let mut sim = SimCounters::default();
    let mut detail = SimDetail::default();
    let mut rejections = SignalRejections::default();
    let mut max_positions_rejected = 0u64;
    for leg in &leg_results {
        add_counters(&mut sim, &leg.counters);
        add_detail(&mut detail, &leg.detail);
        rejections.add(&leg.funnel.entry_rejections);
        rejections.add(&leg.funnel.other_rejections);
        max_positions_rejected += leg.max_positions_rejected;
    }
    let funnel = sum_funnels(&leg_results);
    let regime = match (shared.regime.as_ref(), shared.regime_ready_ms) {
        (Some(series), Some(first_ready_ms)) => {
            let (days_bull, days_bear) =
                series.day_counts(window_first_open_ms, window_last_open_ms);
            Some(RegimeStats {
                symbol: config.regime_symbol.to_string(),
                first_ready_ms,
                days_bull,
                days_bear,
            })
        }
        _ => None,
    };

    // Recherche : la fenêtre jouée commence à la plus précoce des jambes vivantes, et aucune entrée n'a pu
    // être décidée avant `decision_start` (docs S11 §3.4, côté gauche de la purge).
    let mut window_start_ms = window_start_ms;
    let research_result = match (research, state.as_ref()) {
        (Some(run), Some(state)) => {
            debug_assert!(
                outcome
                    .trades
                    .iter()
                    .all(|trade| trade.entry_signal_ms >= state.decision_start_ms()),
                "mode recherche : une entrée a été décidée avant decision_start"
            );
            window_start_ms = leg_results
                .iter()
                .filter(|leg| leg.rows > 0)
                .map(|leg| leg.first_ms)
                .min()
                .unwrap_or(window_start_ms);
            Some(build_result(
                run.spec,
                config,
                run.decision_tf,
                &run.costs_legs,
                shared.regime.is_some(),
                &research_legs,
            )?)
        }
        _ => None,
    };

    let cost_by_leg = outcome.cost_by_leg;
    let result = PortfolioResult {
        config: run_config,
        config_hash,
        initial_cash: config.initial_cash,
        sizing: config.sizing,
        max_positions: config.max_positions,
        quote_gap_ms: config.quote_gap_ms,
        universe: config.universe.to_vec(),
        legs: leg_results,
        warmup_bars: shared.warmup_bars,
        window_start_ms,
        grid_minutes,
        window_first_open_ms,
        window_last_open_ms,
        grid_open_ms: buffers.grid,
        equity: buffers.equity,
        trades: outcome.trades,
        trade_legs: outcome.trade_legs,
        cash_final: outcome.cash,
        equity_final: outcome.cash,
        forced_close: outcome.forced_close,
        n_trades_forced: totals.forced,
        pnl_net: outcome.cash - config.initial_cash,
        pnl_realized: totals.realized,
        pnl_marked: totals.marked,
        fees_total: totals.fees,
        slippage_total: totals.slippage,
        signals_emitted: buffers.emitted,
        signals_accepted: buffers.accepted,
        rejections,
        max_positions_rejected,
        sim,
        quote_gap_exits: detail.quote_gap_exits,
        detail,
        funnel,
        bars_in_position: buffers.bars_in_position,
        max_open_positions: outcome.max_open_positions,
        max_cost_to_initial: outcome.max_cost_to_initial,
        priority: PriorityStats {
            rule: PRIORITY_RULE,
            liquidity_window_days: LIQUIDITY_WINDOW_DAYS,
            rank_days: ranks.days(),
            partial_days: ranks.partial_days(window_first_open_ms, window_last_open_ms),
        },
        regime,
        research: research_result,
    };
    Ok((result, cost_by_leg))
}

/// Totaux accumulés dans l'ordre des trades (boucles `for`, jamais `.sum()`, docs/03 §7.4).
#[derive(Default)]
struct Totals {
    fees: f64,
    slippage: f64,
    realized: f64,
    marked: f64,
    forced: u64,
}

/// Tampons et compteurs de la boucle, tous préalloués avant elle.
struct PlayBuffers {
    grid: Vec<Ts>,
    equity: Vec<f64>,
    /// Vue des ordres en attente donnée à la stratégie, réécrite à chaque appel.
    pending: Vec<PendingOrder>,
    emitted: u64,
    accepted: u64,
    bars_in_position: u64,
    /// `open_time` de la dernière minute de la grille jouée, enregistrée ou non (en recherche, les
    /// minutes d'avant `decision_start` sont jouées sans être enregistrées).
    last_open_ms: Option<Ts>,
}

// ---------------------------------------------------------------------------------------------
// Boucle de la grille
// ---------------------------------------------------------------------------------------------

/// Traite les jambes dont l'échéance `g0 + T` est atteinte à `at_ms`, dans l'ordre du rang : sortie forcée
/// de la jambe en position, annulation des entrées en attente de la jambe à plat. Remet l'échéance à
/// `Ts::MAX` : la jambe n'en a plus tant qu'elle n'a pas eu de minute active.
fn force_quote_gaps(
    book: &mut Book,
    runs: &mut [LegRun<'_>],
    order: &[u16],
    at_ms: Ts,
) -> Result<(), SimError> {
    for &k in order {
        let k = usize::from(k);
        let Some(run) = runs.get_mut(k) else {
            continue;
        };
        if at_ms >= run.deadline {
            if book.position(k).is_some() {
                book.force_exit_quote_gap(k, run.deadline)?;
            } else {
                // Jambe à plat : ses entrées en attente ne peuvent plus remplir dans le trou (un achat
                // market n'expire pas), elles sont annulées pour libérer leur place et leur réservation.
                book.cancel_entries_quote_gap(k)?;
            }
            run.deadline = Ts::MAX;
        }
    }
    Ok(())
}

/// Étape 1 du mode recherche (docs S11 §8.3), après les trous de cotation et avant tout `on_bar` : toute
/// jambe qui vient de sortir de l'univers (ou dont le TF de décision vient d'être exclu par la qualité)
/// voit ses entrées en attente (`Enter` et limits d'achat) annulées, en position comme à plat, pour
/// toutes les jambes dans l'ordre du rang, qu'elles aient ou non une bougie à la minute `t`. Les ventes,
/// les sorties et le stop ne sont pas touchés : une jambe en position va à son terme.
fn cancel_left_universe(
    book: &mut Book,
    runs: &mut [LegRun<'_>],
    order: &[u16],
    state: &mut ResearchState,
    t: Ts,
) -> Result<(), SimError> {
    for &k in order {
        let k = usize::from(k);
        if state.leaves_at(k, t)
            && let Some(run) = runs.get_mut(k)
        {
            run.research.entries_cancelled_universe += book.cancel_entries_quote_gap(k)?;
        }
    }
    Ok(())
}

/// Boucle des étapes de docs S9 §4.2 sur la grille des minutes. `research` : état du mode recherche
/// (docs S11 §8), `None` hors recherche.
#[allow(clippy::too_many_arguments)]
fn play(
    book: &mut Book,
    runs: &mut [LegRun<'_>],
    ranks: &RankTable,
    mut feed: Option<&mut RegimeFeed<'_>>,
    config: &PortfolioConfig<'_>,
    max_notional: Option<f64>,
    buffers: &mut PlayBuffers,
    mut research: Option<&mut ResearchState>,
) -> Result<(), EngineError> {
    let legs = runs.len();
    let fees: FeeSchedule = config.fees;
    let record_from = research
        .as_deref()
        .map_or(Ts::MIN, ResearchState::decision_start_ms);
    if let Some(state) = research.as_deref_mut()
        && let Some(first) = next_open(runs)
    {
        // Éligibilité initiale : rien à annuler, aucun ordre n'existe encore.
        state.init_at(first);
    }
    while let Some(t) = next_open(runs) {
        let now_ms = t.saturating_add(MINUTE_MS);
        let order = ranks.order_at(now_ms);

        // 1. Trous de cotation, avant tout `on_bar`.
        force_quote_gaps(book, runs, order, t)?;
        // 1 bis (recherche). Sortie de l'univers : annulation des entrées en attente des jambes à plat.
        if let Some(state) = research.as_deref_mut() {
            cancel_left_universe(book, runs, order, state, t)?;
        }

        // 2a. Toutes les jambes qui ont des ordres en attente appliquent leurs annulations et expirations
        // échues à `t`, sans fill, dans l'ordre du rang et avant tout `on_bar`. Une échéance est une heure
        // d'horloge, pas un nombre de bougies de la jambe : une limit échue ne doit garder ni sa place de
        // `max_positions` ni son quote réservé, et toutes les échéances de la minute passent avant les fills
        // pour qu'aucune jambe ne soit plafonnée par la réservation d'un ordre mort d'une autre. Le résultat
        // propre de chaque jambe ne change pas : son `on_bar` commence par les mêmes échéances, avec les
        // mêmes compteurs, et n'y trouve plus rien à retirer.
        for &k in order {
            let k = usize::from(k);
            if book.pending_len(k) > 0 {
                book.expire_due(k, t)?;
            }
        }

        // 2. Annulations, expirations, fills et stops de chaque 1m présente, dans l'ordre du rang.
        for &k in order {
            let k = usize::from(k);
            let Some(run) = runs.get_mut(k) else {
                continue;
            };
            let Some(candle) = run.candles.get(run.idx) else {
                continue;
            };
            if candle.open_time_ms != t {
                continue;
            }
            book.on_bar(k, candle)?;
            if candle.volume > 0.0 {
                run.deadline = now_ms.saturating_add(config.quote_gap_ms);
            }
            run.present = true;
        }
        let recorded = t >= record_from;
        if recorded && (0..legs).any(|k| book.position(k).is_some()) {
            buffers.bars_in_position += 1;
        }

        // 3. Equity du portefeuille, une fois par minute de la grille. En recherche, les minutes d'avant
        // `decision_start` sont jouées sans être enregistrées (docs S11 §8.2).
        let equity_i = book.equity();
        if recorded {
            buffers.grid.push(t);
            buffers.equity.push(equity_i);
        }
        buffers.last_open_ms = Some(t);
        book.check_invariants();
        let base = match config.sizing {
            SizingMode::Research => config.initial_cash,
            SizingMode::Compounding => equity_i,
        };

        // 4. Stratégies : toutes voient le même état, aucune soumission avant la fin de l'étape.
        for &k in order {
            let k = usize::from(k);
            let Some(run) = runs.get_mut(k) else {
                continue;
            };
            if !run.present {
                continue;
            }
            let i = run.idx;
            let mask = run.mtf.just_closed(i) & run.declared_mask;
            if mask == 0 {
                continue;
            }
            for (j, current) in run.current.iter_mut().enumerate() {
                let (Some(base_view), Some(counts)) = (run.base.get(j), run.closed.get(j)) else {
                    return Err(EngineError::MissingClosedCount { index: i });
                };
                let count = counts
                    .get(i)
                    .copied()
                    .ok_or(EngineError::MissingClosedCount { index: i })?;
                *current = base_view.with_closed(count)?;
            }
            buffers.pending.clear();
            buffers
                .pending
                .extend(book.pending(k).map(|view| to_pending_order(view, now_ms)));
            let state = BarState {
                now_ms,
                equity: equity_i,
                quote_free: book.quote_free(),
                qty_free: book.qty_free(k),
                position: book.position(k).map(to_position),
                fees,
            };
            let Some(symbol) = config.universe.get(k) else {
                return Err(EngineError::EmptyUniverse);
            };
            run.signals.clear();
            {
                let mut ctx = BarContext::new(symbol, state, &buffers.pending, &run.current, mask)?;
                if run.uses_regime
                    && let Some(feed) = feed.as_deref_mut()
                {
                    let closed = feed.advance(now_ms);
                    ctx = ctx.with_regime(Some(feed.view(closed)?))?;
                }
                run.strategy.on_bar(&ctx, &mut run.signals);
            }
            run.funnel.evaluations += 1;
            run.evaluated = true;
        }

        // 5. Soumission, jambe par jambe dans l'ordre du rang, signal par signal dans l'ordre d'émission.
        for &k in order {
            let k = usize::from(k);
            let Some(run) = runs.get_mut(k) else {
                continue;
            };
            if !run.evaluated {
                continue;
            }
            run.evaluated = false;
            let Some(candle) = run.candles.get(run.idx) else {
                continue;
            };
            debug_assert!(
                config.sizing != SizingMode::Research || base == config.initial_cash,
                "mode recherche : la base de sizing doit valoir le capital initial"
            );
            let context = SubmitContext {
                signal_ms: now_ms,
                equity: base,
                ref_price: candle.close,
            };
            buffers.emitted += run.signals.len() as u64;
            for signal in &run.signals {
                let funnel = &mut run.funnel;
                let class = classify(signal);
                let (emitted, orders, rejections) = match class {
                    SignalClass::Entry => (
                        &mut funnel.entry_signals,
                        &mut funnel.entry_orders,
                        &mut funnel.entry_rejections,
                    ),
                    SignalClass::Exit => (
                        &mut funnel.exit_signals,
                        &mut funnel.exit_orders,
                        &mut funnel.other_rejections,
                    ),
                    SignalClass::Other => (
                        &mut funnel.other_signals,
                        &mut funnel.other_accepted,
                        &mut funnel.other_rejections,
                    ),
                };
                *emitted += 1;
                // Recherche : filtre des entrées à `now_ms`. Le refus est compté, l'ordre n'est pas
                // soumis, aucune fraction de capital n'est enregistrée ; sorties, `MoveStop` et
                // annulations ne sont jamais filtrés.
                if class == SignalClass::Entry
                    && let Some(state) = research.as_deref_mut()
                    && let Some(refusal) = state.refusal(k, now_ms)
                {
                    run.research.record(refusal);
                    continue;
                }
                let fraction = if class == SignalClass::Entry {
                    entry_fraction(signal, base, candle.close)
                } else {
                    None
                };
                let record = match book.submit(k, instruction(signal), context, max_notional) {
                    Ok(()) => {
                        *orders += 1;
                        buffers.accepted += 1;
                        true
                    }
                    Err(BookRejection::Sim(rejection)) => {
                        rejections.record(rejection);
                        matches!(
                            rejection,
                            crate::sim::Rejection::InsufficientQuote
                                | crate::sim::Rejection::BelowMinimum
                        )
                    }
                    Err(BookRejection::MaxPositions) => {
                        run.max_positions_rejected += 1;
                        true
                    }
                    Err(BookRejection::UnknownLeg) => {
                        return Err(EngineError::Sim(SimError::UnknownLeg { leg: k, legs }));
                    }
                };
                if record && let Some(fraction) = fraction {
                    run.entry_fractions.push(fraction);
                }
            }
        }

        // Les jambes présentes avancent d'une bougie.
        for run in runs.iter_mut() {
            if run.present {
                run.idx += 1;
                run.present = false;
            }
        }
    }

    // Fin de fenêtre : un trou encore ouvert est soldé comme un trou de cotation, avant la clôture forcée.
    if let Some(last) = buffers.last_open_ms {
        let end_ms = last.saturating_add(MINUTE_MS);
        let order = ranks.order_at(end_ms);
        // En recherche, une échéance qui tombe à `decision_end` ou au-delà n'est pas soldée en trou de
        // cotation : la position passe à la clôture forcée (`end_of_window`) et elle est purgée
        // (docs S11 §3.4), sinon `exit_ms >= decision_end` sans être `end_of_window` ferait échouer le run.
        let at_ms = match research.as_deref() {
            Some(state) => end_ms.min(state.decision_end_ms().saturating_sub(1)),
            None => end_ms,
        };
        force_quote_gaps(book, runs, order, at_ms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Size;

    #[test]
    fn fraction_demandee_par_type_de_taille() {
        let enter = |size, kind, stop| Signal::EnterLong {
            size,
            kind,
            stop,
            take_profit: None,
            tag: "t",
        };
        // Risque 1 % sur 300 entre 100 et 98 : 1,5 unité, 150 de notional, soit 0,5 de la base.
        let f = entry_fraction(
            &enter(Size::RiskPct(0.01), OrderKind::Market, Some(98.0)),
            300.0,
            100.0,
        );
        assert_eq!(f, Some(0.5));
        // Fraction d'equity : telle quelle.
        let f = entry_fraction(
            &enter(Size::PctEquity(0.25), OrderKind::Market, None),
            300.0,
            100.0,
        );
        assert_eq!(f, Some(0.25));
        // Montant en quote : montant / base.
        let f = entry_fraction(
            &enter(Size::Quote(60.0), OrderKind::Market, None),
            300.0,
            100.0,
        );
        assert_eq!(f, Some(0.2));
        // Limit : le prix de la limit remplace la référence.
        let f = entry_fraction(
            &enter(
                Size::Quote(60.0),
                OrderKind::Limit {
                    price: 50.0,
                    ttl_bars: 5,
                },
                None,
            ),
            300.0,
            100.0,
        );
        assert_eq!(f, Some(0.2));
        // Sizing invalide (risque sans stop) ou signal qui n'est pas une entrée : rien.
        assert_eq!(
            entry_fraction(
                &enter(Size::RiskPct(0.01), OrderKind::Market, None),
                300.0,
                100.0
            ),
            None
        );
        let exit = Signal::ExitLong {
            kind: OrderKind::Market,
            tag: "x",
        };
        assert_eq!(entry_fraction(&exit, 300.0, 100.0), None);
        let sell = Signal::PlaceLimit {
            side: Side::Sell,
            price: 100.0,
            size: Size::Quote(10.0),
            ttl_bars: 5,
            slot: 1,
            tag: "s",
        };
        assert_eq!(entry_fraction(&sell, 300.0, 100.0), None);
        let buy = Signal::PlaceLimit {
            side: Side::Buy,
            price: 100.0,
            size: Size::Quote(30.0),
            ttl_bars: 5,
            slot: 1,
            tag: "b",
        };
        assert_eq!(entry_fraction(&buy, 300.0, 100.0), Some(0.1));
    }

    #[test]
    fn la_somme_des_compteurs_additionne_chaque_champ() {
        let one = SimCounters {
            fills: 1,
            orders_unfilled_at_end: 2,
            ..SimCounters::default()
        };
        let mut total = SimCounters::default();
        add_counters(&mut total, &one);
        add_counters(&mut total, &one);
        assert_eq!(total.fills, 2);
        assert_eq!(total.orders_unfilled_at_end, 4);
        let d = SimDetail {
            quote_gap_exits: 3,
            below_min_exits: 1,
            ..SimDetail::default()
        };
        let mut total = SimDetail::default();
        add_detail(&mut total, &d);
        add_detail(&mut total, &d);
        assert_eq!(total.quote_gap_exits, 6);
        assert_eq!(total.below_min_exits, 2);
    }

    #[test]
    fn le_entonnoir_de_strategie_est_somme_par_nom() {
        let leg = |raw, a, b| LegResult {
            symbol: "X".to_string(),
            start_index: 0,
            rows: 0,
            first_ms: 0,
            last_ms: 0,
            funnel: EngineFunnel {
                evaluations: 1,
                strategy: Some(StrategyFunnelCounts {
                    raw_entry_signals: raw,
                    filtered: vec![("a", a), ("b", b)],
                }),
                ..EngineFunnel::default()
            },
            counters: SimCounters::default(),
            detail: SimDetail::default(),
            max_positions_rejected: 0,
            entry_fractions: Vec::new(),
            trades: 0,
        };
        let total = sum_funnels(&[leg(3, 1, 2), leg(4, 10, 20)]);
        assert_eq!(total.evaluations, 2);
        let strategy = total.strategy.unwrap();
        assert_eq!(strategy.raw_entry_signals, 7);
        assert_eq!(strategy.filtered, vec![("a", 11), ("b", 22)]);
        // Une jambe sans entonnoir de stratégie : pas de somme.
        let mut without = leg(1, 0, 0);
        without.funnel.strategy = None;
        assert!(sum_funnels(&[leg(3, 1, 2), without]).strategy.is_none());
        assert!(sum_funnels(&[]).strategy.is_none());
    }
}
