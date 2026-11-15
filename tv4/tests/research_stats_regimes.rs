mod research_stats_common;
use research_stats_common::trade;
use tv4::research::stats::{
    MarkPoint, MarketSeries, PricePoint, Trend, Volatility, Window, daily_returns, regimes,
};
const DAY: i64 = 86_400_000;
fn market(n: usize) -> MarketSeries {
    MarketSeries {
        symbol: "BTC".into(),
        daily_closes: (0..n)
            .map(|i| PricePoint {
                at_ms: (i as i64 + 1) * DAY,
                close: 100.,
            })
            .collect(),
        valuations: vec![],
    }
}
#[test]
fn egalites_sma_vol_et_labels_de_toutes_les_lignes() {
    let m = market(202);
    let mut a = trade(0, Some(-1.));
    a.entry_ms = 200 * DAY;
    a.exit_ms = 201 * DAY;
    let mut purged = a.clone();
    purged.row = 1;
    purged.included = false;
    let r = regimes(
        &[a, purged],
        Some(&m),
        &[],
        Window {
            start_ms: 200 * DAY,
            end_ms: 202 * DAY,
        },
    )
    .unwrap();
    assert_eq!(r.labels.len(), 2);
    assert_eq!(r.labels[0].trend, Some(Trend::Bear));
    assert_eq!(r.labels[0].volatility, Some(Volatility::Low));
    assert_eq!(r.cells[2].n, 1);
    assert_eq!(r.cells[2].mu_r.value, Some(-1.));
    assert_eq!(r.volatility_origin_ms, Some(31 * DAY));
}
#[test]
fn rendements_uniquement_minuit_consecutif_et_zero_non_invente() {
    let w = Window {
        start_ms: DAY / 2,
        end_ms: 4 * DAY,
    };
    let marks = vec![
        MarkPoint {
            at_ms: DAY / 2,
            equity: 100.,
            day_close: false,
        },
        MarkPoint {
            at_ms: DAY,
            equity: 100.,
            day_close: true,
        },
        MarkPoint {
            at_ms: 2 * DAY,
            equity: 0.,
            day_close: true,
        },
        MarkPoint {
            at_ms: 4 * DAY,
            equity: 0.,
            day_close: true,
        },
    ];
    let r = daily_returns(&marks, w).unwrap();
    assert_eq!(r.expected_intervals, 3);
    assert_eq!(r.intervals.len(), 1);
    assert_eq!(r.intervals[0].value, -1.);
    assert_eq!(r.missing_intervals.len(), 2);
}
#[test]
fn causalite_gaps_chauffe_et_r_absent_conserve_n() {
    let mut m = market(230);
    let mut t = trade(0, None);
    t.entry_ms = 201 * DAY + 1;
    t.exit_ms = 202 * DAY;
    let w = Window {
        start_ms: 200 * DAY,
        end_ms: 203 * DAY,
    };
    let original = regimes(&[t.clone()], Some(&m), &[], w).unwrap();
    m.daily_closes[220].close = 10000.;
    let future = regimes(&[t.clone()], Some(&m), &[], w).unwrap();
    assert_eq!(original.labels, future.labels);
    assert_eq!(original.cells[2].n, 1);
    assert_eq!(original.cells[2].mu_r.value, None);
    assert_eq!(original.missing_r, 1);
    m.daily_closes.remove(200);
    let gap = regimes(&[t.clone()], Some(&m), &[], w).unwrap();
    assert_eq!(gap.missing_labels, 1);
    t.entry_ms = 202 * DAY;
    t.exit_ms = 203 * DAY;
    assert_eq!(regimes(&[t], Some(&m), &[], w).unwrap().missing_labels, 1);
    let bad = MarketSeries {
        daily_closes: vec![PricePoint {
            at_ms: DAY,
            close: f64::NAN,
        }],
        ..market(0)
    };
    assert!(regimes(&[], Some(&bad), &[], w).is_err());
}
#[test]
fn bear_selection_a_gauche_et_duree_sans_trous() {
    let mut m = market(202);
    m.daily_closes[200].close = 200.; // 200j bear, 201j bull
    let marks = vec![
        MarkPoint {
            at_ms: 200 * DAY,
            equity: 100.,
            day_close: false,
        },
        MarkPoint {
            at_ms: 201 * DAY,
            equity: 101.,
            day_close: true,
        },
        MarkPoint {
            at_ms: 202 * DAY,
            equity: 50.,
            day_close: true,
        },
    ];
    let r = regimes(
        &[],
        Some(&m),
        &marks,
        Window {
            start_ms: 200 * DAY,
            end_ms: 202 * DAY,
        },
    )
    .unwrap();
    assert_eq!(r.bear.n_intervals, 1);
    assert_eq!(r.bear.duration_years, 1. / 365.25);
    // Python: math.expm1(math.log(1.01)*365.25)
    assert!((r.bear.annualized_return.value.unwrap() - 36.877540751204215).abs() < 1e-10);
    assert_eq!(r.bear.qualification, None); // non-flat but no bear R
}
#[test]
fn validation_horloges_lignes_dupliquees_et_marques_non_finies() {
    let w = Window {
        start_ms: 0,
        end_ms: DAY,
    };
    let a = trade(0, Some(1.));
    assert!(regimes(&[a.clone(), a], None, &[], w).is_err());
    for marks in [
        vec![MarkPoint {
            at_ms: 0,
            equity: f64::INFINITY,
            day_close: false,
        }],
        vec![
            MarkPoint {
                at_ms: DAY,
                equity: 1.,
                day_close: true,
            },
            MarkPoint {
                at_ms: 0,
                equity: 1.,
                day_close: false,
            },
        ],
    ] {
        assert!(daily_returns(&marks, w).is_err());
    }
    assert!(
        daily_returns(
            &[],
            Window {
                start_ms: i64::MIN,
                end_ms: i64::MAX
            }
        )
        .is_err()
    );
}
#[test]
fn oracle_log_vol_et_mediane_expansive_avec_chauffe_interrompue() {
    let mut m = market(240);
    for (i, p) in m.daily_closes.iter_mut().enumerate() {
        p.close = if i % 2 == 0 { 100. } else { 200. };
    }
    let mut t = trade(0, Some(1.));
    t.entry_ms = 200 * DAY;
    t.exit_ms = 201 * DAY;
    let w = Window {
        start_ms: 200 * DAY,
        end_ms: 240 * DAY,
    };
    let r = regimes(&[t.clone()], Some(&m), &[], w).unwrap();
    // Python statistics.stdev([log(2),-log(2)]*15)*sqrt(365.25).
    let expected = std::f64::consts::LN_2 * (30_f64 / 29.).sqrt() * 365.25_f64.sqrt();
    assert!((r.labels[0].volatility30.value.unwrap() - expected).abs() < 1e-12);
    m.daily_closes.remove(200);
    t.entry_ms = 232 * DAY;
    t.exit_ms = 233 * DAY;
    let r = regimes(&[t], Some(&m), &[], w).unwrap();
    assert_eq!(r.volatility_origin_ms, Some(31 * DAY));
    assert!(r.labels[0].expanding_median.value.is_some());
    assert_eq!(r.labels[0].trend, None);
}
#[test]
fn egalite_sma_non_entiere_reste_bear() {
    let mut m = market(200);
    for p in &mut m.daily_closes {
        p.close = 123.45;
    }
    let mut t = trade(0, Some(0.));
    t.entry_ms = 200 * DAY;
    t.exit_ms = 201 * DAY;
    let r = regimes(
        &[t],
        Some(&m),
        &[],
        Window {
            start_ms: 200 * DAY,
            end_ms: 201 * DAY,
        },
    )
    .unwrap();
    assert_eq!(r.labels[0].trend, Some(Trend::Bear));
}
#[test]
fn bear_global_refuse_trous_de_membership_ou_de_marquage_bear() {
    let m = market(203);
    let w = Window {
        start_ms: 200 * DAY,
        end_ms: 203 * DAY,
    };
    let marks = [
        MarkPoint {
            at_ms: 200 * DAY,
            equity: 100.,
            day_close: true,
        },
        MarkPoint {
            at_ms: 201 * DAY,
            equity: 100.,
            day_close: true,
        },
        MarkPoint {
            at_ms: 203 * DAY,
            equity: 100.,
            day_close: true,
        },
    ];
    let r = regimes(&[], Some(&m), &marks, w).unwrap();
    assert_eq!(r.bear.n_intervals, 1);
    assert_eq!(r.bear.annualized_return.value, None);
    assert_eq!(r.bear.qualification, None);
    let mut m = m;
    m.daily_closes.remove(200);
    let r = regimes(&[], Some(&m), &marks, w).unwrap();
    assert_eq!(r.bear.annualized_return.value, None);
}
#[test]
fn bear_multiplie_facteurs_equity_sans_arrondir_un_petit_facteur_a_zero() {
    let m = market(203);
    let marks = [
        MarkPoint {
            at_ms: 200 * DAY,
            equity: 100.,
            day_close: true,
        },
        MarkPoint {
            at_ms: 201 * DAY,
            equity: 1e-18,
            day_close: true,
        },
        MarkPoint {
            at_ms: 202 * DAY,
            equity: 100.,
            day_close: true,
        },
    ];
    let r = regimes(
        &[],
        Some(&m),
        &marks,
        Window {
            start_ms: 200 * DAY,
            end_ms: 202 * DAY,
        },
    )
    .unwrap();
    assert_eq!(r.bear.annualized_return.value, Some(0.));
}
#[test]
fn qualifications_bear_et_cellules_mesures_descriptives() {
    use tv4::research::stats::BearQualification;
    let m = market(203);
    let w = Window {
        start_ms: 200 * DAY,
        end_ms: 202 * DAY,
    };
    for (r_multiple, end, expected) in [
        (-1., 90., BearQualification::Negative),
        (1., 110., BearQualification::Positive),
        (-1., 100., BearQualification::Flat),
    ] {
        let trades: Vec<_> = (0..30)
            .map(|i| {
                let mut t = trade(i, Some(r_multiple));
                t.entry_ms = 200 * DAY;
                t.exit_ms = 201 * DAY;
                t
            })
            .collect();
        let marks = [
            MarkPoint {
                at_ms: w.start_ms,
                equity: 100.,
                day_close: false,
            },
            MarkPoint {
                at_ms: 201 * DAY,
                equity: 100.,
                day_close: true,
            },
            MarkPoint {
                at_ms: w.end_ms,
                equity: end,
                day_close: false,
            },
        ];
        let r = regimes(&trades, Some(&m), &marks, w).unwrap();
        assert_eq!(r.bear.n_entries, 30);
        assert_eq!(r.bear.qualification, Some(expected));
        assert_eq!(
            r.cells[2].below_expectancy_threshold,
            Some(r_multiple < -0.5)
        );
    }
}
#[test]
fn bull_trou_marque_n_invalide_pas_bear_et_vol_courante_haute() {
    let mut m = market(203);
    m.daily_closes[200].close = 200.;
    m.daily_closes[201].close = 200.;
    let mut t = trade(0, Some(1.));
    t.entry_ms = 201 * DAY;
    t.exit_ms = 202 * DAY;
    let marks = [
        MarkPoint {
            at_ms: 200 * DAY,
            equity: 100.,
            day_close: false,
        },
        MarkPoint {
            at_ms: 201 * DAY,
            equity: 100.,
            day_close: true,
        },
    ];
    let r = regimes(
        &[t],
        Some(&m),
        &marks,
        Window {
            start_ms: 200 * DAY,
            end_ms: 203 * DAY,
        },
    )
    .unwrap();
    assert_eq!(r.labels[0].volatility, Some(Volatility::High));
    assert_eq!(r.labels[0].trend, Some(Trend::Bull));
    assert_eq!(r.bear.annualized_return.value, Some(0.));
    assert!(r.bear.missing_bear_intervals.is_empty());
    let partial = daily_returns(
        &[],
        Window {
            start_ms: 1,
            end_ms: DAY - 1,
        },
    )
    .unwrap();
    assert_eq!(partial.expected_intervals, 0);
    assert_eq!(partial.coverage, None);
}
#[test]
fn validations_prix_historiques_et_horloges_sans_filtrage_purge() {
    let w = Window {
        start_ms: DAY,
        end_ms: 2 * DAY,
    };
    let base = market(2);
    for prices in [
        vec![PricePoint {
            at_ms: DAY + 1,
            close: 1.,
        }],
        vec![
            PricePoint {
                at_ms: DAY,
                close: 1.,
            },
            PricePoint {
                at_ms: DAY,
                close: 2.,
            },
        ],
        vec![
            PricePoint {
                at_ms: 2 * DAY,
                close: 1.,
            },
            PricePoint {
                at_ms: DAY,
                close: 2.,
            },
        ],
    ] {
        let bad = MarketSeries {
            daily_closes: prices,
            ..base.clone()
        };
        assert!(regimes(&[], Some(&bad), &[], w).is_err());
    }
    for field in 0..4 {
        let mut t = trade(0, Some(1.));
        t.included = false;
        match field {
            0 => t.r_multiple = Some(f64::NAN),
            1 => t.exit_ms = -1,
            2 => t.entry_ms = t.exit_ms + 1,
            _ => t.symbol.clear(),
        };
        assert!(regimes(&[t], None, &[], w).is_err());
    }
    for equity in [-1., f64::NAN] {
        assert!(
            daily_returns(
                &[MarkPoint {
                    at_ms: DAY,
                    equity,
                    day_close: true
                }],
                w
            )
            .is_err()
        );
    }
    assert!(
        daily_returns(
            &[],
            Window {
                start_ms: DAY,
                end_ms: DAY
            }
        )
        .is_err()
    );
}
#[test]
fn day_close_hors_minuit_est_incoherent() {
    let marks = [MarkPoint {
        at_ms: DAY + 1,
        equity: 100.,
        day_close: true,
    }];
    assert!(
        daily_returns(
            &marks,
            Window {
                start_ms: DAY,
                end_ms: 2 * DAY
            }
        )
        .is_err()
    );
}
