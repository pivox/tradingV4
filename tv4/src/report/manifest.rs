//! `manifest.json`, schéma S8 (`tv4-manifest-s8`) : l'identité et la provenance d'un run.
//!
//! Écrit en dernier, après `metrics.json`, `trades.csv` et `equity.csv` : sa présence marque un run
//! complet. Il porte tout ce qui change d'un build ou d'une exécution à l'autre (commit, binaire,
//! compilateur, horloge, durées) et n'est donc jamais comparé à l'octet. Aucun chemin absolu n'y figure.
//!
//! Écarts à docs/03 §5.2 : `strategy`, `fees`, `slippage_bps`, `sim_rules` et `warmup_bars` sont dans
//! `config` (l'objet haché lui-même, sans duplication) ; `duration_ms` est remplacé par `timings` ;
//! ajouts `schema`, `hash_format`, `verdict`, `outputs`, `engine.rustc_commit` et `engine.profile`.

use serde::Serialize;

use crate::engine::RunConfig;
use crate::model::{ExchangeId, Timeframe, Ts, hash};
use crate::store::time::format_rfc3339_utc;

use super::ReportError;
use super::metrics::{DataInfo, Metrics};
use super::metrics_s9::{ConfigS9, DataS9, MetricsS9};

/// Identifiant du schéma.
pub const SCHEMA: &str = "tv4-manifest-s8";
/// Identifiant du schéma du mode portefeuille (docs S9 §14.1).
pub const SCHEMA_S9: &str = "tv4-manifest-s9";
/// Identifiant du schéma des runs à profils de coûts (docs/specs/s10.md §12.3).
pub const SCHEMA_S10: &str = "tv4-manifest-s10";
/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = 60_000;

/// Moteur qui a produit le run : version du paquet, commit, compilateur, cible et binaire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EngineInfo {
    pub tv4_version: String,
    pub git_sha: String,
    pub rustc: String,
    pub rustc_commit: String,
    pub target: String,
    pub target_cpu: String,
    pub profile: String,
    /// `blake3:<hex>` du binaire en cours d'exécution.
    pub binary_hash: String,
}

impl EngineInfo {
    /// Valeurs fixées à la compilation par `build.rs`, plus le hash du binaire lu par la CLI.
    pub fn current(binary_hash: String) -> EngineInfo {
        EngineInfo {
            tv4_version: env!("CARGO_PKG_VERSION").to_string(),
            git_sha: crate::GIT_SHA.to_string(),
            rustc: crate::RUSTC_VERSION.to_string(),
            rustc_commit: crate::RUSTC_COMMIT.to_string(),
            target: crate::TARGET.to_string(),
            target_cpu: crate::TARGET_CPU.to_string(),
            profile: crate::BUILD_PROFILE.to_string(),
            binary_hash,
        }
    }
}

/// Données rejouées, telles que le manifeste les décrit.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ManifestData {
    pub source: &'static str,
    pub dataset: Option<String>,
    /// BLAKE3 hexadécimal nu des octets du CSV (fixture) ; `null` pour un dataset.
    pub fixture_blake3: Option<String>,
    pub exchange: ExchangeId,
    pub symbol: String,
    pub tf: Timeframe,
    pub gaps_policy: String,
    pub bars: u64,
    pub synthetic_bars: u64,
    pub first_open_time_ms: Ts,
    pub last_open_time_ms: Ts,
}

/// Fenêtre jouée (après le warmup) : `end` est exclue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ManifestWindow {
    pub start: String,
    pub end: String,
    pub start_ms: Ts,
    pub end_ms: Ts,
}

/// Empreintes des trois fichiers du run, `blake3:<hex>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputFiles {
    #[serde(rename = "equity.csv")]
    pub equity_csv: String,
    #[serde(rename = "metrics.json")]
    pub metrics_json: String,
    #[serde(rename = "trades.csv")]
    pub trades_csv: String,
}

/// Fichiers écrits et pas d'échantillonnage de la courbe d'equity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Outputs {
    pub equity_step: Timeframe,
    pub files: OutputFiles,
}

/// Durées mesurées par la CLI, en millisecondes. Jamais comparées par `tv4 verify`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Timings {
    /// De l'entrée de la commande à l'appel de `run_cell` (lecture et vérification des données).
    pub load_ms: u64,
    /// Durée de `run_cell`.
    pub run_ms: u64,
    /// De l'entrée de la commande à la fin de l'écriture de `equity.csv`.
    pub cli_cold_ms: u64,
}

