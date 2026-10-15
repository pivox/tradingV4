//! Données SYNTHÉTIQUES (aucune donnée de marché) pour les goldens de grille : une oscillation
//! sinusoïdale obtenue par rotation exacte d'un vecteur, sans libm (docs/03 §7.4).
//!
//! Le point `(x, y)` tourne d'un angle de 2π/240 à chaque pas : `y` vaut `sin(2π k / 240)` à l'erreur
//! d'arrondi près, et la suite est reproductible au bit sur toute architecture IEEE 754.

use tv4::model::Candle;

use super::T0;

/// `cos(2π/240)`, littéral : aucun appel à la libm.
pub const COS: f64 = 0.9996573249755573;

/// `sin(2π/240)`, littéral.
pub const SIN: f64 = 0.02617694830787315;

/// Les `n + 1` valeurs `y_0..y_n` de la rotation, avec `y_0 = 0`.
pub fn oscillation(n: usize) -> Vec<f64> {
    let mut x = 1.0_f64;
    let mut y = 0.0_f64;
    let mut ys = Vec::with_capacity(n + 1);
    ys.push(y);
    for _ in 0..n {
        // Les deux nouvelles valeurs viennent des anciennes `x` et `y`.
        let nx = x * COS - y * SIN;
        let ny = x * SIN + y * COS;
        x = nx;
        y = ny;
        ys.push(y);
    }
    ys
}

/// Bougies 1m consécutives (volume 1000) entre les points `mid[i]` et `mid[i + 1]` : open au premier,
/// close au second, mèches de 2 bps de part et d'autre.
pub fn bougies_depuis_mid(mid: &[f64]) -> Vec<Candle> {
    let mut candles = Vec::with_capacity(mid.len().saturating_sub(1));
    for i in 0..mid.len().saturating_sub(1) {
        let open = mid[i];
        let close = mid[i + 1];
        let high = open.max(close) * (1.0 + 0.0002);
        let low = open.min(close) * (1.0 - 0.0002);
        candles.push(Candle {
            open_time_ms: T0 + i as i64 * 60_000,
            open,
            high,
            low,
            close,
            volume: 1000.0,
            trades: None,
        });
    }
    candles
}

/// `n` bougies : `mid[i] = 100 × (1 + amplitude × y_i)`, période de 240 minutes.
pub fn sinusoide(n: usize, amplitude: f64) -> Vec<Candle> {
    let ys = oscillation(n);
    let mid: Vec<f64> = ys.iter().map(|y| 100.0 * (1.0 + amplitude * y)).collect();
    bougies_depuis_mid(&mid)
}

/// `n` bougies : la sinusoïde d'amplitude `amplitude` plus une baisse linéaire de `baisse` (en
/// fraction du prix) étalée de l'index `i0` à `n`, rien avant `i0`.
pub fn tendance_baissiere(n: usize, i0: usize, amplitude: f64, baisse: f64) -> Vec<Candle> {
    let ys = oscillation(n);
    let mid: Vec<f64> = ys
        .iter()
        .enumerate()
        .map(|(i, y)| {
            let rampe = if i > i0 {
                (i - i0) as f64 / (n - i0) as f64
            } else {
                0.0
            };
            100.0 * ((1.0 + amplitude * y) - baisse * rampe)
        })
        .collect();
    bougies_depuis_mid(&mid)
}
