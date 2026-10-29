//! Coûts de la génération s10 côté simulateur (docs/specs/s10.md §5.2, §6, §7) : compteurs, validation
//! d'un [`LegCost`], décomposition de l'impact d'un fill taker et tirages purs.
//!
//! Aucun état de générateur : un tirage est une fonction de (graine, clé de la jambe, flux, horodatage,
//! discriminant), calculée à l'événement (`model::rng`). Les clés sont des `[u64; 4]` sur la pile, rien
//! n'alloue. Le chemin hérité (`LegCost::legacy`) n'appelle aucune de ces fonctions de tirage.

use serde::Serialize;

use crate::model::costs::{EntryMode, LegCost, StopCost, StopSlippage};
use crate::model::rng::{
    STREAM_MAKER_FILL, STREAM_STOP_SLIPPAGE, STREAM_STOP_STRESS, draw, ts_word, unit,
};
use crate::model::{BPS_PER_UNIT, Ts};

use super::SimError;

/// Compteurs propres à la génération s10, une jambe chacun (s10 §6.5). Ils ne sont pas dans
/// [`SimCounters`](super::SimCounters), dont la sérialisation est épinglée par les goldens de S8 et S9.
/// Écrits seulement par le rapport s10 ; nuls pour un coût hérité.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct CostCounters {
    /// Stops remplis avec un tirage de quantile.
    pub stop_fills_drawn: u64,
    /// Stops dont le tirage de stress a réussi (le prix de stress est alors appliqué s'il est pire).
    pub stop_stress_fills: u64,
    /// Entrées market converties en limit maker (§7.1), comptées à l'acceptation de l'ordre.
    pub maker_entries_converted: u64,
    /// Entrées converties marketables à l'arrivée, donc remplies en taker.
    pub maker_entries_taken_at_arrival: u64,
    /// Tirages de fill d'un ordre (ou d'un take-profit) au repos traversé.
    pub maker_fill_draws: u64,
    /// Tirages réussis.
    pub maker_fills: u64,
    /// Tirages échoués : l'ordre ou le take-profit ne peut plus être rempli (§7.3).
    pub maker_fill_missed: u64,
    /// Entrées converties remplies au repos, à `L`, en maker.
    pub maker_entries_filled_maker: u64,
    /// Somme des parts de spread des fills taker (quote).
    pub spread_cost_total: f64,
    /// Somme des débits de sélection adverse (quote).
    pub adverse_selection_total: f64,
}

/// Valide un coût de jambe avant de le confier au simulateur : taux finis dans leurs bornes. Les
/// bornes de §4.1 sont déjà vérifiées par `resolve_leg` ; un `LegCost` construit à la main (tests,
/// chemin hérité) passe par la même garde, pour qu'aucun prix de fill ne puisse devenir nul ou négatif.
pub(super) fn validate_cost(cost: &LegCost) -> Result<(), SimError> {
    let rate_ok = |rate: f64| rate.is_finite() && (0.0..1.0).contains(&rate);
    if !(rate_ok(cost.half_spread_rate) && rate_ok(cost.market_slippage_rate)) {
        return Err(SimError::InvalidRules {
            reason: "le demi-spread et le slippage market doivent être des taux dans [0, 1[",
        });
    }
    if !rate_ok(cost.half_spread_rate + cost.market_slippage_rate) {
        return Err(SimError::InvalidRules {
            reason: "le demi-spread et le slippage market cumulés doivent rester sous 10 000 bps",
        });
    }
    if !(cost.adverse_selection_rate.is_finite() && cost.adverse_selection_rate >= 0.0) {
        return Err(SimError::InvalidRules {
            reason: "la sélection adverse doit être un taux fini positif ou nul",
        });
    }
    match cost.stop {
        StopCost::Fixed { rate } => {
            if !rate_ok(rate) {
                return Err(SimError::InvalidRules {
                    reason: "le taux fixe d'un stop doit être dans [0, 1[",
                });
            }
        }
        StopCost::Drawn {
            dist,
            stress_probability,
            stress_drop,
        } => {
            let q = dist.quantiles();
            if !(q.p100.is_finite() && q.p100 >= 0.0)
                || !rate_ok(cost.half_spread_rate + q.p100 / BPS_PER_UNIT)
            {
                return Err(SimError::InvalidRules {
                    reason: "le demi-spread et le p100 du slippage de stop doivent rester sous 10 000 bps",
                });
            }
            if !(stress_probability.is_finite() && (0.0..=1.0).contains(&stress_probability)) {
                return Err(SimError::InvalidRules {
                    reason: "la probabilité de stress doit être dans [0, 1]",
                });
            }
            if !(stress_drop.is_finite() && stress_drop > 0.0 && stress_drop < 1.0) {
                return Err(SimError::InvalidRules {
                    reason: "la chute de stress doit être dans ]0, 1[",
                });
            }
        }
    }
    if let EntryMode::Maker {
        ttl_minutes,
        fill_probability,
        offset_rate,
    } = cost.entry
    {
        if ttl_minutes < 1 {
            return Err(SimError::InvalidRules {
                reason: "le TTL maker doit valoir au moins une minute",
            });
        }
        if !(fill_probability.is_finite() && (0.0..=1.0).contains(&fill_probability)) {
            return Err(SimError::InvalidRules {
                reason: "la probabilité de fill maker doit être dans [0, 1]",
            });
        }
        if !(offset_rate.is_finite() && offset_rate >= 0.0)
            || !rate_ok(cost.half_spread_rate + offset_rate)
        {
            return Err(SimError::InvalidRules {
                reason: "le décalage de l'entrée maker doit rester sous 10 000 bps avec le demi-spread",
            });
        }
    }
    Ok(())
}

