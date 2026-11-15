//! Entiers uniformes exacts et flux S14 reproductibles, sans dépendance aux performances.
//!
//! Les flux restent indépendants de K, de la position du parent dans un panneau et des chemins.
//! L'appelant résout [`ControlMode::effective`] avant toute dérivation. Les coûts S10 continuent
//! d'utiliser leurs clés et leurs tirages historiques dans `model::rng`.

use num_bigint::BigUint;

use crate::model::{hash::canonical_json, rng::SplitMix64};

use super::types::{ControlMode, PlanningFailure};

const CALENDAR_DOMAIN: &str = "tv4-controls-calendar-v1";
const BOOTSTRAP_DOMAIN: &str = "tv4-controls-bootstrap-v1";

/// Tire uniformément dans `[0, bound)`, par rejet numérique exact uniquement.
///
/// La borne 1 rend zéro sans consommer de mot ; zéro est une entrée invalide. Pour une borne
/// non puissance de deux, l'acceptation dépasse 1/2 ; pour une puissance de deux elle vaut 1.
/// Les tailles et conversions sont contrôlées avant consommation de l'aléa.
pub fn uniform_below(rng: &mut SplitMix64, bound: &BigUint) -> Result<BigUint, PlanningFailure> {
    uniform_below_words(&mut || rng.next_u64(), bound)
}

// Injection privée pour prouver les frontières et l'ordre des mots sans test statistique.
fn uniform_below_words(
    next_word: &mut impl FnMut() -> u64,
    bound: &BigUint,
) -> Result<BigUint, PlanningFailure> {
    if bound == &BigUint::default() {
        return Err(invalid("la borne du tirage doit être strictement positive"));
    }
    let bits = (bound - 1_u32).bits();
    if bits == 0 {
        return Ok(BigUint::default());
    }
    let word_count = ((bits - 1) / 64) + 1;
    let byte_count = word_count
        .checked_mul(8)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| invalid("la taille du tirage dépasse la capacité adressable"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_count)
        .map_err(|_| invalid("la taille du tirage ne peut pas être allouée"))?;
    bytes.resize(byte_count, 0);
    let top_bits = bits % 64;
    loop {
        for (index, chunk) in bytes.chunks_exact_mut(8).enumerate() {
            let mut word = next_word();
            if index == byte_count / 8 - 1 && top_bits != 0 {
                word &= u64::MAX >> (64 - top_bits);
            }
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        let candidate = BigUint::from_bytes_le(&bytes);
        if &candidate < bound {
            return Ok(candidate);
        }
    }
}

/// Flux d'une paire, d'une réplication base zéro et d'une tentative base zéro.
///
/// Le mode doit déjà être effectif : sans filtre BTC, `RemoveBtc.effective(false)` partage
/// exactement le flux de `PreserveBtc`. Tirer ensuite `uniform_below(rng, table.total())`, puis
/// appeler `table.unrank(rank)` ; aucun rejet global des dates n'entre dans cet échantillonnage.
pub fn calendar_rng(
    seed: u64,
    parent_config_hash: &str,
    effective_mode: ControlMode,
    replication: u32,
    attempt: u32,
    symbol: &str,
) -> Result<SplitMix64, PlanningFailure> {
    if symbol.trim().is_empty() {
        return Err(invalid("le symbole du flux calendrier est vide"));
    }
    stream_rng(
        CALENDAR_DOMAIN,
        seed,
        parent_config_hash,
        effective_mode,
        replication,
        attempt,
        symbol,
    )
}

/// Flux bootstrap commun au portefeuille source et aux K journaux de ce parent/mode effectif.
///
/// La clé canonique réserve réplication=0, tentative=0, symbole="" : ce flux n'appartient à
/// aucune paire ni réplication calendrier. Il est réutilisé pour (a)/(b) et les aliases exacts.
/// Ni K, ni l'ordre, ni la présence d'autres parents ne modifient ce flux.
pub fn bootstrap_rng(
    seed: u64,
    parent_config_hash: &str,
    effective_mode: ControlMode,
) -> Result<SplitMix64, PlanningFailure> {
    stream_rng(
        BOOTSTRAP_DOMAIN,
        seed,
        parent_config_hash,
        effective_mode,
        0,
        0,
        "",
    )
}

fn stream_rng(
    domain: &str,
    seed: u64,
    parent_config_hash: &str,
    effective_mode: ControlMode,
    replication: u32,
    attempt: u32,
    symbol: &str,
) -> Result<SplitMix64, PlanningFailure> {
    if parent_config_hash.len() != 64
        || !parent_config_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(
            "le hash parent du flux doit contenir 64 chiffres hexadécimaux minuscules",
        ));
    }
    // Tableau JSON canonique versionné : ordre des champs contractuel, mode snake_case, seed
    // entier u64 exact (y compris MAX), indices base zéro. Aucune identité d'exécution ajoutée.
    let canonical = canonical_json(&(
        domain,
        seed,
        parent_config_hash,
        effective_mode,
        replication,
        attempt,
        symbol,
    ))
    .map_err(|_| invalid("la clé du flux ne peut pas être encodée canoniquement"))?;
    let digest = blake3::hash(canonical.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    Ok(SplitMix64::new(u64::from_le_bytes(bytes)))
}

