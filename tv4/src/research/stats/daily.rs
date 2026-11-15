//! Grille UTC exacte : seuls les intervalles complets dans la fenêtre sont attendus.
use super::types::{MarkPoint, PricePoint, StatsError, TradeRecord, Window, finite, sorted_trades};
use crate::store::time::{MAX_TS_MS, MS_PER_DAY};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub(crate) const YEAR_MS: f64 = 365.25 * MS_PER_DAY as f64;

/// Historique de clôtures disponibles au minuit de fin, distinct des ancres de valorisation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarketSeries {
    pub symbol: String,
    pub daily_closes: Vec<PricePoint>,
    pub valuations: Vec<PricePoint>,
}
/// Rendement simple sur [start_ms,end_ms], exactement un jour UTC complet.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatedReturn {
    pub start_ms: i64,
    pub end_ms: i64,
    pub value: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DailyReturns {
    pub intervals: Vec<DatedReturn>,
    pub expected_intervals: usize,
    pub missing_intervals: Vec<Window>,
    /// Fraction des intervalles UTC complets identifiables ; null si aucun attendu.
    pub coverage: Option<f64>,
}
pub(crate) fn invalid(reason: &str) -> StatsError {
    StatsError::InvalidInput(reason.into())
}
pub(crate) fn timestamp(at: i64) -> Result<(), StatsError> {
    if !(0..MAX_TS_MS).contains(&at) {
        return Err(invalid("horodatage hors domaine calendaire [1970,10000["));
    }
    Ok(())
}
pub(crate) fn validate_window(w: Window) -> Result<(), StatsError> {
    timestamp(w.start_ms)?;
    timestamp(w.end_ms)?;
    if w.end_ms <= w.start_ms {
        return Err(invalid("fenêtre de durée strictement positive requise"));
    }
    Ok(())
}
pub(crate) fn validate_trades(trades: &[TradeRecord]) -> Result<(), StatsError> {
    sorted_trades(trades)?;
    let mut rows = BTreeSet::new();
    for t in trades {
        timestamp(t.entry_ms)?;
        timestamp(t.exit_ms)?;
        if t.exit_ms < t.entry_ms || !rows.insert(t.row) || t.symbol.is_empty() {
            return Err(invalid(
                "horloge, symbole ou identité row du journal invalide",
            ));
        }
    }
    Ok(())
}
pub(crate) fn validate_prices(prices: &[PricePoint], daily: bool) -> Result<(), StatsError> {
    let mut previous = None;
    for p in prices {
        timestamp(p.at_ms)?;
        if !p.close.is_finite()
            || p.close <= 0.
            || previous.is_some_and(|t| t >= p.at_ms)
            || (daily && p.at_ms % MS_PER_DAY != 0)
        {
            return Err(invalid(
                "prix positif fini, dates uniques croissantes et minuits requis",
            ));
        }
        previous = Some(p.at_ms);
    }
    Ok(())
}
pub(crate) fn validate_markets(markets: &[MarketSeries]) -> Result<(), StatsError> {
    let mut symbols = BTreeSet::new();
    for m in markets {
        if m.symbol.is_empty() || !symbols.insert(&m.symbol) {
            return Err(invalid("symboles de marché vides ou dupliqués"));
        }
        validate_prices(&m.daily_closes, true)?;
        validate_prices(&m.valuations, false)?;
    }
    Ok(())
}
pub(crate) fn grid(w: Window) -> impl Iterator<Item = Window> {
    let first = w.start_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY
        + if w.start_ms % MS_PER_DAY == 0 {
            0
        } else {
            MS_PER_DAY
        };
    let last = w.end_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    (first..last)
        .step_by(MS_PER_DAY as usize)
        .map(|start_ms| Window {
            start_ms,
            end_ms: start_ms + MS_PER_DAY,
        })
}
pub(crate) fn point(prices: &[PricePoint], at: i64) -> Option<f64> {
    prices
        .binary_search_by_key(&at, |p| p.at_ms)
        .ok()
        .map(|i| prices[i].close)
}
pub(crate) fn returns_from_prices(
    prices: &[PricePoint],
    w: Window,
) -> Result<DailyReturns, StatsError> {
    let mut intervals = Vec::new();
    let mut missing_intervals = Vec::new();
    for interval in grid(w) {
        if let (Some(a), Some(b)) = (
            point(prices, interval.start_ms),
            point(prices, interval.end_ms),
        ) && a > 0.
        {
            intervals.push(DatedReturn {
                start_ms: interval.start_ms,
                end_ms: interval.end_ms,
                value: finite(b / a - 1.)?,
            });
        } else {
            missing_intervals.push(interval);
        }
    }
    let expected_intervals = intervals.len() + missing_intervals.len();
    Ok(DailyReturns {
        coverage: (expected_intervals > 0)
            .then(|| intervals.len() as f64 / expected_intervals as f64),
        intervals,
        expected_intervals,
        missing_intervals,
    })
}
pub(crate) fn mark_prices(marks: &[MarkPoint], w: Window) -> Result<Vec<PricePoint>, StatsError> {
    validate_window(w)?;
    let mut previous = None;
    let mut prices = Vec::new();
    for p in marks {
        timestamp(p.at_ms)?;
        if !p.equity.is_finite()
            || p.equity < 0.
            || previous.is_some_and(|t| t >= p.at_ms)
            || (p.day_close && p.at_ms % MS_PER_DAY != 0)
            || p.at_ms < w.start_ms
            || p.at_ms > w.end_ms
        {
            return Err(invalid(
                "marquages finis positifs ou nuls, uniques, croissants dans la fenêtre requis",
            ));
        }
        previous = Some(p.at_ms);
        // Une origine/fin à minuit est exacte même sans indicateur day_close.
        if p.day_close || p.at_ms == w.start_ms || p.at_ms == w.end_ms {
            prices.push(PricePoint {
                at_ms: p.at_ms,
                close: p.equity,
            });
        }
    }
    Ok(prices)
}
pub fn daily_returns(marks: &[MarkPoint], window: Window) -> Result<DailyReturns, StatsError> {
    returns_from_prices(&mark_prices(marks, window)?, window)
}
pub(crate) fn mean(values: &[f64]) -> Result<f64, StatsError> {
    let mut result = 0.;
    for (i, &v) in values.iter().enumerate() {
        result = finite(result + finite(v - result)? / (i + 1) as f64)?;
    }
    Ok(result)
}
