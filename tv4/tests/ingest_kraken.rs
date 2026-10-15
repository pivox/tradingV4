//! Tests de bout en bout de `tv4 ingest kraken` (S5, passe A) : on lance le vrai exécutable sur des
//! zips construits à la volée, qui reprennent la structure du dump réel (entrées à la racine nommées
//! `<PAIRE>_<minutes>.csv`, sans en-tête, plus un `MANIFEST.json`).
//!
//! Les contenus viennent de la fixture réelle XBTUSD (`fixtures/kraken_xbtusd_*.csv`) sauf trois
//! échantillons **synthétiques** annoncés comme tels (`AIXBTUSD_1.csv`, `ETHUSD_1.csv`) et le
//! `MANIFEST.json` **synthétique** de `common`. Aucun zip n'est commité.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;
use common::{
    DATASET_HASH_SKIP, HASH_FIXTURE_MOIS, LIGNES_AIXBT_SYNTHETIQUES, ecrire_zip,
    manifeste_synthetique, repo_path, sha256_fichier, texte_fixture_1m,
};

const PARQUET_REL: &str = "data/candles/kraken/BTC-USD/1m/2020-01.parquet";
const MANIFEST_REL: &str = "data/candles/kraken/BTC-USD/1m/2020-01._manifest.json";
const RAPPORT_BTC: &str = "reports/ingest/kraken/BTC-USD.json";
const RESUME: &str = "reports/ingest/kraken/_summary.json";

fn tv4(args: &[String]) -> Output {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(refs)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

fn texte(sortie: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&sortie.stdout).into_owned(),
        String::from_utf8_lossy(&sortie.stderr).into_owned(),
    )
}

fn json(chemin: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(chemin).expect("fichier JSON")).expect("JSON valide")
}

/// Arguments de `tv4 ingest kraken` sur `zip`, avec le SHA-256 donné.
fn args_ingest(root: &Path, zip: &Path, sha: &str, paires: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "ingest",
        "kraken",
        "--root",
        &root.display().to_string(),
        "--zip",
        &zip.display().to_string(),
        "--sha256",
        sha,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    for paire in paires {
        args.push("--pair".to_string());
        args.push((*paire).to_string());
    }
    args
}

/// Ingère `zip` (SHA-256 calculé sur le fichier) pour les paires données.
fn ingest(root: &Path, zip: &Path, paires: &[&str]) -> Output {
    tv4(&args_ingest(root, zip, &sha256_fichier(zip), paires))
}

/// Zip standard : manifeste de couverture `fin`, XBTUSD en 1m, 5m et 60 (fixtures réelles) et
/// `AIXBTUSD_1.csv` synthétique (piège de suffixe).
fn zip_standard(chemin: &Path, fin: &str) {
    let m1 = texte_fixture_1m();
    let m5 = std::fs::read(repo_path("fixtures/kraken_xbtusd_5m.csv")).unwrap();
    let h1 = std::fs::read(repo_path("fixtures/kraken_xbtusd_1h.csv")).unwrap();
    ecrire_zip(
        chemin,
        &[
            ("MANIFEST.json", &manifeste_synthetique(fin)),
            ("XBTUSD_1.csv", m1.as_bytes()),
            ("XBTUSD_5.csv", &m5),
            ("XBTUSD_60.csv", &h1),
            ("AIXBTUSD_1.csv", LIGNES_AIXBT_SYNTHETIQUES.as_bytes()),
        ],
    );
}

/// Zip avec un seul XBTUSD_1.csv donné et le manifeste standard.
fn zip_avec_xbtusd(chemin: &Path, contenu: &str) {
    ecrire_zip(
        chemin,
        &[
            ("MANIFEST.json", &manifeste_synthetique("2026-09-30")),
            ("XBTUSD_1.csv", contenu.as_bytes()),
        ],
    );
}

fn dossier() -> tempfile::TempDir {
    tempfile::tempdir().expect("dossier temporaire")
}

