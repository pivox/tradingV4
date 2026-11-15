//! Contrats possédés S14 : les temps et l'admission courante restent hors du numérique.
use super::identity::*;
use crate::research::{controls::*, stats::StatsSizing};
use crate::{model::Block, report::governance::GovernanceContext, research::controls as numeric};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CONTROLS_FILE: &str = "controls.json";
pub const MANIFEST_FILE: &str = "controls-manifest.json";
pub const INDEX_FILE: &str = "controls-index.json";
pub const SCHEMA: &str = "tv4-controls-v1";
pub const MANIFEST_SCHEMA: &str = "tv4-controls-manifest-v1";
pub const INDEX_SCHEMA: &str = "tv4-controls-index-v1";
pub const CONVENTIONS: &str =
    "native-book_fill-year-utc_single-lot_net-r_best-n-upper-rank_circular-month-b10000-v1";
pub const REGIME_CONVENTIONS: &str = "native-regime-feed_closed-daily-sma200-v1";

macro_rules! dto {
    ($name:ident {$($field:ident:$ty:ty),* $(,)?}) => {
        #[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {$(#[serde(deserialize_with="required_field")] pub $field:$ty),*}
    };
}
fn required_field<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<T, D::Error> {
    T::deserialize(d)
}
macro_rules! mirror {
    ($name:ident,$source:path,{$($field:ident:$ty:ty),* $(,)?}) => {
        dto!($name {$($field:$ty),*});
        impl From<&$source> for $name {
            fn from(value:&$source)->Self {Self {$($field:value.$field.to_owned()),*}}
        }
    };
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Panel,
    Projection,
}

dto!(SourceMetadata {
    config_hash: String,
    geometry_hash: String,
    has_btc_filter: bool
});
dto!(ControlsWindow {
    start_ms: i64,
    end_ms: i64
});
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlsOptions {
    #[serde(with = "decimal_seed")]
    pub seed: u64,
    pub replications: u32,
    pub modes: Vec<ControlMode>,
    pub budgets: Budgets,
}
impl From<&ControlOptions> for ControlsOptions {
    fn from(value: &ControlOptions) -> Self {
        let mut modes = value.modes.clone();
        modes.sort();
        Self {
            seed: value.seed,
            replications: value.replications,
            modes,
            budgets: value.budgets,
        }
    }
}
impl ControlsOptions {
    pub fn as_engine(&self) -> ControlOptions {
        ControlOptions {
            seed: self.seed,
            replications: self.replications,
            modes: self.modes.clone(),
            budgets: self.budgets,
        }
    }
}
mod decimal_seed {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let text = String::deserialize(d)?;
        let value = text.parse::<u64>().map_err(D::Error::custom)?;
        if text != value.to_string() {
            return Err(D::Error::custom("graine décimale non canonique"));
        }
        Ok(value)
    }
}
dto!(ControlsContext {
    engine:EngineIdentity,dataset_hash:String,window:ControlsWindow,initial_cash:f64,sizing:StatsSizing,
    line:Option<ControlLine>,profile:Option<ControlProfile>,costs:Option<CostIdentity>,options:ControlsOptions,
    source_block:Option<Block>,effective_block:Option<Block>
});

