//! RSI de Wilder.

use super::{Indicator, IndicatorError, check_period};

/// RSI de Wilder de période `n`, entrée : close (`f64`). Premier résultat à l'index `n` (il faut
/// `n + 1` entrées pour avoir `n` variations).
///
/// Les moyennes de gains et de pertes sont amorcées par la moyenne simple des `n` premières
/// variations, puis lissées à la Wilder : `avg = (avg × (n - 1) + x) / n`.
///
/// Le résultat s'écrit `100 × g / (g + l)` : pas de division par zéro quand les pertes sont nulles,
/// et exactement 100 dans ce cas puisque `g / g = 1`. Comme `g ≤ g + l` après arrondi, la valeur reste
/// dans [0, 100].
///
/// **Convention du projet : une série plate (`g + l = 0`) rend 50**, la valeur neutre. TA-Lib rend 0,
/// ce qui ferait croire à une survente.
#[derive(Debug, Clone)]
pub struct Rsi {
    period: usize,
    prev: Option<f64>,
    count: usize,
    sum_gain: f64,
    sum_loss: f64,
    avg_gain: f64,
    avg_loss: f64,
}

/// RSI à partir des moyennes de gains `g` et de pertes `l`.
fn rsi_value(g: f64, l: f64) -> f64 {
    let s = g + l;
    if s == 0.0 { 50.0 } else { 100.0 * (g / s) }
}

impl Rsi {
    pub fn new(period: usize) -> Result<Self, IndicatorError> {
        check_period("rsi", period)?;
        Ok(Rsi {
            period,
            prev: None,
            count: 0,
            sum_gain: 0.0,
            sum_loss: 0.0,
            avg_gain: 0.0,
            avg_loss: 0.0,
        })
    }

    pub fn period(&self) -> usize {
        self.period
    }
}

impl Indicator for Rsi {
    type Input = f64;

    fn update(&mut self, x: f64) -> f64 {
        debug_assert!(x.is_finite(), "rsi : entrée non finie");
        let Some(p) = self.prev else {
            self.prev = Some(x);
            return f64::NAN;
        };
        self.prev = Some(x);
        let d = x - p;
        // Pas de `f64::max` : le signe du zéro n'y est pas spécifié.
        let g = if d > 0.0 { d } else { 0.0 };
        let l = if d < 0.0 { -d } else { 0.0 };
        let nf = self.period as f64;
        if self.count < self.period {
            self.sum_gain += g;
            self.sum_loss += l;
            self.count += 1;
            if self.count < self.period {
                return f64::NAN;
            }
            self.avg_gain = self.sum_gain / nf;
            self.avg_loss = self.sum_loss / nf;
        } else {
            let nm1 = (self.period - 1) as f64;
            self.avg_gain = (self.avg_gain * nm1 + g) / nf;
            self.avg_loss = (self.avg_loss * nm1 + l) / nf;
        }
        rsi_value(self.avg_gain, self.avg_loss)
    }

    fn reset(&mut self) {
        self.prev = None;
        self.count = 0;
        self.sum_gain = 0.0;
        self.sum_loss = 0.0;
        self.avg_gain = 0.0;
        self.avg_loss = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::batch;

    #[test]
    fn valeurs_a_la_main_periode_2() {
        // Variations : +1, +1, -1, 0. Amorçage sur les deux premières : g = 1, l = 0 -> 100.
        // Puis g = (1 × 1 + 0) / 2 = 0,5, l = (0 × 1 + 1) / 2 = 0,5 -> 50.
        // Puis g = (0,5 + 0) / 2 = 0,25, l = (0,5 + 0) / 2 = 0,25 -> 50.
        let out = batch(Rsi::new(2).unwrap(), [1.0, 2.0, 3.0, 2.0, 2.0]);
        assert!(out[0].is_nan() && out[1].is_nan());
        assert_eq!(&out[2..], &[100.0, 50.0, 50.0]);
    }

    #[test]
    fn series_monotones_et_constante() {
        let up: Vec<f64> = (1..=30).map(f64::from).collect();
        let down: Vec<f64> = up.iter().rev().copied().collect();
        let flat = vec![42.0; 30];
        assert!(
            batch(Rsi::new(14).unwrap(), up)[14..]
                .iter()
                .all(|&v| v == 100.0)
        );
        assert!(
            batch(Rsi::new(14).unwrap(), down)[14..]
                .iter()
                .all(|&v| v == 0.0)
        );
        assert!(
            batch(Rsi::new(14).unwrap(), flat)[14..]
                .iter()
                .all(|&v| v == 50.0)
        );
    }

    #[test]
    fn le_premier_index_non_nan_est_n() {
        let out = batch(Rsi::new(5).unwrap(), (1..=12).map(|i| f64::from(i * i % 7)));
        for (i, v) in out.iter().enumerate() {
            assert_eq!(v.is_nan(), i < 5, "index {i}");
        }
    }

    #[test]
    fn periode_nulle_refusee() {
        assert_eq!(
            Rsi::new(0).unwrap_err(),
            IndicatorError::ZeroPeriod { indicator: "rsi" }
        );
    }

    #[test]
    fn reset_redonne_les_memes_bits() {
        let data: Vec<f64> = (1..40).map(|i| f64::from(i * 7 % 11) + 1.0).collect();
        let mut rsi = Rsi::new(5).unwrap();
        let first: Vec<u64> = data.iter().map(|&x| rsi.update(x).to_bits()).collect();
        rsi.reset();
        let second: Vec<u64> = data.iter().map(|&x| rsi.update(x).to_bits()).collect();
        assert_eq!(first, second);
    }
}
