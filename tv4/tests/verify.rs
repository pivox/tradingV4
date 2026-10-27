//! `tv4 verify` (S8, spécification §9) : on lance le vrai binaire sur des runs écrits par `tv4 backtest`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tv4::model::hash;

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

/// `tv4 backtest` de fumée (fixture, `sma_cross.toml`, frais OKX) avec des options en plus.
fn backtest(out: &Path, extra: &[&str]) {
    let mut args: Vec<String> = [
        "backtest",
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
        "--out",
        &out.display().to_string(),
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    args.extend(extra.iter().map(ToString::to_string));
    let sortie = tv4(&args);
    assert_eq!(
        sortie.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&sortie.stderr)
    );
}

fn verify(a: &Path, b: &Path) -> (Option<i32>, String, String) {
    let sortie = tv4(&[
        "verify".to_string(),
        a.display().to_string(),
        b.display().to_string(),
    ]);
    (
        sortie.status.code(),
        String::from_utf8_lossy(&sortie.stdout).into_owned(),
        String::from_utf8_lossy(&sortie.stderr).into_owned(),
    )
}

/// Copie récursive (à plat) d'un dossier de run.
fn copy_run(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

/// Met à jour dans le manifeste d'un run le hash annoncé d'un fichier, comme le ferait un run propre.
fn refresh_manifest_hash(run: &Path, file: &str) {
    let manifest_path = run.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let nouveau = hash::blake3_prefixed(&fs::read(run.join(file)).unwrap());
    manifest["outputs"]["files"][file] = Value::String(nouveau.clone());
    if file == "trades.csv" {
        manifest["result_hash"] = Value::String(nouveau);
    }
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

/// Deux runs identiques dans deux process : A et B.
fn deux_runs(dossier: &Path) -> (PathBuf, PathBuf) {
    let (a, b) = (dossier.join("a"), dossier.join("b"));
    backtest(&a, &[]);
    backtest(&b, &[]);
    (a, b)
}

#[test]
fn deux_runs_identiques_sortent_avec_le_code_0() {
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = deux_runs(dossier.path());
    let (code, stdout, stderr) = verify(&a, &b);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    for fichier in ["metrics.json", "trades.csv", "equity.csv"] {
        assert!(
            stdout.contains(&format!("{fichier} : identique (blake3:")),
            "{stdout}"
        );
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(a.join("manifest.json")).unwrap()).unwrap();
    let result_hash = manifest["result_hash"].as_str().unwrap();
    let derniere = stdout.lines().last().unwrap();
    assert_eq!(
        derniere,
        format!("verdict : identiques à l'octet (result_hash {result_hash})")
    );
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn un_run_est_identique_a_lui_meme_meme_si_le_manifeste_differe() {
    // Le manifeste (horloge, binaire, durées) n'est jamais comparé à l'octet : on l'altère en dehors des
    // champs lus par verify et le verdict ne bouge pas.
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = deux_runs(dossier.path());
    let chemin = b.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&chemin).unwrap()).unwrap();
    manifest["timings"]["run_ms"] = Value::from(999_999);
    manifest["started_at"] = Value::from("1999-01-01T00:00:00Z");
    manifest["run_id"] = Value::from("0".repeat(64));
    manifest["engine"]["git_sha"] = Value::from("autre-commit");
    fs::write(&chemin, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (code, stdout, _) = verify(&a, &b);
    assert_eq!(code, Some(0), "{stdout}");
    // La différence de moteur est listée comme information.
    assert!(stdout.contains("engine.git_sha"), "{stdout}");
    assert!(!stdout.contains("timings"), "{stdout}");
}

#[test]
fn un_octet_modifie_dans_equity_csv_donne_le_code_3_et_la_ligne() {
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = deux_runs(dossier.path());
    let equity = b.join("equity.csv");
    let texte = fs::read_to_string(&equity).unwrap();
    // Troisième ligne (deuxième ligne de données) : un chiffre de plus dans l'equity.
    let mut lignes: Vec<String> = texte.lines().map(str::to_string).collect();
    lignes[2] = lignes[2].replacen(',', ",9", 1);
    fs::write(&equity, lignes.join("\n") + "\n").unwrap();
    refresh_manifest_hash(&b, "equity.csv");

    let (code, stdout, stderr) = verify(&a, &b);
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    assert!(stdout.contains("equity.csv : DIFFÉRENT"), "{stdout}");
    assert!(stdout.contains("première ligne différente : 3"), "{stdout}");
    assert!(
        stdout.contains(&format!("A : {}", texte.lines().nth(2).unwrap())),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("B : {}", lignes[2])), "{stdout}");
    assert!(stdout.contains("metrics.json : identique"), "{stdout}");
    assert!(
        stdout
            .lines()
            .last()
            .unwrap()
            .starts_with("verdict : DIFFÉRENTS (1 fichier(s))"),
        "{stdout}"
    );
    assert!(stderr.contains("erreur :"), "{stderr}");
}

#[test]
fn un_fichier_modifie_sans_mettre_a_jour_le_manifeste_est_un_run_incoherent() {
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = deux_runs(dossier.path());
    let trades = b.join("trades.csv");
    let mut octets = fs::read(&trades).unwrap();
    octets.extend_from_slice(b"x\n");
    fs::write(&trades, octets).unwrap();

    let (code, stdout, stderr) = verify(&a, &b);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("run B incohérent : trades.csv ne correspond pas à son manifeste"),
        "{stderr}"
    );
    // Aucune conclusion sur un run altéré : rien sur stdout.
    assert!(stdout.is_empty(), "{stdout}");
    // Et du côté A.
    let (code, _, stderr) = verify(&b, &a);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("run A incohérent : trades.csv"), "{stderr}");
}

