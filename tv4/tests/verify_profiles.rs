//! `tv4 verify` sur les dossiers de profils et sur la famille s10 (docs/specs/s10.md §12.5 et §16,
//! lot E). Les dossiers sont écrits par la bibliothèque (`profiles_common`) ; `tv4 verify` est le vrai
//! binaire, pour les codes de sortie : 0 identiques, 3 différents, 1 illisible, incohérent ou non
//! comparable.
//!
//! Marché inventé : marche synthétique, aucune donnée réelle.

mod common;
mod portfolio_common;
mod profiles_common;
mod report_s9_common;

use std::fs;
use std::path::{Path, PathBuf};

use profiles_common::{FAST_PARAMS, costs_file, dataset_info, render_all, run, scenario};
use report_s9_common::{strings, tv4};
use serde_json::Value;
use tv4::engine::{PortfolioInputs, run_portfolio};
use tv4::model::Timeframe;
use tv4::report::manifest::{ManifestS9Inputs, OutputFiles, Timings};
use tv4::report::metrics_s9::{InstrumentsInfo, MetricsS9, MetricsS9Inputs};
use tv4::report::profiles::RenderedProfile;
use tv4::report::{ManifestS9, write_json_atomic};

const ROWS: usize = 6_000;

fn rendered(seed: u64) -> Vec<RenderedProfile> {
    let data = scenario(ROWS);
    let file = costs_file();
    let result = run(&data, &file, seed, FAST_PARAMS);
    render_all(&result, &data, &dataset_info(&data.series), &file)
}

/// Écrit un dossier de profils complet dans un sous-dossier `name` de `root`.
fn profiles_dir(root: &Path, name: &str, rendered: &[RenderedProfile]) -> PathBuf {
    let dir = root.join(name);
    profiles_common::write_profiles_dir(&dir, rendered, &costs_file());
    dir
}

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

#[test]
fn deux_dossiers_de_profils_identiques_donnent_0() {
    let root = tempfile::tempdir().unwrap();
    let profiles = rendered(1);
    let a = profiles_dir(root.path(), "a", &profiles);
    let b = profiles_dir(root.path(), "b", &profiles);
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("profil baseline : identique"), "{out}");
    assert!(out.contains("profil adverse : identique"), "{out}");
    assert!(out.contains("profil maker : identique"), "{out}");
    assert!(out.contains("profiles.json : identique"), "{out}");
    assert!(out.contains("dossiers de profils identiques"), "{out}");
}

#[test]
fn deux_runs_independants_de_meme_graine_sont_identiques() {
    // Pas la même `ProfilesResult` rendue deux fois : deux exécutions complètes du moteur.
    let root = tempfile::tempdir().unwrap();
    let a = profiles_dir(root.path(), "a", &rendered(1));
    let b = profiles_dir(root.path(), "b", &rendered(1));
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(0), "{out}\n{err}");
}

/// Remplace la première occurrence de `from` par `to` dans un fichier.
fn patch_file(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "{from} absent de {}", path.display());
    fs::write(path, text.replacen(from, to, 1)).unwrap();
}

/// Remplace la dernière occurrence de `from` : les clés sont triées, donc la clé de premier niveau du
/// manifeste vient après celles de `config`.
fn patch_last(path: &Path, from: &str, to: &str) {
    let mut text = fs::read_to_string(path).unwrap();
    let at = text.rfind(from).unwrap_or_else(|| panic!("{from} absent"));
    text.replace_range(at..at + from.len(), to);
    fs::write(path, text).unwrap();
}

