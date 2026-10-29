//! `tv4 verify` sur les runs de recherche s11, les dossiers de profils s11 et les dossiers de lignes
//! (docs/specs/s11.md §9.3 et §9.6, lot E). Les dossiers sont écrits par la bibliothèque
//! (`research_common`) ; `tv4 verify` est le vrai binaire, pour les codes de sortie : 0 identiques,
//! 3 différents, 1 illisible, incohérent ou non comparable.
//!
//! Marché **inventé** : trois jambes FORWARD synthétiques, aucune donnée réelle.

mod common;
mod portfolio_common;
mod profiles_common;
mod report_s9_common;
mod research_common;

use std::fs;
use std::path::{Path, PathBuf};

use profiles_common::{FAST_PARAMS, costs_file, dataset_info, scenario};
use report_s9_common::{strings, tv4};
use research_common::{Fixture, fixture};
use serde_json::Value;
use tv4::engine::{Line, PortfolioInputs, run_portfolio};
use tv4::model::{Timeframe, hash};
use tv4::report::manifest::{ManifestS9Inputs, OutputFiles, Timings};
use tv4::report::metrics_s9::{InstrumentsInfo, MetricsS9, MetricsS9Inputs};
use tv4::report::profiles::RenderedProfile;
use tv4::report::{ManifestS9, write_json_atomic};

fn verify(a: &Path, b: &Path) -> (Option<i32>, String, String) {
    let sortie = tv4(&strings(&[
        "verify",
        &a.display().to_string(),
        &b.display().to_string(),
    ]));
    (
        sortie.status.code(),
        String::from_utf8_lossy(&sortie.stdout).into_owned(),
        String::from_utf8_lossy(&sortie.stderr).into_owned(),
    )
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Remplace la première occurrence de `from` par `to` dans un fichier.
fn patch_file(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "{from} absent de {}", path.display());
    fs::write(path, text.replacen(from, to, 1)).unwrap();
}

/// Modifie `metrics.json` d'un run et refait son hash dans le manifeste : le run reste lisible, seule la
/// cohérence interne est en cause.
fn tamper_metrics(dir: &Path, from: &str, to: &str) {
    let path = dir.join("metrics.json");
    let before = hash::blake3_prefixed(&fs::read(&path).unwrap());
    patch_file(&path, from, to);
    let after = hash::blake3_prefixed(&fs::read(&path).unwrap());
    patch_file(&dir.join("manifest.json"), &before, &after);
}

/// Écrit un dossier de lignes dans `root/name` avec des runs de capital `cash`.
fn lines_dir(root: &Path, name: &str, fx: &Fixture, cash: f64) -> PathBuf {
    let dir = root.join(name);
    fx.write_lines_dir(&dir, cash);
    dir
}

/// Écrit un dossier de lignes dont la ligne `okx_usdc` est `okx`.
fn lines_dir_with(
    root: &Path,
    name: &str,
    fx: &Fixture,
    full: &RenderedProfile,
    okx: &RenderedProfile,
) -> PathBuf {
    let dir = root.join(name);
    fx.write_run(&dir.join("full"), full, Line::Full);
    fx.write_run(&dir.join("okx_usdc"), okx, Line::OkxUsdc);
    fx.lines_report(full, okx).write(&dir).unwrap();
    dir
}

#[test]
fn deux_dossiers_de_lignes_identiques_donnent_0() {
    let root = tempfile::tempdir().unwrap();
    let fx = fixture();
    let a = lines_dir(root.path(), "a", &fx, 300.0);
    // Deux exécutions complètes du moteur, pas le même rendu écrit deux fois.
    let b = lines_dir(root.path(), "b", &fixture(), 300.0);
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("ligne full : identique"), "{out}");
    assert!(out.contains("ligne okx_usdc : identique"), "{out}");
    assert!(out.contains("lines.json : identique"), "{out}");
    assert!(out.contains("dossiers de lignes identiques"), "{out}");
    // Une ligne se vérifie aussi comme un run isolé.
    let (code, out, err) = verify(&a.join("full"), &b.join("full"));
    assert_eq!(code, Some(0), "{out}\n{err}");
}

