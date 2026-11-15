//! Orchestration lexicale : préparation, vrais rejeux et réductions possédées.
use super::*;
use crate::research::controls::bootstrap::paired_bootstrap;
use crate::research::controls::random::bootstrap_rng;
use crate::research::controls::statistics::summarize_panel;
use crate::research::controls::*;

const BOOTSTRAP_SAMPLES: u32 = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct BatchOutcome {
    pub status: PanelStatus,
    pub reasons: Vec<UnavailableReason>,
    /// Cardinalités demandées, conservées même si la préparation ne produit aucune cellule.
    pub requested_sources: u32,
    pub requested_replications: u32,
    pub sources: Vec<SourceSummary>,
    pub statistics: Vec<ModeStatistics>,
    /// Inconnu si la continuation n'a pas été atteinte ; jamais un faux compteur nul.
    pub prep: Option<PrepStats>,
    pub work: EngineWork,
}

/// Exécution complète sans horloge ni I/O ; le callback consomme les vues Full immédiatement.
pub fn run_batch(
    inputs: BatchInputs<'_>,
    validate_observed: &mut dyn FnMut(usize, ObservedRun<'_>) -> Result<ObservedValidation, String>,
) -> Result<BatchOutcome, ControlError> {
    run_batch_observed(inputs, validate_observed, &mut NoopPhaseObserver)
}

/// L'observateur appartient à l'appelant, qui peut mesurer les phases à l'extérieur du calcul.
pub fn run_batch_observed(
    inputs: BatchInputs<'_>,
    validate_observed: &mut dyn FnMut(usize, ObservedRun<'_>) -> Result<ObservedValidation, String>,
    observer: &mut dyn PhaseObserver,
) -> Result<BatchOutcome, ControlError> {
    run_batch_with_work(
        inputs,
        &mut EngineWork::default(),
        validate_observed,
        observer,
    )
}

/// Le compteur externe garde aussi les débits partiels après une erreur technique.
/// Une indisponibilité attendue de préparation devient un résultat inconclusif publiable.
pub fn run_batch_with_work(
    inputs: BatchInputs<'_>,
    work: &mut EngineWork,
    validate_observed: &mut dyn FnMut(usize, ObservedRun<'_>) -> Result<ObservedValidation, String>,
    observer: &mut dyn PhaseObserver,
) -> Result<BatchOutcome, ControlError> {
    let requested_sources =
        u32::try_from(inputs.sources.len()).map_err(|_| invalid("panneau trop grand"))?;
    let requested_replications = inputs.options.replications;
    let result =
        with_prepared_batch_with_work(inputs, work, validate_observed, observer, |batch| {
            let sources = replay_prepared(batch)?;
            batch.observer.phase_started(ControlPhase::Statistics);
            let statistics = statistics(batch, &sources);
            batch.observer.phase_finished(ControlPhase::Statistics);
            Ok((sources, statistics?, batch.prep))
        });
    let (sources, statistics, prep) = match result {
        Ok((sources, statistics, prep)) => (sources, statistics, Some(prep)),
        Err(ControlError::Unavailable(reason)) => {
            return Ok(BatchOutcome {
                status: PanelStatus::Inconclusive,
                reasons: vec![reason],
                requested_sources,
                requested_replications,
                sources: Vec::new(),
                statistics: Vec::new(),
                prep: None,
                work: work.clone(),
            });
        }
        Err(error) => return Err(error),
    };
    let mut reasons = Vec::new();
    for source in &sources {
        for reason in source.observed_reason.into_iter().chain(
            source
                .modes
                .iter()
                .flat_map(|m| m.cells.iter().filter_map(|c| c.reason)),
        ) {
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
        }
    }
    for reason in statistics
        .iter()
        .flat_map(|m| m.panel.sources.iter())
        .filter_map(|s| s.temporal.as_ref().and_then(|t| t.reason))
    {
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    let complete = statistics
        .iter()
        .all(|m| m.panel.status == PanelStatus::Complete);
    Ok(BatchOutcome {
        status: if complete {
            PanelStatus::Complete
        } else {
            PanelStatus::Inconclusive
        },
        reasons,
        requested_sources,
        requested_replications,
        sources,
        statistics,
        prep,
        work: work.clone(),
    })
}

fn unavailable_temporal(
    source: &SourceSummary,
    reason: UnavailableReason,
    point: Option<f64>,
) -> Result<BootstrapSummary, ControlError> {
    Ok(BootstrapSummary {
        available: false,
        reason: Some(reason),
        approximate: true,
        samples: BOOTSTRAP_SAMPLES,
        months: u32::try_from(source.month_coverage.len())
            .map_err(|_| invalid("axe mensuel trop grand"))?,
        complete_months: u32::try_from(
            source
                .month_coverage
                .iter()
                .filter(|m| m.status == MonthCoverageStatus::Complete)
                .count(),
        )
        .map_err(|_| invalid("couverture trop grande"))?,
        point,
        standard_error: None,
        lb90: None,
        lb90_positive: None,
    })
}
fn statistics(
    batch: &PreparedBatch<'_, '_>,
    sources: &[SourceSummary],
) -> Result<Vec<ModeStatistics>, ControlError> {
    let observed: Vec<_> = sources.iter().map(|s| s.observed_score).collect();
    // Cache par parent ET intervention effective. Les p/max restent propres au mode global
    // demandé : un autre parent du panneau peut conserver un véritable filtre BTC.
    let mut temporal: Vec<Vec<(ControlMode, Arc<BootstrapSummary>)>> =
        vec![Vec::new(); sources.len()];
    let mut results = Vec::new();
    for &mode in &batch.options.modes {
        let modes = sources
            .iter()
            .map(|s| {
                s.modes
                    .iter()
                    .find(|m| m.mode == mode)
                    .ok_or_else(|| invalid("mode demandé absent de la réduction"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let cells: Vec<_> = modes.iter().map(|m| m.cells.as_slice()).collect();
        let mut panel = summarize_panel(&observed, &cells, batch.options)?;
        for (i, (source, mode_summary)) in sources.iter().zip(&modes).enumerate() {
            let effective = mode_summary.alias_of.unwrap_or(mode);
            let cached = temporal[i]
                .iter()
                .find(|(m, _)| *m == effective)
                .map(|(_, t)| t.clone());
            let summary = if let Some(cached) = cached {
                cached
            } else {
                let reason = source
                    .observed_reason
                    .or_else(|| mode_summary.cells.iter().find_map(|c| c.reason));
                let estimate = if let Some(reason) = reason {
                    unavailable_temporal(source, reason, panel.sources[i].delta)?
                } else {
                    let controls: Vec<_> = mode_summary
                        .cells
                        .iter()
                        .map(|c| c.months.as_slice())
                        .collect();
                    let mut rng = bootstrap_rng(
                        batch.options.seed,
                        &batch.sources[i].parent_config_hash,
                        effective,
                    )
                    .map_err(ControlsError::from)?;
                    paired_bootstrap(
                        &source.observed_months,
                        &controls,
                        &source.month_coverage,
                        &mut rng,
                        BOOTSTRAP_SAMPLES,
                    )?
                };
                let estimate = Arc::new(estimate);
                temporal[i].push((effective, estimate.clone()));
                estimate
            };
            panel.sources[i].temporal = Some(summary);
        }
        let alias_of = modes
            .first()
            .and_then(|m| m.alias_of)
            .filter(|alias| modes.iter().all(|m| m.alias_of == Some(*alias)));
        results.push(ModeStatistics {
            mode,
            alias_of,
            panel,
        });
    }
    Ok(results)
}
