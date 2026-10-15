//! Tests d'intégration du binaire `tv4` : on lance le vrai exécutable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tv4::model::hash;

mod common;
use common::{DATASET_HASH_SKIP, HASH_FIXTURE_MOIS};

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
    let cas: [(&[&str], &str); 3] = [
        (&["ingest", "okx"], "S7"),
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
fn backtest_15m_sur_la_fixture() {
    // L'écart `single_tf_1m` est levé en S5 : sma_cross 15m (20/50) tourne sur la fixture 1m.
    let dossier = tempfile::tempdir().unwrap();
    let sortie_dir = dossier.path().join("run");
    let sortie = tv4_owned(&backtest_args(
        "config/strategies/sma_cross.toml",
        &sortie_dir,
    ));
    assert_eq!(
        sortie.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&sortie.stderr)
    );
    let valeur: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sortie_dir.join("metrics.json")).unwrap())
            .unwrap();
    // 50 bougies 15m de warmup : la première 1m jouée est la 482e, la fixture ayant des minutes absentes.
    assert_eq!(valeur["window"]["start_index"], 482);
    assert_eq!(valeur["window"]["bars"], 9_518);
    assert_eq!(valeur["config"]["strategy"]["params"]["tf"], "15m");
    // Valeurs de l'oracle Python indépendant (tests/backtest_fixture.rs les épingle trade par trade).
    assert_eq!(valeur["n_trades"], 10);
    assert_eq!(valeur["pnl_net"].as_f64(), Some(-7.401437247087472));
    assert_eq!(valeur["cash_final"].as_f64(), Some(292.5985627529125));
    assert_eq!(
        valeur["config_hash"],
        "e85b28f7cff8f25137970a52a8f5cde0c429cc3cbeee7505f83bd1eed497646a"
    );
    // Six écarts : `single_tf_1m` n'y figure plus.
    let ecarts = valeur["s3_deviations"].as_array().unwrap();
    assert_eq!(ecarts.len(), 6);
    assert!(ecarts.iter().all(|e| e != "single_tf_1m"));
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
    // Code 2 : erreur d'usage clap (ni --fixture ni --dataset : groupe obligatoire).
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

// --- tv4 ingest csv, dataset build, backtest --dataset (S4) ---

const DATASET_HASH_FFILL: &str =
    "blake3:0685813dbc5085b4e87715cf5e7d54041bf478e7079d3b7cbcb68b6826e3697b";
const PARQUET_REL: &str = "data/candles/kraken/BTC-USD/1m/2020-01.parquet";
const MANIFEST_REL: &str = "datasets/fixture_2020_01.json";
const FENETRE_DEBUT: &str = "2020-01-01T00:00:00Z";
const FENETRE_FIN: &str = "2020-01-09T00:00:00Z";

fn texte(sortie: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&sortie.stdout).into_owned(),
        String::from_utf8_lossy(&sortie.stderr).into_owned(),
    )
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// `tv4 ingest csv` avec le CSV donné (1m, kraken, BTC-USD) dans la racine `root`.
fn ingest_csv(root: &Path, csv: &str) -> Output {
    tv4(&[
        "ingest",
        "csv",
        "--csv",
        csv,
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--root",
        &root.display().to_string(),
    ])
}

fn ingest_fixture(root: &Path) -> Output {
    ingest_csv(root, &repo("fixtures/kraken_xbtusd_1m.csv"))
}

/// `tv4 dataset build` sur la fenêtre de la fixture (8 premiers jours de janvier 2020).
fn build(root: &Path, name: &str, policy: &str) -> Output {
    build_fenetre(root, name, policy, FENETRE_DEBUT, FENETRE_FIN)
}

fn build_fenetre(root: &Path, name: &str, policy: &str, from: &str, to: &str) -> Output {
    tv4(&[
        "dataset",
        "build",
        "--root",
        &root.display().to_string(),
        "--name",
        name,
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--from",
        from,
        "--to",
        to,
        "--gaps-policy",
        policy,
    ])
}

/// `tv4 backtest --dataset` : sortie dans `out`.
fn backtest_dataset(root: &Path, manifest: &str, out: &Path) -> Output {
    backtest_dataset_params(root, manifest, out, PARAMS_1M)
}

/// Comme [`backtest_dataset`], avec le fichier de paramètres de `sma_cross` au choix.
fn backtest_dataset_params(root: &Path, manifest: &str, out: &Path, params: &str) -> Output {
    tv4(&[
        "backtest",
        "--root",
        &root.display().to_string(),
        "--dataset",
        manifest,
        "--strategy",
        "sma_cross",
        "--params",
        &repo(params),
        "--fees-file",
        &repo("config/fees.toml"),
        "--out",
        &out.display().to_string(),
    ])
}

