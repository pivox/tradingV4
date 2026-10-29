//! Rapports de recherche s11 (docs/specs/s11.md §9.2, §9.3 et §9.6, lot E) : bloc `research`, purge des
//! trades soldés à la fin de la fenêtre, métriques de trades sur les trades conservés et d'equity sur la
//! fenêtre de décision, `run_id_s11`, dossiers de profils s11 et `lines.json`.
//!
//! Marché **inventé** (`research_common`) : trois jambes FORWARD synthétiques, un régime BTC factice,
//! aucune donnée de marché réelle.

mod common;
mod portfolio_common;
mod profiles_common;
mod research_common;

use std::collections::BTreeSet;

use portfolio_common::{
    MINUTE, Scripted, T0, buy_quote, config, flat_series, instrument, names, run,
};
use profiles_common::costs_file;
use research_common::{Fixture, fixture, fixture_with_warmup, line_input, profiles_line_input};
use serde_json::Value;
use tv4::engine::Line;
use tv4::model::{ExchangeId, Timeframe, hash};
use tv4::report::manifest::{run_id_s9, run_id_s11};
use tv4::report::metrics_s9::{
    DatasetInfo, InstrumentsInfo, MetricsS9, MetricsS9Inputs, SCHEMA, SCHEMA_S11, SeriesInfo,
};
use tv4::report::research::{
    LINES_FILE, LINES_SCHEMA, LinesReport, METRICS_BASIS, UNIVERSE_QUALITY_DEVIATION,
};
use tv4::strategy::Strategy;

fn json(rendered: &tv4::report::profiles::RenderedProfile) -> Value {
    serde_json::to_value(&rendered.metrics).expect("metrics.json")
}

/// Lignes de `trades.csv` : `(colonne → valeur)` par trade.
fn trade_rows(bytes: &[u8]) -> Vec<Vec<(String, String)>> {
    let mut reader = csv::Reader::from_reader(bytes);
    let headers: Vec<String> = reader
        .headers()
        .expect("en-tête")
        .iter()
        .map(str::to_string)
        .collect();
    reader
        .records()
        .map(|record| {
            headers
                .iter()
                .cloned()
                .zip(record.expect("ligne").iter().map(str::to_string))
                .collect()
        })
        .collect()
}

fn column<'a>(row: &'a [(String, String)], name: &str) -> &'a str {
    &row.iter().find(|(key, _)| key == name).expect("colonne").1
}

fn f(value: &Value, pointer: &str) -> f64 {
    value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .expect(pointer)
}

fn u(value: &Value, pointer: &str) -> u64 {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .expect(pointer)
}

// ---------------------------------------------------------------------------------------------
// Purge et base des métriques
// ---------------------------------------------------------------------------------------------

#[test]
fn trades_csv_garde_tous_les_trades_et_la_purge_se_lit_sur_end_of_window() {
    let fx = fixture();
    let rendered = fx.render(Line::Full, 300.0);
    let metrics = json(&rendered);
    let rows = trade_rows(&rendered.trades_bytes);
    let purged_rows = rows
        .iter()
        .filter(|row| column(row, "exit_type") == "end_of_window")
        .count() as u64;
    let kept = u(&metrics, "/research/purge/trades_kept");
    let purged = u(&metrics, "/research/purge/trades_purged");
    assert!(
        kept > 0 && purged > 0,
        "le scénario doit purger et conserver"
    );
    assert_eq!(rows.len() as u64, kept + purged);
    assert_eq!(purged_rows, purged);
    assert_eq!(kept, u(&metrics, "/n_trades"));
    assert_eq!(
        metrics
            .pointer("/research/purge/rule")
            .and_then(Value::as_str),
        Some("end_of_window_v1")
    );
    assert_eq!(
        metrics
            .pointer("/research/metrics_basis")
            .and_then(Value::as_str),
        Some(METRICS_BASIS)
    );
    // Les trades purgés ne sont ni fins de fenêtre « conservées » ni comptés comme sorties forcées.
    assert_eq!(u(&metrics, "/n_trades_forced"), 0);
    assert_eq!(u(&metrics, "/funnel/exits_by_type/end_of_window"), 0);
    assert_eq!(u(&metrics, "/funnel/trades"), kept);
    let by_pair: u64 = metrics["funnel_by_pair"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| pair["trades"].as_u64().unwrap())
        .sum();
    assert_eq!(
        by_pair, kept,
        "funnel_by_pair porte sur les trades conservés"
    );
}