#[test]
fn un_trades_csv_de_okx_usdc_modifie_donne_3() {
    let root = tempfile::tempdir().unwrap();
    let fx = fixture();
    let full = fx.render(Line::Full, 300.0);
    let okx = fx.render(Line::OkxUsdc, 300.0);
    let mut altered = okx.clone();
    let text = String::from_utf8(altered.trades_bytes.clone()).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert!(lines.len() > 1, "la ligne okx_usdc n'a aucun trade");
    let header: Vec<&str> = lines[0].split(',').collect();
    let column = header.iter().position(|h| *h == "entry_price").unwrap();
    let mut cells: Vec<String> = lines[1].split(',').map(str::to_string).collect();
    let price: f64 = cells[column].parse().unwrap();
    cells[column] = format!("{}", price + 0.5);
    lines[1] = cells.join(",");
    altered.trades_bytes = (lines.join("\n") + "\n").into_bytes();

    let a = lines_dir_with(root.path(), "a", &fx, &full, &okx);
    let b = lines_dir_with(root.path(), "b", &fx, &full, &altered);
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(3), "{out}\n{err}");
    assert!(out.contains("ligne full : identique"), "{out}");
    assert!(out.contains("ligne okx_usdc : DIFFÉRENTE"), "{out}");
    assert!(out.contains("lignes en écart : okx_usdc"), "{out}");
}

#[test]
fn deux_capitaux_differents_donnent_1() {
    let root = tempfile::tempdir().unwrap();
    let fx = fixture();
    let a = lines_dir(root.path(), "a", &fx, 300.0);
    let b = lines_dir(root.path(), "b", &fx, 3_000.0);
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("capitaux initiaux différents"), "{err}");
}

#[test]
fn un_dossier_de_lignes_contre_un_run_ou_des_profils_donne_1() {
    let root = tempfile::tempdir().unwrap();
    let fx = fixture();
    let a = lines_dir(root.path(), "a", &fx, 300.0);
    let file = costs_file();
    let profiles = fx.render_profiles(Line::Full, &file, 42);
    let profiles_dir = root.path().join("profils");
    fx.write_profiles_dir(&profiles_dir, &profiles, &file, Line::Full);
    for other in [a.join("full"), profiles_dir.clone()] {
        for (x, y) in [(&a, &other), (&other, &a)] {
            let (code, out, err) = verify(x, y);
            assert_eq!(code, Some(1), "{out}\n{err}");
            assert!(
                err.contains("un dossier de lignes ne se compare qu'à un dossier de lignes"),
                "{err}"
            );
        }
    }
}

#[test]
fn un_lines_json_absent_donne_1() {
    let root = tempfile::tempdir().unwrap();
    let fx = fixture();
    let a = lines_dir(root.path(), "a", &fx, 300.0);
    let b = lines_dir(root.path(), "b", &fx, 300.0);
    fs::remove_file(b.join("lines.json")).unwrap();
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    // Sans lines.json des deux côtés, la racine n'est pas un run : pas de manifeste.
    fs::remove_file(a.join("lines.json")).unwrap();
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("manifest.json"), "{err}");
}

#[test]
fn un_dossier_de_lignes_incomplet_donne_1() {
    let fx = fixture();
    for removed in [
        "okx_usdc/manifest.json",
        "full/trades.csv",
        "full/equity.csv",
    ] {
        let root = tempfile::tempdir().unwrap();
        let a = lines_dir(root.path(), "a", &fx, 300.0);
        let b = lines_dir(root.path(), "b", &fx, 300.0);
        fs::remove_file(b.join(removed)).unwrap();
        let (code, _, err) = verify(&a, &b);
        assert_eq!(code, Some(1), "{removed} : {err}");
        if removed.ends_with("manifest.json") {
            assert!(err.contains("incomplet"), "{removed} : {err}");
        }
    }
    // Un sous-dossier entier absent.
    let root = tempfile::tempdir().unwrap();
    let a = lines_dir(root.path(), "a", &fx, 300.0);
    let b = lines_dir(root.path(), "b", &fx, 300.0);
    fs::remove_dir_all(b.join("okx_usdc")).unwrap();
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("incomplet"), "{err}");
}

