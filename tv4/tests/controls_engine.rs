mod controls_common;
mod portfolio_common;
use controls_common::{Fixture, dataset};
use portfolio_common::{MINUTE, T0, config, flat_series, instrument, names};
use std::collections::BTreeMap;
use tv4::engine::controls::{
    BatchInputs, NoopPhaseObserver, ObservedRun, ObservedValidation, SourceRuntime,
    with_prepared_batch,
};
use tv4::research::controls::{Budgets, ControlMode, ControlOptions};

#[test]
fn prepares_once_and_probes_start_at_slot() {
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let data = dataset(
        universe
            .iter()
            .map(|s| (s.clone(), flat_series(0, 20, 100.0, 1e6)))
            .collect(),
    );
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 2 * MINUTE,
        limit: None,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(2));
    let sources = (0..2)
        .map(|_| SourceRuntime {
            config: config(&universe, &instruments, 300.0),
            costs: None,
            research: None,
            quotas: BTreeMap::new(),
            factory: &factory as _,
        })
        .collect();
    let mut calls = 0;
    with_prepared_batch(
        BatchInputs {
            dataset: data,
            sources,
            options: ControlOptions {
                seed: 1,
                replications: 20,
                modes: vec![ControlMode::PreserveBtc, ControlMode::RemoveBtc],
                budgets: Budgets::default(),
            },
        },
        &mut |_, run| {
            calls += 1;
            let ObservedRun::Plain(result) = run else {
                panic!("plain")
            };
            assert_eq!(result.initial_cash.to_bits(), 300.0f64.to_bits());
            Ok(ObservedValidation::Matched)
        },
        &mut NoopPhaseObserver,
        |batch| {
            assert_eq!(batch.prep.candles_converted, 2);
            assert_eq!(batch.prep.mtf_builds, 2);
            assert_eq!(batch.prep.rank_tables_built, 1);
            assert_eq!(batch.sources.len(), 2);
            assert!(std::sync::Arc::ptr_eq(
                &batch.sources[0].probes,
                &batch.sources[1].probes
            ));
            assert!(std::sync::Arc::ptr_eq(
                &batch.sources[0].modes[0].catalogues,
                &batch.sources[0].modes[1].catalogues
            ));
            assert_eq!(batch.sources[0].modes.len(), 2);
            assert_eq!(
                batch.sources[0].modes[1].alias_of,
                Some(ControlMode::PreserveBtc)
            );
            let probes = &batch.sources[0].probes;
            assert_eq!(probes[0].len(), 20);
            assert_eq!(
                probes[0].last().unwrap().steps,
                2,
                "dernière minute = deux jambes, sans préfixe"
            );
            let cats = &batch.sources[0].modes[0].catalogues;
            assert_eq!(cats.len(), 2);
            assert_eq!(cats[1].intervals[0].decision, 0);
            assert_eq!(
                probes[1][0].trade.as_ref().unwrap().qty,
                0.375,
                "plafond C0/8 préservé"
            );
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(calls, 2);
}

#[test]
fn observed_is_identical_with_real_research_renderer_and_exact_cash() {
    use controls_common::Research;
    use tv4::engine::{PortfolioInputs, run_portfolio_research};
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let data = dataset(vec![(universe[0].clone(), flat_series(0, 20, 100.0, 1e6))]);
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 2 * MINUTE,
        limit: None,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let research = Research::new(T0, T0 + 20 * MINUTE);
    let spec = research.spec();
    let cash = f64::from_bits(300.0f64.to_bits() + 1);
    let cfg = config(&universe, &instruments, cash);
    let expected = run_portfolio_research(
        PortfolioInputs {
            series: &data.series,
            strategies: fixture.set(1),
            config: cfg.clone(),
        },
        &spec,
    )
    .unwrap();
    let bytes = research.render(&expected, &data);
    with_prepared_batch(
        BatchInputs {
            dataset: data.clone(),
            sources: vec![SourceRuntime {
                config: cfg,
                costs: None,
                research: Some(&spec),
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(3),
        },
        &mut |_, run| {
            let ObservedRun::Plain(result) = run else {
                panic!("plain")
            };
            let actual = research.render(result, &data);
            assert_eq!(result.initial_cash.to_bits(), cash.to_bits());
            assert_eq!(actual.metrics_bytes, bytes.metrics_bytes);
            assert_eq!(actual.trades_bytes, bytes.trades_bytes);
            assert_eq!(actual.equity_bytes, bytes.equity_bytes);
            Ok(ObservedValidation::Matched)
        },
        &mut NoopPhaseObserver,
        |batch| {
            let trace = &batch.sources[0].observed_trace;
            assert_eq!(trace.len(), 1);
            assert_eq!(trace[0].trade.entry_ms, expected.trades[0].entry_ms);
            assert_eq!(trace[0].release_ms, trace[0].trade.exit_ms + MINUTE);
            let last = &batch.sources[0].modes[0].catalogues[0]
                .intervals
                .last()
                .unwrap();
            assert!(!last.retained, "purge native terminale");
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn maker_ttl_crosses_year_and_never_filled_is_distinct() {
    use portfolio_common::flat_bar;
    use tv4::store::time::days_from_civil;
    let midnight = days_from_civil(2024, 1, 1) * 86_400_000;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let mut bars: Vec<_> = (-4..8)
        .map(|m| flat_bar(midnight + m * MINUTE, 100.0, 1e6))
        .collect();
    bars[6].low = 98.0; // premier passage sous la limite le 1er janvier, sans annulation à minuit.
    let data = dataset(vec![(universe[0].clone(), bars)]);
    let fixture = Fixture {
        signal_ms: midnight - 2 * MINUTE,
        hold_ms: MINUTE,
        limit: Some((99.0, 5)),
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    with_prepared_batch(
        BatchInputs {
            dataset: data,
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(4),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            let source = &batch.sources[0];
            let interval = source.modes[0].catalogues[0]
                .intervals
                .iter()
                .find(|i| i.signal_ms == midnight - 2 * MINUTE)
                .unwrap();
            assert_eq!(interval.fill_year, 2024);
            assert_eq!(interval.entry_ms, midnight + 2 * MINUTE);
            let outcome = source.probes[0]
                .iter()
                .find(|p| p.decision == interval.decision)
                .unwrap();
            assert_eq!(
                outcome.trade.as_ref().unwrap().entry_liquidity,
                tv4::sim::Liquidity::Maker
            );
            assert!(source.probes[0].iter().any(|p| p.trade.is_none()));
            assert!(source.modes[0].catalogues[0].intervals.len() < source.slots[0].len());
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn quote_gap_release_is_processing_close_and_terminal_short_leg_is_global() {
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 1000 * MINUTE,
        limit: None,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(2));
    let x = flat_series(0, 4, 100.0, 1e6);
    let mut y = flat_series(0, 4, 100.0, 1e6);
    y.extend(flat_series(10, 2, 100.0, 1e6));
    for (gap, expected_release, forced) in [
        (3 * MINUTE, T0 + 11 * MINUTE, false),
        (1000 * MINUTE, T0 + 12 * MINUTE, true),
    ] {
        let data = dataset(vec![
            (universe[0].clone(), x.clone()),
            (universe[1].clone(), y.clone()),
        ]);
        let mut cfg = config(&universe, &instruments, 300.0);
        cfg.quote_gap_ms = gap;
        with_prepared_batch(
            BatchInputs {
                dataset: data,
                sources: vec![SourceRuntime {
                    config: cfg,
                    costs: None,
                    research: None,
                    quotas: BTreeMap::new(),
                    factory: &factory,
                }],
                options: ControlOptions::new(2),
            },
            &mut |_, _| Ok(ObservedValidation::Matched),
            &mut NoopPhaseObserver,
            |batch| {
                let probe = &batch.sources[0].probes[0][0];
                let trade = probe.trade.as_ref().unwrap();
                assert_eq!(probe.release_ms, Some(expected_release));
                assert_eq!(trade.forced_close, forced);
                assert_eq!(
                    trade.exit_ms,
                    if forced {
                        T0 + 4 * MINUTE
                    } else {
                        T0 + 7 * MINUTE
                    }
                );
                assert!(expected_release > trade.exit_ms);
                let trace = batch.sources[0]
                    .observed_trace
                    .iter()
                    .find(|t| t.leg == 0)
                    .unwrap();
                assert_eq!(trace.release_ms, expected_release);
                Ok(())
            },
        )
        .unwrap();
    }
}

#[test]
fn observed_mismatch_and_technical_error_stop_before_catalogues() {
    use tv4::engine::controls::{ControlError, ControlPhase, PhaseObserver};
    use tv4::research::controls::UnavailableReason;
    #[derive(Default)]
    struct Phases(Vec<ControlPhase>);
    impl PhaseObserver for Phases {
        fn phase_started(&mut self, phase: ControlPhase) {
            self.0.push(phase);
        }
    }
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: MINUTE,
        limit: None,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    for technical in [false, true] {
        let mut phases = Phases::default();
        let mut callbacks = 0;
        let error = with_prepared_batch(
            BatchInputs {
                dataset: dataset(vec![(universe[0].clone(), flat_series(0, 5, 100.0, 1e6))]),
                sources: vec![SourceRuntime {
                    config: config(&universe, &instruments, 300.0),
                    costs: None,
                    research: None,
                    quotas: BTreeMap::new(),
                    factory: &factory,
                }],
                options: ControlOptions::new(1),
            },
            &mut |_, _| {
                callbacks += 1;
                if technical {
                    Err("rendu cassé".into())
                } else {
                    Ok(ObservedValidation::Mismatched)
                }
            },
            &mut phases,
            |_| -> Result<(), ControlError> { panic!("aucune continuation") },
        )
        .unwrap_err();
        assert_eq!(callbacks, 1);
        assert!(!phases.0.contains(&ControlPhase::Probes));
        if technical {
            assert!(matches!(error, ControlError::Validation(_)));
        } else {
            assert!(matches!(
                error,
                ControlError::Unavailable(UnavailableReason::ObservedMismatch)
            ));
        }
    }
}

#[test]
fn equal_owned_recipes_are_admitted_and_real_cost_difference_is_rejected() {
    use controls_common::Research;
    use tv4::engine::CostSetup;
    use tv4::engine::controls::ControlError;
    use tv4::model::costs::{CostProfileName, CostsFile};
    let universe = names(&["BTC-USD", "ETH-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 2 * MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(2));
    let file = CostsFile::from_toml_str(include_str!("fixtures/costs_c1.toml")).unwrap();
    let mut other = file.clone();
    let research = Research::new(T0, T0 + 10 * MINUTE);
    let second = Research::new(T0, T0 + 10 * MINUTE);
    let spec = research.spec();
    let other_spec = second.spec();
    for different in [false, true] {
        if different {
            other.stress_calibration_ref.push_str("différent");
        }
        let sources = [(&file, &spec), (&other, &other_spec)]
            .iter()
            .map(|(file, spec)| SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: Some((
                    CostSetup {
                        file,
                        measures: None,
                        seed: 123,
                    },
                    CostProfileName::Baseline,
                )),
                research: Some(*spec),
                quotas: BTreeMap::new(),
                factory: &factory as _,
            })
            .collect();
        let result = with_prepared_batch(
            BatchInputs {
                dataset: dataset(
                    universe
                        .iter()
                        .map(|s| (s.clone(), flat_series(0, 10, 100.0, 1e6)))
                        .collect(),
                ),
                sources,
                options: ControlOptions::new(9),
            },
            &mut |_, run| {
                assert!(matches!(run, ObservedRun::Profile(_)));
                Ok(ObservedValidation::Matched)
            },
            &mut NoopPhaseObserver,
            |batch| {
                assert!(std::sync::Arc::ptr_eq(
                    &batch.sources[0].probes,
                    &batch.sources[1].probes
                ));
                let source = &batch.sources[0];
                for leg in 0..2 {
                    let probe = source.probes[leg][0].trade.as_ref().unwrap();
                    let observed = source.observed_trace.iter().find(|t| t.leg == leg).unwrap();
                    assert_eq!(
                        probe.entry_price.to_bits(),
                        observed.trade.entry_price.to_bits()
                    );
                    assert_eq!(
                        probe.exit_price.to_bits(),
                        observed.trade.exit_price.to_bits()
                    );
                    assert_eq!(probe.qty.to_bits(), observed.trade.qty.to_bits());
                }
                Ok(())
            },
        );
        if different {
            assert!(matches!(result, Err(ControlError::InvalidInput(_))));
        } else {
            result.unwrap();
        }
    }
}

#[test]
fn engine_and_probe_budgets_are_debited_before_the_next_minute() {
    use tv4::engine::controls::ControlError;
    use tv4::research::controls::UnavailableReason;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    for (total, probe, reason) in [
        (0, u64::MAX, UnavailableReason::GlobalEngineBudget),
        (10, 0, UnavailableReason::PreparationBudget),
        (11, u64::MAX, UnavailableReason::GlobalEngineBudget),
    ] {
        let mut options = ControlOptions::new(1);
        options.budgets.total_engine_steps = total;
        options.budgets.probe_steps_per_geometry = probe;
        let mut callbacks = 0;
        let result = with_prepared_batch(
            BatchInputs {
                dataset: dataset(vec![(universe[0].clone(), flat_series(0, 5, 100.0, 1e6))]),
                sources: vec![SourceRuntime {
                    config: config(&universe, &instruments, 300.0),
                    costs: None,
                    research: None,
                    quotas: BTreeMap::new(),
                    factory: &factory,
                }],
                options,
            },
            &mut |_, _| {
                callbacks += 1;
                Ok(ObservedValidation::Matched)
            },
            &mut NoopPhaseObserver,
            |_| Ok(()),
        );
        assert!(matches!(result,Err(ControlError::Unavailable(actual)) if actual==reason));
        assert_eq!(callbacks, if total == 0 { 0 } else { 1 });
    }
}

#[test]
fn unsupported_geometry_is_rejected_before_observed() {
    use portfolio_common::Scripted;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let factory = || {
        Ok(vec![
            Box::new(Scripted::new(Vec::new()).0) as Box<dyn tv4::strategy::Strategy>
        ])
    };
    let result = with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 5, 100.0, 1e6))]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(1),
        },
        &mut |_, _| panic!("refus avant Observed"),
        &mut NoopPhaseObserver,
        |_| Ok(()),
    );
    assert!(matches!(
        result,
        Err(tv4::engine::controls::ControlError::Engine(
            tv4::engine::EngineError::Strategy(
                tv4::strategy::StrategyError::UnsupportedControlGeometry { .. }
            )
        ))
    ));
}

#[test]
fn retained_fill_without_initial_risk_is_kept_without_inventing_zero_r() {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: MINUTE,
        stop: None,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 10, 100.0, 1e6))]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(1),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            let source = &batch.sources[0];
            assert!(source.modes[0].catalogues[0].intervals[0].retained);
            assert!(
                source.probes[0][0]
                    .trade
                    .as_ref()
                    .unwrap()
                    .stop_at_entry
                    .is_none()
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn btc_modes_use_native_closes_across_missing_day_and_missing_terminal_minute() {
    use portfolio_common::{DAY, flat_bar};
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let mut btc: Vec<_> = (0..200)
        .map(|day| {
            flat_bar(
                T0 + (day + 1) * DAY - MINUTE,
                if day == 199 { 200.0 } else { 100.0 },
                1e6,
            )
        })
        .collect();
    // Jour 200 absent. Jour 201 a une minute terminale absente et une clôture très basse.
    btc.push(flat_bar(T0 + 202 * DAY - 2 * MINUTE, 10.0, 1e6));
    btc.push(flat_bar(T0 + 202 * DAY, 10.0, 1e6));
    let opens = [
        T0 + 200 * DAY - MINUTE,
        T0 + 200 * DAY,
        T0 + 201 * DAY - MINUTE,
        T0 + 202 * DAY - MINUTE,
        T0 + 202 * DAY,
        T0 + 202 * DAY + MINUTE,
    ];
    let x = opens
        .into_iter()
        .map(|at| flat_bar(at, 100.0, 1e6))
        .collect();
    let fixture = Fixture {
        signal_ms: T0 + 200 * DAY,
        hold_ms: MINUTE,
        btc: true,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), x), ("BTC-USD".into(), btc)]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(7),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            assert_eq!(batch.prep.candles_converted, 2);
            assert_eq!(batch.prep.regime_series_built, 1);
            let source = &batch.sources[0];
            assert!(source.modes.iter().all(|m| m.alias_of.is_none()));
            let slots = &source.slots[0];
            assert!(
                slots
                    .iter()
                    .find(|s| s.signal_ms == T0 + 201 * DAY)
                    .unwrap()
                    .preserve_btc,
                "jour absent : conserver la dernière 1d close native"
            );
            assert!(
                slots
                    .iter()
                    .find(|s| s.signal_ms == T0 + 202 * DAY)
                    .unwrap()
                    .preserve_btc,
                "minute terminale absente : 1d pas encore close"
            );
            assert!(
                !slots
                    .iter()
                    .find(|s| s.signal_ms == T0 + 202 * DAY + MINUTE)
                    .unwrap()
                    .preserve_btc,
                "première minute suivante réellement close"
            );
            assert_eq!(
                source.probes[0].len(),
                slots.len(),
                "un seul passage de sondes pour les deux projections"
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn research_universe_and_quality_filter_slots_at_native_submission_time() {
    use controls_common::Research;
    use portfolio_common::flat_bar;
    use tv4::model::universe::{Member, UniverseQuarter};
    use tv4::model::{QualitySchedule, QuarterExclusion, Timeframe, UniverseSchedule};
    use tv4::store::time::days_from_civil;
    let start = days_from_civil(2024, 3, 31) * 86_400_000 + 15 * 60 * MINUTE;
    let end = start + 18 * 60 * MINUTE;
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let mut research = Research::new(start, end);
    research.quality = QualitySchedule::from_exclusions(BTreeMap::from([(
        "Y-USD".into(),
        vec![QuarterExclusion {
            quarter: "2024-Q2".into(),
            tfs: vec![Timeframe::H4],
        }],
    )]))
    .unwrap();
    let schedule = UniverseSchedule::new(vec![
        UniverseQuarter::new(
            "2024-Q1",
            vec![Member {
                symbol: "X-USD".into(),
                rank: 1,
            }],
        )
        .unwrap(),
        UniverseQuarter::new(
            "2024-Q2",
            vec![Member {
                symbol: "Y-USD".into(),
                rank: 1,
            }],
        )
        .unwrap(),
    ])
    .unwrap();
    let mut spec = research.spec();
    spec.universe = Some(&schedule);
    let fixture = Fixture {
        signal_ms: start + 60 * MINUTE,
        hold_ms: 4 * 60 * MINUTE,
        tf: Timeframe::H4,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(2));
    let bars: Vec<_> = (0..18 * 60)
        .map(|m| flat_bar(start + m * MINUTE, 100.0, 1e6))
        .collect();
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(universe.iter().map(|s| (s.clone(), bars.clone())).collect()),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: Some(&spec),
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(8),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            let source = &batch.sources[0];
            assert_eq!(source.slots[0].len(), 2);
            assert!(source.slots[1].is_empty());
            assert!(
                source.slots[0]
                    .iter()
                    .all(|s| s.signal_ms < days_from_civil(2024, 4, 1) * 86_400_000)
            );
            assert_eq!(source.slots[0][0].signal_ms, start + 60 * MINUTE);
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn sma_geometry_prepares_indicators_once_and_captures_evaluated_memory() {
    use portfolio_common::flat_bar;
    use tv4::strategy::{controls::GeometryState, registry};
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let bars = (0..70)
        .map(|m| {
            let mut c = flat_bar(T0 + m * MINUTE, 100.0 + (m % 8) as f64, 1e6);
            c.low -= 1.0;
            c.high += 1.0;
            c
        })
        .collect();
    let factory = || {
        Ok(vec![registry::build(
            "sma_cross",
            &toml::from_str::<toml::Value>("tf='1m'\nfast=2\nslow=3\nstop_atr_mult=2.0").unwrap(),
        )?])
    };
    let sources = (0..2)
        .map(|_| SourceRuntime {
            config: config(&universe, &instruments, 300.0),
            costs: None,
            research: None,
            quotas: BTreeMap::new(),
            factory: &factory as _,
        })
        .collect();
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), bars)]),
            sources,
            options: ControlOptions::new(8),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            assert_eq!(batch.prep.candles_converted, 1);
            assert_eq!(batch.prep.mtf_builds, 1);
            assert_eq!(batch.prep.indicator_columns_computed, 3);
            assert_eq!(batch.prep.rank_tables_built, 1);
            let source = &batch.sources[0];
            assert_eq!(
                source.slots[0][0].state,
                GeometryState::SmaCross { prev_diff: None }
            );
            assert!(matches!(
                source.slots[0][1].state,
                GeometryState::SmaCross { prev_diff: Some(_) }
            ));
            for trace in &source.observed_trace {
                let slot = source.slots[0]
                    .iter()
                    .position(|s| s.signal_ms == trace.trade.entry_signal_ms)
                    .unwrap();
                let probe = &source.probes[0][slot];
                let trade = probe.trade.as_ref().unwrap();
                assert_eq!(trade.entry_ms, trace.trade.entry_ms);
                assert_eq!(trade.exit_ms, trace.trade.exit_ms);
                assert_eq!(probe.release_ms, Some(trace.release_ms));
            }
            assert!(!source.observed_trace.is_empty());
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn entry_and_stop_in_same_minute_are_catalogued_and_allow_same_close_next_slot() {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 10 * MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let mut bars = flat_series(0, 8, 100.0, 1e6);
    bars[2].low = 80.0;
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), bars)]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(1),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            let source = &batch.sources[0];
            let interval = &source.modes[0].catalogues[0].intervals[0];
            assert_eq!(interval.entry_ms, T0 + 2 * MINUTE);
            assert_eq!(interval.release_ms, T0 + 3 * MINUTE);
            assert_eq!(
                source.slots[0][interval.next_decision].signal_ms,
                interval.release_ms
            );
            assert_eq!(source.observed_trace[0].release_ms, interval.release_ms);
            assert_eq!(
                source.probes[0][0].trade.as_ref().unwrap().exit_reason,
                tv4::sim::ExitReason::Stop
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn compounding_is_rejected_before_conversion() {
    use tv4::engine::controls::{ControlError, ControlPhase, PhaseObserver};
    #[derive(Default)]
    struct PreparationCount(usize);
    impl PhaseObserver for PreparationCount {
        fn phase_started(&mut self, phase: ControlPhase) {
            if phase == ControlPhase::Preparation {
                self.0 += 1;
            }
        }
    }
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let mut cfg = config(&universe, &instruments, 300.0);
    cfg.sizing = tv4::sim::SizingMode::Compounding;
    let mut count = PreparationCount::default();
    let result = with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 5, 100.0, 1e6))]),
            sources: vec![SourceRuntime {
                config: cfg,
                costs: None,
                research: None,
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(1),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut count,
        |_| Ok(()),
    );
    assert!(matches!(result, Err(ControlError::InvalidInput(_))));
    assert_eq!(count.0, 0, "aucune phase de conversion/préparation");
}

