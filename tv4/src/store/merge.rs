//! Fusion d'une période avec le fichier existant (docs/03 §5.1 et §11, S7 : reprise de `tv4 ingest okx`).
//!
//! `write_partition_with` remplace le contenu d'une période. L'ingestion OKX, elle, écrit un mois en
//! plusieurs fois (un lot de pages après l'autre, puis une reprise après un arrêt) : chaque écriture
//! doit relire le fichier présent, y ajouter les bougies reçues, dédoublonner (la page chevauchante
//! d'une reprise est retéléchargée) et réécrire, le tout **sous le même verrou de partition**, pour
//! qu'un second ingesteur ne s'intercale pas entre la lecture et le `rename`.

use std::path::Path;
use std::time::Duration;

use crate::model::Ts;

use super::StoreError;
use super::columns::Columns;
use super::layout::{Partition, Period, resolve_rel};
use super::lock::PartitionLock;
use super::parquet::{ReadOptions, read_parquet};
use super::partition::{
    ContinuityInput, WriteOptions, WriteOutcome, check_input, check_source, write_locked,
};

/// Fusionne `incoming` (strictement croissant, toutes les lignes dans `period`) avec le fichier de
/// `period` s'il existe, puis écrit le résultat par la séquence de [`super::write_partition_with`]
/// (fichier temporaire, `fsync`, `rename`, manifeste). Le verrou de partition est attendu jusqu'à
/// `lock_timeout` ([`PartitionLock::acquire_wait`]) puis tenu pendant la relecture et l'écriture.
///
/// - À `open_time_ms` égal, la ligne de `incoming` remplace celle du fichier. Un fichier clos dont le
///   contenu fusionné diffère reste refusé ([`StoreError::ClosedFileModified`]) : la fusion ne contourne
///   pas l'immutabilité.
/// - `incoming` peut être vide : le fichier existant est réécrit tel quel, ce qui met à jour `closed` et
///   le bloc `continuity` (fermeture d'un mois dont la couverture vient d'être établie). Sans fichier ni
///   ligne, rien n'est écrit (`Ok(None)`).
/// - Les `rejected_unclosed` de `options.continuity` dont l'horodatage est présent dans le résultat
///   sont retirés du décompte : une bougie qui existe est, par définition, clôturée.
/// - Un fichier existant illisible (autre qu'une erreur d'E/S) est une erreur, jamais écrasé en silence.
pub fn merge_period_with(
    root: &Path,
    partition: &Partition,
    period: Period,
    incoming: &Columns,
    options: &WriteOptions<'_>,
    lock_timeout: Duration,
) -> Result<Option<WriteOutcome>, StoreError> {
    check_source(options.source)?;
    // `write_locked` écrit chaque période présente : une ligne d'une autre période ouvrirait un second
    // fichier. Le contrôle est fait avant toute lecture ou écriture.
    let window = period.window();
    if let Some(&open_time_ms) = incoming
        .open_time_ms()
        .iter()
        .find(|ts| !window.contains(**ts))
    {
        return Err(StoreError::RowOutsideWindow {
            open_time_ms,
            start_ms: window.start_ms(),
            end_ms: window.end_ms(),
        });
    }
    let _verrou = PartitionLock::acquire_wait(root, partition, lock_timeout)?;

    let parquet_path = resolve_rel(root, &partition.file_rel(period))?;
    let merged = if parquet_path.is_file() {
        let existing = read_parquet(&parquet_path, &ReadOptions::default())?;
        if incoming.is_empty() {
            existing
        } else {
            existing.merge_newer(incoming)?
        }
    } else {
        incoming.clone()
    };
    if merged.is_empty() {
        return Ok(None);
    }
    check_input(partition, &merged)?;

    // Une bougie présente ne peut pas être « non clôturée » : on la retire du décompte.
    let rejected: Option<Vec<Ts>> = options.continuity.as_ref().map(|input| {
        input
            .rejected_unclosed
            .iter()
            .copied()
            .filter(|ts| merged.open_time_ms().binary_search(ts).is_err())
            .collect()
    });
    let adjusted = WriteOptions {
        continuity: match (&options.continuity, &rejected) {
            (Some(input), Some(rejected)) => Some(ContinuityInput {
                duplicates: input.duplicates,
                rejected_unclosed: rejected,
            }),
            _ => None,
        },
        ..*options
    };
    let outcomes = write_locked(root, partition, &merged, &adjusted)?;
    Ok(outcomes.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Candle, ExchangeId, Timeframe};
    use crate::store::manifest::read_file_manifest;
    use crate::store::partition::WriteStatus;

    const T_2024_01: Ts = 1_704_067_200_000;
    /// 2030-01-01 : après toutes les périodes.
    const FUTUR: Ts = 1_893_456_000_000;
    const JANVIER: Period = Period::Month {
        year: 2024,
        month: 1,
    };
    const DELAI: Duration = Duration::from_secs(5);

    fn partition() -> Partition {
        Partition::new(ExchangeId::Okx, "BTC-USDC".parse().unwrap(), Timeframe::M1)
    }

    /// Bougies des minutes `minutes` ; `volume` distingue deux lectures de la même minute.
    fn bougies(minutes: std::ops::Range<i64>, volume: f64) -> Columns {
        let candles: Vec<Candle> = minutes
            .map(|m| Candle {
                open_time_ms: T_2024_01 + m * 60_000,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume,
                trades: None,
            })
            .collect();
        Columns::from_candles(&candles)
    }

    fn options<'a>(closed_until_ms: Ts, unclosed: &'a [Ts]) -> WriteOptions<'a> {
        WriteOptions {
            source: "okx-history-candles",
            now_ms: FUTUR,
            closed_until_ms,
            continuity: Some(ContinuityInput {
                duplicates: &[],
                rejected_unclosed: unclosed,
            }),
        }
    }

    fn lire(root: &Path) -> Columns {
        let chemin = resolve_rel(root, &partition().file_rel(JANVIER)).unwrap();
        read_parquet(&chemin, &ReadOptions::default()).unwrap()
    }

    #[test]
    fn fusion_de_deux_lots_qui_se_chevauchent_la_plus_recente_l_emporte() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition();
        let options = options(0, &[]);
        let premier = merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(10..30, 1.0),
            &options,
            DELAI,
        )
        .unwrap()
        .unwrap();
        assert_eq!(premier.status, WriteStatus::Created);
        assert_eq!(premier.row_count, 20);
        // Second lot : minutes 0 à 14, dont 10 à 14 déjà présentes avec un autre volume.
        let second = merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(0..15, 2.0),
            &options,
            DELAI,
        )
        .unwrap()
        .unwrap();
        assert_eq!(second.status, WriteStatus::Rewritten);
        assert_eq!(second.row_count, 30);
        let colonnes = lire(dir.path());
        let minutes: Vec<i64> = colonnes
            .open_time_ms()
            .iter()
            .map(|t| (t - T_2024_01) / 60_000)
            .collect();
        assert_eq!(minutes, (0..30).collect::<Vec<i64>>());
        // Minutes 0 à 14 : lot récent (volume 2) ; 15 à 29 : fichier (volume 1).
        assert!(colonnes.volume()[..15].iter().all(|v| *v == 2.0));
        assert!(colonnes.volume()[15..].iter().all(|v| *v == 1.0));
    }

    #[test]
    fn lot_vide_cloture_le_fichier_existant_sans_changer_son_contenu() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition();
        let ecrit = merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(0..5, 1.0),
            &options(0, &[]),
            DELAI,
        )
        .unwrap()
        .unwrap();
        assert!(!ecrit.closed);
        let clos = merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &Columns::default(),
            &options(FUTUR, &[]),
            DELAI,
        )
        .unwrap()
        .unwrap();
        assert!(clos.closed);
        assert_eq!(clos.content_hash, ecrit.content_hash);
        assert_eq!(clos.status, WriteStatus::Rewritten);
        // Sans fichier ni ligne, rien n'est écrit.
        let vide = tempfile::tempdir().unwrap();
        let rien = merge_period_with(
            vide.path(),
            &p,
            JANVIER,
            &Columns::default(),
            &options(0, &[]),
            DELAI,
        )
        .unwrap();
        assert!(rien.is_none());
    }

    #[test]
    fn un_fichier_clos_n_accepte_pas_un_contenu_different() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition();
        merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(0..5, 1.0),
            &options(FUTUR, &[]),
            DELAI,
        )
        .unwrap();
        // Même minutes, autre volume : le contenu fusionné diffère.
        let erreur = merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(0..5, 9.0),
            &options(0, &[]),
            DELAI,
        )
        .unwrap_err();
        assert!(matches!(erreur, StoreError::ClosedFileModified { .. }));
        // Mêmes valeurs : inchangé.
        let meme = merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(0..5, 1.0),
            &options(0, &[]),
            DELAI,
        )
        .unwrap()
        .unwrap();
        assert_eq!(meme.status, WriteStatus::Unchanged);
        assert!(meme.closed);
    }

    #[test]
    fn rejets_confirm_zero_comptes_sauf_si_la_bougie_existe() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition();
        // Minute 5 jamais vue clôturée ; minute 3 est présente : elle ne compte pas.
        let rejets = [T_2024_01 + 5 * 60_000, T_2024_01 + 3 * 60_000];
        merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(0..5, 1.0),
            &options(0, &rejets),
            DELAI,
        )
        .unwrap();
        let manifeste =
            read_file_manifest(&resolve_rel(dir.path(), &p.manifest_rel(JANVIER)).unwrap())
                .unwrap();
        let continuite = manifeste.continuity.unwrap();
        assert_eq!(continuite.rejected_unclosed, 1);
        assert_eq!(continuite.duplicates, 0);
    }

    #[test]
    fn ligne_hors_de_la_periode_refusee_avant_toute_ecriture() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition();
        // Minute 44 640 = 1er février.
        let erreur = merge_period_with(
            dir.path(),
            &p,
            JANVIER,
            &bougies(44_639..44_641, 1.0),
            &options(0, &[]),
            DELAI,
        )
        .unwrap_err();
        assert!(matches!(erreur, StoreError::RowOutsideWindow { .. }));
        assert!(
            !resolve_rel(dir.path(), &p.file_rel(JANVIER))
                .unwrap()
                .exists()
        );
    }
}