/// Contenu de `manifest.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Manifest {
    pub schema: &'static str,
    pub run_id: String,
    pub config_hash: String,
    pub dataset_hash: String,
    pub hash_format: String,
    /// `blake3:` + BLAKE3 des octets de `trades.csv`.
    pub result_hash: String,
    pub verdict: &'static str,
    pub config: RunConfig,
    pub data: ManifestData,
    pub window: ManifestWindow,
    pub engine: EngineInfo,
    pub outputs: Outputs,
    pub started_at: String,
    pub timings: Timings,
}

/// Ce que la CLI fournit en plus des métriques pour écrire le manifeste.
#[derive(Clone, Debug)]
pub struct ManifestInputs {
    pub hash_format: String,
    pub fixture_blake3: Option<String>,
    pub engine: EngineInfo,
    pub equity_step: Timeframe,
    /// `blake3:<hex>` des octets de `metrics.json`, `trades.csv` et `equity.csv`.
    pub files: OutputFiles,
    pub started_at_ms: Ts,
    pub timings: Timings,
}

/// `run_id` : BLAKE3 hexadécimal de `"{config_hash}|{dataset_hash}|{start_ms},{end_ms}"`.
pub fn run_id(config_hash: &str, dataset_hash: &str, start_ms: Ts, end_ms: Ts) -> String {
    hash::blake3_hex(format!("{config_hash}|{dataset_hash}|{start_ms},{end_ms}").as_bytes())
}

/// `run_id` du mode portefeuille : BLAKE3 hexadécimal de
/// `"{config_hash}|{dataset_hash}|{start_ms},{end_ms}|{initial_cash:?}"`.
///
/// Le capital initial n'est pas dans le `config_hash` du portefeuille (décision du 28/10) : il entre ici,
/// pour que deux runs de capitaux différents n'écrivent pas dans le même dossier par défaut.
pub fn run_id_s9(
    config_hash: &str,
    dataset_hash: &str,
    start_ms: Ts,
    end_ms: Ts,
    initial_cash: f64,
) -> String {
    hash::blake3_hex(
        format!("{config_hash}|{dataset_hash}|{start_ms},{end_ms}|{initial_cash:?}").as_bytes(),
    )
}

fn data_of(data: &DataInfo, fixture_blake3: Option<String>) -> ManifestData {
    ManifestData {
        source: data.source,
        dataset: data.dataset.clone(),
        fixture_blake3,
        exchange: data.exchange,
        symbol: data.symbol.clone(),
        tf: data.tf,
        gaps_policy: data.gaps_policy.clone(),
        bars: data.bars,
        synthetic_bars: data.synthetic_bars,
        first_open_time_ms: data.first_open_time_ms,
        last_open_time_ms: data.last_open_time_ms,
    }
}

impl Manifest {
    /// Construit le manifeste d'un run à partir de ses métriques.
    pub fn new(metrics: &Metrics, inputs: ManifestInputs) -> Result<Manifest, ReportError> {
        let timestamp =
            |ts_ms: Ts| format_rfc3339_utc(ts_ms).map_err(|_| ReportError::Timestamp { ts_ms });
        let start_ms = metrics.window.first_open_time_ms;
        let end_ms = metrics.window.last_open_time_ms + MINUTE_MS;
        let result_hash = inputs.files.trades_csv.clone();
        Ok(Manifest {
            schema: SCHEMA,
            run_id: run_id(
                &metrics.config_hash,
                &metrics.dataset_hash,
                start_ms,
                end_ms,
            ),
            config_hash: metrics.config_hash.clone(),
            dataset_hash: metrics.dataset_hash.clone(),
            hash_format: inputs.hash_format,
            result_hash,
            verdict: metrics.verdict,
            config: metrics.config.clone(),
            data: data_of(&metrics.data, inputs.fixture_blake3),
            window: ManifestWindow {
                start: timestamp(start_ms)?,
                end: timestamp(end_ms)?,
                start_ms,
                end_ms,
            },
            engine: inputs.engine,
            outputs: Outputs {
                equity_step: inputs.equity_step,
                files: inputs.files,
            },
            started_at: timestamp(inputs.started_at_ms)?,
            timings: inputs.timings,
        })
    }
}

