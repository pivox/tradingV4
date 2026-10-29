//! Arithmétique de coût de docs/04 §3.3 (docs/specs/s10.md §12.2) : le bloc `cost_arithmetic` de chaque
//! `metrics.json` s10.
//!
//! Pour chaque trade `i` :
//!
//! * `cost_i = entry_fee + exit_fee + entry_slippage + exit_slippage + entry_spread + exit_spread` ;
//! * `notional_i = qty × entry_ref_price` ;
//! * `gross_i = pnl_net_i + cost_i` ;
//! * `risk_i = risk_quote(i)` (R hors frais, décision S8).
//!
//! Les médianes suivent la convention de S8 ([`distribution::median`]) ; les moyennes sont des sommes
//! dans l'ordre des trades, sans libm. Aucun de ces champs ne change le verdict d'un run. Les portes
//! croisées (baseline **et** adverse, médiane comparée au coût **baseline**) sont dans `profiles.json`
//! ([`profiles`](super::profiles)), pas ici : un run ne connaît que son propre profil.
//!
//! Le seuil coût/stop (`cost_r_limit`) est celui de docs/04 §3.3 : 0,10 R, et 0,15 R pour le profil
//! maker quand le TF de décision est le 4h ou le 1h. Ce sont des seuils de protocole, et non des
//! valeurs de marché.

use serde::Serialize;

use crate::model::costs::CostProfileName;
use crate::model::{Timeframe, Ts};
use crate::sim::Trade;

use super::ReportError;
use super::distribution::median;
use super::metrics::mean;
use super::writers::risk_quote;

/// Seuil coût/stop de docs/04 §3.3, en multiples de R.
pub const COST_R_LIMIT: f64 = 0.10;
/// Seuil coût/stop du profil maker sur 4h ou 1h (docs/04 §3.3, « Conséquence tranchée »).
pub const COST_R_LIMIT_MAKER_INTRADAY: f64 = 0.15;
/// Jours par an du taux annualisé (même convention que `approx`).
const DAYS_PER_YEAR: f64 = 365.25;
/// Millisecondes par jour.
const DAY_MS: Ts = 86_400_000;

/// Somme des coûts par nature, en devise de cotation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CostBreakdown {
    /// `entry_fee + exit_fee`.
    pub fees: f64,
    /// `entry_slippage + exit_slippage`.
    pub slippage: f64,
    /// `entry_spread + exit_spread` : part de spread des fills taker et débits de sélection adverse.
    pub spread: f64,
}

/// Contenu du bloc `cost_arithmetic` (docs/specs/s10.md §12.2). Toutes les clés sont toujours présentes ;
/// une valeur indéfinie vaut `null`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CostArithmetic {
    /// TF de décision de la stratégie (`Strategy::decision_tf`).
    pub decision_tf: Timeframe,
    /// `true` si le TF de décision est le 1d ou le 4h, ou le 1h pour le profil maker.
    pub protocol_tf: bool,
    pub round_trip_cost_mean: Option<f64>,
    pub round_trip_cost_rate_mean: Option<f64>,
    pub round_trip_cost_rate_median: Option<f64>,
    pub cost_breakdown: CostBreakdown,
    pub cost_r_mean: Option<f64>,
    pub cost_r_median: Option<f64>,
    pub trades_with_r: u64,
    pub cost_r_limit: f64,
    /// `cost_r_mean ≤ cost_r_limit` ; `null` sans trade à R défini.
    pub cost_r_pass: Option<bool>,
    pub gross_expectancy: Option<f64>,
    /// `round_trip_cost_mean / gross_expectancy` si l'espérance brute est strictement positive.
    pub cost_to_gross: Option<f64>,
    /// `gross_expectancy > 0` et `round_trip_cost_mean ≤ gross_expectancy / 3`.
    pub cost_third_pass: bool,
    pub gross_median_rate: Option<f64>,
    /// `gross_median_rate ≥ 2 × round_trip_cost_rate_mean` du même profil ; la porte officielle compare
    /// au coût baseline et figure dans `profiles.json`.
    pub gross_median_vs_own_cost_pass: Option<bool>,
    pub trades_per_year: f64,
    pub capital_fraction_mean: Option<f64>,
    /// `trades_per_year × capital_fraction_mean × round_trip_cost_rate_mean`.
    pub hurdle_annual: Option<f64>,
}

