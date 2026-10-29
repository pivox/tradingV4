//! Datasets figés : construction, chargement vérifié et remplissage des trous
//! (docs/03 §5.1 à §5.3, décisions D1, D6 et D10 de S4).
//!
//! # Lecture vérifiée complète
//!
//! Le `content_hash` couvre toutes les lignes du fichier et la colonne `trades` : le build comme le
//! chargement d'un dataset lisent donc les 7 colonnes de chaque fichier en entier. La projection à 6
//! colonnes et le filtre de row groups (`parquet.rs`) ne servent qu'aux lectures sans vérification
//! (bench, campagnes). C'est un écart à la lettre de docs/03 §7.1 point 1, documenté en §5.3.
//!
//! # Remplissage au chargement
//!
//! `ffill` et `zero_volume_marked` synthétisent les bougies manquantes intérieures au chargement
//! seulement ([`fill_gaps`]) : le stockage et le hash ne les voient jamais.
//!
//! # Datasets de recherche (S11)
//!
//! [`build_dataset_research`] construit un dataset d'un bloc de recherche (docs/specs/s11.md §6) : mêmes
//! contrôles et même écriture que [`build_dataset_multi`], plus quatre champs facultatifs du manifeste
//! (`block`, `decision_window`, `universe`, `quality_exclusions`) **hors de `dsh-v1`**. Les exclusions
//! qualité sont calculées sur la série 1m **complète** du store, pas sur la fenêtre du dataset. Un
//! manifeste existant au même `dataset_hash` doit avoir les mêmes champs de recherche
//! ([`StoreError::DatasetMetaMismatch`]), pour les trois constructeurs. [`research_meta`] relit et
//! recoupe ces champs ; [`read_series_window`] lit une série du store avec vérification des hashes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::model::blocks::decision_window;
use crate::model::{
    Block, Candle, ExchangeId, QualitySchedule, QuarterExclusion, Timeframe, Ts, UniverseRef,
};

use super::StoreError;
use super::columns::Columns;
use super::dsh::{self, HASH_FORMAT};
use super::durable::{io_error, write_bytes_durable};
use super::layout::{GapsPolicy, Partition, Period, Symbol, Window, resolve_rel};
use super::lock::{DatasetLock, PartitionLock};
use super::manifest::{
    DatasetFile, DatasetManifest, FileManifest, read_dataset_manifest, read_file_manifest,
    write_dataset_manifest,
};
use super::parquet::{ReadOptions, read_parquet};
use super::partition::check_identity;
use super::quality::quality_report;
use super::time::{MAX_TS_MS, format_rfc3339_utc, parse_rfc3339_utc};
use super::universe::QualityExpectation;

/// Longueur maximale du nom d'un dataset.
const MAX_NAME_LEN: usize = 64;

/// Nombre maximal de bougies synthétiques créées par [`fill_gaps`] (environ 19 ans de 1m) : garde
/// contre une allocation sans borne sur deux bougies très éloignées.
pub const MAX_SYNTHETIC_BARS: u64 = 10_000_000;

/// Demande de construction d'un dataset mono-série (le jalon 1 n'en a pas d'autre).
#[derive(Clone, Debug)]
pub struct BuildRequest<'a> {
    /// `[a-z0-9_]{1,64}` : le manifeste est écrit dans `datasets/<nom>.json`.
    pub name: &'a str,
    pub partition: Partition,
    /// Fenêtre `[début, fin)`, bornes alignées sur le timeframe.
    pub window: Window,
    pub gaps_policy: GapsPolicy,
}

/// Demande de construction d'un dataset multi-séries (S9) : un même exchange, timeframe, fenêtre et
/// `gaps_policy` pour tous les symboles.
#[derive(Clone, Debug)]
pub struct MultiBuildRequest<'a> {
    /// `[a-z0-9_]{1,64}` : le manifeste est écrit dans `datasets/<nom>.json`.
    pub name: &'a str,
    pub exchange: ExchangeId,
    /// Au moins un symbole, sans doublon. L'ordre est sans effet : le manifeste les trie.
    pub symbols: &'a [Symbol],
    pub tf: Timeframe,
    /// Fenêtre `[début, fin)`, bornes alignées sur le timeframe.
    pub window: Window,
    pub gaps_policy: GapsPolicy,
}

/// Ce qu'un build de recherche ajoute au manifeste (s11 §6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResearchMeta {
    /// Bloc du dataset : la fenêtre de décision s'en déduit.
    pub block: Block,
    /// Référence d'univers (`rules_hash`, `window_hash`) quand les symboles viennent d'un univers
    /// trimestriel ; `None` en univers fixe.
    pub universe: Option<UniverseRef>,
    /// Exclusions qualité écrites dans les CSV de l'univers, comparées à celles que le store calcule
    /// ([`StoreError::UniverseQualityMismatch`]). `None` en univers fixe.
    pub universe_quality: Option<QualityExpectation>,
}

/// Métadonnées de recherche d'un manifeste, relues et recoupées par [`research_meta`].
#[derive(Clone, Debug, PartialEq)]
pub struct ResearchInfo {
    pub block: Block,
    /// Début de la fenêtre de décision (début du bloc).
    pub decision_start_ms: Ts,
    /// Fin exclusive de la fenêtre de décision.
    pub decision_end_ms: Ts,
    pub universe: Option<UniverseRef>,
    /// Exclusions qualité par symbole, résolues en intervalles (le 1m n'exclut jamais rien).
    pub quality: QualitySchedule,
}

/// Ce qu'un build a fait du manifeste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildStatus {
    Created,
    /// Le manifeste existait avec le même `dataset_hash` : rien n'a été écrit.
    Unchanged,
}

/// Résultat de [`build_dataset`].
#[derive(Clone, Debug)]
pub struct BuildOutcome {
    /// `datasets/<nom>.json`
    pub manifest_rel: String,
    pub manifest: DatasetManifest,
    pub status: BuildStatus,
    /// Nombre de fichiers du dataset qui sont des copies figées (mois non clos).
    pub snapshots: usize,
}

/// Dataset chargé, après vérification des hashes et `gaps_policy`.
#[derive(Clone, Debug)]
pub struct LoadedDataset {
    pub manifest: DatasetManifest,
    /// Lignes de la fenêtre, après `gaps_policy` (masque `synthetic` rempli).
    pub columns: Columns,
}

/// Dataset multi-séries chargé, après vérification des hashes et `gaps_policy` série par série.
#[derive(Clone, Debug)]
pub struct MultiDataset {
    pub manifest: DatasetManifest,
    /// Une série par symbole, dans l'ordre du manifeste (ordre d'octets des symboles) : lignes de la
    /// fenêtre, après `gaps_policy` (masque `synthetic` rempli).
    pub series: Vec<(String, Columns)>,
}

fn check_name(name: &str) -> Result<(), StoreError> {
    let valide = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valide {
        Ok(())
    } else {
        Err(StoreError::InvalidDatasetName(name.to_string()))
    }
}

/// Lit un fichier Parquet en entier (7 colonnes), recalcule son `content_hash` et le compare à
/// `expected` (`blake3:...`). `shown` est le chemin écrit dans l'erreur (relatif à la racine).
fn read_verified(
    path: &Path,
    shown: &str,
    partition: &Partition,
    period: Period,
    expected: &str,
) -> Result<Columns, StoreError> {
    read_verified_with_hash(path, shown, partition, period, expected).map(|(columns, _)| columns)
}

