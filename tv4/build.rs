//! Script de build : expose le git sha du commit compilé via `env!("TV4_GIT_SHA")`.
//!
//! Le sha est écrit dans le manifeste de chaque run (docs/03-architecture.md §5.2) : un résultat de
//! backtest n'est valide qu'avec le commit du moteur qui l'a produit.
//!
//! Le sha est celui de `git describe --always --dirty --abbrev=12` : 12 caractères hexadécimaux,
//! suivis de `-dirty` quand des fichiers suivis par git ont des modifications non commitées (un
//! binaire compilé ainsi ne correspond à aucun commit). Les fichiers non suivis ne comptent pas.
//!
//! Repli : la valeur `unknown` quand git est absent ou que le dossier n'est pas un dépôt (archive,
//! image Docker sans `.git`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Le suffixe `-dirty` dépend du contenu des fichiers : on relance le script quand le code ou les
    // manifestes changent (cargo parcourt le dossier `src` pour comparer les dates de modification).
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../Cargo.toml");
    println!("cargo:rerun-if-changed=../Cargo.lock");

    let sha = git_output(&["describe", "--always", "--dirty", "--abbrev=12"])
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TV4_GIT_SHA={sha}");

    watch_git_files();
}

/// Lance `git` avec ces arguments et rend la première ligne de la sortie, ou `None` en cas d'échec.
fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Demande à cargo de relancer ce script quand le commit change.
///
/// Dans un worktree git, `.git` est un fichier (pas un dossier) : on interroge donc git pour
/// connaître le vrai dossier. `HEAD` est propre au worktree ; les branches (`refs/`) et
/// `packed-refs` vivent dans le dossier commun.
fn watch_git_files() {
    if let Some(git_dir) = git_output(&["rev-parse", "--git-dir"]) {
        let git_dir = absolute(&git_dir);
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    }
    if let Some(common_dir) = git_output(&["rev-parse", "--git-common-dir"]) {
        let common_dir = absolute(&common_dir);
        // Un chemin inexistant forcerait cargo à relancer ce script à chaque build : on ne
        // déclare que ce qui existe (`packed-refs` n'existe pas dans un dépôt neuf).
        for name in ["packed-refs", "refs"] {
            let path = common_dir.join(name);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

/// `git rev-parse` peut rendre un chemin relatif au dossier courant : on le rend absolu.
fn absolute(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    }
}
