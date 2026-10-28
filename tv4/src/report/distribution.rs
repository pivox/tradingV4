//! Bloc `distribution` de `metrics.json` : ce que la fiche de run affiche (docs/05 §4) sans que le
//! control plane recalcule quoi que ce soit.
//!
//! Sommes dans l'ordre des trades, tris par `f64::total_cmp`, aucune fonction libm.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::sim::Trade;
use crate::store::time::format_rfc3339_utc;

use super::ReportError;
use super::writers::{exit_type, r_multiple};

/// Largeur d'un seau de l'histogramme des R.
const BIN_WIDTH: f64 = 0.5;
/// Borne basse de l'histogramme des R.
const HIST_MIN: f64 = -5.0;
/// Borne haute (exclue) de l'histogramme des R.
const HIST_MAX: f64 = 10.0;
/// Nombre de seaux : `(max − min) / largeur`.
const HIST_BINS: usize = 30;
/// Décalage de l'indice : `floor(r × 2) + 10` (`r × 2` est exact).
const HIST_OFFSET: i64 = 10;
/// Nombres de meilleurs trades retirés pour `pnl_without_best`.
const WITHOUT_BEST: [usize; 4] = [1, 3, 5, 10];

/// Histogramme des multiples de risque sur `[-5, 10[` par seaux de 0,5.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Histogram {
    pub bin_width: f64,
    pub min: f64,
    pub max: f64,
    pub counts: Vec<u64>,
    pub underflow: u64,
    pub overflow: u64,
}

/// PnL net du run sans ses `n` meilleurs trades.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WithoutBest {
    pub n: u64,
    pub pnl_net: Option<f64>,
}

/// Trades regroupés par année UTC de sortie.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct YearRow {
    pub year: i32,
    pub n_trades: u64,
    pub n_wins: u64,
    pub pnl_net: f64,
}

/// Trades regroupés par type de sortie.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExitTypeRow {
    pub exit_type: &'static str,
    pub n_trades: u64,
    pub pnl_net: f64,
}

/// Contenu du bloc `distribution`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Distribution {
    pub pnl_net_mean: Option<f64>,
    pub pnl_net_median: Option<f64>,
    pub r_mean: Option<f64>,
    pub r_median: Option<f64>,
    pub histogram_r: Option<Histogram>,
    pub pnl_without_best: Vec<WithoutBest>,
    pub by_year: Vec<YearRow>,
    pub by_exit_type: Vec<ExitTypeRow>,
}

/// Médiane d'une liste : valeur centrale (impair) ou moyenne des deux centrales (pair) ; `None` si vide.
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n % 2 == 1 {
        sorted.get(n / 2).copied()
    } else {
        let a = sorted.get(n / 2 - 1)?;
        let b = sorted.get(n / 2)?;
        Some((a + b) / 2.0)
    }
}

/// Histogramme des R ; `None` sans aucun R.
pub fn histogram(rs: &[f64]) -> Option<Histogram> {
    if rs.is_empty() {
        return None;
    }
    let mut counts = vec![0u64; HIST_BINS];
    let (mut underflow, mut overflow) = (0u64, 0u64);
    for &r in rs {
        let k = (r * 2.0).floor() as i64 + HIST_OFFSET;
        match usize::try_from(k) {
            Err(_) => underflow += 1,
            Ok(k) => match counts.get_mut(k) {
                Some(slot) => *slot += 1,
                None => overflow += 1,
            },
        }
    }
    Some(Histogram {
        bin_width: BIN_WIDTH,
        min: HIST_MIN,
        max: HIST_MAX,
        counts,
        underflow,
        overflow,
    })
}

/// Somme du PnL net des trades privés de leurs `n` meilleurs (tri décroissant, égalités par index
/// croissant), additionnés dans l'ordre d'origine ; `None` si `n_trades ≤ n`.
pub fn pnl_without_best(trades: &[Trade], n: usize) -> Option<f64> {
    if trades.len() <= n {
        return None;
    }
    let mut order: Vec<usize> = (0..trades.len()).collect();
    order.sort_by(|&a, &b| {
        let pa = trades.get(a).map_or(0.0, |t| t.pnl_net);
        let pb = trades.get(b).map_or(0.0, |t| t.pnl_net);
        pb.total_cmp(&pa).then(a.cmp(&b))
    });
    let mut removed = vec![false; trades.len()];
    for &index in order.iter().take(n) {
        if let Some(flag) = removed.get_mut(index) {
            *flag = true;
        }
    }
    let mut sum = 0.0;
    for (trade, gone) in trades.iter().zip(&removed) {
        if !gone {
            sum += trade.pnl_net;
        }
    }
    Some(sum)
}

