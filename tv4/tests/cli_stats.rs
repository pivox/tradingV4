//! Post-traitement réel, données techniques et registre isolé.
mod common;
mod report_s9_common;
use report_s9_common::{build_dataset, candles, repo, strings, tv4};
use std::{fs, path::Path};
fn fixture(root: &Path, mode: &str) -> std::path::PathBuf {
    fixture_n(root, mode, 6000)
}
fn fixture_n(root: &Path, mode: &str, n: usize) -> std::path::PathBuf {
    build_dataset(
        root,
        "eth",
        &[(
            "ETH-USD",
            candles(n, 22, 0., 150., common::synthetic_candles),
        )],
        n as i64 + 100,
    );
    fs::write(
        root.join("params.toml"),
        "tf = \"15m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0\n",
    )
    .unwrap();
    fs::write(
        root.join("instruments.toml"),
        "[kraken.ETH-USD]\ntick_size = 0.01\nlot_size = 1e-6\nmin_qty = 0.0\nmin_notional = 0.0\n",
    )
    .unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
    let out = root.join("run");
    create_mode(root, mode, &out);
    out
}
fn create_mode(root: &Path, mode: &str, out: &Path) {
    let mut args = strings(&[
        "backtest",
        "--dataset",
        "eth",
        "--root",
        root.to_str().unwrap(),
        "--strategy",
        "sma_cross",
        "--params",
        root.join("params.toml").to_str().unwrap(),
        "--instruments-file",
        root.join("instruments.toml").to_str().unwrap(),
        "--fees-file",
        repo("config/fees.toml").to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--research-root",
        root.to_str().unwrap(),
    ]);
    if mode != "cell" {
        args.extend(strings(&["--symbols", "ETH-USD"]));
    }
    if mode == "profiles" {
        args.extend(strings(&[
            "--costs",
            repo("tv4/tests/fixtures/costs_c1.toml").to_str().unwrap(),
        ]));
    }
    let result = tv4(&args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
#[test]
fn publishes_deterministic_stats_without_reopening_market_data() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = fixture(root, "cell");
    let sources: [Vec<u8>; 5] = [
        "metrics.json",
        "trades.csv",
        "equity.csv",
        "manifest.json",
        "stats-inputs.json",
    ]
    .map(|f| fs::read(out.join(f)).unwrap());
    fs::remove_dir_all(root.join("datasets")).unwrap();
    fs::remove_dir_all(root.join("data/candles")).unwrap();
    fs::remove_file(root.join("instruments.toml")).unwrap();
    let before_registry =
        ["trials.csv", "README.md"].map(|f| fs::read(root.join("research").join(f)).ok());
    let mut first = Vec::new();
    for _ in 0..2 {
        let result = tv4(&strings(&[
            "stats",
            out.to_str().unwrap(),
            "--seed",
            "18446744073709551615",
            "--research-root",
            root.to_str().unwrap(),
        ]));
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = fs::read(out.join("stats.json")).unwrap();
        if first.is_empty() {
            first = bytes;
        } else {
            assert_eq!(first, bytes);
        }
    }
    let json: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(json["schema"], "tv4-stats-v1");
    assert_eq!(json["options"]["seed"], "18446744073709551615");
    for (i, f) in [
        "metrics.json",
        "trades.csv",
        "equity.csv",
        "manifest.json",
        "stats-inputs.json",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(sources[i], fs::read(out.join(f)).unwrap());
    }
    assert_eq!(
        before_registry,
        ["trials.csv", "README.md"].map(|f| fs::read(root.join("research").join(f)).ok())
    );
}

#[test]
fn usage_is_checked_before_any_artifact_or_output_access() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("absent");
    let base = ["stats", path.to_str().unwrap()];
    for extra in [
        vec![],
        vec!["--seed", "-1"],
        vec!["--seed", "1", "--samples", "9999"],
        vec!["--seed", "1", "--bootstrap-block", "4"],
        vec!["--seed", "1", "--bootstrap-block", "11"],
        vec!["--seed", "1", "--correlation-threshold", "NaN"],
        vec!["--seed", "1", "--correlation-threshold", "inf"],
        vec!["--seed", "1", "--correlation-threshold", "1"],
        vec!["--seed", "1", "--dataset", "hidden"],
    ] {
        let mut args = strings(&base);
        args.extend(strings(&extra));
        let result = tv4(&args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}
#[test]
fn profiles_are_all_validated_before_publication_and_external_inputs_are_strict() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = fixture(root, "profiles");
    let baseline = out.join("baseline");
    let output = tv4(&strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "1",
        "--research-root",
        root.to_str().unwrap(),
    ]));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let baseline_before = fs::read(baseline.join("stats.json")).unwrap();
    let packet = fs::read(out.join("maker/stats-inputs.json")).unwrap();
    fs::write(out.join("maker/stats-inputs.json"), b"{}").unwrap();
    let output = tv4(&strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "2",
        "--research-root",
        root.to_str().unwrap(),
    ]));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        baseline_before,
        fs::read(baseline.join("stats.json")).unwrap()
    );
    fs::write(out.join("maker/stats-inputs.json"), &packet).unwrap();
    let output = tv4(&strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "1",
        "--inputs",
        baseline.join("stats-inputs.json").to_str().unwrap(),
    ]));
    assert_eq!(output.status.code(), Some(2));
    let wrong = root.join("wrong.json");
    fs::write(&wrong, &packet).unwrap();
    let output = tv4(&strings(&[
        "stats",
        baseline.to_str().unwrap(),
        "--seed",
        "2",
        "--inputs",
        wrong.to_str().unwrap(),
    ]));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        baseline_before,
        fs::read(baseline.join("stats.json")).unwrap()
    );
}
#[test]
fn old_run_keeps_trade_statistics_and_unknown_registry_without_fabricated_marks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = fixture_n(root, "cell", 15000);
    let unknown = root.join("unknown");
    fs::create_dir(&unknown).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&unknown)
            .status()
            .unwrap()
            .success()
    );
    let mut m: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
    m.as_object_mut().unwrap().remove("stats_inputs");
    fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&m).unwrap(),
    )
    .unwrap();
    fs::remove_file(out.join("stats-inputs.json")).unwrap();
    let result = tv4(&strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "5",
        "--research-root",
        unknown.to_str().unwrap(),
    ]));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let j: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("stats.json")).unwrap()).unwrap();
    assert!(j["bootstrap"]["n_trades"].as_u64().unwrap() > 0);
    assert!(j["concentration"]["total_pnl"].is_number());
    assert!(j["monte_carlo"]["removal_expectancy_median_r"]["value"].is_number());
    assert!(j["monte_carlo"]["dd_initial_p95_2y"]["value"].is_null());
    assert!(j["corpus"]["declared_verified_n"].is_null());
    assert_eq!(j["corpus"]["clustering"]["availability"], "unavailable");
    fs::create_dir_all(unknown.join("research")).unwrap();
    fs::write(unknown.join("research/trials.csv"), b"corrompu\n").unwrap();
    let result = tv4(&strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "6",
        "--research-root",
        unknown.to_str().unwrap(),
    ]));
    assert_eq!(result.status.code(), Some(1));
}

