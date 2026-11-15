//! Labels causaux et rendement des seuls intervalles sélectionnés à leur borne gauche.
mod history;
use super::approx::annualized;
use super::concentration::TradeIdentity;
use super::daily::{
    DailyReturns, MarketSeries, YEAR_MS, daily_returns, mean, validate_markets, validate_trades,
};
use super::types::{MarkPoint, Metric, StatsError, TradeRecord, Window, finite};
use history::History;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trend {
    Bull,
    Bear,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Volatility {
    Low,
    High,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegimeLabel {
    pub trade: TradeIdentity,
    pub close_ms: Option<i64>,
    pub trend: Option<Trend>,
    pub volatility: Option<Volatility>,
    pub volatility30: Metric,
    pub expanding_median: Metric,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegimeCell {
    pub trend: Trend,
    pub volatility: Volatility,
    pub n: usize,
    pub n_r: usize,
    pub mu_r: Metric,
    /// Mesure descriptive du seuil protocolaire, jamais une porte G2.
    pub below_expectancy_threshold: Option<bool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BearQualification {
    Flat,
    Positive,
    Negative,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BearStats {
    pub n_entries: usize,
    pub mu_r: Metric,
    pub n_intervals: usize,
    pub intervals: Vec<Window>,
    pub duration_years: f64,
    pub missing_regime_intervals: Vec<Window>,
    pub missing_bear_intervals: Vec<Window>,
    pub annualized_return: Metric,
    pub qualification: Option<BearQualification>,
    pub qualification_reasons: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegimeStats {
    pub labels: Vec<RegimeLabel>,
    /// Ordre stable bull-low, bull-high, bear-low, bear-high.
    pub cells: Vec<RegimeCell>,
    pub missing_labels: usize,
    pub missing_r: usize,
    pub volatility_origin_ms: Option<i64>,
    pub daily: DailyReturns,
    pub bear: BearStats,
}
fn expectancy(values: &[Option<f64>]) -> Result<Metric, StatsError> {
    if values.is_empty() || values.iter().any(Option::is_none) {
        return Ok(Metric::unavailable(
            "effectif nul ou R manquant : moyenne non identifiable",
        ));
    }
    Metric::available(mean(&values.iter().flatten().copied().collect::<Vec<_>>())?)
}

pub fn regimes(
    trades: &[TradeRecord],
    btc: Option<&MarketSeries>,
    marks: &[MarkPoint],
    window: Window,
) -> Result<RegimeStats, StatsError> {
    validate_trades(trades)?;
    if let Some(m) = btc {
        validate_markets(std::slice::from_ref(m))?;
    }
    let daily = daily_returns(marks, window)?;
    let history = History::new(btc)?;
    let mut ordered: Vec<_> = trades.iter().collect();
    ordered.sort_by_key(|t| t.row);
    let mut labels = Vec::new();
    let mut cell_values: [Vec<Option<f64>>; 4] = std::array::from_fn(|_| Vec::new());
    let mut bear_values = Vec::new();
    let mut missing_labels = 0;
    let mut missing_r = 0;
    for t in ordered {
        let state = history.at(t.entry_ms);
        if t.included {
            missing_r += usize::from(t.r_multiple.is_none());
            if let (Some(trend), Some(vol)) = (state.trend, state.volatility) {
                let i =
                    usize::from(trend == Trend::Bear) * 2 + usize::from(vol == Volatility::High);
                cell_values[i].push(t.r_multiple);
            } else {
                missing_labels += 1;
            }
            if state.trend == Some(Trend::Bear) {
                bear_values.push(t.r_multiple);
            }
        }
        labels.push(RegimeLabel {
            trade: TradeIdentity::from(t),
            close_ms: state.close_ms,
            trend: state.trend,
            volatility: state.volatility,
            volatility30: state.volatility30,
            expanding_median: state.median,
            reasons: state.reasons,
        });
    }
    let mut cells = Vec::new();
    for (i, values) in cell_values.iter().enumerate() {
        let mu_r = expectancy(values)?;
        cells.push(RegimeCell {
            trend: if i < 2 { Trend::Bull } else { Trend::Bear },
            volatility: if i % 2 == 0 {
                Volatility::Low
            } else {
                Volatility::High
            },
            n: values.len(),
            n_r: values.iter().flatten().count(),
            below_expectancy_threshold: mu_r.value.map(|v| values.len() >= 30 && v < -0.5),
            mu_r,
        });
    }
    let prices = super::daily::mark_prices(marks, window)?;
    let mut missing_regime_intervals = Vec::new();
    let mut missing_bear_intervals = Vec::new();
    for interval in super::daily::grid(window) {
        match history.at(interval.start_ms).trend {
            None => missing_regime_intervals.push(interval),
            Some(Trend::Bear)
                if daily
                    .intervals
                    .binary_search_by_key(&interval.start_ms, |d| d.start_ms)
                    .is_err() =>
            {
                missing_bear_intervals.push(interval)
            }
            _ => {}
        }
    }
    let mut growth = 1.;
    let mut intervals = Vec::new();
    for interval in &daily.intervals {
        if history.at(interval.start_ms).trend == Some(Trend::Bear) {
            let start =
                super::daily::point(&prices, interval.start_ms).expect("intervalle observé");
            let end = super::daily::point(&prices, interval.end_ms).expect("intervalle observé");
            growth = finite(growth * finite(end / start)?)?;
            intervals.push(Window {
                start_ms: interval.start_ms,
                end_ms: interval.end_ms,
            });
        }
    }
    let duration_years = intervals.len() as f64 * crate::store::time::MS_PER_DAY as f64 / YEAR_MS;
    let ann = if !missing_regime_intervals.is_empty() || !missing_bear_intervals.is_empty() {
        Metric::unavailable(
            "périmètre bear global inconnu : régime gauche ou marquage bear manquant (dates publiées)",
        )
    } else if intervals.is_empty() {
        Metric::unavailable("aucun intervalle bear quotidien identifiable")
    } else {
        Metric::available(annualized(growth, duration_years)?)?
    };
    let mu_r = expectancy(&bear_values)?;
    let qualification = ann.value.and_then(|r| {
        if r.abs() <= 0.05 {
            Some(BearQualification::Flat)
        } else {
            mu_r.value.map(|m| {
                if m >= 0. {
                    BearQualification::Positive
                } else {
                    BearQualification::Negative
                }
            })
        }
    });
    let qualification_reasons = if qualification.is_none() {
        vec!["rendement bear ou expectancy bear insuffisamment observé".into()]
    } else {
        vec![]
    };
    Ok(RegimeStats {
        labels,
        cells,
        missing_labels,
        missing_r,
        volatility_origin_ms: history.origin,
        daily,
        bear: BearStats {
            n_entries: bear_values.len(),
            mu_r,
            n_intervals: intervals.len(),
            intervals,
            duration_years,
            missing_regime_intervals,
            missing_bear_intervals,
            annualized_return: ann,
            qualification,
            qualification_reasons,
        },
    })
}
