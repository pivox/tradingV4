//! Store de bougies : fichiers Parquet immuables par mois ou par année, manifestes JSON et datasets
//! figés identifiés par `dataset_hash` (docs/03-architecture.md §2 ligne `store`, §5).
//!
//! # Règle de dépendance
//!
//! `store` ne dépend que de `model` (règle vérifiée par `tests/architecture.rs`). Il ne lit aucune
//! source brute : `tv4 ingest csv` fait le lien avec `marketdata` dans la CLI. Les crates `arrow` et
//! `parquet` sont confinées à `parquet.rs`, qui rend des `Vec` possédés ([`Columns`]) : aucun type
//! arrow n'apparaît dans l'API publique du module ni dans [`StoreError`].
//!
//! # Racine de travail
//!
//! Toutes les API prennent `root: &Path`, dossier qui contient `data/`, `datasets/` et `state/`.
//! Les chemins écrits dans les manifestes sont relatifs à cette racine, avec `/` pour séparateur.
//!
//! ```text
//! data/candles/<exchange>/<symbole>/<tf>/<AAAA-MM>.parquet         (TF < 1h : un fichier par mois)
//! data/candles/<exchange>/<symbole>/<tf>/<AAAA>.parquet            (TF >= 1h : un fichier par année)
//! data/candles/<exchange>/<symbole>/<tf>/<période>._manifest.json  (un manifeste par fichier)
//! state/locks/<exchange>/<symbole>/<tf>.lock                       (verrou de partition)
//! state/locks/datasets/<nom>.lock                                  (verrou de build par nom de dataset)
//! datasets/<nom>.json                                              (dataset figé)
//! datasets/snapshots/<dataset_hash>/data/candles/...               (copie des fichiers non clos)
//! ```
//!
//! # « Immuable » ne vaut que pour les périodes closes
//!
//! Un fichier est clos quand la fin de sa période est passée à l'instant d'écriture (`now_ms` passé
//! par l'appelant : le store ne lit jamais l'horloge). Un fichier clos n'est jamais réécrit avec un
//! contenu différent ; un fichier non clos peut l'être, c'est pourquoi `build_dataset` en copie
//! une version figée sous `datasets/snapshots/`.
//!
//! # Hashes
//!
//! `content_hash` et `dataset_hash` suivent `docs/dataset-hash.md` (dsh-v1), implémenté dans [`dsh`] :
//! ils portent sur le contenu logique des bougies, jamais sur les octets Parquet.
//!
//! # Écarts à docs/03 (S4, S5)
//!
//! Les écarts de S4 (lecture vérifiée complète, `period` au lieu de `month`, `gaps_policy` à trois
//! valeurs, champs ajoutés aux manifestes) sont décrits dans docs/03 §5.3. S5 ajoute le bloc optionnel
//! `continuity` des manifestes, [`WriteOptions`] (fin de clôture bornée par la couverture du dump) et
//! [`quality_report`] (rapport de qualité trimestriel, docs/04 §2.5). S7 ajoute [`merge_period_with`]
//! (fusion d'une période avec le fichier existant, sous le verrou de partition attendu par
//! [`PartitionLock::acquire_wait`]) pour l'ingestion OKX par lots et sa reprise. S9 ajoute les datasets
//! multi-séries ([`build_dataset_multi`], [`load_dataset_multi`]) : une série par symbole, hash `dsh-v1`
//! §5.2 sur tous les segments ; [`load_dataset`] garde son refus du multi-séries.

#![forbid(unsafe_code)]

mod columns;
mod dataset;
pub mod dsh;
mod durable;
mod layout;
mod lock;
mod manifest;
mod merge;
mod parquet;
mod partition;
mod quality;
pub mod time;

use std::io;

use thiserror::Error;

use crate::model::{ModelError, Timeframe, Ts};

