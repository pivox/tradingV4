use tv4::research::controls::{
    AnnualQuota, Budgets, Calendar, ControlMode, ControlOptions, EngineWork, Interval,
    PairCatalogue, PlanningFailure, PlanningWork, ProgramBudgets, UnavailableReason,
    validate_quotas,
};

fn quota(year: i32, filled: u32, retained: u32) -> AnnualQuota {
    AnnualQuota {
        year,
        filled,
        retained,
    }
}

fn interval(id: u32, decision: usize, next_decision: usize, year: i32) -> Interval {
    Interval {
        id,
        decision,
        signal_ms: decision as i64 * 10,
        entry_ms: decision as i64 * 10 + 1,
        release_ms: next_decision as i64 * 10,
        next_decision,
        fill_year: year,
        retained: true,
        clocks_hash: "horloges-test".into(),
    }
}

fn catalogue() -> PairCatalogue {
    PairCatalogue {
        symbol: "BTC-USDC".into(),
        intervals: vec![
            interval(10, 0, 2, 2020),
            interval(20, 1, 2, 2020),
            interval(30, 2, 3, 2021),
        ],
    }
}

fn limits() -> Budgets {
    Budgets {
        probe_steps_per_geometry: 3,
        replay_steps_per_replication: 4,
        attempts_per_replication: 2,
        total_engine_steps: 10,
        dp_cells_per_geometry: 2,
        dp_bytes_per_geometry: 48,
    }
}

#[test]
fn quotas_are_ordered_and_allow_zero_years_but_only_one_terminal_purge() {
    assert!(validate_quotas(&[quota(2020, 0, 0), quota(2021, 2, 1)]).is_ok());
    assert!(validate_quotas(&[quota(2020, 1, 0), quota(2021, 0, 0)]).is_ok());
    for invalid in [
        vec![quota(2020, 1, 2)],
        vec![quota(2020, 1, 1), quota(2020, 1, 1)],
        vec![quota(2021, 1, 1), quota(2020, 1, 1)],
        vec![quota(2020, 1, 0), quota(2021, 1, 0)],
        vec![quota(2020, 2, 0)],
        vec![quota(2020, 1, 0), quota(2021, 1, 1)],
    ] {
        assert!(matches!(
            validate_quotas(&invalid),
            Err(PlanningFailure::InvalidInput(_))
        ));
    }
}

#[test]
fn catalogue_next_decision_matches_first_released_candidate_in_both_directions() {
    for next_decision in [1, 3] {
        let mut value = catalogue();
        value.intervals[0].next_decision = next_decision;
        assert!(
            value.validate().is_err(),
            "borne incompatible {next_decision}"
        );
    }
    let value = catalogue();
    assert_eq!(value.intervals[0].release_ms, value.intervals[2].signal_ms);
    value
        .validate()
        .expect("décision à la même clôture que la libération");
}

#[test]
fn catalogue_purged_candidate_cannot_have_a_continuation() {
    let mut value = catalogue();
    value.intervals[0].retained = false;
    assert!(value.validate().is_err());
    value.intervals[0].next_decision = 3;
    value.intervals[0].release_ms = 30;
    value.validate().expect("purge sans continuation native");
}

#[test]
fn catalogue_rejects_inverted_clocks_duplicate_ids_and_decisions() {
    for index in 0..7 {
        let mut value = catalogue();
        match index {
            0 => value.intervals[0].entry_ms = -1,
            1 => value.intervals[0].release_ms = 0,
            2 => value.intervals[0].next_decision = 0,
            3 => value.intervals[1].id = 10,
            4 => value.intervals[1].decision = 0,
            5 => value.intervals[1].signal_ms = 0,
            _ => value.symbol.clear(),
        }
        assert!(value.validate().is_err(), "cas {index}");
    }
}

#[test]
fn catalogue_accepts_maker_fill_years_out_of_candidate_order() {
    let mut value = catalogue();
    value.intervals[0].entry_ms = 40;
    value.intervals[0].release_ms = 50;
    value.intervals[0].next_decision = 5;
    value.intervals[0].fill_year = 2021;
    assert!(value.validate().is_ok());
    Calendar {
        interval_ids: vec![10],
    }
    .validate(&value, &[quota(2021, 1, 1)])
    .expect("candidat maker seul");
    Calendar {
        interval_ids: vec![20, 30],
    }
    .validate(&value, &[quota(2020, 1, 1), quota(2021, 1, 1)])
    .expect("autre calendrier réalisable");
}

