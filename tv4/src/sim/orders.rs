//! Ordres market du simulateur S3 (docs/03-architecture.md §7.2).

use crate::model::Ts;

/// Nature d'un ordre market.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarketOrderKind {
    /// Achat de `qty` unités de la monnaie de base (sizing fait par l'engine).
    Buy { qty: f64 },
    /// Vend toute la position (spot : jamais plus que la quantité détenue).
    SellAll,
}

/// Ordre market en attente : exécutable à partir de la bougie qui suit son émission (règle I3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarketOrder {
    pub kind: MarketOrderKind,
    /// `now_ms` de la bougie du signal (clôture de la 1m qui l'a émis).
    pub signal_ms: Ts,
    pub tag: &'static str,
    /// Stop reçu avec l'entrée : mémorisé, JAMAIS exécuté en S3.
    pub inactive_stop: Option<f64>,
}

/// Raison du refus d'un ordre par [`Simulator::submit`](super::Simulator::submit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// La file d'attente contient déjà `max_pending` ordres.
    PendingFull,
    /// Achat alors qu'une position est ouverte ou qu'un achat est déjà en attente.
    EntryWhileLong,
    /// Vente sans position (pas de short en spot).
    ExitWithoutPosition,
    /// Une vente est déjà en attente.
    DuplicateExit,
}
