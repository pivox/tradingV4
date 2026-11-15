//! Contrat public du sidecar S13. Les noyaux gardent leurs DTO numériques ; la CLI
//! n'ajoute que la provenance possédée, la sélection et la couverture du corpus.
use super::{DelayedEntryStats, VerifiedRun};
use crate::report::stats_inputs::StatsProvenance;
use crate::research::{noise::NoiseContext, stats::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const SCHEMA: &str = "tv4-stats-v1";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section<T> {
    pub availability: Availability,
    pub value: Option<T>,
    pub reasons: Vec<String>,
}
impl<T> Section<T> {
    pub fn available(value: T) -> Self {
        Self {
            availability: Availability::Complete,
            value: Some(value),
            reasons: vec![],
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            availability: Availability::Unavailable,
            value: None,
            reasons: vec![reason.into()],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceFiles {
    pub manifest_json: String,
    pub metrics_json: String,
    pub trades_csv: String,
    pub equity_csv: String,
    pub stats_inputs_json: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub provenance: StatsProvenance,
    pub files: SourceFiles,
    pub layout_files: Vec<LayoutFile>,
    /// Copie de l'admission historique ; distincte de l'instantané courant.
    pub historical_governance: Option<Value>,
}
impl Source {
    pub fn from_run(run: &VerifiedRun, packet: Option<&[u8]>) -> Result<Self, String> {
        use crate::model::hash::blake3_prefixed as hash;
        Ok(Self {
            provenance: super::snapshot(run)?.provenance,
            files: SourceFiles {
                manifest_json: hash(run.manifest_bytes()),
                metrics_json: hash(run.metrics_bytes()),
                trades_csv: hash(run.trades_bytes()),
                equity_csv: hash(run.equity_bytes()),
                stats_inputs_json: packet.map(hash),
            },
            layout_files: vec![],
            historical_governance: run.manifest().get("governance").cloned(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutFile {
    pub file: String,
    pub hash: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SymbolSelection {
    pub btc: Option<String>,
    pub eth: Option<String>,
    pub quote: Option<String>,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub registry_hash: String,
    pub last_event_id: Option<u64>,
    pub declared_n: usize,
    pub registered_hashes: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusMember {
    pub source: Source,
    pub registry_member: Option<bool>,
    pub daily: DailyReturns,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Corpus {
    pub members: Vec<CorpusMember>,
    pub observed_configs: usize,
    pub complete_daily_configs: usize,
    pub declared_verified_n: Option<usize>,
    pub coverage: Option<f64>,
    pub reasons: Vec<String>,
    pub clustering: Section<ClusteringStats>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatsOutput {
    pub schema: String,
    pub options: StatsOptions,
    pub source: Source,
    pub layout_files: Vec<LayoutFile>,
    pub partition: ResearchPartition,
    pub selection: SymbolSelection,
    pub bootstrap: BootstrapStats,
    pub concentration: ConcentrationStats,
    pub monte_carlo: MonteCarloStats,
    pub regimes: RegimeStats,
    pub benchmarks: BenchmarkStats,
    pub delayed_entries: Section<DelayedEntryStats>,
    pub delayed_source: Option<Source>,
    pub registry: Section<RegistrySnapshot>,
    pub current_noise: Option<NoiseContext>,
    pub corpus: Corpus,
    pub methods: Vec<String>,
}
impl StatsOutput {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}
static TEMP_ID: AtomicU64 = AtomicU64::new(0);
/// Tous les calculs et toutes les sérialisations précèdent cet appel. Chaque fichier
/// est synchronisé puis renommé ; un fichier final ne contient jamais de préfixe partiel.
pub fn publish(outputs: &[(PathBuf, Vec<u8>)]) -> Result<(), String> {
    let mut staged = Vec::new();
    let result = (|| {
        for (dir, bytes) in outputs {
            let temp = dir.join(format!(
                ".stats.json.{}.{}.tmp",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = File::options()
                .create_new(true)
                .write(true)
                .open(&temp)
                .map_err(|e| format!("{} : {e}", temp.display()))?;
            staged.push((temp.clone(), dir.join("stats.json")));
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("{} : {e}", temp.display()))?;
        }
        for (temp, dest) in &staged {
            fs::rename(temp, dest).map_err(|e| format!("{} : {e}", dest.display()))?;
        }
        for (dir, _) in outputs {
            File::open(Path::new(dir))
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("{} : {e}", dir.display()))?;
        }
        Ok(())
    })();
    for (temp, _) in &staged {
        let _ = fs::remove_file(temp);
    }
    result
}