/// Racine temporaire avec la fixture ingérée et le dataset `fixture_2020_01` construit en `skip`.
fn racine_prete() -> tempfile::TempDir {
    let racine = tempfile::tempdir().unwrap();
    assert_eq!(ingest_fixture(racine.path()).status.code(), Some(0));
    let sortie = build(racine.path(), "fixture_2020_01", "skip");
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    racine
}

#[test]
fn preuve_s4_ingest_build_backtest_reproduit_s3() {
    let racine = tempfile::tempdir().unwrap();
    let r = racine.path();

    // 1. Ingestion de la fixture.
    let sortie = ingest_fixture(r);
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(0), "{stderr}");
    for attendu in [
        "2020-01.parquet",
        "10000 bougies",
        "créé",
        "période close",
        HASH_FIXTURE_MOIS,
    ] {
        assert!(stdout.contains(attendu), "« {attendu} » absent de {stdout}");
    }
    assert!(r.join(PARQUET_REL).is_file());
    assert!(
        r.join("data/candles/kraken/BTC-USD/1m/2020-01._manifest.json")
            .is_file()
    );
    assert!(r.join("state/locks/kraken/BTC-USD/1m.lock").is_file());

    // 2. Dataset figé.
    let sortie = build(r, "fixture_2020_01", "skip");
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let manifeste = json(&r.join(MANIFEST_REL));
    assert_eq!(manifeste["dataset_hash"], DATASET_HASH_SKIP);
    assert_eq!(manifeste["row_count"], 10_000);
    assert_eq!(manifeste["gaps_policy"], "skip");
    assert_eq!(manifeste["window"][0], FENETRE_DEBUT);
    assert_eq!(manifeste["window"][1], FENETRE_FIN);
    let fichiers = manifeste["files"].as_array().unwrap();
    assert_eq!(fichiers.len(), 1);
    assert_eq!(fichiers[0]["content_hash"], HASH_FIXTURE_MOIS);
    assert_eq!(fichiers[0]["snapshot"], false);

    // 3. Même backtest depuis le dataset et depuis la fixture CSV.
    let out_dataset = r.join("run-dataset");
    let sortie = backtest_dataset(r, MANIFEST_REL, &out_dataset);
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let out_fixture = r.join("run-fixture");
    let sortie = tv4_owned(&backtest_args(PARAMS_1M, &out_fixture));
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);

    // 4. Égalité au bit hors du bloc `data`, et valeurs du point de contrôle S3 en dur.
    let mut dataset = json(&out_dataset.join("metrics.json"));
    let mut fixture = json(&out_fixture.join("metrics.json"));
    let data = dataset["data"].take();
    assert!(fixture["data"].take().is_object());
    assert_eq!(dataset, fixture);
    assert_eq!(dataset["n_trades"], 23);
    assert_eq!(dataset["pnl_net"].as_f64(), Some(-15.408523707748486));
    assert_eq!(dataset["cash_final"].as_f64(), Some(284.5914762922515));
    assert_eq!(dataset["fees_total"].as_f64(), Some(44.5242015770599));
    assert_eq!(
        dataset["config_hash"],
        "3d45e8898218e61e8cf41ced0befe350412d7d0bc6e0f21dab2c3234f8c5f411"
    );
    assert_eq!(data["source"], "dataset");
    assert_eq!(data["dataset"], "fixture_2020_01");
    assert_eq!(data["dataset_hash"], DATASET_HASH_SKIP);
    assert_eq!(data["hash_format"], "dsh-v1");
    assert_eq!(data["bars"], 10_000);
    assert_eq!(data["synthetic_bars"], 0);
}

#[test]
fn ingest_csv_est_idempotent() {
    let racine = tempfile::tempdir().unwrap();
    let r = racine.path();
    assert_eq!(ingest_fixture(r).status.code(), Some(0));
    let parquet = std::fs::read(r.join(PARQUET_REL)).unwrap();
    let manifeste =
        std::fs::read(r.join("data/candles/kraken/BTC-USD/1m/2020-01._manifest.json")).unwrap();

    let sortie = ingest_fixture(r);
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("inchangé"), "{stdout}");
    assert_eq!(std::fs::read(r.join(PARQUET_REL)).unwrap(), parquet);
    assert_eq!(
        std::fs::read(r.join("data/candles/kraken/BTC-USD/1m/2020-01._manifest.json")).unwrap(),
        manifeste
    );
}

#[test]
fn ingest_csv_refuse_le_desordre() {
    let racine = tempfile::tempdir().unwrap();
    let r = racine.path();
    let source = std::fs::read_to_string(repo("fixtures/kraken_xbtusd_1m.csv")).unwrap();
    let lignes: Vec<&str> = source.lines().take(3).collect();
    let permute = format!("{}\n{}\n{}\n", lignes[0], lignes[2], lignes[1]);
    let csv = r.join("desordre.csv");
    std::fs::write(&csv, permute).unwrap();

    let sortie = ingest_csv(&r.join("store"), &csv.display().to_string());
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("non strictement croissant"), "{stderr}");
    assert!(!r.join("store/data").exists());
}

