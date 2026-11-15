mod research_stats_common;
use research_stats_common::trade;
use tv4::research::stats::{
    Availability, MarkPoint, StatsOptions, StatsSizing, TradeClosure, TradeRecord, Window,
    monte_carlo,
};
const DAY: i64 = 86_400_000;
fn fixture() -> (Vec<TradeRecord>, Vec<TradeClosure>, Vec<MarkPoint>, Window) {
    let trades: Vec<_> = (0..40).map(|i| trade(i, Some(1.))).collect();
    let closures = (0..40)
        .map(|row| TradeClosure {
            row,
            at_ms: if row < 20 { 10 * DAY } else { 20 * DAY },
        })
        .collect();
    let marks = (0..=40)
        .map(|day| MarkPoint {
            at_ms: day * DAY,
            equity: 100.,
            day_close: day > 0,
        })
        .collect();
    (
        trades,
        closures,
        marks,
        Window {
            start_ms: 0,
            end_ms: 40 * DAY,
        },
    )
}
fn options() -> StatsOptions {
    let mut o = StatsOptions::new(42);
    o.samples = 100;
    o
}
#[test]
fn plat_partition_atomique_et_cadence_circulaire_sans_perte_idle() {
    let (t, c, m, w) = fixture();
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    assert_eq!(s.availability, Availability::Complete);
    assert_eq!(
        (
            s.n_trades,
            s.n_r,
            s.n_removed,
            s.requested_block_trades,
            s.n_blocks
        ),
        (40, 40, 8, 20, 2)
    );
    assert_eq!(s.block_sizes, vec![20, 20]);
    assert_eq!(s.block_durations_ms, vec![30 * DAY, 10 * DAY]);
    assert_eq!(s.source_duration_ms, 40 * DAY);
    assert_eq!(s.cadence_trades_per_year, Some(365.25));
    assert!(
        s.drawdown_grid.contains("tous points fournis")
            && s.drawdown_grid.contains("quotidiennes")
            && s.drawdown_grid.contains("blocs")
    );
    for value in [
        &s.dd_initial_p95_2y,
        &s.dd_peak_p95_2y,
        &s.return_p5_2y,
        &s.probability_dd20_1y,
    ] {
        assert_eq!(value.value, Some(0.));
    }
    assert_eq!(s.removal_expectancy_median_r.value, Some(1.));
}
#[test]
fn choc_mtm_sans_pnl_de_cloture_et_liquidation_finale_comptent() {
    let (mut t, c, mut m, w) = fixture();
    for trade in &mut t {
        trade.pnl_net = 0.;
    }
    m[5].equity = 50.;
    let a = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    assert_eq!(a.dd_initial_p95_2y.value, Some(0.5));
    assert_eq!(a.probability_dd20_1y.value, Some(1.));
    // Coût terminal porté par le point final, même sans trade conservé à cette date.
    m[40].equity = 99.;
    let b = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    assert!(b.return_p5_2y.value.unwrap() < 0.);
    assert!(b.dd_initial_p95_2y.value.unwrap() > 0.5);
}
#[test]
fn faillite_absorbante_et_sizing_distincts() {
    let (t, c, mut m, w) = fixture();
    for (i, p) in m.iter_mut().enumerate() {
        p.equity = 100. - i as f64;
    }
    let r = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    let k = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Compounding, &options()).unwrap();
    assert_eq!(r.return_p5_2y.value, Some(-1.));
    assert_eq!(r.dd_initial_p95_2y.value, Some(1.));
    assert!(k.return_p5_2y.value.unwrap() > -1.);
    assert!(k.return_p5_2y.value.unwrap() < 0.);
}
#[test]
fn manquants_purge_seuils_et_pas_de_proxy() {
    let (mut t, c, m, w) = fixture();
    t[0].r_multiple = None;
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    assert_eq!(s.n_r, 39);
    assert_eq!(s.availability, Availability::Partial);
    assert_eq!(s.removal_expectancy_median_r.value, None);
    assert_eq!(s.dd_initial_p95_2y.value, Some(0.));
    for (closures, marks) in [
        (&c[..0], &m[..]),
        (&c[..], &m[..0]),
        (&c[..39], &m[..]),
        (&c[..], &m[1..]),
    ] {
        let s = monte_carlo(
            &t,
            closures,
            marks,
            w,
            100.,
            StatsSizing::Research,
            &options(),
        )
        .unwrap();
        assert_eq!(s.dd_initial_p95_2y.value, None);
    }
    let mut incomplete = m.clone();
    incomplete.remove(9);
    assert_eq!(
        monte_carlo(
            &t,
            &c,
            &incomplete,
            w,
            100.,
            StatsSizing::Research,
            &options()
        )
        .unwrap()
        .dd_initial_p95_2y
        .value,
        None
    );
    let mut missing_boundary = m.clone();
    missing_boundary.remove(10);
    assert_eq!(
        monte_carlo(
            &t,
            &c,
            &missing_boundary,
            w,
            100.,
            StatsSizing::Research,
            &options()
        )
        .unwrap()
        .dd_initial_p95_2y
        .value,
        None
    );
    t[0].included = false;
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    assert_eq!((s.n_trades, s.n_r, s.n_removed), (39, 39, 7));
    assert_eq!(s.block_sizes, vec![39]);
    assert_eq!(s.dd_initial_p95_2y.value, None);
    assert_eq!(s.removal_expectancy_median_r.value, Some(1.));
}
#[test]
fn refuse_entrees_incoherentes_et_overflow() {
    let (t, c, m, w) = fixture();
    let run = |t: &[TradeRecord], c: &[TradeClosure], m: &[MarkPoint], w: Window, capital| {
        monte_carlo(t, c, m, w, capital, StatsSizing::Research, &options())
    };
    for capital in [0., -1., f64::NAN, f64::INFINITY] {
        assert!(run(&t, &c, &m, w, capital).is_err());
    }
    let mut bad = c.clone();
    bad.push(c[0]);
    assert!(run(&t, &bad, &m, w, 100.).is_err());
    let mut bad = c.clone();
    bad[0].row = 1000;
    assert!(run(&t, &bad, &m, w, 100.).is_err());
    let mut bad = c.clone();
    bad[0].at_ms = i64::MAX;
    assert!(run(&t, &bad, &m, w, 100.).is_err());
    let mut bad = m.clone();
    bad[0].equity = f64::NAN;
    assert!(run(&t, &c, &bad, w, 100.).is_err());
    let mut bad = m.clone();
    bad.push(m[0]);
    assert!(run(&t, &c, &bad, w, 100.).is_err());
    let mut bad = m.clone();
    bad[0].equity = 99.;
    assert!(run(&t, &c, &bad, w, 100.).is_err());
    let mut bad = t.clone();
    bad[1].row = 0;
    assert!(run(&bad, &c, &m, w, 100.).is_err());
    assert!(
        run(
            &[],
            &[],
            &[],
            Window {
                start_ms: i64::MIN,
                end_ms: i64::MAX
            },
            100.
        )
        .is_err()
    );
}
#[test]
fn ordre_stable_et_flux_suppression_independant() {
    let (mut t, mut c, mut m, w) = fixture();
    for (i, p) in m.iter_mut().enumerate() {
        p.equity = 100. + (i % 7) as f64;
    }
    for (i, p) in t.iter_mut().enumerate() {
        p.r_multiple = Some(i as f64);
    }
    let a = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    t.reverse();
    c.reverse();
    m.reverse();
    assert_eq!(
        a,
        monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap()
    );
    let mut other = options();
    other.seed += 1;
    assert_ne!(
        a,
        monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &other).unwrap()
    );
    c.clear();
    assert_eq!(
        a.removal_expectancy_median_r,
        monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options())
            .unwrap()
            .removal_expectancy_median_r
    );
}