#[test]
fn les_metriques_de_trades_portent_sur_les_conserves_et_celles_d_equity_sur_tout() {
    let fx = fixture();
    let rendered = fx.render(Line::Full, 300.0);
    let metrics = json(&rendered);
    let rows = trade_rows(&rendered.trades_bytes);
    let pnl = |row: &[(String, String)]| column(row, "pnl_net").parse::<f64>().unwrap();
    let kept: Vec<f64> = rows
        .iter()
        .filter(|row| column(row, "exit_type") != "end_of_window")
        .map(|row| pnl(row))
        .collect();
    let purged: f64 = rows
        .iter()
        .filter(|row| column(row, "exit_type") == "end_of_window")
        .map(|row| pnl(row))
        .sum();
    let kept_sum: f64 = kept.iter().sum();
    // Expectancy et profit factor sur les conservés seuls.
    assert!((f(&metrics, "/expectancy_net") - kept_sum / kept.len() as f64).abs() < 1e-12);
    let gross_profit: f64 = kept.iter().filter(|p| **p > 0.0).sum();
    assert!((f(&metrics, "/gross_profit") - gross_profit).abs() < 1e-12);
    assert_eq!(
        u(&metrics, "/n_wins") + u(&metrics, "/n_losses"),
        kept.len() as u64
    );
    // PnL purgé publié, et PnL net d'equity = conservés + purgés.
    assert!((f(&metrics, "/research/purge/pnl_net_purged") - purged).abs() < 1e-12);
    assert!((f(&metrics, "/pnl_net") - (kept_sum + purged)).abs() < 1e-9);
    // Verdict jugé sur l'effectif conservé (3 trades < min_trades).
    assert_eq!(
        metrics.pointer("/verdict").and_then(Value::as_str),
        Some("rejected_too_few_trades")
    );
    // Distribution sur les conservés : aucune sortie end_of_window.
    let exits = metrics["distribution"]["by_exit_type"].as_array().unwrap();
    assert!(exits.iter().all(|e| e["exit_type"] != "end_of_window"));
    let distributed: u64 = exits.iter().map(|e| e["n_trades"].as_u64().unwrap()).sum();
    assert_eq!(distributed, kept.len() as u64);
}

#[test]
fn l_equity_et_l_exposition_portent_sur_la_fenetre_de_decision_seule() {
    let fx = fixture();
    let rendered = fx.render(Line::Full, 300.0);
    let metrics = json(&rendered);
    let days = (fx.end_ms - fx.start_ms) / 86_400_000;
    assert_eq!(u(&metrics, "/exposure/bars_in_window"), days as u64 * 1440);
    let regime_days = u(&metrics, "/regime/days_bull") + u(&metrics, "/regime/days_bear");
    assert_eq!(regime_days, days as u64);
    // La chauffe (210 jours de BTC) n'entre ni dans la grille ni dans l'equity.
    let equity = String::from_utf8(rendered.equity_bytes.clone()).unwrap();
    let first = equity.lines().nth(1).expect("première ligne d'equity");
    let first_ms: i64 = first.split(',').next().unwrap().parse().unwrap();
    assert!(
        first_ms > fx.start_ms && first_ms <= fx.start_ms + 3_600_000,
        "première ligne d'equity à {first_ms}"
    );
    assert_eq!(
        metrics
            .pointer("/data/window/start_ms")
            .and_then(Value::as_i64),
        Some(fx.start_ms)
    );
    assert_eq!(
        metrics
            .pointer("/research/decision_window/start_ms")
            .and_then(Value::as_i64),
        Some(fx.start_ms)
    );
    assert!(
        metrics
            .pointer("/research/regime_used")
            .and_then(Value::as_bool)
            .unwrap()
    );
    assert_eq!(
        metrics
            .pointer("/research/warmup_start")
            .and_then(Value::as_str),
        Some("2026-03-02T00:00:00Z")
    );
}

