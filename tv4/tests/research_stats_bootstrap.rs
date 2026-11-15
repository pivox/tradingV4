mod research_stats_common;
use research_stats_common::{GAUSSIAN, ORACLE_LB90, ORACLE_SE, trade};
use tv4::research::stats::{Availability, StatsOptions, bootstrap, quantile_type7};

#[test]
fn options_et_quantiles_type7_explicites() {
    let o = StatsOptions::new(17);
    assert_eq!((o.seed, o.samples, o.bootstrap_block), (17, 10_000, 5));
    assert_eq!(o.correlation_threshold, 0.7);
    assert_eq!(
        quantile_type7(&[40., 10., 30., 20.], 0.1).unwrap(),
        Some(13.)
    );
    assert_eq!(
        quantile_type7(&[40., 10., 30., 20.], 0.5).unwrap(),
        Some(25.)
    );
    assert_eq!(
        quantile_type7(&[40., 10., 30., 20.], 0.).unwrap(),
        Some(10.)
    );
    assert_eq!(
        quantile_type7(&[40., 10., 30., 20.], 1.).unwrap(),
        Some(40.)
    );
    assert_eq!(quantile_type7(&[], 0.5).unwrap(), None);
    assert_eq!(quantile_type7(&[4.], 0.5).unwrap(), Some(4.));
    for p in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        assert!(quantile_type7(&[1., 2.], p).is_err());
    }
    assert!(quantile_type7(&[f64::NAN], 0.5).is_err());
}

#[test]
fn oracle_gaussien_independant_et_reproductibilite() {
    let trades: Vec<_> = GAUSSIAN
        .iter()
        .enumerate()
        .map(|(i, &r)| trade(i, Some(r)))
        .collect();
    let a = bootstrap(&trades, &StatsOptions::new(42)).unwrap();
    assert_eq!(a.n_trades, 200);
    assert_eq!(a.n_r, 200);
    assert_eq!(a.r_coverage, Some(1.));
    assert_eq!(a.availability, Availability::Complete);
    assert!((a.mean_r.value.unwrap() - 0.25).abs() < 1e-14);
    // ±0,006 = marge conservative > 4 erreurs MC sur 10 000 réplications.
    assert!(
        (a.standard_error_r.value.unwrap() - ORACLE_SE).abs() < 0.006,
        "{a:?}"
    );
    assert!((ORACLE_SE - 0.0884).abs() < 0.002, "{ORACLE_SE}");
    assert!(
        (a.lb90_r.value.unwrap() - ORACLE_LB90).abs() < 0.007,
        "{a:?}"
    );
    let bytes = serde_json::to_vec(&a).unwrap();
    assert_eq!(
        bytes,
        serde_json::to_vec(&bootstrap(&trades, &StatsOptions::new(42)).unwrap()).unwrap()
    );
    assert_ne!(
        bytes,
        serde_json::to_vec(&bootstrap(&trades, &StatsOptions::new(43)).unwrap()).unwrap()
    );
    let mut reversed = trades.clone();
    reversed.reverse();
    assert_eq!(a, bootstrap(&reversed, &StatsOptions::new(42)).unwrap());
}

#[test]
fn signes_et_blocs_preservent_l_autocorrelation() {
    let o = StatsOptions::new(1);
    for value in [-0.5, 0.5] {
        let t: Vec<_> = (0..20).map(|i| trade(i, Some(value))).collect();
        let a = bootstrap(&t, &o).unwrap();
        assert_eq!(a.mean_r.value, Some(value));
        assert_eq!(a.lb90_r.value, Some(value));
        assert_eq!(a.standard_error_r.value, Some(0.));
    }
    let correlated: Vec<_> = (0..200)
        .map(|i| trade(i, Some(if i < 100 { -1. } else { 1. })))
        .collect();
    let alternating: Vec<_> = (0..200)
        .map(|i| trade(i, Some(if i % 2 == 0 { -1. } else { 1. })))
        .collect();
    assert!(
        bootstrap(&correlated, &o)
            .unwrap()
            .standard_error_r
            .value
            .unwrap()
            > 4. * bootstrap(&alternating, &o)
                .unwrap()
                .standard_error_r
                .value
                .unwrap()
    );
}

