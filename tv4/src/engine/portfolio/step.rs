//! Pas minute natif partagé par le portefeuille et ses futurs rejeux contrôlés.

use super::*;

/// Source des signaux, appelée après tous les événements et avant toute soumission.
/// Les observateurs lisent aussi le journal natif : un lot ouvert puis stoppé dans le
/// même pas peut laisser la position à plat avant et après les événements.
pub(super) trait EntryDriver {
    /// Débit avant tout travail : nombre de bougies de jambes présentes à cette minute.
    fn before_minute(&mut self, _steps: u64) -> Result<(), EngineError> {
        Ok(())
    }
    fn evaluate(
        &mut self,
        leg: usize,
        strategy: &mut dyn Strategy,
        ctx: &BarContext<'_>,
        out: &mut Vec<Signal>,
    ) -> Result<(), EngineError>;
    fn after_events(&mut self, _book: &Book, _at_ms: Ts) {}
}

pub(super) struct Observed;
impl EntryDriver for Observed {
    fn evaluate(
        &mut self,
        _leg: usize,
        strategy: &mut dyn Strategy,
        ctx: &BarContext<'_>,
        out: &mut Vec<Signal>,
    ) -> Result<(), EngineError> {
        strategy.on_bar(ctx, out);
        Ok(())
    }
}