/// Comme `read_verified`, et rend aussi le `content_hash` recalculé (`blake3:...`, égal à `expected`
/// puisque toute différence est une erreur). Voie de lecture vérifiée des commandes qui lisent des
/// fichiers sans construire de dataset (`tv4 compare-sources`).
pub fn read_verified_with_hash(
    path: &Path,
    shown: &str,
    partition: &Partition,
    period: Period,
    expected: &str,
) -> Result<(Columns, String), StoreError> {
    let columns = read_parquet(path, &ReadOptions::default())?;
    let (hex, _) = dsh::content_hash(partition, period, &columns)?;
    let actual = dsh::prefixed(&hex);
    if actual != expected {
        return Err(StoreError::FileHashMismatch {
            path: shown.to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok((columns, actual))
}

/// Compare une valeur du manifeste à celle recalculée.
fn check_field<T: PartialEq + ToString>(
    path: &str,
    field: &'static str,
    manifest: T,
    actual: T,
) -> Result<(), StoreError> {
    if manifest == actual {
        Ok(())
    } else {
        Err(StoreError::ManifestMismatch {
            path: path.to_string(),
            field,
            manifest: manifest.to_string(),
            actual: actual.to_string(),
        })
    }
}

/// Ajoute `columns` à la suite de `all` sans copie pour le premier fichier : il est déplacé.
fn append(all: &mut Option<Columns>, columns: Columns) -> Result<(), StoreError> {
    match all {
        Some(existing) => existing.extend_from(&columns),
        None => {
            *all = Some(columns);
            Ok(())
        }
    }
}

/// Un fichier de la partition retenu par le build.
struct Kept {
    period: Period,
    file_rel: String,
    manifest: FileManifest,
}

/// Résultat de la lecture d'une série pour un build : fichiers retenus et lignes concaténées.
struct SeriesRead {
    kept: Vec<Kept>,
    columns: Columns,
}

/// Lit, vérifie et retient les fichiers d'une partition qui touchent la fenêtre (étapes 3 et 4 de
/// [`build_dataset`]). Les verrous sont déjà pris par l'appelant.
fn read_series(
    root: &Path,
    partition: &Partition,
    periods: &[Period],
    window: Window,
) -> Result<SeriesRead, StoreError> {
    let mut kept: Vec<Kept> = Vec::new();
    let mut all: Option<Columns> = None;
    for &period in periods {
        let file_rel = partition.file_rel(period);
        let manifest_rel = partition.manifest_rel(period);
        let parquet_path = resolve_rel(root, &file_rel)?;
        let manifest_path = resolve_rel(root, &manifest_rel)?;
        match (parquet_path.is_file(), manifest_path.is_file()) {
            (false, false) => continue,
            (true, false) => return Err(StoreError::ManifestMissing { path: file_rel }),
            (false, true) => return Err(StoreError::DataFileMissing { path: file_rel }),
            (true, true) => {}
        }
        let manifest = read_file_manifest(&manifest_path)?;
        check_identity(&manifest_rel, &manifest, partition, period)?;
        dsh::strip_prefix(&manifest_rel, &manifest.content_hash)?;
        let columns = read_verified(
            &parquet_path,
            &file_rel,
            partition,
            period,
            &manifest.content_hash,
        )?;
        check_field(
            &manifest_rel,
            "row_count",
            manifest.row_count,
            columns.len() as u64,
        )?;
        let times = columns.open_time_ms();
        if let (Some(&min), Some(&max)) = (times.first(), times.last()) {
            check_field(&manifest_rel, "ts_min", manifest.ts_min, min)?;
            check_field(&manifest_rel, "ts_max", manifest.ts_max, max)?;
        }
        if times.iter().any(|ts| window.contains(*ts)) {
            append(&mut all, columns)?;
            kept.push(Kept {
                period,
                file_rel,
                manifest,
            });
        }
    }
    Ok(SeriesRead {
        kept,
        columns: all.unwrap_or_else(|| Columns::with_capacity(0)),
    })
}

/// Construit un dataset figé : manifeste `datasets/<nom>.json` et snapshots des fichiers non clos.
///
/// Étapes, dans l'ordre :
///
/// - nom et fenêtre contrôlés, puis verrou du nom (`datasets/<nom>.json` est commun à toutes les
///   partitions : sans lui, deux builds de même nom sur deux partitions se dépassent et le second
///   écrase le premier) et verrou de partition (une ingestion ne peut pas réécrire un mois pendant
///   qu'on le copie) ;
/// - filtre de fichiers par période (`Period::covering`) : un fichier absent avec son manifeste est un
///   trou de données ;
/// - relecture complète et vérification du `content_hash` de chaque fichier, puis `dataset_hash` sur
///   la concaténation ;
/// - immutabilité du manifeste (même hash : rien à faire ; autre hash : [`StoreError::DatasetExists`]) ;
/// - snapshots sous `datasets/snapshots/<dataset_hash>/`, puis écriture du manifeste.
pub fn build_dataset(
    root: &Path,
    request: &BuildRequest<'_>,
    now_ms: Ts,
) -> Result<BuildOutcome, StoreError> {
    build_partitions(
        root,
        request.name,
        std::slice::from_ref(&request.partition),
        request.window,
        request.gaps_policy,
        None,
        now_ms,
    )
}

/// Construit un dataset figé multi-séries : une série par symbole, même exchange, même timeframe,
/// même fenêtre et même `gaps_policy` (S9, docs/specs/s9.md §12).
///
/// Mêmes étapes que [`build_dataset`], avec trois différences :
///
/// - les symboles sont triés en ordre d'octets : l'ordre de la demande n'a aucun effet sur le
///   manifeste ni sur le `dataset_hash` ;
/// - les verrous de partition sont pris dans cet ordre trié, ce qui exclut l'interblocage entre deux
///   builds qui partagent des symboles ;
/// - un symbole sans aucune ligne dans la fenêtre est une erreur ([`StoreError::SymbolWithoutData`]) :
///   une série vide ne doit pas passer inaperçue dans un univers.
///
/// Avec un seul symbole, le résultat est identique à celui de [`build_dataset`] à l'octet.
pub fn build_dataset_multi(
    root: &Path,
    request: &MultiBuildRequest<'_>,
    now_ms: Ts,
) -> Result<BuildOutcome, StoreError> {
    let partitions = multi_partitions(request)?;
    build_partitions(
        root,
        request.name,
        &partitions,
        request.window,
        request.gaps_policy,
        None,
        now_ms,
    )
}

/// Construit un dataset figé multi-séries d'un bloc de recherche (s11 §6.2) : les contrôles de
/// [`build_dataset_multi`], plus :
///
/// - `gaps_policy` doit valoir `zero_volume_marked` ([`StoreError::ResearchGapsPolicy`]) et le
///   timeframe `1m` ([`StoreError::ResearchTimeframe`]) ;
/// - la fenêtre doit commencer au plus tard au début du bloc et finir après ce début, sans dépasser la
///   fin d'un bloc fermé ([`StoreError::ResearchWindow`]) ; la fenêtre de décision est
///   `decision_window(bloc, fin de fenêtre)` ;
/// - les exclusions qualité de chaque symbole sont calculées par [`quality_report`] sur sa série 1m
///   complète du store (seaux attendus depuis la première bougie de la série, pas du dataset), pour les
///   TF de 5m à 1d, et écrites pour les trimestres qui recoupent la fenêtre de décision et ont au moins
///   un TF exclu. Avec `universe_quality`, elles sont comparées à celles des CSV de l'univers
///   ([`StoreError::UniverseQualityMismatch`], [`StoreError::UniverseMemberMissing`]) ;
/// - un manifeste existant au même `dataset_hash` mais de métadonnées de recherche différentes est
///   refusé ([`StoreError::DatasetMetaMismatch`]), rien n'est écrit.
///
/// Le `dataset_hash` ne dépend d'aucune métadonnée de recherche : `block` est hors de `dsh-v1`.
pub fn build_dataset_research(
    root: &Path,
    request: &MultiBuildRequest<'_>,
    research: &ResearchMeta,
    now_ms: Ts,
) -> Result<BuildOutcome, StoreError> {
    let partitions = multi_partitions(request)?;
    build_partitions(
        root,
        request.name,
        &partitions,
        request.window,
        request.gaps_policy,
        Some(research),
        now_ms,
    )
}

/// Partitions d'une demande multi-séries : symboles triés en ordre d'octets, sans doublon.
fn multi_partitions(request: &MultiBuildRequest<'_>) -> Result<Vec<Partition>, StoreError> {
    if request.symbols.is_empty() {
        return Err(StoreError::NoSymbols);
    }
    let mut symbols: Vec<&Symbol> = request.symbols.iter().collect();
    symbols.sort();
    if let Some(pair) = symbols.windows(2).find(|p| p[0] == p[1]) {
        return Err(StoreError::DuplicateSymbol {
            symbol: pair[0].as_str().to_string(),
        });
    }
    Ok(symbols
        .into_iter()
        .map(|symbol| Partition::new(request.exchange, symbol.clone(), request.tf))
        .collect())
}

/// Métadonnées de recherche d'un manifeste de dataset, comparées d'un build à l'autre.
#[derive(Clone, Debug, Default, PartialEq)]
struct ManifestMeta {
    block: Option<Block>,
    decision_window: Option<[String; 2]>,
    universe: Option<UniverseRef>,
    quality_exclusions: Option<BTreeMap<String, Vec<QuarterExclusion>>>,
}

impl ManifestMeta {
    fn of(manifest: &DatasetManifest) -> ManifestMeta {
        ManifestMeta {
            block: manifest.block,
            decision_window: manifest.decision_window.clone(),
            universe: manifest.universe.clone(),
            quality_exclusions: manifest.quality_exclusions.clone(),
        }
    }

    /// Premier champ qui diffère de `other`, dans l'ordre du manifeste.
    fn first_difference(&self, other: &ManifestMeta) -> Option<&'static str> {
        if self.block != other.block {
            Some("block")
        } else if self.decision_window != other.decision_window {
            Some("decision_window")
        } else if self.universe != other.universe {
            Some("universe")
        } else if self.quality_exclusions != other.quality_exclusions {
            Some("quality_exclusions")
        } else {
            None
        }
    }
}

/// TF dont la qualité est jugée : de 5m à 1d. Le 1m, qui sert aux fills, n'exclut jamais rien
/// (docs/04 §2.5 et §10, décision 8).
const QUALITY_TFS: [Timeframe; 7] = [
    Timeframe::M5,
    Timeframe::M15,
    Timeframe::M30,
    Timeframe::H1,
    Timeframe::H4,
    Timeframe::H12,
    Timeframe::D1,
];

/// Contrôles d'une demande de recherche avant tout accès au store ; rend la fenêtre de décision.
fn check_research_request(
    block: Block,
    tf: Timeframe,
    gaps_policy: GapsPolicy,
    window: Window,
) -> Result<(Ts, Ts), StoreError> {
    if gaps_policy != GapsPolicy::ZeroVolumeMarked {
        return Err(StoreError::ResearchGapsPolicy { found: gaps_policy });
    }
    if tf != Timeframe::M1 {
        return Err(StoreError::ResearchTimeframe { tf });
    }
    if window.start_ms() > block.span().start_ms {
        return Err(StoreError::ResearchWindow {
            block,
            reason: "la fenêtre commence après le début du bloc".to_string(),
        });
    }
    decision_window(block, window.end_ms()).map_err(|e| StoreError::ResearchWindow {
        block,
        reason: e.to_string(),
    })
}

/// Trimestres de `partition` qui recoupent `[decision_start, decision_end)` et ont au moins un TF de 5m
/// à 1d exclu par la qualité, calculés sur la série 1m complète du store.
fn quality_exclusions_of(
    root: &Path,
    partition: &Partition,
    decision_start_ms: Ts,
    decision_end_ms: Ts,
) -> Result<Vec<QuarterExclusion>, StoreError> {
    let columns = read_series_window(root, partition, None)?;
    let report = quality_report(columns.open_time_ms(), &QUALITY_TFS)?;
    // Lignes triées par (tf croissant, trimestre) : les TF d'un trimestre s'ajoutent par durée
    // croissante.
    let mut by_quarter: BTreeMap<String, Vec<Timeframe>> = BTreeMap::new();
    for row in report.rows.iter().filter(|row| row.excluded) {
        let (start_ms, end_ms) = crate::model::universe::quarter_bounds(&row.quarter)?;
        if start_ms < decision_end_ms && end_ms > decision_start_ms {
            by_quarter
                .entry(row.quarter.clone())
                .or_default()
                .push(row.tf);
        }
    }
    Ok(by_quarter
        .into_iter()
        .map(|(quarter, tfs)| QuarterExclusion { quarter, tfs })
        .collect())
}

/// Texte des TF exclus d'un trimestre pour un message (`1h;4h`, « aucun » si vide).
fn tfs_text(tfs: &[Timeframe]) -> String {
    if tfs.is_empty() {
        "aucun".to_string()
    } else {
        tfs.iter()
            .map(|tf| tf.as_str())
            .collect::<Vec<_>>()
            .join(";")
    }
}

/// Compare les exclusions attendues par les CSV de l'univers à celles du store, pour les trimestres qui
/// recoupent la fenêtre de décision (s11 §4.3).
fn check_universe_quality(
    expected: &QualityExpectation,
    computed: &BTreeMap<String, Vec<QuarterExclusion>>,
    partitions: &[Partition],
    decision_start_ms: Ts,
    decision_end_ms: Ts,
) -> Result<(), StoreError> {
    for ((quarter, symbol), tfs) in expected.entries() {
        let (start_ms, end_ms) = crate::model::universe::quarter_bounds(quarter)?;
        if start_ms >= decision_end_ms || end_ms <= decision_start_ms {
            continue;
        }
        if !partitions.iter().any(|p| p.symbol.as_str() == symbol) {
            return Err(StoreError::UniverseMemberMissing {
                quarter: quarter.clone(),
                symbol: symbol.clone(),
            });
        }
        let store_tfs: &[Timeframe] = computed
            .get(symbol)
            .and_then(|list| list.iter().find(|e| e.quarter == *quarter))
            .map_or(&[], |e| e.tfs.as_slice());
        if store_tfs != tfs.as_slice() {
            return Err(StoreError::UniverseQualityMismatch {
                quarter: quarter.clone(),
                symbol: symbol.clone(),
                universe: tfs_text(tfs),
                store: tfs_text(store_tfs),
            });
        }
    }
    Ok(())
}

/// Cœur commun des builds mono et multi-séries et de recherche. `partitions` est non vide, trié par
/// symbole, sans doublon, et partage exchange et timeframe (garanti par les appelants).
fn build_partitions(
    root: &Path,
    name: &str,
    partitions: &[Partition],
    window: Window,
    gaps_policy: GapsPolicy,
    research: Option<&ResearchMeta>,
    now_ms: Ts,
) -> Result<BuildOutcome, StoreError> {
    check_name(name)?;
    let Some(first) = partitions.first() else {
        return Err(StoreError::NoSymbols);
    };
    let (exchange, tf) = (first.exchange, first.tf);
    window.check_aligned(tf)?;
    let decision = research
        .map(|meta| check_research_request(meta.block, tf, gaps_policy, window))
        .transpose()?;
    let window_text = [
        format_rfc3339_utc(window.start_ms())?,
        format_rfc3339_utc(window.end_ms())?,
    ];
    let built_at = format_rfc3339_utc(now_ms)?;
    let periods = Period::covering(tf, window)?;
    let _verrou_nom = DatasetLock::acquire(root, name)?;
    let _verrous = partitions
        .iter()
        .map(|partition| PartitionLock::acquire(root, partition))
        .collect::<Result<Vec<_>, _>>()?;

    // Étapes 3 et 4 : fichiers présents, vérifiés, avec au moins une ligne dans la fenêtre.
    let mut series: Vec<SeriesRead> = Vec::with_capacity(partitions.len());
    for partition in partitions {
        let read = read_series(root, partition, &periods, window)?;
        if partitions.len() > 1 && read.kept.is_empty() {
            return Err(StoreError::SymbolWithoutData {
                symbol: partition.symbol.as_str().to_string(),
                start_ms: window.start_ms(),
                end_ms: window.end_ms(),
            });
        }
        series.push(read);
    }

    // Étape 5 : dataset_hash.
    let refs: Vec<(&str, Timeframe, &Columns)> = partitions
        .iter()
        .zip(&series)
        .map(|(partition, read)| (partition.symbol.as_str(), tf, &read.columns))
        .collect();
    let (hex, rows) = dsh::dataset_hash(exchange.as_str(), window, gaps_policy, &refs)?;
    if rows == 0 {
        return Err(StoreError::EmptyDataset {
            start_ms: window.start_ms(),
            end_ms: window.end_ms(),
        });
    }
    let dataset_hash = dsh::prefixed(&hex);

    // Étape 5 bis : métadonnées de recherche, exclusions qualité sur la série complète du store.
    let new_meta = match (research, decision) {
        (Some(meta), Some((decision_start_ms, decision_end_ms))) => {
            let mut computed: BTreeMap<String, Vec<QuarterExclusion>> = BTreeMap::new();
            for partition in partitions {
                let list =
                    quality_exclusions_of(root, partition, decision_start_ms, decision_end_ms)?;
                if !list.is_empty() {
                    computed.insert(partition.symbol.as_str().to_string(), list);
                }
            }
            if let Some(expected) = &meta.universe_quality {
                check_universe_quality(
                    expected,
                    &computed,
                    partitions,
                    decision_start_ms,
                    decision_end_ms,
                )?;
            }
            ManifestMeta {
                block: Some(meta.block),
                decision_window: Some([
                    format_rfc3339_utc(decision_start_ms)?,
                    format_rfc3339_utc(decision_end_ms)?,
                ]),
                universe: meta.universe.clone(),
                quality_exclusions: Some(computed),
            }
        }
        _ => ManifestMeta::default(),
    };

    // Étape 6 : immutabilité du manifeste de dataset.
    let manifest_rel = format!("datasets/{name}.json");
    let manifest_path = root.join("datasets").join(format!("{name}.json"));
    if manifest_path.is_file() {
        let existing = read_dataset_manifest(&manifest_path)?;
        if existing.dataset_hash == dataset_hash && existing.hash_format == HASH_FORMAT {
            if let Some(field) = ManifestMeta::of(&existing).first_difference(&new_meta) {
                return Err(StoreError::DatasetMetaMismatch {
                    path: manifest_rel,
                    field,
                });
            }
            let snapshots = existing.files.iter().filter(|f| f.snapshot).count();
            return Ok(BuildOutcome {
                manifest_rel,
                manifest: existing,
                status: BuildStatus::Unchanged,
                snapshots,
            });
        }
        return Err(StoreError::DatasetExists {
            path: manifest_rel,
            existing: existing.dataset_hash,
            new: dataset_hash,
        });
    }

    // Étape 7 : snapshots des fichiers non clos, série par série dans l'ordre des symboles.
    let mut files = Vec::new();
    for (partition, read) in partitions.iter().zip(&series) {
        for item in &read.kept {
            let (path, snapshot) = if item.manifest.closed {
                (item.file_rel.clone(), false)
            } else {
                let rel = format!("datasets/snapshots/{hex}/{}", item.file_rel);
                snapshot_file(root, partition, item, &rel)?;
                (rel, true)
            };
            files.push(DatasetFile {
                path,
                symbol: partition.symbol.clone(),
                tf,
                period: item.period.label(),
                content_hash: item.manifest.content_hash.clone(),
                row_count: item.manifest.row_count,
                snapshot,
            });
        }
    }
    let snapshots = files.iter().filter(|f| f.snapshot).count();

    // Étape 8 : manifeste.
    let manifest = DatasetManifest {
        name: name.to_string(),
        exchange,
        symbols: partitions.iter().map(|p| p.symbol.clone()).collect(),
        tfs: vec![tf],
        window: window_text,
        gaps_policy,
        files,
        row_count: rows,
        dataset_hash,
        hash_format: HASH_FORMAT.to_string(),
        built_at,
        tv4_version: env!("CARGO_PKG_VERSION").to_string(),
        block: new_meta.block,
        decision_window: new_meta.decision_window,
        universe: new_meta.universe,
        quality_exclusions: new_meta.quality_exclusions,
    };
    write_dataset_manifest(&manifest_path, &manifest)?;
    Ok(BuildOutcome {
        manifest_rel,
        manifest,
        status: BuildStatus::Created,
        snapshots,
    })
}

/// Copie figée d'un fichier non clos vers `rel` (sous `datasets/snapshots/`).
///
/// Si la destination existe, son `content_hash` est recalculé : égal, elle est réutilisée ; différent,
/// [`StoreError::SnapshotMismatch`]. Sinon les octets du Parquet vivant sont copiés (le verrou
/// garantit qu'il n'a pas bougé depuis la vérification), puis la copie est relue et vérifiée : une
/// copie corrompue est une erreur, pas un snapshot.
fn snapshot_file(
    root: &Path,
    partition: &Partition,
    item: &Kept,
    rel: &str,
) -> Result<(), StoreError> {
    let destination = resolve_rel(root, rel)?;
    let expected = &item.manifest.content_hash;
    if destination.is_file() {
        return match read_verified(&destination, rel, partition, item.period, expected) {
            Ok(_) => Ok(()),
            Err(StoreError::FileHashMismatch { actual, .. }) => Err(StoreError::SnapshotMismatch {
                path: rel.to_string(),
                expected: expected.clone(),
                actual,
            }),
            Err(autre) => Err(autre),
        };
    }
    let live = resolve_rel(root, &item.file_rel)?;
    let bytes = fs::read(&live).map_err(|source| io_error(&live, source))?;
    write_bytes_durable(&destination, &bytes)?;
    read_verified(&destination, rel, partition, item.period, expected)?;
    Ok(())
}

/// Charge un dataset figé en vérifiant tous ses hashes (D6).
///
/// `manifest_path` est le chemin du manifeste (déjà joint à la racine par l'appelant). Pour chaque
/// fichier listé, le `content_hash` est recalculé sur les 7 colonnes et comparé au manifeste, puis le
/// `dataset_hash` est recalculé sur l'ensemble : tout écart est une erreur qui nomme le fichier (chemin
/// relatif, tel qu'écrit dans le manifeste). `load_dataset` ne prend pas le verrou de partition : un
/// fichier listé est soit clos (jamais modifié), soit un snapshot (jamais réécrit), et le remplacement
/// par `rename` est atomique pour un lecteur.
///
/// Un manifeste de recherche (champ `block` ou autre champ de S11) n'est pas mono-série : il se joue en
/// portefeuille, par [`load_dataset_multi`] ([`StoreError::ResearchNeedsPortfolio`]).
///
/// `load_dataset` n'applique aucun refus de politique : il sait charger `zero_volume_marked`. Le refus
/// de cette politique est dans `tv4 backtest`.
pub fn load_dataset(root: &Path, manifest_path: &Path) -> Result<LoadedDataset, StoreError> {
    let shown = manifest_path.display().to_string();
    let manifest = read_dataset_manifest(manifest_path)?;
    if manifest.block.is_some()
        || manifest.decision_window.is_some()
        || manifest.universe.is_some()
        || manifest.quality_exclusions.is_some()
    {
        // Un dataset de recherche ne se joue qu'en portefeuille (s11 §6.2 et §8.1).
        return Err(StoreError::ResearchNeedsPortfolio { path: shown });
    }
    if manifest.hash_format != HASH_FORMAT {
        return Err(StoreError::UnsupportedHashFormat {
            path: shown,
            found: manifest.hash_format,
        });
    }
    let (Some(symbol), Some(&tf), 1, 1) = (
        manifest.symbols.first(),
        manifest.tfs.first(),
        manifest.symbols.len(),
        manifest.tfs.len(),
    ) else {
        return Err(StoreError::MultiSeriesUnsupported {
            symbols: manifest.symbols.len(),
            tfs: manifest.tfs.len(),
        });
    };
    let window = Window::new(
        parse_rfc3339_utc(&manifest.window[0])?,
        parse_rfc3339_utc(&manifest.window[1])?,
    )?;
    let expected_hex = dsh::strip_prefix(&shown, &manifest.dataset_hash)?;
    let partition = Partition::new(manifest.exchange, symbol.clone(), tf);

    let mut all = read_series_files(root, manifest.files.iter(), &partition)?;
    all.check_strictly_increasing()?;
    let (hex, rows) = dsh::dataset_hash(
        manifest.exchange.as_str(),
        window,
        manifest.gaps_policy,
        &[(symbol.as_str(), tf, &all)],
    )?;
    if hex != expected_hex {
        return Err(StoreError::DatasetHashMismatch {
            path: shown,
            expected: manifest.dataset_hash,
            actual: dsh::prefixed(&hex),
        });
    }
    check_field(&shown, "row_count", manifest.row_count, rows)?;
    all.retain_window(window);
    let columns = fill_gaps(all, tf, manifest.gaps_policy)?;
    Ok(LoadedDataset { manifest, columns })
}

/// Lit les fichiers listés d'une série (tous de la partition donnée), dans l'ordre du manifeste, avec
/// les contrôles de symbole, de timeframe, de période strictement croissante, de `content_hash` et de
/// `row_count`. Rend les lignes concaténées (vides si `files` est vide).
fn read_series_files<'a>(
    root: &Path,
    files: impl Iterator<Item = &'a DatasetFile>,
    partition: &Partition,
) -> Result<Columns, StoreError> {
    let (symbol, tf) = (&partition.symbol, partition.tf);
    let mut all: Option<Columns> = None;
    let mut previous: Option<Period> = None;
    for file in files {
        check_field(&file.path, "symbol", file.symbol.as_str(), symbol.as_str())?;
        check_field(&file.path, "tf", file.tf, tf)?;
        let period = Period::parse(tf, &file.period)?;
        if previous.is_some_and(|p| p >= period) {
            return Err(StoreError::ManifestMismatch {
                path: file.path.clone(),
                field: "period",
                manifest: file.period.clone(),
                actual: "périodes strictement croissantes".to_string(),
            });
        }
        previous = Some(period);
        dsh::strip_prefix(&file.path, &file.content_hash)?;
        let path = resolve_rel(root, &file.path)?;
        if !path.is_file() {
            return Err(StoreError::DataFileMissing {
                path: file.path.clone(),
            });
        }
        let columns = read_verified(&path, &file.path, partition, period, &file.content_hash)?;
        check_field(
            &file.path,
            "row_count",
            file.row_count,
            columns.len() as u64,
        )?;
        append(&mut all, columns)?;
    }
    Ok(all.unwrap_or_else(|| Columns::with_capacity(0)))
}

/// Charge un dataset figé multi-séries en vérifiant tous ses hashes (S9, docs/specs/s9.md §12).
///
/// Un manifeste mono-série se charge aussi : il rend une série. Le manifeste doit lister ses
/// symboles triés en ordre d'octets, sans doublon, avec un seul timeframe ; ses fichiers sont groupés
/// par symbole dans cet ordre. Le `content_hash` de chaque fichier puis le `dataset_hash` sur tous les
/// segments (`dsh-v1` §5.2) sont recalculés ; la politique de trous est appliquée série par série.
///
/// [`load_dataset`] garde son refus des datasets multi-séries : le chemin du jalon 1 ne lit jamais
/// un dataset multi-séries par erreur.
pub fn load_dataset_multi(root: &Path, manifest_path: &Path) -> Result<MultiDataset, StoreError> {
    let shown = manifest_path.display().to_string();
    let manifest = read_dataset_manifest(manifest_path)?;
    if manifest.hash_format != HASH_FORMAT {
        return Err(StoreError::UnsupportedHashFormat {
            path: shown,
            found: manifest.hash_format,
        });
    }
    let (Some(&tf), 1) = (manifest.tfs.first(), manifest.tfs.len()) else {
        return Err(StoreError::MultiTimeframeUnsupported {
            tfs: manifest.tfs.len(),
        });
    };
    if manifest.symbols.is_empty() {
        return Err(StoreError::NoSymbols);
    }
    if let Some(pair) = manifest.symbols.windows(2).find(|p| p[0] >= p[1]) {
        return Err(StoreError::ManifestMismatch {
            path: shown,
            field: "symbols",
            manifest: format!("{}, {}", pair[0].as_str(), pair[1].as_str()),
            actual: "symboles triés en ordre d'octets, sans doublon".to_string(),
        });
    }
    let window = Window::new(
        parse_rfc3339_utc(&manifest.window[0])?,
        parse_rfc3339_utc(&manifest.window[1])?,
    )?;
    let expected_hex = dsh::strip_prefix(&shown, &manifest.dataset_hash)?;

    // Fichiers groupés par symbole : ils doivent suivre l'ordre des symboles du manifeste, et chacun
    // doit appartenir à l'un d'eux.
    let mut groups: Vec<Vec<&DatasetFile>> = vec![Vec::new(); manifest.symbols.len()];
    let mut cursor = 0_usize;
    for file in &manifest.files {
        let Some(index) = manifest.symbols.iter().position(|s| *s == file.symbol) else {
            return Err(StoreError::ManifestMismatch {
                path: file.path.clone(),
                field: "symbol",
                manifest: file.symbol.as_str().to_string(),
                actual: "symbole absent de `symbols`".to_string(),
            });
        };
        if index < cursor {
            return Err(StoreError::ManifestMismatch {
                path: file.path.clone(),
                field: "symbol",
                manifest: file.symbol.as_str().to_string(),
                actual: "fichiers groupés par symbole dans l'ordre de `symbols`".to_string(),
            });
        }
        cursor = index;
        groups[index].push(file);
    }

    let mut loaded: Vec<(Symbol, Columns)> = Vec::with_capacity(groups.len());
    for (symbol, group) in manifest.symbols.iter().zip(&groups) {
        let partition = Partition::new(manifest.exchange, symbol.clone(), tf);
        let columns = read_series_files(root, group.iter().copied(), &partition)?;
        columns.check_strictly_increasing()?;
        loaded.push((symbol.clone(), columns));
    }
    let refs: Vec<(&str, Timeframe, &Columns)> = loaded
        .iter()
        .map(|(symbol, columns)| (symbol.as_str(), tf, columns))
        .collect();
    let (hex, rows) = dsh::dataset_hash(
        manifest.exchange.as_str(),
        window,
        manifest.gaps_policy,
        &refs,
    )?;
    if hex != expected_hex {
        return Err(StoreError::DatasetHashMismatch {
            path: shown,
            expected: manifest.dataset_hash,
            actual: dsh::prefixed(&hex),
        });
    }
    check_field(&shown, "row_count", manifest.row_count, rows)?;
    let mut series = Vec::with_capacity(loaded.len());
    for (symbol, mut columns) in loaded {
        columns.retain_window(window);
        let filled = fill_gaps(columns, tf, manifest.gaps_policy)?;
        series.push((symbol.as_str().to_string(), filled));
    }
    Ok(MultiDataset { manifest, series })
}

/// Lit et recoupe les métadonnées de recherche d'un manifeste (s11 §6.2).
///
/// - Aucun des quatre champs : `Ok(None)`, c'est un manifeste ordinaire (S4 à S10).
/// - `decision_window`, `universe` ou `quality_exclusions` sans `block`, ou `block` sans
///   `decision_window` ni `quality_exclusions` : [`StoreError::ResearchMetaInvalid`].
/// - `decision_window` recalculée à partir de `block` et de la fin de `window` : tout écart est
///   [`StoreError::ResearchMetaInvalid`] ; une fenêtre incompatible avec le bloc est
///   [`StoreError::ResearchWindow`] ; une `gaps_policy` autre que `zero_volume_marked`,
///   [`StoreError::ResearchGapsPolicy`].
/// - `quality_exclusions` est résolu en [`QualitySchedule`] (trimestres ordonnés, jamais le 1m).
pub fn research_meta(manifest: &DatasetManifest) -> Result<Option<ResearchInfo>, StoreError> {
    let invalid =
        |field: &'static str, reason: String| StoreError::ResearchMetaInvalid { field, reason };
    let Some(block) = manifest.block else {
        for (field, present) in [
            ("decision_window", manifest.decision_window.is_some()),
            ("universe", manifest.universe.is_some()),
            ("quality_exclusions", manifest.quality_exclusions.is_some()),
        ] {
            if present {
                return Err(invalid(field, "présent sans `block`".to_string()));
            }
        }
        return Ok(None);
    };
    let Some(stored) = &manifest.decision_window else {
        return Err(invalid(
            "decision_window",
            "absent alors que `block` est présent".to_string(),
        ));
    };
    let Some(exclusions) = &manifest.quality_exclusions else {
        return Err(invalid(
            "quality_exclusions",
            "absent alors que `block` est présent".to_string(),
        ));
    };
    if manifest.gaps_policy != GapsPolicy::ZeroVolumeMarked {
        return Err(StoreError::ResearchGapsPolicy {
            found: manifest.gaps_policy,
        });
    }
    let window = Window::new(
        parse_rfc3339_utc(&manifest.window[0])?,
        parse_rfc3339_utc(&manifest.window[1])?,
    )?;
    let (decision_start_ms, decision_end_ms) =
        check_research_request(block, Timeframe::M1, manifest.gaps_policy, window)?;
    let expected = [
        format_rfc3339_utc(decision_start_ms)?,
        format_rfc3339_utc(decision_end_ms)?,
    ];
    if *stored != expected {
        return Err(invalid(
            "decision_window",
            format!(
                "[{}, {}) écrite, [{}, {}) recalculée à partir du bloc {block} et de la fenêtre",
                stored[0], stored[1], expected[0], expected[1]
            ),
        ));
    }
    let quality = QualitySchedule::from_exclusions(exclusions.clone())?;
    Ok(Some(ResearchInfo {
        block,
        decision_start_ms,
        decision_end_ms,
        universe: manifest.universe.clone(),
        quality,
    }))
}

/// Périodes dont un fichier ou un manifeste existe dans la partition, dans l'ordre. Un dossier absent
/// donne une liste vide.
fn discover_periods(root: &Path, partition: &Partition) -> Result<Vec<Period>, StoreError> {
    let dir = resolve_rel(root, &partition.dir_rel())?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error(&dir, source)),
    };
    let mut periods = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error(&dir, source))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let label = name
            .strip_suffix("._manifest.json")
            .or_else(|| name.strip_suffix(".parquet"));
        if let Some(label) = label {
            periods.insert(Period::parse(partition.tf, label)?);
        }
    }
    Ok(periods.into_iter().collect())
}

