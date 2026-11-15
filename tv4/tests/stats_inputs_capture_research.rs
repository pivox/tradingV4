//! Capture S13 en recherche, profils et changements de layout.
mod cli_research_common;
mod common;
mod stats_inputs_capture_common;
use cli_research_common::repo;
use stats_inputs_capture_common::check;
use std::fs;

#[test]
fn recherche_lignes_et_profils_capturent_les_purges() {
    let w = cli_research_common::World::with_dataset();
    let sub = w.write_subset("sub.json", &["BTC", "SOL"]);
    for costs in [false, true] {
        let out = w.root.join(if costs { "costs" } else { "single" });
        let mut extra = vec!["--okx-subset", sub.to_str().unwrap()];
        let costs_path = repo("config/costs.toml");
        if costs {
            extra.extend(["--costs", costs_path.to_str().unwrap()]);
        }
        cli_research_common::ok(&w.backtest_args(&out, &extra));
        for line in ["full", "okx_usdc"] {
            for profile in if costs {
                vec!["baseline", "adverse", "maker"]
            } else {
                vec![""]
            } {
                let p = check(&out.join(line).join(profile));
                assert_eq!(p.provenance.line.as_deref(), Some(line));
            }
        }
    }
}

#[test]
fn reemission_retire_les_paquets_et_stats_des_anciens_layouts() {
    let w = cli_research_common::World::with_dataset();
    let sub = w.write_subset("sub.json", &["BTC", "SOL"]);
    let costs = repo("config/costs.toml");
    let out = w.root.join("out");
    cli_research_common::ok(&w.backtest_args(
        &out,
        &[
            "--okx-subset",
            sub.to_str().unwrap(),
            "--costs",
            costs.to_str().unwrap(),
        ],
    ));
    let old = out.join("full/baseline");
    fs::write(old.join("stats.json"), "obsolete").unwrap();
    fs::write(old.join("report.html"), "obsolete").unwrap();
    cli_research_common::ok(&w.backtest_args(&out, &[]));
    check(&out);
    for file in [
        "stats-inputs.json",
        "stats.json",
        "report.html",
        "manifest.json",
    ] {
        assert!(!old.join(file).exists(), "ancien layout : {file}");
    }
    fs::write(out.join("stats.json"), "obsolete").unwrap();
    cli_research_common::ok(&w.backtest_args(&out, &["--costs", costs.to_str().unwrap()]));
    assert!(!out.join("stats-inputs.json").exists());
    assert!(!out.join("stats.json").exists());
    assert!(!out.join("manifest.json").exists());
    check(&out.join("baseline"));
}

#[test]
fn une_erreur_ecriture_ne_garde_pas_manifest_ni_paquet_ancien() {
    let w = cli_research_common::World::with_dataset();
    let out = w.root.join("out");
    cli_research_common::ok(&w.backtest_args(&out, &[]));
    check(&out);
    fs::write(out.join("stats.json"), "obsolete").unwrap();
    fs::remove_file(out.join("metrics.json")).unwrap();
    fs::create_dir(out.join("metrics.json")).unwrap();
    let (code, _) = cli_research_common::run(&w.backtest_args(&out, &[]));
    assert_eq!(code, Some(1));
    for file in [
        "manifest.json",
        "stats-inputs.json",
        "stats.json",
        "stats-inputs.json.tmp",
    ] {
        assert!(!out.join(file).exists(), "écriture échouée : {file}");
    }
}
