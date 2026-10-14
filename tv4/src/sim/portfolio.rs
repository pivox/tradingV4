//! Portefeuille spot et trades clos (docs/03-architecture.md §7.2).

use crate::model::Ts;

/// Motif de sortie d'un trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// Vente market issue d'un signal, remplie à l'open de la bougie suivante.
    Signal,
    /// Position soldée à la fin des données, au dernier close (écart S3 documenté).
    ForcedClose,
}

/// Aller-retour clos. `Copy` : le vecteur de trades est préalloué (docs/03 §7.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trade {
    pub entry_signal_ms: Ts,
    /// `open_time` de la bougie de fill d'entrée.
    pub entry_ms: Ts,
    /// Open de la bougie de fill, avant slippage.
    pub entry_ref_price: f64,
    pub entry_price: f64,
    pub entry_fee: f64,
    pub entry_slippage: f64,
    pub exit_signal_ms: Ts,
    pub exit_ms: Ts,
    pub exit_ref_price: f64,
    pub exit_price: f64,
    pub exit_fee: f64,
    pub exit_slippage: f64,
    pub qty: f64,
    /// Produit net de la vente moins coût net de l'achat (frais compris).
    pub pnl_net: f64,
    pub inactive_stop: Option<f64>,
    /// Vrai si le plus bas d'une bougie de la position a touché le stop inactif.
    pub stop_breached: bool,
    pub capped_by_cash: bool,
    pub exit_reason: ExitReason,
    pub entry_tag: &'static str,
    pub exit_tag: &'static str,
}

/// Jambe d'entrée de la position en cours, gardée jusqu'à la sortie pour construire le [`Trade`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct EntryLeg {
    pub signal_ms: Ts,
    pub ref_price: f64,
    pub price: f64,
    pub cost: f64,
    pub fee: f64,
    pub slippage: f64,
    pub tag: &'static str,
    pub inactive_stop: Option<f64>,
    pub stop_breached: bool,
    pub capped_by_cash: bool,
}

/// Portefeuille spot : du cash et une quantité longue, jamais négatifs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Portfolio {
    pub(super) cash: f64,
    pub(super) qty: f64,
    pub(super) avg_price: f64,
    pub(super) opened_at_ms: Ts,
    pub(super) entry: Option<EntryLeg>,
}

impl Portfolio {
    pub(super) fn new(cash: f64) -> Portfolio {
        Portfolio {
            cash,
            qty: 0.0,
            avg_price: 0.0,
            opened_at_ms: 0,
            entry: None,
        }
    }

    pub fn cash(&self) -> f64 {
        self.cash
    }

    /// Quantité détenue (monnaie de base), 0 à plat.
    pub fn qty(&self) -> f64 {
        self.qty
    }

    /// Prix moyen d'entrée (slippage compris, frais exclus), 0 à plat.
    pub fn avg_price(&self) -> f64 {
        self.avg_price
    }

    pub fn opened_at_ms(&self) -> Ts {
        self.opened_at_ms
    }

    pub fn is_long(&self) -> bool {
        self.qty > 0.0
    }
}
