//! Paquet S13 : identité des simulations, sans empreinte circulaire du manifeste.
use tv4::report::stats_inputs::{
    CONVENTIONS, STATS_INPUTS_FILE, STATS_INPUTS_SCHEMA, StatsInputs, StatsInputsRef,
};
use tv4::research::stats::{StatsSizing, Window};

#[test]
fn dto_schema_possede_et_validation_numerique_avant_serialisation() {
    let json = serde_json::json!({"schema":STATS_INPUTS_SCHEMA,"conventions":CONVENTIONS,"provenance":{"run_id":"a".repeat(64),"config_hash":"b".repeat(64),"dataset_hash":format!("blake3:{}","c".repeat(64)),"result_hash":format!("blake3:{}", "a".repeat(64)),"initial_cash":300.,"window":{"start_ms":0,"end_ms":60000},"profile":null,"line":null,"files":{"metrics.json":format!("blake3:{}", "a".repeat(64)),"trades.csv":format!("blake3:{}", "a".repeat(64)),"equity.csv":format!("blake3:{}", "a".repeat(64))}},"pair_symbols":["BTC-USD"],"sizing":"compounding","decision_timeframe_minutes":60,"closures":[],"marks":[],"cash":[],"markets":[]});
    let mut packet: StatsInputs = serde_json::from_value(json).unwrap();
    assert_eq!(
        packet.provenance.window,
        Window {
            start_ms: 0,
            end_ms: 60000
        }
    );
    assert_eq!(packet.sizing, StatsSizing::Compounding);
    assert!(packet.validate().is_ok());
    for field in ["run_id", "config_hash", "dataset_hash"] {
        let mut p = packet.clone();
        match field {
            "run_id" => p.provenance.run_id = "r".into(),
            "config_hash" => p.provenance.config_hash = "c".into(),
            _ => p.provenance.dataset_hash = "d".into(),
        }
        assert!(p.validate().is_err(), "format {field}");
    }
    packet.provenance.initial_cash = f64::NAN;
    assert!(packet.validate().is_err());
    packet.provenance.initial_cash = 300.;
    packet.marks.push(tv4::research::stats::MarkPoint {
        at_ms: 0,
        equity: f64::INFINITY,
        day_close: true,
    });
    assert!(packet.validate().is_err());
    let reference = StatsInputsRef {
        file: STATS_INPUTS_FILE.into(),
        schema: STATS_INPUTS_SCHEMA.into(),
        hash: format!("blake3:{}", "a".repeat(64)),
    };
    assert!(reference.validate().is_ok());
    let mut traversal = reference;
    traversal.file = "../stats-inputs.json".into();
    assert!(traversal.validate().is_err());
    packet.marks.clear();
    packet.decision_timeframe_minutes = 7;
    assert!(packet.validate().is_err(), "TF décision non supporté");
}

