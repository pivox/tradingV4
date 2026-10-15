//! Fonctions pures de prix de fill et de franchissement (docs/04-protocole-recherche.md §3.1,
//! docs/03-architecture.md §7.2 et §7.4).
//!
//! L'ordre des opérations flottantes est imposé et ne change pas (aucun `mul_add`) : c'est lui qui
//! rend le résultat identique d'un process à l'autre.

use crate::model::Slippage;

/// Prix d'un achat market à l'open `open` : `open × (1 + slippage)`.
pub(super) fn market_buy_price(open: f64, slippage: Slippage) -> f64 {
    slippage.buy_price(open)
}

/// Prix d'une vente market à l'open (ou d'une clôture forcée au close) : `ref × (1 − slippage)`.
pub(super) fn market_sell_price(reference: f64, slippage: Slippage) -> f64 {
    slippage.sell_price(reference)
}

/// Prix de fill d'une limit d'achat : `min(limite, open)`, le gap sous la limite donne le meilleur prix.
pub(super) fn limit_buy_price(limit: f64, open: f64) -> f64 {
    if limit <= open { limit } else { open }
}

/// Prix de fill d'une limit de vente (et d'un take-profit) : `max(limite, open)`.
pub(super) fn limit_sell_price(limit: f64, open: f64) -> f64 {
    if limit >= open { limit } else { open }
}

/// Niveau de référence d'un stop franchi : le pire des deux pour le trader, `min(stop, open)`.
pub(super) fn stop_reference(stop: f64, open: f64) -> f64 {
    if stop <= open { stop } else { open }
}

/// Prix de fill d'un stop : la référence moins le slippage de stop (`stop_rate` = slippage + stress).
pub(super) fn stop_price(reference: f64, stop_rate: f64) -> f64 {
    reference * (1.0 - stop_rate)
}

/// Niveau que le plus bas doit atteindre pour remplir une limit d'achat : `limite − tick × kt`, avec
/// `kt` légèrement sous le nombre de ticks pour qu'un franchissement exact soit accepté malgré l'erreur
/// de représentation.
pub(super) fn buy_cross_level(limit: f64, tick: f64, kt: f64) -> f64 {
    limit - tick * kt
}

/// Niveau que le plus haut doit atteindre pour remplir une limit de vente ou un take-profit.
pub(super) fn sell_cross_level(limit: f64, tick: f64, kt: f64) -> f64 {
    limit + tick * kt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bps(value: f64) -> Slippage {
        Slippage::from_bps(value).unwrap()
    }

    #[test]
    fn prix_market_avec_slippage() {
        assert!((market_buy_price(100.0, bps(10.0)) - 100.1).abs() < 1e-9);
        assert!((market_sell_price(110.0, bps(10.0)) - 109.89).abs() < 1e-9);
        assert_eq!(market_buy_price(100.0, Slippage::ZERO), 100.0);
    }

    #[test]
    fn prix_limit_au_meilleur_des_deux() {
        // Achat : un gap sous la limite donne l'open.
        assert_eq!(limit_buy_price(99.0, 98.0), 98.0);
        assert_eq!(limit_buy_price(99.0, 100.0), 99.0);
        assert_eq!(limit_buy_price(99.0, 99.0), 99.0);
        // Vente : un gap au-dessus de la limite donne l'open.
        assert_eq!(limit_sell_price(102.0, 103.0), 103.0);
        assert_eq!(limit_sell_price(102.0, 101.0), 102.0);
    }

    #[test]
    fn stop_au_pire_du_niveau_et_de_l_open() {
        assert_eq!(stop_reference(99.0, 100.5), 99.0);
        assert_eq!(stop_reference(99.0, 98.0), 98.0);
        assert_eq!(stop_price(99.0, 0.0005), 99.0 * (1.0 - 0.0005));
    }

    #[test]
    fn niveaux_de_franchissement() {
        let kt = 1.0 - 1e-6;
        let bas = buy_cross_level(99.0, 0.1, kt);
        assert!((98.9..99.0).contains(&bas));
        let haut = sell_cross_level(102.0, 0.1, kt);
        assert!(102.0 < haut && haut <= 102.1);
    }
}
