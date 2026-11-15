//! Adaptateurs des contrôles vers le seul pas natif du portefeuille.
use super::*;
use crate::engine::controls::{
    ControlError, NativeTrace, ObservedRun, ObservedValidation, PreparedMode, PreparedSlot,
    ProbeOutcome, SourceRuntime,
};
use crate::model::costs::LegCostRecord;
use crate::research::controls::{
    Budgets, ControlMode, ControlOptions, EngineWork, Interval, PairCatalogue,
};
use crate::strategy::controls::{ControlGeometry, EntryIntervention};

fn fresh(
    source: &SourceRuntime<'_>,
    declaration: &[Box<dyn Strategy>],
) -> Result<Vec<Box<dyn Strategy>>, EngineError> {
    let strategies = (source.factory)()?;
    check_config(&strategies, &source.config)?;
    if !super::super::profiles::same_declaration(&strategies, declaration) {
        return Err(EngineError::ProfileStrategiesDiffer);
    }
    Ok(strategies)
}

struct TraceDriver<'a> {
    work: &'a mut EngineWork,
    budgets: &'a Budgets,
    trace: Vec<NativeTrace>,
}
impl step::EntryDriver for TraceDriver<'_> {
    fn before_minute(&mut self, steps: u64) -> Result<(), EngineError> {
        self.work
            .charge_engine(steps, self.budgets)
            .map_err(EngineError::ControlBudget)
    }
    fn evaluate(
        &mut self,
        _: usize,
        strategy: &mut dyn Strategy,
        ctx: &BarContext<'_>,
        out: &mut Vec<Signal>,
    ) -> Result<(), EngineError> {
        strategy.on_bar(ctx, out);
        Ok(())
    }
    fn after_events(&mut self, book: &Book, at_ms: Ts) {
        let (trades, legs) = book.trades();
        for (trade, &leg) in trades.iter().zip(legs).skip(self.trace.len()) {
            self.trace.push(NativeTrace {
                leg: usize::from(leg),
                trade: *trade,
                release_ms: at_ms,
            });
        }
    }
}

fn parts<'a>(
    source: &SourceRuntime<'a>,
    loaded: &Loaded,
    strategies: &[Box<dyn Strategy>],
    checked: &Checked,
    tf: Timeframe,
    reduced: bool,
) -> Result<
    (
        Book,
        PortfolioRunConfig,
        Option<ResearchRun<'a>>,
        Vec<LegCostRecord>,
    ),
    EngineError,
