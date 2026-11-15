//! Rééchantillonnage uniforme de segments marqués, sans proxy de PnL clos.
mod segments;
use super::approx::quantile_type7;
use super::random::{STREAM_MONTE_CARLO, STREAM_REMOVAL, uniform_index};
use super::types::{
    Availability, MarkPoint, Metric, StatsError, StatsOptions, StatsSizing, TradeRecord, Window,
    finite, sorted_trades,
};
use crate::model::rng::{self, SplitMix64};
use segments::{Segment, prepare};
use serde::{Deserialize, Serialize};

pub(crate) const DAY_MS: i64 = 86_400_000;
pub(crate) const YEAR_MS: i64 = 31_557_600_000;
const BLOCK_TRADES: usize = 20;

/// Correspondance vers le point marqué après réalisation, jamais l'heure économique CSV.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeClosure {
    pub row: usize,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonteCarloStats {
    pub availability: Availability,
    pub n_trades: usize,
    pub n_r: usize,
    pub n_removed: usize,
    pub requested_block_trades: usize,
    pub n_blocks: usize,
    pub block_sizes: Vec<usize>,
    pub block_durations_ms: Vec<i64>,
    pub source_duration_ms: i64,
    pub cadence_trades_per_year: Option<f64>,
    pub sizing: StatsSizing,
    pub horizon_one_year_ms: i64,
    pub drawdown_grid: String,
    pub dd_initial_p95_2y: Metric,
    pub dd_peak_p95_2y: Metric,
    pub return_p5_2y: Metric,
    pub removal_expectancy_median_r: Metric,
    pub probability_dd20_1y: Metric,
}