#[test]
fn ingest_kraken_sur_un_zip_de_la_fixture() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    zip_standard(&zip, "2026-09-30");
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("SHA-256 vérifié"), "{stdout}");
    assert!(stdout.contains("XBTUSD -> BTC-USD"), "{stdout}");
    assert!(
        stdout.contains("fin de couverture retenue : 2026-10-01T00:00:00Z"),
        "{stdout}"
    );
    assert!(stdout.contains("10000 gardées"), "{stdout}");
    assert!(
        stdout.contains("951 trous (plus long 480000 ms)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("trimestres exclus (> 1 % absent) en 4h : aucun ; en 1d : aucun"),
        "{stdout}"
    );

    // Parquet et manifeste : même content_hash que `ingest csv` (S4).
    assert!(root.join(PARQUET_REL).is_file());
    let manifeste = json(&root.join(MANIFEST_REL));
    assert_eq!(manifeste["content_hash"], HASH_FIXTURE_MOIS);
    assert_eq!(manifeste["source"], "kraken-ohlcvt");
    assert_eq!(manifeste["closed"], true);
    assert_eq!(manifeste["row_count"], 10_000);
    assert_eq!(
        manifeste["continuity"],
        serde_json::json!({
            "gaps": 951,
            "longest_gap_ms": 480_000,
            "duplicates": 0,
            "rejected_unclosed": 0,
            "ohlc_errors": 0
        })
    );
    // Le piège de suffixe : AIXBTUSD_1.csv n'a pas été lu.
    assert!(!root.join("data/candles/kraken/AIXBT-USD").exists());

    // Rapport de la paire.
    let rapport = json(&root.join(RAPPORT_BTC));
    assert_eq!(rapport["schema"], "tv4-ingest-kraken-v1");
    assert_eq!(rapport["status"], "ok");
    assert_eq!(rapport["pair"], "XBTUSD");
    assert_eq!(rapport["entry"], "XBTUSD_1.csv");
    assert_eq!(rapport["end_of_coverage"], "2026-10-01T00:00:00Z");
    assert_eq!(rapport["continuity"]["stats"]["rows_read"], 10_000);
    assert_eq!(rapport["continuity"]["stats"]["rows_kept"], 10_000);
    assert_eq!(rapport["continuity"]["blocking"], serde_json::json!([]));
    assert_eq!(rapport["files"][0]["path"], PARQUET_REL);
    assert_eq!(rapport["files"][0]["status"], "created");
    assert_eq!(rapport["files"][0]["content_hash"], HASH_FIXTURE_MOIS);

    // Rapport de qualité : le tableau de la spécification (seul trimestre 2020-Q1).
    let qualite = &rapport["quality"];
    assert_eq!(qualite["first_open_time"], "2020-01-01T00:00:00Z");
    assert_eq!(qualite["end_time"], "2020-01-08T21:30:00Z");
    assert_eq!(qualite["threshold"].as_f64(), Some(0.01));
    let attendu: [(&str, u64, u64, u64, f64, bool, i64); 8] = [
        (
            "1m",
            11_370,
            10_000,
            1_370,
            0.12049252418645559,
            true,
            480_000,
        ),
        ("5m", 2_274, 2_271, 3, 0.0013192612137203166, false, 300_000),
        ("15m", 758, 758, 0, 0.0, false, 0),
        ("30m", 379, 379, 0, 0.0, false, 0),
        ("1h", 189, 189, 0, 0.0, false, 0),
        ("4h", 47, 47, 0, 0.0, false, 0),
        ("12h", 15, 15, 0, 0.0, false, 0),
        ("1d", 7, 7, 0, 0.0, false, 0),
    ];
    let lignes = qualite["rows"].as_array().unwrap();
    assert_eq!(lignes.len(), 8);
    for (ligne, (tf, expected, present, missing, fraction, excluded, gap)) in
        lignes.iter().zip(attendu)
    {
        assert_eq!(ligne["tf"], tf);
        assert_eq!(ligne["quarter"], "2020-Q1", "{tf}");
        assert_eq!(ligne["expected"], expected, "{tf}");
        assert_eq!(ligne["present"], present, "{tf}");
        assert_eq!(ligne["missing"], missing, "{tf}");
        // `serde_json` lit les flottants sans `float_roundtrip` : l'analyse du texte peut différer d'un
        // ulp de la valeur écrite (0.12049252418645559 est relu 0.1204925241864556). La fraction exacte
        // est vérifiée au bit dans les tests unitaires du store ; ici, la tolérance est de 1e-15.
        let lue = ligne["missing_fraction"].as_f64().unwrap();
        assert!(
            (lue - fraction).abs() < 1e-15,
            "{tf} : {lue} au lieu de {fraction}"
        );
        assert_eq!(ligne["excluded"], excluded, "{tf}");
        assert_eq!(ligne["longest_gap_ms"], gap, "{tf}");
    }

    // Résumé : le JSON brut du manifeste du zip est recopié.
    let resume = json(&root.join(RESUME));
    assert_eq!(resume["schema"], "tv4-ingest-kraken-summary-v1");
    assert_eq!(resume["sha256_verified"], true);
    assert_eq!(resume["sha256"], sha256_fichier(&zip));
    assert_eq!(resume["entries"], 5);
    assert_eq!(resume["manifest"]["raw"]["release"], "r1");
    assert_eq!(resume["manifest"]["coverage_end"], "2026-09-30");
    assert_eq!(resume["pairs"][0]["status"], "ok");
    assert_eq!(resume["pairs"][0]["rows_kept"], 10_000);
}

