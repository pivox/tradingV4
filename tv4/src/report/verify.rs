//! `tv4 verify` : comparaison de deux dossiers de run, à l'octet (docs/03-architecture.md §7.4,
//! docs/specs/s8.md §9).
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
//!
//! # Mode portefeuille (S9, docs S9 §14.2)
//!
//! Le schéma du manifeste (`tv4-manifest-s8` ou `tv4-manifest-s9`) décide de la famille du run, et
//! `metrics.json` doit être de la même famille. Deux runs de familles différentes donnent
//! [`VerifyError::SchemaMismatch`] (code 1, « schémas différents »). Pour deux runs S9, `verify` fait en
//! plus :
//!
//! * par run, avant toute comparaison : capital identique dans `metrics.json` et le manifeste,
//!   `instruments_hash` identique dans les deux, Σ de `funnel_by_pair` égale à `funnel` (compteurs et
//!   refus), identités de l'entonnoir de docs S9 §9.1, et chaque `symbol` de `trades.csv` dans
//!   `data.symbols` avec autant de lignes que `funnel_by_pair[].trades` ([`VerifyError::Portfolio`]).
//!   Les refus au fill d'une paire sont lus dans `sim_by_pair[].counters`, ce qui donne, pour chaque
//!   paire, l'identité 1 exacte (`entry_signals = entry_orders + refus à la soumission`, la soumission
//!   étant `rejected` moins les refus au fill) et l'identité 2 en inégalité (`entry_orders ≥
//!   entry_fills + refus au fill`) ; le reste des ordres, sommé sur les paires, égale exactement
//!   `funnel.entry_not_filled_by_reason.other` (expirations et annulations, non publiées par paire) ;
//! * entre les deux runs : `initial_cash` égal. Le capital n'est pas dans le `config_hash` du
//!   portefeuille : deux runs de capitaux différents ont le même hash et ne sont jamais déclarés
//!   identiques ([`VerifyError::CapitalMismatch`], code 1).
//!
//! # Profils de coûts (S10, docs S10 §12.5)
//!
//! Un manifeste `tv4-manifest-s10` forme la troisième famille : mêmes contrôles que S9, plus
//! `metrics.costs.profile` égal au `profile` du manifeste et mêmes hashes de fichiers de coûts dans les
//! deux. s8, s9 et s10 ne se comparent pas entre eux ([`VerifyError::SchemaMismatch`], code 1).
//!
//! Un dossier qui contient `profiles.json` est un **dossier de profils** : `verify` en compare deux, ou
//! aucun. Un dossier de profils contre un dossier de run, ou l'inverse, est refusé
//! ([`VerifyError::LayoutMismatch`], code 1). `profiles.json` est relu (schéma, trois profils nommés
//! baseline, adverse et maker, un `dir` simple pour chacun), chaque sous-dossier est vérifié comme un run
//! s10 (ou s11, voir plus bas) et confronté à `profiles.json` (`run_id`, `config_hash`, `result_hash`,
//! `profiles_id`), puis comparé à son homologue. `profiles.json` est comparé à l'octet. Un sous-dossier ou
//! un manifeste absent donne [`VerifyError::ProfilesIncomplete`] (code 1).
//!
//! # Recherche (S11, docs/specs/s11.md §9.6)
//!
//! Un manifeste `tv4-manifest-s11` forme la quatrième famille : un run de recherche, avec ou sans profil
//! de coûts. Mêmes contrôles que s9 (et que s10 quand le manifeste porte un `profile`), plus :
//!
//! * l'entonnoir tient compte des refus d'entrée de la recherche (`research.legs` et `research.funnel`) ;
//! * `trades.csv` garde **tous** les trades : les trades purgés (`exit_type = end_of_window`) sont comptés
//!   à part, les autres égalent `funnel_by_pair[].trades` et `research.purge.trades_kept` (`n_trades`) ;
//! * `research.identity_hash` est recalculé depuis `research.identity`, et `run_id` du manifeste depuis
//!   `config_hash`, `dataset_hash`, la fenêtre, le capital et `identity_hash` (`run_id_s11`) ;
//! * `research.block` et `research.decision_window` sont ceux de `data.block` et `data.decision_window`,
//!   dans `metrics.json` comme dans le manifeste.
//!
//! s11 contre s8, s9 ou s10 donne [`VerifyError::SchemaMismatch`] (code 1). Un dossier de profils s11
//! contre un dossier de profils s10 aussi : les trois profils d'un dossier sont de la même génération
//! ([`VerifyError::Profiles`], « pas un run de la même génération »).
//!
//! Un dossier qui contient `lines.json` est un **dossier de lignes** (`full/` et `okx_usdc/`, chacun un run
//! ou un dossier de profils s11) : `verify` en compare deux, ou aucun ([`VerifyError::LayoutMismatch`]
//! sinon). `lines.json` est relu (schéma, deux lignes `full` puis `okx_usdc`, `run_id` ou `profiles_id` de
//! chacune égal à celui du dossier, `config_hash` de chaque profil, `lines_id` recalculé), chaque ligne est
//! comparée à son homologue comme un run ou un dossier de profils, puis `lines.json` est comparé à
//! l'octet. Code 3 au premier écart, avec les chemins en écart.

use std::fs;
use std::path::{Component, Path};

use serde_json::Value;
use thiserror::Error;

use crate::model::hash;

use super::equity::EQUITY_FILE;
use super::manifest::{profiles_id, run_id_s11};
use super::profiles::{PROFILES_FILE, PROFILES_SCHEMA};
use super::research::{LINES_FILE, LINES_SCHEMA, METRICS_BASIS};
use super::writers::TRADES_FILE;
use super::{manifest, metrics, metrics_s9};

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
    #[error("run {label} (`{dir}`) : `{file}` : schéma inconnu {found} (attendu {expected})")]
    Schema {
        label: &'static str,
        dir: String,
        file: &'static str,
        found: String,
        expected: &'static str,
    },

    /// Deux runs de familles différentes (s8, s9, s10, s11) : ils ne se comparent pas.
    #[error(
        "schémas différents : run A `{a}`, run B `{b}` ; comparer deux runs de la même famille"
    )]
    SchemaMismatch { a: String, b: String },

    /// Un dossier de profils (`profiles.json`) ou de lignes (`lines.json`) contre un dossier d'une autre
    /// nature : ils ne se comparent pas.
    #[error("{rule} : `{a}` est {a_kind}, `{b}` est {b_kind}")]
    LayoutMismatch {
        /// La règle violée, au singulier de la nature du dossier spécial.
        rule: &'static str,
        a: String,
        a_kind: &'static str,
        b: String,
        b_kind: &'static str,
    },

    /// Un dossier de lignes est incomplet (sous-dossier ou manifeste absent) ou son `lines.json` ne
    /// décrit pas deux lignes lisibles.
    #[error("dossier de lignes {label} (`{dir}`) incomplet : {detail}")]
    LinesIncomplete {
        label: &'static str,
        dir: String,
        detail: String,
    },

    /// `lines.json` ne correspond pas aux runs qu'il résume, ou une ligne n'est pas un run s11.
    #[error("dossier de lignes {label} (`{dir}`) incohérent : {detail}")]
    Lines {
        label: &'static str,
        dir: String,
        detail: String,
    },

    /// Un dossier de profils est incomplet (sous-dossier ou manifeste absent) ou son `profiles.json` ne
    /// décrit pas trois profils lisibles.
    #[error("dossier de profils {label} (`{dir}`) incomplet : {detail}")]
    ProfilesIncomplete {
        label: &'static str,
        dir: String,
        detail: String,
    },

    /// `profiles.json` ne correspond pas aux runs qu'il résume, ou un profil n'est pas un run de la même
    /// génération (s10 ou s11) que les autres.
    #[error("dossier de profils {label} (`{dir}`) incohérent : {detail}")]
    Profiles {
        label: &'static str,
        dir: String,
        detail: String,
    },

    /// Deux runs S9 de capitaux initiaux différents : le capital n'est pas dans le `config_hash`, ils
    /// ne sont jamais déclarés identiques.
    #[error(
        "capitaux initiaux différents : run A {a}, run B {b} ; ces runs ne sont pas comparables"
    )]
    CapitalMismatch { a: f64, b: f64 },

    /// Un run S9 est incohérent avec lui-même (entonnoir, symboles de `trades.csv`, capital,
    /// `instruments_hash`).
    #[error("run {label} (`{dir}`) incohérent : {detail}")]
    Portfolio {
        label: &'static str,
        dir: String,
        detail: String,
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
    /// Nombre de fichiers comparés qui diffèrent : parmi les trois d'un run, ou, pour un dossier de
    /// profils, la somme sur les trois profils plus `profiles.json`.
    pub different_files: usize,
}

