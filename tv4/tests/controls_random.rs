//! Flux S14 purs : frontières exactes, identités stables et vrais calendriers DP.
use std::collections::BTreeMap;

use num_bigint::BigUint;
use tv4::model::rng::{STREAM_STOP_SLIPPAGE, SplitMix64, draw, leg_key};
use tv4::research::controls::random::{bootstrap_rng, calendar_rng, uniform_below};
use tv4::research::controls::{
    AnnualQuota, Budgets, CalendarTable, ControlMode, Interval, PairCatalogue, PlanningFailure,
};

const PARENT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_PARENT: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

#[test]
fn canonical_derivation_has_pinned_vectors_including_u64_max_seed() {
    // BLAKE3 de tableaux JSON littéraux, calculé indépendamment du helper de production.
    // ["tv4-controls-calendar-v1",18446744073709551615,PARENT,"remove_btc",199,127,"ETH-USDC"]
    // => 1372ff45f49a24e6b677156fa6fb7699c7e507d7456825aa50608eec8dfb0168.
    // Bootstrap correspondant (indices 0, symbole vide) : dc0f9b8122981cce67f9a185593302fe...
    for (actual, seed) in [
        (
            calendar_rng(
                u64::MAX,
                PARENT,
                ControlMode::RemoveBtc,
                199,
                127,
                "ETH-USDC",
            ),
            0xe624_9af4_45ff_7213,
        ),
        (
            bootstrap_rng(u64::MAX, PARENT, ControlMode::RemoveBtc),
            0xce1c_9822_819b_0fdc,
        ),
        (
            calendar_rng(0, PARENT, ControlMode::PreserveBtc, 0, 0, "BTC-USDC"),
            0x1421_e0c7_1c77_22f6,
        ),
        (
            bootstrap_rng(0, PARENT, ControlMode::PreserveBtc),
            0x47b6_54df_8f08_45f9,
        ),
    ] {
        assert_eq!(actual.expect("vecteur canonique"), SplitMix64::new(seed));
    }
}

#[test]
fn zero_bound_is_explicitly_invalid_and_does_not_consume() {
    let mut rng = SplitMix64::new(42);
    let original = rng;
    assert!(matches!(
        uniform_below(&mut rng, &BigUint::default()),
        Err(PlanningFailure::InvalidInput(_))
    ));
    assert_eq!(rng, original);
}

#[test]
fn uniform_below_one_is_zero_without_consumption() {
    let mut rng = SplitMix64::new(u64::MAX);
    let original = rng;
    assert_eq!(
        uniform_below(&mut rng, &BigUint::from(1_u32)).expect("un"),
        BigUint::default()
    );
    assert_eq!(rng, original);
}

#[test]
fn full_u64_range_is_the_next_splitmix_word() {
    let mut rng = SplitMix64::new(u64::MAX);
    let mut reference = rng;
    assert_eq!(
        uniform_below(&mut rng, &(BigUint::from(1_u32) << 64)).expect("2^64"),
        BigUint::from(reference.next_u64())
    );
    assert_eq!(rng, reference);
}

#[test]
fn stream_inputs_are_explicitly_validated() {
    for parent in ["", " ", "abcd", "../source", &"A".repeat(64)] {
        assert!(matches!(
            calendar_rng(1, parent, ControlMode::PreserveBtc, 0, 0, "BTC-USDC"),
            Err(PlanningFailure::InvalidInput(_))
        ));
        assert!(matches!(
            bootstrap_rng(1, parent, ControlMode::PreserveBtc),
            Err(PlanningFailure::InvalidInput(_))
        ));
    }
    for symbol in ["", " \t"] {
        assert!(matches!(
            calendar_rng(1, PARENT, ControlMode::PreserveBtc, 0, 0, symbol),
            Err(PlanningFailure::InvalidInput(_))
        ));
    }
}

#[test]
fn each_calendar_key_changes_its_own_stream() {
    let initial =
        calendar_rng(1, PARENT, ControlMode::PreserveBtc, 0, 0, "BTC-USDC").expect("flux");
    for changed in [
        calendar_rng(2, PARENT, ControlMode::PreserveBtc, 0, 0, "BTC-USDC"),
        calendar_rng(1, OTHER_PARENT, ControlMode::PreserveBtc, 0, 0, "BTC-USDC"),
        calendar_rng(1, PARENT, ControlMode::RemoveBtc, 0, 0, "BTC-USDC"),
        calendar_rng(1, PARENT, ControlMode::PreserveBtc, 1, 0, "BTC-USDC"),
        calendar_rng(1, PARENT, ControlMode::PreserveBtc, 0, 1, "BTC-USDC"),
        calendar_rng(1, PARENT, ControlMode::PreserveBtc, 0, 0, "ETH-USDC"),
    ] {
        assert_ne!(initial, changed.expect("flux distinct"));
    }
}