fn year_of(ts_ms: i64) -> Result<i32, ReportError> {
    let out_of_range = || ReportError::Timestamp { ts_ms };
    let text = format_rfc3339_utc(ts_ms).map_err(|_| out_of_range())?;
    text.get(..4)
        .and_then(|year| year.parse::<i32>().ok())
        .ok_or_else(out_of_range)
}

/// Calcule le bloc. `expectancy_net` et `expectancy_r` sont ceux de `metrics.json` (moyennes déjà
/// calculées dans l'ordre des trades).
pub fn compute(
    trades: &[Trade],
    expectancy_net: Option<f64>,
    expectancy_r: Option<f64>,
) -> Result<Distribution, ReportError> {
    let pnls: Vec<f64> = trades.iter().map(|t| t.pnl_net).collect();
    let rs: Vec<f64> = trades.iter().filter_map(r_multiple).collect();

    let mut years: BTreeMap<i32, (u64, u64, f64)> = BTreeMap::new();
    let mut kinds: BTreeMap<&'static str, (u64, f64)> = BTreeMap::new();
    for t in trades {
        let year = years.entry(year_of(t.exit_ms)?).or_insert((0, 0, 0.0));
        year.0 += 1;
        if t.pnl_net > 0.0 {
            year.1 += 1;
        }
        year.2 += t.pnl_net;
        let kind = kinds.entry(exit_type(t)).or_insert((0, 0.0));
        kind.0 += 1;
        kind.1 += t.pnl_net;
    }

    Ok(Distribution {
        pnl_net_mean: expectancy_net,
        pnl_net_median: median(&pnls),
        r_mean: expectancy_r,
        r_median: median(&rs),
        histogram_r: histogram(&rs),
        pnl_without_best: WITHOUT_BEST
            .iter()
            .map(|&n| WithoutBest {
                n: n as u64,
                pnl_net: pnl_without_best(trades, n),
            })
            .collect(),
        by_year: years
            .into_iter()
            .map(|(year, (n_trades, n_wins, pnl_net))| YearRow {
                year,
                n_trades,
                n_wins,
                pnl_net,
            })
            .collect(),
        by_exit_type: kinds
            .into_iter()
            .map(|(exit_type, (n_trades, pnl_net))| ExitTypeRow {
                exit_type,
                n_trades,
                pnl_net,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{ExitReason, Liquidity};

    /// Trade minimal : PnL, instant de sortie, motif ; stop et quantité pour fixer un risque connu.
    fn trade(pnl: f64, exit_ms: i64, reason: ExitReason, risk: Option<f64>) -> Trade {
        Trade {
            lot_id: 0,
            entry_signal_ms: 0,
            entry_order_ms: 0,
            entry_ms: 0,
            entry_ref_price: 100.0,
            entry_price: 100.0,
            entry_fee: 0.0,
            entry_slippage: 0.0,
            entry_liquidity: Liquidity::Taker,
            exit_signal_ms: 0,
            exit_order_ms: 0,
            exit_ms,
            exit_ref_price: 100.0,
            exit_price: 100.0,
            exit_fee: 0.0,
            exit_slippage: 0.0,
            exit_liquidity: Liquidity::Taker,
            qty: 1.0,
            pnl_net: pnl,
            mfe: 0.0,
            mae: 0.0,
            stop_at_exit: None,
            // Risque `qty × (100 − stop)` = `risk`.
            stop_at_entry: risk.map(|r| 100.0 - r),
            stop_moved: false,
            capped_by_cash: false,
            exit_reason: reason,
            forced_close: reason == ExitReason::ForcedClose,
            entry_tag: "t",
            exit_tag: "t",
        }
    }

    const Y2021: i64 = 1_609_459_200_000; // 2021-01-01T00:00:00Z
    const Y2022: i64 = 1_640_995_200_000; // 2022-01-01T00:00:00Z

    #[test]
    fn mediane_impaire_paire_et_vide() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(&[5.0]), Some(5.0));
    }

    #[test]
    fn histogramme_bornes_underflow_et_overflow() {
        assert_eq!(histogram(&[]), None);
        let h = histogram(&[
            -5.0, -4.75, -5.01, 0.0, 0.49, 0.5, 9.99, 10.0, 100.0, -100.0,
        ])
        .unwrap();
        assert_eq!((h.bin_width, h.min, h.max), (0.5, -5.0, 10.0));
        assert_eq!(h.counts.len(), 30);
        // -5.0 et -4.75 : seau 0 ; 0.0 et 0.49 : seau 10 ; 0.5 : seau 11 ; 9.99 : seau 29.
        assert_eq!(h.counts[0], 2);
        assert_eq!(h.counts[10], 2);
        assert_eq!(h.counts[11], 1);
        assert_eq!(h.counts[29], 1);
        assert_eq!(h.underflow, 2); // -5.01 et -100
        assert_eq!(h.overflow, 2); // 10.0 et 100
        let total: u64 = h.counts.iter().sum::<u64>() + h.underflow + h.overflow;
        assert_eq!(total, 10);
    }

    #[test]
    fn pnl_sans_les_meilleurs_avec_egalites() {
        let t: Vec<Trade> = [5.0, -1.0, 5.0, 2.0]
            .iter()
            .map(|&p| trade(p, Y2021, ExitReason::Signal, None))
            .collect();
        // Retirer le meilleur : égalité 5.0 / 5.0, l'index 0 part, reste -1 + 5 + 2 = 6.
        assert_eq!(pnl_without_best(&t, 1), Some(6.0));
        assert_eq!(pnl_without_best(&t, 3), Some(-1.0));
        assert_eq!(pnl_without_best(&t, 4), None);
        assert_eq!(pnl_without_best(&t, 10), None);
        assert_eq!(pnl_without_best(&[], 1), None);
    }

    #[test]
    fn bloc_complet_sur_des_trades_ecrits_a_la_main() {
        let trades = vec![
            trade(2.0, Y2021, ExitReason::Signal, Some(1.0)),
            trade(-1.0, Y2021 + 1000, ExitReason::Stop, Some(1.0)),
            trade(0.0, Y2022, ExitReason::Signal, None),
            trade(3.0, Y2022 + 1000, ExitReason::ForcedClose, Some(2.0)),
        ];
        let d = compute(&trades, Some(1.0), Some(0.5)).unwrap();
        assert_eq!(d.pnl_net_mean, Some(1.0));
        assert_eq!(d.pnl_net_median, Some(1.0));
        assert_eq!(d.r_mean, Some(0.5));
        // R définis : 2.0, -1.0, 1.5 ; médiane 1.5.
        assert_eq!(d.r_median, Some(1.5));
        assert_eq!(
            d.histogram_r.as_ref().map(|h| h.counts.iter().sum::<u64>()),
            Some(3)
        );
        assert_eq!(
            d.by_year,
            vec![
                YearRow {
                    year: 2021,
                    n_trades: 2,
                    n_wins: 1,
                    pnl_net: 1.0
                },
                YearRow {
                    year: 2022,
                    n_trades: 2,
                    n_wins: 1,
                    pnl_net: 3.0
                },
            ]
        );
        assert_eq!(
            d.by_exit_type,
            vec![
                ExitTypeRow {
                    exit_type: "end_of_window",
                    n_trades: 1,
                    pnl_net: 3.0
                },
                ExitTypeRow {
                    exit_type: "stop_loss",
                    n_trades: 1,
                    pnl_net: -1.0
                },
                ExitTypeRow {
                    exit_type: "strategy_exit",
                    n_trades: 2,
                    pnl_net: 2.0
                },
            ]
        );
        let ns: Vec<u64> = d.pnl_without_best.iter().map(|w| w.n).collect();
        assert_eq!(ns, vec![1, 3, 5, 10]);
        assert_eq!(d.pnl_without_best[0].pnl_net, Some(1.0));
        assert_eq!(d.pnl_without_best[1].pnl_net, Some(-1.0));
        assert_eq!(d.pnl_without_best[2].pnl_net, None);
    }

    #[test]
    fn aucun_trade() {
        let d = compute(&[], None, None).unwrap();
        assert_eq!(d.pnl_net_median, None);
        assert_eq!(d.histogram_r, None);
        assert!(d.by_year.is_empty() && d.by_exit_type.is_empty());
        assert!(d.pnl_without_best.iter().all(|w| w.pnl_net.is_none()));
    }

    #[test]
    fn un_instant_hors_plage_est_une_erreur() {
        let t = vec![trade(1.0, -1, ExitReason::Signal, None)];
        assert!(matches!(
            compute(&t, None, None),
            Err(ReportError::Timestamp { ts_ms: -1 })
        ));
    }
}