/// Exécute exactement une minute depuis les curseurs présents, sans rechercher un préfixe.
#[allow(clippy::too_many_arguments)]
pub(super) fn advance_minute<D: EntryDriver>(
    book: &mut Book,
    runs: &mut [LegRun<'_>],
    ranks: &RankTable,
    mut feed: Option<&mut RegimeFeed<'_>>,
    config: &PortfolioConfig<'_>,
    max_notional: Option<f64>,
    buffers: &mut PlayBuffers,
    mut research: Option<&mut ResearchState>,
    t: Ts,
    driver: &mut D,
) -> Result<(), EngineError> {
    let steps = runs
        .iter()
        .filter(|run| {
            run.candles
                .get(run.idx)
                .is_some_and(|c| c.open_time_ms == t)
        })
        .count() as u64;
    driver.before_minute(steps)?;
    let legs = runs.len();
    let record_from = research
        .as_deref()
        .map_or(Ts::MIN, ResearchState::decision_start_ms);
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
    driver.after_events(book, now_ms);
    let recorded = t >= record_from;
    if recorded && (0..legs).any(|k| book.position(k).is_some()) {
        buffers.bars_in_position += 1;
    }

    // 3. Equity du portefeuille, une fois par minute de la grille. En recherche, les minutes d'avant
    // `decision_start` sont jouées sans être enregistrées (docs S11 §8.2).
    let equity_i = book.equity();
    if recorded {
        buffers.record(t, equity_i, book.cash());
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
        prepare_views(&run.base, &mut run.current, &run.closed, i)?;
        run.signals.clear();
        with_context(
            book,
            k,
            config,
            equity_i,
            now_ms,
            &mut buffers.pending,
            &run.current,
            mask,
            run.uses_regime,
            feed.as_deref_mut(),
            |ctx| driver.evaluate(k, run.strategy.as_mut(), ctx, &mut run.signals),
        )?;
        run.funnel.evaluations += 1;
        run.evaluated = true;
    }

    submit_signals(
        book,
        runs,
        order,
        config,
        base,
        max_notional,
        buffers,
        research,
        now_ms,
    )?;

    // Les jambes présentes avancent d'une bougie.
    for run in runs.iter_mut() {
        if run.present {
            run.idx += 1;
            run.present = false;
        }
    }
    Ok(())
}

/// Réécrit les seules vues closes visibles à l'index courant.
pub(super) fn prepare_views<'a>(
    base: &[TfView<'a>],
    current: &mut [TfView<'a>],
    closed: &[&[usize]],
    i: usize,
) -> Result<(), EngineError> {
    for (j, current) in current.iter_mut().enumerate() {
        let (Some(base_view), Some(counts)) = (base.get(j), closed.get(j)) else {
            return Err(EngineError::MissingClosedCount { index: i });
        };
        let count = counts
            .get(i)
            .copied()
            .ok_or(EngineError::MissingClosedCount { index: i })?;
        *current = base_view.with_closed(count)?;
    }
    Ok(())
}

/// Contexte unique des évaluations observées, préparations causales et drivers.
/// Les champs sont séparés pour permettre l'emprunt indépendant de strategy/signals.
#[allow(clippy::too_many_arguments)]
pub(super) fn with_context(
    book: &Book,
    k: usize,
    config: &PortfolioConfig<'_>,
    equity_i: f64,
    now_ms: Ts,
    pending: &mut Vec<PendingOrder>,
    current: &[TfView<'_>],
    mask: u8,
    uses_regime: bool,
    feed: Option<&mut RegimeFeed<'_>>,
    evaluate: impl FnOnce(&BarContext<'_>) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    pending.clear();
    pending.extend(book.pending(k).map(|view| to_pending_order(view, now_ms)));
    let state = BarState {
        now_ms,
        equity: equity_i,
        quote_free: book.quote_free(),
        qty_free: book.qty_free(k),
        position: book.position(k).map(to_position),
        fees: config.fees,
    };
    let Some(symbol) = config.universe.get(k) else {
        return Err(EngineError::EmptyUniverse);
    };
    let mut ctx = BarContext::new(symbol, state, pending, current, mask)?;
    if uses_regime && let Some(feed) = feed {
        let closed = feed.advance(now_ms);
        ctx = ctx.with_regime(Some(feed.view(closed)?))?;
    }
    evaluate(&ctx)
}

/// Soumet toutes les demandes après toutes les snapshots, dans l'ordre natif du rang.
#[allow(clippy::too_many_arguments)]
fn submit_signals(
    book: &mut Book,
    runs: &mut [LegRun<'_>],
    order: &[u16],
    config: &PortfolioConfig<'_>,
    base: f64,
    max_notional: Option<f64>,
    buffers: &mut PlayBuffers,
    mut research: Option<&mut ResearchState>,
    now_ms: Ts,
) -> Result<(), EngineError> {
    let legs = runs.len();
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExchangeId, FeeSchedule, InstrumentRules, Slippage};
    use crate::sim::{ExitReason, SimRules};
    use crate::strategy::{IndicatorSpec, Size};

    struct Idle;
    impl Strategy for Idle {
        fn id(&self) -> &'static str {
            "step_fixture"
        }
        fn version(&self) -> &'static str {
            "0"
        }
        fn params_json(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn timeframes(&self) -> &[Timeframe] {
            &[Timeframe::M1]
        }
        fn indicators(&self) -> Vec<IndicatorSpec> {
            Vec::new()
        }
        fn warmup_bars(&self) -> usize {
            0
        }
        fn on_bar(&mut self, _: &BarContext<'_>, _: &mut Vec<Signal>) {}
    }

    #[derive(Default)]
    struct Driver {
        calls: Vec<Ts>,
        events: Vec<(Ts, Ts, usize)>,
        seen_trades: usize,
    }
    impl EntryDriver for Driver {
        fn evaluate(
            &mut self,
            _: usize,
            _: &mut dyn Strategy,
            ctx: &BarContext<'_>,
            out: &mut Vec<Signal>,
        ) -> Result<(), EngineError> {
            self.calls.push(ctx.now_ms());
            if ctx.position().is_none()
                && (self.calls.len() == 1
                    || self
                        .events
                        .last()
                        .is_some_and(|event| event.0 == ctx.now_ms()))
            {
                out.push(Signal::EnterLong {
                    size: Size::Quote(100.0),
                    kind: OrderKind::Market,
                    stop: Some(90.0),
                    take_profit: None,
                    tag: "step",
                });
            }
            Ok(())
        }
        fn after_events(&mut self, book: &Book, at_ms: Ts) {
            let (trades, legs) = book.trades();
            for (trade, &leg) in trades.iter().zip(legs).skip(self.seen_trades) {
                self.events.push((at_ms, trade.exit_ms, usize::from(leg)));
            }
            self.seen_trades = trades.len();
        }
    }

    fn candle(minute: i64, low: f64) -> Candle {
        Candle {
            open_time_ms: minute * MINUTE_MS,
            open: 100.0,
            high: 100.0,
            low,
            close: 100.0,
            volume: 1e6,
            trades: None,
        }
    }

    fn config<'a>(
        universe: &'a [String],
        instruments: &'a [InstrumentRules],
    ) -> PortfolioConfig<'a> {
        PortfolioConfig {
            universe,
            regime_symbol: "BTC",
            fees: FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
            slippage: Slippage::ZERO,
            initial_cash: 1000.0,
            sim_rules: SimRules {
                max_volume_share: 1e9,
                ..SimRules::default()
            },
            instruments,
            instruments_hash: None,
            max_positions: 1,
            sizing: SizingMode::Research,
            min_order_bump_max_risk: None,
            quote_gap_ms: 3 * MINUTE_MS,
            min_trades: 1,
        }
    }

    fn run<'a>(candles: &'a [Candle], mtf: &'a Mtf<'a>, idx: usize) -> LegRun<'a> {
        let base = vec![TfView::new(Timeframe::M1, candles, &[], &[]).unwrap()];
        LegRun {
            candles,
            mtf,
            strategy: Box::new(Idle),
            uses_regime: false,
            declared_mask: Timeframe::M1.bit(),
            current: base.clone(),
            base,
            closed: vec![mtf.closed_counts(Timeframe::M1).unwrap()],
            idx,
            present: false,
            evaluated: false,
            signals: Vec::with_capacity(8),
            funnel: EngineFunnel::default(),
            entry_fractions: Vec::with_capacity(8),
            max_positions_rejected: 0,
            deadline: Ts::MAX,
            start_index: idx,
            rows: candles.len() - idx,
            research: ResearchLegCounters::default(),
        }
    }

    struct FailingDriver;
    impl EntryDriver for FailingDriver {
        fn evaluate(
            &mut self,
            _: usize,
            _: &mut dyn Strategy,
            _: &BarContext<'_>,
            out: &mut Vec<Signal>,
        ) -> Result<(), EngineError> {
            // Même une demande émise avant l'erreur ne doit pas atteindre Book::submit.
            out.push(Signal::EnterLong {
                size: Size::Quote(100.0),
                kind: OrderKind::Market,
                stop: None,
                take_profit: None,
                tag: "failed",
            });
            Err(crate::strategy::StrategyError::UnsupportedControlGeometry {
                id: "fixture",
                reason: "état incompatible",
            }
            .into())
        }
    }

    #[test]
    fn driver_failure_propagates_before_any_submission() {
        let candles = [candle(0, 100.0)];
        let mtf = Mtf::build(&candles, &[Timeframe::M1]).unwrap();
        let mut runs = [run(&candles, &mtf, 0)];
        let universe = vec!["X".to_owned()];
        let instruments = [InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).unwrap()];
        let config = config(&universe, &instruments);
        let mut book = Book::new(book_config(&config, candles.len()), &instruments).unwrap();
        let ranks = RankTable::build(&[&candles]);
        let mut buffers = PlayBuffers::new(&runs, Ts::MIN, 8, Recording::Reduced);
        let result = advance_minute(
            &mut book,
            &mut runs,
            &ranks,
            None,
            &config,
            Some(1000.0),
            &mut buffers,
            None,
            0,
            &mut FailingDriver,
        );
        assert!(matches!(
            result,
            Err(EngineError::Strategy(
                crate::strategy::StrategyError::UnsupportedControlGeometry {
                    id: "fixture",
                    reason: "état incompatible"
                }
            ))
        ));
        assert_eq!(book.pending_len(0), 0);
        assert_eq!(buffers.emitted, 0);
        assert_eq!(buffers.accepted, 0);
        assert_eq!(book.cash(), config.initial_cash);
    }

    #[test]
    fn isolated_minute_starts_at_cursor_and_releases_before_same_close_entry() {
        let candles = [
            candle(0, 100.0),
            candle(1, 100.0),
            candle(2, 100.0),
            candle(3, 80.0),
            candle(4, 100.0),
            candle(5, 100.0),
        ];
        let mtf = Mtf::build(&candles, &[Timeframe::M1]).unwrap();
        let mut runs = [run(&candles, &mtf, 1)];
        let universe = vec!["X".to_owned()];
        let instruments = [InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).unwrap()];
        let config = config(&universe, &instruments);
        let mut book = Book::new(book_config(&config, candles.len()), &instruments).unwrap();
        let ranks = RankTable::build(&[&candles]);
        let mut buffers = PlayBuffers::new(&runs, Ts::MIN, 8, Recording::Reduced);
        let mut driver = Driver::default();
        for minute in 1..=5 {
            advance_minute(
                &mut book,
                &mut runs,
                &ranks,
                None,
                &config,
                Some(1000.0),
                &mut buffers,
                None,
                minute * MINUTE_MS,
                &mut driver,
            )
            .unwrap();
        }
        assert_eq!(
            driver.calls,
            (2..=6).map(|minute| minute * MINUTE_MS).collect::<Vec<_>>()
        );
        // L'entrée puis le stop de la même minute étaient invisibles dans position() avant/après.
        assert_eq!(driver.events, vec![(4 * MINUTE_MS, 3 * MINUTE_MS, 0)]);
        assert_eq!(book.trades().0[0].exit_reason, ExitReason::Stop);
        assert_eq!(book.position(0).unwrap().opened_at_ms, 5 * MINUTE_MS);
        assert_eq!(runs[0].funnel.entry_orders, 2);
        assert_eq!(buffers.recorded_minutes, 5);
        assert_eq!(buffers.first_recorded_open_ms, Some(MINUTE_MS));
        assert_eq!(buffers.last_open_ms, Some(5 * MINUTE_MS));
        for capacity in [
            buffers.grid.capacity(),
            buffers.equity.capacity(),
            buffers.cash.capacity(),
        ] {
            assert_eq!(capacity, 0);
        }
    }

    #[test]
    fn quote_gap_trace_uses_processing_close_and_terminal_uses_global_time() {
        let candles = [
            candle(0, 100.0),
            candle(1, 100.0),
            candle(2, 100.0),
            candle(10, 100.0),
            candle(11, 100.0),
            candle(12, 100.0),
        ];
        let mtf = Mtf::build(&candles, &[Timeframe::M1]).unwrap();
        let mut runs = [run(&candles, &mtf, 0)];
        let universe = vec!["X".to_owned()];
        let instruments = [InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).unwrap()];
        let config = config(&universe, &instruments);
        let mut book = Book::new(book_config(&config, candles.len()), &instruments).unwrap();
        let ranks = RankTable::build(&[&candles]);
        let mut buffers = PlayBuffers::new(&runs, Ts::MIN, 8, Recording::Reduced);
        let mut driver = Driver::default();
        for minute in [0, 1, 2, 10, 11, 12] {
            advance_minute(
                &mut book,
                &mut runs,
                &ranks,
                None,
                &config,
                Some(1000.0),
                &mut buffers,
                None,
                minute * MINUTE_MS,
                &mut driver,
            )
            .unwrap();
        }
        assert_eq!(driver.events, vec![(11 * MINUTE_MS, 6 * MINUTE_MS, 0)]);
        assert_eq!(book.trades().0[0].exit_reason, ExitReason::QuoteGap);
        assert_eq!(book.position(0).unwrap().opened_at_ms, 12 * MINUTE_MS);
        book.close_all_at_end(&[candles.last().copied()]).unwrap();
        driver.after_events(&book, 20 * MINUTE_MS);
        assert_eq!(driver.events[1], (20 * MINUTE_MS, 13 * MINUTE_MS, 0));
    }
}
