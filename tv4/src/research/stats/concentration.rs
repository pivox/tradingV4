//! Concentration du PnL net en quote ; groupement par mois de sortie UTC.
use super::types::{Availability, Metric, StatsError, TradeRecord, finite, sorted_trades};
use crate::store::time::{MS_PER_DAY, civil_from_days};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TradeIdentity {
    pub symbol: String,
    pub lot_id: u64,
    pub entry_ms: i64,
    pub exit_ms: i64,
    pub row: usize,
}

impl From<&TradeRecord> for TradeIdentity {
    fn from(t: &TradeRecord) -> Self {
        Self {
            symbol: t.symbol.clone(),
            lot_id: t.lot_id,
            entry_ms: t.entry_ms,
            exit_ms: t.exit_ms,
            row: t.row,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupRemoval {
    pub id: String,
    pub pnl: f64,
    pub pnl_without: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TradeRemoval {
    pub count: usize,
    pub removed: Vec<TradeIdentity>,
    pub pnl_without: Metric,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConcentrationStats {
    pub availability: Availability,
    pub n_trades: usize,
    pub total_pnl: f64,
    pub best_month: Option<GroupRemoval>,
    pub best_pair: Option<GroupRemoval>,
    pub without_top: Vec<TradeRemoval>,
    pub top3_share: Metric,
    pub best_month_positive_share: Metric,
}

fn month_id(trade: &TradeRecord) -> String {
    let (year, month, _) = civil_from_days(trade.exit_ms.div_euclid(MS_PER_DAY));
    format!("{year:04}-{month:02}")
}

fn best_group(
    groups: &BTreeMap<String, f64>,
    trades: &[&TradeRecord],
    belongs_to: impl Fn(&TradeRecord, &str) -> bool,
) -> Result<Option<GroupRemoval>, StatsError> {
    let mut best: Option<(&String, f64)> = None;
    for (id, &pnl) in groups {
        if best.is_none_or(|(_, previous)| pnl > previous) {
            best = Some((id, pnl));
        }
    }
    best.map(|(id, pnl)| {
        // Ne pas soustraire deux agrégats arrondis : un petit reste signé serait perdu.
        let mut pnl_without = 0.;
        for trade in trades {
            if !belongs_to(trade, id) {
                pnl_without = finite(pnl_without + trade.pnl_net)?;
            }
        }
        Ok(GroupRemoval {
            id: id.clone(),
            pnl,
            pnl_without,
        })
    })
    .transpose()
}

pub fn concentration(trades: &[TradeRecord]) -> Result<ConcentrationStats, StatsError> {
    let sorted = sorted_trades(trades)?;
    let n = sorted.len();
    let mut total = 0.;
    let mut months = BTreeMap::new();
    let mut pairs = BTreeMap::new();
    for t in &sorted {
        total = finite(total + t.pnl_net)?;
        let month_pnl = months.entry(month_id(t)).or_insert(0.);
        *month_pnl = finite(*month_pnl + t.pnl_net)?;
        let pair_pnl = pairs.entry(t.symbol.clone()).or_insert(0.);
        *pair_pnl = finite(*pair_pnl + t.pnl_net)?;
    }
    let best_month = best_group(&months, &sorted, |trade, id| month_id(trade) == id)?;
    let best_pair = best_group(&pairs, &sorted, |trade, id| trade.symbol == id)?;
    let mut ranked = sorted.clone();
    ranked.sort_by(|a, b| {
        // +0 et -0 représentent le même PnL : ils suivent aussi le départage lexical.
        let pnl_order = if a.pnl_net == b.pnl_net {
            std::cmp::Ordering::Equal
        } else {
            b.pnl_net.total_cmp(&a.pnl_net)
        };
        pnl_order.then_with(|| {
            (&a.symbol, a.lot_id, a.entry_ms, a.exit_ms, a.row)
                .cmp(&(&b.symbol, b.lot_id, b.entry_ms, b.exit_ms, b.row))
        })
    });
    let mut without_top = Vec::new();
    for count in [1, 3, 5, 10] {
        let (removed, pnl_without) = if n > count {
            let removed: Vec<_> = ranked[..count]
                .iter()
                .map(|t| TradeIdentity::from(*t))
                .collect();
            let mut remaining = 0.;
            // Même ordre séquentiel que les autres agrégats, après retrait des identités classées.
            for t in &sorted {
                if !ranked[..count]
                    .iter()
                    .any(|removed| std::ptr::eq(*removed, *t))
                {
                    remaining = finite(remaining + t.pnl_net)?;
                }
            }
            (removed, Metric::available(remaining)?)
        } else {
            (
                vec![],
                Metric::unavailable(
                    "effectif insuffisant pour retirer N meilleurs trades et conserver un trade",
                ),
            )
        };
        without_top.push(TradeRemoval {
            count,
            removed,
            pnl_without,
        });
    }
    let top3_share = if n >= 3 && total > 0. {
        let mut top = 0.;
        for t in &ranked[..3] {
            top = finite(top + t.pnl_net)?;
        }
        Metric::available(top / total)?
    } else {
        Metric::unavailable("trois trades et PnL net total strictement positif requis")
    };
    let mut positive_months = 0.;
    for &pnl in months.values().filter(|&&p| p > 0.) {
        positive_months = finite(positive_months + pnl)?;
    }
    let best_month_positive_share = if let Some(best) = &best_month
        && positive_months > 0.
    {
        Metric::available(best.pnl / positive_months)?
    } else {
        Metric::unavailable("aucun mois au PnL strictement positif")
    };
    let availability = if n == 0 {
        Availability::Unavailable
    } else if without_top.iter().all(|r| r.pnl_without.value.is_some())
        && top3_share.value.is_some()
        && best_month_positive_share.value.is_some()
    {
        Availability::Complete
    } else {
        Availability::Partial
    };
    Ok(ConcentrationStats {
        availability,
        n_trades: n,
        total_pnl: total,
        best_month,
        best_pair,
        without_top,
        top3_share,
        best_month_positive_share,
    })
}
