//! Projections versionnées : un programme précède les prix, son exécution lie les données.
use crate::{
    model::{ModelError, hash},
    research::{
        controls::{AnnualQuota, ControlMode, ProgramBudgets, validate_quotas},
        stats::{StatsSizing, Window},
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const PROGRAM_SCHEMA: &str = "tv4-control-program-v1";
const ALGORITHM: &str = "interval-year-dp-biguint-v1";
const QUOTA_RULE: &str = "entry-fill-utc-filled-retained-v1";

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identité de contrôle : {0}")]
    Invalid(String),
    #[error(transparent)]
    Model(#[from] ModelError),
}
fn invalid(reason: &str) -> IdentityError {
    IdentityError::Invalid(reason.into())
}
fn check_hash(value: &str) -> Result<(), IdentityError> {
    let value = value.strip_prefix("blake3:").unwrap_or(value);
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("hash canonique 64 hex minuscules requis"));
    }
    Ok(())
}

/// La graine reste un u64 en mémoire et une chaîne décimale canonique en JSON.
mod decimal_seed {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        let seed = value.parse::<u64>().map_err(D::Error::custom)?;
        if seed.to_string() != value {
            return Err(D::Error::custom("graine décimale non canonique"));
        }
        Ok(seed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NullProgramIdentity {
    schema: String,
    parent_config_hash: String,
    geometry_hash: String,
    effective_intervention: ControlMode,
    #[serde(with = "decimal_seed")]
    master_seed: u64,
    replication_index: u32,
    algorithm: String,
    program_budgets: ProgramBudgets,
    quota_rule: String,
}
impl NullProgramIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        parent_config_hash: String,
        geometry_hash: String,
        mode: ControlMode,
        has_btc_filter: bool,
        master_seed: u64,
        replication_index: u32,
        program_budgets: ProgramBudgets,
    ) -> Result<Self, IdentityError> {
        let value = Self {
            schema: PROGRAM_SCHEMA.into(),
            parent_config_hash,
            geometry_hash,
            effective_intervention: mode.effective(has_btc_filter),
            master_seed,
            replication_index,
            algorithm: ALGORITHM.into(),
            program_budgets,
            quota_rule: QUOTA_RULE.into(),
        };
        value.validate()?;
        Ok(value)
    }
    pub fn parent_config_hash(&self) -> &str {
        &self.parent_config_hash
    }
    pub fn geometry_hash(&self) -> &str {
        &self.geometry_hash
    }
    pub fn effective_intervention(&self) -> ControlMode {
        self.effective_intervention
    }
    pub fn master_seed(&self) -> u64 {
        self.master_seed
    }
    pub fn replication_index(&self) -> u32 {
        self.replication_index
    }
    pub fn program_budgets(&self) -> ProgramBudgets {
        self.program_budgets
    }
    pub fn validate(&self) -> Result<(), IdentityError> {
        if self.schema != PROGRAM_SCHEMA
            || self.algorithm != ALGORITHM
            || self.quota_rule != QUOTA_RULE
        {
            return Err(invalid("domaine, algorithme ou règle de quota inconnus"));
        }
        // Le registre emploie la forme nue ; un même programme n'a pas deux orthographes.
        if self.parent_config_hash.starts_with("blake3:")
            || self.geometry_hash.starts_with("blake3:")
        {
            return Err(invalid("hashes de programme nus requis"));
        }
        check_hash(&self.parent_config_hash)?;
        check_hash(&self.geometry_hash)?;
        Ok(())
    }
    pub fn config_hash(&self) -> Result<String, IdentityError> {
        self.validate()?;
        Ok(hash::hash_json(self)?)
    }
}

