//! Aléa seedé des profils de coûts : SplitMix64 **à compteur**, écrit dans le dépôt, sans crate
//! (docs/specs/s10.md §8).
//!
//! Un tirage est une fonction pure de ses entrées : [`draw`] prend la graine et quatre mots (la clé
//! de la jambe, le flux, l'horodatage de l'événement, un discriminant) et rend un `u64`. Il n'y a
//! aucun état mutable de générateur : l'ordre d'itération, la présence d'autres jambes et le profil
//! n'ont aucune influence sur la valeur d'un événement donné (nombres aléatoires communs).
//!
//! Tout est arithmétique entière (`wrapping_*`, décalages), puis une conversion exacte en `[0, 1)`
//! ([`unit`] : 53 bits, puissance de deux) : le résultat est le même à l'octet sur x86-64 et sur
//! aarch64. Les vecteurs de test sont épinglés par `tests/cost_rng.rs`.
//!
//! Ce n'est pas un générateur cryptographique. La constante d'or ci-dessous n'a le droit d'apparaître
//! que dans ce fichier et dans `marketdata/okx/bucket.rs` (jitter des retries) : un seul générateur
//! sert la simulation (`tests/architecture.rs`).

/// Constante d'or de SplitMix64 (`2^64 / φ`, rendue impaire).
pub const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// Flux du tirage de quantile du slippage d'un stop (s10 §5.2).
pub const STREAM_STOP_SLIPPAGE: u64 = 1;
/// Flux du tirage de stress d'un stop (s10 §5.2).
pub const STREAM_STOP_STRESS: u64 = 2;
/// Flux du tirage de fill d'un ordre maker au repos (s10 §7.3).
pub const STREAM_MAKER_FILL: u64 = 3;

/// `2^-53`, rendu exactement (quotient de deux puissances de deux).
const UNIT_SCALE: f64 = 1.0 / 9_007_199_254_740_992.0;

/// Fonction de mélange de SplitMix64.
pub const fn mix(z: u64) -> u64 {
    let z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Tirage à compteur : `h = seed`, puis pour chaque mot `w`, `h = mix((h ^ w) + G)`.
///
/// Pure et sans allocation : les quatre mots vivent sur la pile de l'appelant.
pub const fn draw(seed: u64, words: [u64; 4]) -> u64 {
    let mut h = seed;
    let mut i = 0;
    while i < 4 {
        h = mix((h ^ words[i]).wrapping_add(GOLDEN_GAMMA));
        i += 1;
    }
    h
}

/// Conversion exacte d'un tirage en `[0, 1)` : les 53 bits de poids fort, fois `2^-53`.
pub fn unit(x: u64) -> f64 {
    (x >> 11) as f64 * UNIT_SCALE
}

/// Horodatage (ms UTC, `i64`) en mot de tirage : complément à deux, `i64 as u64`.
pub const fn ts_word(ts_ms: i64) -> u64 {
    ts_ms as u64
}

/// Clé d'une jambe : les 8 premiers octets de `BLAKE3(symbole en UTF-8)`, lus en petit-boutiste.
///
/// Tirée du symbole et non de l'indice dans l'univers : une jambe garde ses tirages quel que soit
/// l'ordre de l'univers ou la présence d'autres jambes (s10 §8.4).
pub fn leg_key(symbol: &str) -> u64 {
    let digest = blake3::hash(symbol.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

/// SplitMix64 séquentiel (`next_u64 = mix(state += G)`), pour les tests et pour `research`.
///
/// La simulation, elle, n'en a pas besoin : elle appelle [`draw`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Générateur initialisé par `seed`.
    pub const fn new(seed: u64) -> SplitMix64 {
        SplitMix64 { state: seed }
    }

    /// Entier de 64 bits suivant.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        mix(self.state)
    }

    /// Flottant suivant, dans `[0, 1)`.
    pub fn next_unit(&mut self) -> f64 {
        unit(self.next_u64())
    }
}