#[test]
fn purge_ne_cree_pas_de_frontiere_et_zero_source_reste_absorbant() {
    let (mut t, mut c, mut m, w) = fixture();
    let mut purged = trade(100, None);
    purged.included = false;
    t.push(purged);
    c.push(TradeClosure {
        row: 100,
        at_ms: 35 * DAY,
    });
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    assert_eq!(s.block_durations_ms, vec![30 * DAY, 10 * DAY]);
    for p in &mut m[25..] {
        p.equity = 0.;
    }
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Compounding, &options()).unwrap();
    // Le wrap commence à equity source 100, atteint un facteur 0 au jour 25,
    // puis les 0/0 de la queue ne sont jamais nécessaires : equity synthétique=0.
    assert_eq!(s.availability, Availability::Complete);
    assert_eq!(s.dd_initial_p95_2y.value, Some(1.));
    assert_eq!(s.dd_peak_p95_2y.value, Some(1.));
    assert_eq!(s.return_p5_2y.value, Some(-1.));
    assert_eq!(s.probability_dd20_1y.value, Some(1.));
    assert_eq!(s.removal_expectancy_median_r.value, Some(1.));
    assert_eq!(
        monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options())
            .unwrap()
            .return_p5_2y
            .value,
        Some(-1.)
    );
    m[40].equity = 1.;
    assert!(monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).is_err());
}

