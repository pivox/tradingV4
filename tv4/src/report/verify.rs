//! `tv4 verify` : comparaison de deux dossiers de run, à l'octet (docs/03-architecture.md §7.4,
//! spécification S8 §9).
//!
//! Le module lit les fichiers d'un run (`manifest.json`, `metrics.json`, `trades.csv`, `equity.csv`) et
//! rend un compte rendu en lignes de texte ; il ne contient aucune logique métier. La comparaison
//! décisive est celle des octets : les `serde_json::Value` ne servent qu'à nommer les chemins en écart
//! quand deux fichiers JSON diffèrent (sans la fonctionnalité `float_roundtrip` de `serde_json`, une
//! relecture peut déplacer un flottant d'un ULP, ce qui ne change jamais le verdict).
//!
//! Étapes :
//!
//! 1. lecture des quatre fichiers de chaque dossier (absent, illisible, JSON invalide ou schéma
//!    inconnu : [`VerifyError`]) ;
//! 2. cohérence interne de chaque run : le BLAKE3 de chaque fichier égale `outputs.files` du manifeste
//!    et `result_hash` égale celui de `trades.csv` (un run altéré n'est pas comparé) ;
//! 3. comparaison à l'octet de `metrics.json`, `trades.csv` et `equity.csv` ;
//! 4. informations du manifeste (configuration, données, moteur), qui n'influent pas sur le verdict.
//!
//! `timings`, `started_at`, `run_id` et `outputs.equity_step` du manifeste sont ignorés.

use std::fs;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::model::hash;

use super::equity::EQUITY_FILE;
use super::writers::TRADES_FILE;
use super::{manifest, metrics};

/// Nom du fichier de métriques d'un run.
const METRICS_FILE: &str = "metrics.json";
/// Nom du manifeste d'un run.
const MANIFEST_FILE: &str = "manifest.json";
/// Les trois fichiers comparés à l'octet, dans l'ordre du compte rendu.
const COMPARED_FILES: [&str; 3] = [METRICS_FILE, TRADES_FILE, EQUITY_FILE];
/// Clés de `engine` listées comme information quand elles diffèrent.
const ENGINE_KEYS: [&str; 6] = [
    "git_sha",
    "binary_hash",
    "rustc",
    "target",
    "target_cpu",
    "profile",
];
/// Nombre maximal de chemins JSON en écart affichés par fichier.
const MAX_JSON_PATHS: usize = 20;
/// Longueur maximale (en caractères) d'une ligne CSV citée dans le compte rendu.
const MAX_LINE_CHARS: usize = 200;

/// Erreurs de `tv4 verify` : un run illisible ou incohérent n'est pas comparable.
#[derive(Debug, Error)]
pub enum VerifyError {
    /// Un fichier du run est absent ou illisible.
    #[error("run {label} (`{dir}`) : lecture de `{file}` impossible : {source}")]
    Read {
        label: &'static str,
        dir: String,
        file: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// Un fichier JSON du run n'est pas du JSON valide.
    #[error("run {label} (`{dir}`) : `{file}` n'est pas du JSON valide : {source}")]
    Json {
        label: &'static str,
        dir: String,
        file: &'static str,
        #[source]
        source: serde_json::Error,
    },

    /// Le champ `schema` d'un fichier JSON n'est pas celui que ce binaire sait lire.
    #[error("run {label} (`{dir}`) : `{file}` : schéma inconnu {found} (attendu `{expected}`)")]
    Schema {
        label: &'static str,
        dir: String,
        file: &'static str,
        found: String,
        expected: &'static str,
    },

    /// Un champ obligatoire du manifeste manque ou n'a pas le bon type.
    #[error("run {label} (`{dir}`) : manifest.json : champ `{path}` absent ou mal typé")]
    Manifest {
        label: &'static str,
        dir: String,
        path: String,
    },

    /// Un fichier du run ne correspond pas au hash que son manifeste annonce.
    #[error(
        "run {label} incohérent : {file} ne correspond pas à son manifeste (dossier `{dir}` : {found} lu, {expected} annoncé)"
    )]
    Inconsistent {
        label: &'static str,
        dir: String,
        file: &'static str,
        expected: String,
        found: String,
    },
}

/// Compte rendu d'une comparaison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    /// Lignes du compte rendu, dernière ligne (verdict) comprise.
    pub lines: Vec<String>,
    /// Nombre de fichiers (parmi les trois comparés) qui diffèrent.
    pub different_files: usize,
}

impl VerifyReport {
    /// Vrai si les trois fichiers sont identiques à l'octet.
    pub fn identical(&self) -> bool {
        self.different_files == 0
    }
}

