//! Tests d'intégration du binaire `tv4` : on lance le vrai exécutable.

use std::process::{Command, Output};

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
    let cas: [(&[&str], &str); 6] = [
        (&["ingest", "kraken"], "S5"),
        (&["ingest", "okx"], "S7"),
        (&["dataset", "build"], "S4"),
        (&["backtest"], "S3"),
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