/// `profiles_id` : BLAKE3 hexadécimal de `"{run_id_baseline}|{run_id_adverse}|{run_id_maker}"` (docs/specs/
/// s10.md §12.4), dans cet ordre. Il identifie le dossier de profils.
pub fn profiles_id(baseline: &str, adverse: &str, maker: &str) -> String {
    hash::blake3_hex(format!("{baseline}|{adverse}|{maker}").as_bytes())
}

/// Contenu de `manifest.json` du mode portefeuille (`tv4-manifest-s9`), et des runs à profils de coûts
/// (`tv4-manifest-s10`, qui réutilise cette struct, docs/specs/s10.md §12.3).
///
/// Mêmes champs d'identité et de provenance que S8, plus le capital initial (hors du `config_hash`) et
/// le fichier d'instruments utilisé. Le bloc `data` est celui de `metrics.json`. Les six champs de coûts
/// ne sont écrits qu'en s10 : un manifeste s9 est identique à l'octet à celui d'avant S10.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ManifestS9 {
    pub schema: &'static str,
    pub run_id: String,
    pub config_hash: String,
    pub dataset_hash: String,
    pub hash_format: String,
    /// `blake3:` + BLAKE3 des octets de `trades.csv`.
    pub result_hash: String,
    pub verdict: &'static str,
    /// Capital initial : hors du `config_hash`, mais comparé par `tv4 verify`.
    pub initial_cash: f64,
    /// `instruments_hash` du fichier OKX ; `null` pour les règles TOML.
    pub instruments_hash: Option<String>,
    /// Chemin du fichier d'instruments tel que donné à la CLI ; `null` pour les règles TOML.
    pub instruments_file: Option<String>,
    pub config: ConfigS9,
    pub data: DataS9,
    pub window: ManifestWindow,
    pub engine: EngineInfo,
    pub outputs: Outputs,
    pub started_at: String,
    pub timings: Timings,
    /// Nom du profil de coûts (s10) : `baseline`, `adverse` ou `maker`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<&'static str>,
    /// Chemin du fichier de coûts tel que donné à la CLI (s10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub costs_file: Option<String>,
    /// BLAKE3 hexadécimal des octets du fichier de coûts (s10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub costs_file_hash: Option<String>,
    /// Chemin du fichier de mesures de coûts tel que donné à la CLI (s10, facultatif).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_measures_file: Option<String>,
    /// BLAKE3 hexadécimal des octets du fichier de mesures (s10, facultatif).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_measures_hash: Option<String>,
    /// Identifiant du dossier de profils auquel ce run appartient (s10, [`profiles_id`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profiles_id: Option<String>,
}

/// Ce que la CLI fournit en plus pour écrire le manifeste d'un profil s10.
#[derive(Clone, Debug)]
pub struct ManifestS10Inputs {
    /// Les entrées communes au manifeste s9.
    pub base: ManifestS9Inputs,
    /// Chemin du fichier de coûts tel que donné à la CLI.
    pub costs_file: String,
    /// Chemin du fichier de mesures tel que donné, s'il y en a un.
    pub cost_measures_file: Option<String>,
    /// [`profiles_id`] du dossier de profils.
    pub profiles_id: String,
}

/// Ce que la CLI fournit en plus des métriques pour écrire le manifeste du portefeuille.
#[derive(Clone, Debug)]
pub struct ManifestS9Inputs {
    pub instruments_file: Option<String>,
    pub engine: EngineInfo,
    pub equity_step: Timeframe,
    /// `blake3:<hex>` des octets de `metrics.json`, `trades.csv` et `equity.csv`.
    pub files: OutputFiles,
    pub started_at_ms: Ts,
    pub timings: Timings,
}