#[test]
fn effectifs_r_manquants_et_purge_ne_fabriquent_pas_de_precision() {
    let o = StatsOptions::new(2);
    let empty = bootstrap(&[], &o).unwrap();
    assert_eq!(empty.n_trades, 0);
    assert_eq!(empty.r_coverage, None);
    assert_eq!(empty.availability, Availability::Unavailable);
    let one = bootstrap(&[trade(0, Some(2.))], &o).unwrap();
    assert_eq!(one.mean_r.value, Some(2.));
    assert_eq!(one.lb90_r.value, None);
    assert_eq!(one.standard_error_r.value, None);
    assert_eq!(one.availability, Availability::Partial);
    let missing = bootstrap(
        &[trade(0, Some(2.)), trade(1, None), trade(2, Some(3.))],
        &o,
    )
    .unwrap();
    assert_eq!(
        (missing.n_trades, missing.n_r, missing.r_coverage),
        (3, 2, Some(2. / 3.))
    );
    assert_eq!(missing.mean_r.value, None);
    assert_eq!(missing.lb90_r.value, None);
    assert_eq!(missing.standard_error_r.value, None);
    assert!(!missing.mean_r.reasons.is_empty());
    let mut purged = trade(1, None);
    purged.included = false;
    let kept = bootstrap(&[trade(0, Some(2.)), purged, trade(2, Some(2.))], &o).unwrap();
    assert_eq!((kept.n_trades, kept.n_r), (2, 2));
    assert_eq!(kept.mean_r.value, Some(2.));
}

#[test]
fn refuse_les_options_et_valeurs_non_finies() {
    let t = [trade(0, Some(1.)), trade(1, Some(2.))];
    for block in [0, 4, 11] {
        let mut o = StatsOptions::new(1);
        o.bootstrap_block = block;
        assert!(bootstrap(&t, &o).is_err());
    }
    for samples in [0, 1] {
        let mut o = StatsOptions::new(1);
        o.samples = samples;
        assert!(bootstrap(&t, &o).is_err());
    }
    for threshold in [-0.1, 1., f64::NAN] {
        let mut o = StatsOptions::new(1);
        o.correlation_threshold = threshold;
        assert!(bootstrap(&t, &o).is_err());
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut bad = t.clone();
        bad[0].r_multiple = Some(value);
        assert!(bootstrap(&bad, &StatsOptions::new(0)).is_err());
        let mut bad = t.clone();
        bad[0].pnl_net = value;
        assert!(bootstrap(&bad, &StatsOptions::new(0)).is_err());
    }
    let mut small = StatsOptions::new(1);
    small.samples = 2;
    assert!(bootstrap(&t, &small).is_ok());
}

#[test]
fn entrees_partagees_numeriques_serialisables() {
    use tv4::research::stats::{CashPoint, MarkPoint, PricePoint, StatsSizing, Window};
    let window = Window {
        start_ms: 0,
        end_ms: 86_400_000,
    };
    let mark = MarkPoint {
        at_ms: 0,
        equity: 300.,
        day_close: false,
    };
    let price = PricePoint {
        at_ms: 0,
        close: 100.,
    };
    let cash = CashPoint {
        at_ms: 0,
        cash: 300.,
    };
    let json = serde_json::to_string(&(
        window,
        mark,
        price,
        cash,
        StatsSizing::Research,
        StatsSizing::Compounding,
    ))
    .unwrap();
    assert!(json.contains("research") && json.contains("compounding") && json.contains("cash"));
}

#[test]
fn la_graine_u64_est_une_chaine_decimale_stricte_sans_perte() {
    for seed in [0, 42, u64::MAX] {
        let options = StatsOptions::new(seed);
        let mut json = serde_json::to_value(&options).unwrap();
        assert_eq!(json["seed"], serde_json::Value::String(seed.to_string()));
        assert_eq!(
            serde_json::from_value::<StatsOptions>(json.clone()).unwrap(),
            options
        );
        json["seed"] = serde_json::json!(42);
        assert!(serde_json::from_value::<StatsOptions>(json).is_err());
    }
    for invalid in [
        "",
        "-1",
        "+1",
        " 1",
        "1 ",
        "01",
        "1.0",
        "18446744073709551616",
    ] {
        let mut json = serde_json::to_value(StatsOptions::new(0)).unwrap();
        json["seed"] = serde_json::Value::String(invalid.into());
        assert!(
            serde_json::from_value::<StatsOptions>(json).is_err(),
            "{invalid}"
        );
    }
}