/// Lit une série du store avec vérification : pour chaque fichier retenu, `content_hash` recalculé sur
/// les 7 colonnes, `row_count`, `ts_min` et `ts_max` recoupés avec le manifeste du fichier. Rend les
/// lignes **réelles** (aucune bougie synthétique) strictement croissantes.
///
/// - `window = Some(w)` : les lignes de `[début, fin)`, lues dans les seuls fichiers qui touchent `w` ;
/// - `window = None` : toute la série, tous les fichiers de la partition (c'est la lecture des
///   exclusions qualité, s11 §4.3, et de `tv4 universe verify`, §4.5).
///
/// Une partition absente rend une série vide. La lecture ne prend pas de verrou : un fichier clos n'est
/// jamais modifié et le remplacement d'un fichier non clos est atomique (comme [`load_dataset`]).
pub fn read_series_window(
    root: &Path,
    partition: &Partition,
    window: Option<Window>,
) -> Result<Columns, StoreError> {
    let (periods, wide) = match window {
        Some(window) => (Period::covering(partition.tf, window)?, window),
        None => (
            discover_periods(root, partition)?,
            Window::new(0, MAX_TS_MS)?,
        ),
    };
    let mut columns = read_series(root, partition, &periods, wide)?.columns;
    columns.check_strictly_increasing()?;
    columns.retain_window(wide);
    Ok(columns)
}

