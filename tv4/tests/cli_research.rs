//! `tv4 backtest` en mode recherche (S11, lot F, docs/specs/s11.md §8, §9, §11 et §14) : on lance le vrai
//! binaire sur des datasets de bloc **synthétiques** (voir `cli_research_common`), sans donnée de marché
//! réelle ni secret.
//!
//! Couvre : une ligne (`full`) ou deux lignes (`full`, `okx_usdc`, `lines.json`), avec ou sans `--costs` ;
//! l'égalité de `okx_usdc` avec un run seul sur ses jambes ; la garde provisoire de VALIDATION et
//! HOLDOUT ; l'obligation du sous-ensemble sur les blocs protégés ; les refus d'usage (code 2) ; la
//! compatibilité d'un dataset sans bloc.

mod cli_research_common;
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use cli_research_common::{World, json, ok, repo, run, strings};
use serde_json::Value;
use tv4::model::{Block, ExchangeId, Timeframe, Ts};
use tv4::store::{
    Columns, GapsPolicy, MultiBuildRequest, Partition, ResearchMeta, Symbol, Window,
    build_dataset_multi, build_dataset_research, write_partition,
};

const MINUTE: Ts = 60_000;
/// 2030-01-01T00:00:00Z : tous les mois des tests sont clos à cet instant.
const FUTUR: Ts = 1_893_456_000_000;
/// Minutes des datasets de blocs protégés.
const MINUTES: usize = 6_000;

fn shown(path: &Path) -> String {
    path.display().to_string()
}

fn costs_file() -> String {
    shown(&repo("config/costs.toml"))
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("{} : {e}", path.display()))
}

