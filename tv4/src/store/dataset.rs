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

use std::fs;
use std::path::Path;

use crate::model::{Candle, Timeframe, Ts};

use super::StoreError;
use super::columns::Columns;
use super::dsh::{self, HASH_FORMAT};
use super::durable::{io_error, write_bytes_durable};
use super::layout::{GapsPolicy, Partition, Period, Window, resolve_rel};
use super::lock::{DatasetLock, PartitionLock};
use super::manifest::{
    DatasetFile, DatasetManifest, FileManifest, read_dataset_manifest, read_file_manifest,
    write_dataset_manifest,
};
use super::parquet::{ReadOptions, read_parquet};
use super::partition::check_identity;
use super::time::{MAX_TS_MS, format_rfc3339_utc, parse_rfc3339_utc};

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
    Ok(columns)
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
    check_name(request.name)?;
    let partition = &request.partition;
    let window = request.window;
    window.check_aligned(partition.tf)?;
    let window_text = [
        format_rfc3339_utc(window.start_ms())?,
        format_rfc3339_utc(window.end_ms())?,
    ];
    let built_at = format_rfc3339_utc(now_ms)?;
    let periods = Period::covering(partition.tf, window)?;
    let _verrou_nom = DatasetLock::acquire(root, request.name)?;
    let _verrou = PartitionLock::acquire(root, partition)?;

    // Étapes 3 et 4 : fichiers présents, vérifiés, avec au moins une ligne dans la fenêtre.
    let mut kept: Vec<Kept> = Vec::new();
    let mut all: Option<Columns> = None;
    for period in periods {
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

    // Étape 5 : dataset_hash.
    let all = all.unwrap_or_else(|| Columns::with_capacity(0));
    let symbol = partition.symbol.as_str();
    let (hex, rows) = dsh::dataset_hash(
        partition.exchange.as_str(),
        window,
        request.gaps_policy,
        &[(symbol, partition.tf, &all)],
    )?;
    if rows == 0 {
        return Err(StoreError::EmptyDataset {
            start_ms: window.start_ms(),
            end_ms: window.end_ms(),
        });
    }
    let dataset_hash = dsh::prefixed(&hex);

    // Étape 6 : immutabilité du manifeste de dataset.
    let manifest_rel = format!("datasets/{}.json", request.name);
    let manifest_path = root.join("datasets").join(format!("{}.json", request.name));
    if manifest_path.is_file() {
        let existing = read_dataset_manifest(&manifest_path)?;
        if existing.dataset_hash == dataset_hash && existing.hash_format == HASH_FORMAT {
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

    // Étape 7 : snapshots des fichiers non clos.
    let mut files = Vec::with_capacity(kept.len());
    for item in &kept {
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
            tf: partition.tf,
            period: item.period.label(),
            content_hash: item.manifest.content_hash.clone(),
            row_count: item.manifest.row_count,
            snapshot,
        });
    }
    let snapshots = files.iter().filter(|f| f.snapshot).count();

    // Étape 8 : manifeste.
    let manifest = DatasetManifest {
        name: request.name.to_string(),
        exchange: partition.exchange,
        symbols: vec![partition.symbol.clone()],
        tfs: vec![partition.tf],
        window: window_text,
        gaps_policy: request.gaps_policy,
        files,
        row_count: rows,
        dataset_hash,
        hash_format: HASH_FORMAT.to_string(),
        built_at,
        tv4_version: env!("CARGO_PKG_VERSION").to_string(),
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
/// `load_dataset` n'applique aucun refus de politique : il sait charger `zero_volume_marked`. Le refus
/// de cette politique est dans `tv4 backtest`.
pub fn load_dataset(root: &Path, manifest_path: &Path) -> Result<LoadedDataset, StoreError> {
    let shown = manifest_path.display().to_string();
    let manifest = read_dataset_manifest(manifest_path)?;
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

    let mut all: Option<Columns> = None;
    let mut previous: Option<Period> = None;
    for file in &manifest.files {
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
        let columns = read_verified(&path, &file.path, &partition, period, &file.content_hash)?;
        check_field(
            &file.path,
            "row_count",
            file.row_count,
            columns.len() as u64,
        )?;
        append(&mut all, columns)?;
    }
    let mut all = all.unwrap_or_else(|| Columns::with_capacity(0));
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
