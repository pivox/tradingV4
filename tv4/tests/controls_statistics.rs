mod controls_common;
mod portfolio_common;

use std::sync::Arc;
use tv4::model::rng::SplitMix64;
use tv4::research::controls::bootstrap::{delta_for_month_indices, paired_bootstrap};
use tv4::research::controls::statistics::summarize_panel;
use tv4::research::controls::*;

fn cell(replication: u32, score: Option<f64>) -> CellSummary {
    CellSummary {
        replication,
        accepted_attempt: score.map(|_| 0),
        attempts: u32::from(score.is_some()),
        availability: if score.is_some() {
            CellAvailability::Complete
        } else {
            CellAvailability::Unavailable
        },
        reason: score.is_none().then_some(UnavailableReason::AttemptsBudget),
        score,
        months: Vec::new(),
        quota_checks: Vec::new(),
        quantity_changed: false,
        replay_trace_hash: String::new(),
        rejections: Vec::new(),
    }
}
fn row(scores: &[f64]) -> Vec<CellSummary> {
    scores
        .iter()
        .enumerate()
        .map(|(k, &v)| cell(k as u32, Some(v)))
        .collect()
}
fn options(k: u32) -> ControlOptions {
    let mut o = ControlOptions::new(u64::MAX);
    o.replications = k;
    o
}
fn moments(values: &[(f64, u32)]) -> Vec<MonthMoment> {
    values
        .iter()
        .enumerate()
        .map(|(m, &(r_sum, count))| MonthMoment {
            year: 2024,
            month: m as u8 + 1,
            r_sum,
            count,
        })
        .collect()
}
fn coverage(n: u8) -> Vec<MonthCoverage> {
    (1..=n)
        .map(|month| MonthCoverage {
            year: 2024,
            month,
            status: MonthCoverageStatus::Complete,
        })
        .collect()
}
fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-12, "{a} != {b}");
}