fn verify(a: &Path, b: &Path) -> (Option<i32>, String) {
    run(&strings(&["verify", &shown(a), &shown(b)]))
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .expect("dossier")
        .map(|e| {
            e.expect("entrée")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------------------------
// Une ligne
// ---------------------------------------------------------------------------------------------

#[test]
fn une_ligne_full_ecrit_quatre_fichiers_de_schema_s11() {
    let w = World::with_dataset();
    let out = w.root.join("out");
    let (stdout, stderr) = ok(&w.backtest_args(&out, &[]));
    assert!(stdout.contains("ligne full : run "), "{stdout}");
    assert!(
        stderr.is_empty(),
        "aucun avertissement sur FORWARD : {stderr}"
    );
    assert_eq!(
        names(&out),
        [
            "equity.csv",
            "manifest.json",
            "metrics.json",
            "stats-inputs.json",
            "trades.csv"
        ]
    );

    let metrics = json(&out.join("metrics.json"));
    assert_eq!(metrics["schema"], "tv4-metrics-s11");
    let research = &metrics["research"];
    assert_eq!(research["block"], "FORWARD");
    assert_eq!(research["line"], "full");
    assert_eq!(research["guard"], "none");
    assert_eq!(research["subset"], Value::Null);
    assert_eq!(research["universe"]["mode"], "quarterly");
    assert_eq!(research["metrics_basis"], "kept_trades");
    assert_eq!(research["purge"]["rule"], "end_of_window_v1");
    assert_eq!(research["legs"].as_array().map(Vec::len), Some(3));
    assert_eq!(metrics["data"]["block"], "FORWARD");
    assert_eq!(
        metrics["data"]["decision_window"]["start"],
        "2026-07-01T00:00:00Z"
    );

    // `trades.csv` garde tous les trades, purgés compris ; `n_trades` ne compte que les conservés.
    let trades = String::from_utf8(read(&out.join("trades.csv"))).expect("utf-8");
    let rows = trades.lines().count() - 1;
    let kept = research["purge"]["trades_kept"].as_u64().expect("kept") as usize;
    let purged = research["purge"]["trades_purged"].as_u64().expect("purged") as usize;
    assert_eq!(rows, kept + purged);
    assert_eq!(metrics["n_trades"].as_u64(), Some(kept as u64));
    assert!(kept > 0, "le marché synthétique doit produire des trades");

    let manifest = json(&out.join("manifest.json"));
    assert_eq!(manifest["schema"], "tv4-manifest-s11");
    assert_eq!(manifest["research_inputs"]["universe"], shown(&w.universe));
    assert!(manifest["research_inputs"].get("okx_subset").is_none());
    assert!(manifest["run_id"].as_str().is_some_and(|id| id.len() == 64));
}

#[test]
fn deux_runs_identiques_passent_verify_et_un_capital_different_est_refuse() {
    let w = World::with_dataset();
    let (a, b, c) = (w.root.join("a"), w.root.join("b"), w.root.join("c"));
    ok(&w.backtest_args(&a, &[]));
    ok(&w.backtest_args(&b, &[]));
    ok(&w.backtest_args(&c, &["--capital", "3000"]));
    let (code, texte) = verify(&a, &b);
    assert_eq!(code, Some(0), "{texte}");
    let (code, texte) = verify(&a, &c);
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("capitaux initiaux différents"), "{texte}");
    // Même `config_hash` et mêmes trades à 300 € et à 3 000 € : seul le capital diffère.
    let (ma, mc) = (json(&a.join("metrics.json")), json(&c.join("metrics.json")));
    assert_eq!(ma["config_hash"], mc["config_hash"]);
}

#[test]
fn le_run_est_rejoue_dans_le_meme_dossier_et_retire_les_fichiers_d_une_autre_forme() {
    let w = World::with_dataset();
    let sub = w.write_subset("sub.json", &["BTC", "SOL"]);
    let out = w.root.join("out");
    ok(&w.backtest_args(&out, &["--okx-subset", &shown(&sub)]));
    assert_eq!(names(&out), ["full", "lines.json", "okx_usdc"]);
    // Un run simple dans le même dossier : `lines.json` d'avant disparaît, jamais mêlé au nouveau run.
    ok(&w.backtest_args(&out, &[]));
    assert!(!out.join("lines.json").exists());
    assert!(out.join("manifest.json").exists());
    let again = w.root.join("again");
    ok(&w.backtest_args(&again, &[]));
    let (code, texte) = verify(&out, &again);
    assert_eq!(code, Some(0), "{texte}");
}

/// Arguments d'un run S8 (fixture du dépôt) écrivant dans `out`.
fn s8_args(out: &Path) -> Vec<String> {
    strings(&[
        "backtest",
        "--fixture",
        &shown(&repo("fixtures/kraken_xbtusd_1m.csv")),
        "--strategy",
        "sma_cross",
        "--params",
        &shown(&repo("config/strategies/sma_cross_1m.toml")),
        "--fees-file",
        &shown(&repo("config/fees.toml")),
        "--instruments-file",
        &shown(&repo("config/instruments.toml")),
        "--out",
        &shown(out),
    ])
}

#[test]
fn un_run_s8_s9_ou_s10_retire_le_lines_json_d_un_dossier_de_lignes() {
    let w = World::with_dataset();
    build_plain(&w);
    let sub = w.write_subset("sub.json", &["BTC", "SOL"]);
    let sub_text = shown(&sub);
    let costs = costs_file();
    for forme in ["s8", "s9", "s10"] {
        let args_pour = |dir: &Path| -> Vec<String> {
            match forme {
                "s8" => s8_args(dir),
                "s9" => plain_args(&w, dir, &[]),
                _ => plain_args(&w, dir, &["--costs", costs.as_str()]),
            }
        };
        // Un dossier de lignes (`full/`, `okx_usdc/`, `lines.json`), puis un run d'une autre forme dans le
        // même dossier : `lines.json` d'avant disparaît, sans quoi verify et le control plane classeraient
        // encore le dossier comme un dossier de lignes.
        let out = w.root.join(format!("melange_{forme}"));
        ok(&w.backtest_args(&out, &["--okx-subset", sub_text.as_str()]));
        assert!(out.join("lines.json").exists(), "{forme}");
        ok(&args_pour(&out));
        assert!(!out.join("lines.json").exists(), "{forme}");

        // Le même run dans un dossier neuf : verify les juge identiques.
        let neuf = w.root.join(format!("neuf_{forme}"));
        ok(&args_pour(&neuf));
        let (code, texte) = verify(&out, &neuf);
        assert_eq!(code, Some(0), "{forme} : {texte}");
    }
}

// ---------------------------------------------------------------------------------------------
// Deux lignes
// ---------------------------------------------------------------------------------------------

#[test]
fn deux_lignes_ecrivent_full_okx_usdc_et_lines_json() {
    let w = World::with_dataset();
    let sub = w.write_subset("sub.json", &["BTC", "SOL"]);
    let out = w.root.join("out");
    let (stdout, _) = ok(&w.backtest_args(&out, &["--okx-subset", &shown(&sub)]));
    assert!(stdout.contains("ligne full : run "), "{stdout}");
    assert!(stdout.contains("ligne okx_usdc : run "), "{stdout}");
    assert!(
        stdout.contains("survivance résiduelle (single)"),
        "{stdout}"
    );
    assert_eq!(names(&out), ["full", "lines.json", "okx_usdc"]);
    for line in ["full", "okx_usdc"] {
        assert_eq!(
            names(&out.join(line)),
            [
                "equity.csv",
                "manifest.json",
                "metrics.json",
                "stats-inputs.json",
                "trades.csv"
            ],
            "{line}"
        );
    }

    let full = json(&out.join("full/metrics.json"));
    let okx = json(&out.join("okx_usdc/metrics.json"));
    let full_id = json(&out.join("full/manifest.json"))["run_id"].clone();
    let okx_id = json(&out.join("okx_usdc/manifest.json"))["run_id"].clone();
    // Même configuration, deux identités : deux `run_id`.
    assert_eq!(full["config_hash"], okx["config_hash"]);
    assert_ne!(full_id, okx_id);
    assert_eq!(full["dataset_hash"], okx["dataset_hash"]);
    assert_eq!(full["research"]["line"], "full");
    assert_eq!(okx["research"]["line"], "okx_usdc");
    assert_eq!(
        okx["research"]["subset"]["subset_hash"],
        json(&sub)["subset_hash"]
    );
    assert_eq!(
        okx["research"]["subset"]["legs_excluded"],
        serde_json::json!(["ETH-USD"])
    );
    assert_eq!(
        okx["data"]["symbols"],
        serde_json::json!(["BTC-USD", "SOL-USD"])
    );
    assert_eq!(
        full["data"]["symbols"],
        serde_json::json!(["BTC-USD", "ETH-USD", "SOL-USD"])
    );

    let okx_manifest = json(&out.join("okx_usdc/manifest.json"));
    assert_eq!(okx_manifest["research_inputs"]["okx_subset"], shown(&sub));
    let full_manifest = json(&out.join("full/manifest.json"));
    assert!(full_manifest["research_inputs"].get("okx_subset").is_none());

    // lines.json : deux lignes dans l'ordre, identités, jambes, sous-ensemble.
    let lines = json(&out.join("lines.json"));
    assert_eq!(lines["schema"], "tv4-lines-v1");
    let entries = lines["lines"].as_array().expect("lines");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["line"], "full");
    assert_eq!(entries[0]["dir"], "full");
    assert_eq!(entries[0]["run_id"], full_id);
    assert_eq!(entries[1]["line"], "okx_usdc");
    assert_eq!(entries[1]["run_id"], okx_id);
    assert_eq!(
        entries[1]["legs"],
        serde_json::json!(["BTC-USD", "SOL-USD"])
    );
    assert_eq!(lines["subset"]["bases"], serde_json::json!(["BTC", "SOL"]));
    assert_eq!(
        lines["universe_window_hash"],
        full["research"]["universe"]["window_hash"]
    );

    // Chaque entrée `by_profile` de `lines.json` est recoupée avec le `metrics.json` de sa ligne : un
    // `by_profile` faux mais cohérent avec le résidu ne passerait pas.
    let metric = |m: &Value, key: &str| m[key].as_f64().expect(key);
    let optional = |m: &Value, key: &str| m[key].as_f64();
    for (index, run) in [(0usize, &full), (1usize, &okx)] {
        let stats = &entries[index]["by_profile"]["single"];
        let line = entries[index]["line"].clone();
        assert_eq!(stats["n_trades"], run["n_trades"], "{line}");
        assert_eq!(
            stats["trades_purged"], run["research"]["purge"]["trades_purged"],
            "{line}"
        );
        assert_eq!(
            stats["identity_hash"], run["research"]["identity_hash"],
            "{line}"
        );
        assert_eq!(stats["verdict"], run["verdict"], "{line}");
        for key in ["expectancy_net", "expectancy_r", "profit_factor"] {
            match (optional(stats, key), optional(run, key)) {
                (Some(lu), Some(attendu)) => assert!(
                    (lu - attendu).abs() < 1e-15,
                    "{line} {key} : {lu} contre {attendu}"
                ),
                (lu, attendu) => assert_eq!(lu, attendu, "{line} {key}"),
            }
        }
        for (key, attendu) in [
            ("return_total", metric(run, "return_total")),
            (
                "max_drawdown_1h",
                run["max_drawdown"]["h1"]["pct"].as_f64().expect("h1.pct"),
            ),
        ] {
            let lu = metric(stats, key);
            assert!(
                (lu - attendu).abs() < 1e-15,
                "{line} {key} : {lu} contre {attendu}"
            );
        }
    }

    // Survivance résiduelle : full moins okx_usdc, recalculée ici depuis les entrées `by_profile`
    // (elles-mêmes recoupées avec les deux `metrics.json` ci-dessus).
    let residual = &lines["residual_survivorship"]["single"];
    assert_eq!(
        residual["n_trades"].as_i64(),
        Some(full["n_trades"].as_i64().unwrap() - okx["n_trades"].as_i64().unwrap())
    );
    for residual_key in [
        "expectancy_net",
        "expectancy_r",
        "return_total",
        "max_drawdown_1h",
    ] {
        let from_lines = |line: usize| {
            entries[line]["by_profile"]["single"][residual_key]
                .as_f64()
                .expect(residual_key)
        };
        let expected = from_lines(0) - from_lines(1);
        let got = metric(residual, residual_key);
        assert!(
            (got - expected).abs() < 1e-12,
            "{residual_key} : {got} contre {expected}"
        );
    }

    // Deux runs identiques : `tv4 verify` sur les dossiers de lignes, puis sur une ligne seule.
    let again = w.root.join("again");
    ok(&w.backtest_args(&again, &["--okx-subset", &shown(&sub)]));
    let (code, texte) = verify(&out, &again);
    assert_eq!(code, Some(0), "{texte}");
    assert!(texte.contains("dossiers de lignes identiques"), "{texte}");
    let (code, texte) = verify(&out.join("okx_usdc"), &again.join("okx_usdc"));
    assert_eq!(code, Some(0), "{texte}");
    // Un dossier de lignes contre un dossier de run : refusé.
    let (code, texte) = verify(&out, &again.join("full"));
    assert_eq!(code, Some(1), "{texte}");
}

#[test]
fn la_ligne_okx_usdc_egale_un_run_seul_sur_ses_jambes() {
    let w = World::with_dataset();
    let sub = w.write_subset("sub.json", &["BTC", "SOL"]);
    let out = w.root.join("out");
    ok(&w.backtest_args(&out, &["--okx-subset", &shown(&sub)]));

    // Run seul sur BTC-USD et SOL-USD : symboles fixes, règles d'instrument de l'univers.
    let alone = w.root.join("alone");
    let mut args = w.backtest_args(&alone, &[]);
    let index = args
        .iter()
        .position(|a| a == "--universe")
        .expect("--universe");
    args.drain(index..=index + 1);
    args.extend(strings(&[
        "--symbols",
        "BTC-USD,SOL-USD",
        "--instruments-file",
        &shown(&w.universe.join("instruments_kraken.toml")),
    ]));
    ok(&args);
    for file in ["trades.csv", "equity.csv"] {
        assert_eq!(
            read(&out.join("okx_usdc").join(file)),
            read(&alone.join(file)),
            "{file}"
        );
    }
    let alone_metrics = json(&alone.join("metrics.json"));
    let okx = json(&out.join("okx_usdc/metrics.json"));
    for key in ["n_trades", "pnl_net", "expectancy_net", "max_drawdown_1h"] {
        assert_eq!(alone_metrics[key], okx[key], "{key}");
    }
    // Les identités diffèrent (univers trimestriel contre symboles fixes, ligne okx_usdc) : deux run_id.
    let alone_id = json(&alone.join("manifest.json"))["run_id"].clone();
    let okx_id = json(&out.join("okx_usdc/manifest.json"))["run_id"].clone();
    assert_ne!(alone_id, okx_id);
}

#[test]
fn avec_costs_chaque_ligne_est_un_dossier_de_trois_profils() {
    let w = World::with_dataset();
    let sub = w.write_subset("sub.json", &["BTC", "SOL"]);
    let out = w.root.join("out");
    let (stdout, _) = ok(&w.backtest_args(
        &out,
        &["--okx-subset", &shown(&sub), "--costs", &costs_file()],
    ));
    assert!(stdout.contains("ligne full : profils "), "{stdout}");
    assert!(stdout.contains("ligne okx_usdc : profils "), "{stdout}");
    assert_eq!(names(&out), ["full", "lines.json", "okx_usdc"]);
    for line in ["full", "okx_usdc"] {
        assert_eq!(
            names(&out.join(line)),
            ["adverse", "baseline", "maker", "profiles.json"],
            "{line}"
        );
        for profile in ["baseline", "adverse", "maker"] {
            let dir = out.join(line).join(profile);
            let manifest = json(&dir.join("manifest.json"));
            assert_eq!(manifest["schema"], "tv4-manifest-s11", "{line}/{profile}");
            assert_eq!(manifest["profile"], profile);
            let metrics = json(&dir.join("metrics.json"));
            assert_eq!(metrics["schema"], "tv4-metrics-s11");
            assert_eq!(metrics["research"]["line"], line);
            assert_eq!(metrics["costs"]["profile"], profile);
        }
    }
    let lines = json(&out.join("lines.json"));
    let profiles: Vec<&String> = lines["residual_survivorship"]
        .as_object()
        .expect("objet")
        .keys()
        .collect();
    assert_eq!(profiles, ["adverse", "baseline", "maker"]);
    assert_eq!(
        lines["lines"][1]["by_profile"].as_object().map(|o| o.len()),
        Some(3)
    );

    let again = w.root.join("again");
    ok(&w.backtest_args(
        &again,
        &["--okx-subset", &shown(&sub), "--costs", &costs_file()],
    ));
    let (code, texte) = verify(&out, &again);
    assert_eq!(code, Some(0), "{texte}");
}

#[test]
fn un_sous_ensemble_sans_base_commune_echoue_en_code_1_sans_rien_ecrire() {
    let w = World::with_dataset();
    let sub = w.write_subset("zzz.json", &["ZZZ"]);
    let out = w.root.join("out");
    let (code, texte) = run(&w.backtest_args(&out, &["--okx-subset", &shown(&sub)]));
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("aucune jambe"), "{texte}");
    assert!(!out.exists(), "rien ne doit être écrit : {:?}", out);
}