/// Les clôtures purgées éventuellement fournies sont validées sans créer de frontière.
/// Un ratio source 0/0 atteint avant tout facteur zéro dans son segment rend
/// la projection capitalisée indisponible. Après un facteur zéro, la faillite
/// synthétique est absorbante : aucun ratio suivant n'est nécessaire. Les points
/// strictement après l'horizon ne sont jamais utilisés, y compris pour ce contrôle.
/// Les flux suppression et projection ne dépendent ni du nombre de tirages de l'autre,
/// ni de la disponibilité de ses entrées.
pub fn monte_carlo(
    trades: &[TradeRecord],
    closures: &[TradeClosure],
    marks: &[MarkPoint],
    window: Window,
    capital: f64,
    sizing: StatsSizing,
    options: &StatsOptions,
) -> Result<MonteCarloStats, StatsError> {
    options.validate()?;
    let kept = sorted_trades(trades)?;
    let source_duration_ms = window
        .end_ms
        .checked_sub(window.start_ms)
        .filter(|&length| length > 0)
        .ok_or_else(|| {
            StatsError::InvalidInput("fenêtre vide, inversée ou durée hors plage".into())
        })?;
    if !capital.is_finite() || capital <= 0. {
        return Err(StatsError::InvalidInput(
            "capital strictement positif et fini requis".into(),
        ));
    }
    let prepared = prepare(trades, closures, marks, window, capital, sizing)?;
    let n = kept.len();
    let n_r = kept.iter().filter(|t| t.r_multiple.is_some()).count();
    let unavailable = Metric::unavailable("projection marquée non identifiable");
    let mut result = MonteCarloStats {
        availability: Availability::Unavailable,
        n_trades: n, n_r, n_removed: n / 5,
        requested_block_trades: BLOCK_TRADES,
        n_blocks: prepared.sizes.len(), block_sizes: prepared.sizes,
        block_durations_ms: prepared.durations,
        source_duration_ms,
        cadence_trades_per_year: if n == 0 { None } else { Some(finite(n as f64 * YEAR_MS as f64 / source_duration_ms as f64)?) },
        sizing, horizon_one_year_ms: YEAR_MS,
        drawdown_grid: "tous points fournis, dont clôtures quotidiennes et réalisations/blocs ; grille conservatrice".into(),
        dd_initial_p95_2y: unavailable.clone(), dd_peak_p95_2y: unavailable.clone(),
        return_p5_2y: unavailable.clone(), removal_expectancy_median_r: unavailable.clone(),
        probability_dd20_1y: unavailable,
    };
    result.removal_expectancy_median_r = removal(&kept, options)?;
    let reason = if n < BLOCK_TRADES {
        Some("au moins 20 trades conservés requis")
    } else if source_duration_ms < 30 * DAY_MS {
        Some("au moins 30 jours observés requis pour la projection annuelle")
    } else if result.n_blocks < 2 {
        Some("au moins deux blocs temporels identifiables requis")
    } else {
        prepared.unavailable.as_deref()
    };
    if let Some(reason) = reason {
        result.dd_initial_p95_2y = Metric::unavailable(reason);
        result.dd_peak_p95_2y = Metric::unavailable(reason);
        result.return_p5_2y = Metric::unavailable(reason);
        result.probability_dd20_1y = Metric::unavailable(reason);
    } else if sizing == StatsSizing::Compounding
        && prepared.segments.iter().any(|s| {
            for point in &s.steps {
                if point.offset_ms > 2 * YEAR_MS {
                    break;
                }
                if point.factor.is_none() {
                    return true;
                }
                // Une equity synthétique positive devient zéro ici ; les 0/0
                // suivants, même au raccord tail→head, ne sont plus nécessaires.
                if point.factor.is_some_and(|factor| factor == 0.) {
                    break;
                }
            }
            false
        })
    {
        let reason = "ratio de capitalisation non identifiable après une equity source nulle";
        result.dd_initial_p95_2y = Metric::unavailable(reason);
        result.dd_peak_p95_2y = Metric::unavailable(reason);
        result.return_p5_2y = Metric::unavailable(reason);
        result.probability_dd20_1y = Metric::unavailable(reason);
    } else {
        let mut rng = SplitMix64::new(rng::draw(options.seed, [STREAM_MONTE_CARLO, 0, 0, 0]));
        let mut dd = Vec::with_capacity(options.samples as usize);
        let mut peak_dd = Vec::with_capacity(options.samples as usize);
        let mut returns = Vec::with_capacity(options.samples as usize);
        let mut stops = 0u32;
        for _ in 0..options.samples {
            let path = simulate(&prepared.segments, capital, sizing, &mut rng)?;
            dd.push(path.dd_initial);
            peak_dd.push(path.dd_peak);
            returns.push(path.return_2y);
            stops += u32::from(path.stop_1y);
        }
        result.dd_initial_p95_2y = quantile_metric(&dd, 0.95)?;
        result.dd_peak_p95_2y = quantile_metric(&peak_dd, 0.95)?;
        result.return_p5_2y = quantile_metric(&returns, 0.05)?;
        result.probability_dd20_1y = Metric::available(stops as f64 / options.samples as f64)?;
    }
    let metrics = [
        &result.dd_initial_p95_2y,
        &result.dd_peak_p95_2y,
        &result.return_p5_2y,
        &result.removal_expectancy_median_r,
        &result.probability_dd20_1y,
    ];
    let count = metrics.iter().filter(|m| m.value.is_some()).count();
    result.availability = match count {
        0 => Availability::Unavailable,
        5 => Availability::Complete,
        _ => Availability::Partial,
    };
    Ok(result)
}

fn quantile_metric(values: &[f64], p: f64) -> Result<Metric, StatsError> {
    match quantile_type7(values, p)? {
        Some(value) => Metric::available(value),
        None => Ok(Metric::unavailable("aucun tirage")),
    }
}

