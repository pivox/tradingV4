mod common;
mod report_s9_common;
use report_s9_common::{build_dataset, candles, repo, strings, tv4};
use std::fs;
use tv4::report::stats;
#[test]
fn all_cli_constructors_resolve_one_real_decision_tf_and_preserve_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_dataset(
        root,
        "eth",
        &[(
            "ETH-USD",
            candles(6000, 22, 0., 150., common::synthetic_candles),
        )],
        6100,
    );
    let params = root.join("params.toml");
    fs::write(
        &params,
        "tf = \"15m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0\n[trend]\ntf = \"1h\"\nsma = 5\n",
    )
    .unwrap();
    let inst = root.join("instruments.toml");
    fs::write(
        &inst,
        "[kraken.ETH-USD]\ntick_size = 0.01\nlot_size = 1e-6\nmin_qty = 0.0\nmin_notional = 0.0\n",
    )
    .unwrap();
    for mode in ["cell", "portfolio", "profiles"] {
        let mut hashes = Vec::new();
        let mut runs = Vec::new();
        let mut dirs = Vec::new();
        for delay in [None, Some("0"), Some("1")] {
            let out = root.join(format!("{mode}-{delay:?}"));
            let mut args = strings(&[
                "backtest",
                "--dataset",
                "eth",
                "--root",
                root.to_str().unwrap(),
                "--strategy",
                "sma_cross",
                "--params",
                params.to_str().unwrap(),
                "--instruments-file",
                inst.to_str().unwrap(),
                "--fees-file",
                repo("config/fees.toml").to_str().unwrap(),
                "--out",
                out.to_str().unwrap(),
            ]);
            if mode != "cell" {
                args.extend(strings(&["--symbols", "ETH-USD"]));
            }
            if mode == "profiles" {
                args.extend(strings(&[
                    "--costs",
                    repo("tv4/tests/fixtures/costs_c1.toml").to_str().unwrap(),
                ]));
            }
            if let Some(d) = delay {
                args.extend(strings(&["--entry-delay-bars", d]));
            }
            let result = tv4(&args);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let dir = if mode == "profiles" {
                out.join("baseline")
            } else {
                out
            };
            let run = stats::read_run(&dir).unwrap();
            let extra = &run.metrics()["config"]["sim_rules"]["extra_entry_delay_bars"];
            if delay == Some("1") {
                assert_eq!(extra, 15);
            } else {
                assert!(extra.is_null());
            }
            hashes.push(run.manifest()["config_hash"].clone());
            runs.push(run);
            dirs.push(dir);
        }
        assert_eq!(hashes[0], hashes[1]);
        assert_ne!(hashes[0], hashes[2]);
        let comparison = stats::compare_delayed_entries(&runs[0], &runs[2]).unwrap();
        assert!(!comparison.eligible_dev_g2);
        assert_eq!(comparison.delay_minutes, 15);
        assert!(stats::compare_delayed_entries(&runs[2], &runs[2]).is_err());
        // Une translation CSV ne devient jamais un rejeu : le lecteur contrôle les octets sources.
        let mut csv = csv::Reader::from_reader(runs[2].trades_bytes());
        let headers = csv.headers().unwrap().clone();
        let entry_index = headers.iter().position(|h| h == "entry_ms").unwrap();
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer.write_record(&headers).unwrap();
        for record in csv.records() {
            let record = record.unwrap();
            let mut values: Vec<_> = record.iter().map(str::to_owned).collect();
            values[entry_index] =
                (values[entry_index].parse::<i64>().unwrap() + 15 * 60_000).to_string();
            writer.write_record(&values).unwrap();
        }
        fs::write(dirs[2].join("trades.csv"), writer.into_inner().unwrap()).unwrap();
        assert!(stats::read_run(&dirs[2]).is_err());
        assert_eq!(
            stats::compare_delayed_entries(&runs[0], &runs[2]).unwrap(),
            comparison
        );
    }
}
#[test]
fn delay_two_is_a_cli_usage_error_for_backtest_and_bench() {
    for command in ["backtest", "bench"] {
        let output = tv4(&strings(&[
            command,
            "--fixture",
            "absent.csv",
            "--strategy",
            "sma_cross",
            "--params",
            "absent.toml",
            "--entry-delay-bars",
            "2",
        ]));
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("0..=1"));
    }
}
