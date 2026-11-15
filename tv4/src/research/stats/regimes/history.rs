use super::{Trend, Volatility};
use crate::research::stats::approx::{ln, quantile_type7, sample_standard_deviation};
use crate::research::stats::daily::{MarketSeries, mean};
use crate::research::stats::types::{Metric, StatsError};
use crate::store::time::MS_PER_DAY;

#[derive(Clone)]
pub(super) struct State {
    pub close_ms: Option<i64>,
    pub trend: Option<Trend>,
    pub volatility: Option<Volatility>,
    pub volatility30: Metric,
    pub median: Metric,
    pub reasons: Vec<String>,
}
pub(super) struct History {
    states: Vec<(i64, State)>,
    pub origin: Option<i64>,
}
impl State {
    fn missing(reason: &str) -> Self {
        Self {
            close_ms: None,
            trend: None,
            volatility: None,
            volatility30: Metric::unavailable(reason),
            median: Metric::unavailable(reason),
            reasons: vec![reason.into()],
        }
    }
}
impl History {
    pub fn new(market: Option<&MarketSeries>) -> Result<Self, StatsError> {
        let Some(market) = market else {
            return Ok(Self {
                states: vec![],
                origin: None,
            });
        };
        let p = &market.daily_closes;
        let mut states = Vec::new();
        let mut run = 0;
        let mut valid_vols = Vec::new();
        let mut origin = None;
        for (i, close) in p.iter().enumerate() {
            run = if i > 0 && close.at_ms - p[i - 1].at_ms == MS_PER_DAY {
                run + 1
            } else {
                1
            };
            let mut s = State::missing("historique consécutif insuffisant (SMA200 / volatilité30)");
            s.close_ms = Some(close.at_ms);
            if run >= 200 {
                let sma = mean(
                    &p[i + 1 - 200..=i]
                        .iter()
                        .map(|p| p.close)
                        .collect::<Vec<_>>(),
                )?;
                s.trend = Some(if close.close > sma {
                    Trend::Bull
                } else {
                    Trend::Bear
                });
            }
            if run >= 31 {
                let returns = p[i - 30..=i]
                    .windows(2)
                    .map(|v| Ok(ln(v[1].close)? - ln(v[0].close)?))
                    .collect::<Result<Vec<_>, StatsError>>()?;
                let vol = sample_standard_deviation(&returns)? * 365.25_f64.sqrt();
                valid_vols.push(vol);
                let median =
                    quantile_type7(&valid_vols, 0.5)?.expect("volatilité courante incluse");
                origin.get_or_insert(close.at_ms);
                s.volatility30 = Metric::available(vol)?;
                s.median = Metric::available(median)?;
                s.volatility = Some(if vol > median {
                    Volatility::High
                } else {
                    Volatility::Low
                });
            }
            if s.trend.is_some() && s.volatility.is_some() {
                s.reasons.clear();
            }
            states.push((close.at_ms, s));
        }
        Ok(Self { states, origin })
    }
    pub fn at(&self, at_ms: i64) -> State {
        let expected = at_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY;
        self.states
            .binary_search_by_key(&expected, |(at, _)| *at)
            .ok()
            .map(|i| self.states[i].1.clone())
            .unwrap_or_else(|| State::missing("clôture BTC attendue au dernier minuit UTC absente"))
    }
}