impl VerifyReport {
    /// Vrai si tous les fichiers comparés sont identiques à l'octet.
    pub fn identical(&self) -> bool {
        self.different_files == 0
    }
}

/// Famille d'un run : le schéma de son manifeste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    S8,
    S9,
    S10,
    S11,
}

impl Family {
    fn of(schema: Option<&str>) -> Option<Family> {
        match schema {
            Some(manifest::SCHEMA) => Some(Family::S8),
            Some(manifest::SCHEMA_S9) => Some(Family::S9),
            Some(manifest::SCHEMA_S10) => Some(Family::S10),
            Some(manifest::SCHEMA_S11) => Some(Family::S11),
            _ => None,
        }
    }

    fn metrics_schema(self) -> &'static str {
        match self {
            Family::S8 => metrics::SCHEMA,
            Family::S9 => metrics_s9::SCHEMA,
            Family::S10 => metrics_s9::SCHEMA_S10,
            Family::S11 => metrics_s9::SCHEMA_S11,
        }
    }

    /// Vrai pour le mode portefeuille (s9, s10 et s11) : mêmes contrôles d'identités.
    fn is_portfolio(self) -> bool {
        matches!(self, Family::S9 | Family::S10 | Family::S11)
    }

    /// Vrai pour les générations qui peuvent former un dossier de profils (s10 et s11).
    fn can_be_profile(self) -> bool {
        matches!(self, Family::S10 | Family::S11)
    }
}

/// Schémas de manifeste acceptés, pour les messages d'erreur.
const MANIFEST_SCHEMAS: &str =
    "`tv4-manifest-s8`, `tv4-manifest-s9`, `tv4-manifest-s10` ou `tv4-manifest-s11`";

/// Un run lu et vérifié en interne.
struct LoadedRun {
    family: Family,
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

/// Résultat de la comparaison de deux runs chargés : lignes de compte rendu et fichiers différents.
struct Comparison {
    lines: Vec<String>,
    different_files: usize,
}

/// Compare deux runs chargés : famille, capital, puis les trois fichiers à l'octet et l'information du
/// manifeste. Ne produit ni en-tête ni verdict.
fn compare_runs(a: &LoadedRun, b: &LoadedRun) -> Result<Comparison, VerifyError> {
    if a.family != b.family {
        return Err(VerifyError::SchemaMismatch {
            a: string_at(&a.manifest, &["schema"]).unwrap_or_default(),
            b: string_at(&b.manifest, &["schema"]).unwrap_or_default(),
        });
    }
    if a.family.is_portfolio() {
        let (cash_a, cash_b) = (
            number_at(&a.manifest, &["initial_cash"]).unwrap_or(f64::NAN),
            number_at(&b.manifest, &["initial_cash"]).unwrap_or(f64::NAN),
        );
        if cash_a.to_bits() != cash_b.to_bits() {
            return Err(VerifyError::CapitalMismatch {
                a: cash_a,
                b: cash_b,
            });
        }
    }

    let mut lines = Vec::new();
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
    describe_research_info(&a.metrics, &b.metrics, &mut lines);
    Ok(Comparison {
        lines,
        different_files,
    })
}

/// Nature d'un dossier pour le message de [`VerifyError::LayoutMismatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Run,
    Profiles,
    Lines,
}

impl Layout {
    /// Un dossier de lignes (`lines.json`) prime sur un dossier de profils (`profiles.json`), qui prime
    /// sur un dossier de run.
    fn of(dir: &Path) -> Layout {
        if dir.join(LINES_FILE).exists() {
            Layout::Lines
        } else if dir.join(PROFILES_FILE).exists() {
            Layout::Profiles
        } else {
            Layout::Run
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Layout::Run => "un dossier de run",
            Layout::Profiles => "un dossier de profils",
            Layout::Lines => "un dossier de lignes",
        }
    }
}

/// Compare les dossiers `dir_a` et `dir_b` : deux runs, deux dossiers de profils (ceux qui contiennent
/// `profiles.json`) ou deux dossiers de lignes (ceux qui contiennent `lines.json`).
///
/// Rend [`VerifyError`] si l'un des deux est illisible ou incohérent avec son manifeste, ou s'ils ne sont
/// pas de la même nature ; sinon un compte rendu dont [`VerifyReport::identical`] donne le verdict.
pub fn verify(dir_a: &Path, dir_b: &Path) -> Result<VerifyReport, VerifyError> {
    let (layout_a, layout_b) = (Layout::of(dir_a), Layout::of(dir_b));
    match (layout_a, layout_b) {
        (Layout::Lines, Layout::Lines) => verify_lines(dir_a, dir_b),
        (Layout::Profiles, Layout::Profiles) => verify_profiles(dir_a, dir_b),
        (Layout::Run, Layout::Run) => verify_runs(dir_a, dir_b),
        _ => Err(VerifyError::LayoutMismatch {
            rule: if layout_a == Layout::Lines || layout_b == Layout::Lines {
                "un dossier de lignes ne se compare qu'à un dossier de lignes"
            } else {
                "un dossier de profils ne se compare qu'à un dossier de profils"
            },
            a: dir_a.display().to_string(),
            a_kind: layout_a.kind(),
            b: dir_b.display().to_string(),
            b_kind: layout_b.kind(),
        }),
    }
}

/// Compare deux dossiers de run.
fn verify_runs(dir_a: &Path, dir_b: &Path) -> Result<VerifyReport, VerifyError> {
    let a = load_run("A", dir_a)?;
    let b = load_run("B", dir_b)?;
    let mut comparison = compare_runs(&a, &b)?;
    let mut lines = vec![
        format!("run A : {}", dir_a.display()),
        format!("run B : {}", dir_b.display()),
    ];
    lines.append(&mut comparison.lines);
    let different_files = comparison.different_files;
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

/// Les trois profils d'un dossier de profils, chargés et confrontés à `profiles.json`.
struct LoadedProfiles {
    bytes: Vec<u8>,
    value: Value,
    runs: Vec<(&'static str, LoadedRun)>,
}

/// Noms des profils, dans l'ordre fixe de `profiles.json`.
const PROFILE_NAMES: [&str; 3] = ["baseline", "adverse", "maker"];

/// Vrai si `dir` est un nom de sous-dossier simple : un seul composant normal, sans séparateur.
fn is_plain_dir_name(dir: &str) -> bool {
    let mut components = Path::new(dir).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(name)), None) if name == dir
    )
}

