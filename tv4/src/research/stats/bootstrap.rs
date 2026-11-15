//! Bootstrap circulaire par blocs contigus de trades conservés, R déjà nets.
use super::approx::{quantile_type7, sample_standard_deviation};
use super::types::{
    Availability, Metric, StatsError, StatsOptions, TradeRecord, finite, sorted_trades,
};
use crate::model::rng::{self, SplitMix64};
use serde::{Deserialize, Serialize};

/// Espaces de tirages distincts réservés aux post-traitements S13.
pub const STREAM_BOOTSTRAP: u64 = 0x5331_3300_0000_0001;
pub const STREAM_MONTE_CARLO: u64 = 0x5331_3300_0000_0002;
pub const STREAM_REMOVAL: u64 = 0x5331_3300_0000_0003;
pub const STREAM_MONTHLY_BOOTSTRAP: u64 = 0x5331_3300_0000_0004;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootstrapStats {
    pub availability: Availability,
    pub n_trades: usize,
    pub n_r: usize,
    pub r_coverage: Option<f64>,
    pub mean_r: Metric,
    pub lb90_r: Metric,
    pub standard_error_r: Metric,
}

/// Échantillonnage entier uniforme par rejet de la zone incomplète de 2^64.
fn uniform_index(rng: &mut SplitMix64, bound: u64) -> usize {
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = rng.next_u64();
        if value >= threshold {
            return (value % bound) as usize;
        }
    }
}

pub fn bootstrap(
    trades: &[TradeRecord],
    options: &StatsOptions,
) -> Result<BootstrapStats, StatsError> {
    options.validate()?;
    let sorted = sorted_trades(trades)?;
    let n = sorted.len();
    let n_r = sorted.iter().filter(|t| t.r_multiple.is_some()).count();
    let coverage = if n == 0 {
        None
    } else {
        Some(n_r as f64 / n as f64)
    };
    let unavailable = Metric::unavailable(if n == 0 {
        "aucun trade conservé"
    } else {
        "au moins un R conservé est absent ; estimations pooled indisponibles"
    });
    let mut result = BootstrapStats {
        availability: Availability::Unavailable,
        n_trades: n,
        n_r,
        r_coverage: coverage,
        mean_r: unavailable.clone(),
        lb90_r: unavailable.clone(),
        standard_error_r: unavailable,
    };
    if n == 0 || n != n_r {
        return Ok(result);
    }
    let values: Vec<_> = sorted.iter().filter_map(|t| t.r_multiple).collect();
    let mut sum = 0.;
    for &r in &values {
        sum = finite(sum + r)?;
    }
    result.mean_r = Metric::available(sum / n as f64)?;
    if n < 2 {
        result.availability = Availability::Partial;
        result.lb90_r =
            Metric::unavailable("au moins deux trades conservés requis pour le bootstrap");
        result.standard_error_r = result.lb90_r.clone();
        return Ok(result);
    }
    let bound = u64::try_from(n)
        .map_err(|_| StatsError::InvalidInput("effectif hors capacité du RNG".into()))?;
    let mut rng = SplitMix64::new(rng::draw(options.seed, [STREAM_BOOTSTRAP, 0, 0, 0]));
    let mut means = Vec::with_capacity(options.samples as usize);
    for _ in 0..options.samples {
        let mut sampled = 0;
        let mut sum = 0.;
        while sampled < n {
            let start = uniform_index(&mut rng, bound);
            let length = options.bootstrap_block.min(n - sampled);
            for offset in 0..length {
                sum = finite(sum + values[(start + offset) % n])?;
            }
            sampled += length;
        }
        means.push(finite(sum / n as f64)?);
    }
    let lower = quantile_type7(&means, 0.1)?
        .ok_or_else(|| StatsError::InvalidInput("bootstrap vide".into()))?;
    result.lb90_r = Metric::available(lower)?;
    result.standard_error_r = Metric::available(sample_standard_deviation(&means)?)?;
    result.availability = Availability::Complete;
    Ok(result)
}