impl ManifestS9 {
    /// Construit le manifeste d'un run portefeuille à partir de ses métriques.
    pub fn new(metrics: &MetricsS9, inputs: ManifestS9Inputs) -> Result<ManifestS9, ReportError> {
        let timestamp =
            |ts_ms: Ts| format_rfc3339_utc(ts_ms).map_err(|_| ReportError::Timestamp { ts_ms });
        let window = &metrics.data.window;
        let result_hash = inputs.files.trades_csv.clone();
        Ok(ManifestS9 {
            schema: SCHEMA_S9,
            run_id: metrics.run_id(),
            config_hash: metrics.config_hash.clone(),
            dataset_hash: metrics.dataset_hash.clone(),
            hash_format: metrics.data.hash_format.clone(),
            result_hash,
            verdict: metrics.verdict,
            initial_cash: metrics.config.initial_cash,
            instruments_hash: metrics.instruments.instruments_hash.clone(),
            instruments_file: inputs.instruments_file,
            config: metrics.config.clone(),
            data: metrics.data.clone(),
            window: ManifestWindow {
                start: window.start.clone(),
                end: window.end.clone(),
                start_ms: window.start_ms,
                end_ms: window.end_ms,
            },
            engine: inputs.engine,
            outputs: Outputs {
                equity_step: inputs.equity_step,
                files: inputs.files,
            },
            started_at: timestamp(inputs.started_at_ms)?,
            timings: inputs.timings,
            profile: None,
            costs_file: None,
            costs_file_hash: None,
            cost_measures_file: None,
            cost_measures_hash: None,
            profiles_id: None,
        })
    }

    /// Construit le manifeste d'un profil s10 à partir de ses métriques (`tv4-metrics-s10`).
    ///
    /// Le profil et les hashes des fichiers de coûts viennent de `metrics.costs`, pour qu'ils ne puissent
    /// pas diverger de `metrics.json`. Refuse des métriques qui n'ont pas de bloc `costs`.
    pub fn new_s10(
        metrics: &MetricsS9,
        inputs: ManifestS10Inputs,
    ) -> Result<ManifestS9, ReportError> {
        let Some(costs) = metrics.costs.as_ref() else {
            return Err(ReportError::Series {
                detail: "les métriques n'ont pas de bloc `costs` : ce n'est pas un run s10"
                    .to_string(),
            });
        };
        let mut manifest = ManifestS9::new(metrics, inputs.base)?;
        manifest.schema = SCHEMA_S10;
        manifest.profile = Some(costs.profile.as_str());
        manifest.costs_file = Some(inputs.costs_file);
        manifest.costs_file_hash = Some(costs.costs_file_hash.clone());
        manifest.cost_measures_file = inputs.cost_measures_file;
        manifest.cost_measures_hash = costs.cost_measures_hash.clone();
        manifest.profiles_id = Some(inputs.profiles_id);
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_s9_depend_du_capital() {
        let a = run_id_s9("aa", "blake3:bb", 60_000, 120_000, 300.0);
        assert_eq!(a.len(), 64);
        assert_eq!(
            a,
            hash::blake3_hex(b"aa|blake3:bb|60000,120000|300.0"),
            "format documente"
        );
        assert_ne!(a, run_id_s9("aa", "blake3:bb", 60_000, 120_000, 3_000.0));
        assert_ne!(a, run_id("aa", "blake3:bb", 60_000, 120_000));
    }

    #[test]
    fn profiles_id_est_le_blake3_des_trois_run_id() {
        assert_eq!(
            profiles_id("aa", "bb", "cc"),
            hash::blake3_hex(b"aa|bb|cc"),
            "format documente"
        );
        assert_ne!(profiles_id("aa", "bb", "cc"), profiles_id("bb", "aa", "cc"));
    }

    #[test]
    fn run_id_est_le_blake3_de_la_chaine_documentee() {
        let id = run_id("aa", "blake3:bb", 60_000, 120_000);
        assert_eq!(id, hash::blake3_hex(b"aa|blake3:bb|60000,120000"));
        assert_eq!(id.len(), 64);
        assert_ne!(id, run_id("aa", "blake3:bb", 60_000, 180_000));
        assert_ne!(id, run_id("ab", "blake3:bb", 60_000, 120_000));
    }

    #[test]
    fn run_id_chaine_epinglee() {
        // Chaîne `a|blake3:b|1,2` : valeur recoupée avec le module Python `blake3`.
        assert_eq!(
            run_id("a", "blake3:b", 1, 2),
            "2e90b3a1a1bc3ec2a176431b99b355efe50844195473167f9daca361b7a7ead7"
        );
    }

    #[test]
    fn les_noms_de_fichiers_sont_ceux_du_schema() {
        let files = OutputFiles {
            equity_csv: "blake3:e".into(),
            metrics_json: "blake3:m".into(),
            trades_csv: "blake3:t".into(),
        };
        let json = hash::canonical_json(&files).unwrap();
        assert_eq!(
            json,
            r#"{"equity.csv":"blake3:e","metrics.json":"blake3:m","trades.csv":"blake3:t"}"#
        );
    }
}