#[test]
fn marked_monte_carlo_is_wired_on_a_real_45_day_technical_run() {
    let tmp = tempfile::Builder::new()
        .prefix("tv4-s13-lot6-")
        .tempdir()
        .unwrap();
    let root = tmp.path();
    let out = fixture_n(root, "cell", 64_800);
    let args = strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "18446744073709551615",
        "--research-root",
        root.to_str().unwrap(),
    ]);
    let result = tv4(&args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let first = fs::read(out.join("stats.json")).unwrap();
    let j: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert!(j["monte_carlo"]["n_trades"].as_u64().unwrap() >= 40);
    assert!(j["monte_carlo"]["n_blocks"].as_u64().unwrap() >= 2);
    assert!(j["monte_carlo"]["dd_initial_p95_2y"]["value"].is_number());
    assert_eq!(j["corpus"]["complete_daily_configs"], 1);
    let result = tv4(&args);
    assert!(result.status.success());
    assert_eq!(first, fs::read(out.join("stats.json")).unwrap());
    create_mode(root, "portfolio", &root.join("portfolio"));
    create_mode(root, "profiles", &root.join("profiles"));
    let retained = tmp.keep();
    println!("S13_FIXTURE={}", retained.display());
}

#[test]
fn corpus_is_order_independent_deduplicated_and_never_reduces_declared_n() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = fixture(root, "profiles");
    let base = out.join("baseline");
    let adverse = out.join("adverse");
    let maker = out.join("maker");
    let stats = |peers: &[&Path]| {
        let mut args = strings(&[
            "stats",
            base.to_str().unwrap(),
            "--seed",
            "1",
            "--research-root",
            root.to_str().unwrap(),
        ]);
        for p in peers {
            args.extend(strings(&["--peer-run", p.to_str().unwrap()]));
        }
        let result = tv4(&args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::read(base.join("stats.json")).unwrap()
    };
    assert_eq!(stats(&[&out, &adverse]), stats(&[&adverse, &out]));
    let first = stats(&[&maker, &adverse, &maker]);
    assert_eq!(first, stats(&[&adverse, &maker]));
    let all: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(all["corpus"]["observed_configs"], 3);
    assert_eq!(all["corpus"]["declared_verified_n"], 3);
    assert!(all["corpus"]["clustering"]["value"]["global_n_eff"]["value"].is_number());
    let partial: serde_json::Value = serde_json::from_slice(&stats(&[])).unwrap();
    assert_eq!(partial["corpus"]["observed_configs"], 1);
    assert_eq!(partial["corpus"]["declared_verified_n"], 3);
    assert!(partial["corpus"]["clustering"]["value"]["global_n_eff"]["value"].is_null());
    assert_eq!(partial["current_noise"]["n"], 3);
    assert!(partial["current_noise"]["n_eff"].is_null());
    assert_eq!(partial["current_noise"]["q95"], all["current_noise"]["q95"]);
}

