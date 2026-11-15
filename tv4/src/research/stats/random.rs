//! Flux statistiques et échantillonnage entier commun sans biais.
use crate::model::rng::SplitMix64;

/// Espaces de tirages distincts réservés aux post-traitements S13.
pub const STREAM_BOOTSTRAP: u64 = 0x5331_3300_0000_0001;
pub const STREAM_MONTE_CARLO: u64 = 0x5331_3300_0000_0002;
pub const STREAM_REMOVAL: u64 = 0x5331_3300_0000_0003;
pub const STREAM_MONTHLY_BOOTSTRAP: u64 = 0x5331_3300_0000_0004;

/// Échantillonnage entier uniforme par rejet de la zone incomplète de 2^64.
pub(crate) fn uniform_index(rng: &mut SplitMix64, bound: u64) -> usize {
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = rng.next_u64();
        if value >= threshold {
            return (value % bound) as usize;
        }
    }
}
