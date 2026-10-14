//! Boucle de backtest sur la grille 1m, multi-timeframe (docs/03-architecture.md §7.1 et §7.2).

use crate::indicators::{Atr, Ema, Rsi, Sma, Vwap, batch};
use crate::model::{Candle, Timeframe, Ts};
use crate::mtf::Mtf;
use crate::sim::{ExitReason, SimCounters, Simulator, Trade};
use crate::strategy::{
    BarContext, BarState, IndicatorKind, Position, Signal, Strategy, TfView, validate_declaration,
};

use super::config::{CellConfig, RunConfig};
use super::signals::{SignalContext, SignalRejections, dispatch};
use super::{EngineError, LoopObserver, NoopObserver};

/// Capacité initiale du vecteur de trades. Un aller-retour occupe au moins deux bougies ; au-delà de
/// cette borne, la croissance est amortie (docs/03 §7.2 ligne 6).
const TRADES_INITIAL_CAPACITY: usize = 4096;
/// Capacité du tampon de signaux, réutilisé à chaque bougie.
const SIGNALS_CAPACITY: usize = 16;

/// Résultat d'une cellule.
#[derive(Clone, Debug)]
pub struct CellResult {
    /// Objet haché du `config_hash`.
    pub config: RunConfig,
    /// Hash blake3 hexadécimal de `config`.
    pub config_hash: String,
    pub warmup_bars: usize,
    /// Index de la première bougie jouée.
    pub start_index: usize,
    pub bars_in_window: usize,
    pub window_first_open_ms: Ts,
    pub window_last_open_ms: Ts,
    pub trades: Vec<Trade>,
    /// Une valeur par bougie de `[start, n)`, mesurée à l'étape 3, avant la clôture forcée.
    pub equity: Vec<f64>,
    pub cash_final: f64,
    pub qty_final: f64,
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
    pub rejections: SignalRejections,
    pub sim: SimCounters,
}

/// Rejoue `candles` avec `strategy` (voir [`run_cell_observed`]).
pub fn run_cell(
    candles: &[Candle],
    strategy: Box<dyn Strategy>,
    config: &CellConfig<'_>,
) -> Result<CellResult, EngineError> {
    run_cell_observed(candles, strategy, config, &mut NoopObserver)
}

