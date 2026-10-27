//! Golden files de run (S8, `docs/specs/s8.md` §13) : deux backtests sur la fixture, avec les frais OKX et
//! les défauts de la CLI, doivent redonner les trois fichiers versionnés dans `tests/golden_runs/`.
//!
//! * `sma_cross_15m_okx/` : le backtest de fumée (`config/strategies/sma_cross.toml`), 2 trades,
//!   verdict `rejected_too_few_trades` ;
//! * `sma_cross_1m_court_okx/` : `sma_cross` 1m 5/20 (`params.toml` versionné dans le dossier), 49
//!   trades, verdict `rejected_negative_expectancy`. Il couvre ce que le premier ne voit pas : un
//!   verdict autre que `rejected_too_few_trades`, `pnl_without_best` pour n = 3, 5 et 10, un
//!   `histogram_r` garni. `return_annualized` y reste `null` : la fixture dure moins de 30 jours, et la
//!   valeur est couverte par la table de référence de `report/approx.rs`.
//!
//! Le double run de la CI ne voit que les écarts entre deux exécutions du même binaire ; ce golden voit
//! la dérive entre commits (docs/03-architecture.md §7.4).
//!
//! * `trades.csv` et `equity.csv` : égalité à l'octet.
//! * `metrics.json` : la chaîne canonique privée du bloc `approx` est égale à l'octet ; chaque valeur
//!   de `approx` (seule à passer par la libm) est à `approx::ULP_TOLERANCE` ULP au plus, et
//!   `ulp_tolerance` y est égale à cette constante. La relecture JSON n'est pas utilisée pour les
//!   flottants (sans `float_roundtrip`, `serde_json` peut les relire à un ULP).
//! * pour le run de fumée, la projection S6 de `trades.csv` redonne le BLAKE3 de l'oracle, pour que le
//!   golden ne dérive pas en silence sur les colonnes S6.
//!
//! Mise à jour : `TV4_GOLDEN_UPDATE=1 cargo test --test golden_run` réécrit les fichiers (refusée si la
//! variable `CI` est définie). Relire le diff avant de commiter, comme pour les `.snap`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tv4::model::hash;
use tv4::report::approx::ULP_TOLERANCE;
use tv4::report::writers::project_s6;

/// BLAKE3 de la projection S6 de `trades.csv` pour `sma_cross.toml` : valeur de l'oracle S6.
const ORACLE_S6_TRADES_BLAKE3: &str =
    "e26a9b934782945495ab36aa5e38b755573027c60312d0a0526d4ff6fe9e9cc4";
/// Fichiers déterministes d'un run (le manifeste ne l'est pas).
const GOLDEN_FILES: [&str; 3] = ["metrics.json", "trades.csv", "equity.csv"];
/// Dossier du golden de fumée.
const SMOKE_DIR: &str = "sma_cross_15m_okx";
/// Dossier du second golden (≥ 30 trades) ; il contient aussi son `params.toml`.
const SHORT_DIR: &str = "sma_cross_1m_court_okx";

fn repo(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

fn golden_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_runs")
        .join(name)
}

/// Lance le backtest de la fixture avec le fichier de paramètres `params`, dans `out`.
fn golden_run(params: &Path, out: &Path) {
    let sortie = Command::new(env!("CARGO_BIN_EXE_tv4"))
        .arg("backtest")
        .arg("--fixture")
        .arg(repo("fixtures/kraken_xbtusd_1m.csv"))
        .args(["--strategy", "sma_cross", "--params"])
        .arg(params)
        .args(["--fees", "okx", "--fees-file"])
        .arg(repo("config/fees.toml"))
        .arg("--instruments-file")
        .arg(repo("config/instruments.toml"))
        .arg("--out")
        .arg(out)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé");
    assert_eq!(
        sortie.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&sortie.stderr)
    );
}

/// Sépare `metrics.json` en (texte sans le bloc `approx`, texte du bloc `approx`).
///
/// Le fichier est du JSON canonique compact (clés triées) : `"approx":{...}` est un objet plat sans
/// objet imbriqué, suivi d'une virgule ou de l'accolade finale.
fn split_approx(metrics: &str) -> (String, String) {
    const KEY: &str = "\"approx\":{";
    let start = metrics
        .find(KEY)
        .expect("bloc approx absent de metrics.json");
    let body_start = start + KEY.len();
    let body_len = metrics[body_start..]
        .find('}')
        .expect("bloc approx non fermé");
    let end = body_start + body_len + 1;
    let approx = metrics[body_start..body_start + body_len].to_string();
    // Retire aussi la virgule qui sépare le bloc de son voisin.
    let mut rest = format!("{}{}", &metrics[..start], &metrics[end..]);
    if let Some(position) = rest.find(",,") {
        rest.replace_range(position..position + 1, "");
    } else if rest.contains(",}") {
        rest = rest.replace(",}", "}");
    } else if rest.contains("{,") {
        rest = rest.replace("{,", "{");
    }
    (rest, approx)
}

