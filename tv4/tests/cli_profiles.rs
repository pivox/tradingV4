//! `tv4 backtest --symbols ... --costs ...` (S10, lot F, docs/specs/s10.md §14.1 et §16) : on lance le vrai
//! binaire sur un dataset multi-séries synthétique écrit par l'API du store, et on vérifie la disposition
//! `DIR/baseline`, `DIR/adverse`, `DIR/maker` puis `DIR/profiles.json`, les refus d'usage (code 2), la
//! graine, les mesures de coûts, et l'absence de `profiles.json` quand un profil échoue.
//!
//! Marché inventé (`common::synthetic_candles`) ; le fichier de coûts est celui du dépôt
//! (`config/costs.toml`). Le fichier de mesures de l'un des tests est écrit ici avec des valeurs
//! synthétiques de plomberie : ce ne sont pas des mesures de marché.

mod common;
mod report_s9_common;

use std::fs;
use std::path::{Path, PathBuf};

use report_s9_common::{build_dataset, candles, repo, strings, tv4};
use serde_json::Value;

const MINUTES: usize = 6_000;
const PROFILES: [&str; 3] = ["baseline", "adverse", "maker"];
const RUN_FILES: [&str; 4] = ["metrics.json", "trades.csv", "equity.csv", "manifest.json"];

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

fn costs_file() -> String {
    repo("tv4/tests/fixtures/costs_c1.toml")
        .display()
        .to_string()
}

/// Arguments de `tv4 backtest --dataset duo`, sortie dans `out` si elle est donnée, suivis de `extra`.
fn backtest_args(w: &Work, out: Option<&Path>, extra: &[&str]) -> Vec<String> {
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
    ]);
    if let Some(out) = out {
        args.extend(["--out".to_string(), out.display().to_string()]);
    }
    args.extend(strings(extra));
    args
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn launch(args: &[String]) -> Outcome {
    let sortie = tv4(args);
    Outcome {
        code: sortie.status.code(),
        stdout: String::from_utf8_lossy(&sortie.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&sortie.stderr).into_owned(),
    }
}

/// `tv4 backtest --symbols BTC-USD,ETH-USD --costs <copie figée de config/costs.toml>` suivi de `extra`.
fn run_profiles(w: &Work, out: &Path, extra: &[&str]) -> Outcome {
    let costs = costs_file();
    let mut all = vec!["--symbols", "BTC-USD,ETH-USD", "--costs", &costs];
    all.extend_from_slice(extra);
    launch(&backtest_args(w, Some(out), &all))
}

