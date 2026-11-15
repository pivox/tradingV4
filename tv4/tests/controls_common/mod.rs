//! Marchés et géométrie exogènes synthétiques des contrôles natifs.
#![allow(dead_code)]
use std::sync::Arc;
use tv4::model::{Candle, Timeframe};
use tv4::store::{Columns, MultiDataset};
use tv4::strategy::controls::{
    ControlGeometry, EntryIntervention, GeometryCapabilities, GeometryState,
};
use tv4::strategy::{BarContext, IndicatorSpec, OrderKind, Signal, Size, Strategy, StrategyError};

#[derive(Clone)]
pub struct Fixture {
    pub signal_ms: i64,
    pub observed_every_ms: Option<i64>,
    pub entry_until_ms: Option<i64>,
    pub variable_hold: bool,
    pub stagger_observed: bool,
    pub hold_ms: i64,
    pub limit: Option<(f64, u32)>,
    pub stop: Option<f64>,
    pub tf: Timeframe,
    pub btc: bool,
    pub warmup: usize,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            signal_ms: 0,
            observed_every_ms: None,
            entry_until_ms: None,
            variable_hold: false,
            stagger_observed: false,
            hold_ms: 0,
            limit: None,
            stop: Some(90.0),
            tf: Timeframe::M1,
            btc: false,
            warmup: 0,
        }
    }
}
impl Fixture {
    pub fn signal(&self) -> Signal {
        Signal::EnterLong {
            size: Size::Quote(1000.0),
            kind: self
                .limit
                .map_or(OrderKind::Market, |(price, ttl_bars)| OrderKind::Limit {
                    price,
                    ttl_bars,
                }),
            stop: self.stop,
            take_profit: None,
            tag: "controls",
        }
    }
    pub fn set(&self, n: usize) -> Vec<Box<dyn Strategy>> {
        (0..n)
            .map(|_| Box::new(self.clone()) as Box<dyn Strategy>)
            .collect()
    }
}
impl Strategy for Fixture {
    fn id(&self) -> &'static str {
        "controls_fixture"
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({"hold": self.hold_ms,"signal":self.signal_ms,"limit":self.limit,"tf":self.tf,"btc":self.btc,"stop":self.stop,"warmup":self.warmup,"observed_every":self.observed_every_ms,"until":self.entry_until_ms,"variable":self.variable_hold,"stagger":self.stagger_observed})
    }
    fn timeframes(&self) -> &[Timeframe] {
        std::slice::from_ref(&self.tf)
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn uses_regime(&self) -> bool {
        self.btc
    }
    fn warmup_bars(&self) -> usize {
        self.warmup
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        self.exits(ctx, out);
        let observed_signal = self.signal_ms
            + if self.stagger_observed && ctx.symbol().starts_with("Y-") {
                10 * 60_000
            } else {
                0
            };
        if (ctx.now_ms() == observed_signal
            || self.observed_every_ms.is_some_and(|period| {
                ctx.now_ms() >= observed_signal && (ctx.now_ms() - observed_signal) % period == 0
            }))
            && ctx.position().is_none()
            && ctx.pending().is_empty()
            && let Some(signal) = self.entry(ctx, EntryIntervention::PreserveBtc)
        {
            out.push(signal);
        }
    }
    fn control_geometry(&self) -> Result<Box<dyn ControlGeometry>, StrategyError> {
        Ok(Box::new(self.clone()))
    }
}
impl ControlGeometry for Fixture {
    fn descriptor(&self) -> serde_json::Value {
        self.params_json()
    }
    fn capabilities(&self) -> GeometryCapabilities {
        GeometryCapabilities {
            single_lot: true,
            partial_exits: false,
            quantity_dependent: false,
            reconstructible: true,
            has_btc_filter: self.btc,
        }
    }
    fn capture_state(&self) -> GeometryState {
        GeometryState::Stateless
    }
    fn restore_state(&mut self, s: &GeometryState) -> Result<(), StrategyError> {
        assert_eq!(*s, GeometryState::Stateless);
        Ok(())
    }
    fn advance_flat(&mut self, _: &BarContext<'_>) {}
    fn entry(&self, ctx: &BarContext<'_>, mode: EntryIntervention) -> Option<Signal> {
        if self.entry_until_ms.is_some_and(|end| ctx.now_ms() >= end)
            || !ctx.just_closed(self.tf)
            || (self.btc
                && mode == EntryIntervention::PreserveBtc
                && ctx.regime().and_then(|r| r.bull()) != Some(true))
        {
            None
        } else {
            Some(self.signal())
        }
    }
    fn exits(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        if ctx.just_closed(self.tf)
            && ctx.position().is_some_and(|p| {
                ctx.now_ms()
                    >= p.opened_at_ms
                        + if self.variable_hold {
                            (1 + p.opened_at_ms.div_euclid(60_000).rem_euclid(7)) * 60_000
                        } else {
                            self.hold_ms
                        }
            })
        {
            out.push(Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "exit",
            });
        }
    }
}
pub fn dataset(series: Vec<(String, Vec<Candle>)>) -> Arc<MultiDataset> {
    let manifest=serde_json::from_value(serde_json::json!({"name":"controls","exchange":"okx","symbols":["X-USD"],"tfs":["1m"],"window":["2024-01-01T00:00:00Z","2024-01-02T00:00:00Z"],"gaps_policy":"skip","files":[],"dataset_hash":"blake3:test","row_count":0,"hash_format":"dsh-v1","built_at":"2024-01-01T00:00:00Z","tv4_version":"test"})).unwrap();
    Arc::new(MultiDataset {
        manifest,
        series: series
            .into_iter()
            .map(|(s, c)| (s, Columns::from_candles(&c)))
            .collect(),
    })
}

