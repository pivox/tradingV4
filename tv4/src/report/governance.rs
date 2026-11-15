//! Provenance de l'admission ; hors identité de simulation et config_hash.
use crate::{model::Block, research::noise::NoiseContext};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HypothesisContext {
    pub identifiant: String,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GovernanceContext {
    pub schema: String,
    pub config_hash: String,
    pub dataset_hash: String,
    pub trial_id: u64,
    pub read_id: u64,
    pub hypothesis: Option<HypothesisContext>,
    pub frozen_tag: Option<String>,
    pub frozen_at: Option<String>,
    pub source_block: Option<Block>,
    pub effective_block: Option<Block>,
    pub generation: u32,
    pub claim_holdout: Option<u64>,
    pub noise: NoiseContext,
}