// ---------------------------------------------------------------------------------------------
// Refus d'usage
// ---------------------------------------------------------------------------------------------

fn refus(args: &[String], needle: &str) {
    let (code, texte) = run(args);
    assert_eq!(code, Some(2), "{args:?}\n{texte}");
    assert!(texte.contains(needle), "{needle} absent de : {texte}");
}

#[test]
fn un_dataset_de_bloc_sans_symbols_ni_universe_est_refuse() {
    let w = World::with_dataset();
    let out = w.root.join("out");
    let mut args = w.backtest_args(&out, &[]);
    let index = args
        .iter()
        .position(|a| a == "--universe")
        .expect("--universe");
    args.drain(index..=index + 1);
    refus(&args, "mode portefeuille");
    assert!(!out.exists());
}

#[test]
fn universe_est_incompatible_avec_symbols_et_les_fichiers_d_instruments() {
    let w = World::with_dataset();
    let out = w.root.join("out");
    let okx = shown(&repo("tv4/tests/fixtures/okx_instruments_spot.json"));
    let toml = shown(&w.universe.join("instruments_kraken.toml"));
    for extra in [
        vec!["--symbols", "BTC-USD,ETH-USD"],
        vec!["--instruments-file", toml.as_str()],
        vec!["--instruments-okx", okx.as_str()],
    ] {
        let args = w.backtest_args(&out, &extra);
        let (code, texte) = run(&args);
        assert_eq!(code, Some(2), "{extra:?}\n{texte}");
        assert!(texte.contains("cannot be used with"), "{texte}");
    }
    assert!(!out.exists());
}

#[test]
fn un_univers_d_une_autre_fenetre_est_refuse_en_code_2() {
    let w = World::with_dataset();
    // Un autre univers (autre zip, mêmes paires) : mêmes règles, `window_hash` différent.
    let other = World::bare();
    let variant = other._dir.path().join("variante.zip");
    let sha = cli_research_common::ecrire_zip_variante(&variant, 1);
    let mut build = other.build_args();
    build[3] = shown(&variant);
    build[5] = sha;
    ok(&build);

    let out = w.root.join("out");
    let mut args = w.backtest_args(&out, &[]);
    let index = args
        .iter()
        .position(|a| a == "--universe")
        .expect("--universe");
    args[index + 1] = shown(&other.universe);
    let (code, texte) = run(&args);
    assert_eq!(code, Some(2), "{texte}");
    assert!(texte.contains("window_hash"), "{texte}");
    assert!(!out.exists());
}

#[test]
fn universe_sur_un_dataset_a_symboles_fixes_est_refuse() {
    let w = World::with_universe();
    // Dataset de bloc sans univers : `--symbol` seulement.
    ok(&strings(&[
        "dataset",
        "build",
        "--root",
        &shown(&w.root),
        "--name",
        "fixe",
        "--exchange",
        "kraken",
        "--tf",
        "1m",
        "--block",
        "FORWARD",
        "--to",
        "2026-09-01T00:00:00Z",
        "--warmup-days",
        "5",
        "--symbol",
        "BTC-USD",
        "--symbol",
        "ETH-USD",
    ]));
    let out = w.root.join("out");
    let mut args = w.backtest_args(&out, &[]);
    let index = args
        .iter()
        .position(|a| a == "--dataset")
        .expect("--dataset");
    args[index + 1] = "fixe".to_string();
    let (code, texte) = run(&args);
    assert_eq!(code, Some(2), "{texte}");
    assert!(texte.contains("sans univers"), "{texte}");

    // Avec des symboles fixes, le même dataset se joue : une ligne.
    let mut args = w.backtest_args(&out, &[]);
    args[index + 1] = "fixe".to_string();
    let u = args
        .iter()
        .position(|a| a == "--universe")
        .expect("--universe");
    args.drain(u..=u + 1);
    args.extend(strings(&[
        "--symbols",
        "BTC-USD,ETH-USD",
        "--instruments-file",
        &shown(&w.universe.join("instruments_kraken.toml")),
    ]));
    ok(&args);
    let metrics = json(&out.join("metrics.json"));
    assert_eq!(metrics["research"]["universe"]["mode"], "fixed");
    assert_eq!(
        metrics["research"]["universe"]["symbols"],
        serde_json::json!(["BTC-USD", "ETH-USD"])
    );
}