#[test]
fn exact_maxima_ties_family_and_sample_mcse() {
    let a = row(&[0., 2., 1.]);
    let b = row(&[1., 1., 3.]);
    let p = summarize_panel(&[Some(2.), Some(4.)], &[&a, &b], &options(3)).unwrap();
    assert_eq!(p.maxima, vec![1., 2., 3.]);
    assert_eq!(p.status, PanelStatus::Complete);
    assert_eq!(p.sources[0].exceedances, Some(2));
    assert_eq!(p.sources[0].p_value, Some(0.75));
    assert_eq!(p.sources[0].ties, Some(1));
    assert_eq!(p.family_score, Some(4.));
    assert_eq!(p.family_exceedances, Some(0));
    assert_eq!(p.family_p_value, Some(0.25));
    assert_eq!(p.sources[0].delta, Some(1.));
    close(p.sources[0].mc_standard_error.unwrap(), 1. / 3_f64.sqrt());
    assert_eq!(p.critical_rank, 4);
    assert_eq!(p.critical_value, None);
}
#[test]
fn smoke_resolution_and_integer_critical_rank_are_not_type7() {
    for (k, smoke, rank) in [
        (1, true, 2),
        (20, true, 20),
        (199, true, 190),
        (200, false, 191),
    ] {
        let a = row(&(0..k).map(f64::from).collect::<Vec<_>>());
        let p = summarize_panel(&[Some(-1.)], &[&a], &options(k)).unwrap();
        assert_eq!(p.smoke, smoke);
        assert_eq!(p.critical_rank, rank);
        assert_eq!(p.resolution, 1. / f64::from(k + 1));
        assert_eq!(
            p.critical_value,
            (rank <= u64::from(k)).then_some((rank - 1) as f64)
        );
        assert_eq!(p.sources[0].mc_standard_error.is_some(), k >= 2);
    }
}
#[test]
fn constant_equal_scores_cannot_become_success() {
    let a = row(&[2.; 20]);
    let p = summarize_panel(&[Some(2.)], &[&a], &options(20)).unwrap();
    assert_eq!(p.sources[0].p_value, Some(1.));
    assert_eq!(p.family_p_value, Some(1.));
    assert!(p.sources[0].constant_controls);
    assert_eq!(p.sources[0].ties, Some(20));
    assert_eq!(p.sources[0].above_critical, Some(false));
}
#[test]
fn missing_cell_or_observed_forbids_every_global_p_but_keeps_complete_descriptions() {
    let a = row(&[0., 2., 1.]);
    let mut b = row(&[1., 1., 3.]);
    b[1] = cell(1, None);
    for observed in [[Some(2.), Some(4.)], [Some(2.), None]] {
        let p = summarize_panel(&observed, &[&a, &b], &options(3)).unwrap();
        assert_eq!(p.status, PanelStatus::Inconclusive);
        assert!(p.maxima.is_empty());
        assert_eq!(p.family_p_value, None);
        assert_eq!(p.critical_value, None);
        assert!(
            p.sources
                .iter()
                .all(|s| s.p_value.is_none() && s.exceedances.is_none())
        );
        assert_eq!(p.sources[0].delta, Some(1.));
    }
}
#[test]
fn malformed_matrix_and_nonfinite_values_are_technical_errors() {
    let a = row(&[1., 2.]);
    let o = options(2);
    assert!(summarize_panel(&[], &[], &o).is_err());
    assert!(summarize_panel(&[Some(1.)], &[], &o).is_err());
    assert!(summarize_panel(&[Some(1.)], &[&a[..1]], &o).is_err());
    assert!(summarize_panel(&vec![Some(1.); 28], &vec![a.as_slice(); 28], &o).is_err());
    let mut wrong = a.clone();
    wrong[0].replication = 1;
    assert!(summarize_panel(&[Some(1.)], &[&wrong], &o).is_err());
    let mut wrong = a.clone();
    wrong[0].availability = CellAvailability::Unavailable;
    assert!(summarize_panel(&[Some(1.)], &[&wrong], &o).is_err());
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            summarize_panel(&[Some(value)], &[&a], &o),
            Err(ControlsError::NonFinite)
        );
        assert_eq!(
            summarize_panel(&[None], &[&row(&[value, 2.])], &o),
            Err(ControlsError::NonFinite)
        );
    }
}
#[test]
fn bootstrap_common_indices_average_ratios_not_pooled_controls() {
    let observed = moments(&[(2., 1), (18., 9)]);
    let c1 = moments(&[(0., 1), (0., 1)]);
    let c2 = moments(&[(0., 1), (20., 10)]);
    let controls = [c1.as_slice(), c2.as_slice()];
    close(
        delta_for_month_indices(&observed, &controls, &[0, 0])
            .unwrap()
            .unwrap(),
        2.,
    );
    close(
        delta_for_month_indices(&observed, &controls, &[1, 1])
            .unwrap()
            .unwrap(),
        1.,
    );
    close(
        delta_for_month_indices(&observed, &controls, &[0, 1])
            .unwrap()
            .unwrap(),
        12. / 11.,
    );
}
#[test]
fn identical_months_have_zero_temporal_se_and_point_bound() {
    let observed = moments(&[(2., 1), (2., 1)]);
    let c = moments(&[(1., 1), (1., 1)]);
    let result =
        paired_bootstrap(&observed, &[&c], &coverage(2), &mut SplitMix64::new(7), 100).unwrap();
    assert!(result.available);
    assert!(result.approximate);
    assert_eq!(result.point, Some(1.));
    assert_eq!(result.lb90, Some(1.));
    assert_eq!(result.standard_error, Some(0.));
    assert_eq!(result.lb90_positive, Some(true));
}
#[test]
fn one_zero_draw_invalidates_whole_bootstrap_without_filtering() {
    let observed = moments(&[(0., 0), (2., 1)]);
    let c = moments(&[(1., 1), (1., 1)]);
    assert_eq!(
        delta_for_month_indices(&observed, &[&c], &[0, 0]).unwrap(),
        None
    );
    let r = paired_bootstrap(&observed, &[&c], &coverage(2), &mut SplitMix64::new(1), 100).unwrap();
    assert!(!r.available);
    assert_eq!(r.reason, Some(UnavailableReason::BootstrapZeroDenominator));
    assert_eq!(r.standard_error, None);
    assert_eq!(r.lb90, None);
}
#[test]
fn partial_and_excluded_months_are_sampled_and_preserve_full_point() {
    let observed = moments(&[(1., 1), (1., 1), (20., 1), (30., 1)]);
    let c = moments(&[(0., 1); 4]);
    let mut cov = coverage(4);
    cov[2].status = MonthCoverageStatus::Partial;
    cov[3].status = MonthCoverageStatus::Excluded;
    let a = paired_bootstrap(&observed, &[&c], &cov, &mut SplitMix64::new(17), 1000).unwrap();
    assert_eq!(a.point, Some(13.));
    assert_eq!(a.months, 4);
    assert_eq!(a.complete_months, 2);
    assert!(a.standard_error.unwrap() > 1.);
    cov[1].status = MonthCoverageStatus::Partial;
    let b = paired_bootstrap(&observed, &[&c], &cov, &mut SplitMix64::new(17), 1000).unwrap();
    assert_eq!(b.reason, Some(UnavailableReason::InsufficientMonths));
    assert_eq!(b.point, Some(13.));
}
#[test]
fn bootstrap_rejects_inconsistent_axes_unknown_r_and_nonfinite_data() {
    let observed = moments(&[(1., 1), (1., 1)]);
    let mut c = observed.clone();
    c[1].month = 3;
    assert!(paired_bootstrap(&observed, &[&c], &coverage(2), &mut SplitMix64::new(1), 2).is_err());
    c = observed.clone();
    c[1].r_sum = f64::NAN;
    assert_eq!(
        paired_bootstrap(&observed, &[&c], &coverage(2), &mut SplitMix64::new(1), 2),
        Err(ControlsError::NonFinite)
    );
    c = observed.clone();
    c[1].count = 0;
    assert!(paired_bootstrap(&observed, &[&c], &coverage(2), &mut SplitMix64::new(1), 2).is_err());
    assert!(delta_for_month_indices(&observed, &[&observed], &[0, 2]).is_err());
    assert!(
        paired_bootstrap(
            &observed,
            &[&observed],
            &coverage(2),
            &mut SplitMix64::new(1),
            1
        )
        .is_err()
    );
}
#[test]
fn master_seed_max_bootstrap_is_deterministic_and_effective_alias_shares_stream() {
    use tv4::research::controls::random::bootstrap_rng;
    let h = "a".repeat(64);
    let obs = moments(&[(2., 1), (18., 9)]);
    let c = moments(&[(0., 1), (20., 10)]);
    let mut a = bootstrap_rng(u64::MAX, &h, ControlMode::PreserveBtc).unwrap();
    let mut b = bootstrap_rng(u64::MAX, &h, ControlMode::RemoveBtc.effective(false)).unwrap();
    assert_eq!(
        paired_bootstrap(&obs, &[&c], &coverage(2), &mut a, 100).unwrap(),
        paired_bootstrap(&obs, &[&c], &coverage(2), &mut b, 100).unwrap()
    );
    assert_eq!(a, b);
}

