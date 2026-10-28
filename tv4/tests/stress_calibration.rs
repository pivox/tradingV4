//! Calibration du stress (docs/specs/s10.md §11, lot D) sur des séries 1m **construites à la main**.
//!
//! Aucune donnée de marché : chaque minute vaut `niveau ± 0,5`, soit un range de 1,0, ce qui donne
//! exactement un TR de 1,0 et donc un ATR14 de 1,0 (sommes entières, divisions exactes) pour les 1m
//! comme pour les bougies 1h. Un « krach » est une minute qui remplace l'une d'elles. Le niveau
//! passe de 100 à 95 après le krach.
//!
//! Chronologie commune : le jour commence à minuit UTC, la minute `i` ouvre à `i` minutes. La
//! première bougie 1h où l'ATR14 est défini est la 14e (indice 13, 13:00-14:00, clôture à 14:00) :
//! `E = 100`, et avec `k = 2`, `S = 98`. Le krach de la plupart des tests est à 14:30 (minute 870).

use std::collections::BTreeMap;
use std::path::Path;

use tv4::model::costs::CostsFile;
use tv4::model::hash::{blake3_hex, canonical_json};
use tv4::model::{Candle, ModelError, Timeframe, Ts};
use tv4::research::stress::{
    DatasetRef, STRESS_SCHEMA, StressCalibration, StressClass, StressError, StressParams,
    calibrate, median, p95_nearest_rank,
};
use tv4::store::Columns;

/// 2024-01-01T00:00:00Z, un multiple d'un jour.
const T0: Ts = 1_704_067_200_000;
const MIN: i64 = 60_000;
const DAY_MINUTES: i64 = 1_440;
/// 14:30, la minute du krach commun.
const CRASH: i64 = 14 * 60 + 30;