/// Sépare l'impact d'un fill taker (`qty × |prix − référence|`, défavorable) en part de slippage et part
/// de spread : `spread = min(impact, qty × référence × h)`, `slippage = impact − spread` (s10 §6.3).
/// Rend `(slippage, spread)`. Pour un demi-spread nul, l'impact reste tel quel et le spread vaut `0.0` :
/// l'expression de S9 n'est pas touchée.
pub(super) fn split_impact(
    impact: f64,
    qty: f64,
    reference: f64,
    half_spread_rate: f64,
) -> (f64, f64) {
    if half_spread_rate == 0.0 {
        return (impact, 0.0);
    }
    let cap = qty * reference * half_spread_rate;
    let spread = if cap < impact { cap } else { impact };
    (impact - spread, spread)
}

/// Sélection adverse d'un fill au repos, débitée à part : `qty × L × taux` (s10 §7.4), c'est-à-dire
/// `notional × taux` où `notional = qty × L` est déjà calculé par l'appelant. Rend `0.0` sans calcul
/// quand le taux est nul (profils baseline et adverse) : le terme n'est alors pas écrit.
pub(super) fn adverse_debit(notional: f64, adverse_selection_rate: f64) -> f64 {
    if adverse_selection_rate == 0.0 {
        0.0
    } else {
        notional * adverse_selection_rate
    }
}

/// Prix d'un stop tiré (s10 §5.2) pour la 1m ouverte à `open_time_ms`. `reference = min(stop, open)`.
///
/// 1. `u1` tiré dans le flux `STOP_SLIPPAGE` ; `rate = h + quantile(u1) / 10 000` ; `prix = ref × (1 − rate)` ;
/// 2. `u2` tiré dans le flux `STOP_STRESS` ; si `u2 < stress_probability`, `prix = min(prix, stop × (1 − drop))`.
///
/// Rend `(prix, stress_tiré)`. Le slippage de stop n'est jamais favorable : `quantile ≥ 0`.
pub(super) fn drawn_stop_price(
    cost: &LegCost,
    dist: &StopSlippage,
    stress_probability: f64,
    stress_drop: f64,
    stop: f64,
    reference: f64,
    open_time_ms: Ts,
) -> (f64, bool) {
    let ts = ts_word(open_time_ms);
    let u1 = unit(draw(cost.seed, [cost.leg_key, STREAM_STOP_SLIPPAGE, ts, 0]));
    let rate = cost.half_spread_rate + dist.quantile_bps(u1) / BPS_PER_UNIT;
    let mut price = reference * (1.0 - rate);
    let u2 = unit(draw(cost.seed, [cost.leg_key, STREAM_STOP_STRESS, ts, 0]));
    let stressed = u2 < stress_probability;
    if stressed {
        let floor = stop * (1.0 - stress_drop);
        if floor < price {
            price = floor;
        }
    }
    (price, stressed)
}