#[test]
fn resolved_aliases_share_exact_calendar_and_bootstrap_streams() {
    let preserve = ControlMode::PreserveBtc.effective(false);
    let remove = ControlMode::RemoveBtc.effective(false);
    assert_eq!(
        calendar_rng(1, PARENT, preserve, 4, 2, "BTC-USDC"),
        calendar_rng(1, PARENT, remove, 4, 2, "BTC-USDC")
    );
    assert_eq!(
        bootstrap_rng(1, PARENT, preserve),
        bootstrap_rng(1, PARENT, remove)
    );
}

#[test]
fn calendar_bootstrap_and_historical_cost_streams_are_distinct() {
    let mut calendar =
        calendar_rng(7, PARENT, ControlMode::PreserveBtc, 0, 0, "BTC-USDC").expect("calendrier");
    let mut bootstrap = bootstrap_rng(7, PARENT, ControlMode::PreserveBtc).expect("bootstrap");
    let cost = draw(
        7,
        [
            leg_key("BTC-USDC"),
            STREAM_STOP_SLIPPAGE,
            1_609_459_200_000,
            0,
        ],
    );
    let (calendar, bootstrap) = (calendar.next_u64(), bootstrap.next_u64());
    assert_ne!(calendar, bootstrap);
    assert_ne!(calendar, cost);
    assert_ne!(bootstrap, cost);
}

fn small_table() -> (PairCatalogue, Vec<AnnualQuota>, CalendarTable) {
    let catalogue = PairCatalogue {
        symbol: "BTC-USDC".into(),
        intervals: (0..10_u32)
            .map(|id| {
                let signal_ms = 1_577_836_800_000 + i64::from(id) * 60_000;
                Interval {
                    id,
                    decision: id as usize,
                    signal_ms,
                    entry_ms: signal_ms,
                    release_ms: signal_ms + 120_000,
                    next_decision: id as usize + 2,
                    fill_year: 2020,
                    retained: true,
                    clocks_hash: "fixture".into(),
                }
            })
            .collect(),
    };
    let quotas = vec![AnnualQuota {
        year: 2020,
        filled: 3,
        retained: 3,
    }];
    let table = CalendarTable::build(&catalogue, &quotas, &Budgets::default()).expect("DP réelle");
    (catalogue, quotas, table)
}

fn calendars(replications: u32) -> Vec<Vec<u32>> {
    let (catalogue, quotas, table) = small_table();
    (0..replications)
        .map(|replication| {
            let mut rng = calendar_rng(
                u64::MAX,
                PARENT,
                ControlMode::PreserveBtc,
                replication,
                0,
                &catalogue.symbol,
            )
            .expect("flux");
            let rank = uniform_below(&mut rng, table.total()).expect("rang exact");
            let calendar = table.unrank(rank).expect("désindexation réelle");
            calendar
                .validate(&catalogue, &quotas)
                .expect("calendrier valide");
            calendar.interval_ids
        })
        .collect()
}

#[test]
fn k200_is_the_exact_prefix_of_k1000_with_real_dp_and_validation() {
    assert_eq!(calendars(200), calendars(1_000)[..200]);
}

fn peer_streams(peers: &[&str]) -> BTreeMap<String, (Vec<u64>, Vec<u64>)> {
    peers
        .iter()
        .map(|parent| {
            let mut calendar =
                calendar_rng(42, parent, ControlMode::RemoveBtc, 199, 127, "ETH-USDC")
                    .expect("calendrier");
            let mut bootstrap =
                bootstrap_rng(42, parent, ControlMode::RemoveBtc).expect("bootstrap");
            (
                (*parent).into(),
                (
                    (0..8).map(|_| calendar.next_u64()).collect(),
                    (0..8).map(|_| bootstrap.next_u64()).collect(),
                ),
            )
        })
        .collect()
}

#[test]
fn peer_order_and_presence_do_not_change_either_stream() {
    assert_eq!(
        peer_streams(&[PARENT, OTHER_PARENT]),
        peer_streams(&[OTHER_PARENT, PARENT])
    );
    assert_eq!(
        peer_streams(&[PARENT])[PARENT],
        peer_streams(&[OTHER_PARENT, PARENT])[PARENT]
    );
}
