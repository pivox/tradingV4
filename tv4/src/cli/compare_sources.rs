//! `tv4 compare-sources` : mesure du biais de lieu Kraken USD / OKX (docs/04-protocole-recherche.md
//! §2.5, docs/03-architecture.md §11 point 7 et §15.1 ligne 13), troisième partie de S7.
//!
//! La commande lit les 1m des deux partitions du store (`kraken/<SYMBOLE>/1m` et `okx/<INSTID>/1m`),
//! retient leur recouvrement et mesure trois choses. Elle n'écrit rien dans le store.
//!
//! # Plage comparée
//!
//! Chaque source a une plage « présente » `[première 1m, dernière 1m + 1 min)`, lue dans les manifestes
//! de ses fichiers (un trou au milieu ne la raccourcit pas : ses minutes comptent comme présentes d'un
//! seul côté). La plage comparée est l'intersection des deux, bornée par `--from` (inclus) et `--to`
//! (exclu). Une intersection vide est une erreur. Les plages `--exclude` sont semi-ouvertes `[A, B)`,
//! comme `--from` et `--to`, à la granularité du jour UTC.
//!
//! # Clôtures 4h et 1d
//!
//! Les 1m de chaque source sont agrégées par [`crate::mtf::aggregate`] (le code du moteur), sur la plage
//! comparée seulement. Une barre n'est comparée que si elle tient **entièrement** dans la plage (une barre
//! amputée par le début d'une source n'a pas la même clôture), si elle est présente des deux côtés (au moins
//! une 1m de chaque) et si elle ne recoupe aucune plage exclue : une barre à moitié exclue est écartée en
//! entier, pas recalculée sur ses minutes restantes. `Δ = close_okx / close_kraken − 1`.
//!
//! Le verdict applique les seuils de docs/04 §2.5 : médiane de |Δ| des clôtures 4h au plus 0,10 %, et au plus
//! 1 % des jours avec |Δ| > 0,5 %. Le protocole ne dit pas sur quelle barre se lit « jours » : lecture
//! retenue, les clôtures 1d UTC (à valider). Si l'une des deux séries de barres est vide, le verdict est
//! « indéterminé » sauf si l'autre critère échoue déjà (alors « basis »).
//!
//! # Mèches 1m
//!
//! Mesure complémentaire, informative, sans verdict : sur les minutes présentes des deux côtés, l'écart
//! des plus-bas `low_kraken / low_okx − 1` et des plus-hauts `high_kraken / high_okx − 1`, et la part des
//! minutes où la mèche Kraken va plus loin qu'OKX (plus bas de plus de t, plus haut de plus de t) pour
//! t = 0,1 %, 0,5 % et 1 %. C'est un proxy de « stop déclenché par une mèche Kraken absente d'OKX » ; la
//! mesure par stop réel viendra avec les stratégies.
//!
//! # Calcul déterministe
//!
//! Pas de `HashMap`, jointures par balayage de deux séries triées, quantiles par tri total
//! (`f64::total_cmp`) sur un `Vec`. Définition des quantiles : **rang le plus proche** (nearest rank),
//! `x[⌈n·p/100⌉ − 1]` sur la série triée de `n` valeurs, sans interpolation ; la « médiane » d'une série
//! paire est donc la valeur basse des deux centrales. Le rapport JSON ne contient ni horloge ni durée :
//! deux exécutions sur les mêmes fichiers donnent le même octet. Il liste les fichiers lus avec leur
//! `content_hash`.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::Args;
use serde::Serialize;

use super::{CliError, UtcDate};
use crate::model::{Candle, ExchangeId, Timeframe, Ts};
use crate::mtf;
use crate::report;
use crate::store::time::format_rfc3339_utc;
use crate::store::{
    Columns, Partition, Period, Projection, ReadOptions, Symbol, Window, read_file_manifest,
    read_parquet, resolve_rel,
};

/// Dossier des rapports, relatif à la racine de travail.
const REPORT_DIR_REL: &str = "reports/compare-sources";

/// Schéma du rapport JSON.
const SCHEMA: &str = "tv4-compare-sources-v1";

/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = Timeframe::M1.ms();

/// Seuil sur la médiane de |Δclose 4h| (docs/04 §2.5 : « médiane |Δclose 4h| ≤ 0,10 % »). Au-delà, la
/// paire est marquée « basis ».
pub const MEDIAN_ABS_DELTA_4H_MAX: f64 = 0.001;

/// |Δclose| au-delà duquel un jour compte comme écart (docs/04 §2.5 : « jours avec |Δ| > 0,5 % »).
pub const DAY_DELTA_THRESHOLD: f64 = 0.005;

/// Part maximale, en pourcent, des jours dépassant [`DAY_DELTA_THRESHOLD`] (docs/04 §2.5 : « ≤ 1 % des
/// jours »). Entier pour comparer sans flottant : `écarts × 100 ≤ jours × 1`.
pub const DAYS_ABOVE_MAX_PERCENT: u64 = 1;

