//! Sous-commandes `tv4 ingest ...` : `csv` depuis S4, `kraken` depuis S5 ; `okx` (S7) reste vide.
//!
//! `ingest kraken` n'a aucun calcul propre : elle enchaîne `marketdata` (zip vérifié, continuité),
//! `store` (écriture, rapport de qualité) et `report` (rapports JSON). Les rapports sont écrits sous
//! `<racine>/reports/ingest/kraken/` : un `<SYMBOLE>.json` par paire et un `_summary.json`.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::path::PathBuf;
use std::time::Instant;

use clap::Args;
use serde::Serialize;

use super::{CliError, now_ms};
use crate::marketdata::continuity::{self, CheckedSeries, ContinuityReport};
use crate::marketdata::kraken::KrakenCsvReader;
use crate::marketdata::kraken_zip::{
    KrakenManifest, KrakenPair, KrakenZip, KrakenZipError, Sha256Hex,
};
use crate::model::{ExchangeId, Timeframe, Ts};
use crate::report;
use crate::store::time::{MS_PER_DAY, format_rfc3339_utc, parse_rfc3339_utc};
use crate::store::{
    self, Columns, ContinuityCounts, ContinuityInput, Partition, QualityReport, Symbol,
    WriteOptions, WriteOutcome, WriteStatus,
};

/// Source de provenance écrite dans les manifestes par `tv4 ingest csv`.
const SOURCE_KRAKEN_CSV: &str = "kraken-csv";

/// Source de provenance écrite dans les manifestes par `tv4 ingest kraken`.
const SOURCE_KRAKEN_OHLCVT: &str = "kraken-ohlcvt";

/// Dossier des rapports d'ingestion Kraken, relatif à la racine de travail.
const REPORT_DIR_REL: &str = "reports/ingest/kraken";

/// Schéma du rapport d'une paire.
const SCHEMA_PAIR: &str = "tv4-ingest-kraken-v1";

/// Schéma du résumé.
const SCHEMA_SUMMARY: &str = "tv4-ingest-kraken-summary-v1";

/// Taille moyenne d'une ligne Kraken en octets, pour dimensionner le `Vec<Candle>` (une quarantaine).
const BYTES_PER_ROW: u64 = 40;

/// Arguments de `tv4 ingest csv`.
#[derive(Debug, Args)]
pub struct IngestCsvArgs {
    /// CSV au format OHLCVT Kraken, sans en-tête (timestamp en secondes,open,high,low,close,volume,trades).
    #[arg(long, value_name = "CSV")]
    pub csv: PathBuf,
    /// Exchange canonique des données (kraken, okx, bitvavo_usdc).
    #[arg(long, value_name = "EXCHANGE")]
    pub exchange: ExchangeId,
    /// Symbole canonique BASE-QUOTE (BTC-USD pour XBTUSD).
    #[arg(long, value_name = "SYMBOLE")]
    pub symbol: Symbol,
    /// Timeframe des bougies du CSV (1m, 5m, ..., 1d).
    #[arg(long, value_name = "TF")]
    pub tf: Timeframe,
    /// Racine de travail (contient data/, datasets/, state/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
}

/// `tv4 ingest okx` : API REST OKX `history-candles`, écrite à l'étape S7.
pub(super) fn okx() -> Result<(), CliError> {
    Err(CliError::not_implemented("ingest okx", "S7"))
}

