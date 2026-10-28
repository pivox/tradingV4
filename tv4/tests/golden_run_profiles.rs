//! Golden file des trois profils de coûts (S10, lot E, docs/specs/s10.md §16) : un backtest à deux jambes
//! (BTC-USD et ETH-USD, dataset synthétique du golden `portfolio_2legs`) joué en baseline, adverse et maker
//! doit redonner les fichiers versionnés dans `tests/golden_runs/portfolio_profiles/`.
//!
//! * `params.toml` : paramètres de `sma_cross` du golden (versionnés avec lui) ;
//! * `costs.toml` : **copie figée** des défauts de `config/costs.toml`. Le golden ne lit jamais
//!   `config/costs.toml` : y inscrire le stress calibré (docs/specs/s10.md §11.3) ne le change pas ;
//! * `baseline/`, `adverse/`, `maker/` : `metrics.json`, `trades.csv`, `equity.csv` de chaque profil, sans
//!   manifeste (il n'est pas déterministe), comme `portfolio_2legs` ;
//! * `profiles.json` : le résumé. Il ne contient ni horodatage ni durée, donc il se compare à l'octet, et
//!   son `profiles_id` est épinglé par le golden (le `run_id` n'a pas de commit dans sa formule).
//!
//! La CLI de S10 (lot F) n'existe pas encore à ce stade : le test pilote la bibliothèque comme elle le
//! fera (`load_dataset_multi`, `run_portfolio_profiles`, `render_profile`, `ProfilesReport`). Même règle
//! de comparaison que `golden_run_portfolio.rs` : octets pour les CSV et `profiles.json`, chaîne canonique
//! de `metrics.json` privée de `approx` à l'octet, `approx` à `approx::ULP_TOLERANCE` ULP.
//!
//! Le dataset est construit à chaque exécution par `common::synthetic_candles` (graines 11 et 22, volumes
//! multipliés par 2000 et 150, trous sur ETH) : son `dataset_hash` est donc une valeur du golden. Les règles
//! d'instrument sont celles du dépôt de test (tick 0,1, lot 1e-8, aucun minimum), sans fichier OKX.
//!
//! Mise à jour : `TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_profiles` réécrit les fichiers (refusée
//! si la variable `CI` est définie). Relire le diff avant de commiter, comme pour les `.snap`.

mod common;
mod portfolio_common;
mod profiles_common;
mod report_s9_common;

use std::fs;
use std::path::{Path, PathBuf};

use report_s9_common::{build_dataset, candles, repo};
use tv4::engine::{
    CostSetup, DEFAULT_MAX_POSITIONS, DEFAULT_QUOTE_GAP_MS, NoopObserver, PortfolioConfig,
    ProfilesInputs, run_portfolio_profiles,
};
use tv4::model::costs::CostsFile;
use tv4::model::{ExchangeId, FeeSchedule, InstrumentRules, Slippage, Timeframe};
use tv4::report::approx::ULP_TOLERANCE;
use tv4::report::metrics_s9::{
    CostsInputs, DatasetInfo, InstrumentsInfo, MetricsS9Inputs, SeriesInfo,
};
use tv4::report::profiles::{RenderedProfile, render_profile};
use tv4::sim::{SimRules, SizingMode};
use tv4::store;
use tv4::strategy::{Strategy, registry};

/// Dossier du golden.
const GOLDEN: &str = "portfolio_profiles";
const MINUTES: usize = 30_000;
const PROFILES: [&str; 3] = ["baseline", "adverse", "maker"];
const RUN_FILES: [&str; 3] = ["metrics.json", "trades.csv", "equity.csv"];
const UNIVERSE: [&str; 2] = ["BTC-USD", "ETH-USD"];

fn golden_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_runs")
        .join(name)
}

fn sma_cross(params: &toml::Value) -> Box<dyn Strategy> {
    registry::build("sma_cross", params).expect("stratégie valide")
}

