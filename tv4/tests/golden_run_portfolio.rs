//! Golden file de run du portefeuille (S9, lot F, docs/specs/s9.md §18) : un backtest multi-paires sur un
//! dataset synthétique (deux jambes, BTC-USD et ETH-USD) doit redonner les fichiers versionnés dans
//! `tests/golden_runs/portfolio_2legs/`.
//!
//! * `params.toml` : paramètres de `sma_cross` du golden (versionnés avec lui) ;
//! * `metrics.json`, `trades.csv`, `equity.csv` : le résultat. Mêmes règles de comparaison que
//!   `golden_run.rs` : octets pour les deux CSV, chaîne canonique de `metrics.json` privée de `approx`
//!   à l'octet, `approx` à `approx::ULP_TOLERANCE` ULP.
//!
//! Le dataset est construit à chaque exécution par `common::synthetic_candles` (graines 11 et 22,
//! volumes multipliés par 2000 et 150, trous sur ETH) puis stocké dans un dossier temporaire : son
//! `dataset_hash` est donc une valeur du golden. Modifier `synthetic_candles` ou la construction du
//! dataset change le golden. Le fichier d'instruments OKX est `tests/fixtures/okx_instruments_spot.json`
//! (BTC-USDC et ETH-USDC `live`).
//!
//! Le golden doit couvrir une paire exécutable et une paire rejetée par G0, des refus de volume, de
//! `max_positions`, et plus de 30 trades : `le_golden_couvre_...` échoue si une mise à jour le vide.
//!
//! Mise à jour : `TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_portfolio` réécrit les fichiers
//! (refusée si la variable `CI` est définie). Relire le diff avant de commiter, comme pour les `.snap`.

mod common;
mod report_s9_common;

use std::fs;
use std::path::{Path, PathBuf};

use report_s9_common::{build_dataset, candles, repo, strings, tv4};
use tv4::report::approx::ULP_TOLERANCE;

/// Fichiers déterministes d'un run (le manifeste ne l'est pas).
const GOLDEN_FILES: [&str; 3] = ["metrics.json", "trades.csv", "equity.csv"];
/// Dossier du golden.
const GOLDEN: &str = "portfolio_2legs";
const MINUTES: usize = 30_000;

fn golden_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_runs")
        .join(name)
}

/// Construit le dataset du golden dans `root` et lance `tv4 backtest --symbols ...` dans `out`.
fn golden_run(params: &Path, root: &Path, out: &Path) {
    build_dataset(
        root,
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
    let okx = format!(
        "{}/tests/fixtures/okx_instruments_spot.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut args = strings(&[
        "backtest",
        "--dataset",
        "duo",
        "--strategy",
        "sma_cross",
        "--symbols",
        "BTC-USD,ETH-USD",
        "--max-positions",
        "1",
        "--sizing",
        "compounding",
        "--instruments-okx",
        &okx,
    ]);
    args.extend([
        "--root".to_string(),
        root.display().to_string(),
        "--params".to_string(),
        params.display().to_string(),
        "--fees".to_string(),
        "okx".to_string(),
        "--fees-file".to_string(),
        repo("config/fees.toml").display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ]);
    let sortie = tv4(&args);
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
    golden_run(params, dossier.path(), &out);
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
fn le_run_du_portefeuille_redonne_le_golden() {
    compare_golden(GOLDEN, &golden_dir(GOLDEN).join("params.toml"));
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
fn le_golden_est_complet_et_sans_manifeste() {
    assert_eq!(
        noms_du_dossier(&golden_dir(GOLDEN)),
        ["equity.csv", "metrics.json", "params.toml", "trades.csv"]
    );
}

/// Le golden doit couvrir ce qui fait la différence avec S8 ; si une mise à jour le vide, ce test échoue.
#[test]
fn le_golden_couvre_les_deux_jambes_g0_et_les_refus() {
    let texte = fs::read_to_string(golden_dir(GOLDEN).join("metrics.json")).unwrap();
    let m: serde_json::Value = serde_json::from_str(&texte).unwrap();
    assert_eq!(m["schema"], "tv4-metrics-s9");
    assert!(m["n_trades"].as_u64().unwrap() >= 30, "moins de 30 trades");
    assert_eq!(m["approx"]["ulp_tolerance"], ULP_TOLERANCE);
    let verdicts: Vec<&str> = m["executability"]["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(verdicts.len(), 2);
    assert!(verdicts.contains(&"executable"), "{verdicts:?}");
    assert!(
        verdicts.iter().any(|v| v.starts_with("rejected_")),
        "{verdicts:?}"
    );
    let pairs = m["funnel_by_pair"].as_array().unwrap();
    for pair in pairs {
        assert!(pair["trades"].as_u64().unwrap() > 0, "{pair}");
    }
    let rejected = |key: &str| -> u64 {
        pairs
            .iter()
            .map(|p| p["rejected"][key].as_u64().unwrap())
            .sum()
    };
    assert!(rejected("volume") > 0, "aucun refus de volume");
    assert!(rejected("max_positions") > 0, "aucun refus max_positions");
    assert!(m["portfolio"]["positions_cap_hits"].as_u64().unwrap() > 0);
    assert_eq!(m["g1_executable"]["at_reference_capital"], true);
    assert_eq!(m["instruments"]["source"], "okx");
}
