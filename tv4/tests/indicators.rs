//! Tests d'intégration des indicateurs : parité avec des implémentations naïves O(n·k) (proptest),
//! stabilité par préfixe I1 (docs/03-architecture.md §7.5), reprise en flux et reset.

mod common;

use std::collections::BTreeSet;

use proptest::prelude::*;
use tv4::indicators::{Atr, Ema, Indicator, Rsi, Sma, Vwap, batch, batch_into};
use tv4::model::Candle;

const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 86_400_000;
/// 2024-01-01T00:00:00Z.
const MIDNIGHT_MS: i64 = 1_704_067_200_000;

// ---------------------------------------------------------------------------------------------
// Générateur synthétique
// ---------------------------------------------------------------------------------------------

#[test]
fn le_generateur_produit_des_bougies_valides_et_deterministes() {
    let a = common::synthetic_candles(10_000, 7, MIDNIGHT_MS, 0.05);
    assert_eq!(a.len(), 10_000);
    for candle in &a {
        candle.validate().unwrap();
    }
    assert!(a.windows(2).all(|w| w[0].open_time_ms < w[1].open_time_ms));
    assert!(
        a.windows(2)
            .any(|w| w[1].open_time_ms - w[0].open_time_ms > MINUTE_MS),
        "des trous sont attendus"
    );
    assert!(
        a.iter().any(|c| c.volume == 0.0),
        "des volumes nuls sont attendus"
    );
    assert_eq!(a, common::synthetic_candles(10_000, 7, MIDNIGHT_MS, 0.05));
    assert_ne!(a, common::synthetic_candles(10_000, 8, MIDNIGHT_MS, 0.05));

    let dense = common::synthetic_candles(1_000, 7, MIDNIGHT_MS, 0.0);
    assert!(
        dense
            .windows(2)
            .all(|w| w[1].open_time_ms - w[0].open_time_ms == MINUTE_MS)
    );
}

// ---------------------------------------------------------------------------------------------
// Implémentations naïves : tout est recalculé depuis le début pour chaque index, sans état.
// ---------------------------------------------------------------------------------------------

fn naive_sma(xs: &[f64], n: usize) -> Vec<f64> {
    (0..xs.len())
        .map(|i| {
            if i + 1 < n {
                f64::NAN
            } else {
                xs[i + 1 - n..=i].iter().fold(0.0, |s, &x| s + x) / n as f64
            }
        })
        .collect()
}

fn naive_ema(xs: &[f64], n: usize) -> Vec<f64> {
    let nf = n as f64;
    let alpha = 2.0 / (nf + 1.0);
    (0..xs.len())
        .map(|i| {
            if i + 1 < n {
                return f64::NAN;
            }
            let mut value = xs[..n].iter().fold(0.0, |s, &x| s + x) / nf;
            for &x in &xs[n..=i] {
                value += alpha * (x - value);
            }
            value
        })
        .collect()
}

fn rsi_from(g: f64, l: f64) -> f64 {
    let s = g + l;
    if s == 0.0 { 50.0 } else { 100.0 * (g / s) }
}

fn gain(d: f64) -> f64 {
    if d > 0.0 { d } else { 0.0 }
}

fn loss(d: f64) -> f64 {
    if d < 0.0 { -d } else { 0.0 }
}

fn naive_rsi(xs: &[f64], n: usize) -> Vec<f64> {
    let nf = n as f64;
    let nm1 = (n - 1) as f64;
    let deltas: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).collect();
    (0..xs.len())
        .map(|i| {
            if i < n {
                return f64::NAN;
            }
            // deltas[j - 1] est la variation de l'index j ; l'amorçage porte sur les variations 1 à n.
            let mut g = deltas[..n].iter().fold(0.0, |s, &d| s + gain(d)) / nf;
            let mut l = deltas[..n].iter().fold(0.0, |s, &d| s + loss(d)) / nf;
            for &d in &deltas[n..i] {
                g = (g * nm1 + gain(d)) / nf;
                l = (l * nm1 + loss(d)) / nf;
            }
            rsi_from(g, l)
        })
        .collect()
}

fn naive_true_ranges(candles: &[Candle]) -> Vec<f64> {
    candles
        .iter()
        .enumerate()
        .map(|(j, c)| {
            if j == 0 {
                c.high - c.low
            } else {
                let pc = candles[j - 1].close;
                (c.high - c.low)
                    .max((c.high - pc).abs())
                    .max((c.low - pc).abs())
            }
        })
        .collect()
}