#[test]
fn calendars_require_exact_quotas_nonoverlap_known_ordered_ids() {
    let value = catalogue();
    let quotas = [quota(2020, 1, 1), quota(2021, 1, 1)];
    Calendar {
        interval_ids: vec![10, 30],
    }
    .validate(&value, &quotas)
    .expect("libération et décision au même instant");
    for ids in [
        vec![10, 20],
        vec![30, 10],
        vec![10, 10],
        vec![999],
        vec![10],
    ] {
        assert!(
            Calendar { interval_ids: ids }
                .validate(&value, &quotas)
                .is_err()
        );
    }
    Calendar {
        interval_ids: vec![],
    }
    .validate(&value, &[quota(2020, 0, 0)])
    .expect("quota nul conservé");
    assert!(
        Calendar {
            interval_ids: vec![10]
        }
        .validate(&value, &[quota(2020, 0, 0)])
        .is_err()
    );
}

#[test]
fn calendar_checks_clocks_and_terminal_purge_in_addition_to_indices() {
    let mut value = catalogue();
    value.intervals[0].retained = false;
    assert!(
        Calendar {
            interval_ids: vec![10, 30]
        }
        .validate(&value, &[quota(2020, 1, 0), quota(2021, 1, 1)])
        .is_err()
    );
    value.intervals[0].retained = true;
    value.intervals[2].retained = false;
    Calendar {
        interval_ids: vec![10, 30],
    }
    .validate(&value, &[quota(2020, 1, 1), quota(2021, 1, 0)])
    .expect("purge terminale");
    value.intervals[0].release_ms = 21;
    assert!(
        Calendar {
            interval_ids: vec![10, 30]
        }
        .validate(&value, &[quota(2020, 1, 1), quota(2021, 1, 0)])
        .is_err()
    );
}

#[test]
fn engine_debits_are_atomic_at_local_and_global_limits_and_reset_scopes() {
    let budgets = limits();
    let mut work = EngineWork::default();
    work.charge_probe(3, &budgets).expect("borne sonde exacte");
    let before = work.clone();
    assert_eq!(
        work.charge_probe(1, &budgets),
        Err(UnavailableReason::PreparationBudget)
    );
    assert_eq!(work, before);
    work.begin_geometry();
    work.charge_probe(3, &budgets)
        .expect("autre géométrie, même plafond local");
    assert_eq!(work.probe_steps, 6);
    work.charge_replay(4, &budgets)
        .expect("borne rejeu et globale exactes");
    assert_eq!(work.total_steps, 10);
    let before = work.clone();
    assert_eq!(
        work.charge_replay(1, &budgets),
        Err(UnavailableReason::ReplayBudget)
    );
    assert_eq!(work, before);
    work.begin_replication();
    let before = work.clone();
    assert_eq!(
        work.charge_replay(1, &budgets),
        Err(UnavailableReason::GlobalEngineBudget)
    );
    assert_eq!(work, before);
    assert_eq!(work.replay_steps, 4);
}

#[test]
fn replay_attempts_and_steps_are_shared_within_a_replication() {
    let mut budgets = limits();
    budgets.total_engine_steps = 100;
    let mut work = EngineWork::default();
    work.charge_attempt(&budgets).expect("tentative 1");
    work.charge_replay(2, &budgets).expect("rejeu 1");
    work.charge_attempt(&budgets).expect("tentative 2");
    work.charge_replay(2, &budgets).expect("rejeu 2");
    let before = work.clone();
    assert_eq!(
        work.charge_attempt(&budgets),
        Err(UnavailableReason::AttemptsBudget)
    );
    assert_eq!(
        work.charge_replay(1, &budgets),
        Err(UnavailableReason::ReplayBudget)
    );
    assert_eq!(work, before);
    work.begin_replication();
    work.charge_attempt(&budgets).expect("réplication suivante");
    work.charge_replay(4, &budgets)
        .expect("autre plafond local");
    assert_eq!(work.replay_steps, 8);
}