fn native_batch(
    options: ControlOptions,
    validation: Result<tv4::engine::controls::ObservedValidation, String>,
) -> Result<tv4::engine::controls::BatchOutcome, tv4::engine::controls::ControlError> {
    use controls_common::{Fixture, dataset};
    use portfolio_common::{MINUTE, T0, config, flat_series, instrument, names};
    use std::collections::BTreeMap;
    use tv4::engine::controls::*;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.)];
    let fixture = Fixture {
        signal_ms: T0 + MINUTE,
        hold_ms: 2 * MINUTE,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let mut callbacks = 0;
    let result = run_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), flat_series(0, 30, 100., 1e6))]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.),
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
            callbacks += 1;
            let ObservedRun::Plain(result) = run else {
                panic!("plain")
            };
            assert_eq!(result.trades.len(), 1);
            validation.clone()
        },
    );
    assert_eq!(callbacks, 1);
    result
}
#[test]
fn complete_run_batch_executes_native_book_once_per_alias_and_publishes_owned_statistics() {
    use tv4::engine::controls::ObservedValidation;
    let result = native_batch(options(20), Ok(ObservedValidation::Matched)).unwrap();
    assert_eq!(result.requested_sources, 1);
    assert_eq!(result.requested_replications, 20);
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.statistics.len(), 2);
    assert_eq!(result.statistics[0].panel.status, PanelStatus::Complete);
    assert_eq!(result.statistics[0].panel.maxima.len(), 20);
    assert_eq!(
        result.statistics[1].alias_of,
        Some(ControlMode::PreserveBtc)
    );
    assert_eq!(result.work.replay_steps, 20 * 30);
    assert_eq!(result.prep.unwrap().candles_converted, 1);
    let a = result.statistics[0].panel.sources[0]
        .temporal
        .as_ref()
        .unwrap();
    let b = result.statistics[1].panel.sources[0]
        .temporal
        .as_ref()
        .unwrap();
    assert!(Arc::ptr_eq(a, b));
    assert_eq!(a.samples, 10000);
    assert_eq!(a.reason, Some(UnavailableReason::InsufficientMonths));
}
#[test]
fn expected_preparation_failure_returns_publishable_outcome_and_keeps_partial_work() {
    use tv4::engine::controls::ObservedValidation;
    for (mut opts, validation, reason) in [
        (
            options(3),
            ObservedValidation::Mismatched,
            UnavailableReason::ObservedMismatch,
        ),
        (
            options(3),
            ObservedValidation::Matched,
            UnavailableReason::PreparationBudget,
        ),
    ] {
        if reason == UnavailableReason::PreparationBudget {
            opts.budgets.probe_steps_per_geometry = 0;
        }
        let result = native_batch(opts, Ok(validation)).unwrap();
        assert_eq!(result.status, PanelStatus::Inconclusive);
        assert_eq!(result.reasons, vec![reason]);
        assert_eq!(result.requested_sources, 1);
        assert_eq!(result.requested_replications, 3);
        assert!(result.prep.is_none());
        assert!(result.sources.is_empty());
        assert!(result.statistics.is_empty());
        assert!(result.work.total_steps > 0);
    }
    assert!(native_batch(options(3), Err("rendu I/O refusé".into())).is_err());
}
#[test]
fn replay_budget_keeps_all_requested_cells_and_forbids_global_p() {
    use tv4::engine::controls::ObservedValidation;
    let mut opts = options(3);
    opts.budgets.replay_steps_per_replication = 0;
    let r = native_batch(opts, Ok(ObservedValidation::Matched)).unwrap();
    assert_eq!(r.status, PanelStatus::Inconclusive);
    assert_eq!(r.sources[0].modes[0].cells.len(), 3);
    assert!(r.reasons.contains(&UnavailableReason::ReplayBudget));
    assert!(
        r.statistics
            .iter()
            .all(|m| m.panel.family_p_value.is_none() && m.panel.maxima.is_empty())
    );
}
#[test]
fn planning_diagnostics_overflow_is_transactional() {
    use controls_common::{Fixture, dataset};
    use portfolio_common::{MINUTE, T0, config, flat_series, instrument, names};
    use std::collections::BTreeMap;
    use tv4::engine::controls::*;
    for overflow_bytes in [false, true] {
        let universe = names(&["X-USD"]);
        let instruments = vec![instrument(0.)];
        let fixture = Fixture {
            signal_ms: T0 + MINUTE,
            ..Default::default()
        };
        let factory = || Ok(fixture.set(1));
        let mut work = EngineWork::default();
        work.planning_cells = if overflow_bytes { 0 } else { u64::MAX };
        work.planning_logical_bytes = if overflow_bytes { u64::MAX } else { 0 };
        let previous = (work.planning_cells, work.planning_logical_bytes);
        let result = with_prepared_batch_with_work(
            BatchInputs {
                dataset: dataset(vec![(universe[0].clone(), flat_series(0, 15, 100., 1e6))]),
                sources: vec![SourceRuntime {
                    config: config(&universe, &instruments, 300.),
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
                options: options(1),
            },
            &mut work,
            &mut |_, _| Ok(ObservedValidation::Matched),
            &mut NoopPhaseObserver,
            replay_prepared,
        );
        assert!(matches!(result, Err(ControlError::InvalidInput(_))));
        assert_eq!((work.planning_cells, work.planning_logical_bytes), previous);
    }
}

#[test]
fn mixed_btc_panel_reuses_parent_bootstrap_but_recomputes_requested_global_maxima() {
    use controls_common::{Fixture, dataset};
    use portfolio_common::{DAY, MINUTE, T0, config, flat_bar, instrument, names};
    use std::collections::BTreeMap;
    use tv4::engine::controls::*;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.)];
    let mut btc: Vec<_> = (0..200)
        .map(|d| {
            flat_bar(
                T0 + (d + 1) * DAY - MINUTE,
                if d == 199 { 200. } else { 100. },
                1e6,
            )
        })
        .collect();
    btc.push(flat_bar(T0 + 202 * DAY - MINUTE, 10., 1e6));
    btc.push(flat_bar(T0 + 202 * DAY + 20 * MINUTE, 10., 1e6));
    let mut x: Vec<_> = (-1..14)
        .map(|m| flat_bar(T0 + 200 * DAY + m * MINUTE, 100., 1e6))
        .collect();
    x.extend((0..14).map(|m| flat_bar(T0 + 202 * DAY + m * MINUTE, 100. + m as f64, 1e6)));
    let plain = Fixture {
        signal_ms: T0 + 202 * DAY + MINUTE,
        hold_ms: MINUTE,
        ..Default::default()
    };
    let filtered = Fixture {
        signal_ms: T0 + 200 * DAY + MINUTE,
        hold_ms: MINUTE,
        btc: true,
        ..Default::default()
    };
    let f1 = move || Ok(plain.set(1));
    let f2 = move || Ok(filtered.set(1));
    type Factory =
        dyn Fn() -> Result<Vec<Box<dyn tv4::strategy::Strategy>>, tv4::strategy::StrategyError>;
    let factories: [&Factory; 2] = [&f1, &f2];
    #[derive(Default)]
    struct Phases(Vec<(bool, ControlPhase)>);
    impl PhaseObserver for Phases {
        fn phase_started(&mut self, p: ControlPhase) {
            self.0.push((true, p));
        }
        fn phase_finished(&mut self, p: ControlPhase) {
            self.0.push((false, p));
        }
    }
    let mut phases = Phases::default();
    let mut callbacks = 0;
    let result = run_batch_observed(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), x), ("BTC-USD".into(), btc)]),
            sources: factories
                .iter()
                .map(|&factory| SourceRuntime {
                    config: config(&universe, &instruments, 300.),
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
                    factory,
                })
                .collect(),
            options: options(20),
        },
        &mut |_, _| {
            callbacks += 1;
            Ok(ObservedValidation::Matched)
        },
        &mut phases,
    )
    .unwrap();
    assert_eq!(callbacks, 2);
    assert_eq!(result.status, PanelStatus::Complete);
    assert!(result.prep.unwrap().regime_series_built >= 1);
    assert!(phases.0.ends_with(&[
        (true, ControlPhase::Statistics),
        (false, ControlPhase::Statistics)
    ]));
    assert!(result.statistics.iter().all(|m| m.alias_of.is_none()));
    assert_ne!(
        result.statistics[0].panel.maxima,
        result.statistics[1].panel.maxima
    );
    assert!(Arc::ptr_eq(
        result.statistics[0].panel.sources[0]
            .temporal
            .as_ref()
            .unwrap(),
        result.statistics[1].panel.sources[0]
            .temporal
            .as_ref()
            .unwrap()
    ));
    assert!(!Arc::ptr_eq(
        result.statistics[0].panel.sources[1]
            .temporal
            .as_ref()
            .unwrap(),
        result.statistics[1].panel.sources[1]
            .temporal
            .as_ref()
            .unwrap()
    ));
    for mode in &result.statistics {
        let expected: Vec<_> = (0..20)
            .map(|k| {
                result
                    .sources
                    .iter()
                    .map(|s| {
                        s.modes.iter().find(|m| m.mode == mode.mode).unwrap().cells[k]
                            .score
                            .unwrap()
                    })
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        assert_eq!(mode.panel.maxima, expected);
        for s in &mode.panel.sources {
            let b = expected.iter().filter(|&&m| m >= s.score.unwrap()).count() as u32;
            assert_eq!(s.exceedances, Some(b));
            close(s.p_value.unwrap(), f64::from(1 + b) / 21.);
        }
    }
}
#[test]
fn too_many_sources_and_invalid_options_are_refused_before_loading_or_callback() {
    use controls_common::{Fixture, dataset};
    use portfolio_common::{config, instrument, names};
    use std::collections::BTreeMap;
    use tv4::engine::controls::*;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.)];
    let fixture = Fixture::default();
    let factory = || Ok(fixture.set(1));
    for (n, k) in [(28, 1), (0, 1), (1, 0)] {
        let r = run_batch(
            BatchInputs {
                dataset: dataset(Vec::new()),
                sources: (0..n)
                    .map(|_| SourceRuntime {
                        config: config(&universe, &instruments, 300.),
                        costs: None,
                        research: None,
                        quotas: BTreeMap::new(),
                        factory: &factory,
                    })
                    .collect(),
                options: options(k),
            },
            &mut |_, _| panic!("aucun callback avant validation"),
        );
        assert!(matches!(
            r,
            Err(ControlError::InvalidInput(_)
                | ControlError::Numeric(ControlsError::InvalidInput(_)))
        ));
    }
}

