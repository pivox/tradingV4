//! Bloc `approx` de `metrics.json` : les seules valeurs qui passent par la libm (`ln`, `powf`).
//!
//! Ce fichier est le **seul** de la crate autorisé à appeler `ln`, `exp`, `powf` et leurs cousines
//! (`tests/architecture.rs` le vérifie) : la libm n'est exacte au bit ni d'une machine à l'autre ni
//! d'une version de glibc à l'autre (docs/03 §7.4). Les résultats sont donc comparés à une tolérance
//! de [`ULP_TOLERANCE`] ULP en CI, jamais à l'octet entre deux architectures.
//!
//! La glibc annonce moins de 1 ULP pour `exp`, `log` et `pow` (à valider pour la libm d'Apple au
//! premier `cargo test` sur le Mac).

use serde::Serialize;

/// Écart maximal admis, en ULP, entre deux machines pour chaque valeur du bloc.
pub const ULP_TOLERANCE: u32 = 1;
/// Année de 365,25 jours en millisecondes.
const YEAR_MS: f64 = 31_557_600_000.0;
/// En dessous de 30 jours, annualiser n'a pas de sens : `return_annualized` vaut `null`.
const MIN_SPAN_MS: i64 = 30 * 86_400_000;

/// Contenu du bloc `approx`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Approx {
    pub ulp_tolerance: u32,
    /// `ln(equity_final / initial_cash)` ; `null` si le ratio est ≤ 0.
    pub ln_return: Option<f64>,
    /// `ratio^(année / durée) − 1` ; `null` sous 30 jours, si le ratio est ≤ 0 ou si le résultat n'est
    /// pas fini.
    pub return_annualized: Option<f64>,
}

/// Calcule le bloc pour un capital initial, une equity finale et la durée de la fenêtre.
pub fn compute(initial_cash: f64, equity_final: f64, span_ms: i64) -> Approx {
    let ratio = equity_final / initial_cash;
    let valid = ratio.is_finite() && ratio > 0.0;
    let ln_return = valid.then(|| ratio.ln()).filter(|v| v.is_finite());
    let return_annualized = (valid && span_ms >= MIN_SPAN_MS)
        .then(|| ratio.powf(YEAR_MS / span_ms as f64) - 1.0)
        .filter(|v| v.is_finite());
    Approx {
        ulp_tolerance: ULP_TOLERANCE,
        ln_return,
        return_annualized,
    }
}

