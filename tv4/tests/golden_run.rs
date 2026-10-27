//! Golden file de run (S8, spécification §13) : le backtest de fumée (fixture, `sma_cross.toml`, frais
//! OKX, défauts de la CLI) doit redonner les trois fichiers versionnés dans `tests/golden_runs/`.
//!
//! Le double run de la CI ne voit que les écarts entre deux exécutions du même binaire ; ce golden voit
//! la dérive entre commits (docs/03-architecture.md §7.4).
//!
//! * `trades.csv` et `equity.csv` : égalité à l'octet.
//! * `metrics.json` : la chaîne canonique privée du bloc `approx` est égale à l'octet ; chaque valeur
//!   de `approx` (seule à passer par la libm) est à 1 ULP au plus. La relecture JSON n'est pas utilisée
//!   pour les flottants (sans `float_roundtrip`, `serde_json` peut les relire à un ULP).
//! * la projection S6 de `trades.csv` redonne le BLAKE3 de l'oracle, pour que le golden ne dérive pas en
//!   silence sur les colonnes S6.
//!
//! Mise à jour : `TV4_GOLDEN_UPDATE=1 cargo test --test golden_run` réécrit les fichiers (refusée si la
//! variable `CI` est définie). Relire le diff avant de commiter, comme pour les `.snap`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tv4::model::hash;
use tv4::report::writers::project_s6;

/// BLAKE3 de la projection S6 de `trades.csv` pour `sma_cross.toml` : valeur de l'oracle S6.
const ORACLE_S6_TRADES_BLAKE3: &str =
    "e26a9b934782945495ab36aa5e38b755573027c60312d0a0526d4ff6fe9e9cc4";
/// Fichiers déterministes d'un run (le manifeste ne l'est pas).
const GOLDEN_FILES: [&str; 3] = ["metrics.json", "trades.csv", "equity.csv"];
/// Écart maximal admis sur une valeur de `approx`, en ULP (`approx.ulp_tolerance`).
const ULP_TOLERANCE: u64 = 1;

fn repo(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_runs")
        .join("sma_cross_15m_okx")
}

/// Lance le backtest de fumée dans `out`.
fn smoke_run(out: &Path) {
    let sortie = Command::new(env!("CARGO_BIN_EXE_tv4"))
        .arg("backtest")
        .arg("--fixture")
        .arg(repo("fixtures/kraken_xbtusd_1m.csv"))
        .args(["--strategy", "sma_cross", "--params"])
        .arg(repo("config/strategies/sma_cross.toml"))
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

#[test]
fn le_run_de_fumee_redonne_le_golden() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    smoke_run(&out);
    let golden = golden_dir();

    if std::env::var_os("TV4_GOLDEN_UPDATE").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "TV4_GOLDEN_UPDATE est refusée en CI : mettre le golden à jour en local et relire le diff"
        );
        fs::create_dir_all(&golden).unwrap();
        for file in GOLDEN_FILES {
            fs::copy(out.join(file), golden.join(file)).unwrap();
        }
        return;
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
        match (golden_value, run_value) {
            (None, None) => {}
            (Some(g), Some(r)) => assert!(
                ulp_distance(*g, *r) <= ULP_TOLERANCE,
                "approx.{key} : {g} contre {r}, plus de {ULP_TOLERANCE} ULP d'écart"
            ),
            _ => panic!(
                "approx.{key} : nul d'un côté seulement ({golden_value:?} contre {run_value:?})"
            ),
        }
    }

    // Les colonnes S6 ne dérivent pas en silence : projection = valeur de l'oracle.
    let projete = project_s6(&fs::read(golden.join("trades.csv")).unwrap()).unwrap();
    assert_eq!(hash::blake3_hex(&projete), ORACLE_S6_TRADES_BLAKE3);
}

#[test]
fn le_golden_est_complet_et_sans_manifeste() {
    let golden = golden_dir();
    let mut noms: Vec<String> = fs::read_dir(&golden)
        .expect("dossier golden absent")
        .map(|entree| entree.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    noms.sort();
    assert_eq!(noms, ["equity.csv", "metrics.json", "trades.csv"]);
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
