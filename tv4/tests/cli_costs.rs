//! `tv4 costs calibrate-stress` (S10, lot F, docs/specs/s10.md §11.2, §14.2 et §16) : on lance le vrai
//! binaire sur la fixture `fixtures/kraken_xbtusd_1m.csv`, ingérée puis figée en dataset par la CLI. On
//! vérifie le fichier écrit (schéma, datasets, `dataset_hash`), la stabilité du `measurement_hash` entre
//! deux runs, l'absence d'écrasement, l'écriture sous `<root>/calibration/` et les erreurs d'usage
//! (code 2). Aucune valeur de marché n'est attendue : les comptes viennent du binaire.

mod report_s9_common;

use std::fs;
use std::path::{Path, PathBuf};

use report_s9_common::{repo, tv4};
use serde_json::Value;

const DATASET: &str = "fixture_2020_01";
const DATASET_ETH: &str = "fixture_eth";
const FENETRE_DEBUT: &str = "2020-01-01T00:00:00Z";
const FENETRE_FIN: &str = "2020-01-09T00:00:00Z";

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn launch(args: &[&str]) -> Outcome {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let sortie = tv4(&args);
    Outcome {
        code: sortie.status.code(),
        stdout: String::from_utf8_lossy(&sortie.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&sortie.stderr).into_owned(),
    }
}

/// Racine temporaire avec la fixture ingérée et le dataset `fixture_2020_01` construit en `skip`.
fn racine_prete() -> tempfile::TempDir {
    let racine = tempfile::tempdir().unwrap();
    let root = racine.path().display().to_string();
    let csv = repo("fixtures/kraken_xbtusd_1m.csv").display().to_string();
    let ingest = launch(&[
        "ingest",
        "csv",
        "--csv",
        &csv,
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--root",
        &root,
    ]);
    assert_eq!(ingest.code, Some(0), "{}", ingest.stderr);
    let build = launch(&[
        "dataset",
        "build",
        "--root",
        &root,
        "--name",
        DATASET,
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--from",
        FENETRE_DEBUT,
        "--to",
        FENETRE_FIN,
        "--gaps-policy",
        "skip",
    ]);
    assert_eq!(build.code, Some(0), "{}", build.stderr);
    racine
}

/// Comme [`racine_prete`], plus un second dataset `fixture_eth` : la même fixture ingérée sous ETH-USD.
fn racine_a_deux_datasets() -> tempfile::TempDir {
    let racine = racine_prete();
    let root = racine.path().display().to_string();
    let csv = repo("fixtures/kraken_xbtusd_1m.csv").display().to_string();
    let ingest = launch(&[
        "ingest",
        "csv",
        "--csv",
        &csv,
        "--exchange",
        "kraken",
        "--symbol",
        "ETH-USD",
        "--tf",
        "1m",
        "--root",
        &root,
    ]);
    assert_eq!(ingest.code, Some(0), "{}", ingest.stderr);
    let build = launch(&[
        "dataset",
        "build",
        "--root",
        &root,
        "--name",
        DATASET_ETH,
        "--exchange",
        "kraken",
        "--symbol",
        "ETH-USD",
        "--tf",
        "1m",
        "--from",
        FENETRE_DEBUT,
        "--to",
        FENETRE_FIN,
        "--gaps-policy",
        "skip",
    ]);
    assert_eq!(build.code, Some(0), "{}", build.stderr);
    racine
}

fn costs_file() -> String {
    repo("tv4/tests/fixtures/costs_c1.toml")
        .display()
        .to_string()
}

/// `tv4 costs calibrate-stress` sur le dataset de la fixture, suivi de `extra`.
fn calibrate(root: &Path, extra: &[&str]) -> Outcome {
    let root = root.display().to_string();
    let costs = costs_file();
    let mut args = vec![
        "costs",
        "calibrate-stress",
        "--root",
        &root,
        "--dataset",
        DATASET,
        "--costs",
        &costs,
    ];
    args.extend_from_slice(extra);
    launch(&args)
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Chemin du fichier annoncé sur la première ligne de la sortie standard.
fn announced(outcome: &Outcome) -> PathBuf {
    let line = outcome
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("fichier : "))
        .unwrap_or_else(|| panic!("ligne « fichier : » absente de {}", outcome.stdout));
    PathBuf::from(line)
}

