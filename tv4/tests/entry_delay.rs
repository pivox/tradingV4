//! Marché inventé : délai, réservations et TTL sans accès externe.
use tv4::model::{Candle, ExchangeId, FeeSchedule, InstrumentRules, Side, Slippage};
use tv4::sim::{Instruction, OrderPrice, Rejection, SimRules, Simulator, SizeSpec, SubmitContext};
const M: i64 = 60_000;
fn simulator(delay: u32) -> Simulator {
    Simulator::new(
        FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
        Slippage::ZERO,
        SimRules {
            extra_entry_delay_bars: delay,
            ..SimRules::default()
        },
        InstrumentRules::new(0.01, 1e-6, 0., 0.).unwrap(),
        1000.,
        16,
    )
    .unwrap()
}
fn bar(at: i64) -> Candle {
    Candle {
        open_time_ms: at,
        open: 100.,
        high: 101.,
        low: 99.,
        close: 100.,
        volume: 1e6,
        trades: None,
    }
}
fn ctx(signal_ms: i64) -> SubmitContext {
    SubmitContext {
        signal_ms,
        equity: 1000.,
        ref_price: 100.,
    }
}
fn enter(limit: bool) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(100.),
        price: if limit {
            OrderPrice::Limit {
                price: 99.5,
                ttl_bars: 2,
            }
        } else {
            OrderPrice::Market
        },
        stop: Some(90.),
        take_profit: None,
        tag: "entry",
    }
}
#[test]
fn buys_wait_exactly_the_extra_minutes_and_keep_their_reservation() {
    for delay in [1, 15, 240] {
        for kind in 0..3 {
            let mut s = simulator(delay);
            let order = if kind == 2 {
                Instruction::Place {
                    side: Side::Buy,
                    price: 99.5,
                    size: SizeSpec::Quote(100.),
                    ttl_bars: 2,
                    slot: 1,
                    tag: "buy",
                }
            } else {
                enter(kind == 1)
            };
            s.submit(order, ctx(0)).unwrap();
            assert_eq!(s.pending_len(), 1);
            if kind > 0 {
                assert!(s.quote_free() < s.cash());
                assert_eq!(
                    s.pending().next().unwrap().expires_at_ms,
                    Some((i64::from(delay) + 3) * M)
                );
            }
            for minute in 0..=delay {
                s.on_bar(&bar(i64::from(minute) * M)).unwrap();
                assert_eq!(s.qty(), 0.);
                assert_eq!(s.pending_len(), 1);
            }
            s.on_bar(&bar((i64::from(delay) + 1) * M)).unwrap();
            assert!(s.qty() > 0.);
        }
    }
}
#[test]
fn extra_zero_is_absent_and_delay_is_bounded() {
    assert!(
        serde_json::to_value(SimRules::default())
            .unwrap()
            .get("extra_entry_delay_bars")
            .is_none()
    );
    let mut r = SimRules {
        extra_entry_delay_bars: 1440,
        ..SimRules::default()
    };
    assert!(r.validate().is_ok());
    r.extra_entry_delay_bars = 1441;
    assert!(r.validate().is_err());
}
#[test]
fn entry_activation_and_buy_ttl_overflow_are_explicit_rejections() {
    let mut s = simulator(1);
    assert_eq!(
        s.submit(enter(false), ctx(i64::MAX - M)),
        Err(Rejection::TimestampOverflow)
    );
    assert_eq!(
        s.submit(enter(true), ctx(i64::MAX - 3 * M)),
        Err(Rejection::TimestampOverflow)
    );
    assert_eq!(s.pending_len(), 0);
}
#[test]
fn cancel_keeps_the_global_delay() {
    for instruction in [Instruction::CancelAll, Instruction::CancelSlot { slot: 1 }] {
        let mut s = simulator(15);
        s.submit(
            Instruction::Place {
                side: Side::Buy,
                price: 99.5,
                size: SizeSpec::Quote(100.),
                ttl_bars: 2,
                slot: 1,
                tag: "buy",
            },
            ctx(0),
        )
        .unwrap();
        s.submit(instruction, ctx(0)).unwrap();
        s.on_bar(&bar(M)).unwrap();
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.quote_free(), s.cash());
    }
}

