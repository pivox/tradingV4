//! Average True Range de Wilder.

use super::{Indicator, IndicatorError, check_period};
use crate::model::Candle;

/// ATR de Wilder de période `n`, entrée : bougie (seuls `high`, `low` et `close` servent).
///
/// `TR_0 = high - low` (pas de close précédent), puis
/// `TR = max(high - low, |high - close_préc|, |low - close_préc|)`. Version d'origine de Wilder :
/// l'amorçage est la moyenne simple des `n` premiers TR (TR_0 compris), premier résultat à l'index
/// `n - 1`, puis `ATR = (ATR × (n - 1) + TR) / n`. Ce choix garde `warmup_bars = max(slow, 14)` de
/// docs/03 §6 sans écart.
#[derive(Debug, Clone)]
pub struct Atr {
    period: usize,
    prev_close: Option<f64>,
    count: usize,
    sum_tr: f64,
    value: f64,
}

impl Atr {
    pub fn new(period: usize) -> Result<Self, IndicatorError> {
        check_period("atr", period)?;
        Ok(Atr {
            period,
            prev_close: None,
            count: 0,
            sum_tr: 0.0,
            value: 0.0,
        })
    }

    pub fn period(&self) -> usize {
        self.period
    }
}

impl Indicator for Atr {
    type Input = Candle;

    fn update(&mut self, c: Candle) -> f64 {
        debug_assert!(
            c.high.is_finite() && c.low.is_finite() && c.close.is_finite(),
            "atr : bougie non finie"
        );
        // `max` et `abs` sont exacts : pas d'arrondi ajouté.
        let tr = match self.prev_close {
            None => c.high - c.low,
            Some(pc) => (c.high - c.low)
                .max((c.high - pc).abs())
                .max((c.low - pc).abs()),
        };
        self.prev_close = Some(c.close);
        let nf = self.period as f64;
        if self.count < self.period {
            self.sum_tr += tr;
            self.count += 1;
            if self.count < self.period {
                return f64::NAN;
            }
            self.value = self.sum_tr / nf;
        } else {
            let nm1 = (self.period - 1) as f64;
            self.value = (self.value * nm1 + tr) / nf;
        }
        self.value
    }

    fn reset(&mut self) {
        self.prev_close = None;
        self.count = 0;
        self.sum_tr = 0.0;
        self.value = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::batch;

    /// Bougie de test : (high, low, close), open = close, valeurs inventées.
    fn candle(high: f64, low: f64, close: f64) -> Candle {
        Candle {
            open_time_ms: 0,
            open: close,
            high,
            low,
            close,
            volume: 1.0,
            trades: None,
        }
    }

    #[test]
    fn valeurs_a_la_main_periode_2() {
        // TR_0 = 10-8 = 2 ; TR_1 = max(3, |12-9|, |9-9|) = 3 ; amorçage (2+3)/2 = 2,5.
        // TR_2 = max(1, |11-11|, |10-11|) = 1 ; ATR = (2,5 × 1 + 1) / 2 = 1,75.
        let candles = [
            candle(10.0, 8.0, 9.0),
            candle(12.0, 9.0, 11.0),
            candle(11.0, 10.0, 10.0),
        ];
        let out = batch(Atr::new(2).unwrap(), candles);
        assert!(out[0].is_nan());
        assert_eq!(&out[1..], &[2.5, 1.75]);
    }

    #[test]
    fn le_gap_compte_dans_le_true_range() {
        // Close précédent 9, puis h = 15, l = 14 : TR = max(1, 6, 5) = 6.
        let out = batch(
            Atr::new(1).unwrap(),
            [candle(10.0, 8.0, 9.0), candle(15.0, 14.0, 14.5)],
        );
        assert_eq!(out, [2.0, 6.0]);
    }

    #[test]
    fn periode_1_est_le_tr_brut() {
        let out = batch(
            Atr::new(1).unwrap(),
            [candle(10.0, 8.0, 9.0), candle(12.0, 9.0, 11.0)],
        );
        assert_eq!(out, [2.0, 3.0]);
    }

    #[test]
    fn periode_nulle_refusee() {
        assert_eq!(
            Atr::new(0).unwrap_err(),
            IndicatorError::ZeroPeriod { indicator: "atr" }
        );
    }

    #[test]
    fn reset_redonne_les_memes_bits() {
        let candles: Vec<Candle> = (1..30)
            .map(|i| candle(f64::from(i) + 2.0, f64::from(i), f64::from(i) + 1.0))
            .collect();
        let mut atr = Atr::new(4).unwrap();
        let first: Vec<u64> = candles.iter().map(|&c| atr.update(c).to_bits()).collect();
        atr.reset();
        let second: Vec<u64> = candles.iter().map(|&c| atr.update(c).to_bits()).collect();
        assert_eq!(first, second);
    }
}