#[test]
fn preuve_s5_zip_dataset_backtest_reproduit_s3() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    zip_standard(&zip, "2026-09-30");
    assert_eq!(ingest(&root, &zip, &["XBTUSD"]).status.code(), Some(0));

    // Même dataset que la preuve S4, construit sur le fichier écrit par `ingest kraken`.
    let build = tv4(&[
        "dataset",
        "build",
        "--root",
        &root.display().to_string(),
        "--name",
        "fixture_2020_01",
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--from",
        "2020-01-01T00:00:00Z",
        "--to",
        "2020-01-09T00:00:00Z",
        "--gaps-policy",
        "skip",
    ]
    .map(String::from));
    assert_eq!(build.status.code(), Some(0), "{}", texte(&build).1);
    let manifeste = json(&root.join("datasets/fixture_2020_01.json"));
    assert_eq!(manifeste["dataset_hash"], DATASET_HASH_SKIP);
    assert_eq!(manifeste["row_count"], 10_000);

    let sortie_run = tmp.path().join("run");
    let run = tv4(&[
        "backtest".to_string(),
        "--root".to_string(),
        root.display().to_string(),
        "--dataset".to_string(),
        "datasets/fixture_2020_01.json".to_string(),
        "--strategy".to_string(),
        "sma_cross".to_string(),
        "--params".to_string(),
        repo_path("config/strategies/sma_cross_1m.toml")
            .display()
            .to_string(),
        "--fees-file".to_string(),
        repo_path("config/fees.toml").display().to_string(),
        "--instruments-file".to_string(),
        repo_path("config/instruments.toml").display().to_string(),
        "--out".to_string(),
        sortie_run.display().to_string(),
    ]);
    assert_eq!(run.status.code(), Some(0), "{}", texte(&run).1);
    let metrics = json(&sortie_run.join("metrics.json"));
    // Les valeurs de l'oracle S6 sont dans `preuve_s5_valeurs_figees` ; la preuve
    // d'égalité entre chemins de données reste active ci-dessous.
    assert!(metrics["n_trades"].as_u64().unwrap() > 0);
    assert_eq!(metrics["data"]["dataset_hash"], DATASET_HASH_SKIP);
    let direct = tmp.path().join("run_fixture_1m");
    let run = tv4(&[
        "backtest".to_string(),
        "--fixture".to_string(),
        repo_path("fixtures/kraken_xbtusd_1m.csv")
            .display()
            .to_string(),
        "--strategy".to_string(),
        "sma_cross".to_string(),
        "--params".to_string(),
        repo_path("config/strategies/sma_cross_1m.toml")
            .display()
            .to_string(),
        "--fees-file".to_string(),
        repo_path("config/fees.toml").display().to_string(),
        "--instruments-file".to_string(),
        repo_path("config/instruments.toml").display().to_string(),
        "--out".to_string(),
        direct.display().to_string(),
    ]);
    assert_eq!(run.status.code(), Some(0), "{}", texte(&run).1);
    let d = json(&direct.join("metrics.json"));
    for cle in [
        "n_trades",
        "pnl_net",
        "cash_final",
        "fees_total",
        "config_hash",
    ] {
        assert_eq!(metrics[cle], d[cle], "{cle}");
    }
    // trades.csv (S6) : identique à l'octet entre `ingest kraken` + dataset et `--fixture`.
    let csv_dataset = std::fs::read(sortie_run.join("trades.csv")).unwrap();
    let csv_direct = std::fs::read(direct.join("trades.csv")).unwrap();
    assert!(csv_direct.len() > 200);
    assert_eq!(csv_dataset, csv_direct);

    // Passe B : le même dataset rejoué en 15m (agrégation 1m vers 15m dans le moteur) donne les mêmes
    // résultats que `--fixture`, au bit.
    let params_15m = repo_path("config/strategies/sma_cross.toml");
    let fees = repo_path("config/fees.toml");
    let instruments = repo_path("config/instruments.toml");
    let depuis_dataset = tmp.path().join("run15_dataset");
    let run = tv4(&[
        "backtest".to_string(),
        "--root".to_string(),
        root.display().to_string(),
        "--dataset".to_string(),
        "datasets/fixture_2020_01.json".to_string(),
        "--strategy".to_string(),
        "sma_cross".to_string(),
        "--params".to_string(),
        params_15m.display().to_string(),
        "--fees-file".to_string(),
        fees.display().to_string(),
        "--instruments-file".to_string(),
        instruments.display().to_string(),
        "--out".to_string(),
        depuis_dataset.display().to_string(),
    ]);
    assert_eq!(run.status.code(), Some(0), "{}", texte(&run).1);
    let depuis_csv = tmp.path().join("run15_fixture");
    let run = tv4(&[
        "backtest".to_string(),
        "--fixture".to_string(),
        repo_path("fixtures/kraken_xbtusd_1m.csv")
            .display()
            .to_string(),
        "--strategy".to_string(),
        "sma_cross".to_string(),
        "--params".to_string(),
        params_15m.display().to_string(),
        "--fees-file".to_string(),
        fees.display().to_string(),
        "--instruments-file".to_string(),
        instruments.display().to_string(),
        "--out".to_string(),
        depuis_csv.display().to_string(),
    ]);
    assert_eq!(run.status.code(), Some(0), "{}", texte(&run).1);
    let a = json(&depuis_dataset.join("metrics.json"));
    let b = json(&depuis_csv.join("metrics.json"));
    for cle in [
        "n_trades",
        "pnl_net",
        "cash_final",
        "fees_total",
        "config_hash",
    ] {
        assert_eq!(a[cle], b[cle], "{cle}");
    }
    assert_eq!(a["window"], b["window"]);
}

