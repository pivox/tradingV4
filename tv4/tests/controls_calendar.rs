//! Constantes issues de calendar_oracle.py : sous-ensembles indépendants de la DP Rust.
use std::collections::BTreeSet;

use num_bigint::BigUint;
use serde::Deserialize;
use tv4::research::controls::{
    AnnualQuota, Budgets, CalendarTable, Interval, PairCatalogue, PlanningFailure, PlanningWork,
};

#[derive(Deserialize)]
struct OracleCase {
    name: String,
    intervals: Vec<OracleInterval>,
    quotas: Vec<(i32, u32, u32)>,
    total: u32,
    calendars: Vec<Vec<u32>>,
}

#[derive(Deserialize)]
struct OracleInterval {
    id: u32,
    decision: usize,
    signal_ms: i64,
    entry_ms: i64,
    release_ms: i64,
    next_decision: usize,
    fill_year: i32,
    retained: bool,
}

fn catalogue(intervals: Vec<OracleInterval>) -> PairCatalogue {
    PairCatalogue {
        symbol: "BTC-USDC".into(),
        intervals: intervals
            .into_iter()
            .map(|x| Interval {
                id: x.id,
                decision: x.decision,
                signal_ms: x.signal_ms,
                entry_ms: x.entry_ms,
                release_ms: x.release_ms,
                next_decision: x.next_decision,
                fill_year: x.fill_year,
                retained: x.retained,
                clocks_hash: "fixture".into(),
            })
            .collect(),
    }
}

fn quota(year: i32, filled: u32, retained: u32) -> AnnualQuota {
    AnnualQuota {
        year,
        filled,
        retained,
    }
}

fn regular(n: u32, duration: usize) -> PairCatalogue {
    // Tous les signaux et fills de cette fixture restent réellement en 2020 UTC.
    const START_MS: i64 = 1_577_836_800_000;
    PairCatalogue {
        symbol: "BTC-USDC".into(),
        intervals: (0..n)
            .map(|id| {
                let decision = id as usize;
                let signal_ms = START_MS + i64::from(id) * 60_000;
                Interval {
                    id,
                    decision,
                    signal_ms,
                    entry_ms: signal_ms,
                    release_ms: signal_ms + i64::try_from(duration).expect("durée") * 60_000,
                    next_decision: decision + duration,
                    fill_year: 2020,
                    retained: true,
                    clocks_hash: "fixture".into(),
                }
            })
            .collect(),
    }
}

#[test]
fn every_small_rank_bijects_with_the_independent_subset_oracle() {
    let cases: Vec<OracleCase> = serde_json::from_str(ORACLE_JSON).expect("oracle figé");
    for case in cases {
        let catalogue = catalogue(case.intervals);
        let quotas: Vec<_> = case
            .quotas
            .into_iter()
            .map(|(y, f, r)| quota(y, f, r))
            .collect();
        catalogue.validate().expect("catalogue cohérent");
        let result = CalendarTable::build(&catalogue, &quotas, &Budgets::default());
        if case.total == 0 {
            assert!(
                matches!(result, Err(PlanningFailure::EmptySupport)),
                "{}",
                case.name
            );
            continue;
        }
        let table = result.expect(&case.name);
        assert_eq!(table.total(), &BigUint::from(case.total), "{}", case.name);
        let mut actual = BTreeSet::new();
        for rank in 0..case.total {
            let calendar = table.unrank(BigUint::from(rank)).expect("rang valide");
            calendar
                .validate(&catalogue, &quotas)
                .expect("calendrier conforme");
            assert!(
                actual.insert(calendar.interval_ids),
                "rang non injectif : {}",
                case.name
            );
        }
        assert_eq!(
            actual,
            case.calendars.into_iter().collect(),
            "{}",
            case.name
        );
    }
}

fn binomial(n: u32, k: u32) -> BigUint {
    // Produit/division exact indépendant de tout parcours d'intervalles.
    (1..=k).fold(BigUint::from(1_u32), |acc, i| acc * (n - k + i) / i)
}

