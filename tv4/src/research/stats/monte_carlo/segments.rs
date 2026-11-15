//! Partition circulaire non chevauchante ; chaque intervalle observé apparaît une fois.
use super::{BLOCK_TRADES, DAY_MS, TradeClosure};
use crate::research::stats::types::{
    MarkPoint, StatsError, StatsSizing, TradeRecord, Window, finite,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Step {
    pub offset_ms: i64,
    pub delta: f64,
    // Absent en Research (inutilisé) ; en Compounding, absent uniquement si base source=0.
    pub factor: Option<f64>,
}
pub(super) struct Segment {
    pub duration_ms: i64,
    pub steps: Vec<Step>,
}
pub(super) struct Prepared {
    pub sizes: Vec<usize>,
    pub durations: Vec<i64>,
    pub segments: Vec<Segment>,
    pub unavailable: Option<String>,
}
fn invalid(message: &str) -> StatsError {
    StatsError::InvalidInput(message.into())
}

pub(super) fn prepare(
    trades: &[TradeRecord],
    closures: &[TradeClosure],
    marks: &[MarkPoint],
    window: Window,
    capital: f64,
    sizing: StatsSizing,
) -> Result<Prepared, StatsError> {
    let length = window
        .end_ms
        .checked_sub(window.start_ms)
        .ok_or_else(|| invalid("durée hors plage"))?;
    let mut rows = BTreeMap::new();
    for t in trades {
        if rows.insert(t.row, t).is_some() {
            return Err(invalid("identifiant de ligne dupliqué"));
        }
        if t.entry_ms > t.exit_ms || t.exit_ms < window.start_ms || t.exit_ms > window.end_ms {
            return Err(invalid("horodatages de trade incohérents avec la fenêtre"));
        }
    }
    let mut seen = BTreeSet::new();
    let mut groups = BTreeMap::<i64, usize>::new();
    for c in closures {
        let trade = rows
            .get(&c.row)
            .ok_or_else(|| invalid("clôture d'une ligne inconnue"))?;
        if !seen.insert(c.row) {
            return Err(invalid("clôture de ligne dupliquée"));
        }
        if c.at_ms < trade.exit_ms || c.at_ms < window.start_ms || c.at_ms > window.end_ms {
            return Err(invalid("instant de réalisation incohérent"));
        }
        if trade.included {
            *groups.entry(c.at_ms).or_default() += 1;
        }
    }
    let mut points = BTreeMap::new();
    for p in marks {
        if !p.equity.is_finite()
            || p.equity < 0.
            || p.at_ms < window.start_ms
            || p.at_ms > window.end_ms
        {
            return Err(invalid("marquage non fini, négatif ou hors fenêtre"));
        }
        if p.day_close && p.at_ms.rem_euclid(DAY_MS) != 0 {
            return Err(invalid("clôture journalière hors frontière UTC"));
        }
        if points.insert(p.at_ms, p).is_some() {
            return Err(invalid("instant de marquage dupliqué"));
        }
    }
    if points
        .get(&window.start_ms)
        .is_some_and(|p| p.equity != capital)
    {
        return Err(invalid("marquage initial différent du capital"));
    }
    let mut bankrupt = false;
    for p in points.values() {
        if bankrupt && p.equity > 0. {
            return Err(invalid("equity source positive après faillite"));
        }
        bankrupt |= p.equity == 0.;
    }
    let mut blocks: Vec<(i64, usize)> = Vec::new();
    let mut count = 0;
    let mut last = None;
    for (&at, &n) in &groups {
        count += n;
        last = Some(at);
        if count >= BLOCK_TRADES {
            blocks.push((at, count));
            count = 0;
        }
    }
    if let Some(at) = last
        && (count > 0 || blocks.last().is_some_and(|b| b.0 != at))
    {
        if let Some(block) = blocks.last_mut() {
            block.0 = at;
            block.1 += count;
        } else {
            blocks.push((at, count));
        }
    }
    let mut prepared = Prepared {
        sizes: blocks.iter().map(|b| b.1).collect(),
        durations: vec![],
        segments: vec![],
        unavailable: None,
    };
    let missing_closure = trades.iter().any(|t| t.included && !seen.contains(&t.row));
    let first_day = (window.start_ms as i128).div_euclid(DAY_MS as i128) + 1;
    let last_day = (window.end_ms as i128).div_euclid(DAY_MS as i128);
    let expected_days = (last_day - first_day + 1).max(0);
    let actual_days = points
        .values()
        .filter(|p| p.day_close && p.at_ms > window.start_ms)
        .count() as i128;
    if missing_closure || groups.is_empty() {
        prepared.unavailable =
            Some("correspondance des clôtures conservées absente ou incomplète".into());
    } else if !points.contains_key(&window.start_ms)
        || !points.contains_key(&window.end_ms)
        || actual_days != expected_days
        || groups.keys().any(|t| !points.contains_key(t))
    {
        prepared.unavailable =
            Some("marquage initial, final, quotidien ou de frontière exact absent".into());
    }
    let Some(&(end, _)) = blocks.last() else {
        return Ok(prepared);
    };
    let initial_left = end
        .checked_sub(length)
        .ok_or_else(|| invalid("frontière circulaire hors plage temporelle"))?;
    let mut left = initial_left;
    for (i, &(right, _)) in blocks.iter().enumerate() {
        let duration = right
            .checked_sub(left)
            .ok_or_else(|| invalid("durée de segment hors plage"))?;
        prepared.durations.push(duration);
        if duration <= 0 {
            prepared.unavailable = Some("segment de durée nulle non résoluble".into());
        }
        if prepared.unavailable.is_none() {
            let mut steps = Vec::new();
            if i == 0 {
                let tail_start = end;
                append(&mut steps, &points, tail_start, window.end_ms, 0, sizing)?;
                append(
                    &mut steps,
                    &points,
                    window.start_ms,
                    right,
                    window.end_ms - tail_start,
                    sizing,
                )?;
            } else {
                append(&mut steps, &points, left, right, 0, sizing)?;
            }
            prepared.segments.push(Segment {
                duration_ms: duration,
                steps,
            });
        }
        left = right;
    }
    Ok(prepared)
}

fn append(
    steps: &mut Vec<Step>,
    points: &BTreeMap<i64, &MarkPoint>,
    left: i64,
    right: i64,
    offset: i64,
    sizing: StatsSizing,
) -> Result<(), StatsError> {
    let mut previous = points
        .get(&left)
        .ok_or_else(|| invalid("frontière exacte absente"))?
        .equity;
    for (&at, p) in points.range((
        std::ops::Bound::Excluded(left),
        std::ops::Bound::Included(right),
    )) {
        let delta = finite(p.equity - previous)?;
        let factor = match sizing {
            StatsSizing::Research => None,
            StatsSizing::Compounding if previous > 0. => {
                // Un ratio non fini est une erreur, pas un 0/0 non identifiable.
                Some(finite(p.equity / previous)?)
            }
            StatsSizing::Compounding => None,
        };
        steps.push(Step {
            offset_ms: offset + (at - left),
            delta,
            factor,
        });
        previous = p.equity;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trade(row: usize) -> TradeRecord {
        TradeRecord {
            row,
            lot_id: row as u64,
            symbol: "BTC-USDC".into(),
            entry_ms: 0,
            exit_ms: 1,
            qty: 1.,
            entry_price: 100.,
            exit_price: 100.,
            entry_fee: 0.,
            exit_fee: 0.,
            pnl_net: 0.,
            r_multiple: Some(1.),
            included: true,
        }
    }
    #[test]
    fn partition_avec_ties_et_reliquat_couvre_une_fois_chaque_intervalle() {
        let trades: Vec<_> = (0..41).map(trade).collect();
        let mut closures: Vec<_> = (0..41)
            .map(|row| TradeClosure {
                row,
                at_ms: match row {
                    0..20 => 10 * DAY_MS,
                    20 => 12 * DAY_MS,
                    21 => 14 * DAY_MS,
                    _ => 20 * DAY_MS,
                },
            })
            .collect();
        closures.reverse();
        let marks: Vec<_> = (0..=40)
            .map(|d| MarkPoint {
                at_ms: d * DAY_MS,
                equity: 100. + d as f64,
                day_close: d > 0,
            })
            .collect();
        let p = prepare(
            &trades,
            &closures,
            &marks,
            Window {
                start_ms: 0,
                end_ms: 40 * DAY_MS,
            },
            100.,
            StatsSizing::Compounding,
        )
        .expect("partition");
        assert_eq!(p.sizes, vec![20, 21]);
        assert_eq!(p.durations, vec![30 * DAY_MS, 10 * DAY_MS]);
        assert_eq!(p.segments.iter().map(|s| s.steps.len()).sum::<usize>(), 40);
        assert!(
            p.segments
                .iter()
                .flat_map(|s| &s.steps)
                .all(|s| s.delta == 1.)
        );
        let first = &p.segments[0];
        assert_eq!(
            first.steps.iter().map(|s| s.offset_ms).collect::<Vec<_>>(),
            (1..=30).map(|d| d * DAY_MS).collect::<Vec<_>>()
        );
        // Tail E120→E140, puis head E100→E110 : pas de saut E140→E100.
        assert_eq!(first.steps.iter().map(|s| s.delta).sum::<f64>(), 30.);
    }
    #[test]
    fn oracle_90_jours_wrap_liquidation_et_facteurs_sans_reset() {
        let trades: Vec<_> = (0..44).map(trade).collect();
        let closures: Vec<_> = (0..44)
            .map(|row| TradeClosure {
                row,
                at_ms: match row {
                    0..20 => 10 * DAY_MS,
                    20 => 20 * DAY_MS,
                    21 => 30 * DAY_MS,
                    22..42 => 40 * DAY_MS,
                    42 => 50 * DAY_MS,
                    _ => 60 * DAY_MS,
                },
            })
            .collect();
        let equity = |day| match day {
            0..10 => 100.,
            10..20 => 120.,
            20..30 => 119.,
            30..40 => 118.,
            40..50 => 138.,
            50..60 => 137.,
            60..90 => 136.,
            _ => 135.,
        };
        let marks: Vec<_> = (0..=90)
            .map(|d| MarkPoint {
                at_ms: d * DAY_MS,
                equity: equity(d),
                day_close: d > 0,
            })
            .collect();
        let p = prepare(
            &trades,
            &closures,
            &marks,
            Window {
                start_ms: 0,
                end_ms: 90 * DAY_MS,
            },
            100.,
            StatsSizing::Compounding,
        )
        .expect("oracle 90j");
        assert_eq!(p.sizes, vec![20, 24]);
        assert_eq!(p.durations, vec![40 * DAY_MS, 50 * DAY_MS]);
        let deltas: Vec<_> = p
            .segments
            .iter()
            .map(|s| s.steps.iter().map(|p| p.delta).sum::<f64>())
            .collect();
        assert_eq!(deltas, vec![19., 16.]);
        let factors: Vec<_> = p
            .segments
            .iter()
            .map(|s| {
                s.steps
                    .iter()
                    .map(|p| p.factor.expect("source positive"))
                    .product::<f64>()
            })
            .collect();
        assert!((factors[0] - (135. / 136.) * (120. / 100.)).abs() < 1e-14);
        assert!((factors[1] - 136. / 120.).abs() < 1e-14);
        assert!((factors.iter().product::<f64>() - 1.35).abs() < 1e-14);
    }
}
