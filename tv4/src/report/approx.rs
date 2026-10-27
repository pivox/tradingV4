//! Bloc `approx` de `metrics.json` : les seules valeurs qui passent par la libm (`ln`, `powf`).
//!
//! Ce fichier est le **seul** de la crate autorisé à appeler `ln`, `exp`, `powf` et leurs cousines
//! (`tests/architecture.rs` le vérifie) : la libm n'est exacte au bit ni d'une machine à l'autre ni
//! d'une version de glibc à l'autre (docs/03 §7.4). Les résultats sont donc comparés à une tolérance
//! de [`ULP_TOLERANCE`] ULP en CI, jamais à l'octet entre deux architectures.
//!
//! `return_annualized` vaut `exp_m1(y × ln(ratio))` et non `ratio.powf(y) − 1` : près de 0, la
//! soustraction de 1 amplifie l'écart d'1 ULP de `powf` (ratio 1,0000001 sur un an : plusieurs
//! millions d'ULP), alors que `exp_m1` reste bien conditionnée. L'erreur de la formule retenue vient
//! de l'argument `y × ln(ratio)` (deux arrondis) ; elle croît avec sa valeur absolue `|x|`, d'environ
//! `|x|` ULP.

use serde::Serialize;

/// Écart maximal admis, en ULP, entre une valeur du bloc et la référence exacte (Python `decimal`,
/// 50 chiffres), pour chaque valeur du bloc, `return_annualized` comprise.
///
/// Origine de la borne : écart maximal mesuré sur la table de référence des tests de ce fichier
/// (23 cas, glibc 2.39) = 18 ULP, atteint par le cas extrême ratio 10 sur 30 jours (`|x|` = 28) ;
/// `ln_return` y est exact (0 ULP) et `return_annualized` tient 2 ULP au plus pour `|x|` ≤ 3
/// (annualisé jusqu'à +1 900 %), le cas courant. Valeur arrondie à 20. Un ULP de plus que 1 sur
/// `return_annualized` est le prix du conditionnement : `powf(...) − 1` tenait 1 ULP sur `powf`,
/// mais des centaines à des millions d'ULP une fois le 1 retranché. La libm d'Apple n'est pas
/// mesurée ici : la relire au premier `cargo test` sur le Mac.
pub const ULP_TOLERANCE: u32 = 20;
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
    /// `ratio^(année / durée) − 1`, calculé `exp_m1(y × ln(ratio))` ; `null` sous 30 jours, si le ratio est ≤ 0 ou si le résultat n'est
    /// pas fini.
    pub return_annualized: Option<f64>,
}

