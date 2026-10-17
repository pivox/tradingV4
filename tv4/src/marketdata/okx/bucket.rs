//! Token bucket maison et jitter du retry (docs/03-architecture.md §3.1, ligne HTTP).
//!
//! Ni l'un ni l'autre ne justifie une crate (`governor`, `rand`) : un `Mutex<Instant>` et dix lignes
//! de générateur suffisent, et le comportement se teste sans horloge réelle.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// Distribue des créneaux d'envoi espacés d'au moins `spacing`, à tous les threads qui le partagent.
///
/// L'état est un seul `Mutex<Instant>` : le prochain créneau libre. Prendre un créneau le réserve
/// (le prochain recule de `spacing`) puis dort jusqu'à lui, hors du verrou. Les créneaux sont donc
/// servis dans l'ordre de réservation, sans rafale : 125 ms d'espacement donnent 8 requêtes par
/// seconde au maximum, sous la limite de 20 requêtes par 2 s et par IP d'OKX. Chaque tentative d'une
/// requête, retries compris, prend un créneau (l'appelant le garantit).
#[derive(Debug)]
pub struct TokenBucket {
    spacing: Duration,
    next_slot: Mutex<Instant>,
}

impl TokenBucket {
    /// Bucket dont le premier créneau est libre tout de suite.
    pub fn new(spacing: Duration) -> Self {
        TokenBucket {
            spacing,
            next_slot: Mutex::new(Instant::now()),
        }
    }

    /// Réserve le prochain créneau, dort jusqu'à lui et le renvoie (l'instant prévu, pas l'instant
    /// de réveil, qui lui est postérieur ou égal : les tests comparent donc des valeurs exactes).
    pub fn acquire(&self) -> Instant {
        let slot = {
            let mut next = self
                .next_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let slot = (*next).max(Instant::now());
            *next = slot + self.spacing;
            slot
        };
        // `Instant` est monotone : `saturating_duration_since` vaut zéro si le créneau est passé.
        let wait = slot.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            thread::sleep(wait);
        }
        slot
    }

    /// Repousse le prochain créneau à `maintenant + duration` pour tous les threads, sauf s'il est
    /// déjà plus tard. Trois threads qui reçoivent un 429 en même temps provoquent donc une seule
    /// pause de `duration`, pas trois. Les créneaux déjà réservés avant l'appel sont conservés : au plus
    /// une requête par thread part encore, ce qui est le prix de l'absence de file centrale.
    pub fn pause(&self, duration: Duration) {
        let until = Instant::now() + duration;
        let mut next = self
            .next_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *next < until {
            *next = until;
        }
    }
}

/// Générateur pseudo-aléatoire SplitMix64 (Steele, Lea, Flood, 2014) partagé entre threads.
///
/// L'état est un compteur atomique qui avance de la constante d'or `0x9E37_79B9_7F4A_7C15` à chaque
/// tirage ; la sortie est ce compteur passé dans la fonction de mélange de SplitMix64. Sans verrou, et
/// deux threads ne reçoivent jamais la même valeur. Ce n'est pas un générateur cryptographique : il ne
/// sert qu'à désynchroniser les retries. Il tourne à graine fixe dans les tests.
#[derive(Debug)]
pub struct Jitter {
    state: AtomicU64,
}

impl Jitter {
    /// Générateur initialisé par `seed`.
    pub fn new(seed: u64) -> Self {
        Jitter {
            state: AtomicU64::new(seed),
        }
    }

    /// Entier de 64 bits suivant.
    fn next_u64(&self) -> u64 {
        let mut z = self
            .state
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Durée tirée dans `[0, max]`, à la microseconde.
    pub fn up_to(&self, max: Duration) -> Duration {
        let micros = u64::try_from(max.as_micros()).unwrap_or(u64::MAX);
        if micros == 0 {
            return Duration::ZERO;
        }
        // Le biais du modulo est sans importance pour un jitter.
        Duration::from_micros(self.next_u64() % (micros.saturating_add(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_creneaux_sont_espaces_exactement() {
        let bucket = TokenBucket::new(Duration::from_millis(5));
        let slots: Vec<Instant> = (0..4).map(|_| bucket.acquire()).collect();
        for pair in slots.windows(2) {
            assert_eq!(pair[1] - pair[0], Duration::from_millis(5));
        }
    }

    #[test]
    fn la_pause_repousse_le_prochain_creneau() {
        let bucket = TokenBucket::new(Duration::from_millis(1));
        let avant = Instant::now();
        bucket.pause(Duration::from_millis(40));
        let slot = bucket.acquire();
        assert!(slot >= avant + Duration::from_millis(40));
        // Une pause plus courte que l'attente déjà programmée ne raccourcit rien.
        bucket.pause(Duration::from_millis(40));
        bucket.pause(Duration::from_millis(1));
        assert!(bucket.acquire() >= slot + Duration::from_millis(1));
    }

    #[test]
    fn le_jitter_reste_dans_la_borne_et_varie() {
        let jitter = Jitter::new(42);
        let max = Duration::from_millis(10);
        let tirages: Vec<Duration> = (0..200).map(|_| jitter.up_to(max)).collect();
        assert!(tirages.iter().all(|d| *d <= max));
        assert!(tirages.iter().any(|d| *d != tirages[0]));
        assert_eq!(jitter.up_to(Duration::ZERO), Duration::ZERO);
        // Même graine, même suite.
        let a = Jitter::new(7);
        let b = Jitter::new(7);
        assert_eq!(a.up_to(max), b.up_to(max));
    }
}