/// Comme [`run_cell`], avec un observateur appelé juste avant et juste après la boucle.
///
/// Contrôle les données, agrège le 1m vers les timeframes déclarés par la stratégie ([`Mtf::build`])
/// puis délègue à [`run_cell_mtf`].
pub fn run_cell_observed(
    candles: &[Candle],
    strategy: Box<dyn Strategy>,
    config: &CellConfig<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<CellResult, EngineError> {
    config.sim_rules.validate()?;
    validate_declaration(strategy.as_ref())?;
    check_candles(candles)?;
    let mtf = Mtf::build(candles, strategy.timeframes())?;
    run_prepared(&mtf, strategy, config, observer)
}

/// Rejoue sur un [`Mtf`] déjà construit (campagnes du jalon 1.5, tests du mutant de docs/03 §7.5).
///
/// Le `Mtf` doit contenir tous les timeframes déclarés par la stratégie, sinon
/// [`EngineError::MissingTimeframe`].
pub fn run_cell_mtf(
    mtf: &Mtf<'_>,
    strategy: Box<dyn Strategy>,
    config: &CellConfig<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<CellResult, EngineError> {
    config.sim_rules.validate()?;
    validate_declaration(strategy.as_ref())?;
    check_candles(mtf.m1())?;
    run_prepared(mtf, strategy, config, observer)
}

/// Un indicateur calculé hors boucle : sa position dans les tableaux par timeframe déclaré.
struct IndicatorSlot {
    name: &'static str,
    /// Index du timeframe dans l'ordre de `Strategy::timeframes`.
    tf_index: usize,
    /// Index de la colonne parmi celles de ce timeframe.
    column_index: usize,
}

/// Colonne d'un indicateur sur la série `candles` de son timeframe, par `batch` (docs/03 §1).
fn indicator_column(kind: IndicatorKind, candles: &[Candle]) -> Result<Vec<f64>, EngineError> {
    Ok(match kind {
        IndicatorKind::Sma { period } => batch(Sma::new(period)?, candles.iter().map(|c| c.close)),
        IndicatorKind::Ema { period } => batch(Ema::new(period)?, candles.iter().map(|c| c.close)),
        IndicatorKind::Rsi { period } => batch(Rsi::new(period)?, candles.iter().map(|c| c.close)),
        IndicatorKind::Atr { period } => batch(Atr::new(period)?, candles.iter().copied()),
        IndicatorKind::Vwap => batch(Vwap::new(), candles.iter().copied()),
    })
}

/// Corps commun de [`run_cell_observed`] et [`run_cell_mtf`] : déclaration et données déjà contrôlées.
fn run_prepared(
    mtf: &Mtf<'_>,
    mut strategy: Box<dyn Strategy>,
    config: &CellConfig<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<CellResult, EngineError> {
    // --- Préparation, hors boucle : les allocations sont permises (docs/03 §7.1) ---
    let candles = mtf.m1();
    let n = candles.len();

    // Timeframes déclarés, dans l'ordre de déclaration ; `tf_max` fixe l'unité du warmup.
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

    // Indicateurs : colonnes calculées sur la série de leur timeframe, rangées dans l'ordre de
    // `indicators()` restreint à ce timeframe (c'est l'ordre que `BarContext::ind_at` attend).
    let specs = strategy.indicators();
    let mut names: Vec<Vec<&'static str>> = vec![Vec::new(); declared.len()];
    let mut columns: Vec<Vec<Vec<f64>>> = vec![Vec::new(); declared.len()];
    let mut slots: Vec<IndicatorSlot> = Vec::with_capacity(specs.len());
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
        slots.push(IndicatorSlot {
            name: spec.name,
            tf_index,
            column_index: tf_columns.len(),
        });
        tf_names.push(spec.name);
        tf_columns.push(indicator_column(spec.kind, series)?);
    }
    let column_refs: Vec<Vec<&[f64]>> = columns
        .iter()
        .map(|tf_columns| tf_columns.iter().map(Vec::as_slice).collect())
        .collect();

    // Warmup en bougies du plus grand timeframe déclaré (docs/03 §7.2, écart S5 point 1) : la première
    // 1m jouée est la première dont les `warmup_bars` bougies sont toutes clôturées à la fin de la 1m
    // précédente. Pour une stratégie 1m, `closed_count[i - 1] = i` donne `start = warmup_bars` (S3).
    let warmup_bars = strategy.warmup_bars();
    let counts_max = declared
        .iter()
        .position(|tf| *tf == tf_max)
        .and_then(|k| closed.get(k).copied())
        .ok_or(EngineError::MissingTimeframe { tf: tf_max })?;
    let start = (0..n)
        .find(|&i| {
            let before = if i == 0 {
                0
            } else {
                counts_max.get(i - 1).copied().unwrap_or(0)
            };
            before >= warmup_bars
        })
        .ok_or(EngineError::NotEnoughBars {
            warmup_bars,
            bars: n,
        })?;

    // Refus de démarrer (S3 point 2 généralisé) : pour chaque indicateur, dans l'ordre de
    // `indicators()`, la dernière valeur visible à la première 1m jouée doit être finie. L'index est
    // celui de la série du timeframe de l'indicateur.
    for slot in &slots {
        let visible = closed
            .get(slot.tf_index)
            .and_then(|counts| counts.get(start))
            .copied()
            .unwrap_or(0);
        let index = visible.saturating_sub(1);
        let value = columns
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

    // Une vue par timeframe déclaré, bornée plus tard à `closed_count[tf][i]`.
    let mut base_views: Vec<TfView<'_>> = Vec::with_capacity(declared.len());
    for (k, &tf) in declared.iter().enumerate() {
        let (Some(series), Some(tf_names), Some(tf_refs)) =
            (tf_candles.get(k), names.get(k), column_refs.get(k))
        else {
            return Err(EngineError::MissingTimeframe { tf });
        };
        base_views.push(TfView::new(tf, series, tf_names, tf_refs)?);
    }
    let views = base_views.clone();

    let window = n - start;
    let trades_capacity = (window / 2 + 1).min(TRADES_INITIAL_CAPACITY);
    let mut sim = Simulator::new(
        config.fees,
        config.slippage,
        config.sim_rules,
        config.initial_cash,
        trades_capacity,
    )?;
    let run_config = RunConfig::new(strategy.as_ref(), config, warmup_bars);
    let config_hash = run_config.hash()?;
    let window_candles = candles.get(start..).ok_or(EngineError::NotEnoughBars {
        warmup_bars,
        bars: n,
    })?;
    let mut buffers = LoopBuffers {
        signals: Vec::with_capacity(SIGNALS_CAPACITY),
        equity: Vec::with_capacity(window),
        emitted: 0,
        accepted: 0,
        rejections: SignalRejections::default(),
    };

    // --- Boucle : 0 allocation attendue (tolérance documentée : 1 pour 10 000 bougies) ---
    observer.loop_started();
    let looped = run_loop(
        window_candles,
        start,
        LoopViews {
            mtf,
            declared_mask,
            base: base_views,
            current: views,
            closed,
        },
        config,
        strategy.as_mut(),
        &mut sim,
        &mut buffers,
    );
    observer.loop_finished();
    looped?;

    // --- Fin de run, hors boucle ---
    let last = candles.last().ok_or(EngineError::EmptyData)?;
    let out = sim.finish(last)?;

    // Totaux : boucles `for` explicites dans l'ordre des trades, pas de `.sum()` (docs/03 §7.4).
    let mut fees_total = 0.0;
    let mut slippage_total = 0.0;
    let mut pnl_realized = 0.0;
    let mut pnl_marked = 0.0;
    let mut n_trades_forced = 0u64;
    for trade in &out.trades {
        fees_total += trade.entry_fee;
        fees_total += trade.exit_fee;
        slippage_total += trade.entry_slippage;
        slippage_total += trade.exit_slippage;
        match trade.exit_reason {
            ExitReason::Signal => pnl_realized += trade.pnl_net,
            ExitReason::ForcedClose => {
                pnl_marked += trade.pnl_net;
                n_trades_forced += 1;
            }
        }
    }
    let first_in_window = window_candles
        .first()
        .map_or(last.open_time_ms, |c| c.open_time_ms);

    Ok(CellResult {
        config: run_config,
        config_hash,
        warmup_bars,
        start_index: start,
        bars_in_window: window,
        window_first_open_ms: first_in_window,
        window_last_open_ms: last.open_time_ms,
        trades: out.trades,
        equity: buffers.equity,
        cash_final: out.cash,
        qty_final: out.qty,
        equity_final: out.cash,
        forced_close: out.forced_close,
        n_trades_forced,
        pnl_net: out.cash - config.initial_cash,
        pnl_realized,
        pnl_marked,
        fees_total,
        slippage_total,
        signals_emitted: buffers.emitted,
        signals_accepted: buffers.accepted,
        rejections: buffers.rejections.with_sim_counters(&out.counters),
        sim: out.counters,
    })
}

/// Contrôle des données : valides, alignées sur la minute, strictement croissantes. Les trous sont
/// admis (politique `skip`, docs/03 §5.1) : la bougie suivante est la suivante dans la série.
fn check_candles(candles: &[Candle]) -> Result<(), EngineError> {
    if candles.is_empty() {
        return Err(EngineError::EmptyData);
    }
    let mut previous: Option<Ts> = None;
    for (index, candle) in candles.iter().enumerate() {
        candle.validate()?;
        if !Timeframe::M1.is_aligned(candle.open_time_ms) {
            return Err(EngineError::NotOneMinute {
                open_time_ms: candle.open_time_ms,
            });
        }
        if let Some(previous_ms) = previous
            && candle.open_time_ms <= previous_ms
        {
            return Err(EngineError::NotIncreasing {
                index,
                previous_ms,
                open_time_ms: candle.open_time_ms,
            });
        }
        previous = Some(candle.open_time_ms);
    }
    Ok(())
}

/// Tampons et compteurs de la boucle, tous préalloués avant elle.
struct LoopBuffers {
    signals: Vec<Signal>,
    equity: Vec<f64>,
    emitted: u64,
    accepted: u64,
    rejections: SignalRejections,
}

/// Vues par timeframe déclaré et index de clôture, préparés hors boucle.
struct LoopViews<'a> {
    mtf: &'a Mtf<'a>,
    /// OU des `Timeframe::bit` des timeframes déclarés par la stratégie.
    declared_mask: u8,
    /// Vues à `closed = 0`, dans l'ordre de `Strategy::timeframes`.
    base: Vec<TfView<'a>>,
    /// Vues de la bougie courante, réécrites sur place (aucune allocation dans la boucle).
    current: Vec<TfView<'a>>,
    /// `closed_count` de chaque timeframe déclaré, même ordre que `base`.
    closed: Vec<&'a [usize]>,
}

/// Boucle des étapes 1 à 5 de docs/03 §7.2 sur `window_candles`, qui commence à l'index `start`.
///
/// La stratégie n'est appelée à la 1m `i` que si une bougie de l'un de ses timeframes déclarés vient de
/// s'y clôturer (`just_closed[i] & declared_mask != 0`) ; sinon aucun signal ne naît, mais les fills,
/// l'observation intrabar et l'equity de la bougie ont lieu comme toujours.
fn run_loop(
    window_candles: &[Candle],
    start: usize,
    mut views: LoopViews<'_>,
    config: &CellConfig<'_>,
    strategy: &mut dyn Strategy,
    sim: &mut Simulator,
    buffers: &mut LoopBuffers,
) -> Result<(), EngineError> {
    for (offset, candle) in window_candles.iter().enumerate() {
        let i = start + offset;
        // 1. Fills des ordres émis à i-1 ou avant, à open[i].
        sim.on_open(candle)?;
        // 2. Observation intrabar (en S3, seuls les franchissements de stop sont marqués).
        sim.observe_intrabar(candle);
        // 3. Equity à la clôture de la bougie.
        let equity_i = sim.equity_at(candle.close);
        buffers.equity.push(equity_i);
        // 4. Stratégie, appelée seulement quand un de ses timeframes vient de clôturer une bougie.
        let mask = views.mtf.just_closed(i) & views.declared_mask;
        if mask == 0 {
            continue;
        }
        let now_ms = candle.open_time_ms.saturating_add(Timeframe::M1.ms());
        for (k, current) in views.current.iter_mut().enumerate() {
            let (Some(base), Some(counts)) = (views.base.get(k), views.closed.get(k)) else {
                return Err(EngineError::MissingClosedCount { index: i });
            };
            let count = counts
                .get(i)
                .copied()
                .ok_or(EngineError::MissingClosedCount { index: i })?;
            *current = base.with_closed(count)?;
        }
        let portfolio = sim.portfolio();
        let position = portfolio.is_long().then(|| Position {
            qty: portfolio.qty(),
            avg_price: portfolio.avg_price(),
            stop: None,
            take_profit: None,
            opened_at_ms: portfolio.opened_at_ms(),
        });
        // Aucune réservation en S3 : tout le cash et toute la quantité sont libres.
        let state = BarState {
            now_ms,
            equity: equity_i,
            quote_free: portfolio.cash(),
            qty_free: portfolio.qty(),
            position,
            fees: config.fees,
        };
        debug_assert!(!sim.has_pending());
        buffers.signals.clear();
        {
            let ctx = BarContext::new(config.symbol, state, &[], &views.current, mask)?;
            strategy.on_bar(&ctx, &mut buffers.signals);
        }
        // 5. Traduction des signaux : exécutables à partir de i+1 seulement.
        buffers.emitted += buffers.signals.len() as u64;
        let context = SignalContext {
            ref_price: candle.close,
            equity: equity_i,
            signal_ms: now_ms,
        };
        for signal in &buffers.signals {
            if dispatch(signal, context, sim, &mut buffers.rejections) {
                buffers.accepted += 1;
            }
        }
    }
    Ok(())
}