/// Applique la politique de trous aux lignes réelles d'une fenêtre (strictement croissantes,
/// alignées sur `tf`).
///
/// `Skip` : `columns` est rendu tel quel, sans copie. `Ffill` et `ZeroVolumeMarked` : entre deux
/// bougies réelles consécutives séparées de plus d'une période, insère une bougie par période
/// manquante, avec `open = high = low = close =` close de la bougie réelle précédente,
/// `volume = 0.0`, `trades = None` et `synthetic = true`. Rien avant la première ni après la dernière
/// bougie réelle. Les deux politiques produisent les mêmes bougies : elles diffèrent par l'en-tête du
/// hash et par ce que le simulateur aura le droit d'y faire (S6 : aucun fill dans une bougie
/// `zero_volume_marked`).
///
/// L'entrée est contrôlée avant toute allocation, quelle que soit la politique : ordre strictement
/// croissant ([`StoreError::NotStrictlyIncreasing`]) et horodatages dans `[0, MAX_TS_MS)`
/// ([`StoreError::TimestampOutOfRange`]), ce qui exclut tout débordement des écarts. Un écart qui
/// n'est pas un multiple de la durée de `tf` est une erreur
/// ([`crate::model::ModelError::MisalignedOpenTime`], impossible si le store a écrit les données),
/// et plus de [`MAX_SYNTHETIC_BARS`] bougies à créer aussi ([`StoreError::TooManySyntheticBars`]).
pub fn fill_gaps(
    columns: Columns,
    tf: Timeframe,
    policy: GapsPolicy,
) -> Result<Columns, StoreError> {
    fill_gaps_bounded(columns, tf, policy, MAX_SYNTHETIC_BARS)
}

