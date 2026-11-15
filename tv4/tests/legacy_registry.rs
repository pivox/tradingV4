//! Les démonstrations hors bloc et les benchmarks sont comptés sans polluer le checkout.
mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tv4::research::registry::Registry;

fn repo(path: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(path)
        .display()
        .to_string()
}
fn setup() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(root.path())
            .status()
            .unwrap()
            .success()
    );
    let mut csv = String::new();
    for bar in common::synthetic_candles(400, 42, 1_577_836_800_000, 0.0) {
        writeln!(
            &mut csv,
            "{},{},{},{},{},{},{}",
            bar.open_time_ms / 1000,
            bar.open,
            bar.high,
            bar.low,
            bar.close,
            bar.volume,
            bar.trades.unwrap_or(0)
        )
        .unwrap();
    }
    fs::write(root.path().join("exemple.csv"), csv).unwrap();
    fs::write(
        root.path().join("params.toml"),
        "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0\n",
    )
    .unwrap();
    root
}
fn run(root: &Path, command: &str, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args([command, "--research-root"])
        .arg(root)
        .arg("--fixture")
        .arg(root.join("exemple.csv"))
        .args(["--strategy", "sma_cross", "--params"])
        .arg(root.join("params.toml"))
        .args([
            "--fees-file",
            &repo("config/fees.toml"),
            "--instruments-file",
            &repo("config/instruments.toml"),
        ])
        .args(extra)
        .output()
        .unwrap()
}
fn registry(root: &Path) -> Registry {
    let path = root.join("research/trials.csv");
    assert!(path.is_file(), "le run doit créer son registre");
    Registry::from_csv(&fs::read(path).unwrap()).unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn out(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

#[test]
fn une_fixture_enregistre_le_hash_et_rejoue_sans_inventer_un_nouvel_essai() {
    let root = setup();
    let a = out(root.path(), "a");
    let b = out(root.path(), "b");
    let first = run(root.path(), "backtest", &["--out", a.to_str().unwrap()]);
    success(&first);
    let state = registry(root.path());
    assert_eq!(state.n(), 1);
    assert_eq!(state.reads().len(), 1);
    assert!(state.pending_reads().is_empty());
    let second = run(root.path(), "backtest", &["--out", b.to_str().unwrap()]);
    success(&second);
    let state = registry(root.path());
    assert_eq!(state.n(), 1);
    assert_eq!(state.reads().len(), 2);
    assert!(state.pending_reads().is_empty());
    assert_eq!(
        fs::read(a.join("metrics.json")).unwrap(),
        fs::read(b.join("metrics.json")).unwrap()
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(b.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["governance"]["noise"]["n"], 1);
    assert_eq!(manifest["governance"]["read_id"], 2);
    assert!(String::from_utf8_lossy(&second.stdout).contains("N = 1"));
}

#[test]
fn le_benchmark_ne_compte_pas_chaque_repetition_comme_un_essai() {
    let root = setup();
    let result = run(root.path(), "bench", &["--repeat", "2"]);
    success(&result);
    let state = registry(root.path());
    assert_eq!(state.n(), 1);
    assert_eq!(state.reads().len(), 1);
    assert!(state.pending_reads().is_empty());
    assert!(String::from_utf8_lossy(&result.stdout).contains("N = 1"));
}

#[test]
fn bench_refuse_les_blocs_proteges_avant_la_lecture_de_parquet() {
    let root = setup();
    for (block, start, end) in [
        ("VALIDATION", "2023-03-01T00:00:00Z", "2023-04-01T00:00:00Z"),
        ("HOLDOUT", "2025-03-01T00:00:00Z", "2025-04-01T00:00:00Z"),
    ] {
        let path = root.path().join("protege.json");
        let hash = format!("blake3:{}", "0".repeat(64));
        let manifest = serde_json::json!({
            "name": "protege", "exchange": "kraken", "symbols": ["BTC-USD"], "tfs": ["1m"],
            "window": [start, end], "decision_window": [start, end], "block": block,
            "gaps_policy": "zero_volume_marked", "quality_exclusions": {}, "row_count": 1,
            "dataset_hash": hash, "hash_format": "dsh-v1", "built_at": "2026-11-01T00:00:00Z",
            "tv4_version": "0.1.0", "files": [{"path": "absent.parquet", "symbol": "BTC-USD",
                "tf": "1m", "period": "2025-03", "content_hash": hash, "row_count": 1, "snapshot": false}]
        });
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_tv4"))
            .args(["bench", "--research-root"])
            .arg(root.path())
            .arg("--dataset")
            .arg(&path)
            .args(["--strategy", "sma_cross", "--params"])
            .arg(root.path().join("params.toml"))
            .args([
                "--fees-file",
                &repo("config/fees.toml"),
                "--instruments-file",
                &repo("config/instruments.toml"),
                "--repeat",
                "1",
            ])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{block}: {stderr}");
        assert!(
            stderr.contains("bench") && stderr.contains("protég"),
            "{stderr}"
        );
        assert!(
            !root.path().join("research/trials.csv").exists(),
            "refus sans admission"
        );
    }
}

#[test]
fn le_portefeuille_et_ses_trois_profils_inscrivent_leurs_configurations_reelles() {
    use tv4::model::{ExchangeId, Timeframe};
    use tv4::store::{Columns, GapsPolicy, MultiBuildRequest, Partition, Window};
    for profiles in [false, true] {
        let root = setup();
        let start = 1_577_836_800_000;
        let now = 1_893_456_000_000;
        let symbols: [tv4::store::Symbol; 2] =
            ["BTC-USD".parse().unwrap(), "ETH-USD".parse().unwrap()];
        for (i, symbol) in symbols.iter().enumerate() {
            let data = common::synthetic_candles(400, 42 + i as u64, start, 0.0);
            tv4::store::write_partition(
                root.path(),
                &Partition::new(ExchangeId::Kraken, symbol.clone(), Timeframe::M1),
                &Columns::from_candles(&data),
                "synthetic",
                now,
            )
            .unwrap();
        }
        tv4::store::build_dataset_multi(
            root.path(),
            &MultiBuildRequest {
                name: "compte",
                exchange: ExchangeId::Kraken,
                symbols: &symbols,
                tf: Timeframe::M1,
                window: Window::new(start, start + 400 * 60_000).unwrap(),
                gaps_policy: GapsPolicy::Skip,
            },
            now,
        )
        .unwrap();
        let instruments = root.path().join("instruments.toml");
        fs::write(&instruments, "[kraken.BTC-USD]\ntick_size=0.1\nlot_size=1e-8\nmin_qty=0.0\nmin_notional=0.0\n[kraken.ETH-USD]\ntick_size=0.1\nlot_size=1e-8\nmin_qty=0.0\nmin_notional=0.0\n").unwrap();
        let out = root.path().join("run");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tv4"));
        cmd.args(["backtest", "--research-root"])
            .arg(root.path())
            .arg("--root")
            .arg(root.path())
            .args([
                "--dataset",
                "compte",
                "--symbols",
                "BTC-USD,ETH-USD",
                "--strategy",
                "sma_cross",
                "--params",
            ])
            .arg(root.path().join("params.toml"))
            .args([
                "--fees-file",
                &repo("config/fees.toml"),
                "--instruments-file",
                instruments.to_str().unwrap(),
            ])
            .arg("--out")
            .arg(&out);
        if profiles {
            cmd.args(["--costs", &repo("config/costs.toml")]);
        }
        let result = cmd.output().unwrap();
        success(&result);
        let state = registry(root.path());
        assert_eq!(state.n(), if profiles { 3 } else { 1 });
        assert_eq!(state.reads().len(), 1);
        assert!(state.pending_reads().is_empty());
        for part in if profiles {
            vec!["baseline", "adverse", "maker"]
        } else {
            vec![""]
        } {
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(out.join(part).join("manifest.json")).unwrap())
                    .unwrap();
            let hash = manifest["config_hash"].as_str().unwrap();
            assert!(state.trials().keys().any(|key| key.as_str() == hash));
            assert_eq!(manifest["governance"]["config_hash"], hash);
        }
    }
}

/// La copie ouverte pour l'aiguillage est remplacée avant l'ouverture du chemin legacy.
#[cfg(unix)]
#[test]
fn un_manifeste_remplace_ne_contourne_pas_l_admission_protegee() {
    use std::io::Write;
    for profiles in [false, true] {
        let root = setup();
        let path = root.path().join("remplace.json");
        assert!(
            Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let hash = format!("blake3:{}", "0".repeat(64));
        let ordinary = serde_json::json!({
            "name": "remplace", "exchange": "kraken", "symbols": ["BTC-USD"], "tfs": ["1m"],
            "window": ["2025-03-01T00:00:00Z", "2025-04-01T00:00:00Z"],
            "gaps_policy": "zero_volume_marked", "row_count": 1,
            "dataset_hash": hash, "hash_format": "dsh-v1", "built_at": "2026-11-01T00:00:00Z",
            "tv4_version": "0.1.0", "files": [{"path": "absent.parquet", "symbol": "BTC-USD",
                "tf": "1m", "period": "2025-03", "content_hash": hash, "row_count": 1, "snapshot": false}]
        });
        let mut protected = ordinary.clone();
        protected["block"] = serde_json::json!("HOLDOUT");
        protected["decision_window"] = ordinary["window"].clone();
        protected["quality_exclusions"] = serde_json::json!({});
        let swap = path.clone();
        let writer = std::thread::spawn(move || {
            let mut fifo = fs::OpenOptions::new().write(true).open(&swap).unwrap();
            fifo.write_all(&serde_json::to_vec(&ordinary).unwrap())
                .unwrap();
            fs::remove_file(&swap).unwrap();
            fs::write(&swap, serde_json::to_vec(&protected).unwrap()).unwrap();
            // EOF débloque dispatch seulement après le remplacement.
        });
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tv4"));
        cmd.args(["backtest", "--research-root"])
            .arg(root.path())
            .arg("--root")
            .arg(root.path())
            .arg("--dataset")
            .arg(&path)
            .args([
                "--symbols",
                "BTC-USD",
                "--strategy",
                "sma_cross",
                "--params",
            ])
            .arg(root.path().join("params.toml"))
            .args([
                "--fees-file",
                &repo("config/fees.toml"),
                "--instruments-file",
                &repo("config/instruments.toml"),
            ])
            .arg("--out")
            .arg(root.path().join("run"));
        if profiles {
            cmd.args(["--costs", &repo("config/costs.toml")]);
        }
        let result = cmd.output().unwrap();
        writer.join().unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_eq!(result.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains("manifeste") && stderr.contains("recherche"),
            "{stderr}"
        );
        assert!(!root.path().join("research/trials.csv").exists());
    }
}
