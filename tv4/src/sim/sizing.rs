//! Sizing au risque (docs/03-architecture.md §7.2, étape 5). Fonction pure, sans type `strategy`.

/// Quantité pour risquer `risk_pct` de `equity` entre `ref_price` et `stop`.
///
/// Exige `equity` fini et > 0, `risk_pct` fini dans `(0, 1]`, `ref_price` et `stop` finis avec
/// `0 < stop < ref_price`. Rend `equity × risk_pct / (ref_price − stop)` s'il est fini et > 0, sinon
/// `None`.
pub fn risk_qty(equity: f64, risk_pct: f64, ref_price: f64, stop: f64) -> Option<f64> {
    if !(equity.is_finite() && equity > 0.0) {
        return None;
    }
    if !(risk_pct.is_finite() && risk_pct > 0.0 && risk_pct <= 1.0) {
        return None;
    }
    if !(ref_price.is_finite() && stop.is_finite() && stop > 0.0 && stop < ref_price) {
        return None;
    }
    let qty = equity * risk_pct / (ref_price - stop);
    (qty.is_finite() && qty > 0.0).then_some(qty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantite_a_la_main() {
        // 300 × 1 % = 3 de risque, 2 d'écart entre 100 et 98 : 1,5 unité.
        assert_eq!(risk_qty(300.0, 0.01, 100.0, 98.0), Some(1.5));
        assert_eq!(risk_qty(1000.0, 0.01, 99.5, 89.5), Some(1.0));
    }

    #[test]
    fn entrees_invalides() {
        let nan = f64::NAN;
        let inf = f64::INFINITY;
        let cas = [
            (
                "stop égal à la référence",
                risk_qty(300.0, 0.01, 100.0, 100.0),
            ),
            ("stop au-dessus", risk_qty(300.0, 0.01, 100.0, 101.0)),
            ("stop nul", risk_qty(300.0, 0.01, 100.0, 0.0)),
            ("stop négatif", risk_qty(300.0, 0.01, 100.0, -1.0)),
            ("pct nul", risk_qty(300.0, 0.0, 100.0, 98.0)),
            ("pct > 1", risk_qty(300.0, 1.5, 100.0, 98.0)),
            ("pct NaN", risk_qty(300.0, nan, 100.0, 98.0)),
            ("equity nulle", risk_qty(0.0, 0.01, 100.0, 98.0)),
            ("equity négative", risk_qty(-5.0, 0.01, 100.0, 98.0)),
            ("equity NaN", risk_qty(nan, 0.01, 100.0, 98.0)),
            ("référence infinie", risk_qty(300.0, 0.01, inf, 98.0)),
            ("stop NaN", risk_qty(300.0, 0.01, 100.0, nan)),
        ];
        for (label, resultat) in cas {
            assert_eq!(resultat, None, "{label}");
        }
    }

    #[test]
    fn pct_egal_a_un_est_accepte() {
        assert_eq!(risk_qty(100.0, 1.0, 10.0, 5.0), Some(20.0));
    }
}
