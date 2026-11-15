mod controls_common;
mod portfolio_common;
use controls_common::{Fixture, dataset};
use portfolio_common::{MINUTE, T0, config, flat_series, instrument, names};
use std::collections::BTreeMap;
use std::sync::Arc;
use tv4::engine::controls::{
    BatchInputs, NoopPhaseObserver, ObservedValidation, SourceRuntime, replay_prepared,
    with_prepared_batch,
};
use tv4::research::controls::{AnnualQuota, CellAvailability, ControlMode, ControlOptions};

#[test]
fn native_replays_reduce_and_share_exact_mode_aliases() {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 2 * MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let mut options = ControlOptions::new(17);
    options.replications = 20;
    let mut observed_oracle = None;
    let summaries = with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 30, 100.0, 1e6))]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::from([(
                    universe[0].clone(),
                    vec![AnnualQuota {
                        year: 2024,
                        filled: 1,
                        retained: 1,
                    }],
                )]),
                factory: &factory,
            }],
            options,
        },
        &mut |_, run| {
            let tv4::engine::controls::ObservedRun::Plain(result) = run else {
                panic!("plain")
            };
            let t = &result.trades[0];
            observed_oracle =
                Some(t.pnl_net / (t.qty * (t.entry_price - t.stop_at_entry.unwrap())));
            Ok(ObservedValidation::Matched)
        },
        &mut NoopPhaseObserver,
        |batch| {
            let result = replay_prepared(batch)?;
            assert_eq!(
                batch.work.replay_steps,
                20 * 30,
                "l'alias n'exécute aucun Book supplémentaire"
            );
            Ok(result)
        },
    )
    .unwrap();
    let source = &summaries[0];
    assert_eq!(source.observed_score, observed_oracle);
    assert_eq!(source.modes[0].mode, ControlMode::PreserveBtc);
    assert!(Arc::ptr_eq(&source.modes[0].cells, &source.modes[1].cells));
    assert!(Arc::ptr_eq(
        &source.modes[0].pair_supports,
        &source.modes[1].pair_supports
    ));
    assert_eq!(source.modes[0].planning_reason, None);
    assert_eq!(source.modes[0].pair_supports[0].symbol, "X-USD");
    assert_eq!(
        source.modes[0].pair_supports[0].cardinality,
        Some(source.modes[0].catalogues[0].intervals.len().to_string())
    );
    assert!(Arc::ptr_eq(
        &source.modes[0].catalogues,
        &source.modes[1].catalogues
    ));
    assert_eq!(source.modes[0].cells.len(), 20);
    for cell in source.modes[0].cells.iter() {
        assert_eq!(cell.availability, CellAvailability::Complete);
        assert_eq!(cell.attempts, 1);
        assert_eq!(cell.accepted_attempt, Some(0));
        assert_eq!(cell.months[0].count, 1);
        assert!(cell.quota_checks.iter().all(|q| q.conformant));
        assert_eq!(cell.replay_trace_hash.len(), 64);
    }
}

