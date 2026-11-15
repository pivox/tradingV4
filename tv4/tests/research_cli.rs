//! Les commandes de recherche opèrent uniquement sur des dépôts synthétiques.
use sha2::{Digest as _, Sha256};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tv4::model::blocks::Block;
use tv4::research::registry::{
    Admission, AdmissionTime, ConfigSpec, Digest, HypothesisRef, Registry, UtcDate, encode_events,
    header_csv,
};

const FIELDS: [&str; 18] = [
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
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn git(root: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tmp");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    fs::create_dir_all(dir.path().join("research/frozen")).expect("research");
    fs::write(dir.path().join("initial"), "fixture").expect("initial");
    commit(dir.path());
    dir
}
fn commit(root: &Path) {
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "fixture"]);
}
fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .current_dir(root)
        .args(["research"])
        .args(args)
        .output()
        .expect("CLI")
}
fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("UTF8")
}
fn refused(out: Output, text: &str) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(text),
        "attendu {text}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn fiche(root: &Path) -> Vec<u8> {
    let text = FIELDS
        .iter()
        .map(|key| {
            format!(
                "{key} : {}\n",
                match *key {
                    "H-NNN" => "H-001",
                    "Famille" => "F1",
                    _ => "renseigné",
                }
            )
        })
        .collect::<String>();
    fs::write(root.join("research/H-001.md"), &text).expect("fiche");
    text.into_bytes()
}
fn seed(root: &Path, bytes: &[u8]) {
    let hypothesis =
        HypothesisRef::new("H-001".into(), Digest::new(sha(bytes)).expect("sha")).expect("ref");
    let date = UtcDate::new("2026-01-01T00:00:00Z").expect("date");
    let batch = Registry::default()
        .propose_admission(&Admission {
            configs: vec![ConfigSpec {
                config_hash: Digest::new("a".repeat(64)).expect("hash"),
                family: "F1".into(),
                hypothesis: Some(hypothesis),
            }],
            dataset_hash: Digest::new("b".repeat(64)).expect("dataset"),
            block: Some(Block::Dev),
            time: AdmissionTime::new(date.clone(), date.quarter()).expect("time"),
            generation: 0,
            decision_start_ms: None,
            decision_end_ms: None,
        })
        .expect("admission");
    let mut csv = header_csv();
    csv.extend(encode_events(&batch.events).expect("events"));
    fs::write(root.join("research/trials.csv"), csv).expect("csv");
}
fn candidate(root: &Path, fiche: &[u8]) {
    let binary = fs::read(env!("CARGO_BIN_EXE_tv4")).expect("binary");
    let json = serde_json::json!({"schema": "tv4-frozen-candidate-v1", "hypothesis": "H-001", "fiche_sha256": sha(fiche), "config_hashes": ["a".repeat(64)], "engine_commit": tv4::GIT_SHA, "engine_binary_hash": format!("blake3:{}", blake3::hash(&binary).to_hex()), "okx_subset_hash": format!("blake3:{}", "c".repeat(64))});
    fs::write(
        root.join("research/frozen/H-001.json"),
        serde_json::to_vec_pretty(&json).expect("json"),
    )
    .expect("candidate");
}
fn frozen(annotated: bool) -> tempfile::TempDir {
    let dir = repo();
    let root = dir.path();
    let bytes = fiche(root);
    seed(root, &bytes);
    candidate(root, &bytes);
    commit(root);
    if annotated {
        git(root, &["tag", "-a", "frozen/H-001", "-m", "gel"]);
    } else {
        git(root, &["tag", "frozen/H-001"]);
    }
    dir
}
#[test]
fn status_vide_et_resolution_depuis_un_sous_dossier() {
    let dir = repo();
    let sub = dir.path().join("research/frozen");
    assert!(success(cli(&sub, &["status"])).contains("N = 0"));
    assert!(!dir.path().join("research/trials.csv").exists());
    let readme = fs::read_to_string(dir.path().join("research/README.md")).expect("README");
    assert!(
        readme.contains("VALIDATION")
            && readme.contains("HOLDOUT")
            && readme.contains("Génération")
    );
}
#[test]
fn research_root_explicite_est_independant_du_cwd() {
    let dir = repo();
    let other = tempfile::tempdir().expect("tmp");
    success(cli(
        other.path(),
        &[
            "status",
            "--research-root",
            dir.path().to_str().expect("path"),
        ],
    ));
    assert!(dir.path().join("research/README.md").exists());
    assert!(!other.path().join("research").exists());
}
#[test]
fn claim_exige_identite_fiche_candidate_et_tag() {
    let dir = repo();
    let root = dir.path();
    for id in ["H-000", "../H-001", "H-01", "H-0001", "--bad"] {
        refused(cli(root, &["claim", "--hypothesis", id]), "");
    }
    refused(cli(root, &["claim", "--hypothesis", "H-001"]), "fiche");
    let bytes = fiche(root);
    refused(cli(root, &["claim", "--hypothesis", "H-001"]), "commit");
    commit(root);
    refused(cli(root, &["claim", "--hypothesis", "H-001"]), "candidat");
    candidate(root, &bytes);
    seed(root, &bytes);
    commit(root);
    refused(cli(root, &["claim", "--hypothesis", "H-001"]), "tag");
}
#[test]
fn tag_leger_et_annote_claim_persistant_append_et_doublon_refuse() {
    for annotated in [false, true] {
        let dir = frozen(annotated);
        let root = dir.path();
        let prefix = fs::read(root.join("research/trials.csv")).expect("prefix");
        success(cli(root, &["claim", "--hypothesis", "H-001"]));
        let bytes = fs::read(root.join("research/trials.csv")).expect("csv");
        assert!(bytes.starts_with(&prefix));
        let reg = Registry::from_csv(&bytes).expect("registre");
        assert_eq!(reg.n(), 1);
        assert_eq!(reg.claims().len(), 1);
        assert_eq!(reg.pending_reads().len(), 1);
        refused(cli(root, &["claim", "--hypothesis", "H-001"]), "doublon");
        assert_eq!(
            bytes,
            fs::read(root.join("research/trials.csv")).expect("inchangé")
        );
    }
}
#[test]
fn fiche_modifiee_ou_identite_ou_champ_invalide_refusee() {
    for change in ["modified", "identity", "missing", "duplicate"] {
        let dir = frozen(false);
        let root = dir.path();
        let path = root.join("research/H-001.md");
        let text = fs::read_to_string(&path).expect("fiche");
        let changed = match change {
            "modified" => text.replace("renseigné", "autre"),
            "identity" => text.replace("H-NNN : H-001", "H-NNN : H-002"),
            "missing" => text.replace("Timeframe : renseigné", "Timeframe : "),
            _ => format!("{text}Famille : F1\n"),
        };
        fs::write(&path, changed).expect("modification");
        if change != "modified" {
            commit(root);
        }
        refused(cli(root, &["claim", "--hypothesis", "H-001"]), "fiche");
    }
}
#[test]
fn candidat_incoherent_et_inconnu_refuse_sans_append() {
    for field in [
        "schema",
        "hypothesis",
        "fiche_sha256",
        "engine_commit",
        "engine_binary_hash",
        "okx_subset_hash",
        "config_hashes",
        "unknown",
    ] {
        let dir = frozen(false);
        let root = dir.path();
        let path = root.join("research/frozen/H-001.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        value[field] = if field == "config_hashes" {
            serde_json::json!(["d".repeat(64)])
        } else {
            serde_json::json!("invalide")
        };
        fs::write(&path, serde_json::to_vec(&value).expect("json")).expect("write");
        commit(root);
        git(root, &["tag", "-f", "frozen/H-001"]);
        let prefix = fs::read(root.join("research/trials.csv")).expect("prefix");
        refused(cli(root, &["claim", "--hypothesis", "H-001"]), "");
        assert_eq!(
            prefix,
            fs::read(root.join("research/trials.csv")).expect("csv")
        );
    }
}
#[test]
fn tag_doit_couvrir_les_octets_courants_et_head() {
    let dir = frozen(false);
    let root = dir.path();
    let path = root.join("research/frozen/H-001.json");
    let mut bytes = fs::read(&path).expect("candidate");
    bytes.push(b'\n');
    fs::write(&path, bytes).expect("write");
    commit(root);
    refused(cli(root, &["claim", "--hypothesis", "H-001"]), "gel");
}
#[test]
fn journal_tronque_refuse_sans_reparation() {
    let dir = repo();
    let root = dir.path();
    fs::write(root.join("research/trials.csv"), b"trial_id,").expect("csv");
    refused(cli(root, &["status"]), "tronqué");
    assert_eq!(
        fs::read(root.join("research/trials.csv")).expect("csv"),
        b"trial_id,"
    );
}
#[cfg(unix)]
#[test]
fn liens_symboliques_fiche_candidat_et_parent_refuses_avant_lecture() {
    use std::os::unix::fs::symlink;
    for rel in [
        "research/H-001.md",
        "research/frozen/H-001.json",
        "research/frozen",
    ] {
        let dir = frozen(false);
        let outside = tempfile::tempdir().expect("outside");
        let path = dir.path().join(rel);
        fs::rename(&path, outside.path().join("target")).expect("move");
        symlink(outside.path().join("target"), &path).expect("symlink");
        refused(
            cli(dir.path(), &["claim", "--hypothesis", "H-001"]),
            "symbolique",
        );
    }
}
#[test]
fn deux_claims_concurrents_ne_publient_qu_un_claim() {
    let dir = frozen(false);
    let root = dir.path();
    let mut first = Command::new(env!("CARGO_BIN_EXE_tv4"))
        .current_dir(root)
        .args(["research", "claim", "--hypothesis", "H-001"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("first");
    let second = cli(root, &["claim", "--hypothesis", "H-001"]);
    let first = first.wait().expect("wait");
    assert_ne!(first.success(), second.status.success());
    let reg =
        Registry::from_csv(&fs::read(root.join("research/trials.csv")).expect("csv")).expect("reg");
    assert_eq!(reg.claims().len(), 1);
}
#[test]
fn verrou_permanent_exclut_un_second_processus_puis_se_libere() {
    let dir = repo();
    let root = dir.path();
    fs::create_dir_all(root.join("state/locks")).expect("locks");
    let path = root.join("state/locks/research.lock");
    let lock = fs::File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .expect("lock");
    lock.try_lock().expect("take");
    refused(cli(root, &["status"]), "verrou");
    lock.unlock().expect("unlock");
    success(cli(root, &["status"]));
    assert!(path.exists());
}
#[test]
fn empreinte_binaire_moteur_valide_mais_differente_est_refusee() {
    let dir = frozen(false);
    let root = dir.path();
    let path = root.join("research/frozen/H-001.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("json")).expect("parse");
    value["engine_binary_hash"] = serde_json::json!(format!("blake3:{}", "0".repeat(64)));
    fs::write(&path, serde_json::to_vec(&value).expect("json")).expect("write");
    commit(root);
    git(root, &["tag", "-f", "frozen/H-001"]);
    refused(
        cli(root, &["claim", "--hypothesis", "H-001"]),
        "empreinte du binaire",
    );
}
#[test]
fn echec_readme_ne_rembourse_pas_le_claim_deja_synchronise() {
    let dir = frozen(false);
    let root = dir.path();
    fs::create_dir(root.join("research/README.md")).expect("obstacle");
    let out = cli(root, &["claim", "--hypothesis", "H-001"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reg = Registry::from_csv(&fs::read(root.join("research/trials.csv")).expect("csv"))
        .expect("registre");
    assert_eq!(reg.claims().len(), 1);
    fs::remove_dir(root.join("research/README.md")).expect("retirer obstacle");
    success(cli(root, &["status"]));
    refused(cli(root, &["claim", "--hypothesis", "H-001"]), "doublon");
}
#[test]
fn identite_en_cle_h001_est_acceptee_et_tous_les_champs_sont_obligatoires() {
    let dir = repo();
    let root = dir.path();
    let bytes = fiche(root);
    let bytes = String::from_utf8(bytes)
        .expect("UTF8")
        .replace("H-NNN : H-001", "H-001 : hypothèse de test")
        .into_bytes();
    fs::write(root.join("research/H-001.md"), &bytes).expect("fiche");
    seed(root, &bytes);
    candidate(root, &bytes);
    commit(root);
    git(root, &["tag", "frozen/H-001"]);
    success(cli(root, &["claim", "--hypothesis", "H-001"]));
    for field in FIELDS.into_iter().skip(1) {
        let missing = String::from_utf8(bytes.clone())
            .expect("UTF8")
            .lines()
            .filter(|line| !line.starts_with(&format!("{field} :")))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.join("research/H-001.md"), missing).expect("fiche");
        commit(root);
        refused(cli(root, &["claim", "--hypothesis", "H-001"]), field);
    }
}

fn frozen_profiles(count: usize) -> tempfile::TempDir {
    let dir = frozen(false);
    let root = dir.path();
    let csv_path = root.join("research/trials.csv");
    let mut csv = fs::read(&csv_path).expect("csv");
    let registry = Registry::from_csv(&csv).expect("registre");
    let reference = registry
        .trials()
        .values()
        .next()
        .expect("essai")
        .hypothesis
        .clone();
    let hashes = ["a".repeat(64), "e".repeat(64), "f".repeat(64)];
    let date = UtcDate::new("2026-01-02T00:00:00Z").expect("date");
    let batch = registry
        .propose_admission(&Admission {
            configs: hashes[..count]
                .iter()
                .map(|hash| ConfigSpec {
                    config_hash: Digest::new(hash.clone()).expect("hash"),
                    family: "F1".into(),
                    hypothesis: reference.clone(),
                })
                .collect(),
            dataset_hash: Digest::new("b".repeat(64)).expect("dataset"),
            block: Some(Block::Dev),
            time: AdmissionTime::new(date.clone(), date.quarter()).expect("time"),
            generation: 0,
            decision_start_ms: None,
            decision_end_ms: None,
        })
        .expect("admission");
    csv.extend(encode_events(&batch.events).expect("événements"));
    fs::write(&csv_path, csv).expect("csv");
    let path = root.join("research/frozen/H-001.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("json")).expect("parse");
    value["config_hashes"] = serde_json::json!(&hashes[..count]);
    fs::write(&path, serde_json::to_vec(&value).expect("json")).expect("write");
    commit(root);
    git(root, &["tag", "-f", "frozen/H-001"]);
    dir
}

#[test]
fn candidat_gele_a_deux_profils_connus_est_refuse_sans_ecriture() {
    let dir = frozen_profiles(2);
    let root = dir.path();
    let prefix = fs::read(root.join("research/trials.csv")).expect("préfixe");
    refused(
        cli(root, &["claim", "--hypothesis", "H-001"]),
        "un ou trois",
    );
    assert_eq!(
        prefix,
        fs::read(root.join("research/trials.csv")).expect("csv inchangé")
    );
}

#[test]
fn candidat_gele_aux_trois_profils_connus_inscrit_trois_claims() {
    let dir = frozen_profiles(3);
    let root = dir.path();
    success(cli(root, &["claim", "--hypothesis", "H-001"]));
    let registry = Registry::from_csv(&fs::read(root.join("research/trials.csv")).expect("csv"))
        .expect("registre");
    assert_eq!(registry.claims().len(), 3);
    assert_eq!(registry.n(), 3);
}

fn frozen_timeline(
    registered_at: &str,
    linked_at: Option<&str>,
    frozen_at: &str,
) -> tempfile::TempDir {
    let dir = frozen(false);
    let root = dir.path();
    let csv_path = root.join("research/trials.csv");
    let current = Registry::from_csv(&fs::read(&csv_path).expect("csv")).expect("registre");
    let reference = current
        .trials()
        .values()
        .next()
        .expect("essai")
        .hypothesis
        .clone()
        .expect("fiche");
    let hash = Digest::new("a".repeat(64)).expect("hash");
    let date = UtcDate::new(registered_at).expect("date inscription");
    let batch = Registry::default()
        .propose_admission(&Admission {
            configs: vec![ConfigSpec {
                config_hash: hash.clone(),
                family: "F1".into(),
                hypothesis: linked_at.is_none().then_some(reference.clone()),
            }],
            dataset_hash: Digest::new("b".repeat(64)).expect("dataset"),
            block: linked_at.is_none().then_some(Block::Dev),
            time: AdmissionTime::new(date.clone(), date.quarter()).expect("time"),
            generation: 0,
            decision_start_ms: None,
            decision_end_ms: None,
        })
        .expect("admission");
    let mut events = batch.events;
    if let Some(linked_at) = linked_at {
        let registry = Registry::default()
            .with_events(&events)
            .expect("registre avant liaison");
        events.extend(
            registry
                .propose_link(
                    &[hash],
                    &reference,
                    "F1",
                    &UtcDate::new(linked_at).expect("date liaison"),
                )
                .expect("liaison"),
        );
    }
    let mut csv = header_csv();
    csv.extend(encode_events(&events).expect("événements"));
    fs::write(csv_path, csv).expect("csv");
    git(root, &["add", "."]);
    let amended = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "--amend", "--no-edit", "--no-gpg-sign"])
        .env("GIT_COMMITTER_DATE", frozen_at)
        .output()
        .expect("date commit gel");
    assert!(
        amended.status.success(),
        "{}",
        String::from_utf8_lossy(&amended.stderr)
    );
    git(root, &["tag", "-f", "frozen/H-001"]);
    dir
}

fn refuse_late_timeline(registered: &str, linked: Option<&str>) {
    let dir = frozen_timeline(registered, linked, "2025-12-31T23:59:59Z");
    let root = dir.path();
    let prefix = fs::read(root.join("research/trials.csv")).expect("préfixe");
    refused(cli(root, &["claim", "--hypothesis", "H-001"]), "gel");
    assert_eq!(
        prefix,
        fs::read(root.join("research/trials.csv")).expect("csv inchangé")
    );
}

#[test]
fn inscription_posterieure_au_gel_est_refusee_sans_ecriture() {
    refuse_late_timeline("2026-01-01T00:00:00Z", None);
}

#[test]
fn liaison_posterieure_au_gel_est_refusee_sans_ecriture() {
    refuse_late_timeline("2025-12-01T00:00:00Z", Some("2026-01-01T00:00:00Z"));
}

#[test]
fn inscription_et_liaison_connues_avant_ou_a_la_seconde_du_gel_restent_admissibles() {
    for (registered, linked) in [
        ("2025-12-01T00:00:00Z", None),
        ("2025-12-01T00:00:00Z", Some("2025-12-15T00:00:00Z")),
        ("2026-01-01T00:00:00Z", None),
        ("2026-01-01T00:00:00Z", Some("2026-01-01T00:00:00Z")),
    ] {
        let dir = frozen_timeline(registered, linked, "2026-01-01T00:00:00Z");
        success(cli(dir.path(), &["claim", "--hypothesis", "H-001"]));
    }
}
