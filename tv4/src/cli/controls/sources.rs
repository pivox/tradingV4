//! Les feuilles sont contrôlées et conservées ; aucune recette historique n'est suivie.
use super::{error, plan::LoadedPlan, usage};
use crate::{
    cli::CliError,
    model::Block,
    report::{
        controls::{ControlsContext, ControlsWindow, identity::*},
        governance::GovernanceContext,
        stats::{self, RunSnapshot},
        verify::VerifiedRun,
    },
    research::stats::StatsSizing,
};
use serde_json::Value;
use std::path::PathBuf;
pub struct Sources {
    pub runs: Vec<VerifiedRun>,
    pub snapshots: Vec<RunSnapshot>,
    pub context: ControlsContext,
}
pub fn value<'a>(v: &'a Value, pointer: &str) -> Result<&'a Value, CliError> {
    v.pointer(pointer)
        .ok_or_else(|| error(format!("champ source absent : {pointer}")))
}
pub fn string(v: &Value, pointer: &str) -> Result<String, CliError> {
    value(v, pointer)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| error(format!("chaîne source absente : {pointer}")))
}
pub fn governance(run: &VerifiedRun) -> Result<Option<GovernanceContext>, CliError> {
    run.manifest()
        .get("governance")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()).map_err(error))
        .transpose()
}
impl Sources {
    pub fn load(paths: &[PathBuf], plan: &LoadedPlan) -> Result<Self, CliError> {
        let mut pairs = paths
            .iter()
            .map(|p| {
                let r = stats::read_run(p)?;
                let s = stats::snapshot(&r).map_err(error)?;
                if r.stats_inputs().is_none() {
                    return Err(usage("stats-inputs.json obligatoire"));
                }
                Ok((r, s))
            })
            .collect::<Result<Vec<_>, CliError>>()?;
        pairs.sort_by(|a, b| a.1.provenance.config_hash.cmp(&b.1.provenance.config_hash));
        if pairs.len() != plan.plan.parents.len()
            || pairs.iter().map(|(_, s)| &s.provenance.config_hash).ne(plan
                .plan
                .parents
                .iter()
                .map(|p| &p.config_hash))
        {
            return Err(usage("roster des sources différent du plan"));
        }
        let (r, s) = pairs.first().ok_or_else(|| usage("panneau vide"))?;
        let g = governance(r)?;
        let block = r
            .metrics()
            .pointer("/research/block")
            .map(|v| serde_json::from_value::<Block>(v.clone()).map_err(error))
            .transpose()?;
        let source = g.as_ref().map_or(block, |g| g.source_block);
        let effective = g.as_ref().map_or(block, |g| g.effective_block);
        let costs = r
            .metrics()
            .get("costs")
            .filter(|c| !c.is_null())
            .map(|c| {
                Ok::<_, CliError>(CostIdentity {
                    config_hash: string(c, "/costs_file_hash")?,
                    seed: value(c, "/seed")?
                        .as_u64()
                        .ok_or_else(|| error("cost seed invalide"))?,
                })
            })
            .transpose()?;
        let context = ControlsContext {
            engine: EngineIdentity {
                commit: crate::GIT_SHA.into(),
                binary_hash: crate::cli::research::binary_hash()?,
            },
            dataset_hash: s.provenance.dataset_hash.clone(),
            window: ControlsWindow {
                start_ms: s.provenance.window.start_ms,
                end_ms: s.provenance.window.end_ms,
            },
            initial_cash: s.provenance.initial_cash,
            sizing: s.sizing,
            line: plan.plan.line,
            profile: plan.plan.profile,
            costs,
            options: plan.report_options(),
            source_block: source,
            effective_block: effective,
        };
        let expected_profile = plan.plan.profile.map(|p| match p {
            ControlProfile::Baseline => "baseline",
            ControlProfile::Adverse => "adverse",
            ControlProfile::Maker => "maker",
        });
        let expected_line = plan.plan.line.map(|l| match l {
            ControlLine::Full => "full",
            ControlLine::OkxUsdc => "okx_usdc",
        });
        if context.window != plan.plan.window
            || context.initial_cash.to_bits() != plan.plan.initial_cash.to_bits()
        {
            return Err(usage("fenêtre ou capital exact différent du plan"));
        }
        for (r, s) in &pairs {
            let g = governance(r)?;
            let b = r
                .metrics()
                .pointer("/research/block")
                .map(|v| serde_json::from_value::<Block>(v.clone()).map_err(error))
                .transpose()?;
            let source_block = g.as_ref().map_or(b, |g| g.source_block);
            let effective_block = g.as_ref().map_or(b, |g| g.effective_block);
            if effective_block != Some(Block::Dev)
                && !(effective_block.is_none() && plan.plan.technical_nonqualifying)
            {
                return Err(usage(
                    "controls exige DEV effectif ou un plan technique explicitement non qualifiant",
                ));
            }
            if b.is_some() && b != effective_block
                || source_block != source
                || effective_block != effective
            {
                return Err(usage("blocs sources/effectifs non homogènes"));
            }
            if string(r.manifest(), "/engine/git_sha")? != context.engine.commit
                || string(r.manifest(), "/engine/binary_hash")? != context.engine.binary_hash
            {
                return Err(usage("commit ou binaire moteur différent de la source"));
            }
            if s.sizing != StatsSizing::Research
                || s.provenance.dataset_hash != context.dataset_hash
                || s.provenance.initial_cash.to_bits() != context.initial_cash.to_bits()
                || s.provenance.window != s_window(&context)
                || s.provenance.profile.as_deref() != expected_profile
                || s.provenance.line.as_deref() != expected_line
                || s.pair_symbols != pairs[0].1.pair_symbols
            {
                return Err(usage(
                    "sources non homogènes : dataset/fenêtre/C0/Research/ligne/profil/univers",
                ));
            }
            let first = &pairs[0].0;
            for pointer in [
                "/config/costs",
                "/costs/costs_file_hash",
                "/costs/cost_measures_hash",
                "/costs/seed",
                "/costs/profile",
                "/config/fees",
                "/config/slippage_bps",
                "/config/portfolio/max_positions",
                "/config/portfolio/quote_gap_ms",
                "/config/portfolio/sizing",
                "/config/portfolio/min_order_bump",
                "/config/portfolio/liquidity_window_days",
                "/config/portfolio/priority",
                "/research/universe",
                "/research/quality_exclusions",
            ] {
                if r.metrics().pointer(pointer) != first.metrics().pointer(pointer) {
                    return Err(usage(format!("sources non homogènes : {pointer}")));
                }
            }
        }
        let (runs, snapshots) = pairs.into_iter().unzip();
        Ok(Self {
            runs,
            snapshots,
            context,
        })
    }
}
fn s_window(c: &ControlsContext) -> crate::research::stats::Window {
    crate::research::stats::Window {
        start_ms: c.window.start_ms,
        end_ms: c.window.end_ms,
    }
}
