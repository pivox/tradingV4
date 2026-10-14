//! Boucle de backtest 1m (docs/03-architecture.md §7.1 et §7.2).

use crate::indicators::{Atr, Ema, Rsi, Sma, Vwap, batch};
use crate::model::{Candle, Timeframe, Ts};
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
pub fn run_cell_observed(
    candles: &[Candle],
    mut strategy: Box<dyn Strategy>,
    config: &CellConfig<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<CellResult, EngineError> {
    // --- Préparation, hors boucle : les allocations sont permises (docs/03 §7.1) ---
    config.sim_rules.validate()?;
    validate_declaration(strategy.as_ref())?;
    if strategy.timeframes() != [Timeframe::M1] {
        return Err(EngineError::UnsupportedTimeframes {
            declared: strategy.timeframes().to_vec(),
        });
    }
    check_candles(candles)?;
    let n = candles.len();

    let specs = strategy.indicators();
    let mut names: Vec<&'static str> = Vec::with_capacity(specs.len());
    let mut columns: Vec<Vec<f64>> = Vec::with_capacity(specs.len());
    for spec in &specs {
        names.push(spec.name);
        columns.push(match spec.kind {
            IndicatorKind::Sma { period } => {
                batch(Sma::new(period)?, candles.iter().map(|c| c.close))
            }
            IndicatorKind::Ema { period } => {
                batch(Ema::new(period)?, candles.iter().map(|c| c.close))
            }
            IndicatorKind::Rsi { period } => {
                batch(Rsi::new(period)?, candles.iter().map(|c| c.close))
            }
            IndicatorKind::Atr { period } => batch(Atr::new(period)?, candles.iter().copied()),
            IndicatorKind::Vwap => batch(Vwap::new(), candles.iter().copied()),
        });
    }
    let column_refs: Vec<&[f64]> = columns.iter().map(Vec::as_slice).collect();

    let warmup_bars = strategy.warmup_bars();
    let start = warmup_bars;
    if start >= n {
        return Err(EngineError::NotEnoughBars {
            warmup_bars,
            bars: n,
        });
    }
    for (name, column) in names.iter().zip(&columns) {
        if column.get(start).is_none_or(|value| !value.is_finite()) {
            return Err(EngineError::WarmupTooShort { name, index: start });
        }
    }

    let base = TfView::new(Timeframe::M1, candles, &names, &column_refs)?;
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
        base,
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

/// Boucle des étapes 1 à 5 de docs/03 §7.2 sur `window_candles`, qui commence à l'index `start`.
fn run_loop(
    window_candles: &[Candle],
    start: usize,
    base: TfView<'_>,
    config: &CellConfig<'_>,
    strategy: &mut dyn Strategy,
    sim: &mut Simulator,
    buffers: &mut LoopBuffers,
) -> Result<(), EngineError> {
    let mut views = [base];
    for (offset, candle) in window_candles.iter().enumerate() {
        let i = start + offset;
        // 1. Fills des ordres émis à i-1 ou avant, à open[i].
        sim.on_open(candle)?;
        // 2. Observation intrabar (en S3, seuls les franchissements de stop sont marqués).
        sim.observe_intrabar(candle);
        // 3. Equity à la clôture de la bougie.
        let equity_i = sim.equity_at(candle.close);
        buffers.equity.push(equity_i);
        // 4. Stratégie, appelée à chaque 1m (le TF 1m vient toujours de clôturer).
        let now_ms = candle.open_time_ms.saturating_add(Timeframe::M1.ms());
        views[0] = base.with_closed(i + 1)?;
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
            let ctx = BarContext::new(config.symbol, state, &[], &views, Timeframe::M1.bit())?;
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