/// Valeurs de l'oracle S6 du `--fixture` 1m (`sma_cross.fixture_1m_defauts` : 3 trades, pnl_net
/// -3.198931025058812, cash_final 296.8010689749412, config_hash 138bd048…) et 15m
/// (`sma_cross.fixture_15m_defauts` : 2 trades, pnl_net -4.517856616208519, cash_final
/// 295.4821433837915, config_hash de26831f…), recoupées au bit avec le moteur par la revue du 15/10.
/// Avant S6 : 23 trades, -15.408523707748486, 284.5914762922515, `3d45e889…` en 1m ; 10 trades,
/// -7.401437247087472, 292.5985627529125, `e85b28f7…` en 15m.
#[test]
fn preuve_s5_valeurs_figees() {
    let tmp = dossier();
    let lancer = |params: &str, nom: &str| {
        let out = tmp.path().join(nom);
        let run = tv4(&[
            "backtest".to_string(),
            "--fixture".to_string(),
            repo_path("fixtures/kraken_xbtusd_1m.csv")
                .display()
                .to_string(),
            "--strategy".to_string(),
            "sma_cross".to_string(),
            "--params".to_string(),
            repo_path(params).display().to_string(),
            "--fees-file".to_string(),
            repo_path("config/fees.toml").display().to_string(),
            "--instruments-file".to_string(),
            repo_path("config/instruments.toml").display().to_string(),
            "--out".to_string(),
            out.display().to_string(),
        ]);
        assert_eq!(run.status.code(), Some(0), "{}", texte(&run).1);
        json(&out.join("metrics.json"))
    };
    let m1 = lancer("config/strategies/sma_cross_1m.toml", "run1m");
    assert_eq!(m1["n_trades"], 3);
    assert_eq!(m1["pnl_net"].as_f64(), Some(-3.198931025058812));
    assert_eq!(m1["cash_final"].as_f64(), Some(296.8010689749412));
    assert_eq!(
        m1["config_hash"],
        "138bd048fe1c2f7551f389b1149b457ae3b2486ce6bbe2b6f426ceb036f88253"
    );
    let m15 = lancer("config/strategies/sma_cross.toml", "run15m");
    assert_eq!(m15["n_trades"], 2);
    assert_eq!(m15["pnl_net"].as_f64(), Some(-4.517856616208519));
    assert_eq!(m15["cash_final"].as_f64(), Some(295.4821433837915));
    assert_eq!(
        m15["config_hash"],
        "de26831fecaa5f438dddc7148219f81f40fa19537771b299340483a9062dc4b6"
    );
}