/// Seuils de mèche de la mesure complémentaire (docs/04 §2.5, mesure sur les 1m) : 0,1 %, 0,5 %, 1 %.
pub const WICK_THRESHOLDS: [f64; 3] = [0.001, 0.005, 0.01];

// ---------------------------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------------------------

/// Plage de jours UTC `AAAA-MM-JJ..AAAA-MM-JJ`, semi-ouverte : le premier jour inclus, le second exclu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateRange(pub Range<Ts>);

impl FromStr for DateRange {
    type Err = String;

    fn from_str(text: &str) -> Result<DateRange, String> {
        let invalid = || {
            format!(
                "plage « {text} » invalide : AAAA-MM-JJ..AAAA-MM-JJ attendu (premier jour inclus, second exclu)"
            )
        };
        let (start, end) = text.split_once("..").ok_or_else(invalid)?;
        let start = start.parse::<UtcDate>()?.0;
        let end = end.parse::<UtcDate>()?.0;
        if start >= end {
            return Err(format!(
                "plage « {text} » vide : le premier jour doit précéder le second"
            ));
        }
        Ok(DateRange(start..end))
    }
}

/// Arguments de `tv4 compare-sources`.
#[derive(Debug, Args)]
pub struct CompareSourcesArgs {
    /// Symbole canonique Kraken, lu sous `data/candles/kraken/<SYMBOLE>/1m` (par exemple BTC-USD).
    #[arg(long, value_name = "SYMBOLE")]
    pub kraken: Symbol,
    /// Instrument OKX, lu sous `data/candles/okx/<INSTID>/1m` (par exemple BTC-USDC ou BTC-USDT).
    #[arg(long, value_name = "INSTID")]
    pub okx: Symbol,
    /// Début inclus de la plage comparée, date UTC AAAA-MM-JJ (défaut : début du recouvrement).
    #[arg(long, value_name = "AAAA-MM-JJ")]
    pub from: Option<UtcDate>,
    /// Fin exclue de la plage comparée, date UTC AAAA-MM-JJ (défaut : fin du recouvrement).
    #[arg(long, value_name = "AAAA-MM-JJ")]
    pub to: Option<UtcDate>,
    /// Plage de jours UTC exclue du calcul, premier jour inclus et second exclu (par exemple
    /// 2022-05-09..2022-05-16 pour le dépeg USDT) ; répétable.
    #[arg(long = "exclude", value_name = "AAAA-MM-JJ..AAAA-MM-JJ")]
    pub exclude: Vec<DateRange>,
    /// Racine de travail (contient data/, reports/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
}

/// Réglages d'une comparaison, injectables par les tests.
#[derive(Clone, Debug)]
pub struct CompareSourcesOptions {
    pub root: PathBuf,
    /// Symbole de la partition `kraken/<symbole>/1m`.
    pub kraken: Symbol,
    /// Symbole de la partition `okx/<symbole>/1m`.
    pub okx: Symbol,
    /// Début inclus de la plage comparée.
    pub from: Option<Ts>,
    /// Fin exclue de la plage comparée.
    pub to: Option<Ts>,
    /// Plages `[début, fin)` exclues du calcul.
    pub exclude: Vec<Range<Ts>>,
}

impl From<&CompareSourcesArgs> for CompareSourcesOptions {
    fn from(args: &CompareSourcesArgs) -> CompareSourcesOptions {
        CompareSourcesOptions {
            root: args.root.clone(),
            kraken: args.kraken.clone(),
            okx: args.okx.clone(),
            from: args.from.map(|d| d.0),
            to: args.to.map(|d| d.0),
            exclude: args.exclude.iter().map(|r| r.0.clone()).collect(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rapport
// ---------------------------------------------------------------------------------------------

/// Plage `[start, end)` en millisecondes et en RFC 3339.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RangeReport {
    pub start_ms: Ts,
    pub end_ms: Ts,
    pub start: String,
    pub end: String,
}

impl RangeReport {
    fn new(range: &Range<Ts>) -> RangeReport {
        RangeReport {
            start_ms: range.start,
            end_ms: range.end,
            start: date_text(range.start),
            end: date_text(range.end),
        }
    }
}

/// Un fichier Parquet lu.
#[derive(Clone, Debug, Serialize)]
pub struct FileReport {
    /// Chemin relatif à la racine.
    pub path: String,
    pub rows: u64,
    pub content_hash: String,
}

/// Une source : sa partition, sa plage présente et ses fichiers.
#[derive(Clone, Debug, Serialize)]
pub struct SourceReport {
    /// `kraken/BTC-USD/1m`.
    pub partition: String,
    /// De la première 1m à la dernière 1m + 1 min, exclue.
    pub present: RangeReport,
    pub files: Vec<FileReport>,
}

/// Seuils du protocole appliqués (docs/04 §2.5), pour que le rapport se suffise.
#[derive(Clone, Debug, Serialize)]
pub struct ThresholdsReport {
    /// Médiane de |Δclose 4h| au plus égale à ce seuil.
    pub median_abs_delta_4h_max: f64,
    /// |Δclose 1d| au-delà duquel un jour compte.
    pub day_delta_threshold: f64,
    /// Part maximale des jours comptés, en pourcent.
    pub days_above_max_percent: u64,
    /// Seuils de la mesure des mèches 1m.
    pub wick_thresholds: [f64; 3],
}

/// Décompte des minutes de la plage comparée.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MinuteCounts {
    /// Présentes des deux côtés (comparées).
    pub common: u64,
    /// Présentes chez Kraken seulement (non comparées).
    pub kraken_only: u64,
    /// Présentes chez OKX seulement (non comparées).
    pub okx_only: u64,
    /// Minutes (d'au moins une source) retirées par `--exclude`.
    pub excluded: u64,
}

/// Statistiques d'une série de Δ signés.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeltaStats {
    pub median_signed: f64,
    pub median_abs: f64,
    pub p95_abs: f64,
    pub p99_abs: f64,
    pub max_abs: f64,
    pub max_abs_open_time_ms: Ts,
    pub max_abs_at: String,
}

/// Part d'une série au-delà d'un seuil.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShareReport {
    pub threshold: f64,
    pub count: u64,
    pub total: u64,
    /// `count / total`, absent si `total = 0`.
    pub share: Option<f64>,
}

