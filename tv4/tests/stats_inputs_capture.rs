//! Capture S13 à partir des seules données admises, via le vrai binaire.
mod stats_inputs_capture_common;
use stats_inputs_capture_common::check;
mod common;
mod report_s9_common;
use report_s9_common::{build_dataset, candles, repo, strings, tv4};
use std::fs;
use tv4::report::stats;
#[test]
fn chaque_generation_capture_un_paquet_valide_sans_btc_ajoute() {
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
    let instruments = root.join("instruments.toml");
    fs::write(
        &instruments,
        "[kraken.ETH-USD]\ntick_size = 0.01\nlot_size = 1e-6\nmin_qty = 0.0\nmin_notional = 0.0\n",
    )
    .unwrap();
    for mode in ["cell", "portfolio", "profiles"] {
        let out = root.join(mode);
        let mut args = strings(&[
            "backtest",
            "--dataset",
            "eth",
            "--strategy",
            "sma_cross",
            "--root",
            root.to_str().unwrap(),
            "--params",
            params.to_str().unwrap(),
            "--instruments-file",
            instruments.to_str().unwrap(),
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
        let result = tv4(&args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for dir in if mode == "profiles" {
            vec![out.join("baseline"), out.join("adverse"), out.join("maker")]
        } else {
            vec![out]
        } {
            let p = check(&dir);
            assert_eq!(p.pair_symbols, ["ETH-USD"]);
            assert_eq!(p.markets.len(), 1);
            assert_eq!(p.markets[0].symbol, "ETH-USD");
            assert_eq!(p.decision_timeframe_minutes, 15);
            assert!(!p.markets[0].daily_closes.is_empty());
            assert!(p.marks.iter().any(|m| m.day_close));
            assert!(!p.closures.is_empty());
            let bytes = fs::read(dir.join("equity.csv")).unwrap();
            let sampled: Vec<i64> = csv::Reader::from_reader(bytes.as_slice())
                .records()
                .map(|r| r.unwrap()[0].parse().unwrap())
                .collect();
            assert!(
                p.closures.iter().any(|c| !sampled.contains(&c.at_ms)),
                "sorties capturées sur la courbe minute native, hors échantillonnage 1h"
            );
        }
    }
}

#[test]
fn une_jambe_terminee_plus_tot_est_liquidee_a_la_fin_globale_sans_prix_impute() {
    use report_s9_common::{MINUTE, T0};
    use tv4::model::Candle;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let series = |n: usize, trend: bool| {
        (0..n)
            .map(|i| {
                let price = if !trend || i < 1700 {
                    100.
                } else if i < 1850 {
                    100. - (i - 1700) as f64 * 0.1
                } else {
                    85. + (i - 1850) as f64 * 0.4
                };
                Candle {
                    open_time_ms: T0 + i as i64 * MINUTE,
                    open: price,
                    high: price + 0.1,
                    low: price - 0.1,
                    close: price,
                    volume: 1e6,
                    trades: None,
                }
            })
            .collect()
    };
    build_dataset(
        root,
        "short",
        &[
            ("BTC-USD", series(2000, true)),
            ("ETH-USD", series(6000, false)),
        ],
        6100,
    );
    let params = root.join("params.toml");
    fs::write(
        &params,
        "tf = \"15m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0\n",
    )
    .unwrap();
    let instruments = root.join("instruments.toml");
    fs::write(&instruments,"[kraken.BTC-USD]\ntick_size = 0.1\nlot_size = 1e-8\nmin_qty = 0.0\nmin_notional = 0.0\n[kraken.ETH-USD]\ntick_size = 0.01\nlot_size = 1e-6\nmin_qty = 0.0\nmin_notional = 0.0\n").unwrap();
    let out = root.join("out");
    let result = tv4(&strings(&[
        "backtest",
        "--dataset",
        "short",
        "--symbols",
        "BTC-USD,ETH-USD",
        "--strategy",
        "sma_cross",
        "--root",
        root.to_str().unwrap(),
        "--params",
        params.to_str().unwrap(),
        "--fees-file",
        repo("config/fees.toml").to_str().unwrap(),
        "--instruments-file",
        instruments.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let p = check(&out);
    let source = stats::read_run(&out).unwrap();
    let image = stats::snapshot(&source).unwrap();
    assert_eq!(
        image.terminal_rows.len(),
        1,
        "le croisement synthétique doit garder un lot BTC jusqu'à la fin"
    );
    let row = image.terminal_rows[0];
    assert_eq!(image.trades[row].symbol, "BTC-USD");
    assert!(image.trades[row].included, "S9 conserve ForcedClose");
    assert!(image.trades[row].exit_ms < p.provenance.window.end_ms);
    assert_eq!(p.closures[row].at_ms, p.provenance.window.end_ms);
    let btc = p.markets.iter().find(|m| m.symbol == "BTC-USD").unwrap();
    assert!(
        !btc.valuations
            .iter()
            .any(|v| v.at_ms == p.provenance.window.end_ms),
        "aucun prix BTC extrapolé"
    );
    assert_eq!(p.cash.last().unwrap().cash, image.cash_final);
    assert_eq!(p.marks.last().unwrap().equity, image.equity_final);
}