#[test]
fn different_geometries_share_conversion_and_keep_their_own_preparation() {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let a = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: MINUTE,
        ..Default::default()
    };
    let b = Fixture {
        hold_ms: 2 * MINUTE,
        ..a.clone()
    };
    let first = || Ok(a.set(1));
    let second = || Ok(b.set(1));
    let sources = vec![
        SourceRuntime {
            config: config(&universe, &instruments, 300.0),
            costs: None,
            research: None,
            quotas: BTreeMap::new(),
            factory: &first,
        },
        SourceRuntime {
            config: config(&universe, &instruments, 300.0),
            costs: None,
            research: None,
            quotas: BTreeMap::new(),
            factory: &second,
        },
    ];
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 10, 100.0, 1e6))]),
            sources,
            options: ControlOptions::new(1),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            assert_eq!(batch.prep.candles_converted, 1);
            assert_eq!(batch.prep.mtf_builds, 2);
            assert_eq!(batch.prep.rank_tables_built, 2);
            assert_ne!(
                batch.sources[0].parent_config_hash,
                batch.sources[1].parent_config_hash
            );
            assert_ne!(
                batch.sources[0].geometry_hash,
                batch.sources[1].geometry_hash
            );
            assert!(!std::sync::Arc::ptr_eq(
                &batch.sources[0].probes,
                &batch.sources[1].probes
            ));
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn partial_work_survives_budget_failure_before_continuation() {
    use tv4::engine::controls::{ControlError, with_prepared_batch_with_work};
    use tv4::research::controls::{EngineWork, UnavailableReason};
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    for (total, local, expected_total, expected_probe, reason) in [
        (11, u64::MAX, 11, 1, UnavailableReason::GlobalEngineBudget),
        (100, 0, 10, 0, UnavailableReason::PreparationBudget),
    ] {
        let mut options = ControlOptions::new(1);
        options.budgets.total_engine_steps = total;
        options.budgets.probe_steps_per_geometry = local;
        let mut work = EngineWork::default();
        let result = with_prepared_batch_with_work(
            BatchInputs {
                dataset: dataset(vec![(universe[0].clone(), flat_series(0, 5, 100.0, 1e6))]),
                sources: vec![SourceRuntime {
                    config: config(&universe, &instruments, 300.0),
                    costs: None,
                    research: None,
                    quotas: BTreeMap::new(),
                    factory: &factory,
                }],
                options,
            },
            &mut work,
            &mut |_, _| Ok(ObservedValidation::Matched),
            &mut NoopPhaseObserver,
            |_| -> Result<(), ControlError> { panic!("pas de continuation après le budget") },
        );
        assert!(matches!(result,Err(ControlError::Unavailable(actual)) if actual==reason));
        assert_eq!(work.total_steps, expected_total);
        assert_eq!(work.probe_steps, expected_probe);
        assert_eq!(work.replay_steps, 0);
    }
}