#[test]
fn un_fichier_absent_est_une_erreur_qui_nomme_le_dossier_et_le_fichier() {
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = deux_runs(dossier.path());
    for fichier in ["manifest.json", "metrics.json", "trades.csv", "equity.csv"] {
        let copie = dossier.path().join(format!("sans-{fichier}"));
        copy_run(&b, &copie);
        fs::remove_file(copie.join(fichier)).unwrap();
        let (code, _, stderr) = verify(&a, &copie);
        assert_eq!(code, Some(1), "{fichier} : {stderr}");
        assert!(
            stderr.contains(&format!("run B (`{}`)", copie.display())),
            "{stderr}"
        );
        assert!(stderr.contains(fichier), "{stderr}");
    }
}

#[test]
fn un_json_invalide_ou_un_schema_inconnu_sont_des_erreurs() {
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = deux_runs(dossier.path());

    let invalide = dossier.path().join("invalide");
    copy_run(&b, &invalide);
    fs::write(invalide.join("metrics.json"), b"{pas du json").unwrap();
    let (code, _, stderr) = verify(&a, &invalide);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("metrics.json") && stderr.contains("JSON"),
        "{stderr}"
    );

    let inconnu = dossier.path().join("inconnu");
    copy_run(&b, &inconnu);
    let chemin = inconnu.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&chemin).unwrap()).unwrap();
    manifest["schema"] = Value::from("tv4-manifest-s6");
    fs::write(&chemin, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (code, _, stderr) = verify(&inconnu, &a);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("run A") && stderr.contains("schéma inconnu `tv4-manifest-s6`"),
        "{stderr}"
    );
}

#[test]
fn deux_min_trades_differents_donnent_le_code_3_et_l_avertissement_de_config() {
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = (dossier.path().join("a"), dossier.path().join("b"));
    backtest(&a, &[]);
    backtest(&b, &["--min-trades", "1"]);
    let (code, stdout, _) = verify(&a, &b);
    assert_eq!(code, Some(3), "{stdout}");
    assert!(stdout.contains("metrics.json : DIFFÉRENT"), "{stdout}");
    assert!(stdout.contains("min_trades"), "{stdout}");
    assert!(stdout.contains("config_hash"), "{stdout}");
    assert!(
        stdout.contains("attention : configurations (ou données) différentes"),
        "{stdout}"
    );
    // Les trades ne changent pas.
    assert!(stdout.contains("trades.csv : identique"), "{stdout}");
}

#[test]
fn l_usage_est_verifie_par_clap() {
    let sortie = tv4(&["verify".to_string()]);
    assert_eq!(sortie.status.code(), Some(2));
}