#[test]
fn dataset_build_idempotent_et_immuable() {
    let racine = racine_prete();
    let r = racine.path();
    let avant = std::fs::read(r.join(MANIFEST_REL)).unwrap();

    let sortie = build(r, "fixture_2020_01", "skip");
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("inchangé"), "{stdout}");
    assert_eq!(std::fs::read(r.join(MANIFEST_REL)).unwrap(), avant);

    // Même nom, autre politique : autre dataset_hash, refusé.
    let sortie = build(r, "fixture_2020_01", "ffill");
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(MANIFEST_REL), "{stderr}");
    assert!(stderr.contains(DATASET_HASH_SKIP), "{stderr}");
    assert!(stderr.contains(DATASET_HASH_FFILL), "{stderr}");
    assert_eq!(std::fs::read(r.join(MANIFEST_REL)).unwrap(), avant);
}

#[test]
fn dataset_build_erreurs() {
    let racine = tempfile::tempdir().unwrap();
    let r = racine.path();
    assert_eq!(ingest_fixture(r).status.code(), Some(0));

    // Code 2 : erreur d'usage (date sans heure, politique inconnue).
    let sortie = build_fenetre(r, "d", "skip", "2020-01-01", FENETRE_FIN);
    assert_eq!(sortie.status.code(), Some(2), "{}", texte(&sortie).1);
    let sortie = build(r, "d", "bfill");
    assert_eq!(sortie.status.code(), Some(2), "{}", texte(&sortie).1);

    // Code 1 : fenêtre vide, puis fenêtre sans données.
    let sortie = build_fenetre(r, "d", "skip", FENETRE_DEBUT, FENETRE_DEBUT);
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("fenêtre vide"), "{stderr}");
    let sortie = build_fenetre(
        r,
        "d",
        "skip",
        "2021-01-01T00:00:00Z",
        "2021-01-02T00:00:00Z",
    );
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("aucune bougie"), "{stderr}");
    assert!(!r.join("datasets/d.json").exists());
}

#[test]
fn backtest_refuse_un_fichier_modifie() {
    let racine = racine_prete();
    let r = racine.path();

    // Parquet valide au contenu différent : les 9 999 premières lignes, ingérées dans une autre racine.
    let source = std::fs::read_to_string(repo("fixtures/kraken_xbtusd_1m.csv")).unwrap();
    let tronque: String = source
        .lines()
        .take(9_999)
        .map(|l| format!("{l}\n"))
        .collect();
    let autre = tempfile::tempdir().unwrap();
    let csv = autre.path().join("tronque.csv");
    std::fs::write(&csv, tronque).unwrap();
    let sortie = ingest_csv(autre.path(), &csv.display().to_string());
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    std::fs::copy(autre.path().join(PARQUET_REL), r.join(PARQUET_REL)).unwrap();

    let out = r.join("run");
    let sortie = backtest_dataset(r, MANIFEST_REL, &out);
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(PARQUET_REL), "{stderr}");
    assert!(stderr.contains("content_hash"), "{stderr}");
    assert!(!out.join("metrics.json").exists());
}

#[test]
fn backtest_refuse_zero_volume_marked() {
    let racine = racine_prete();
    let r = racine.path();
    let sortie = build(r, "fixture_zvm", "zero_volume_marked");
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);

    let out = r.join("run");
    let sortie = backtest_dataset(r, "datasets/fixture_zvm.json", &out);
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("zero_volume_marked"), "{stderr}");
    assert!(stderr.contains("S6"), "{stderr}");
    // Le chemin cité est celui passé à --dataset, pas celui joint à la racine.
    assert!(
        stderr.contains("`datasets/fixture_zvm.json`"),
        "chemin tel que passé attendu : {stderr}"
    );
    assert!(
        !stderr.contains(&r.display().to_string()),
        "la racine ne doit pas apparaître : {stderr}"
    );
    assert!(!out.join("metrics.json").exists());
}

/// Réécrit le manifeste de dataset `MANIFEST_REL` de `r`.
fn modifier_manifeste(r: &Path, f: impl FnOnce(&mut serde_json::Value)) {
    let chemin = r.join(MANIFEST_REL);
    let mut valeur = json(&chemin);
    f(&mut valeur);
    std::fs::write(&chemin, serde_json::to_vec_pretty(&valeur).unwrap()).unwrap();
}

