//! Tests d'intégration du binaire `tv4` : on lance le vrai exécutable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tv4::model::hash;

fn tv4(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(args)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

#[test]
fn version_contient_le_git_sha() {
    let sortie = tv4(&["--version"]);
    assert!(sortie.status.success());
    let texte = String::from_utf8_lossy(&sortie.stdout);
    let sha = env!("TV4_GIT_SHA");
    assert!(!sha.is_empty());
    assert!(texte.contains(sha), "sha {sha} absent de « {texte} »");
    assert!(texte.contains(env!("CARGO_PKG_VERSION")), "{texte}");
}

#[test]
fn le_sha_vient_de_git_quand_git_est_disponible() {
    // Dans le dépôt (CI ou worktree), le sha n'est pas le repli « unknown ».
    let git_ok = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .is_ok_and(|sortie| sortie.status.success());
    if git_ok {
        assert_ne!(env!("TV4_GIT_SHA"), "unknown");
    }
}

#[test]
fn les_sous_commandes_vides_sortent_en_erreur_avec_leur_etape() {
    let cas: [(&[&str], &str); 5] = [
        (&["ingest", "kraken"], "S5"),
        (&["ingest", "okx"], "S7"),
        (&["dataset", "build"], "S4"),
        (&["verify"], "S8"),
        (&["bench"], "S8"),
    ];
    for (args, etape) in cas {
        let sortie = tv4(args);
        // Code 1 : erreur applicative (clap utilise 2 pour une erreur d'usage).
        assert_eq!(sortie.status.code(), Some(1), "{args:?}");
        assert!(
            sortie.stdout.is_empty(),
            "{args:?} ne doit rien écrire sur stdout"
        );
        let erreur = String::from_utf8_lossy(&sortie.stderr);
        assert!(erreur.contains("erreur :"), "{args:?} : {erreur}");
        assert!(
            erreur.contains("n'est pas encore implémentée"),
            "{args:?} : {erreur}"
        );
        assert!(erreur.contains(etape), "{args:?} : {erreur}");
    }
}

#[test]
fn une_sous_commande_inconnue_est_une_erreur_d_usage() {
    let sortie = tv4(&["grid"]);
    assert_eq!(sortie.status.code(), Some(2));
}

#[test]
fn sans_argument_l_aide_est_affichee_avec_un_code_non_nul() {
    let sortie = tv4(&[]);
    assert!(!sortie.status.success());
}

// --- tv4 backtest (S3) ---

fn repo(relative: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
        .display()
        .to_string()
}

/// Arguments de `tv4 backtest` avec les fichiers du dépôt, sortie dans `out`.
fn backtest_args(params: &str, out: &Path) -> Vec<String> {
    vec![
        "backtest".to_string(),
        "--fixture".to_string(),
        repo("fixtures/kraken_xbtusd_1m.csv"),
        "--strategy".to_string(),
        "sma_cross".to_string(),
        "--params".to_string(),
        repo(params),
        "--fees-file".to_string(),
        repo("config/fees.toml"),
        "--out".to_string(),
        out.display().to_string(),
    ]
}

fn tv4_owned(args: &[String]) -> Output {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    tv4(&refs)
}

const PARAMS_1M: &str = "config/strategies/sma_cross_1m.toml";

#[test]
fn backtest_ecrit_metrics_json() {
    let dossier = tempfile::tempdir().unwrap();
    let sortie_dir = dossier.path().join("run");
    let sortie = tv4_owned(&backtest_args(PARAMS_1M, &sortie_dir));
    assert_eq!(
        sortie.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&sortie.stderr)
    );
    let texte = std::fs::read_to_string(sortie_dir.join("metrics.json")).unwrap();
    assert!(!sortie_dir.join("metrics.json.tmp").exists());
    let valeur: serde_json::Value = serde_json::from_str(&texte).unwrap();
    assert_eq!(valeur["n_trades"], 23);
    assert_eq!(valeur["schema"], "tv4-metrics-s3");
    // Contenu canonique : clés triées, une seule fin de ligne.
    assert_eq!(
        texte,
        format!("{}\n", hash::canonical_json(&valeur).unwrap())
    );
    let stdout = String::from_utf8_lossy(&sortie.stdout);
    assert!(stdout.contains("23 trades"), "{stdout}");
}

#[test]
fn deux_process_meme_metrics() {
    let dossier = tempfile::tempdir().unwrap();
    let mut contenus = Vec::new();
    for nom in ["a", "b"] {
        let sortie_dir = dossier.path().join(nom);
        let sortie = tv4_owned(&backtest_args(PARAMS_1M, &sortie_dir));
        assert_eq!(sortie.status.code(), Some(0));
        contenus.push(std::fs::read(sortie_dir.join("metrics.json")).unwrap());
    }
    assert_eq!(contenus[0], contenus[1]);
}

#[test]
fn backtest_refuse_le_15m() {
    let dossier = tempfile::tempdir().unwrap();
    let sortie_dir = dossier.path().join("run");
    let sortie = tv4_owned(&backtest_args(
        "config/strategies/sma_cross.toml",
        &sortie_dir,
    ));
    assert_eq!(sortie.status.code(), Some(1));
    let erreur = String::from_utf8_lossy(&sortie.stderr);
    assert!(erreur.contains("S5"), "{erreur}");
    assert!(erreur.contains("sma_cross_1m.toml"), "{erreur}");
    assert!(!sortie_dir.join("metrics.json").exists());
}

#[test]
fn backtest_erreurs_d_usage() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    let avec = |extra: &[&str]| {
        let mut args = backtest_args(PARAMS_1M, &out);
        args.extend(extra.iter().map(|s| (*s).to_string()));
        args
    };
    // Code 2 : erreur d'usage clap.
    let sans_fixture: Vec<String> = {
        let mut args = backtest_args(PARAMS_1M, &out);
        let position = args.iter().position(|a| a == "--fixture").unwrap();
        args.drain(position..position + 2);
        args
    };
    assert_eq!(tv4_owned(&sans_fixture).status.code(), Some(2));
    for extra in [
        ["--slippage-bps", "nan"],
        ["--slippage-bps", "-1"],
        ["--fees", "binance"],
    ] {
        let sortie = tv4_owned(&avec(&extra));
        assert_eq!(sortie.status.code(), Some(2), "{extra:?}");
    }
    // Code 1 : erreur applicative.
    for extra in [["--capital", "0"], ["--fees", "kraken"]] {
        let sortie = tv4_owned(&avec(&extra));
        assert_eq!(sortie.status.code(), Some(1), "{extra:?}");
        assert!(!out.join("metrics.json").exists(), "{extra:?}");
    }
}

#[test]
fn backtest_fichier_absent() {
    let dossier = tempfile::tempdir().unwrap();
    let mut args = backtest_args(PARAMS_1M, &dossier.path().join("run"));
    let position = args.iter().position(|a| a == "--fixture").unwrap();
    args[position + 1] = "/n/existe/pas.csv".to_string();
    let sortie = tv4_owned(&args);
    assert_eq!(sortie.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&sortie.stderr).contains("/n/existe/pas.csv"));
}