/// Capture fournie par l'admission CLI, jamais relue depuis le registre par report.
pub struct CurrentAdmission {
    pub contexts: BTreeMap<String, GovernanceContext>,
    pub candidates: u64,
    pub null_controls: u64,
}
mirror!(HypothesisRecord,crate::report::governance::HypothesisContext,{identifiant:String,sha256:String});
mirror!(NoiseRecord,crate::research::noise::NoiseContext,{
    n:u64,n_eff:Option<u64>,selection_years:f64,expected:Option<f64>,approx_q90:Option<f64>,q95:Option<f64>
});
dto!(AdmissionRecord {
    schema:String,config_hash:String,dataset_hash:String,trial_id:u64,read_id:u64,
    hypothesis:Option<HypothesisRecord>,frozen_tag:Option<String>,frozen_at:Option<String>,
    source_block:Option<Block>,effective_block:Option<Block>,generation:u32,claim_holdout:Option<u64>,noise:NoiseRecord
});
impl From<&GovernanceContext> for AdmissionRecord {
    fn from(g: &GovernanceContext) -> Self {
        Self {
            schema: g.schema.clone(),
            config_hash: g.config_hash.clone(),
            dataset_hash: g.dataset_hash.clone(),
            trial_id: g.trial_id,
            read_id: g.read_id,
            hypothesis: g.hypothesis.as_ref().map(Into::into),
            frozen_tag: g.frozen_tag.clone(),
            frozen_at: g.frozen_at.clone(),
            source_block: g.source_block,
            effective_block: g.effective_block,
            generation: g.generation,
            claim_holdout: g.claim_holdout,
            noise: (&g.noise).into(),
        }
    }
}
dto!(ControlsSource {
    identity:SourceIdentity,geometry_hash:String,has_btc_filter:bool,cost_seed:Option<u64>,regime_conventions:String,
    quotas:BTreeMap<String,Vec<AnnualQuota>>,historical_admission:Option<AdmissionRecord>
});
mirror!(MonthMomentDto,numeric::MonthMoment,{year:i32,month:u8,r_sum:f64,count:u32});
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonthCoverageKind {
    Complete,
    Partial,
    Excluded,
}
dto!(MonthCoverageDto {
    year: i32,
    month: u8,
    status: MonthCoverageKind
});
impl From<&MonthCoverage> for MonthCoverageDto {
    fn from(m: &MonthCoverage) -> Self {
        Self {
            year: m.year,
            month: m.month,
            status: match m.status {
                MonthCoverageStatus::Complete => MonthCoverageKind::Complete,
                MonthCoverageStatus::Partial => MonthCoverageKind::Partial,
                MonthCoverageStatus::Excluded => MonthCoverageKind::Excluded,
            },
        }
    }
}
mirror!(PairQuotaDto,numeric::PairQuotaCheck,{symbol:String,expected:Vec<AnnualQuota>,actual:Vec<AnnualQuota>,conformant:bool});
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    OccupiedPair,
    TraceMismatch,
    QuotaMismatch,
}
dto!(AttemptRejectionDto {
    attempt: u32,
    reason: RejectionReason
});
impl From<&AttemptRejection> for AttemptRejectionDto {
    fn from(r: &AttemptRejection) -> Self {
        Self {
            attempt: r.attempt,
            reason: match r.reason {
                ReplayRejection::OccupiedPair => RejectionReason::OccupiedPair,
                ReplayRejection::TraceMismatch => RejectionReason::TraceMismatch,
                ReplayRejection::QuotaMismatch => RejectionReason::QuotaMismatch,
            },
        }
    }
}
dto!(CellDto {
    replication:u32,program:NullProgramIdentity,config_hash:String,control_run_id:String,replay_hash:Option<String>,
    accepted_attempt:Option<u32>,attempts:u32,availability:CellAvailability,reason:Option<UnavailableReason>,
    score:Option<f64>,months:Vec<MonthMomentDto>,quota_checks:Vec<PairQuotaDto>,quantity_changed:bool,
    replay_trace_hash:Option<String>,rejections:Vec<AttemptRejectionDto>
});
mirror!(PairSupportDto,numeric::PairSupportSummary,{symbol:String,cardinality:Option<String>});
dto!(ModeResult {
    mode:ControlMode,alias_of:Option<ControlMode>,pair_supports:Vec<PairSupportDto>,planning_reason:Option<UnavailableReason>,cells:Vec<CellDto>
});
dto!(ObservedDto {score:Option<f64>,reason:Option<UnavailableReason>,months:Vec<MonthMomentDto>});
dto!(SourceResult {config_hash:String,observed:ObservedDto,month_coverage:Vec<MonthCoverageDto>,modes:Vec<ModeResult>});
mirror!(BootstrapDto,numeric::BootstrapSummary,{
    available:bool,reason:Option<UnavailableReason>,approximate:bool,samples:u32,months:u32,complete_months:u32,
    point:Option<f64>,standard_error:Option<f64>,lb90:Option<f64>,lb90_positive:Option<bool>
});
dto!(SourceStatisticsDto {
    config_hash:String,score:Option<f64>,control_mean:Option<f64>,delta:Option<f64>,mc_standard_error:Option<f64>,constant_controls:bool,
    exceedances:Option<u32>,ties:Option<u32>,p_value:Option<f64>,delta_at_least_010:Option<bool>,above_critical:Option<bool>,temporal:Option<BootstrapDto>
});
impl SourceStatisticsDto {
    pub(super) fn from_source(config_hash: String, s: &SourceStatistics) -> Self {
        Self {
            config_hash,
            score: s.score,
            control_mean: s.control_mean,
            delta: s.delta,
            mc_standard_error: s.mc_standard_error,
            constant_controls: s.constant_controls,
            exceedances: s.exceedances,
            ties: s.ties,
            p_value: s.p_value,
            delta_at_least_010: s.delta_at_least_010,
            above_critical: s.above_critical,
            temporal: s.temporal.as_deref().map(Into::into),
        }
    }
}
dto!(ModeStatisticsDto {
    mode:ControlMode,alias_of:Option<ControlMode>,status:PanelStatus,n_grid:u32,replications:u32,smoke:bool,numeric_admissible:bool,
    resolution:f64,critical_rank:u64,critical_value:Option<f64>,maxima:Vec<f64>,family_score:Option<f64>,family_exceedances:Option<u32>,
    family_p_value:Option<f64>,sources:Vec<SourceStatisticsDto>
});
dto!(ControlsDocument {
    schema:String,kind:DocumentKind,conventions:String,control_run_id:String,panel_hash:Option<String>,canonical_reference:String,
    context:ControlsContext,sources:Vec<ControlsSource>,status:PanelStatus,reasons:Vec<UnavailableReason>,n_grid:u32,replications:u32,
    smoke:bool,technical_only:bool,catalogue_hash:String,results:Vec<SourceResult>,statistics:Vec<ModeStatisticsDto>
});
dto!(FileRef {
    file: String,
    hash: String
});
dto!(RoleCountsDto {
    candidates: u64,
    null_controls: u64
});
dto!(ControlsManifest {
    schema:String,kind:DocumentKind,control_run_id:String,numeric:FileRef,sources:Vec<SourceIdentity>,
    read_id:u64,n_registre:u64,role_counts:RoleCountsDto,admissions:Vec<AdmissionRecord>,panel_hash:Option<String>,projection:Option<String>
});
dto!(IndexSource {
    config_hash: String,
    dir: String,
    controls: FileRef,
    manifest: FileRef
});
dto!(ControlsIndex {
    schema:String,complete:bool,control_run_id:String,canonical_reference:String,panel:FileRef,manifest:FileRef,sources:Vec<IndexSource>
});