/// Les cinq octets sources sont liés séparément, y compris le manifeste historique.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHashes {
    pub metrics: String,
    pub trades: String,
    pub equity: String,
    pub stats_inputs: String,
    pub manifest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub config_hash: String,
    pub run_id: String,
    pub hashes: SourceHashes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlLine {
    Full,
    OkxUsdc,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlProfile {
    Baseline,
    Adverse,
    Maker,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineIdentity {
    pub commit: String,
    pub binary_hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostIdentity {
    pub config_hash: String,
    pub seed: u64,
}

/// L'ordre des sources et des programmes est normalisé avant hachage, jamais l'ordre causal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRunIdentity {
    pub program_hashes: Vec<String>,
    pub sources: Vec<SourceIdentity>,
    pub catalogue_hash: String,
    pub quotas: BTreeMap<String, BTreeMap<String, Vec<AnnualQuota>>>,
    pub dataset_hash: String,
    pub window: Window,
    /// Capital du vrai run source, sans tolérance ULP du paquet statistique.
    pub initial_cash: f64,
    pub sizing: StatsSizing,
    pub profile: Option<ControlProfile>,
    pub line: Option<ControlLine>,
    pub costs: Option<CostIdentity>,
    pub engine: EngineIdentity,
    pub total_engine_steps: u64,
}
impl ControlRunIdentity {
    pub fn control_run_id(&self) -> Result<String, IdentityError> {
        let mut value = self.clone();
        value.program_hashes.sort();
        value
            .sources
            .sort_by(|a, b| a.config_hash.cmp(&b.config_hash));
        if value.program_hashes.is_empty()
            || value.sources.is_empty()
            || value.sources.len() > 27
            || value.program_hashes.windows(2).any(|p| p[0] == p[1])
            || value
                .sources
                .windows(2)
                .any(|p| p[0].config_hash == p[1].config_hash)
        {
            return Err(invalid(
                "programmes/sources requis, distincts, sources bornées à 27",
            ));
        }
        if !value.initial_cash.is_finite()
            || value.initial_cash <= 0.
            || value.window.start_ms < 0
            || value.window.end_ms <= value.window.start_ms
        {
            return Err(invalid("capital, fenêtre ou plafond global invalide"));
        }
        for h in &value.program_hashes {
            check_hash(h)?;
        }
        for s in &value.sources {
            for h in [
                &s.config_hash,
                &s.run_id,
                &s.hashes.metrics,
                &s.hashes.trades,
                &s.hashes.equity,
                &s.hashes.stats_inputs,
                &s.hashes.manifest,
            ] {
                check_hash(h)?;
            }
        }
        for h in [
            &value.catalogue_hash,
            &value.dataset_hash,
            &value.engine.binary_hash,
        ] {
            check_hash(h)?;
        }
        if value.engine.commit.is_empty() {
            return Err(invalid("commit moteur absent"));
        }
        if let Some(costs) = &value.costs {
            check_hash(&costs.config_hash)?;
            if costs.seed >= crate::model::costs::MAX_COST_SEED {
                return Err(invalid("graine de coûts hors limites"));
            }
        }
        if value
            .quotas
            .keys()
            .ne(value.sources.iter().map(|s| &s.config_hash))
        {
            return Err(invalid(
                "quotas requis pour chaque parent source exactement",
            ));
        }
        for pairs in value.quotas.values() {
            for (symbol, quotas) in pairs {
                if symbol.is_empty() {
                    return Err(invalid("symbole de quota vide"));
                }
                validate_quotas(quotas).map_err(|_| invalid("quotas invalides"))?;
            }
        }
        Ok(hash::hash_json(&("tv4-control-run-v1", value))?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayIdentity {
    pub control_run_id: String,
    pub program_hash: String,
    pub attempt: u32,
    /// Trace native : engage les calendriers effectifs et les événements/quantités du moteur.
    pub replay_trace_hash: String,
}
impl ReplayIdentity {
    pub fn replay_hash(&self) -> Result<String, IdentityError> {
        for h in [
            &self.control_run_id,
            &self.program_hash,
            &self.replay_trace_hash,
        ] {
            check_hash(h)?;
        }
        Ok(hash::hash_json(&("tv4-control-replay-v1", self))?)
    }
}