> {
    let config = &source.config;
    let mut book_config = book_config(config, loaded.window_max());
    if reduced {
        book_config.trades_capacity = 1;
    }
    let strategy = strategies.first().ok_or(EngineError::EmptyUniverse)?;
    let (book, mut run_config, legs) = if let Some((costs, profile)) = &source.costs {
        let (leg_costs, legs) = super::super::profiles::resolve_legs(config, costs, *profile, tf)?;
        let run_config = PortfolioRunConfig::with_costs(
            strategy.as_ref(),
            config,
            checked.warmup_bars,
            checked.uses_regime,
            costs.seed,
            *profile,
            &legs,
        )?;
        (
            Book::with_costs(book_config, config.instruments, &leg_costs)?,
            run_config,
            legs,
        )
    } else {
        (
            Book::new(book_config, config.instruments)?,
            PortfolioRunConfig::new(
                strategy.as_ref(),
                config,
                checked.warmup_bars,
                checked.uses_regime,
            ),
            Vec::new(),
        )
    };
    let research = source.research.map(|spec| ResearchRun {
        spec,
        decision_tf: tf,
        costs_legs: legs
            .iter()
            .map(|leg| (leg.symbol.clone(), leg.hashed()))
            .collect(),
    });
    if let Some(spec) = source.research {
        run_config = run_config.into_research(ResearchHashed::new(spec.universe_spec.clone(), tf));
    }
    Ok((book, run_config, research, legs))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::engine) fn observed(
    source: &SourceRuntime<'_>,
    loaded: &Loaded,
    shared: &Shared<'_>,
    checked: &Checked,
    declaration: &[Box<dyn Strategy>],
    tf: Timeframe,
    work: &mut EngineWork,
    budgets: &Budgets,
    index: usize,
    validate: &mut dyn FnMut(usize, ObservedRun<'_>) -> Result<ObservedValidation, String>,
) -> Result<(Vec<NativeTrace>, String), ControlError> {
    let strategies = fresh(source, declaration)?;
    let (book, run_config, research, legs) =
        parts(source, loaded, &strategies, checked, tf, false)?;
    let hash = run_config.hash().map_err(EngineError::from)?;
    let mut driver = TraceDriver {
        work,
        budgets,
        trace: Vec::new(),
    };
    let (result, cost_by_leg) = play_profile_driven(
        shared,
        strategies,
        book,
        &source.config,
        run_config,
        hash.clone(),
        research.as_ref(),
        &mut NoopObserver,
        &mut driver,
    )?;
    let validation = if let Some((_, profile)) = source.costs {
        validate(
            index,
            ObservedRun::Profile(&super::super::profiles::ProfileRun {
                profile,
                result,
                legs,
                cost_by_leg,
            }),
        )
    } else {
        validate(index, ObservedRun::Plain(&result))
    }
    .map_err(ControlError::Validation)?;
    if validation == ObservedValidation::Mismatched {
        return Err(ControlError::Unavailable(
            crate::research::controls::UnavailableReason::ObservedMismatch,
        ));
    }
    Ok((driver.trace, hash))
}

struct FlatDriver<'a> {
    geometries: Vec<Box<dyn ControlGeometry>>,
    slots: Vec<Vec<PreparedSlot>>,
    eligibility: Option<ResearchState>,
    tf: Timeframe,
    work: &'a mut EngineWork,
    budgets: &'a Budgets,
}
impl step::EntryDriver for FlatDriver<'_> {
    fn before_minute(&mut self, steps: u64) -> Result<(), EngineError> {
        self.work
            .charge_engine(steps, self.budgets)
            .map_err(EngineError::ControlBudget)
    }
    fn evaluate(
        &mut self,
        leg: usize,
        _: &mut dyn Strategy,
        ctx: &BarContext<'_>,
        _: &mut Vec<Signal>,
    ) -> Result<(), EngineError> {
        let geometry = &mut self.geometries[leg];
        // Seed avant évaluation ; la mémoire avance même quand les entrées sont refusées.
        let state = geometry.capture_state();
        if ctx.just_closed(self.tf)
            && self
                .eligibility
                .as_mut()
                .is_none_or(|s| s.refusal(leg, ctx.now_ms()).is_none())
            && let Some(signal) = geometry.entry(ctx, EntryIntervention::RemoveBtc)
        {
            if !matches!(signal, Signal::EnterLong { .. }) {
                return Err(EngineError::Strategy(
                    crate::strategy::StrategyError::UnsupportedControlGeometry {
                        id: "controls",
                        reason: "la géométrie doit produire une entrée single_lot",
                    },
                ));
            }
            let preserve = geometry.entry(ctx, EntryIntervention::PreserveBtc);
            if preserve.is_some_and(|s| s != signal) {
                return Err(EngineError::Strategy(
                    crate::strategy::StrategyError::UnsupportedControlGeometry {
                        id: "controls",
                        reason: "retirer BTC modifie l'intention d'entrée",
                    },
                ));
            }
            self.slots[leg].push(PreparedSlot {
                signal_ms: ctx.now_ms(),
                open_ms: ctx.now_ms().saturating_sub(MINUTE_MS),
                state,
                signal,
                preserve_btc: preserve.is_some(),
            });
        }
        geometry.advance_flat(ctx);
        Ok(())
    }
}