/// Ce que le calcul lit d'un run.
#[derive(Clone, Copy, Debug)]
pub struct CostArithmeticInput<'a> {
    pub trades: &'a [Trade],
    pub profile: CostProfileName,
    pub decision_tf: Timeframe,
    /// Durée de la fenêtre jouée en millisecondes (strictement positive).
    pub span_ms: Ts,
    /// `open_time` de chaque minute de la grille, croissant, aligné sur `equity`.
    pub grid_open_ms: &'a [Ts],
    /// Equity du portefeuille à la clôture de chaque minute de la grille.
    pub equity: &'a [f64],
}

/// Seuil coût/stop du profil et du TF de décision.
pub fn cost_r_limit(profile: CostProfileName, decision_tf: Timeframe) -> f64 {
    let maker_intraday =
        profile == CostProfileName::Maker && matches!(decision_tf, Timeframe::H4 | Timeframe::H1);
    if maker_intraday {
        COST_R_LIMIT_MAKER_INTRADAY
    } else {
        COST_R_LIMIT
    }
}

/// `true` si le TF de décision entre dans le périmètre du protocole (docs/04 §3.3).
pub fn protocol_tf(profile: CostProfileName, decision_tf: Timeframe) -> bool {
    match decision_tf {
        Timeframe::D1 | Timeframe::H4 => true,
        Timeframe::H1 => profile == CostProfileName::Maker,
        _ => false,
    }
}

/// Equity de la dernière minute de grille dont l'ouverture est ≤ `t_ms`, ou `None` avant la grille.
fn equity_at(grid_open_ms: &[Ts], equity: &[f64], t_ms: Ts) -> Option<f64> {
    let after = grid_open_ms.partition_point(|&open| open <= t_ms);
    after.checked_sub(1).and_then(|i| equity.get(i)).copied()
}