impl ShareReport {
    fn new(threshold: f64, count: usize, total: usize) -> ShareReport {
        ShareReport {
            threshold,
            count: count as u64,
            total: total as u64,
            share: (total > 0).then(|| count as f64 / total as f64),
        }
    }
}

/// Clôtures d'un timeframe de décision.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BarReport {
    pub tf: Timeframe,
    /// Barres complètes dans la plage, côté Kraken.
    pub bars_kraken: u64,
    /// Barres complètes dans la plage, côté OKX.
    pub bars_okx: u64,
    /// Barres communes écartées parce qu'elles recoupent une plage exclue.
    pub bars_excluded: u64,
    /// Barres comparées.
    pub bars_common: u64,
    /// Statistiques de Δ, absentes sans barre comparée.
    pub delta: Option<DeltaStats>,
    /// Part des barres comparées avec |Δ| > [`DAY_DELTA_THRESHOLD`] ; sur les 1d, c'est le critère « jours ».
    pub above_day_threshold: ShareReport,
}

/// Verdict du protocole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Les deux critères sont tenus.
    Ok,
    /// Au moins un critère échoue : la paire est marquée « basis ».
    Basis,
    /// Pas assez de barres pour trancher.
    Indeterminate,
}

impl Verdict {
    const fn label(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Basis => "basis",
            Verdict::Indeterminate => "indéterminé",
        }
    }
}

/// Une direction de mèche : Kraken va plus loin qu'OKX (plus bas pour `low`, plus haut pour `high`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WickSide {
    /// Médiane de l'écart relatif signé (`low_kraken / low_okx − 1`, resp. `high`).
    pub median_signed: f64,
    pub p95_abs: f64,
    pub p99_abs: f64,
    /// Une part par seuil de [`WICK_THRESHOLDS`] : mèche Kraken plus bas (resp. plus haut) de plus de t.
    pub kraken_beyond_okx: Vec<ShareReport>,
}

/// Mesure complémentaire des mèches 1m, sans verdict.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WickReport {
    /// Minutes présentes des deux côtés.
    pub minutes: u64,
    pub low: Option<WickSide>,
    pub high: Option<WickSide>,
}

/// Rapport complet, écrit tel quel en JSON.
#[derive(Clone, Debug, Serialize)]
pub struct CompareReport {
    pub schema: &'static str,
    /// Version et commit de l'exécutable.
    pub tv4_version: &'static str,
    pub kraken: SourceReport,
    pub okx: SourceReport,
    /// Plage comparée (intersection bornée par `--from` et `--to`).
    pub compared: RangeReport,
    pub exclusions: Vec<RangeReport>,
    pub thresholds: ThresholdsReport,
    pub minutes: MinuteCounts,
    pub bars_4h: BarReport,
    pub bars_1d: BarReport,
    pub verdict: Verdict,
    /// Critères en échec, ou motif de l'indétermination ; vide si le verdict est « ok ».
    pub verdict_reasons: Vec<String>,
    pub wicks_1m: WickReport,
}

// ---------------------------------------------------------------------------------------------
// Calcul pur
// ---------------------------------------------------------------------------------------------

