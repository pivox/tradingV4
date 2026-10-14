//! Indicateurs incrémentaux (docs/03-architecture.md §1 principe 2, §4 et §7.5).
//!
//! Chaque indicateur est une structure à état avec `update(x) -> f64` en O(1) strict et sans
//! allocation (les fenêtres glissantes utilisent un ring buffer alloué à la construction). Le calcul
//! par lot est le fold de `update` ([`batch`]), jamais une implémentation séparée : c'est ce qui rend
//! vraie la stabilité par préfixe (I1) `indic(data[..k]) == indic(data)[..k]`, au bit près.
//!
//! Règles communes aux cinq indicateurs :
//!
//! - le warmup rend exactement la constante [`f64::NAN`] ;
//! - toute somme est une boucle explicite (pas de `.sum::<f64>()`, dont la valeur initiale a varié) ;
//! - aucun `mul_add` : les arrondis sont ceux des opérations IEEE séparées (docs/03 §7.4) ;
//! - sur des données avec trous (politique `skip`), « la bougie précédente » est la précédente dans la
//!   série, quel que soit l'écart de temps.
//!
//! Le module ne dépend que de `model`.

pub mod atr;
pub mod ema;
pub mod rsi;
pub mod sma;
pub mod vwap;

pub use atr::Atr;
pub use ema::Ema;
pub use rsi::Rsi;
pub use sma::Sma;
pub use vwap::Vwap;

use crate::model::MAX_INDICATOR_PERIOD;
use thiserror::Error;

/// Paramètre d'indicateur invalide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IndicatorError {
    #[error("{indicator} : période nulle, elle doit valoir au moins 1")]
    ZeroPeriod { indicator: &'static str },
    #[error("{indicator} : période {period} supérieure au maximum {max}")]
    PeriodTooLarge {
        indicator: &'static str,
        period: usize,
        max: usize,
    },
}

/// Indicateur incrémental : `update` en O(1), sans allocation, rend [`f64::NAN`] en warmup.
pub trait Indicator {
    /// `f64` (close) pour SMA, EMA et RSI ; `Candle` pour ATR et VWAP.
    type Input: Copy;

    /// Intègre une valeur et rend l'indicateur à cette position.
    fn update(&mut self, input: Self::Input) -> f64;

    /// Revient à l'état neuf, sans réallouer.
    fn reset(&mut self);
}

/// Batch = fold de `update` (docs/03 §1), jamais une implémentation séparée.
pub fn batch<I: Indicator>(
    mut indicator: I,
    inputs: impl IntoIterator<Item = I::Input>,
) -> Vec<f64> {
    let inputs = inputs.into_iter();
    let mut out = Vec::with_capacity(inputs.size_hint().0);
    batch_into(&mut indicator, inputs, &mut out);
    out
}

/// Ajoute à `out` (sans le vider) en poursuivant l'état courant de l'indicateur (sans reset) :
/// reprise en flux.
pub fn batch_into<I: Indicator>(
    indicator: &mut I,
    inputs: impl IntoIterator<Item = I::Input>,
    out: &mut Vec<f64>,
) {
    out.extend(inputs.into_iter().map(|x| indicator.update(x)));
}

/// Contrôle commun des périodes : `1..=MAX_INDICATOR_PERIOD`.
pub(crate) fn check_period(indicator: &'static str, period: usize) -> Result<(), IndicatorError> {
    if period == 0 {
        Err(IndicatorError::ZeroPeriod { indicator })
    } else if period > MAX_INDICATOR_PERIOD {
        Err(IndicatorError::PeriodTooLarge {
            indicator,
            period,
            max: MAX_INDICATOR_PERIOD,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controle_des_periodes() {
        assert_eq!(
            check_period("x", 0),
            Err(IndicatorError::ZeroPeriod { indicator: "x" })
        );
        assert_eq!(check_period("x", 1), Ok(()));
        assert_eq!(check_period("x", 100_000), Ok(()));
        assert_eq!(
            check_period("x", 100_001),
            Err(IndicatorError::PeriodTooLarge {
                indicator: "x",
                period: 100_001,
                max: 100_000
            })
        );
        assert!(
            check_period("sma", 0)
                .unwrap_err()
                .to_string()
                .contains("sma")
        );
    }

    #[test]
    fn batch_est_le_fold_de_update() {
        let data = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
        let mut manual = Sma::new(3).unwrap();
        let expected: Vec<f64> = data.iter().map(|&x| manual.update(x)).collect();
        let got = batch(Sma::new(3).unwrap(), data);
        assert_eq!(got.len(), expected.len());
        for (a, b) in got.iter().zip(&expected) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn batch_into_ajoute_sans_vider() {
        let mut indicator = Sma::new(2).unwrap();
        let mut out = vec![-1.0];
        batch_into(&mut indicator, [2.0, 4.0], &mut out);
        batch_into(&mut indicator, [6.0], &mut out);
        assert!(out[0] == -1.0 && out[1].is_nan());
        assert_eq!(&out[2..], &[3.0, 5.0]);
    }
}