/// Calcule le bloc `cost_arithmetic` d'un run.
pub fn compute(input: &CostArithmeticInput<'_>) -> Result<CostArithmetic, ReportError> {
    if input.grid_open_ms.len() != input.equity.len() {
        return Err(ReportError::Series {
            detail: format!(
                "{} minutes de grille pour {} valeurs d'equity",
                input.grid_open_ms.len(),
                input.equity.len()
            ),
        });
    }
    if input.span_ms <= 0 {
        return Err(ReportError::Series {
            detail: format!(
                "fenêtre de {} ms : une durée positive est attendue",
                input.span_ms
            ),
        });
    }
    let n = input.trades.len() as u64;

    let mut breakdown = CostBreakdown {
        fees: 0.0,
        slippage: 0.0,
        spread: 0.0,
    };
    let mut cost_sum = 0.0;
    let mut gross_sum = 0.0;
    let mut cost_rates = Vec::with_capacity(input.trades.len());
    let mut gross_rates = Vec::with_capacity(input.trades.len());
    let mut cost_rate_sum = 0.0;
    let mut cost_rs = Vec::new();
    let mut cost_r_sum = 0.0;
    let mut fraction_sum = 0.0;
    let mut fraction_n = 0u64;
    let mut rate_n = 0u64;

    for trade in input.trades {
        let fees = trade.entry_fee + trade.exit_fee;
        let slippage = trade.entry_slippage + trade.exit_slippage;
        let spread = trade.entry_spread + trade.exit_spread;
        let cost = fees + slippage + spread;
        let gross = trade.pnl_net + cost;
        breakdown.fees += fees;
        breakdown.slippage += slippage;
        breakdown.spread += spread;
        cost_sum += cost;
        gross_sum += gross;

        let notional = trade.qty * trade.entry_ref_price;
        if notional > 0.0 {
            let cost_rate = cost / notional;
            cost_rates.push(cost_rate);
            cost_rate_sum += cost_rate;
            gross_rates.push(gross / notional);
            rate_n += 1;
        }
        if let Some(risk) = risk_quote(trade).filter(|risk| *risk > 0.0) {
            let cost_r = cost / risk;
            cost_rs.push(cost_r);
            cost_r_sum += cost_r;
        }
        if let Some(equity) = equity_at(input.grid_open_ms, input.equity, trade.entry_ms)
            && equity > 0.0
        {
            fraction_sum += trade.qty * trade.entry_price / equity;
            fraction_n += 1;
        }
    }

    let n_r = cost_rs.len() as u64;
    let round_trip_cost_mean = mean(cost_sum, n);
    let gross_expectancy = mean(gross_sum, n);
    let round_trip_cost_rate_mean = mean(cost_rate_sum, rate_n);
    let cost_r_mean = mean(cost_r_sum, n_r);
    let limit = cost_r_limit(input.profile, input.decision_tf);
    let gross_median_rate = median(&gross_rates);
    let capital_fraction_mean = mean(fraction_sum, fraction_n);
    let trades_per_year = n as f64 / (input.span_ms as f64 / DAY_MS as f64 / DAYS_PER_YEAR);

    let cost_to_gross = match (round_trip_cost_mean, gross_expectancy) {
        (Some(cost), Some(gross)) if gross > 0.0 => Some(cost / gross),
        _ => None,
    };
    let cost_third_pass = match (round_trip_cost_mean, gross_expectancy) {
        (Some(cost), Some(gross)) => gross > 0.0 && cost <= gross / 3.0,
        _ => false,
    };
    let hurdle_annual = match (capital_fraction_mean, round_trip_cost_rate_mean) {
        (Some(fraction), Some(rate)) => Some(trades_per_year * fraction * rate),
        _ => None,
    };

    Ok(CostArithmetic {
        decision_tf: input.decision_tf,
        protocol_tf: protocol_tf(input.profile, input.decision_tf),
        round_trip_cost_mean,
        round_trip_cost_rate_mean,
        round_trip_cost_rate_median: median(&cost_rates),
        cost_breakdown: breakdown,
        cost_r_mean,
        cost_r_median: median(&cost_rs),
        trades_with_r: n_r,
        cost_r_limit: limit,
        cost_r_pass: cost_r_mean.map(|r| r <= limit),
        gross_expectancy,
        cost_to_gross,
        cost_third_pass,
        gross_median_rate,
        gross_median_vs_own_cost_pass: match (gross_median_rate, round_trip_cost_rate_mean) {
            (Some(gross), Some(rate)) => Some(gross >= 2.0 * rate),
            _ => None,
        },
        trades_per_year,
        capital_fraction_mean,
        hurdle_annual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_seuil_de_0_15_r_ne_vaut_qu_en_maker_sur_4h_ou_1h() {
        use CostProfileName::{Adverse, Baseline, Maker};
        assert_eq!(cost_r_limit(Maker, Timeframe::H4), 0.15);
        assert_eq!(cost_r_limit(Maker, Timeframe::H1), 0.15);
        assert_eq!(cost_r_limit(Maker, Timeframe::D1), 0.10);
        assert_eq!(cost_r_limit(Maker, Timeframe::M15), 0.10);
        assert_eq!(cost_r_limit(Baseline, Timeframe::H4), 0.10);
        assert_eq!(cost_r_limit(Adverse, Timeframe::H1), 0.10);
    }

    #[test]
    fn le_tf_de_protocole_est_1d_4h_ou_1h_en_maker() {
        use CostProfileName::{Baseline, Maker};
        assert!(protocol_tf(Baseline, Timeframe::D1));
        assert!(protocol_tf(Baseline, Timeframe::H4));
        assert!(!protocol_tf(Baseline, Timeframe::H1));
        assert!(protocol_tf(Maker, Timeframe::H1));
        assert!(!protocol_tf(Maker, Timeframe::M15));
    }

    #[test]
    fn l_equity_d_un_instant_est_celle_de_la_derniere_minute_de_grille() {
        let grid = [100, 160, 220];
        let equity = [1.0, 2.0, 3.0];
        assert_eq!(equity_at(&grid, &equity, 99), None);
        assert_eq!(equity_at(&grid, &equity, 100), Some(1.0));
        assert_eq!(equity_at(&grid, &equity, 219), Some(2.0));
        assert_eq!(equity_at(&grid, &equity, 10_000), Some(3.0));
    }

    #[test]
    fn une_grille_et_une_equity_de_longueurs_differentes_sont_refusees() {
        let input = CostArithmeticInput {
            trades: &[],
            profile: CostProfileName::Baseline,
            decision_tf: Timeframe::D1,
            span_ms: DAY_MS,
            grid_open_ms: &[0, 60_000],
            equity: &[1.0],
        };
        assert!(compute(&input).is_err());
    }

    #[test]
    fn sans_trade_tout_est_indefini_sauf_les_booleens_et_le_rythme() {
        let input = CostArithmeticInput {
            trades: &[],
            profile: CostProfileName::Baseline,
            decision_tf: Timeframe::D1,
            span_ms: DAY_MS,
            grid_open_ms: &[0],
            equity: &[300.0],
        };
        let c = compute(&input).unwrap();
        assert_eq!(c.round_trip_cost_mean, None);
        assert_eq!(c.cost_r_mean, None);
        assert_eq!(c.cost_r_pass, None);
        assert_eq!(c.gross_median_rate, None);
        assert_eq!(c.gross_median_vs_own_cost_pass, None);
        assert_eq!(c.hurdle_annual, None);
        assert!(!c.cost_third_pass);
        assert_eq!(c.trades_per_year, 0.0);
        assert_eq!(c.trades_with_r, 0);
    }
}