pub use self::columns::{Columns, ColumnsParts};
pub use self::dataset::{
    BuildOutcome, BuildRequest, BuildStatus, LoadedDataset, MultiBuildRequest, MultiDataset,
    build_dataset, build_dataset_multi, fill_gaps, load_dataset, load_dataset_multi,
    read_verified_with_hash,
};
pub use self::layout::{GapsPolicy, Partition, Period, Symbol, Window, resolve_rel};
pub use self::lock::{DatasetLock, PartitionLock};
pub use self::manifest::{
    ContinuityCounts, DatasetFile, DatasetManifest, FileManifest, read_dataset_manifest,
    read_file_manifest,
};
pub use self::merge::merge_period_with;
pub use self::parquet::{
    FileInfo, Projection, ReadOptions, RowGroupInfo, inspect_parquet, read_parquet,
    select_row_groups,
};
pub use self::partition::{
    ContinuityInput, WriteOptions, WriteOutcome, WriteStatus, write_partition, write_partition_with,
};
pub use self::quality::{QUALITY_MISSING_THRESHOLD, QualityReport, QualityRow, quality_report};
pub use self::time::UtcMs;

/// Toutes les erreurs du module `store`. Aucun type arrow ou parquet : leurs erreurs sont converties
/// en texte dans `parquet.rs`. Les hashes des messages sont rendus préfixés (`blake3:...`).
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("accès à `{path}` impossible : {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },

    #[error("fichier Parquet `{path}` : {message}")]
    Parquet { path: String, message: String },

    #[error(
        "fichier Parquet `{path}` : schéma inattendu ({detail}) ; 7 colonnes attendues (open_time_ms Int64, open/high/low/close/volume Float64 non nullables, trades UInt32 nullable)"
    )]
    BadSchema { path: String, detail: String },

    #[error("manifeste `{path}` : JSON invalide : {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    Model(#[from] ModelError),

    #[error(
        "symbole « {0} » invalide : forme canonique BASE-QUOTE en majuscules attendue ([A-Z0-9]+-[A-Z0-9]+, par exemple BTC-USD)"
    )]
    InvalidSymbol(String),

    #[error("nom de dataset « {0} » invalide : [a-z0-9_], 1 à 64 caractères")]
    InvalidDatasetName(String),

    #[error("source « {0} » invalide : [a-z0-9-], 1 à 32 caractères")]
    InvalidSource(String),

    #[error("gaps_policy « {0} » inconnue (attendues : skip, ffill, zero_volume_marked)")]
    UnknownGapsPolicy(String),

    #[error(
        "date « {text} » invalide : format attendu AAAA-MM-JJTHH:MM:SSZ (UTC, sans fraction ni décalage)"
    )]
    InvalidDate { text: String },

    #[error("horodatage {ts_ms} hors de [0, 253402300800000) (1970 à 9999)")]
    TimestampOutOfRange { ts_ms: Ts },

    #[error("période « {label} » invalide pour un fichier {tf}")]
    InvalidPeriod { tf: Timeframe, label: String },

    #[error("fenêtre vide : début {start_ms} ≥ fin {end_ms}")]
    EmptyWindow { start_ms: Ts, end_ms: Ts },

    #[error("borne de fenêtre {ts_ms} non alignée sur une bougie {tf}")]
    WindowNotAligned { tf: Timeframe, ts_ms: Ts },

    #[error(
        "chemin `{path}` refusé : relatif à la racine, sous data/candles/ ou datasets/snapshots/, sans `..`"
    )]
    UnsafePath { path: String },

    #[error(
        "dsh-v1 : le champ d'en-tête `{field}` vaut « {value} », hors de son jeu de caractères"
    )]
    HeaderField { field: &'static str, value: String },

    #[error(
        "dsh-v1 : la bougie {open_time_ms} est hors de la fenêtre du fichier [{start_ms}, {end_ms})"
    )]
    RowOutsideWindow {
        open_time_ms: Ts,
        start_ms: Ts,
        end_ms: Ts,
    },

    #[error("dsh-v1 : deux bougies différentes ont le même open_time {open_time_ms}")]
    DivergentDuplicate { open_time_ms: Ts },

    #[error("dsh-v1 : la série {symbol} {tf} apparaît deux fois")]
    DuplicateSegment { symbol: String, tf: Timeframe },

    #[error(
        "la colonne trades n'a pas été lue (projection à 6 colonnes) : hash et écriture impossibles"
    )]
    MissingTrades,

    #[error("colonnes de longueurs différentes : {detail}")]
    ColumnLengths { detail: String },

    #[error("aucune bougie à écrire")]
    EmptyInput,

    #[error(
        "bougie {index} : open_time {open_time_ms} non strictement croissant (précédent : {previous_ms}) ; tri et dédoublonnage relèvent de l'ingestion (marketdata::continuity)"
    )]
    NotStrictlyIncreasing {
        index: usize,
        previous_ms: Ts,
        open_time_ms: Ts,
    },

    #[error("bougie {open_time_ms} synthétique : le store n'écrit que des bougies réelles")]
    SyntheticNotStorable { open_time_ms: Ts },

    #[error(
        "la partition {partition} est déjà verrouillée (`{path}`) : une ingestion ou un build tourne déjà sur cette partition"
    )]
    LockBusy { partition: String, path: String },

    #[error(
        "le dataset « {name} » est déjà en cours de construction (`{path}`) : un autre build de ce nom tourne"
    )]
    DatasetBusy { name: String, path: String },

    #[error(
        "`{path}` est clos (période terminée) : réécriture refusée, son content_hash serait {actual} au lieu de {expected}"
    )]
    ClosedFileModified {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("manifeste `{path}` incohérent : `{field}` vaut {manifest}, attendu {actual}")]
    ManifestMismatch {
        path: String,
        field: &'static str,
        manifest: String,
        actual: String,
    },

    #[error(
        "`{path}` existe sans son _manifest.json (écriture interrompue ?) : relancer l'ingestion"
    )]
    ManifestMissing { path: String },

    #[error("`{path}` est listé ou a un manifeste, mais le fichier Parquet est absent")]
    DataFileMissing { path: String },

    #[error(
        "le fichier `{path}` n'a plus le content_hash attendu : {expected} dans le manifeste, {actual} recalculé"
    )]
    FileHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("`{path}` : hash_format « {found} » non pris en charge (attendu dsh-v1)")]
    UnsupportedHashFormat { path: String, found: String },

    #[error(
        "`{path}` : hash « {value} » mal formé (attendu blake3: suivi de 64 caractères hexadécimaux minuscules)"
    )]
    MalformedHash { path: String, value: String },

    #[error(
        "remplissage des trous : {count} bougies synthétiques à créer, au-delà du maximum de {max} ; vérifier la fenêtre et les données"
    )]
    TooManySyntheticBars { count: u64, max: u64 },

    #[error("aucune bougie dans la fenêtre [{start_ms}, {end_ms})")]
    EmptyDataset { start_ms: Ts, end_ms: Ts },

    #[error(
        "`{path}` existe déjà avec dataset_hash {existing} ; le nouveau build donnerait {new} : un dataset figé ne se remplace pas, choisir un autre nom"
    )]
    DatasetExists {
        path: String,
        existing: String,
        new: String,
    },

    #[error("snapshot `{path}` déjà présent avec content_hash {actual} au lieu de {expected}")]
    SnapshotMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error(
        "dataset `{path}` : dataset_hash recalculé {actual}, {expected} dans le manifeste ; run refusé"
    )]
    DatasetHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error(
        "dataset à {symbols} symbole(s) et {tfs} timeframe(s) : S4 ne charge que des datasets mono-série"
    )]
    MultiSeriesUnsupported { symbols: usize, tfs: usize },

    #[error("dataset à {tfs} timeframe(s) : un dataset multi-séries n'a qu'un seul timeframe")]
    MultiTimeframeUnsupported { tfs: usize },

    #[error("aucun symbole : un dataset a au moins une série")]
    NoSymbols,

    #[error("symbole {symbol} demandé deux fois : un symbole n'est une série qu'une fois")]
    DuplicateSymbol { symbol: String },

    #[error(
        "symbole {symbol} sans aucune bougie dans la fenêtre [{start_ms}, {end_ms}) : rien dans le stockage pour cette série"
    )]
    SymbolWithoutData {
        symbol: String,
        start_ms: Ts,
        end_ms: Ts,
    },
}
