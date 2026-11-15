//! Comparaison pure de deux résultats déjà vérifiés : aucun rejeu ni accès au store.
use super::{RunSnapshot, VerifiedRun, snapshot};
use crate::report::stats_inputs::StatsProvenance;
use crate::research::stats::{Metric, TradeRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryDelaySample {
    pub source: StatsProvenance,
    pub n: usize,
    pub n_r: usize,
    /// `r_multiple` publié est déjà net de tous les coûts.
    pub mean_r_net: Metric,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DelayedEntryStats {
    pub original: EntryDelaySample,
    pub delayed: EntryDelaySample,
    pub delay_minutes: u32,
    pub delayed_to_original_ratio: Metric,
    /// Information d'éligibilité seulement ; aucun verdict de porte.
    pub eligible_dev_g2: bool,
    pub eligibility_reasons: Vec<String>,
}

/// Les trois empreintes de chaque source ont été vérifiées par le lecteur unique.
/// Cette fonction ne conserve que des images possédées et ne rouvre aucun fichier.
pub fn compare_delayed_entries(
    original: &VerifiedRun,
    delayed: &VerifiedRun,
) -> Result<DelayedEntryStats, String> {
    let base = snapshot(original)?;
    let candidate = snapshot(delayed)?;
    let tf = decision_minutes(original, &base)?;
    if decision_minutes(delayed, &candidate)? != tf {
        return Err("TF de décision décalé absent ou différent".into());
    }
    validate_pair(
        &base,
        original.manifest(),
        original.metrics(),
        &candidate,
        delayed.manifest(),
        delayed.metrics(),
        tf,
    )?;
    let original_sample = sample(&base);
    let delayed_sample = sample(&candidate);
    let delayed_to_original_ratio = ratio(&original_sample.mean_r_net, &delayed_sample.mean_r_net);
    let mut eligibility_reasons = Vec::new();
    if !verifiable_dev(original.manifest(), original.metrics(), &base) {
        eligibility_reasons.push("original : bloc DEV non vérifiable".into());
    }
    if !verifiable_dev(delayed.manifest(), delayed.metrics(), &candidate) {
        eligibility_reasons.push("décalé : bloc DEV non vérifiable".into());
    }
    Ok(DelayedEntryStats {
        original: original_sample,
        delayed: delayed_sample,
        delay_minutes: tf,
        delayed_to_original_ratio,
        eligible_dev_g2: eligibility_reasons.is_empty(),
        eligibility_reasons,
    })
}

fn decision_minutes(run: &VerifiedRun, snap: &RunSnapshot) -> Result<u32, String> {
    resolve_decision_minutes(
        run.stats_inputs()
            .map(|p| p.inputs.decision_timeframe_minutes),
        snap.decision_timeframe_minutes,
        run.metrics(),
    )
}

fn resolve_decision_minutes(
    captured: Option<u32>,
    historical: Option<u32>,
    metrics: &Value,
) -> Result<u32, String> {
    let mut sources = Vec::new();
    sources.extend(captured);
    sources.extend(historical);
    // Ces contrats nomment explicitement le TF de décision, contrairement à un params.tf arbitraire.
    for pointer in [
        "/config/research/decision_tf",
        "/config/costs/entry/decision_tf",
    ] {
        if let Some(value) = metrics.pointer(pointer) {
            let tf = value
                .as_str()
                .and_then(|v| v.parse::<crate::model::Timeframe>().ok())
                .ok_or_else(|| format!("TF de décision invalide : {pointer}"))?;
            sources.push((tf.ms() / 60_000) as u32);
        }
    }
    let first = sources
        .first()
        .copied()
        .ok_or("TF de décision non identifiable")?;
    if sources.iter().any(|tf| *tf != first)
        || !crate::model::Timeframe::ALL
            .iter()
            .any(|tf| tf.ms() / 60_000 == i64::from(first))
    {
        return Err("TF de décision incohérent ou non supporté".into());
    }
    Ok(first)
}

fn normalized_config(metrics: &Value) -> Result<(Value, u64), String> {
    let mut config = metrics
        .get("config")
        .filter(|c| c.is_object())
        .cloned()
        .ok_or("config absente")?;
    let rules = config
        .get_mut("sim_rules")
        .and_then(Value::as_object_mut)
        .ok_or("sim_rules absent")?;
    let delay = rules
        .remove("extra_entry_delay_bars")
        .map(|v| v.as_u64().ok_or("délai supplémentaire mal typé"))
        .transpose()?
        .unwrap_or(0);
    if delay > 1440 {
        return Err("délai supplémentaire hors borne".into());
    }
    Ok((config, delay))
}

fn validate_pair(
    base: &RunSnapshot,
    bm: &Value,
    bv: &Value,
    candidate: &RunSnapshot,
    cm: &Value,
    cv: &Value,
    tf: u32,
) -> Result<(), String> {
    if base.provenance.dataset_hash != candidate.provenance.dataset_hash {
        return Err("dataset_hash différent".into());
    }
    if base.provenance.initial_cash.to_bits() != candidate.provenance.initial_cash.to_bits() {
        return Err("capital différent".into());
    }
    if base.provenance.window != candidate.provenance.window {
        return Err("fenêtre réelle différente".into());
    }
    if base.provenance.profile != candidate.provenance.profile
        || base.provenance.line != candidate.provenance.line
    {
        return Err("profil ou ligne différent".into());
    }
    if base.pair_symbols != candidate.pair_symbols
        || base.market_symbols != candidate.market_symbols
        || base.sizing != candidate.sizing
    {
        return Err("univers ou dimensionnement différent".into());
    }
    if base.provenance.config_hash == candidate.provenance.config_hash {
        return Err("le délai supplémentaire exige un config_hash distinct".into());
    }
    for manifest in [bm, cm] {
        let engine = manifest
            .get("engine")
            .and_then(Value::as_object)
            .ok_or("identité du moteur absente ou mal typée")?;
        for field in [
            "tv4_version",
            "git_sha",
            "rustc",
            "target",
            "target_cpu",
            "binary_hash",
        ] {
            if engine
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|v| v.trim().is_empty())
            {
                return Err(format!(
                    "identité du moteur non vérifiable : engine.{field}"
                ));
            }
        }
    }
    for pointer in ["/schema", "/engine"] {
        if bm.pointer(pointer) != cm.pointer(pointer) {
            return Err(format!("famille ou moteur différent : {pointer}"));
        }
    }
    for pointer in [
        "/schema",
        "/research/identity",
        "/research/universe",
        "/research/decision_window",
        "/data/block",
    ] {
        if bv.pointer(pointer) != cv.pointer(pointer) {
            return Err(format!("identité ou layout différent : {pointer}"));
        }
    }
    let (base_config, base_delay) = normalized_config(bv)?;
    let (candidate_config, candidate_delay) = normalized_config(cv)?;
    if base_delay != 0 {
        return Err("le run original est déjà décalé".into());
    }
    if tf == 0 || candidate_delay != u64::from(tf) {
        return Err("le délai candidat doit valoir exactement un TF de décision".into());
    }
    if base_config != candidate_config {
        return Err("configuration différente hors délai supplémentaire".into());
    }
    Ok(())
}

fn sample(snap: &RunSnapshot) -> EntryDelaySample {
    let (n, n_r, mean_r_net) = mean_r(&snap.trades);
    EntryDelaySample {
        source: snap.provenance.clone(),
        n,
        n_r,
        mean_r_net,
    }
}
fn mean_r(trades: &[TradeRecord]) -> (usize, usize, Metric) {
    let kept: Vec<_> = trades.iter().filter(|t| t.included).collect();
    let n = kept.len();
    let n_r = kept.iter().filter(|t| t.r_multiple.is_some()).count();
    let metric = if n == 0 {
        Metric::unavailable("aucun trade conservé")
    } else if n != n_r {
        Metric::unavailable("R net absent sur des trades conservés")
    } else {
        // Division avant somme pour éviter un débordement artificiel de la somme des R.
        let mut sum = 0.;
        let mut compensation = 0.;
        for r in kept.iter().filter_map(|t| t.r_multiple) {
            let term = r / n as f64 - compensation;
            let next = sum + term;
            compensation = (next - sum) - term;
            sum = next;
        }
        finite_metric(sum)
    };
    (n, n_r, metric)
}
fn finite_metric(value: f64) -> Metric {
    Metric::available(value).unwrap_or_else(|_| Metric::unavailable("résultat non fini"))
}
fn ratio(base: &Metric, candidate: &Metric) -> Metric {
    match (base.value, candidate.value) {
        (Some(b), Some(d)) if b > 0. => finite_metric(d / b),
        (Some(_), _) => {
            Metric::unavailable("expectancy originale non positive ou R décalés incomplets")
        }
        _ => Metric::unavailable("R nets incomplets"),
    }
}
fn verifiable_dev(manifest: &Value, metrics: &Value, snap: &RunSnapshot) -> bool {
    let span = crate::model::Block::Dev.span();
    metrics.pointer("/data/source").and_then(Value::as_str) == Some("dataset")
        && metrics.pointer("/data/block").and_then(Value::as_str) == Some("DEV")
        && metrics.pointer("/research/block").and_then(Value::as_str) == Some("DEV")
        && span.contains(snap.provenance.window.start_ms)
        && span.contains(snap.provenance.window.end_ms - 1)
        && manifest
            .pointer("/governance/effective_block")
            .is_none_or(|v| v == "DEV")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::stats_inputs::SimulationHashes;
    use crate::research::stats::{StatsSizing, Window};
    use serde_json::json;
    fn snap() -> RunSnapshot {
        RunSnapshot {
            provenance: StatsProvenance {
                run_id: "a".repeat(64),
                config_hash: "a".repeat(64),
                dataset_hash: format!("blake3:{}", "b".repeat(64)),
                result_hash: format!("blake3:{}", "c".repeat(64)),
                initial_cash: 300.,
                window: Window {
                    start_ms: 1_600_000_000_000,
                    end_ms: 1_600_000_060_000,
                },
                profile: None,
                line: None,
                files: SimulationHashes {
                    metrics_json: "m".into(),
                    trades_csv: "t".into(),
                    equity_csv: "e".into(),
                },
            },
            pair_symbols: vec!["ETH-USD".into()],
            market_symbols: vec!["ETH-USD".into()],
            sizing: StatsSizing::Research,
            trades: vec![],
            terminal_rows: vec![],
            decision_timeframe_minutes: Some(15),
            cash_final: 300.,
            equity_final: 300.,
        }
    }
    fn candidate() -> RunSnapshot {
        let mut candidate = snap();
        candidate.provenance.config_hash = "d".repeat(64);
        candidate
    }
    fn manifest() -> Value {
        json!({"schema":"leaf","engine":{"tv4_version":"0.1.0","git_sha":"abc","rustc":"rustc 1.97.0","target":"x86_64-unknown-linux-gnu","target_cpu":"generic","binary_hash":format!("blake3:{}", "e".repeat(64))}})
    }
    fn config(delay: Option<u32>) -> Value {
        let mut v = json!({"schema":"metrics", "config":{"sim_rules":{"activation_delay_bars":1},"costs":{"seed":1,"spread":5},"strategy":{"id":"test","params":{"tf":"1h","decision":"15m"}}}});
        if let Some(delay) = delay {
            v["config"]["sim_rules"]["extra_entry_delay_bars"] = json!(delay);
        }
        v
    }
    #[test]
    fn only_the_extra_delay_is_removed_from_the_config() {
        let base = snap();
        let manifest = manifest();
        for delay in [None, Some(0)] {
            assert!(
                validate_pair(
                    &base,
                    &manifest,
                    &config(delay),
                    &candidate(),
                    &manifest,
                    &config(Some(15)),
                    15
                )
                .is_ok()
            );
        }
        for delay in [None, Some(0), Some(1), Some(30), Some(1441)] {
            assert!(
                validate_pair(
                    &base,
                    &manifest,
                    &config(None),
                    &candidate(),
                    &manifest,
                    &config(delay),
                    15
                )
                .is_err()
            );
        }
        assert!(
            validate_pair(
                &base,
                &manifest,
                &config(Some(15)),
                &candidate(),
                &manifest,
                &config(Some(15)),
                15
            )
            .is_err()
        );
        for pointer in [
            "/config/sim_rules/activation_delay_bars",
            "/config/costs/seed",
            "/config/costs/spread",
            "/config/strategy/params/tf",
        ] {
            let mut modified = config(Some(15));
            *modified.pointer_mut(pointer).unwrap() = json!(42);
            assert!(
                validate_pair(
                    &base,
                    &manifest,
                    &config(None),
                    &candidate(),
                    &manifest,
                    &modified,
                    15
                )
                .is_err(),
                "{pointer}"
            );
        }
    }
    #[test]
    fn identities_must_match_even_when_config_hash_intentionally_omits_them() {
        let b = snap();
        let m = manifest();
        for change in 0..7 {
            let mut c = candidate();
            match change {
                0 => c.provenance.dataset_hash = "other".into(),
                1 => c.provenance.initial_cash = 301.,
                2 => c.provenance.window.end_ms += 60_000,
                3 => c.provenance.profile = Some("maker".into()),
                4 => c.provenance.line = Some("okx_usdc".into()),
                5 => c.pair_symbols.push("BTC-USD".into()),
                _ => c.market_symbols.push("BTC-USD".into()),
            }
            assert!(validate_pair(&b, &m, &config(None), &c, &m, &config(Some(15)), 15).is_err());
        }
        for pointer in [
            "/research/identity",
            "/research/universe",
            "/research/decision_window",
            "/data/block",
        ] {
            let mut v = config(Some(15));
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            v[parent.trim_start_matches('/')] = json!({key:"changed"});
            assert!(
                validate_pair(&b, &m, &config(None), &candidate(), &m, &v, 15).is_err(),
                "{pointer}"
            );
        }
    }
    fn trade(r: Option<f64>, included: bool) -> TradeRecord {
        TradeRecord {
            row: 0,
            lot_id: 1,
            symbol: "ETH-USD".into(),
            entry_ms: 1,
            exit_ms: 2,
            qty: 1.,
            entry_price: 100.,
            exit_price: 110.,
            entry_fee: 10.,
            exit_fee: 10.,
            pnl_net: -10.,
            r_multiple: r,
            included,
        }
    }
    #[test]
    fn mean_uses_net_r_without_removing_neighbors_or_charging_costs_again() {
        let (n, n_r, mean) = mean_r(&[
            trade(Some(2.), true),
            trade(Some(4.), true),
            trade(None, false),
        ]);
        assert_eq!((n, n_r), (2, 2));
        assert_eq!(mean.value, Some(3.));
        let (n, n_r, missing) = mean_r(&[trade(Some(2.), true), trade(None, true)]);
        assert_eq!((n, n_r), (2, 1));
        assert!(missing.value.is_none());
        assert!(ratio(&mean, &missing).value.is_none());
        assert_eq!(ratio(&mean, &finite_metric(1.5)).value, Some(0.5));
        for r in [0., -1.] {
            assert!(ratio(&finite_metric(r), &mean).value.is_none());
        }
        assert!(mean_r(&[]).2.value.is_none());
    }
    #[test]
    fn dev_eligibility_requires_a_verifiable_block_and_never_relabels_fixtures() {
        let b = snap();
        let mut metrics =
            json!({"data":{"source":"dataset","block":"DEV"},"research":{"block":"DEV"}});
        assert!(verifiable_dev(&json!({}), &metrics, &b));
        metrics["data"]["source"] = json!("fixture");
        assert!(!verifiable_dev(&json!({}), &metrics, &b));
        for v in [
            json!({}),
            json!({"data":{"source":"dataset","block":"FORWARD"},"research":{"block":"FORWARD"}}),
        ] {
            assert!(!verifiable_dev(&json!({}), &v, &b));
        }
        metrics["data"]["source"] = json!("dataset");
        assert!(!verifiable_dev(
            &json!({"governance":{"effective_block":"HOLDOUT"}}),
            &metrics,
            &b
        ));
    }
    #[test]
    fn tf_resolution_never_uses_a_generic_param_or_maximum_filter_tf() {
        let generic = json!({"config":{"strategy":{"params":{"tf":"4h"}}}});
        assert!(resolve_decision_minutes(None, None, &generic).is_err());
        let costs = json!({"config":{"costs":{"entry":{"decision_tf":"15m"}}}});
        assert_eq!(resolve_decision_minutes(None, None, &costs).unwrap(), 15);
        let research = json!({"config":{"research":{"decision_tf":"15m"}}});
        assert_eq!(resolve_decision_minutes(None, None, &research).unwrap(), 15);
        assert!(resolve_decision_minutes(Some(240), Some(15), &costs).is_err());
    }
    #[test]
    fn engine_identity_must_be_present_well_typed_and_complete() {
        let base = snap();
        let candidate = candidate();
        let check = |m: &Value| {
            validate_pair(
                &base,
                m,
                &config(None),
                &candidate,
                m,
                &config(Some(15)),
                15,
            )
        };
        assert!(check(&manifest()).is_ok());
        assert!(check(&json!({"schema":"leaf"})).is_err());
        for engine in [Value::Null, json!("engine"), json!([]), json!({})] {
            assert!(check(&json!({"schema":"leaf","engine":engine})).is_err());
        }
        for field in [
            "tv4_version",
            "git_sha",
            "rustc",
            "target",
            "target_cpu",
            "binary_hash",
        ] {
            let mut incomplete = manifest();
            incomplete["engine"].as_object_mut().unwrap().remove(field);
            assert!(check(&incomplete).is_err(), "absent {field}");
            for value in [json!(""), json!(" "), json!(42), Value::Null] {
                incomplete["engine"][field] = value;
                assert!(check(&incomplete).is_err(), "invalid {field}");
            }
        }
    }
    #[test]
    fn the_extra_delay_requires_a_distinct_verified_config_hash() {
        let base = snap();
        let m = manifest();
        assert!(validate_pair(&base, &m, &config(None), &base, &m, &config(Some(15)), 15).is_err());
        assert!(
            validate_pair(
                &base,
                &m,
                &config(None),
                &candidate(),
                &m,
                &config(Some(15)),
                15
            )
            .is_ok()
        );
    }
}
