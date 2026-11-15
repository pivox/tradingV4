//! Admissions sur dépôts et métadonnées synthétiques, sans ouverture de marché réel.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}
fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    git(d.path(), &["config", "user.name", "Test"]);
    git(d.path(), &["config", "user.email", "test@example.invalid"]);
    fs::write(d.path().join("initial"), "fixture").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-qm", "fixture"]);
    d
}
fn fiche(root: &Path, commit: bool) {
    fs::create_dir_all(root.join("research")).unwrap();
    let fields = [
        "H-NNN",
        "Date / auteur",
        "Famille",
        "Hypothèse économique",
        "Règle d'entrée",
        "Règle de sortie",
        "Paramètres",
        "Timeframe",
        "Univers",
        "Profil de coûts",
        "Faisabilité G0",
        "n_min / μ_cible",
        "Signaux attendus",
        "Contrôle",
        "Règle d'abandon",
        "Connaissances antérieures",
        "Claim holdout",
        "dataset_hash",
    ];
    let text = fields
        .iter()
        .map(|f| {
            format!(
                "{f} : {}\n",
                match *f {
                    "H-NNN" => "H-001",
                    "Famille" => "F1",
                    _ => "renseigné",
                }
            )
        })
        .collect::<String>();
    fs::write(root.join("research/H-001.md"), text).unwrap();
    if commit {
        git(root, &["add", "research/H-001.md"]);
        git(root, &["commit", "-qm", "fiche"]);
    }
}
fn invoke(root: &Path, hypothesis: bool) -> Output {
    invoke_delay(root, hypothesis, 0, false)
}
fn invoke_delay(root: &Path, hypothesis: bool, delay: u32, validation: bool) -> Output {
    let hash = format!("blake3:{}", "0".repeat(64));
    let mut m = serde_json::json!({"name":"dev","exchange":"kraken","symbols":["BTC-USD"],"tfs":["1m"],"window":["2018-01-01T00:00:00Z","2018-01-02T00:00:00Z"],"gaps_policy":"zero_volume_marked","row_count":1440,"dataset_hash":hash,"hash_format":"dsh-v1","built_at":"2026-11-01T00:00:00Z","tv4_version":"0.1.0","block":"DEV","decision_window":["2018-01-01T00:00:00Z","2018-01-02T00:00:00Z"],"quality_exclusions":{},"files":[{"path":"data/candles/kraken/BTC-USD/1m/absent.parquet","symbol":"BTC-USD","tf":"1m","period":"2018-01","content_hash":hash,"row_count":1440,"snapshot":false}]});
    if validation {
        m["block"] = "VALIDATION".into();
        m["window"] = serde_json::json!(["2023-03-01T00:00:00Z", "2023-03-02T00:00:00Z"]);
        m["decision_window"] = m["window"].clone();
    }
    fs::write(root.join("dev.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_tv4"));
    c.current_dir(root)
        .args(["backtest", "--root"])
        .arg(root)
        .args([
            "--dataset",
            "dev.json",
            "--symbols",
            "BTC-USD",
            "--strategy",
            "sma_cross",
            "--params",
        ])
        .arg(project.join("config/strategies/sma_cross.toml"))
        .arg("--fees-file")
        .arg(project.join("config/fees.toml"))
        .arg("--instruments-file")
        .arg(project.join("config/instruments.toml"))
        .arg("--research-root")
        .arg(root);
    c.arg("--entry-delay-bars").arg(delay.to_string());
    if validation {
        c.arg("--okx-subset").arg(root.join("subset.json"));
    }
    if hypothesis {
        c.args(["--hypothesis", "H-001"]);
    }
    c.output().unwrap()
}
#[test]
fn dev_sans_fiche_refuse_avant_parquet_sans_append() {
    let d = repo();
    let o = invoke(d.path(), false);
    assert_eq!(
        o.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("hypothesis"));
    assert!(!d.path().join("research/trials.csv").exists());
}
#[test]
fn dev_fiche_non_commitee_refuse_avant_parquet_sans_append() {
    let d = repo();
    fiche(d.path(), false);
    let o = invoke(d.path(), true);
    assert_eq!(
        o.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!d.path().join("research/trials.csv").exists());
}
#[test]
fn dev_admis_avant_parquet_et_rejeu_n_stable() {
    let d = repo();
    fiche(d.path(), true);
    for _ in 0..2 {
        let o = invoke(d.path(), true);
        assert_eq!(
            o.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let csv = fs::read(d.path().join("research/trials.csv")).unwrap();
    let r = tv4::research::registry::Registry::from_csv(&csv).unwrap();
    assert_eq!(r.n(), 1);
    assert_eq!(r.reads().len(), 2);
    assert!(
        r.events()
            .iter()
            .any(|e| e.status == tv4::research::registry::Status::Failed)
    );
}

#[test]
fn delayed_configuration_is_a_new_s12_trial_before_any_market_read() {
    let d = repo();
    fiche(d.path(), true);
    for delay in [0, 1, 1] {
        let o = invoke_delay(d.path(), true, delay, false);
        assert_eq!(
            o.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let csv = fs::read(d.path().join("research/trials.csv")).unwrap();
    let registry = tv4::research::registry::Registry::from_csv(&csv).unwrap();
    assert_eq!(registry.n(), 2);
    assert_eq!(registry.reads().len(), 3);
}

#[test]
fn a_frozen_normal_config_refuses_the_delayed_variant_before_missing_parquet() {
    use sha2::{Digest as _, Sha256};
    use std::collections::BTreeMap;
    use tv4::model::universe::{PairSubset, SubsetRule, SubsetSource};
    let d = repo();
    let root = d.path();
    fiche(root, true);
    let first = invoke_delay(root, true, 0, false);
    assert_eq!(first.status.code(), Some(1));
    let registry_bytes = fs::read(root.join("research/trials.csv")).unwrap();
    let registry = tv4::research::registry::Registry::from_csv(&registry_bytes).unwrap();
    let hash = registry.trials().keys().next().unwrap().as_str();
    let subset = PairSubset::new(
        "okx_usdc_tradable",
        SubsetSource {
            instruments_file: "synthetic.json".into(),
            instruments_hash: format!("blake3:{}", "7".repeat(64)),
            site: "eea".into(),
            fetched_at: "2026-10-30T10:15:00Z".into(),
        },
        SubsetRule {
            quote: "USDC".into(),
            state: "live".into(),
            trade_quote_ccy: true,
            aliases: BTreeMap::new(),
        },
        vec!["BTC".into()],
    )
    .unwrap();
    tv4::store::universe::write_pair_subset(&root.join("subset.json"), &subset).unwrap();
    let fiche_bytes = fs::read(root.join("research/H-001.md")).unwrap();
    let fiche_sha = Sha256::digest(&fiche_bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let binary = fs::read(env!("CARGO_BIN_EXE_tv4")).unwrap();
    let candidate = serde_json::json!({"schema":"tv4-frozen-candidate-v1","hypothesis":"H-001","fiche_sha256":fiche_sha,"config_hashes":[hash],"engine_commit":tv4::GIT_SHA,"engine_binary_hash":tv4::model::hash::blake3_prefixed(&binary),"okx_subset_hash":subset.subset_hash});
    fs::create_dir_all(root.join("research/frozen")).unwrap();
    fs::write(
        root.join("research/frozen/H-001.json"),
        serde_json::to_vec(&candidate).unwrap(),
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "gel synthétique"]);
    git(
        root,
        &["tag", "-a", "frozen/H-001", "-m", "gel synthétique"],
    );
    let rejected = invoke_delay(root, true, 1, true);
    assert_eq!(
        rejected.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let error = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        error.contains("ensemble exact des configurations gelées"),
        "{error}"
    );
    assert!(!error.contains("absent.parquet"));
    assert_eq!(
        fs::read(root.join("research/trials.csv")).unwrap(),
        registry_bytes
    );
}
