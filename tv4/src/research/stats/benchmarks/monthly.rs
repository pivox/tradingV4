use super::{BenchmarkInput, ResearchPartition, calendar};
use crate::model::rng::{SplitMix64, leg_key};
use crate::research::stats::approx::quantile_type7;
use crate::research::stats::daily::{point, returns_from_prices};
use crate::research::stats::random::{STREAM_MONTHLY_BOOTSTRAP, uniform_index};
use crate::research::stats::types::{Metric, PricePoint, StatsError, StatsOptions, Window, finite};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonthCoverage {
    pub window: Window,
    pub expected_intervals: usize,
    pub observed_intervals: usize,
    pub coverage: f64,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonthlyComparison {
    pub method: String,
    pub used_months: Vec<Window>,
    pub excluded_months: Vec<MonthCoverage>,
    pub point_excess: Metric,
    pub lb95_excess: Metric,
}
fn difference(
    factors: &[(f64, f64)],
    indices: impl Iterator<Item = usize>,
) -> Result<f64, StatsError> {
    let (mut strategy, mut benchmark) = (1., 1.);
    for i in indices {
        strategy = finite(strategy * factors[i].0)?;
        benchmark = finite(benchmark * factors[i].1)?;
    }
    finite(strategy - benchmark)
}
pub(super) fn compare(
    id: &str,
    s: &[PricePoint],
    b: &[PricePoint],
    input: BenchmarkInput<'_>,
    options: &StatsOptions,
) -> Result<MonthlyComparison, StatsError> {
    let sd = returns_from_prices(s, input.window)?;
    let bd = returns_from_prices(b, input.window)?;
    let mut used_months = Vec::new();
    let mut excluded_months = Vec::new();
    let mut factors = Vec::new();
    for w in calendar::months(input.window) {
        let expected = ((w.end_ms - w.start_ms) / crate::store::time::MS_PER_DAY) as usize;
        let observed = sd
            .intervals
            .iter()
            .filter(|d| {
                d.start_ms >= w.start_ms
                    && d.end_ms <= w.end_ms
                    && bd
                        .intervals
                        .binary_search_by_key(&d.start_ms, |b| b.start_ms)
                        .is_ok()
            })
            .count();
        let anchors = point(s, w.start_ms)
            .zip(point(s, w.end_ms))
            .zip(point(b, w.start_ms).zip(point(b, w.end_ms)));
        if w.start_ms >= input.window.start_ms
            && w.end_ms <= input.window.end_ms
            && observed == expected
            && let Some(((s0, s1), (b0, b1))) = anchors
            && s0 > 0.
            && b0 > 0.
        {
            used_months.push(w);
            factors.push((finite(s1 / s0)?, finite(b1 / b0)?));
        } else {
            excluded_months.push(MonthCoverage {
                window: w,
                expected_intervals: expected,
                observed_intervals: observed,
                coverage: observed as f64 / expected as f64,
                reasons: vec![
                    "mois UTC partiel ou observations quotidiennes/ancres manquantes".into(),
                ],
            });
        }
    }
    let point_excess = if factors.is_empty() {
        Metric::unavailable("aucun mois UTC complet commun entièrement observé")
    } else {
        Metric::available(difference(&factors, 0..factors.len())?)?
    };
    let lb95_excess = if input.partition != ResearchPartition::Dev {
        Metric::unavailable("bootstrap mensuel réservé à DEV ; point seulement sur bloc protégé")
    } else if factors.len() < 2 {
        Metric::unavailable("au moins deux mois UTC complets requis")
    } else {
        let mut rng = SplitMix64::new(options.seed ^ STREAM_MONTHLY_BOOTSTRAP ^ leg_key(id));
        let mut draws = Vec::with_capacity(options.samples as usize);
        for _ in 0..options.samples {
            draws.push(difference(
                &factors,
                (0..factors.len()).map(|_| uniform_index(&mut rng, factors.len() as u64)),
            )?);
        }
        Metric::available(quantile_type7(&draws, 0.05)?.expect("B>=2"))?
    };
    Ok(MonthlyComparison {
        method: "utc_complete_month_srs_with_replacement_paired_v1".into(),
        used_months,
        excluded_months,
        point_excess,
        lb95_excess,
    })
}
