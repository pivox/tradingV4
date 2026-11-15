mod research_stats_common;
use research_stats_common::trade;
use tv4::research::stats::{Availability, concentration};
use tv4::store::time::parse_rfc3339_utc;

#[test]
fn dix_trades_avec_retraits_identifies_et_mois_utc() {
    let mut t: Vec<_> = (0..10).map(|i| trade(i, None)).collect();
    for (i, v) in t.iter_mut().enumerate() {
        v.pnl_net = (i + 1) as f64;
        v.symbol = if i < 5 { "BTC-USDC" } else { "ETH-USDC" }.into();
        v.exit_ms = parse_rfc3339_utc(if i < 5 {
            "2026-01-31T23:59:59Z"
        } else {
            "2026-02-01T00:00:00Z"
        })
        .unwrap();
    }
    let a = concentration(&t).unwrap();
    assert_eq!(a.n_trades, 10);
    assert_eq!(a.total_pnl, 55.);
    let month = a.best_month.as_ref().unwrap();
    assert_eq!(month.id, "2026-02");
    assert_eq!(month.pnl, 40.);
    assert_eq!(month.pnl_without, 15.);
    assert_eq!(a.best_pair.as_ref().unwrap().id, "ETH-USDC");
    assert_eq!(a.best_month_positive_share.value, Some(40. / 55.));
    assert_eq!(a.top3_share.value, Some(27. / 55.));
    assert_eq!(
        a.without_top
            .iter()
            .map(|x| (x.count, x.pnl_without.value))
            .collect::<Vec<_>>(),
        [(1, Some(45.)), (3, Some(28.)), (5, Some(15.)), (10, None)]
    );
    assert_eq!(
        a.without_top[1]
            .removed
            .iter()
            .map(|x| x.row)
            .collect::<Vec<_>>(),
        [9, 8, 7]
    );
    assert_eq!(a.availability, Availability::Partial);
    t.reverse();
    assert_eq!(a, concentration(&t).unwrap());
}

#[test]
fn egalites_lexicales_et_purge() {
    let mut b = trade(0, None);
    b.symbol = "BBB".into();
    b.pnl_net = 2.;
    b.exit_ms = 2_678_400_000;
    let mut a = trade(1, None);
    a.symbol = "AAA".into();
    a.pnl_net = 2.;
    a.exit_ms = 0;
    let mut purged = trade(2, None);
    purged.included = false;
    purged.pnl_net = 100.;
    let r = concentration(&[b, a, purged]).unwrap();
    assert_eq!(r.best_pair.unwrap().id, "AAA");
    assert_eq!(r.best_month.unwrap().id, "1970-01");
    assert_eq!(r.without_top[0].removed[0].symbol, "AAA");
    assert_eq!(r.without_top[0].pnl_without.value, Some(2.));
}

#[test]
fn pertes_denominations_nulles_et_effectifs_insuffisants() {
    for values in [[-2., -1.], [2., -2.]] {
        let t: Vec<_> = values
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let mut t = trade(i, None);
                t.pnl_net = p;
                t
            })
            .collect();
        let a = concentration(&t).unwrap();
        assert_eq!(a.top3_share.value, None);
        assert_eq!(a.best_month_positive_share.value, None);
        assert_eq!(a.without_top[1].pnl_without.value, None);
    }
    let e = concentration(&[]).unwrap();
    assert_eq!(e.availability, Availability::Unavailable);
    assert_eq!(e.best_month, None);
    assert_eq!(e.top3_share.value, None);
    let one = concentration(&[trade(0, None)]).unwrap();
    assert_eq!(one.without_top[0].pnl_without.value, None);
}

#[test]
fn entree_non_finie_prix_invalides_et_debordements_refuses() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut t = trade(0, None);
        t.pnl_net = value;
        assert!(concentration(&[t]).is_err());
    }
    for value in [0., -1., f64::NAN] {
        let mut t = trade(0, None);
        t.entry_price = value;
        assert!(concentration(&[t]).is_err());
    }
    let mut t = trade(0, None);
    t.pnl_net = f64::MAX;
    assert!(concentration(&[t.clone(), t]).is_err());
}

#[test]
fn les_zeros_signes_sont_des_egalites_lexicales() {
    let mut a = trade(0, None);
    a.symbol = "AAA".into();
    a.pnl_net = -0.;
    let mut b = trade(1, None);
    b.symbol = "BBB".into();
    b.pnl_net = 0.;
    let mut c = trade(2, None);
    c.pnl_net = -1.;
    let r = concentration(&[a, b, c]).unwrap();
    assert_eq!(r.without_top[0].removed[0].symbol, "AAA");
}

#[test]
fn onze_trades_permettent_tous_les_retraits() {
    let t: Vec<_> = (0..11)
        .map(|i| {
            let mut t = trade(i, None);
            t.pnl_net = (i + 1) as f64;
            t
        })
        .collect();
    let a = concentration(&t).unwrap();
    assert_eq!(a.availability, Availability::Complete);
    assert_eq!(
        a.without_top
            .iter()
            .map(|x| x.pnl_without.value)
            .collect::<Vec<_>>(),
        [Some(55.), Some(36.), Some(21.), Some(1.)]
    );
    assert_eq!(a.top3_share.value, Some(30. / 66.));
    assert_eq!(a.best_month_positive_share.value, Some(1.));
}

#[test]
fn le_ratio_mensuel_utilise_les_mois_positifs_quand_le_total_perd() {
    let mut a = trade(0, None);
    a.pnl_net = 2.;
    a.exit_ms = 0;
    let mut b = trade(1, None);
    b.pnl_net = -10.;
    b.exit_ms = 2_678_400_000;
    let result = concentration(&[a, b]).unwrap();
    assert_eq!(result.total_pnl, -8.);
    assert_eq!(result.best_month_positive_share.value, Some(1.));
    assert_eq!(result.top3_share.value, None);
}

#[test]
fn retrait_du_meilleur_mois_preserve_un_petit_reste_signe() {
    for remainder in [1., -1.] {
        let mut best = trade(0, None);
        best.pnl_net = 1e16;
        best.exit_ms = parse_rfc3339_utc("2026-01-15T00:00:00Z").unwrap();
        let mut remaining = trade(1, None);
        remaining.pnl_net = remainder;
        remaining.exit_ms = parse_rfc3339_utc("2026-02-15T00:00:00Z").unwrap();
        let result = concentration(&[best, remaining]).unwrap();
        let removed = result.best_month.unwrap();
        assert_eq!(removed.id, "2026-01");
        assert_eq!(removed.pnl_without, remainder, "reste {remainder}");
        assert_eq!(result.without_top[0].pnl_without.value, Some(remainder));
    }
}

#[test]
fn retrait_de_la_meilleure_paire_preserve_un_petit_reste_signe() {
    for remainder in [1., -1.] {
        let mut best = trade(0, None);
        best.pnl_net = 1e16;
        best.symbol = "AAA".into();
        let mut remaining = trade(1, None);
        remaining.pnl_net = remainder;
        remaining.symbol = "BBB".into();
        let result = concentration(&[best, remaining]).unwrap();
        let removed = result.best_pair.unwrap();
        assert_eq!(removed.id, "AAA");
        assert_eq!(removed.pnl_without, remainder, "reste {remainder}");
        assert_eq!(result.without_top[0].pnl_without.value, Some(remainder));
    }
}
