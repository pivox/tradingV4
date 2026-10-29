//! Fonctions pures de prix de fill et de franchissement (docs/04-protocole-recherche.md §3.1,
//! docs/03-architecture.md §7.2 et §7.4).
//!
//! L'ordre des opérations flottantes est imposé et ne change pas (aucun `mul_add`) : c'est lui qui
//! rend le résultat identique d'un process à l'autre.

use crate::model::Candle;

// Les prix taker prennent le taux composé `rate = h + s` (demi-spread plus slippage market, s10 §6.2),
// calculé une fois par `LegCost::taker_rate`. Pour le chemin hérité `h = 0.0` et `0.0 + s = s` exactement :
// `reference × (1.0 + rate)` rend les mêmes bits que `Slippage::buy_price`, et `reference × (1.0 − rate)`
// ceux de `Slippage::sell_price`.

/// Prix d'un achat market à l'open `open` : `open × (1 + (h + s))`.
pub(super) fn market_buy_price(open: f64, rate: f64) -> f64 {
    open * (1.0 + rate)
}

/// Prix d'une vente market à l'open (ou d'une clôture forcée au close) : `ref × (1 − (h + s))`.
pub(super) fn market_sell_price(reference: f64, rate: f64) -> f64 {
    reference * (1.0 - rate)
}

/// Prix d'un achat limit déjà marketable à son arrivée et dont l'open franchit encore la limite
/// (`open < limite`) : `min(open × (1 + (h + s)), limite)`. L'ordre prend la liquidité (taker) au prix
/// d'un achat market, plafonné par sa limite.
pub(super) fn marketable_buy_price(limit: f64, open: f64, rate: f64) -> f64 {
    let price = market_buy_price(open, rate);
    if price <= limit { price } else { limit }
}

/// Prix d'une vente limit (ou d'un take-profit) marketable à l'arrivée dont l'open franchit encore la
/// limite (`open > limite`) : `max(open × (1 − (h + s)), limite)`.
pub(super) fn marketable_sell_price(limit: f64, open: f64, rate: f64) -> f64 {
    let price = market_sell_price(open, rate);
    if price >= limit { price } else { limit }
}

/// Prix de référence d'un ordre à son arrivée (`active_from_ms`) : l'open de la 1m qui s'ouvre à cet
/// instant (`candle`, première 1m vue avec `open_time >= active_from_ms`), sinon le close de la dernière
/// 1m antérieure (minute absente de la série). Sans 1m antérieure (jamais le cas dans un run, où un
/// ordre naît après une bougie), l'open de `candle`.
pub(super) fn arrival_price(active_from_ms: i64, candle: &Candle, last_close: Option<f64>) -> f64 {
    if candle.open_time_ms == active_from_ms {
        candle.open
    } else {
        last_close.unwrap_or(candle.open)
    }
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

    use crate::model::Slippage;

    /// Taux composé d'un slippage scalaire seul (`h = 0`) : la valeur que `LegCost::legacy` précalcule.
    fn bps(value: f64) -> f64 {
        Slippage::from_bps(value).unwrap().rate()
    }

    #[test]
    fn prix_market_avec_slippage() {
        assert!((market_buy_price(100.0, bps(10.0)) - 100.1).abs() < 1e-9);
        assert!((market_sell_price(110.0, bps(10.0)) - 109.89).abs() < 1e-9);
        assert_eq!(market_buy_price(100.0, 0.0), 100.0);
    }

    #[test]
    fn le_taux_compose_rejoue_le_slippage_scalaire_au_bit() {
        for value in [0.0, 0.1, 1.0, 5.0, 12.345, 199.9] {
            let slippage = Slippage::from_bps(value).unwrap();
            let rate = 0.0 + slippage.rate();
            for price in [0.5, 99.95, 100.0, 63_412.7] {
                assert_eq!(
                    market_buy_price(price, rate).to_bits(),
                    slippage.buy_price(price).to_bits()
                );
                assert_eq!(
                    market_sell_price(price, rate).to_bits(),
                    slippage.sell_price(price).to_bits()
                );
            }
        }
    }

    #[test]
    fn le_taux_compose_additionne_demi_spread_et_slippage() {
        // h = 5 bp, s = 3 bp : 8 bp de chaque côté.
        let rate = 0.0005 + 0.0003;
        assert_eq!(market_buy_price(100.0, rate), 100.0 * (1.0 + rate));
        assert_eq!(market_sell_price(100.0, rate), 100.0 * (1.0 - rate));
        // Plafonné par la limite : 100.08 au-dessus de 100.05.
        assert_eq!(marketable_buy_price(100.05, 100.0, rate), 100.05);
        assert_eq!(marketable_sell_price(99.95, 100.0, rate), 99.95);
    }

    #[test]
    fn prix_limit_marketable_plafonne_par_la_limite() {
        // Achat : open × (1 + slippage) sous la limite, sinon la limite.
        assert!((marketable_buy_price(100.0, 98.0, bps(10.0)) - 98.098).abs() < 1e-9);
        assert_eq!(marketable_buy_price(98.05, 98.0, bps(10.0)), 98.05);
        assert_eq!(marketable_buy_price(99.0, 98.0, 0.0), 98.0);
        // Vente : open × (1 − slippage) au-dessus de la limite, sinon la limite.
        assert!((marketable_sell_price(100.0, 103.0, bps(10.0)) - 102.897).abs() < 1e-9);
        assert_eq!(marketable_sell_price(102.95, 103.0, bps(10.0)), 102.95);
        assert_eq!(marketable_sell_price(102.0, 103.0, 0.0), 103.0);
    }

    #[test]
    fn prix_d_arrivee() {
        let bougie = |open_time_ms| Candle {
            open_time_ms,
            open: 101.0,
            high: 102.0,
            low: 100.0,
            close: 101.5,
            volume: 1.0,
            trades: None,
        };
        // La 1m d'activation existe : son open.
        assert_eq!(arrival_price(120_000, &bougie(120_000), Some(99.0)), 101.0);
        // Minute absente : le close de la dernière 1m antérieure.
        assert_eq!(arrival_price(120_000, &bougie(180_000), Some(99.0)), 99.0);
        assert_eq!(arrival_price(120_000, &bougie(180_000), None), 101.0);
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