#[test]
fn source_and_external_packet_errors_preserve_the_existing_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = fixture(root, "cell");
    let args = strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "1",
        "--research-root",
        root.to_str().unwrap(),
    ]);
    assert!(tv4(&args).status.success());
    let before = fs::read(out.join("stats.json")).unwrap();
    for name in [
        "metrics.json",
        "trades.csv",
        "equity.csv",
        "manifest.json",
        "stats-inputs.json",
    ] {
        let path = out.join(name);
        let original = fs::read(&path).unwrap();
        let mut corrupt = original.clone();
        corrupt.extend(b"corrompu");
        fs::write(&path, corrupt).unwrap();
        assert_eq!(tv4(&args).status.code(), Some(1), "{name}");
        assert_eq!(before, fs::read(out.join("stats.json")).unwrap());
        fs::write(path, original).unwrap();
    }
    let packet: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("stats-inputs.json")).unwrap()).unwrap();
    let external = root.join("external.json");
    for (pointer, value) in [
        ("/schema", serde_json::json!("other")),
        ("/provenance/initial_cash", serde_json::json!(301.)),
        (
            "/provenance/window/start_ms",
            serde_json::json!(1_704_067_260_000i64),
        ),
        ("/provenance/profile", serde_json::json!("maker")),
        ("/provenance/line", serde_json::json!("full")),
    ] {
        let mut wrong = packet.clone();
        *wrong.pointer_mut(pointer).unwrap() = value;
        fs::write(&external, serde_json::to_vec(&wrong).unwrap()).unwrap();
        let mut args = args.clone();
        args.extend(strings(&["--inputs", external.to_str().unwrap()]));
        assert_eq!(tv4(&args).status.code(), Some(1), "{pointer}");
        assert_eq!(before, fs::read(out.join("stats.json")).unwrap());
    }
    let mut bad_pair = args.clone();
    bad_pair.extend(strings(&["--delayed-run", out.to_str().unwrap()]));
    assert_eq!(tv4(&bad_pair).status.code(), Some(2));
    assert_eq!(before, fs::read(out.join("stats.json")).unwrap());
}

#[test]
fn incomplete_daily_panel_is_disclosed_and_contradictory_duplicates_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = fixture(root, "cell");
    let peer = root.join("modified-peer");
    fs::create_dir(&peer).unwrap();
    for name in [
        "metrics.json",
        "trades.csv",
        "equity.csv",
        "manifest.json",
        "stats-inputs.json",
    ] {
        fs::copy(out.join(name), peer.join(name)).unwrap();
    }
    let mut packet: tv4::report::stats_inputs::StatsInputs =
        serde_json::from_slice(&fs::read(peer.join("stats-inputs.json")).unwrap()).unwrap();
    let pos = packet
        .marks
        .iter()
        .position(|m| {
            m.day_close
                && m.at_ms != packet.provenance.window.end_ms
                && !packet.closures.iter().any(|c| c.at_ms == m.at_ms)
        })
        .unwrap();
    packet.marks.remove(pos);
    let bytes = serde_json::to_vec(&packet).unwrap();
    fs::write(peer.join("stats-inputs.json"), &bytes).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(peer.join("manifest.json")).unwrap()).unwrap();
    manifest["stats_inputs"]["hash"] = serde_json::json!(tv4::model::hash::blake3_prefixed(&bytes));
    fs::write(
        peer.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    tv4::report::stats::read_run(&peer)
        .expect("paquet technique incomplet mais explicite et cohérent");
    let args = strings(&[
        "stats",
        peer.to_str().unwrap(),
        "--seed",
        "1",
        "--research-root",
        root.to_str().unwrap(),
    ]);
    let result = tv4(&args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let j: serde_json::Value =
        serde_json::from_slice(&fs::read(peer.join("stats.json")).unwrap()).unwrap();
    assert_eq!(j["corpus"]["observed_configs"], 1);
    assert_eq!(j["corpus"]["complete_daily_configs"], 0);
    assert_eq!(j["corpus"]["clustering"]["availability"], "unavailable");
    assert!(j["current_noise"]["n_eff"].is_null());
    let mut args = args.clone();
    args.extend(strings(&["--peer-run", out.to_str().unwrap()]));
    assert_eq!(tv4(&args).status.code(), Some(2));
}

#[test]
fn outside_git_keeps_n_unknown_and_explicit_bad_root_is_an_artifact_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out = fixture(root, "cell");
    let outside = tempfile::tempdir().unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_tv4"))
        .current_dir(outside.path())
        .args(["stats", out.to_str().unwrap(), "--seed", "1"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let j: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("stats.json")).unwrap()).unwrap();
    assert_eq!(j["registry"]["availability"], "unavailable");
    assert!(j["corpus"]["declared_verified_n"].is_null());
    assert!(j["current_noise"].is_null());
    let result = tv4(&strings(&[
        "stats",
        out.to_str().unwrap(),
        "--seed",
        "1",
        "--research-root",
        root.join("absent").to_str().unwrap(),
    ]));
    assert_eq!(result.status.code(), Some(1));
}