/// `tv4 ingest csv` : lit un CSV Kraken (lecteur S2) et écrit les fichiers mensuels ou annuels du
/// store, avec verrou de partition et `_manifest.json`.
///
/// Le store refuse le désordre et les doublons (horodatages strictement croissants) ; le contrôle de
/// continuité complet (doublons exacts, trous, rapport) est fait par `tv4 ingest kraken` (S5).
pub(super) fn csv(args: &IngestCsvArgs) -> Result<(), CliError> {
    let candles = KrakenCsvReader::open(&args.csv, args.tf)?.collect::<Result<Vec<_>, _>>()?;
    let columns = Columns::from_candles(&candles);
    let partition = Partition::new(args.exchange, args.symbol.clone(), args.tf);
    let outcomes = store::write_partition(
        &args.root,
        &partition,
        &columns,
        SOURCE_KRAKEN_CSV,
        now_ms()?,
    )?;
    for outcome in &outcomes {
        let statut = match outcome.status {
            WriteStatus::Created => "créé",
            WriteStatus::Rewritten => "réécrit",
            WriteStatus::Unchanged => "inchangé",
        };
        let cloture = if outcome.closed {
            "période close"
        } else {
            "période non close"
        };
        println!(
            "{} : {} bougies, {statut} ({cloture}), content_hash {}",
            outcome.file_rel, outcome.row_count, outcome.content_hash
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// tv4 ingest kraken
// ---------------------------------------------------------------------------------------------

/// Arguments de `tv4 ingest kraken`.
#[derive(Debug, Args)]
pub struct IngestKrakenArgs {
    /// Zip OHLCVT complet de Kraken (Kraken_OHLCVT_Full.zip réassemblé).
    #[arg(long, value_name = "ZIP")]
    pub zip: PathBuf,
    /// SHA-256 publié par Kraken pour ce zip (64 caractères hexadécimaux), vérifié avant toute lecture.
    #[arg(long, value_name = "HEX")]
    pub sha256: Sha256Hex,
    /// Paire Kraken à ingérer en 1m (XBTUSD, ETHUSD...) ; répétable.
    #[arg(long = "pair", value_name = "PAIRE", required = true)]
    pub pairs: Vec<KrakenPair>,
    /// Racine de travail (contient data/, reports/, state/). Une racine dédiée au dump évite tout
    /// conflit avec la fixture ingérée par `ingest csv` : les fichiers clos y sont immuables.
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
}

/// Résultat d'une ingestion, pour les tests et le bench.
#[derive(Debug)]
pub struct IngestSummary {
    /// Paires écrites (statut `ok`).
    pub pairs_ok: usize,
    /// Paires en échec : (nom Kraken, motif lisible).
    pub pairs_failed: Vec<(String, String)>,
    /// Chemin du `_summary.json`.
    pub summary_path: PathBuf,
}

/// Statut d'une paire dans les rapports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PairStatus {
    Ok,
    Blocked,
    EntryMissing,
    StoreError,
    /// Fichiers écrits, mais le rapport de qualité (étape postérieure à l'écriture) a échoué.
    QualityError,
    ReadError,
}

impl PairStatus {
    const fn as_str(self) -> &'static str {
        match self {
            PairStatus::Ok => "ok",
            PairStatus::Blocked => "blocked",
            PairStatus::EntryMissing => "entry_missing",
            PairStatus::StoreError => "store_error",
            PairStatus::QualityError => "quality_error",
            PairStatus::ReadError => "read_error",
        }
    }
}

/// Un fichier écrit (ou conservé) dans le rapport d'une paire.
#[derive(Serialize)]
struct FileReport {
    path: String,
    period: String,
    status: &'static str,
    row_count: u64,
    closed: bool,
    content_hash: String,
    continuity: Option<ContinuityCounts>,
}

impl FileReport {
    fn from_outcome(outcome: &WriteOutcome) -> FileReport {
        FileReport {
            path: outcome.file_rel.clone(),
            period: outcome.period.label(),
            status: match outcome.status {
                WriteStatus::Created => "created",
                WriteStatus::Rewritten => "rewritten",
                WriteStatus::Unchanged => "unchanged",
            },
            row_count: outcome.row_count,
            closed: outcome.closed,
            content_hash: outcome.content_hash.clone(),
            continuity: outcome.continuity,
        }
    }
}

/// Rapport d'une paire : `reports/ingest/kraken/<SYMBOLE>.json`.
#[derive(Serialize)]
struct PairReport {
    schema: &'static str,
    pair: String,
    symbol: String,
    entry: String,
    tf: Timeframe,
    status: PairStatus,
    error: Option<String>,
    end_of_coverage: String,
    ingested_at: String,
    duration_ms: u64,
    continuity: Option<ContinuityReport>,
    files: Vec<FileReport>,
    quality: Option<QualityReport>,
}

/// Une paire dans le résumé.
#[derive(Serialize)]
struct SummaryPair {
    pair: String,
    symbol: String,
    status: PairStatus,
    rows_kept: u64,
    files: usize,
    report: String,
}

/// `_summary.json`.
#[derive(Serialize)]
struct SummaryReport {
    schema: &'static str,
    zip: String,
    zip_size: u64,
    sha256: String,
    sha256_verified: bool,
    entries: usize,
    manifest: Option<KrakenManifest>,
    end_of_coverage: String,
    ingested_at: String,
    duration_ms: u64,
    pairs: Vec<SummaryPair>,
}

/// Fin de couverture exclusive lue dans le manifeste du zip : `AAAA-MM-JJ` est inclusive (le lendemain
/// à 00:00 UTC), `AAAA-MM-JJTHH:MM:SSZ` est prise telle quelle ; toute autre forme est ignorée.
fn parse_coverage_end(text: &str) -> Option<Ts> {
    if text.len() == 10 {
        let debut_du_jour = parse_rfc3339_utc(&format!("{text}T00:00:00Z")).ok()?;
        Some(debut_du_jour + MS_PER_DAY)
    } else {
        parse_rfc3339_utc(text).ok()
    }
}

/// Valeur de manifeste pour l'impression : `?` si absente.
fn or_unknown<T: Display>(value: &Option<T>) -> String {
    value
        .as_ref()
        .map_or_else(|| "?".to_string(), ToString::to_string)
}

fn seconds(started: Instant) -> String {
    format!("{:.2}", started.elapsed().as_secs_f64())
}

fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Ingestion complète d'un zip Kraken ; imprime le résumé sur la sortie standard.
///
/// 1. Le SHA-256 du zip entier est vérifié en flux ; en cas d'échec rien n'est écrit (ni `data/`, ni
///    `reports/`). Même règle si le manifeste annonce un en-tête.
/// 2. La fin de couverture est `min(couverture du manifeste, now_ms)` (`now_ms` seul sans couverture
///    lisible) : une bougie dont la fin la dépasse n'est pas clôturée, et un fichier n'est clos que si
///    sa période se termine avant elle.
/// 3. Chaque paire, dans l'ordre de la ligne de commande : lecture en flux, contrôle de continuité,
///    écriture du 1m seul (les TF supérieurs sont calculés au chargement, docs/03 §7.1), rapport de
///    qualité, rapport JSON. Une paire bloquée ou en échec n'empêche pas les suivantes.
pub fn ingest_kraken(args: &IngestKrakenArgs, now_ms: Ts) -> Result<IngestSummary, CliError> {
    let total_started = Instant::now();

    // Avant toute lecture : paires en double et symboles canoniques valides.
    let mut vues = BTreeSet::new();
    for pair in &args.pairs {
        if !vues.insert(pair.as_str()) {
            return Err(CliError::DuplicatePair {
                pair: pair.as_str().to_string(),
            });
        }
        pair.symbol().parse::<Symbol>()?;
    }

    // 1. SHA-256 puis ouverture.
    let sha_started = Instant::now();
    let mut zip = KrakenZip::open_verified(&args.zip, &args.sha256)?;
    println!(
        "zip : {} ({} octets, {} entrées), SHA-256 vérifié : {} ({} s)",
        args.zip.display(),
        zip.size(),
        zip.entry_count(),
        zip.sha256(),
        seconds(sha_started)
    );

    // 2. Manifeste et fin de couverture.
    let manifest = zip.manifest()?;
    if manifest.as_ref().and_then(|m| m.has_header) == Some(true) {
        return Err(KrakenZipError::HeaderUnsupported.into());
    }
    let mut coverage_end: Option<Ts> = None;
    match &manifest {
        None => println!(
            "avertissement : MANIFEST.json absent du zip, fin de couverture = instant présent"
        ),
        Some(m) => {
            if let Some(erreur) = &m.parse_error {
                println!("avertissement : MANIFEST.json illisible ({erreur}), ignoré");
            }
            println!(
                "manifeste : product {}, release {}, couverture {} -> {}, {} paires, {} fichiers, {} lignes, généré le {}",
                or_unknown(&m.product),
                or_unknown(&m.release),
                or_unknown(&m.coverage_start),
                or_unknown(&m.coverage_end),
                or_unknown(&m.pairs),
                or_unknown(&m.files),
                or_unknown(&m.rows),
                or_unknown(&m.generated_at)
            );
            if let Some(text) = &m.coverage_end {
                coverage_end = parse_coverage_end(text);
                if coverage_end.is_none() {
                    println!(
                        "avertissement : couverture « {text} » illisible (AAAA-MM-JJ ou AAAA-MM-JJTHH:MM:SSZ attendu), ignorée"
                    );
                }
            }
        }
    }
    let end_ms = coverage_end.map_or(now_ms, |c| c.min(now_ms));
    let end_text = format_rfc3339_utc(end_ms)?;
    let ingested_at = format_rfc3339_utc(now_ms)?;
    println!("fin de couverture retenue : {end_text}");

    // 3. Les paires.
    let report_dir = args.root.join(REPORT_DIR_REL);
    let mut summary_pairs = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for pair in &args.pairs {
        let started = Instant::now();
        let mut report = PairReport {
            schema: SCHEMA_PAIR,
            pair: pair.as_str().to_string(),
            symbol: pair.symbol().to_string(),
            entry: pair.entry_1m(),
            tf: Timeframe::M1,
            status: PairStatus::Ok,
            error: None,
            end_of_coverage: end_text.clone(),
            ingested_at: ingested_at.clone(),
            duration_ms: 0,
            continuity: None,
            files: Vec::new(),
            quality: None,
        };
        let outcomes = process_pair(&mut zip, pair, args, end_ms, now_ms, &mut report)?;
        report.duration_ms = millis(started);
        let report_name = format!("{}.json", pair.symbol());
        let report_path = report::write_json_atomic(&report_dir, &report_name, &report)?;
        print_pair(pair, &report, &outcomes, started, &report_path);
        let rows_kept = report.continuity.as_ref().map_or(0, |c| c.stats.rows_kept);
        if report.status != PairStatus::Ok {
            let motif = report
                .error
                .clone()
                .unwrap_or_else(|| report.status.as_str().to_string());
            failed.push((pair.as_str().to_string(), motif));
        }
        summary_pairs.push(SummaryPair {
            pair: pair.as_str().to_string(),
            symbol: pair.symbol().to_string(),
            status: report.status,
            rows_kept,
            files: report.files.len(),
            report: format!("{REPORT_DIR_REL}/{report_name}"),
        });
    }

    // 4. Résumé.
    let summary = SummaryReport {
        schema: SCHEMA_SUMMARY,
        zip: args.zip.display().to_string(),
        zip_size: zip.size(),
        sha256: zip.sha256().to_string(),
        sha256_verified: true,
        entries: zip.entry_count(),
        manifest,
        end_of_coverage: end_text,
        ingested_at,
        duration_ms: millis(total_started),
        pairs: summary_pairs,
    };
    let summary_path = report::write_json_atomic(&report_dir, "_summary.json", &summary)?;
    println!(
        "résumé : {} ; durée totale {} s",
        summary_path.display(),
        seconds(total_started)
    );
    Ok(IngestSummary {
        pairs_ok: args.pairs.len() - failed.len(),
        pairs_failed: failed,
        summary_path,
    })
}

/// Traite une paire et remplit `report` ; ne rend `Err` que pour une erreur qui interdit de continuer
/// (écriture d'un rapport impossible). Les échecs propres à la paire sont des statuts du rapport.
fn process_pair(
    zip: &mut KrakenZip,
    pair: &KrakenPair,
    args: &IngestKrakenArgs,
    end_ms: Ts,
    now_ms: Ts,
    report: &mut PairReport,
) -> Result<Vec<WriteOutcome>, CliError> {
    // 1. Lecture en flux et contrôle de continuité.
    let size = match zip.entry_size_1m(pair) {
        Ok(size) => size,
        Err(error) => return Ok(read_failure(report, &error)),
    };
    let capacity = usize::try_from(size / BYTES_PER_ROW).unwrap_or(0);
    let checked = match zip.rows_1m(pair) {
        Ok(rows) => continuity::check_m1_with_capacity(rows, end_ms, capacity),
        Err(error) => return Ok(read_failure(report, &error)),
    };
    let CheckedSeries {
        candles,
        report: continuity_report,
    } = checked;

    // 2. Une série bloquante n'écrit rien dans data/.
    if continuity_report.is_blocking() {
        report.status = PairStatus::Blocked;
        report.continuity = Some(continuity_report);
        return Ok(Vec::new());
    }

    // 3. Écriture : colonnes puis libération immédiate des bougies (pic mémoire).
    let columns = Columns::from_candles(&candles);
    drop(candles);
    let symbol: Symbol = pair.symbol().parse()?;
    let partition = Partition::new(ExchangeId::Kraken, symbol, Timeframe::M1);
    let options = WriteOptions {
        source: SOURCE_KRAKEN_OHLCVT,
        now_ms,
        closed_until_ms: end_ms,
        continuity: Some(ContinuityInput {
            duplicates: &continuity_report.duplicate_times,
            rejected_unclosed: &continuity_report.unclosed_times,
        }),
    };
    let written = store::write_partition_with(&args.root, &partition, &columns, &options);
    let outcomes = match written {
        Ok(outcomes) => outcomes,
        Err(error) => {
            report.status = PairStatus::StoreError;
            report.error = Some(error.to_string());
            report.continuity = Some(continuity_report);
            return Ok(Vec::new());
        }
    };
    report.files = outcomes.iter().map(FileReport::from_outcome).collect();
    apply_quality(
        report,
        store::quality_report(columns.open_time_ms(), &Timeframe::ALL),
    );
    report.continuity = Some(continuity_report);
    Ok(outcomes)
}

/// Range le rapport de qualité dans celui de la paire. Les fichiers sont déjà écrits à ce stade : un
/// échec du rapport ne doit pas passer pour un échec d'écriture (`store_error`), il a son propre statut
/// (`quality_error`), la paire reste comptée en échec parce que son rapport est incomplet.
fn apply_quality(
    report: &mut PairReport,
    quality: Result<QualityReport, crate::store::StoreError>,
) {
    match quality {
        Ok(quality) => report.quality = Some(quality),
        Err(error) => {
            report.status = PairStatus::QualityError;
            report.error = Some(format!("rapport de qualité : {error}"));
        }
    }
}

/// Une entrée absente ou illisible n'arrête pas les autres paires : le statut et le message sont
/// portés par le rapport.
fn read_failure(report: &mut PairReport, error: &KrakenZipError) -> Vec<WriteOutcome> {
    report.status = match error {
        KrakenZipError::EntryMissing { .. } => PairStatus::EntryMissing,
        _ => PairStatus::ReadError,
    };
    report.error = Some(error.to_string());
    Vec::new()
}

/// Impression d'une paire, une ligne par fait (voir la doc de `ingest_kraken`).
fn print_pair(
    pair: &KrakenPair,
    report: &PairReport,
    outcomes: &[WriteOutcome],
    started: Instant,
    report_path: &std::path::Path,
) {
    let tete = format!("{} -> {}", pair.as_str(), pair.symbol());
    let Some(c) = &report.continuity else {
        println!(
            "{tete} : {} : rien n'est écrit",
            report.error.as_deref().unwrap_or("échec de lecture")
        );
        println!("  rapport : {}", report_path.display());
        return;
    };
    let s = &c.stats;
    let comptes = format!(
        "{} lignes lues, {} gardées, {} doublons exacts, {} hors ordre, {} non clôturées, {} trous (plus long {} ms), {} mouvements > 20 %",
        s.rows_read,
        s.rows_kept,
        s.duplicates,
        s.out_of_order,
        s.rejected_unclosed,
        s.gaps,
        s.longest_gap_ms,
        s.large_moves
    );
    match report.status {
        PairStatus::Blocked => {
            let motifs: Vec<&str> = c.blocking.iter().map(|k| k.as_str()).collect();
            println!(
                "{tete} : {comptes} : BLOQUÉE ({}) : rien n'est écrit",
                motifs.join(", ")
            );
        }
        PairStatus::StoreError => println!(
            "{tete} : {comptes} : ÉCHEC d'écriture ({})",
            report.error.as_deref().unwrap_or("?")
        ),
        PairStatus::QualityError => println!(
            "{tete} : {comptes} : fichiers écrits ({}), mais ÉCHEC du rapport de qualité ({})",
            outcomes.len(),
            report.error.as_deref().unwrap_or("?")
        ),
        _ => {
            let compte = |statut| outcomes.iter().filter(|o| o.status == statut).count();
            println!(
                "{tete} : {comptes} : écrite, {} fichiers ({} créés, {} réécrits, {} inchangés), {} s",
                outcomes.len(),
                compte(WriteStatus::Created),
                compte(WriteStatus::Rewritten),
                compte(WriteStatus::Unchanged),
                seconds(started)
            );
            let date = |ms: Option<Ts>| {
                ms.and_then(|t| format_rfc3339_utc(t).ok())
                    .unwrap_or_else(|| "?".to_string())
            };
            let exclus: Vec<&str> = report
                .quality
                .iter()
                .flat_map(|q| &q.rows)
                .filter(|r| r.tf == Timeframe::M1 && r.excluded)
                .map(|r| r.quarter.as_str())
                .collect();
            let exclus = if exclus.is_empty() {
                "aucun".to_string()
            } else {
                exclus.join(", ")
            };
            println!(
                "  première bougie {}, dernière {} ; trimestres exclus en 1m (> 1 % absent) : {exclus}",
                date(c.first_open_ms),
                date(c.last_open_ms)
            );
        }
    }
    println!("  rapport : {}", report_path.display());
}

/// `tv4 ingest kraken` : enrobage de [`ingest_kraken`] ; code de sortie non nul si une paire a échoué.
pub(super) fn kraken(args: &IngestKrakenArgs) -> Result<(), CliError> {
    let summary = ingest_kraken(args, now_ms()?)?;
    if summary.pairs_failed.is_empty() {
        return Ok(());
    }
    let failed: Vec<String> = summary
        .pairs_failed
        .iter()
        .map(|(pair, motif)| format!("{pair} ({motif})"))
        .collect();
    Err(CliError::IngestIncomplete {
        failed: failed.join(" ; "),
        summary: summary.summary_path.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn couverture_en_date_inclusive_ou_instant() {
        // 2020-01-07 est inclusive : la fin exclusive est le lendemain 00:00 UTC.
        assert_eq!(parse_coverage_end("2020-01-07"), Some(1_578_441_600_000));
        assert_eq!(
            parse_coverage_end("2020-01-08T00:00:00Z"),
            Some(1_578_441_600_000)
        );
        // 2026-09-30 inclusive : fin au 2026-10-01 00:00 UTC.
        assert_eq!(
            parse_coverage_end("2026-09-30"),
            parse_rfc3339_utc("2026-10-01T00:00:00Z").ok()
        );
        for illisible in [
            "",
            "demain",
            "2020-13-01",
            "2020/01/07",
            "2020-01-07T00:00:00",
        ] {
            assert_eq!(parse_coverage_end(illisible), None, "« {illisible} »");
        }
    }

    fn rapport_vide() -> PairReport {
        PairReport {
            schema: SCHEMA_PAIR,
            pair: "XBTUSD".to_string(),
            symbol: "BTC-USD".to_string(),
            entry: "XBTUSD_1.csv".to_string(),
            tf: Timeframe::M1,
            status: PairStatus::Ok,
            error: None,
            end_of_coverage: String::new(),
            ingested_at: String::new(),
            duration_ms: 0,
            continuity: None,
            files: Vec::new(),
            quality: None,
        }
    }

    #[test]
    fn echec_du_rapport_de_qualite_n_est_pas_un_echec_d_ecriture() {
        // Revue S5-R5 : l'étape est postérieure à l'écriture des fichiers, son échec a son propre statut.
        let mut rapport = rapport_vide();
        apply_quality(&mut rapport, Err(crate::store::StoreError::EmptyInput));
        assert_eq!(rapport.status, PairStatus::QualityError);
        assert_eq!(rapport.status.as_str(), "quality_error");
        assert_ne!(rapport.status, PairStatus::StoreError);
        assert!(
            rapport
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("rapport de qualité : ")),
            "{:?}",
            rapport.error
        );
        assert!(rapport.quality.is_none());

        // Et un rapport réussi laisse le statut `ok`.
        let mut rapport = rapport_vide();
        let serie: Vec<Ts> = (0..3).map(|k| k * 60_000).collect();
        apply_quality(
            &mut rapport,
            store::quality_report(&serie, &[Timeframe::M1]),
        );
        assert_eq!(rapport.status, PairStatus::Ok);
        assert!(rapport.quality.is_some());
    }

    #[test]
    fn valeur_de_manifeste_absente_imprimee_point_d_interrogation() {
        assert_eq!(or_unknown::<u64>(&None), "?");
        assert_eq!(or_unknown(&Some(12_350_u64)), "12350");
        assert_eq!(or_unknown(&Some("r1".to_string())), "r1");
    }
}
