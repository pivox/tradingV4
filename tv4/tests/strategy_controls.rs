//! Géométrie S14 sur des colonnes synthétiques, sans moteur ni données de marché.

use tv4::model::{Candle, ExchangeId, FeeSchedule, Timeframe};
use tv4::strategy::controls::{EntryIntervention, GeometryState};
use tv4::strategy::sma_cross::{SmaCross, SmaCrossParams, TrendParams};
use tv4::strategy::{
    BarContext, BarState, IndicatorSpec, OrderKind, Position, Signal, Size, Strategy,
    StrategyError, StrategyFunnel, TfView,
};

const T0: i64 = 1_704_067_200_000;

fn strategy(trend: bool) -> SmaCross {
    SmaCross::new(SmaCrossParams {
        tf: Timeframe::M15,
        fast: 2,
        slow: 3,
        stop_atr_mult: 2.0,
        trend: trend.then_some(TrendParams {
            tf: Timeframe::H1,
            sma: 2,
        }),
    })
    .unwrap()
}

fn position() -> Position {
    Position {
        qty: 1.0,
        avg_price: 100.0,
        stop: None,
        take_profit: None,
        opened_at_ms: T0,
    }
}

struct Fixture {
    candles: Vec<Candle>,
    trend_candles: Vec<Candle>,
    fast: Vec<f64>,
    slow: Vec<f64>,
    atr: Vec<f64>,
    trend: Vec<f64>,
}

impl Fixture {
    fn new(diffs: &[f64]) -> Self {
        let candles: Vec<_> = (0..diffs.len())
            .map(|i| Candle {
                open_time_ms: T0 + i as i64 * Timeframe::M15.ms(),
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 1.0,
                trades: None,
            })
            .collect();
        let trend_candles = candles
            .iter()
            .enumerate()
            .map(|(i, c)| Candle {
                open_time_ms: T0 - (diffs.len() - i) as i64 * Timeframe::H1.ms(),
                ..*c
            })
            .collect();
        Self {
            candles,
            trend_candles,
            fast: diffs.iter().map(|d| 100.0 + d).collect(),
            slow: vec![100.0; diffs.len()],
            atr: vec![1.0; diffs.len()],
            trend: vec![99.0; diffs.len()],
        }
    }

    fn with_ctx<R>(
        &self,
        i: usize,
        pos: Option<Position>,
        mask: u8,
        f: impl FnOnce(&BarContext<'_>) -> R,
    ) -> R {
        let columns = [
            self.fast.as_slice(),
            self.slow.as_slice(),
            self.atr.as_slice(),
        ];
        let trend_columns = [self.trend.as_slice()];
        let views = [
            TfView::new(
                Timeframe::M15,
                &self.candles,
                &["fast", "slow", "atr"],
                &columns,
            )
            .unwrap()
            .with_closed(i + 1)
            .unwrap(),
            TfView::new(
                Timeframe::H1,
                &self.trend_candles,
                &["trend"],
                &trend_columns,
            )
            .unwrap()
            .with_closed(i + 1)
            .unwrap(),
        ];
        let state = BarState {
            now_ms: self.candles[i].open_time_ms + Timeframe::M15.ms(),
            equity: 300.0,
            quote_free: 300.0,
            qty_free: pos.map_or(0.0, |p| p.qty),
            position: pos,
            fees: FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
        };
        let ctx = BarContext::new("TEST-USDC", state, &[], &views, mask).unwrap();
        f(&ctx)
    }
}

/// Oracle de la logique Observed 1.1.0, indépendant des helpers de production.
#[derive(Default)]
struct ObservedOracle {
    prev: Option<f64>,
    raw: u64,
    stop: u64,
    trend: u64,
}
impl ObservedOracle {
    fn step(&mut self, fixture: &Fixture, i: usize, has_position: bool) -> Vec<Signal> {
        let (fast, slow, atr) = (fixture.fast[i], fixture.slow[i], fixture.atr[i]);
        if !(fast.is_finite() && slow.is_finite() && atr.is_finite()) {
            self.prev = None;
            return vec![];
        }
        let diff = fast - slow;
        let stop = fixture.candles[i].close - 2.0 * atr;
        let mut out = vec![];
        match (self.prev, has_position) {
            (Some(p), false) if p <= 0.0 && diff > 0.0 => {
                self.raw += 1;
                if stop <= 0.0 || stop.is_nan() {
                    self.stop += 1;
                } else if !fixture.trend[i].is_finite()
                    || fixture.trend_candles[i].close <= fixture.trend[i]
                {
                    self.trend += 1;
                } else {
                    out.push(Signal::EnterLong {
                        size: Size::RiskPct(0.01),
                        kind: OrderKind::Market,
                        stop: Some(stop),
                        take_profit: None,
                        tag: "cross_up",
                    });
                }
            }
            (Some(p), true) if p >= 0.0 && diff < 0.0 => out.push(Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "cross_down",
            }),
            _ => {}
        }
        self.prev = Some(diff);
        out
    }
}