#[test]
fn sha256_faux_aucune_ecriture() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    zip_standard(&zip, "2026-09-30");
    let faux = "00".repeat(32);
    let sortie = tv4(&args_ingest(&root, &zip, &faux, &["XBTUSD"]));
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&faux), "{stderr}");
    assert!(stderr.contains(&sha256_fichier(&zip)), "{stderr}");
    assert!(!root.join("data").exists());
    assert!(!root.join("reports").exists());
}

#[test]
fn sha256_mal_forme_erreur_d_usage() {
    let tmp = dossier();
    let zip = tmp.path().join("dump.zip");
    zip_standard(&zip, "2026-09-30");
    for mauvais in ["abc", &"g".repeat(64), &"a".repeat(63)] {
        let sortie = tv4(&args_ingest(tmp.path(), &zip, mauvais, &["XBTUSD"]));
        assert_eq!(sortie.status.code(), Some(2), "{mauvais}");
    }
    assert!(!tmp.path().join("reports").exists());
}

#[test]
fn entree_absente_sans_confusion_de_suffixe() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    ecrire_zip(
        &zip,
        &[
            ("MANIFEST.json", &manifeste_synthetique("2026-09-30")),
            ("AIXBTUSD_1.csv", LIGNES_AIXBT_SYNTHETIQUES.as_bytes()),
        ],
    );
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("XBTUSD"), "{stderr}");
    assert!(stdout.contains("XBTUSD_1.csv"), "{stdout}");
    let rapport = json(&root.join(RAPPORT_BTC));
    assert_eq!(rapport["status"], "entry_missing");
    assert!(rapport["error"].as_str().unwrap().contains("XBTUSD_1.csv"));
    assert_eq!(rapport["continuity"], serde_json::Value::Null);
    // Rien d'écrit pour BTC-USD, et AIXBTUSD n'a pas été confondu avec XBTUSD.
    assert!(!root.join("data").exists());
}

#[test]
fn paire_bloquee_n_empeche_pas_les_autres() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    // Échantillon SYNTHÉTIQUE : deux lignes différentes pour le même horodatage.
    let eth = "\
1577836800,130.0,131.0,129.0,130.5,10.0,3
1577836860,130.5,131.5,130.0,131.0,12.0,4
1577836860,130.5,132.5,130.0,131.0,12.0,4
";
    ecrire_zip(
        &zip,
        &[
            ("MANIFEST.json", &manifeste_synthetique("2026-09-30")),
            ("XBTUSD_1.csv", texte_fixture_1m().as_bytes()),
            ("ETHUSD_1.csv", eth.as_bytes()),
        ],
    );
    let sortie = ingest(&root, &zip, &["XBTUSD", "ETHUSD"]);
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("ETHUSD"), "{stderr}");
    assert!(stdout.contains("BLOQUÉE (divergent_duplicate)"), "{stdout}");
    assert!(root.join(PARQUET_REL).is_file());
    assert!(!root.join("data/candles/kraken/ETH-USD").exists());
    let rapport = json(&root.join("reports/ingest/kraken/ETH-USD.json"));
    assert_eq!(rapport["status"], "blocked");
    assert_eq!(
        rapport["continuity"]["blocking"],
        serde_json::json!(["divergent_duplicate"])
    );
    let resume = json(&root.join(RESUME));
    assert_eq!(resume["pairs"][0]["status"], "ok");
    assert_eq!(resume["pairs"][1]["status"], "blocked");
}