#[test]
fn fixed_duration_1000_slots_matches_exact_binomial_beyond_u128() {
    let catalogue = regular(1000, 5);
    let table = CalendarTable::build(&catalogue, &[quota(2020, 100, 100)], &Budgets::default())
        .expect("support");
    let expected = binomial(604, 100);
    assert!(expected.bits() > 128);
    assert_eq!(table.total(), &expected);
    for rank in [BigUint::from(0_u32), &expected / 2_u32, &expected - 1_u32] {
        table
            .unrank(rank)
            .expect("rang")
            .validate(&catalogue, &[quota(2020, 100, 100)])
            .expect("conforme");
    }
}

#[test]
fn logical_budget_counts_keys_lengths_and_exact_limbs() {
    let catalogue = regular(1, 1);
    let quotas = [quota(2020, 1, 1)];
    let budgets = Budgets {
        dp_cells_per_geometry: 3,
        dp_bytes_per_geometry: 136,
        ..Budgets::default()
    };
    let mut work = PlanningWork::default();
    let table = CalendarTable::build_with_work(&catalogue, &quotas, &budgets, &mut work)
        .expect("borne inclusive");
    assert_eq!(table.total(), &BigUint::from(1_u32));
    // Racine=1, skip=0 (aucun limb), fin=1 : 3*(32+8) + 2*8.
    assert_eq!(
        work,
        PlanningWork {
            cells: 3,
            logical_bytes: 136
        }
    );
    let mut too_small = budgets;
    too_small.dp_bytes_per_geometry -= 1;
    assert!(matches!(
        CalendarTable::build(&catalogue, &quotas, &too_small),
        Err(PlanningFailure::Budget { .. })
    ));
}

#[test]
fn exact_limb_bytes_grow_beyond_a_single_word() {
    let mut work = PlanningWork::default();
    let table = CalendarTable::build_with_work(
        &regular(80, 1),
        &[quota(2020, 40, 40)],
        &Budgets::default(),
        &mut work,
    )
    .expect("support");
    assert_eq!(table.total(), &binomial(80, 40));
    assert!(table.total().bits() > 64);
    assert!(work.logical_bytes > work.cells * 40 + 8);
    let mut exact = Budgets {
        dp_cells_per_geometry: work.cells,
        dp_bytes_per_geometry: work.logical_bytes,
        ..Budgets::default()
    };
    assert!(CalendarTable::build(&regular(80, 1), &[quota(2020, 40, 40)], &exact).is_ok());
    exact.dp_bytes_per_geometry -= 1;
    assert!(matches!(
        CalendarTable::build(&regular(80, 1), &[quota(2020, 40, 40)], &exact),
        Err(PlanningFailure::Budget { .. })
    ));
}

#[test]
fn planning_budget_is_shared_between_tables() {
    let budgets = Budgets {
        dp_cells_per_geometry: 5,
        ..Budgets::default()
    };
    let mut work = PlanningWork::default();
    CalendarTable::build_with_work(&regular(1, 1), &[quota(2020, 1, 1)], &budgets, &mut work)
        .expect("première paire");
    let failure =
        CalendarTable::build_with_work(&regular(1, 1), &[quota(2020, 1, 1)], &budgets, &mut work)
            .expect_err("deuxième paire");
    assert_eq!(
        failure,
        PlanningFailure::Budget {
            cells: work.cells,
            logical_bytes: work.logical_bytes
        }
    );
    assert_eq!(work.cells, 5);
}

#[test]
fn large_catalogue_stops_after_ten_discovered_states() {
    let budgets = Budgets {
        dp_cells_per_geometry: 10,
        ..Budgets::default()
    };
    let mut work = PlanningWork::default();
    let failure = CalendarTable::build_with_work(
        &regular(200_000, 1),
        &[quota(2020, 1, 1)],
        &budgets,
        &mut work,
    )
    .expect_err("budget");
    assert_eq!(
        work,
        PlanningWork {
            cells: 10,
            logical_bytes: 400
        }
    );
    assert_eq!(
        failure,
        PlanningFailure::Budget {
            cells: 10,
            logical_bytes: 400
        }
    );
}