#[test]
fn un_trades_csv_de_maker_modifie_donne_3() {
    let root = tempfile::tempdir().unwrap();
    let good = rendered(1);
    let mut altered = good.clone();
    // Un chiffre du premier prix d'entrée du maker : le CSV reste valide, ses hashes sont refaits.
    let text = String::from_utf8(altered[2].trades_bytes.clone()).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert!(lines.len() > 1, "le profil maker n'a aucun trade");
    let header: Vec<&str> = lines[0].split(',').collect();
    let column = header.iter().position(|h| *h == "entry_price").unwrap();
    let mut cells: Vec<String> = lines[1].split(',').map(str::to_string).collect();
    let price: f64 = cells[column].parse().unwrap();
    cells[column] = format!("{}", price + 0.5);
    lines[1] = cells.join(",");
    altered[2].trades_bytes = (lines.join("\n") + "\n").into_bytes();

    let a = profiles_dir(root.path(), "a", &good);
    let b = profiles_dir(root.path(), "b", &altered);
    let (code, out, err) = verify(&a, &b);
    assert_eq!(code, Some(3), "{out}\n{err}");
    assert!(out.contains("profil baseline : identique"), "{out}");
    assert!(out.contains("profil adverse : identique"), "{out}");
    assert!(
        out.contains("profil maker : DIFFÉRENT (1 fichier(s))"),
        "{out}"
    );
    assert!(out.contains("trades.csv : DIFFÉRENT"), "{out}");
    // profiles.json porte le result_hash du maker : il diffère aussi.
    assert!(out.contains("profiles.json : DIFFÉRENT"), "{out}");
    assert!(out.contains("profils en écart : maker"), "{out}");
}

#[test]
fn deux_graines_differentes_donnent_3() {
    let root = tempfile::tempdir().unwrap();
    let a = profiles_dir(root.path(), "a", &rendered(1));
    let b = profiles_dir(root.path(), "b", &rendered(2));
    let (code, out, _) = verify(&a, &b);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("attention : configurations"), "{out}");
}

#[test]
fn un_dossier_de_profils_contre_un_run_donne_1() {
    let root = tempfile::tempdir().unwrap();
    let profiles = rendered(1);
    let a = profiles_dir(root.path(), "a", &profiles);
    let b = profiles_dir(root.path(), "b", &profiles);
    for (x, y) in [(&a, &b.join("baseline")), (&b.join("baseline"), &a)] {
        let (code, out, err) = verify(x, y);
        assert_eq!(code, Some(1), "{out}\n{err}");
        assert!(
            err.contains("un dossier de profils ne se compare qu'à un dossier de profils"),
            "{err}"
        );
    }
}

#[test]
fn un_profiles_json_absent_donne_1() {
    let root = tempfile::tempdir().unwrap();
    let profiles = rendered(1);
    let a = profiles_dir(root.path(), "a", &profiles);
    let b = profiles_dir(root.path(), "b", &profiles);
    fs::remove_file(b.join("profiles.json")).unwrap();
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    // Sans profiles.json des deux côtés, la racine n'est pas un run : pas de manifeste.
    fs::remove_file(a.join("profiles.json")).unwrap();
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("manifest.json"), "{err}");
}

#[test]
fn un_dossier_de_profils_incomplet_donne_1() {
    let profiles = rendered(1);
    for removed in [
        "maker/manifest.json",
        "adverse/manifest.json",
        "baseline/trades.csv",
    ] {
        let root = tempfile::tempdir().unwrap();
        let a = profiles_dir(root.path(), "a", &profiles);
        let b = profiles_dir(root.path(), "b", &profiles);
        fs::remove_file(b.join(removed)).unwrap();
        let (code, _, err) = verify(&a, &b);
        assert_eq!(code, Some(1), "{removed} : {err}");
        if removed.ends_with("manifest.json") {
            assert!(err.contains("incomplet"), "{removed} : {err}");
        }
    }
    // Un sous-dossier entier absent.
    let root = tempfile::tempdir().unwrap();
    let a = profiles_dir(root.path(), "a", &profiles);
    let b = profiles_dir(root.path(), "b", &profiles);
    fs::remove_dir_all(b.join("adverse")).unwrap();
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("incomplet"), "{err}");
}

