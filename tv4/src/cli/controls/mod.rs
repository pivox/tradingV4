//! Admission S12 puis un chargement unique ; publication après terminaison de toutes les réservations.
mod plan;
mod replay;
mod sources;
use super::{
    CliError,
    research::{
        self, ResearchArgs,
        admission::{ControlSpecs, Session},
    },
};
use crate::{
    model::Block,
    report::controls::{self, CurrentAdmission, identity::NullProgramIdentity},
    research::{
        controls::CellAvailability,
        registry::{ConfigSpec, Metrics, TrialResult},
    },
};
use clap::Args;
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};
#[derive(Debug, Args)]
pub struct ControlsArgs {
    pub source_leaf: PathBuf,
    #[arg(long)]
    pub peer_run: Vec<PathBuf>,
    #[arg(long)]
    pub plan: PathBuf,
    #[arg(long)]
    pub root: PathBuf,
    #[arg(long)]
    pub seed: u64,
    #[arg(long)]
    pub out: PathBuf,
}
fn usage(message: impl ToString) -> CliError {
    CliError::Usage {
        option: "controls".into(),
        detail: message.to_string(),
    }
}
fn error(message: impl ToString) -> CliError {
    CliError::Stats(format!("controls : {}", message.to_string()))
}
pub(super) fn run(args: &ControlsArgs) -> Result<(), CliError> {
    let plan = plan::LoadedPlan::load(&args.plan, args.seed)?;
    let mut paths = vec![args.source_leaf.clone()];
    paths.extend(args.peer_run.clone());
    let sources = sources::Sources::load(&paths, &plan)?;
    let recipe = replay::Recipe::prepare(&plan, &sources)?;
    match fs::symlink_metadata(&args.out) {
        Ok(_) => return Err(usage("--out doit être un nouveau dossier")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    if !args
        .out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .is_dir()
    {
        return Err(usage("parent du nouveau dossier absent"));
    }
    let mut parents = Vec::new();
    let mut hypotheses = Vec::new();
    for (run, hash) in sources.runs.iter().zip(&recipe.hashes) {
        let g = sources::governance(run)?;
        let h = g
            .and_then(|g| g.hypothesis)
            .map(|h| {
                research::hypothesis::load_hypothesis(&plan.root, &h.identifiant).map(|f| (h, f))
            })
            .transpose()?;
        if let Some((h, f)) = &h
            && h.sha256 != f.reference.sha256().as_str()
        {
            return Err(usage("fiche historique différente du HEAD commité"));
        }
        let hypothesis = h.as_ref().map(|(_, f)| f.reference.clone());
        let family = h.map_or_else(|| "unregistered_hypothesis".into(), |(_, f)| f.family);
        parents.push(ConfigSpec {
            config_hash: research::digest(hash)?,
            family,
            hypothesis: hypothesis.clone(),
        });
        hypotheses.push(hypothesis);
    }
    let mut specs = ControlSpecs::new(parents)?;
    let mut nulls = BTreeMap::new();
    for (i, metadata) in recipe.metadata.iter().enumerate() {
        for mode in plan.options.effective_modes(metadata.has_btc_filter) {
            for k in 0..plan.options.replications {
                let p = NullProgramIdentity::new(
                    metadata.config_hash.clone(),
                    metadata.geometry_hash.clone(),
                    mode,
                    metadata.has_btc_filter,
                    plan.options.seed,
                    k,
                    (&plan.options.budgets).into(),
                )
                .map_err(error)?;
                let hash = p.config_hash().map_err(error)?;
                specs.push_null(
                    research::digest(&hash)?,
                    &recipe.records[i],
                    hypotheses[i].clone(),
                )?;
                nulls.insert(hash, (i, mode, k));
            }
        }
    }
    let mut session = Session::begin_specs_with_prepare(
        &ResearchArgs {
            research_root: Some(plan.root.clone()),
            hypothesis: None,
        },
        &specs,
        &sources.context.dataset_hash,
        sources.context.source_block,
        recipe
            .info
            .as_ref()
            .map(|i| (i.decision_start_ms, i.decision_end_ms)),
        |block, window| {
            if block != Some(Block::Dev) && !(block.is_none() && plan.plan.technical_nonqualifying)
            {
                return Err(usage("bloc effectif interdit avant séries"));
            }
            if block != sources.context.effective_block
                || window
                    != recipe
                        .info
                        .as_ref()
                        .map(|i| (i.decision_start_ms, i.decision_end_ms))
            {
                return Err(usage(
                    "rotation ou fenêtre effective différente de la source",
                ));
            }
            Ok(())
        },
    )?;
    let result = (|| {
        let dataset = Arc::new(crate::store::load_dataset_multi_manifest(
            &args.root,
            &plan.base.join(&plan.plan.dataset_manifest.file),
            recipe.manifest.clone(),
        )?);
        let outcome = recipe.run(&plan, &sources, dataset)?;
        let mut results = BTreeMap::new();
        for (i, hash) in recipe.hashes.iter().enumerate() {
            let s = outcome.sources.get(i);
            let score = s.and_then(|s| s.observed_score);
            let failed = s.is_none_or(|s| s.observed_reason.is_some());
            results.insert(
                hash.clone(),
                TrialResult {
                    config_hash: research::digest(hash)?,
                    failed,
                    metrics: if failed {
                        Metrics::default()
                    } else {
                        Metrics {
                            mu_r: score,
                            n: Some(
                                sources.snapshots[i]
                                    .trades
                                    .iter()
                                    .filter(|t| t.included)
                                    .count() as u64,
                            ),
                            pf: None,
                        }
                    },
                },
            );
        }
        for (hash, (i, mode, k)) in &nulls {
            let c = outcome
                .sources
                .get(*i)
                .and_then(|s| {
                    s.modes
                        .iter()
                        .find(|m| m.mode.effective(recipe.metadata[*i].has_btc_filter) == *mode)
                })
                .and_then(|m| m.cells.get(*k as usize));
            let complete = c.is_some_and(|c| c.availability == CellAvailability::Complete);
            results.insert(
                hash.clone(),
                TrialResult {
                    config_hash: research::digest(hash)?,
                    failed: !complete,
                    metrics: if complete {
                        Metrics {
                            mu_r: c.and_then(|c| c.score),
                            n: None,
                            pf: None,
                        }
                    } else {
                        Metrics::default()
                    },
                },
            );
        }
        let contexts = results
            .keys()
            .map(|h| Ok((h.clone(), session.context(h)?)))
            .collect::<Result<BTreeMap<_, _>, CliError>>()?;
        let counts = session.role_counts();
        let admission = CurrentAdmission {
            contexts,
            candidates: counts.candidates as u64,
            null_controls: counts.null_controls as u64,
        };
        // Construire et valider tous les octets avant d'annoncer les lectures terminées.
        let measurements = serde_json::json!({
            "dataset_loads": 1,
            "prep": outcome.prep.map(|p| serde_json::json!({
                "candles_converted": p.candles_converted,
                "mtf_builds": p.mtf_builds,
                "regime_series_built": p.regime_series_built,
                "indicator_columns_computed": p.indicator_columns_computed,
                "rank_tables_built": p.rank_tables_built,
            })),
            "work": {
                "probe_steps": outcome.work.probe_steps,
                "replay_steps": outcome.work.replay_steps,
                "total_steps": outcome.work.total_steps,
                "planning_cells": outcome.work.planning_cells,
                "planning_logical_bytes": outcome.work.planning_logical_bytes,
            }
        });
        let bundle = controls::bundle::build_bundle(
            &sources.context,
            &recipe.metadata,
            &sources.runs,
            &outcome,
            &admission,
        )
        .map_err(error)?
        .with_measurements(&measurements)
        .map_err(error)?;
        session.finish(&results.into_values().collect::<Vec<_>>())?;
        controls::bundle::publish(&args.out, &bundle).map_err(error)?;
        println!(
            "contrôles S14 publiés : {} sources, K={}, {:?}",
            sources.runs.len(),
            plan.options.replications,
            outcome.status
        );
        Ok(())
    })();
    if result.is_err() {
        session.fail()?;
    }
    result
}
