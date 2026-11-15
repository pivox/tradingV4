//! Contrôles S14 réellement lancés dans des processus séparés.
use std::process::Command;
#[test]
fn controls_command_is_exposed_with_explicit_required_inputs() {
    let out = Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(["controls", "--help"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let help = String::from_utf8(out.stdout).unwrap();
    for option in ["--peer-run", "--plan", "--root", "--seed", "--out"] {
        assert!(help.contains(option), "{help}");
    }
}
mod cli_controls_common;
mod common;
use cli_controls_common::*;
#[test]
fn native_processes_publish_identical_controls_and_distinct_admissions() {
    let w = World::new();
    let before = w
        .sources
        .iter()
        .map(|s| {
            [
                "metrics.json",
                "trades.csv",
                "equity.csv",
                "stats-inputs.json",
                "manifest.json",
            ]
            .map(|f| std::fs::read(s.join(f)).unwrap())
        })
        .collect::<Vec<_>>();
    for s in &w.sources {
        for f in [
            "metrics.json",
            "trades.csv",
            "equity.csv",
            "stats-inputs.json",
            "manifest.json",
        ] {
            let p = s.join(f);
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(p, perms).unwrap();
        }
    }
    use std::os::unix::fs::PermissionsExt;
    for source in &w.sources {
        std::fs::set_permissions(source, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    ok(w.root(), &w.args("one"));
    let mut inverse = w.args("two");
    inverse.swap(1, 3);
    ok(w.root(), &inverse);
    assert_eq!(
        std::fs::read(w.root().join("one/controls.json")).unwrap(),
        std::fs::read(w.root().join("two/controls.json")).unwrap()
    );
    let index = read(w.root().join("one/controls-index.json"));
    for source in index["sources"].as_array().unwrap() {
        let dir = source["dir"].as_str().unwrap();
        assert_eq!(
            std::fs::read(w.root().join("one").join(dir).join("controls.json")).unwrap(),
            std::fs::read(w.root().join("two").join(dir).join("controls.json")).unwrap()
        );
    }
    let a = read(w.root().join("one/controls-manifest.json"));
    let b = read(w.root().join("two/controls-manifest.json"));
    assert_ne!(a["read_id"], b["read_id"]);
    assert_eq!(a["role_counts"]["null_controls"], 40);
    let measurements = read(w.root().join("one/controls-measurements.json"));
    assert_eq!(measurements["dataset_loads"], 1);
    assert_eq!(measurements["prep"]["candles_converted"], 1);
    let p = read(w.root().join("one/controls.json"));
    assert_eq!(p["smoke"], true);
    assert_eq!(p["technical_only"], true);
    for source in &w.sources {
        std::fs::set_permissions(source, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (s, bytes) in w.sources.iter().zip(before) {
        for (f, b) in [
            "metrics.json",
            "trades.csv",
            "equity.csv",
            "stats-inputs.json",
            "manifest.json",
        ]
        .into_iter()
        .zip(bytes)
        {
            assert_eq!(std::fs::read(s.join(f)).unwrap(), b);
        }
    }
}
#[test]
fn rewriting_a_controls_bundle_and_projection_removes_old_completion_markers() {
    let w = World::new();
    ok(w.root(), &w.args("bundle"));
    let index = read(w.root().join("bundle/controls-index.json"));
    let leaf = w
        .root()
        .join("bundle")
        .join(index["sources"][0]["dir"].as_str().unwrap());
    for target in [&leaf, &w.root().join("bundle")] {
        let mut args = w.backtest.clone();
        args.extend(["--out".into(), target.display().to_string()]);
        ok(w.root(), &args);
        for file in [
            "controls.json",
            "controls-manifest.json",
            "controls-index.json",
            "controls-measurements.json",
        ] {
            assert!(!target.join(file).exists(), "ancien {file} conservé");
        }
        tv4::report::stats::read_run(target).unwrap();
    }
    assert!(
        w.root().join("bundle/runs").is_dir(),
        "les copies et fichiers inconnus restent présents"
    );
}
#[test]
fn prefix_is_stable_when_extending_k_and_budget_exhaustion_is_published() {
    let w = World::new();
    ok(w.root(), &w.args("small"));
    w.edit_plan(|p| {
        p["options"].as_object_mut().unwrap().remove("replications");
    });
    ok(w.root(), &w.args("large"));
    let a = read(w.root().join("small/controls.json"));
    let b = read(w.root().join("large/controls.json"));
    for (a, b) in a["results"]
        .as_array()
        .unwrap()
        .iter()
        .zip(b["results"].as_array().unwrap())
    {
        for (a, b) in a["modes"]
            .as_array()
            .unwrap()
            .iter()
            .zip(b["modes"].as_array().unwrap())
        {
            for (a, b) in a["cells"]
                .as_array()
                .unwrap()
                .iter()
                .zip(b["cells"].as_array().unwrap())
            {
                assert_eq!(a["config_hash"], b["config_hash"]);
                assert_eq!(a["score"], b["score"]);
                assert_eq!(a["replay_trace_hash"], b["replay_trace_hash"]);
            }
        }
    }
    w.edit_plan(|p| p["options"]["budgets"]["total_engine_steps"] = 0.into());
    ok(w.root(), &w.args("budget"));
    let budget = read(w.root().join("budget/controls.json"));
    assert_eq!(budget["status"], "inconclusive");
    assert!(
        budget["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "global_engine_budget")
    );
    assert_eq!(
        read(w.root().join("budget/controls-measurements.json"))["prep"],
        serde_json::Value::Null
    );
}
fn assert_exit(w: &World, args: &[String], code: i32, contains: &str) {
    let o = run(w.root(), args);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(code), "{stderr}");
    assert!(stderr.contains(contains), "{stderr}");
}
#[test]
fn strict_committed_recipes_and_seed_are_checked_before_parquet() {
    let w = World::new();
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    let before = std::fs::read(w.root().join("research/trials.csv")).unwrap();
    let mut args = w.args("badseed");
    let i = args.iter().position(|s| s == "--seed").unwrap() + 1;
    args[i] = "8".into();
    assert_exit(&w, &args, 2, "graine");
    w.edit_plan(|p| p["unexpected"] = true.into());
    assert_exit(&w, &w.args("unknown"), 2, "unknown field");
    w.edit_plan(|p| {
        p.as_object_mut().unwrap().remove("unexpected");
        p["parents"][0]["params"]["file"] = "../params0.toml".into();
    });
    assert_exit(&w, &w.args("traversal"), 2, "sans traversée");
    assert_eq!(
        before,
        std::fs::read(w.root().join("research/trials.csv")).unwrap()
    );
}
#[test]
fn engine_and_mandatory_packet_guards_precede_market_loading() {
    for mismatch in ["git_sha", "binary_hash", "packet"] {
        let w = World::new();
        let source = &w.sources[0];
        let path = source.join("manifest.json");
        let mut m = read(&path);
        if mismatch == "packet" {
            m.as_object_mut().unwrap().remove("stats_inputs");
        } else {
            m["engine"][mismatch] = if mismatch == "git_sha" {
                "different".into()
            } else {
                format!("blake3:{}", "a".repeat(64)).into()
            };
        }
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        std::fs::remove_dir_all(w.root().join("data")).unwrap();
        assert_exit(
            &w,
            &w.args("refused"),
            2,
            if mismatch == "packet" {
                "obligatoire"
            } else {
                "moteur"
            },
        );
        assert!(!w.root().join("refused").exists());
    }
}
#[test]
fn different_exact_run_capital_is_refused_before_parquet() {
    let w = World::new();
    let mut args = w.backtest.clone();
    args.extend([
        "--capital".into(),
        "301".into(),
        "--out".into(),
        w.sources[1].display().to_string(),
    ]);
    ok(w.root(), &args);
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("capital"), 2, "capital");
}
#[test]
fn admitted_s13_one_ulp_capital_preserves_book_capital_and_all_observed_hashes() {
    let w = World::new();
    for s in &w.sources {
        let path = s.join("stats-inputs.json");
        let mut packet: tv4::report::stats_inputs::StatsInputs =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        packet.provenance.initial_cash =
            f64::from_bits(packet.provenance.initial_cash.to_bits() + 1);
        packet.marks[0].equity = packet.provenance.initial_cash;
        let bytes = serde_json::to_vec(&packet).unwrap();
        let mut m = read(s.join("manifest.json"));
        m["stats_inputs"]["hash"] = tv4::model::hash::blake3_prefixed(&bytes).into();
        std::fs::write(s.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        tv4::report::stats::read_run(s).unwrap();
    }
    let before = w
        .sources
        .iter()
        .map(|s| std::fs::read(s.join("stats-inputs.json")).unwrap())
        .collect::<Vec<_>>();
    ok(w.root(), &w.args("ulp"));
    let c = read(w.root().join("ulp/controls.json"));
    assert_eq!(c["context"]["initial_cash"], 300.);
    assert!(
        !c["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "observed_mismatch")
    );
    for (s, b) in w.sources.iter().zip(before) {
        assert_eq!(std::fs::read(s.join("stats-inputs.json")).unwrap(), b);
    }
}
mod cli_research_common;
#[test]
fn owned_universe_reader_survives_removed_files_and_preserves_validation() {
    let w = cli_research_common::World::with_universe();
    let historical = tv4::store::universe::load(&w.universe).unwrap();
    let mut files = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(&w.universe).unwrap() {
        let e = entry.unwrap();
        if e.path().is_file() {
            files.insert(
                e.file_name().to_str().unwrap().to_owned(),
                std::fs::read(e.path()).unwrap(),
            );
        }
    }
    std::fs::remove_dir_all(&w.universe).unwrap();
    let owned = tv4::store::universe::load_from_bytes(&w.universe, &files).unwrap();
    assert_eq!(historical.rules_hash(), owned.rules_hash());
    assert_eq!(historical.instruments_text(), owned.instruments_text());
    assert_eq!(
        historical
            .window_hash(1_782_864_000_000, 1_790_812_800_000)
            .unwrap(),
        owned
            .window_hash(1_782_864_000_000, 1_790_812_800_000)
            .unwrap()
    );
    let quarter = historical.file().quarters[0].file.clone();
    files.get_mut(&quarter).unwrap().push(b'x');
    let message = tv4::store::universe::load_from_bytes(&w.universe, &files)
        .unwrap_err()
        .to_string();
    std::fs::create_dir(&w.universe).unwrap();
    for (name, bytes) in &files {
        std::fs::write(w.universe.join(name), bytes).unwrap();
    }
    assert_eq!(
        message,
        tv4::store::universe::load(&w.universe)
            .unwrap_err()
            .to_string()
    );
    files.remove(&quarter);
    assert!(tv4::store::universe::load_from_bytes(&w.universe, &files).is_err());
}
#[test]
fn protected_forward_is_refused_even_before_any_parquet_can_be_opened() {
    let w = World::new();
    let research = cli_research_common::World::with_dataset();
    let out = research.root.join("source");
    cli_research_common::ok(&research.backtest_args(&out, &["--equity-step", "1m"]));
    let m = read(out.join("metrics.json"));
    let hash = m["config_hash"].clone();
    w.edit_plan(|p| {
        let params = p["parents"][0]["params"].clone();
        p["parents"] = serde_json::json!([{"config_hash":hash,"params":params}]);
        p["window"]["start_ms"] = m["data"]["window"]["start_ms"].clone();
        p["window"]["end_ms"] = m["data"]["window"]["end_ms"].clone();
    });
    std::fs::remove_dir_all(research.root.join("data")).unwrap();
    let mut args = w.args("forbidden");
    args[1] = out.display().to_string();
    args.drain(2..4);
    assert_exit(&w, &args, 2, "DEV effectif");
}
#[test]
fn market_io_failure_marks_every_reserved_parent_and_null_failed_without_bundle() {
    let w = World::new();
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    let result = run(w.root(), &w.args("missing-data"));
    assert_eq!(
        result.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!w.root().join("missing-data").exists());
    let csv = std::fs::read_to_string(w.root().join("research/trials.csv")).unwrap();
    let mut r = csv::Reader::from_reader(csv.as_bytes());
    let headers = r.headers().unwrap().clone();
    let status = headers.iter().position(|s| s == "statut").unwrap();
    let read = headers.iter().position(|s| s == "read_id").unwrap();
    let rows = r.records().map(Result::unwrap).collect::<Vec<_>>();
    let latest = rows
        .iter()
        .filter_map(|r| r[read].parse::<u64>().ok())
        .max()
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r[read].parse::<u64>().ok() == Some(latest) && &r[status] == "failed")
            .count(),
        42
    );
}
#[test]
fn geometry_preflight_is_pure_and_agrees_with_runtime_metadata() {
    let w = World::new();
    let plan = read(&w.plan);
    let mut hashes = Vec::new();
    for p in plan["parents"].as_array().unwrap() {
        let params = toml::from_str(
            &std::fs::read_to_string(w.root().join(p["params"]["file"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        let strategies = vec![tv4::strategy::registry::build("sma_cross", &params).unwrap()];
        let pure = tv4::engine::controls::preflight_geometry(&strategies).unwrap();
        assert_eq!(pure.decision_tf, tv4::model::Timeframe::M15);
        assert!(!pure.has_btc_filter);
        hashes.push(pure.geometry_hash);
    }
    ok(w.root(), &w.args("geometry"));
    let output = read(w.root().join("geometry/controls.json"));
    for (source, hash) in output["sources"].as_array().unwrap().iter().zip(hashes) {
        assert_eq!(source["geometry_hash"], hash);
    }
}
#[test]
fn native_cost_profiles_use_exact_historical_observed_rendering() {
    for profile in ["baseline", "adverse", "maker"] {
        let w = World::with_profile(profile);
        ok(w.root(), &w.args("cost-controls"));
        let c = read(w.root().join("cost-controls/controls.json"));
        assert_eq!(c["context"]["profile"], profile);
        assert!(
            !c["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r == "observed_mismatch"),
            "{profile}: {}",
            c["reasons"]
        );
    }
}
#[test]
fn publication_io_failure_keeps_terminal_consumed_admission_without_complete_bundle() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    let parent = w.root().join("readonly-output");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut args = w.args("unused");
    *args.last_mut().unwrap() = parent.join("out").display().to_string();
    let o = run(w.root(), &args);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!parent.join("out/controls-index.json").exists());
    assert!(!parent.join("out").exists());
    let bytes = std::fs::read(w.root().join("research/trials.csv")).unwrap();
    let mut reader = csv::Reader::from_reader(bytes.as_slice());
    let headers = reader.headers().unwrap().clone();
    let ri = headers.iter().position(|h| h == "read_id").unwrap();
    let st = headers.iter().position(|h| h == "statut").unwrap();
    let rows = reader.records().map(Result::unwrap).collect::<Vec<_>>();
    let latest = rows
        .iter()
        .filter_map(|r| r[ri].parse::<u64>().ok())
        .max()
        .unwrap();
    let terminal = rows
        .iter()
        .filter(|r| {
            r[ri].parse::<u64>().ok() == Some(latest) && ["failed", "completed"].contains(&&r[st])
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 42);
    assert!(terminal.iter().any(|r| &r[st] == "completed"));
}
#[test]
fn dev_sources_keep_historical_governance_and_reproduce_research_hashes() {
    let w = World::dev();
    ok(w.root(), &w.args("dev-controls"));
    let c = read(w.root().join("dev-controls/controls.json"));
    assert_eq!(c["context"]["effective_block"], "DEV");
    assert_eq!(c["technical_only"], false);
    assert!(
        !c["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "observed_mismatch"),
        "{}",
        c["reasons"]
    );
    let m = read(w.root().join("dev-controls/controls-manifest.json"));
    let current = m["read_id"].as_u64().unwrap();
    for s in c["sources"].as_array().unwrap() {
        assert!(s["historical_admission"]["read_id"].as_u64().unwrap() < current);
    }
}
#[test]
fn line_and_unclassified_policy_are_refused_before_parquet() {
    let w = World::dev();
    w.edit_plan(|p| p["line"] = "okx_usdc".into());
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("wrong-line"), 2, "ligne/sous-ensemble");
    let w = World::new();
    w.edit_plan(|p| p["technical_nonqualifying"] = false.into());
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(
        &w,
        &w.args("implicit-dev"),
        2,
        "explicitement non qualifiant",
    );
}
#[test]
fn fixed_universe_subset_replays_only_its_line_and_keeps_full_parent_identity() {
    let w = World::dev_line("okx_usdc");
    ok(w.root(), &w.args("subset-controls"));
    let c = read(w.root().join("subset-controls/controls.json"));
    assert_eq!(c["context"]["line"], "okx_usdc");
    assert!(
        !c["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "observed_mismatch")
    );
}
#[test]
fn wrong_dataset_and_real_decision_tf_are_refused_on_owned_metadata_before_parquet() {
    let w = World::new();
    let dataset = w.root().join("datasets/technical.json");
    let mut m = read(&dataset);
    m["dataset_hash"] = format!("blake3:{}", "a".repeat(64)).into();
    std::fs::write(&dataset, serde_json::to_vec(&m).unwrap()).unwrap();
    git(w.root(), &["add", "datasets/technical.json"]);
    git(w.root(), &["commit", "-qm", "dataset incohérent"]);
    w.edit_plan(|p| p["dataset_manifest"] = file(w.root(), "datasets/technical.json"));
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("dataset"), 2, "dataset/TF/bloc");
    let w = World::new();
    for i in 0..2 {
        let p = w.root().join(format!("params{i}.toml"));
        std::fs::write(p, "tf=\"4h\"\nfast=3\nslow=8\nstop_atr_mult=2.0\n").unwrap();
    }
    git(w.root(), &["add", "params0.toml", "params1.toml"]);
    git(w.root(), &["commit", "-qm", "autre TF déclaré"]);
    w.edit_plan(|p| {
        for parent in p["parents"].as_array_mut().unwrap() {
            let name = parent["params"]["file"].as_str().unwrap().to_owned();
            parent["params"] = file(w.root(), &name);
        }
    });
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("tf"), 2, "TF déclaré");
}
#[test]
fn mixed_profiles_and_lines_are_refused_before_parquet() {
    let mut w = World::with_profile("baseline");
    let old = read(w.sources[1].join("metrics.json"))["config_hash"].clone();
    w.sources[1] = w.sources[1].parent().unwrap().join("adverse");
    let new = read(w.sources[1].join("metrics.json"))["config_hash"].clone();
    w.edit_plan(|p| {
        for parent in p["parents"].as_array_mut().unwrap() {
            if parent["config_hash"] == old {
                parent["config_hash"] = new.clone();
            }
        }
        p["parents"]
            .as_array_mut()
            .unwrap()
            .sort_by(|a, b| a["config_hash"].as_str().cmp(&b["config_hash"].as_str()));
    });
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("mixed-profiles"), 2, "profil");
    let mut w = World::dev_line("full");
    w.sources[1] = w.sources[1].parent().unwrap().join("okx_usdc");
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("mixed-lines"), 2, "ligne");
}
#[test]
fn changed_committed_recipe_and_existing_output_do_not_consume_admission() {
    let w = World::new();
    let before = std::fs::read(w.root().join("research/trials.csv")).unwrap();
    std::fs::create_dir(w.root().join("exists")).unwrap();
    assert_exit(&w, &w.args("exists"), 2, "nouveau dossier");
    std::fs::write(w.root().join("params0.toml"), "tf=\"4h\"\n").unwrap();
    assert_exit(&w, &w.args("uncommitted"), 2, "modifié depuis HEAD");
    assert_eq!(
        before,
        std::fs::read(w.root().join("research/trials.csv")).unwrap()
    );
}
#[test]
fn changed_s11_instrument_recipe_is_refused_before_any_parquet_read() {
    let w = World::dev();
    let path = w.root().join("instruments.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("tick_size=0.01", "tick_size=0.02");
    std::fs::write(&path, text).unwrap();
    git(w.root(), &["add", "instruments.toml"]);
    git(w.root(), &["commit", "-qm", "autres règles instrument"]);
    w.edit_plan(|p| p["instruments"] = file(w.root(), "instruments.toml"));
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("bad-instrument"), 2, "identité de recherche");
}
#[test]
fn invalid_plan_position_limit_is_a_usage_error_before_parquet() {
    let w = World::new();
    w.edit_plan(|p| p["max_positions"] = 0.into());
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("invalid-limit"), 2, "positions");
}
#[test]
fn cli_replication_bound_is_checked_before_reservation_and_parquet() {
    let w = World::new();
    w.edit_plan(|p| p["options"]["replications"] = 1001.into());
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    let before = std::fs::read(w.root().join("research/trials.csv")).unwrap();
    assert_exit(&w, &w.args("too-many"), 2, "K");
    assert_eq!(
        before,
        std::fs::read(w.root().join("research/trials.csv")).unwrap()
    );
    assert!(!w.root().join("too-many").exists());
}
#[test]
fn missing_committed_parameter_recipe_is_usage_and_does_not_reserve() {
    let w = World::new();
    let before = std::fs::read(w.root().join("research/trials.csv")).unwrap();
    std::fs::remove_file(w.root().join("params0.toml")).unwrap();
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    assert_exit(&w, &w.args("missing-recipe"), 2, "absent");
    assert_eq!(
        before,
        std::fs::read(w.root().join("research/trials.csv")).unwrap()
    );
}
#[test]
fn invalid_pure_recipe_options_are_usage_before_reservation() {
    for (key, value) in [
        ("fees_grid", serde_json::json!("unknown")),
        ("slippage_bps", serde_json::json!(-1.0)),
    ] {
        let w = World::new();
        w.edit_plan(|p| p[key] = value);
        std::fs::remove_dir_all(w.root().join("data")).unwrap();
        let before = std::fs::read(w.root().join("research/trials.csv")).unwrap();
        let o = run(w.root(), &w.args("invalid-option"));
        assert_eq!(
            o.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            before,
            std::fs::read(w.root().join("research/trials.csv")).unwrap()
        );
    }
}
#[test]
fn cli_replication_endpoints_are_accepted_without_silent_reduction() {
    let w = World::new();
    w.edit_plan(|p| {
        p["options"]["replications"] = 1.into();
        p["options"]["budgets"]["total_engine_steps"] = 0.into();
    });
    ok(w.root(), &w.args("minimum-k"));
    assert_eq!(
        read(w.root().join("minimum-k/controls.json"))["replications"],
        1
    );
    w.edit_plan(|p| p["options"]["replications"] = 1000.into());
    ok(w.root(), &w.args("maximum-k"));
    assert_eq!(
        read(w.root().join("maximum-k/controls.json"))["replications"],
        1000
    );
}
#[test]
fn corrupt_source_remains_an_artifact_error_before_reservation() {
    let w = World::new();
    let before = std::fs::read(w.root().join("research/trials.csv")).unwrap();
    let path = w.sources[0].join("trades.csv");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(b'\n');
    std::fs::write(path, bytes).unwrap();
    std::fs::remove_dir_all(w.root().join("data")).unwrap();
    let o = run(w.root(), &w.args("corrupt-source"));
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(
        before,
        std::fs::read(w.root().join("research/trials.csv")).unwrap()
    );
    assert!(!w.root().join("corrupt-source").exists());
}
