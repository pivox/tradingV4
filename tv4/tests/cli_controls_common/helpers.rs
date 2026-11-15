#![allow(dead_code)]
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
pub fn run(root: &Path, args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}
pub fn ok(root: &Path, args: &[String]) -> Output {
    let o = run(root, args);
    assert!(
        o.status.success(),
        "{args:?}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}
pub fn git(root: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
pub fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
pub fn file(root: &Path, name: &str) -> Value {
    json!({"file":name,"hash":format!("blake3:{}",blake3::hash(&fs::read(root.join(name)).unwrap()).to_hex())})
}

pub fn repo(relative: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}
pub fn future() -> i64 {
    tv4::store::time::days_from_civil(2030, 1, 1) * 86_400_000
}