/// Un run lu et vérifié en interne.
struct LoadedRun {
    manifest: Value,
    metrics_bytes: Vec<u8>,
    metrics: Value,
    trades_bytes: Vec<u8>,
    equity_bytes: Vec<u8>,
}

impl LoadedRun {
    /// Octets du fichier nommé (un des trois comparés).
    fn bytes(&self, file: &str) -> &[u8] {
        match file {
            METRICS_FILE => &self.metrics_bytes,
            TRADES_FILE => &self.trades_bytes,
            _ => &self.equity_bytes,
        }
    }
}

/// Compare les runs `dir_a` et `dir_b`.
///
/// Rend [`VerifyError`] si l'un des deux est illisible ou incohérent avec son manifeste ; sinon un
/// compte rendu dont [`VerifyReport::identical`] donne le verdict.
pub fn verify(dir_a: &Path, dir_b: &Path) -> Result<VerifyReport, VerifyError> {
    let a = load_run("A", dir_a)?;
    let b = load_run("B", dir_b)?;

    let mut lines = vec![
        format!("run A : {}", dir_a.display()),
        format!("run B : {}", dir_b.display()),
    ];
    let mut different_files = 0usize;
    for file in COMPARED_FILES {
        let (bytes_a, bytes_b) = (a.bytes(file), b.bytes(file));
        if bytes_a == bytes_b {
            lines.push(format!(
                "{file} : identique ({})",
                hash::blake3_prefixed(bytes_a)
            ));
            continue;
        }
        different_files += 1;
        lines.push(format!("{file} : DIFFÉRENT"));
        if file == METRICS_FILE {
            describe_json_diff(&a.metrics, &b.metrics, &mut lines);
        } else {
            describe_csv_diff(bytes_a, bytes_b, &mut lines);
        }
    }
    describe_manifest_info(&a.manifest, &b.manifest, &mut lines);

    if different_files == 0 {
        let result_hash = string_at(&a.manifest, &["result_hash"]).unwrap_or_default();
        lines.push(format!(
            "verdict : identiques à l'octet (result_hash {result_hash})"
        ));
    } else {
        lines.push(format!(
            "verdict : DIFFÉRENTS ({different_files} fichier(s))"
        ));
    }
    Ok(VerifyReport {
        lines,
        different_files,
    })
}

/// Lit les quatre fichiers d'un run et contrôle sa cohérence interne.
fn load_run(label: &'static str, dir: &Path) -> Result<LoadedRun, VerifyError> {
    let shown = dir.display().to_string();
    let read = |file: &'static str| -> Result<Vec<u8>, VerifyError> {
        fs::read(dir.join(file)).map_err(|source| VerifyError::Read {
            label,
            dir: shown.clone(),
            file,
            source,
        })
    };
    let parse =
        |file: &'static str, bytes: &[u8], expected: &'static str| -> Result<Value, VerifyError> {
            let value: Value =
                serde_json::from_slice(bytes).map_err(|source| VerifyError::Json {
                    label,
                    dir: shown.clone(),
                    file,
                    source,
                })?;
            let found = value.get("schema").and_then(Value::as_str);
            if found != Some(expected) {
                return Err(VerifyError::Schema {
                    label,
                    dir: shown.clone(),
                    file,
                    found: found.map_or_else(|| "absent".to_string(), |s| format!("`{s}`")),
                    expected,
                });
            }
            Ok(value)
        };

    let manifest_bytes = read(MANIFEST_FILE)?;
    let manifest = parse(MANIFEST_FILE, &manifest_bytes, manifest::SCHEMA)?;
    let metrics_bytes = read(METRICS_FILE)?;
    let metrics = parse(METRICS_FILE, &metrics_bytes, metrics::SCHEMA)?;
    let trades_bytes = read(TRADES_FILE)?;
    let equity_bytes = read(EQUITY_FILE)?;

    let missing = |path: &str| VerifyError::Manifest {
        label,
        dir: shown.clone(),
        path: path.to_string(),
    };
    let inconsistent =
        |file: &'static str, expected: &str, found: &str| VerifyError::Inconsistent {
            label,
            dir: shown.clone(),
            file,
            expected: expected.to_string(),
            found: found.to_string(),
        };
    for (file, bytes) in [
        (METRICS_FILE, &metrics_bytes),
        (TRADES_FILE, &trades_bytes),
        (EQUITY_FILE, &equity_bytes),
    ] {
        let expected = string_at(&manifest, &["outputs", "files", file])
            .ok_or_else(|| missing(&format!("outputs.files.{file}")))?;
        let found = hash::blake3_prefixed(bytes);
        if found != expected {
            return Err(inconsistent(file, &expected, &found));
        }
    }
    let result_hash =
        string_at(&manifest, &["result_hash"]).ok_or_else(|| missing("result_hash"))?;
    let trades_hash = hash::blake3_prefixed(&trades_bytes);
    if result_hash != trades_hash {
        return Err(inconsistent("result_hash", &result_hash, &trades_hash));
    }
    Ok(LoadedRun {
        manifest,
        metrics_bytes,
        metrics,
        trades_bytes,
        equity_bytes,
    })
}