/// Lit `profiles.json` et les trois sous-dossiers d'un dossier de profils.
fn load_profiles(label: &'static str, dir: &Path) -> Result<LoadedProfiles, VerifyError> {
    let shown_dir = dir.display().to_string();
    let incomplete = |detail: String| VerifyError::ProfilesIncomplete {
        label,
        dir: shown_dir.clone(),
        detail,
    };
    let incoherent = |detail: String| VerifyError::Profiles {
        label,
        dir: shown_dir.clone(),
        detail,
    };
    let bytes = fs::read(dir.join(PROFILES_FILE)).map_err(|source| VerifyError::Read {
        label,
        dir: shown_dir.clone(),
        file: PROFILES_FILE,
        source,
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| VerifyError::Json {
        label,
        dir: shown_dir.clone(),
        file: PROFILES_FILE,
        source,
    })?;
    let found = value.get("schema").and_then(Value::as_str);
    if found != Some(PROFILES_SCHEMA) {
        return Err(VerifyError::Schema {
            label,
            dir: shown_dir.clone(),
            file: PROFILES_FILE,
            found: found.map_or_else(|| "absent".to_string(), |s| format!("`{s}`")),
            expected: PROFILES_SCHEMA,
        });
    }
    let entries = value
        .get("profiles")
        .and_then(Value::as_array)
        .filter(|entries| entries.len() == PROFILE_NAMES.len())
        .ok_or_else(|| incomplete("`profiles.json` ne décrit pas trois profils".to_string()))?;

    let mut runs = Vec::with_capacity(PROFILE_NAMES.len());
    for (name, entry) in PROFILE_NAMES.iter().zip(entries) {
        let entry_name = entry.get("name").and_then(Value::as_str);
        if entry_name != Some(name) {
            return Err(incomplete(format!(
                "le profil attendu est `{name}`, `profiles.json` porte {entry_name:?}"
            )));
        }
        let sub = entry
            .get("dir")
            .and_then(Value::as_str)
            .filter(|sub| is_plain_dir_name(sub))
            .ok_or_else(|| incomplete(format!("le profil `{name}` n'a pas de `dir` simple")))?;
        let sub_dir = dir.join(sub);
        if !sub_dir.join(MANIFEST_FILE).is_file() {
            return Err(incomplete(format!(
                "sous-dossier `{sub}` ou son manifeste absent (profil `{name}`)"
            )));
        }
        let run = load_run(label, &sub_dir)?;
        if !run.family.can_be_profile() {
            return Err(incoherent(format!(
                "le profil `{name}` n'est pas un run s10 ou s11 (schéma {})",
                shown(&run.manifest, &["schema"])
            )));
        }
        if let Some((first, other)) = runs
            .first()
            .map(|(first, other): &(&str, LoadedRun)| (first, other))
            && other.family != run.family
        {
            return Err(incoherent(format!(
                "le profil `{name}` n'est pas un run de la même génération que `{first}` \
                 (schéma {} contre {})",
                shown(&run.manifest, &["schema"]),
                shown(&other.manifest, &["schema"])
            )));
        }
        // `profiles.json` doit décrire exactement ce run.
        for key in ["run_id", "config_hash", "result_hash"] {
            let (wanted, found) = (string_at(entry, &[key]), string_at(&run.manifest, &[key]));
            if wanted.is_none() || wanted != found {
                return Err(incoherent(format!(
                    "profil `{name}` : `{key}` vaut {} dans profiles.json et {} dans le manifeste",
                    wanted.unwrap_or_else(|| "(absent)".to_string()),
                    found.unwrap_or_else(|| "(absent)".to_string())
                )));
            }
        }
        if string_at(&run.manifest, &["profile"]).as_deref() != Some(name) {
            return Err(incoherent(format!(
                "le manifeste du sous-dossier `{sub}` n'est pas celui du profil `{name}`"
            )));
        }
        runs.push((*name, run));
    }

    let ids: Vec<String> = runs
        .iter()
        .map(|(_, run)| string_at(&run.manifest, &["run_id"]).unwrap_or_default())
        .collect();
    let expected_id = match ids.as_slice() {
        [baseline, adverse, maker] => profiles_id(baseline, adverse, maker),
        _ => {
            return Err(incomplete(
                "`profiles.json` ne décrit pas trois profils".to_string(),
            ));
        }
    };
    if string_at(&value, &["profiles_id"]).as_deref() != Some(expected_id.as_str()) {
        return Err(incoherent(format!(
            "profiles_id {} pour {expected_id} recalculé depuis les trois run_id",
            shown(&value, &["profiles_id"])
        )));
    }
    for (name, run) in &runs {
        if string_at(&run.manifest, &["profiles_id"]).as_deref() != Some(expected_id.as_str()) {
            return Err(incoherent(format!(
                "le manifeste du profil `{name}` porte profiles_id {} pour {expected_id}",
                shown(&run.manifest, &["profiles_id"])
            )));
        }
    }
    Ok(LoadedProfiles { bytes, value, runs })
}

/// Comparaison de deux dossiers de profils chargés : lignes de compte rendu (sans en-tête ni verdict),
/// fichiers différents et profils en écart.
struct ProfilesComparison {
    lines: Vec<String>,
    different_files: usize,
    different_profiles: Vec<&'static str>,
}

/// Compare chaque profil à son homologue, puis `profiles.json` à l'octet.
fn compare_profiles(
    a: &LoadedProfiles,
    b: &LoadedProfiles,
) -> Result<ProfilesComparison, VerifyError> {
    let mut lines = Vec::new();
    let mut different_files = 0usize;
    let mut different_profiles = Vec::new();
    for ((name, run_a), (_, run_b)) in a.runs.iter().zip(&b.runs) {
        let comparison = compare_runs(run_a, run_b)?;
        let verdict = if comparison.different_files == 0 {
            "identique".to_string()
        } else {
            different_profiles.push(*name);
            format!("DIFFÉRENT ({} fichier(s))", comparison.different_files)
        };
        lines.push(format!("profil {name} : {verdict}"));
        for line in comparison.lines {
            lines.push(format!("  {line}"));
        }
        different_files += comparison.different_files;
    }
    if a.bytes == b.bytes {
        lines.push(format!(
            "{PROFILES_FILE} : identique ({})",
            hash::blake3_prefixed(&a.bytes)
        ));
    } else {
        different_files += 1;
        lines.push(format!("{PROFILES_FILE} : DIFFÉRENT"));
        describe_json_diff(&a.value, &b.value, &mut lines);
    }
    Ok(ProfilesComparison {
        lines,
        different_files,
        different_profiles,
    })
}

/// Compare deux dossiers de profils : chaque profil à son homologue, puis `profiles.json` à l'octet.
fn verify_profiles(dir_a: &Path, dir_b: &Path) -> Result<VerifyReport, VerifyError> {
    let a = load_profiles("A", dir_a)?;
    let b = load_profiles("B", dir_b)?;
    let mut lines = vec![
        format!("profils A : {}", dir_a.display()),
        format!("profils B : {}", dir_b.display()),
    ];
    let mut comparison = compare_profiles(&a, &b)?;
    lines.append(&mut comparison.lines);
    let different_files = comparison.different_files;
    if different_files == 0 {
        lines.push(format!(
            "verdict : dossiers de profils identiques à l'octet (profiles_id {})",
            shown(&a.value, &["profiles_id"])
        ));
    } else {
        let detail = if comparison.different_profiles.is_empty() {
            String::new()
        } else {
            format!(
                " ; profils en écart : {}",
                comparison.different_profiles.join(", ")
            )
        };
        lines.push(format!(
            "verdict : DIFFÉRENTS ({different_files} fichier(s){detail})"
        ));
    }
    Ok(VerifyReport {
        lines,
        different_files,
    })
}

// ---------------------------------------------------------------------------------------------
// Dossier de lignes (S11)
// ---------------------------------------------------------------------------------------------

/// Noms des deux lignes, dans l'ordre fixe de `lines.json`.
const LINE_NAMES: [&str; 2] = ["full", "okx_usdc"];

/// Une ligne chargée : un run, ou un dossier de profils.
enum LineBody {
    Run(Box<LoadedRun>),
    Profiles(Box<LoadedProfiles>),
}

/// Un dossier de lignes chargé et confronté à `lines.json`.
struct LoadedLines {
    bytes: Vec<u8>,
    value: Value,
    lines: Vec<(&'static str, LineBody)>,
}

/// Lit `lines.json` et les deux sous-dossiers d'un dossier de lignes.
fn load_lines(label: &'static str, dir: &Path) -> Result<LoadedLines, VerifyError> {
    let shown_dir = dir.display().to_string();
    let incomplete = |detail: String| VerifyError::LinesIncomplete {
        label,
        dir: shown_dir.clone(),
        detail,
    };
    let incoherent = |detail: String| VerifyError::Lines {
        label,
        dir: shown_dir.clone(),
        detail,
    };
    let bytes = fs::read(dir.join(LINES_FILE)).map_err(|source| VerifyError::Read {
        label,
        dir: shown_dir.clone(),
        file: LINES_FILE,
        source,
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| VerifyError::Json {
        label,
        dir: shown_dir.clone(),
        file: LINES_FILE,
        source,
    })?;
    let found = value.get("schema").and_then(Value::as_str);
    if found != Some(LINES_SCHEMA) {
        return Err(VerifyError::Schema {
            label,
            dir: shown_dir.clone(),
            file: LINES_FILE,
            found: found.map_or_else(|| "absent".to_string(), |s| format!("`{s}`")),
            expected: LINES_SCHEMA,
        });
    }
    let entries = value
        .get("lines")
        .and_then(Value::as_array)
        .filter(|entries| entries.len() == LINE_NAMES.len())
        .ok_or_else(|| incomplete("`lines.json` ne décrit pas deux lignes".to_string()))?;

    let mut lines: Vec<(&'static str, LineBody)> = Vec::with_capacity(LINE_NAMES.len());
    let mut ids = Vec::with_capacity(LINE_NAMES.len());
    for (name, entry) in LINE_NAMES.iter().zip(entries) {
        let entry_name = entry.get("line").and_then(Value::as_str);
        if entry_name != Some(name) {
            return Err(incomplete(format!(
                "la ligne attendue est `{name}`, `lines.json` porte {entry_name:?}"
            )));
        }
        let sub = entry
            .get("dir")
            .and_then(Value::as_str)
            .filter(|sub| is_plain_dir_name(sub))
            .ok_or_else(|| incomplete(format!("la ligne `{name}` n'a pas de `dir` simple")))?;
        let sub_dir = dir.join(sub);
        let is_profiles = sub_dir.join(PROFILES_FILE).exists();
        let (body, id) = if is_profiles {
            let profiles = load_profiles(label, &sub_dir)?;
            let id = string_at(&profiles.value, &["profiles_id"]).unwrap_or_default();
            if string_at(entry, &["profiles_id"]).as_deref() != Some(id.as_str()) {
                return Err(incoherent(format!(
                    "ligne `{name}` : profiles_id {} dans lines.json et {id} dans `{sub}/profiles.json`",
                    shown(entry, &["profiles_id"])
                )));
            }
            (LineBody::Profiles(Box::new(profiles)), id)
        } else {
            if !sub_dir.join(MANIFEST_FILE).is_file() {
                return Err(incomplete(format!(
                    "sous-dossier `{sub}` ou son manifeste absent (ligne `{name}`)"
                )));
            }
            let run = load_run(label, &sub_dir)?;
            let id = string_at(&run.manifest, &["run_id"]).unwrap_or_default();
            if string_at(entry, &["run_id"]).as_deref() != Some(id.as_str()) {
                return Err(incoherent(format!(
                    "ligne `{name}` : run_id {} dans lines.json et {id} dans le manifeste de `{sub}`",
                    shown(entry, &["run_id"])
                )));
            }
            (LineBody::Run(Box::new(run)), id)
        };
        ids.push(id);
        lines.push((*name, body));
    }

    // Chaque run est un run de recherche (s11), de la ligne qui le porte, et son `config_hash` est celui
    // de `lines.json` pour son profil.
    let config_hashes = value
        .get("config_hash")
        .and_then(Value::as_object)
        .ok_or_else(|| incoherent("`config_hash` absent ou mal typé".to_string()))?;
    for (name, body) in &lines {
        let runs: Vec<(&str, &LoadedRun)> = match body {
            LineBody::Run(run) => vec![("single", run.as_ref())],
            LineBody::Profiles(profiles) => profiles.runs.iter().map(|(n, r)| (*n, r)).collect(),
        };
        for (profile, run) in runs {
            if run.family != Family::S11 {
                return Err(incoherent(format!(
                    "la ligne `{name}` (profil `{profile}`) n'est pas un run s11 (schéma {})",
                    shown(&run.manifest, &["schema"])
                )));
            }
            let wanted = config_hashes.get(profile).and_then(Value::as_str);
            let found = string_at(&run.manifest, &["config_hash"]);
            if wanted.is_none() || wanted != found.as_deref() {
                return Err(incoherent(format!(
                    "ligne `{name}`, profil `{profile}` : config_hash {} dans lines.json et {} dans le \
                     manifeste",
                    wanted.unwrap_or("(absent)"),
                    found.unwrap_or_else(|| "(absent)".to_string())
                )));
            }
            let line_in_metrics = string_at(&run.metrics, &["research", "line"]);
            if line_in_metrics.as_deref() != Some(name) {
                return Err(incoherent(format!(
                    "la ligne `{name}` (profil `{profile}`) porte research.line {line_in_metrics:?}"
                )));
            }
        }
    }
    let expected_id = match ids.as_slice() {
        [full, okx] => hash::blake3_hex(format!("{full}|{okx}").as_bytes()),
        _ => {
            return Err(incomplete(
                "`lines.json` ne décrit pas deux lignes".to_string(),
            ));
        }
    };
    if string_at(&value, &["lines_id"]).as_deref() != Some(expected_id.as_str()) {
        return Err(incoherent(format!(
            "lines_id {} pour {expected_id} recalculé depuis les deux identifiants",
            shown(&value, &["lines_id"])
        )));
    }
    Ok(LoadedLines {
        bytes,
        value,
        lines,
    })
}

/// Compare deux dossiers de lignes : chaque ligne à son homologue, puis `lines.json` à l'octet.
fn verify_lines(dir_a: &Path, dir_b: &Path) -> Result<VerifyReport, VerifyError> {
    let a = load_lines("A", dir_a)?;
    let b = load_lines("B", dir_b)?;
    let mut lines = vec![
        format!("lignes A : {}", dir_a.display()),
        format!("lignes B : {}", dir_b.display()),
    ];
    let mut different_files = 0usize;
    let mut different_lines = Vec::new();
    for ((name, body_a), (_, body_b)) in a.lines.iter().zip(&b.lines) {
        let (verdict, mut detail, files) = match (body_a, body_b) {
            (LineBody::Run(run_a), LineBody::Run(run_b)) => {
                let comparison = compare_runs(run_a, run_b)?;
                (
                    comparison.different_files,
                    comparison.lines,
                    comparison.different_files,
                )
            }
            (LineBody::Profiles(pa), LineBody::Profiles(pb)) => {
                let comparison = compare_profiles(pa, pb)?;
                (
                    comparison.different_files,
                    comparison.lines,
                    comparison.different_files,
                )
            }
            _ => {
                return Err(VerifyError::LayoutMismatch {
                    rule: "une ligne est un run dans les deux dossiers, ou un dossier de profils dans les deux",
                    a: dir_a.join(name).display().to_string(),
                    a_kind: line_kind(body_a),
                    b: dir_b.join(name).display().to_string(),
                    b_kind: line_kind(body_b),
                });
            }
        };
        if verdict == 0 {
            lines.push(format!("ligne {name} : identique"));
        } else {
            different_lines.push(*name);
            lines.push(format!("ligne {name} : DIFFÉRENTE ({verdict} fichier(s))"));
        }
        for line in detail.drain(..) {
            lines.push(format!("  {line}"));
        }
        different_files += files;
    }
    if a.bytes == b.bytes {
        lines.push(format!(
            "{LINES_FILE} : identique ({})",
            hash::blake3_prefixed(&a.bytes)
        ));
    } else {
        different_files += 1;
        lines.push(format!("{LINES_FILE} : DIFFÉRENT"));
        describe_json_diff(&a.value, &b.value, &mut lines);
    }
    if different_files == 0 {
        lines.push(format!(
            "verdict : dossiers de lignes identiques à l'octet (lines_id {})",
            shown(&a.value, &["lines_id"])
        ));
    } else {
        let detail = if different_lines.is_empty() {
            String::new()
        } else {
            format!(" ; lignes en écart : {}", different_lines.join(", "))
        };
        lines.push(format!(
            "verdict : DIFFÉRENTS ({different_files} fichier(s){detail})"
        ));
    }
    Ok(VerifyReport {
        lines,
        different_files,
    })
}

fn line_kind(body: &LineBody) -> &'static str {
    match body {
        LineBody::Run(_) => "un run",
        LineBody::Profiles(_) => "un dossier de profils",
    }
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
    // Le schéma du manifeste décide de la famille ; un schéma inconnu est cité tel quel.
    let manifest_value: Value =
        serde_json::from_slice(&manifest_bytes).map_err(|source| VerifyError::Json {
            label,
            dir: shown.clone(),
            file: MANIFEST_FILE,
            source,
        })?;
    let found_schema = manifest_value.get("schema").and_then(Value::as_str);
    let family = Family::of(found_schema).ok_or_else(|| VerifyError::Schema {
        label,
        dir: shown.clone(),
        file: MANIFEST_FILE,
        found: found_schema.map_or_else(|| "absent".to_string(), |s| format!("`{s}`")),
        expected: MANIFEST_SCHEMAS,
    })?;
    let manifest = manifest_value;
    let metrics_bytes = read(METRICS_FILE)?;
    let metrics = parse(METRICS_FILE, &metrics_bytes, family.metrics_schema())?;
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
    if family.is_portfolio() {
        check_portfolio(&metrics, &manifest, &trades_bytes).map_err(|detail| {
            VerifyError::Portfolio {
                label,
                dir: shown.clone(),
                detail,
            }
        })?;
    }
    // Un run s11 porte des coûts quand c'est un profil (`profile` dans le manifeste).
    let has_costs = family == Family::S10
        || (family == Family::S11
            && (manifest.get("profile").is_some() || metrics.get("costs").is_some()));
    if has_costs {
        check_costs(&metrics, &manifest).map_err(|detail| VerifyError::Portfolio {
            label,
            dir: shown.clone(),
            detail,
        })?;
    }
    if family == Family::S11 {
        check_research(&metrics, &manifest).map_err(|detail| VerifyError::Portfolio {
            label,
            dir: shown.clone(),
            detail,
        })?;
    }
    Ok(LoadedRun {
        family,
        manifest,
        metrics_bytes,
        metrics,
        trades_bytes,
        equity_bytes,
    })
}

/// Nombre situé au chemin `path` d'un objet JSON, si c'est un nombre.
fn number_at(value: &Value, path: &[&str]) -> Option<f64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_f64()
}

/// Entier non signé situé au chemin `path`.
fn uint_at(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_u64()
}

/// Somme des entiers d'un objet JSON (les motifs de refus), `None` si une valeur n'en est pas un.
fn sum_object(value: Option<&Value>) -> Option<u64> {
    let mut total = 0u64;
    for entry in value?.as_object()?.values() {
        total = total.checked_add(entry.as_u64()?)?;
    }
    Some(total)
}

/// Compteurs de `sim_by_pair[].counters` qui sont des refus au fill d'une entrée (mêmes champs que
/// `entry_not_filled_by_reason`, hors `other`).
const AT_FILL_COUNTERS: [&str; 5] = [
    "buys_rejected_insufficient_cash",
    "buys_rejected_below_minimum",
    "buys_rejected_volume_share",
    "entries_invalid_risk_at_fill",
    "entries_dropped_while_long",
];

/// Contrôles de cohérence d'un run S9 (docs S9 §14.2), sans rien recalculer du moteur : seulement des
/// identités entre les blocs de `metrics.json`, le manifeste et `trades.csv`.
fn check_portfolio(metrics: &Value, manifest: &Value, trades_bytes: &[u8]) -> Result<(), String> {
    let missing = |path: &str| format!("champ `{path}` absent ou mal typé");

    // Capital et fichier d'instruments : identiques dans metrics.json et dans le manifeste.
    let cash_metrics = number_at(metrics, &["config", "initial_cash"])
        .ok_or_else(|| missing("config.initial_cash"))?;
    let cash_manifest =
        number_at(manifest, &["initial_cash"]).ok_or_else(|| missing("initial_cash"))?;
    if cash_metrics.to_bits() != cash_manifest.to_bits() {
        return Err(format!(
            "capital {cash_metrics} dans metrics.json et {cash_manifest} dans le manifeste"
        ));
    }
    let hash_metrics = metrics.pointer("/instruments/instruments_hash");
    let hash_manifest = manifest.get("instruments_hash");
    if hash_metrics.is_none() || hash_manifest.is_none() || hash_metrics != hash_manifest {
        return Err(format!(
            "instruments_hash {} dans metrics.json et {} dans le manifeste",
            hash_metrics.map_or_else(|| "(absent)".to_string(), Value::to_string),
            hash_manifest.map_or_else(|| "(absent)".to_string(), Value::to_string)
        ));
    }

    // Σ funnel_by_pair = funnel.
    let pairs = metrics
        .get("funnel_by_pair")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("funnel_by_pair"))?;
    let symbols: Vec<&str> = metrics
        .pointer("/data/symbols")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("data.symbols"))?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let pair_symbols: Vec<&str> = pairs
        .iter()
        .filter_map(|pair| pair.get("symbol").and_then(Value::as_str))
        .collect();
    if pair_symbols != symbols {
        return Err(format!(
            "funnel_by_pair couvre {pair_symbols:?} pour l'univers {symbols:?}"
        ));
    }
    let mut totals = [0u64; 5];
    let keys = [
        "evaluations",
        "entry_signals",
        "entry_orders",
        "entry_fills",
        "trades",
    ];
    // Compteurs du simulateur par paire : seuls les refus au fill d'une entrée en sortent (les
    // compteurs `buys_rejected_*` et `entries_*` ne comptent que des entrées ; les refus de ventes
    // vont dans `below_min_exits`, hors de cette liste).
    let sim_by_pair = metrics
        .get("sim_by_pair")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("sim_by_pair"))?;
    let sim_symbols: Vec<&str> = sim_by_pair
        .iter()
        .filter_map(|pair| pair.get("symbol").and_then(Value::as_str))
        .collect();
    if sim_symbols != symbols {
        return Err(format!(
            "sim_by_pair couvre {sim_symbols:?} pour l'univers {symbols:?}"
        ));
    }
    // Refus de recherche par paire (s11) : fenêtre de décision, hors univers, qualité. Ils s'ajoutent à
    // l'identité 1 et restent hors de `rejected`.
    let research = metrics.get("research");
    let mut refused_by_pair = Vec::with_capacity(pair_symbols.len());
    for name in &pair_symbols {
        let refused = match research {
            None => 0,
            Some(block) => {
                let leg = block
                    .get("legs")
                    .and_then(Value::as_array)
                    .and_then(|legs| {
                        legs.iter()
                            .find(|leg| leg.get("symbol").and_then(Value::as_str) == Some(name))
                    })
                    .ok_or_else(|| format!("research.legs n'a pas de jambe `{name}`"))?;
                let mut sum = 0u64;
                for key in RESEARCH_REFUSALS {
                    sum += uint_at(leg, &[key])
                        .ok_or_else(|| missing(&format!("research.legs[{name}].{key}")))?;
                }
                sum
            }
        };
        refused_by_pair.push(refused);
    }
    let refused_total: u64 = refused_by_pair.iter().sum();
    let mut rejected_sum = 0u64;
    // Σ des refus au fill par paire, et Σ de `orders - fills - refus au fill` (expirations et
    // annulations d'entrées, qui ne sont publiées qu'en agrégat).
    let mut at_fill_by_pair_sum = 0u64;
    let mut unfilled_rest_sum = 0u64;
    for ((pair, sim), refused) in pairs.iter().zip(sim_by_pair).zip(&refused_by_pair) {
        let name = pair.get("symbol").and_then(Value::as_str).unwrap_or("?");
        for (total, key) in totals.iter_mut().zip(keys) {
            *total += uint_at(pair, &[key])
                .ok_or_else(|| missing(&format!("funnel_by_pair[{name}].{key}")))?;
        }
        let rejected = sum_object(pair.get("rejected"))
            .ok_or_else(|| missing(&format!("funnel_by_pair[{name}].rejected")))?;
        let (signals, orders, fills) = (
            uint_at(pair, &["entry_signals"]).unwrap_or(0),
            uint_at(pair, &["entry_orders"]).unwrap_or(0),
            uint_at(pair, &["entry_fills"]).unwrap_or(0),
        );
        let mut at_fill = 0u64;
        for key in AT_FILL_COUNTERS {
            at_fill += uint_at(sim, &["counters", key])
                .ok_or_else(|| missing(&format!("sim_by_pair[{name}].counters.{key}")))?;
        }
        // `rejected` = refus à la soumission + refus au fill.
        let submit = rejected.checked_sub(at_fill).ok_or_else(|| {
            format!("{name} : {at_fill} refus au fill pour {rejected} refus d'entrée au total")
        })?;
        // Identité 1 (docs S9 §9.1), exacte par paire.
        if signals != orders + submit + refused {
            return Err(format!(
                "{name} : {signals} signaux d'entrée pour {orders} ordres, {submit} refus à la soumission \
                 et {refused} refus de recherche"
            ));
        }
        // Identité 2, en inégalité par paire : expirations et annulations ne sont pas publiées par
        // paire, la différence est vérifiée en agrégat plus bas.
        let unfilled = orders.checked_sub(fills + at_fill).ok_or_else(|| {
            format!(
                "{name} : {orders} ordres d'entrée pour {fills} lots ouverts et {at_fill} refus au fill"
            )
        })?;
        rejected_sum += rejected;
        at_fill_by_pair_sum += at_fill;
        unfilled_rest_sum += unfilled;
    }
    for (total, key) in totals.iter().zip(keys) {
        let found =
            uint_at(metrics, &["funnel", key]).ok_or_else(|| missing(&format!("funnel.{key}")))?;
        if *total != found {
            return Err(format!(
                "Σ funnel_by_pair.{key} = {total} pour funnel.{key} = {found}"
            ));
        }
    }
    let funnel = metrics.get("funnel").ok_or_else(|| missing("funnel"))?;
    let by_reason = sum_object(funnel.get("entry_rejected_by_reason"))
        .ok_or_else(|| missing("funnel.entry_rejected_by_reason"))?;
    let max_positions = uint_at(funnel, &["entry_rejected_max_positions"])
        .ok_or_else(|| missing("funnel.entry_rejected_max_positions"))?;
    let entry_signals = totals[1];
    let entry_orders = totals[2];
    if entry_signals != entry_orders + by_reason + max_positions + refused_total {
        return Err(format!(
            "funnel : {entry_signals} signaux d'entrée pour {entry_orders} ordres, {by_reason} refus, \
             {max_positions} refus max_positions et {refused_total} refus de recherche"
        ));
    }
    let mut at_fill = 0u64;
    for key in [
        "insufficient_cash",
        "below_minimum",
        "volume_share",
        "invalid_risk_at_fill",
        "dropped_while_long",
    ] {
        at_fill += uint_at(funnel, &["entry_not_filled_by_reason", key])
            .ok_or_else(|| missing(&format!("funnel.entry_not_filled_by_reason.{key}")))?;
    }
    if at_fill_by_pair_sum != at_fill {
        return Err(format!(
            "Σ sim_by_pair des refus au fill = {at_fill_by_pair_sum} pour {at_fill} dans funnel.entry_not_filled_by_reason"
        ));
    }
    // Identité 2 (docs S9 §9.1), exacte en agrégat : le reste des ordres d'entrée est l'ensemble des
    // expirations et annulations, publié sous `other`.
    let other = uint_at(funnel, &["entry_not_filled_by_reason", "other"])
        .ok_or_else(|| missing("funnel.entry_not_filled_by_reason.other"))?;
    if unfilled_rest_sum != other {
        return Err(format!(
            "Σ par paire de (ordres - lots - refus au fill) = {unfilled_rest_sum} pour funnel.entry_not_filled_by_reason.other = {other}"
        ));
    }
    if rejected_sum != by_reason + max_positions + at_fill {
        return Err(format!(
            "Σ funnel_by_pair.rejected = {rejected_sum} pour {} refus au total du funnel",
            by_reason + max_positions + at_fill
        ));
    }

    // trades.csv : chaque symbole appartient à l'univers, et les comptes sont ceux de funnel_by_pair. En
    // recherche, `trades.csv` garde aussi les trades purgés (`exit_type = end_of_window`), que
    // `funnel_by_pair` ne compte pas : ils sont comptés à part.
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b',')
        .from_reader(trades_bytes);
    let headers = reader
        .headers()
        .map_err(|e| format!("trades.csv illisible : {e}"))?
        .clone();
    let column = headers
        .iter()
        .position(|name| name == "symbol")
        .ok_or_else(|| "trades.csv n'a pas de colonne `symbol`".to_string())?;
    let exit_column = headers.iter().position(|name| name == "exit_type");
    if research.is_some() && exit_column.is_none() {
        return Err("trades.csv n'a pas de colonne `exit_type`".to_string());
    }
    let mut counts = vec![0u64; symbols.len()];
    let mut purged_rows = 0u64;
    for record in reader.records() {
        let record = record.map_err(|e| format!("trades.csv illisible : {e}"))?;
        let symbol = record.get(column).unwrap_or_default();
        let Some(index) = symbols.iter().position(|s| *s == symbol) else {
            return Err(format!(
                "trades.csv : le symbole `{symbol}` n'est pas dans data.symbols {symbols:?}"
            ));
        };
        if research.is_some() && exit_column.and_then(|c| record.get(c)) == Some("end_of_window") {
            purged_rows += 1;
            continue;
        }
        if let Some(count) = counts.get_mut(index) {
            *count += 1;
        }
    }
    for (pair, count) in pairs.iter().zip(&counts) {
        let name = pair.get("symbol").and_then(Value::as_str).unwrap_or("?");
        let expected = uint_at(pair, &["trades"]).unwrap_or(0);
        if expected != *count {
            return Err(format!(
                "trades.csv a {count} lignes conservées pour `{name}`, funnel_by_pair en annonce {expected}"
            ));
        }
    }
    if let Some(block) = research {
        let purged = uint_at(block, &["purge", "trades_purged"])
            .ok_or_else(|| missing("research.purge.trades_purged"))?;
        if purged != purged_rows {
            return Err(format!(
                "trades.csv a {purged_rows} lignes `end_of_window`, research.purge.trades_purged en annonce {purged}"
            ));
        }
        let kept = uint_at(block, &["purge", "trades_kept"])
            .ok_or_else(|| missing("research.purge.trades_kept"))?;
        let kept_rows: u64 = counts.iter().sum();
        let n_trades = uint_at(metrics, &["n_trades"]).ok_or_else(|| missing("n_trades"))?;
        if kept != kept_rows || kept != n_trades {
            return Err(format!(
                "research.purge.trades_kept = {kept} pour {kept_rows} lignes conservées de trades.csv \
                 et n_trades = {n_trades}"
            ));
        }
    }
    Ok(())
}

