//! Tests de `tv4 compare-sources` (docs/03-architecture.md §11, S7, troisième partie) sur des stores
//! temporaires remplis par l'API du store : bougies synthétiques, aucun réseau.
//!
//! Les prix ne dépendent que de l'horodatage (une rampe qui se répète toutes les 1000 minutes), pour que
//! deux sources construites avec la même fonction soient identiques et qu'un décalage se lise exactement.

// Une plage exclue = une plage : le `vec![a..b]` est voulu.
#![allow(clippy::single_range_in_vec_init)]

use std::fs;
use std::ops::Range;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

use tv4::cli::{CliError, CompareReport, CompareSourcesOptions, Verdict, compare_sources};
use tv4::model::{Candle, ExchangeId, Timeframe, Ts};
use tv4::store::{Columns, Partition, PartitionLock, StoreError, Symbol, write_partition};

const MINUTE: Ts = 60_000;
const HOUR: Ts = 60 * MINUTE;
const DAY: Ts = 24 * HOUR;
/// 2024-01-01T00:00:00Z.
const T0: Ts = 1_704_067_200_000;
/// « Maintenant » des écritures : après toutes les périodes.
const NOW: Ts = 1_893_456_000_000;

fn price(ts: Ts) -> f64 {
    100.0 + ((ts / MINUTE) % 1000) as f64 * 0.01
}

/// Une 1m plate : open = high = low = close = `p`.
fn flat(ts: Ts, p: f64) -> Candle {
    Candle {
        open_time_ms: ts,
        open: p,
        high: p,
        low: p,
        close: p,
        volume: 1.0,
        trades: None,
    }
}

/// Minutes de `range`, au cours `f(ts)`.
fn series(range: Range<Ts>, f: impl Fn(Ts) -> f64) -> Vec<Candle> {
    (range.start / MINUTE..range.end / MINUTE)
        .map(|m| flat(m * MINUTE, f(m * MINUTE)))
        .collect()
}

fn sym(text: &str) -> Symbol {
    text.parse().expect("symbole valide")
}

fn store(root: &Path, exchange: ExchangeId, symbol: &str, candles: &[Candle]) {
    let partition = Partition::new(exchange, sym(symbol), Timeframe::M1);
    write_partition(
        root,
        &partition,
        &Columns::from_candles(candles),
        "synthetic",
        NOW,
    )
    .expect("écriture du store");
}

/// Store avec BTC-USD chez Kraken et BTC-USDC chez OKX.
fn stores(kraken: &[Candle], okx: &[Candle]) -> TempDir {
    let dir = tempfile::tempdir().expect("dossier temporaire");
    store(dir.path(), ExchangeId::Kraken, "BTC-USD", kraken);
    store(dir.path(), ExchangeId::Okx, "BTC-USDC", okx);
    dir
}

fn options(root: &Path) -> CompareSourcesOptions {
    CompareSourcesOptions {
        root: root.to_path_buf(),
        kraken: sym("BTC-USD"),
        okx: sym("BTC-USDC"),
        from: None,
        to: None,
        exclude: Vec::new(),
    }
}

fn compare(root: &Path) -> CompareReport {
    compare_sources(&options(root)).expect("comparaison").0
}

