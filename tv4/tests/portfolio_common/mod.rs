//! Aides communes aux tests du moteur portefeuille (S9, lot E). Marché inventé : aucune donnée réelle.
//!
//! Inclus par `mod portfolio_common;` dans chaque binaire de test ; chacun n'en utilise qu'une partie.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use tv4::engine::{
    DEFAULT_MAX_POSITIONS, DEFAULT_QUOTE_GAP_MS, PortfolioConfig, PortfolioInputs, PortfolioResult,
    run_portfolio,
};
use tv4::model::{Candle, ExchangeId, FeeSchedule, InstrumentRules, Slippage, Timeframe, Ts};
use tv4::sim::{SimRules, SizingMode};
use tv4::store::Columns;
use tv4::strategy::{BarContext, IndicatorSpec, Signal, Size, Strategy};

/// 2024-01-01T00:00:00Z.
pub const T0: Ts = 1_704_067_200_000;
pub const MINUTE: Ts = 60_000;
pub const DAY: Ts = 86_400_000;

pub fn fees() -> FeeSchedule {
    FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap()
}

/// Règles par défaut du simulateur, volume non limitant : seul le portefeuille est en jeu.
pub fn rules() -> SimRules {
    SimRules {
        max_volume_share: 1.0e9,
        ..SimRules::default()
    }
}

pub fn instrument(min_qty: f64) -> InstrumentRules {
    InstrumentRules::new(0.1, 1e-8, min_qty, 0.0).unwrap()
}

/// Bougie 1m plate à `price`.
pub fn flat_bar(open_time_ms: Ts, price: f64, volume: f64) -> Candle {
    Candle {
        open_time_ms,
        open: price,
        high: price,
        low: price,
        close: price,
        volume,
        trades: None,
    }
}

/// `count` 1m consécutives à partir de la minute `first` (depuis `T0`), prix plat.
pub fn flat_series(first: i64, count: i64, price: f64, volume: f64) -> Vec<Candle> {
    (first..first + count)
        .map(|m| flat_bar(T0 + m * MINUTE, price, volume))
        .collect()
}

pub fn columns(candles: &[Candle]) -> Columns {
    Columns::from_candles(candles)
}

/// Configuration de test : frais OKX, slippage nul, `sim_rules` sans limite de volume, recherche, N = 8,
/// trou de 7 jours.
pub fn config<'a>(
    universe: &'a [String],
    instruments: &'a [InstrumentRules],
    initial_cash: f64,
) -> PortfolioConfig<'a> {
    PortfolioConfig {
        universe,
        regime_symbol: "BTC-USD",
        fees: fees(),
        slippage: Slippage::ZERO,
        initial_cash,
        sim_rules: rules(),
        instruments,
        instruments_hash: None,
        max_positions: DEFAULT_MAX_POSITIONS,
        sizing: SizingMode::Research,
        min_order_bump_max_risk: None,
        quote_gap_ms: DEFAULT_QUOTE_GAP_MS,
        min_trades: 30,
    }
}

pub fn names(symbols: &[&str]) -> Vec<String> {
    symbols.iter().map(|s| (*s).to_string()).collect()
}

/// Ce que la stratégie a vu, partagé avec le test.
#[derive(Default, Debug)]
pub struct Spy {
    /// `(now_ms, open_time de la dernière 1d du régime, bull)` à chaque appel qui lit le régime.
    pub regime_calls: Vec<(Ts, Option<Ts>, Option<bool>)>,
    /// `(now_ms, equity, quote_free)` à chaque appel.
    pub states: Vec<(Ts, f64, f64)>,
    /// `now_ms` de chaque appel.
    pub calls: Vec<Ts>,
}

/// Stratégie de test M1 : émet chaque signal du plan une fois, au premier appel dont `now_ms` est au
/// moins celui du plan (donc exactement à `now_ms` si la jambe a cette minute, plus tard sinon). Avec
/// un `RegimeEntry`, elle lit le régime : à chaque `now_ms` multiple de `every_ms`, elle entre si elle
/// est à plat et que `bull() == Some(true)`, et sort si elle est en position et que `bull()` n'est pas
/// `Some(true)`.
pub struct Scripted {
    plan: Vec<(Ts, Signal)>,
    fired: Vec<bool>,
    spy: Arc<Mutex<Spy>>,
    regime_entry: Option<RegimeEntry>,
}