#[test]
fn un_lines_json_qui_ne_decrit_pas_les_runs_donne_1() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let a = lines_dir(root.path(), "a", &fx, 300.0);
    let lines = read_json(&a.join("lines.json"));
    let run_id = lines["lines"][0]["run_id"].as_str().unwrap().to_string();
    let swapped = format!(
        "{}{}",
        &run_id[..63],
        if run_id.ends_with('0') { '1' } else { '0' }
    );

    // run_id d'une ligne changé dans lines.json.
    let b = lines_dir(root.path(), "b", &fx, 300.0);
    patch_file(&b.join("lines.json"), &run_id, &swapped);
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("run_id"), "{err}");

    // lines_id incohérent.
    let c = lines_dir(root.path(), "c", &fx, 300.0);
    let lines_id = lines["lines_id"].as_str().unwrap();
    patch_file(&c.join("lines.json"), lines_id, &"0".repeat(64));
    let (code, _, err) = verify(&a, &c);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("lines_id"), "{err}");

    // Un `dir` qui sort du dossier.
    let d = lines_dir(root.path(), "d", &fx, 300.0);
    patch_file(
        &d.join("lines.json"),
        "\"dir\":\"full\"",
        "\"dir\":\"../a/full\"",
    );
    let (code, _, err) = verify(&a, &d);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("dir"), "{err}");

    // config_hash de lines.json qui n'est pas celui des runs.
    let e = lines_dir(root.path(), "e", &fx, 300.0);
    let config_hash = lines["config_hash"]["single"].as_str().unwrap();
    patch_file(&e.join("lines.json"), config_hash, &"0".repeat(64));
    let (code, _, err) = verify(&a, &e);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("config_hash"), "{err}");

    // Schéma de lines.json inconnu.
    let g = lines_dir(root.path(), "g", &fx, 300.0);
    patch_file(&g.join("lines.json"), "tv4-lines-v1", "tv4-lines-v2");
    let (code, _, err) = verify(&a, &g);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("tv4-lines-v1"), "{err}");
}

#[test]
fn deux_lignes_qui_ne_portent_pas_leur_nom_donnent_1() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let a = lines_dir(root.path(), "a", &fx, 300.0);
    // Les sous-dossiers sont échangés : `full/` contient le run de okx_usdc.
    let b = lines_dir(root.path(), "b", &fx, 300.0);
    let tmp = root.path().join("tmp");
    fs::rename(b.join("full"), &tmp).unwrap();
    fs::rename(b.join("okx_usdc"), b.join("full")).unwrap();
    fs::rename(&tmp, b.join("okx_usdc")).unwrap();
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
}

#[test]
fn deux_runs_s11_isoles_se_comparent_comme_deux_runs() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let rendered = fx.render(Line::Full, 300.0);
    let a = root.path().join("a");
    let b = root.path().join("b");
    fx.write_run(&a, &rendered, Line::Full);
    fx.write_run(&b, &rendered, Line::Full);
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("identiques à l'octet"), "{out}");
    // Deux lignes différentes : fichiers différents (3) et information sur l'identité de recherche.
    let okx = root.path().join("okx");
    fx.write_run(&okx, &fx.render(Line::OkxUsdc, 300.0), Line::OkxUsdc);
    let (code, out, err) = verify(&a, &okx);
    assert_eq!(code, Some(3), "{out}\n{err}");
    assert!(out.contains("identités de recherche différentes"), "{out}");
}

#[test]
fn un_run_s11_dont_les_metriques_contredisent_le_manifeste_ou_le_bloc_research_donne_1() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let rendered = fx.render(Line::Full, 300.0);
    let good = root.path().join("good");
    fx.write_run(&good, &rendered, Line::Full);
    let metrics = read_json(&good.join("metrics.json"));
    let purged = metrics["research"]["purge"]["trades_purged"]
        .as_u64()
        .unwrap();
    let identity_hash = metrics["research"]["identity_hash"]
        .as_str()
        .unwrap()
        .to_string();

    let cases: [(&str, String, String, &str); 4] = [
        (
            "purge",
            format!("\"trades_purged\":{purged}"),
            format!("\"trades_purged\":{}", purged + 1),
            "end_of_window",
        ),
        (
            "identity_hash",
            identity_hash.clone(),
            format!("blake3:{}", "0".repeat(64)),
            "identity_hash",
        ),
        (
            "metrics_basis",
            "\"metrics_basis\":\"kept_trades\"".to_string(),
            "\"metrics_basis\":\"all_trades\"".to_string(),
            "metrics_basis",
        ),
        (
            "ligne",
            "\"line\":\"full\",\"metrics_basis\"".to_string(),
            "\"line\":\"okx_usdc\",\"metrics_basis\"".to_string(),
            "research.line",
        ),
    ];
    for (name, from, to, expected) in cases {
        let bad = root.path().join(format!("bad-{name}"));
        fx.write_run(&bad, &rendered, Line::Full);
        tamper_metrics(&bad, &from, &to);
        let (code, _, err) = verify(&good, &bad);
        assert_eq!(code, Some(1), "{name} : {err}");
        assert!(err.contains(expected), "{name} : {err}");
    }

    // run_id du manifeste qui ne se recalcule pas (identité de recherche comprise).
    let bad = root.path().join("bad-run-id");
    fx.write_run(&bad, &rendered, Line::Full);
    let manifest = read_json(&bad.join("manifest.json"));
    let run_id = manifest["run_id"].as_str().unwrap().to_string();
    patch_file(&bad.join("manifest.json"), &run_id, &"0".repeat(64));
    let (code, _, err) = verify(&good, &bad);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("run_id"), "{err}");
}