/// Distance en ULP entre deux flottants finis de même signe (différence des représentations
/// binaires) ; `None` pour des signes différents ou des valeurs non finies. Deux zéros valent 0.
pub fn ulp_distance(a: f64, b: f64) -> Option<u64> {
    if !(a.is_finite() && b.is_finite()) {
        return None;
    }
    if a == 0.0 && b == 0.0 {
        return Some(0);
    }
    if a.is_sign_negative() != b.is_sign_negative() {
        return None;
    }
    Some(a.to_bits().abs_diff(b.to_bits()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Ln,
        Exp,
        Powf,
    }

    /// Références calculées avec Python `decimal` à 60 chiffres, arrondies au `f64` le plus proche par
    /// `float(str(d))`. Commande exacte (Python 3, bibliothèque standard seule) :
    ///
    /// ```text
    /// python3 - <<'PY'
    /// from decimal import Decimal, getcontext
    /// getcontext().prec = 60
    /// f = lambda d: float(str(d))
    /// YEAR_MS = 31_557_600_000
    /// ln = lambda x: f(Decimal(x).ln())
    /// exp = lambda x: f(Decimal(x).exp())
    /// # powf(x, y) = exp(y ln x) avec y = YEAR_MS / durée_ms en double (division IEEE) :
    /// powf = lambda x, y: f((Decimal(y) * Decimal(x).ln()).exp())
    /// PY
    /// ```
    ///
    /// Les durées sont 30 j, 365 j, 3 650 j, 100 j, 730 j et 31 j ; `y = YEAR_MS / (jours × 86 400 000)`.
    const TABLE: [(Op, f64, f64, f64); 16] = [
        (Op::Ln, 0.5, 0.0, -std::f64::consts::LN_2),
        (Op::Ln, 0.98, 0.0, -0.020202707317519466),
        (Op::Ln, 1.0423, 0.0, 0.041429809763139556),
        (Op::Ln, 1.5, 0.0, 0.4054651081081644),
        (Op::Ln, 2.0, 0.0, std::f64::consts::LN_2),
        (Op::Ln, 3.0, 0.0, 1.0986122886681098),
        (Op::Exp, -2.0, 0.0, 0.1353352832366127),
        (Op::Exp, -0.015, 0.0, 0.9851119396030626),
        (Op::Exp, 0.7, 0.0, 2.0137527074704766),
        (Op::Exp, 1.0986122886681098, 0.0, 3.0000000000000004),
        (Op::Powf, 0.5, 12.175, 0.00021625183571834965),
        (Op::Powf, 1.5, 1.0006849315068493, 1.5004166315911227),
        (Op::Powf, 3.0, 0.10006849315068493, 1.1162071625797612),
        (Op::Powf, 1.2, 3.6525, 1.946298738649169),
        (Op::Powf, 0.9, 0.5003424657534247, 0.9486490679278371),
        (Op::Powf, 1.0423, 11.78225806451613, 1.6292775735996654),
    ];

    #[test]
    fn la_libm_reste_a_un_ulp_de_la_reference_decimale() {
        for (op, x, y, attendu) in TABLE {
            let obtenu = match op {
                Op::Ln => x.ln(),
                Op::Exp => x.exp(),
                Op::Powf => x.powf(y),
            };
            let distance = ulp_distance(obtenu, attendu).expect("même signe, finis");
            assert!(
                distance <= u64::from(ULP_TOLERANCE),
                "{op:?}({x}, {y}) = {obtenu:?}, attendu {attendu:?}, {distance} ULP"
            );
        }
    }

    #[test]
    fn les_exposants_de_la_table_sont_ceux_de_la_formule() {
        for (ratio, jours, y) in [
            (0.5, 30i64, 12.175),
            (1.5, 365, 1.0006849315068493),
            (3.0, 3650, 0.10006849315068493),
            (1.2, 100, 3.6525),
            (0.9, 730, 0.5003424657534247),
            (1.0423, 31, 11.78225806451613),
        ] {
            let span = jours * 86_400_000;
            assert_eq!(YEAR_MS / span as f64, y, "{ratio} sur {jours} j");
        }
    }

    #[test]
    fn compute_reproduit_les_cas_de_reference() {
        // 300 → 150 (ratio 0,5) sur 30 jours.
        let a = compute(300.0, 150.0, 30 * 86_400_000);
        assert_eq!(a.ln_return, Some(-std::f64::consts::LN_2));
        let annualise = a.return_annualized.expect("30 jours suffisent");
        let reference = 0.00021625183571834965 - 1.0;
        assert!(
            ulp_distance(annualise, reference).unwrap() <= 2,
            "{annualise}"
        );
        assert_eq!(a.ulp_tolerance, 1);
    }

    #[test]
    fn indefini_sous_30_jours_ou_sans_ratio_positif() {
        let court = compute(300.0, 330.0, 29 * 86_400_000);
        assert!(court.ln_return.is_some());
        assert_eq!(court.return_annualized, None);
        let ruine = compute(300.0, 0.0, 365 * 86_400_000);
        assert_eq!((ruine.ln_return, ruine.return_annualized), (None, None));
        let nan = compute(300.0, f64::NAN, 365 * 86_400_000);
        assert_eq!((nan.ln_return, nan.return_annualized), (None, None));
    }

    #[test]
    fn distance_en_ulp() {
        assert_eq!(ulp_distance(1.0, 1.0), Some(0));
        assert_eq!(
            ulp_distance(1.0, f64::from_bits(1.0f64.to_bits() + 1)),
            Some(1)
        );
        assert_eq!(ulp_distance(0.0, -0.0), Some(0));
        assert_eq!(ulp_distance(1.0, -1.0), None);
        assert_eq!(ulp_distance(f64::NAN, 1.0), None);
    }
}