/// Remplace le champ `champ` de la ligne `index` (à partir de 0) du texte de la fixture.
fn altere(texte: &str, index: usize, champ: usize, valeur: &str) -> String {
    texte
        .lines()
        .enumerate()
        .map(|(i, ligne)| {
            if i == index {
                let mut champs: Vec<&str> = ligne.split(',').collect();
                champs[champ] = valeur;
                champs.join(",")
            } else {
                ligne.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn chaque_cas_bloquant_rien_n_est_ecrit() {
    let fixture = texte_fixture_1m();
    let lignes: Vec<&str> = fixture.lines().collect();
    // Ligne 10 (index 9) : on choisit une ligne dont high > low, pour que les permutations de champs
    // aient un effet.
    let i = lignes
        .iter()
        .position(|l| {
            let c: Vec<&str> = l.split(',').collect();
            c[2].parse::<f64>().unwrap() > c[3].parse::<f64>().unwrap()
        })
        .expect("une ligne avec high > low");
    let champs: Vec<&str> = lignes[i].split(',').collect();
    let (high, low) = (champs[2], champs[3]);
    let ajout_divergent = format!(
        "{}{}\n",
        fixture,
        // Même horodatage que la première ligne, volume différent.
        {
            let mut c: Vec<&str> = lignes[0].split(',').collect();
            c[5] = "999.5";
            c.join(",")
        }
    );
    let sans_trades = fixture.replacen(lignes[i], lignes[i].rsplit_once(',').unwrap().0, 1);
    let horodatage_decale = {
        let ts: i64 = champs[0].parse().unwrap();
        altere(&fixture, i, 0, &(ts + 30).to_string())
    };
    let cas: Vec<(&str, String, &str)> = vec![
        ("doublon divergent", ajout_divergent, "divergent_duplicate"),
        (
            "low > high",
            altere(&altere(&fixture, i, 2, low), i, 3, high),
            "ohlc_error",
        ),
        (
            "open hors de [low, high]",
            altere(
                &fixture,
                i,
                1,
                &format!("{}", high.parse::<f64>().unwrap() + 1.0),
            ),
            "ohlc_error",
        ),
        ("prix 0", altere(&fixture, i, 1, "0"), "ohlc_error"),
        ("volume -1", altere(&fixture, i, 5, "-1"), "ohlc_error"),
        ("nan", altere(&fixture, i, 4, "nan"), "non_finite"),
        ("inf", altere(&fixture, i, 4, "inf"), "non_finite"),
        ("horodatage +30 s", horodatage_decale, "misaligned"),
        ("ligne à 6 champs", sans_trades, "format_error"),
    ];
    for (nom, contenu, motif) in cas {
        let tmp = dossier();
        let root = tmp.path().join("racine");
        let zip = tmp.path().join("dump.zip");
        zip_avec_xbtusd(&zip, &contenu);
        let sortie = ingest(&root, &zip, &["XBTUSD"]);
        let (stdout, stderr) = texte(&sortie);
        assert_eq!(sortie.status.code(), Some(1), "{nom} : {stderr}");
        assert!(stdout.contains("BLOQUÉE"), "{nom} : {stdout}");
        let rapport = json(&root.join(RAPPORT_BTC));
        assert_eq!(rapport["status"], "blocked", "{nom}");
        let blocage = rapport["continuity"]["blocking"].as_array().unwrap();
        assert!(
            blocage.iter().any(|b| b == motif),
            "{nom} : {motif} absent de {blocage:?}"
        );
        assert!(
            !root.join("data").exists(),
            "{nom} : rien ne doit être écrit"
        );
        assert!(rapport["quality"].is_null(), "{nom}");
    }
}

#[test]
fn cas_non_bloquants_meme_contenu() {
    let fixture = texte_fixture_1m();
    let mut lignes: Vec<String> = fixture.lines().map(String::from).collect();
    // Une ligne dupliquée à l'identique, deux lignes permutées.
    let doublon = lignes[99].clone();
    lignes.insert(100, doublon);
    lignes.swap(200, 201);
    let contenu = lignes.join("\n") + "\n";
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    zip_avec_xbtusd(&zip, &contenu);
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let rapport = json(&root.join(RAPPORT_BTC));
    let stats = &rapport["continuity"]["stats"];
    assert_eq!(stats["rows_read"], 10_001);
    assert_eq!(stats["duplicates"], 1);
    assert!(stats["out_of_order"].as_u64().unwrap() >= 1);
    assert_eq!(stats["rows_kept"], 10_000);
    // Le contenu écrit est celui de la fixture.
    assert_eq!(rapport["files"][0]["content_hash"], HASH_FIXTURE_MOIS);
    assert_eq!(
        json(&root.join(MANIFEST_REL))["continuity"]["duplicates"],
        1
    );
}

#[test]
fn couverture_bornee_non_cloturees_et_mois_non_clos() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    // Couverture inclusive jusqu'au 07/01 : fin exclusive 2020-01-08T00:00:00Z.
    zip_standard(&zip, "2020-01-07");
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let rapport = json(&root.join(RAPPORT_BTC));
    assert_eq!(rapport["end_of_coverage"], "2020-01-08T00:00:00Z");
    assert_eq!(rapport["continuity"]["stats"]["rejected_unclosed"], 1_263);
    assert_eq!(rapport["continuity"]["stats"]["rows_kept"], 8_737);
    assert_eq!(rapport["files"][0]["closed"], false);
    let manifeste = json(&root.join(MANIFEST_REL));
    assert_eq!(manifeste["closed"], false);
    assert_eq!(manifeste["row_count"], 8_737);
    assert_eq!(manifeste["continuity"]["rejected_unclosed"], 1_263);
}

#[test]
fn manifeste_absent_ou_illisible_tolere() {
    let tmp = dossier();
    // Sans MANIFEST.json.
    let root = tmp.path().join("sans");
    let zip = tmp.path().join("sans.zip");
    ecrire_zip(&zip, &[("XBTUSD_1.csv", texte_fixture_1m().as_bytes())]);
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("avertissement : MANIFEST.json absent"),
        "{stdout}"
    );
    assert!(root.join(PARQUET_REL).is_file());
    assert_eq!(
        json(&root.join(RESUME))["manifest"],
        serde_json::Value::Null
    );

    // Avec un JSON invalide.
    let root = tmp.path().join("illisible");
    let zip = tmp.path().join("illisible.zip");
    ecrire_zip(
        &zip,
        &[
            ("MANIFEST.json", b"{ pas du json"),
            ("XBTUSD_1.csv", texte_fixture_1m().as_bytes()),
        ],
    );
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    let (stdout, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("avertissement : MANIFEST.json illisible"),
        "{stdout}"
    );
    let resume = json(&root.join(RESUME));
    assert!(resume["manifest"]["parse_error"].is_string());
    assert!(root.join(PARQUET_REL).is_file());
}

#[test]
fn has_header_vrai_refuse() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    let manifeste = String::from_utf8(manifeste_synthetique("2026-09-30"))
        .unwrap()
        .replace("\"has_header\":false", "\"has_header\":true");
    ecrire_zip(
        &zip,
        &[
            ("MANIFEST.json", manifeste.as_bytes()),
            ("XBTUSD_1.csv", texte_fixture_1m().as_bytes()),
        ],
    );
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("has_header"), "{stderr}");
    assert!(!root.join("data").exists());
    assert!(!root.join("reports").exists());
}

