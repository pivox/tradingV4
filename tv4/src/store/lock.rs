//! Verrou de partition (docs/03 §5.1, décision D2 de S4).
//!
//! Un seul écrivain à la fois par (exchange, symbole, timeframe) : `write_partition` et
//! `build_dataset` prennent ce verrou. `build_dataset` prend en plus un verrou par nom de dataset
//! ([`DatasetLock`]) : `datasets/<nom>.json` est commun à toutes les partitions, le verrou de
//! partition seul laisserait deux builds de même nom se dépasser (contrôle d'existence puis
//! écriture, D10). `load_dataset` ne prend aucun verrou : un fichier listé est soit
//! clos (jamais modifié), soit un snapshot (jamais réécrit), et le remplacement par `rename` est
//! atomique pour un lecteur.

use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use super::StoreError;
use super::durable::io_error;
use super::layout::Partition;

/// Verrou exclusif d'une partition : `std::fs::File::try_lock` (Rust 1.89), non bloquant, un verrou
/// déjà pris est une erreur immédiate.
///
/// Libéré au drop ou à la mort du processus (verrou consultatif `flock` sous Linux, sur la
/// description de fichier ouverte : deux `open` du même fichier dans un même processus s'excluent
/// aussi, ce que les tests utilisent). Le fichier de verrou n'est jamais supprimé (supprimer un
/// fichier de verrou ouvre une course). Valable sur un système de fichiers local seulement, et
/// seulement contre les processus qui prennent ce même verrou.
#[derive(Debug)]
pub struct PartitionLock {
    _file: File,
    path: PathBuf,
}

/// Prend le verrou du fichier `rel` (chemin relatif à `root`, séparateur `/`) : `Ok(None)` si un
/// autre écrivain le tient.
fn take(root: &Path, rel: &str) -> Result<Option<(File, PathBuf)>, StoreError> {
    let path = rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    }
    let file = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    match file.try_lock() {
        Ok(()) => Ok(Some((file, path))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(source)) => Err(StoreError::Io {
            path: rel.to_string(),
            source,
        }),
    }
}

impl PartitionLock {
    /// Prend le verrou de `partition` sous `root/state/locks/...`.
    ///
    /// [`StoreError::LockBusy`] si un autre écrivain le tient.
    pub fn acquire(root: &Path, partition: &Partition) -> Result<PartitionLock, StoreError> {
        let rel = partition.lock_rel();
        match take(root, &rel)? {
            Some((file, path)) => Ok(PartitionLock { _file: file, path }),
            None => Err(StoreError::LockBusy {
                partition: partition.label(),
                path: rel,
            }),
        }
    }

    /// Comme [`PartitionLock::acquire`], mais attend jusqu'à `timeout` que l'écrivain en cours finisse
    /// (sondage toutes les 5 ms). Les trois threads de `tv4 ingest okx` écrivent des périodes
    /// différentes d'une même partition : leurs écritures, de quelques dizaines de millisecondes, se
    /// succèdent sous le même verrou. [`StoreError::LockBusy`] si le verrou n'est pas libéré à temps.
    pub fn acquire_wait(
        root: &Path,
        partition: &Partition,
        timeout: Duration,
    ) -> Result<PartitionLock, StoreError> {
        let deadline = Instant::now() + timeout;
        loop {
            match PartitionLock::acquire(root, partition) {
                Err(StoreError::LockBusy { .. }) if Instant::now() < deadline => {
                    thread::sleep(LOCK_POLL);
                }
                other => return other,
            }
        }
    }

    /// Chemin du fichier de verrou.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Intervalle de sondage de [`PartitionLock::acquire_wait`].
const LOCK_POLL: Duration = Duration::from_millis(5);

/// Verrou exclusif d'un nom de dataset : `state/locks/datasets/<nom>.lock`, mêmes propriétés que
/// [`PartitionLock`]. Le nom est supposé déjà contrôlé (`[a-z0-9_]{1,64}`, jamais de `/`). Ce chemin
/// ne peut pas heurter un verrou de partition : aucun exchange ne s'appelle `datasets`.
#[derive(Debug)]
pub struct DatasetLock {
    _file: File,
    path: PathBuf,
}

impl DatasetLock {
    /// Prend le verrou du dataset `name` sous `root/state/locks/datasets/`.
    ///
    /// [`StoreError::DatasetBusy`] si un autre build de ce nom tourne.
    pub fn acquire(root: &Path, name: &str) -> Result<DatasetLock, StoreError> {
        let rel = format!("state/locks/datasets/{name}.lock");
        match take(root, &rel)? {
            Some((file, path)) => Ok(DatasetLock { _file: file, path }),
            None => Err(StoreError::DatasetBusy {
                name: name.to_string(),
                path: rel,
            }),
        }
    }

