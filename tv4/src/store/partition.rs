//! Ingestion d'une partition : `write_partition` (docs/03 §5.1, décisions D3, D4 et D5 de S4).
//!
//! Le store n'écrit que des bougies réelles, triées, sans doublon, alignées sur le timeframe et
//! valides (NaN, infini, OHLC incohérent et volume négatif sont refusés ici, à l'écriture, comme
//! l'exige docs/03 §7.4). Tri, dédoublonnage et contrôle de continuité relèvent de l'ingestion (S5).

use std::ops::Range;
use std::path::Path;

use crate::model::Ts;

use super::StoreError;
use super::columns::Columns;
use super::dsh::{self, HASH_FORMAT};
use super::durable::write_durable;
use super::layout::{Partition, Period, resolve_rel};
use super::lock::PartitionLock;
use super::manifest::{FileManifest, read_file_manifest, write_file_manifest};
use super::parquet::{ReadOptions, read_parquet, write_parquet};
use super::time::format_rfc3339_utc;

/// Ce qu'une écriture a fait d'un fichier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStatus {
    /// Premier écrit de la période (un Parquet orphelin sans manifeste est écrasé).
    Created,
    /// Période non close, contenu ou clôture changés : Parquet et manifeste remplacés.
    Rewritten,
    /// Même contenu : ni Parquet ni manifeste réécrits (`ingested_at` est conservé).
    Unchanged,
}

/// Résultat de l'écriture d'une période.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub period: Period,
    /// Chemin relatif à la racine du fichier Parquet.
    pub file_rel: String,
    pub row_count: u64,
    pub closed: bool,
    /// `blake3:<hex>`
    pub content_hash: String,
    pub status: WriteStatus,
}

/// Longueur maximale de `source`.
const MAX_SOURCE_LEN: usize = 32;

/// Période prête à être écrite (phase 1). Elle garde la plage de lignes, pas une copie : la tranche
/// n'est recopiée qu'au moment d'écrire (phase 2).
struct Pending {
    period: Period,
    range: Range<usize>,
    hash_hex: String,
    /// Clôture à écrire dans le manifeste (pour `Unchanged`, celle du manifeste conservé).
    closed: bool,
    status: WriteStatus,
}

fn check_source(source: &str) -> Result<(), StoreError> {
    let valide = !source.is_empty()
        && source.len() <= MAX_SOURCE_LEN
        && source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valide {
        Ok(())
    } else {
        Err(StoreError::InvalidSource(source.to_string()))
    }
}

/// Contrôles d'entrée communs à toutes les périodes (phase 1, étape 1).
fn check_input(partition: &Partition, columns: &Columns) -> Result<(), StoreError> {
    if columns.is_empty() {
        return Err(StoreError::EmptyInput);
    }
    if columns.trades().is_none() {
        return Err(StoreError::MissingTrades);
    }
    if let Some(index) = columns.synthetic().iter().position(|s| *s) {
        return Err(StoreError::SyntheticNotStorable {
            open_time_ms: columns.open_time_ms()[index],
        });
    }
    columns.check_strictly_increasing()?;
    for index in 0..columns.len() {
        let Some(candle) = columns.get(index) else {
            continue;
        };
        partition.tf.check_alignment(candle.open_time_ms)?;
        candle.validate()?;
    }
    Ok(())
}

/// Plages consécutives de `columns` par période de fichier, dans l'ordre.
fn split_by_period(
    partition: &Partition,
    columns: &Columns,
) -> Result<Vec<(Period, Range<usize>)>, StoreError> {
    let mut plages: Vec<(Period, Range<usize>)> = Vec::new();
    for (index, &ts) in columns.open_time_ms().iter().enumerate() {
        let period = Period::containing(partition.tf, ts)?;
        match plages.last_mut() {
            Some((p, plage)) if *p == period => plage.end = index + 1,
            _ => plages.push((period, index..index + 1)),
        }
    }
    Ok(plages)
}