fn dense(variable: bool) -> Vec<tv4::research::controls::SourceSummary> {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: if variable { 30 * MINUTE } else { 2 * MINUTE },
        observed_every_ms: Some(10 * MINUTE),
        entry_until_ms: Some(T0 + 1001 * MINUTE),
        variable_hold: variable,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let mut cfg = config(&universe, &instruments, 1_000_000.0);
    cfg.max_positions = 1;
    if variable {
        cfg.sim_rules.activation_delay_bars = 0;
    }
    let mut options = ControlOptions::new(73);
    options.replications = 200;
    options.modes = vec![ControlMode::PreserveBtc];
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(
                universe[0].clone(),
                flat_series(0, 1010, 100.0, 1e9),
            )]),
            sources: vec![SourceRuntime {
                config: cfg,
                costs: None,
                research: None,
                quotas: BTreeMap::from([(
                    universe[0].clone(),
                    vec![AnnualQuota {
                        year: 2024,
                        filled: 100,
                        retained: 100,
                    }],
                )]),
                factory: &factory,
            }],
            options,
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            let cat = &batch.sources[0].modes[0].catalogues[0];
            assert_eq!(cat.intervals.len(), 1000);
            let durations: std::collections::BTreeSet<_> = cat
                .intervals
                .iter()
                .map(|i| (i.release_ms - i.signal_ms) / MINUTE)
                .collect();
            assert_eq!(
                durations,
                if variable {
                    (2..=8).collect()
                } else {
                    std::collections::BTreeSet::from([5])
                }
            );
            replay_prepared(batch)
        },
    )
    .unwrap()
}
#[test]
fn dense_book_1000_slots_q100_200_seeds_have_no_intrapair_rejections() {
    let results = dense(false);
    let cells = &results[0].modes[0].cells;
    // Calendriers de100 dates distantes de5 parmi1000 : C(604,100), en entier exact.
    let mut cardinality = num_bigint::BigUint::from(1_u32);
    for i in 1_u32..=100 {
        cardinality *= 504 + i;
        cardinality /= i;
    }
    assert_eq!(
        results[0].modes[0].pair_supports[0].cardinality,
        Some(cardinality.to_str_radix(10))
    );
    assert_eq!(cells.len(), 200);
    for cell in cells.iter() {
        assert_eq!(cell.availability, CellAvailability::Complete);
        assert_eq!(cell.attempts, 1);
        assert_eq!(cell.months[0].count, 100);
        assert_eq!(cell.months.iter().map(|m| m.count).sum::<u32>(), 100);
        let monthly_sum: f64 = cell.months.iter().map(|m| m.r_sum).sum();
        let tolerance = 4.0 * f64::EPSILON * 100.0 * monthly_sum.abs();
        assert!((monthly_sum - cell.score.unwrap() * 100.0).abs() <= tolerance);
        assert!(cell.rejections.is_empty());
    }
    assert!(
        cells
            .iter()
            .map(|c| &c.replay_trace_hash)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            >= 190
    );
}
#[test]
fn variable_native_occupation_2_to_8_ignores_declared_bound30() {
    let results = dense(true);
    assert!(results[0].modes[0].cells.iter().all(|c| c.attempts == 1
        && c.months[0].count == 100
        && c.availability == CellAvailability::Complete));
}

