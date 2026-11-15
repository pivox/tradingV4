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
    let hash = format!("blake3:{}", "0".repeat(64));
    let m = serde_json::json!({"name":"dev","exchange":"kraken","symbols":["BTC-USD"],"tfs":["1m"],"window":["2018-01-01T00:00:00Z","2018-01-02T00:00:00Z"],"gaps_policy":"zero_volume_marked","row_count":1440,"dataset_hash":hash,"hash_format":"dsh-v1","built_at":"2026-11-01T00:00:00Z","tv4_version":"0.1.0","block":"DEV","decision_window":["2018-01-01T00:00:00Z","2018-01-02T00:00:00Z"],"quality_exclusions":{},"files":[{"path":"data/candles/kraken/BTC-USD/1m/absent.parquet","symbol":"BTC-USD","tf":"1m","period":"2018-01","content_hash":hash,"row_count":1440,"snapshot":false}]});
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