/// Tirage de fill d'un ordre au repos du profil maker (s10 §7.3) : un seul par ordre, de clé
/// `[leg_key, MAKER_FILL, active_from_ms, prix.to_bits() ^ (côté << 63)]`, côté 0 pour un achat et 1
/// pour une vente. Réussi si `u < fill_probability`. Le take-profit utilise le côté vente et
/// `tp_active_from_ms`.
pub(super) fn maker_fill_succeeds(
    cost: &LegCost,
    fill_probability: f64,
    active_from_ms: Ts,
    price: f64,
    sell: bool,
) -> bool {
    let side_bit = u64::from(sell) << 63;
    let u = unit(draw(
        cost.seed,
        [
            cost.leg_key,
            STREAM_MAKER_FILL,
            ts_word(active_from_ms),
            price.to_bits() ^ side_bit,
        ],
    ));
    u < fill_probability
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Slippage;
    use crate::model::costs::StopSlippageBps;

    fn drawn(h: f64) -> LegCost {
        LegCost {
            half_spread_rate: h,
            market_slippage_rate: 0.0001,
            stop: StopCost::Drawn {
                dist: StopSlippage::from_quantiles(StopSlippageBps {
                    p50: 5.0,
                    p95: 50.0,
                    p99: 200.0,
                    p100: 200.0,
                }),
                stress_probability: 0.5,
                stress_drop: 0.03,
            },
            entry: EntryMode::Taker,
            adverse_selection_rate: 0.0,
            leg_key: 7,
            seed: 1,
        }
    }

    #[test]
    fn le_cout_herite_est_valide() {
        let cost = LegCost::legacy(Slippage::from_bps(10.0).unwrap(), 5.0);
        assert!(validate_cost(&cost).is_ok());
    }

    #[test]
    fn un_cout_hors_bornes_est_refuse() {
        let mut cost = drawn(0.0005);
        assert!(validate_cost(&cost).is_ok());
        cost.half_spread_rate = 1.0;
        assert!(validate_cost(&cost).is_err());
        cost.half_spread_rate = f64::NAN;
        assert!(validate_cost(&cost).is_err());
        let mut cost = drawn(0.0005);
        cost.adverse_selection_rate = -1e-4;
        assert!(validate_cost(&cost).is_err());
        let mut cost = drawn(0.0005);
        cost.stop = StopCost::Drawn {
            dist: StopSlippage::from_quantiles(StopSlippageBps {
                p50: 1.0,
                p95: 2.0,
                p99: 3.0,
                p100: 10_000.0,
            }),
            stress_probability: 0.1,
            stress_drop: 0.03,
        };
        assert!(validate_cost(&cost).is_err(), "h + p100 doit rester sous 1");
        let mut cost = drawn(0.0005);
        cost.entry = EntryMode::Maker {
            ttl_minutes: 0,
            fill_probability: 0.5,
            offset_rate: 0.0,
        };
        assert!(validate_cost(&cost).is_err());
        cost.entry = EntryMode::Maker {
            ttl_minutes: 5,
            fill_probability: 1.5,
            offset_rate: 0.0,
        };
        assert!(validate_cost(&cost).is_err());
        cost.entry = EntryMode::Maker {
            ttl_minutes: 5,
            fill_probability: 0.5,
            offset_rate: 0.9999,
        };
        assert!(validate_cost(&cost).is_err());
        cost.entry = EntryMode::Maker {
            ttl_minutes: 5,
            fill_probability: 0.5,
            offset_rate: 0.0001,
        };
        assert!(validate_cost(&cost).is_ok());
    }

    #[test]
    fn la_decomposition_somme_a_l_impact() {
        // Impact de 10 dont le spread plafonné vaut 4 : slippage 6.
        let (slippage, spread) = split_impact(10.0, 2.0, 100.0, 0.02);
        assert_eq!(spread, 4.0);
        assert_eq!(slippage, 6.0);
        // Impact inférieur au plafond : tout est du spread.
        let (slippage, spread) = split_impact(1.0, 2.0, 100.0, 0.02);
        assert_eq!((slippage, spread), (0.0, 1.0));
        // Demi-spread nul : expression de S9, spread à 0.
        let (slippage, spread) = split_impact(3.25, 2.0, 100.0, 0.0);
        assert_eq!(
            (slippage.to_bits(), spread.to_bits()),
            (3.25f64.to_bits(), 0)
        );
    }

    #[test]
    fn le_debit_de_selection_adverse_est_nul_sans_taux() {
        assert_eq!(adverse_debit(1000.0, 0.0), 0.0);
        assert_eq!(adverse_debit(1000.0, 0.00015), 1000.0 * 0.00015);
    }

    #[test]
    fn le_prix_de_stop_tire_n_est_jamais_meilleur_que_la_reference_moins_le_demi_spread() {
        let cost = drawn(0.0005);
        let StopCost::Drawn {
            dist,
            stress_probability,
            stress_drop,
        } = cost.stop
        else {
            unreachable!("drawn() rend un stop tiré");
        };
        for ts in 0..500i64 {
            let (price, _) = drawn_stop_price(
                &cost,
                &dist,
                stress_probability,
                stress_drop,
                100.0,
                100.0,
                ts * 60_000,
            );
            assert!(price <= 100.0 * (1.0 - cost.half_spread_rate), "{price}");
            assert!(price > 0.0);
        }
    }

    #[test]
    fn le_tirage_de_fill_depend_du_cote_et_de_la_cle() {
        let cost = drawn(0.0);
        let mut differs = 0;
        for ts in 0..200i64 {
            let buy = maker_fill_succeeds(&cost, 0.5, ts * 60_000, 100.0, false);
            let sell = maker_fill_succeeds(&cost, 0.5, ts * 60_000, 100.0, true);
            assert_eq!(
                buy,
                maker_fill_succeeds(&cost, 0.5, ts * 60_000, 100.0, false)
            );
            differs += usize::from(buy != sell);
        }
        assert!(differs > 0, "le côté entre dans la clé du tirage");
        assert!(!maker_fill_succeeds(&cost, 0.0, 0, 100.0, false));
        assert!(maker_fill_succeeds(&cost, 1.0, 0, 100.0, false));
    }
}
