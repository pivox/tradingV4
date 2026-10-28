//! Drawdown d'une série d'equity (docs/03-architecture.md §7.3).
//!
//! Le pic part du capital initial et se met à jour par `if e > peak { peak = e }`, jamais par
//! `f64::max` (signe du zéro, docs/03 §7.4). Le point retenu est le premier qui maximise le drawdown
//! (mise à jour sur `>` strict).

use serde::Serialize;

use crate::model::Ts;

/// Drawdown maximal d'une série.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Dd {
    /// `max (1 − e / pic)`, fraction ≥ 0 ; 0.0 si l'equity ne passe jamais sous son pic.
    pub pct: f64,
    /// `pic − e` au point retenu, ≥ 0.
    pub quote: f64,
    /// Instant du pic ; `None` si le pic est le capital initial ou si `pct = 0`.
    pub peak_ms: Option<Ts>,
    /// Instant du creux ; `None` si `pct = 0`.
    pub trough_ms: Option<Ts>,
}

/// Suit le pic courant et le pire drawdown d'une série lue dans l'ordre.
#[derive(Clone, Copy, Debug)]
pub struct DrawdownTracker {
    peak: f64,
    peak_at: Option<Ts>,
    best: Dd,
}

impl DrawdownTracker {
    /// Suivi qui part de `initial_cash` comme pic.
    pub fn new(initial_cash: f64) -> DrawdownTracker {
        DrawdownTracker {
            peak: initial_cash,
            peak_at: None,
            best: Dd {
                pct: 0.0,
                quote: 0.0,
                peak_ms: None,
                trough_ms: None,
            },
        }
    }

    /// Prend l'equity `equity` relevée à l'instant `at_ms` et rend le drawdown courant `1 − e / pic`
    /// (≥ 0, fraction).
    pub fn step(&mut self, equity: f64, at_ms: Ts) -> f64 {
        if equity > self.peak {
            self.peak = equity;
            self.peak_at = Some(at_ms);
        }
        let dd = 1.0 - equity / self.peak;
        if dd > self.best.pct {
            self.best = Dd {
                pct: dd,
                quote: self.peak - equity,
                peak_ms: self.peak_at,
                trough_ms: Some(at_ms),
            };
        }
        dd
    }

    /// Pire drawdown vu jusqu'ici.
    pub fn worst(&self) -> Dd {
        self.best
    }
}

/// Drawdown maximal de la série `equity` relevée aux instants `at_ms` (même longueur).
pub fn max_drawdown(initial_cash: f64, at_ms: &[Ts], equity: &[f64]) -> Dd {
    let mut tracker = DrawdownTracker::new(initial_cash);
    for (&at, &e) in at_ms.iter().zip(equity) {
        tracker.step(e, at);
    }
    tracker.worst()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serie_monotone_croissante_sans_drawdown() {
        let dd = max_drawdown(100.0, &[1, 2, 3], &[101.0, 102.0, 103.0]);
        assert_eq!(
            dd,
            Dd {
                pct: 0.0,
                quote: 0.0,
                peak_ms: None,
                trough_ms: None
            }
        );
    }

    #[test]
    fn creux_unique_depuis_un_pic_de_la_serie() {
        let dd = max_drawdown(100.0, &[10, 20, 30, 40], &[110.0, 120.0, 90.0, 100.0]);
        assert_eq!(dd.pct, 1.0 - 90.0 / 120.0);
        assert_eq!(dd.quote, 30.0);
        assert_eq!(dd.peak_ms, Some(20));
        assert_eq!(dd.trough_ms, Some(30));
    }

    #[test]
    fn le_pic_peut_etre_le_capital_initial() {
        let dd = max_drawdown(100.0, &[10, 20], &[95.0, 98.0]);
        assert_eq!(dd.pct, 1.0 - 95.0 / 100.0);
        assert_eq!(dd.quote, 5.0);
        assert_eq!(dd.peak_ms, None);
        assert_eq!(dd.trough_ms, Some(10));
    }

    #[test]
    fn a_egalite_le_premier_creux_est_retenu() {
        let dd = max_drawdown(100.0, &[10, 20, 30], &[90.0, 95.0, 90.0]);
        assert_eq!(dd.trough_ms, Some(10));
    }

    #[test]
    fn le_drawdown_courant_est_positif_ou_nul() {
        let mut t = DrawdownTracker::new(100.0);
        assert_eq!(t.step(100.0, 1).to_bits(), 0.0f64.to_bits());
        assert_eq!(t.step(110.0, 2).to_bits(), 0.0f64.to_bits());
        assert_eq!(t.step(99.0, 3), 1.0 - 99.0 / 110.0);
    }
}
