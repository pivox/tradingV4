//! `tv4 bench` (S8, spécification §10) : on lance le vrai binaire sur la fixture.

use std::path::PathBuf;
use std::process::{Command, Output};

fn tv4(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(args)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

fn repo(relative: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
        .display()
        .to_string()
}

/// Arguments communs à `backtest` et à `bench` (mêmes entrées).
fn inputs() -> Vec<String> {
    [
        "--fixture",
        &repo("fixtures/kraken_xbtusd_1m.csv"),
        "--strategy",
        "sma_cross",
        "--params",
        &repo("config/strategies/sma_cross.toml"),
        "--fees-file",
        &repo("config/fees.toml"),
        "--instruments-file",
        &repo("config/instruments.toml"),
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

#[test]
fn bench_sur_la_fixture_mesure_la_boucle_et_on_bar() {
    let mut args = vec!["bench".to_string()];
    args.extend(inputs());
    args.extend(["--repeat".to_string(), "1".to_string()]);
    let sortie = tv4(&args);
    let stdout = String::from_utf8_lossy(&sortie.stdout);
    let stderr = String::from_utf8_lossy(&sortie.stderr);
    assert_eq!(sortie.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("ns/1m"), "{stdout}");
    assert!(stdout.contains("ns/appel"), "{stdout}");
    assert!(stdout.contains("tv4 bench : sma_cross 1.1.0"), "{stdout}");
    assert!(stdout.contains("1 répétitions"), "{stdout}");

    // Le nombre d'appels de on_bar égale `funnel.evaluations` d'un backtest identique.
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    let mut backtest = vec!["backtest".to_string()];
    backtest.extend(inputs());
    backtest.extend(["--out".to_string(), out.display().to_string()]);
    let sortie = tv4(&backtest);
    assert_eq!(sortie.status.code(), Some(0));
    let metrics: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("metrics.json")).unwrap()).unwrap();
    let evaluations = metrics["funnel"]["evaluations"].as_u64().unwrap();
    assert!(evaluations > 0);
    let bars = metrics["window"]["bars"].as_u64().unwrap();
    assert!(
        stdout.contains(&format!("{bars} bougies 1m dans la fenêtre")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("on_bar : {evaluations} appels,")),
        "{stdout}"
    );
}

#[test]
fn bench_n_ecrit_rien_et_refuse_repeat_zero() {
    let dossier = tempfile::tempdir().unwrap();
    let mut args = vec!["bench".to_string()];
    args.extend(inputs());
    args.extend(["--root".to_string(), dossier.path().display().to_string()]);
    args.extend(["--repeat".to_string(), "1".to_string()]);
    assert_eq!(tv4(&args).status.code(), Some(0));
    assert_eq!(std::fs::read_dir(dossier.path()).unwrap().count(), 0);

    let mut zero = vec!["bench".to_string()];
    zero.extend(inputs());
    zero.extend(["--repeat".to_string(), "0".to_string()]);
    assert_eq!(tv4(&zero).status.code(), Some(2));
}