/// Joue les trois profils du golden et écrit leurs fichiers, puis `profiles.json`, dans `out`.
fn golden_run(root: &Path, out: &Path) {
    let manifest_path = build_dataset(
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
    let loaded = store::load_dataset_multi(root, &manifest_path).expect("dataset multi-séries");
    let dataset = DatasetInfo {
        name: loaded.manifest.name.clone(),
        exchange: loaded.manifest.exchange,
        tf: Timeframe::M1,
        gaps_policy: loaded.manifest.gaps_policy.as_str().to_string(),
        hash_format: loaded.manifest.hash_format.clone(),
        dataset_hash: loaded.manifest.dataset_hash.clone(),
        series: loaded
            .series
            .iter()
            .map(|(symbol, columns)| SeriesInfo {
                symbol: symbol.clone(),
                rows: columns.len() as u64,
                first_ms: columns.open_time_ms().first().copied().unwrap_or(0),
                last_ms: columns.open_time_ms().last().copied().unwrap_or(0),
            })
            .collect(),
    };

    let golden = golden_dir(GOLDEN);
    let params_text = fs::read_to_string(golden.join("params.toml")).expect("params.toml");
    let params: toml::Value = toml::from_str(&params_text).expect("params.toml valide");
    let costs = CostsFile::load(&golden.join("costs.toml")).expect("costs.toml du golden");
    let fees = FeeSchedule::load(ExchangeId::Okx, &repo("config/fees.toml")).expect("frais OKX");

    let universe: Vec<String> = UNIVERSE.iter().map(|s| (*s).to_string()).collect();
    let rules = vec![InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).expect("règles"); universe.len()];
    let set =
        || -> Vec<Box<dyn Strategy>> { universe.iter().map(|_| sma_cross(&params)).collect() };
    let result = run_portfolio_profiles(
        ProfilesInputs {
            series: &loaded.series,
            strategies: [set(), set(), set()],
            config: PortfolioConfig {
                universe: &universe,
                regime_symbol: "BTC-USD",
                fees,
                slippage: Slippage::ZERO,
                initial_cash: 300.0,
                sim_rules: SimRules::default(),
                instruments: &rules,
                instruments_hash: None,
                max_positions: 1,
                sizing: SizingMode::Compounding,
                min_order_bump_max_risk: None,
                quote_gap_ms: DEFAULT_QUOTE_GAP_MS,
                min_trades: 30,
            },
            costs: CostSetup {
                file: &costs,
                measures: None,
                seed: 1,
            },
        },
        &mut NoopObserver,
    )
    .expect("profils valides");
    assert_eq!(
        DEFAULT_MAX_POSITIONS, 8,
        "la valeur par défaut n'est pas celle du golden : N = 1"
    );

    let rendered: Vec<RenderedProfile> = result
        .runs
        .iter()
        .map(|run| {
            render_profile(
                run,
                MetricsS9Inputs {
                    dataset: dataset.clone(),
                    instruments: InstrumentsInfo::toml(),
                    okx_pairs: None,
                    markets: profiles_common::markets(&loaded.series, &universe),
                },
                CostsInputs {
                    file: &costs,
                    file_label: "costs.toml",
                    measures: None,
                    measures_label: None,
                },
                Timeframe::H1,
            )
            .expect("rendu du profil")
        })
        .collect();
    for (profile, name) in rendered.iter().zip(PROFILES) {
        profile
            .write_files(&out.join(name))
            .expect("fichiers du profil");
    }
    profiles_common::profiles_report(&rendered, &costs)
        .write(out)
        .expect("profiles.json");
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

/// Compare `metrics.json` d'un profil au golden : texte hors `approx` à l'octet, `approx` en ULP.
fn compare_metrics(profile: &str, expected: &str, got: &str) {
    let (expected_rest, expected_approx) = split_approx(expected);
    let (got_rest, got_approx) = split_approx(got);
    assert!(
        expected_rest == got_rest,
        "{profile}/metrics.json (hors approx) diffère du golden ; si le changement est voulu : \
         TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_profiles, puis relire le diff"
    );
    let (golden_values, run_values) = (approx_values(&expected_approx), approx_values(&got_approx));
    assert_eq!(
        golden_values.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        run_values.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        "{profile} : clés de approx différentes"
    );
    for ((key, golden_value), (_, run_value)) in golden_values.iter().zip(&run_values) {
        if key == "ulp_tolerance" {
            let annoncee = Some(f64::from(ULP_TOLERANCE));
            assert_eq!(
                (*golden_value, *run_value),
                (annoncee, annoncee),
                "{profile} : approx.ulp_tolerance diffère de approx::ULP_TOLERANCE"
            );
            continue;
        }
        match (golden_value, run_value) {
            (None, None) => {}
            (Some(g), Some(r)) => assert!(
                ulp_distance(*g, *r) <= u64::from(ULP_TOLERANCE),
                "{profile} : approx.{key} : {g} contre {r}, plus de {ULP_TOLERANCE} ULP d'écart"
            ),
            _ => panic!("{profile} : approx.{key} : nul d'un côté seulement"),
        }
    }
}

/// Relance le run, puis le compare au golden (ou le réécrit avec `TV4_GOLDEN_UPDATE`).
fn compare_golden() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    golden_run(dossier.path(), &out);
    let golden = golden_dir(GOLDEN);

    if std::env::var_os("TV4_GOLDEN_UPDATE").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "TV4_GOLDEN_UPDATE est refusée en CI : mettre le golden à jour en local et relire le diff"
        );
        for profile in PROFILES {
            fs::create_dir_all(golden.join(profile)).unwrap();
            for file in RUN_FILES {
                fs::copy(
                    out.join(profile).join(file),
                    golden.join(profile).join(file),
                )
                .unwrap();
            }
        }
        fs::copy(out.join("profiles.json"), golden.join("profiles.json")).unwrap();
        return;
    }

    for profile in PROFILES {
        for file in ["trades.csv", "equity.csv"] {
            let attendu = fs::read(golden.join(profile).join(file))
                .expect("golden absent : TV4_GOLDEN_UPDATE=1");
            let obtenu = fs::read(out.join(profile).join(file)).unwrap();
            assert!(
                attendu == obtenu,
                "{profile}/{file} diffère du golden ({} contre {} octets) ; si le changement est voulu : \
                 TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_profiles, puis relire le diff",
                attendu.len(),
                obtenu.len()
            );
        }
        let attendu = fs::read_to_string(golden.join(profile).join("metrics.json"))
            .expect("golden absent : TV4_GOLDEN_UPDATE=1");
        let obtenu = fs::read_to_string(out.join(profile).join("metrics.json")).unwrap();
        compare_metrics(profile, &attendu, &obtenu);
    }
    let attendu =
        fs::read(golden.join("profiles.json")).expect("golden absent : TV4_GOLDEN_UPDATE=1");
    let obtenu = fs::read(out.join("profiles.json")).unwrap();
    assert!(
        attendu == obtenu,
        "profiles.json diffère du golden ; si le changement est voulu : \
         TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_profiles, puis relire le diff"
    );
}

