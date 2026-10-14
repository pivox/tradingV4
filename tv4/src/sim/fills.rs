//! Fonctions pures de fill market (docs/03-architecture.md §7.2 et §7.4).
//!
//! L'ordre des opérations flottantes est imposé et ne change pas (aucun `mul_add`) : c'est lui qui
//! rend le résultat identique d'un process à l'autre.

use crate::model::Slippage;

/// Résultat du calcul d'un achat market.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BuyFill {
    /// Quantité achetée, éventuellement plafonnée par le cash.
    pub qty: f64,
    /// Prix payé par unité, slippage compris.
    pub price: f64,
    /// `qty × price`, frais exclus.
    pub cost: f64,
    /// Frais taker : `cost × taker`.
    pub fee: f64,
    /// Coût du slippage : `qty × (price − référence)`.
    pub slippage: f64,
    /// Vrai si la quantité demandée a été réduite par le plafond de cash.
    pub capped: bool,
}

/// Achat market à l'open `open`, plafonné à `cash × (1 − margin) / (px × (1 + taker))`.
///
/// Rend `None` si la quantité finale n'est pas finie et strictement positive.
pub(super) fn buy_fill(
    cash: f64,
    requested: f64,
    open: f64,
    slippage: Slippage,
    taker: f64,
    margin: f64,
) -> Option<BuyFill> {
    let price = slippage.buy_price(open);
    let cap = cash * (1.0 - margin) / (price * (1.0 + taker));
    let capped = cap < requested;
    let qty = if capped { cap } else { requested };
    if !(qty.is_finite() && qty > 0.0) {
        return None;
    }
    let cost = qty * price;
    let fee = cost * taker;
    Some(BuyFill {
        qty,
        price,
        cost,
        fee,
        slippage: qty * (price - open),
        capped,
    })
}

/// Résultat du calcul d'une vente market.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SellFill {
    /// Prix reçu par unité, slippage compris.
    pub price: f64,
    /// `qty × price`, frais exclus.
    pub proceeds: f64,
    /// Frais taker : `proceeds × taker`.
    pub fee: f64,
    /// Coût du slippage : `qty × (référence − price)`.
    pub slippage: f64,
}

/// Vente market de `qty` au prix de référence `reference` (l'open d'un fill de signal, le close de
/// la dernière bougie pour la clôture forcée).
pub(super) fn sell_fill(qty: f64, reference: f64, slippage: Slippage, taker: f64) -> SellFill {
    let price = slippage.sell_price(reference);
    let proceeds = qty * price;
    let fee = proceeds * taker;
    SellFill {
        price,
        proceeds,
        fee,
        slippage: qty * (reference - price),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn achat_a_la_main() {
        let s = Slippage::from_bps(10.0).unwrap();
        let fill = buy_fill(1000.0, 1.0, 100.0, s, 0.0035, 0.05).unwrap();
        assert!((fill.price - 100.1).abs() < 1e-9);
        assert!((fill.fee - 0.35035).abs() < 1e-9);
        assert!((fill.slippage - 0.1).abs() < 1e-9);
        assert!(!fill.capped);
    }

    #[test]
    fn quantite_nulle_ou_non_finie_refusee() {
        let s = Slippage::ZERO;
        assert!(buy_fill(100.0, 0.0, 10.0, s, 0.0, 0.05).is_none());
        assert!(buy_fill(100.0, f64::NAN, 10.0, s, 0.0, 0.05).is_none());
        assert!(buy_fill(0.0, 1.0, 10.0, s, 0.0, 0.05).is_none());
    }

    #[test]
    fn vente_a_la_main() {
        let s = Slippage::from_bps(10.0).unwrap();
        let fill = sell_fill(1.0, 110.0, s, 0.0035);
        assert!((fill.price - 109.89).abs() < 1e-9);
        assert!((fill.fee - 0.384615).abs() < 1e-9);
        assert!((fill.slippage - 0.11).abs() < 1e-9);
    }
}