#[test]
fn sell_exit_and_move_stop_keep_the_global_delay() {
    for instruction in [
        Instruction::Exit {
            price: OrderPrice::Market,
            tag: "exit",
        },
        Instruction::Exit {
            price: OrderPrice::Limit {
                price: 100.5,
                ttl_bars: 2,
            },
            tag: "exit",
        },
        Instruction::Place {
            side: Side::Sell,
            price: 100.5,
            size: SizeSpec::Quote(200.),
            ttl_bars: 2,
            slot: 1,
            tag: "sell",
        },
    ] {
        let mut s = simulator(15);
        s.submit(enter(false), ctx(0)).unwrap();
        s.on_bar(&bar(16 * M)).unwrap();
        assert!(s.qty() > 0.);
        s.submit(instruction, ctx(17 * M)).unwrap();
        s.on_bar(&bar(17 * M)).unwrap();
        assert!(s.qty() > 0.);
        s.on_bar(&bar(18 * M)).unwrap();
        assert!(s.qty() < 0.011);
    }
    let mut s = simulator(15);
    s.submit(enter(false), ctx(0)).unwrap();
    s.on_bar(&bar(16 * M)).unwrap();
    s.submit(Instruction::MoveStop { stop: 99.5 }, ctx(17 * M))
        .unwrap();
    s.on_bar(&bar(17 * M)).unwrap();
    assert!(s.qty() > 0.);
    s.on_bar(&bar(18 * M)).unwrap();
    assert_eq!(s.qty(), 0.);
}

#[test]
fn maker_conversion_keeps_the_delayed_activation_and_ttl() {
    use tv4::model::costs::{EntryMode, LegCost, StopCost};
    let cost = LegCost {
        half_spread_rate: 0.,
        market_slippage_rate: 0.,
        stop: StopCost::Fixed { rate: 0. },
        entry: EntryMode::Maker {
            ttl_minutes: 2,
            fill_probability: 1.,
            offset_rate: 0.005,
        },
        adverse_selection_rate: 0.,
        leg_key: 1,
        seed: 1,
    };
    let mut s = Simulator::with_cost(
        FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
        cost,
        SimRules {
            extra_entry_delay_bars: 15,
            ..SimRules::default()
        },
        InstrumentRules::new(0.01, 1e-6, 0., 0.).unwrap(),
        1000.,
        16,
    )
    .unwrap();
    s.submit(enter(false), ctx(0)).unwrap();
    assert!(s.quote_free() < s.cash());
    assert_eq!(s.pending().next().unwrap().expires_at_ms, Some(18 * M));
    s.on_bar(&bar(15 * M)).unwrap();
    assert_eq!(s.qty(), 0.);
    s.on_bar(&bar(16 * M)).unwrap();
    assert!(s.qty() > 0.);
}

#[test]
fn ttl_runs_for_its_full_duration_after_delayed_activation() {
    for place in [false, true] {
        let mut s = simulator(15);
        let instruction = if place {
            Instruction::Place {
                side: Side::Buy,
                price: 99.5,
                size: SizeSpec::Quote(100.),
                ttl_bars: 2,
                slot: 1,
                tag: "ttl",
            }
        } else {
            enter(true)
        };
        s.submit(instruction, ctx(0)).unwrap();
        for minute in 0..18 {
            let mut candle = bar(minute * M);
            candle.low = 100.;
            s.on_bar(&candle).unwrap();
            assert_eq!(s.pending_len(), 1);
        }
        s.on_bar(&bar(18 * M)).unwrap();
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.qty(), 0.);
        assert_eq!(s.quote_free(), s.cash());
    }
}
