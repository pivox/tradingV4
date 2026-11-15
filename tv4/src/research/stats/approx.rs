//! Mathématique approximative confinée hors simulation et hors config_hash.
use super::types::{StatsError, finite};

/// Quantile empirique type 7 : tri, rang `(n-1)*p`, interpolation linéaire.
/// Une série vide n'a pas de quantile ; une valeur non finie est une erreur.
pub fn quantile_type7(values: &[f64], probability: f64) -> Result<Option<f64>, StatsError> {
    if !probability.is_finite()
        || !(0.0..=1.0).contains(&probability)
        || values.iter().any(|v| !v.is_finite())
    {
        return Err(StatsError::InvalidInput(
            "quantile : valeurs finies et probabilité dans [0,1] requises".into(),
        ));
    }
    if values.is_empty() {
        return Ok(None);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (sorted.len() - 1) as f64 * probability;
    let lower = rank as usize;
    let weight = rank - lower as f64;
    let upper = (lower + 1).min(sorted.len() - 1);
    Ok(Some(finite(
        sorted[lower] * (1. - weight) + sorted[upper] * weight,
    )?))
}

pub(crate) fn sample_standard_deviation(values: &[f64]) -> Result<f64, StatsError> {
    if values.len() < 2 {
        return Err(StatsError::InvalidInput(
            "écart-type : au moins deux valeurs requises".into(),
        ));
    }
    let mut mean = 0.;
    let mut m2 = 0.;
    for (i, &value) in values.iter().enumerate() {
        let delta = finite(value - mean)?;
        mean = finite(mean + delta / (i + 1) as f64)?;
        m2 = finite(m2 + delta * (value - mean))?;
    }
    finite((m2 / (values.len() - 1) as f64).sqrt())
}

pub(crate) fn ln(value: f64) -> Result<f64, StatsError> {
    if !value.is_finite() || value <= 0. {
        return Err(StatsError::InvalidInput(
            "logarithme : argument positif fini requis".into(),
        ));
    }
    finite(value.ln())
}
pub(crate) fn exp_m1(value: f64) -> Result<f64, StatsError> {
    finite(value)?;
    finite(value.exp_m1())
}
pub(crate) fn powf(value: f64, exponent: f64) -> Result<f64, StatsError> {
    finite(value)?;
    finite(exponent)?;
    if value < 0. || (value == 0. && exponent <= 0.) {
        return Err(StatsError::InvalidInput(
            "puissance : domaine réel positif requis".into(),
        ));
    }
    finite(value.powf(exponent))
}
pub(crate) fn annualized(growth: f64, years: f64) -> Result<f64, StatsError> {
    if growth == 0. {
        return Ok(-1.);
    }
    exp_m1(ln(growth)? / years)
}