#[test]
fn two_hundred_thousand_slots_are_evaluated_without_recursion() {
    let catalogue = regular(200_000, 1);
    let quotas = [quota(2020, 1, 1)];
    let table =
        CalendarTable::build(&catalogue, &quotas, &Budgets::default()).expect("pile explicite");
    assert_eq!(table.total(), &BigUint::from(200_000_u32));
    for rank in [0_u32, 199_999] {
        table
            .unrank(BigUint::from(rank))
            .expect("rang")
            .validate(&catalogue, &quotas)
            .expect("conforme");
    }
}

#[test]
fn empty_support_invalid_inputs_and_bad_ranks_remain_distinct() {
    let catalogue = regular(2, 2);
    let budgets = Budgets::default();
    assert!(matches!(
        CalendarTable::build(&catalogue, &[quota(2020, 2, 2)], &budgets),
        Err(PlanningFailure::EmptySupport)
    ));
    assert!(matches!(
        CalendarTable::build(&catalogue, &[quota(2020, 1, 2)], &budgets),
        Err(PlanningFailure::InvalidInput(_))
    ));
    assert!(matches!(
        CalendarTable::build(
            &catalogue,
            &[quota(2021, 0, 0), quota(2020, 0, 0)],
            &budgets
        ),
        Err(PlanningFailure::InvalidInput(_))
    ));
    let mut invalid = catalogue.clone();
    invalid.intervals[0].next_decision = 1;
    assert!(matches!(
        CalendarTable::build(&invalid, &[], &budgets),
        Err(PlanningFailure::InvalidInput(_))
    ));
    let table = CalendarTable::build(&catalogue, &[], &budgets).expect("calendrier vide");
    assert!(
        table
            .unrank(BigUint::from(0_u32))
            .expect("vide")
            .interval_ids
            .is_empty()
    );
    for rank in [BigUint::from(1_u32), BigUint::from(1_u32) << 200_u32] {
        assert!(matches!(
            table.unrank(rank),
            Err(PlanningFailure::InvalidInput(_))
        ));
    }
}

#[test]
fn counter_overflow_is_a_transactional_budget_failure() {
    for (cells, logical_bytes) in [(u64::MAX, 0), (0, u64::MAX)] {
        let mut work = PlanningWork {
            cells,
            logical_bytes,
        };
        let before = work.clone();
        let budgets = Budgets {
            dp_cells_per_geometry: u64::MAX,
            dp_bytes_per_geometry: u64::MAX,
            ..Budgets::default()
        };
        assert!(matches!(
            CalendarTable::build_with_work(&regular(1, 1), &[], &budgets, &mut work),
            Err(PlanningFailure::Budget { .. })
        ));
        assert_eq!(work, before);
    }
}

#[test]
fn build_and_unrank_preserve_all_caller_inputs() {
    let catalogue = regular(8, 2);
    let quotas = vec![quota(2020, 3, 3)];
    let budgets = Budgets::default();
    let before = (catalogue.clone(), quotas.clone(), budgets);
    let table = CalendarTable::build(&catalogue, &quotas, &budgets).expect("support");
    for rank in 0_u32..20 {
        table.unrank(BigUint::from(rank)).expect("rang");
    }
    assert_eq!((catalogue, quotas, budgets), before);
}

