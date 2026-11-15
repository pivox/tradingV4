//! Références de prix et cash descriptives, sans I/O ni allocation implicite.
mod calendar;
mod monthly;
mod reference;
use super::daily::{
    MarketSeries, YEAR_MS, invalid, mark_prices, point, validate_markets, validate_trades,
    validate_window,
};
use super::types::{
    CashPoint, MarkPoint, Metric, PricePoint, StatsError, StatsOptions, TradeRecord, Window, finite,
};
pub use monthly::{MonthCoverage, MonthlyComparison};
pub use reference::BenchmarkReference;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkSymbols {
    pub btc: String,
    pub eth: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchPartition {
    Dev,
    Validation,
    Holdout,
    Forward,
    Unclassified,
}
#[derive(Clone, Copy)]
pub struct BenchmarkInput<'a> {
    pub trades: &'a [TradeRecord],
    /// Univers effectivement joué, distinct des marchés de contexte BTC/ETH.
    pub pair_symbols: &'a [String],
    pub marks: &'a [MarkPoint],
    pub cash: &'a [CashPoint],
    pub markets: &'a [MarketSeries],
    pub capital: f64,
    pub window: Window,
    pub symbols: &'a BenchmarkSymbols,
    pub partition: ResearchPartition,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairBenchmark {
    pub symbol: String,
    /// Union des positions détenues, y compris purgées, divisée par la durée de fenêtre.
    pub exposure_fraction: f64,
    pub benchmark: BenchmarkReference,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkStats {
    pub cash_convention: String,
    pub pair_aggregate_convention: String,
    pub cash_reference_quote: Metric,
    pub cash_reference_return: Metric,
    pub strategy_total_return: Metric,
    pub strategy_daily: super::daily::DailyReturns,
    pub strategy_calmar: Metric,
    pub btc: BenchmarkReference,
    pub basket_50_50: BenchmarkReference,
    pub pairs: Vec<PairBenchmark>,
    pub equal_weight_pairs: BenchmarkReference,
}
fn cash_integral(input: BenchmarkInput<'_>) -> Result<Metric, StatsError> {
    let mut previous = None;
    for p in input.cash {
        if !p.cash.is_finite()
            || p.cash < 0.
            || p.at_ms < input.window.start_ms
            || p.at_ms > input.window.end_ms
            || previous.is_some_and(|t| t >= p.at_ms)
        {
            return Err(invalid(
                "cash non négatif fini, dates uniques croissantes dans la fenêtre requis",
            ));
        }
        previous = Some(p.at_ms);
    }
    if input
        .cash
        .first()
        .is_none_or(|p| p.at_ms != input.window.start_ms)
    {
        return Ok(Metric::unavailable(
            "solde de cash à l'origine absent : couverture intégrale inconnue",
        ));
    }
    let mut integral = 0.;
    for (i, p) in input.cash.iter().enumerate() {
        let end = input
            .cash
            .get(i + 1)
            .map_or(input.window.end_ms, |p| p.at_ms);
        integral = finite(integral + p.cash * 0.017 * ((end - p.at_ms) as f64 / YEAR_MS))?;
    }
    Metric::available(integral)
}
fn exposure(trades: &[TradeRecord], symbol: &str, w: Window) -> f64 {
    let mut intervals: Vec<_> = trades
        .iter()
        .filter(|t| t.symbol == symbol)
        .map(|t| (t.entry_ms.max(w.start_ms), t.exit_ms.min(w.end_ms)))
        .filter(|(a, b)| b > a)
        .collect();
    intervals.sort_unstable();
    let mut duration = 0_i64;
    let mut active: Option<(i64, i64)> = None;
    for (a, b) in intervals {
        if let Some((left, right)) = active {
            if a <= right {
                active = Some((left, right.max(b)));
            } else {
                duration += right - left;
                active = Some((a, b));
            }
        } else {
            active = Some((a, b));
        }
    }
    if let Some((a, b)) = active {
        duration += b - a;
    }
    duration as f64 / (w.end_ms - w.start_ms) as f64
}
fn dates(w: Window) -> Vec<i64> {
    let mut dates = vec![w.start_ms];
    for d in super::daily::grid(w) {
        if dates.last() != Some(&d.start_ms) {
            dates.push(d.start_ms);
        }
        dates.push(d.end_ms);
    }
    if dates.last() != Some(&w.end_ms) {
        dates.push(w.end_ms);
    }
    dates
}
fn passive(at: i64, w: Window) -> Result<f64, StatsError> {
    super::approx::powf(1.017, (at - w.start_ms) as f64 / YEAR_MS)
}
fn mixture(
    market: Option<&MarketSeries>,
    fraction: f64,
    w: Window,
) -> Result<Vec<PricePoint>, StatsError> {
    let p0 = market.and_then(|m| point(&m.valuations, w.start_ms));
    let mut result = Vec::new();
    for at_ms in dates(w) {
        let price = if fraction == 0. {
            Some(1.)
        } else {
            market
                .and_then(|m| point(&m.valuations, at_ms))
                .zip(p0)
                .map(|(p, p0)| p / p0)
        };
        if let Some(ratio) = price {
            result.push(PricePoint {
                at_ms,
                close: finite(fraction * ratio + (1. - fraction) * passive(at_ms, w)?)?,
            });
        }
    }
    Ok(result)
}
pub fn benchmarks(
    input: BenchmarkInput<'_>,
    options: &StatsOptions,
) -> Result<BenchmarkStats, StatsError> {
    options.validate()?;
    validate_window(input.window)?;
    validate_trades(input.trades)?;
    validate_markets(input.markets)?;
    if !input.capital.is_finite()
        || input.capital <= 0.
        || input.symbols.btc.is_empty()
        || input.symbols.eth.is_empty()
        || input.symbols.btc == input.symbols.eth
    {
        return Err(invalid(
            "capital positif fini et symboles BTC/ETH explicites distincts requis",
        ));
    }
    for m in input.markets {
        for p in &m.valuations {
            if p.at_ms < input.window.start_ms
                || p.at_ms > input.window.end_ms
                || (p.at_ms != input.window.start_ms
                    && p.at_ms != input.window.end_ms
                    && p.at_ms % crate::store::time::MS_PER_DAY != 0)
            {
                return Err(invalid(
                    "valorisation hors fenêtre ou hors ancre/minuit UTC",
                ));
            }
        }
    }
    let strategy = mark_prices(input.marks, input.window)?;
    if point(&strategy, input.window.start_ms).is_some_and(|equity| equity != input.capital) {
        return Err(invalid("marquage initial différent du capital"));
    }
    let find = |symbol: &str| input.markets.iter().find(|m| m.symbol == symbol);
    let btc_trajectory = mixture(find(&input.symbols.btc), 1., input.window)?;
    let eth_trajectory = mixture(find(&input.symbols.eth), 1., input.window)?;
    let mut basket = Vec::new();
    for p in &btc_trajectory {
        if let Some(eth) = point(&eth_trajectory, p.at_ms) {
            basket.push(PricePoint {
                at_ms: p.at_ms,
                close: finite(p.close * 0.5 + eth * 0.5)?,
            });
        }
    }
    let calculate =
        |id: &str, p: Vec<PricePoint>| reference::calculate(id, p, &strategy, input, options);
    let btc = calculate("btc_buy_and_hold", btc_trajectory)?;
    let basket_50_50 = calculate("initial_50_50_btc_eth", basket)?;
    let mut symbols = BTreeSet::new();
    for symbol in input.pair_symbols {
        if symbol.is_empty() || !symbols.insert(symbol.as_str()) {
            return Err(invalid("univers joué : symboles non vides uniques requis"));
        }
    }
    if input
        .trades
        .iter()
        .any(|t| !symbols.contains(t.symbol.as_str()))
    {
        return Err(invalid("trade hors univers joué explicite"));
    }
    let mut pairs = Vec::new();
    for symbol in symbols {
        let f = exposure(input.trades, symbol, input.window);
        pairs.push(PairBenchmark {
            symbol: symbol.into(),
            exposure_fraction: f,
            benchmark: calculate(
                &format!("exposure:{symbol}"),
                mixture(find(symbol), f, input.window)?,
            )?,
        });
    }
    let mut aggregate = Vec::new();
    if !pairs.is_empty() {
        for at_ms in dates(input.window) {
            let values: Option<Vec<_>> = pairs
                .iter()
                .map(|p| point(&p.benchmark.trajectory, at_ms))
                .collect();
            if let Some(values) = values {
                aggregate.push(PricePoint {
                    at_ms,
                    close: super::daily::mean(&values)?,
                });
            }
        }
    }
    let cash_reference_quote = cash_integral(input)?;
    let cash_reference_return = match cash_reference_quote.value {
        Some(v) => Metric::available(v / input.capital)?,
        None => Metric::unavailable("intégrale de cash non identifiable"),
    };
    Ok(BenchmarkStats {
        cash_convention: "protocol_v1_rate_0.017_simple_prorata_left_after_events".into(),
        pair_aggregate_convention:
            "descriptive_equal_weight_pair_trajectories_no_shared_allocation_claim".into(),
        cash_reference_quote,
        cash_reference_return,
        strategy_total_return: reference::total(&strategy, input.window, input.capital)?,
        strategy_daily: super::daily::returns_from_prices(&strategy, input.window)?,
        strategy_calmar: reference::calmar(&strategy, input.window)?,
        btc,
        basket_50_50,
        equal_weight_pairs: calculate("descriptive_equal_weight_pairs", aggregate)?,
        pairs,
    })
}