/// Un row group par mois : la plage entière pour un fichier mensuel, une plage par mois présent pour
/// un fichier annuel.
fn row_groups(partition: &Partition, slice: &Columns) -> Result<Vec<Range<usize>>, StoreError> {
    if Period::is_monthly(partition.tf) {
        return Ok(std::iter::once(0..slice.len()).collect());
    }
    let mut groupes: Vec<(Period, Range<usize>)> = Vec::new();
    for (index, &ts) in slice.open_time_ms().iter().enumerate() {
        // Période mensuelle d'un TF mensuel, utilisée comme simple clé de découpage par mois.
        let mois = Period::containing(crate::model::Timeframe::M1, ts)?;
        match groupes.last_mut() {
            Some((m, plage)) if *m == mois => plage.end = index + 1,
            _ => groupes.push((mois, index..index + 1)),
        }
    }
    Ok(groupes.into_iter().map(|(_, plage)| plage).collect())
}

/// `true` si le Parquet présent sur disque contient bien les lignes dont le `content_hash` est
/// `expected` (`blake3:...`). Un fichier illisible, au mauvais schéma ou dont le contenu ne se hache
/// pas (ligne hors de la période, doublon divergent) compte comme différent : la réingestion le
/// réécrit. Seule une erreur d'E/S est remontée (disque en panne, droits), pas masquée en réécriture.
fn parquet_has_hash(
    parquet_path: &Path,
    partition: &Partition,
    period: Period,
    expected: &str,
) -> Result<bool, StoreError> {
    let columns = match read_parquet(parquet_path, &ReadOptions::default()) {
        Ok(columns) => columns,
        Err(erreur @ StoreError::Io { .. }) => return Err(erreur),
        Err(_) => return Ok(false),
    };
    Ok(dsh::content_hash(partition, period, &columns)
        .is_ok_and(|(hex, _)| dsh::prefixed(&hex) == expected))
}