pub struct Research {
    pub start: i64,
    pub end: i64,
    pub quality: tv4::model::QualitySchedule,
    pub universe_spec: tv4::engine::UniverseSpec,
    pub identity: tv4::engine::ResearchIdentity,
}
impl Research {
    pub fn new(start: i64, end: i64) -> Self {
        Self {
            start,
            end,
            quality: Default::default(),
            universe_spec: tv4::engine::UniverseSpec::Fixed {
                symbols: vec!["X-USD".into()],
            },
            identity: tv4::engine::ResearchIdentity {
                block: tv4::model::Block::Validation,
                decision_start_ms: start,
                decision_end_ms: end,
                universe_window_hash: None,
                quality_hash: tv4::model::QualitySchedule::default().hash().unwrap(),
                line: tv4::engine::Line::Full,
                subset_hash: None,
                legs_hash: String::new(),
            },
        }
    }
    pub fn spec(&self) -> tv4::engine::ResearchSpec<'_> {
        tv4::engine::ResearchSpec {
            block: tv4::model::Block::Validation,
            decision_start_ms: self.start,
            decision_end_ms: self.end,
            universe: None,
            universe_spec: &self.universe_spec,
            quality: &self.quality,
            identity: &self.identity,
        }
    }
    pub fn render(
        &self,
        result: &tv4::engine::PortfolioResult,
        data: &MultiDataset,
    ) -> tv4::report::profiles::RenderedProfile {
        use tv4::report::executability::LegMarket;
        use tv4::report::metrics_s9::{DatasetInfo, InstrumentsInfo, MetricsS9Inputs, SeriesInfo};
        use tv4::report::research::{Guard, ResearchInputs, render_research_run};
        render_research_run(
            result,
            MetricsS9Inputs {
                dataset: DatasetInfo {
                    name: "controls".into(),
                    exchange: tv4::model::ExchangeId::Okx,
                    tf: Timeframe::M1,
                    gaps_policy: "skip".into(),
                    hash_format: "dsh-v1".into(),
                    dataset_hash: "blake3:test".into(),
                    series: data
                        .series
                        .iter()
                        .map(|(s, c)| SeriesInfo {
                            symbol: s.clone(),
                            rows: c.len() as u64,
                            first_ms: c.open_time_ms()[0],
                            last_ms: *c.open_time_ms().last().unwrap(),
                        })
                        .collect(),
                },
                instruments: InstrumentsInfo::toml(),
                okx_pairs: None,
                markets: data
                    .series
                    .iter()
                    .map(|(_, c)| LegMarket {
                        open_time_ms: c.open_time_ms(),
                        low: c.low(),
                        close: c.close(),
                        volume: c.volume(),
                    })
                    .collect(),
            },
            &ResearchInputs {
                guard: Guard::None,
                warmup_start_ms: data.series[0].1.open_time_ms()[0],
                universe: None,
                quality: &self.quality,
                subset: None,
            },
            Timeframe::M1,
        )
        .unwrap()
    }
}