#[test]
fn constant_fractional_months_have_exactly_zero_sampling_spread() {
    let observed = moments(&[(1., 3), (1., 3)]);
    let c = moments(&[(0., 1), (0., 1)]);
    let r = paired_bootstrap(
        &observed,
        &[&c],
        &coverage(2),
        &mut SplitMix64::new(19),
        10000,
    )
    .unwrap();
    assert_eq!(r.standard_error, Some(0.));
    assert_eq!(r.lb90, r.point);
    let a = row(&[1. / 3.; 200]);
    let p = summarize_panel(&[Some(1. / 3.)], &[&a], &options(200)).unwrap();
    assert_eq!(p.sources[0].mc_standard_error, Some(0.));
}
#[test]
fn bootstrap_p10_is_type7_and_sample_se_uses_b_minus_one() {
    let observed = moments(&[(2., 1), (18., 9)]);
    let c1 = moments(&[(0., 1), (0., 1)]);
    let c2 = moments(&[(0., 1), (20., 10)]);
    // SplitMix64(seed=1), borne2 : les quatre bits bas sont 1,1,0,1.
    // Les deux deltas sont donc 1 et 12/11 ; p10 type7 = 1 + (1/11)*.1.
    let r = paired_bootstrap(
        &observed,
        &[&c1, &c2],
        &coverage(2),
        &mut SplitMix64::new(1),
        2,
    )
    .unwrap();
    close(r.lb90.unwrap(), 1. + 1. / 110.);
    close(r.standard_error.unwrap(), 1. / (11. * 2_f64.sqrt()));
}
#[test]
fn missing_observed_alone_disables_global_ranks_on_otherwise_complete_matrix() {
    let a = row(&[1., 2.]);
    let p = summarize_panel(&[None], &[&a], &options(2)).unwrap();
    assert_eq!(p.status, PanelStatus::Inconclusive);
    assert_eq!(p.family_score, None);
    assert_eq!(p.family_p_value, None);
    assert_eq!(p.sources[0].control_mean, Some(1.5));
    assert_eq!(p.sources[0].delta, None);
}
#[test]
fn complete_monthly_native_batch_runs_ten_thousand_shared_bootstrap_draws() {
    use controls_common::{Fixture, dataset};
    use portfolio_common::{DAY, MINUTE, T0, config, flat_bar, instrument, names};
    use std::collections::BTreeMap;
    use tv4::engine::controls::*;
    use tv4::model::Timeframe;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.)];
    let fixture = Fixture {
        signal_ms: T0 + DAY,
        observed_every_ms: Some(2 * DAY),
        entry_until_ms: Some(T0 + 55 * DAY),
        tf: Timeframe::D1,
        ..Default::default()
    };
    let factory = || Ok(fixture.set(1));
    let mut candles = vec![flat_bar(T0, 100., 1e6)];
    candles.extend((0..60).map(|d| flat_bar(T0 + (d + 1) * DAY - MINUTE, 100., 1e6)));
    let result = run_batch(
        BatchInputs {
            dataset: dataset(vec![(universe[0].clone(), candles)]),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.),
                costs: None,
                research: None,
                quotas: BTreeMap::from([(
                    universe[0].clone(),
                    vec![AnnualQuota {
                        year: 2024,
                        filled: 27,
                        retained: 27,
                    }],
                )]),
                factory: &factory,
            }],
            options: options(2),
        },
        &mut |_, run| {
            let ObservedRun::Plain(r) = run else {
                panic!("plain")
            };
            assert_eq!(r.trades.len(), 27);
            Ok(ObservedValidation::Matched)
        },
    )
    .unwrap();
    assert_eq!(result.status, PanelStatus::Complete);
    let temporal = result.statistics[0].panel.sources[0]
        .temporal
        .as_ref()
        .unwrap();
    assert_eq!(temporal.samples, 10000);
    assert_eq!(temporal.complete_months, 2);
    assert_eq!(temporal.months, 2);
    assert!(temporal.available);
    assert!(temporal.standard_error.is_some());
    assert!(temporal.lb90.is_some());
    assert!(Arc::ptr_eq(
        temporal,
        result.statistics[1].panel.sources[0]
            .temporal
            .as_ref()
            .unwrap()
    ));
}
