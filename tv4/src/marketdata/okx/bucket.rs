//! Token bucket maison et jitter du retry (docs/03-architecture.md §3.1, ligne HTTP).
//!
//! Ni l'un ni l'autre ne justifie une crate (`governor`, `rand`) : un `Mutex` et dix lignes
//! de générateur suffisent, et le comportement se teste sans horloge réelle.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// État du bucket, tenu sous un seul verrou.
#[derive(Debug)]
struct BucketState {
    /// Prochain créneau libre.
    next_slot: Instant,
    /// Fin de la dernière pause posée par [`TokenBucket::pause`] (`None` : jamais de pause).
    paused_until: Option<Instant>,
}

/// Distribue des créneaux d'envoi espacés d'au moins `spacing`, à tous les threads qui le partagent.
///
/// L'état est un seul `Mutex` : le prochain créneau libre et la fin de la dernière pause. Prendre un
/// créneau le réserve (le prochain recule de `spacing`) puis dort jusqu'à lui, hors du verrou. Les
/// créneaux sont donc servis dans l'ordre de réservation, sans rafale : 125 ms d'espacement donnent 8
/// requêtes par seconde au maximum, sous la limite de 20 requêtes par 2 s et par IP d'OKX. Chaque
/// tentative d'une requête, retries compris, prend un créneau (l'appelant le garantit).
///
/// Une pause posée **après** la réservation d'un créneau le couvre peut-être : au réveil, [`acquire`]
/// revérifie sous le verrou et, si la pause couvre le créneau, dort jusqu'à la fin de la pause puis
/// reprend un nouveau créneau. Aucune requête ne part donc pendant une pause, même celle d'un thread
/// qui dormait déjà dans `acquire` (constat S7-CLI-1).
///
/// [`acquire`]: TokenBucket::acquire
#[derive(Debug)]
pub struct TokenBucket {
    spacing: Duration,
    state: Mutex<BucketState>,
}

impl TokenBucket {
    /// Bucket dont le premier créneau est libre tout de suite.
    pub fn new(spacing: Duration) -> Self {
        TokenBucket {
            spacing,
            state: Mutex::new(BucketState {
                next_slot: Instant::now(),
                paused_until: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BucketState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Réserve le prochain créneau, dort jusqu'à lui et le renvoie (l'instant prévu, pas l'instant
    /// de réveil, qui lui est postérieur ou égal : les tests comparent donc des valeurs exactes).
    ///
    /// Si une pause posée entre la réservation et le réveil couvre le créneau (`slot < paused_until`),
    /// le créneau est abandonné : le thread dort jusqu'à `paused_until` et en réserve un nouveau.
    pub fn acquire(&self) -> Instant {
        loop {
            let slot = {
                let mut state = self.lock();
                let slot = state.next_slot.max(Instant::now());
                state.next_slot = slot + self.spacing;
                slot
            };
            sleep_until(slot);
            // Revérification après le sommeil : une pause posée depuis la réservation ?
            let paused_until = self.lock().paused_until;
            match paused_until {
                Some(until) if slot < until => sleep_until(until),
                _ => return slot,
            }
        }
    }

    /// Repousse le prochain créneau à `maintenant + duration` pour tous les threads, sauf s'il est
    /// déjà plus tard. Trois threads qui reçoivent un 429 en même temps provoquent donc une seule
    /// pause de `duration`, pas trois. Les threads qui dorment déjà dans [`acquire`](Self::acquire)
    /// sur un créneau antérieur à la fin de la pause le revérifient au réveil et attendent la fin de
    /// la pause : aucune requête ne part avant elle.
    pub fn pause(&self, duration: Duration) {
        let until = Instant::now() + duration;
        let mut state = self.lock();
        if state.next_slot < until {
            state.next_slot = until;
        }
        if state.paused_until.is_none_or(|current| current < until) {
            state.paused_until = Some(until);
        }
    }
}

/// Dort jusqu'à `instant` (rien si déjà passé : `Instant` est monotone, la soustraction sature).
fn sleep_until(instant: Instant) {
    let wait = instant.saturating_duration_since(Instant::now());
    if !wait.is_zero() {
        thread::sleep(wait);
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

    /// S7-CLI-1 : une pause posée après la réservation d'un créneau futur retient aussi le thread qui
    /// dort déjà dans `acquire` ; sa requête part au moins `pause` après la pose de la pause.
    #[test]
    fn la_pause_retient_un_thread_qui_a_deja_reserve_un_creneau() {
        let spacing = Duration::from_millis(100);
        let pause = Duration::from_millis(300);
        let bucket = TokenBucket::new(spacing);
        let premier = bucket.acquire();
        let (pose, repris) = thread::scope(|scope| {
            // Ce thread réserve le créneau `premier + 100 ms` puis dort dedans.
            let dormeur = scope.spawn(|| bucket.acquire());
            // 20 ms : le thread a réservé (il est parti depuis longtemps) mais dort encore.
            thread::sleep(Duration::from_millis(20));
            let pose = Instant::now();
            bucket.pause(pause);
            (pose, dormeur.join().unwrap())
        });
        assert!(repris >= premier + spacing, "le créneau suit le premier");
        assert!(
            repris >= pose + pause,
            "la requête du dormeur part {:?} après la pause, moins que {pause:?}",
            repris.saturating_duration_since(pose)
        );
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