// ---------------------------------------------------------------------------------------------
// Dataset sans bloc : compatibilité S9 et refus des options de recherche
// ---------------------------------------------------------------------------------------------

fn build_plain(w: &World) {
    ok(&strings(&[
        "dataset",
        "build",
        "--root",
        &shown(&w.root),
        "--name",
        "plain",
        "--exchange",
        "kraken",
        "--tf",
        "1m",
        "--symbol",
        "BTC-USD",
        "--symbol",
        "ETH-USD",
        "--from",
        "2026-07-01T00:00:00Z",
        "--to",
        "2026-08-01T00:00:00Z",
        "--gaps-policy",
        "skip",
    ]));
}

fn plain_args(w: &World, out: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = w.backtest_args(out, &[]);
    let index = args
        .iter()
        .position(|a| a == "--dataset")
        .expect("--dataset");
    args[index + 1] = "plain".to_string();
    let u = args
        .iter()
        .position(|a| a == "--universe")
        .expect("--universe");
    args.drain(u..=u + 1);
    args.extend(strings(&[
        "--symbols",
        "BTC-USD,ETH-USD",
        "--instruments-file",
        &shown(&w.universe.join("instruments_kraken.toml")),
    ]));
    args.extend(strings(extra));
    args
}

#[test]
fn un_dataset_sans_bloc_joue_en_s9_et_refuse_les_options_de_recherche() {
    let w = World::with_universe();
    ok(&w.dataset_args("fwd", "5"));
    build_plain(&w);
    let out = w.root.join("out");
    let (_, stderr) = ok(&plain_args(&w, &out, &[]));
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(json(&out.join("metrics.json"))["schema"], "tv4-metrics-s9");
    assert_eq!(
        json(&out.join("manifest.json"))["schema"],
        "tv4-manifest-s9"
    );

    let sub = w.write_subset("sub.json", &["BTC"]);
    let sub_text = shown(&sub);
    for (extra, option) in [
        (vec!["--okx-subset", sub_text.as_str()], "--okx-subset"),
        (
            vec!["--unguarded-protected-read"],
            "--unguarded-protected-read",
        ),
    ] {
        let again = w.root.join("refuse");
        refus(&plain_args(&w, &again, &extra), option);
        assert!(!again.exists());
    }
    // `--universe` sur un dataset sans bloc : l'option remplace --symbols.
    let again = w.root.join("refuse");
    let mut args = w.backtest_args(&again, &[]);
    let index = args
        .iter()
        .position(|a| a == "--dataset")
        .expect("--dataset");
    args[index + 1] = "plain".to_string();
    refus(&args, "--universe");
    assert!(!again.exists());
}

// ---------------------------------------------------------------------------------------------
// Blocs protégés : garde provisoire, sous-ensemble obligatoire, avertissement §3.4
// ---------------------------------------------------------------------------------------------

struct Protected {
    _dir: tempfile::TempDir,
    root: PathBuf,
    params: PathBuf,
    instruments: PathBuf,
}

/// Écrit BTC-USD et ETH-USD en 1m synthétiques sur `MINUTES` minutes depuis `start`.
fn write_series(root: &Path, start: Ts) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for (symbol, seed, scale) in [("BTC-USD", 11_u64, 2000.0), ("ETH-USD", 22, 150.0)] {
        let mut candles = common::synthetic_candles(MINUTES, seed, start, 0.0);
        for candle in &mut candles {
            candle.volume *= scale;
        }
        let symbol: Symbol = symbol.parse().expect("symbole");
        write_partition(
            root,
            &Partition::new(ExchangeId::Kraken, symbol.clone(), Timeframe::M1),
            &Columns::from_candles(&candles),
            "synthetic",
            FUTUR,
        )
        .expect("partition");
        symbols.push(symbol);
    }
    symbols
}

/// Fenêtre de `MINUTES` minutes depuis `start`.
fn window_of(start: Ts) -> Window {
    Window::new(start, start + MINUTES as Ts * MINUTE).expect("fenêtre")
}

/// Dataset `name` de ce bloc (symboles fixes), sur la fenêtre de `window_of(start)`.
fn build_block(root: &Path, name: &str, block: Block, start: Ts, symbols: &[Symbol]) {
    build_dataset_research(
        root,
        &MultiBuildRequest {
            name,
            exchange: ExchangeId::Kraken,
            symbols,
            tf: Timeframe::M1,
            window: window_of(start),
            gaps_policy: GapsPolicy::ZeroVolumeMarked,
        },
        &ResearchMeta {
            block,
            universe: None,
            universe_quality: None,
        },
        FUTUR,
    )
    .expect("dataset de bloc");
}

/// Racine avec BTC-USD et ETH-USD en 1m synthétiques depuis le début de `block`, un dataset `bloc` de ce
/// bloc si `with_block_dataset`, et dans tous les cas un dataset `sans_bloc` (politique `skip`) sur
/// `[début, début + 6 000 min)`.
fn protected(block: Block, with_block_dataset: bool) -> Protected {
    let dir = tempfile::tempdir().expect("dossier temporaire");
    let root = dir.path().join("racine");
    fs::create_dir_all(&root).expect("racine");
    cli_research_common::init_research(&root);
    let start = block.span().start_ms;
    let symbols = write_series(&root, start);
    let window = window_of(start);
    if with_block_dataset {
        build_block(&root, "bloc", block, start, &symbols);
    }
    build_dataset_multi(
        &root,
        &MultiBuildRequest {
            name: "sans_bloc",
            exchange: ExchangeId::Kraken,
            symbols: &symbols,
            tf: Timeframe::M1,
            window,
            gaps_policy: GapsPolicy::Skip,
        },
        FUTUR,
    )
    .expect("dataset sans bloc");
    let params = root.join("params.toml");
    fs::write(
        &params,
        "tf = \"15m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0\n",
    )
    .expect("params");
    let instruments = root.join("instruments.toml");
    fs::write(
        &instruments,
        "[kraken.BTC-USD]\ntick_size = 0.1\nlot_size = 1e-8\nmin_qty = 0.0\nmin_notional = 0.0\n\n\
         [kraken.ETH-USD]\ntick_size = 0.01\nlot_size = 1e-6\nmin_qty = 0.0\nmin_notional = 0.0\n",
    )
    .expect("instruments");
    Protected {
        _dir: dir,
        root,
        params,
        instruments,
    }
}

impl Protected {
    /// Ajoute un dataset de bloc `name` sur des séries d'un autre bloc (mêmes paires, autres mois).
    fn add_block(&self, name: &str, block: Block) {
        let start = block.span().start_ms;
        let symbols = write_series(&self.root, start);
        build_block(&self.root, name, block, start, &symbols);
    }

    fn args(&self, dataset: &str, out: &Path, extra: &[&str]) -> Vec<String> {
        let mut args = strings(&[
            "backtest",
            "--dataset",
            dataset,
            "--strategy",
            "sma_cross",
            "--symbols",
            "BTC-USD,ETH-USD",
        ]);
        args.extend([
            "--root".to_string(),
            shown(&self.root),
            "--params".to_string(),
            shown(&self.params),
            "--fees-file".to_string(),
            shown(&repo("config/fees.toml")),
            "--instruments-file".to_string(),
            shown(&self.instruments),
            "--out".to_string(),
            shown(out),
        ]);
        args.extend([
            "--research-root".into(),
            shown(&self.root),
            "--hypothesis".into(),
            "H-001".into(),
        ]);
        args.extend(strings(extra));
        args
    }