fn scenario(
    fixture: Fixture,
    pairs: usize,
    maxpos: u16,
    minqty: f64,
    volume: f64,
    options: ControlOptions,
    alter: impl FnOnce(&mut tv4::engine::controls::PreparedBatch<'_, '_>),
) -> Result<Vec<tv4::research::controls::SourceSummary>, tv4::engine::controls::ControlError> {
    let universe = names(&["X-USD", "Y-USD"][..pairs]);
    let instruments = vec![instrument(minqty); pairs];
    let factory = || Ok(fixture.set(pairs));
    let mut cfg = config(&universe, &instruments, 300.0);
    cfg.max_positions = maxpos;
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(
                universe
                    .iter()
                    .map(|s| (s.clone(), flat_series(0, 30, 100.0, volume)))
                    .collect(),
            ),
            sources: vec![SourceRuntime {
                config: cfg,
                costs: None,
                research: None,
                quotas: universe
                    .iter()
                    .map(|s| {
                        (
                            s.clone(),
                            vec![AnnualQuota {
                                year: 2024,
                                filled: 1,
                                retained: 1,
                            }],
                        )
                    })
                    .collect(),
                factory: &factory,
            }],
            options,
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            alter(batch);
            replay_prepared(batch)
        },
    )
}
fn small_options() -> ControlOptions {
    let mut o = ControlOptions::new(151);
    o.replications = 20;
    o.modes = vec![ControlMode::PreserveBtc];
    o
}
fn stagger() -> Fixture {
    Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 2 * MINUTE,
        stagger_observed: true,
        ..Default::default()
    }
}
#[test]
fn two_pairs_sufficient_capacity_accept_and_cash_clipping_keeps_native_quantity() {
    let result = scenario(stagger(), 2, 2, 0.0, 1e6, small_options(), |_| {}).unwrap();
    let cells = &result[0].modes[0].cells;
    assert!(
        cells
            .iter()
            .all(|c| c.availability == CellAvailability::Complete && c.attempts == 1)
    );
    assert!(cells.iter().any(|c| c.quantity_changed));
}
#[test]
fn shared_max_positions_retries_tuples_without_restoring_observed_dates() {
    let result = scenario(stagger(), 2, 1, 0.0, 1e6, small_options(), |_| {}).unwrap();
    let cells = &result[0].modes[0].cells;
    assert!(
        cells
            .iter()
            .all(|c| c.availability == CellAvailability::Complete)
    );
    assert!(cells.iter().any(|c| c.attempts > 1));
    assert!(cells.iter().any(|c| !c.rejections.is_empty()));
}
#[test]
fn observed_outside_support_never_draws_or_replays() {
    let result = scenario(stagger(), 1, 1, 0.0, 1e6, small_options(), |batch| {
        batch.sources[0].observed_trace[0].release_ms += MINUTE
    })
    .unwrap();
    assert!(result[0].modes[0].cells.iter().all(|c| c.reason
        == Some(tv4::research::controls::UnavailableReason::ObservedOutsideSupport)
        && c.attempts == 0));
}
#[test]
fn missing_initial_risk_is_accepted_once_and_never_zero_filled() {
    let result = scenario(
        Fixture {
            stop: None,
            ..stagger()
        },
        1,
        1,
        0.0,
        1e6,
        small_options(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        result[0].observed_reason,
        Some(tv4::research::controls::UnavailableReason::MissingPositiveRisk)
    );
    assert!(result[0].observed_months.is_empty());
    assert!(result[0].modes[0].cells.iter().all(|c| c.months.is_empty()));
    assert!(
        result[0].modes[0]
            .cells
            .iter()
            .all(|c| c.accepted_attempt == Some(0)
                && c.score.is_none()
                && c.reason
                    == Some(tv4::research::controls::UnavailableReason::MissingPositiveRisk))
    );
}
#[test]
fn replay_attempts_planning_and_global_budgets_remain_explicit() {
    use tv4::research::controls::UnavailableReason as R;
    for (kind, reason) in [
        (0, R::AttemptsBudget),
        (1, R::ReplayBudget),
        (2, R::PlanningBudget),
        (3, R::GlobalEngineBudget),
    ] {
        let mut options = small_options();
        match kind {
            0 => options.budgets.attempts_per_replication = 0,
            1 => options.budgets.replay_steps_per_replication = 0,
            2 => options.budgets.dp_cells_per_geometry = 0,
            _ => {}
        }
        let result = scenario(stagger(), 1, 1, 0.0, 1e6, options, |batch| {
            if kind == 3 {
                let left = batch.options.budgets.total_engine_steps - batch.work.total_steps;
                batch
                    .work
                    .charge_engine(left, &batch.options.budgets)
                    .unwrap();
            }
        })
        .unwrap();
        assert!(
            result[0].modes[0]
                .cells
                .iter()
                .all(|c| c.reason == Some(reason))
        );
    }
}
#[test]
fn same_master_seed_produces_exactly_same_owned_reductions() {
    let a = scenario(stagger(), 2, 1, 0.0, 1e6, small_options(), |_| {}).unwrap();
    let b = scenario(stagger(), 2, 1, 0.0, 1e6, small_options(), |_| {}).unwrap();
    assert_eq!(a, b);
}
#[test]
fn inconsistent_supplied_quotas_and_partial_source_are_technical_errors() {
    assert!(
        scenario(stagger(), 1, 1, 0.0, 1e6, small_options(), |b| b.sources[0]
            .observed_trace
            .clear())
        .is_err()
    );
    assert!(
        scenario(stagger(), 1, 1, 0.0, 1e6, small_options(), |b| {
            let t = b.sources[0].observed_trace[0].clone();
            b.sources[0].observed_trace.push(t);
        })
        .is_err()
    );
}

fn month_case_window(
    missing_day: Option<i64>,
    inactive_pair: bool,
    excluded_pair: bool,
    leading_empty_month: bool,
) -> Vec<tv4::research::controls::SourceSummary> {
    use controls_common::Research;
    use tv4::model::{QualitySchedule, QuarterExclusion, Timeframe};
    let universe = names(if inactive_pair || excluded_pair {
        &["X-USD", "Y-USD"]
    } else {
        &["X-USD"]
    });
    let instruments = vec![instrument(0.0); universe.len()];
    let fixture = Fixture {
        signal_ms: T0 + 60 * MINUTE,
        hold_ms: 0,
        tf: Timeframe::H1,
        entry_until_ms: Some(T0 + 61 * MINUTE),
        ..Default::default()
    };
    let factory = || Ok(fixture.set(universe.len()));
    let mut candles = flat_series(0, 180, 100.0, 1e6);
    for day in 0..31 {
        if Some(day) != missing_day {
            candles.extend(flat_series((day + 1) * 1440 - 1, 1, 100.0, 1e6));
        }
    }
    let data = dataset(
        universe
            .iter()
            .enumerate()
            .map(|(leg, s)| {
                (
                    s.clone(),
                    if inactive_pair && leg == 1 {
                        flat_series(0, 30, 100.0, 1e6)
                    } else {
                        candles.clone()
                    },
                )
            })
            .collect(),
    );
    let mut research = Research::new(
        if leading_empty_month {
            T0 - 31 * 1440 * MINUTE
        } else {
            T0
        },
        T0 + 31 * 1440 * MINUTE,
    );
    research.universe_spec = tv4::engine::UniverseSpec::Fixed {
        symbols: universe.clone(),
    };
    if excluded_pair {
        research.quality = QualitySchedule::from_exclusions(BTreeMap::from([(
            "Y-USD".into(),
            vec![QuarterExclusion {
                quarter: "2024-Q1".into(),
                tfs: vec![Timeframe::H1],
            }],
        )]))
        .unwrap();
        research.identity.quality_hash = research.quality.hash().unwrap();
    }
    let spec = research.spec();
    let mut options = small_options();
    options.replications = 1;
    let quotas = universe
        .iter()
        .enumerate()
        .map(|(leg, s)| {
            (
                s.clone(),
                vec![AnnualQuota {
                    year: 2024,
                    filled: if leg == 0 || (!inactive_pair && !excluded_pair) {
                        1
                    } else {
                        0
                    },
                    retained: if leg == 0 || (!inactive_pair && !excluded_pair) {
                        1
                    } else {
                        0
                    },
                }],
            )
        })
        .collect();
    with_prepared_batch(
        BatchInputs {
            dataset: data,
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: Some(&spec),
                quotas,
                factory: &factory,
            }],
            options,
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        replay_prepared,
    )
    .unwrap()
}
#[test]
fn missing_internal_utc_day_does_not_claim_complete_month() {
    use tv4::research::controls::MonthCoverageStatus as S;
    assert_eq!(
        month_case(None, false, false)[0].month_coverage[0].status,
        S::Complete
    );
    assert_eq!(
        month_case(Some(13), false, false)[0].month_coverage[0].status,
        S::Partial
    );
}
#[test]
fn inactive_leg_and_single_pair_quality_exclusion_keep_union_month_observed() {
    use tv4::research::controls::MonthCoverageStatus as S;
    assert_eq!(
        month_case(None, true, false)[0].month_coverage[0].status,
        S::Complete
    );
    assert_eq!(
        month_case(None, false, true)[0].month_coverage[0].status,
        S::Complete
    );
}
#[test]
fn bounded_short_window_is_partial_and_missing_scores_keep_cell_identity() {
    let result = scenario(stagger(), 1, 1, 0.0, 1e6, small_options(), |_| {}).unwrap();
    assert_eq!(
        result[0].month_coverage[0].status,
        tv4::research::controls::MonthCoverageStatus::Partial
    );
    assert!(
        result[0].modes[0]
            .cells
            .iter()
            .enumerate()
            .all(|(k, c)| c.replication == k as u32)
    );
}
#[test]
fn cash_minimum_and_attempt_limit_expose_rejections_without_quota_repair() {
    let result = scenario(stagger(), 2, 2, 1.45, 1e6, small_options(), |_| {}).unwrap();
    assert!(result[0].modes[0].cells.iter().any(|c| c.attempts > 1));
    assert!(
        result[0].modes[0]
            .cells
            .iter()
            .all(|c| c.quota_checks.iter().all(|q| q.conformant))
    );
    let mut options = small_options();
    options.budgets.attempts_per_replication = 1;
    let result = scenario(stagger(), 2, 1, 0.0, 1e6, options, |_| {}).unwrap();
    assert!(result[0].modes[0].cells.iter().any(|c| c.reason
        == Some(tv4::research::controls::UnavailableReason::AttemptsBudget)
        && c.attempts == 1
        && c.accepted_attempt.is_none()
        && c.rejections.len() == 1));
}
#[test]
fn empty_unique_calendar_is_accepted_with_no_retained_trade_reason() {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 - MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let result = with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 10, 100.0, 1e6))]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::from([(
                    universe[0].clone(),
                    vec![AnnualQuota {
                        year: 2024,
                        filled: 0,
                        retained: 0,
                    }],
                )]),
                factory: &factory,
            }],
            options: small_options(),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        replay_prepared,
    )
    .unwrap();
    assert_eq!(
        result[0].modes[0].pair_supports[0].cardinality.as_deref(),
        Some("1")
    );
    assert_eq!(result[0].modes[0].planning_reason, None);
    assert!(
        result[0].modes[0]
            .cells
            .iter()
            .all(|c| c.accepted_attempt == Some(0)
                && c.reason == Some(tv4::research::controls::UnavailableReason::NoRetainedTrades))
    );
}
#[test]
fn duplicate_native_parents_are_refused_before_any_matrix() {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = stagger();
    let factory = || Ok(fixture.set(1));
    let sources = (0..2)
        .map(|_| SourceRuntime {
            config: config(&universe, &instruments, 300.0),
            costs: None,
            research: None,
            quotas: BTreeMap::from([(
                universe[0].clone(),
                vec![AnnualQuota {
                    year: 2024,
                    filled: 1,
                    retained: 1,
                }],
            )]),
            factory: &factory as _,
        })
        .collect();
    let result = with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 30, 100.0, 1e6))]),
            sources,
            options: small_options(),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            assert_eq!(batch.work.replay_steps, 0);
            replay_prepared(batch)
        },
    );
    assert!(matches!(
        result,
        Err(tv4::engine::controls::ControlError::InvalidInput(_))
    ));
}
#[test]
fn actual_maker_fill_year_and_terminal_purge_drive_replay_quotas() {
    use controls_common::Research;
    use portfolio_common::flat_bar;
    use tv4::store::time::days_from_civil;
    let midnight = days_from_civil(2024, 1, 1) * 86_400_000;
    for purged in [false, true] {
        let universe = names(&["X-USD"]);
        let instruments = vec![instrument(0.0)];
        let mut bars: Vec<_> = (-4..8)
            .map(|m| flat_bar(midnight + m * MINUTE, 100.0, 1e6))
            .collect();
        bars[6].low = 98.0;
        let fixture = Fixture {
            signal_ms: midnight - 2 * MINUTE,
            hold_ms: if purged { 30 * MINUTE } else { MINUTE },
            limit: Some((99.0, 5)),
            entry_until_ms: Some(midnight),
            ..Default::default()
        };
        let factory = || Ok(fixture.set(1));
        let research = Research::new(midnight - 4 * MINUTE, midnight + 8 * MINUTE);
        let spec = research.spec();
        let result = with_prepared_batch(
            BatchInputs {
                dataset: dataset(vec![(universe[0].clone(), bars)]),
                sources: vec![SourceRuntime {
                    config: config(&universe, &instruments, 300.0),
                    costs: None,
                    research: Some(&spec),
                    quotas: BTreeMap::from([(
                        universe[0].clone(),
                        vec![AnnualQuota {
                            year: 2024,
                            filled: 1,
                            retained: if purged { 0 } else { 1 },
                        }],
                    )]),
                    factory: &factory,
                }],
                options: small_options(),
            },
            &mut |_, _| Ok(ObservedValidation::Matched),
            &mut NoopPhaseObserver,
            replay_prepared,
        )
        .unwrap();
        assert!(
            result[0].modes[0]
                .cells
                .iter()
                .all(|c| c.accepted_attempt == Some(0)
                    && c.quota_checks[0].actual[0].year == 2024
                    && c.quota_checks[0].conformant)
        );
        if purged {
            assert!(
                result[0].modes[0].cells.iter().all(|c| c.reason
                    == Some(tv4::research::controls::UnavailableReason::NoRetainedTrades))
            );
        }
    }
}
#[test]
fn maker_volume_refusals_remove_slots_before_exact_calendar_sampling() {
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let fixture = Fixture {
        signal_ms: T0 + 2 * MINUTE,
        limit: Some((99.0, 8)),
        entry_until_ms: Some(T0 + 25 * MINUTE),
        ..stagger()
    };
    let factory = || Ok(fixture.set(2));
    let mut bars = flat_series(0, 35, 100.0, 1e6);
    for (i, c) in bars.iter_mut().enumerate() {
        c.low = 98.0;
        if i % 2 == 0 {
            c.volume = 1.51;
        }
    }
    let mut cfg = config(&universe, &instruments, 300.0);
    cfg.max_positions = 2;
    cfg.sim_rules.max_volume_share = 1.0;
    let result = with_prepared_batch(
        BatchInputs {
            dataset: dataset(universe.iter().map(|s| (s.clone(), bars.clone())).collect()),
            sources: vec![SourceRuntime {
                config: cfg,
                costs: None,
                research: None,
                quotas: universe
                    .iter()
                    .map(|s| {
                        (
                            s.clone(),
                            vec![AnnualQuota {
                                year: 2024,
                                filled: 1,
                                retained: 1,
                            }],
                        )
                    })
                    .collect(),
                factory: &factory,
            }],
            options: small_options(),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        replay_prepared,
    )
    .unwrap();
    let cells = &result[0].modes[0].cells;
    assert!(
        cells
            .iter()
            .all(|c| c.availability == CellAvailability::Complete)
    );
    assert!(result[0].modes[0].catalogues.iter().all(|cat| {
        cat.intervals.len() < 24
            && cat
                .intervals
                .iter()
                .all(|i| ((i.entry_ms - T0) / MINUTE) % 2 == 1)
    }));
    assert!(
        cells
            .iter()
            .all(|c| c.quota_checks.iter().all(|q| q.conformant))
    );
}

fn month_case(
    missing_day: Option<i64>,
    inactive_pair: bool,
    excluded_pair: bool,
) -> Vec<tv4::research::controls::SourceSummary> {
    month_case_window(missing_day, inactive_pair, excluded_pair, false)
}
#[test]
fn declared_window_keeps_zero_trade_months_and_discloses_unobserved_prefix() {
    let result = month_case_window(None, false, false, true);
    let source = &result[0];
    assert_eq!(source.month_coverage.len(), 2);
    assert_eq!(
        source.month_coverage[0].status,
        tv4::research::controls::MonthCoverageStatus::Excluded
    );
    assert_eq!(source.observed_months[0].count, 0);
    assert_eq!(source.observed_months[0].r_sum, 0.0);
    assert_eq!(
        source.modes[0].cells[0].months[0],
        source.observed_months[0]
    );
}
#[test]
fn support_ignores_lot_ids_and_reduction_uses_real_book_not_probe_pnl() {
    let result = scenario(stagger(), 1, 1, 0.0, 1e6, small_options(), |batch| {
        batch.sources[0].observed_trace[0].trade.lot_id = 999;
        for probe in Arc::make_mut(&mut batch.sources[0].probes)
            .iter_mut()
            .flatten()
        {
            if let Some(trade) = probe.trade.as_mut() {
                trade.pnl_net = f64::NAN;
            }
        }
    })
    .unwrap();
    assert!(result[0].modes[0].cells.iter().all(
        |c| c.availability == CellAvailability::Complete && c.score.is_some_and(f64::is_finite)
    ));
}
#[test]
fn nonfinite_observed_pnl_is_a_technical_error_even_without_risk() {
    for stop in [Some(90.0), None] {
        let result = scenario(
            Fixture { stop, ..stagger() },
            1,
            1,
            0.0,
            1e6,
            small_options(),
            |b| b.sources[0].observed_trace[0].trade.pnl_net = f64::NAN,
        );
        assert!(matches!(
            result,
            Err(tv4::engine::controls::ControlError::Numeric(
                tv4::research::controls::ControlsError::NonFinite
            ))
        ));
    }
}
#[test]
fn scheduled_intrabar_entry_stop_uses_native_release_close() {
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        entry_until_ms: Some(T0 + 2 * MINUTE),
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let mut bars = flat_series(0, 5, 100.0, 1e6);
    bars[2].low = 80.0;
    let result = with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), bars)]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::from([(
                    universe[0].clone(),
                    vec![AnnualQuota {
                        year: 2024,
                        filled: 1,
                        retained: 1,
                    }],
                )]),
                factory: &factory,
            }],
            options: small_options(),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        |batch| {
            assert_eq!(
                batch.sources[0].observed_trace[0].release_ms,
                T0 + 3 * MINUTE
            );
            assert_eq!(
                batch.sources[0].observed_trace[0].trade.exit_ms,
                T0 + 2 * MINUTE
            );
            replay_prepared(batch)
        },
    )
    .unwrap();
    assert!(
        result[0].modes[0]
            .cells
            .iter()
            .all(|c| c.accepted_attempt == Some(0) && c.score.is_some_and(|r| r < 0.0))
    );
}
fn terminal_month_case(hold_ms: i64) -> Vec<tv4::research::controls::SourceSummary> {
    use portfolio_common::flat_bar;
    let midnight = tv4::store::time::days_from_civil(2024, 2, 1) * 86_400_000;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let bars = (-10..0)
        .map(|m| flat_bar(midnight + m * MINUTE, 100.0, 1e6))
        .collect();
    let fixture = Fixture {
        signal_ms: midnight - 9 * MINUTE,
        hold_ms,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    with_prepared_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), bars)]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.0),
                costs: None,
                research: None,
                quotas: BTreeMap::from([(
                    universe[0].clone(),
                    vec![AnnualQuota {
                        year: 2024,
                        filled: 1,
                        retained: 1,
                    }],
                )]),
                factory: &factory,
            }],
            options: small_options(),
        },
        &mut |_, _| Ok(ObservedValidation::Matched),
        &mut NoopPhaseObserver,
        replay_prepared,
    )
    .unwrap()
}
#[test]
fn retained_forced_close_at_next_month_boundary_keeps_its_r() {
    let result = terminal_month_case(30 * MINUTE);
    let source = &result[0];
    assert_eq!(source.observed_months.len(), 2);
    assert_eq!(source.observed_months[1].month, 2);
    assert_eq!(source.observed_months[1].count, 1);
    assert_eq!(
        source.month_coverage[1].status,
        tv4::research::controls::MonthCoverageStatus::Partial
    );
    for cell in source.modes[0].cells.iter() {
        assert_eq!(cell.months.len(), 2);
        assert_eq!(cell.months.iter().map(|m| m.count).sum::<u32>(), 1);
        assert_eq!(cell.months[1].r_sum, cell.score.unwrap());
    }
}
#[test]
fn a_control_only_terminal_month_extends_the_common_source_and_k_axis() {
    let result = terminal_month_case(0);
    let source = &result[0];
    assert_eq!(source.observed_months.len(), 2);
    assert_eq!(source.observed_months[1].count, 0);
    assert!(source.modes[0].cells.iter().all(|c| c.months.len() == 2));
    assert!(source.modes[0].cells.iter().any(|c| c.months[1].count == 1));
}

