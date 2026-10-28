//! `tv4 backtest --symbols ...` et `tv4 verify` en mode portefeuille (S9, lot F, docs/specs/s9.md §14 et
//! §15.1) : on lance le vrai binaire sur un dataset multi-séries synthétique écrit par l'API du store.
//!
//! Marché inventé (`common::synthetic_candles`), instruments du fichier de test
//! `tests/fixtures/okx_instruments_spot.json` : aucune donnée réelle, aucun secret.

mod common;
mod report_s9_common;

use std::fs;
use std::path::{Path, PathBuf};

use report_s9_common::{build_dataset, candles, repo, strings, tv4};
use serde_json::Value;
use tv4::model::hash;

const MINUTES: usize = 6_000;
/// Fichier d'instruments OKX de test (BTC-USDC et ETH-USDC `live`, ADA-USDC `suspend`).
fn okx_file() -> String {
    format!(
        "{}/tests/fixtures/okx_instruments_spot.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Dossier de travail : racine du store avec un dataset `duo` (BTC-USD et ETH-USD) et un `params.toml`.
struct Work {
    _dir: tempfile::TempDir,
    root: PathBuf,
    params: PathBuf,
    instruments: PathBuf,
}

fn work() -> Work {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    build_dataset(
        &root,
        "duo",
        &[
            (
                "BTC-USD",
                candles(MINUTES, 11, 0.0, 2000.0, common::synthetic_candles),
            ),
            (
                "ETH-USD",
                candles(MINUTES, 22, 0.001, 150.0, common::synthetic_candles),
            ),
        ],
        MINUTES as i64 + 100,
    );
    let params = root.join("params.toml");
    fs::write(
        &params,
        "tf = \"15m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0\n",
    )
    .unwrap();
    let instruments = root.join("instruments.toml");
    fs::write(
        &instruments,
        "[kraken.BTC-USD]\ntick_size = 0.1\nlot_size = 1e-8\nmin_qty = 0.0\nmin_notional = 0.0\n\n\
         [kraken.ETH-USD]\ntick_size = 0.01\nlot_size = 1e-6\nmin_qty = 0.0\nmin_notional = 0.0\n",
    )
    .unwrap();
    Work {
        _dir: dir,
        root,
        params,
        instruments,
    }
}

/// Arguments de `tv4 backtest --symbols BTC-USD,ETH-USD` écrivant dans `out`, suivis de `extra`.
fn backtest_args(w: &Work, out: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = strings(&["backtest", "--dataset", "duo", "--strategy", "sma_cross"]);
    args.extend([
        "--root".to_string(),
        w.root.display().to_string(),
        "--params".to_string(),
        w.params.display().to_string(),
        "--fees-file".to_string(),
        repo("config/fees.toml").display().to_string(),
        "--instruments-file".to_string(),
        w.instruments.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ]);
    args.extend(strings(extra));
    args
}

fn portfolio(w: &Work, out: &Path, extra: &[&str]) -> (Option<i32>, String, String) {
    let mut all = vec!["--symbols", "BTC-USD,ETH-USD"];
    all.extend_from_slice(extra);
    let sortie = tv4(&backtest_args(w, out, &all));
    (
        sortie.status.code(),
        String::from_utf8_lossy(&sortie.stdout).into_owned(),
        String::from_utf8_lossy(&sortie.stderr).into_owned(),
    )
}

fn ok_portfolio(w: &Work, out: &Path, extra: &[&str]) {
    let (code, stdout, stderr) = portfolio(w, out, extra);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
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

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn un_run_portefeuille_ecrit_quatre_fichiers_de_schema_s9() {
    let w = work();
    let out = w.root.join("run");
    let (code, stdout, stderr) = portfolio(
        &w,
        &out,
        &["--max-positions", "1", "--instruments-okx", &okx_file()],
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    for file in ["metrics.json", "trades.csv", "equity.csv", "manifest.json"] {
        assert!(out.join(file).is_file(), "{file} absent");
    }
    let metrics = json(&out.join("metrics.json"));
    let manifest = json(&out.join("manifest.json"));
    assert_eq!(metrics["schema"], "tv4-metrics-s9");
    assert_eq!(manifest["schema"], "tv4-manifest-s9");
    assert_eq!(
        metrics["data"]["symbols"],
        serde_json::json!(["BTC-USD", "ETH-USD"])
    );
    assert_eq!(metrics["portfolio"]["legs"], 2);
    assert_eq!(metrics["portfolio"]["max_open_positions"], 1);
    assert_eq!(metrics["instruments"]["source"], "okx");
    assert_eq!(metrics["executability"]["venue_matches_execution"], false);
    // Le capital est écrit dans les deux fichiers, et le hash du manifeste est celui des métriques.
    assert_eq!(metrics["config"]["initial_cash"], 300.0);
    assert_eq!(manifest["initial_cash"], 300.0);
    assert_eq!(manifest["config_hash"], metrics["config_hash"]);
    assert_eq!(manifest["dataset_hash"], metrics["dataset_hash"]);
    assert_eq!(
        manifest["instruments_hash"],
        metrics["instruments"]["instruments_hash"]
    );
    // Le manifeste annonce les hashs des trois fichiers.
    for file in ["metrics.json", "trades.csv", "equity.csv"] {
        let announced = manifest["outputs"]["files"][file].as_str().unwrap();
        assert_eq!(
            announced,
            hash::blake3_prefixed(&fs::read(out.join(file)).unwrap()),
            "{file}"
        );
    }
    // Chaque ligne de trades.csv a un symbole de l'univers.
    let trades = fs::read_to_string(out.join("trades.csv")).unwrap();
    let mut reader = csv::Reader::from_reader(trades.as_bytes());
    let header = reader.headers().unwrap().clone();
    let symbol_column = header.iter().position(|h| h == "symbol").unwrap();
    let mut rows = 0_u64;
    for row in reader.records() {
        let row = row.unwrap();
        assert!(["BTC-USD", "ETH-USD"].contains(&&row[symbol_column]));
        rows += 1;
    }
    assert_eq!(rows, metrics["n_trades"].as_u64().unwrap());
    assert!(rows > 0, "le run de test doit trader");
    // Le résumé imprimé est celui d'un run portefeuille.
    assert!(
        format!("{stdout}{stderr}").contains("sur 2 paires"),
        "{stdout}{stderr}"
    );
}

#[test]
fn deux_runs_portefeuille_identiques_passent_verify() {
    let w = work();
    let (a, b) = (w.root.join("a"), w.root.join("b"));
    ok_portfolio(
        &w,
        &a,
        &["--max-positions", "1", "--instruments-okx", &okx_file()],
    );
    ok_portfolio(
        &w,
        &b,
        &["--max-positions", "1", "--instruments-okx", &okx_file()],
    );
    for file in ["metrics.json", "trades.csv", "equity.csv"] {
        assert_eq!(
            fs::read(a.join(file)).unwrap(),
            fs::read(b.join(file)).unwrap(),
            "{file}"
        );
    }
    let (code, stdout, stderr) = verify(&a, &b);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("verdict : identiques à l'octet"),
        "{stdout}"
    );
}

#[test]
fn verify_refuse_deux_runs_de_capitaux_differents() {
    let w = work();
    let (a, b) = (w.root.join("a"), w.root.join("b"));
    ok_portfolio(&w, &a, &["--capital", "300"]);
    ok_portfolio(&w, &b, &["--capital", "3000"]);
    // Le capital est hors du config_hash : même hash, mais jamais déclarés identiques.
    let (ma, mb) = (json(&a.join("metrics.json")), json(&b.join("metrics.json")));
    assert_eq!(ma["config_hash"], mb["config_hash"]);
    assert_eq!(ma["portfolio"]["initial_cash"], 300.0);
    assert_eq!(mb["portfolio"]["initial_cash"], 3000.0);
    let (ra, rb) = (
        json(&a.join("manifest.json")),
        json(&b.join("manifest.json")),
    );
    assert_eq!(ra["config_hash"], rb["config_hash"]);
    assert_ne!(ra["run_id"], rb["run_id"], "le run_id porte le capital");
    let (code, stdout, stderr) = verify(&a, &b);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("capitaux initiaux différents"), "{stderr}");
    assert!(!stdout.contains("verdict : identiques"), "{stdout}");
}

#[test]
fn verify_refuse_un_run_s8_contre_un_run_s9() {
    let w = work();
    let (s9, s8) = (w.root.join("s9"), w.root.join("s8"));
    ok_portfolio(&w, &s9, &[]);
    let sortie = tv4(&strings(&[
        "backtest",
        "--fixture",
        &repo("fixtures/kraken_xbtusd_1m.csv").display().to_string(),
        "--strategy",
        "sma_cross",
        "--params",
        &repo("config/strategies/sma_cross.toml")
            .display()
            .to_string(),
        "--fees-file",
        &repo("config/fees.toml").display().to_string(),
        "--instruments-file",
        &repo("config/instruments.toml").display().to_string(),
        "--out",
        &s8.display().to_string(),
    ]));
    assert_eq!(
        sortie.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&sortie.stderr)
    );
    let (code, _, stderr) = verify(&s9, &s8);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("schémas différents"), "{stderr}");
    let (code, _, stderr) = verify(&s8, &s9);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("schémas différents"), "{stderr}");
}