/// Chaîne située au chemin `path` d'un objet JSON, si elle existe.
fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(str::to_string)
}

/// Information sur les manifestes : configuration, données et moteur. Aucun effet sur le verdict.
fn describe_manifest_info(a: &Value, b: &Value, lines: &mut Vec<String>) {
    let differs = |key: &str| string_at(a, &[key]) != string_at(b, &[key]);
    if differs("config_hash") || differs("dataset_hash") {
        lines.push(
            "attention : configurations (ou données) différentes, ces runs ne sont pas censés être identiques"
                .to_string(),
        );
        for key in ["config_hash", "dataset_hash"] {
            if differs(key) {
                lines.push(format!(
                    "  {key} : {} contre {}",
                    shown(a, &[key]),
                    shown(b, &[key])
                ));
            }
        }
    }
    let mut engine_lines = Vec::new();
    for key in ENGINE_KEYS {
        let path = ["engine", key];
        if string_at(a, &path) != string_at(b, &path) {
            engine_lines.push(format!(
                "  engine.{key} : {} contre {}",
                shown(a, &path),
                shown(b, &path)
            ));
        }
    }
    if !engine_lines.is_empty() {
        lines.push(
            "information : moteurs différents (comparer deux builds est légitime)".to_string(),
        );
        lines.append(&mut engine_lines);
    }
}

/// Valeur textuelle d'un chemin du manifeste, `(absent)` sinon.
fn shown(value: &Value, path: &[&str]) -> String {
    string_at(value, path).unwrap_or_else(|| "(absent)".to_string())
}

/// Chemins JSON en écart entre deux fichiers (au plus [`MAX_JSON_PATHS`] affichés).
fn describe_json_diff(a: &Value, b: &Value, lines: &mut Vec<String>) {
    let mut paths = Vec::new();
    diff_paths(a, b, &mut String::new(), &mut paths);
    if paths.is_empty() {
        lines.push(
            "  aucun chemin en écart après relecture : seule la mise en forme ou un flottant à 1 ULP de relecture diffère"
                .to_string(),
        );
        return;
    }
    lines.push(format!("  chemins en écart : {}", paths.len()));
    for path in paths.iter().take(MAX_JSON_PATHS) {
        lines.push(format!("    {path}"));
    }
    if paths.len() > MAX_JSON_PATHS {
        lines.push(format!("    … et {} autres", paths.len() - MAX_JSON_PATHS));
    }
}

/// Parcourt `a` et `b` en parallèle et note le chemin de chaque valeur qui diffère (`max_drawdown.m1.pct`,
/// `distribution.by_year[0].pnl_net`).
fn diff_paths(a: &Value, b: &Value, path: &mut String, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(map_a), Value::Object(map_b)) => {
            // Les deux cartes sont triées par clé (pas de `preserve_order`) : union ordonnée.
            let mut keys: Vec<&String> = map_a.keys().chain(map_b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let saved = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
                match (map_a.get(key), map_b.get(key)) {
                    (Some(x), Some(y)) => diff_paths(x, y, path, out),
                    _ => out.push(path.clone()),
                }
                path.truncate(saved);
            }
        }
        (Value::Array(list_a), Value::Array(list_b)) => {
            for index in 0..list_a.len().max(list_b.len()) {
                let saved = path.len();
                path.push_str(&format!("[{index}]"));
                match (list_a.get(index), list_b.get(index)) {
                    (Some(x), Some(y)) => diff_paths(x, y, path, out),
                    _ => out.push(path.clone()),
                }
                path.truncate(saved);
            }
        }
        _ => {
            if a != b {
                out.push(if path.is_empty() {
                    "(racine)".to_string()
                } else {
                    path.clone()
                });
            }
        }
    }
}

/// Lignes (sans le `\n` final) d'un fichier texte.
fn text_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|&byte| byte == b'\n').collect();
    if bytes.last() == Some(&b'\n') {
        lines.pop();
    }
    lines
}

/// Une ligne citée dans le compte rendu : au plus [`MAX_LINE_CHARS`] caractères.
fn quote_line(line: Option<&&[u8]>) -> String {
    match line {
        Some(bytes) => String::from_utf8_lossy(bytes)
            .chars()
            .take(MAX_LINE_CHARS)
            .collect(),
        None => "(absente)".to_string(),
    }
}