#[test]
fn backtest_refuse_un_dataset_hors_1m_en_citant_le_chemin_passe() {
    let racine = racine_prete();
    let r = racine.path();
    modifier_manifeste(r, |v| v["tfs"] = serde_json::json!(["5m"]));
    let out = r.join("run");
    let sortie = backtest_dataset(r, MANIFEST_REL, &out);
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&format!("`{MANIFEST_REL}`")), "{stderr}");
    assert!(stderr.contains("en 5m"), "{stderr}");
    assert!(!stderr.contains(&r.display().to_string()), "{stderr}");
    assert!(!out.join("metrics.json").exists());
}

#[test]
fn backtest_refuse_un_manifeste_sans_serie_sans_replis_sur_le_1m() {
    for (champ, symbols, tfs) in [("tfs", 1, 0), ("symbols", 0, 1)] {
        let racine = racine_prete();
        let r = racine.path();
        modifier_manifeste(r, |v| v[champ] = serde_json::json!([]));
        let out = r.join("run");
        let sortie = backtest_dataset(r, MANIFEST_REL, &out);
        let (_, stderr) = texte(&sortie);
        assert_eq!(sortie.status.code(), Some(1), "{champ} : {stderr}");
        assert!(
            stderr.contains(&format!("{symbols} symbole(s) et {tfs} timeframe(s)")),
            "{champ} : {stderr}"
        );
        // Pas le message contradictoire « en 1m : le moteur ne rejoue que du 1m ».
        assert!(
            !stderr.contains("ne rejoue que du 1m"),
            "{champ} : {stderr}"
        );
        assert!(!out.join("metrics.json").exists());
    }
}

#[test]
fn backtest_ffill_rejoue_les_bougies_synthetiques() {
    let racine = racine_prete();
    let r = racine.path();
    let sortie = build(r, "fixture_ffill", "ffill");
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);

    let out = r.join("run");
    let sortie = backtest_dataset(r, "datasets/fixture_ffill.json", &out);
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let metrics = json(&out.join("metrics.json"));
    assert_eq!(metrics["data"]["bars"], 11_370);
    assert_eq!(metrics["data"]["synthetic_bars"], 1_370);
    assert_eq!(metrics["data"]["gaps_policy"], "ffill");
    assert_eq!(metrics["data"]["dataset_hash"], DATASET_HASH_FFILL);
}

#[test]
fn backtest_ffill_multi_tf_inclut_les_bougies_synthetiques() {
    // Revue S5-R6 (E5) : les 1 370 minutes synthétiques du dataset `ffill` entrent dans l'agrégation
    // 15m comme les autres (volume 0, prix de la clôture précédente). Valeurs de l'oracle Python
    // indépendant (`replay` appliqué à la fixture dont les trous sont remplis à la main).
    let racine = racine_prete();
    let r = racine.path();
    let sortie = build(r, "fixture_ffill", "ffill");
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);

    let out = r.join("run15");
    let sortie = backtest_dataset_params(
        r,
        "datasets/fixture_ffill.json",
        &out,
        "config/strategies/sma_cross.toml",
    );
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let metrics = json(&out.join("metrics.json"));
    assert_eq!(metrics["data"]["bars"], 11_370);
    assert_eq!(metrics["data"]["synthetic_bars"], 1_370);
    // Sans trou, 50 bougies 15m = 750 minutes : start_index 750, et non 482 comme en politique `skip`.
    assert_eq!(metrics["window"]["start_index"], 750);
    assert_eq!(metrics["window"]["bars"], 10_620);
    assert_eq!(metrics["n_trades"], 10);
    assert_eq!(metrics["signals"]["emitted"], 20);
    assert_eq!(metrics["pnl_net"].as_f64(), Some(-7.330802213582558));
    assert_eq!(metrics["cash_final"].as_f64(), Some(292.66919778641744));
    assert_eq!(metrics["fees_total"].as_f64(), Some(18.99174758766156));
}

#[test]
fn backtest_fixture_ou_dataset() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    // Aucun des deux : erreur d'usage.
    let mut args = backtest_args(PARAMS_1M, &out);
    let position = args.iter().position(|a| a == "--fixture").unwrap();
    args.drain(position..position + 2);
    assert_eq!(tv4_owned(&args).status.code(), Some(2));
    // Les deux : erreur d'usage.
    let mut args = backtest_args(PARAMS_1M, &out);
    args.extend(["--dataset".to_string(), "datasets/x.json".to_string()]);
    assert_eq!(tv4_owned(&args).status.code(), Some(2));
    assert!(!out.join("metrics.json").exists());
}

#[test]
fn deux_process_meme_metrics_dataset() {
    let racine = racine_prete();
    let r = racine.path();
    let mut contenus = Vec::new();
    for nom in ["a", "b"] {
        let out = r.join(nom);
        let sortie = backtest_dataset(r, MANIFEST_REL, &out);
        assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
        contenus.push(std::fs::read(out.join("metrics.json")).unwrap());
    }
    assert_eq!(contenus[0], contenus[1]);
}