// Régénération : python3 tv4/tests/controls_common/calendar_oracle.py --rust
const ORACLE_JSON: &str = r#"[{"calendars":[[0,2,4],[0,2,5],[0,2,6],[0,2,7],[0,3,5],[0,3,6],[0,3,7],[0,4,6],[0,4,7],[0,5,7],[1,3,5],[1,3,6],[1,3,7],[1,4,6],[1,4,7],[1,5,7],[2,4,6],[2,4,7],[2,5,7],[3,5,7]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":3,"release_ms":1577836980000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":5,"release_ms":1577837100000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":true,"signal_ms":1577837220000}],"name":"fixed_8_2_3","quotas":[[2020,3,3]],"total":20},{"calendars":[[]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":true,"signal_ms":1577837220000},{"decision":8,"entry_ms":1577837280000,"fill_year":2020,"id":8,"next_decision":10,"release_ms":1577837400000,"retained":true,"signal_ms":1577837280000},{"decision":9,"entry_ms":1577837340000,"fill_year":2020,"id":9,"next_decision":11,"release_ms":1577837460000,"retained":true,"signal_ms":1577837340000}],"name":"variable_10_q0","quotas":[[2020,0,0]],"total":1},{"calendars":[[0],[1],[2],[3],[4],[5],[6],[7],[8],[9]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":true,"signal_ms":1577837220000},{"decision":8,"entry_ms":1577837280000,"fill_year":2020,"id":8,"next_decision":10,"release_ms":1577837400000,"retained":true,"signal_ms":1577837280000},{"decision":9,"entry_ms":1577837340000,"fill_year":2020,"id":9,"next_decision":11,"release_ms":1577837460000,"retained":true,"signal_ms":1577837340000}],"name":"variable_10_q1","quotas":[[2020,1,1]],"total":10},{"calendars":[[0,2],[0,3],[0,4],[0,5],[0,6],[0,7],[0,8],[0,9],[1,4],[1,5],[1,6],[1,7],[1,8],[1,9],[2,4],[2,5],[2,6],[2,7],[2,8],[2,9],[3,7],[3,8],[3,9],[4,6],[4,7],[4,8],[4,9],[5,8],[5,9],[6,8],[6,9],[7,9]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":true,"signal_ms":1577837220000},{"decision":8,"entry_ms":1577837280000,"fill_year":2020,"id":8,"next_decision":10,"release_ms":1577837400000,"retained":true,"signal_ms":1577837280000},{"decision":9,"entry_ms":1577837340000,"fill_year":2020,"id":9,"next_decision":11,"release_ms":1577837460000,"retained":true,"signal_ms":1577837340000}],"name":"variable_10_q2","quotas":[[2020,2,2]],"total":32},{"calendars":[[0,2,4],[0,2,5],[0,2,6],[0,2,7],[0,2,8],[0,2,9],[0,3,7],[0,3,8],[0,3,9],[0,4,6],[0,4,7],[0,4,8],[0,4,9],[0,5,8],[0,5,9],[0,6,8],[0,6,9],[0,7,9],[1,4,6],[1,4,7],[1,4,8],[1,4,9],[1,5,8],[1,5,9],[1,6,8],[1,6,9],[1,7,9],[2,4,6],[2,4,7],[2,4,8],[2,4,9],[2,5,8],[2,5,9],[2,6,8],[2,6,9],[2,7,9],[3,7,9],[4,6,8],[4,6,9],[4,7,9]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":true,"signal_ms":1577837220000},{"decision":8,"entry_ms":1577837280000,"fill_year":2020,"id":8,"next_decision":10,"release_ms":1577837400000,"retained":true,"signal_ms":1577837280000},{"decision":9,"entry_ms":1577837340000,"fill_year":2020,"id":9,"next_decision":11,"release_ms":1577837460000,"retained":true,"signal_ms":1577837340000}],"name":"variable_10_q3","quotas":[[2020,3,3]],"total":40},{"calendars":[[0,2,4,6],[0,2,4,7],[0,2,4,8],[0,2,4,9],[0,2,5,8],[0,2,5,9],[0,2,6,8],[0,2,6,9],[0,2,7,9],[0,3,7,9],[0,4,6,8],[0,4,6,9],[0,4,7,9],[1,4,6,8],[1,4,6,9],[1,4,7,9],[2,4,6,8],[2,4,6,9],[2,4,7,9]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":true,"signal_ms":1577837220000},{"decision":8,"entry_ms":1577837280000,"fill_year":2020,"id":8,"next_decision":10,"release_ms":1577837400000,"retained":true,"signal_ms":1577837280000},{"decision":9,"entry_ms":1577837340000,"fill_year":2020,"id":9,"next_decision":11,"release_ms":1577837460000,"retained":true,"signal_ms":1577837340000}],"name":"variable_10_q4","quotas":[[2020,4,4]],"total":19},{"calendars":[[0,2,6],[0,2,7],[0,2,8],[0,2,9],[0,2,10],[0,2,11],[0,3,8],[0,3,9],[0,3,10],[0,3,11],[0,4,10],[0,4,11],[0,7,9],[0,7,10],[0,7,11],[0,8,11],[1,4,10],[1,4,11],[1,7,9],[1,7,10],[1,7,11],[1,8,11],[2,7,9],[2,7,10],[2,7,11],[2,8,11],[3,8,11]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":8,"release_ms":1577837280000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":10,"release_ms":1577837400000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":12,"release_ms":1577837520000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":14,"release_ms":1577837640000,"retained":true,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":true,"signal_ms":1577837220000},{"decision":8,"entry_ms":1577837280000,"fill_year":2020,"id":8,"next_decision":11,"release_ms":1577837460000,"retained":true,"signal_ms":1577837280000},{"decision":9,"entry_ms":1577837340000,"fill_year":2020,"id":9,"next_decision":13,"release_ms":1577837580000,"retained":true,"signal_ms":1577837340000},{"decision":10,"entry_ms":1577837400000,"fill_year":2020,"id":10,"next_decision":15,"release_ms":1577837700000,"retained":true,"signal_ms":1577837400000},{"decision":11,"entry_ms":1577837460000,"fill_year":2020,"id":11,"next_decision":17,"release_ms":1577837820000,"retained":true,"signal_ms":1577837460000}],"name":"variable_2_to_8","quotas":[[2020,3,3]],"total":27},{"calendars":[[0,6],[0,7],[1,6],[1,7],[2,6],[2,7],[3,6],[3,7],[4,6],[4,7],[5,7]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":3,"release_ms":1577836980000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":5,"release_ms":1577837100000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":false,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":false,"signal_ms":1577837220000}],"name":"terminal_purge","quotas":[[2020,2,1]],"total":11},{"calendars":[[1,2],[1,3]],"intervals":[{"decision":0,"entry_ms":1609459200000,"fill_year":2021,"id":0,"next_decision":4,"release_ms":1609459320000,"retained":true,"signal_ms":1609459080000},{"decision":1,"entry_ms":1609459140000,"fill_year":2020,"id":1,"next_decision":2,"release_ms":1609459200000,"retained":true,"signal_ms":1609459140000},{"decision":2,"entry_ms":1609459200000,"fill_year":2021,"id":2,"next_decision":3,"release_ms":1609459260000,"retained":true,"signal_ms":1609459200000},{"decision":4,"entry_ms":1609459320000,"fill_year":2021,"id":3,"next_decision":5,"release_ms":1609459380000,"retained":true,"signal_ms":1609459320000}],"name":"non_monotone_fills","quotas":[[2020,1,1],[2021,1,1]],"total":2},{"calendars":[[0],[2],[3]],"intervals":[{"decision":0,"entry_ms":1609459200000,"fill_year":2021,"id":0,"next_decision":4,"release_ms":1609459320000,"retained":true,"signal_ms":1609459080000},{"decision":1,"entry_ms":1609459140000,"fill_year":2020,"id":1,"next_decision":2,"release_ms":1609459200000,"retained":true,"signal_ms":1609459140000},{"decision":2,"entry_ms":1609459200000,"fill_year":2021,"id":2,"next_decision":3,"release_ms":1609459260000,"retained":true,"signal_ms":1609459200000},{"decision":4,"entry_ms":1609459320000,"fill_year":2021,"id":3,"next_decision":5,"release_ms":1609459380000,"retained":true,"signal_ms":1609459320000}],"name":"zero_before_january_fill","quotas":[[2020,0,0],[2021,1,1]],"total":3},{"calendars":[],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":3,"release_ms":1577836980000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":5,"release_ms":1577837100000,"retained":true,"signal_ms":1577836980000}],"name":"missing_year","quotas":[[2021,1,1]],"total":0},{"calendars":[[]],"intervals":[{"decision":0,"entry_ms":1609459200000,"fill_year":2021,"id":0,"next_decision":4,"release_ms":1609459320000,"retained":true,"signal_ms":1609459080000},{"decision":1,"entry_ms":1609459140000,"fill_year":2020,"id":1,"next_decision":2,"release_ms":1609459200000,"retained":true,"signal_ms":1609459140000},{"decision":2,"entry_ms":1609459200000,"fill_year":2021,"id":2,"next_decision":3,"release_ms":1609459260000,"retained":true,"signal_ms":1609459200000},{"decision":4,"entry_ms":1609459320000,"fill_year":2021,"id":3,"next_decision":5,"release_ms":1609459380000,"retained":true,"signal_ms":1609459320000}],"name":"all_zero","quotas":[[2020,0,0],[2021,0,0]],"total":1},{"calendars":[[]],"intervals":[{"decision":0,"entry_ms":1609459200000,"fill_year":2021,"id":0,"next_decision":4,"release_ms":1609459320000,"retained":true,"signal_ms":1609459080000},{"decision":1,"entry_ms":1609459140000,"fill_year":2020,"id":1,"next_decision":2,"release_ms":1609459200000,"retained":true,"signal_ms":1609459140000},{"decision":2,"entry_ms":1609459200000,"fill_year":2021,"id":2,"next_decision":3,"release_ms":1609459260000,"retained":true,"signal_ms":1609459200000},{"decision":4,"entry_ms":1609459320000,"fill_year":2021,"id":3,"next_decision":5,"release_ms":1609459380000,"retained":true,"signal_ms":1609459320000}],"name":"absent_quotas","quotas":[],"total":1},{"calendars":[[0,6],[0,7],[1,6],[1,7],[2,6],[2,7],[3,6],[3,7],[4,6],[4,7],[5,7]],"intervals":[{"decision":0,"entry_ms":1577836800000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1577836920000,"retained":true,"signal_ms":1577836800000},{"decision":1,"entry_ms":1577836860000,"fill_year":2020,"id":1,"next_decision":3,"release_ms":1577836980000,"retained":true,"signal_ms":1577836860000},{"decision":2,"entry_ms":1577836920000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1577837040000,"retained":true,"signal_ms":1577836920000},{"decision":3,"entry_ms":1577836980000,"fill_year":2020,"id":3,"next_decision":5,"release_ms":1577837100000,"retained":true,"signal_ms":1577836980000},{"decision":4,"entry_ms":1577837040000,"fill_year":2020,"id":4,"next_decision":6,"release_ms":1577837160000,"retained":true,"signal_ms":1577837040000},{"decision":5,"entry_ms":1577837100000,"fill_year":2020,"id":5,"next_decision":7,"release_ms":1577837220000,"retained":true,"signal_ms":1577837100000},{"decision":6,"entry_ms":1577837160000,"fill_year":2020,"id":6,"next_decision":8,"release_ms":1577837280000,"retained":false,"signal_ms":1577837160000},{"decision":7,"entry_ms":1577837220000,"fill_year":2020,"id":7,"next_decision":9,"release_ms":1577837340000,"retained":false,"signal_ms":1577837220000}],"name":"zero_after_terminal_purge","quotas":[[2020,2,1],[2021,0,0]],"total":11},{"calendars":[[0,3,5],[1,3,5]],"intervals":[{"decision":0,"entry_ms":1609459020000,"fill_year":2020,"id":0,"next_decision":2,"release_ms":1609459140000,"retained":true,"signal_ms":1609459020000},{"decision":1,"entry_ms":1609459080000,"fill_year":2020,"id":1,"next_decision":3,"release_ms":1609459200000,"retained":true,"signal_ms":1609459080000},{"decision":2,"entry_ms":1609459140000,"fill_year":2020,"id":2,"next_decision":4,"release_ms":1609459260000,"retained":true,"signal_ms":1609459140000},{"decision":3,"entry_ms":1609459200000,"fill_year":2021,"id":3,"next_decision":5,"release_ms":1609459320000,"retained":true,"signal_ms":1609459200000},{"decision":4,"entry_ms":1609459260000,"fill_year":2021,"id":4,"next_decision":6,"release_ms":1609459380000,"retained":true,"signal_ms":1609459260000},{"decision":5,"entry_ms":1609459320000,"fill_year":2021,"id":5,"next_decision":7,"release_ms":1609459440000,"retained":true,"signal_ms":1609459320000}],"name":"two_years","quotas":[[2020,1,1],[2021,2,2]],"total":2}]"#;