/// Écrit les bougies d'une partition, une période (mois ou année) par fichier, avec manifeste.
///
/// Chaque période présente dans `columns` **remplace** le contenu de son fichier (pas de fusion avec
/// les lignes déjà stockées : la fusion relève de S5 et S7). `now_ms` est l'instant d'écriture
/// (horloge injectée, D4) : une période est close quand sa fin est inférieure ou égale à `now_ms`.
/// `source` est la provenance écrite dans le manifeste.
///
/// Déroulé en deux phases. Phase 1 : contrôle des entrées, verrou de partition (gardé jusqu'à la fin
/// de la fonction), découpage par période, `content_hash` de chacune et décision (créer, réécrire,
/// laisser) d'après le manifeste existant ; **aucune écriture** si une période échoue ici. Phase 2 :
/// pour chaque période à écrire, le Parquet par `write_durable`, **puis** le manifeste. Un arrêt entre
/// les deux laisse un Parquet neuf et un manifeste ancien (ou absent) : `build_dataset` le détecte
/// (`FileHashMismatch` ou `ManifestMissing`) et une nouvelle ingestion le répare, **quel que soit le
/// contenu réingéré** : une période n'est déclarée [`WriteStatus::Unchanged`] que si le hash du
/// manifeste est égal à celui des lignes fournies **et** si le Parquet présent, relu en entier,
/// donne ce même hash (sinon il est réécrit, ce que même un mois clos permet, puisque le contenu
/// réécrit a exactement le hash du manifeste). Le prix est une relecture de chaque Parquet déjà
/// présent à chaque réingestion de même contenu. Une erreur d'E/S en phase 2 peut laisser les
/// périodes précédentes écrites : chacune reste cohérente (atomicité par fichier).
///
/// Règle d'immutabilité (D5) : un fichier clos déjà présent n'est jamais réécrit avec un contenu
/// différent ([`StoreError::ClosedFileModified`]) ; à contenu identique, rien n'est touché. Un mois
/// non clos peut être réécrit.
///
/// Conséquence voulue de D4 : une ingestion partielle d'un mois passé (par exemple la fixture, 8
/// jours de janvier 2020) marque ce mois clos. Le compléter plus tard exige de supprimer le fichier
/// et son manifeste à la main ; S5 décidera d'un mode de fusion.
pub fn write_partition(
    root: &Path,
    partition: &Partition,
    columns: &Columns,
    source: &str,
    now_ms: Ts,
) -> Result<Vec<WriteOutcome>, StoreError> {
    // Phase 1 : contrôles, verrou, décisions.
    check_source(source)?;
    check_input(partition, columns)?;
    let ingested_at = format_rfc3339_utc(now_ms)?;
    let _verrou = PartitionLock::acquire(root, partition)?;

    let mut pending = Vec::new();
    for (period, range) in split_by_period(partition, columns)? {
        let (hash_hex, _) = dsh::content_hash(partition, period, &columns.slice(range.clone()))?;
        let closed = period.window().end_ms() <= now_ms;
        let file_rel = partition.file_rel(period);
        let manifest_rel = partition.manifest_rel(period);
        let manifest_path = resolve_rel(root, &manifest_rel)?;
        let parquet_path = resolve_rel(root, &file_rel)?;
        let mut closed_out = closed;
        let status = if manifest_path.is_file() {
            let existing = read_file_manifest(&manifest_path)?;
            check_identity(&manifest_rel, &existing, partition, period)?;
            let same_hash = existing.content_hash == dsh::prefixed(&hash_hex);
            if existing.closed && !same_hash {
                return Err(StoreError::ClosedFileModified {
                    path: file_rel,
                    expected: existing.content_hash,
                    actual: dsh::prefixed(&hash_hex),
                });
            }
            // Un Parquet disparu, illisible ou de contenu différent est réparé plutôt que déclaré
            // inchangé (arrêt entre le `rename` du Parquet et l'écriture du manifeste, altération).
            let parquet_conforme = same_hash
                && parquet_path.is_file()
                && parquet_has_hash(&parquet_path, partition, period, &existing.content_hash)?;
            // Un fichier déjà clos le reste, même si l'horloge injectée recule.
            closed_out = closed || (same_hash && existing.closed);
            if parquet_conforme && (existing.closed || !closed) {
                // Le manifeste conservé fait foi.
                closed_out = existing.closed;
                WriteStatus::Unchanged
            } else {
                WriteStatus::Rewritten
            }
        } else {
            WriteStatus::Created
        };
        pending.push(Pending {
            period,
            range,
            hash_hex,
            closed: closed_out,
            status,
        });
    }

    // Phase 2 : écriture, Parquet d'abord puis manifeste.
    let mut outcomes = Vec::with_capacity(pending.len());
    for item in pending {
        let file_rel = partition.file_rel(item.period);
        let row_count = item.range.len() as u64;
        if item.status != WriteStatus::Unchanged {
            let slice = columns.slice(item.range.clone());
            let parquet_path = resolve_rel(root, &file_rel)?;
            let groupes = row_groups(partition, &slice)?;
            write_durable(&parquet_path, |file| {
                write_parquet(file, &parquet_path, &slice, &groupes)
            })?;
            let times = slice.open_time_ms();
            let manifest = FileManifest {
                exchange: partition.exchange,
                symbol: partition.symbol.clone(),
                tf: partition.tf,
                period: item.period.label(),
                closed: item.closed,
                row_count,
                ts_min: times[0],
                ts_max: times[times.len() - 1],
                content_hash: dsh::prefixed(&item.hash_hex),
                hash_format: HASH_FORMAT.to_string(),
                source: source.to_string(),
                ingested_at: ingested_at.clone(),
            };
            let manifest_path = resolve_rel(root, &partition.manifest_rel(item.period))?;
            write_file_manifest(&manifest_path, &manifest)?;
        }
        outcomes.push(WriteOutcome {
            period: item.period,
            file_rel,
            row_count,
            closed: item.closed,
            content_hash: dsh::prefixed(&item.hash_hex),
            status: item.status,
        });
    }
    Ok(outcomes)
}

