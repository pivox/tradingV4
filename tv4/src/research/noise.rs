//! Contexte de sélection sous la nullité : maximum de N normales indépendantes.
//!
//! Calcul hors simulation, approximatif (tolérance numérique testée), sans RNG ni horloge.
//! L'inverse normale utilise l'approximation rationnelle d'Acklam. On travaille en
//! probabilité de queue pour conserver la précision même quand N approche u64::MAX.

use serde::{Deserialize, Serialize};

/// Repères du Sharpe annualisé de l'excédent sur le benchmark à exposition égale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseContext {
    pub n: u64,
    /// Calcul du clustering réservé à S13.
    pub n_eff: Option<u64>,
    pub selection_years: f64,
    pub expected: Option<f64>,
    pub approx_q90: Option<f64>,
    pub q95: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("la fenêtre de sélection doit être finie et strictement positive")]
pub struct InvalidSelectionWindow;

/// BLP pour l'espérance, borne fermée indicative pour q90, quantile exact du
/// maximum de N normales pour q95. Les trois résultats restent des approximations
/// du Sharpe réel : ils ne constituent pas une porte d'acceptation de stratégie.
pub fn noise_context(n: u64, years: f64) -> Result<NoiseContext, InvalidSelectionWindow> {
    if !years.is_finite() || years <= 0.0 {
        return Err(InvalidSelectionWindow);
    }
    let mut value = NoiseContext {
        n,
        n_eff: None,
        selection_years: years,
        expected: None,
        approx_q90: None,
        q95: None,
    };
    if n == 0 {
        return Ok(value);
    }
    let count = n as f64;
    let scale = years.sqrt();
    const GAMMA: f64 = 0.577_215_664_901_532_9;
    value.expected = Some(if n == 1 {
        0.0
    } else {
        ((1.0 - GAMMA) * inverse_survival(1.0 / count)
            + GAMMA * inverse_survival(1.0 / (count * std::f64::consts::E)))
            / scale
    });
    value.approx_q90 = Some((2.0 * count.ln()).sqrt() / scale);
    // 1 - 0.95^(1/N), sans soustraction de deux nombres proches de 1.
    let tail = -(0.95_f64.ln() / count).exp_m1();
    value.q95 = Some(inverse_survival(tail) / scale);
    Ok(value)
}

fn polynomial(coefficients: &[f64], x: f64) -> f64 {
    coefficients.iter().fold(0.0, |acc, c| acc * x + c)
}

/// Inverse de Q(x) = 1 - Phi(x), pour les probabilités de queue dans (0, 0.5].
/// Coefficients de l'approximation rationnelle d'Acklam ; erreur relative sur le
/// quantile inférieure à 1.2e-9 dans ce domaine. Aucun 1-p n'est calculé en queue.
fn inverse_survival(p: f64) -> f64 {
    debug_assert!(p > 0.0 && p <= 0.5);
    if p < 0.024_25 {
        const C: [f64; 6] = [
            -7.784_894_002_430_293e-3,
            -3.223_964_580_411_365e-1,
            -2.400_758_277_161_838,
            -2.549_732_539_343_734,
            4.374_664_141_464_968,
            2.938_163_982_698_783,
        ];
        const D: [f64; 4] = [
            7.784_695_709_041_462e-3,
            3.224_671_290_700_398e-1,
            2.445_134_137_142_996,
            3.754_408_661_907_416,
        ];
        let q = (-2.0 * p.ln()).sqrt();
        return -polynomial(&C, q) / (polynomial(&D, q) * q + 1.0);
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    let q = p - 0.5;
    let r = q * q;
    -polynomial(&A, r) * q / (polynomial(&B, r) * r + 1.0)
}