#[test]
fn tirage_uniforme_des_blocs_et_non_des_trades_avec_groupes_20_1_1_40() {
    const YEAR: i64 = 31_557_600_000;
    let t: Vec<_> = (0..62).map(|i| trade(i, Some(1.))).collect();
    let c: Vec<_> = (0..62)
        .map(|row| TradeClosure {
            row,
            at_ms: match row {
                0..20 => 2 * YEAR,
                20 => 5 * YEAR / 2,
                21 => 3 * YEAR,
                _ => 4 * YEAR,
            },
        })
        .collect();
    let w = Window {
        start_ms: 0,
        end_ms: 4 * YEAR,
    };
    let mut m: Vec<_> = (0..=w.end_ms / DAY)
        .map(|d| MarkPoint {
            at_ms: d * DAY,
            equity: if d == 1 { 50. } else { 100. },
            day_close: d > 0,
        })
        .collect();
    for at_ms in [2 * YEAR, 5 * YEAR / 2, 3 * YEAR, 4 * YEAR] {
        if !m.iter().any(|p| p.at_ms == at_ms) {
            m.push(MarkPoint {
                at_ms,
                equity: 100.,
                day_close: false,
            });
        }
    }
    let mut o = options();
    o.samples = 10_000;
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &o).unwrap();
    assert_eq!(s.block_sizes, vec![20, 42]);
    assert_eq!(s.block_durations_ms, vec![2 * YEAR, 2 * YEAR]);
    assert!(
        (s.probability_dd20_1y.value.unwrap() - 0.5).abs() < 0.02,
        "{s:?}"
    );
}

#[test]
fn oracle_python_exhaustif_deux_blocs_et_dd_capital_initial_superieur_a_un() {
    // Oracle Python indépendant : itertools.product([A,B],repeat=2), capital=100.
    // A : rendements [0.5,2.2] ; B : [0.5,8.0]. Quantiles numpy method='linear'.
    // DD/C0=[.55,.55,2.,2.], DD/sommet=[.5]*4, rendement=[.21,3.4,3.4,15.].
    // p95(DD)=2 ; p5(rendement)=.6885 en loi EXACTE pondérée 4 points.
    // Pour l'empirique 10k, la masse AA>=5% donne p5=.21 (quantile de la CDF discrète).
    const YEAR: i64 = 31_557_600_000;
    let t: Vec<_> = (0..40).map(|i| trade(i, Some(1.))).collect();
    let c: Vec<_> = (0..40)
        .map(|row| TradeClosure {
            row,
            at_ms: if row < 20 { YEAR } else { 2 * YEAR },
        })
        .collect();
    let w = Window {
        start_ms: 0,
        end_ms: 2 * YEAR,
    };
    let value = |at: i64| {
        if at < DAY {
            100.
        } else if at < YEAR {
            50.
        } else if at < YEAR + DAY {
            110.
        } else if at < 2 * YEAR {
            55.
        } else {
            440.
        }
    };
    let mut m: Vec<_> = (0..=w.end_ms / DAY)
        .map(|d| MarkPoint {
            at_ms: d * DAY,
            equity: value(d * DAY),
            day_close: d > 0,
        })
        .collect();
    for at_ms in [YEAR, YEAR + DAY, 2 * YEAR] {
        m.push(MarkPoint {
            at_ms,
            equity: value(at_ms),
            day_close: false,
        });
    }
    let mut o = options();
    o.samples = 10_000;
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Compounding, &o).unwrap();
    assert!(
        (s.dd_initial_p95_2y.value.unwrap() - 2.).abs() < 1e-14,
        "{s:?}"
    );
    assert!((s.dd_peak_p95_2y.value.unwrap() - 0.5).abs() < 1e-14);
    assert!((s.return_p5_2y.value.unwrap() - 0.21).abs() < 1e-14);
    assert_eq!(s.probability_dd20_1y.value, Some(1.));
}