#[test]
fn engine_cumulative_and_global_overflow_never_partially_debit() {
    let budgets = Budgets {
        probe_steps_per_geometry: u64::MAX,
        replay_steps_per_replication: u64::MAX,
        total_engine_steps: u64::MAX,
        ..limits()
    };
    for phase in 0..3 {
        let mut work = EngineWork::default();
        match phase {
            0 => work.probe_steps = u64::MAX,
            1 => work.replay_steps = u64::MAX,
            _ => work.total_steps = u64::MAX,
        }
        let before = work.clone();
        let result = match phase {
            0 => work.charge_probe(1, &budgets),
            1 => work.charge_replay(1, &budgets),
            _ => work.charge_engine(1, &budgets),
        };
        assert!(result.is_err());
        assert_eq!(work, before);
    }
    let mut work = EngineWork::default();
    work.charge_probe(u64::MAX, &budgets)
        .expect("borne u64 exacte");
    let before = work.clone();
    assert_eq!(
        work.charge_probe(1, &budgets),
        Err(UnavailableReason::PreparationBudget)
    );
    assert_eq!(work, before);
}

#[test]
fn planning_work_shares_geometry_limits_and_checks_both_additions_before_mutation() {
    let budgets = limits();
    let mut work = PlanningWork::default();
    work.charge(1, 40, &budgets).expect("premier compte");
    work.charge(1, 8, &budgets).expect("deux bornes exactes");
    for (cells, bytes) in [(1, 0), (0, 1), (u64::MAX, 0), (0, u64::MAX)] {
        let before = work.clone();
        assert!(matches!(
            work.charge(cells, bytes, &budgets),
            Err(PlanningFailure::Budget { .. })
        ));
        assert_eq!(work, before);
    }
    let unbounded = Budgets {
        dp_cells_per_geometry: u64::MAX,
        dp_bytes_per_geometry: u64::MAX,
        ..budgets
    };
    let mut work = PlanningWork::default();
    work.charge(u64::MAX, u64::MAX, &unbounded)
        .expect("bornes maximales");
    let before = work.clone();
    assert!(work.charge(1, 0, &unbounded).is_err());
    assert!(work.charge(0, 1, &unbounded).is_err());
    assert_eq!(work, before);
}

#[test]
fn program_projection_excludes_invocation_budget_and_btc_alias_is_exact() {
    let budgets = Budgets::default();
    assert_eq!(
        (
            budgets.probe_steps_per_geometry,
            budgets.dp_cells_per_geometry,
            budgets.dp_bytes_per_geometry,
            budgets.attempts_per_replication,
            budgets.replay_steps_per_replication,
            budgets.total_engine_steps
        ),
        (
            10_000_000_000,
            2_000_000,
            268_435_456,
            128,
            5_000_000_000,
            500_000_000_000
        )
    );
    let projection = ProgramBudgets::from(&budgets);
    let changed = Budgets {
        total_engine_steps: 1,
        ..budgets
    };
    assert_eq!(projection, ProgramBudgets::from(&changed));
    let json = serde_json::to_value(projection).expect("projection JSON");
    assert!(json.get("total_engine_steps").is_none());
    assert_eq!(json.as_object().expect("objet").len(), 5);
    assert_eq!(
        ControlMode::RemoveBtc.effective(false),
        ControlMode::PreserveBtc
    );
    assert_eq!(
        ControlMode::RemoveBtc.effective(true),
        ControlMode::RemoveBtc
    );
    let mut options = ControlOptions::new(42);
    assert_eq!(options.replications, 200);
    assert_eq!(
        options.effective_modes(false),
        vec![ControlMode::PreserveBtc]
    );
    assert!(options.validate().is_ok());
    options.replications = 0;
    assert!(options.validate().is_err());
    options.replications = 20;
    options.modes.clear();
    assert!(options.validate().is_err());
    options.modes = vec![ControlMode::PreserveBtc, ControlMode::PreserveBtc];
    assert!(options.validate().is_err());
}