/// Valeurs du bloc `approx` : (clé, nombre ou `None` pour `null`).
fn approx_values(body: &str) -> Vec<(String, Option<f64>)> {
    body.split(',')
        .map(|pair| {
            let (key, value) = pair.split_once(':').expect("paire clé:valeur attendue");
            let value = match value {
                "null" => None,
                number => Some(number.parse::<f64>().expect("nombre dans approx")),
            };
            (key.trim_matches('"').to_string(), value)
        })
        .collect()
}

/// Distance en ULP entre deux flottants finis de même signe.
fn ulp_distance(a: f64, b: f64) -> u64 {
    assert!(
        a.is_finite() && b.is_finite(),
        "valeurs non finies : {a} {b}"
    );
    if a == b {
        return 0;
    }
    assert_eq!(
        a.is_sign_negative(),
        b.is_sign_negative(),
        "signes opposés : {a} {b}"
    );
    a.to_bits().abs_diff(b.to_bits())
}

/// Relance le run avec `params`, puis le compare au golden `name` (ou le réécrit avec
/// `TV4_GOLDEN_UPDATE`). Retourne le dossier du golden.
fn compare_golden(name: &str, params: &Path) -> PathBuf {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    golden_run(params, &out);
    let golden = golden_dir(name);

    if std::env::var_os("TV4_GOLDEN_UPDATE").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "TV4_GOLDEN_UPDATE est refusée en CI : mettre le golden à jour en local et relire le diff"
        );
        fs::create_dir_all(&golden).unwrap();
        for file in GOLDEN_FILES {
            fs::copy(out.join(file), golden.join(file)).unwrap();
        }
        return golden;
    }

    for file in ["trades.csv", "equity.csv"] {
        let attendu = fs::read(golden.join(file)).expect("golden absent : TV4_GOLDEN_UPDATE=1");
        let obtenu = fs::read(out.join(file)).unwrap();
        assert!(
            attendu == obtenu,
            "{file} diffère du golden ({} contre {} octets) ; si le changement est voulu : \
             TV4_GOLDEN_UPDATE=1 cargo test --test golden_run, puis relire le diff",
            attendu.len(),
            obtenu.len()
        );
    }

    let attendu = fs::read_to_string(golden.join("metrics.json")).expect("golden absent");
    let obtenu = fs::read_to_string(out.join("metrics.json")).unwrap();
    let (attendu_sans, attendu_approx) = split_approx(&attendu);
    let (obtenu_sans, obtenu_approx) = split_approx(&obtenu);
    assert!(
        attendu_sans == obtenu_sans,
        "metrics.json (hors approx) diffère du golden ; si le changement est voulu : \
         TV4_GOLDEN_UPDATE=1 cargo test --test golden_run, puis relire le diff"
    );
    let (golden_values, run_values) = (
        approx_values(&attendu_approx),
        approx_values(&obtenu_approx),
    );
    assert_eq!(
        golden_values.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        run_values.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        "clés de approx différentes"
    );
    for ((key, golden_value), (_, run_value)) in golden_values.iter().zip(&run_values) {
        if key == "ulp_tolerance" {
            // La tolérance annoncée est celle du code, pas une valeur à comparer en ULP.
            let annoncee = Some(f64::from(ULP_TOLERANCE));
            assert_eq!(
                (*golden_value, *run_value),
                (annoncee, annoncee),
                "approx.ulp_tolerance diffère de approx::ULP_TOLERANCE ; si le changement est voulu : \
                 TV4_GOLDEN_UPDATE=1 cargo test --test golden_run, puis relire le diff"
            );
            continue;
        }
        match (golden_value, run_value) {
            (None, None) => {}
            (Some(g), Some(r)) => assert!(
                ulp_distance(*g, *r) <= u64::from(ULP_TOLERANCE),
                "approx.{key} : {g} contre {r}, plus de {ULP_TOLERANCE} ULP d'écart"
            ),
            _ => panic!(
                "approx.{key} : nul d'un côté seulement ({golden_value:?} contre {run_value:?})"
            ),
        }
    }

    golden
}

