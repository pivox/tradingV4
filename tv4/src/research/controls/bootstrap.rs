//! Bootstrap mensuel commun, approximatif pour les fragments de durées différentes.
use super::random::uniform_below;
use super::statistics::{finite, invalid, mean_and_sd};
use super::types::*;
use crate::model::rng::SplitMix64;
use num_bigint::BigUint;

fn validate_axis(
    observed: &[MonthMoment],
    controls: &[&[MonthMoment]],
) -> Result<(), ControlsError> {
    if observed.is_empty() || controls.is_empty() {
        return Err(invalid(
            "le bootstrap exige des mois source et au moins un contrôle",
        ));
    }
    for row in std::iter::once(observed).chain(controls.iter().copied()) {
        if row.len() != observed.len() {
            return Err(invalid("axes mensuels de tailles différentes"));
        }
        for (index, m) in row.iter().enumerate() {
            finite(m.r_sum)?;
            if !(1..=12).contains(&m.month) || (m.count == 0 && m.r_sum != 0.) {
                return Err(invalid("moment mensuel incohérent"));
            }
            if (m.year, m.month) != (observed[index].year, observed[index].month)
                || (index > 0 && (row[index - 1].year, row[index - 1].month) >= (m.year, m.month))
            {
                return Err(invalid(
                    "les axes mensuels doivent être identiques et strictement croissants",
                ));
            }
        }
    }
    Ok(())
}
fn ratio(row: &[MonthMoment], indices: &[usize]) -> Result<Option<f64>, ControlsError> {
    let mut sum = 0.;
    let mut count = 0_u64;
    for &index in indices {
        let month = row
            .get(index)
            .ok_or_else(|| invalid("indice mensuel hors de l'axe"))?;
        sum = finite(sum + month.r_sum)?;
        count = count
            .checked_add(u64::from(month.count))
            .ok_or_else(|| invalid("effectif bootstrap débordé"))?;
    }
    if count == 0 {
        Ok(None)
    } else {
        Ok(Some(finite(sum / count as f64)?))
    }
}
fn delta_validated(
    observed: &[MonthMoment],
    controls: &[&[MonthMoment]],
    indices: &[usize],
) -> Result<Option<f64>, ControlsError> {
    let obs = ratio(observed, indices)?;
    let mut sum = 0.;
    let mut available = obs.is_some();
    // Même après un zéro, vérifier tous les journaux : une valeur non finie reste technique.
    for row in controls {
        if let Some(value) = ratio(row, indices)? {
            sum = finite(sum + value)?;
        } else {
            available = false;
        }
    }
    if available {
        Ok(Some(finite(
            obs.ok_or_else(|| invalid("ratio source absent"))? - sum / controls.len() as f64,
        )?))
    } else {
        Ok(None)
    }
}
/// Oracle pur : une même séquence de m indices sert à la source et aux K journaux.
/// None signifie qu'au moins un dénominateur requis est nul ; aucun ratio agrégé entre K.
pub fn delta_for_month_indices(
    observed: &[MonthMoment],
    controls: &[&[MonthMoment]],
    indices: &[usize],
) -> Result<Option<f64>, ControlsError> {
    validate_axis(observed, controls)?;
    if indices.len() != observed.len() {
        return Err(invalid("un tirage doit contenir m indices mensuels"));
    }
    delta_validated(observed, controls, indices)
}

/// B>=2 pour les oracles ; le moteur fixe B=10000. L'objet RNG est consommé directement.
/// Deux mois Complete sont une condition de couverture, jamais un filtre de moments.
/// Une seule réplication à dénominateur nul invalide toute l'incertitude temporelle.
pub fn paired_bootstrap(
    observed: &[MonthMoment],
    controls: &[&[MonthMoment]],
    coverage: &[MonthCoverage],
    rng: &mut SplitMix64,
    samples: u32,
) -> Result<BootstrapSummary, ControlsError> {
    validate_axis(observed, controls)?;
    if samples < 2 {
        return Err(invalid("le bootstrap exige B>=2"));
    }
    if coverage.len() != observed.len()
        || coverage
            .iter()
            .zip(observed)
            .any(|(c, m)| (c.year, c.month) != (m.year, m.month))
    {
        return Err(invalid(
            "la couverture doit qualifier exactement l'axe mensuel",
        ));
    }
    let complete_months = u32::try_from(
        coverage
            .iter()
            .filter(|m| m.status == MonthCoverageStatus::Complete)
            .count(),
    )
    .map_err(|_| invalid("couverture trop grande"))?;
    let months = u32::try_from(observed.len()).map_err(|_| invalid("axe mensuel trop grand"))?;
    let point = delta_validated(observed, controls, &(0..observed.len()).collect::<Vec<_>>())?;
    let mut result = BootstrapSummary {
        available: false,
        reason: None,
        approximate: true,
        samples,
        months,
        complete_months,
        point,
        standard_error: None,
        lb90: None,
        lb90_positive: None,
    };
    if point.is_none() {
        result.reason = Some(UnavailableReason::BootstrapZeroDenominator);
        return Ok(result);
    }
    if complete_months < 2 {
        result.reason = Some(UnavailableReason::InsufficientMonths);
        return Ok(result);
    }
    let bound = BigUint::from(observed.len());
    let mut deltas = Vec::with_capacity(samples as usize);
    let mut indices = vec![0; observed.len()];
    for _ in 0..samples {
        for index in &mut indices {
            *index = usize::try_from(uniform_below(rng, &bound)?)
                .map_err(|_| invalid("indice bootstrap hors capacité"))?;
        }
        let Some(delta) = delta_validated(observed, controls, &indices)? else {
            result.reason = Some(UnavailableReason::BootstrapZeroDenominator);
            return Ok(result);
        };
        deltas.push(delta);
    }
    let (_, sd) = mean_and_sd(&deltas)?;
    result.standard_error = sd;
    result.lb90 = crate::research::stats::quantile_type7(&deltas, 0.1)
        .map_err(|_| ControlsError::NonFinite)?;
    result.lb90_positive = result.lb90.map(|v| v > 0.);
    result.available = true;
    Ok(result)
}