#[test]
fn exposition_regime_et_trades_ne_dependent_pas_de_la_longueur_de_la_chauffe() {
    let short = fixture_with_warmup(210).render(Line::Full, 300.0);
    let long = fixture_with_warmup(300).render(Line::Full, 300.0);
    let (a, b) = (json(&short), json(&long));
    assert_eq!(a["exposure"], b["exposure"]);
    assert_eq!(a["regime"]["days_bull"], b["regime"]["days_bull"]);
    assert_eq!(a["regime"]["days_bear"], b["regime"]["days_bear"]);
    assert!(a["regime"]["days_bull"].as_u64().unwrap() > 0);
    assert!(a["regime"]["days_bear"].as_u64().unwrap() > 0);
    assert_eq!(short.trades_bytes, long.trades_bytes);
    assert_eq!(short.equity_bytes, long.equity_bytes);
    assert_ne!(
        a["research"]["warmup_start"], b["research"]["warmup_start"],
        "seule la chauffe diffère"
    );
}

// ---------------------------------------------------------------------------------------------
// Bloc research, identité, schéma
// ---------------------------------------------------------------------------------------------

#[test]
fn le_schema_est_s11_et_le_bloc_research_a_les_cles_de_la_spec() {
    let fx = fixture();
    let metrics = json(&fx.render(Line::Full, 300.0));
    assert_eq!(metrics["schema"], SCHEMA_S11);
    assert_eq!(SCHEMA_S11, "tv4-metrics-s11");
    let research = metrics["research"].as_object().expect("bloc research");
    let keys: BTreeSet<&str> = research.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = [
        "block",
        "blocks_version",
        "decision_window",
        "funnel",
        "guard",
        "identity",
        "identity_hash",
        "legs",
        "line",
        "metrics_basis",
        "purge",
        "quality_exclusions",
        "regime_used",
        "subset",
        "universe",
        "warmup_start",
    ]
    .into_iter()
    .collect();
    assert_eq!(keys, expected);
    assert_eq!(research["block"], "FORWARD");
    assert_eq!(research["blocks_version"], "b1");
    assert_eq!(research["guard"], "none");
    assert_eq!(research["line"], "full");
    assert!(research["subset"].is_null());
    let identity: BTreeSet<&str> = research["identity"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        identity,
        [
            "block",
            "decision_end_ms",
            "decision_start_ms",
            "legs_hash",
            "line",
            "quality_hash",
            "subset_hash",
            "universe_window_hash",
        ]
        .into_iter()
        .collect()
    );
    // Écart déclaré : la qualité d'univers se lit sur les données du trimestre.
    let deviations: Vec<&str> = metrics["sim_deviations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(deviations.contains(&UNIVERSE_QUALITY_DEVIATION));
    // Le dataset porte le bloc et la fenêtre de décision.
    assert_eq!(metrics["data"]["block"], "FORWARD");
    assert_eq!(
        metrics["data"]["decision_window"],
        research["decision_window"]
    );
}

#[test]
fn le_detail_par_trimestre_vient_du_calendrier_et_des_exclusions() {
    let fx = fixture();
    let metrics = json(&fx.render(Line::Full, 300.0));
    let universe = &metrics["research"]["universe"];
    assert_eq!(universe["mode"], "quarterly");
    assert_eq!(universe["rules_hash"], research_common::RULES_HASH);
    assert_eq!(universe["decision_tf"], "4h");
    let quarters = universe["quarters"].as_array().unwrap();
    assert_eq!(quarters.len(), 2);
    assert_eq!(quarters[0]["quarter"], "2026-Q3");
    assert_eq!(quarters[0]["members"], 2);
    assert_eq!(quarters[0]["eligible_legs"], 2);
    assert_eq!(quarters[0]["excluded_quality"], serde_json::json!([]));
    assert_eq!(quarters[1]["quarter"], "2026-Q4");
    assert_eq!(quarters[1]["members"], 3);
    assert_eq!(quarters[1]["eligible_legs"], 2);
    assert_eq!(
        quarters[1]["excluded_quality"],
        serde_json::json!(["BBB-USD"])
    );
    assert_eq!(
        metrics["research"]["quality_exclusions"],
        serde_json::json!({"BBB-USD": [{"quarter": "2026-Q4", "tfs": ["4h"]}]})
    );
    // Refus de recherche par jambe et au total, avec les trois motifs présents dans le scénario.
    let funnel = &metrics["research"]["funnel"];
    assert!(u(&metrics, "/research/funnel/entries_outside_decision_window") > 0);
    assert!(u(&metrics, "/research/funnel/entries_not_in_universe") > 0);
    assert!(u(&metrics, "/research/funnel/entries_excluded_quality") > 0);
    for key in [
        "entries_outside_decision_window",
        "entries_not_in_universe",
        "entries_excluded_quality",
        "entries_cancelled_universe",
    ] {
        let sum: u64 = metrics["research"]["legs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|leg| leg[key].as_u64().unwrap())
            .sum();
        assert_eq!(sum, funnel[key].as_u64().unwrap(), "{key}");
    }
    // Seule BBB est exclue par la qualité, seule CCC est hors univers au Q3.
    let leg = |symbol: &str| {
        metrics["research"]["legs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|leg| leg["symbol"] == symbol)
            .unwrap()
            .clone()
    };
    assert_eq!(leg("AAA-USD")["entries_excluded_quality"], 0);
    assert!(leg("BBB-USD")["entries_excluded_quality"].as_u64().unwrap() > 0);
    assert!(leg("CCC-USD")["entries_not_in_universe"].as_u64().unwrap() > 0);
    assert_eq!(leg("AAA-USD")["entries_not_in_universe"], 0);
}

#[test]
fn l_identite_est_hachee_a_part_et_le_run_id_la_porte() {
    let fx = fixture();
    let full = fx.render(Line::Full, 300.0);
    let okx = fx.render(Line::OkxUsdc, 300.0);
    let (mf, mo) = (&full.metrics, &okx.metrics);
    let (rf, ro) = (mf.research.as_ref().unwrap(), mo.research.as_ref().unwrap());
    // Même configuration gelée, identités et run_id différents.
    assert_eq!(mf.config_hash, mo.config_hash);
    assert_ne!(rf.identity_hash, ro.identity_hash);
    assert_ne!(mf.run_id(), mo.run_id());
    // L'identité hachée est celle de l'objet écrit.
    let recomputed = format!(
        "blake3:{}",
        hash::hash_json(&serde_json::to_value(&rf.identity).unwrap()).unwrap()
    );
    assert_eq!(rf.identity_hash, recomputed);
    // run_id : formule de S11, différente de celle de S9.
    let expected = run_id_s11(
        &mf.config_hash,
        &mf.dataset_hash,
        mf.data.window.start_ms,
        mf.data.window.end_ms,
        300.0,
        &rf.identity_hash,
    );
    assert_eq!(mf.run_id(), expected);
    assert_ne!(
        mf.run_id(),
        run_id_s9(
            &mf.config_hash,
            &mf.dataset_hash,
            mf.data.window.start_ms,
            mf.data.window.end_ms,
            300.0
        )
    );
    // Le manifeste porte le même run_id, le schéma s11 et les chemins d'entrée de recherche.
    let manifest = serde_json::to_value(fx.manifest(&full, Line::Full)).unwrap();
    assert_eq!(manifest["schema"], "tv4-manifest-s11");
    assert_eq!(manifest["run_id"], mf.run_id());
    assert_eq!(
        manifest["research_inputs"],
        serde_json::json!({"universe": "synthetic-universe"})
    );
    let manifest_okx = serde_json::to_value(fx.manifest(&okx, Line::OkxUsdc)).unwrap();
    assert_eq!(
        manifest_okx["research_inputs"],
        serde_json::json!({"okx_subset": "synthetic-subset.json", "universe": "synthetic-universe"})
    );
    // Les chemins d'entrée ne sont jamais dans metrics.json.
    assert!(json(&full).get("research_inputs").is_none());
}

#[test]
fn le_capital_change_le_run_id_mais_ni_le_config_hash_ni_l_identite() {
    let fx = fixture();
    let a = fx.render(Line::Full, 300.0).metrics;
    let b = fx.render(Line::Full, 3_000.0).metrics;
    assert_eq!(a.config_hash, b.config_hash);
    assert_eq!(
        a.research.as_ref().unwrap().identity_hash,
        b.research.as_ref().unwrap().identity_hash
    );
    assert_ne!(a.run_id(), b.run_id());
}

#[test]
fn le_rendu_est_deterministe_a_l_octet() {
    let fx = fixture();
    let a = fx.render(Line::Full, 300.0);
    let b = fx.render(Line::Full, 300.0);
    assert_eq!(a.metrics_bytes, b.metrics_bytes);
    assert_eq!(a.trades_bytes, b.trades_bytes);
    assert_eq!(a.equity_bytes, b.equity_bytes);
}

#[test]
fn la_ligne_okx_usdc_porte_son_sous_ensemble_et_ses_seules_jambes() {
    let fx = fixture();
    let metrics = json(&fx.render(Line::OkxUsdc, 300.0));
    let research = &metrics["research"];
    assert_eq!(research["line"], "okx_usdc");
    assert_eq!(research["subset"]["kind"], "okx_usdc");
    assert_eq!(research["subset"]["subset_hash"], fx.subset.subset_hash);
    assert_eq!(
        research["subset"]["legs_excluded"],
        serde_json::json!(["BBB-USD"])
    );
    assert_eq!(research["identity"]["subset_hash"], fx.subset.subset_hash);
    let symbols: Vec<&str> = research["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|leg| leg["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(symbols, ["AAA-USD", "CCC-USD"]);
    assert_eq!(
        metrics["data"]["symbols"],
        serde_json::json!(["AAA-USD", "CCC-USD"])
    );
    // Au Q4, BBB n'est pas jouée : ni éligible, ni exclue par la qualité.
    let q4 = &research["universe"]["quarters"][1];
    assert_eq!(q4["members"], 3);
    assert_eq!(q4["eligible_legs"], 2);
    assert_eq!(q4["excluded_quality"], serde_json::json!([]));
}

// ---------------------------------------------------------------------------------------------
// Cohérence refusée, s9 inchangé
// ---------------------------------------------------------------------------------------------

#[test]
fn des_entrees_incoherentes_sont_refusees() {
    let fx = fixture();
    let result = fx.play(Line::OkxUsdc, 300.0);
    let render = |inputs: &tv4::report::research::ResearchInputs<'_>| {
        tv4::report::research::render_research_run(
            &result,
            MetricsS9Inputs {
                dataset: fx.dataset_info(Line::OkxUsdc),
                instruments: InstrumentsInfo::toml(),
                okx_pairs: None,
                markets: profiles_common::markets(
                    &fx.line_series(Line::OkxUsdc),
                    &fx.played(Line::OkxUsdc),
                ),
            },
            inputs,
            Timeframe::H1,
        )
    };
    let excluded = fx.excluded(Line::OkxUsdc);
    // Témoin.
    assert!(render(&fx.inputs(&excluded, Line::OkxUsdc)).is_ok());
    // La ligne okx_usdc exige son sous-ensemble.
    let mut inputs = fx.inputs(&excluded, Line::OkxUsdc);
    inputs.subset = None;
    assert!(render(&inputs).is_err());
    // Des exclusions qualité qui ne sont pas celles de l'identité.
    let other = tv4::model::QualitySchedule::default();
    let mut inputs = fx.inputs(&excluded, Line::OkxUsdc);
    inputs.quality = &other;
    assert!(render(&inputs).is_err());
    // Un calendrier d'univers absent en univers trimestriel.
    let mut inputs = fx.inputs(&excluded, Line::OkxUsdc);
    inputs.universe = None;
    assert!(render(&inputs).is_err());
}

#[test]
fn un_resultat_de_recherche_n_a_pas_de_chemin_s9_ni_l_inverse() {
    let fx = fixture();
    let research = fx.play(Line::Full, 300.0);
    let series = fx.line_series(Line::Full);
    let inputs = || MetricsS9Inputs {
        dataset: fx.dataset_info(Line::Full),
        instruments: InstrumentsInfo::toml(),
        okx_pairs: None,
        markets: profiles_common::markets(&series, &fx.played(Line::Full)),
    };
    assert!(MetricsS9::new(&research, inputs()).is_err());

    // Un run ordinaire : pas de bloc research, schéma s9.
    let universe = names(&["AAA-USD"]);
    let instruments = vec![instrument(0.0)];
    let data = vec![("AAA-USD", flat_series(0, 200, 100.0, 10.0))];
    let (strategy, _) = Scripted::new(vec![(T0 + 30 * MINUTE, buy_quote(50.0))]);
    let plain = run(
        &data,
        vec![Box::new(strategy) as Box<dyn Strategy>],
        config(&universe, &instruments, 300.0),
    );
    let columns = common_columns(&data);
    let metrics = MetricsS9::new(
        &plain,
        MetricsS9Inputs {
            dataset: DatasetInfo {
                name: "synthetique".to_string(),
                exchange: ExchangeId::Kraken,
                tf: Timeframe::M1,
                gaps_policy: "skip".to_string(),
                hash_format: "dsh-v1".to_string(),
                dataset_hash: "blake3:aa".to_string(),
                series: vec![SeriesInfo {
                    symbol: "AAA-USD".to_string(),
                    rows: columns.len() as u64,
                    first_ms: T0,
                    last_ms: T0 + 199 * MINUTE,
                }],
            },
            instruments: InstrumentsInfo::toml(),
            okx_pairs: None,
            markets: profiles_common::markets(&[("AAA-USD".to_string(), columns)], &universe),
        },
    )
    .expect("metrics s9");
    let value = serde_json::to_value(&metrics).unwrap();
    assert_eq!(value["schema"], SCHEMA);
    assert!(value.get("research").is_none());
    assert!(value["data"].get("block").is_none());
    assert!(value["data"].get("decision_window").is_none());
    assert!(metrics.research.is_none());
    // Et `new_research` refuse un résultat sans recherche.
    let bad = tv4::report::metrics_s9::MetricsS9::new_research(
        &plain,
        MetricsS9Inputs {
            dataset: fx.dataset_info(Line::Full),
            instruments: InstrumentsInfo::toml(),
            okx_pairs: None,
            markets: Vec::new(),
        },
        &fx.inputs(&[], Line::Full),
    );
    assert!(bad.is_err());
}

fn common_columns(data: &[(&str, Vec<tv4::model::Candle>)]) -> tv4::store::Columns {
    tv4::store::Columns::from_candles(&data[0].1)
}

// ---------------------------------------------------------------------------------------------
// Profils de coûts s11
// ---------------------------------------------------------------------------------------------

#[test]
fn un_profil_de_recherche_est_au_schema_s11_avec_ses_blocs_de_couts() {
    let fx = fixture();
    let file = costs_file();
    let rendered = fx.render_profiles(Line::Full, &file, 42);
    assert_eq!(rendered.len(), 3);
    let mut identities = BTreeSet::new();
    for (profile, name) in rendered.iter().zip(["baseline", "adverse", "maker"]) {
        let metrics = json(profile);
        assert_eq!(metrics["schema"], SCHEMA_S11);
        assert_eq!(metrics["costs"]["profile"], name);
        assert!(metrics["cost_arithmetic"].is_object());
        assert!(metrics["cost_counters"].is_object());
        assert!(metrics["research"].is_object());
        // Les trades.csv de profils portent la colonne `spread_cost` de s10 et tous les trades.
        let rows = trade_rows(&profile.trades_bytes);
        let kept = u(&metrics, "/research/purge/trades_kept");
        let purged = u(&metrics, "/research/purge/trades_purged");
        assert_eq!(rows.len() as u64, kept + purged);
        assert_eq!(kept, u(&metrics, "/n_trades"));
        identities.insert(
            profile
                .metrics
                .research
                .as_ref()
                .unwrap()
                .identity_hash
                .clone(),
        );
    }
    // Chaque profil a sa propre identité (valeurs de coûts par jambe) et son config_hash.
    assert_eq!(identities.len(), 3);
    let hashes: BTreeSet<&str> = rendered
        .iter()
        .map(|r| r.metrics.config_hash.as_str())
        .collect();
    assert_eq!(hashes.len(), 3);
    // profiles.json accepte la génération s11.
    let report = profiles_common::profiles_report(&rendered, &file);
    assert_eq!(report.profiles_id.len(), 64);
}

#[test]
fn profiles_json_refuse_de_melanger_les_generations_ou_les_lignes() {
    let fx = fixture();
    let file = costs_file();
    let full = fx.render_profiles(Line::Full, &file, 42);
    let okx = fx.render_profiles(Line::OkxUsdc, &file, 42);
    let mixed = [full[0].clone(), full[1].clone(), okx[2].clone()];
    let hashes: Vec<String> = mixed.iter().map(|r| r.result_hash()).collect();
    let input = |i: usize| tv4::report::profiles::ProfileInput {
        metrics: &mixed[i].metrics,
        result_hash: &hashes[i],
    };
    let result =
        tv4::report::profiles::ProfilesReport::build(&tv4::report::profiles::ProfilesInput {
            baseline: input(0),
            adverse: input(1),
            maker: input(2),
            min_maker_entry_share: file.profiles.maker.min_maker_entry_share,
        });
    assert!(
        result.is_err(),
        "deux lignes ne font pas un dossier de profils"
    );
}

// ---------------------------------------------------------------------------------------------
// lines.json
// ---------------------------------------------------------------------------------------------

#[test]
fn lines_json_decrit_les_deux_lignes_et_le_residu() {
    let fx = fixture();
    let full = fx.render(Line::Full, 300.0);
    let okx = fx.render(Line::OkxUsdc, 300.0);
    let report = fx.lines_report(&full, &okx);
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["schema"], LINES_SCHEMA);
    assert_eq!(value["block"], "FORWARD");
    assert_eq!(value["dataset_hash"], research_common::DATASET_HASH);
    assert_eq!(value["initial_cash"], 300.0);
    assert_eq!(value["config_hash"]["single"], full.metrics.config_hash);
    assert_eq!(value["subset"]["subset_hash"], fx.subset.subset_hash);
    assert_eq!(value["subset"]["bases"], serde_json::json!(["AAA", "CCC"]));
    let lines = value["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["line"], "full");
    assert_eq!(lines[0]["dir"], "full");
    assert_eq!(lines[0]["run_id"], full.metrics.run_id());
    assert!(lines[0].get("profiles_id").is_none());
    assert_eq!(lines[1]["line"], "okx_usdc");
    assert_eq!(lines[1]["run_id"], okx.metrics.run_id());
    assert_eq!(
        lines[0]["legs"],
        serde_json::json!(["AAA-USD", "BBB-USD", "CCC-USD"])
    );
    assert_eq!(lines[1]["legs"], serde_json::json!(["AAA-USD", "CCC-USD"]));
    let stats = &lines[0]["by_profile"]["single"];
    assert_eq!(stats["n_trades"], full.metrics.n_trades);
    assert_eq!(
        stats["identity_hash"],
        full.metrics.research.as_ref().unwrap().identity_hash
    );
    // Résiduel full − okx_usdc.
    let residual = &value["residual_survivorship"]["single"];
    assert_eq!(
        residual["n_trades"].as_i64().unwrap(),
        full.metrics.n_trades as i64 - okx.metrics.n_trades as i64
    );
    let expected =
        hash::blake3_hex(format!("{}|{}", full.metrics.run_id(), okx.metrics.run_id()).as_bytes());
    assert_eq!(value["lines_id"], expected);
    // Sérialisation canonique et déterministe, écrite telle quelle.
    let tmp = tempfile::tempdir().unwrap();
    report.write(tmp.path()).unwrap();
    let written = std::fs::read(tmp.path().join(LINES_FILE)).unwrap();
    assert_eq!(written, report.bytes().unwrap());
    assert_eq!(
        report.bytes().unwrap(),
        fx.lines_report(&full, &okx).bytes().unwrap()
    );
}

#[test]
fn lines_json_refuse_deux_lignes_qui_ne_decrivent_pas_la_meme_chose() {
    let fx = fixture();
    let full = fx.render(Line::Full, 300.0);
    let okx = fx.render(Line::OkxUsdc, 300.0);
    let build = |a: &Fixture,
                 b: &tv4::report::profiles::RenderedProfile,
                 c: &tv4::report::profiles::RenderedProfile| {
        LinesReport::build(
            &line_input(Line::Full, b),
            &line_input(Line::OkxUsdc, c),
            &a.subset,
        )
    };
    assert!(build(&fx, &full, &okx).is_ok());
    // Une ligne jouée deux fois : la seconde n'est pas okx_usdc.
    assert!(build(&fx, &full, &full).is_err());
    // Capital différent entre les deux lignes.
    let okx_3000 = fx.render(Line::OkxUsdc, 3_000.0);
    assert!(build(&fx, &full, &okx_3000).is_err());
    // Un run seul d'un côté, un dossier de profils de l'autre.
    let file = costs_file();
    let profiles = fx.render_profiles(Line::OkxUsdc, &file, 42);
    assert!(
        LinesReport::build(
            &line_input(Line::Full, &full),
            &profiles_line_input(Line::OkxUsdc, &profiles),
            &fx.subset
        )
        .is_err()
    );
}

#[test]
fn lines_json_accepte_deux_dossiers_de_profils() {
    let fx = fixture();
    let file = costs_file();
    let full = fx.render_profiles(Line::Full, &file, 42);
    let okx = fx.render_profiles(Line::OkxUsdc, &file, 42);
    let report = LinesReport::build(
        &profiles_line_input(Line::Full, &full),
        &profiles_line_input(Line::OkxUsdc, &okx),
        &fx.subset,
    )
    .expect("lines.json de deux dossiers de profils");
    let value = serde_json::to_value(&report).unwrap();
    for profile in ["baseline", "adverse", "maker"] {
        assert!(value["config_hash"][profile].is_string(), "{profile}");
        assert!(
            value["residual_survivorship"][profile].is_object(),
            "{profile}"
        );
        assert!(
            value["lines"][0]["by_profile"][profile].is_object(),
            "{profile}"
        );
    }
    assert!(value["lines"][0].get("run_id").is_none());
    assert!(value["lines"][0]["profiles_id"].is_string());
}