/// Nombres de lignes, première ligne en écart (numérotée à partir de 1) et les deux lignes.
fn describe_csv_diff(a: &[u8], b: &[u8], lines: &mut Vec<String>) {
    let (lines_a, lines_b) = (text_lines(a), text_lines(b));
    lines.push(format!(
        "  lignes : {} contre {}",
        lines_a.len(),
        lines_b.len()
    ));
    let first = (0..lines_a.len().max(lines_b.len())).find(|&i| lines_a.get(i) != lines_b.get(i));
    match first {
        Some(index) => {
            lines.push(format!("  première ligne différente : {}", index + 1));
            lines.push(format!("    A : {}", quote_line(lines_a.get(index))));
            lines.push(format!("    B : {}", quote_line(lines_b.get(index))));
        }
        None => lines.push("  seule la fin de fichier diffère (octet final)".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(a: &Value, b: &Value) -> Vec<String> {
        let mut out = Vec::new();
        diff_paths(a, b, &mut String::new(), &mut out);
        out
    }

    #[test]
    fn les_chemins_json_en_ecart_sont_nommes() {
        let a = json!({"max_drawdown": {"m1": {"pct": 0.1, "quote": 3.0}}, "n": 1, "l": [1, 2, 3]});
        let b = json!({"max_drawdown": {"m1": {"pct": 0.2, "quote": 3.0}}, "n": 1, "l": [1, 9]});
        assert_eq!(paths(&a, &b), ["l[1]", "l[2]", "max_drawdown.m1.pct"]);
    }

    #[test]
    fn une_cle_presente_d_un_seul_cote_est_un_ecart() {
        let a = json!({"a": 1, "b": {"c": 2}});
        let b = json!({"a": 1, "b": {}});
        assert_eq!(paths(&a, &b), ["b.c"]);
        assert_eq!(paths(&json!(1), &json!(2)), ["(racine)"]);
        assert!(paths(&a, &a).is_empty());
    }

    #[test]
    fn plus_de_vingt_chemins_sont_tronques_avec_le_compte() {
        let a = Value::Array((0..25).map(|i| json!(i)).collect());
        let b = Value::Array((0..25).map(|i| json!(i + 1)).collect());
        let mut lines = Vec::new();
        describe_json_diff(&a, &b, &mut lines);
        assert_eq!(lines[0], "  chemins en écart : 25");
        assert_eq!(lines.len(), 1 + MAX_JSON_PATHS + 1);
        assert_eq!(lines.last().unwrap(), "    … et 5 autres");
    }

    #[test]
    fn la_premiere_ligne_csv_differente_est_citee() {
        let mut lines = Vec::new();
        describe_csv_diff(b"h\n1,2\n3,4\n", b"h\n1,2\n3,5\n", &mut lines);
        assert_eq!(lines[0], "  lignes : 3 contre 3");
        assert_eq!(lines[1], "  première ligne différente : 3");
        assert_eq!(lines[2], "    A : 3,4");
        assert_eq!(lines[3], "    B : 3,5");
    }

    #[test]
    fn un_csv_plus_court_ou_sans_retour_final_est_decrit() {
        let mut lines = Vec::new();
        describe_csv_diff(b"h\n1\n", b"h\n", &mut lines);
        assert_eq!(lines[1], "  première ligne différente : 2");
        assert_eq!(lines[3], "    B : (absente)");

        let mut lines = Vec::new();
        describe_csv_diff(b"h\n1\n", b"h\n1", &mut lines);
        assert_eq!(lines[1], "  seule la fin de fichier diffère (octet final)");
    }

    #[test]
    fn une_ligne_citee_est_coupee_a_deux_cents_caracteres() {
        let longue = "é".repeat(500);
        let mut lines = Vec::new();
        describe_csv_diff(longue.as_bytes(), b"x", &mut lines);
        assert_eq!(
            lines[2].chars().count(),
            "    A : ".chars().count() + MAX_LINE_CHARS
        );
    }

    #[test]
    fn l_information_du_manifeste_signale_config_et_moteur() {
        let a = json!({"config_hash": "aa", "dataset_hash": "d", "engine": {"git_sha": "x", "rustc": "1"}});
        let b = json!({"config_hash": "bb", "dataset_hash": "d", "engine": {"git_sha": "y", "rustc": "1"}});
        let mut lines = Vec::new();
        describe_manifest_info(&a, &b, &mut lines);
        assert!(
            lines[0].starts_with("attention : configurations"),
            "{lines:?}"
        );
        assert_eq!(lines[1], "  config_hash : aa contre bb");
        assert!(lines.iter().any(|l| l == "  engine.git_sha : x contre y"));
        assert!(!lines.iter().any(|l| l.contains("dataset_hash")));
        let mut rien = Vec::new();
        describe_manifest_info(&a, &a, &mut rien);
        assert!(rien.is_empty());
    }
}