#[test]
fn un_profiles_json_qui_ne_decrit_pas_les_runs_donne_1() {
    let profiles = rendered(1);
    let root = tempfile::tempdir().unwrap();
    let a = profiles_dir(root.path(), "a", &profiles);
    let run_id = read_json(&a.join("baseline/manifest.json"))["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let swapped = format!(
        "{}{}",
        &run_id[..63],
        if run_id.ends_with('0') { '1' } else { '0' }
    );

    // run_id d'un profil changé dans profiles.json.
    let b = profiles_dir(root.path(), "b", &profiles);
    patch_file(&b.join("profiles.json"), &run_id, &swapped);
    let (code, _, err) = verify(&a, &b);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("run_id"), "{err}");

    // Un `dir` qui sort du dossier.
    let c = profiles_dir(root.path(), "c", &profiles);
    patch_file(
        &c.join("profiles.json"),
        "\"dir\":\"baseline\"",
        "\"dir\":\"../a/baseline\"",
    );
    let (code, _, err) = verify(&a, &c);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("dir"), "{err}");

    // Schéma de profiles.json inconnu.
    let d = profiles_dir(root.path(), "d", &profiles);
    patch_file(
        &d.join("profiles.json"),
        "tv4-profiles-v1",
        "tv4-profiles-v2",
    );
    let (code, _, err) = verify(&a, &d);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("tv4-profiles-v1"), "{err}");
}

#[test]
fn un_run_s10_dont_le_manifeste_contredit_les_metriques_donne_1() {
    let profiles = rendered(1);
    let root = tempfile::tempdir().unwrap();
    let a = profiles_dir(root.path(), "a", &profiles);
    let b = profiles_dir(root.path(), "b", &profiles);
    // Profil du manifeste différent de `metrics.costs.profile` ; les hashes de fichiers restent bons.
    patch_last(
        &b.join("baseline/manifest.json"),
        "\"profile\":\"baseline\"",
        "\"profile\":\"adverse\"",
    );
    let (code, _, err) = verify(&a.join("baseline"), &b.join("baseline"));
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("`adverse` dans le manifeste"), "{err}");

    // Hash du fichier de coûts différent entre manifeste et métriques.
    let c = profiles_dir(root.path(), "c", &profiles);
    let hash = read_json(&c.join("baseline/manifest.json"))["costs_file_hash"]
        .as_str()
        .unwrap()
        .to_string();
    patch_file(&c.join("baseline/manifest.json"), &hash, &"0".repeat(64));
    let (code, _, err) = verify(&a.join("baseline"), &c.join("baseline"));
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("costs_file_hash"), "{err}");
}

#[test]
fn deux_runs_s10_isoles_se_comparent_comme_deux_runs() {
    let root = tempfile::tempdir().unwrap();
    let profiles = rendered(1);
    let a = profiles_dir(root.path(), "a", &profiles);
    let b = profiles_dir(root.path(), "b", &profiles);
    let (code, out, err) = verify(&a.join("maker"), &b.join("maker"));
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("identiques à l'octet"), "{out}");
}

/// Un run s9 écrit par la bibliothèque (moteur hérité, `MetricsS9::new`, `ManifestS9::new`).
fn s9_run_dir(root: &Path) -> PathBuf {
    let data = scenario(ROWS);
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
    let dir = root.join("s9");
    let hash = |b: &[u8]| tv4::model::hash::blake3_prefixed(b);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("metrics.json"), &metrics_bytes).unwrap();
    fs::write(dir.join("trades.csv"), &trades_bytes).unwrap();
    fs::write(dir.join("equity.csv"), &equity_bytes).unwrap();
    let manifest = ManifestS9::new(
        &metrics,
        ManifestS9Inputs {
            instruments_file: None,
            engine: profiles_common::engine_info(),
            equity_step: Timeframe::H1,
            files: OutputFiles {
                equity_csv: hash(&equity_bytes),
                metrics_json: hash(&metrics_bytes),
                trades_csv: hash(&trades_bytes),
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
    write_json_atomic(&dir, "manifest.json", &manifest).unwrap();
    dir
}

#[test]
fn s9_contre_s10_donne_1() {
    let root = tempfile::tempdir().unwrap();
    let s9 = s9_run_dir(root.path());
    let a = profiles_dir(root.path(), "a", &rendered(1));
    // Le run s9 seul est lisible et se compare à lui-même.
    let (code, out, err) = verify(&s9, &s9);
    assert_eq!(code, Some(0), "{out}\n{err}");
    for other in [a.join("baseline"), a.join("maker")] {
        for (x, y) in [(&s9, &other), (&other, &s9)] {
            let (code, _, err) = verify(x, y);
            assert_eq!(code, Some(1), "{err}");
            assert!(err.contains("tv4-manifest-s9"), "{err}");
            assert!(err.contains("tv4-manifest-s10"), "{err}");
        }
    }
}
