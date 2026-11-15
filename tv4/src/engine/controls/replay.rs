//! Échantillonnage et réduction immédiate des vrais journaux de portefeuille.
use super::*;
use crate::research::controls::random::{calendar_rng, uniform_below};
use crate::research::controls::*;
use crate::sim::Trade;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn technical(message: &str) -> ControlError {
    ControlError::InvalidInput(message.into())
}
fn retained(source: &PreparedSource<'_, '_>, t: &Trade) -> bool {
    source.runtime.research.is_none_or(|s| {
        !crate::model::blocks::is_purged(
            t.entry_signal_ms,
            t.exit_ms,
            s.decision_end_ms,
            t.forced_close,
        )
    })
}
fn year(ts: Ts) -> Result<i32, ControlError> {
    i32::try_from(crate::store::time::civil_from_days(ts.div_euclid(86_400_000)).0)
        .map_err(|_| technical("année UTC hors borne"))
}
fn fingerprint(
    source: &PreparedSource<'_, '_>,
    trace: &NativeTrace,
) -> Result<String, ControlError> {
    let t = &trace.trade;
    if [
        t.entry_ref_price,
        t.entry_price,
        t.exit_ref_price,
        t.exit_price,
    ]
    .iter()
    .any(|v| !v.is_finite())
        || [t.stop_at_entry, t.stop_at_exit]
            .iter()
            .flatten()
            .any(|v| !v.is_finite())
    {
        return Err(ControlsError::NonFinite.into());
    }
    crate::model::hash::hash_json(&serde_json::json!({
        "signal":t.entry_signal_ms,"entry_order":t.entry_order_ms,"fill":t.entry_ms,
        "entry_ref":t.entry_ref_price,"entry_price":t.entry_price,"entry_liquidity":t.entry_liquidity,
        "exit_signal":t.exit_signal_ms,"exit_order":t.exit_order_ms,"exit":t.exit_ms,
        "exit_ref":t.exit_ref_price,"exit_price":t.exit_price,"exit_liquidity":t.exit_liquidity,
        "release":trace.release_ms,"reason":t.exit_reason,"forced":t.forced_close,
        "stop_entry":t.stop_at_entry,"stop_exit":t.stop_at_exit,"moved":t.stop_moved,
        "entry_tag":t.entry_tag,"exit_tag":t.exit_tag,"retained":retained(source,t),
    })).map_err(|e|ControlError::Engine(e.into()))
}
fn quota_checks(
    source: &PreparedSource<'_, '_>,
    trace: &[NativeTrace],
) -> Result<Vec<PairQuotaCheck>, ControlError> {
    let universe = source.runtime.config.universe;
    if source.runtime.quotas.len() != universe.len()
        || universe
            .iter()
            .any(|s| !source.runtime.quotas.contains_key(s))
    {
        return Err(technical("quotas exigés pour chaque paire de l'univers"));
    }
    let mut actual = vec![BTreeMap::<i32, (u32, u32)>::new(); universe.len()];
    let mut lots = BTreeSet::new();
    let mut previous = vec![None::<Ts>; universe.len()];
    let mut ordered: Vec<_> = trace.iter().collect();
    ordered.sort_by_key(|t| (t.leg, t.trade.entry_signal_ms));
    for t in ordered {
        if t.leg >= universe.len() || !lots.insert((t.leg, t.trade.lot_id)) {
            return Err(technical("sortie partielle ou jambe inconnue"));
        }
        if previous[t.leg].is_some_and(|release| release > t.trade.entry_signal_ms) {
            return Err(technical("pyramidage ou chevauchement single_lot"));
        }
        previous[t.leg] = Some(t.release_ms);
        let counts = actual[t.leg].entry(year(t.trade.entry_ms)?).or_default();
        counts.0 = counts
            .0
            .checked_add(1)
            .ok_or_else(|| technical("quota débordé"))?;
        if retained(source, &t.trade) {
            counts.1 = counts
                .1
                .checked_add(1)
                .ok_or_else(|| technical("quota débordé"))?;
        }
    }
    universe
        .iter()
        .enumerate()
        .map(|(leg, symbol)| {
            let expected = source.runtime.quotas[symbol].clone();
            validate_quotas(&expected).map_err(ControlsError::from)?;
            for q in &expected {
                actual[leg].entry(q.year).or_default();
            }
            let values: Vec<_> = actual[leg]
                .iter()
                .map(|(&year, &(filled, retained))| AnnualQuota {
                    year,
                    filled,
                    retained,
                })
                .collect();
            Ok(PairQuotaCheck {
                symbol: symbol.clone(),
                conformant: values == expected,
                expected,
                actual: values,
            })
        })
        .collect()
}
fn source_in_support(
    source: &PreparedSource<'_, '_>,
    catalogues: &[PairCatalogue],
) -> Result<bool, ControlError> {
    for t in &source.observed_trace {
        let Some(cat) = catalogues.get(t.leg) else {
            return Err(technical("jambe source absente"));
        };
        let expected = fingerprint(source, t)?;
        let mut found = false;
        for interval in cat
            .intervals
            .iter()
            .filter(|i| i.signal_ms == t.trade.entry_signal_ms)
        {
            let probe = &source.probes[t.leg][interval.decision];
            if let (Some(trade), Some(release_ms)) = (probe.trade, probe.release_ms)
                && fingerprint(
                    source,
                    &NativeTrace {
                        leg: t.leg,
                        trade,
                        release_ms,
                    },
                )? == expected
            {
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
    }
    Ok(true)
}
fn coverage(source: &PreparedSource<'_, '_>) -> Result<Vec<MonthCoverage>, ControlError> {
    let bounds = portfolio::controls::played_bounds(source);
    let Some(mut start) = bounds.iter().flatten().map(|b| b.0).min() else {
        return Ok(Vec::new());
    };
    let mut end = bounds
        .iter()
        .flatten()
        .map(|b| b.1)
        .max()
        .ok_or_else(|| technical("fenêtre absente"))?;
    if let Some(spec) = source.runtime.research {
        // L'axe garde aussi les mois déclarés sans observation, explicitement qualifiés.
        start = spec.decision_start_ms;
        end = spec.decision_end_ms;
    }
    if end <= start {
        return Ok(Vec::new());
    }
    let (mut y, mut m, _) = crate::store::time::civil_from_days(start.div_euclid(86_400_000));
    let mut result = Vec::new();
    loop {
        let month_start = crate::store::time::days_from_civil(y, m, 1) * 86_400_000;
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        let month_end = crate::store::time::days_from_civil(ny, nm, 1) * 86_400_000;
        if month_start >= end {
            break;
        }
        let days =
            portfolio::controls::daily_coverage(source, month_start.max(start), month_end.min(end));
        let excluded = days.iter().all(|(eligible, _)| !*eligible);
        let complete =
            start <= month_start && end >= month_end && days.iter().all(|(_, observed)| *observed);
        result.push(MonthCoverage {
            year: i32::try_from(y).map_err(|_| technical("année hors borne"))?,
            month: u8::try_from(m).map_err(|_| technical("mois hors borne"))?,
            status: if excluded {
                MonthCoverageStatus::Excluded
            } else if complete {
                MonthCoverageStatus::Complete
            } else {
                MonthCoverageStatus::Partial
            },
        });
        y = ny;
        m = nm;
    }
    Ok(result)
}
struct Reduction {
    score: Option<f64>,
    reason: Option<UnavailableReason>,
    months: Vec<MonthMoment>,
}
fn reduce(
    source: &PreparedSource<'_, '_>,
    trace: &[NativeTrace],
    coverage: &[MonthCoverage],
) -> Result<Reduction, ControlError> {
    let mut months: Vec<_> = coverage
        .iter()
        .map(|m| MonthMoment {
            year: m.year,
            month: m.month,
            r_sum: 0.0,
            count: 0,
        })
        .collect();
    let mut total = 0.0;
    let mut count = 0_u32;
    let mut missing = false;
    let mut absolute_sum = 0.0;
    for event in trace.iter().filter(|t| retained(source, &t.trade)) {
        let t = &event.trade;
        if !t.qty.is_finite() || !t.pnl_net.is_finite() {
            return Err(ControlsError::NonFinite.into());
        }
        let risk = t.stop_at_entry.map(|stop| t.qty * (t.entry_price - stop));
        if risk.is_some_and(|r| !r.is_finite()) {
            return Err(ControlsError::NonFinite.into());
        }
        let Some(risk) = risk.filter(|r| *r > 0.0) else {
            missing = true;
            continue;
        };
        if !risk.is_finite() || !t.pnl_net.is_finite() {
            return Err(ControlsError::NonFinite.into());
        }
        let r = t.pnl_net / risk;
        if !r.is_finite() {
            return Err(ControlsError::NonFinite.into());
        }
        total += r;
        absolute_sum += r.abs();
        count = count
            .checked_add(1)
            .ok_or_else(|| technical("effectif hors borne"))?;
        let (y, m, _) = crate::store::time::civil_from_days(t.exit_ms.div_euclid(86_400_000));
        let y = i32::try_from(y).map_err(|_| technical("année hors borne"))?;
        if let Some(moment) = months
            .iter_mut()
            .find(|v| v.year == y && u32::from(v.month) == m)
        {
            moment.r_sum += r;
            moment.count += 1;
        } else {
            // La clôture économique terminale peut appartenir au mois suivant.
            months.push(MonthMoment {
                year: y,
                month: u8::try_from(m).map_err(|_| technical("mois hors borne"))?,
                r_sum: r,
                count: 1,
            });
        }
    }
    if !total.is_finite()
        || !absolute_sum.is_finite()
        || months.iter().any(|m| !m.r_sum.is_finite())
    {
        return Err(ControlsError::NonFinite.into());
    }
    if missing {
        return Ok(Reduction {
            score: None,
            reason: Some(UnavailableReason::MissingPositiveRisk),
            // Une somme inconnue n'est jamais publiée comme un vrai mois sans trades.
            months: Vec::new(),
        });
    }
    let month_count = months
        .iter()
        .try_fold(0_u32, |n, m| n.checked_add(m.count))
        .ok_or_else(|| technical("effectif mensuel débordé"))?;
    let month_sum: f64 = months.iter().map(|m| m.r_sum).sum();
    // Deux regroupements de n additions : borne prudente 4*n*epsilon*somme(|R|).
    // Les effectifs sont exacts ; aucune tolérance ne permet de supprimer un R.
    let tolerance = 4.0 * f64::EPSILON * f64::from(count.max(1)) * absolute_sum;
    if month_count != count || (month_sum - total).abs() > tolerance {
        return Err(technical(
            "la réduction mensuelle ne conserve pas les R du journal",
        ));
    }
    if count == 0 {
        return Ok(Reduction {
            score: None,
            reason: Some(UnavailableReason::NoRetainedTrades),
            months,
        });
    }
    Ok(Reduction {
        score: Some(total / f64::from(count)),
        reason: None,
        months,
    })
}
fn unavailable(
    replication: u32,
    reason: UnavailableReason,
    attempts: u32,
    rejections: Vec<AttemptRejection>,
) -> CellSummary {
    CellSummary {
        replication,
        accepted_attempt: None,
        attempts,
        availability: CellAvailability::Unavailable,
        reason: Some(reason),
        score: None,
        months: Vec::new(),
        quota_checks: Vec::new(),
        quantity_changed: false,
        replay_trace_hash: String::new(),
        rejections,
    }
}
fn trace_hash(
    source: &PreparedSource<'_, '_>,
    trace: &[NativeTrace],
    calendars: &[Calendar],
) -> Result<String, ControlError> {
    let mut events = Vec::new();
    for t in trace {
        if !t.trade.qty.is_finite() {
            return Err(ControlsError::NonFinite.into());
        }
        events.push((t.leg, fingerprint(source, t)?, t.trade.qty));
    }
    let ids: Vec<_> = calendars.iter().map(|c| &c.interval_ids).collect();
    crate::model::hash::hash_json(&(ids, events)).map_err(|e| ControlError::Engine(e.into()))
}
#[allow(clippy::too_many_arguments)]
fn replay_cell(
    source: &PreparedSource<'_, '_>,
    mode: ControlMode,
    catalogues: &[PairCatalogue],
    tables: &[Arc<CalendarTable>],
    replication: u32,
    options: &ControlOptions,
    work: &mut EngineWork,
    coverage: &[MonthCoverage],
) -> Result<CellSummary, ControlError> {
    work.begin_replication();
    let mut attempts = 0;
    let mut rejections = Vec::new();
    loop {
        if let Err(reason) = work.charge_attempt(&options.budgets) {
            return Ok(unavailable(replication, reason, attempts, rejections));
        }
        let attempt = attempts;
        attempts += 1;
        let mut calendars = Vec::new();
        let mut selections = Vec::new();
        for (cat, table) in catalogues.iter().zip(tables) {
            let mut rng = calendar_rng(
                options.seed,
                &source.parent_config_hash,
                mode,
                replication,
                attempt,
                &cat.symbol,
            )
            .map_err(ControlsError::from)?;
            let rank = uniform_below(&mut rng, table.total()).map_err(ControlsError::from)?;
            let calendar = table.unrank(rank).map_err(ControlsError::from)?;
            selections.push(
                calendar
                    .interval_ids
                    .iter()
                    .map(|id| {
                        cat.intervals
                            .iter()
                            .find(|i| i.id == *id)
                            .map(|i| i.decision)
                            .ok_or_else(|| technical("intervalle tiré absent"))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            calendars.push(calendar);
        }
        let run = match portfolio::controls::scheduled(source, selections, work, &options.budgets) {
            Ok(run) => run,
            Err(ControlError::Unavailable(reason)) => {
                return Ok(unavailable(replication, reason, attempts, rejections));
            }
            Err(e) => return Err(e),
        };
        let checks = quota_checks(source, &run.trace)?;
        let mut expected = Vec::new();
        for (leg, calendar) in calendars.iter().enumerate() {
            for id in &calendar.interval_ids {
                let decision = catalogues[leg]
                    .intervals
                    .iter()
                    .find(|i| i.id == *id)
                    .ok_or_else(|| technical("intervalle absent"))?
                    .decision;
                let p = &source.probes[leg][decision];
                expected.push(NativeTrace {
                    leg,
                    trade: p.trade.ok_or_else(|| technical("sonde sans trade"))?,
                    release_ms: p
                        .release_ms
                        .ok_or_else(|| technical("sonde sans libération"))?,
                });
            }
        }
        let mut actual: Vec<_> = run.trace.iter().collect();
        actual.sort_by_key(|t| (t.leg, t.trade.entry_signal_ms));
        expected.sort_by_key(|t| (t.leg, t.trade.entry_signal_ms));
        let mut same = actual.len() == expected.len();
        let mut quantity_changed = false;
        for (a, b) in actual.iter().zip(&expected) {
            same &= a.leg == b.leg && fingerprint(source, a)? == fingerprint(source, b)?;
            quantity_changed |= a.trade.qty.to_bits() != b.trade.qty.to_bits();
        }
        let rejection = if run.occupied {
            Some(ReplayRejection::OccupiedPair)
        } else if !same {
            Some(ReplayRejection::TraceMismatch)
        } else if checks.iter().any(|q| !q.conformant) {
            Some(ReplayRejection::QuotaMismatch)
        } else {
            None
        };
        if let Some(reason) = rejection {
            rejections.push(AttemptRejection { attempt, reason });
            continue;
        }
        // L'acceptation est terminée avant toute lecture de PnL ou de risque R.
        let Reduction {
            score,
            reason,
            months,
        } = reduce(source, &run.trace, coverage)?;
        return Ok(CellSummary {
            replication,
            accepted_attempt: Some(attempt),
            attempts,
            availability: if score.is_some() {
                CellAvailability::Complete
            } else {
                CellAvailability::Unavailable
            },
            reason,
            score,
            months,
            quota_checks: checks,
            quantity_changed,
            replay_trace_hash: trace_hash(source, &run.trace, &calendars)?,
            rejections,
        });
    }
}

fn align_months(months: &mut Vec<MonthMoment>, coverage: &[MonthCoverage]) {
    for m in coverage {
        if !months
            .iter()
            .any(|v| v.year == m.year && v.month == m.month)
        {
            months.push(MonthMoment {
                year: m.year,
                month: m.month,
                r_sum: 0.0,
                count: 0,
            });
        }
    }
    months.sort_by_key(|m| (m.year, m.month));
}
struct AlignedCells {
    original: Arc<Vec<CellSummary>>,
    aligned: Arc<Vec<CellSummary>>,
}
/// Union des seuls mois réellement représentés par la source et les réalisations acceptées.
/// Les aliases reçoivent le même Arc aligné, sans dupliquer leurs cellules physiques.
fn align_axis(
    coverage: &mut Vec<MonthCoverage>,
    observed: &mut Vec<MonthMoment>,
    observed_reason: Option<UnavailableReason>,
    modes: &mut [ModeSummary],
) {
    let old_len = coverage.len();
    for month in observed.iter().chain(
        modes
            .iter()
            .flat_map(|m| m.cells.iter())
            .flat_map(|c| c.months.iter()),
    ) {
        if !coverage
            .iter()
            .any(|m| m.year == month.year && m.month == month.month)
        {
            coverage.push(MonthCoverage {
                year: month.year,
                month: month.month,
                status: MonthCoverageStatus::Partial,
            });
        }
    }
    coverage.sort_by_key(|m| (m.year, m.month));
    if observed_reason != Some(UnavailableReason::MissingPositiveRisk) {
        align_months(observed, coverage);
    }
    if coverage.len() == old_len {
        return;
    }
    let mut aligned: Vec<AlignedCells> = Vec::new();
    for mode in modes {
        if let Some(cells) = aligned
            .iter()
            .find(|cells| Arc::ptr_eq(&cells.original, &mode.cells))
        {
            mode.cells = cells.aligned.clone();
            continue;
        }
        let old = mode.cells.clone();
        let mut cells = old.as_ref().clone();
        for cell in &mut cells {
            if cell.accepted_attempt.is_some()
                && cell.reason != Some(UnavailableReason::MissingPositiveRisk)
            {
                align_months(&mut cell.months, coverage);
            }
        }
        let ready = Arc::new(cells);
        aligned.push(AlignedCells {
            original: old,
            aligned: ready.clone(),
        });
        mode.cells = ready;
    }
}

/// À appeler dans la continuation de préparation. Aucun emprunt du marché ne sort.
pub fn replay_prepared(
    batch: &mut PreparedBatch<'_, '_>,
) -> Result<Vec<SourceSummary>, ControlError> {
    let mut parents = BTreeSet::new();
    if batch
        .sources
        .iter()
        .any(|s| !parents.insert(&s.parent_config_hash))
    {
        return Err(technical(
            "les parents du panneau doivent avoir des configurations distinctes",
        ));
    }
    let mut planning: Vec<(Arc<Vec<Vec<PreparedSlot>>>, PlanningWork)> = Vec::new();
    let mut summaries = Vec::new();
    for source in &batch.sources {
        if quota_checks(source, &source.observed_trace)?
            .iter()
            .any(|q| !q.conformant)
        {
            return Err(technical(
                "les quotas fournis diffèrent des lots Observed réels",
            ));
        }
        let mut coverage = coverage(source)?;
        let Reduction {
            score: observed_score,
            reason: observed_reason,
            months: mut observed_months,
        } = reduce(source, &source.observed_trace, &coverage)?;
        let group = if let Some(i) = planning
            .iter()
            .position(|(slots, _)| Arc::ptr_eq(slots, &source.slots))
        {
            i
        } else {
            planning.push((source.slots.clone(), PlanningWork::default()));
            planning.len() - 1
        };
        let mut modes = Vec::<ModeSummary>::new();
        for prepared in &source.modes {
            if let Some(alias) = modes
                .iter()
                .find(|m| Arc::ptr_eq(&m.catalogues, &prepared.catalogues))
            {
                modes.push(ModeSummary {
                    mode: prepared.mode,
                    alias_of: prepared.alias_of,
                    catalogues: prepared.catalogues.clone(),
                    pair_supports: alias.pair_supports.clone(),
                    planning_reason: alias.planning_reason,
                    cells: alias.cells.clone(),
                });
                continue;
            }
            let support = source_in_support(source, &prepared.catalogues)?;
            batch.observer.phase_started(ControlPhase::Planning);
            let before = planning[group].1.clone();
            let mut pair_supports: Vec<_> = prepared
                .catalogues
                .iter()
                .map(|cat| PairSupportSummary {
                    symbol: cat.symbol.clone(),
                    cardinality: None,
                })
                .collect();
            let tables = (|| -> Result<Vec<Arc<CalendarTable>>, PlanningFailure> {
                let mut tables = Vec::new();
                for (cat, pair_support) in prepared.catalogues.iter().zip(&mut pair_supports) {
                    match CalendarTable::build_with_work(
                        cat,
                        &source.runtime.quotas[&cat.symbol],
                        &batch.options.budgets,
                        &mut planning[group].1,
                    ) {
                        Ok(table) => {
                            pair_support.cardinality = Some(table.total().to_str_radix(10));
                            tables.push(Arc::new(table));
                        }
                        Err(failure) => {
                            if failure == PlanningFailure::EmptySupport {
                                pair_support.cardinality = Some("0".into());
                            }
                            return Err(failure);
                        }
                    }
                }
                Ok(tables)
            })();
            let planning_reason = match &tables {
                Err(PlanningFailure::EmptySupport) => Some(UnavailableReason::EmptySupport),
                Err(PlanningFailure::Budget { .. }) => Some(UnavailableReason::PlanningBudget),
                _ => None,
            };
            // Les deux cumuls sont vérifiés avant mutation : un diagnostic débordé
            // reste une erreur technique, sans demi-réservation ni saturation.
            let diagnostics = (|| -> Result<(), ControlError> {
                let cells = batch
                    .work
                    .planning_cells
                    .checked_add(planning[group].1.cells - before.cells)
                    .ok_or_else(|| technical("cumul des cellules de planification débordé"))?;
                let bytes = batch
                    .work
                    .planning_logical_bytes
                    .checked_add(planning[group].1.logical_bytes - before.logical_bytes)
                    .ok_or_else(|| technical("cumul des octets de planification débordé"))?;
                batch.work.planning_cells = cells;
                batch.work.planning_logical_bytes = bytes;
                Ok(())
            })();
            batch.observer.phase_finished(ControlPhase::Planning);
            diagnostics?;
            let failure = if !support {
                Some(UnavailableReason::ObservedOutsideSupport)
            } else {
                match &tables {
                    Err(PlanningFailure::EmptySupport) => Some(UnavailableReason::EmptySupport),
                    Err(PlanningFailure::Budget { .. }) => Some(UnavailableReason::PlanningBudget),
                    Err(e) => return Err(ControlsError::from(e.clone()).into()),
                    Ok(_) => None,
                }
            };
            let cells = if let Some(reason) = failure {
                (0..batch.options.replications)
                    .map(|k| unavailable(k, reason, 0, Vec::new()))
                    .collect()
            } else {
                let tables = tables.map_err(ControlsError::from)?;
                batch.observer.phase_started(ControlPhase::Replay);
                let result = (0..batch.options.replications)
                    .map(|k| {
                        replay_cell(
                            source,
                            prepared.alias_of.unwrap_or(prepared.mode),
                            &prepared.catalogues,
                            &tables,
                            k,
                            batch.options,
                            batch.work,
                            &coverage,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>();
                batch.observer.phase_finished(ControlPhase::Replay);
                result?
            };
            modes.push(ModeSummary {
                mode: prepared.mode,
                alias_of: prepared.alias_of,
                catalogues: prepared.catalogues.clone(),
                pair_supports: Arc::new(pair_supports),
                planning_reason,
                cells: Arc::new(cells),
            });
        }
        align_axis(
            &mut coverage,
            &mut observed_months,
            observed_reason,
            &mut modes,
        );
        summaries.push(SourceSummary {
            quotas: source.runtime.quotas.clone(),
            observed_score,
            observed_reason,
            observed_months,
            month_coverage: coverage,
            modes,
            planning_work: planning[group].1.clone(),
        });
    }
    Ok(summaries)
}