#[test]
fn paquet_refuse_toutes_les_valeurs_non_finies_et_les_grilles_incoherentes() {
    use tv4::research::stats::{CashPoint, MarkPoint, MarketSeries, PricePoint, TradeClosure};
    let h = format!("blake3:{}", "a".repeat(64));
    let base:StatsInputs=serde_json::from_value(serde_json::json!({"schema":STATS_INPUTS_SCHEMA,"conventions":CONVENTIONS,"provenance":{"run_id":"a".repeat(64),"config_hash":"b".repeat(64),"dataset_hash":format!("blake3:{}","c".repeat(64)),"result_hash":h,"initial_cash":300.,"window":{"start_ms":0,"end_ms":172800000},"profile":null,"line":null,"files":{"metrics.json":h,"trades.csv":h,"equity.csv":h}},"pair_symbols":["BTC-USD"],"sizing":"compounding","decision_timeframe_minutes":60,"closures":[],"marks":[],"cash":[],"markets":[]})).unwrap();
    for case in 0..17 {
        let mut p = base.clone();
        match case {
            0 => p.cash.push(CashPoint {
                at_ms: 0,
                cash: f64::NAN,
            }),
            1 => p.cash.push(CashPoint {
                at_ms: 1,
                cash: 300.,
            }),
            2 => {
                p.marks = vec![MarkPoint {
                    at_ms: 1,
                    equity: 1.,
                    day_close: true,
                }]
            }
            3 => {
                p.marks = vec![
                    MarkPoint {
                        at_ms: 0,
                        equity: 300.,
                        day_close: true,
                    },
                    MarkPoint {
                        at_ms: 1,
                        equity: 0.,
                        day_close: false,
                    },
                    MarkPoint {
                        at_ms: 2,
                        equity: 1.,
                        day_close: false,
                    },
                ]
            }
            4 => {
                p.marks = vec![MarkPoint {
                    at_ms: 1,
                    equity: -1.,
                    day_close: false,
                }]
            }
            5 => {
                p.closures = vec![
                    TradeClosure { row: 0, at_ms: 1 },
                    TradeClosure { row: 0, at_ms: 2 },
                ]
            }
            6 => {
                p.closures = vec![TradeClosure {
                    row: 0,
                    at_ms: 172800001,
                }]
            }
            7 => p.pair_symbols.push("BTC-USD".into()),
            8 => {
                p.markets = vec![MarketSeries {
                    symbol: "BTC-USD".into(),
                    daily_closes: vec![PricePoint {
                        at_ms: 86400000,
                        close: f64::NAN,
                    }],
                    valuations: vec![],
                }]
            }
            9 => {
                p.markets = vec![MarketSeries {
                    symbol: "BTC-USD".into(),
                    daily_closes: vec![],
                    valuations: vec![PricePoint {
                        at_ms: 1,
                        close: 1.,
                    }],
                }]
            }
            10 => {
                p.markets = vec![MarketSeries {
                    symbol: "BTC-USD".into(),
                    daily_closes: vec![
                        PricePoint {
                            at_ms: 86400000,
                            close: 1.,
                        },
                        PricePoint {
                            at_ms: 86400000,
                            close: 2.,
                        },
                    ],
                    valuations: vec![],
                }]
            }
            11 => {
                p.markets = vec![MarketSeries {
                    symbol: "BTC-USD".into(),
                    daily_closes: vec![],
                    valuations: vec![PricePoint {
                        at_ms: 0,
                        close: 0.,
                    }],
                }]
            }
            12 => p.provenance.files.metrics_json = "blake3:xyz".into(),
            13 => p.provenance.window.end_ms = 0,
            14 => {
                p.cash = vec![
                    CashPoint {
                        at_ms: 0,
                        cash: 300.,
                    },
                    CashPoint {
                        at_ms: 0,
                        cash: 299.,
                    },
                ]
            }
            15 => {
                p.marks = vec![MarkPoint {
                    at_ms: 0,
                    equity: 300. + 1e-9,
                    day_close: true,
                }]
            }
            _ => p.decision_timeframe_minutes = 7,
        }
        assert!(p.validate().is_err(), "cas {case}");
    }
    // Cash après événements à gauche : son ancre initiale n'est pas obligatoirement C0.
    let mut valid = base;
    valid.cash.push(CashPoint {
        at_ms: 0,
        cash: 125.,
    });
    valid.markets.push(MarketSeries {
        symbol: "BTC-USD".into(),
        daily_closes: vec![PricePoint {
            at_ms: 0,
            close: 100.,
        }],
        valuations: vec![
            PricePoint {
                at_ms: 0,
                close: 101.,
            },
            PricePoint {
                at_ms: 172800000,
                close: 102.,
            },
        ],
    });
    valid.validate().unwrap();
    let bytes = tv4::report::json_bytes(&valid).unwrap();
    assert_eq!(
        serde_json::from_slice::<StatsInputs>(&bytes).unwrap(),
        valid
    );
    let text = String::from_utf8(bytes)
        .unwrap()
        .replacen("125.0", "1e400", 1);
    assert!(serde_json::from_str::<StatsInputs>(&text).is_err());
}
