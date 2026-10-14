//! Moyenne mobile exponentielle (EMA), amorcée par la SMA des `n` premières valeurs.

use super::{Indicator, IndicatorError, check_period};

/// EMA de période `n`, entrée : close (`f64`). `alpha = 2 / (n + 1)`.
///
/// Amorçage : SMA des `n` premières valeurs, premier résultat à l'index `n - 1`. Pour `n = 1`,
/// `alpha = 1` : identité à 1 ulp près (`v + (x - v)` n'est pas toujours exactement `x`).
#[derive(Debug, Clone)]
pub struct Ema {
    period: usize,
    alpha: f64,
    count: usize,
    seed_sum: f64,
    value: f64,
}

impl Ema {
    pub fn new(period: usize) -> Result<Self, IndicatorError> {
        check_period("ema", period)?;
        Ok(Ema {
            period,
            alpha: 2.0 / (period as f64 + 1.0),
            count: 0,
            seed_sum: 0.0,
            value: 0.0,
        })
    }

    pub fn period(&self) -> usize {
        self.period
    }
}

impl Indicator for Ema {
    type Input = f64;

    fn update(&mut self, x: f64) -> f64 {
        debug_assert!(x.is_finite(), "ema : entrée non finie");
        if self.count < self.period {
            self.seed_sum += x;
            self.count += 1;
            if self.count < self.period {
                return f64::NAN;
            }
            self.value = self.seed_sum / self.period as f64;
            return self.value;
        }
        self.value += self.alpha * (x - self.value);
        self.value
    }

    fn reset(&mut self) {
        self.count = 0;
        self.seed_sum = 0.0;
        self.value = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::batch;

    #[test]
    fn valeurs_a_la_main_periode_3() {
        // alpha = 0,5 ; amorçage (1+2+3)/3 = 2 ; puis 2 + 0,5 (4-2) = 3 ; 3 + 0,5 (5-3) = 4.
        let out = batch(Ema::new(3).unwrap(), [1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!(out[0].is_nan() && out[1].is_nan());
        assert_eq!(&out[2..], &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn periode_1_sur_des_entiers_est_l_identite() {
        let data = [5.0, 7.0, 2.0, 100.0];
        assert_eq!(batch(Ema::new(1).unwrap(), data), data);
    }

    #[test]
    fn periode_nulle_refusee() {
        assert_eq!(
            Ema::new(0).unwrap_err(),
            IndicatorError::ZeroPeriod { indicator: "ema" }
        );
    }

    #[test]
    fn reset_redonne_les_memes_bits() {
        let data: Vec<f64> = (1..40).map(|i| f64::from(i) * 0.7).collect();
        let mut ema = Ema::new(5).unwrap();
        let first: Vec<u64> = data.iter().map(|&x| ema.update(x).to_bits()).collect();
        ema.reset();
        let second: Vec<u64> = data.iter().map(|&x| ema.update(x).to_bits()).collect();
        assert_eq!(first, second);
    }
}