/// Rang le plus proche : `sorted[⌈n·percent/100⌉ − 1]`, `percent` dans 1..=100, `sorted` trié croissant.
/// Aucune interpolation, donc aucune arithmétique sur les valeurs.
fn nearest_rank(sorted: &[f64], percent: usize) -> Option<f64> {
    let n = sorted.len();
    let rank = (n * percent).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

/// Copie triée par ordre total.
fn sorted_copy(values: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut sorted: Vec<f64> = values.collect();
    sorted.sort_by(f64::total_cmp);
    sorted
}

/// Statistiques de `(open_time, Δ)` ; `None` si la série est vide. Le maximum de |Δ| garde la plus
/// ancienne date en cas d'égalité.
fn delta_stats(deltas: &[(Ts, f64)]) -> Option<DeltaStats> {
    let signed = sorted_copy(deltas.iter().map(|d| d.1));
    let abs = sorted_copy(deltas.iter().map(|d| d.1.abs()));
    let mut max: Option<(Ts, f64)> = None;
    for &(ts, delta) in deltas {
        if max.is_none_or(|(_, best)| delta.abs() > best) {
            max = Some((ts, delta.abs()));
        }
    }
    let (max_ts, max_abs) = max?;
    Some(DeltaStats {
        median_signed: nearest_rank(&signed, 50)?,
        median_abs: nearest_rank(&abs, 50)?,
        p95_abs: nearest_rank(&abs, 95)?,
        p99_abs: nearest_rank(&abs, 99)?,
        max_abs,
        max_abs_open_time_ms: max_ts,
        max_abs_at: date_text(max_ts),
    })
}

/// Sous-tranche de `candles` (triées par `open_time`) dans `[range.start, range.end)`.
fn restrict<'a>(candles: &'a [Candle], range: &Range<Ts>) -> &'a [Candle] {
    let low = candles.partition_point(|c| c.open_time_ms < range.start);
    let high = candles.partition_point(|c| c.open_time_ms < range.end);
    candles.get(low..high).unwrap_or(&[])
}

/// Vrai si `ts` tombe dans une plage exclue.
fn is_excluded(ts: Ts, exclusions: &[Range<Ts>]) -> bool {
    exclusions.iter().any(|r| r.contains(&ts))
}

/// Vrai si la barre `[open, open + durée)` recoupe une plage exclue.
fn bar_is_excluded(open: Ts, tf: Timeframe, exclusions: &[Range<Ts>]) -> bool {
    let end = open.saturating_add(tf.ms());
    exclusions.iter().any(|r| open < r.end && end > r.start)
}

/// Compare les clôtures de `tf` : barres du moteur ([`mtf::aggregate`]), complètes dans `range`.
fn compare_bars(
    kraken: &[Candle],
    okx: &[Candle],
    tf: Timeframe,
    range: &Range<Ts>,
    exclusions: &[Range<Ts>],
) -> BarReport {
    let complete = |m1: &[Candle]| -> Vec<Candle> {
        mtf::aggregate(m1, tf)
            .into_iter()
            .filter(|b| b.open_time_ms >= range.start && b.open_time_ms + tf.ms() <= range.end)
            .collect()
    };
    let (bars_k, bars_o) = (complete(kraken), complete(okx));
    let (mut i, mut j) = (0, 0);
    let mut deltas: Vec<(Ts, f64)> = Vec::new();
    let mut excluded = 0_u64;
    while let (Some(k), Some(o)) = (bars_k.get(i), bars_o.get(j)) {
        if k.open_time_ms < o.open_time_ms {
            i += 1;
        } else if k.open_time_ms > o.open_time_ms {
            j += 1;
        } else {
            if bar_is_excluded(k.open_time_ms, tf, exclusions) {
                excluded += 1;
            } else {
                deltas.push((k.open_time_ms, o.close / k.close - 1.0));
            }
            i += 1;
            j += 1;
        }
    }
    let above = deltas
        .iter()
        .filter(|d| d.1.abs() > DAY_DELTA_THRESHOLD)
        .count();
    BarReport {
        tf,
        bars_kraken: bars_k.len() as u64,
        bars_okx: bars_o.len() as u64,
        bars_excluded: excluded,
        bars_common: deltas.len() as u64,
        delta: delta_stats(&deltas),
        above_day_threshold: ShareReport::new(DAY_DELTA_THRESHOLD, above, deltas.len()),
    }
}

/// Une direction de mèche à partir des écarts signés ; `beyond(écart, t)` dit si Kraken va plus loin.
fn wick_side(diffs: &[f64], beyond: impl Fn(f64, f64) -> bool) -> Option<WickSide> {
    let signed = sorted_copy(diffs.iter().copied());
    let abs = sorted_copy(diffs.iter().map(|d| d.abs()));
    Some(WickSide {
        median_signed: nearest_rank(&signed, 50)?,
        p95_abs: nearest_rank(&abs, 95)?,
        p99_abs: nearest_rank(&abs, 99)?,
        kraken_beyond_okx: WICK_THRESHOLDS
            .iter()
            .map(|&t| {
                let count = diffs.iter().filter(|&&d| beyond(d, t)).count();
                ShareReport::new(t, count, diffs.len())
            })
            .collect(),
    })
}

/// Résultat du calcul sur deux séries 1m.
#[derive(Clone, Debug)]
struct Measures {
    minutes: MinuteCounts,
    bars_4h: BarReport,
    bars_1d: BarReport,
    verdict: Verdict,
    verdict_reasons: Vec<String>,
    wicks: WickReport,
}