    fn subset(&self, bases: &[&str]) -> PathBuf {
        let world = World::bare();
        let path = world.write_subset("sous_ensemble.json", bases);
        let kept = self._dir.path().join("sous_ensemble.json");
        fs::copy(&path, &kept).expect("copie");
        kept
    }
}

#[test]
fn les_blocs_proteges_refusent_la_lecture_sans_candidat_gele() {
    for block in [Block::Validation, Block::Holdout] {
        let p = protected(block, true);
        let sub = p.subset(&["BTC"]);
        let out = p._dir.path().join("out");
        let (code, texte) = run(&p.args("bloc", &out, &["--okx-subset", &shown(&sub)]));
        assert_eq!(code, Some(2), "{block} : {texte}");
        assert!(texte.contains("frozen"), "{block} : {texte}");
        assert!(!out.exists(), "{block} : rien ne doit être lu ni écrit");
    }
}

#[test]
fn les_blocs_proteges_exigent_le_sous_ensemble() {
    for block in [Block::Validation, Block::Holdout] {
        let p = protected(block, true);
        let out = p._dir.path().join("out");
        let (code, texte) = run(&p.args("bloc", &out, &[]));
        assert_eq!(code, Some(2), "{block} : {texte}");
        assert!(texte.contains("--okx-subset"), "{block} : {texte}");
        assert!(texte.contains("full et okx_usdc"), "{texte}");
        assert!(!out.exists());
    }
}

#[test]
fn la_lecture_protegee_admise_est_tracee_dans_chaque_rapport() {
    for block in [Block::Validation, Block::Holdout] {
        let p = protected(block, true);
        p.add_block("dev_seed", Block::Dev);
        let seed = p._dir.path().join("seed");
        ok(&p.args("dev_seed", &seed, &[]));
        let hash = json(&seed.join("metrics.json"))["config_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let sub = p.subset(&["BTC"]);
        let subset = json(&sub)["subset_hash"].as_str().unwrap().to_owned();
        cli_research_common::freeze(&p.root, &[hash], &subset);
        if block == Block::Holdout {
            ok(&strings(&[
                "research",
                "claim",
                "--research-root",
                &shown(&p.root),
                "--hypothesis",
                "H-001",
            ]));
        }
        let out = p._dir.path().join("out");
        let (stdout, stderr) = ok(&p.args("bloc", &out, &["--okx-subset", &shown(&sub)]));
        assert!(stderr.is_empty(), "{stderr}");
        assert!(stdout.contains("N_eff = non calculé"));
        for line in ["full", "okx_usdc"] {
            let metrics = json(&out.join(line).join("metrics.json"));
            assert_eq!(metrics["research"]["guard"], "guarded");
            assert_eq!(metrics["research"]["block"], block.to_string());
            let manifest = json(&out.join(line).join("manifest.json"));
            assert_eq!(manifest["governance"]["schema"], "tv4-governance-v1");
            assert_eq!(manifest["governance"]["noise"]["n"], 1);
            assert_eq!(manifest["governance"]["effective_block"], block.to_string());
        }
        let lines = json(&out.join("lines.json"));
        assert_eq!(lines["lines"][1]["legs"], serde_json::json!(["BTC-USD"]));
        let (code, text) = verify(&out, &out);
        assert_eq!(code, Some(0), "{text}");
    }
}

#[test]
fn le_bypass_s11_est_supprime() {
    let p = protected(Block::Holdout, true);
    let out = p._dir.path().join("out");
    let (code, text) = run(&p.args("bloc", &out, &["--unguarded-protected-read"]));
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("unexpected argument"), "{text}");
    assert!(!p.root.join("research/trials.csv").exists());
}

#[test]
fn un_dataset_sans_bloc_qui_recoupe_un_bloc_protege_avertit_sur_stderr() {
    let p = protected(Block::Validation, false);
    let out = p._dir.path().join("out");
    let (_, stderr) = ok(&p.args("sans_bloc", &out, &[]));
    assert!(stderr.contains("recoupe VALIDATION"), "{stderr}");
    assert!(stderr.contains("aucune garde ne s'applique"), "{stderr}");
    // Le rapport reste un rapport S9 : aucune trace de recherche.
    assert_eq!(json(&out.join("metrics.json"))["schema"], "tv4-metrics-s9");

    // Fenêtre dans l'embargo ou hors des blocs protégés : pas d'avertissement.
    let q = protected(Block::Dev, false);
    let out = q._dir.path().join("out");
    let (_, stderr) = ok(&q.args("sans_bloc", &out, &[]));
    assert!(!stderr.contains("recoupe"), "{stderr}");
}

#[test]
fn les_options_de_recherche_sont_refusees_sur_un_dataset_sans_bloc_protege() {
    let p = protected(Block::Holdout, false);
    let sub = p.subset(&["BTC"]);
    let out = p._dir.path().join("out");
    let (code, texte) = run(&p.args("sans_bloc", &out, &["--okx-subset", &shown(&sub)]));
    assert_eq!(code, Some(2), "{texte}");
    assert!(texte.contains("pas un dataset de recherche"), "{texte}");
    assert!(!out.exists());
}

#[test]
fn la_meme_configuration_sur_deux_blocs_a_le_meme_config_hash_et_deux_identites() {
    let p = protected(Block::Dev, true);
    p.add_block("forward", Block::Forward);
    let (a, b) = (p._dir.path().join("a"), p._dir.path().join("b"));
    ok(&p.args("bloc", &a, &[]));
    ok(&p.args("forward", &b, &[]));
    let (ma, mb) = (json(&a.join("metrics.json")), json(&b.join("metrics.json")));
    assert_eq!(ma["research"]["block"], "DEV");
    assert_eq!(mb["research"]["block"], "FORWARD");
    assert_eq!(
        ma["config_hash"], mb["config_hash"],
        "config inchangée entre blocs"
    );
    assert_ne!(
        ma["research"]["identity_hash"], mb["research"]["identity_hash"],
        "l'identité de recherche porte le bloc"
    );
    let (ia, ib) = (
        json(&a.join("manifest.json"))["run_id"].clone(),
        json(&b.join("manifest.json"))["run_id"].clone(),
    );
    assert_ne!(ia, ib);
    // Rejouer le même bloc redonne la même identité.
    let again = p._dir.path().join("again");
    ok(&p.args("bloc", &again, &[]));
    assert_eq!(json(&again.join("manifest.json"))["run_id"], ia);
    let (code, texte) = verify(&a, &again);
    assert_eq!(code, Some(0), "{texte}");
    let (code, texte) = verify(&a, &b);
    assert_ne!(code, Some(0), "deux blocs ne sont pas identiques : {texte}");
}

#[test]
fn le_meme_contenu_avec_et_sans_bloc_donne_deux_run_id() {
    let p = protected(Block::Dev, true);
    let (a, b) = (p._dir.path().join("a"), p._dir.path().join("b"));
    ok(&p.args("bloc", &a, &[]));
    ok(&p.args("sans_bloc", &b, &[]));
    assert_eq!(json(&a.join("metrics.json"))["schema"], "tv4-metrics-s11");
    assert_eq!(json(&b.join("metrics.json"))["schema"], "tv4-metrics-s9");
    assert_ne!(
        json(&a.join("manifest.json"))["run_id"],
        json(&b.join("manifest.json"))["run_id"]
    );
    // s11 contre s9 : jamais comparés comme identiques.
    let (code, texte) = verify(&a, &b);
    assert_eq!(code, Some(1), "{texte}");
}

#[test]
fn une_jambe_absente_du_dataset_est_refusee_en_code_1() {
    let p = protected(Block::Dev, true);
    let out = p._dir.path().join("out");
    let mut args = p.args("bloc", &out, &[]);
    let index = args
        .iter()
        .position(|a| a == "--symbols")
        .expect("--symbols");
    args[index + 1] = "BTC-USD,XRP-USD".to_string();
    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("XRP-USD"), "{texte}");
    assert!(!out.exists());
}