fn ok_profiles(w: &Work, out: &Path, extra: &[&str]) -> Outcome {
    let outcome = run_profiles(w, out, extra);
    assert_eq!(
        outcome.code,
        Some(0),
        "{}{}",
        outcome.stdout,
        outcome.stderr
    );
    outcome
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Écarts du simulateur (`sim_deviations`) d'un `metrics.json`.
fn deviations(metrics: &Value) -> Vec<String> {
    metrics["sim_deviations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap().to_string())
        .collect()
}

fn verify(a: &Path, b: &Path) -> Outcome {
    launch(&[
        "verify".to_string(),
        a.display().to_string(),
        b.display().to_string(),
    ])
}

#[test]
fn costs_sans_symbols_est_refuse_en_code_2_sans_rien_ecrire() {
    let w = work();
    let out = w.root.join("sortie");
    let costs = costs_file();
    let outcome = launch(&backtest_args(&w, Some(&out), &["--costs", &costs]));
    assert_eq!(
        outcome.code,
        Some(2),
        "{}{}",
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("--costs") && outcome.stderr.contains("--symbols"),
        "{}",
        outcome.stderr
    );
    assert!(!out.exists(), "aucun dossier ne doit être créé");
}

#[test]
fn slippage_scalaire_avec_costs_est_refuse_en_code_2() {
    let w = work();
    let out = w.root.join("sortie");
    // Même une valeur nulle ou égale au défaut : l'option est « donnée ».
    for slippage in ["0", "5", "1.5"] {
        let outcome = run_profiles(&w, &out, &["--slippage-bps", slippage]);
        assert_eq!(
            outcome.code,
            Some(2),
            "{slippage} : {}{}",
            outcome.stdout,
            outcome.stderr
        );
        assert!(
            outcome.stderr.contains("--slippage-bps"),
            "{}",
            outcome.stderr
        );
        assert!(!out.exists(), "{slippage}");
    }
}

#[test]
fn les_options_de_cout_sans_costs_et_une_graine_hors_plage_sont_refusees() {
    let w = work();
    let out = w.root.join("sortie");
    let base = ["--symbols", "BTC-USD,ETH-USD"];
    let costs = costs_file();
    for extra in [
        vec!["--cost-seed", "3"],
        vec!["--cost-measures", "m.json"],
        vec!["--costs", &costs, "--cost-seed", "9007199254740992"],
        vec!["--costs", &costs, "--cost-seed", "-1"],
    ] {
        let mut all = base.to_vec();
        all.extend(extra.iter().copied());
        let outcome = launch(&backtest_args(&w, Some(&out), &all));
        assert_eq!(
            outcome.code,
            Some(2),
            "{extra:?} : {}{}",
            outcome.stdout,
            outcome.stderr
        );
        assert!(!out.exists(), "{extra:?}");
    }
}

#[test]
fn un_run_s10_ecrit_trois_runs_puis_profiles_json() {
    let w = work();
    let out = w.root.join("sortie");
    let outcome = ok_profiles(&w, &out, &[]);
    for name in PROFILES {
        for file in RUN_FILES {
            assert!(out.join(name).join(file).is_file(), "{name}/{file} absent");
        }
        assert!(
            outcome.stdout.contains(&format!("profil {name} :")),
            "{}",
            outcome.stdout
        );
    }
    assert!(out.join("profiles.json").is_file());
    assert!(outcome.stdout.contains("étiquette :"), "{}", outcome.stdout);

    let profiles = json(&out.join("profiles.json"));
    assert_eq!(profiles["schema"], "tv4-profiles-v1");
    assert_eq!(profiles["fee_grid"], "okx_regular");
    assert_eq!(profiles["seed"], 1);
    let rows = profiles["profiles"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    // Le marché synthétique fait trader la stratégie : les profils ne sont pas vides.
    assert!(rows[0]["n_trades"].as_u64().unwrap() > 0, "{}", rows[0]);
    let mut hashes = Vec::new();
    for (row, name) in rows.iter().zip(PROFILES) {
        assert_eq!(row["name"], name);
        assert_eq!(row["dir"], name);
        let metrics = json(&out.join(name).join("metrics.json"));
        let manifest = json(&out.join(name).join("manifest.json"));
        assert_eq!(metrics["schema"], "tv4-metrics-s10");
        assert_eq!(manifest["schema"], "tv4-manifest-s10");
        assert_eq!(manifest["profile"], name);
        assert_eq!(metrics["costs"]["profile"], name);
        assert_eq!(manifest["profiles_id"], profiles["profiles_id"]);
        assert_eq!(manifest["run_id"], row["run_id"]);
        assert_eq!(metrics["config_hash"], row["config_hash"]);
        assert_eq!(
            manifest["costs_file_hash"], profiles["costs_file_hash"],
            "{name}"
        );
        // `metrics.json` ne porte pas de chemin propre à la machine ; le manifeste garde le chemin donné.
        assert_eq!(metrics["costs"]["costs_file"], "costs_c1.toml");
        assert_eq!(manifest["costs_file"], costs_file());
        assert_eq!(metrics["costs"]["fee_grid"], "okx_regular");
        // Le nom de la grille est dans le bloc haché du `config_hash`.
        assert_eq!(
            metrics["config"]["costs"]["fee_grid"]["grid"],
            "okx_regular"
        );
        assert_eq!(metrics["config"]["schema"], "tv4-portfolio-config-v2");
        hashes.push(metrics["config_hash"].as_str().unwrap().to_string());
    }
    // Deux profils de coûts, deux `config_hash` (critère 1 de l'issue).
    assert_eq!(hashes.len(), 3);
    assert!(hashes[0] != hashes[1] && hashes[1] != hashes[2] && hashes[0] != hashes[2]);
}

#[test]
fn deux_runs_identiques_se_verifient_et_une_autre_graine_change_les_hash() {
    let w = work();
    let (a, b, c) = (w.root.join("a"), w.root.join("b"), w.root.join("c"));
    ok_profiles(&w, &a, &[]);
    ok_profiles(&w, &b, &[]);
    let same = verify(&a, &b);
    assert_eq!(same.code, Some(0), "{}{}", same.stdout, same.stderr);
    assert_eq!(
        fs::read(a.join("profiles.json")).unwrap(),
        fs::read(b.join("profiles.json")).unwrap(),
        "profiles.json est déterministe"
    );

    ok_profiles(&w, &c, &["--cost-seed", "2"]);
    let profiles_c = json(&c.join("profiles.json"));
    assert_eq!(profiles_c["seed"], 2);
    let profiles_a = json(&a.join("profiles.json"));
    assert_ne!(profiles_a["profiles_id"], profiles_c["profiles_id"]);
    for (row_a, row_c) in profiles_a["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .zip(profiles_c["profiles"].as_array().unwrap())
    {
        assert_ne!(row_a["config_hash"], row_c["config_hash"], "{row_a}");
    }
    let different = verify(&a, &c);
    assert_ne!(
        different.code,
        Some(0),
        "{}{}",
        different.stdout,
        different.stderr
    );
}

#[test]
fn fees_okx_et_okx_regular_ecrivent_le_meme_profiles_json() {
    let w = work();
    let (a, b) = (w.root.join("okx"), w.root.join("okx_regular"));
    ok_profiles(&w, &a, &["--fees", "okx"]);
    ok_profiles(&w, &b, &["--fees", "okx_regular"]);
    let (bytes_a, bytes_b) = (
        fs::read(a.join("profiles.json")).unwrap(),
        fs::read(b.join("profiles.json")).unwrap(),
    );
    assert_eq!(bytes_a, bytes_b, "l'alias `okx` redonne `okx_regular`");
    let profiles = json(&a.join("profiles.json"));
    assert_eq!(profiles["fee_grid"], "okx_regular");
    for name in PROFILES {
        assert_eq!(
            fs::read(a.join(name).join("trades.csv")).unwrap(),
            fs::read(b.join(name).join("trades.csv")).unwrap(),
            "{name}"
        );
    }
    // Une autre grille nommée change le nom haché.
    let c = w.root.join("bitvavo");
    ok_profiles(&w, &c, &["--fees", "bitvavo_usdc"]);
    let profiles_c = json(&c.join("profiles.json"));
    assert_eq!(profiles_c["fee_grid"], "bitvavo_usdc");
    assert_ne!(profiles["profiles_id"], profiles_c["profiles_id"]);
}

#[test]
fn sans_out_les_profils_vont_sous_runs_profiles_id() {
    let w = work();
    let costs = costs_file();
    let outcome = launch(&backtest_args(
        &w,
        None,
        &["--symbols", "BTC-USD,ETH-USD", "--costs", &costs],
    ));
    assert_eq!(
        outcome.code,
        Some(0),
        "{}{}",
        outcome.stdout,
        outcome.stderr
    );
    let runs = w.root.join("runs");
    let dirs: Vec<PathBuf> = fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    let profiles = json(&dirs[0].join("profiles.json"));
    assert_eq!(
        dirs[0].file_name().unwrap().to_string_lossy(),
        profiles["profiles_id"].as_str().unwrap()
    );
}

#[test]
fn un_profil_en_echec_ne_laisse_pas_de_profiles_json() {
    let w = work();
    let out = w.root.join("sortie");
    ok_profiles(&w, &out, &[]);
    assert!(out.join("profiles.json").is_file());

    // `maker` devient un fichier : le dossier du profil maker ne peut plus être écrit (un dossier en
    // lecture seule ne suffirait pas, le test peut tourner sous root). L'ancien `profiles.json` doit
    // avoir disparu, et aucun nouveau n'est écrit.
    fs::remove_dir_all(out.join("maker")).unwrap();
    fs::write(out.join("maker"), b"pas un dossier").unwrap();
    let outcome = run_profiles(&w, &out, &[]);
    assert_eq!(
        outcome.code,
        Some(1),
        "{}{}",
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        !out.join("profiles.json").exists(),
        "profiles.json doit être absent après un échec"
    );
    // Les profils écrits avant l'échec n'ont pas non plus de `profiles.json` pour les valider.
    assert!(out.join("baseline").join("manifest.json").is_file());
    // `tv4 verify` refuse un tel dossier (code 1).
    let refused = verify(&out, &out);
    assert_eq!(
        refused.code,
        Some(1),
        "{}{}",
        refused.stdout,
        refused.stderr
    );
}

#[test]
fn les_mesures_de_carnet_sont_lues_hachees_et_remplacent_les_defauts() {
    let w = work();
    // Valeurs synthétiques de plomberie, pas des mesures : 24 h au moins par base (04 §3.2).
    let measures = w.root.join("mesures.json");
    fs::write(
        &measures,
        r#"{
  "schema": "tv4-cost-measures-v1",
  "venue": "okx-test",
  "captured_from_ms": 1700000000000,
  "captured_to_ms": 1700172800000,
  "bases": {
    "BTC": {"inst_id": "BTC-USDC", "hours": 48.0,
            "half_spread_bps": {"median": 0.25, "p95": 0.75},
            "market_slippage_bps": {"median": 0.5}},
    "ETH": {"inst_id": "ETH-USDC", "hours": 48.0,
            "half_spread_bps": {"median": 0.25, "p95": 0.75},
            "market_slippage_bps": {"median": 0.5}}
  }
}
"#,
    )
    .unwrap();
    let (plain, measured) = (w.root.join("plain"), w.root.join("measured"));
    ok_profiles(&w, &plain, &[]);
    ok_profiles(
        &w,
        &measured,
        &["--cost-measures", &measures.display().to_string()],
    );
    let profiles = json(&measured.join("profiles.json"));
    assert!(profiles["cost_measures_hash"].is_string());
    assert!(json(&plain.join("profiles.json"))["cost_measures_hash"].is_null());
    assert_ne!(
        profiles["profiles_id"],
        json(&plain.join("profiles.json"))["profiles_id"]
    );
    for name in PROFILES {
        let metrics = json(&measured.join(name).join("metrics.json"));
        let manifest = json(&measured.join(name).join("manifest.json"));
        assert_eq!(metrics["costs"]["cost_measures_file"], "mesures.json");
        assert_eq!(
            manifest["cost_measures_file"],
            measures.display().to_string()
        );
        assert_eq!(
            metrics["costs"]["cost_measures_hash"],
            profiles["cost_measures_hash"]
        );
        let legs = metrics["costs"]["legs"].as_array().unwrap();
        assert!(!legs.is_empty());
        assert!(
            legs.iter().all(|leg| leg["spread_source"] == "measured"),
            "{legs:?}"
        );
        assert!(
            legs.iter().all(|leg| leg["slippage_source"] == "measured"),
            "{legs:?}"
        );
        assert!(
            !deviations(&metrics).contains(&"cost_defaults_unmeasured".to_string()),
            "mesures complètes : plus de défaut utilisé, {:?}",
            deviations(&metrics)
        );
        // Le stress n'est pas calibré par des mesures de carnet : l'autre écart reste.
        assert!(deviations(&metrics).contains(&"stop_stress_uncalibrated".to_string()));
        // Sans mesures, les défauts sont signalés (symétrie).
        let plain_metrics = json(&plain.join(name).join("metrics.json"));
        assert!(deviations(&plain_metrics).contains(&"cost_defaults_unmeasured".to_string()));
    }
    // Mesures partielles : ETH sans `market_slippage_bps` garde son slippage par défaut, et le rapport
    // continue de le dire.
    let partial_measures = w.root.join("partielles.json");
    fs::write(
        &partial_measures,
        fs::read_to_string(&measures).unwrap().replace(
            r#""ETH": {"inst_id": "ETH-USDC", "hours": 48.0,
            "half_spread_bps": {"median": 0.25, "p95": 0.75},
            "market_slippage_bps": {"median": 0.5}}"#,
            r#""ETH": {"inst_id": "ETH-USDC", "hours": 48.0,
            "half_spread_bps": {"median": 0.25, "p95": 0.75}}"#,
        ),
    )
    .unwrap();
    assert_ne!(
        fs::read_to_string(&partial_measures).unwrap(),
        fs::read_to_string(&measures).unwrap(),
        "le gabarit de mesures partielles doit s'appliquer"
    );
    let partial = w.root.join("partial");
    ok_profiles(
        &w,
        &partial,
        &["--cost-measures", &partial_measures.display().to_string()],
    );
    for name in PROFILES {
        let metrics = json(&partial.join(name).join("metrics.json"));
        let legs = metrics["costs"]["legs"].as_array().unwrap();
        let eth = legs.iter().find(|l| l["symbol"] == "ETH-USD").unwrap();
        assert_eq!(eth["spread_source"], "measured", "{name}");
        assert_eq!(eth["slippage_source"], "default", "{name}");
        let btc = legs.iter().find(|l| l["symbol"] == "BTC-USD").unwrap();
        assert_eq!(btc["slippage_source"], "measured", "{name}");
        assert!(
            deviations(&metrics).contains(&"cost_defaults_unmeasured".to_string()),
            "{name} : {:?}",
            deviations(&metrics)
        );
    }
    // Un fichier de mesures illisible : erreur applicative, rien d'écrit.
    let bad = w.root.join("bad");
    let outcome = run_profiles(&w, &bad, &["--cost-measures", "/n/existe/pas.json"]);
    assert_eq!(
        outcome.code,
        Some(1),
        "{}{}",
        outcome.stdout,
        outcome.stderr
    );
    assert!(!bad.exists());
}

/// Un run s9 écrit dans un ancien dossier de profils retire son `profiles.json` : verify et le control
/// plane classent un dossier sur cette seule présence.
#[test]
fn un_run_s9_dans_un_ancien_dossier_de_profils_en_retire_le_profiles_json() {
    let w = work();
    let (x, y) = (w.root.join("x"), w.root.join("y"));
    ok_profiles(&w, &x, &[]);
    assert!(x.join("profiles.json").exists());
    let args = ["--symbols", "BTC-USD,ETH-USD"];
    for dir in [&x, &y] {
        let outcome = launch(&backtest_args(&w, Some(dir), &args));
        assert_eq!(
            outcome.code,
            Some(0),
            "{}{}",
            outcome.stdout,
            outcome.stderr
        );
    }
    assert!(
        !x.join("profiles.json").exists(),
        "profiles.json périmé retiré"
    );
    let same = verify(&x, &y);
    assert_eq!(same.code, Some(0), "{}{}", same.stdout, same.stderr);
}

/// Même discipline pour un run s8 (dataset d'une seule série) écrit dans un ancien dossier de profils.
#[test]
fn un_run_s8_dans_un_ancien_dossier_de_profils_en_retire_le_profiles_json() {
    let w = work();
    build_dataset(
        &w.root,
        "solo",
        &[(
            "BTC-USD",
            candles(MINUTES, 11, 0.0, 2000.0, common::synthetic_candles),
        )],
        MINUTES as i64 + 100,
    );
    let (x, y) = (w.root.join("x"), w.root.join("y"));
    ok_profiles(&w, &x, &[]);
    assert!(x.join("profiles.json").exists());
    for dir in [&x, &y] {
        let mut args = backtest_args(&w, Some(dir), &[]);
        assert_eq!(args[2], "duo");
        args[2] = "solo".to_string();
        let outcome = launch(&args);
        assert_eq!(
            outcome.code,
            Some(0),
            "{}{}",
            outcome.stdout,
            outcome.stderr
        );
    }
    assert!(
        !x.join("profiles.json").exists(),
        "profiles.json périmé retiré"
    );
    let same = verify(&x, &y);
    assert_eq!(same.code, Some(0), "{}{}", same.stdout, same.stderr);
}