#[test]
fn sma_geometry_preserves_observed() {
    let mut fixture = Fixture::new(&[
        -1.0,
        1.0,
        -1.0,
        1.0,
        -1.0,
        1.0,
        -1.0,
        1.0,
        f64::NAN,
        1.0,
        -1.0,
        1.0,
    ]);
    fixture.atr[3] = 50.0;
    fixture.trend[5] = 101.0;
    let mut observed = strategy(true);
    let params = observed.params_json();
    let mut geometry = observed.control_geometry().unwrap();
    let mut oracle = ObservedOracle::default();
    let mut has_position = false;
    for i in 0..fixture.fast.len() {
        fixture.with_ctx(
            i,
            has_position.then(position),
            Timeframe::M15.bit(),
            |ctx| {
                let expected = oracle.step(&fixture, i, has_position);
                let mut actual = vec![];
                observed.on_bar(ctx, &mut actual);
                assert_eq!(actual, expected, "Observed au pas {i}");
                let mut exits = vec![];
                geometry.exits(ctx, &mut exits);
                assert_eq!(
                    exits,
                    expected
                        .iter()
                        .filter(|s| matches!(s, Signal::ExitLong { .. }))
                        .copied()
                        .collect::<Vec<_>>()
                );
                if expected
                    .iter()
                    .any(|s| matches!(s, Signal::EnterLong { .. }))
                {
                    assert_eq!(
                        geometry.entry(ctx, EntryIntervention::PreserveBtc),
                        expected.first().copied()
                    );
                }
                for signal in expected {
                    has_position = matches!(signal, Signal::EnterLong { .. });
                }
            },
        );
    }
    assert_eq!(
        observed.funnel(),
        Some(StrategyFunnel {
            raw_entry_signals: oracle.raw,
            filtered: vec![
                ("stop_not_positive", oracle.stop),
                ("trend_filter", oracle.trend)
            ]
        })
    );
    assert_eq!((oracle.raw, oracle.stop, oracle.trend), (5, 1, 1));
    assert_eq!(observed.version(), "1.1.0");
    assert_eq!(observed.params_json(), params);
    assert!(!observed.uses_regime());
    assert_eq!(
        observed.control_geometry().unwrap().capture_state(),
        GeometryState::SmaCross { prev_diff: None }
    );
}

#[test]
fn sma_seed_is_causal_after_gap() {
    let fixture = Fixture::new(&[-1.0, 1.0, -2.0, 3.0, -4.0, 5.0]);
    let mut changed = Fixture::new(&[-1.0, 1.0, -2.0, 3.0, -4.0, 5.0]);
    for i in 4..changed.fast.len() {
        changed.fast[i] = 1e20;
        changed.slow[i] = -1e20;
        changed.atr[i] = f64::NAN;
        changed.trend[i] = f64::INFINITY;
        changed.candles[i] = Candle {
            open: 1000.0,
            high: 2000.0,
            low: 1.0,
            close: 1500.0,
            volume: 42.0,
            trades: Some(42),
            ..changed.candles[i]
        };
        changed.trend_candles[i] = changed.candles[i];
    }
    let collect = |fixture: &Fixture| {
        let mut geometry = strategy(true).control_geometry().unwrap();
        let mut slots = vec![];
        // Le moteur n'a évalué ni le warmup 0..2, ni le pas 2 pendant le trou.
        for i in [1, 3] {
            fixture.with_ctx(i, None, Timeframe::M15.bit(), |ctx| {
                slots.push((
                    geometry.capture_state(),
                    geometry.entry(ctx, EntryIntervention::PreserveBtc),
                ));
                geometry.advance_flat(ctx);
            });
        }
        slots
    };
    let slots = collect(&fixture);
    assert_eq!(slots, collect(&changed));
    assert_eq!(slots[0].0, GeometryState::SmaCross { prev_diff: None });
    assert_eq!(
        slots[1].0,
        GeometryState::SmaCross {
            prev_diff: Some(1.0)
        }
    );
    assert!(slots.iter().all(|(_, entry)| entry.is_some()));
}

#[test]
fn trend_only_close_keeps_previous_diff() {
    let fixture = Fixture::new(&[-1.0, 1.0, -1.0]);
    let mut observed = strategy(true);
    let mut geometry = observed.control_geometry().unwrap();
    fixture.with_ctx(0, None, Timeframe::M15.bit(), |ctx| {
        observed.on_bar(ctx, &mut vec![]);
        geometry.advance_flat(ctx);
    });
    let seed = geometry.capture_state();
    fixture.with_ctx(1, None, Timeframe::H1.bit(), |ctx| {
        geometry.advance_flat(ctx);
        geometry.exits(ctx, &mut vec![]);
        assert_eq!(geometry.entry(ctx, EntryIntervention::PreserveBtc), None);
        observed.on_bar(ctx, &mut vec![]);
    });
    assert_eq!(geometry.capture_state(), seed);
    fixture.with_ctx(1, None, Timeframe::M15.bit(), |ctx| {
        let mut out = vec![];
        observed.on_bar(ctx, &mut out);
        assert!(matches!(out.as_slice(), [Signal::EnterLong { .. }]));
    });
}