/// Verdict de docs/04 §2.5 à partir des clôtures 4h et 1d.
fn verdict(bars_4h: &BarReport, bars_1d: &BarReport) -> (Verdict, Vec<String>) {
    let mut reasons = Vec::new();
    let mut undetermined = Vec::new();
    match &bars_4h.delta {
        Some(d) if d.median_abs > MEDIAN_ABS_DELTA_4H_MAX => reasons.push(format!(
            "médiane |Δclose 4h| {} > {}",
            pct(d.median_abs, 4),
            pct(MEDIAN_ABS_DELTA_4H_MAX, 2)
        )),
        Some(_) => {}
        None => undetermined.push("aucune clôture 4h comparable".to_string()),
    }
    let days = &bars_1d.above_day_threshold;
    if days.total == 0 {
        undetermined.push("aucune clôture 1d comparable".to_string());
    } else if days.count * 100 > days.total * DAYS_ABOVE_MAX_PERCENT {
        reasons.push(format!(
            "{} jours sur {} avec |Δ| > {} (> {} % des jours)",
            days.count,
            days.total,
            pct(DAY_DELTA_THRESHOLD, 2),
            DAYS_ABOVE_MAX_PERCENT
        ));
    }
    if !reasons.is_empty() {
        (Verdict::Basis, reasons)
    } else if !undetermined.is_empty() {
        (Verdict::Indeterminate, undetermined)
    } else {
        (Verdict::Ok, Vec::new())
    }
}

/// Mesure complète sur deux séries 1m triées. Seules les minutes de `range` comptent ; les minutes de
/// `exclusions` sont retirées, les barres qui les recoupent aussi.
fn measure(
    kraken: &[Candle],
    okx: &[Candle],
    range: &Range<Ts>,
    exclusions: &[Range<Ts>],
) -> Measures {
    let kraken = restrict(kraken, range);
    let okx = restrict(okx, range);

    // Minutes : balayage des deux séries triées, une seule passe.
    let mut minutes = MinuteCounts {
        common: 0,
        kraken_only: 0,
        okx_only: 0,
        excluded: 0,
    };
    let mut low_diffs: Vec<f64> = Vec::new();
    let mut high_diffs: Vec<f64> = Vec::new();
    let (mut i, mut j) = (0, 0);
    loop {
        let (k, o) = (kraken.get(i), okx.get(j));
        let (ts, in_kraken, in_okx) = match (k, o) {
            (Some(k), Some(o)) if k.open_time_ms == o.open_time_ms => (k.open_time_ms, true, true),
            (Some(k), Some(o)) if k.open_time_ms < o.open_time_ms => (k.open_time_ms, true, false),
            (Some(_), Some(o)) => (o.open_time_ms, false, true),
            (Some(k), None) => (k.open_time_ms, true, false),
            (None, Some(o)) => (o.open_time_ms, false, true),
            (None, None) => break,
        };
        if is_excluded(ts, exclusions) {
            minutes.excluded += 1;
        } else {
            match (k.filter(|_| in_kraken), o.filter(|_| in_okx)) {
                (Some(k), Some(o)) => {
                    minutes.common += 1;
                    low_diffs.push(k.low / o.low - 1.0);
                    high_diffs.push(k.high / o.high - 1.0);
                }
                (Some(_), None) => minutes.kraken_only += 1,
                _ => minutes.okx_only += 1,
            }
        }
        i += usize::from(in_kraken);
        j += usize::from(in_okx);
    }

    let bars_4h = compare_bars(kraken, okx, Timeframe::H4, range, exclusions);
    let bars_1d = compare_bars(kraken, okx, Timeframe::D1, range, exclusions);
    let (verdict, verdict_reasons) = verdict(&bars_4h, &bars_1d);
    Measures {
        wicks: WickReport {
            minutes: minutes.common,
            // Mèche basse : Kraken plus bas qu'OKX de plus de t, soit `diff < −t`.
            low: wick_side(&low_diffs, |d, t| d < -t),
            // Mèche haute : Kraken plus haut qu'OKX de plus de t, soit `diff > t`.
            high: wick_side(&high_diffs, |d, t| d > t),
        },
        minutes,
        bars_4h,
        bars_1d,
        verdict,
        verdict_reasons,
    }
}

// ---------------------------------------------------------------------------------------------
// Lecture du store
// ---------------------------------------------------------------------------------------------

/// Fichier d'une partition avec la plage de son manifeste.
struct SourceFile {
    rel: String,
    rows: u64,
    content_hash: String,
    ts_min: Ts,
    ts_max: Ts,
}

/// Partition 1m inventoriée : ses fichiers dans l'ordre des périodes.
struct Source {
    partition: Partition,
    files: Vec<SourceFile>,
}

impl Source {
    /// Première 1m, ou `None` sans fichier.
    fn first(&self) -> Option<Ts> {
        self.files.iter().map(|f| f.ts_min).min()
    }

    /// Dernière 1m, ou `None` sans fichier.
    fn last(&self) -> Option<Ts> {
        self.files.iter().map(|f| f.ts_max).max()
    }

    /// Plage présente `[première, dernière + 1 min)`.
    fn present(&self) -> Option<Range<Ts>> {
        Some(self.first()?..self.last()?.saturating_add(MINUTE_MS))
    }
}