/// Calcule le bloc pour un capital initial, une equity finale et la durée de la fenêtre.
pub fn compute(initial_cash: f64, equity_final: f64, span_ms: i64) -> Approx {
    let ratio = equity_final / initial_cash;
    let valid = ratio.is_finite() && ratio > 0.0;
    let ln_return = valid.then(|| ratio.ln()).filter(|v| v.is_finite());
    let return_annualized = (valid && span_ms >= MIN_SPAN_MS)
        .then(|| (YEAR_MS / span_ms as f64 * ratio.ln()).exp_m1())
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

    /// Capital initial de tous les cas de la table.
    const INITIAL_CASH: f64 = 300.0;

    /// Références (equity finale, durée en ms, `ln_return`, `return_annualized`) calculées avec Python
    /// `decimal` à 50 chiffres, arrondies au `f64` le plus proche par `float(str(d))`. Le ratio et
    /// l'exposant sont ceux de [`compute`] : `ratio = equity / 300` et `y = YEAR_MS / durée` en
    /// divisions IEEE, puis `ln_return = ln(ratio)` et `return_annualized = exp(y × ln(ratio)) − 1`
    /// exacts. Commande (Python 3, bibliothèque standard seule) :
    ///
    /// ```text
    /// python3 - <<'PY'
    /// from decimal import Decimal, getcontext
    /// getcontext().prec = 50
    /// f = lambda d: float(str(d))
    /// def ref(equity, span_ms):
    ///     ratio = equity / 300.0                 # division IEEE, comme en Rust
    ///     y = 31_557_600_000.0 / float(span_ms)  # idem
    ///     ln = Decimal(ratio).ln()
    ///     ann = f((Decimal(y) * ln).exp() - 1) if span_ms >= 30 * 86_400_000 else None
    ///     return f(ln), ann
    /// PY
    /// ```
    ///
    /// Les cas couvrent le ratio de la fixture S8 (0,98494, 7,4 jours : `null` annualisé), des ratios
    /// de 0,5 à 10, des ratios proches de 1 (là où `powf(...) − 1` échouait) et des durées de 30 à
    /// 3 650 jours. Les trois derniers sont des cas extrêmes (`|x|` de 13 à 28).
    #[allow(clippy::type_complexity)]
    const TABLE: [(f64, i64, f64, Option<f64>); 23] = [
        // fixture : ratio 0,98494, 7,4 j, annualisé null
        (295.4821433837915, 637200000, -0.015174068117169492, None),
        // ratio 0,5 sur 30 j
        (
            150.0,
            2592000000,
            -std::f64::consts::LN_2,
            Some(-0.9997837481642816),
        ),
        // ratio 0,5 sur 730 j
        (
            150.0,
            63072000000,
            -std::f64::consts::LN_2,
            Some(-0.29306105131452115),
        ),
        // ratio 2 sur 3 650 j
        (
            600.0,
            315360000000,
            std::f64::consts::LN_2,
            Some(0.07182434708347513),
        ),
        // ratio 2 sur 365 j
        (
            600.0,
            31536000000,
            std::f64::consts::LN_2,
            Some(1.0009497421168565),
        ),
        // ratio 2 sur 100 j
        (
            600.0,
            8640000000,
            std::f64::consts::LN_2,
            Some(11.575117715244136),
        ),
        // ratio 1,001 sur 730 j
        (
            300.3,
            63072000000,
            0.000999500333083645,
            Some(0.0005002175282588024),
        ),
        // ratio 1,01 sur 365 j
        (
            303.0,
            31536000000,
            0.009950330853168092,
            Some(0.010006883471512386),
        ),
        // ratio 1,01 sur 31 j
        (
            303.0,
            2678400000,
            0.009950330853168092,
            Some(0.12438628902530488),
        ),
        // ratio 1,0000001 sur 365 j
        (
            300.00003,
            31536000000,
            9.999999483634247e-08,
            Some(1.0006849299034195e-07),
        ),
        // ratio 0,9999999 sur 365 j
        (
            299.99997,
            31536000000,
            -1.0000000494736474e-07,
            Some(-1.000684930945863e-07),
        ),
        // ratio 1,1 sur 30 j
        (
            330.0,
            2592000000,
            0.09531017980432493,
            Some(2.19121409718457),
        ),
        // ratio 1,5 sur 3 650 j
        (
            450.0,
            315360000000,
            0.4054651081081644,
            Some(0.04140866515773347),
        ),
        // ratio 0,9 sur 730 j
        (
            270.0,
            63072000000,
            -0.10536051565782628,
            Some(-0.051350932072162916),
        ),
        // ratio 1,0423 sur 31 j
        (
            312.69,
            2678400000,
            0.041429809763139556,
            Some(0.6292775735996655),
        ),
        // ratio 3 sur 3 650 j
        (
            900.0,
            315360000000,
            1.0986122886681098,
            Some(0.11620716257976105),
        ),
        // ratio 3 sur 100 j
        (
            900.0,
            8640000000,
            1.0986122886681098,
            Some(54.29493894511652),
        ),
        // ratio 1,2 sur 100 j
        (
            360.0,
            8640000000,
            0.1823215567939546,
            Some(0.946298738649169),
        ),
        // ratio 1,00333 sur 90 j
        (
            301.0,
            7776000000,
            0.0033277900926747457,
            Some(0.013596889706929358),
        ),
        // ratio 0,99 sur 90 j
        (
            297.0,
            7776000000,
            -0.01005033585350145,
            Some(-0.039966993178387714),
        ),
        // ratio 3 sur 31 j (|x| = 12,9)
        (
            900.0,
            2678400000,
            1.0986122886681098,
            Some(418374.02486098546),
        ),
        // ratio 5 sur 31 j (|x| = 19,0)
        (
            1500.0,
            2678400000,
            1.6094379124341003,
            Some(171966941.77105084),
        ),
        // ratio 10 sur 30 j (|x| = 28,0)
        (
            3000.0,
            2592000000,
            std::f64::consts::LN_10,
            Some(1496235656093.4358),
        ),
    ];

    #[test]
    fn chaque_valeur_du_bloc_tient_la_tolerance_de_la_reference_decimale() {
        let mut pire = 0;
        for (equity, span_ms, ln_attendu, annualise_attendu) in TABLE {
            let a = compute(INITIAL_CASH, equity, span_ms);
            assert_eq!(a.ulp_tolerance, ULP_TOLERANCE);
            let ln_obtenu = a.ln_return.expect("ratio positif");
            let distance = ulp_distance(ln_obtenu, ln_attendu).expect("même signe, finis");
            assert!(
                distance <= u64::from(ULP_TOLERANCE),
                "ln_return({equity}) = {ln_obtenu:?}, attendu {ln_attendu:?}, {distance} ULP"
            );
            pire = pire.max(distance);
            match (a.return_annualized, annualise_attendu) {
                (None, None) => {}
                (Some(obtenu), Some(attendu)) => {
                    let distance = ulp_distance(obtenu, attendu).expect("même signe, finis");
                    assert!(
                        distance <= u64::from(ULP_TOLERANCE),
                        "return_annualized({equity}, {span_ms}) = {obtenu:?}, attendu {attendu:?}, \
                         {distance} ULP"
                    );
                    pire = pire.max(distance);
                }
                (obtenu, attendu) => {
                    panic!("{equity} sur {span_ms} ms : {obtenu:?}, attendu {attendu:?}")
                }
            }
        }
        // La borne documentée est celle qui est mesurée sur glibc : elle ne doit pas y être desserrée
        // sans raison. Ailleurs (libm d'Apple), une libm plus précise ne doit pas faire échouer le test.
        if cfg!(all(target_os = "linux", target_env = "gnu")) {
            assert!(
                pire + 2 >= u64::from(ULP_TOLERANCE),
                "la table n'atteint plus que {pire} ULP : resserrer ULP_TOLERANCE ({ULP_TOLERANCE})"
            );
        }
    }

    #[test]
    fn exp_m1_tient_la_ou_powf_moins_un_echoue() {
        // Ratio 1,0000001 sur un an : `powf(...) − 1` perd des millions d'ULP, `exp_m1` aucun.
        let (equity, span_ms) = (300.00003, 365 * 86_400_000);
        let ratio = equity / INITIAL_CASH;
        let naif = ratio.powf(YEAR_MS / span_ms as f64) - 1.0;
        let (_, _, _, attendu) = TABLE
            .iter()
            .copied()
            .find(|c| c.0 == equity && c.1 == span_ms)
            .expect("cas présent dans la table");
        let attendu = attendu.expect("un an suffit");
        let retenu = compute(INITIAL_CASH, equity, span_ms)
            .return_annualized
            .expect("un an suffit");
        assert!(ulp_distance(naif, attendu).unwrap() > 1_000);
        assert!(ulp_distance(retenu, attendu).unwrap() <= 1);
    }

    #[test]
    fn les_exposants_de_la_formule_sont_ceux_de_la_division_ieee() {
        for (jours, y) in [
            (30i64, 12.175),
            (365, 1.0006849315068493),
            (3650, 0.10006849315068493),
            (100, 3.6525),
            (730, 0.5003424657534247),
            (31, 11.78225806451613),
        ] {
            let span = jours * 86_400_000;
            assert_eq!(YEAR_MS / span as f64, y, "{jours} j");
        }
    }

    #[test]
    fn compute_reproduit_les_cas_de_reference() {
        // 300 → 150 (ratio 0,5) sur 30 jours.
        let a = compute(300.0, 150.0, 30 * 86_400_000);
        assert_eq!(a.ln_return, Some(-std::f64::consts::LN_2));
        assert!(a.return_annualized.is_some());
        assert_eq!(a.ulp_tolerance, ULP_TOLERANCE);
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
