//! Lecture et écriture Parquet : le seul fichier du dépôt qui nomme `arrow` ou `parquet`
//! (docs/03 §3.1, §5.1 et §10 ligne S4 ; vérifié par `tests/architecture.rs`).
//!
//! L'API arrow est la plus verbeuse du jalon : elle reste ici, et ne sort que des [`Columns`]
//! (`Vec` possédés) et des types simples ([`ReadOptions`], [`FileInfo`]). Les erreurs des deux crates
//! sont converties en texte dans [`StoreError::Parquet`].
//!
//! # Format d'un fichier
//!
//! 7 colonnes dans cet ordre : `open_time_ms` Int64 non nullable, `open`, `high`, `low`, `close`,
//! `volume` Float64 non nullables, `trades` UInt32 nullable. Compression zstd niveau 3, un
//! `RecordBatch` et un row group par mois (au plus 44 640 lignes), statistiques min/max par row
//! group, `created_by = "tv4"`. Le dictionnaire est désactivé : mesuré sur un mois synthétique de
//! 44 640 lignes, il coûte 1,28 fois la taille (2,24 Mo contre 1,75 Mo) et deux fois le temps
//! d'écriture, les prix 1m variant trop pour qu'il paie.
//!
//! # Lecture
//!
//! Le schéma est contrôlé avant tout. La sélection des row groups se fait sur leurs statistiques
//! (`with_row_groups`), puis les lignes sont filtrées exactement en mémoire. La projection renumérote
//! les colonnes : on lit par nom, jamais par index.

use std::fs::File;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray, Float64Array, Int64Array, RecordBatch, UInt32Array};
use arrow::datatypes::{DataType, Field, Float64Type, Int64Type, Schema, SchemaRef, UInt32Type};
use parquet::arrow::ArrowWriter;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::ParquetMetaData;
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::file::statistics::Statistics;

use crate::model::Ts;

use super::StoreError;
use super::columns::{Columns, ColumnsParts};
use super::layout::Window;

/// Noms des 7 colonnes, dans l'ordre du fichier.
const COLUMN_NAMES: [&str; 7] = [
    "open_time_ms",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "trades",
];

/// Nombre maximal de lignes d'un row group : un mois de 1m de 31 jours (docs/03 §5.1).
pub const MAX_ROWS_PER_GROUP: usize = 44_640;
/// Niveau de compression zstd.
pub const ZSTD_LEVEL: i32 = 3;
/// Valeur de `created_by` écrite dans le footer.
pub const CREATED_BY: &str = "tv4";

fn parquet_error(path: &Path) -> impl Fn(parquet::errors::ParquetError) -> StoreError + '_ {
    move |e| StoreError::Parquet {
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

fn arrow_error(path: &Path) -> impl Fn(arrow::error::ArrowError) -> StoreError + '_ {
    move |e| StoreError::Parquet {
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

fn bad_schema(path: &Path, detail: impl Into<String>) -> StoreError {
    StoreError::BadSchema {
        path: path.display().to_string(),
        detail: detail.into(),
    }
}

/// Schéma arrow d'un fichier de bougies.
fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new(COLUMN_NAMES[0], DataType::Int64, false),
        Field::new(COLUMN_NAMES[1], DataType::Float64, false),
        Field::new(COLUMN_NAMES[2], DataType::Float64, false),
        Field::new(COLUMN_NAMES[3], DataType::Float64, false),
        Field::new(COLUMN_NAMES[4], DataType::Float64, false),
        Field::new(COLUMN_NAMES[5], DataType::Float64, false),
        Field::new(COLUMN_NAMES[6], DataType::UInt32, true),
    ]))
}

fn properties(path: &Path) -> Result<WriterProperties, StoreError> {
    let level = ZstdLevel::try_new(ZSTD_LEVEL).map_err(parquet_error(path))?;
    Ok(WriterProperties::builder()
        .set_compression(Compression::ZSTD(level))
        .set_max_row_group_row_count(Some(MAX_ROWS_PER_GROUP))
        .set_created_by(CREATED_BY.to_string())
        .set_statistics_enabled(EnabledStatistics::Chunk)
        .set_dictionary_enabled(false)
        .build())
}