/// Remplace le texte `from` par `to` dans un fichier du run, puis met à jour le hash annoncé.
fn tamper(run: &Path, file: &str, from: &str, to: &str) {
    let path = run.join(file);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "`{from}` absent de {file}");
    fs::write(&path, text.replacen(from, to, 1)).unwrap();
    let manifest_path = run.join("manifest.json");
    let mut manifest = json(&manifest_path);
    let new_hash = hash::blake3_prefixed(&fs::read(&path).unwrap());
    manifest["outputs"]["files"][file] = Value::String(new_hash.clone());
    if file == "trades.csv" {
        manifest["result_hash"] = Value::String(new_hash);
    }
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[test]
fn verify_detecte_un_run_portefeuille_incoherent_avec_lui_meme() {
    let w = work();
    let (a, b) = (w.root.join("a"), w.root.join("b"));
    ok_portfolio(&w, &a, &[]);
    ok_portfolio(&w, &b, &[]);
    // Un symbole de trades.csv hors de l'univers.
    let c = w.root.join("c");
    fs::create_dir_all(&c).unwrap();
    for entry in fs::read_dir(&a).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), c.join(entry.file_name())).unwrap();
    }
    tamper(&c, "trades.csv", "BTC-USD", "XRP-USD");
    let (code, _, stderr) = verify(&c, &b);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("incohérent"), "{stderr}");
    assert!(stderr.contains("XRP-USD"), "{stderr}");
    // Un compteur de funnel_by_pair qui ne somme plus au funnel.
    let d = w.root.join("d");
    fs::create_dir_all(&d).unwrap();
    for entry in fs::read_dir(&a).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), d.join(entry.file_name())).unwrap();
    }
    tamper(&d, "metrics.json", "\"evaluations\":", "\"evaluations\":1");
    let (code, _, stderr) = verify(&d, &b);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("incohérent"), "{stderr}");
}