/// Entrée conditionnée au régime.
pub struct RegimeEntry {
    pub every_ms: Ts,
    pub signal: Signal,
}

impl Scripted {
    pub fn new(plan: Vec<(Ts, Signal)>) -> (Scripted, Arc<Mutex<Spy>>) {
        let spy = Arc::new(Mutex::new(Spy::default()));
        let fired = vec![false; plan.len()];
        (
            Scripted {
                plan,
                fired,
                spy: Arc::clone(&spy),
                regime_entry: None,
            },
            spy,
        )
    }

    pub fn with_regime(entry: RegimeEntry) -> (Scripted, Arc<Mutex<Spy>>) {
        let spy = Arc::new(Mutex::new(Spy::default()));
        (
            Scripted {
                plan: Vec::new(),
                fired: Vec::new(),
                spy: Arc::clone(&spy),
                regime_entry: Some(entry),
            },
            spy,
        )
    }
}

impl Strategy for Scripted {
    fn id(&self) -> &'static str {
        "scripted"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M1]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn uses_regime(&self) -> bool {
        self.regime_entry.is_some()
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        let now = ctx.now_ms();
        {
            let mut spy = self.spy.lock().unwrap();
            spy.calls.push(now);
            spy.states.push((now, ctx.equity(), ctx.quote_free()));
        }
        for ((at, signal), fired) in self.plan.iter().zip(self.fired.iter_mut()) {
            if !*fired && now >= *at {
                *fired = true;
                out.push(*signal);
            }
        }
        if let Some(entry) = &self.regime_entry {
            let regime = ctx.regime();
            let bull = regime.and_then(|view| view.bull());
            let last = regime.and_then(|view| view.closed().last().map(|c| c.open_time_ms));
            self.spy
                .lock()
                .unwrap()
                .regime_calls
                .push((now, last, bull));
            if now.rem_euclid(entry.every_ms) == 0 {
                if ctx.position().is_none() && bull == Some(true) {
                    out.push(entry.signal);
                } else if ctx.position().is_some() && bull != Some(true) {
                    out.push(sell_all());
                }
            }
        }
    }
}

/// Signal d'achat market au risque de `risk` de la base, stop à `stop`.
pub fn buy_risk(risk: f64, stop: f64) -> Signal {
    Signal::EnterLong {
        size: Size::RiskPct(risk),
        kind: tv4::strategy::OrderKind::Market,
        stop: Some(stop),
        take_profit: None,
        tag: "buy",
    }
}

/// Signal d'achat market pour `quote` de monnaie de cotation.
pub fn buy_quote(quote: f64) -> Signal {
    Signal::EnterLong {
        size: Size::Quote(quote),
        kind: tv4::strategy::OrderKind::Market,
        stop: None,
        take_profit: None,
        tag: "buy",
    }
}

pub fn sell_all() -> Signal {
    Signal::ExitLong {
        kind: tv4::strategy::OrderKind::Market,
        tag: "sell",
    }
}

/// Lance un portefeuille sur des bougies par symbole.
pub fn run(
    series: &[(&str, Vec<Candle>)],
    strategies: Vec<Box<dyn Strategy>>,
    config: PortfolioConfig<'_>,
) -> PortfolioResult {
    let data: Vec<(String, Columns)> = series
        .iter()
        .map(|(symbol, candles)| ((*symbol).to_string(), columns(candles)))
        .collect();
    run_portfolio(PortfolioInputs {
        series: &data,
        strategies,
        config,
    })
    .expect("run valide")
}

/// Comme [`run`], en rendant l'erreur.
pub fn try_run(
    series: &[(&str, Vec<Candle>)],
    strategies: Vec<Box<dyn Strategy>>,
    config: PortfolioConfig<'_>,
) -> Result<PortfolioResult, tv4::engine::EngineError> {
    let data: Vec<(String, Columns)> = series
        .iter()
        .map(|(symbol, candles)| ((*symbol).to_string(), columns(candles)))
        .collect();
    run_portfolio(PortfolioInputs {
        series: &data,
        strategies,
        config,
    })
}