#[test]
fn support_cardinalities_survive_partial_planning_budget_and_proven_empty_support() {
    use tv4::research::controls::UnavailableReason as R;
    let complete = scenario(stagger(), 2, 2, 0.0, 1e6, small_options(), |_| {}).unwrap();
    let mut options = small_options();
    assert_eq!(complete[0].planning_work.cells % 2, 0);
    options.budgets.dp_cells_per_geometry = complete[0].planning_work.cells / 2;
    let partial = scenario(stagger(), 2, 2, 0.0, 1e6, options, |_| {}).unwrap();
    let mode = &partial[0].modes[0];
    assert_eq!(mode.planning_reason, Some(R::PlanningBudget));
    assert_eq!(mode.pair_supports.len(), 2);
    assert_eq!(
        mode.pair_supports[0].cardinality,
        complete[0].modes[0].pair_supports[0].cardinality
    );
    assert!(mode.pair_supports[0].cardinality.is_some());
    assert_eq!(mode.pair_supports[1].cardinality, None);
    let empty = scenario(stagger(), 2, 2, 0.0, 1e6, small_options(), |batch| {
        Arc::make_mut(&mut batch.sources[0].modes[0].catalogues)[0]
            .intervals
            .clear();
    })
    .unwrap();
    let mode = &empty[0].modes[0];
    assert_eq!(mode.planning_reason, Some(R::EmptySupport));
    assert_eq!(mode.pair_supports[0].cardinality.as_deref(), Some("0"));
    assert_eq!(
        mode.pair_supports[1].cardinality, None,
        "une paire non atteinte reste inconnue"
    );
}
