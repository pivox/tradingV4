//! Couverture du panel et instantané S12 sans append ni projection README.
use super::*;
use std::collections::BTreeMap;
pub(super) fn registry(args: &StatsArgs) -> Result<Section<RegistrySnapshot>, CliError> {
    let root = match research::resolve_root(args.research_root.as_deref()) {
        Ok(root) => root,
        Err(e) if args.research_root.is_none() => {
            return Ok(Section::unavailable(format!(
                "registre courant non associé : {e}"
            )));
        }
        Err(e) => return Err(error(e)),
    };
    // La garde est détruite ici, avant tous les calculs longs.
    let journal = research::journal::Journal::open(&root).map_err(error)?;
    let r = journal.registry();
    Ok(Section::available(RegistrySnapshot {
        registry_hash: crate::model::hash::blake3_prefixed(journal.loaded_bytes()),
        last_event_id: r.events().last().map(|e| e.event_id.0),
        declared_n: r.n(),
        registered_hashes: r.trials().keys().map(ToString::to_string).collect(),
    }))
}
pub(super) fn build(
    current: &Leaf,
    leaves: &[Leaf],
    peers: &[Leaf],
    registry: &Section<RegistrySnapshot>,
    options: &StatsOptions,
) -> Result<Corpus, CliError> {
    let p = &current.image.provenance;
    let mut distinct = BTreeMap::<String, CorpusMember>::new();
    for leaf in leaves
        .iter()
        .chain(peers)
        .filter(|l| l.image.provenance.line == p.line)
    {
        let q = &leaf.image.provenance;
        if q.dataset_hash != p.dataset_hash
            || q.initial_cash.to_bits() != p.initial_cash.to_bits()
            || q.window != p.window
        {
            return Err(usage(
                "corpus : dataset, capital, fenêtre réelle et ligne identiques requis",
            ));
        }
        let key = q
            .config_hash
            .strip_prefix("blake3:")
            .unwrap_or(&q.config_hash)
            .to_owned();
        let member = CorpusMember {
            source: leaf.source()?,
            registry_member: registry
                .value
                .as_ref()
                .map(|r| r.registered_hashes.binary_search(&key).is_ok()),
            daily: leaf.daily()?,
        };
        if let Some(previous) = distinct.get(&key) {
            // run_id/manifest peuvent différer (provenance historique), les résultats et
            // entrées statistiques d'une configuration doivent être strictement identiques.
            if previous.daily != member.daily
                || previous.source.provenance.result_hash != q.result_hash
                || previous.source.files.metrics_json != member.source.files.metrics_json
                || previous.source.files.trades_csv != member.source.files.trades_csv
                || previous.source.files.equity_csv != member.source.files.equity_csv
                || previous.source.files.stats_inputs_json != member.source.files.stats_inputs_json
            {
                return Err(usage(
                    "corpus : config_hash dupliqué avec sources contradictoires",
                ));
            }
        } else {
            distinct.insert(key, member);
        }
    }
    let members = distinct.into_values().collect::<Vec<_>>();
    let m = members.len();
    let declared = registry
        .value
        .as_ref()
        .filter(|r| r.declared_n > 0 && members.iter().all(|m| m.registry_member == Some(true)))
        .map(|r| r.declared_n);
    let complete = members
        .iter()
        .filter(|m| {
            m.daily.expected_intervals > 0
                && m.daily.missing_intervals.is_empty()
                && m.daily.intervals.len() == m.daily.expected_intervals
        })
        .count();
    let mut reasons = Vec::new();
    if declared.is_none() {
        reasons.push(
            "N global inconnu : tous les hashes du panel ne sont pas associés au même registre"
                .into(),
        );
    }
    if complete != m {
        reasons.push(format!("panel quotidien incomplet : {complete}/{m} configurations complètes ; aucune intersection ni imputation"));
    }
    let clustering = if complete != m {
        Section::unavailable(
            "séries quotidiennes complètes requises pour toutes les configurations observées",
        )
    } else {
        let series = members
            .iter()
            .map(|m| PeerSeries {
                config_hash: m.source.provenance.config_hash.clone(),
                dataset_hash: p.dataset_hash.clone(),
                line: p.line.clone().unwrap_or_else(|| "single".into()),
                capital: p.initial_cash,
                window: p.window,
                daily_returns: m.daily.intervals.clone(),
            })
            .collect::<Vec<_>>();
        Section::available(
            crate::research::stats::clustering(&series, declared, options).map_err(error)?,
        )
    };
    Ok(Corpus {
        members,
        observed_configs: m,
        complete_daily_configs: complete,
        declared_verified_n: declared,
        coverage: declared.map(|n| m as f64 / n as f64),
        reasons,
        clustering,
    })
}
