//! Rapports d'un run (docs/03-architecture.md §2). Un run écrit quatre fichiers de façon atomique :
//! `metrics.json` ([`metrics`]), `trades.csv` ([`writers`]), `equity.csv` ([`equity`]) puis, en
//! dernier, `manifest.json` ([`manifest`]) dont la présence marque un run complet. [`write_json_atomic`]
//! sert aussi aux rapports d'ingestion de `tv4 ingest kraken` (S5).
//!
//! Le module lit les résultats de `engine` ; il ne connaît pas les stratégies (id, version et
//! paramètres lui arrivent dans `RunConfig`) et ne remonte pas vers la CLI. Les sommes et moyennes sont
//! des boucles `for` dans l'ordre des trades (docs/03 §7.4) ; la libm n'est appelée que dans
//! [`approx`].
//!
//! Le mode portefeuille de S9 ajoute [`metrics_s9`] (`tv4-metrics-s9`), [`executability`] (G0 et G1) et
//! la variante `tv4-manifest-s9` de [`manifest`] ; `trades.csv` et `equity.csv` gardent le format de S8
//! (la colonne `symbol` porte la paire de chaque trade). Le chemin de S8 n'est pas touché.
//!
//! La génération s11 ([`research`]) ajoute le bloc `research` de `metrics.json` (`tv4-metrics-s11`), la
//! purge des trades ouverts à la fin de la fenêtre de décision et `lines.json` (`tv4-lines-v1`), qui résume
//! les deux lignes d'un rapport de validation ou de holdout.
//!
//! Écarts d'implémentation (S8) à docs/03 :
//!
//! 1. `manifest.json` est défini ici et non dans `engine/manifest.rs` (§4) : `engine` n'écrit rien et
//!    ne connaît ni l'horloge ni le binaire.
//! 2. La clôture forcée qui suit la dernière 1m n'entre pas dans `equity.csv` ni dans les drawdowns :
//!    `equity_final` reste dans `metrics.json` à part.
//! 3. `equity.csv` d'un run de 2 ans compte au plus 17 520 lignes de données au pas `1h` (moins avec le
//!    warmup et les heures sans transaction).

#![forbid(unsafe_code)]

pub mod approx;
pub mod cost_arithmetic;
pub mod distribution;
pub mod drawdown;
pub mod equity;
pub mod executability;
pub mod manifest;
pub mod metrics;
pub mod metrics_s9;
pub mod profiles;
pub mod research;
pub mod verify;
pub mod writers;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::model::{ModelError, hash};

pub use equity::write_equity_csv;
pub use manifest::{
    EngineInfo, Manifest, ManifestS9, ManifestS10Inputs, profiles_id, run_id, run_id_s9, run_id_s11,
};
pub use metrics::{DataInfo, DatasetRef, Metrics, RunVerdict};
pub use metrics_s9::MetricsS9;
pub use writers::{TradesContext, write_trades_csv};

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

    #[error("production du CSV impossible : {0}")]
    Csv(String),

    #[error("lecture d'un CSV de rapport impossible : {0}")]
    CsvRead(String),

    /// L'entonnoir ne boucle pas : un compteur est incohérent avec les autres (bug du moteur).
    #[error("entonnoir incohérent : {detail}")]
    FunnelInconsistent { detail: String },

    #[error("instant {ts_ms} ms hors de la plage des dates")]
    Timestamp { ts_ms: i64 },

    #[error("débordement d'entier dans {context}")]
    Overflow { context: &'static str },

    #[error("série incohérente : {detail}")]
    Series { detail: String },
}

/// Octets de `value` en JSON canonique (clés triées), suivi d'un `\n` : ce que
/// [`write_json_atomic`] écrit.
pub fn json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, ReportError> {
    let mut text = hash::canonical_json(value)?;
    text.push('\n');
    Ok(text.into_bytes())
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
    write_bytes_atomic(dir, name, &json_bytes(value)?)
}

/// Écrit `bytes` dans `dir/name` de façon atomique (même schéma que [`write_json_atomic`]).
pub(crate) fn write_bytes_atomic(
    dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf, ReportError> {
    let io = |path: &Path| {
        let path = path.display().to_string();
        move |source| ReportError::Io { path, source }
    };
    fs::create_dir_all(dir).map_err(io(dir))?;
    let final_path = dir.join(name);
    let tmp_path = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp_path).map_err(io(&tmp_path))?;
    file.write_all(bytes).map_err(io(&tmp_path))?;
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
