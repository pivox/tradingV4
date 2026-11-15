//! Oracles indépendants (Python statistics.NormalDist), calcul hors simulation.
use tv4::research::noise::noise_context;

#[test]
fn les_trois_reperes_du_protocole_corrige_sont_reproduits() {
    for (n, years, expected, q90, q95) in [
        (
            436,
            2.0,
            2.129177505212912,
            2.4652874565350458,
            2.6005802983838495,
        ),
        (
            250,
            5.0,
            1.269047768077612,
            1.4861306696064442,
            1.5801692449982165,
        ),
        (
            50,
            5.0,
            1.017993690855926,
            1.250923339845915,
            1.3786667933089218,
        ),
    ] {
        let result = noise_context(n, years).expect("fenêtre positive");
        assert_eq!(result.n, n);
        assert_eq!(result.n_eff, None);
        assert!((result.expected.expect("espérance") - expected).abs() < 1e-7);
        assert!((result.approx_q90.expect("q90") - q90).abs() < 1e-12);
        assert!((result.q95.expect("q95") - q95).abs() < 1e-7);
    }
}

#[test]
fn zero_et_un_essai_ne_creent_pas_de_statistique_infinie() {
    let zero = noise_context(0, 5.0).expect("aucun essai");
    assert_eq!(zero.expected, None);
    assert_eq!(zero.approx_q90, None);
    assert_eq!(zero.q95, None);
    let one = noise_context(1, 1.0).expect("un essai");
    assert_eq!(one.expected, Some(0.0));
    assert_eq!(one.approx_q90, Some(0.0));
    assert!((one.q95.expect("normale standard") - 1.6448536269514722).abs() < 1e-7);
}

#[test]
fn les_fenetres_invalides_sont_refusees_meme_sans_essai() {
    for years in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(noise_context(0, years).is_err());
        assert!(noise_context(50, years).is_err());
    }
}

#[test]
fn grand_n_ne_perd_pas_la_queue_par_soustraction_a_un() {
    let value = noise_context(u64::MAX, 1.0).expect("grand compteur");
    assert!((value.q95.expect("q95 fini") - 9.397995696545438).abs() < 1e-7);
    assert!(value.expected.expect("espérance finie").is_finite());
}

#[test]
fn le_bruit_monte_avec_n_et_diminue_avec_la_duree_de_selection() {
    let mut previous = 0.0;
    for n in [1, 2, 3, 10, 50, 250, 436, 10_000, u64::MAX] {
        let a = noise_context(n, 1.0).expect("un an");
        let b = noise_context(n, 4.0).expect("quatre ans");
        assert!(a.q95.expect("q95") > previous);
        previous = a.q95.expect("q95");
        assert!((b.q95.expect("q95") * 2.0 - previous).abs() < 1e-12);
    }
}