#[test]
fn paire_en_double_refusee() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    zip_standard(&zip, "2026-09-30");
    let sortie = ingest(&root, &zip, &["XBTUSD", "XBTUSD"]);
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1));
    assert!(stderr.contains("demandée deux fois"), "{stderr}");
    assert!(!root.join("data").exists());
    assert!(!root.join("reports").exists());
}

#[test]
fn reingestion_inchangee() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    zip_standard(&zip, "2026-09-30");
    assert_eq!(ingest(&root, &zip, &["XBTUSD"]).status.code(), Some(0));
    let avant = std::fs::read(root.join(MANIFEST_REL)).unwrap();
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let rapport = json(&root.join(RAPPORT_BTC));
    let fichiers = rapport["files"].as_array().unwrap();
    assert!(!fichiers.is_empty());
    assert!(
        fichiers.iter().all(|f| f["status"] == "unchanged"),
        "{fichiers:?}"
    );
    // Le manifeste (et son `ingested_at`) n'a pas bougé.
    assert_eq!(std::fs::read(root.join(MANIFEST_REL)).unwrap(), avant);
    assert!(
        texte(&sortie)
            .0
            .contains("0 créés, 0 réécrits, 1 inchangés")
    );
}

#[test]
fn kraken_apres_csv_meme_contenu_ajoute_le_bloc() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip = tmp.path().join("dump.zip");
    zip_standard(&zip, "2026-09-30");
    // 1. `ingest csv` de la fixture : pas de bloc `continuity`.
    let csv = tv4(&[
        "ingest",
        "csv",
        "--csv",
        &repo_path("fixtures/kraken_xbtusd_1m.csv")
            .display()
            .to_string(),
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--root",
        &root.display().to_string(),
    ]
    .map(String::from));
    assert_eq!(csv.status.code(), Some(0), "{}", texte(&csv).1);
    let avant = json(&root.join(MANIFEST_REL));
    assert!(avant.get("continuity").is_none());
    assert_eq!(avant["source"], "kraken-csv");
    // 2. `ingest kraken` sur la même racine : le bloc est ajouté, le contenu est le même.
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie).1);
    let rapport = json(&root.join(RAPPORT_BTC));
    assert_eq!(rapport["files"][0]["status"], "rewritten");
    let apres = json(&root.join(MANIFEST_REL));
    assert_eq!(apres["continuity"]["gaps"], 951);
    assert_eq!(apres["content_hash"], avant["content_hash"]);
    // 3. `ingest csv` de nouveau : le manifeste reste intact.
    let octets = std::fs::read(root.join(MANIFEST_REL)).unwrap();
    let encore = tv4(&[
        "ingest",
        "csv",
        "--csv",
        &repo_path("fixtures/kraken_xbtusd_1m.csv")
            .display()
            .to_string(),
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--root",
        &root.display().to_string(),
    ]
    .map(String::from));
    assert_eq!(encore.status.code(), Some(0));
    assert_eq!(std::fs::read(root.join(MANIFEST_REL)).unwrap(), octets);
}

#[test]
fn zip_corrompu_bloque_la_paire() {
    let tmp = dossier();
    let root = tmp.path().join("racine");
    let zip: PathBuf = tmp.path().join("dump.zip");
    zip_avec_xbtusd(&zip, &texte_fixture_1m());
    // Un octet de l'entrée compressée est inversé (le manifeste, petit, est au début ; le milieu du
    // fichier est dans les données de XBTUSD_1.csv). Le SHA-256 passé est celui du zip corrompu : la
    // vérification d'intégrité du zip passe, c'est la lecture de l'entrée qui échoue (deflate ou CRC).
    let mut octets = std::fs::read(&zip).unwrap();
    let milieu = octets.len() / 2;
    octets[milieu] ^= 0xFF;
    std::fs::write(&zip, &octets).unwrap();
    let sortie = ingest(&root, &zip, &["XBTUSD"]);
    let (_, stderr) = texte(&sortie);
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    let rapport = json(&root.join(RAPPORT_BTC));
    assert_eq!(rapport["status"], "blocked");
    assert!(rapport["continuity"]["format_error"].is_string());
    assert!(
        rapport["continuity"]["blocking"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "format_error")
    );
    assert!(!root.join("data").exists());
}