/// Inventaire d'une partition 1m : fichiers `<période>.parquet` du dossier et plage de chaque manifeste.
fn scan_source(root: &Path, partition: Partition) -> Result<Source, CliError> {
    let dir = resolve_rel(root, &partition.dir_rel())?;
    let missing = || CliError::CompareMissingSource {
        partition: partition.label(),
        dir: dir.display().to_string(),
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Err(missing()),
        Err(source) => {
            return Err(CliError::Read {
                path: dir.display().to_string(),
                source,
            });
        }
    };
    let mut periods: Vec<Period> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CliError::Read {
            path: dir.display().to_string(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Les autres fichiers du dossier (manifestes, `.tmp` orphelins) ne sont pas des périodes.
        if let Some(period) = name
            .strip_suffix(".parquet")
            .and_then(|stem| Period::parse(partition.tf, stem).ok())
        {
            periods.push(period);
        }
    }
    periods.sort();
    let mut files = Vec::with_capacity(periods.len());
    for period in periods {
        let manifest = read_file_manifest(&resolve_rel(root, &partition.manifest_rel(period))?)?;
        files.push(SourceFile {
            rel: partition.file_rel(period),
            rows: manifest.row_count,
            content_hash: manifest.content_hash,
            ts_min: manifest.ts_min,
            ts_max: manifest.ts_max,
        });
    }
    if files.is_empty() {
        return Err(missing());
    }
    Ok(Source { partition, files })
}

/// Lit les 1m de `window` dans les fichiers qui la recoupent (sans la colonne `trades`).
fn read_source(root: &Path, source: &Source, window: Window) -> Result<Vec<Candle>, CliError> {
    let options = ReadOptions {
        projection: Projection::NoTrades,
        window: Some(window),
        ..ReadOptions::default()
    };
    let mut candles = Vec::new();
    for file in &source.files {
        if !window.intersects(file.ts_min, file.ts_max) {
            continue;
        }
        let columns: Columns = read_parquet(&resolve_rel(root, &file.rel)?, &options)?;
        candles.extend(columns.to_candles());
    }
    Ok(candles)
}

impl Source {
    fn report(&self) -> Result<SourceReport, CliError> {
        let present = self.present().ok_or(CliError::CompareMissingSource {
            partition: self.partition.label(),
            dir: self.partition.dir_rel(),
        })?;
        Ok(SourceReport {
            partition: self.partition.label(),
            present: RangeReport::new(&present),
            files: self
                .files
                .iter()
                .map(|f| FileReport {
                    path: f.rel.clone(),
                    rows: f.rows,
                    content_hash: f.content_hash.clone(),
                })
                .collect(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Commande
// ---------------------------------------------------------------------------------------------

/// Date RFC 3339 d'un instant, ou le nombre brut en dernier recours (hors plage du store).
fn date_text(ms: Ts) -> String {
    format_rfc3339_utc(ms).unwrap_or_else(|_| ms.to_string())
}

/// Fraction en pourcentage à `digits` décimales, virgule française : `0,2000 %`.
fn pct(fraction: f64, digits: usize) -> String {
    format!("{:.digits$} %", fraction * 100.0).replace('.', ",")
}

/// Compare les deux sources, écrit le rapport JSON et le rend avec son chemin.
pub fn compare_sources(
    options: &CompareSourcesOptions,
) -> Result<(CompareReport, PathBuf), CliError> {
    let kraken = scan_source(
        &options.root,
        Partition::new(ExchangeId::Kraken, options.kraken.clone(), Timeframe::M1),
    )?;
    let okx = scan_source(
        &options.root,
        Partition::new(ExchangeId::Okx, options.okx.clone(), Timeframe::M1),
    )?;
    let (kraken_report, okx_report) = (kraken.report()?, okx.report()?);
    let (Some(k), Some(o)) = (kraken.present(), okx.present()) else {
        return Err(CliError::CompareNoOverlap {
            detail: "une source est vide".to_string(),
        });
    };
    let range = k.start.max(o.start).max(options.from.unwrap_or(Ts::MIN))
        ..k.end.min(o.end).min(options.to.unwrap_or(Ts::MAX));
    let window = Window::new(range.start, range.end).map_err(|_| CliError::CompareNoOverlap {
        detail: format!(
            "Kraken {} -> {}, OKX {} -> {}{}",
            date_text(k.start),
            date_text(k.end),
            date_text(o.start),
            date_text(o.end),
            if options.from.is_some() || options.to.is_some() {
                ", bornées par --from/--to"
            } else {
                ""
            }
        ),
    })?;

    let kraken_m1 = read_source(&options.root, &kraken, window)?;
    let okx_m1 = read_source(&options.root, &okx, window)?;
    let measures = measure(&kraken_m1, &okx_m1, &range, &options.exclude);
    if measures.minutes.common == 0 {
        return Err(CliError::CompareNoCommonMinute {
            detail: format!("{} -> {}", date_text(range.start), date_text(range.end)),
        });
    }

    let report = CompareReport {
        schema: SCHEMA,
        tv4_version: crate::LONG_VERSION,
        kraken: kraken_report,
        okx: okx_report,
        compared: RangeReport::new(&range),
        exclusions: options.exclude.iter().map(RangeReport::new).collect(),
        thresholds: ThresholdsReport {
            median_abs_delta_4h_max: MEDIAN_ABS_DELTA_4H_MAX,
            day_delta_threshold: DAY_DELTA_THRESHOLD,
            days_above_max_percent: DAYS_ABOVE_MAX_PERCENT,
            wick_thresholds: WICK_THRESHOLDS,
        },
        minutes: measures.minutes,
        bars_4h: measures.bars_4h,
        bars_1d: measures.bars_1d,
        verdict: measures.verdict,
        verdict_reasons: measures.verdict_reasons,
        wicks_1m: measures.wicks,
    };
    let name = format!("{}_{}.json", options.kraken, options.okx);
    let path = report::write_json_atomic(&options.root.join(REPORT_DIR_REL), &name, &report)?;
    Ok((report, path))
}

/// Ligne de statistiques d'une série de clôtures.
fn print_bars(label: &str, bars: &BarReport) {
    println!(
        "{label} : {} barres comparées (Kraken {}, OKX {}, {} écartées par --exclude)",
        bars.bars_common, bars.bars_kraken, bars.bars_okx, bars.bars_excluded
    );
    match &bars.delta {
        Some(d) => println!(
            "  Δ = close_okx / close_kraken − 1 : médiane signée {}, médiane |Δ| {}, p95 {}, p99 {}, max {} le {}",
            pct(d.median_signed, 4),
            pct(d.median_abs, 4),
            pct(d.p95_abs, 4),
            pct(d.p99_abs, 4),
            pct(d.max_abs, 4),
            d.max_abs_at
        ),
        None => println!("  aucune barre comparable"),
    }
    let above = &bars.above_day_threshold;
    println!(
        "  |Δ| > {} : {} sur {} ({})",
        pct(above.threshold, 2),
        above.count,
        above.total,
        above.share.map_or_else(|| "?".to_string(), |s| pct(s, 2))
    );
}

/// Une direction de mèche, une ligne par seuil.
fn print_wick(label: &str, side: Option<&WickSide>) {
    let Some(side) = side else {
        return;
    };
    println!(
        "  {label} : médiane {}, p95 |écart| {}, p99 |écart| {}",
        pct(side.median_signed, 4),
        pct(side.p95_abs, 4),
        pct(side.p99_abs, 4)
    );
    let parts: Vec<String> = side
        .kraken_beyond_okx
        .iter()
        .map(|s| {
            format!(
                "> {} : {} ({})",
                pct(s.threshold, 1),
                s.count,
                s.share.map_or_else(|| "?".to_string(), |v| pct(v, 4))
            )
        })
        .collect();
    println!("    Kraken au-delà d'OKX {}", parts.join(" ; "));
}

/// Résumé lisible sur la sortie standard.
fn print_summary(report: &CompareReport, path: &Path) {
    println!(
        "compare-sources : {} contre {}",
        report.kraken.partition, report.okx.partition
    );
    for source in [&report.kraken, &report.okx] {
        println!(
            "  {} : {} -> {} (exclu), {} fichiers",
            source.partition,
            source.present.start,
            source.present.end,
            source.files.len()
        );
    }
    println!(
        "plage comparée : {} -> {} (exclu)",
        report.compared.start, report.compared.end
    );
    for e in &report.exclusions {
        println!("  exclusion : {} -> {} (exclu)", e.start, e.end);
    }
    let m = &report.minutes;
    println!(
        "minutes : {} communes, {} Kraken seul, {} OKX seul, {} exclues",
        m.common, m.kraken_only, m.okx_only, m.excluded
    );
    print_bars("clôtures 4h", &report.bars_4h);
    print_bars("clôtures 1d", &report.bars_1d);
    println!(
        "verdict : {} (seuils docs/04 §2.5 : médiane |Δ4h| ≤ {}, jours avec |Δ| > {} ≤ {} %)",
        report.verdict.label(),
        pct(MEDIAN_ABS_DELTA_4H_MAX, 2),
        pct(DAY_DELTA_THRESHOLD, 2),
        DAYS_ABOVE_MAX_PERCENT
    );
    for reason in &report.verdict_reasons {
        println!("  {reason}");
    }
    println!(
        "mèches 1m (informatif, sans verdict) : {} minutes communes",
        report.wicks_1m.minutes
    );
    print_wick(
        "mèche basse (low_kraken / low_okx − 1)",
        report.wicks_1m.low.as_ref(),
    );
    print_wick(
        "mèche haute (high_kraken / high_okx − 1)",
        report.wicks_1m.high.as_ref(),
    );
    println!("rapport : {}", path.display());
}

/// `tv4 compare-sources` : code de sortie nul même pour un verdict « basis » (c'est une mesure) ; non nul
/// pour un recouvrement vide ou une source absente.
pub(super) fn run(args: &CompareSourcesArgs) -> Result<(), CliError> {
    let (report, path) = compare_sources(&CompareSourcesOptions::from(args))?;
    print_summary(&report, &path);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // une plage exclue = une plage
mod tests {
    use super::*;

    const DAY: Ts = 86_400_000;
    /// 2024-01-01T00:00:00Z.
    const T0: Ts = 1_704_067_200_000;

    /// Minutes de `[start, end)` au cours `price(ts)`, bougie plate.
    fn flat(start: Ts, end: Ts, price: impl Fn(Ts) -> f64) -> Vec<Candle> {
        (start / MINUTE_MS..end / MINUTE_MS)
            .map(|m| {
                let ts = m * MINUTE_MS;
                let p = price(ts);
                Candle {
                    open_time_ms: ts,
                    open: p,
                    high: p,
                    low: p,
                    close: p,
                    volume: 1.0,
                    trades: None,
                }
            })
            .collect()
    }

    fn price(ts: Ts) -> f64 {
        100.0 + ((ts / MINUTE_MS) % 1000) as f64 * 0.01
    }

    #[test]
    fn rang_le_plus_proche() {
        let s: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(nearest_rank(&s, 50), Some(5.0));
        assert_eq!(nearest_rank(&s, 95), Some(10.0));
        assert_eq!(nearest_rank(&s, 99), Some(10.0));
        let cent: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(nearest_rank(&cent, 95), Some(95.0));
        assert_eq!(nearest_rank(&cent, 99), Some(99.0));
        assert_eq!(nearest_rank(&[7.0], 50), Some(7.0));
        assert_eq!(nearest_rank(&[], 50), None);
    }

    #[test]
    fn stats_de_delta_max_a_la_plus_ancienne_date_en_cas_d_egalite() {
        let stats = delta_stats(&[(1, 0.01), (2, -0.01), (3, 0.0)]).expect("série non vide");
        assert_eq!(stats.max_abs, 0.01);
        assert_eq!(stats.max_abs_open_time_ms, 1);
        assert_eq!(stats.median_signed, 0.0);
        assert!(delta_stats(&[]).is_none());
    }

    #[test]
    fn plage_exclue_se_lit_et_refuse_les_mal_formees() {
        let r: DateRange = "2022-05-09..2022-05-16".parse().expect("plage valide");
        assert_eq!(r.0, 1_652_054_400_000..1_652_659_200_000);
        for mauvaise in [
            "2022-05-09",
            "2022-05-16..2022-05-09",
            "2022-05-09..2022-05-09",
            "2022-05-09..",
            "..2022-05-09",
            "2022-05-09..2022-13-01",
        ] {
            assert!(mauvaise.parse::<DateRange>().is_err(), "« {mauvaise} »");
        }
    }

    #[test]
    fn barre_a_moitie_exclue_est_ecartee_en_entier() {
        let ex = [T0 + 3 * 3_600_000..T0 + 5 * 3_600_000];
        // 00-04 et 04-08 recoupent [03h, 05h) ; 08-12 non.
        assert!(bar_is_excluded(T0, Timeframe::H4, &ex));
        assert!(bar_is_excluded(T0 + 4 * 3_600_000, Timeframe::H4, &ex));
        assert!(!bar_is_excluded(T0 + 8 * 3_600_000, Timeframe::H4, &ex));
        // Fin exclue : une plage qui finit à l'ouverture de la barre ne la recoupe pas.
        assert!(!bar_is_excluded(
            T0 + 4 * 3_600_000,
            Timeframe::H4,
            &[T0..T0 + 4 * 3_600_000]
        ));
    }

    #[test]
    fn mesure_sur_series_identiques() {
        let k = flat(T0, T0 + 3 * DAY, price);
        let m = measure(&k, &k.clone(), &(T0..T0 + 3 * DAY), &[]);
        assert_eq!(m.minutes.common, 3 * 1440);
        assert_eq!((m.minutes.kraken_only, m.minutes.okx_only), (0, 0));
        assert_eq!(m.bars_4h.bars_common, 18);
        assert_eq!(m.bars_1d.bars_common, 3);
        let d = m.bars_4h.delta.expect("barres comparées");
        assert_eq!((d.median_abs, d.max_abs), (0.0, 0.0));
        assert_eq!(m.verdict, Verdict::Ok);
    }

    #[test]
    fn sans_jour_complet_le_verdict_est_indetermine() {
        // 6 h : une seule barre 4h complète à partir de T0 ; aucune barre 1d.
        let k = flat(T0, T0 + 6 * 3_600_000, price);
        let m = measure(&k, &k.clone(), &(T0..T0 + 6 * 3_600_000), &[]);
        assert_eq!(m.bars_4h.bars_common, 1);
        assert_eq!(m.bars_1d.bars_common, 0);
        assert_eq!(m.verdict, Verdict::Indeterminate);
        // Un critère déjà en échec l'emporte sur l'indétermination.
        let o = flat(T0, T0 + 6 * 3_600_000, |ts| price(ts) * 1.01);
        assert_eq!(
            measure(&k, &o, &(T0..T0 + 6 * 3_600_000), &[]).verdict,
            Verdict::Basis
        );
    }
}