struct ProbeDriver<'a> {
    leg: usize,
    signal: Signal,
    requested: bool,
    geometry: Box<dyn ControlGeometry>,
    release_ms: Option<Ts>,
    work: &'a mut EngineWork,
    budgets: &'a Budgets,
    steps: u64,
}
impl step::EntryDriver for ProbeDriver<'_> {
    fn before_minute(&mut self, steps: u64) -> Result<(), EngineError> {
        self.work
            .charge_probe(steps, self.budgets)
            .map_err(EngineError::ControlBudget)?;
        self.steps += steps;
        Ok(())
    }
    fn evaluate(
        &mut self,
        leg: usize,
        _: &mut dyn Strategy,
        ctx: &BarContext<'_>,
        out: &mut Vec<Signal>,
    ) -> Result<(), EngineError> {
        if leg == self.leg {
            if !self.requested {
                out.push(self.signal);
                self.requested = true;
            }
            self.geometry.exits(ctx, out);
        }
        Ok(())
    }
    fn after_events(&mut self, book: &Book, at_ms: Ts) {
        if !book.trades().0.is_empty() && self.release_ms.is_none() {
            self.release_ms = Some(at_ms);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn probe(
    source: &SourceRuntime<'_>,
    loaded: &Loaded,
    shared: &Shared<'_>,
    declaration: &[Box<dyn Strategy>],
    tf: Timeframe,
    leg: usize,
    decision: usize,
    slot: &PreparedSlot,
    work: &mut EngineWork,
    budgets: &Budgets,
) -> Result<ProbeOutcome, ControlError> {
    let strategies = fresh(source, declaration)?;
    let checked = check_config(&strategies, &source.config)?;
    let mut geometry = strategies
        .get(leg)
        .ok_or(EngineError::EmptyUniverse)?
        .control_geometry()?;
    geometry.restore_state(&slot.state)?;
    let (mut book, _, research, _) = parts(source, loaded, &strategies, &checked, tf, true)?;
    let mut driver = ProbeDriver {
        leg,
        signal: slot.signal,
        requested: false,
        geometry,
        release_ms: None,
        work,
        budgets,
        steps: 0,
    };
    let mut feed = shared
        .regime
        .as_ref()
        .map(|series| RegimeFeed::at(series, slot.open_ms));
    with_runs(
        shared,
        strategies,
        &source.config,
        research.is_some(),
        Some(slot.open_ms),
        Recording::Reduced,
        |mut runs| {
            // Pas d'ordre avant le slot ; les premières échéances viennent des vraies bougies jouées.
            let mut state = source
                .research
                .map(|spec| ResearchState::new(spec, source.config.universe, tf));
            if let Some(state) = state.as_mut() {
                state.init_at(slot.open_ms);
            }
            let mut buffers = PlayBuffers::new(
                &runs,
                Ts::MIN,
                source.config.sim_rules.max_pending,
                Recording::Reduced,
            );
            while let Some(t) = next_open(&runs) {
                step::advance_minute(
                    &mut book,
                    &mut runs,
                    &shared.ranks,
                    feed.as_mut(),
                    &source.config,
                    Some(source.config.initial_cash / f64::from(source.config.max_positions)),
                    &mut buffers,
                    state.as_mut(),
                    t,
                    &mut driver,
                )?;
                if driver.requested && book.position(leg).is_none() && book.pending_len(leg) == 0 {
                    break;
                }
            }
            if next_open(&runs).is_none() {
                finish_clock(
                    &mut book,
                    &mut runs,
                    &shared.ranks,
                    &buffers,
                    state.as_ref(),
                    &mut driver,
                )?;
                let last: Vec<Option<Candle>> =
                    runs.iter().map(|r| r.candles.last().copied()).collect();
                book.close_all_at_end(&last)?;
                if let Some(last) = buffers.last_open_ms {
                    step::EntryDriver::after_events(
                        &mut driver,
                        &book,
                        last.saturating_add(MINUTE_MS),
                    );
                }
            }
            Ok(())
        },
    )?;
    let trade = book.trades().0.first().copied();
    Ok(ProbeOutcome {
        decision,
        steps: driver.steps,
        trade,
        release_ms: driver.release_ms,
    })
}

#[allow(clippy::type_complexity)]
pub(in crate::engine) fn prepare_catalogues(
    source: &SourceRuntime<'_>,
    loaded: &Loaded,
    shared: &Shared<'_>,
    declaration: &[Box<dyn Strategy>],
    tf: Timeframe,
    options: &ControlOptions,
    work: &mut EngineWork,
) -> Result<
    (
        Vec<Vec<PreparedSlot>>,
        Vec<Vec<ProbeOutcome>>,
        Vec<PreparedMode>,
    ),
    ControlError,
> {
    let strategies = fresh(source, declaration)?;
    let geometries = strategies
        .iter()
        .map(|s| s.control_geometry())
        .collect::<Result<Vec<_>, _>>()?;
    let has_btc = geometries.iter().any(|g| g.capabilities().has_btc_filter);
    let mut flat = FlatDriver {
        geometries,
        slots: vec![Vec::new(); strategies.len()],
        eligibility: source
            .research
            .map(|spec| ResearchState::new(spec, source.config.universe, tf)),
        tf,
        work,
        budgets: &options.budgets,
    };
    let checked = check_config(&strategies, &source.config)?;
    let (mut book, _, research, _) = parts(source, loaded, &strategies, &checked, tf, true)?;
    let mut feed = shared.regime.as_ref().map(RegimeFeed::new);
    with_runs(
        shared,
        strategies,
        &source.config,
        research.is_some(),
        None,
        Recording::Reduced,
        |mut runs| {
            let mut state = source
                .research
                .map(|spec| ResearchState::new(spec, source.config.universe, tf));
            let mut buffers = PlayBuffers::new(
                &runs,
                Ts::MIN,
                source.config.sim_rules.max_pending,
                Recording::Reduced,
            );
            play(
                &mut book,
                &mut runs,
                &shared.ranks,
                feed.as_mut(),
                &source.config,
                Some(source.config.initial_cash / f64::from(source.config.max_positions)),
                &mut buffers,
                state.as_mut(),
                &mut flat,
            )
        },
    )?;
    let slots = flat.slots;
    let mut probes = Vec::with_capacity(slots.len());
    for (leg, pair_slots) in slots.iter().enumerate() {
        let mut pair = Vec::with_capacity(pair_slots.len());
        for (decision, slot) in pair_slots.iter().enumerate() {
            pair.push(probe(
                source,
                loaded,
                shared,
                declaration,
                tf,
                leg,
                decision,
                slot,
                work,
                &options.budgets,
            )?);
        }
        probes.push(pair);
    }
    let mut modes = Vec::new();
    let mut effective_catalogues = BTreeMap::new();
    for &mode in &options.modes {
        let effective = mode.effective(has_btc);
        if let Some(catalogues) = effective_catalogues.get(&effective) {
            modes.push(PreparedMode {
                mode,
                alias_of: (mode != effective).then_some(effective),
                catalogues: std::sync::Arc::clone(catalogues),
            });
            continue;
        }
        let mut catalogues = Vec::new();
        for (leg, pair_slots) in slots.iter().enumerate() {
            let admissible =
                |slot: &PreparedSlot| effective == ControlMode::RemoveBtc || slot.preserve_btc;
            let mut intervals = Vec::new();
            for outcome in &probes[leg] {
                let slot = &pair_slots[outcome.decision];
                if !admissible(slot) {
                    continue;
                }
                let (Some(trade), Some(release_ms)) = (&outcome.trade, outcome.release_ms) else {
                    continue;
                };
                let next_decision = pair_slots
                    .partition_point(|s| s.signal_ms < release_ms)
                    .max(outcome.decision + 1);
                let retained = source.research.is_none_or(|spec| {
                    !crate::model::blocks::is_purged(
                        trade.entry_signal_ms,
                        trade.exit_ms,
                        spec.decision_end_ms,
                        trade.forced_close,
                    )
                });
                let fill_year = i32::try_from(
                    crate::store::time::civil_from_days(trade.entry_ms.div_euclid(86_400_000)).0,
                )
                .map_err(|_| ControlError::InvalidInput("année de fill hors borne".into()))?;
                let clocks_hash=crate::model::hash::hash_json(&serde_json::json!({"signal_ms":trade.entry_signal_ms,"entry_order_ms":trade.entry_order_ms,"entry_ms":trade.entry_ms,"exit_signal_ms":trade.exit_signal_ms,"exit_order_ms":trade.exit_order_ms,"exit_ms":trade.exit_ms,"release_ms":release_ms,"exit_reason":trade.exit_reason,"retained":retained})).map_err(EngineError::from)?;
                let id = u32::try_from(outcome.decision)
                    .map_err(|_| ControlError::InvalidInput("trop de slots".into()))?;
                intervals.push(Interval {
                    id,
                    decision: outcome.decision,
                    signal_ms: slot.signal_ms,
                    entry_ms: trade.entry_ms,
                    release_ms,
                    next_decision,
                    fill_year,
                    retained,
                    clocks_hash,
                });
            }
            let catalogue = PairCatalogue {
                symbol: source.config.universe[leg].clone(),
                intervals,
            };
            catalogue
                .validate()
                .map_err(|e| ControlError::Numeric(e.into()))?;
            catalogues.push(catalogue);
        }
        let catalogues = std::sync::Arc::new(catalogues);
        effective_catalogues.insert(effective, std::sync::Arc::clone(&catalogues));
        modes.push(PreparedMode {
            mode,
            alias_of: (mode != effective).then_some(effective),
            catalogues,
        });
    }
    Ok((slots, probes, modes))
}