#[test]
fn trois_profils_et_deux_lignes_reservent_une_seule_lecture_et_trois_essais() {
    let p = protected(Block::Holdout, true);
    p.add_block("dev_seed", Block::Dev);
    let seed = p._dir.path().join("seed");
    ok(&p.args("dev_seed", &seed, &["--costs", &costs_file()]));
    let hashes = ["baseline", "adverse", "maker"]
        .iter()
        .map(|profile| {
            json(&seed.join(profile).join("metrics.json"))["config_hash"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let sub = p.subset(&["BTC"]);
    let subset = json(&sub)["subset_hash"].as_str().unwrap().to_owned();
    cli_research_common::freeze(&p.root, &hashes, &subset);
    ok(&strings(&[
        "research",
        "claim",
        "--research-root",
        &shown(&p.root),
        "--hypothesis",
        "H-001",
    ]));
    let out = p._dir.path().join("out");
    ok(&p.args(
        "bloc",
        &out,
        &["--costs", &costs_file(), "--okx-subset", &shown(&sub)],
    ));
    let registry =
        tv4::research::registry::Registry::from_csv(&read(&p.root.join("research/trials.csv")))
            .unwrap();
    assert_eq!(registry.n(), 3);
    assert_eq!(registry.reads().len(), 2);
    assert_eq!(registry.claims().len(), 3);
    let holdout = registry
        .reads()
        .values()
        .find(|r| r.block == Some(Block::Holdout))
        .unwrap();
    assert_eq!(holdout.config_hashes.len(), 3);
    assert!(holdout.is_complete());
    for (i, profile) in ["baseline", "adverse", "maker"].iter().enumerate() {
        for line in ["full", "okx_usdc"] {
            let manifest = json(&out.join(line).join(profile).join("manifest.json"));
            assert_eq!(manifest["governance"]["noise"]["n"], 3);
            assert_eq!(manifest["governance"]["read_id"], holdout.id.0);
            assert_eq!(manifest["config_hash"], hashes[i]);
        }
    }
    // Les métriques du registre sont celles de full, chaque profil une seule fois.
    for hash in hashes {
        let event = registry
            .events()
            .iter()
            .find(|e| {
                e.read_id == Some(holdout.id)
                    && e.config_hash.as_str() == hash
                    && e.status == tv4::research::registry::Status::Completed
            })
            .unwrap();
        let name = ["baseline", "adverse", "maker"]
            .into_iter()
            .find(|name| {
                json(&out.join("full").join(name).join("metrics.json"))["config_hash"] == hash
            })
            .unwrap();
        assert_eq!(
            event.metrics.n,
            Some(
                json(&out.join("full").join(name).join("metrics.json"))["n_trades"]
                    .as_u64()
                    .unwrap()
            )
        );
    }
}

#[test]
fn tous_les_refus_proteges_precedent_parquet_et_conservent_le_prefixe() {
    for case in ["tag", "fiche", "hash", "claim", "subset", "profils"] {
        let p = protected(Block::Holdout, true);
        p.add_block("dev_seed", Block::Dev);
        let seed = p._dir.path().join("seed");
        ok(&p.args("dev_seed", &seed, &[]));
        let hash = json(&seed.join("metrics.json"))["config_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let sub = p.subset(&["BTC"]);
        let subset = json(&sub)["subset_hash"].as_str().unwrap().to_owned();
        cli_research_common::freeze(&p.root, &[hash], &subset);
        let mut extra = vec!["--okx-subset".to_owned(), shown(&sub)];
        match case {
            "tag" => cli_research_common::git(&p.root, &["tag", "-d", "frozen/H-001"]),
            "fiche" => {
                let path = p.root.join("research/H-001.md");
                let mut text = fs::read_to_string(&path).unwrap();
                text.push('\n');
                fs::write(path, text).unwrap();
            }
            "hash" => {
                fs::write(
                    &p.params,
                    "tf = \"15m\"\nfast = 6\nslow = 20\nstop_atr_mult = 2.0\n",
                )
                .unwrap();
            }
            "subset" => {
                let other = p.subset(&["BTC", "ETH"]);
                extra[1] = shown(&other);
            }
            "profils" => extra.extend(["--costs".into(), costs_file()]),
            "claim" => {}
            _ => unreachable!(),
        }
        let csv = p.root.join("research/trials.csv");
        let prefix = read(&csv);
        fs::remove_dir_all(p.root.join("data")).unwrap();
        let out = p._dir.path().join("out");
        let refs = extra.iter().map(String::as_str).collect::<Vec<_>>();
        let (code, text) = run(&p.args("bloc", &out, &refs));
        assert_eq!(code, Some(2), "{case}: {text}");
        assert!(!text.contains("parquet"), "{case}: {text}");
        assert_eq!(read(&csv), prefix, "{case}");
        assert!(!out.exists());
    }
}

fn seed_rotation(root: &Path, seed_registry: &tv4::research::registry::Registry) -> String {
    use tv4::research::registry::{
        Admission, AdmissionTime, ConfigSpec, Digest, Registry, UtcDate, encode_events, header_csv,
    };
    let (hash, trial) = seed_registry.trials().first_key_value().unwrap();
    let config = ConfigSpec {
        config_hash: hash.clone(),
        family: trial.family.clone(),
        hypothesis: trial.hypothesis.clone(),
    };
    let date = UtcDate::new("2024-01-01T00:00:00Z").unwrap();
    let admission = Admission {
        configs: vec![config.clone()],
        dataset_hash: Digest::new("b".repeat(64)).unwrap(),
        block: Some(Block::Dev),
        time: AdmissionTime::new(date.clone(), date.quarter()).unwrap(),
        generation: 0,
        decision_start_ms: Some(Block::Dev.span().start_ms),
        decision_end_ms: Block::Dev.span().end_ms,
    };
    let mut reg = Registry::default()
        .with_events(
            &Registry::default()
                .propose_admission(&admission)
                .unwrap()
                .events,
        )
        .unwrap();
    // Pré-histoire synthétique : trois lectures de la génération initiale, sans résultats.
    for date in [
        "2025-01-01T00:00:00Z",
        "2025-04-01T00:00:00Z",
        "2025-07-01T00:00:00Z",
    ] {
        let date = UtcDate::new(date).unwrap();
        reg = reg
            .with_events(
                &reg.propose_claims(
                    std::slice::from_ref(hash),
                    config.hypothesis.as_ref().unwrap(),
                    &date,
                )
                .unwrap(),
            )
            .unwrap();
        let request = Admission {
            configs: vec![config.clone()],
            dataset_hash: Digest::new("b".repeat(64)).unwrap(),
            block: Some(Block::Holdout),
            time: AdmissionTime::new(date.clone(), date.quarter()).unwrap(),
            generation: 0,
            decision_start_ms: Some(Block::Holdout.span().start_ms),
            decision_end_ms: Block::Holdout.span().end_ms,
        };
        reg = reg
            .with_events(&reg.propose_admission(&request).unwrap().events)
            .unwrap();
    }
    let mut csv = header_csv();
    csv.extend(encode_events(reg.events()).unwrap());
    fs::write(root.join("research/trials.csv"), csv).unwrap();
    hash.to_string()
}

#[test]
fn rotation_relabelle_holdout_et_ne_joue_que_le_forward_postgel() {
    use tv4::research::registry::Registry;
    let p = protected(Block::Holdout, true);
    p.add_block("dev_seed", Block::Dev);
    let seed = p._dir.path().join("seed");
    ok(&p.args("dev_seed", &seed, &[]));
    let seed_registry = Registry::from_csv(&read(&p.root.join("research/trials.csv"))).unwrap();
    let hash = seed_rotation(&p.root, &seed_registry);
    let sub = p.subset(&["BTC"]);
    let subset = json(&sub)["subset_hash"].as_str().unwrap().to_owned();
    cli_research_common::freeze(&p.root, std::slice::from_ref(&hash), &subset);
    let old = p._dir.path().join("ancien");
    ok(&p.args("bloc", &old, &["--okx-subset", &shown(&sub)]));
    let old_manifest = json(&old.join("full/manifest.json"));
    assert_eq!(old_manifest["governance"]["source_block"], "HOLDOUT");
    assert_eq!(old_manifest["governance"]["effective_block"], "VALIDATION");
    assert_eq!(
        json(&old.join("full/metrics.json"))["research"]["block"],
        "VALIDATION"
    );
    let frozen_at = old_manifest["governance"]["frozen_at"].as_str().unwrap();
    let frozen_ms = tv4::store::time::parse_rfc3339_utc(frozen_at).unwrap();
    let (year, month, _) = tv4::store::time::civil_from_days(frozen_ms.div_euclid(86_400_000));
    let quarter = (month - 1) / 3;
    let first_month = quarter * 3 + 4;
    let (first_year, first_month) = if first_month > 12 {
        (year + 1, 1)
    } else {
        (year, first_month)
    };
    let (end_year, end_month) = if first_month == 10 {
        (first_year + 1, 1)
    } else {
        (first_year, first_month + 3)
    };
    let decision_start = tv4::store::time::days_from_civil(first_year, first_month, 1) * 86_400_000;
    let decision_end = tv4::store::time::days_from_civil(end_year, end_month, 1) * 86_400_000;
    let symbols = write_series(&p.root, decision_start);
    build_dataset_research(
        &p.root,
        &MultiBuildRequest {
            name: "renouvele",
            exchange: ExchangeId::Kraken,
            symbols: &symbols,
            tf: Timeframe::M1,
            window: Window::new(Block::Forward.span().start_ms, decision_end).unwrap(),
            gaps_policy: GapsPolicy::ZeroVolumeMarked,
        },
        &ResearchMeta {
            block: Block::Forward,
            universe: None,
            universe_quality: None,
        },
        FUTUR,
    )
    .unwrap();
    ok(&strings(&[
        "research",
        "claim",
        "--research-root",
        &shown(&p.root),
        "--hypothesis",
        "H-001",
    ]));
    let out = p._dir.path().join("nouveau");
    ok(&p.args("renouvele", &out, &["--okx-subset", &shown(&sub)]));
    let manifest = json(&out.join("full/manifest.json"));
    let metrics = json(&out.join("full/metrics.json"));
    assert_eq!(manifest["governance"]["source_block"], "FORWARD");
    assert_eq!(manifest["governance"]["effective_block"], "HOLDOUT");
    assert_eq!(manifest["governance"]["generation"], 1);
    assert_eq!(metrics["research"]["block"], "HOLDOUT");
    assert_eq!(metrics["research"]["identity"]["block"], "HOLDOUT");
    assert_eq!(
        metrics["research"]["decision_window"]["start_ms"],
        decision_start
    );
    assert_eq!(
        metrics["research"]["decision_window"]["end_ms"],
        decision_end
    );
    assert_eq!(metrics["config_hash"], hash.to_string());
    let reg = Registry::from_csv(&read(&p.root.join("research/trials.csv"))).unwrap();
    let last = reg.reads().last_key_value().unwrap().1;
    assert_eq!(last.decision_start_ms, Some(decision_start));
    assert_eq!(last.decision_end_ms, Some(decision_end));
    assert!(last.is_complete());
    let paths = [
        p.root.join("research/trials.csv"),
        p.root.join("research/README.md"),
    ];
    let before = paths.each_ref().map(|path| read(path));
    for (dir, expected) in [(&old, "validation"), (&out, "holdout")] {
        ok(&strings(&[
            "stats",
            &shown(dir),
            "--seed",
            "101",
            "--research-root",
            &shown(&p.root),
        ]));
        let stats = json(&dir.join("full/stats.json"));
        assert_eq!(stats["partition"], expected);
        assert_eq!(
            stats["benchmarks"]["btc"]["monthly"]["lb95_excess"]["availability"],
            "unavailable"
        );
        assert_eq!(
            stats["benchmarks"]["btc"]["monthly"]["lb95_excess"]["value"],
            Value::Null
        );
    }
    assert_eq!(before, paths.each_ref().map(|path| read(path)));
    // Une ancienne source sans admission S12 ne peut emprunter cette rotation.
    let path = old.join("full/manifest.json");
    let original = read(&path);
    let mut legacy = json(&path);
    legacy.as_object_mut().unwrap().remove("governance");
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let (code, text) = run(&strings(&[
        "stats",
        &shown(&old.join("full")),
        "--seed",
        "101",
        "--research-root",
        &shown(&p.root),
    ]));
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("governance"), "{text}");
    fs::write(path, original).unwrap();
    assert_eq!(before, paths.each_ref().map(|path| read(path)));
}

#[test]
fn lecture_protegee_interrompue_consomme_claim_et_quota_sans_remboursement() {
    let p = protected(Block::Holdout, true);
    p.add_block("dev_seed", Block::Dev);
    let seed = p._dir.path().join("seed");
    ok(&p.args("dev_seed", &seed, &[]));
    let hash = json(&seed.join("metrics.json"))["config_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let sub = p.subset(&["BTC"]);
    let subset = json(&sub)["subset_hash"].as_str().unwrap().to_owned();
    cli_research_common::freeze(&p.root, &[hash], &subset);
    ok(&strings(&[
        "research",
        "claim",
        "--research-root",
        &shown(&p.root),
        "--hypothesis",
        "H-001",
    ]));
    fs::remove_dir_all(p.root.join("data")).unwrap();
    let out = p._dir.path().join("out");
    let args = p.args("bloc", &out, &["--okx-subset", &shown(&sub)]);
    let (code, text) = run(&args);
    assert_eq!(code, Some(1), "{text}");
    assert!(!out.exists());
    let csv = p.root.join("research/trials.csv");
    let prefix = read(&csv);
    let registry = tv4::research::registry::Registry::from_csv(&prefix).unwrap();
    assert_eq!(registry.n(), 1);
    assert_eq!(registry.reads().len(), 2);
    assert!(
        registry
            .claims()
            .values()
            .all(|claim| claim.consumed_by.is_some())
    );
    assert_eq!(
        registry
            .events()
            .iter()
            .filter(|e| e.status == tv4::research::registry::Status::Failed)
            .count(),
        1
    );
    let (code, text) = run(&args);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("quota HOLDOUT"), "{text}");
    assert_eq!(read(&csv), prefix);
}

#[test]
fn rotation_quarterly_reduit_union_et_recalcule_window_hash_sans_changer_config() {
    use std::collections::BTreeMap;
    use tv4::research::registry::Registry;
    use tv4::store::time::{civil_from_days, days_from_civil, format_rfc3339_utc};
    use tv4::store::universe::{
        InstrumentRule, QuarterBuild, UnionEntry, UniverseBuild, load, write_universe,
    };
    let p = protected(Block::Forward, false);
    let world = World::with_universe();
    let source = load(&world.universe).unwrap();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&p.root)
        .args(["show", "-s", "--format=%ct", "HEAD"])
        .output()
        .unwrap();
    let seconds = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse::<i64>()
        .unwrap();
    let (year, month, _) = civil_from_days(seconds.div_euclid(86_400));
    let next_month = ((month - 1) / 3) * 3 + 4;
    let (year, month) = if next_month > 12 {
        (year + 1, 1)
    } else {
        (year, next_month)
    };
    let decision_start = days_from_civil(year, month, 1) * 86_400_000;
    let (end_year, end_month) = if month == 10 {
        (year + 1, 1)
    } else {
        (year, month + 3)
    };
    let decision_end = days_from_civil(end_year, end_month, 1) * 86_400_000;
    let mut quarters = Vec::new();
    let mut cursor = Block::Forward.span().start_ms;
    let old_rows = source.quarters()[0]
        .rows
        .iter()
        .filter(|r| r.base == "BTC" || r.base == "ETH")
        .cloned()
        .collect::<Vec<_>>();
    while cursor < decision_end {
        let (year, month, _) = civil_from_days(cursor.div_euclid(86_400_000));
        let rows = old_rows
            .iter()
            .filter(|row| cursor < decision_start || row.base == "BTC")
            .cloned()
            .collect::<Vec<_>>();
        quarters.push(QuarterBuild {
            quarter: format!("{year}-Q{}", (month - 1) / 3 + 1),
            eligible: rows.len() as u32,
            quality_hashes: rows
                .iter()
                .map(|row| (row.symbol.clone(), row.input_hash.clone()))
                .collect::<BTreeMap<_, _>>(),
            rows,
        });
        let (year, month) = if month == 10 {
            (year + 1, 1)
        } else {
            (year, month + 3)
        };
        cursor = days_from_civil(year, month, 1) * 86_400_000;
    }
    let mut source_info = source.file().source.clone();
    source_info.coverage_end = format_rfc3339_utc(decision_end).unwrap();
    let universe = p._dir.path().join("quarterly");
    write_universe(
        &universe,
        &UniverseBuild {
            source: source_info,
            rules: source.file().rules.clone(),
            quarters,
            at: vec![],
            rejected_pairs: vec![],
            unrecognized_pairs: vec![],
            union: old_rows
                .iter()
                .map(|row| UnionEntry {
                    symbol: row.symbol.clone(),
                    kraken_pair: row.kraken_pair.clone(),
                    first_quarter: "2026-Q3".into(),
                })
                .collect(),
            instruments: old_rows
                .iter()
                .map(|row| {
                    let rules = tv4::model::InstrumentRules::from_toml_str(
                        ExchangeId::Kraken,
                        &row.symbol,
                        source.instruments_text(),
                    )
                    .unwrap();
                    InstrumentRule {
                        symbol: row.symbol.clone(),
                        tick_size: rules.tick_size,
                        lot_size: rules.lot_size,
                        min_qty: rules.min_qty,
                        min_notional: rules.min_notional,
                    }
                })
                .collect(),
            instruments_method: "decimals_v1".into(),
        },
    )
    .unwrap();
    let loaded = load(&universe).unwrap();
    let symbols = write_series(&p.root, decision_start);
    build_dataset_research(
        &p.root,
        &MultiBuildRequest {
            name: "quarterly",
            exchange: ExchangeId::Kraken,
            symbols: &symbols,
            tf: Timeframe::M1,
            window: Window::new(Block::Forward.span().start_ms, decision_end).unwrap(),
            gaps_policy: GapsPolicy::ZeroVolumeMarked,
        },
        &ResearchMeta {
            block: Block::Forward,
            universe: Some(
                loaded
                    .reference(Block::Forward.span().start_ms, decision_end)
                    .unwrap(),
            ),
            universe_quality: None,
        },
        FUTUR,
    )
    .unwrap();
    let args_for = |out: &Path, subset: Option<&Path>| {
        let mut args = p.args("quarterly", out, &[]);
        for option in ["--symbols", "--instruments-file"] {
            let index = args.iter().position(|arg| arg == option).unwrap();
            args.drain(index..index + 2);
        }
        args.extend(["--universe".into(), shown(&universe)]);
        if let Some(subset) = subset {
            args.extend(["--okx-subset".into(), shown(subset)]);
        }
        args
    };
    let seed = p._dir.path().join("seed");
    ok(&args_for(&seed, None));
    let seed_metrics = json(&seed.join("metrics.json"));
    let seed_registry = Registry::from_csv(&read(&p.root.join("research/trials.csv"))).unwrap();
    let hash = seed_rotation(&p.root, &seed_registry);
    let sub = p.subset(&["BTC"]);
    let subset = json(&sub)["subset_hash"].as_str().unwrap().to_owned();
    cli_research_common::freeze(&p.root, std::slice::from_ref(&hash), &subset);
    ok(&strings(&[
        "research",
        "claim",
        "--research-root",
        &shown(&p.root),
        "--hypothesis",
        "H-001",
    ]));
    let out = p._dir.path().join("out");
    ok(&args_for(&out, Some(&sub)));
    let metrics = json(&out.join("full/metrics.json"));
    let manifest = json(&out.join("full/manifest.json"));
    assert_eq!(metrics["config_hash"], hash);
    assert_eq!(manifest["governance"]["effective_block"], "HOLDOUT");
    assert_eq!(
        metrics["research"]["identity"]["universe_window_hash"],
        loaded.window_hash(decision_start, decision_end).unwrap()
    );
    assert_ne!(
        metrics["research"]["identity"]["universe_window_hash"],
        seed_metrics["research"]["identity"]["universe_window_hash"]
    );
    assert_eq!(
        json(&out.join("lines.json"))["lines"][0]["legs"],
        serde_json::json!(["BTC-USD"])
    );
    assert_eq!(
        json(&out.join("lines.json"))["lines"][1]["legs"],
        serde_json::json!(["BTC-USD"])
    );
}

fn frozen_holdout_for_process_test() -> (Protected, PathBuf) {
    let p = protected(Block::Holdout, true);
    p.add_block("dev_seed", Block::Dev);
    let seed = p._dir.path().join("seed");
    ok(&p.args("dev_seed", &seed, &[]));
    let hash = json(&seed.join("metrics.json"))["config_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let sub = p.subset(&["BTC"]);
    let subset = json(&sub)["subset_hash"].as_str().unwrap().to_owned();
    cli_research_common::freeze(&p.root, &[hash], &subset);
    ok(&strings(&[
        "research",
        "claim",
        "--research-root",
        &shown(&p.root),
        "--hypothesis",
        "H-001",
    ]));
    (p, sub)
}

#[test]
fn deux_processus_ne_consomment_pas_le_meme_claim_et_quota_holdout() {
    let (p, sub) = frozen_holdout_for_process_test();
    let launch = |name: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_tv4"))
            .args(p.args(
                "bloc",
                &p._dir.path().join(name),
                &["--okx-subset", &shown(&sub)],
            ))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut a = launch("a");
    let mut b = launch("b");
    let mut codes = [
        a.wait().unwrap().code().unwrap(),
        b.wait().unwrap().code().unwrap(),
    ];
    codes.sort();
    assert_eq!(codes, [0, 2]);
    let reg =
        tv4::research::registry::Registry::from_csv(&read(&p.root.join("research/trials.csv")))
            .unwrap();
    assert_eq!(
        reg.reads()
            .values()
            .filter(|r| r.block == Some(Block::Holdout))
            .count(),
        1
    );
    assert_eq!(
        reg.claims()
            .values()
            .filter(|c| c.consumed_by.is_some())
            .count(),
        1
    );
    assert!(reg.pending_reads().is_empty());
}

#[test]
fn stats_existing_protected_run_never_consumes_another_claim_or_counter() {
    let (p, sub) = frozen_holdout_for_process_test();
    let out = p._dir.path().join("already-admitted");
    ok(&p.args("bloc", &out, &["--okx-subset", &shown(&sub)]));
    let paths = [
        p.root.join("research/trials.csv"),
        p.root.join("research/README.md"),
    ];
    let before = paths.each_ref().map(|path| read(path));
    ok(&strings(&[
        "stats",
        &shown(&out),
        "--seed",
        "101",
        "--research-root",
        &shown(&p.root),
    ]));
    assert_eq!(before, paths.each_ref().map(|path| read(path)));
    assert_eq!(
        json(&out.join("okx_usdc/stats.json"))["partition"],
        "holdout"
    );
}