/// Un `RecordBatch` des lignes `range` (copie des tranches dans des tableaux arrow).
fn batch(path: &Path, columns: &Columns, range: Range<usize>) -> Result<RecordBatch, StoreError> {
    let trades = columns.trades().ok_or(StoreError::MissingTrades)?;
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(
            columns.open_time_ms()[range.clone()].to_vec(),
        )),
        Arc::new(Float64Array::from(columns.open()[range.clone()].to_vec())),
        Arc::new(Float64Array::from(columns.high()[range.clone()].to_vec())),
        Arc::new(Float64Array::from(columns.low()[range.clone()].to_vec())),
        Arc::new(Float64Array::from(columns.close()[range.clone()].to_vec())),
        Arc::new(Float64Array::from(columns.volume()[range.clone()].to_vec())),
        Arc::new(UInt32Array::from(trades[range].to_vec())),
    ];
    RecordBatch::try_new(schema(), arrays).map_err(arrow_error(path))
}

/// Écrit `columns` dans `file`, un `RecordBatch` suivi de `flush()` par plage de `groups` (un row
/// group chacune, 1 à 44 640 lignes). `path` ne sert qu'aux messages. Pas de `sync_all` : c'est
/// `durable.rs` qui s'en charge.
pub(crate) fn write_parquet(
    file: &mut File,
    path: &Path,
    columns: &Columns,
    groups: &[Range<usize>],
) -> Result<(), StoreError> {
    if columns.trades().is_none() {
        return Err(StoreError::MissingTrades);
    }
    // Contrôle des groupes avant le premier octet écrit : `batch` ne vérifie pas les bornes.
    for group in groups {
        let rows = group.end.saturating_sub(group.start);
        if group.start >= group.end || group.end > columns.len() || rows > MAX_ROWS_PER_GROUP {
            return Err(StoreError::Parquet {
                path: path.display().to_string(),
                message: format!(
                    "groupe de lignes {}..{} invalide (1 à {MAX_ROWS_PER_GROUP} lignes, dans les {} lignes fournies)",
                    group.start,
                    group.end,
                    columns.len()
                ),
            });
        }
    }
    let mut writer = ArrowWriter::try_new(file, schema(), Some(properties(path)?))
        .map_err(parquet_error(path))?;
    for group in groups {
        writer
            .write(&batch(path, columns, group.clone())?)
            .map_err(parquet_error(path))?;
        writer.flush().map_err(parquet_error(path))?;
    }
    writer.close().map_err(parquet_error(path))?;
    Ok(())
}

/// Colonnes lues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    /// Les 7 colonnes (vérification de hash, écriture, snapshot).
    All,
    /// Les 6 colonnes sans `trades` (lecture pour calcul seulement, bench).
    NoTrades,
}

/// Options de [`read_parquet`].
#[derive(Clone, Copy, Debug)]
pub struct ReadOptions {
    pub projection: Projection,
    /// Filtre de row groups par statistiques, puis filtre exact des lignes en mémoire.
    pub window: Option<Window>,
    /// Taille des `RecordBatch` de lecture ; sans effet sur les valeurs (testé).
    pub batch_size: usize,
}

impl Default for ReadOptions {
    fn default() -> ReadOptions {
        ReadOptions {
            projection: Projection::All,
            window: None,
            batch_size: 8192,
        }
    }
}

/// Statistiques d'un row group sur `open_time_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowGroupInfo {
    pub rows: u64,
    pub min_open_time_ms: Option<Ts>,
    pub max_open_time_ms: Option<Ts>,
}

/// Contenu du footer d'un fichier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub created_by: Option<String>,
    pub rows: u64,
    pub row_groups: Vec<RowGroupInfo>,
}

fn row_group_infos(metadata: &ParquetMetaData) -> Vec<RowGroupInfo> {
    metadata
        .row_groups()
        .iter()
        .map(|rg| {
            let (min, max) = match rg.column(0).statistics() {
                Some(Statistics::Int64(s)) => (s.min_opt().copied(), s.max_opt().copied()),
                _ => (None, None),
            };
            RowGroupInfo {
                rows: u64::try_from(rg.num_rows()).unwrap_or(0),
                min_open_time_ms: min,
                max_open_time_ms: max,
            }
        })
        .collect()
}