/// Contrôle l'identité d'un manifeste existant (exchange, symbole, tf, période, format de hash).
pub(super) fn check_identity(
    manifest_rel: &str,
    existing: &FileManifest,
    partition: &Partition,
    period: Period,
) -> Result<(), StoreError> {
    let mismatch = |field: &'static str, manifest: String, actual: String| {
        Err(StoreError::ManifestMismatch {
            path: manifest_rel.to_string(),
            field,
            manifest,
            actual,
        })
    };
    if existing.exchange != partition.exchange {
        return mismatch(
            "exchange",
            existing.exchange.to_string(),
            partition.exchange.to_string(),
        );
    }
    if existing.symbol != partition.symbol {
        return mismatch(
            "symbol",
            existing.symbol.to_string(),
            partition.symbol.to_string(),
        );
    }
    if existing.tf != partition.tf {
        return mismatch("tf", existing.tf.to_string(), partition.tf.to_string());
    }
    if existing.period != period.label() {
        return mismatch("period", existing.period.clone(), period.label());
    }
    if existing.hash_format != HASH_FORMAT {
        return mismatch(
            "hash_format",
            existing.hash_format.clone(),
            HASH_FORMAT.to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // une plage de lignes = un row group
mod tests {
    use super::*;
    use crate::model::{Candle, ExchangeId, Timeframe};
    use crate::store::parquet::inspect_parquet;
    use std::fs;

    /// 2024-01-31T23:58:00Z.
    const FIN_JANVIER: i64 = 1_706_745_480_000;
    const T_2024_01: i64 = 1_704_067_200_000;
    const T_2024_02: i64 = 1_706_745_600_000;
    const T_2024_03: i64 = 1_709_251_200_000;
    /// 2030-01-01T00:00:00Z : après toutes les périodes des tests.
    const FUTUR: i64 = 1_893_456_000_000;

    fn partition(tf: Timeframe) -> Partition {
        Partition::new(ExchangeId::Kraken, "SYN-USD".parse().unwrap(), tf)
    }

    /// `n` bougies synthétiques (prix inventés, autour de 100) depuis `debut`, au pas de `tf`.
    fn bougies(debut: i64, n: usize, tf: Timeframe) -> Columns {
        let candles: Vec<Candle> = (0..n)
            .map(|i| {
                let base = 100.0 + (i % 40) as f64 * 0.5;
                Candle {
                    open_time_ms: debut + i as i64 * tf.ms(),
                    open: base,
                    high: base + 1.0,
                    low: base - 1.0,
                    close: base + 0.25,
                    volume: 1.0 + (i % 5) as f64,
                    trades: Some((i % 9) as u32),
                }
            })
            .collect();
        Columns::from_candles(&candles)
    }

    fn lire(root: &Path, p: &Partition, periode: Period) -> (Vec<u8>, Vec<u8>) {
        (
            fs::read(resolve_rel(root, &p.file_rel(periode)).unwrap()).unwrap(),
            fs::read(resolve_rel(root, &p.manifest_rel(periode)).unwrap()).unwrap(),
        )
    }

    const JANVIER: Period = Period::Month {
        year: 2024,
        month: 1,
    };
    const FEVRIER: Period = Period::Month {
        year: 2024,
        month: 2,
    };

    #[test]
    fn un_fichier_et_un_manifeste_par_mois() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let colonnes = bougies(FIN_JANVIER, 5, Timeframe::M1);
        let sorties = write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        assert_eq!(sorties.len(), 2);
        assert_eq!(sorties[0].period, JANVIER);
        assert_eq!(sorties[1].period, FEVRIER);
        assert_eq!(
            sorties[0].file_rel,
            "data/candles/kraken/SYN-USD/1m/2024-01.parquet"
        );
        for (sortie, plage) in sorties.iter().zip([0..2, 2..5]) {
            assert_eq!(sortie.status, WriteStatus::Created);
            assert!(sortie.closed);
            let tranche = colonnes.slice(plage);
            let (hex, lignes) = dsh::content_hash(&p, sortie.period, &tranche).unwrap();
            assert_eq!(sortie.content_hash, format!("blake3:{hex}"));
            let manifeste = read_file_manifest(
                &resolve_rel(dir.path(), &p.manifest_rel(sortie.period)).unwrap(),
            )
            .unwrap();
            assert_eq!(manifeste.row_count, lignes);
            assert_eq!(manifeste.ts_min, tranche.open_time_ms()[0]);
            assert_eq!(manifeste.ts_max, tranche.open_time_ms()[tranche.len() - 1]);
            assert_eq!(manifeste.content_hash, sortie.content_hash);
            assert_eq!(manifeste.hash_format, "dsh-v1");
            assert_eq!(manifeste.period, sortie.period.label());
            assert_eq!(manifeste.source, "synthetic");
            assert_eq!(manifeste.ingested_at, "2030-01-01T00:00:00Z");
            assert!(manifeste.closed);
        }
    }

    #[test]
    fn fichier_annuel_pour_un_tf_horaire() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::H1);
        // Janvier (744), février (696, bissextile) et mars (744) 2024 en 1h.
        let colonnes = bougies(T_2024_01, 744 + 696 + 744, Timeframe::H1);
        let sorties = write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        assert_eq!(sorties.len(), 1);
        assert_eq!(sorties[0].period, Period::Year { year: 2024 });
        assert_eq!(
            sorties[0].file_rel,
            "data/candles/kraken/SYN-USD/1h/2024.parquet"
        );
        let info =
            inspect_parquet(&resolve_rel(dir.path(), &sorties[0].file_rel).unwrap()).unwrap();
        let lignes: Vec<u64> = info.row_groups.iter().map(|g| g.rows).collect();
        assert_eq!(lignes, vec![744, 696, 744]);
        assert_eq!(info.row_groups[1].min_open_time_ms, Some(T_2024_02));
        assert_eq!(info.row_groups[2].min_open_time_ms, Some(T_2024_03));
    }

    #[test]
    fn entrees_refusees() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let ok = bougies(T_2024_01, 3, Timeframe::M1);
        let ecrire = |colonnes: &Columns, source: &str| {
            write_partition(dir.path(), &p, colonnes, source, FUTUR)
        };
        assert!(matches!(
            ecrire(&Columns::with_capacity(0), "synthetic"),
            Err(StoreError::EmptyInput)
        ));
        // Désordre et doublon.
        let b = ok.to_candles();
        let desordre = Columns::from_candles(&[b[1], b[0]]);
        assert!(matches!(
            ecrire(&desordre, "synthetic"),
            Err(StoreError::NotStrictlyIncreasing { .. })
        ));
        let doublon = Columns::from_candles(&[b[0], b[0]]);
        assert!(matches!(
            ecrire(&doublon, "synthetic"),
            Err(StoreError::NotStrictlyIncreasing { .. })
        ));
        // Désalignement.
        let mut decale = b[0];
        decale.open_time_ms += 1;
        assert!(matches!(
            ecrire(&Columns::from_candles(&[decale]), "synthetic"),
            Err(StoreError::Model(_))
        ));
        // NaN.
        let mut nan = b[0];
        nan.close = f64::NAN;
        assert!(matches!(
            ecrire(&Columns::from_candles(&[nan]), "synthetic"),
            Err(StoreError::Model(_))
        ));
        // OHLC incohérent.
        let mut incoherent = b[0];
        incoherent.high = incoherent.low - 1.0;
        assert!(ecrire(&Columns::from_candles(&[incoherent]), "synthetic").is_err());
        // Colonnes sans trades.
        let mut parts = ok.clone().into_parts();
        parts.trades = None;
        assert!(matches!(
            ecrire(&Columns::from_parts(parts).unwrap(), "synthetic"),
            Err(StoreError::MissingTrades)
        ));
        // Bougie synthétique.
        let mut avec_synth = Columns::with_capacity(1);
        avec_synth.push(&b[0], true);
        assert!(matches!(
            ecrire(&avec_synth, "synthetic"),
            Err(StoreError::SyntheticNotStorable { .. })
        ));
        // Source invalide.
        for source in ["", "Synthetic", "a b", &"x".repeat(33)] {
            assert!(
                matches!(ecrire(&ok, source), Err(StoreError::InvalidSource(_))),
                "source « {source} »"
            );
        }
        // Rien n'a été écrit.
        assert!(!dir.path().join("data").exists());
    }

    #[test]
    fn closed_selon_l_instant_injecte() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let colonnes = bougies(T_2024_01, 3, Timeframe::M1);
        let fin = JANVIER.window().end_ms();
        let non_clos = write_partition(dir.path(), &p, &colonnes, "synthetic", fin - 1).unwrap();
        assert!(!non_clos[0].closed);
        let clos = write_partition(dir.path(), &p, &colonnes, "synthetic", fin).unwrap();
        assert!(clos[0].closed);
        // Même contenu, mais la clôture a changé : réécrit.
        assert_eq!(clos[0].status, WriteStatus::Rewritten);
        let manifeste =
            read_file_manifest(&resolve_rel(dir.path(), &p.manifest_rel(JANVIER)).unwrap())
                .unwrap();
        assert!(manifeste.closed);
        assert_eq!(manifeste.ingested_at, "2024-02-01T00:00:00Z");
    }

    #[test]
    fn mois_non_clos_reecrit() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let maintenant = T_2024_01 + 86_400_000;
        let premier = write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 3, Timeframe::M1),
            "synthetic",
            maintenant,
        )
        .unwrap();
        assert_eq!(premier[0].status, WriteStatus::Created);
        assert!(!premier[0].closed);
        let second = write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 7, Timeframe::M1),
            "synthetic",
            maintenant,
        )
        .unwrap();
        assert_eq!(second[0].status, WriteStatus::Rewritten);
        assert_eq!(second[0].row_count, 7);
        assert_ne!(premier[0].content_hash, second[0].content_hash);
        let manifeste =
            read_file_manifest(&resolve_rel(dir.path(), &p.manifest_rel(JANVIER)).unwrap())
                .unwrap();
        assert_eq!(manifeste.row_count, 7);
        assert_eq!(manifeste.content_hash, second[0].content_hash);
        // Un non clos identique reste inchangé.
        let troisieme = write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 7, Timeframe::M1),
            "synthetic",
            maintenant,
        )
        .unwrap();
        assert_eq!(troisieme[0].status, WriteStatus::Unchanged);
    }

    #[test]
    fn mois_clos_identique_sans_effet() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let colonnes = bougies(T_2024_01, 4, Timeframe::M1);
        write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        let avant = lire(dir.path(), &p, JANVIER);
        let sortie =
            write_partition(dir.path(), &p, &colonnes, "autre-source", FUTUR + 1000).unwrap();
        assert_eq!(sortie[0].status, WriteStatus::Unchanged);
        assert!(sortie[0].closed);
        assert_eq!(lire(dir.path(), &p, JANVIER), avant);
        // `ingested_at` et `source` d'origine sont conservés.
        let manifeste = String::from_utf8(avant.1).unwrap();
        assert!(manifeste.contains("\"ingested_at\": \"2030-01-01T00:00:00Z\""));
        assert!(manifeste.contains("\"source\": \"synthetic\""));
    }

    #[test]
    fn mois_clos_modifie_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 4, Timeframe::M1),
            "synthetic",
            FUTUR,
        )
        .unwrap();
        let avant = lire(dir.path(), &p, JANVIER);
        let erreur = write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 5, Timeframe::M1),
            "synthetic",
            FUTUR,
        )
        .unwrap_err();
        assert!(matches!(erreur, StoreError::ClosedFileModified { .. }));
        assert!(erreur.to_string().contains("2024-01.parquet"));
        assert_eq!(lire(dir.path(), &p, JANVIER), avant);
    }

    #[test]
    fn rien_n_est_ecrit_si_une_periode_echoue() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        // Février seul, clos.
        write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_02, 3, Timeframe::M1),
            "synthetic",
            FUTUR,
        )
        .unwrap();
        let fevrier_avant = lire(dir.path(), &p, FEVRIER);
        // Deux mois : janvier est nouveau, février est clos et modifié.
        let deux_mois = bougies(FIN_JANVIER, 6, Timeframe::M1);
        let erreur = write_partition(dir.path(), &p, &deux_mois, "synthetic", FUTUR).unwrap_err();
        assert!(matches!(erreur, StoreError::ClosedFileModified { .. }));
        assert!(
            !resolve_rel(dir.path(), &p.file_rel(JANVIER))
                .unwrap()
                .exists(),
            "janvier ne doit pas avoir été écrit"
        );
        assert!(
            !resolve_rel(dir.path(), &p.manifest_rel(JANVIER))
                .unwrap()
                .exists()
        );
        assert_eq!(lire(dir.path(), &p, FEVRIER), fevrier_avant);
    }

    #[test]
    fn verrou_pris_ecriture_refusee() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let _verrou = PartitionLock::acquire(dir.path(), &p).unwrap();
        let erreur = write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 3, Timeframe::M1),
            "synthetic",
            FUTUR,
        )
        .unwrap_err();
        assert!(matches!(erreur, StoreError::LockBusy { .. }));
        assert!(!dir.path().join("data").exists());
    }

    #[test]
    fn manifeste_d_une_autre_serie_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 3, Timeframe::M1),
            "synthetic",
            FUTUR,
        )
        .unwrap();
        // On falsifie le symbole du manifeste existant.
        let chemin = resolve_rel(dir.path(), &p.manifest_rel(JANVIER)).unwrap();
        let texte = fs::read_to_string(&chemin)
            .unwrap()
            .replace("SYN-USD", "OTH-USD");
        fs::write(&chemin, texte).unwrap();
        let erreur = write_partition(
            dir.path(),
            &p,
            &bougies(T_2024_01, 3, Timeframe::M1),
            "synthetic",
            FUTUR,
        )
        .unwrap_err();
        assert!(matches!(
            erreur,
            StoreError::ManifestMismatch {
                field: "symbol",
                ..
            }
        ));
    }

    #[test]
    fn parquet_disparu_repare() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let colonnes = bougies(T_2024_01, 3, Timeframe::M1);
        write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        fs::remove_file(resolve_rel(dir.path(), &p.file_rel(JANVIER)).unwrap()).unwrap();
        let sortie = write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        assert_eq!(sortie[0].status, WriteStatus::Rewritten);
        assert!(
            resolve_rel(dir.path(), &p.file_rel(JANVIER))
                .unwrap()
                .is_file()
        );
    }
    /// Remplace le Parquet de `periode` par `colonnes`, sans toucher au manifeste.
    fn remplacer_parquet(root: &Path, p: &Partition, periode: Period, colonnes: &Columns) {
        let chemin = resolve_rel(root, &p.file_rel(periode)).unwrap();
        write_durable(&chemin, |f| {
            write_parquet(f, &chemin, colonnes, &[0..colonnes.len()])
        })
        .unwrap();
    }

    #[test]
    fn parquet_altere_d_un_mois_clos_repare() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let colonnes = bougies(T_2024_01, 4, Timeframe::M1);
        write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        let (parquet_avant, _) = lire(dir.path(), &p, JANVIER);
        // Le Parquet est remplacé par un autre contenu valide, le manifeste (clos) ne bouge pas.
        remplacer_parquet(
            dir.path(),
            &p,
            JANVIER,
            &bougies(T_2024_01, 5, Timeframe::M1),
        );
        assert_ne!(lire(dir.path(), &p, JANVIER).0, parquet_avant);
        // Réingérer le contenu d'origine d'un mois clos répare le fichier au lieu de répondre
        // « inchangé ».
        let sortie = write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR + 1).unwrap();
        assert_eq!(sortie[0].status, WriteStatus::Rewritten);
        assert!(sortie[0].closed);
        assert_eq!(lire(dir.path(), &p, JANVIER).0, parquet_avant);
        // Et la réingestion suivante est bien sans effet.
        let encore = write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR + 2).unwrap();
        assert_eq!(encore[0].status, WriteStatus::Unchanged);
    }

    #[test]
    fn parquet_illisible_repare() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        let colonnes = bougies(T_2024_01, 4, Timeframe::M1);
        write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        let chemin = resolve_rel(dir.path(), &p.file_rel(JANVIER)).unwrap();
        fs::write(&chemin, b"pas du parquet").unwrap();
        let sortie = write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        assert_eq!(sortie[0].status, WriteStatus::Rewritten);
        assert!(inspect_parquet(&chemin).is_ok());
    }

    #[test]
    fn mois_complet_en_un_seul_row_group() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition(Timeframe::M1);
        // Janvier 2024 : 31 jours de 1m, soit 44 640 lignes, le maximum d'un row group.
        let colonnes = bougies(T_2024_01, 44_640, Timeframe::M1);
        let sortie = write_partition(dir.path(), &p, &colonnes, "synthetic", FUTUR).unwrap();
        assert_eq!(sortie[0].row_count, 44_640);
        let info = inspect_parquet(&resolve_rel(dir.path(), &sortie[0].file_rel).unwrap()).unwrap();
        assert_eq!(info.row_groups.len(), 1);
        assert_eq!(info.rows, 44_640);
        assert_eq!(info.row_groups[0].rows, 44_640);
    }
}