fn removal(kept: &[&TradeRecord], options: &StatsOptions) -> Result<Metric, StatsError> {
    let n = kept.len();
    let removed = n / 5;
    if removed == 0 {
        return Ok(Metric::unavailable(
            "aucun trade retiré : floor(20% × n) vaut zéro",
        ));
    }
    if kept.iter().any(|t| t.r_multiple.is_none()) {
        return Ok(Metric::unavailable(
            "au moins un R conservé est absent ; suppression pooled indisponible",
        ));
    }
    let values: Vec<_> = kept.iter().filter_map(|t| t.r_multiple).collect();
    let mut rng = SplitMix64::new(rng::draw(options.seed, [STREAM_REMOVAL, 0, 0, 0]));
    let mut means = Vec::with_capacity(options.samples as usize);
    for _ in 0..options.samples {
        let mut indices: Vec<_> = (0..n).collect();
        // Fisher-Yates partiel : sous-ensemble uniforme sans remise.
        for i in 0..removed {
            let j = i + uniform_index(&mut rng, (n - i) as u64);
            indices.swap(i, j);
        }
        // Somme en ordre source stable, indépendante de l'ordre du tirage.
        let mut selected = vec![false; n];
        for &i in &indices[..removed] {
            selected[i] = true;
        }
        let mut sum = 0.;
        for (i, &value) in values.iter().enumerate() {
            if !selected[i] {
                sum = finite(sum + value)?;
            }
        }
        means.push(finite(sum / (n - removed) as f64)?);
    }
    quantile_metric(&means, 0.5)
}

struct PathStats {
    dd_initial: f64,
    dd_peak: f64,
    return_2y: f64,
    stop_1y: bool,
}
fn simulate(
    segments: &[Segment],
    capital: f64,
    sizing: StatsSizing,
    rng: &mut SplitMix64,
) -> Result<PathStats, StatsError> {
    let mut clock = 0i64;
    let mut equity = capital;
    let mut peak = capital;
    let mut dd_initial: f64 = 0.;
    let mut dd_peak: f64 = 0.;
    let mut stop_1y = false;
    let horizon = 2 * YEAR_MS;
    while clock < horizon {
        let segment = &segments[uniform_index(rng, segments.len() as u64)];
        let remaining = horizon - clock;
        for step in &segment.steps {
            if step.offset_ms > remaining {
                break;
            }
            if equity > 0. {
                equity = match sizing {
                    StatsSizing::Research => finite(equity + step.delta)?.max(0.),
                    StatsSizing::Compounding => finite(
                        equity
                            * step.factor.ok_or_else(|| {
                                StatsError::InvalidInput(
                                    "ratio de marquage nul non identifiable".into(),
                                )
                            })?,
                    )?
                    .max(0.),
                };
            }
            peak = peak.max(equity);
            let loss = finite(peak - equity)?;
            dd_initial = dd_initial.max(finite(loss / capital)?);
            dd_peak = dd_peak.max(finite(loss / peak)?);
            if clock + step.offset_ms <= YEAR_MS && loss >= 0.2 * capital {
                stop_1y = true;
            }
        }
        // Le dernier point connu est tenu jusqu'à H : aucun point futur interpolé.
        clock += segment.duration_ms.min(remaining);
        if equity == 0. {
            break;
        }
    }
    Ok(PathStats {
        dd_initial,
        dd_peak,
        return_2y: finite(equity / capital - 1.)?,
        stop_1y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use segments::Step;
    #[test]
    fn reliquat_horizon_ignore_point_futur_et_faillite_ne_ressuscite_pas() {
        let horizon = 2 * YEAR_MS;
        let segments = vec![Segment {
            duration_ms: horizon + 2,
            steps: vec![
                Step {
                    offset_ms: 1,
                    delta: -10.,
                    factor: Some(0.9),
                },
                Step {
                    offset_ms: horizon + 1,
                    delta: 1000.,
                    factor: Some(12.),
                },
            ],
        }];
        let mut rng = SplitMix64::new(1);
        let s = simulate(&segments, 100., StatsSizing::Research, &mut rng).expect("simulation");
        assert!((s.return_2y + 0.1).abs() < 1e-15);
        assert_eq!(s.dd_initial, 0.1);
        assert!(!s.stop_1y);
        let segments = vec![Segment {
            duration_ms: horizon,
            steps: vec![
                Step {
                    offset_ms: 1,
                    delta: -110.,
                    factor: Some(0.),
                },
                Step {
                    offset_ms: 2,
                    delta: 1000.,
                    factor: Some(12.),
                },
            ],
        }];
        let s = simulate(&segments, 100., StatsSizing::Research, &mut rng).expect("faillite");
        assert_eq!(s.return_2y, -1.);
        assert_eq!(s.dd_initial, 1.);
        assert!(s.stop_1y);
    }
}