#[test]
fn les_trois_profils_redonnent_le_golden() {
    compare_golden();
}

/// Noms des fichiers d'un dossier, triés.
fn noms_du_dossier(dir: &Path) -> Vec<String> {
    let mut noms: Vec<String> = fs::read_dir(dir)
        .expect("dossier golden absent")
        .map(|entree| entree.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    noms.sort();
    noms
}

#[test]
fn le_golden_est_complet_et_sans_manifeste() {
    let golden = golden_dir(GOLDEN);
    assert_eq!(
        noms_du_dossier(&golden),
        [
            "adverse",
            "baseline",
            "costs.toml",
            "maker",
            "params.toml",
            "profiles.json"
        ]
    );
    for profile in PROFILES {
        assert_eq!(
            noms_du_dossier(&golden.join(profile)),
            ["equity.csv", "metrics.json", "trades.csv"],
            "{profile}"
        );
    }
}

/// Le fichier de coûts du golden est une copie des défauts de `config/costs.toml` à sa date, mais n'en
/// dépend pas : le golden lit le sien, et son hash est celui de ses octets.
#[test]
fn le_golden_lit_son_propre_fichier_de_couts() {
    let golden = golden_dir(GOLDEN);
    let own = CostsFile::load(&golden.join("costs.toml")).expect("costs.toml du golden");
    let m: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(golden.join("baseline").join("metrics.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(m["costs"]["costs_file_hash"], own.file_hash());
    assert_eq!(m["costs"]["costs_file"], "costs.toml");
    let profiles: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(golden.join("profiles.json")).unwrap()).unwrap();
    assert_eq!(profiles["costs_file_hash"], own.file_hash());
}

/// Le golden doit couvrir ce qui fait la différence avec S9 ; si une mise à jour le vide, ce test échoue.
#[test]
fn le_golden_couvre_les_trois_profils_et_leurs_blocs() {
    let golden = golden_dir(GOLDEN);
    let read = |profile: &str| -> serde_json::Value {
        serde_json::from_str(
            &fs::read_to_string(golden.join(profile).join("metrics.json")).unwrap(),
        )
        .unwrap()
    };
    let profiles: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(golden.join("profiles.json")).unwrap()).unwrap();
    assert_eq!(profiles["schema"], "tv4-profiles-v1");
    assert_eq!(profiles["fee_grid"], "okx_regular");
    assert_eq!(profiles["profiles"].as_array().unwrap().len(), 3);

    let mut hashes = Vec::new();
    for (i, profile) in PROFILES.iter().enumerate() {
        let m = read(profile);
        assert_eq!(m["schema"], "tv4-metrics-s10");
        assert_eq!(m["costs"]["profile"], *profile);
        assert_eq!(m["approx"]["ulp_tolerance"], ULP_TOLERANCE);
        assert!(m["n_trades"].as_u64().unwrap() >= 1, "{profile} sans trade");
        assert_eq!(profiles["profiles"][i]["config_hash"], m["config_hash"]);
        hashes.push(m["config_hash"].as_str().unwrap().to_string());
        // Le coût de spread est calculé, et la colonne `spread_cost` du CSV est remplie.
        assert!(
            m["cost_arithmetic"]["cost_breakdown"]["spread"]
                .as_f64()
                .unwrap()
                > 0.0,
            "{profile}"
        );
        let csv = fs::read_to_string(golden.join(profile).join("trades.csv")).unwrap();
        let mut reader = csv::Reader::from_reader(csv.as_bytes());
        let column = reader
            .headers()
            .unwrap()
            .iter()
            .position(|h| h == "spread_cost")
            .unwrap();
        for record in reader.records() {
            assert!(
                !record.unwrap().get(column).unwrap().is_empty(),
                "{profile}"
            );
        }
    }
    hashes.sort();
    hashes.dedup();
    assert_eq!(hashes.len(), 3, "trois profils, trois config_hash");

    // Le profil maker a converti des entrées, et les deux compteurs de la part maker sont publiés.
    let maker = read("maker");
    let counters = &maker["cost_counters"];
    assert!(counters["maker_entries_converted"].as_u64().unwrap() > 0);
    assert!(
        counters["maker_entries_filled_maker"].as_u64().unwrap()
            + counters["maker_entries_taken_at_arrival"].as_u64().unwrap()
            > 0
    );
    assert!(counters["maker_fill_draws"].as_u64().unwrap() > 0);
    assert_eq!(
        profiles["maker"]["entries_converted"],
        counters["maker_entries_converted"]
    );
    // Les stops sont tirés dans les trois profils.
    for profile in PROFILES {
        assert!(
            read(profile)["cost_counters"]["stop_fills_drawn"]
                .as_u64()
                .unwrap()
                > 0
        );
    }
}