/// Copie le run `from` dans `to`.
fn copy_run(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

/// Modifie `metrics.json` d'un run par `edit`, puis met à jour le hash annoncé dans le manifeste.
fn tamper_metrics(run: &Path, edit: impl FnOnce(&mut Value)) {
    let path = run.join("metrics.json");
    let mut metrics = json(&path);
    edit(&mut metrics);
    fs::write(&path, serde_json::to_vec(&metrics).unwrap()).unwrap();
    let manifest_path = run.join("manifest.json");
    let mut manifest = json(&manifest_path);
    manifest["outputs"]["files"]["metrics.json"] =
        Value::String(hash::blake3_prefixed(&fs::read(&path).unwrap()));
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

/// Déplace une unité de `funnel_by_pair[from].key` vers `funnel_by_pair[to].key` (les sommes ne bougent
/// pas), ou de `funnel_by_pair[from].rejected.key` si `sub` est `Some`.
fn move_one(metrics: &mut Value, from: usize, to: usize, key: &str, sub: Option<&str>) {
    for (index, delta) in [(from, -1i64), (to, 1i64)] {
        let pair = &mut metrics["funnel_by_pair"][index];
        let cell = match sub {
            Some(sub) => &mut pair[sub][key],
            None => &mut pair[key],
        };
        *cell = Value::from(cell.as_i64().unwrap() + delta);
    }
}

/// Deux copies du même run altérées de la même façon ne diffèrent pas : seul le contrôle de cohérence
/// d'un run peut les refuser.
fn assert_incoherent_when_tampered_alike(base: &Path, edit: impl Fn(&mut Value), expected: &str) {
    let root = base.parent().unwrap();
    let (c, d) = (root.join("c"), root.join("d"));
    copy_run(base, &c);
    copy_run(base, &d);
    tamper_metrics(&c, &edit);
    tamper_metrics(&d, &edit);
    assert_eq!(
        fs::read(c.join("metrics.json")).unwrap(),
        fs::read(d.join("metrics.json")).unwrap()
    );
    let (code, _, stderr) = verify(&c, &d);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("incohérent"), "{stderr}");
    assert!(stderr.contains(expected), "{stderr}");
}

#[test]
fn verify_accepte_un_run_portefeuille_coherent_avec_des_refus_par_paire() {
    let w = work();
    let (a, b) = (w.root.join("a"), w.root.join("b"));
    ok_portfolio(&w, &a, &["--max-positions", "1"]);
    ok_portfolio(&w, &b, &["--max-positions", "1"]);
    let metrics = json(&a.join("metrics.json"));
    let refused: u64 = metrics["funnel_by_pair"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["rejected"]["max_positions"].as_u64().unwrap())
        .sum();
    assert!(refused > 0, "le run doit avoir des refus max_positions");
    let (code, stdout, stderr) = verify(&a, &b);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("identiques à l'octet"), "{stdout}");
}