fn naive_atr(candles: &[Candle], n: usize) -> Vec<f64> {
    let nf = n as f64;
    let nm1 = (n - 1) as f64;
    let tr = naive_true_ranges(candles);
    (0..candles.len())
        .map(|i| {
            if i + 1 < n {
                return f64::NAN;
            }
            let mut value = tr[..n].iter().fold(0.0, |s, &t| s + t) / nf;
            for &t in &tr[n..=i] {
                value = (value * nm1 + t) / nf;
            }
            value
        })
        .collect()
}

fn naive_vwap(candles: &[Candle]) -> Vec<f64> {
    let day = |c: &Candle| c.open_time_ms.div_euclid(DAY_MS);
    (0..candles.len())
        .map(|i| {
            let mut start = i;
            while start > 0 && day(&candles[start - 1]) == day(&candles[i]) {
                start -= 1;
            }
            let valid = start > 0 || candles[0].open_time_ms.rem_euclid(DAY_MS) == 0;
            let (mut pv, mut v) = (0.0, 0.0);
            for c in &candles[start..=i] {
                let tp = (c.high + c.low + c.close) / 3.0;
                pv += tp * c.volume;
                v += c.volume;
            }
            let tp = (candles[i].high + candles[i].low + candles[i].close) / 3.0;
            if !valid {
                f64::NAN
            } else if v > 0.0 {
                pv / v
            } else {
                tp
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Générateurs proptest
// ---------------------------------------------------------------------------------------------

fn price() -> impl Strategy<Value = f64> {
    // Le mélange d'échelles met la compensation de Neumaier à l'épreuve ; les répétitions de 100
    // donnent des deltas nuls et des séries plates pour le RSI.
    prop_oneof![1e-2f64..1.0, 1.0f64..1e5, Just(100.0)]
}

fn series() -> impl Strategy<Value = Vec<f64>> {
    prop::collection::vec(price(), 0..400)
}

type RawCandle = (f64, f64, f64, f64, f64, i64);

fn candle_series() -> impl Strategy<Value = Vec<Candle>> {
    let start = prop_oneof![
        Just(MIDNIGHT_MS),
        (0i64..1440).prop_map(|m| MIDNIGHT_MS + m * MINUTE_MS),
    ];
    let raw = (
        1e-2f64..1e5,
        0.0f64..0.1,
        0.0f64..=1.0,
        0.0f64..=1.0,
        prop_oneof![Just(0.0), 0.0f64..1e3],
        1i64..=720,
    );
    (start, prop::collection::vec(raw, 0..400)).prop_map(|(start, raws)| {
        let mut open_time_ms = start;
        raws.into_iter()
            .map(
                |(low, spread, pos_close, pos_open, volume, step_min): RawCandle| {
                    let high = low * (1.0 + spread);
                    let close = (low + pos_close * (high - low)).min(high);
                    let open = (low + pos_open * (high - low)).min(high);
                    let candle = Candle {
                        open_time_ms,
                        open,
                        high,
                        low,
                        close,
                        volume,
                        trades: None,
                    };
                    open_time_ms += step_min * MINUTE_MS;
                    candle
                },
            )
            .collect()
    })
}

fn assert_same_bits(label: &str, got: &[f64], expected: &[f64]) -> Result<(), TestCaseError> {
    prop_assert_eq!(got.len(), expected.len(), "{}: longueurs", label);
    for (i, (a, b)) in got.iter().zip(expected).enumerate() {
        prop_assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "{} : index {} : {} contre {}",
            label,
            i,
            a,
            b
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn sma_egale_la_version_naive(xs in series(), n in 1usize..=128) {
        let got = batch(Sma::new(n).unwrap(), xs.iter().copied());
        let expected = naive_sma(&xs, n);
        prop_assert_eq!(got.len(), expected.len());
        for i in 0..xs.len() {
            prop_assert_eq!(got[i].is_nan(), expected[i].is_nan(), "NaN à l'index {}", i);
            if !got[i].is_nan() {
                let window = &xs[i + 1 - n..=i];
                let mean_abs = window.iter().fold(0.0, |s, &x| s + x.abs()) / n as f64;
                prop_assert!(
                    (got[i] - expected[i]).abs() <= 1e-12 * mean_abs,
                    "index {} : {} contre {}", i, got[i], expected[i]
                );
            }
        }
    }

    #[test]
    fn ema_egale_la_version_naive(xs in series(), n in 1usize..=64) {
        let got = batch(Ema::new(n).unwrap(), xs.iter().copied());
        assert_same_bits("ema", &got, &naive_ema(&xs, n))?;
    }

    #[test]
    fn rsi_egale_la_version_naive(xs in series(), n in 1usize..=64) {
        let got = batch(Rsi::new(n).unwrap(), xs.iter().copied());
        assert_same_bits("rsi", &got, &naive_rsi(&xs, n))?;
        for v in got.iter().filter(|v| !v.is_nan()) {
            prop_assert!((0.0..=100.0).contains(v), "rsi hors de [0, 100] : {}", v);
        }
    }

    #[test]
    fn atr_egale_la_version_naive(candles in candle_series(), n in 1usize..=64) {
        let got = batch(Atr::new(n).unwrap(), candles.iter().copied());
        assert_same_bits("atr", &got, &naive_atr(&candles, n))?;
        for v in got.iter().filter(|v| !v.is_nan()) {
            prop_assert!(v.is_finite() && *v >= 0.0, "atr négatif ou non fini : {}", v);
        }
    }

    #[test]
    fn vwap_egale_la_version_naive(candles in candle_series()) {
        let got = batch(Vwap::new(), candles.iter().copied());
        assert_same_bits("vwap", &got, &naive_vwap(&candles))?;
    }
}

// ---------------------------------------------------------------------------------------------
// Stabilité par préfixe (I1), reprise en flux et reset
// ---------------------------------------------------------------------------------------------

const N: usize = 5_000;

/// Vérification d'un noyau sur une grille de k.
type GridCheck = Box<dyn Fn(&BTreeSet<usize>)>;

/// Un noyau (indicateur + période) lié à ses données, avec les trois vérifications.
struct Kernel {
    label: String,
    periods: Vec<usize>,
    uses_day_changes: bool,
    check_prefix: GridCheck,
    check_resume: GridCheck,
    check_reset: Box<dyn Fn()>,
}

fn assert_bits_eq(context: &str, got: &[f64], expected: &[f64]) {
    assert_eq!(got.len(), expected.len(), "{context} : longueurs");
    for (j, (a, b)) in got.iter().zip(expected).enumerate() {
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "{context}, j = {j} : {a} contre {b}"
        );
    }
}

fn kernel<I>(label: String, period: Option<usize>, make: fn() -> I, data: Vec<I::Input>) -> Kernel
where
    I: Indicator + 'static,
    I::Input: 'static,
{
    let data = std::rc::Rc::new(data);
    let full = std::rc::Rc::new(batch(make(), data.iter().copied()));
    let (d1, f1, l1) = (data.clone(), full.clone(), label.clone());
    let check_prefix = Box::new(move |ks: &BTreeSet<usize>| {
        for &k in ks {
            let pre = batch(make(), d1[..k].iter().copied());
            assert_eq!(pre.len(), k, "{l1}, k = {k} : longueur du préfixe");
            assert_bits_eq(&format!("{l1}, k = {k}"), &pre, &f1[..k]);
        }
    });
    let (d2, f2, l2) = (data.clone(), full.clone(), label.clone());
    let check_resume = Box::new(move |ks: &BTreeSet<usize>| {
        for &k in ks {
            let mut indicator = make();
            let mut out = Vec::new();
            batch_into(&mut indicator, d2[..k].iter().copied(), &mut out);
            batch_into(&mut indicator, d2[k..].iter().copied(), &mut out);
            assert_bits_eq(&format!("{l2} : reprise à k = {k}"), &out, &f2);
        }
    });
    let l3 = label.clone();
    let check_reset = Box::new(move || {
        let mut indicator = make();
        // État sale : on consomme un préfixe, puis on remet à zéro.
        let _ = batch_into_new(&mut indicator, &data[..data.len() / 3]);
        indicator.reset();
        let again = batch_into_new(&mut indicator, &data);
        assert_bits_eq(&format!("{l3} : reset"), &again, &full);
    });
    Kernel {
        label,
        periods: period.into_iter().collect(),
        uses_day_changes: false,
        check_prefix,
        check_resume,
        check_reset,
    }
}

fn batch_into_new<I: Indicator>(indicator: &mut I, inputs: &[I::Input]) -> Vec<f64> {
    let mut out = Vec::new();
    batch_into(indicator, inputs.iter().copied(), &mut out);
    out
}

fn kernels() -> Vec<Kernel> {
    // Départ en milieu de journée : exerce le warmup du VWAP ; trous et volumes nuls inclus.
    let start = MIDNIGHT_MS + 13 * 3_600_000 + 37 * MINUTE_MS;
    let candles = common::synthetic_candles(N, 0x5EED, start, 0.05);
    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let mut kernels = Vec::new();
    for p in [1usize, 2, 20, 200] {
        kernels.push(kernel(
            format!("sma({p})"),
            Some(p),
            // Les périodes sont capturées par des fonctions distinctes : `make` est un `fn()`.
            match p {
                1 => || Sma::new(1).unwrap(),
                2 => || Sma::new(2).unwrap(),
                20 => || Sma::new(20).unwrap(),
                200 => || Sma::new(200).unwrap(),
                other => unreachable!("période non prévue : {other}"),
            },
            closes.clone(),
        ));
    }
    for p in [1usize, 20, 200] {
        kernels.push(kernel(
            format!("ema({p})"),
            Some(p),
            match p {
                1 => || Ema::new(1).unwrap(),
                20 => || Ema::new(20).unwrap(),
                200 => || Ema::new(200).unwrap(),
                other => unreachable!("période non prévue : {other}"),
            },
            closes.clone(),
        ));
    }
    for p in [1usize, 14] {
        kernels.push(kernel(
            format!("rsi({p})"),
            Some(p),
            match p {
                1 => || Rsi::new(1).unwrap(),
                14 => || Rsi::new(14).unwrap(),
                other => unreachable!("période non prévue : {other}"),
            },
            closes.clone(),
        ));
        kernels.push(kernel(
            format!("atr({p})"),
            Some(p),
            match p {
                1 => || Atr::new(1).unwrap(),
                14 => || Atr::new(14).unwrap(),
                other => unreachable!("période non prévue : {other}"),
            },
            candles.clone(),
        ));
    }
    let mut vwap = kernel("vwap".to_string(), None, Vwap::new, candles);
    vwap.uses_day_changes = true;
    kernels.push(vwap);
    kernels
}

/// Grille de k (bornée à `[0, N]`) : petits k, puissances de 2, voisinage des périodes, extrémités,
/// changements de jour pour le VWAP et 64 valeurs tirées d'un SplitMix64 de graine `0xC0FFEE`.
fn grid(kernel: &Kernel, day_changes: &[usize]) -> BTreeSet<usize> {
    let mut ks: BTreeSet<usize> = [0, 1, 2, 3, N - 1, N].into_iter().collect();
    let mut power = 1usize;
    while power <= N {
        ks.insert(power);
        power *= 2;
    }
    for &p in &kernel.periods {
        for k in p.saturating_sub(2)..=p + 2 {
            ks.insert(k);
        }
    }
    if kernel.uses_day_changes {
        for &s in day_changes {
            ks.extend([s.saturating_sub(1), s, s + 1]);
        }
    }
    let mut rng = common::SplitMix64(0xC0FFEE);
    for _ in 0..64 {
        ks.insert((rng.next_u64() % (N as u64 + 1)) as usize);
    }
    ks.into_iter().filter(|&k| k <= N).collect()
}

fn day_changes() -> Vec<usize> {
    let start = MIDNIGHT_MS + 13 * 3_600_000 + 37 * MINUTE_MS;
    let candles = common::synthetic_candles(N, 0x5EED, start, 0.05);
    candles
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            w[0].open_time_ms.div_euclid(DAY_MS) != w[1].open_time_ms.div_euclid(DAY_MS)
        })
        .map(|(i, _)| i + 1)
        .collect()
}

#[test]
fn stabilite_par_prefixe_pour_chaque_noyau() {
    let changes = day_changes();
    assert!(
        !changes.is_empty(),
        "les données doivent traverser plusieurs jours"
    );
    for kernel in kernels() {
        let ks = grid(&kernel, &changes);
        assert!(ks.len() > 60, "{} : grille trop petite", kernel.label);
        (kernel.check_prefix)(&ks);
    }
}

#[test]
fn reprise_en_flux_pour_chaque_noyau() {
    let changes = day_changes();
    for kernel in kernels() {
        (kernel.check_resume)(&grid(&kernel, &changes));
    }
}

#[test]
fn reset_donne_une_instance_neuve() {
    for kernel in kernels() {
        (kernel.check_reset)();
    }
}
