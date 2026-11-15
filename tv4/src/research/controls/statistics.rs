//! Rangs Monte Carlo conservateurs : le panneau demandé reste indivisible.
use super::types::*;

pub(super) fn finite(value: f64) -> Result<f64, ControlsError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ControlsError::NonFinite)
    }
}
pub(super) fn invalid(message: &str) -> ControlsError {
    ControlsError::InvalidInput(message.into())
}
/// Moyenne dans l'ordre de réplication ; écart-type échantillonnal (K-1).
pub(super) fn mean_and_sd(values: &[f64]) -> Result<(f64, Option<f64>), ControlsError> {
    if values.is_empty() {
        return Err(invalid("une série numérique vide n'a pas de moyenne"));
    }
    let mut sum = 0.;
    for &value in values {
        sum = finite(sum + finite(value)?)?;
    }
    let mean = finite(sum / values.len() as f64)?;
    let sd = if values.len() < 2 {
        None
    } else {
        // Welford évite une fausse dispersion des constantes fractionnaires lorsque
        // la somme répétée déplace légèrement la moyenne arithmétique calculée.
        let mut running_mean = values[0];
        let mut squares = 0.;
        for (index, &value) in values.iter().enumerate().skip(1) {
            let d = finite(value - running_mean)?;
            running_mean = finite(running_mean + d / (index + 1) as f64)?;
            squares = finite(squares + finite(d * finite(value - running_mean)?)?)?;
        }
        Some(finite((squares / (values.len() - 1) as f64).sqrt())?)
    };
    Ok((mean, sd))
}

/// Un seul mode demandé, N sources distinctes vérifiées par l'appelant moteur.
/// Les tranches empruntent les cellules partagées ; None n'est jamais remplacé par zéro.
/// N, K et les indices sont stricts. Une absence numérique légitime rend le panneau
/// inconclusif ; une incohérence de type ou une valeur non finie reste une erreur.
pub fn summarize_panel(
    observed: &[Option<f64>],
    cells: &[&[CellSummary]],
    options: &ControlOptions,
) -> Result<PanelStatistics, ControlsError> {
    options.validate()?;
    if !(1..=27).contains(&observed.len()) || cells.len() != observed.len() {
        return Err(invalid(
            "le panneau exige 1 à 27 sources et une ligne par source",
        ));
    }
    for score in observed.iter().flatten() {
        finite(*score)?;
    }
    for row in cells {
        if row.len() != options.replications as usize {
            return Err(invalid("une ligne ne possède pas K cellules"));
        }
        for (k, cell) in row.iter().enumerate() {
            if let Some(score) = cell.score {
                finite(score)?;
            }
            if cell.replication as usize != k {
                return Err(invalid("les réplications doivent être exactement 0..K"));
            }
            let coherent = match cell.availability {
                CellAvailability::Complete => cell.score.is_some() && cell.reason.is_none(),
                CellAvailability::Unavailable => cell.score.is_none() && cell.reason.is_some(),
            };
            if !coherent {
                return Err(invalid(
                    "score, disponibilité et raison de cellule incohérents",
                ));
            }
        }
    }
    let complete = observed.iter().all(Option::is_some)
        && cells
            .iter()
            .all(|row| row.iter().all(|c| c.score.is_some()));
    let k = options.replications;
    let resolution = 1. / (f64::from(k) + 1.);
    let critical_rank = (95 * (u64::from(k) + 1)).div_ceil(100);
    let maxima = if complete {
        (0..k as usize)
            .map(|column| {
                cells
                    .iter()
                    .filter_map(|row| row[column].score)
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let critical_value = if complete && critical_rank <= u64::from(k) {
        let mut sorted = maxima.clone();
        sorted.sort_by(f64::total_cmp);
        Some(sorted[critical_rank as usize - 1])
    } else {
        None
    };
    let family_score = if observed.iter().all(Option::is_some) {
        Some(
            observed
                .iter()
                .flatten()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
        )
    } else {
        None
    };
    let exceedances = |score: f64| maxima.iter().filter(|&&m| m >= score).count() as u32;
    let p_value = |b: u32| (1. + f64::from(b)) / (f64::from(k) + 1.);
    let family_exceedances = if complete {
        family_score.map(exceedances)
    } else {
        None
    };
    let sources = observed
        .iter()
        .zip(cells)
        .map(|(&score, row)| {
            let values: Option<Vec<f64>> = row.iter().map(|c| c.score).collect();
            let (control_mean, mc_standard_error, constant_controls) = if let Some(values) = values
            {
                let (mean, sd) = mean_and_sd(&values)?;
                (
                    Some(mean),
                    sd.map(|s| s / f64::from(k).sqrt()),
                    values.iter().all(|v| *v == values[0]),
                )
            } else {
                (None, None, false)
            };
            let delta = score
                .zip(control_mean)
                .map(|(s, m)| finite(s - m))
                .transpose()?;
            let b = if complete {
                score.map(exceedances)
            } else {
                None
            };
            Ok(SourceStatistics {
                score,
                control_mean,
                delta,
                mc_standard_error,
                constant_controls,
                exceedances: b,
                p_value: b.map(p_value),
                ties: if complete {
                    score.map(|s| maxima.iter().filter(|&&m| m == s).count() as u32)
                } else {
                    None
                },
                delta_at_least_010: delta.map(|d| d >= 0.10),
                above_critical: score.zip(critical_value).map(|(s, c)| s > c),
                temporal: None,
            })
        })
        .collect::<Result<_, ControlsError>>()?;
    Ok(PanelStatistics {
        status: if complete {
            PanelStatus::Complete
        } else {
            PanelStatus::Inconclusive
        },
        n_grid: observed.len() as u32,
        replications: k,
        smoke: k < 200,
        numeric_admissible: complete && k >= 200,
        resolution,
        critical_rank,
        critical_value,
        maxima,
        family_score,
        family_exceedances,
        family_p_value: family_exceedances.map(p_value),
        sources,
    })
}
