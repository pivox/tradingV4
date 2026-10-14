//! Générateur synthétique déterministe, partagé par les tests d'intégration et le bench criterion.
//!
//! Aucune crate aléatoire et aucune donnée de marché : une marche multiplicative pilotée par un
//! SplitMix64. Les prix sont inventés (autour de 100).

// Inclus par plusieurs crates de test et par le bench : chacun n'utilise qu'une partie.
#![allow(dead_code)]

use tv4::model::Candle;

/// SplitMix64 standard : générateur de 64 bits, déterministe, sans dépendance.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Valeur dans [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }
}

/// `n` bougies 1m synthétiques valides, avec des trous si `gap_prob > 0` et des volumes nuls (2 %).
///
/// `open` = close précédent (100,0 au départ) ; `close = open × (1 + (u - 0,5) × 0,002)` ;
/// `high` et `low` débordent de 0 à 5 bps ; `open_time` avance d'une minute, plus 1 à 30 minutes
/// quand un trou est tiré.
pub fn synthetic_candles(n: usize, seed: u64, start_ms: i64, gap_prob: f64) -> Vec<Candle> {
    let mut rng = SplitMix64(seed);
    let mut candles = Vec::with_capacity(n);
    let mut open = 100.0_f64;
    let mut open_time_ms = start_ms;
    for _ in 0..n {
        let u = rng.next_f64();
        let u2 = rng.next_f64();
        let u3 = rng.next_f64();
        let u4 = rng.next_f64();
        let u5 = rng.next_f64();
        // Longueur du trou tirée à part : `u5 < gap_prob` borne u5 et ne peut pas servir deux fois.
        let u6 = rng.next_f64();
        let close = open * (1.0 + (u - 0.5) * 0.002);
        let high = open.max(close) * (1.0 + u2 * 0.0005);
        let low = open.min(close) * (1.0 - u3 * 0.0005);
        let volume = if u4 < 0.02 { 0.0 } else { u4 * 5.0 };
        candles.push(Candle {
            open_time_ms,
            open,
            high,
            low,
            close,
            volume,
            trades: None,
        });
        open = close;
        open_time_ms += 60_000;
        if u5 < gap_prob {
            open_time_ms += (1 + (u6 * 30.0) as i64) * 60_000;
        }
    }
    candles
}
