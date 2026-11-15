mod research_stats_common;
use research_stats_common::trade;
use tv4::research::stats::{
    BenchmarkInput, BenchmarkSymbols, CashPoint, MarkPoint, MarketSeries, PricePoint,
    ResearchPartition, StatsOptions, Window, benchmarks,
};
const DAY: i64 = 86_400_000;
#[test]
fn cash_simple_et_basket_initial_sans_reequilibrage() {
    let w = Window {
        start_ms: 0,
        end_ms: 2 * DAY,
    };
    let marks = vec![
        MarkPoint {
            at_ms: 0,
            equity: 100.,
            day_close: false,
        },
        MarkPoint {
            at_ms: DAY,
            equity: 110.,
            day_close: true,
        },
        MarkPoint {
            at_ms: 2 * DAY,
            equity: 100.,
            day_close: true,
        },
    ];
    let markets: Vec<_> = [("BTC", [100., 200., 100.]), ("ETH", [100., 100., 200.])]
        .into_iter()
        .map(|(s, p)| MarketSeries {
            symbol: s.into(),
            daily_closes: vec![],
            valuations: p
                .into_iter()
                .enumerate()
                .map(|(i, close)| PricePoint {
                    at_ms: i as i64 * DAY,
                    close,
                })
                .collect(),
        })
        .collect();
    let cash = vec![
        CashPoint {
            at_ms: 0,
            cash: 50.,
        },
        CashPoint {
            at_ms: 2 * DAY,
            cash: 0.,
        },
    ];
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let options = StatsOptions::new(42);
    let input = BenchmarkInput {
        trades: &[],
        pair_symbols: &["BTC".into()],
        marks: &marks,
        cash: &cash,
        markets: &markets,
        capital: 100.,
        window: w,
        symbols: &symbols,
        partition: ResearchPartition::Dev,
    };
    let r = benchmarks(input, &options).unwrap();
    assert_eq!(r.basket_50_50.total_return.value, Some(0.5));
    assert_eq!(r.basket_50_50.daily.intervals[1].value, 0.);
    assert!((r.cash_reference_quote.value.unwrap() - 50. * 0.017 * 2. / 365.25).abs() < 1e-15);
    assert_eq!(
        r.cash_reference_return.value,
        r.cash_reference_quote.value.map(|v| v / 100.)
    );
    let mut t = trade(0, Some(1.));
    t.entry_ms = 0;
    t.exit_ms = DAY;
    t.symbol = "BTC".into();
    let mut overlap = t.clone();
    overlap.row = 1;
    overlap.included = false;
    let r = benchmarks(
        BenchmarkInput {
            trades: &[t, overlap],
            ..input
        },
        &options,
    )
    .unwrap();
    assert_eq!(r.pairs[0].exposure_fraction, 0.5);
    assert!((r.pairs[0].benchmark.trajectory[1].close - 1.5000230766681308).abs() < 1e-14);
}
fn ts(s: &str) -> i64 {
    tv4::store::time::parse_rfc3339_utc(s).unwrap()
}
#[test]
fn mois_complets_bootstrap_commun_partition_et_absences() {
    let start = ts("2024-01-15T00:00:00Z");
    let end = ts("2024-04-15T00:00:00Z");
    let n = (end - start) / DAY;
    let marks: Vec<_> = (0..=n)
        .map(|i| MarkPoint {
            at_ms: start + i * DAY,
            equity: 100. + i as f64,
            day_close: true,
        })
        .collect();
    let markets = vec![MarketSeries {
        symbol: "BTC".into(),
        daily_closes: vec![],
        valuations: marks
            .iter()
            .map(|p| PricePoint {
                at_ms: p.at_ms,
                close: p.equity,
            })
            .collect(),
    }];
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let o = StatsOptions::new(u64::MAX);
    let input = BenchmarkInput {
        trades: &[],
        pair_symbols: &["BTC".into()],
        marks: &marks,
        cash: &[],
        markets: &markets,
        capital: 100.,
        window: Window {
            start_ms: start,
            end_ms: end,
        },
        symbols: &symbols,
        partition: ResearchPartition::Dev,
    };
    let r = benchmarks(input, &o).unwrap();
    assert_eq!(r.btc.monthly.used_months.len(), 2);
    assert_eq!(r.btc.monthly.excluded_months.len(), 2);
    assert_eq!(
        r.btc.monthly.used_months[0],
        Window {
            start_ms: ts("2024-02-01T00:00:00Z"),
            end_ms: ts("2024-03-01T00:00:00Z")
        }
    );
    assert_eq!(r.btc.monthly.point_excess.value, Some(0.));
    assert_eq!(r.btc.monthly.lb95_excess.value, Some(0.));
    assert_eq!(r.btc.excess_sharpe.value, None);
    assert_eq!(r.basket_50_50.total_return.value, None);
    assert_eq!(r.cash_reference_return.value, None);
    let v = benchmarks(
        BenchmarkInput {
            partition: ResearchPartition::Holdout,
            ..input
        },
        &o,
    )
    .unwrap();
    assert_eq!(v.btc.monthly.lb95_excess.value, None);
    assert_eq!(v.btc.monthly.point_excess.value, Some(0.));
    let mut gaps = markets.clone();
    gaps[0].valuations.remove(20);
    let g = benchmarks(
        BenchmarkInput {
            markets: &gaps,
            ..input
        },
        &o,
    )
    .unwrap();
    assert!(g.btc.total_return.value.is_some());
    assert_eq!(g.btc.excess_sharpe.value, None);
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        serde_json::to_string(&benchmarks(input, &o).unwrap()).unwrap()
    );
}
#[test]
fn calmar_frontiere_36_mois_calendaires_et_cash_subdivise() {
    let start = ts("2020-02-29T00:00:00Z");
    let end = ts("2023-02-28T00:00:00Z");
    let n = (end - start) / DAY;
    let marks: Vec<_> = (0..=n)
        .map(|i| MarkPoint {
            at_ms: start + i * DAY,
            equity: if i == 1 { 90. } else { 100. + i as f64 / 100. },
            day_close: true,
        })
        .collect();
    let markets = vec![MarketSeries {
        symbol: "BTC".into(),
        daily_closes: vec![],
        valuations: marks
            .iter()
            .map(|p| PricePoint {
                at_ms: p.at_ms,
                close: p.equity,
            })
            .collect(),
    }];
    let cash = [
        CashPoint {
            at_ms: start,
            cash: 25.,
        },
        CashPoint {
            at_ms: end,
            cash: 100.,
        },
    ];
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let o = StatsOptions {
        samples: 2,
        ..StatsOptions::new(1)
    };
    let input = BenchmarkInput {
        trades: &[],
        pair_symbols: &["BTC".into()],
        marks: &marks,
        cash: &cash,
        markets: &markets,
        capital: 100.,
        window: Window {
            start_ms: start,
            end_ms: end,
        },
        symbols: &symbols,
        partition: ResearchPartition::Validation,
    };
    let r = benchmarks(input, &o).unwrap();
    assert!(r.btc.calmar.value.is_some());
    let split = [
        cash[0],
        CashPoint {
            at_ms: start + DAY,
            cash: 25.,
        },
        cash[1],
    ];
    let s = benchmarks(
        BenchmarkInput {
            cash: &split,
            ..input
        },
        &o,
    )
    .unwrap();
    assert!(
        (r.cash_reference_quote.value.unwrap() - s.cash_reference_quote.value.unwrap()).abs()
            < 1e-13
    );
    let shorter = Window {
        end_ms: end - 1,
        ..input.window
    };
    let mut shorter_marks = marks[..marks.len() - 1].to_vec();
    shorter_marks.push(MarkPoint {
        at_ms: end - 1,
        equity: 110.,
        day_close: false,
    });
    let short = benchmarks(
        BenchmarkInput {
            window: shorter,
            marks: &shorter_marks,
            markets: &[],
            cash: &[],
            ..input
        },
        &o,
    )
    .unwrap();
    assert_eq!(short.strategy_calmar.value, None);
}
#[test]
fn prix_presents_invalides_et_cash_negatif_refuses() {
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let o = StatsOptions::new(1);
    let w = Window {
        start_ms: 0,
        end_ms: DAY,
    };
    let bad = [MarketSeries {
        symbol: "BTC".into(),
        daily_closes: vec![],
        valuations: vec![PricePoint {
            at_ms: 0,
            close: 0.,
        }],
    }];
    let i = BenchmarkInput {
        trades: &[],
        pair_symbols: &["BTC".into()],
        marks: &[],
        cash: &[],
        markets: &bad,
        capital: 100.,
        window: w,
        symbols: &symbols,
        partition: ResearchPartition::Dev,
    };
    assert!(benchmarks(i, &o).is_err());
    let cash = [CashPoint {
        at_ms: 0,
        cash: -1.,
    }];
    assert!(
        benchmarks(
            BenchmarkInput {
                markets: &[],
                cash: &cash,
                ..i
            },
            &o
        )
        .is_err()
    );
}
#[test]
fn bootstrap_oracle_deux_mois_apparies_et_petits_exces_resolus() {
    let start = ts("2024-01-01T00:00:00Z");
    let middle = ts("2024-02-01T00:00:00Z");
    let end = ts("2024-03-01T00:00:00Z");
    let marks: Vec<_> = (start..=end)
        .step_by(DAY as usize)
        .map(|at_ms| MarkPoint {
            at_ms,
            equity: if at_ms == end {
                99.
            } else if at_ms >= middle {
                110.
            } else {
                100.
            },
            day_close: true,
        })
        .collect();
    let market = MarketSeries {
        symbol: "BTC".into(),
        daily_closes: vec![],
        valuations: marks
            .iter()
            .map(|p| PricePoint {
                at_ms: p.at_ms,
                close: if p.at_ms == end {
                    96.
                } else if p.at_ms >= middle {
                    120.
                } else {
                    100.
                },
            })
            .collect(),
    };
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let options = StatsOptions::new(56);
    let input = BenchmarkInput {
        trades: &[],
        pair_symbols: &["BTC".into()],
        marks: &marks,
        cash: &[],
        markets: std::slice::from_ref(&market),
        capital: 100.,
        window: Window {
            start_ms: start,
            end_ms: end,
        },
        symbols: &symbols,
        partition: ResearchPartition::Dev,
    };
    let r = benchmarks(input, &options).unwrap();
    assert!((r.btc.monthly.point_excess.value.unwrap() - 0.03).abs() < 1e-14);
    // Four equally probable paired draws: -.23, .03, .03, .17; p5 is -.23.
    assert!((r.btc.monthly.lb95_excess.value.unwrap() + 0.23).abs() < 1e-14);
    let small_marks: Vec<_> = marks
        .iter()
        .enumerate()
        .map(|(i, p)| MarkPoint {
            equity: 100. * (1. + (i * i) as f64 * 1e-8),
            ..*p
        })
        .collect();
    let mut small_market = market.clone();
    for p in &mut small_market.valuations {
        p.close = 100.;
    }
    let small = benchmarks(
        BenchmarkInput {
            marks: &small_marks,
            markets: std::slice::from_ref(&small_market),
            ..input
        },
        &options,
    )
    .unwrap();
    assert!(small.btc.excess_sharpe.value.is_some());
}
#[test]
fn exposition_union_clipee_purge_et_oracle_trajectoire_statique() {
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let options = StatsOptions::new(1);
    let w = Window {
        start_ms: 10 * DAY,
        end_ms: 20 * DAY,
    };
    let trades: Vec<_> = [(8, 12), (11, 14), (18, 22)]
        .into_iter()
        .enumerate()
        .map(|(i, (a, b))| {
            let mut t = trade(i, Some(1.));
            t.symbol = "BTC".into();
            t.entry_ms = a * DAY;
            t.exit_ms = b * DAY;
            t.included = i != 1;
            t
        })
        .collect();
    let markets = [MarketSeries {
        symbol: "BTC".into(),
        daily_closes: vec![],
        valuations: vec![
            PricePoint {
                at_ms: w.start_ms,
                close: 100.,
            },
            PricePoint {
                at_ms: w.end_ms,
                close: 100.,
            },
        ],
    }];
    let r = benchmarks(
        BenchmarkInput {
            trades: &trades,
            pair_symbols: &["BTC".into()],
            marks: &[],
            cash: &[],
            markets: &markets,
            capital: 100.,
            window: w,
            symbols: &symbols,
            partition: ResearchPartition::Dev,
        },
        &options,
    )
    .unwrap();
    assert_eq!(r.pairs[0].exposure_fraction, 0.6);
    assert!(r.pairs[0].benchmark.total_return.value.is_some());
    assert_eq!(r.pairs[0].benchmark.excess_sharpe.value, None);
}
#[test]
fn validations_series_capital_symboles_et_cash_horloge() {
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let o = StatsOptions::new(1);
    let w = Window {
        start_ms: DAY,
        end_ms: 2 * DAY,
    };
    let i = BenchmarkInput {
        trades: &[],
        pair_symbols: &["BTC".into()],
        marks: &[],
        cash: &[],
        markets: &[],
        capital: 100.,
        window: w,
        symbols: &symbols,
        partition: ResearchPartition::Dev,
    };
    for capital in [0., -1., f64::NAN] {
        assert!(benchmarks(BenchmarkInput { capital, ..i }, &o).is_err());
    }
    for symbols in [
        BenchmarkSymbols {
            btc: "".into(),
            eth: "ETH".into(),
        },
        BenchmarkSymbols {
            btc: "BTC".into(),
            eth: "BTC".into(),
        },
    ] {
        assert!(
            benchmarks(
                BenchmarkInput {
                    symbols: &symbols,
                    ..i
                },
                &o
            )
            .is_err()
        );
    }
    for dates in [[DAY, DAY], [2 * DAY, DAY], [0, DAY], [DAY, 3 * DAY]] {
        let cash = dates.map(|at_ms| CashPoint { at_ms, cash: 10. });
        assert!(benchmarks(BenchmarkInput { cash: &cash, ..i }, &o).is_err());
    }
    for at_ms in [0, DAY + 1, 3 * DAY] {
        let markets = [MarketSeries {
            symbol: "BTC".into(),
            daily_closes: vec![],
            valuations: vec![PricePoint { at_ms, close: 100. }],
        }];
        assert!(
            benchmarks(
                BenchmarkInput {
                    markets: &markets,
                    ..i
                },
                &o
            )
            .is_err()
        );
    }
    let m = MarketSeries {
        symbol: "BTC".into(),
        daily_closes: vec![],
        valuations: vec![],
    };
    assert!(
        benchmarks(
            BenchmarkInput {
                markets: &[m.clone(), m],
                ..i
            },
            &o
        )
        .is_err()
    );
}
#[test]
fn calmar_negatif_autorise_et_dd_nul_reste_absent() {
    let start = ts("2020-01-01T00:00:00Z");
    let end = ts("2023-01-01T00:00:00Z");
    let marks: Vec<_> = (start..=end)
        .step_by(DAY as usize)
        .map(|at_ms| MarkPoint {
            at_ms,
            equity: if at_ms == end { 50. } else { 100. },
            day_close: true,
        })
        .collect();
    let markets = [MarketSeries {
        symbol: "BTC".into(),
        daily_closes: vec![],
        valuations: marks
            .iter()
            .map(|p| PricePoint {
                at_ms: p.at_ms,
                close: 100.,
            })
            .collect(),
    }];
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let o = StatsOptions {
        samples: 2,
        ..StatsOptions::new(0)
    };
    let r = benchmarks(
        BenchmarkInput {
            trades: &[],
            pair_symbols: &["BTC".into()],
            marks: &marks,
            cash: &[],
            markets: &markets,
            capital: 100.,
            window: Window {
                start_ms: start,
                end_ms: end,
            },
            symbols: &symbols,
            partition: ResearchPartition::Validation,
        },
        &o,
    )
    .unwrap();
    assert!(r.strategy_calmar.value.unwrap() < 0.);
    assert_eq!(r.btc.calmar.value, None);
}
#[test]
fn forward_et_non_classe_sont_point_seul_sans_requalifier_dev() {
    for partition in [ResearchPartition::Forward, ResearchPartition::Unclassified] {
        let symbols = BenchmarkSymbols {
            btc: "BTC".into(),
            eth: "ETH".into(),
        };
        let result = benchmarks(
            BenchmarkInput {
                trades: &[],
                pair_symbols: &["BTC".into()],
                marks: &[],
                cash: &[],
                markets: &[],
                capital: 100.,
                window: Window {
                    start_ms: 0,
                    end_ms: DAY,
                },
                symbols: &symbols,
                partition,
            },
            &StatsOptions::new(1),
        )
        .unwrap();
        assert_eq!(result.btc.monthly.lb95_excess.value, None);
        assert!(result.btc.monthly.lb95_excess.reasons[0].contains("DEV"));
        assert!(serde_json::to_string(&partition).unwrap().contains(
            if partition == ResearchPartition::Forward {
                "forward"
            } else {
                "unclassified"
            }
        ));
    }
}
#[test]
fn universe_joue_explicite_ne_confond_pas_btc_contexte_et_eth_joue() {
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let pairs = vec!["ETH".to_string()];
    let markets: Vec<_> = ["BTC", "ETH"]
        .into_iter()
        .map(|symbol| MarketSeries {
            symbol: symbol.into(),
            daily_closes: vec![],
            valuations: vec![
                PricePoint {
                    at_ms: 0,
                    close: 100.,
                },
                PricePoint {
                    at_ms: DAY,
                    close: 200.,
                },
            ],
        })
        .collect();
    let mut t = trade(0, Some(1.));
    t.symbol = "ETH".into();
    t.entry_ms = 0;
    t.exit_ms = DAY;
    let input = BenchmarkInput {
        trades: &[t],
        pair_symbols: &pairs,
        marks: &[],
        cash: &[],
        markets: &markets,
        capital: 100.,
        window: Window {
            start_ms: 0,
            end_ms: DAY,
        },
        symbols: &symbols,
        partition: ResearchPartition::Dev,
    };
    let r = benchmarks(input, &StatsOptions::new(1)).unwrap();
    assert_eq!(r.pairs.len(), 1);
    assert_eq!(r.pairs[0].symbol, "ETH");
    assert_eq!(
        r.equal_weight_pairs.trajectory,
        r.pairs[0].benchmark.trajectory
    );
    let empty = benchmarks(
        BenchmarkInput {
            trades: &[],
            ..input
        },
        &StatsOptions::new(1),
    )
    .unwrap();
    assert_eq!(empty.pairs.len(), 1);
    assert_eq!(empty.pairs[0].exposure_fraction, 0.);
    let duplicates = vec!["ETH".into(), "ETH".into()];
    assert!(
        benchmarks(
            BenchmarkInput {
                pair_symbols: &duplicates,
                ..input
            },
            &StatsOptions::new(1)
        )
        .is_err()
    );
    assert!(
        benchmarks(
            BenchmarkInput {
                pair_symbols: &[],
                ..input
            },
            &StatsOptions::new(1)
        )
        .is_err()
    );
}
#[test]
fn marquage_initial_different_du_capital_est_incoherent() {
    let marks = [MarkPoint {
        at_ms: 0,
        equity: 99.,
        day_close: false,
    }];
    let symbols = BenchmarkSymbols {
        btc: "BTC".into(),
        eth: "ETH".into(),
    };
    let input = BenchmarkInput {
        trades: &[],
        pair_symbols: &[],
        marks: &marks,
        cash: &[],
        markets: &[],
        capital: 100.,
        window: Window {
            start_ms: 0,
            end_ms: DAY,
        },
        symbols: &symbols,
        partition: ResearchPartition::Unclassified,
    };
    assert!(benchmarks(input, &StatsOptions::new(1)).is_err());
}