fn close_to(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

#[test]
fn deux_series_identiques_donnent_des_deltas_nuls_et_le_verdict_ok() {
    let k = series(T0..T0 + 3 * DAY, price);
    let dir = stores(&k, &k);
    let (report, path) = compare_sources(&options(dir.path())).expect("comparaison");

    assert_eq!(report.verdict, Verdict::Ok);
    assert!(report.verdict_reasons.is_empty());
    assert_eq!(report.minutes.common, 3 * 1440);
    assert_eq!(
        (report.minutes.kraken_only, report.minutes.okx_only),
        (0, 0)
    );
    assert_eq!(report.bars_4h.bars_common, 18);
    assert_eq!(report.bars_1d.bars_common, 3);
    for bars in [&report.bars_4h, &report.bars_1d] {
        let delta = bars.delta.as_ref().expect("barres comparées");
        assert_eq!(delta.median_signed, 0.0);
        assert_eq!(delta.median_abs, 0.0);
        assert_eq!(delta.p95_abs, 0.0);
        assert_eq!(delta.p99_abs, 0.0);
        assert_eq!(delta.max_abs, 0.0);
        assert_eq!(bars.above_day_threshold.count, 0);
    }
    let low = report.wicks_1m.low.as_ref().expect("mèches basses");
    assert_eq!(low.median_signed, 0.0);
    assert!(low.kraken_beyond_okx.iter().all(|s| s.count == 0));
    // Le rapport est sous reports/compare-sources/<kraken>_<okx>.json.
    assert_eq!(
        path,
        dir.path()
            .join("reports/compare-sources/BTC-USD_BTC-USDC.json")
    );
    assert!(path.is_file());
}

#[test]
fn un_decalage_constant_de_0_2_pour_cent_marque_la_paire_basis() {
    let k = series(T0..T0 + 3 * DAY, price);
    let o = series(T0..T0 + 3 * DAY, |ts| price(ts) * 1.002);
    let dir = stores(&k, &o);
    let report = compare(dir.path());

    for bars in [&report.bars_4h, &report.bars_1d] {
        let delta = bars.delta.as_ref().expect("barres comparées");
        close_to(delta.median_signed, 0.002);
        close_to(delta.median_abs, 0.002);
        close_to(delta.max_abs, 0.002);
    }
    assert_eq!(report.verdict, Verdict::Basis);
    assert_eq!(
        report.verdict_reasons.len(),
        1,
        "{:?}",
        report.verdict_reasons
    );
    assert!(report.verdict_reasons[0].contains("4h"));
    // 0,2 % reste sous le seuil de 0,5 % par jour.
    assert_eq!(report.bars_1d.above_day_threshold.count, 0);
}

#[test]
fn un_ecart_de_0_08_pour_cent_reste_ok() {
    let k = series(T0..T0 + 3 * DAY, price);
    let o = series(T0..T0 + 3 * DAY, |ts| price(ts) * 1.0008);
    let dir = stores(&k, &o);
    assert_eq!(compare(dir.path()).verdict, Verdict::Ok);
}

/// 100 jours ; OKX décalé de +1 % sur les jours `shifted` (indices à partir de 0).
fn cent_jours(shifted: &[Ts]) -> TempDir {
    let end = T0 + 100 * DAY;
    let k = series(T0..end, price);
    let o = series(T0..end, |ts| {
        let day = (ts - T0) / DAY;
        if shifted.contains(&day) {
            price(ts) * 1.01
        } else {
            price(ts)
        }
    });
    stores(&k, &o)
}

#[test]
fn part_des_jours_en_ecart_calculee_exactement_et_seuil_de_1_pour_cent() {
    // 2 jours sur 100 : 2 % > 1 % -> basis, alors que la médiane des 4h est nulle.
    let dir = cent_jours(&[10, 50]);
    let report = compare(dir.path());
    assert_eq!(report.bars_1d.bars_common, 100);
    assert_eq!(report.bars_1d.above_day_threshold.count, 2);
    assert_eq!(report.bars_1d.above_day_threshold.total, 100);
    assert_eq!(report.bars_1d.above_day_threshold.share, Some(0.02));
    // Les mêmes jours vus en 4h : 6 barres par jour.
    assert_eq!(report.bars_4h.bars_common, 600);
    assert_eq!(report.bars_4h.above_day_threshold.count, 12);
    close_to(report.bars_4h.delta.as_ref().expect("4h").median_abs, 0.0);
    close_to(report.bars_1d.delta.as_ref().expect("1d").max_abs, 0.01);
    assert_eq!(
        report.bars_1d.delta.as_ref().expect("1d").max_abs_at,
        "2024-01-11T00:00:00Z"
    );
    assert_eq!(report.verdict, Verdict::Basis);
    assert!(report.verdict_reasons[0].contains("2 jours sur 100"));

    // 1 jour sur 100 : exactement 1 %, ce qui est dans le seuil (« ≤ 1 % ») -> ok.
    let dir = cent_jours(&[10]);
    let report = compare(dir.path());
    assert_eq!(report.bars_1d.above_day_threshold.count, 1);
    assert_eq!(report.bars_1d.above_day_threshold.share, Some(0.01));
    assert_eq!(report.verdict, Verdict::Ok);
}

#[test]
fn recouvrement_partiel_et_minutes_d_un_seul_cote_comptees_sans_etre_comparees() {
    // Kraken : jours 0 à 10. OKX : du jour 5 à 07:13 au jour 15. Recouvrement : [5j 07:13, 10j).
    let okx_start = T0 + 5 * DAY + 7 * HOUR + 13 * MINUTE;
    let mut k = series(T0..T0 + 10 * DAY, price);
    let mut o = series(okx_start..T0 + 15 * DAY, price);
    // Dans le recouvrement : 5 minutes absentes chez Kraken, 3 chez OKX, et une barre 4h entière
    // (jour 7, 08h-12h) absente chez Kraken.
    let manque_k: Vec<Ts> = (0..5).map(|i| T0 + 6 * DAY + i * MINUTE).collect();
    let trou_4h = T0 + 7 * DAY + 8 * HOUR..T0 + 7 * DAY + 12 * HOUR;
    k.retain(|c| !manque_k.contains(&c.open_time_ms) && !trou_4h.contains(&c.open_time_ms));
    let manque_o: Vec<Ts> = (0..3).map(|i| T0 + 8 * DAY + i * MINUTE).collect();
    o.retain(|c| !manque_o.contains(&c.open_time_ms));
    let dir = stores(&k, &o);
    let report = compare(dir.path());

    assert_eq!(report.compared.start_ms, okx_start);
    assert_eq!(report.compared.end_ms, T0 + 10 * DAY);
    let minutes_recouvrement = ((T0 + 10 * DAY - okx_start) / MINUTE) as u64;
    let trou = 240;
    // Les minutes du trou 4h sont chez OKX seulement, en plus des 3 minutes absentes de chez OKX.
    assert_eq!(report.minutes.okx_only, 5 + trou);
    assert_eq!(report.minutes.kraken_only, 3);
    assert_eq!(report.minutes.common, minutes_recouvrement - 5 - trou - 3);
    // Barres 4h complètes dans [5j 07:13, 10j) : de 5j 08:00 à 10j, soit 28 barres chez OKX ; Kraken
    // n'a pas celle du jour 7 08h-12h.
    assert_eq!(report.bars_4h.bars_okx, 28);
    assert_eq!(report.bars_4h.bars_kraken, 27);
    assert_eq!(report.bars_4h.bars_common, 27);
    // Jours complets : 6, 7, 8, 9 (le jour 5 est amputé). Le jour 7 est présent chez Kraken malgré
    // sa barre 4h manquante : sa clôture 1d est celle de 23:59.
    assert_eq!(report.bars_1d.bars_common, 4);
    assert_eq!(report.verdict, Verdict::Ok);
}

#[test]
fn exclude_retire_les_minutes_et_les_barres_qui_recoupent_la_plage() {
    // Le jour 1 est décalé de +1 % chez OKX ; on l'exclut.
    let k = series(T0..T0 + 3 * DAY, price);
    let o = series(T0..T0 + 3 * DAY, |ts| {
        if (T0 + DAY..T0 + 2 * DAY).contains(&ts) {
            price(ts) * 1.01
        } else {
            price(ts)
        }
    });
    let dir = stores(&k, &o);
    let sans = compare(dir.path());
    assert_eq!(sans.bars_1d.above_day_threshold.count, 1);
    assert_eq!(sans.verdict, Verdict::Basis);

    let mut avec = options(dir.path());
    avec.exclude = vec![T0 + DAY..T0 + 2 * DAY];
    let (avec, _) = compare_sources(&avec).expect("comparaison");
    assert_eq!(avec.minutes.excluded, 1440);
    assert_eq!(avec.minutes.common, 2 * 1440);
    assert_eq!(avec.bars_1d.bars_common, 2);
    assert_eq!(avec.bars_1d.bars_excluded, 1);
    assert_eq!(avec.bars_4h.bars_common, 12);
    assert_eq!(avec.bars_4h.bars_excluded, 6);
    assert_eq!(avec.bars_1d.above_day_threshold.count, 0);
    assert_eq!(avec.verdict, Verdict::Ok);
    assert_eq!(avec.exclusions.len(), 1);
    assert_eq!(avec.exclusions[0].start, "2024-01-02T00:00:00Z");

    // Exclusion qui coupe deux barres 4h (03h-05h du jour 0) : elles tombent en entier, et le jour aussi.
    let mut milieu = options(dir.path());
    milieu.exclude = vec![T0 + 3 * HOUR..T0 + 5 * HOUR];
    let (milieu, _) = compare_sources(&milieu).expect("comparaison");
    assert_eq!(milieu.minutes.excluded, 120);
    assert_eq!(milieu.bars_4h.bars_excluded, 2);
    assert_eq!(milieu.bars_1d.bars_excluded, 1);
    assert_eq!(milieu.bars_1d.bars_common, 2);
}

#[test]
fn meche_basse_kraken_absente_d_okx_comptee_aux_seuils_0_1_et_0_5_pas_1() {
    let mut k = series(T0..T0 + 2 * DAY, price);
    let o = k.clone();
    // Une minute où la mèche basse de Kraken descend de 0,6 % sous celle d'OKX.
    let cible = 500;
    k[cible].low = k[cible].open * (1.0 - 0.006);
    let dir = stores(&k, &o);
    let report = compare(dir.path());

    let total = 2 * 1440;
    assert_eq!(report.wicks_1m.minutes, total);
    let low = report.wicks_1m.low.as_ref().expect("mèches basses");
    let comptes: Vec<(f64, u64)> = low
        .kraken_beyond_okx
        .iter()
        .map(|s| (s.threshold, s.count))
        .collect();
    assert_eq!(comptes, [(0.001, 1), (0.005, 1), (0.01, 0)]);
    close_to(
        low.kraken_beyond_okx[0].share.expect("part"),
        1.0 / total as f64,
    );
    assert_eq!(low.median_signed, 0.0);
    // La mèche haute n'est pas touchée, et la clôture non plus.
    let high = report.wicks_1m.high.as_ref().expect("mèches hautes");
    assert!(high.kraken_beyond_okx.iter().all(|s| s.count == 0));
    assert_eq!(report.verdict, Verdict::Ok);
}

#[test]
fn meche_haute_kraken_au_dessus_d_okx_est_comptee_symetriquement() {
    let mut k = series(T0..T0 + 2 * DAY, price);
    let o = k.clone();
    k[10].high = k[10].close * 1.012;
    k[20].high = k[20].close * 1.003;
    // Une mèche haute plus basse chez Kraken n'est pas « au-delà ».
    let mut o = o;
    o[30].high = o[30].close * 1.02;
    let dir = stores(&k, &o);
    let report = compare(dir.path());

    let high = report.wicks_1m.high.as_ref().expect("mèches hautes");
    let comptes: Vec<u64> = high.kraken_beyond_okx.iter().map(|s| s.count).collect();
    assert_eq!(comptes, [2, 1, 1]);
    let low = report.wicks_1m.low.as_ref().expect("mèches basses");
    assert!(low.kraken_beyond_okx.iter().all(|s| s.count == 0));
}

#[test]
fn recouvrement_vide_est_une_erreur() {
    // Kraken jours 0-2, OKX jours 5-7.
    let k = series(T0..T0 + 2 * DAY, price);
    let o = series(T0 + 5 * DAY..T0 + 7 * DAY, price);
    let dir = stores(&k, &o);
    let erreur = compare_sources(&options(dir.path())).expect_err("recouvrement vide");
    assert!(
        matches!(erreur, CliError::CompareNoOverlap { .. }),
        "{erreur}"
    );
    assert!(erreur.to_string().contains("recouvrement vide"));
    // Aucun rapport n'est écrit.
    assert!(!dir.path().join("reports").exists());

    // Recouvrement non vide mais vidé par --from/--to.
    let k = series(T0..T0 + 4 * DAY, price);
    let dir = stores(&k, &k);
    let mut bornes = options(dir.path());
    bornes.from = Some(T0 + 6 * DAY);
    assert!(matches!(
        compare_sources(&bornes),
        Err(CliError::CompareNoOverlap { .. })
    ));
}

#[test]
fn source_absente_est_une_erreur_explicite() {
    let dir = tempfile::tempdir().expect("dossier temporaire");
    store(
        dir.path(),
        ExchangeId::Kraken,
        "BTC-USD",
        &series(T0..T0 + DAY, price),
    );
    let erreur = compare_sources(&options(dir.path())).expect_err("OKX absent");
    assert!(
        matches!(erreur, CliError::CompareMissingSource { .. }),
        "{erreur}"
    );
    assert!(erreur.to_string().contains("okx/BTC-USDC/1m"), "{erreur}");
}

#[test]
fn from_et_to_bornent_la_plage_comparee() {
    let k = series(T0..T0 + 10 * DAY, price);
    let dir = stores(&k, &k);
    let mut borne = options(dir.path());
    borne.from = Some(T0 + 2 * DAY);
    borne.to = Some(T0 + 5 * DAY);
    let (report, _) = compare_sources(&borne).expect("comparaison");
    assert_eq!(report.compared.start, "2024-01-03T00:00:00Z");
    assert_eq!(report.compared.end, "2024-01-06T00:00:00Z");
    assert_eq!(report.minutes.common, 3 * 1440);
    assert_eq!(report.bars_1d.bars_common, 3);
}

#[test]
fn le_rapport_est_deterministe_et_cite_ses_fichiers_et_seuils() {
    let k = series(T0..T0 + 3 * DAY, price);
    let o = series(T0..T0 + 3 * DAY, |ts| price(ts) * 1.0004);
    let dir = stores(&k, &o);
    let (_, path) = compare_sources(&options(dir.path())).expect("comparaison");
    let premier = fs::read(&path).expect("rapport");
    compare_sources(&options(dir.path())).expect("comparaison");
    assert_eq!(fs::read(&path).expect("rapport"), premier);

    let json: Value = serde_json::from_slice(&premier).expect("JSON");
    assert_eq!(json["schema"], "tv4-compare-sources-v1");
    assert_eq!(json["kraken"]["partition"], "kraken/BTC-USD/1m");
    assert_eq!(json["okx"]["partition"], "okx/BTC-USDC/1m");
    assert_eq!(
        json["kraken"]["files"][0]["path"],
        "data/candles/kraken/BTC-USD/1m/2024-01.parquet"
    );
    assert!(
        json["okx"]["files"][0]["content_hash"]
            .as_str()
            .is_some_and(|h| h.starts_with("blake3:"))
    );
    assert_eq!(json["thresholds"]["median_abs_delta_4h_max"], 0.001);
    assert_eq!(json["thresholds"]["day_delta_threshold"], 0.005);
    assert_eq!(json["thresholds"]["days_above_max_percent"], 1);
    assert_eq!(json["thresholds"]["wick_thresholds"][2], 0.01);
    assert_eq!(json["verdict"], "ok");
    assert_eq!(json["bars_4h"]["tf"], "4h");
    assert_eq!(json["bars_1d"]["tf"], "1d");
}

// --- Constats de la revue de la PR S7 ----------------------------------------------------------------

/// CS-1 : deux séries identiques dont OKX a des trous en fin de barre ne sont pas un « basis ». Trou de
/// 20h à 24h le jour 3 (la barre 1d du jour 3 et la barre 4h de 16h-20h sont intactes ; la barre 4h de 20h
/// n'a aucune minute chez OKX) et barre 4h du jour 6 de 08h où OKX n'a que la première minute.
#[test]
fn trou_en_fin_de_jour_cote_okx_sur_series_identiques() {
    let k = series(T0..T0 + 10 * DAY, price);
    let mut o = k.clone();
    let trou = T0 + 3 * DAY + 20 * HOUR..T0 + 4 * DAY;
    o.retain(|c| !trou.contains(&c.open_time_ms));
    let trou_2 = T0 + 6 * DAY + 8 * HOUR + MINUTE..T0 + 6 * DAY + 12 * HOUR;
    o.retain(|c| !trou_2.contains(&c.open_time_ms));
    let dir = stores(&k, &o);
    let report = compare(dir.path());

    // Les prix sont identiques : chaque clôture comparée à la même minute vaut Δ = 0. Avant la
    // correction, la clôture 1d du jour 3 d'OKX était celle de 19:59 comparée à celle de 23:59 de Kraken.
    for bars in [&report.bars_4h, &report.bars_1d] {
        let delta = bars.delta.as_ref().expect("barres comparées");
        assert_eq!(delta.max_abs, 0.0, "{:?}", bars.tf);
    }
    assert_eq!(report.verdict, Verdict::Ok, "{:?}", report.verdict_reasons);
    assert_eq!(report.bars_1d.above_day_threshold.count, 0);
}

/// CS-1 : les barres dont la minute commune n'est pas la dernière minute présente d'une source sont
/// comptées (clôture décalée), les barres sans minute commune sont écartées et comptées.
#[test]
fn barres_a_cloture_decalee_et_barres_sans_minute_commune_sont_comptees() {
    let k = series(T0..T0 + 10 * DAY, price);
    let mut o = k.clone();
    let trou = T0 + 3 * DAY + 20 * HOUR..T0 + 4 * DAY;
    o.retain(|c| !trou.contains(&c.open_time_ms));
    let trou_2 = T0 + 6 * DAY + 8 * HOUR + MINUTE..T0 + 6 * DAY + 12 * HOUR;
    o.retain(|c| !trou_2.contains(&c.open_time_ms));
    let dir = stores(&k, &o);
    let report = compare(dir.path());
    // 1d : seul le jour 3 a sa dernière minute commune avant la dernière de Kraken (19:59 contre 23:59).
    assert_eq!(report.bars_1d.bars_common, 10);
    assert_eq!(report.bars_1d.bars_shifted_close, 1);
    assert_eq!(report.bars_1d.bars_without_common_minute, 0);
    // 4h : la barre 08h du jour 6 (une seule minute chez OKX) est décalée ; la barre 20h du jour 3 n'existe
    // pas chez OKX, elle compte dans bars_kraken mais pas dans les barres communes.
    assert_eq!(report.bars_4h.bars_kraken, 60);
    assert_eq!(report.bars_4h.bars_okx, 59);
    assert_eq!(report.bars_4h.bars_common, 59);
    assert_eq!(report.bars_4h.bars_shifted_close, 1);
    assert_eq!(report.bars_4h.bars_without_common_minute, 0);

    // Barre sans minute commune : Kraken a 08h-10h, OKX 10h-12h de la barre 4h 08h du jour 1.
    let k = series(T0..T0 + 3 * DAY, price);
    let barre = T0 + DAY + 8 * HOUR;
    let mut kraken = k.clone();
    kraken.retain(|c| !(barre + 2 * HOUR..barre + 4 * HOUR).contains(&c.open_time_ms));
    let mut okx = k;
    okx.retain(|c| !(barre..barre + 2 * HOUR).contains(&c.open_time_ms));
    let dir = stores(&kraken, &okx);
    let report = compare(dir.path());
    assert_eq!(report.bars_4h.bars_kraken, 18);
    assert_eq!(report.bars_4h.bars_okx, 18);
    assert_eq!(report.bars_4h.bars_common, 17);
    assert_eq!(report.bars_4h.bars_without_common_minute, 1);
    assert_eq!(report.bars_4h.bars_shifted_close, 0);
    // La barre 1d du jour 1 a 23:59 des deux côtés : comparée, non décalée.
    assert_eq!(report.bars_1d.bars_common, 3);
    assert_eq!(report.bars_1d.bars_shifted_close, 0);
    assert_eq!(report.verdict, Verdict::Ok);
}

/// CS-2 : un Parquet remplacé sans toucher au manifeste (arrêt entre le `rename` et l'écriture du
/// manifeste, ingestion concurrente) est une erreur, pas un verdict calculé sur des données non vérifiées.
#[test]
fn hash_du_manifeste_non_verifie() {
    let k = series(T0..T0 + 3 * DAY, price);
    let dir = stores(&k, &k);
    let avant = compare(dir.path());
    assert_eq!(avant.verdict, Verdict::Ok);
    let hash_avant = avant.okx.files[0].content_hash.clone();
    assert!(hash_avant.starts_with("blake3:"));

    // Parquet OKX décalé de +1 %, manifeste inchangé.
    let autre = tempfile::tempdir().expect("dossier temporaire");
    store(
        autre.path(),
        ExchangeId::Okx,
        "BTC-USDC",
        &series(T0..T0 + 3 * DAY, |ts| price(ts) * 1.01),
    );
    let rel = "data/candles/okx/BTC-USDC/1m/2024-01.parquet";
    fs::copy(autre.path().join(rel), dir.path().join(rel)).expect("copie");

    let resultat = compare_sources(&options(dir.path()));
    assert!(
        matches!(
            resultat,
            Err(CliError::Store(StoreError::FileHashMismatch { .. }))
        ),
        "{resultat:?}"
    );
}

/// CS-2 : la lecture prend le verrou de partition, comme `build_dataset` : une ingestion en cours la
/// retient.
#[test]
fn partition_verrouillee_par_une_ingestion_refusee() {
    let k = series(T0..T0 + 3 * DAY, price);
    let dir = stores(&k, &k);
    let partition = Partition::new(ExchangeId::Okx, sym("BTC-USDC"), Timeframe::M1);
    let verrou = PartitionLock::acquire(dir.path(), &partition).expect("verrou");
    let resultat = compare_sources(&options(dir.path()));
    assert!(
        matches!(resultat, Err(CliError::Store(StoreError::LockBusy { .. }))),
        "{resultat:?}"
    );
    drop(verrou);
    assert!(compare_sources(&options(dir.path())).is_ok());
}

// --- Binaire ------------------------------------------------------------------------------------

fn tv4(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(args)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

#[test]
fn binaire_de_bout_en_bout() {
    let k = series(T0..T0 + 3 * DAY, price);
    let o = series(T0..T0 + 3 * DAY, |ts| price(ts) * 1.002);
    let dir = stores(&k, &o);
    let root = dir.path().to_str().expect("chemin UTF-8");
    let sortie = tv4(&[
        "compare-sources",
        "--root",
        root,
        "--kraken",
        "BTC-USD",
        "--okx",
        "BTC-USDC",
        "--exclude",
        "2024-01-03..2024-01-04",
    ]);
    let texte = String::from_utf8_lossy(&sortie.stdout);
    // Un verdict « basis » est une mesure, pas une erreur : code 0.
    assert!(
        sortie.status.success(),
        "{texte} {}",
        String::from_utf8_lossy(&sortie.stderr)
    );
    assert!(texte.contains("verdict : basis"), "{texte}");
    assert!(
        texte.contains("clôtures 4h : 12 barres comparées"),
        "{texte}"
    );
    assert!(
        texte.contains("clôtures 1d : 2 barres comparées"),
        "{texte}"
    );
    assert!(texte.contains("médiane |Δ| 0,2000 %"), "{texte}");
    assert!(texte.contains("mèches 1m"), "{texte}");
    assert!(texte.contains("BTC-USD_BTC-USDC.json"), "{texte}");

    let rapport = dir
        .path()
        .join("reports/compare-sources/BTC-USD_BTC-USDC.json");
    let json: Value = serde_json::from_slice(&fs::read(rapport).expect("rapport")).expect("JSON");
    assert_eq!(json["verdict"], "basis");
    assert_eq!(json["minutes"]["excluded"], 1440);
    assert_eq!(json["exclusions"][0]["start"], "2024-01-03T00:00:00Z");
}

#[test]
fn binaire_recouvrement_vide_sort_en_erreur() {
    let k = series(T0..T0 + 2 * DAY, price);
    let o = series(T0 + 5 * DAY..T0 + 7 * DAY, price);
    let dir = stores(&k, &o);
    let root = dir.path().to_str().expect("chemin UTF-8");
    let sortie = tv4(&[
        "compare-sources",
        "--root",
        root,
        "--kraken",
        "BTC-USD",
        "--okx",
        "BTC-USDC",
    ]);
    assert_eq!(sortie.status.code(), Some(1));
    assert!(sortie.stdout.is_empty());
    let erreur = String::from_utf8_lossy(&sortie.stderr);
    assert!(erreur.contains("erreur :"), "{erreur}");
    assert!(erreur.contains("recouvrement vide"), "{erreur}");
}

#[test]
fn binaire_refuse_une_plage_exclue_mal_formee() {
    let sortie = tv4(&[
        "compare-sources",
        "--kraken",
        "BTC-USD",
        "--okx",
        "BTC-USDC",
        "--exclude",
        "2024-01-05..2024-01-01",
    ]);
    // Erreur d'usage de clap.
    assert_eq!(sortie.status.code(), Some(2));
}