fn invalid(message: &str) -> PlanningFailure {
    PlanningFailure::InvalidInput(message.into())
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;

    use super::uniform_below_words;

    fn forced(bound: &BigUint, words: &[u64]) -> (BigUint, usize) {
        let mut consumed = 0;
        let value = uniform_below_words(
            &mut || {
                let word = words[consumed];
                consumed += 1;
                word
            },
            bound,
        )
        .expect("borne valide");
        (value, consumed)
    }

    #[test]
    fn power_of_two_64_consumes_exactly_one_whole_word() {
        let bound = BigUint::from(1_u32) << 64;
        assert_eq!(forced(&bound, &[u64::MAX]), (BigUint::from(u64::MAX), 1));
    }

    #[test]
    fn equal_bound_64_plus_one_is_rejected_in_little_endian_order() {
        let bound = (BigUint::from(1_u32) << 64) + 1_u32;
        assert_eq!(
            forced(&bound, &[1, 1, 0, 1]),
            (BigUint::from(1_u32) << 64, 4)
        );
    }

    #[test]
    fn bound_130_minus_17_masks_high_bits_and_rejects_equal_boundary() {
        let bound = (BigUint::from(1_u32) << 130) - 17_u32;
        // 2^130-17 = [MAX-16, MAX, 3] en mots LE. Les bits hauts parasites sont masqués.
        assert_eq!(
            forced(
                &bound,
                &[u64::MAX - 16, u64::MAX, u64::MAX, 42, 7, u64::MAX - 3]
            ),
            (BigUint::from(42_u32) + (BigUint::from(7_u32) << 64), 6)
        );
        assert_eq!(
            forced(&bound, &[u64::MAX - 17, u64::MAX, u64::MAX]),
            (&bound - 1_u32, 3)
        );
    }

    #[test]
    fn words_and_bytes_are_little_endian() {
        let bound = BigUint::from(1_u32) << 128;
        let expected =
            BigUint::from_bytes_le(&[8, 7, 6, 5, 4, 3, 2, 1, 24, 23, 22, 21, 20, 19, 18, 17]);
        assert_eq!(
            forced(&bound, &[0x0102_0304_0506_0708, 0x1112_1314_1516_1718]),
            (expected, 2)
        );
    }

    #[test]
    fn several_rejections_consume_only_the_required_words() {
        assert_eq!(
            forced(&BigUint::from(5_u32), &[5, 6, 7, 13, 4]),
            (BigUint::from(4_u32), 5)
        );
    }

    #[test]
    fn exhaustive_low_bits_cover_small_bound_without_modulo() {
        for word in 0..8 {
            let (value, consumed) = forced(&BigUint::from(5_u32), &[word, 0]);
            if word < 5 {
                assert_eq!((value, consumed), (BigUint::from(word), 1));
            } else {
                assert_eq!((value, consumed), (BigUint::default(), 2));
            }
        }
    }
}
