//! Golden file des deux lignes de recherche (S11, lot E, docs/specs/s11.md §9.2 et §9.3) : un run de
//! recherche sur un dataset FORWARD **synthétique** à trois jambes, joué en ligne `full` puis en ligne
//! `okx_usdc`, doit redonner les fichiers versionnés dans `tests/golden_runs/research_lines/`.
//!
//! * `full/`, `okx_usdc/` : `metrics.json` (`tv4-metrics-s11`), `trades.csv` (tous les trades, purgés
//!   compris) et `equity.csv` de chaque ligne, sans manifeste (il n'est pas déterministe : horodatage,
//!   durées, build), comme les autres goldens ;
//! * `lines.json` : le résumé des deux lignes. Il ne contient ni horodatage ni durée, donc il se compare
//!   à l'octet, et son `lines_id` est épinglé par le golden.
//!
//! **Tout est inventé.** Le marché est celui de `research_common` : BTC-USD plat à 100 pendant la chauffe
//! puis 110 / 90 / 115 sur la fenêtre de décision (un régime factice, pas un prix de BTC), trois jambes
//! en marche aléatoire déterministe (`common::synthetic_candles`), un univers de deux trimestres
//! (2026-Q3, 2026-Q4) dont CCC-USD n'entre qu'au second, BBB-USD exclue en 4h au second par la qualité,
//! un sous-ensemble `okx_usdc` {AAA, CCC} et une stratégie de test qui lit le régime (`uses_regime()`
//! vrai). Le dataset est en mémoire, sans manifeste de dataset : son `dataset_hash` est un libellé fixe du
//! golden, pas un hash de contenu. Aucune donnée de marché réelle.
//!
//! La CLI de recherche (lot F) n'est pas nécessaire : le test pilote la bibliothèque comme elle le
//! fera (`run_portfolio_research`, `render_research_run`, `LinesReport`). Même règle de comparaison que
//! `golden_run_profiles.rs` : octets pour les CSV et `lines.json`, chaîne canonique de `metrics.json`
//! privée de `approx` à l'octet, `approx` à `approx::ULP_TOLERANCE` ULP.
//!
//! Mise à jour : `TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_research` réécrit les fichiers (refusée
//! si la variable `CI` est définie). Relire le diff avant de commiter, comme pour les `.snap`.

mod common;
mod portfolio_common;
mod profiles_common;
mod research_common;

use std::fs;
use std::path::{Path, PathBuf};

use research_common::fixture;
use tv4::engine::Line;
use tv4::report::approx::ULP_TOLERANCE;

/// Dossier du golden.
const GOLDEN: &str = "research_lines";
const LINES: [(&str, Line); 2] = [("full", Line::Full), ("okx_usdc", Line::OkxUsdc)];
const RUN_FILES: [&str; 3] = ["metrics.json", "trades.csv", "equity.csv"];

fn golden_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden_runs")
        .join(name)
}

/// Joue les deux lignes et écrit leurs fichiers, puis `lines.json`, dans `out` (sans manifestes).
fn golden_run(out: &Path) {
    let fx = fixture();
    let full = fx.render(Line::Full, 300.0);
    let okx = fx.render(Line::OkxUsdc, 300.0);
    full.write_files(&out.join("full"))
        .expect("fichiers de full");
    okx.write_files(&out.join("okx_usdc"))
        .expect("fichiers de okx_usdc");
    fx.lines_report(&full, &okx).write(out).expect("lines.json");
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
         TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_research, puis relire le diff"
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
    golden_run(&out);
    let golden = golden_dir(GOLDEN);

    if std::env::var_os("TV4_GOLDEN_UPDATE").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "TV4_GOLDEN_UPDATE est refusée en CI : mettre le golden à jour en local et relire le diff"
        );
        for (line, _) in LINES {
            fs::create_dir_all(golden.join(line)).unwrap();
            for file in RUN_FILES {
                fs::copy(out.join(line).join(file), golden.join(line).join(file)).unwrap();
            }
        }
        fs::copy(out.join("lines.json"), golden.join("lines.json")).unwrap();
        return;
    }

    for (line, _) in LINES {
        for file in ["trades.csv", "equity.csv"] {
            let attendu = fs::read(golden.join(line).join(file))
                .expect("golden absent : TV4_GOLDEN_UPDATE=1");
            let obtenu = fs::read(out.join(line).join(file)).unwrap();
            assert!(
                attendu == obtenu,
                "{line}/{file} diffère du golden ({} contre {} octets) ; si le changement est voulu : \
                 TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_research, puis relire le diff",
                attendu.len(),
                obtenu.len()
            );
        }
        let attendu = fs::read_to_string(golden.join(line).join("metrics.json"))
            .expect("golden absent : TV4_GOLDEN_UPDATE=1");
        let obtenu = fs::read_to_string(out.join(line).join("metrics.json")).unwrap();
        compare_metrics(line, &attendu, &obtenu);
    }
    let attendu = fs::read(golden.join("lines.json")).expect("golden absent : TV4_GOLDEN_UPDATE=1");
    let obtenu = fs::read(out.join("lines.json")).unwrap();
    assert!(
        attendu == obtenu,
        "lines.json diffère du golden ; si le changement est voulu : \
         TV4_GOLDEN_UPDATE=1 cargo test --test golden_run_research, puis relire le diff"
    );
}

