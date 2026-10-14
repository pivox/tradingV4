//! Moyenne mobile simple (SMA) à somme glissante compensée.

use super::{Indicator, IndicatorError, check_period};

/// SMA de période `n`, entrée : close (`f64`). Premier résultat à l'index `n - 1`.
///
/// Ring buffer de `n` valeurs et somme glissante compensée de Neumaier. Une somme glissante naïve
/// dérive avec les ajouts et retraits, et cette dérive peut devenir catastrophique quand l'échelle
/// des prix change. Avec Neumaier, l'erreur reste à environ 2 ε près de la somme vraie de la fenêtre
/// (plus un terme en N·ε²·Σ|x| négligeable), pour un coût O(1) strict.
#[derive(Debug, Clone)]
pub struct Sma {
    period: usize,
    buf: Vec<f64>,
    head: usize,
    count: usize,
    sum: f64,
    comp: f64,
}

/// Ajout compensé de Neumaier : `comp` accumule l'erreur d'arrondi de chaque addition.
fn neumaier_add(sum: &mut f64, comp: &mut f64, x: f64) {
    let t = *sum + x;
    if sum.abs() >= x.abs() {
        *comp += (*sum - t) + x;
    } else {
        *comp += (x - t) + *sum;
    }
    *sum = t;
}

impl Sma {
    pub fn new(period: usize) -> Result<Self, IndicatorError> {
        check_period("sma", period)?;
        Ok(Sma {
            period,
            buf: vec![0.0; period],
            head: 0,
            count: 0,
            sum: 0.0,
            comp: 0.0,
        })
    }

    pub fn period(&self) -> usize {
        self.period
    }
}

impl Indicator for Sma {
    type Input = f64;

    fn update(&mut self, x: f64) -> f64 {
        debug_assert!(x.is_finite(), "sma : entrée non finie");
        let n = self.period;
        // Invariant : `head < n == buf.len()` (initialisé à 0, remis à 0 dès qu'il atteint n), donc
        // l'indexation ne peut pas sortir de la tranche. C'est la seule indexation à index calculé de la lib.
        if self.count == n {
            let oldest = self.buf[self.head];
            neumaier_add(&mut self.sum, &mut self.comp, -oldest);
        }
        self.buf[self.head] = x;
        self.head = if self.head + 1 == n { 0 } else { self.head + 1 };
        neumaier_add(&mut self.sum, &mut self.comp, x);
        if self.count < n {
            self.count += 1;
        }
        if self.count < n {
            f64::NAN
        } else {
            (self.sum + self.comp) / n as f64
        }
    }

    fn reset(&mut self) {
        self.head = 0;
        self.count = 0;
        self.sum = 0.0;
        self.comp = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::batch;

    fn bits(values: &[f64]) -> Vec<u64> {
        values.iter().map(|v| v.to_bits()).collect()
    }

    #[test]
    fn periode_nulle_refusee() {
        assert_eq!(
            Sma::new(0).unwrap_err(),
            IndicatorError::ZeroPeriod { indicator: "sma" }
        );
        assert!(Sma::new(100_001).is_err());
    }

    #[test]
    fn valeurs_a_la_main_periode_3() {
        let out = batch(Sma::new(3).unwrap(), [1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!(out[0].is_nan() && out[1].is_nan());
        assert_eq!(&out[2..], &[2.0, 3.0, 4.0]);
        assert_eq!(out[0].to_bits(), f64::NAN.to_bits(), "NAN exact en warmup");
    }

    #[test]
    fn periode_1_est_l_identite() {
        let data = [5.0, 7.5, 0.25, 100.0];
        assert_eq!(batch(Sma::new(1).unwrap(), data), data);
    }

    #[test]
    fn reset_redonne_les_memes_bits() {
        let data: Vec<f64> = (1..50).map(|i| f64::from(i) * 1.37).collect();
        let mut sma = Sma::new(7).unwrap();
        let first: Vec<f64> = data.iter().map(|&x| sma.update(x)).collect();
        sma.reset();
        let second: Vec<f64> = data.iter().map(|&x| sma.update(x)).collect();
        assert_eq!(bits(&first), bits(&second));
    }

    #[test]
    fn melange_d_echelles_reste_precis() {
        let n = 10_000;
        let period = 100;
        let data: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { 1e5 } else { 1e-2 })
            .collect();
        let out = batch(Sma::new(period).unwrap(), data.iter().copied());
        for i in (period - 1)..n {
            let direct = data[i + 1 - period..=i].iter().fold(0.0, |s, &x| s + x) / period as f64;
            let rel = (out[i] - direct).abs() / direct.abs();
            assert!(rel < 1e-12, "index {i} : {} contre {direct}", out[i]);
        }
    }

    /// Changement de régime : une longue série de grands prix, puis une fenêtre de petits prix. C'est
    /// le cas qui justifie la somme de Neumaier : une somme glissante naïve garde l'erreur d'arrondi
    /// laissée par les grandes valeurs (écart relatif ≈ 3,6e-9 mesuré), Neumaier reste à ≈ 3e-16.
    #[test]
    fn changement_de_regime_reste_precis() {
        let period = 100;
        let mut data: Vec<f64> = (0..10_000)
            .map(|i| 1e5 + f64::from(i % 97) * 0.37)
            .collect();
        data.extend((0..1_000).map(|i| 1e-2 + f64::from(i % 13) * 1e-4));
        let out = batch(Sma::new(period).unwrap(), data.iter().copied());
        assert_eq!(out.len(), data.len());
        for i in (data.len() - 900)..data.len() {
            let direct = data[i + 1 - period..=i].iter().fold(0.0, |s, &x| s + x) / period as f64;
            let rel = (out[i] - direct).abs() / direct.abs();
            assert!(
                rel < 1e-12,
                "index {i} : {} contre {direct} (rel {rel:e})",
                out[i]
            );
        }
    }
}