#[test]
fn verify_refuse_un_ordre_deplace_entre_paires() {
    let w = work();
    let a = w.root.join("a");
    ok_portfolio(&w, &a, &["--max-positions", "1"]);
    let metrics = json(&a.join("metrics.json"));
    let orders = |i: usize| {
        metrics["funnel_by_pair"][i]["entry_orders"]
            .as_u64()
            .unwrap()
    };
    // La paire qui cède un ordre en a au moins un.
    let (from, to) = if orders(0) > 0 { (0, 1) } else { (1, 0) };
    assert!(orders(from) > 0, "aucune paire n'a d'ordre d'entrée");
    assert_incoherent_when_tampered_alike(
        &a,
        |m| move_one(m, from, to, "entry_orders", None),
        "signaux d'entrée",
    );
}

#[test]
fn verify_refuse_des_refus_deplaces_entre_paires() {
    let w = work();
    let a = w.root.join("a");
    ok_portfolio(&w, &a, &["--max-positions", "1"]);
    let metrics = json(&a.join("metrics.json"));
    let refused = |i: usize| {
        metrics["funnel_by_pair"][i]["rejected"]["max_positions"]
            .as_u64()
            .unwrap()
    };
    let (from, to) = if refused(0) > 0 { (0, 1) } else { (1, 0) };
    assert!(refused(from) > 0, "aucune paire n'a de refus max_positions");
    // Les sommes et `fills + rejected <= signals` restent vrais ; seule l'identité 1 par paire casse.
    assert_incoherent_when_tampered_alike(
        &a,
        |m| move_one(m, from, to, "max_positions", Some("rejected")),
        "signaux d'entrée",
    );
}

