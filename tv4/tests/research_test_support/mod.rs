//! Les tests legacy du vrai binaire n'écrivent jamais dans le registre du checkout.
use std::ffi::OsStr;
use std::process::Command;

/// Le garde doit rester vivant jusqu'au retour du sous-processus. Les scénarios
/// de recherche qui fournissent leur propre checkout gardent leurs vrais compteurs.
pub fn isolate<T: AsRef<OsStr>>(command: &mut Command, args: &[T]) -> Option<tempfile::TempDir> {
    if !matches!(args.first().map(AsRef::as_ref), Some(first) if first == OsStr::new("backtest") || first == OsStr::new("bench"))
        || args.iter().any(|arg| {
            let arg = arg.as_ref();
            arg == OsStr::new("--research-root")
                || arg
                    .to_str()
                    .is_some_and(|s| s.starts_with("--research-root="))
        })
    {
        return None;
    }
    let root = tempfile::tempdir().expect("registre de test isolé");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(root.path())
        .output()
        .expect("git init du registre de test");
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    command.arg("--research-root").arg(root.path());
    Some(root)
}