#[test]
fn un_run_s11_dont_l_entonnoir_perd_un_refus_de_recherche_donne_1() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let rendered = fx.render(Line::Full, 300.0);
    let good = root.path().join("good");
    fx.write_run(&good, &rendered, Line::Full);
    let metrics = read_json(&good.join("metrics.json"));
    let n = metrics["research"]["funnel"]["entries_excluded_quality"]
        .as_u64()
        .unwrap();
    // Le total change, pas les jambes : Σ jambes ≠ total.
    let bad = root.path().join("bad");
    fx.write_run(&bad, &rendered, Line::Full);
    let text = fs::read_to_string(bad.join("metrics.json")).unwrap();
    let from =
        format!("\"funnel\":{{\"entries_cancelled_universe\":0,\"entries_excluded_quality\":{n}");
    assert!(text.contains(&from), "forme du bloc research inattendue");
    tamper_metrics(
        &bad,
        &from,
        &format!(
            "\"funnel\":{{\"entries_cancelled_universe\":0,\"entries_excluded_quality\":{}",
            n + 1
        ),
    );
    let (code, _, err) = verify(&good, &bad);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("entries_excluded_quality"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Dossiers de profils s11
// ---------------------------------------------------------------------------------------------

#[test]
fn deux_dossiers_de_profils_s11_identiques_donnent_0() {
    let fx = fixture();
    let file = costs_file();
    let root = tempfile::tempdir().unwrap();
    let rendered = fx.render_profiles(Line::Full, &file, 42);
    let a = root.path().join("a");
    let b = root.path().join("b");
    fx.write_profiles_dir(&a, &rendered, &file, Line::Full);
    fx.write_profiles_dir(
        &b,
        &fx.render_profiles(Line::Full, &file, 42),
        &file,
        Line::Full,
    );
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("profil baseline : identique"), "{out}");
    assert!(out.contains("dossiers de profils identiques"), "{out}");
    // Deux graines : les profils diffèrent (3).
    let c = root.path().join("c");
    fx.write_profiles_dir(
        &c,
        &fx.render_profiles(Line::Full, &file, 43),
        &file,
        Line::Full,
    );
    let (code, out, _) = verify(&a, &c);
    assert_eq!(code, Some(3), "{out}");
}

#[test]
fn un_profil_s11_contredit_par_son_manifeste_donne_1() {
    let fx = fixture();
    let file = costs_file();
    let root = tempfile::tempdir().unwrap();
    let rendered = fx.render_profiles(Line::Full, &file, 42);
    let a = root.path().join("a");
    fx.write_profiles_dir(&a, &rendered, &file, Line::Full);
    let b = root.path().join("b");
    fx.write_profiles_dir(&b, &rendered, &file, Line::Full);
    let hash = read_json(&b.join("baseline/manifest.json"))["costs_file_hash"]
        .as_str()
        .unwrap()
        .to_string();
    patch_file(&b.join("baseline/manifest.json"), &hash, &"0".repeat(64));
    let (code, _, err) = verify(&a.join("baseline"), &b.join("baseline"));
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("costs_file_hash"), "{err}");
}