#[test]
fn terminal_release_ignores_later_raw_candles_of_never_live_leg() {
    use controls_common::Research;
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let fixture = Fixture {
        signal_ms: T0 + 4 * MINUTE,
        hold_ms: 100 * MINUTE,
        warmup: 3,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(2));
    let research = Research::new(T0, T0 + 30 * MINUTE);
    let spec = research.spec();
    let data = dataset(vec![
        (universe[0].clone(), flat_series(0, 10, 100.0, 1e6)),
        (universe[1].clone(), flat_series(20, 2, 100.0, 1e6)),
    ]);
    with_prepared_batch(
        BatchInputs {
            dataset: data,
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: Some(&spec),
                quotas: BTreeMap::new(),
                factory: &factory,
            }],
            options: ControlOptions::new(1),
        },
        &mut |_, run| {
            let ObservedRun::Plain(result) = run else {
                panic!("plain")
            };
            assert_eq!(result.legs[1].rows, 0);
            assert_eq!(result.window_last_open_ms, T0 + 9 * MINUTE);
            Ok(ObservedValidation::Matched)
        },
        &mut NoopPhaseObserver,
        |batch| {
            let source = &batch.sources[0];
            assert!(source.slots[1].is_empty());
            let observed = &source.observed_trace[0];
            let probe = &source.probes[0][0];
            assert_eq!(observed.release_ms, T0 + 10 * MINUTE);
            assert_eq!(probe.release_ms, Some(observed.release_ms));
            assert_eq!(
                probe.trade.as_ref().unwrap().exit_ms,
                observed.trade.exit_ms
            );
            assert_eq!(probe.trade.as_ref().unwrap().exit_ms, T0 + 10 * MINUTE);
            Ok(())
        },
    )
    .unwrap();
}
