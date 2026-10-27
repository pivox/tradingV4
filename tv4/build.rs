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
//!
//! S8 ajoute les variables de l'identité du binaire (`engine` de `manifest.json`) :
//!
//! - `TV4_RUSTC_VERSION` et `TV4_RUSTC_COMMIT` : lignes `release:` et `commit-hash:` de `$RUSTC -vV`
//!   (repli `unknown`) ;
//! - `TV4_TARGET` : variable `TARGET` du script de build ; `TV4_PROFILE` : variable `PROFILE` ;
//! - `TV4_TARGET_CPU` : valeur de `-C target-cpu=X` dans `CARGO_ENCODED_RUSTFLAGS` (formes `-C
//!   target-cpu=X`, `-Ctarget-cpu=X` et `-C` suivi de `target-cpu=X`), sinon la ligne de
//!   `$RUSTC --print target-cpus --target $TARGET` qui porte `This is the default target CPU`
//!   (premier mot), sinon `default`.

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

    emit_toolchain_identity();
    watch_git_files();
}

/// Variable d'environnement du script de build, ou `unknown` si elle manque (ne devrait pas arriver).
fn env_or_unknown(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unknown".to_string())
}

/// Sortie texte de `$RUSTC` avec ces arguments (`None` en cas d'échec).
fn rustc_output(args: &[&str]) -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = Command::new(rustc).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Valeur de la ligne `key: valeur` de `rustc -vV`.
fn rustc_verbose_field(verbose: &str, key: &str) -> Option<String> {
    verbose
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `target-cpu` demandé par les drapeaux de compilation (`CARGO_ENCODED_RUSTFLAGS`, séparés par
/// `\x1f`), le dernier l'emportant comme pour rustc.
fn target_cpu_from_flags(encoded: &str) -> Option<String> {
    let flags: Vec<&str> = encoded.split('\x1f').collect();
    let mut found = None;
    let mut index = 0;
    while let Some(flag) = flags.get(index) {
        if let Some(value) = flag.strip_prefix("-Ctarget-cpu=") {
            found = Some(value.to_string());
        } else if *flag == "-C"
            && let Some(value) = flags
                .get(index + 1)
                .and_then(|next| next.strip_prefix("target-cpu="))
        {
            found = Some(value.to_string());
            index += 1;
        }
        index += 1;
    }
    found.filter(|value| !value.is_empty())
}

/// CPU par défaut de la cible : ligne de `--print target-cpus` marquée `This is the default target CPU`.
fn default_target_cpu(target: &str) -> Option<String> {
    let listing = rustc_output(&["--print", "target-cpus", "--target", target])?;
    listing
        .lines()
        .find(|line| line.contains("This is the default target CPU"))
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string)
}

/// Exporte la version et le commit du compilateur, la cible, son CPU et le profil.
fn emit_toolchain_identity() {
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    let verbose = rustc_output(&["-vV"]).unwrap_or_default();
    let version = rustc_verbose_field(&verbose, "release:").unwrap_or_else(|| "unknown".into());
    let commit = rustc_verbose_field(&verbose, "commit-hash:").unwrap_or_else(|| "unknown".into());
    let target = env_or_unknown("TARGET");
    let cpu = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .ok()
        .and_then(|flags| target_cpu_from_flags(&flags))
        .or_else(|| default_target_cpu(&target))
        .unwrap_or_else(|| "default".to_string());
    println!("cargo:rustc-env=TV4_RUSTC_VERSION={version}");
    println!("cargo:rustc-env=TV4_RUSTC_COMMIT={commit}");
    println!("cargo:rustc-env=TV4_TARGET={target}");
    println!("cargo:rustc-env=TV4_TARGET_CPU={cpu}");
    println!("cargo:rustc-env=TV4_PROFILE={}", env_or_unknown("PROFILE"));
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