/// Un run s10 (profils de coûts hors recherche) et un run s9, écrits par la bibliothèque.
fn legacy_dirs(root: &Path) -> (PathBuf, PathBuf) {
    let data = scenario(6_000);
    let file = costs_file();
    let result = profiles_common::run(&data, &file, 42, FAST_PARAMS);
    let rendered = profiles_common::render_all(&result, &data, &dataset_info(&data.series), &file);
    let s10 = root.join("s10");
    profiles_common::write_profiles_dir(&s10, &rendered, &file);

    let result = run_portfolio(PortfolioInputs {
        series: &data.series,
        strategies: (0..2)
            .map(|_| profiles_common::sma_cross(FAST_PARAMS))
            .collect(),
        config: portfolio_common::config(&data.universe, &data.rules, 300.0),
    })
    .expect("run hérité");
    let metrics = MetricsS9::new(
        &result,
        MetricsS9Inputs {
            dataset: dataset_info(&data.series),
            instruments: InstrumentsInfo::toml(),
            okx_pairs: None,
            markets: profiles_common::markets(&data.series, &data.universe),
        },
    )
    .expect("metrics s9");
    let symbols: Vec<&str> = result.universe.iter().map(String::as_str).collect();
    let metrics_bytes = tv4::report::json_bytes(&metrics).unwrap();
    let trades_bytes = tv4::report::writers::trades_csv_legs(
        &result.trades,
        &result.trade_legs,
        &symbols,
        metrics.span_ms(),
    )
    .unwrap();
    let equity_bytes = tv4::report::equity::equity_csv(
        &result.grid_open_ms,
        &result.equity,
        result.initial_cash,
        Timeframe::H1,
    )
    .unwrap();
    let s9 = root.join("s9");
    fs::create_dir_all(&s9).unwrap();
    fs::write(s9.join("metrics.json"), &metrics_bytes).unwrap();
    fs::write(s9.join("trades.csv"), &trades_bytes).unwrap();
    fs::write(s9.join("equity.csv"), &equity_bytes).unwrap();
    let manifest = ManifestS9::new(
        &metrics,
        ManifestS9Inputs {
            instruments_file: None,
            engine: profiles_common::engine_info(),
            equity_step: Timeframe::H1,
            files: OutputFiles {
                equity_csv: hash::blake3_prefixed(&equity_bytes),
                metrics_json: hash::blake3_prefixed(&metrics_bytes),
                trades_csv: hash::blake3_prefixed(&trades_bytes),
            },
            started_at_ms: portfolio_common::T0,
            timings: Timings {
                load_ms: 1,
                run_ms: 2,
                cli_cold_ms: 3,
            },
        },
    )
    .expect("manifeste s9");
    write_json_atomic(&s9, "manifest.json", &manifest).unwrap();
    (s9, s10)
}

#[test]
fn s11_contre_s9_ou_s10_donne_1() {
    let fx = fixture();
    let file = costs_file();
    let root = tempfile::tempdir().unwrap();
    let (s9, s10) = legacy_dirs(root.path());
    let s11 = root.path().join("s11");
    fx.write_run(&s11, &fx.render(Line::Full, 300.0), Line::Full);
    let s11_profiles = root.path().join("s11-profils");
    fx.write_profiles_dir(
        &s11_profiles,
        &fx.render_profiles(Line::Full, &file, 42),
        &file,
        Line::Full,
    );
    // Un run s11 contre un run s9 ou s10, et un dossier de profils s11 contre un dossier s10.
    for (x, y) in [
        (s11.clone(), s9.clone()),
        (s9.clone(), s11.clone()),
        (s11.clone(), s10.join("baseline")),
        (s10.join("baseline"), s11_profiles.join("baseline")),
    ] {
        let (code, _, err) = verify(&x, &y);
        assert_eq!(code, Some(1), "{err}");
        assert!(
            err.contains("tv4-manifest-s11") || err.contains("tv4-manifest-s10"),
            "{err}"
        );
    }
    let (code, _, err) = verify(&s11_profiles, &s10);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("tv4-manifest-s1"), "{err}");
    // Un run s11 de profil se lit comme un run isolé.
    let (code, out, err) = verify(&s11_profiles.join("maker"), &s11_profiles.join("maker"));
    assert_eq!(code, Some(0), "{out}\n{err}");
}

#[test]
fn un_dossier_de_lignes_de_profils_s11_se_verifie_ligne_a_ligne() {
    let fx = fixture();
    let file = costs_file();
    let root = tempfile::tempdir().unwrap();
    let write = |name: &str, seed: u64| {
        let dir = root.path().join(name);
        let full = fx.render_profiles(Line::Full, &file, seed);
        let okx = fx.render_profiles(Line::OkxUsdc, &file, seed);
        fx.write_profiles_dir(&dir.join("full"), &full, &file, Line::Full);
        fx.write_profiles_dir(&dir.join("okx_usdc"), &okx, &file, Line::OkxUsdc);
        tv4::report::research::LinesReport::build(
            &research_common::profiles_line_input(Line::Full, &full),
            &research_common::profiles_line_input(Line::OkxUsdc, &okx),
            &fx.subset,
        )
        .unwrap()
        .write(&dir)
        .unwrap();
        dir
    };
    let a = write("a", 42);
    let b = write("b", 42);
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("ligne full : identique"), "{out}");
    assert!(out.contains("profil baseline : identique"), "{out}");
    assert!(out.contains("dossiers de lignes identiques"), "{out}");
    let c = write("c", 43);
    let (code, out, _) = verify(&a, &c);
    assert_eq!(code, Some(3), "{out}");
}