#[test]
fn le_fichier_est_ecrit_sous_calibration_au_schema_avec_les_datasets() {
    let racine = racine_prete();
    let r = racine.path();
    let outcome = calibrate(r, &["--tf", "4h", "--k", "2"]);
    assert_eq!(
        outcome.code,
        Some(0),
        "{}{}",
        outcome.stdout,
        outcome.stderr
    );
    let path = announced(&outcome);
    assert_eq!(path.parent().unwrap(), r.join("calibration"));
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("stress-")
            && name.ends_with("Z.json")
            && name.len() == "stress-".len() + 16 + ".json".len(),
        "{name}"
    );
    let stamp = &name["stress-".len()..name.len() - ".json".len()];
    assert!(
        stamp
            .chars()
            .all(|c| c.is_ascii_digit() || c == 'T' || c == 'Z'),
        "{name}"
    );

    let file = json(&path);
    assert_eq!(file["schema"], "tv4-stress-calibration-v1");
    assert!(file["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(file["commit"].is_string());
    assert_eq!(file["params"]["tfs"], serde_json::json!(["4h"]));
    assert_eq!(file["params"]["k"], serde_json::json!([2.0]));
    assert_eq!(file["params"]["horizon_bars"], 30);
    assert_eq!(file["params"]["activation_delay_ms"], 60_000);
    assert_eq!(file["params"]["atr_period"], 14);
    let dataset = json(&r.join("datasets").join(format!("{DATASET}.json")));
    assert_eq!(
        file["datasets"],
        serde_json::json!([{"name": DATASET, "dataset_hash": dataset["dataset_hash"]}])
    );
    let results = file["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["symbol"], "BTC-USD");
    assert_eq!(results[0]["class"], "major");
    assert_eq!(results[0]["tf"], "4h");
    let pooled = file["pooled"].as_array().unwrap();
    assert_eq!(pooled.len(), 1);
    assert_eq!(pooled[0]["class"], "major");
    assert_eq!(
        results[0]["positions"].as_u64().unwrap(),
        results[0]["triggered"].as_u64().unwrap() + results[0]["expired"].as_u64().unwrap()
    );
    // La sortie standard rappelle le `measurement_hash` du fichier et le tableau `pooled`.
    let hash = file["measurement_hash"].as_str().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(
        outcome
            .stdout
            .contains(&format!("measurement_hash : {hash}")),
        "{}",
        outcome.stdout
    );
    assert!(outcome.stdout.contains("major 4h"), "{}", outcome.stdout);
}

#[test]
fn deux_runs_donnent_le_meme_measurement_hash_et_n_ecrasent_rien() {
    let racine = racine_prete();
    let r = racine.path();
    let (a, b) = (r.join("a.json"), r.join("b.json"));
    let (a_arg, b_arg) = (a.display().to_string(), b.display().to_string());
    let first = calibrate(r, &["--out", &a_arg]);
    assert_eq!(first.code, Some(0), "{}{}", first.stdout, first.stderr);
    let second = calibrate(r, &["--out", &b_arg]);
    assert_eq!(second.code, Some(0), "{}{}", second.stdout, second.stderr);
    let (file_a, file_b) = (json(&a), json(&b));
    assert_eq!(file_a["measurement_hash"], file_b["measurement_hash"]);
    assert_eq!(file_a["results"], file_b["results"]);
    assert_eq!(file_a["pooled"], file_b["pooled"]);
    // Les paramètres par défaut : 1d et 4h, trois multiples d'ATR.
    assert_eq!(file_a["params"]["tfs"], serde_json::json!(["1d", "4h"]));
    assert_eq!(file_a["results"].as_array().unwrap().len(), 2 * 3);

    // Pas d'écrasement : le fichier existe, code 1, contenu intact, aucun `.tmp` oublié.
    let before = fs::read(&a).unwrap();
    let refused = calibrate(r, &["--out", &a_arg]);
    assert_eq!(
        refused.code,
        Some(1),
        "{}{}",
        refused.stdout,
        refused.stderr
    );
    assert!(refused.stderr.contains("existe déjà"), "{}", refused.stderr);
    assert_eq!(fs::read(&a).unwrap(), before);
    assert!(!r.join("a.json.tmp").exists());

    // Des paramètres différents changent le `measurement_hash`.
    let c = r.join("c.json");
    let other = calibrate(r, &["--out", &c.display().to_string(), "--range-mult", "4"]);
    assert_eq!(other.code, Some(0), "{}{}", other.stdout, other.stderr);
    assert_ne!(json(&c)["measurement_hash"], file_a["measurement_hash"]);
}

#[test]
fn le_nom_par_defaut_ne_s_ecrase_pas_non_plus() {
    let racine = racine_prete();
    let r = racine.path();
    let outcome = calibrate(r, &["--tf", "1d", "--k", "2"]);
    assert_eq!(
        outcome.code,
        Some(0),
        "{}{}",
        outcome.stdout,
        outcome.stderr
    );
    let path = announced(&outcome);
    let before = fs::read(&path).unwrap();
    // Un fichier de même nom déjà présent (même seconde) n'est pas écrasé : on le vérifie en demandant
    // explicitement ce chemin.
    let again = calibrate(
        r,
        &[
            "--tf",
            "1d",
            "--k",
            "2",
            "--out",
            &path.display().to_string(),
        ],
    );
    assert_eq!(again.code, Some(1), "{}{}", again.stdout, again.stderr);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn les_erreurs_d_usage_sortent_en_code_2() {
    let racine = racine_prete();
    let r = racine.path();
    let root = r.display().to_string();
    let costs = costs_file();
    let base = |dataset: &str, extra: &[&str]| -> Outcome {
        let mut args = vec![
            "costs",
            "calibrate-stress",
            "--root",
            &root,
            "--dataset",
            dataset,
            "--costs",
            &costs,
        ];
        args.extend_from_slice(extra);
        launch(&args)
    };
    // Dataset absent.
    let absent = base("n_existe_pas", &[]);
    assert_eq!(absent.code, Some(2), "{}{}", absent.stdout, absent.stderr);
    assert!(absent.stderr.contains("n_existe_pas"), "{}", absent.stderr);
    // TF inconnu, k <= 0, horizon 0, range <= 0 : clap.
    for extra in [
        ["--tf", "3h"],
        ["--k", "0"],
        ["--k", "-2"],
        ["--horizon-bars", "0"],
        ["--range-mult", "0"],
    ] {
        let outcome = base(DATASET, &extra);
        assert_eq!(outcome.code, Some(2), "{extra:?} : {}", outcome.stderr);
    }
    // Le 1m n'est pas un TF de décision, et un TF répété non plus : refus du module, aussi en code 2.
    for extra in [["--tf", "1m"], ["--tf", "4h"]] {
        let mut args = extra.to_vec();
        if extra[1] == "4h" {
            args.extend(["--tf", "4h"]);
        }
        let outcome = base(DATASET, &args);
        assert_eq!(
            outcome.code,
            Some(2),
            "{args:?} : {}{}",
            outcome.stdout,
            outcome.stderr
        );
    }
    assert!(!r.join("calibration").exists(), "aucun fichier écrit");
    // Sans --dataset : clap.
    let sans = launch(&["costs", "calibrate-stress", "--root", &root]);
    assert_eq!(sans.code, Some(2));
    // Fichier de coûts absent : erreur applicative, code 1.
    let sans_costs = launch(&[
        "costs",
        "calibrate-stress",
        "--root",
        &root,
        "--dataset",
        DATASET,
        "--costs",
        "/n/existe/pas.toml",
    ]);
    assert_eq!(sans_costs.code, Some(1), "{}", sans_costs.stderr);
}

#[test]
fn l_ordre_des_options_dataset_ne_change_ni_les_resultats_ni_le_measurement_hash() {
    let racine = racine_a_deux_datasets();
    let r = racine.path();
    let root = r.display().to_string();
    let costs = costs_file();
    let lancer = |datasets: [&str; 2], nom: &str| -> Value {
        let out = r.join(nom);
        let outcome = launch(&[
            "costs",
            "calibrate-stress",
            "--root",
            &root,
            "--dataset",
            datasets[0],
            "--dataset",
            datasets[1],
            "--costs",
            &costs,
            "--tf",
            "1h",
            "--k",
            "2",
            "--horizon-bars",
            "3",
            "--out",
            &out.display().to_string(),
        ]);
        assert_eq!(
            outcome.code,
            Some(0),
            "{}{}",
            outcome.stdout,
            outcome.stderr
        );
        json(&out)
    };
    let direct = lancer([DATASET, DATASET_ETH], "direct.json");
    let inverse = lancer([DATASET_ETH, DATASET], "inverse.json");
    let symboles: Vec<&str> = direct["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(symboles, ["BTC-USD", "ETH-USD"], "symbole croissant");
    assert_eq!(direct["results"], inverse["results"]);
    assert_eq!(direct["pooled"], inverse["pooled"]);
    assert_eq!(direct["datasets"], inverse["datasets"]);
    assert_eq!(direct["measurement_hash"], inverse["measurement_hash"]);
}
