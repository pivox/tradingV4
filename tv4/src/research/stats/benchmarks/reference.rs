use super::{BenchmarkInput, calendar, monthly};
use crate::research::stats::approx::{annualized, sample_standard_deviation};
use crate::research::stats::daily::{DailyReturns, YEAR_MS, mean, point, returns_from_prices};
use crate::research::stats::types::{Metric, PricePoint, StatsError, StatsOptions, Window, finite};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkReference {
    pub id: String,
    /// Capital normalisé à 1 à l'origine ; points manquants conservés absents.
    pub trajectory: Vec<PricePoint>,
    pub total_return: Metric,
    pub daily: DailyReturns,
    pub excess_sharpe: Metric,
    pub monthly: monthly::MonthlyComparison,
    pub calmar: Metric,
}
pub(super) fn total(p: &[PricePoint], w: Window, initial: f64) -> Result<Metric, StatsError> {
    if let Some(last) = point(p, w.end_ms) {
        Metric::available(last / initial - 1.)
    } else {
        Ok(Metric::unavailable("ancre finale absente"))
    }
}
pub(super) fn calmar(p: &[PricePoint], w: Window) -> Result<Metric, StatsError> {
    let daily = returns_from_prices(p, w)?;
    if !calendar::at_least_36_months(w)
        || !daily.missing_intervals.is_empty()
        || daily.expected_intervals == 0
    {
        return Ok(Metric::unavailable(
            "Calmar : 36 mois calendaires et grille quotidienne complète requis",
        ));
    }
    let (Some(start), Some(end)) = (point(p, w.start_ms), point(p, w.end_ms)) else {
        return Ok(Metric::unavailable("Calmar : ancres absentes"));
    };
    if start <= 0. {
        return Ok(Metric::unavailable("Calmar : capital initial marqué nul"));
    }
    let mut peak = start;
    let mut dd = 0_f64;
    // Uniquement ancres et minuits : les sorties intrajournalières ne changent pas la définition.
    for v in p.iter().filter(|p| {
        p.at_ms == w.start_ms
            || p.at_ms == w.end_ms
            || p.at_ms % crate::store::time::MS_PER_DAY == 0
    }) {
        peak = peak.max(v.close);
        dd = dd.max(1. - v.close / peak);
    }
    if dd == 0. {
        return Ok(Metric::unavailable("Calmar : drawdown quotidien nul"));
    }
    Metric::available(annualized(end / start, (w.end_ms - w.start_ms) as f64 / YEAR_MS)? / dd)
}
pub(super) fn calculate(
    id: &str,
    trajectory: Vec<PricePoint>,
    strategy: &[PricePoint],
    input: BenchmarkInput<'_>,
    options: &StatsOptions,
) -> Result<BenchmarkReference, StatsError> {
    let daily = returns_from_prices(&trajectory, input.window)?;
    let sdaily = returns_from_prices(strategy, input.window)?;
    let excess_sharpe = if daily.expected_intervals >= 2
        && daily.missing_intervals.is_empty()
        && sdaily.missing_intervals.is_empty()
    {
        let excess: Vec<_> = sdaily
            .intervals
            .iter()
            .zip(&daily.intervals)
            .map(|(s, b)| finite(s.value - b.value))
            .collect::<Result<_, _>>()?;
        let std = sample_standard_deviation(&excess)?;
        // Deux divisions et leur soustraction ne résolvent pas une variance sous
        // huit ULP de leur échelle. Cette borne est numérique, sans seuil économique.
        let scale = sdaily
            .intervals
            .iter()
            .chain(&daily.intervals)
            .map(|r| r.value.abs())
            .fold(1., f64::max);
        let roundoff_bound = 8. * f64::EPSILON * scale;
        if std > roundoff_bound {
            Metric::available(365.25_f64.sqrt() * mean(&excess)? / std)?
        } else {
            Metric::unavailable(
                "Sharpe : variance de l'excédent nulle ou non résolue (borne 8 ULP f64 de l'échelle des rendements)",
            )
        }
    } else {
        Metric::unavailable(
            "Sharpe : grille quotidienne complète identique et deux rendements requis",
        )
    };
    let total_return = if point(&trajectory, input.window.start_ms).is_some() {
        total(&trajectory, input.window, 1.)?
    } else {
        Metric::unavailable("ancre initiale du benchmark absente")
    };
    let monthly = monthly::compare(id, strategy, &trajectory, input, options)?;
    let calmar = calmar(&trajectory, input.window)?;
    Ok(BenchmarkReference {
        id: id.into(),
        trajectory,
        total_return,
        daily,
        excess_sharpe,
        monthly,
        calmar,
    })
}