#[test]
fn les_deux_lignes_redonnent_le_golden() {
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
    assert_eq!(noms_du_dossier(&golden), ["full", "lines.json", "okx_usdc"]);
    for (line, _) in LINES {
        assert_eq!(
            noms_du_dossier(&golden.join(line)),
            ["equity.csv", "metrics.json", "trades.csv"],
            "{line}"
        );
    }
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Le golden doit couvrir ce qui fait s11 : purge, trimestres, exclusion qualité, régime, deux lignes de
/// même `config_hash`. Si une mise à jour le vide, ce test échoue.
#[test]
fn le_golden_couvre_la_purge_les_trimestres_et_les_deux_lignes() {
    let golden = golden_dir(GOLDEN);
    let lines = read_json(&golden.join("lines.json"));
    assert_eq!(lines["schema"], "tv4-lines-v1");
    assert_eq!(lines["lines"].as_array().unwrap().len(), 2);

    let mut config_hashes = Vec::new();
    let mut identities = Vec::new();
    for (i, (line, _)) in LINES.iter().enumerate() {
        let m = read_json(&golden.join(line).join("metrics.json"));
        assert_eq!(m["schema"], "tv4-metrics-s11");
        assert_eq!(m["approx"]["ulp_tolerance"], ULP_TOLERANCE);
        assert_eq!(m["research"]["line"], *line);
        assert_eq!(m["research"]["block"], "FORWARD");
        assert_eq!(m["research"]["regime_used"], true);
        assert_eq!(lines["lines"][i]["line"], *line);
        assert_eq!(lines["lines"][i]["run_id"].as_str().unwrap().len(), 64);
        config_hashes.push(m["config_hash"].as_str().unwrap().to_string());
        identities.push(m["research"]["identity_hash"].as_str().unwrap().to_string());

        // Purge : au moins un trade conservé et un purgé ; trades.csv les contient tous.
        let kept = m["research"]["purge"]["trades_kept"].as_u64().unwrap();
        let purged = m["research"]["purge"]["trades_purged"].as_u64().unwrap();
        assert!(
            kept >= 1 && purged >= 1,
            "{line} : kept {kept}, purged {purged}"
        );
        assert_eq!(m["n_trades"].as_u64().unwrap(), kept);
        let csv = fs::read_to_string(golden.join(line).join("trades.csv")).unwrap();
        let mut reader = csv::Reader::from_reader(csv.as_bytes());
        let column = reader
            .headers()
            .unwrap()
            .iter()
            .position(|h| h == "exit_type")
            .unwrap();
        let (mut rows, mut end_of_window) = (0u64, 0u64);
        for record in reader.records() {
            rows += 1;
            if record.unwrap().get(column) == Some("end_of_window") {
                end_of_window += 1;
            }
        }
        assert_eq!(rows, kept + purged, "{line}");
        assert_eq!(end_of_window, purged, "{line}");

        // Refus de recherche : fenêtre, hors univers (CCC avant le second trimestre), qualité (BBB).
        let funnel = &m["research"]["funnel"];
        assert!(funnel["entries_outside_decision_window"].as_u64().unwrap() > 0);
        assert!(funnel["entries_not_in_universe"].as_u64().unwrap() > 0);
        if *line == "full" {
            assert!(funnel["entries_excluded_quality"].as_u64().unwrap() > 0);
            assert_eq!(
                m["research"]["universe"]["quarters"][1]["excluded_quality"][0],
                "BBB-USD"
            );
        } else {
            assert_eq!(funnel["entries_excluded_quality"], 0, "BBB n'est pas jouée");
            assert_eq!(m["research"]["subset"]["legs_excluded"][0], "BBB-USD");
        }
        assert_eq!(
            m["research"]["universe"]["quarters"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(m["regime"]["days_bull"].as_u64().unwrap() > 0);
        assert!(m["regime"]["days_bear"].as_u64().unwrap() > 0);
    }
    // Même configuration gelée, identités et run_id différents.
    assert_eq!(config_hashes[0], config_hashes[1]);
    assert_eq!(lines["config_hash"]["single"], config_hashes[0]);
    assert_ne!(identities[0], identities[1]);
    assert_ne!(lines["lines"][0]["run_id"], lines["lines"][1]["run_id"]);
    assert!(lines["residual_survivorship"]["single"]["n_trades"].is_i64());
}