#[test]
fn restored_seed_preserves_inverse_crossing_and_rejects_wrong_state() {
    let fixture = Fixture::new(&[1.0, -1.0, f64::NAN, -1.0, 1.0]);
    let mut geometry = strategy(false).control_geometry().unwrap();
    fixture.with_ctx(0, None, Timeframe::M15.bit(), |ctx| {
        geometry.advance_flat(ctx)
    });
    let seed = geometry.capture_state();
    fixture.with_ctx(2, None, Timeframe::M15.bit(), |ctx| {
        geometry.advance_flat(ctx)
    });
    assert_eq!(
        geometry.capture_state(),
        GeometryState::SmaCross { prev_diff: None }
    );
    geometry.restore_state(&seed).unwrap();
    assert!(geometry.restore_state(&GeometryState::Stateless).is_err());
    assert_eq!(geometry.capture_state(), seed);
    fixture.with_ctx(1, Some(position()), Timeframe::M15.bit(), |ctx| {
        let mut out = vec![];
        geometry.exits(ctx, &mut out);
        assert_eq!(
            out,
            [Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "cross_down"
            }]
        );
    });
    assert_eq!(
        geometry.capture_state(),
        GeometryState::SmaCross {
            prev_diff: Some(-1.0)
        }
    );
}

#[test]
fn random_entry_replaces_only_trigger_and_preserves_admissibility() {
    let mut fixture = Fixture::new(&[-1.0; 8]);
    fixture.atr = vec![1.0, 50.0, f64::NAN, f64::INFINITY, 2.0, 1.0, 1.0, 1.0];
    fixture.trend[4] = 100.0;
    fixture.trend[5] = f64::NAN;
    fixture.fast[6] = f64::INFINITY;
    let geometry = strategy(true).control_geometry().unwrap();
    for i in 0..fixture.fast.len() {
        fixture.with_ctx(i, None, Timeframe::M15.bit(), |ctx| {
            let entry = geometry.entry(ctx, EntryIntervention::PreserveBtc);
            assert_eq!(entry.is_some(), i == 0 || i == 7, "pas {i}");
            assert_eq!(entry, geometry.entry(ctx, EntryIntervention::RemoveBtc));
            if let Some(Signal::EnterLong { stop, .. }) = entry {
                assert_eq!(stop, Some(98.0));
            }
        });
    }
    // L'entrée est une intention indépendante du cash et de la quantité du portefeuille.
    fixture.with_ctx(0, Some(position()), Timeframe::M15.bit(), |ctx| {
        assert!(
            geometry
                .entry(ctx, EntryIntervention::PreserveBtc)
                .is_some()
        );
    });
}

struct Unsupported;
impl Strategy for Unsupported {
    fn id(&self) -> &'static str {
        "unsupported_fixture"
    }
    fn version(&self) -> &'static str {
        "1.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M15]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        vec![]
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn on_bar(&mut self, _: &BarContext<'_>, _: &mut Vec<Signal>) {}
}

#[test]
fn unsupported_geometry_is_explicit() {
    assert!(
        matches!(Unsupported.control_geometry(), Err(StrategyError::UnsupportedControlGeometry { id: "unsupported_fixture", reason }) if !reason.is_empty())
    );
}

#[test]
fn geometry_descriptor_is_stable_and_contains_no_run_state() {
    let mut geometry = strategy(true).control_geometry().unwrap();
    let descriptor = geometry.descriptor();
    assert_eq!(descriptor["contract_version"], "1.0.0");
    assert_eq!(descriptor["decision_tf"], "15m");
    assert_eq!(descriptor["warmup_bars"], 14);
    assert_eq!(descriptor["indicators"].as_array().unwrap().len(), 4);
    assert!(descriptor["clocks"].is_object());
    let caps = geometry.capabilities();
    assert!(caps.single_lot && caps.reconstructible);
    assert!(!caps.partial_exits && !caps.quantity_dependent && !caps.has_btc_filter);
    let fixture = Fixture::new(&[1.0]);
    fixture.with_ctx(0, None, Timeframe::M15.bit(), |ctx| {
        geometry.advance_flat(ctx)
    });
    assert_eq!(geometry.descriptor(), descriptor);
    assert_eq!(descriptor["capabilities"]["has_btc_filter"], false);
    assert_ne!(
        descriptor,
        strategy(false).control_geometry().unwrap().descriptor()
    );
}
