//! Layouts S11 et conservation des compteurs sur backtests techniques explicites.
mod cli_research_common;
mod common;
use cli_research_common::{World, json, ok, run, strings};
use std::{fs, path::Path};
fn image(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    ["research/trials.csv", "research/README.md"]
        .iter()
        .filter_map(|f| {
            let p = root.join(f);
            fs::read(&p).ok().map(|b| (p, b))
        })
        .collect()
}
#[test]
fn all_layouts_delay_and_registry_snapshot_are_read_only() {
    let w = World::with_dataset();
    let subset = w.write_subset("sub.json", &["BTC", "SOL"]);
    let costs = cli_research_common::repo("config/costs.toml");
    let mut runs = Vec::new();
    for mode in ["research", "lines", "lines-profiles"] {
        let out = w.root.join(mode);
        let delayed = w.root.join(format!("{mode}-delayed"));
        let mut extra = Vec::new();
        if mode != "research" {
            extra.extend(["--okx-subset", subset.to_str().unwrap()]);
        }
        if mode == "lines-profiles" {
            extra.extend(["--costs", costs.to_str().unwrap()]);
        }
        ok(&w.backtest_args(&out, &extra));
        let mut delayed_extra = extra.clone();
        delayed_extra.extend(["--entry-delay-bars", "1"]);
        ok(&w.backtest_args(&delayed, &delayed_extra));
        let before = image(&w.root);
        ok(&strings(&[
            "stats",
            out.to_str().unwrap(),
            "--seed",
            "8",
            "--research-root",
            w.root.to_str().unwrap(),
            "--delayed-run",
            delayed.to_str().unwrap(),
        ]));
        assert_eq!(before, image(&w.root));
        let leaf = if mode == "research" {
            out.clone()
        } else if mode == "lines" {
            out.join("full")
        } else {
            out.join("full/baseline")
        };
        let j = json(&leaf.join("stats.json"));
        assert_eq!(j["delayed_entries"]["availability"], "complete");
        assert!(
            !j["delayed_entries"]["value"]["eligible_dev_g2"]
                .as_bool()
                .unwrap()
        );
        assert!(j["registry"]["value"]["declared_n"].as_u64().unwrap() > 0);
        runs.push(out);
    }
    // Fournir un root de peers à deux lignes à une feuille full ne doit prendre que full.
    let target = runs[1].join("full");
    let (code, text) = run(&strings(&[
        "stats",
        target.to_str().unwrap(),
        "--seed",
        "9",
        "--research-root",
        w.root.to_str().unwrap(),
        "--peer-run",
        runs[2].to_str().unwrap(),
    ]));
    assert_eq!(code, Some(0), "{text}");
    let j = json(&target.join("stats.json"));
    for member in j["corpus"]["members"].as_array().unwrap() {
        assert_eq!(member["source"]["provenance"]["line"], "full");
    }
    if std::env::var_os("TV4_S13_RETAIN_FIXTURES").is_some() {
        println!("S13_RESEARCH_FIXTURE={}", w._dir.keep().display());
    }
}