/// Indices des row groups dont `[min, max]` recoupe `window` ; un row group sans statistiques est
/// gardé (le filtre exact des lignes tranche ensuite). Sans fenêtre, tous.
pub fn select_row_groups(infos: &[RowGroupInfo], window: Option<Window>) -> Vec<usize> {
    infos
        .iter()
        .enumerate()
        .filter(
            |(_, info)| match (window, info.min_open_time_ms, info.max_open_time_ms) {
                (Some(w), Some(min), Some(max)) => w.intersects(min, max),
                _ => true,
            },
        )
        .map(|(index, _)| index)
        .collect()
}

fn open_builder(path: &Path) -> Result<ParquetRecordBatchReaderBuilder<File>, StoreError> {
    let file = File::open(path).map_err(|source| StoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(parquet_error(path))?;
    if builder.schema().fields() != schema().fields() {
        return Err(bad_schema(path, format!("{:?}", builder.schema().fields())));
    }
    Ok(builder)
}

/// Footer seulement : `created_by`, nombre de lignes, statistiques par row group.
pub fn inspect_parquet(path: &Path) -> Result<FileInfo, StoreError> {
    let builder = open_builder(path)?;
    let metadata = builder.metadata();
    Ok(FileInfo {
        created_by: metadata.file_metadata().created_by().map(str::to_string),
        rows: u64::try_from(metadata.file_metadata().num_rows()).unwrap_or(0),
        row_groups: row_group_infos(metadata),
    })
}

/// Colonne `name` du lot, typée `T` et sans valeur nulle (sinon [`StoreError::BadSchema`]).
macro_rules! colonne {
    ($path:expr, $batch:expr, $name:expr, $type:ty) => {{
        let array = $batch
            .column_by_name($name)
            .and_then(|c| c.as_primitive_opt::<$type>())
            .ok_or_else(|| {
                bad_schema($path, format!("colonne `{}` absente ou mal typée", $name))
            })?;
        if array.null_count() != 0 {
            return Err(bad_schema(
                $path,
                format!("colonne `{}` : valeurs nulles interdites", $name),
            ));
        }
        array
    }};
}

/// Lit un fichier : contrôle du schéma, sélection des row groups, projection, filtre exact.
///
/// `ReadOptions::batch_size` nul est refusé. Avec `Projection::NoTrades`, la colonne `trades` du
/// résultat est absente (`Columns::trades() == None`).
pub fn read_parquet(path: &Path, options: &ReadOptions) -> Result<Columns, StoreError> {
    if options.batch_size == 0 {
        return Err(StoreError::Parquet {
            path: path.display().to_string(),
            message: "batch_size nul".to_string(),
        });
    }
    let builder = open_builder(path)?;
    let row_groups = select_row_groups(&row_group_infos(builder.metadata()), options.window);
    let capacity = row_groups
        .iter()
        .filter_map(|i| builder.metadata().row_groups().get(*i))
        .map(|rg| usize::try_from(rg.num_rows()).unwrap_or(0))
        .sum::<usize>();
    let all = options.projection == Projection::All;
    let width = if all { 7 } else { 6 };
    let mask = ProjectionMask::roots(builder.parquet_schema(), 0..width);
    let reader = builder
        .with_projection(mask)
        .with_row_groups(row_groups)
        .with_batch_size(options.batch_size)
        .build()
        .map_err(parquet_error(path))?;

    let mut parts = ColumnsParts {
        open_time_ms: Vec::with_capacity(capacity),
        open: Vec::with_capacity(capacity),
        high: Vec::with_capacity(capacity),
        low: Vec::with_capacity(capacity),
        close: Vec::with_capacity(capacity),
        volume: Vec::with_capacity(capacity),
        trades: all.then(|| Vec::with_capacity(capacity)),
        synthetic: Vec::new(),
    };
    for lot in reader {
        let lot = lot.map_err(arrow_error(path))?;
        let times = colonne!(path, lot, "open_time_ms", Int64Type);
        parts.open_time_ms.extend_from_slice(times.values());
        let open = colonne!(path, lot, "open", Float64Type);
        parts.open.extend_from_slice(open.values());
        let high = colonne!(path, lot, "high", Float64Type);
        parts.high.extend_from_slice(high.values());
        let low = colonne!(path, lot, "low", Float64Type);
        parts.low.extend_from_slice(low.values());
        let close = colonne!(path, lot, "close", Float64Type);
        parts.close.extend_from_slice(close.values());
        let volume = colonne!(path, lot, "volume", Float64Type);
        parts.volume.extend_from_slice(volume.values());
        if let Some(trades) = &mut parts.trades {
            let array = lot
                .column_by_name("trades")
                .and_then(|c| c.as_primitive_opt::<UInt32Type>())
                .ok_or_else(|| bad_schema(path, "colonne `trades` absente ou mal typée"))?;
            trades.extend(array.iter());
        }
    }
    let mut columns = Columns::from_parts(parts)?;
    if let Some(window) = options.window {
        columns.retain_window(window);
    }
    Ok(columns)
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // une plage de lignes = un row group
mod tests {
    use super::*;
    use crate::model::Candle;

    const T_2024_01: i64 = 1_704_067_200_000;
    const MS_HEURE: i64 = 3_600_000;

    /// Bougies horaires synthétiques (prix inventés) depuis `debut`, `n` lignes.
    fn horaires(debut: i64, n: usize) -> Vec<Candle> {
        (0..n)
            .map(|i| {
                let base = 100.0 + (i % 50) as f64 * 0.25;
                Candle {
                    open_time_ms: debut + i as i64 * MS_HEURE,
                    open: base,
                    high: base + 1.0,
                    low: base - 1.0,
                    close: base + 0.5,
                    volume: 1.0 + (i % 7) as f64,
                    trades: if i % 3 == 0 { None } else { Some(i as u32) },
                }
            })
            .collect()
    }

    /// Fichier annuel de trois mois consécutifs de 1h : janvier, février (bissextile), mars 2024.
    fn trois_mois() -> (Columns, Vec<Range<usize>>) {
        let janvier = 744;
        let fevrier = 696;
        let mars = 744;
        let candles = horaires(T_2024_01, janvier + fevrier + mars);
        (
            Columns::from_candles(&candles),
            vec![
                0..janvier,
                janvier..janvier + fevrier,
                janvier + fevrier..janvier + fevrier + mars,
            ],
        )
    }

    fn ecrire(
        dir: &Path,
        nom: &str,
        columns: &Columns,
        groups: &[Range<usize>],
    ) -> std::path::PathBuf {
        let path = dir.join(nom);
        let mut file = File::create(&path).unwrap();
        write_parquet(&mut file, &path, columns, groups).unwrap();
        path
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
    fn aller_retour_sept_colonnes_au_bit() {
        let mut candles = horaires(T_2024_01, 10);
        candles[1].volume = -0.0;
        candles[2].trades = Some(0);
        candles[3].trades = Some(17);
        candles[4].trades = None;
        let colonnes = Columns::from_candles(&candles);
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &[0..10]);
        let lu = read_parquet(&path, &ReadOptions::default()).unwrap();
        assert_eq!(lu, colonnes);
        assert_eq!(bits(&lu), bits(&colonnes));
        assert_eq!(lu.volume()[1].to_bits(), (-0.0_f64).to_bits());
        assert_eq!(lu.trades().unwrap()[2], Some(0));
        assert_eq!(lu.trades().unwrap()[3], Some(17));
        assert_eq!(lu.trades().unwrap()[4], None);
        assert!(lu.synthetic().iter().all(|s| !s));
    }

    #[test]
    fn created_by_vaut_tv4_et_statistiques_presentes() {
        let (colonnes, groupes) = trois_mois();
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &groupes);
        let info = inspect_parquet(&path).unwrap();
        assert_eq!(info.created_by.as_deref(), Some("tv4"));
        assert_eq!(info.rows, 2184);
        assert!(info.row_groups.iter().all(|g| g.min_open_time_ms.is_some()));
    }

    #[test]
    fn compression_zstd_sur_les_sept_colonnes() {
        let (colonnes, groupes) = trois_mois();
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &groupes);
        let builder = open_builder(&path).unwrap();
        let metadata = builder.metadata();
        assert_eq!(metadata.num_row_groups(), 3);
        for groupe in metadata.row_groups() {
            assert_eq!(groupe.num_columns(), COLUMN_NAMES.len());
            for colonne in groupe.columns() {
                let nom = colonne.column_path().string();
                // Le format Parquet n'enregistre pas le niveau zstd (le lecteur rend 1) : seul le
                // codec est vérifiable ici.
                assert!(
                    matches!(colonne.compression(), Compression::ZSTD(_)),
                    "colonne `{nom}` : {:?}",
                    colonne.compression()
                );
            }
        }
    }

    #[test]
    fn un_row_group_par_mois_dans_un_fichier_annuel() {
        let (colonnes, groupes) = trois_mois();
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &groupes);
        let info = inspect_parquet(&path).unwrap();
        let lignes: Vec<u64> = info.row_groups.iter().map(|g| g.rows).collect();
        assert_eq!(lignes, vec![744, 696, 744]);
        for (g, plage) in info.row_groups.iter().zip(&groupes) {
            assert_eq!(
                g.min_open_time_ms,
                Some(colonnes.open_time_ms()[plage.start])
            );
            assert_eq!(
                g.max_open_time_ms,
                Some(colonnes.open_time_ms()[plage.end - 1])
            );
        }
    }

    #[test]
    fn selection_des_row_groups_par_statistiques() {
        let (colonnes, groupes) = trois_mois();
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &groupes);
        let infos = inspect_parquet(&path).unwrap().row_groups;
        // Février 2024 : [1706745600000, 1709251200000).
        let fevrier = Window::new(1_706_745_600_000, 1_709_251_200_000).unwrap();
        assert_eq!(select_row_groups(&infos, Some(fevrier)), vec![1]);
        assert_eq!(select_row_groups(&infos, None), vec![0, 1, 2]);
        let sans_stats = vec![RowGroupInfo {
            rows: 5,
            min_open_time_ms: None,
            max_open_time_ms: None,
        }];
        assert_eq!(select_row_groups(&sans_stats, Some(fevrier)), vec![0]);
    }

    #[test]
    fn projection_six_colonnes_et_fenetre() {
        let (colonnes, groupes) = trois_mois();
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &groupes);
        let fevrier = Window::new(1_706_745_600_000, 1_709_251_200_000).unwrap();
        let lu = read_parquet(
            &path,
            &ReadOptions {
                projection: Projection::NoTrades,
                window: Some(fevrier),
                batch_size: 8192,
            },
        )
        .unwrap();
        assert_eq!(lu.len(), 696);
        assert!(lu.trades().is_none());
        assert_eq!(lu.open_time_ms()[0], 1_706_745_600_000);
        assert_eq!(bits(&lu), bits(&colonnes.slice(744..744 + 696)));
    }

    #[test]
    fn batch_size_sans_effet() {
        let (colonnes, groupes) = trois_mois();
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &groupes);
        let reference = read_parquet(&path, &ReadOptions::default()).unwrap();
        assert_eq!(reference, colonnes);
        let partition = crate::store::Partition::new(
            crate::model::ExchangeId::Kraken,
            "BTC-USD".parse().unwrap(),
            crate::model::Timeframe::H1,
        );
        let periode = crate::store::Period::Year { year: 2024 };
        let hash_ref = crate::store::dsh::content_hash(&partition, periode, &reference).unwrap();
        for batch_size in [1024, 4096, 5000, 8192] {
            let lu = read_parquet(
                &path,
                &ReadOptions {
                    batch_size,
                    ..ReadOptions::default()
                },
            )
            .unwrap();
            assert_eq!(lu, reference, "batch_size {batch_size}");
            assert_eq!(
                crate::store::dsh::content_hash(&partition, periode, &lu).unwrap(),
                hash_ref
            );
        }
        let zero = read_parquet(
            &path,
            &ReadOptions {
                batch_size: 0,
                ..ReadOptions::default()
            },
        );
        assert!(matches!(zero, Err(StoreError::Parquet { .. })));
    }

    #[test]
    fn fenetre_hors_fichier_zero_ligne() {
        let (colonnes, groupes) = trois_mois();
        let dir = tempfile::tempdir().unwrap();
        let path = ecrire(dir.path(), "a.parquet", &colonnes, &groupes);
        let avant = Window::new(0, T_2024_01).unwrap();
        let lu = read_parquet(
            &path,
            &ReadOptions {
                window: Some(avant),
                ..ReadOptions::default()
            },
        )
        .unwrap();
        assert!(lu.is_empty());
        assert!(lu.trades().is_some());
    }

    #[test]
    fn fichier_non_parquet_erreur_qui_nomme_le_fichier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("faux.parquet");
        std::fs::write(
            &path,
            b"ceci n'est pas du parquet, mais assez long pour un pied de page",
        )
        .unwrap();
        let erreur = read_parquet(&path, &ReadOptions::default()).unwrap_err();
        assert!(matches!(erreur, StoreError::Parquet { .. }));
        assert!(erreur.to_string().contains("faux.parquet"), "{erreur}");
        let absent = read_parquet(&dir.path().join("absent.parquet"), &ReadOptions::default());
        assert!(matches!(absent, Err(StoreError::Io { .. })));
    }

    /// Écrit un fichier avec un schéma arbitraire, comme le ferait un autre outil.
    fn ecrire_schema(path: &Path, schema: SchemaRef, arrays: Vec<ArrayRef>) {
        let file = File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema.clone(), None).unwrap();
        writer
            .write(&RecordBatch::try_new(schema, arrays).unwrap())
            .unwrap();
        writer.close().unwrap();
    }

    #[test]
    fn schema_inattendu_refuse() {
        let dir = tempfile::tempdir().unwrap();
        // Six colonnes : trades manque.
        let six = Arc::new(Schema::new(
            COLUMN_NAMES[..6]
                .iter()
                .enumerate()
                .map(|(i, nom)| {
                    Field::new(
                        *nom,
                        if i == 0 {
                            DataType::Int64
                        } else {
                            DataType::Float64
                        },
                        false,
                    )
                })
                .collect::<Vec<_>>(),
        ));
        let mut arrays: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(vec![60_000_i64]))];
        for _ in 0..5 {
            arrays.push(Arc::new(Float64Array::from(vec![1.0_f64])));
        }
        let six_path = dir.path().join("six.parquet");
        ecrire_schema(&six_path, six, arrays.clone());
        assert!(matches!(
            read_parquet(&six_path, &ReadOptions::default()),
            Err(StoreError::BadSchema { .. })
        ));
        // Sept colonnes mais trades en Int64.
        let mut champs: Vec<Field> = schema()
            .fields()
            .iter()
            .map(|f| f.as_ref().clone())
            .collect();
        champs[6] = Field::new("trades", DataType::Int64, true);
        arrays.push(Arc::new(Int64Array::from(vec![Some(3_i64)])));
        let int_path = dir.path().join("int.parquet");
        ecrire_schema(&int_path, Arc::new(Schema::new(champs)), arrays);
        let erreur = inspect_parquet(&int_path).unwrap_err();
        assert!(matches!(erreur, StoreError::BadSchema { .. }), "{erreur}");
        assert!(erreur.to_string().contains("int.parquet"));
    }

    #[test]
    fn groupe_vide_ou_trop_grand_refuse() {
        let colonnes = Columns::from_candles(&horaires(T_2024_01, 10));
        let dir = tempfile::tempdir().unwrap();
        for (nom, groupes) in [("vide.parquet", vec![3..3]), ("hors.parquet", vec![0..11])] {
            let path = dir.path().join(nom);
            let mut file = File::create(&path).unwrap();
            assert!(
                matches!(
                    write_parquet(&mut file, &path, &colonnes, &groupes),
                    Err(StoreError::Parquet { .. })
                ),
                "{nom}"
            );
        }
        let grand = Columns::from_candles(&horaires(T_2024_01, MAX_ROWS_PER_GROUP + 1));
        let path = dir.path().join("grand.parquet");
        let mut file = File::create(&path).unwrap();
        assert!(write_parquet(&mut file, &path, &grand, &[0..MAX_ROWS_PER_GROUP + 1]).is_err());
        // Colonnes sans trades : refus avant d'écrire.
        let mut parts = colonnes.into_parts();
        parts.trades = None;
        let sans = Columns::from_parts(parts).unwrap();
        assert!(matches!(
            write_parquet(&mut file, &path, &sans, &[0..10]),
            Err(StoreError::MissingTrades)
        ));
    }
}