#[test]
fn horizon_partiel_tient_le_dernier_point_sans_point_futur_ni_ratio_hors_horizon() {
    const YEAR: i64 = 31_557_600_000;
    let t: Vec<_> = (0..40).map(|i| trade(i, Some(1.))).collect();
    let c: Vec<_> = (0..40)
        .map(|row| TradeClosure {
            row,
            at_ms: if row < 20 { 3 * YEAR } else { 6 * YEAR },
        })
        .collect();
    let w = Window {
        start_ms: 0,
        end_ms: 6 * YEAR,
    };
    let mut m: Vec<_> = (0..=w.end_ms / DAY)
        .map(|d| MarkPoint {
            at_ms: d * DAY,
            equity: if d * DAY > 11 * YEAR / 2 { 0. } else { 100. },
            day_close: d > 0,
        })
        .collect();
    for at_ms in [3 * YEAR, 6 * YEAR] {
        m.push(MarkPoint {
            at_ms,
            equity: if at_ms == 6 * YEAR { 0. } else { 100. },
            day_close: false,
        });
    }
    // Chaque bloc dure trois ans. Le premier zéro source n'arrive qu'après 2,5 ans
    // dans le second : aucune projection ne peut atteindre ce point ou le 0/0 suivant.
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Compounding, &options()).unwrap();
    assert_eq!(s.dd_initial_p95_2y.value, Some(0.));
    assert_eq!(s.return_p5_2y.value, Some(0.));
    assert_eq!(s.probability_dd20_1y.value, Some(0.));
}

#[test]
fn aucun_retrait_effectif_et_fenetre_trop_courte_restent_indisponibles() {
    let (t, c, m, w) = fixture();
    let mut o = options();
    o.samples = 2;
    let empty = monte_carlo(&[], &[], &[], w, 100., StatsSizing::Research, &o).unwrap();
    assert_eq!(empty.availability, Availability::Unavailable);
    assert_eq!(empty.n_removed, 0);
    assert_eq!(empty.removal_expectancy_median_r.value, None);
    let small = monte_carlo(&t[..4], &c[..4], &m, w, 100., StatsSizing::Research, &o).unwrap();
    assert_eq!(small.n_removed, 0);
    assert_eq!(small.dd_initial_p95_2y.value, None);
    assert_eq!(small.removal_expectancy_median_r.value, None);
    let short = Window {
        start_ms: 0,
        end_ms: 20 * DAY,
    };
    let s = monte_carlo(&t, &c, &m[..=20], short, 100., StatsSizing::Research, &o).unwrap();
    assert_eq!(s.dd_initial_p95_2y.value, None);
    assert!(s.dd_initial_p95_2y.reasons[0].contains("30 jours"));
    assert_eq!(s.removal_expectancy_median_r.value, Some(1.));
}

#[test]
fn ratio_nul_avant_tout_facteur_zero_est_non_identifiable() {
    let (t, c, mut m, w) = fixture();
    for p in &mut m[5..] {
        p.equity = 0.;
    }
    // Le wrap commence après la faillite source (jour 20), avant le préfixe
    // contenant le facteur zéro : le premier rendement est un 0/0 nécessaire.
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Compounding, &options()).unwrap();
    assert_eq!(s.availability, Availability::Partial);
    for metric in [
        &s.dd_initial_p95_2y,
        &s.dd_peak_p95_2y,
        &s.return_p5_2y,
        &s.probability_dd20_1y,
    ] {
        assert_eq!(metric.value, None);
        assert!(metric.reasons[0].contains("nulle"));
    }
    assert_eq!(s.removal_expectancy_median_r.value, Some(1.));
}

#[test]
fn research_ne_calcule_pas_un_ratio_inutilise_qui_deborde() {
    let (t, c, mut m, w) = fixture();
    m[5].equity = 1e-310;
    // Tous les marquages et deltas en quote sont finis. Seul le quotient
    // 100 / 1e-310 déborde : il n'appartient pas au sizing Research.
    let s = monte_carlo(&t, &c, &m, w, 100., StatsSizing::Research, &options()).unwrap();
    assert_eq!(s.availability, Availability::Complete);
    assert_eq!(s.dd_initial_p95_2y.value, Some(1.));
    assert_eq!(s.return_p5_2y.value, Some(-1.));
    assert_eq!(s.probability_dd20_1y.value, Some(1.));
    assert!(matches!(
        monte_carlo(&t, &c, &m, w, 100., StatsSizing::Compounding, &options()),
        Err(tv4::research::stats::StatsError::NonFiniteResult)
    ));
}