    /// Chemin du fichier de verrou.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExchangeId, Timeframe};

    fn partition(symbole: &str, tf: Timeframe) -> Partition {
        Partition::new(ExchangeId::Kraken, symbole.parse().unwrap(), tf)
    }

    #[test]
    fn second_verrou_refuse_puis_libere_au_drop() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition("BTC-USD", Timeframe::M1);
        let premier = PartitionLock::acquire(dir.path(), &p).unwrap();
        let erreur = PartitionLock::acquire(dir.path(), &p).unwrap_err();
        assert!(matches!(erreur, StoreError::LockBusy { .. }));
        let message = erreur.to_string();
        assert!(message.contains("kraken/BTC-USD/1m"), "{message}");
        assert!(
            message.contains("state/locks/kraken/BTC-USD/1m.lock"),
            "{message}"
        );
        drop(premier);
        PartitionLock::acquire(dir.path(), &p).unwrap();
    }

    #[test]
    fn attente_du_verrou_jusqu_a_sa_liberation_ou_au_delai() {
        let dir = tempfile::tempdir().unwrap();
        let p = partition("BTC-USD", Timeframe::M1);
        let premier = PartitionLock::acquire(dir.path(), &p).unwrap();
        // Délai trop court : le verrou est toujours tenu.
        let erreur =
            PartitionLock::acquire_wait(dir.path(), &p, Duration::from_millis(20)).unwrap_err();
        assert!(matches!(erreur, StoreError::LockBusy { .. }));
        // Un autre thread libère le verrou après 40 ms : l'attente aboutit.
        let libere = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            drop(premier);
        });
        let verrou = PartitionLock::acquire_wait(dir.path(), &p, Duration::from_secs(10));
        libere.join().unwrap();
        assert!(verrou.is_ok(), "{verrou:?}");
    }

    #[test]
    fn verrou_de_dataset_par_nom() {
        let dir = tempfile::tempdir().unwrap();
        let premier = DatasetLock::acquire(dir.path(), "ref").unwrap();
        assert_eq!(
            premier.path(),
            dir.path().join("state/locks/datasets/ref.lock")
        );
        let erreur = DatasetLock::acquire(dir.path(), "ref").unwrap_err();
        assert!(matches!(erreur, StoreError::DatasetBusy { .. }));
        let message = erreur.to_string();
        assert!(message.contains("« ref »"), "{message}");
        assert!(
            message.contains("state/locks/datasets/ref.lock"),
            "{message}"
        );
        // Un autre nom, et un verrou de partition, restent libres.
        let _autre = DatasetLock::acquire(dir.path(), "autre").unwrap();
        let _partition =
            PartitionLock::acquire(dir.path(), &partition("BTC-USD", Timeframe::M1)).unwrap();
        drop(premier);
        DatasetLock::acquire(dir.path(), "ref").unwrap();
    }

    #[test]
    fn partitions_differentes_independantes() {
        let dir = tempfile::tempdir().unwrap();
        let _a = PartitionLock::acquire(dir.path(), &partition("BTC-USD", Timeframe::M1)).unwrap();
        let _b = PartitionLock::acquire(dir.path(), &partition("BTC-USD", Timeframe::M5)).unwrap();
        let _c = PartitionLock::acquire(dir.path(), &partition("ETH-USD", Timeframe::M1)).unwrap();
    }

    #[test]
    fn fichier_de_verrou_sous_state_locks() {
        let dir = tempfile::tempdir().unwrap();
        let verrou =
            PartitionLock::acquire(dir.path(), &partition("BTC-USD", Timeframe::M1)).unwrap();
        assert_eq!(
            verrou.path(),
            dir.path()
                .join("state")
                .join("locks")
                .join("kraken")
                .join("BTC-USD")
                .join("1m.lock")
        );
        assert!(verrou.path().is_file());
        // Le fichier n'est pas supprimé à la libération.
        let chemin = verrou.path().to_path_buf();
        drop(verrou);
        assert!(chemin.is_file());
    }
}
