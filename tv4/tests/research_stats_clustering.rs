use tv4::research::stats::{DatedReturn, PeerSeries, StatsOptions, Window, clustering};
const DAY: i64 = 86_400_000;
fn peer(hash: &str, values: &[f64]) -> PeerSeries {
    PeerSeries {
        config_hash: hash.into(),
        dataset_hash: "data".into(),
        line: "shared".into(),
        capital: 100.,
        window: Window {
            start_ms: 0,
            end_ms: values.len() as i64 * DAY,
        },
        daily_returns: values
            .iter()
            .enumerate()
            .map(|(i, &value)| DatedReturn {
                start_ms: i as i64 * DAY,
                end_ms: (i as i64 + 1) * DAY,
                value,
            })
            .collect(),
    }
}
#[test]
fn constantes_singletons_dedup_et_couverture_globale() {
    let a = peer("a", &[0., 0., 0.]);
    let b = peer("b", &[0., 0., 0.]);
    let p = [a.clone(), b, a];
    let r = clustering(&p, Some(3), &StatsOptions::new(1)).unwrap();
    assert_eq!(r.observed_configs, 2);
    assert_eq!(r.clusters.len(), 2);
    assert_eq!(r.global_n_eff.value, None);
    let r = clustering(&p, Some(2), &StatsOptions::new(1)).unwrap();
    assert_eq!(r.global_n_eff.value, Some(2.));
    let mut bad = p[0].clone();
    bad.daily_returns[0].value = 0.1;
    assert!(clustering(&[p[0].clone(), bad], Some(2), &StatsOptions::new(1)).is_err());
}
#[test]
fn connexite_transitive_et_seuil_strict() {
    // Orthogonal centered vectors x,y with equal norm: A=x, B=.8x+.6y, C=.28x+.96y.
    let a = peer("a", &[0.1, -0.1, 0.1, -0.1]);
    let b = peer("b", &[0.14, -0.02, 0.02, -0.14]);
    let c = peer("c", &[0.124, 0.068, -0.068, -0.124]);
    let r = clustering(&[c, b, a], Some(3), &StatsOptions::new(1)).unwrap();
    assert_eq!(
        r.clusters,
        vec![vec!["a".to_string(), "b".to_string(), "c".to_string()]]
    );
    let a = peer("a", &[0.1, -0.1, 0.1, -0.1]);
    let b = peer("b", &[1., 1., -1., -1.]);
    let mut o = StatsOptions::new(1);
    o.correlation_threshold = 0.;
    assert_eq!(clustering(&[a, b], None, &o).unwrap().clusters.len(), 2);
}
#[test]
fn refuse_grille_incomplete_capital_dataset_ligne_et_n_incoherents() {
    let a = peer("a", &[0.1, 0.2, 0.3]);
    let mut bad = a.clone();
    bad.config_hash = "b".into();
    bad.daily_returns.remove(1);
    assert!(clustering(&[a.clone(), bad], None, &StatsOptions::new(1)).is_err());
    for field in 0..4 {
        let mut b = a.clone();
        b.config_hash = "b".into();
        match field {
            0 => b.capital = 101.,
            1 => b.dataset_hash = "other".into(),
            2 => b.line = "other".into(),
            _ => b.daily_returns[0].value = f64::NAN,
        };
        assert!(clustering(&[a.clone(), b], Some(2), &StatsOptions::new(1)).is_err());
    }
    assert!(clustering(&[a], Some(0), &StatsOptions::new(1)).is_err());
}
#[test]
fn frontiere_07_strictement_superieure_sans_epsilon_economique() {
    let x = peer("x", &[0.1, -0.1, 0., 0.]);
    // Orthogonal residual with norm sqrt(.51) gives Pearson exactly .7 analytically.
    let z = 0.51_f64.sqrt() * 0.1;
    let y = peer("y", &[0.07, -0.07, z, -z]);
    let r = clustering(&[x, y], Some(2), &StatsOptions::new(1)).unwrap();
    assert_eq!(r.clusters.len(), 2);
    let empty = clustering(&[], None, &StatsOptions::new(1)).unwrap();
    assert_eq!(empty.global_n_eff.value, None);
}
#[test]
fn entree_invalide_et_ordre_stable() {
    let a = peer("a", &[0.1, 0.2, 0.3]);
    let b = peer("b", &[0.1, 0.2, 0.3]);
    let o = StatsOptions::new(1);
    assert_eq!(
        clustering(&[b.clone(), a.clone()], Some(2), &o),
        clustering(&[a.clone(), b], Some(2), &o)
    );
    assert_eq!(
        clustering(std::slice::from_ref(&a), Some(2), &o)
            .unwrap()
            .coverage,
        Some(0.5)
    );
    for field in 0..5 {
        let mut p = a.clone();
        match field {
            0 => p.config_hash.clear(),
            1 => p.capital = 0.,
            2 => p.daily_returns[0].end_ms += 1,
            3 => p.daily_returns[0].value = -1.01,
            _ => p.window.end_ms = p.window.start_ms,
        };
        assert!(clustering(&[p], None, &o).is_err());
    }
    let one = peer("one", &[0.1]);
    assert_eq!(
        clustering(&[one], Some(1), &o).unwrap().global_n_eff.value,
        Some(1.)
    );
}