/// Les trois compteurs de refus de recherche qui entrent dans l'identité de l'entonnoir d'entrée (le
/// quatrième, `entries_cancelled_universe`, compte des ordres déjà acceptés).
const RESEARCH_REFUSALS: [&str; 3] = [
    "entries_outside_decision_window",
    "entries_not_in_universe",
    "entries_excluded_quality",
];

/// Contrôles propres à un run s11 : le bloc `research` est cohérent avec `data`, avec `trades.csv` (déjà
/// vu par `check_portfolio`) et avec le manifeste ; l'identité de recherche est recalculée, et le
/// `run_id` aussi.
fn check_research(metrics: &Value, manifest: &Value) -> Result<(), String> {
    let missing = |path: &str| format!("champ `{path}` absent ou mal typé");
    let research = metrics
        .get("research")
        .filter(|r| r.is_object())
        .ok_or_else(|| missing("research"))?;

    // Identité de recherche : `hash_json` de l'objet tel qu'écrit (clés triées, comme le struct).
    let identity = research
        .get("identity")
        .ok_or_else(|| missing("research.identity"))?;
    let recomputed = format!(
        "{}{}",
        hash::HASH_PREFIX,
        hash::hash_json(identity).map_err(|e| format!("research.identity non hachable : {e}"))?
    );
    let written =
        string_at(research, &["identity_hash"]).ok_or_else(|| missing("research.identity_hash"))?;
    if recomputed != written {
        return Err(format!(
            "research.identity_hash = {written} pour {recomputed} recalculé depuis research.identity"
        ));
    }

    // run_id : recalculé par la formule de S11 depuis les champs du manifeste.
    let config_hash =
        string_at(manifest, &["config_hash"]).ok_or_else(|| missing("config_hash"))?;
    let dataset_hash =
        string_at(manifest, &["dataset_hash"]).ok_or_else(|| missing("dataset_hash"))?;
    let start_ms = manifest
        .pointer("/window/start_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| missing("window.start_ms"))?;
    let end_ms = manifest
        .pointer("/window/end_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| missing("window.end_ms"))?;
    let cash = number_at(manifest, &["initial_cash"]).ok_or_else(|| missing("initial_cash"))?;
    let run_id = run_id_s11(
        &config_hash,
        &dataset_hash,
        start_ms,
        end_ms,
        cash,
        &written,
    );
    if string_at(manifest, &["run_id"]).as_deref() != Some(run_id.as_str()) {
        return Err(format!(
            "run_id {} pour {run_id} recalculé (identité de recherche comprise)",
            shown(manifest, &["run_id"])
        ));
    }

    // Le dataset porte le bloc et la fenêtre de décision du bloc `research`, dans metrics.json comme dans
    // le manifeste.
    for (root, value) in [("metrics.json", metrics), ("manifest.json", manifest)] {
        for key in ["block", "decision_window"] {
            let in_data = value.pointer(&format!("/data/{key}"));
            let in_research = research.get(key);
            if in_data.is_none() || in_data != in_research {
                return Err(format!(
                    "{root} : data.{key} {} et research.{key} {}",
                    in_data.map_or_else(|| "(absent)".to_string(), Value::to_string),
                    in_research.map_or_else(|| "(absent)".to_string(), Value::to_string)
                ));
            }
        }
    }
    // Le bloc et la fenêtre de l'identité sont ceux du bloc `research`.
    if identity.get("block") != research.get("block") {
        return Err("research.identity.block diffère de research.block".to_string());
    }
    let window_start = research
        .pointer("/decision_window/start_ms")
        .and_then(Value::as_i64);
    let window_end = research
        .pointer("/decision_window/end_ms")
        .and_then(Value::as_i64);
    if identity.get("decision_start_ms").and_then(Value::as_i64) != window_start
        || identity.get("decision_end_ms").and_then(Value::as_i64) != window_end
    {
        return Err(
            "research.identity porte une fenêtre de décision différente de research.decision_window"
                .to_string(),
        );
    }
    let basis =
        string_at(research, &["metrics_basis"]).ok_or_else(|| missing("research.metrics_basis"))?;
    if basis != METRICS_BASIS {
        return Err(format!(
            "research.metrics_basis = `{basis}` pour `{METRICS_BASIS}` attendu"
        ));
    }

    // Ligne et sous-ensemble : `full` n'a pas de sous-ensemble, `okx_usdc` en a un, dont le hash est
    // celui de l'identité.
    let line = string_at(research, &["line"]).ok_or_else(|| missing("research.line"))?;
    if string_at(identity, &["line"]).as_deref() != Some(line.as_str()) {
        return Err(format!(
            "research.line = `{line}` pour {} dans research.identity",
            shown(identity, &["line"])
        ));
    }
    let subset = research.get("subset").filter(|v| !v.is_null());
    match (line.as_str(), subset) {
        ("full", None) => {}
        ("okx_usdc", Some(subset)) => {
            if string_at(subset, &["subset_hash"]) != string_at(identity, &["subset_hash"]) {
                return Err(format!(
                    "research.subset.subset_hash {} pour {} dans research.identity",
                    shown(subset, &["subset_hash"]),
                    shown(identity, &["subset_hash"])
                ));
            }
        }
        ("full", Some(_)) => return Err("la ligne `full` porte un sous-ensemble".to_string()),
        ("okx_usdc", None) => {
            return Err("la ligne `okx_usdc` n'a pas de sous-ensemble".to_string());
        }
        (other, _) => return Err(format!("research.line = `{other}` inconnue")),
    }

    // Σ des jambes = entonnoir de recherche.
    let legs = research
        .get("legs")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("research.legs"))?;
    for key in RESEARCH_REFUSALS
        .iter()
        .copied()
        .chain(["entries_cancelled_universe"])
    {
        let mut sum = 0u64;
        for leg in legs {
            sum +=
                uint_at(leg, &[key]).ok_or_else(|| missing(&format!("research.legs[].{key}")))?;
        }
        let total = uint_at(research, &["funnel", key])
            .ok_or_else(|| missing(&format!("research.funnel.{key}")))?;
        if sum != total {
            return Err(format!(
                "Σ research.legs.{key} = {sum} pour research.funnel.{key} = {total}"
            ));
        }
    }
    Ok(())
}

/// Contrôles propres à un run s10 : le profil et les hashes des fichiers de coûts sont les mêmes dans
/// `metrics.json` et dans le manifeste, et les trois blocs de coûts sont présents.
fn check_costs(metrics: &Value, manifest: &Value) -> Result<(), String> {
    let missing = |path: &str| format!("champ `{path}` absent ou mal typé");
    for block in ["costs", "cost_arithmetic", "cost_counters"] {
        if !metrics.get(block).is_some_and(Value::is_object) {
            return Err(missing(block));
        }
    }
    let profile_metrics =
        string_at(metrics, &["costs", "profile"]).ok_or_else(|| missing("costs.profile"))?;
    let profile_manifest = string_at(manifest, &["profile"]).ok_or_else(|| missing("profile"))?;
    if profile_metrics != profile_manifest {
        return Err(format!(
            "profil `{profile_metrics}` dans metrics.json et `{profile_manifest}` dans le manifeste"
        ));
    }
    let hash_metrics = string_at(metrics, &["costs", "costs_file_hash"])
        .ok_or_else(|| missing("costs.costs_file_hash"))?;
    let hash_manifest =
        string_at(manifest, &["costs_file_hash"]).ok_or_else(|| missing("costs_file_hash"))?;
    if hash_metrics != hash_manifest {
        return Err(format!(
            "costs_file_hash {hash_metrics} dans metrics.json et {hash_manifest} dans le manifeste"
        ));
    }
    // Le fichier de mesures est facultatif : `null` dans les métriques, absent du manifeste.
    let measures_metrics = string_at(metrics, &["costs", "cost_measures_hash"]);
    let measures_manifest = string_at(manifest, &["cost_measures_hash"]);
    if measures_metrics != measures_manifest {
        return Err(format!(
            "cost_measures_hash {measures_metrics:?} dans metrics.json et {measures_manifest:?} dans le manifeste"
        ));
    }
    Ok(())
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

/// Information sur l'identité de recherche (s11) : deux runs de blocs ou de lignes différents ne sont pas
/// censés être identiques. Aucun effet sur le verdict.
fn describe_research_info(a: &Value, b: &Value, lines: &mut Vec<String>) {
    let path = ["research", "identity_hash"];
    if string_at(a, &path) != string_at(b, &path) {
        lines.push(
            "attention : identités de recherche différentes (bloc, ligne, sous-ensemble ou jambes), ces \
             runs ne sont pas censés être identiques"
                .to_string(),
        );
        lines.push(format!(
            "  research.identity_hash : {} contre {}",
            shown(a, &path),
            shown(b, &path)
        ));
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

    /// Un run S9 minimal et cohérent à deux paires : BTC a 10 signaux d'entrée, 3 refus
    /// `max_positions` à la soumission, 7 ordres dont 2 lots, 1 refus de volume au fill et 4
    /// expirations ; ETH a 5 signaux, 5 ordres et 5 lots.
    fn coherent_run() -> (Value, Value, Vec<u8>) {
        let zero_counters = json!({
            "buys_rejected_insufficient_cash": 0, "buys_rejected_below_minimum": 0,
            "buys_rejected_volume_share": 0, "entries_invalid_risk_at_fill": 0,
            "entries_dropped_while_long": 0
        });
        let mut btc_counters = zero_counters.clone();
        btc_counters["buys_rejected_volume_share"] = json!(1);
        let no_rejection = json!({
            "min_notional": 0, "lot_step": 0, "cash": 0, "max_positions": 0, "volume": 0, "other": 0
        });
        let mut btc_rejected = no_rejection.clone();
        btc_rejected["max_positions"] = json!(3);
        btc_rejected["volume"] = json!(1);
        let metrics = json!({
            "config": {"initial_cash": 300.0},
            "instruments": {"instruments_hash": "blake3:aa"},
            "data": {"symbols": ["BTC-USDC", "ETH-USDC"]},
            "funnel_by_pair": [
                {"symbol": "BTC-USDC", "evaluations": 100, "entry_signals": 10, "entry_orders": 7,
                 "entry_fills": 2, "trades": 2, "rejected": btc_rejected},
                {"symbol": "ETH-USDC", "evaluations": 50, "entry_signals": 5, "entry_orders": 5,
                 "entry_fills": 5, "trades": 5, "rejected": no_rejection}
            ],
            "sim_by_pair": [
                {"symbol": "BTC-USDC", "counters": btc_counters},
                {"symbol": "ETH-USDC", "counters": zero_counters}
            ],
            "funnel": {
                "evaluations": 150, "entry_signals": 15, "entry_orders": 12, "entry_fills": 7,
                "trades": 7,
                "entry_rejected_by_reason": {"insufficient_quote": 0},
                "entry_rejected_max_positions": 3,
                "entry_not_filled_by_reason": {
                    "insufficient_cash": 0, "below_minimum": 0, "volume_share": 1,
                    "invalid_risk_at_fill": 0, "dropped_while_long": 0, "other": 4
                }
            }
        });
        let manifest = json!({"initial_cash": 300.0, "instruments_hash": "blake3:aa"});
        let mut csv = String::from("symbol\n");
        csv.push_str(&"BTC-USDC\n".repeat(2));
        csv.push_str(&"ETH-USDC\n".repeat(5));
        (metrics, manifest, csv.into_bytes())
    }

    fn check(metrics: &Value, manifest: &Value, csv: &[u8]) -> Result<(), String> {
        check_portfolio(metrics, manifest, csv)
    }

    #[test]
    fn un_run_portefeuille_coherent_passe_le_controle_des_identites() {
        let (metrics, manifest, csv) = coherent_run();
        assert_eq!(check(&metrics, &manifest, &csv), Ok(()));
    }

    #[test]
    fn un_ordre_deplace_entre_paires_casse_l_identite_1_par_paire() {
        let (mut metrics, manifest, csv) = coherent_run();
        // Les sommes de `funnel_by_pair` restent celles de `funnel`, les inégalités restent vraies.
        metrics["funnel_by_pair"][0]["entry_orders"] = json!(6);
        metrics["funnel_by_pair"][1]["entry_orders"] = json!(6);
        let erreur = check(&metrics, &manifest, &csv).unwrap_err();
        assert!(erreur.contains("BTC-USDC"), "{erreur}");
        assert!(erreur.contains("signaux d'entrée"), "{erreur}");
    }

    #[test]
    fn des_refus_a_la_soumission_deplaces_entre_paires_cassent_l_identite_1() {
        let (mut metrics, manifest, csv) = coherent_run();
        // 2 refus `max_positions` de BTC passent à ETH : Σ rejected et toutes les sommes de
        // `funnel` restent vraies, seule l'identité 1 de chaque paire est fausse.
        metrics["funnel_by_pair"][0]["rejected"]["max_positions"] = json!(1);
        metrics["funnel_by_pair"][1]["rejected"]["max_positions"] = json!(2);
        let erreur = check(&metrics, &manifest, &csv).unwrap_err();
        assert!(erreur.contains("BTC-USDC"), "{erreur}");
        assert!(erreur.contains("signaux d'entrée"), "{erreur}");
    }

    #[test]
    fn des_refus_au_fill_deplaces_entre_paires_sont_refuses() {
        let (mut metrics, manifest, csv) = coherent_run();
        // Le refus de volume de BTC est attribué à ETH dans `sim_by_pair` seulement : la somme au
        // fill reste 1, mais `rejected` de BTC n'a plus de refus à la soumission qui boucle.
        metrics["sim_by_pair"][0]["counters"]["buys_rejected_volume_share"] = json!(0);
        metrics["sim_by_pair"][1]["counters"]["buys_rejected_volume_share"] = json!(1);
        let erreur = check(&metrics, &manifest, &csv).unwrap_err();
        assert!(erreur.contains("signaux d'entrée"), "{erreur}");
    }

    #[test]
    fn plus_de_refus_au_fill_que_de_refus_d_entree_est_refuse() {
        let (mut metrics, manifest, csv) = coherent_run();
        metrics["sim_by_pair"][1]["counters"]["buys_rejected_below_minimum"] = json!(1);
        let erreur = check(&metrics, &manifest, &csv).unwrap_err();
        assert!(erreur.contains("ETH-USDC"), "{erreur}");
        assert!(erreur.contains("refus au fill"), "{erreur}");
    }

    #[test]
    fn des_expirations_en_trop_ou_en_moins_cassent_l_identite_2_en_agregat() {
        // Identité 2 en inégalité par paire : trop de lots et de refus au fill pour les ordres.
        let (mut metrics, manifest, csv) = coherent_run();
        metrics["funnel_by_pair"][0]["entry_fills"] = json!(7);
        metrics["funnel_by_pair"][1]["entry_fills"] = json!(0);
        let erreur = check(&metrics, &manifest, &csv).unwrap_err();
        assert!(erreur.contains("BTC-USDC"), "{erreur}");
        assert!(erreur.contains("lots ouverts"), "{erreur}");
        // Identité 2 exacte en agrégat : `other` ne vaut pas le reste des ordres.
        let (mut metrics, manifest, csv) = coherent_run();
        metrics["funnel"]["entry_not_filled_by_reason"]["other"] = json!(3);
        let erreur = check(&metrics, &manifest, &csv).unwrap_err();
        assert!(erreur.contains("other"), "{erreur}");
    }

    #[test]
    fn sim_by_pair_doit_couvrir_l_univers_dans_l_ordre() {
        let (mut metrics, manifest, csv) = coherent_run();
        metrics["sim_by_pair"][0]["symbol"] = json!("ETH-USDC");
        metrics["sim_by_pair"][1]["symbol"] = json!("BTC-USDC");
        let erreur = check(&metrics, &manifest, &csv).unwrap_err();
        assert!(erreur.contains("sim_by_pair"), "{erreur}");
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
