//! Provenance S12 des rapports synthétiques, indépendante de leur résultat de simulation.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tv4::model::hash;

fn copy_run(root: &Path, name: &str, research: bool) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../control-plane/tests/fixtures")
        .join(if research {
            "run-recherche"
        } else {
            "run-exemple"
        });
    let dir = root.join(name);
    fs::create_dir(&dir).unwrap();
    for file in ["manifest.json", "metrics.json", "trades.csv", "equity.csv"] {
        fs::copy(source.join(file), dir.join(file)).unwrap();
    }
    let mut manifest = read(&dir, "manifest.json");
    for file in ["metrics.json", "trades.csv", "equity.csv"] {
        manifest["outputs"]["files"][file] =
            json!(hash::blake3_prefixed(&fs::read(dir.join(file)).unwrap()));
    }
    manifest["result_hash"] = manifest["outputs"]["files"]["trades.csv"].clone();
    write(&dir, "manifest.json", &manifest);
    dir
}
fn read(dir: &Path, file: &str) -> Value {
    serde_json::from_slice(&fs::read(dir.join(file)).unwrap()).unwrap()
}
fn write(dir: &Path, file: &str, value: &Value) {
    fs::write(dir.join(file), serde_json::to_vec(value).unwrap()).unwrap();
}
fn context(dir: &Path) -> Value {
    let m = read(dir, "manifest.json");
    json!({"schema":"tv4-governance-v1","config_hash":m["config_hash"],"dataset_hash":m["dataset_hash"],"trial_id":17,"read_id":23,"hypothesis":{"identifiant":"H-001","sha256":"a".repeat(64)},"frozen_tag":null,"frozen_at":null,"source_block":null,"effective_block":null,"generation":0,"claim_holdout":null,"noise":{"n":50,"n_eff":null,"selection_years":5,"expected":1.01799,"approx_q90":1.25092,"q95":1.37867}})
}
fn set_context(dir: &Path, g: Value) {
    let mut m = read(dir, "manifest.json");
    m["governance"] = g;
    write(dir, "manifest.json", &m);
}
fn verify(a: &Path, b: &Path) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tv4"))
        .arg("verify")
        .arg(a)
        .arg(b)
        .output()
        .unwrap();
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}
fn frame(dir: &Path, source: &str, effective: &str, generation: u32, guard: &str) {
    let mut metrics = read(dir, "metrics.json");
    let mut manifest = read(dir, "manifest.json");
    metrics["research"]["block"] = json!(effective);
    metrics["research"]["identity"]["block"] = json!(effective);
    metrics["data"]["block"] = json!(effective);
    manifest["data"]["block"] = json!(effective);
    metrics["research"]["guard"] = json!(guard);
    let identity = format!(
        "blake3:{}",
        hash::hash_json(&metrics["research"]["identity"]).unwrap()
    );
    metrics["research"]["identity_hash"] = json!(identity);
    manifest["run_id"] = json!(tv4::report::manifest::run_id_s11(
        manifest["config_hash"].as_str().unwrap(),
        manifest["dataset_hash"].as_str().unwrap(),
        manifest["window"]["start_ms"].as_i64().unwrap(),
        manifest["window"]["end_ms"].as_i64().unwrap(),
        manifest["initial_cash"].as_f64().unwrap(),
        &identity
    ));
    write(dir, "metrics.json", &metrics);
    manifest["outputs"]["files"]["metrics.json"] = json!(hash::blake3_prefixed(
        &fs::read(dir.join("metrics.json")).unwrap()
    ));
    write(dir, "manifest.json", &manifest);
    let mut g = context(dir);
    g["source_block"] = json!(source);
    g["effective_block"] = json!(effective);
    g["generation"] = json!(generation);
    if ["VALIDATION", "HOLDOUT"].contains(&effective) {
        g["frozen_tag"] = json!("frozen/H-001");
        g["frozen_at"] = json!("2026-01-02T03:04:05Z");
    }
    if effective == "HOLDOUT" {
        g["claim_holdout"] = json!(3);
    }
    set_context(dir, g);
}
#[test]
fn anciens_contextes_absents_et_null_restent_lisibles() {
    let root = tempfile::tempdir().unwrap();
    for research in [false, true] {
        let dir = copy_run(
            root.path(),
            if research { "research" } else { "legacy" },
            research,
        );
        assert_eq!(verify(&dir, &dir).0, Some(0));
        set_context(&dir, Value::Null);
        assert_eq!(verify(&dir, &dir).0, Some(0));
    }
}
#[test]
fn relectures_identiques_avec_compteurs_historiques_differents_donnent_zero() {
    let root = tempfile::tempdir().unwrap();
    let a = copy_run(root.path(), "a", true);
    let b = copy_run(root.path(), "b", true);
    frame(&a, "DEV", "DEV", 0, "none");
    frame(&b, "DEV", "DEV", 0, "none");
    let mut g = read(&b, "manifest.json")["governance"].clone();
    g["trial_id"] = json!(29);
    g["read_id"] = json!(45);
    g["noise"] =
        serde_json::to_value(tv4::research::noise::noise_context(436, 5.0).unwrap()).unwrap();
    set_context(&b, g);
    let (code, text) = verify(&a, &b);
    assert_eq!(code, Some(0), "{text}");
}
#[test]
fn contexte_present_exige_types_schema_et_hashes_coherents() {
    let cases = [
        ("/schema", json!("tv4-governance-v2")),
        ("/config_hash", json!("f".repeat(64))),
        ("/dataset_hash", json!(format!("blake3:{}", "f".repeat(64)))),
        ("/trial_id", json!(0)),
        ("/read_id", json!("23")),
        ("/read_id", json!(-1)),
        ("/generation", json!(-1)),
        ("/noise/n", json!(0)),
        ("/noise/n_eff", json!(0)),
        ("/noise/n_eff", json!(51)),
        ("/noise/selection_years", json!(0)),
        ("/noise/selection_years", json!(-1)),
        ("/noise/selection_years", json!("INF")),
        ("/noise/expected", json!(false)),
        ("/noise/q95", json!("1.37")),
        ("/hypothesis/sha256", json!("bad")),
        ("/hypothesis/identifiant", json!("H-000")),
        ("/hypothesis/identifiant", json!("H-0001")),
        ("/hypothesis/identifiant", json!("H-01")),
        ("/source_block", json!("OTHER")),
        ("/effective_block", json!("DEV")),
        ("/claim_holdout", json!(1)),
        ("/frozen_at", json!("2026-02-31T03:04:05Z")),
        ("/frozen_at", json!("2026-01-02T99:04:05Z")),
        ("/frozen_at", json!("2026-01-02T03:04:05+01:00")),
    ];
    let root = tempfile::tempdir().unwrap();
    for (i, (path, value)) in cases.into_iter().enumerate() {
        let dir = copy_run(root.path(), &format!("bad{i}"), false);
        let mut g = context(&dir);
        *g.pointer_mut(path).unwrap() = value;
        set_context(&dir, g);
        let (code, text) = verify(&dir, &dir);
        assert_eq!(code, Some(1), "{path}: {text}");
        assert!(text.contains("governance"), "{path}: {text}");
    }
}
#[test]
fn chaque_champ_nullable_est_obligatoire_si_le_contexte_existe() {
    let root = tempfile::tempdir().unwrap();
    for field in [
        "hypothesis",
        "frozen_tag",
        "frozen_at",
        "source_block",
        "effective_block",
        "claim_holdout",
    ] {
        let dir = copy_run(root.path(), field, false);
        let mut g = context(&dir);
        g.as_object_mut().unwrap().remove(field);
        set_context(&dir, g);
        let (code, text) = verify(&dir, &dir);
        assert_eq!(code, Some(1), "{field}: {text}");
    }
    let dir = copy_run(root.path(), "noise", false);
    let mut g = context(&dir);
    g["noise"].as_object_mut().unwrap().remove("n_eff");
    set_context(&dir, g);
    assert_eq!(verify(&dir, &dir).0, Some(1));
}
#[test]
fn trames_initiales_et_rotations_acceptees() {
    let root = tempfile::tempdir().unwrap();
    for (i, (source, effective, generation)) in [
        ("DEV", "DEV", 0),
        ("VALIDATION", "VALIDATION", 0),
        ("HOLDOUT", "HOLDOUT", 0),
        ("HOLDOUT", "VALIDATION", 1),
        ("FORWARD", "HOLDOUT", 2),
        ("DEV", "DEV", 1),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = copy_run(root.path(), &format!("frame{i}"), true);
        frame(
            &dir,
            source,
            effective,
            generation,
            if ["VALIDATION", "HOLDOUT"].contains(&effective) {
                "guarded"
            } else {
                "none"
            },
        );
        let (code, text) = verify(&dir, &dir);
        assert_eq!(code, Some(0), "{source}/{effective}: {text}");
    }
}
#[test]
fn protections_incompletes_ou_incoherentes_refusees() {
    let cases = [
        ("/hypothesis", Value::Null),
        ("/frozen_tag", Value::Null),
        ("/frozen_tag", json!("frozen/H-002")),
        ("/frozen_at", Value::Null),
        ("/claim_holdout", Value::Null),
        ("/claim_holdout", json!(0)),
        ("/source_block", json!("FORWARD")),
        ("/effective_block", json!("VALIDATION")),
    ];
    let root = tempfile::tempdir().unwrap();
    for (i, (path, value)) in cases.into_iter().enumerate() {
        let dir = copy_run(root.path(), &format!("protected{i}"), true);
        frame(&dir, "HOLDOUT", "HOLDOUT", 0, "guarded");
        let mut m = read(&dir, "manifest.json");
        *m["governance"].pointer_mut(path).unwrap() = value;
        write(&dir, "manifest.json", &m);
        let (code, text) = verify(&dir, &dir);
        assert_eq!(code, Some(1), "{path}: {text}");
        assert!(text.contains("governance"), "{path}: {text}");
    }
}
#[test]
fn guarded_exige_contexte_et_protection_exige_guarded() {
    let root = tempfile::tempdir().unwrap();
    let dir = copy_run(root.path(), "missing", true);
    frame(&dir, "DEV", "DEV", 0, "guarded");
    let mut m = read(&dir, "manifest.json");
    m.as_object_mut().unwrap().remove("governance");
    write(&dir, "manifest.json", &m);
    let (code, text) = verify(&dir, &dir);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("governance"));
    let dir = copy_run(root.path(), "unguarded", true);
    frame(&dir, "HOLDOUT", "HOLDOUT", 0, "unguarded");
    let (code, text) = verify(&dir, &dir);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("research.guard"));
}
#[test]
fn utc_fractionnaire_et_null_des_estimations_restent_lisibles() {
    let root = tempfile::tempdir().unwrap();
    let dir = copy_run(root.path(), "fraction", false);
    let mut g = context(&dir);
    g["frozen_at"] = json!("2026-01-02T03:04:05.123456789+00:00");
    g["noise"]["n"] = json!(1);
    g["noise"]["expected"] = json!(0);
    g["noise"]["approx_q90"] = json!(0);
    g["noise"]["q95"] = Value::Null;
    set_context(&dir, g);
    let (code, text) = verify(&dir, &dir);
    assert_eq!(code, Some(0), "{text}");
}
