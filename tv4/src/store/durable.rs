//! Écriture durable d'un fichier (docs/03 §5.1) : `.tmp`, `sync_all`, `rename`, `sync_all` du dossier.
//!
//! Un lecteur ne voit jamais un fichier à moitié écrit : le contenu est d'abord complet sur disque
//! sous `<nom>.tmp`, puis le `rename` (atomique sur un même système de fichiers) le publie, puis le
//! `sync_all` du dossier rend le `rename` lui-même durable.
//!
//! Limite assumée : les dossiers créés par `create_dir_all` ne sont pas fsyncés un à un, seul le
//! dossier du fichier l'est, comme le demande docs/03 §5.1. Linux seulement : ouvrir un dossier en
//! lecture pour le fsyncer n'est pas portable.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::StoreError;

/// Erreur d'E/S qui nomme `path`.
pub(crate) fn io_error(path: &Path, source: std::io::Error) -> StoreError {
    StoreError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// `sync_all` d'un dossier (rend durable un `rename` ou une création de fichier dedans).
pub(crate) fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|source| io_error(dir, source))
}

/// Nom du fichier temporaire : `<nom>.tmp` dans le même dossier.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Écrit `path` de façon durable : `<nom>.tmp` dans le même dossier (créé au besoin),
/// `write(&mut file)`, `File::sync_all`, `fs::rename` vers `path`, puis `sync_all` du dossier.
///
/// En cas d'erreur avant le `rename`, le `.tmp` est supprimé (au mieux) et l'erreur rendue. Un `.tmp`
/// orphelin d'un crash précédent est écrasé (`File::create` tronque).
pub(crate) fn write_durable(
    path: &Path,
    write: impl FnOnce(&mut File) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    // Un nom nu (`okx.json`) a pour parent `Some("")`, pas `None` : le dossier est alors le dossier
    // courant, et `""` ne peut ni être créé ni être synchronisé.
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir).map_err(|source| io_error(dir, source))?;
    let tmp = tmp_path(path);
    let ecrit = (|| {
        let mut file = File::create(&tmp).map_err(|source| io_error(&tmp, source))?;
        write(&mut file)?;
        file.sync_all().map_err(|source| io_error(&tmp, source))?;
        fs::rename(&tmp, path).map_err(|source| io_error(path, source))
    })();
    if let Err(erreur) = ecrit {
        // Au mieux : le `.tmp` peut ne pas exister, et l'erreur d'origine prime.
        let _ = fs::remove_file(&tmp);
        return Err(erreur);
    }
    sync_dir(dir)
}

/// [`write_durable`] pour un contenu déjà en mémoire.
pub(crate) fn write_bytes_durable(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    write_durable(path, |file| {
        file.write_all(bytes)
            .map_err(|source| io_error(path, source))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fichiers(dir: &Path) -> Vec<String> {
        let mut noms: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        noms.sort();
        noms
    }

    #[test]
    fn ecriture_durable_remplace_sans_laisser_de_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sous").join("dossier").join("x.json");
        write_bytes_durable(&path, b"un").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"un");
        write_bytes_durable(&path, b"deux").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"deux");
        assert_eq!(fichiers(path.parent().unwrap()), vec!["x.json"]);
    }

    #[test]
    fn tmp_orphelin_ecrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        fs::write(
            dir.path().join("x.json.tmp"),
            b"reste d'un crash, plus long que le contenu",
        )
        .unwrap();
        write_bytes_durable(&path, b"ok").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"ok");
        assert_eq!(fichiers(dir.path()), vec!["x.json"]);
    }

    #[test]
    fn erreur_d_ecriture_supprime_le_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        write_bytes_durable(&path, b"ancien").unwrap();
        let resultat = write_durable(&path, |file| {
            file.write_all(b"moitie").unwrap();
            Err(StoreError::EmptyInput)
        });
        assert!(matches!(resultat, Err(StoreError::EmptyInput)));
        // L'ancien contenu est intact et aucun `.tmp` ne traîne.
        assert_eq!(fs::read(&path).unwrap(), b"ancien");
        assert_eq!(fichiers(dir.path()), vec!["x.json"]);
    }
}