/// [`fill_gaps`] avec une borne explicite sur les bougies synthétiques (les tests la réduisent).
fn fill_gaps_bounded(
    columns: Columns,
    tf: Timeframe,
    policy: GapsPolicy,
    max_synthetic: u64,
) -> Result<Columns, StoreError> {
    columns.check_strictly_increasing()?;
    let times = columns.open_time_ms();
    // Strictement croissant : la première et la dernière valeur bornent toutes les autres.
    for ts in [times.first(), times.last()].into_iter().flatten() {
        if !(0..MAX_TS_MS).contains(ts) {
            return Err(StoreError::TimestampOutOfRange { ts_ms: *ts });
        }
    }
    if policy == GapsPolicy::Skip {
        return Ok(columns);
    }
    let step = tf.ms();
    // Premier passage : alignement et nombre de bougies à créer, avant d'allouer quoi que ce soit.
    let mut synthetic: u64 = 0;
    for pair in times.windows(2) {
        let gap = pair[1] - pair[0];
        if gap % step != 0 {
            return Err(crate::model::ModelError::MisalignedOpenTime {
                timeframe: tf,
                open_time_ms: pair[1],
            }
            .into());
        }
        synthetic = synthetic.saturating_add((gap / step - 1).unsigned_abs());
    }
    if synthetic > max_synthetic {
        return Err(StoreError::TooManySyntheticBars {
            count: synthetic,
            max: max_synthetic,
        });
    }
    if synthetic == 0 {
        return Ok(columns);
    }
    // `synthetic <= max_synthetic` : une borne de l'ordre de 10^7, loin de `usize::MAX`.
    let mut out = Columns::with_capacity(columns.len() + usize::try_from(synthetic).unwrap_or(0));
    // `with_capacity` crée toujours la colonne trades ; si l'entrée n'en a pas, on la retire.
    if columns.trades().is_none() {
        let mut parts = out.into_parts();
        parts.trades = None;
        out = Columns::from_parts(parts)?;
    }
    for index in 0..columns.len() {
        let Some(candle) = columns.get(index) else {
            continue;
        };
        out.push(&candle, columns.synthetic()[index]);
        let Some(&next) = columns.open_time_ms().get(index + 1) else {
            continue;
        };
        let mut t = candle.open_time_ms + step;
        while t < next {
            out.push(
                &Candle {
                    open_time_ms: t,
                    open: candle.close,
                    high: candle.close,
                    low: candle.close,
                    close: candle.close,
                    volume: 0.0,
                    trades: None,
                },
                true,
            );
            t += step;
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // une plage de lignes = un row group
mod tests {
    use super::*;
    use crate::model::ExchangeId;
    use crate::store::partition::write_partition;
    use serde_json::Value;

    const T_2024_01: i64 = 1_704_067_200_000;
    const T_2024_02: i64 = 1_706_745_600_000;
    const T_2024_03: i64 = 1_709_251_200_000;
    /// 2023-12-31T23:58:00Z... plus précisément 2024-01-31T23:58:00Z.
    const FIN_JANVIER: i64 = 1_706_745_480_000;
    const MINUTE: i64 = 60_000;
    /// 2030-01-01T00:00:00Z : après toutes les périodes des tests.
    const FUTUR: i64 = 1_893_456_000_000;
    /// Un jour dans février 2024 : janvier est clos, février ne l'est pas.
    const EN_FEVRIER: i64 = T_2024_02 + 86_400_000;

    fn partition() -> Partition {
        Partition::new(
            ExchangeId::Kraken,
            "SYN-USD".parse().unwrap(),
            Timeframe::M1,
        )
    }

    /// `n` bougies 1m synthétiques (prix inventés) depuis `debut`.
    fn bougies(debut: i64, n: usize) -> Columns {
        let candles: Vec<Candle> = (0..n)
            .map(|i| {
                let base = 100.0 + (i % 40) as f64 * 0.5;
                Candle {
                    open_time_ms: debut + i as i64 * MINUTE,
                    open: base,
                    high: base + 1.0,
                    low: base - 1.0,
                    close: base + 0.25,
                    volume: 1.0 + (i % 5) as f64,
                    trades: if i % 2 == 0 { Some(i as u32) } else { None },
                }
            })
            .collect();
        Columns::from_candles(&candles)
    }

    fn ecrire(root: &Path, colonnes: &Columns, now_ms: Ts) {
        write_partition(root, &partition(), colonnes, "synthetic", now_ms).unwrap();
    }

    fn demande(name: &str, debut: i64, fin: i64, politique: GapsPolicy) -> BuildRequest<'_> {
        BuildRequest {
            name,
            partition: partition(),
            window: Window::new(debut, fin).unwrap(),
            gaps_policy: politique,
        }
    }

    fn modifier_json(path: &Path, f: impl FnOnce(&mut Value)) {
        let mut valeur: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        f(&mut valeur);
        fs::write(path, serde_json::to_vec_pretty(&valeur).unwrap()).unwrap();
    }

    fn bits(c: &Columns) -> Vec<[u64; 5]> {
        (0..c.len())
            .map(|i| {
                [
                    c.open()[i].to_bits(),
                    c.high()[i].to_bits(),
                    c.low()[i].to_bits(),
                    c.close()[i].to_bits(),
                    c.volume()[i].to_bits(),
                ]
            })
            .collect()
    }

    #[test]
    fn build_puis_load_skip() {
        let dir = tempfile::tempdir().unwrap();
        let colonnes = bougies(T_2024_01, 20);
        ecrire(dir.path(), &colonnes, FUTUR);
        // Fenêtre : les 10 premières minutes.
        let r = demande(
            "essai",
            T_2024_01,
            T_2024_01 + 10 * MINUTE,
            GapsPolicy::Skip,
        );
        let sortie = build_dataset(dir.path(), &r, FUTUR).unwrap();
        assert_eq!(sortie.status, BuildStatus::Created);
        assert_eq!(sortie.manifest_rel, "datasets/essai.json");
        assert_eq!(sortie.manifest.row_count, 10);
        assert_eq!(sortie.snapshots, 0);
        assert_eq!(sortie.manifest.window[0], "2024-01-01T00:00:00Z");
        assert_eq!(sortie.manifest.window[1], "2024-01-01T00:10:00Z");
        assert_eq!(sortie.manifest.files.len(), 1);
        assert_eq!(sortie.manifest.files[0].row_count, 20);
        assert!(!sortie.manifest.files[0].snapshot);
        let charge = load_dataset(dir.path(), &dir.path().join("datasets/essai.json")).unwrap();
        let attendu = colonnes.slice(0..10);
        assert_eq!(charge.columns.len(), 10);
        assert_eq!(bits(&charge.columns), bits(&attendu));
        assert_eq!(charge.columns.open_time_ms(), attendu.open_time_ms());
        assert_eq!(charge.columns.trades(), attendu.trades());
        assert_eq!(charge.columns.synthetic_count(), 0);
        assert_eq!(charge.manifest, sortie.manifest);
    }

    #[test]
    fn build_selectionne_les_fichiers_par_periode() {
        let dir = tempfile::tempdir().unwrap();
        for debut in [T_2024_01, T_2024_02, T_2024_03] {
            ecrire(dir.path(), &bougies(debut, 5), FUTUR);
        }
        let r = demande(
            "fevrier",
            T_2024_02,
            T_2024_02 + 5 * MINUTE,
            GapsPolicy::Skip,
        );
        let sortie = build_dataset(dir.path(), &r, FUTUR).unwrap();
        assert_eq!(sortie.manifest.files.len(), 1);
        assert_eq!(
            sortie.manifest.files[0].path,
            "data/candles/kraken/SYN-USD/1m/2024-02.parquet"
        );
        // Sur deux mois : deux fichiers, dans l'ordre.
        let r = demande(
            "deux_mois",
            T_2024_01,
            T_2024_02 + 5 * MINUTE,
            GapsPolicy::Skip,
        );
        let sortie = build_dataset(dir.path(), &r, FUTUR).unwrap();
        let periodes: Vec<&str> = sortie
            .manifest
            .files
            .iter()
            .map(|f| f.period.as_str())
            .collect();
        assert_eq!(periodes, vec!["2024-01", "2024-02"]);
        assert_eq!(sortie.manifest.row_count, 10);
    }

    #[test]
    fn fichier_sans_ligne_dans_la_fenetre_non_liste() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 1), FUTUR);
        ecrire(dir.path(), &bougies(T_2024_02, 3), FUTUR);
        // La fenêtre commence une heure après l'unique bougie de janvier.
        let r = demande(
            "sans_janvier",
            T_2024_01 + 60 * MINUTE,
            T_2024_02 + 3 * MINUTE,
            GapsPolicy::Skip,
        );
        let sortie = build_dataset(dir.path(), &r, FUTUR).unwrap();
        assert_eq!(sortie.manifest.files.len(), 1);
        assert_eq!(sortie.manifest.files[0].period, "2024-02");
        assert_eq!(sortie.manifest.row_count, 3);
    }

    #[test]
    fn snapshot_des_mois_non_clos() {
        let dir = tempfile::tempdir().unwrap();
        // Deux bougies en janvier (clos), quatre en février (non clos).
        ecrire(dir.path(), &bougies(FIN_JANVIER, 6), EN_FEVRIER);
        let r = demande(
            "snap",
            FIN_JANVIER,
            FIN_JANVIER + 6 * MINUTE,
            GapsPolicy::Skip,
        );
        let sortie = build_dataset(dir.path(), &r, EN_FEVRIER).unwrap();
        assert_eq!(sortie.snapshots, 1);
        let hex = sortie
            .manifest
            .dataset_hash
            .strip_prefix("blake3:")
            .unwrap();
        let [janvier, fevrier] = sortie.manifest.files.as_slice() else {
            panic!("deux fichiers attendus");
        };
        assert!(!janvier.snapshot);
        assert_eq!(
            janvier.path,
            "data/candles/kraken/SYN-USD/1m/2024-01.parquet"
        );
        assert!(fevrier.snapshot);
        assert_eq!(
            fevrier.path,
            format!("datasets/snapshots/{hex}/data/candles/kraken/SYN-USD/1m/2024-02.parquet")
        );
        let vivant = dir
            .path()
            .join("data/candles/kraken/SYN-USD/1m/2024-02.parquet");
        let copie = resolve_rel(dir.path(), &fevrier.path).unwrap();
        assert_eq!(fs::read(&vivant).unwrap(), fs::read(&copie).unwrap());
        let avant = load_dataset(dir.path(), &dir.path().join("datasets/snap.json")).unwrap();
        assert_eq!(avant.columns.len(), 6);

        // Le mois vivant continue d'être alimenté (plus de lignes, toujours non clos).
        ecrire(dir.path(), &bougies(T_2024_02, 10), EN_FEVRIER + 1000);
        assert_ne!(fs::read(&vivant).unwrap(), fs::read(&copie).unwrap());
        let apres = load_dataset(dir.path(), &dir.path().join("datasets/snap.json")).unwrap();
        assert_eq!(apres.columns, avant.columns);
    }

    #[test]
    fn rebuild_meme_hash_sans_effet() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        let r = demande("fixe", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        let premier = build_dataset(dir.path(), &r, FUTUR).unwrap();
        let chemin = dir.path().join("datasets/fixe.json");
        let octets = fs::read(&chemin).unwrap();
        let second = build_dataset(dir.path(), &r, FUTUR + 1_000_000).unwrap();
        assert_eq!(second.status, BuildStatus::Unchanged);
        assert_eq!(second.manifest, premier.manifest);
        assert_eq!(fs::read(&chemin).unwrap(), octets);
    }

    #[test]
    fn rebuild_autre_hash_refuse() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        let skip = demande("fixe", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        let premier = build_dataset(dir.path(), &skip, FUTUR).unwrap();
        let ffill = demande(
            "fixe",
            T_2024_01,
            T_2024_01 + 10 * MINUTE,
            GapsPolicy::Ffill,
        );
        let erreur = build_dataset(dir.path(), &ffill, FUTUR).unwrap_err();
        let StoreError::DatasetExists {
            path,
            existing,
            new,
        } = &erreur
        else {
            panic!("DatasetExists attendu, reçu {erreur}");
        };
        assert_eq!(path, "datasets/fixe.json");
        assert_eq!(existing, &premier.manifest.dataset_hash);
        assert_ne!(new, existing);
        assert!(erreur.to_string().contains("datasets/fixe.json"));
    }

    /// Dataset de référence sur un mois clos de 20 bougies.
    fn dataset_clos(root: &Path) -> std::path::PathBuf {
        ecrire(root, &bougies(T_2024_01, 20), FUTUR);
        let r = demande("ref", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        build_dataset(root, &r, FUTUR).unwrap();
        root.join("datasets/ref.json")
    }

    #[test]
    fn load_refuse_un_fichier_modifie() {
        let dir = tempfile::tempdir().unwrap();
        let manifeste = dataset_clos(dir.path());
        // Remplacement du Parquet clos par un autre Parquet valide.
        let chemin = dir
            .path()
            .join("data/candles/kraken/SYN-USD/1m/2024-01.parquet");
        let autre = bougies(T_2024_01, 21);
        super::super::durable::write_durable(&chemin, |f| {
            super::super::parquet::write_parquet(f, &chemin, &autre, &[0..21])
        })
        .unwrap();
        let erreur = load_dataset(dir.path(), &manifeste).unwrap_err();
        assert!(
            matches!(erreur, StoreError::FileHashMismatch { .. }),
            "{erreur}"
        );
        assert!(
            erreur
                .to_string()
                .contains("`data/candles/kraken/SYN-USD/1m/2024-01.parquet`"),
            "{erreur}"
        );
    }

    #[test]
    fn load_refuse_un_dataset_hash_falsifie() {
        let dir = tempfile::tempdir().unwrap();
        let manifeste = dataset_clos(dir.path());
        modifier_json(&manifeste, |v| {
            v["dataset_hash"] = Value::String(format!("blake3:{}", "0".repeat(64)));
        });
        assert!(matches!(
            load_dataset(dir.path(), &manifeste),
            Err(StoreError::DatasetHashMismatch { .. })
        ));
        modifier_json(&manifeste, |v| {
            v["dataset_hash"] = Value::String("pas-un-hash".to_string());
        });
        assert!(matches!(
            load_dataset(dir.path(), &manifeste),
            Err(StoreError::MalformedHash { .. })
        ));
    }

    #[test]
    fn load_refuse_hash_format_inconnu() {
        let dir = tempfile::tempdir().unwrap();
        let manifeste = dataset_clos(dir.path());
        modifier_json(&manifeste, |v| {
            v["hash_format"] = Value::String("dsh-v2".to_string());
        });
        assert!(matches!(
            load_dataset(dir.path(), &manifeste),
            Err(StoreError::UnsupportedHashFormat { .. })
        ));
    }

    #[test]
    fn load_refuse_chemin_hors_racine() {
        let dir = tempfile::tempdir().unwrap();
        let manifeste = dataset_clos(dir.path());
        for chemin in ["../secret.parquet", "/etc/passwd", "runs/x.parquet"] {
            modifier_json(&manifeste, |v| {
                v["files"][0]["path"] = Value::String(chemin.to_string());
            });
            assert!(
                matches!(
                    load_dataset(dir.path(), &manifeste),
                    Err(StoreError::UnsafePath { .. })
                ),
                "{chemin}"
            );
        }
    }

    #[test]
    fn load_refuse_multi_series() {
        let dir = tempfile::tempdir().unwrap();
        let manifeste = dataset_clos(dir.path());
        modifier_json(&manifeste, |v| {
            v["symbols"] = serde_json::json!(["SYN-USD", "OTH-USD"]);
        });
        assert!(matches!(
            load_dataset(dir.path(), &manifeste),
            Err(StoreError::MultiSeriesUnsupported { symbols: 2, tfs: 1 })
        ));
    }

    #[test]
    fn load_refuse_un_fichier_absent() {
        let dir = tempfile::tempdir().unwrap();
        let manifeste = dataset_clos(dir.path());
        fs::remove_file(
            dir.path()
                .join("data/candles/kraken/SYN-USD/1m/2024-01.parquet"),
        )
        .unwrap();
        assert!(matches!(
            load_dataset(dir.path(), &manifeste),
            Err(StoreError::DataFileMissing { .. })
        ));
    }

    #[test]
    fn build_refuse_manifeste_incoherent() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        let manifeste_fichier = dir
            .path()
            .join("data/candles/kraken/SYN-USD/1m/2024-01._manifest.json");
        modifier_json(&manifeste_fichier, |v| {
            v["content_hash"] = Value::String(format!("blake3:{}", "1".repeat(64)));
        });
        let r = demande("x", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        assert!(matches!(
            build_dataset(dir.path(), &r, FUTUR),
            Err(StoreError::FileHashMismatch { .. })
        ));
        assert!(!dir.path().join("datasets/x.json").exists());
    }

    #[test]
    fn build_refuse_row_count_incoherent() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        let manifeste_fichier = dir
            .path()
            .join("data/candles/kraken/SYN-USD/1m/2024-01._manifest.json");
        modifier_json(&manifeste_fichier, |v| v["row_count"] = Value::from(19));
        let r = demande("x", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        assert!(matches!(
            build_dataset(dir.path(), &r, FUTUR),
            Err(StoreError::ManifestMismatch {
                field: "row_count",
                ..
            })
        ));
    }

    #[test]
    fn build_refuse_parquet_sans_manifeste() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        fs::remove_file(
            dir.path()
                .join("data/candles/kraken/SYN-USD/1m/2024-01._manifest.json"),
        )
        .unwrap();
        let r = demande("x", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        assert!(matches!(
            build_dataset(dir.path(), &r, FUTUR),
            Err(StoreError::ManifestMissing { .. })
        ));
    }

    #[test]
    fn build_refuse_fenetre_non_alignee() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        let r = demande(
            "x",
            T_2024_01 + 1,
            T_2024_01 + 10 * MINUTE,
            GapsPolicy::Skip,
        );
        assert!(matches!(
            build_dataset(dir.path(), &r, FUTUR),
            Err(StoreError::WindowNotAligned { .. })
        ));
    }

    #[test]
    fn build_refuse_dataset_vide() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        // Fenêtre dans le fichier mais après la dernière bougie, puis fenêtre sans fichier.
        for (debut, fin) in [
            (T_2024_01 + 100 * MINUTE, T_2024_01 + 110 * MINUTE),
            (T_2024_03, T_2024_03 + 10 * MINUTE),
        ] {
            let r = demande("vide", debut, fin, GapsPolicy::Skip);
            assert!(matches!(
                build_dataset(dir.path(), &r, FUTUR),
                Err(StoreError::EmptyDataset { .. })
            ));
        }
        assert!(!dir.path().join("datasets/vide.json").exists());
    }

    #[test]
    fn build_refuse_nom_invalide() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        for nom in ["", "Majuscule", "a/b", "../x", "a-b", &"x".repeat(65)] {
            let r = demande(nom, T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
            assert!(
                matches!(
                    build_dataset(dir.path(), &r, FUTUR),
                    Err(StoreError::InvalidDatasetName(_))
                ),
                "« {nom} »"
            );
        }
    }

    #[test]
    fn build_verrou_pris_refuse() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        let _verrou = PartitionLock::acquire(dir.path(), &partition()).unwrap();
        let r = demande("x", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        assert!(matches!(
            build_dataset(dir.path(), &r, FUTUR),
            Err(StoreError::LockBusy { .. })
        ));
    }

    #[test]
    fn remplissage_ffill_et_zero_volume_marked() {
        let dir = tempfile::tempdir().unwrap();
        // Bougies à t0, t0 + 1 min et t0 + 4 min : deux minutes manquantes entre les deux dernières.
        let t0 = T_2024_01 + 10 * MINUTE;
        let reelles: Vec<Candle> = [0_i64, 1, 4]
            .iter()
            .enumerate()
            .map(|(k, minute)| Candle {
                open_time_ms: t0 + minute * MINUTE,
                open: 100.0 + k as f64,
                high: 102.0 + k as f64,
                low: 99.0 + k as f64,
                close: 101.0 + k as f64,
                volume: 2.0,
                trades: Some(5),
            })
            .collect();
        ecrire(dir.path(), &Columns::from_candles(&reelles), FUTUR);
        // Fenêtre plus large que les données : rien ne doit être synthétisé avant ni après.
        let (debut, fin) = (T_2024_01, T_2024_01 + 60 * MINUTE);
        for (politique, nom) in [
            (GapsPolicy::Skip, "skip"),
            (GapsPolicy::Ffill, "ffill"),
            (GapsPolicy::ZeroVolumeMarked, "zvm"),
        ] {
            build_dataset(dir.path(), &demande(nom, debut, fin, politique), FUTUR).unwrap();
            let charge =
                load_dataset(dir.path(), &dir.path().join(format!("datasets/{nom}.json"))).unwrap();
            let c = &charge.columns;
            if politique == GapsPolicy::Skip {
                assert_eq!(c.len(), 3);
                assert_eq!(c.synthetic(), &[false, false, false]);
                continue;
            }
            assert_eq!(c.len(), 5);
            assert_eq!(c.synthetic(), &[false, false, true, true, false]);
            assert_eq!(c.synthetic_count(), 2);
            for (i, minute) in [0_i64, 1, 2, 3, 4].iter().enumerate() {
                assert_eq!(c.open_time_ms()[i], t0 + minute * MINUTE);
            }
            for i in [2, 3] {
                for valeur in [c.open()[i], c.high()[i], c.low()[i], c.close()[i]] {
                    assert_eq!(valeur.to_bits(), 102.0_f64.to_bits(), "close de t0 + 1 min");
                }
                assert_eq!(c.volume()[i].to_bits(), 0.0_f64.to_bits());
                assert_eq!(c.trades().unwrap()[i], None);
            }
            // Les réelles sont intactes.
            assert_eq!(c.get(4), Some(reelles[2]));
            // Une bougie synthétique est valide pour le moteur.
            assert!(c.get(2).unwrap().validate().is_ok());
        }
    }

    #[test]
    fn remplissage_refuse_un_ecart_desaligne() {
        let mut c = Columns::from_candles(&bougies(T_2024_01, 2).to_candles());
        let mut parts = c.clone().into_parts();
        parts.open_time_ms[1] += 1;
        c = Columns::from_parts(parts).unwrap();
        assert!(matches!(
            fill_gaps(c.clone(), Timeframe::M1, GapsPolicy::Ffill),
            Err(StoreError::Model(_))
        ));
        // Skip ne contrôle pas l'alignement.
        assert!(fill_gaps(c, Timeframe::M1, GapsPolicy::Skip).is_ok());
    }

    #[test]
    fn remplissage_sans_colonne_trades() {
        let mut parts = bougies(T_2024_01, 3).into_parts();
        parts.open_time_ms[2] += 2 * MINUTE;
        parts.trades = None;
        let c = Columns::from_parts(parts).unwrap();
        let rempli = fill_gaps(c, Timeframe::M1, GapsPolicy::Ffill).unwrap();
        assert_eq!(rempli.len(), 5);
        assert!(rempli.trades().is_none());
    }
    /// Deux bougies 1m valides aux ouvertures `a` et `b` (prix inventés).
    fn deux_bougies(a: i64, b: i64) -> Columns {
        let bougie = |open_time_ms| Candle {
            open_time_ms,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 1.0,
            trades: Some(1),
        };
        Columns::from_candles(&[bougie(a), bougie(b)])
    }

    #[test]
    fn remplissage_horodatages_extremes_sans_panique() {
        for politique in [
            GapsPolicy::Skip,
            GapsPolicy::Ffill,
            GapsPolicy::ZeroVolumeMarked,
        ] {
            // Écart qui déborderait un `i64` : erreur, jamais de panique par débordement.
            let erreur = fill_gaps(
                deux_bougies(i64::MIN + 8, i64::MAX - 7),
                Timeframe::M1,
                politique,
            )
            .unwrap_err();
            assert!(
                matches!(erreur, StoreError::TimestampOutOfRange { .. }),
                "{politique:?} : {erreur}"
            );
            // Négatif ou au-delà de l'an 9999.
            assert!(matches!(
                fill_gaps(deux_bougies(-MINUTE, 0), Timeframe::M1, politique),
                Err(StoreError::TimestampOutOfRange { .. })
            ));
            assert!(matches!(
                fill_gaps(
                    deux_bougies(MAX_TS_MS - 2 * MINUTE, MAX_TS_MS),
                    Timeframe::M1,
                    politique
                ),
                Err(StoreError::TimestampOutOfRange { .. })
            ));
        }
    }

    #[test]
    fn remplissage_refuse_un_ordre_non_strict() {
        for politique in [GapsPolicy::Skip, GapsPolicy::Ffill] {
            for (a, b) in [(MINUTE, MINUTE), (2 * MINUTE, MINUTE)] {
                assert!(matches!(
                    fill_gaps(deux_bougies(a, b), Timeframe::M1, politique),
                    Err(StoreError::NotStrictlyIncreasing { .. })
                ));
            }
        }
    }

    #[test]
    fn remplissage_borne_le_nombre_de_bougies_synthetiques() {
        // 6e13 ms de 1m : un milliard de bougies à créer, refusé avant toute allocation.
        let erreur = fill_gaps(
            deux_bougies(0, 60_000_000_000_000),
            Timeframe::M1,
            GapsPolicy::Ffill,
        )
        .unwrap_err();
        let StoreError::TooManySyntheticBars { count, max } = erreur else {
            panic!("TooManySyntheticBars attendu, reçu {erreur}");
        };
        assert_eq!(count, 999_999_999);
        assert_eq!(max, MAX_SYNTHETIC_BARS);
        // Skip n'en crée aucune.
        assert!(
            fill_gaps(
                deux_bougies(0, 60_000_000_000_000),
                Timeframe::M1,
                GapsPolicy::Skip
            )
            .is_ok()
        );
        // À la borne exacte : accepté ; une bougie de plus : refusé (borne réduite pour le test).
        let a_la_borne = fill_gaps_bounded(
            deux_bougies(0, 101 * MINUTE),
            Timeframe::M1,
            GapsPolicy::Ffill,
            100,
        )
        .unwrap();
        assert_eq!(a_la_borne.len(), 102);
        assert_eq!(a_la_borne.synthetic_count(), 100);
        assert!(matches!(
            fill_gaps_bounded(
                deux_bougies(0, 102 * MINUTE),
                Timeframe::M1,
                GapsPolicy::Ffill,
                100
            ),
            Err(StoreError::TooManySyntheticBars {
                count: 101,
                max: 100
            })
        ));
    }

    #[test]
    fn remplissage_sans_trou_rend_les_colonnes_telles_quelles() {
        let c = bougies(T_2024_01, 5);
        let rempli = fill_gaps(c.clone(), Timeframe::M1, GapsPolicy::Ffill).unwrap();
        assert_eq!(rempli, c);
    }

    #[test]
    fn build_verrou_de_nom_pris_refuse() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        let _verrou = DatasetLock::acquire(dir.path(), "x").unwrap();
        let r = demande("x", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        assert!(matches!(
            build_dataset(dir.path(), &r, FUTUR),
            Err(StoreError::DatasetBusy { .. })
        ));
        assert!(!dir.path().join("datasets/x.json").exists());
        // Un autre nom n'est pas gêné.
        let autre = demande("y", T_2024_01, T_2024_01 + 10 * MINUTE, GapsPolicy::Skip);
        assert!(build_dataset(dir.path(), &autre, FUTUR).is_ok());
    }

    /// Même partition de stockage mais autre symbole.
    fn autre_partition() -> Partition {
        Partition::new(
            ExchangeId::Kraken,
            "SYM-USD".parse().unwrap(),
            Timeframe::M1,
        )
    }

    #[test]
    fn deux_builds_de_meme_nom_sur_deux_partitions_ne_s_ecrasent_pas() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), &bougies(T_2024_01, 20), FUTUR);
        write_partition(
            dir.path(),
            &autre_partition(),
            &bougies(T_2024_01, 30),
            "synthetic",
            FUTUR,
        )
        .unwrap();
        let fenetre = Window::new(T_2024_01, T_2024_01 + 10 * MINUTE).unwrap();
        for essai in 0..30 {
            let nom = format!("course{essai}");
            let barriere = std::sync::Barrier::new(2);
            let resultats: Vec<Result<BuildOutcome, StoreError>> = std::thread::scope(|scope| {
                [partition(), autre_partition()]
                    .into_iter()
                    .map(|partition| {
                        let (racine, nom, barriere) = (dir.path(), nom.as_str(), &barriere);
                        scope.spawn(move || {
                            let r = BuildRequest {
                                name: nom,
                                partition,
                                window: fenetre,
                                gaps_policy: GapsPolicy::Skip,
                            };
                            barriere.wait();
                            build_dataset(racine, &r, FUTUR)
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect()
            });
            let crees = resultats
                .iter()
                .filter(|r| matches!(r, Ok(sortie) if sortie.status == BuildStatus::Created))
                .count();
            assert_eq!(
                crees, 1,
                "essai {essai} : un seul build peut créer le manifeste"
            );
            // Le manifeste sur disque est celui du build qui a annoncé « créé ».
            let gagnant = resultats.iter().find_map(|r| r.as_ref().ok()).unwrap();
            let sur_disque =
                read_dataset_manifest(&dir.path().join(format!("datasets/{nom}.json"))).unwrap();
            assert_eq!(sur_disque, gagnant.manifest, "essai {essai}");
            // L'autre build a été refusé proprement (verrou ou dataset existant).
            for r in &resultats {
                if let Err(e) = r {
                    assert!(
                        matches!(
                            e,
                            StoreError::DatasetBusy { .. }
                                | StoreError::LockBusy { .. }
                                | StoreError::DatasetExists { .. }
                        ),
                        "essai {essai} : {e}"
                    );
                }
            }
        }
    }

    #[test]
    fn reingestion_apres_arret_entre_parquet_et_manifeste_repare_le_build() {
        let dir = tempfile::tempdir().unwrap();
        let manifeste_fichier = dir
            .path()
            .join("data/candles/kraken/SYN-USD/1m/2024-02._manifest.json");
        // Février 2024 non clos : un ancien contenu (H1), puis un contenu plus long (H2).
        let ancien = bougies(T_2024_02, 4);
        ecrire(dir.path(), &ancien, EN_FEVRIER);
        let manifeste_h1 = fs::read(&manifeste_fichier).unwrap();
        ecrire(dir.path(), &bougies(T_2024_02, 9), EN_FEVRIER + 1000);
        // Arrêt simulé entre le `rename` du Parquet et l'écriture du manifeste : Parquet H2,
        // manifeste H1.
        fs::write(&manifeste_fichier, &manifeste_h1).unwrap();
        let r = demande("x", T_2024_02, T_2024_02 + 4 * MINUTE, GapsPolicy::Skip);
        assert!(matches!(
            build_dataset(dir.path(), &r, EN_FEVRIER + 2000),
            Err(StoreError::FileHashMismatch { .. })
        ));
        // Réingérer l'ancien contenu (celui du manifeste) répare la paire au lieu de répondre
        // « inchangé ».
        let sortie = write_partition(
            dir.path(),
            &partition(),
            &ancien,
            "synthetic",
            EN_FEVRIER + 3000,
        )
        .unwrap();
        assert_eq!(sortie[0].status, crate::store::WriteStatus::Rewritten);
        let construit = build_dataset(dir.path(), &r, EN_FEVRIER + 4000).unwrap();
        assert_eq!(construit.status, BuildStatus::Created);
        assert_eq!(construit.snapshots, 1);
        let charge = load_dataset(dir.path(), &dir.path().join("datasets/x.json")).unwrap();
        assert_eq!(charge.columns.len(), 4);
    }
}