#[test]
fn le_run_de_fumee_redonne_le_golden() {
    let golden = compare_golden(SMOKE_DIR, &repo("config/strategies/sma_cross.toml"));
    if std::env::var_os("TV4_GOLDEN_UPDATE").is_some() {
        return;
    }
    // Les colonnes S6 ne dérivent pas en silence : projection = valeur de l'oracle.
    let projete = project_s6(&fs::read(golden.join("trades.csv")).unwrap()).unwrap();
    assert_eq!(hash::blake3_hex(&projete), ORACLE_S6_TRADES_BLAKE3);
}

#[test]
fn le_run_court_a_trente_trades_redonne_le_golden() {
    compare_golden(SHORT_DIR, &golden_dir(SHORT_DIR).join("params.toml"));
}

/// Noms des fichiers d'un dossier golden, triés.
fn noms_du_dossier(golden: &Path) -> Vec<String> {
    let mut noms: Vec<String> = fs::read_dir(golden)
        .expect("dossier golden absent")
        .map(|entree| entree.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    noms.sort();
    noms
}

#[test]
fn les_goldens_sont_complets_et_sans_manifeste() {
    assert_eq!(
        noms_du_dossier(&golden_dir(SMOKE_DIR)),
        ["equity.csv", "metrics.json", "trades.csv"]
    );
    assert_eq!(
        noms_du_dossier(&golden_dir(SHORT_DIR)),
        ["equity.csv", "metrics.json", "params.toml", "trades.csv"]
    );
}

/// Le second golden doit couvrir ce que le premier ne voit pas ; si une mise à jour le vide (moins de
/// 30 trades, verdict `rejected_too_few_trades`, champs `null`), ce test échoue.
#[test]
fn le_second_golden_couvre_les_champs_du_verdict_et_de_la_distribution() {
    let texte = fs::read_to_string(golden_dir(SHORT_DIR).join("metrics.json")).unwrap();
    let m: serde_json::Value = serde_json::from_str(&texte).unwrap();
    assert!(m["n_trades"].as_u64().unwrap() >= 30, "moins de 30 trades");
    assert_ne!(m["verdict"], "rejected_too_few_trades");
    assert_eq!(m["verdict"], "rejected_negative_expectancy");
    let distribution = &m["distribution"];
    let sans_meilleurs = distribution["pnl_without_best"].as_array().unwrap();
    let ns: Vec<u64> = sans_meilleurs
        .iter()
        .map(|e| e["n"].as_u64().unwrap())
        .collect();
    assert_eq!(ns, [1, 3, 5, 10]);
    for entree in sans_meilleurs {
        assert!(!entree["pnl_net"].is_null(), "{entree}");
    }
    let histogramme = &distribution["histogram_r"];
    let comptes: u64 = histogramme["counts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_u64().unwrap())
        .sum();
    let (sous, sur) = (
        histogramme["underflow"].as_u64().unwrap(),
        histogramme["overflow"].as_u64().unwrap(),
    );
    assert_eq!(comptes + sous + sur, m["trades_with_r"].as_u64().unwrap());
    assert!(comptes >= 20, "histogramme peu garni : {comptes}");
    assert_eq!(m["approx"]["ulp_tolerance"], ULP_TOLERANCE);
    assert!(
        m["approx"]["return_annualized"].is_null(),
        "la fixture dure moins de 30 jours"
    );
}

#[test]
fn le_decoupage_de_approx_est_exact() {
    let metrics =
        r#"{"a":1,"approx":{"ln_return":-0.5,"return_annualized":null,"ulp_tolerance":1},"b":2}"#;
    let (sans, approx) = split_approx(metrics);
    assert_eq!(sans, r#"{"a":1,"b":2}"#);
    assert_eq!(
        approx_values(&approx),
        [
            ("ln_return".to_string(), Some(-0.5)),
            ("return_annualized".to_string(), None),
            ("ulp_tolerance".to_string(), Some(1.0)),
        ]
    );
    // En tête et en fin d'objet.
    assert_eq!(split_approx(r#"{"approx":{"x":1},"b":2}"#).0, r#"{"b":2}"#);
    assert_eq!(split_approx(r#"{"a":1,"approx":{"x":1}}"#).0, r#"{"a":1}"#);
}

#[test]
fn la_distance_en_ulp_compte_les_flottants_voisins() {
    let x = 1.5_f64;
    let suivant = f64::from_bits(x.to_bits() + 1);
    assert_eq!(ulp_distance(x, x), 0);
    assert_eq!(ulp_distance(x, suivant), 1);
    assert_eq!(ulp_distance(suivant, x), 1);
    assert_eq!(ulp_distance(-x, -f64::from_bits(x.to_bits() + 3)), 3);
}
