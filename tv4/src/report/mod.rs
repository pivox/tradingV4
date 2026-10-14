//! Rapports d'un run (docs/03-architecture.md §2). En S3 : `metrics.json`, écrit de façon atomique.
//! Les rapports complets (`trades.csv`, `equity.csv`, `manifest.json`, verdict) arrivent en S8.
//!
//! Le module lit les résultats de `engine` ; il ne connaît pas les stratégies (id, version et
//! paramètres lui arrivent dans `RunConfig`) et ne remonte pas vers la CLI.

#![forbid(unsafe_code)]

pub mod metrics;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::model::{ModelError, hash};

pub use metrics::{DataInfo, EngineInfo, Metrics};

/// Erreurs d'écriture d'un rapport.
#[derive(Debug, Error)]
pub enum ReportError {
    #[error("écriture de `{path}` impossible : {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Json(#[from] ModelError),

    #[error("aucune bougie : les bornes des données sont inconnues")]
    NoCandles,
}

/// Écrit `value` en JSON canonique (clés triées), suivi d'un `\n`, dans `dir/name`.
///
/// L'écriture est atomique : `dir/<name>.tmp` est écrit puis `sync_all`, et `fs::rename` l'installe
/// sous `name` en écrasant un fichier existant. Pas de fsync du dossier. Rend le chemin final.
pub fn write_json_atomic<T: Serialize>(
    dir: &Path,
    name: &str,
    value: &T,
) -> Result<PathBuf, ReportError> {
    let io = |path: &Path| {
        let path = path.display().to_string();
        move |source| ReportError::Io { path, source }
    };
    let mut text = hash::canonical_json(value)?;
    text.push('\n');
    fs::create_dir_all(dir).map_err(io(dir))?;
    let final_path = dir.join(name);
    let tmp_path = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp_path).map_err(io(&tmp_path))?;
    file.write_all(text.as_bytes()).map_err(io(&tmp_path))?;
    file.sync_all().map_err(io(&tmp_path))?;
    drop(file);
    fs::rename(&tmp_path, &final_path).map_err(io(&final_path))?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecriture_atomique_et_ecrasement() {
        let dir = std::env::temp_dir().join(format!("tv4-report-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let sous_dossier = dir.join("a").join("b");
        let valeur = serde_json::json!({"z": 1, "a": [1.5, 2.0]});
        let chemin = write_json_atomic(&sous_dossier, "x.json", &valeur).unwrap();
        assert_eq!(chemin, sous_dossier.join("x.json"));
        assert_eq!(
            fs::read_to_string(&chemin).unwrap(),
            "{\"a\":[1.5,2.0],\"z\":1}\n"
        );
        assert!(!sous_dossier.join("x.json.tmp").exists());
        write_json_atomic(&sous_dossier, "x.json", &serde_json::json!({"k": 2})).unwrap();
        assert_eq!(fs::read_to_string(&chemin).unwrap(), "{\"k\":2}\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn un_nan_est_refuse() {
        let dir = std::env::temp_dir().join(format!("tv4-report-nan-{}", std::process::id()));
        let erreur = write_json_atomic(&dir, "x.json", &vec![f64::NAN]).unwrap_err();
        assert!(matches!(erreur, ReportError::Json(_)));
        assert!(!dir.join("x.json").exists());
    }
}