#[derive(Clone, Copy)]
struct Minute {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

impl Minute {
    fn candle(self, index: i64) -> Candle {
        Candle {
            open_time_ms: T0 + index * MIN,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            trades: None,
        }
    }
}

/// Minute normale autour de `level`.
fn calm(level: f64) -> Minute {
    Minute {
        open: level,
        high: level + 0.5,
        low: level - 0.5,
        close: level,
        volume: 1.0,
    }
}

/// Krach : ouvre à 100, plus haut 100, clôture à `close`.
fn crash(low: f64, close: f64) -> Minute {
    Minute {
        open: 100.0,
        high: 100.0,
        low,
        close,
        volume: 1.0,
    }
}

/// Minute synthétique à volume nul (bougie d'un dataset comblé), range nul.
fn filler(level: f64, low: f64) -> Minute {
    Minute {
        open: level,
        high: level,
        low,
        close: level,
        volume: 0.0,
    }
}

/// Construit `minutes` minutes. Le niveau vaut 100 jusqu'à `shift_at` exclu, puis 95. `overrides`
/// remplace des minutes ; `absent` retire des minutes (trou dans la série).
fn build(
    minutes: i64,
    shift_at: Option<i64>,
    overrides: &[(i64, Minute)],
    absent: std::ops::Range<i64>,
) -> Columns {
    let overrides: BTreeMap<i64, Minute> = overrides.iter().copied().collect();
    let mut candles = Vec::new();
    for i in 0..minutes {
        if absent.contains(&i) {
            continue;
        }
        let level = match shift_at {
            Some(at) if i >= at => 95.0,
            _ => 100.0,
        };
        let minute = overrides.get(&i).copied().unwrap_or_else(|| calm(level));
        candles.push(minute.candle(i));
    }
    Columns::from_candles(&candles)
}

/// Un jour avec un krach de `low`/`close` à 14:30.
fn day_with_crash(low: f64, close: f64) -> Columns {
    build(
        DAY_MINUTES,
        Some(CRASH + 1),
        &[(CRASH, crash(low, close))],
        0..0,
    )
}

fn costs() -> CostsFile {
    CostsFile::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/costs_c1.toml"))
        .expect("copie figée costs_c1.toml lisible")
}

fn params(k: &[f64], horizon_bars: usize) -> StressParams {
    StressParams {
        tfs: vec![Timeframe::H1],
        k: k.to_vec(),
        horizon_bars,
        range_mult: 5.0,
    }
}

fn run_one(columns: Columns, k: f64, horizon_bars: usize) -> StressCalibration {
    calibrate(
        &[("BTC-USD".to_string(), columns)],
        &costs(),
        &params(&[k], horizon_bars),
    )
    .expect("calibration")
}

#[test]
fn un_krach_donne_des_comptes_exacts() {
    // Positions : bougie 13 (clôture 14:00, déclenchée à 14:30) ; puis 14 (clôture 15:00), 17 (18:00)
    // et 20 (21:00), chacune expirée au bout de 3 h ; la bougie 23 (clôture 24:00) a une fenêtre qui
    // dépasse la fin des données : censurée, non comptée.
    let calibration = run_one(day_with_crash(90.0, 95.0), 2.0, 3);
    assert_eq!(calibration.schema, STRESS_SCHEMA);
    assert_eq!(calibration.results.len(), 1);
    let r = &calibration.results[0];
    assert_eq!(r.symbol, "BTC-USD");
    assert_eq!(r.class, StressClass::Major);
    assert_eq!(r.tf, Timeframe::H1);
    assert_eq!(r.k, 2.0);
    assert_eq!(r.positions, 4);
    assert_eq!(r.triggered, 1);
    assert_eq!(r.expired, 3);
    assert_eq!(r.stress_m1, 1);
    assert_eq!(r.share_m1, Some(1.0));
    // La bougie 1h 14:00-15:00 a un range de 10,5 > 5 × 1,0.
    assert_eq!(r.stress_tf, 1);
    assert_eq!(r.share_tf, Some(1.0));
    // S = 100 - 2 × 1,0 = 98.
    assert_eq!(r.stress_depth_low.median, Some((98.0 - 90.0) / 98.0));
    assert_eq!(r.stress_depth_low.p95, Some((98.0 - 90.0) / 98.0));
    assert_eq!(r.stress_depth_close.median, Some((98.0 - 95.0) / 98.0));
    assert_eq!(r.stress_depth_close.p95, Some((98.0 - 95.0) / 98.0));
    // Un seul groupe, majors.
    assert_eq!(calibration.pooled.len(), 1);
    let p = &calibration.pooled[0];
    assert_eq!((p.class, p.tf), (StressClass::Major, Timeframe::H1));
    assert_eq!((p.triggered, p.stress_m1, p.stress_tf), (1, 1, 1));
}

#[test]
fn sans_declenchement_les_positions_expirent_et_les_parts_sont_nulles() {
    // Série plate : S = 98 n'est jamais touché. Fenêtres de 3 h : bougies 13, 16 et 19 expirent ;
    // la bougie 22 (clôture 23:00, fin de fenêtre 26:00) est censurée.
    let calibration = run_one(build(DAY_MINUTES, None, &[], 0..0), 2.0, 3);
    let r = &calibration.results[0];
    assert_eq!((r.positions, r.triggered, r.expired), (3, 0, 3));
    assert_eq!((r.stress_m1, r.share_m1), (0, None));
    assert_eq!((r.stress_tf, r.share_tf), (0, None));
    assert_eq!(r.stress_depth_low.median, None);
    assert_eq!(r.stress_depth_low.p95, None);
    let p = &calibration.pooled[0];
    assert_eq!((p.triggered, p.share_m1, p.share_tf), (0, None, None));
}

#[test]
fn la_fenetre_se_termine_a_la_fin_des_donnees_sans_etre_comptee() {
    // Fin des données à 14:45 : la position de la bougie 13 est déclenchée (14:30), et la bougie 14
    // n'est pas close. Une seule position.
    let short = build(885, Some(CRASH + 1), &[(CRASH, crash(90.0, 95.0))], 0..0);
    let r = &run_one(short, 2.0, 3).results[0];
    assert_eq!((r.positions, r.triggered, r.expired), (1, 1, 0));
    // Le stress 1m compte, le stress tf non : la bougie 1h 14:00-15:00 (range partiel 10,5 > 5 × 1,0)
    // n'est pas close à la fin des données.
    assert_eq!(r.stress_m1, 1);
    assert_eq!(r.stress_tf, 0);
}

#[test]
fn le_stop_n_est_actif_qu_une_minute_apres_la_cloture() {
    // Série calme à 100, sans bascule de niveau. La position de la bougie 13 clôt à 14:00 (minute 840)
    // avec S = 100 - 2 × 1,0 = 98 ; le stop n'est actif qu'à partir de 14:01 (04 §3.1 point 1).
    let avec_krach_a = |minute: i64| {
        run_one(
            build(DAY_MINUTES, None, &[(minute, crash(90.0, 100.0))], 0..0),
            2.0,
            3,
        )
    };
    // Un krach dans la minute qui suit la clôture, [close_time, close_time + 60 000) : pas de
    // déclenchement, les trois positions expirent (valeurs recalculées avant d'être figées).
    let r = &avec_krach_a(840).results[0];
    assert_eq!((r.positions, r.triggered, r.expired), (3, 0, 3));
    assert_eq!((r.stress_m1, r.stress_tf), (0, 0));
    // Le même krach une minute plus tard déclenche la position de la bougie 13 : 1 déclenchement, 1
    // stress 1m et, la bougie 14 (range 10 > 5 × ATR) étant close, 1 stress tf ; les trois positions
    // suivantes expirent.
    let r = &avec_krach_a(841).results[0];
    assert_eq!((r.positions, r.triggered, r.expired), (4, 1, 3));
    assert_eq!((r.stress_m1, r.stress_tf), (1, 1));
}

#[test]
fn un_stop_a_zero_ou_moins_ne_peut_pas_etre_touche_et_la_position_expire() {
    // k = 200 : S = 100 - 200 × 1,0 = -100 <= 0. Même un krach à 1,0 ne le touche pas : la position
    // expire (04 §3.1).
    let r = &run_one(
        build(DAY_MINUTES, None, &[(CRASH, crash(1.0, 100.0))], 0..0),
        200.0,
        3,
    )
    .results[0];
    assert!(r.positions > 0);
    assert_eq!(r.triggered, 0);
    assert_eq!(r.expired, r.positions);
    assert_eq!((r.stress_m1, r.stress_tf), (0, 0));
}

fn jour_avec_krach_a_1430(level_shift: Option<i64>) -> Columns {
    build(
        DAY_MINUTES,
        level_shift,
        &[(CRASH, crash(90.0, 95.0))],
        0..0,
    )
}

#[test]
fn le_measurement_hash_ne_depend_pas_de_l_ordre_des_series() {
    let btc = jour_avec_krach_a_1430(Some(CRASH + 1));
    let xrp = jour_avec_krach_a_1430(None);
    let costs = costs();
    let params = params(&[2.0, 3.0], 3);
    let un = calibrate(
        &[
            ("BTC-USD".to_string(), btc.clone()),
            ("XRP-USD".to_string(), xrp.clone()),
        ],
        &costs,
        &params,
    )
    .unwrap();
    let autre = calibrate(
        &[("XRP-USD".to_string(), xrp), ("BTC-USD".to_string(), btc)],
        &costs,
        &params,
    )
    .unwrap();
    assert_eq!(un.results.len(), 4);
    assert_eq!(un.results[0].symbol, "BTC-USD", "symbole croissant");
    assert_eq!(un.results, autre.results);
    assert_eq!(un.pooled, autre.pooled);
    assert_eq!(un.measurement_hash, autre.measurement_hash);
    assert_eq!(
        canonical_json(&un).unwrap(),
        canonical_json(&autre).unwrap(),
        "même JSON à l'octet"
    );
}

#[test]
fn sans_atr_defini_il_n_y_a_aucune_position() {
    // 10 bougies 1h seulement : l'ATR14 n'est jamais défini.
    let r = &run_one(build(600, None, &[], 0..0), 2.0, 3).results[0];
    assert_eq!((r.positions, r.triggered, r.expired), (0, 0, 0));
}

#[test]
fn la_calibration_est_causale() {
    // Deux séries identiques jusqu'au krach inclus (minute 870), puis très différentes. Aucune
    // position ne s'ouvre après le krach (la bougie 14 n'est pas close à 14:45) : tout le résultat
    // doit être identique.
    let quiet = build(885, Some(CRASH + 1), &[(CRASH, crash(90.0, 95.0))], 0..0);
    let wild: Vec<(i64, Minute)> = std::iter::once((CRASH, crash(90.0, 95.0)))
        .chain((CRASH + 5..885).map(|i| {
            (
                i,
                Minute {
                    open: 95.0,
                    high: 300.0,
                    low: 1.0,
                    close: 95.0,
                    volume: 1.0,
                },
            )
        }))
        .collect();
    let noisy = build(885, Some(CRASH + 1), &wild, 0..0);
    let a = run_one(quiet, 2.0, 3);
    let b = run_one(noisy, 2.0, 3);
    assert_eq!(a.results, b.results);
    assert_eq!(a.pooled, b.pooled);
    assert_eq!(a.measurement_hash, b.measurement_hash);
    assert_eq!(a.results[0].stress_m1, 1);
}

#[test]
fn l_atr_des_1m_exclut_la_minute_du_declenchement() {
    // Range du krach : 6. ATR14 des 1m antérieures : 1,0, donc seuil 5,0 : stress. Si la minute du
    // krach entrait dans son propre ATR, il vaudrait (13 + 6) / 14 et le seuil 6,79 : pas de stress.
    let r = &run_one(day_with_crash(94.0, 96.0), 2.0, 3).results[0];
    assert_eq!((r.triggered, r.stress_m1), (1, 1));
    assert_eq!(r.stress_depth_low.median, Some((98.0 - 94.0) / 98.0));
    assert_eq!(r.stress_depth_close.median, Some((98.0 - 96.0) / 98.0));
}

#[test]
fn un_range_sous_le_seuil_n_est_pas_un_stress_mais_reste_un_declenchement() {
    // Range 4 contre un seuil de 5 : déclenché (low 96 ≤ 98), pas en stress.
    let r = &run_one(day_with_crash(96.0, 97.0), 2.0, 3).results[0];
    assert_eq!((r.triggered, r.stress_m1, r.share_m1), (1, 0, Some(0.0)));
    assert_eq!(r.stress_depth_low.median, None);
}

#[test]
fn les_minutes_a_volume_nul_n_entrent_pas_dans_l_atr_des_1m() {
    // Krach de range 4 (pas un stress contre l'ATR 1,0). Variante A : 20 minutes absentes avant le
    // krach. Variante B : les mêmes minutes présentes, synthétiques, à volume nul et range nul. Si
    // elles entraient dans l'ATR14 des 1m, il tomberait à ≈ 0,23 et le krach de 4 serait un stress.
    let overrides = [(CRASH, crash(96.0, 97.0))];
    let absent = build(DAY_MINUTES, Some(CRASH + 1), &overrides, 850..870);
    let fillers: Vec<(i64, Minute)> = (850..870)
        .map(|i| (i, filler(100.0, 100.0)))
        .chain(overrides)
        .collect();
    let filled = build(DAY_MINUTES, Some(CRASH + 1), &fillers, 0..0);
    let a = run_one(absent, 2.0, 3);
    let b = run_one(filled, 2.0, 3);
    assert_eq!(a.results[0].stress_m1, 0);
    assert_eq!(a.results[0].triggered, 1);
    assert_eq!(
        a.results, b.results,
        "les 1m à volume nul changent le résultat"
    );
    assert_eq!(a.measurement_hash, b.measurement_hash);
}

#[test]
fn une_minute_a_volume_nul_ne_declenche_rien() {
    // Minute 860 : volume nul, low 80 (sous le stop de 98). Elle ne déclenche pas : le
    // déclenchement est le krach de 14:30, de low 96.
    let minutes = [(860, filler(100.0, 80.0)), (CRASH, crash(96.0, 97.0))];
    let columns = build(DAY_MINUTES, Some(CRASH + 1), &minutes, 0..0);
    let r = &run_one(columns, 2.0, 3).results[0];
    assert_eq!((r.triggered, r.stress_m1), (1, 0));

    // Même série avec un krach réel à 14:30 plus profond : la profondeur vient de cette minute-là.
    let minutes = [(860, filler(100.0, 80.0)), (CRASH, crash(90.0, 95.0))];
    let columns = build(DAY_MINUTES, Some(CRASH + 1), &minutes, 0..0);
    let r = &run_one(columns, 2.0, 3).results[0];
    assert_eq!(r.stress_depth_low.median, Some((98.0 - 90.0) / 98.0));
}

#[test]
fn une_nouvelle_position_s_ouvre_a_la_cloture_qui_suit_le_declenchement() {
    // Horizon d'une bougie, krach à 14:59 (dernière minute de la bougie 14:00-15:00). Le
    // déclenchement s'achève à 15:00 : la bougie qui clôture à 15:00 peut ouvrir la position
    // suivante. Bougies 14 à 22 : neuf positions expirées (la 22 expire à 24:00, fin des données) ;
    // la bougie 23 est censurée. Avec une comparaison stricte, la bougie 14 serait sautée : 8.
    let minutes = [(899, crash(90.0, 95.0))];
    let columns = build(DAY_MINUTES, Some(900), &minutes, 0..0);
    let r = &run_one(columns, 2.0, 1).results[0];
    assert_eq!((r.triggered, r.expired, r.positions), (1, 9, 10));
}

#[test]
fn deux_positions_ne_se_chevauchent_pas() {
    // Un seul déclenchement dans la journée du krach : la position de la bougie 13 se termine à
    // 14:31, aucune autre position n'est ouverte avant. Avec un chevauchement, les bougies 14 à 22
    // ouvriraient aussi une position à chaque clôture et `positions` dépasserait 4.
    let r = &run_one(day_with_crash(90.0, 95.0), 2.0, 3).results[0];
    assert_eq!(r.positions, r.triggered + r.expired);
    assert_eq!(r.positions, 4);
}

#[test]
fn plusieurs_k_et_regroupement_par_classe() {
    // Profondeurs par k (ATR 1,0, krach low 90, close 95) :
    //   k = 1,5 : S = 98,5 ; k = 2 : S = 98 ; k = 3 : S = 97.
    let columns = day_with_crash(90.0, 95.0);
    let series = vec![
        ("BTC-USD".to_string(), columns.clone()),
        ("XRP-USD".to_string(), columns),
    ];
    let p = StressParams {
        tfs: vec![Timeframe::H1],
        k: vec![1.5, 2.0, 3.0],
        horizon_bars: 3,
        range_mult: 5.0,
    };
    let calibration = calibrate(&series, &costs(), &p).unwrap();

    // Ordre de `results` : série, TF, k.
    let order: Vec<(&str, f64)> = calibration
        .results
        .iter()
        .map(|r| (r.symbol.as_str(), r.k))
        .collect();
    assert_eq!(
        order,
        vec![
            ("BTC-USD", 1.5),
            ("BTC-USD", 2.0),
            ("BTC-USD", 3.0),
            ("XRP-USD", 1.5),
            ("XRP-USD", 2.0),
            ("XRP-USD", 3.0)
        ]
    );
    for r in &calibration.results {
        assert_eq!(
            (r.triggered, r.expired, r.stress_m1),
            (1, 3, 1),
            "{} k={}",
            r.symbol,
            r.k
        );
        let expected_class = if r.symbol == "BTC-USD" {
            StressClass::Major
        } else {
            StressClass::Alt
        };
        assert_eq!(r.class, expected_class);
    }

    // `pooled` : majors puis alts, toutes valeurs de k confondues.
    assert_eq!(calibration.pooled.len(), 2);
    let (major, alt) = (&calibration.pooled[0], &calibration.pooled[1]);
    assert_eq!(
        (major.class, alt.class),
        (StressClass::Major, StressClass::Alt)
    );
    let lows = [
        (98.5 - 90.0) / 98.5,
        (98.0 - 90.0) / 98.0,
        (97.0 - 90.0) / 97.0,
    ];
    let closes = [
        (98.5 - 95.0) / 98.5,
        (98.0 - 95.0) / 98.0,
        (97.0 - 95.0) / 97.0,
    ];
    for pool in [major, alt] {
        assert_eq!((pool.triggered, pool.stress_m1, pool.stress_tf), (3, 3, 3));
        assert_eq!(pool.share_m1, Some(1.0));
        // Trois valeurs : médiane = celle du milieu (k = 2), p95 = le maximum (k = 1,5).
        assert_eq!(pool.stress_depth_low.median, Some(lows[1]));
        assert_eq!(pool.stress_depth_low.p95, Some(lows[0]));
        assert_eq!(pool.stress_depth_close.median, Some(closes[1]));
        assert_eq!(pool.stress_depth_close.p95, Some(closes[0]));
    }
}

#[test]
fn deux_classes_dans_le_meme_groupe_s_additionnent() {
    // ETH-USDC (major) et BTC-USD (major) partagent un groupe : triggered = 2.
    let columns = day_with_crash(90.0, 95.0);
    let series = vec![
        ("BTC-USD".to_string(), columns.clone()),
        ("ETH-USDC".to_string(), columns),
    ];
    let calibration = calibrate(&series, &costs(), &params(&[2.0], 3)).unwrap();
    assert_eq!(calibration.pooled.len(), 1);
    let pool = &calibration.pooled[0];
    assert_eq!(pool.class, StressClass::Major);
    assert_eq!((pool.triggered, pool.stress_m1), (2, 2));
    // Deux valeurs identiques : médiane et p95 égales.
    assert_eq!(pool.stress_depth_low.median, Some((98.0 - 90.0) / 98.0));
    assert_eq!(pool.stress_depth_low.p95, Some((98.0 - 90.0) / 98.0));
}

#[test]
fn le_stress_tf_est_independant_du_stress_m1() {
    // Krach de range 4 : pas de stress m1 (4 ≤ 5). La bougie 1h qui le contient a un range de
    // 100,5 - 96 = 4,5 ≤ 5 : pas de stress tf non plus. Avec une mèche à 90 sur une minute à volume
    // nul, la bougie 1h passe à 10,5 : stress tf sans stress m1.
    let minutes = [(CRASH, crash(96.0, 97.0)), (CRASH + 3, filler(95.0, 90.0))];
    let columns = build(DAY_MINUTES, Some(CRASH + 1), &minutes, 0..0);
    let r = &run_one(columns, 2.0, 3).results[0];
    assert_eq!((r.triggered, r.stress_m1, r.stress_tf), (1, 0, 1));
}

#[test]
fn le_measurement_hash_est_stable_et_exclut_created_at_et_commit() {
    let a = run_one(day_with_crash(90.0, 95.0), 2.0, 3);
    let b = run_one(day_with_crash(90.0, 95.0), 2.0, 3);
    assert_eq!(a, b);
    assert_eq!(a.measurement_hash.len(), 64);
    assert_eq!(a.measurement_hash, a.compute_hash().unwrap());

    // Le hash est celui du JSON canonique sans `measurement_hash`.
    let mut value = serde_json::to_value(&a).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("measurement_hash")
        .unwrap();
    assert!(value.get("created_at").is_none());
    assert!(value.get("commit").is_none());
    assert_eq!(
        a.measurement_hash,
        blake3_hex(canonical_json(&value).unwrap().as_bytes())
    );

    // Le fichier ajoute created_at et commit, sans toucher au hash.
    let one = serde_json::to_value(a.file("2026-10-28T12:00:00Z", "aaaa")).unwrap();
    let two = serde_json::to_value(a.file("2027-01-01T00:00:00Z", "bbbb")).unwrap();
    assert_eq!(one["created_at"], "2026-10-28T12:00:00Z");
    assert_eq!(one["commit"], "aaaa");
    assert_eq!(one["measurement_hash"], two["measurement_hash"]);
    assert_eq!(one["measurement_hash"], a.measurement_hash.as_str());
    assert_eq!(one["schema"], STRESS_SCHEMA);
    assert_eq!(one["costs_file_hash"], costs().file_hash());
    assert_eq!(one["params"]["activation_delay_ms"], 60_000);
    assert_eq!(one["params"]["atr_period"], 14);
    assert_eq!(one["params"]["horizon_bars"], 3);
    assert_eq!(one["params"]["range_mult"], 5.0);
    assert_eq!(one["params"]["tfs"], serde_json::json!(["1h"]));
    assert_eq!(one["params"]["k"], serde_json::json!([2.0]));
    assert_eq!(one["results"][0]["class"], "major");
    assert_eq!(one["results"][0]["tf"], "1h");
    assert_eq!(one["pooled"][0]["class"], "major");
}

#[test]
fn le_hash_depend_des_donnees_des_parametres_et_des_datasets() {
    let base = run_one(day_with_crash(90.0, 95.0), 2.0, 3);
    let other_data = run_one(day_with_crash(91.0, 95.0), 2.0, 3);
    let other_params = run_one(day_with_crash(90.0, 95.0), 2.0, 4);
    assert_ne!(base.measurement_hash, other_data.measurement_hash);
    assert_ne!(base.measurement_hash, other_params.measurement_hash);

    let datasets = vec![
        DatasetRef {
            name: "b".into(),
            dataset_hash: "22".into(),
        },
        DatasetRef {
            name: "a".into(),
            dataset_hash: "11".into(),
        },
    ];
    let with = base.clone().with_datasets(datasets.clone()).unwrap();
    assert_ne!(with.measurement_hash, base.measurement_hash);
    assert_eq!(with.measurement_hash, with.compute_hash().unwrap());
    // Triés par nom : l'ordre de l'appelant ne compte pas.
    assert_eq!(with.datasets[0].name, "a");
    let reversed = base
        .with_datasets(datasets.into_iter().rev().collect())
        .unwrap();
    assert_eq!(with.measurement_hash, reversed.measurement_hash);
}

#[test]
fn les_parametres_par_defaut_sont_ceux_de_la_spec() {
    let p = StressParams::default();
    assert_eq!(p.tfs, vec![Timeframe::D1, Timeframe::H4]);
    assert_eq!(p.k, vec![1.5, 2.0, 3.0]);
    assert_eq!(p.horizon_bars, 30);
    assert_eq!(p.range_mult, 5.0);
}

#[test]
fn les_parametres_invalides_sont_refuses() {
    let series = vec![("BTC-USD".to_string(), day_with_crash(90.0, 95.0))];
    let costs = costs();
    let base = params(&[2.0], 3);
    let bad = [
        StressParams {
            tfs: vec![],
            ..base.clone()
        },
        StressParams {
            tfs: vec![Timeframe::M1],
            ..base.clone()
        },
        StressParams {
            tfs: vec![Timeframe::H1, Timeframe::H1],
            ..base.clone()
        },
        StressParams {
            k: vec![],
            ..base.clone()
        },
        StressParams {
            k: vec![0.0],
            ..base.clone()
        },
        StressParams {
            k: vec![-1.0],
            ..base.clone()
        },
        StressParams {
            k: vec![f64::NAN],
            ..base.clone()
        },
        StressParams {
            k: vec![2.0, 2.0],
            ..base.clone()
        },
        StressParams {
            horizon_bars: 0,
            ..base.clone()
        },
        StressParams {
            horizon_bars: usize::MAX,
            ..base.clone()
        },
        StressParams {
            range_mult: 0.0,
            ..base.clone()
        },
        StressParams {
            range_mult: f64::INFINITY,
            ..base.clone()
        },
    ];
    for p in &bad {
        assert!(
            matches!(
                calibrate(&series, &costs, p),
                Err(StressError::InvalidParams { .. })
            ),
            "{p:?}"
        );
    }
}

#[test]
fn les_series_invalides_sont_refusees() {
    let costs = costs();
    let p = params(&[2.0], 3);
    assert!(matches!(
        calibrate(&[], &costs, &p),
        Err(StressError::NoSeries)
    ));
    assert!(matches!(
        calibrate(&[("BTC-USD".into(), Columns::default())], &costs, &p),
        Err(StressError::EmptySeries { .. })
    ));
    let one = day_with_crash(90.0, 95.0);
    assert!(matches!(
        calibrate(
            &[
                ("BTC-USD".into(), one.clone()),
                ("BTC-USD".into(), one.clone())
            ],
            &costs,
            &p
        ),
        Err(StressError::DuplicateSymbol { .. })
    ));
    // Symbole sans tiret : pas d'actif de base.
    assert!(matches!(
        calibrate(&[("BTC".into(), one)], &costs, &p),
        Err(StressError::Model(ModelError::CostsSymbol { .. }))
    ));
    // Série non croissante.
    let unordered = Columns::from_candles(&[calm(100.0).candle(2), calm(100.0).candle(1)]);
    assert!(matches!(
        calibrate(&[("BTC-USD".into(), unordered)], &costs, &p),
        Err(StressError::Mtf { .. })
    ));
}

#[test]
fn mediane_et_p95_exposes_suivent_la_spec() {
    assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
    assert_eq!(
        p95_nearest_rank(&(1..=20).map(f64::from).collect::<Vec<_>>()),
        Some(19.0)
    );
}