#[test]
fn les_options_du_portefeuille_sont_refusees_sans_symbols() {
    let w = work();
    let out = w.root.join("run");
    for extra in [
        &["--max-positions", "2"][..],
        &["--sizing", "compounding"],
        &["--instruments-okx", "x.json"],
        &["--okx-quote", "USDC"],
        &["--regime-symbol", "BTC-USD"],
        &["--quote-gap-days", "3"],
        &["--min-order-bump-risk", "0.015"],
    ] {
        let sortie = tv4(&backtest_args(&w, &out, extra));
        let stderr = String::from_utf8_lossy(&sortie.stderr);
        assert_eq!(sortie.status.code(), Some(1), "{extra:?} : {stderr}");
        assert!(
            stderr.contains("n'a de sens qu'avec --symbols"),
            "{extra:?} : {stderr}"
        );
        assert!(stderr.contains(extra[0]), "{extra:?} : {stderr}");
    }
    assert!(!out.exists(), "aucune sortie ne doit être écrite");
}

#[test]
fn symbols_avec_fixture_et_valeurs_invalides_sont_refuses() {
    let w = work();
    let out = w.root.join("run");
    // --fixture à la place de --dataset.
    let sortie = tv4(&strings(&[
        "backtest",
        "--fixture",
        &repo("fixtures/kraken_xbtusd_1m.csv").display().to_string(),
        "--symbols",
        "BTC-USD",
        "--strategy",
        "sma_cross",
        "--params",
        &w.params.display().to_string(),
        "--out",
        &out.display().to_string(),
    ]));
    let stderr = String::from_utf8_lossy(&sortie.stderr);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("utiliser --dataset, pas --fixture"),
        "{stderr}"
    );

    for (extra, needle) in [
        (&["--capital", "0"][..], "--capital"),
        (&["--capital", "-5"], "--capital"),
        (
            &["--min-order-bump-risk", "0.015"],
            "réservé à --sizing compounding",
        ),
        (
            &["--sizing", "compounding", "--min-order-bump-risk", "2"],
            "]0, 1]",
        ),
        (&["--symbols", "BTC-USD,,ETH-USD"], "sans élément vide"),
    ] {
        // `--symbols` est donné deux fois pour le dernier cas : on part de `backtest_args` sans lui.
        let mut args = backtest_args(&w, &out, extra);
        if !extra.contains(&"--symbols") {
            args.extend(strings(&["--symbols", "BTC-USD,ETH-USD"]));
        }
        let sortie = tv4(&args);
        let stderr = String::from_utf8_lossy(&sortie.stderr);
        assert!(
            matches!(sortie.status.code(), Some(1 | 2)),
            "{extra:?} : {stderr}"
        );
        assert!(stderr.contains(needle), "{extra:?} : {stderr}");
    }
    assert!(!out.exists());
}

#[test]
fn un_fichier_d_instruments_okx_altere_est_refuse() {
    let w = work();
    let out = w.root.join("run");
    // Fichier d'instruments OKX dont une paire est renommée : son hash est faux (fichier modifié), le run
    // est refusé avant toute écriture.
    let altered = w.root.join("okx.json");
    let text = fs::read_to_string(okx_file()).unwrap();
    fs::write(&altered, text.replace("ETH-USDC", "ETH-USDX")).unwrap();
    let (code, _, stderr) = portfolio(
        &w,
        &out,
        &["--instruments-okx", &altered.display().to_string()],
    );
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        !out.exists(),
        "aucune sortie sur un fichier altéré : {stderr}"
    );
}
