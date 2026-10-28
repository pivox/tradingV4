//! Priorité des signaux d'une même minute (docs S9 §5) : horodatage d'abord, puis rang de liquidité 90 j
//! du jour UTC, puis l'ordre de l'univers en cas d'égalité exacte, jamais l'alphabet.
//!
//! Marché inventé : chaque paire est plate à 100, avec un volume constant par jour. Les deux paires
//! émettent un achat market à minuit du jour 2 ; `max_positions = 1` ne laisse entrer que la première
//! soumise, l'autre est refusée et comptée (`max_positions_rejected`). Le gagnant est donc la paire que
//! le moteur a servie en premier.
//!
//! Les derniers tests couvrent l'autre moitié de la règle de §5.1 : le même rang sert aux `on_bar` de la
//! minute (étape 2 de §4.2). Avec N ≥ 2, aucune entrée n'est refusée à la soumission ; deux achats market
//! simultanés qui dépassent le cash remplissent sur la même 1m, et c'est l'ordre des `on_bar` qui décide
//! quelle jambe est plafonnée par le cash (« le cash ne permet qu'une entrée complète », §5.4).

mod portfolio_common;

use portfolio_common::{
    DAY, MINUTE, Scripted, T0, buy_quote, config, flat_bar, instrument, names, run,
};
use tv4::engine::{PortfolioConfig, PortfolioResult};
use tv4::model::Candle;
use tv4::sim::SizingMode;
use tv4::strategy::Strategy;

/// Instant du signal : minuit du jour 2 (la 1m 23:59 du jour 1 clôt à cet instant).
const SIGNAL_MS: i64 = T0 + 2 * DAY;

/// Paire plate à 100 sur 2 jours et 3 heures, dont le volume de la 1m vaut `volume(jour)`. `missing`
/// retire une 1m.
fn leg(volume: impl Fn(i64) -> f64, missing: Option<i64>) -> Vec<Candle> {
    (0..2 * 1440 + 180)
        .map(|m| T0 + m * MINUTE)
        .filter(|open| Some(*open) != missing)
        .map(|open| flat_bar(open, 100.0, volume((open - T0).div_euclid(DAY))))
        .collect()
}

fn config_n1<'a>(
    universe: &'a [String],
    instruments: &'a [tv4::model::InstrumentRules],
) -> PortfolioConfig<'a> {
    PortfolioConfig {
        max_positions: 1,
        sizing: SizingMode::Compounding,
        ..config(universe, instruments, 1000.0)
    }
}

fn buyer() -> Box<dyn Strategy> {
    Box::new(Scripted::new(vec![(SIGNAL_MS, buy_quote(50.0))]).0)
}

/// Lance deux paires qui achètent à minuit du jour 2, dans l'ordre `universe`.
fn duel(universe: &[&str], data: Vec<(&str, Vec<Candle>)>) -> PortfolioResult {
    let universe = names(universe);
    let instruments = vec![instrument(0.0); universe.len()];
    run(
        &data,
        (0..universe.len()).map(|_| buyer()).collect(),
        config_n1(&universe, &instruments),
    )
}

/// Indice (dans l'univers) de l'unique paire qui a trade, après avoir vérifié que l'autre a été refusée
/// par `max_positions`.
fn winner(result: &PortfolioResult) -> usize {
    assert_eq!(
        result.trades.len(),
        1,
        "un seul trade : {:?}",
        result.trades
    );
    let k = usize::from(result.trade_legs[0]);
    let loser = 1 - k;
    assert_eq!(result.legs[k].funnel.entry_orders, 1);
    assert_eq!(result.legs[loser].funnel.entry_orders, 0);
    assert_eq!(result.legs[loser].max_positions_rejected, 1);
    assert_eq!(result.max_positions_rejected, 1);
    // L'identité de l'entonnoir : un signal accepté ou refusé, jamais perdu.
    for leg in &result.legs {
        assert_eq!(
            leg.funnel.entry_signals,
            leg.funnel.entry_orders
                + leg.funnel.entry_rejections.total()
                + leg.max_positions_rejected
        );
    }
    k
}

#[test]
fn a_horodatage_egal_la_paire_la_plus_liquide_est_servie_la_premiere() {
    // ZZZ est la plus liquide : elle entre, quelle que soit sa place alphabétique ou dans l'univers.
    let result = duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![
            ("AAA-USD", leg(|_| 1.0, None)),
            ("ZZZ-USD", leg(|_| 5.0, None)),
        ],
    );
    assert_eq!(winner(&result), 1);
    // Les volumes inversés inversent l'entrée.
    let result = duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![
            ("AAA-USD", leg(|_| 5.0, None)),
            ("ZZZ-USD", leg(|_| 1.0, None)),
        ],
    );
    assert_eq!(winner(&result), 0);
}

#[test]
fn l_horodatage_passe_avant_le_rang() {
    // ZZZ est la plus liquide, mais sa 1m de 23:59 manque : son signal naît une minute plus tard (à la
    // 1m 00:00), AAA (moins liquide) a déjà été servie.
    let result = duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![
            ("AAA-USD", leg(|_| 1.0, None)),
            ("ZZZ-USD", leg(|_| 5.0, Some(SIGNAL_MS - MINUTE))),
        ],
    );
    assert_eq!(winner(&result), 0);
    assert_eq!(result.trades[0].entry_signal_ms, SIGNAL_MS);
}

#[test]
fn jamais_l_alphabet() {
    // Mêmes données dans le même ordre d'univers, noms inversés : le résultat ne change pas, à la colonne
    // `symbol` près.
    let small = || leg(|_| 1.0, None);
    let large = || leg(|_| 5.0, None);
    let a = duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![("AAA-USD", small()), ("ZZZ-USD", large())],
    );
    let b = duel(
        &["ZZZ-USD", "AAA-USD"],
        vec![("ZZZ-USD", small()), ("AAA-USD", large())],
    );
    assert_eq!(winner(&a), 1);
    assert_eq!(winner(&b), 1);
    assert_eq!(a.trades, b.trades);
    assert_eq!(a.trade_legs, b.trade_legs);
    assert_eq!(a.equity, b.equity);
    assert_eq!(a.universe[1], "ZZZ-USD");
    assert_eq!(b.universe[1], "AAA-USD");
}

#[test]
fn a_scores_egaux_l_ordre_de_l_univers_departage() {
    let same = || leg(|_| 3.0, None);
    // Univers [AAA, ZZZ] : la première de la liste gagne.
    let a = duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![("AAA-USD", same()), ("ZZZ-USD", same())],
    );
    assert_eq!(winner(&a), 0);
    // Univers [ZZZ, AAA] : la première de la liste gagne, c'est maintenant ZZZ.
    let b = duel(
        &["ZZZ-USD", "AAA-USD"],
        vec![("AAA-USD", same()), ("ZZZ-USD", same())],
    );
    assert_eq!(winner(&b), 0);
    // L'univers fixe, des noms sans rapport avec l'alphabet : même gagnant.
    let c = duel(
        &["QQQ-USD", "BBB-USD"],
        vec![("QQQ-USD", same()), ("BBB-USD", same())],
    );
    assert_eq!(winner(&c), 0);
    // Le hash distingue les deux ordres (le départage en dépend).
    assert_ne!(a.config_hash, b.config_hash);
}

#[test]
fn le_rang_du_jour_ne_depend_pas_des_volumes_du_jour() {
    // AAA a un volume énorme à partir du jour 2 (le jour du signal) : le rang du jour 2 ne regarde que
    // les jours 0 et 1, où ZZZ est plus liquide. ZZZ entre.
    let result = duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![
            ("AAA-USD", leg(|day| if day >= 2 { 1e9 } else { 1.0 }, None)),
            ("ZZZ-USD", leg(|_| 5.0, None)),
        ],
    );
    assert_eq!(winner(&result), 1);
    // Ces volumes ne changent pas non plus le rang calculé le jour 2 : `RankTable` est causale.
    assert_eq!(result.priority.rule, "signal_time_then_liquidity_90d");
    assert_eq!(result.priority.liquidity_window_days, 90);
    // Moins de 90 jours de données : tous les jours de la fenêtre sont partiels.
    assert!(result.priority.partial_days >= 2);
}

// ---------------------------------------------------------------------------------------------
// Rang des `on_bar` : fills concurrents sur le cash, N >= 2 (docs S9 §5.1 et §5.4)
// ---------------------------------------------------------------------------------------------

/// Paire plate à 100 de volume constant `volume`, sans trou.
fn constant_leg(volume: f64) -> Vec<Candle> {
    leg(|_| volume, None)
}

/// Lance deux paires qui achètent chacune 800 au marché à minuit du jour 2, cash 1 000, N = 8 : les deux
/// entrées sont acceptées à la soumission, mais le cash ne permet qu'une entrée complète.
fn cash_duel(universe: &[&str], data: Vec<(&str, Vec<Candle>)>) -> PortfolioResult {
    let universe = names(universe);
    let instruments = vec![instrument(0.0); universe.len()];
    let strategies: Vec<Box<dyn Strategy>> = (0..universe.len())
        .map(|_| {
            Box::new(Scripted::new(vec![(SIGNAL_MS, buy_quote(800.0))]).0) as Box<dyn Strategy>
        })
        .collect();
    run(
        &data,
        strategies,
        PortfolioConfig {
            max_positions: 8,
            sizing: SizingMode::Compounding,
            ..config(&universe, &instruments, 1000.0)
        },
    )
}

/// Résultat d'une jambe dans un duel de cash : `(capped_by_cash, qty, entries_capped_by_cash)`.
fn cash_outcome(result: &PortfolioResult, leg: usize) -> (bool, f64, u64) {
    let index = result
        .trade_legs
        .iter()
        .position(|k| usize::from(*k) == leg)
        .unwrap_or_else(|| panic!("la jambe {leg} n'a pas tradé : {:?}", result.trades));
    let trade = &result.trades[index];
    (
        trade.capped_by_cash,
        trade.qty,
        result.legs[leg].counters.entries_capped_by_cash,
    )
}

/// Vérifie qu'un duel de cash a servi `first` en entier et plafonné l'autre, avec N = 8 (aucun refus
/// `max_positions`).
fn assert_first_served(result: &PortfolioResult, first: usize) {
    let second = 1 - first;
    assert_eq!(result.trades.len(), 2, "{:?}", result.trades);
    assert_eq!(result.max_positions_rejected, 0);
    let (capped, qty, counter) = cash_outcome(result, first);
    assert!(!capped, "la jambe servie en premier n'est pas plafonnée");
    assert!((qty - 8.0).abs() < 1e-9, "qty = {qty}");
    assert_eq!(counter, 0);
    let (capped, qty, counter) = cash_outcome(result, second);
    assert!(capped, "la jambe servie ensuite est plafonnée par le cash");
    assert!(qty < 8.0 && qty > 0.0, "qty = {qty}");
    assert_eq!(counter, 1);
}

#[test]
fn le_cash_va_d_abord_a_la_paire_la_plus_liquide_aux_on_bar() {
    // ZZZ (indice 1) est la plus liquide : elle remplit en entier, AAA est plafonnée par le cash restant.
    let result = cash_duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![
            ("AAA-USD", constant_leg(10.0)),
            ("ZZZ-USD", constant_leg(50.0)),
        ],
    );
    assert_first_served(&result, 1);
    // Les volumes inversés inversent le plafonnement : la boucle de l'étape 2 suit le rang.
    let result = cash_duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![
            ("AAA-USD", constant_leg(50.0)),
            ("ZZZ-USD", constant_leg(10.0)),
        ],
    );
    assert_first_served(&result, 0);
}

#[test]
fn le_cash_ne_suit_jamais_l_alphabet_aux_on_bar() {
    // Mêmes données, noms inversés : la jambe liquide (indice 1 dans les deux univers) est servie en
    // premier dans les deux cas, et les fills sont identiques.
    let a = cash_duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![
            ("AAA-USD", constant_leg(10.0)),
            ("ZZZ-USD", constant_leg(50.0)),
        ],
    );
    let b = cash_duel(
        &["ZZZ-USD", "AAA-USD"],
        vec![
            ("ZZZ-USD", constant_leg(10.0)),
            ("AAA-USD", constant_leg(50.0)),
        ],
    );
    assert_first_served(&a, 1);
    assert_first_served(&b, 1);
    assert_eq!(a.trades, b.trades);
    assert_eq!(a.trade_legs, b.trade_legs);
    assert_eq!(a.equity, b.equity);
    assert_eq!(a.universe[1], "ZZZ-USD");
    assert_eq!(b.universe[1], "AAA-USD");
}

#[test]
fn a_scores_egaux_l_univers_decide_qui_le_cash_sert_en_premier() {
    let same = || constant_leg(3.0);
    // Univers [AAA, ZZZ] : la première de la liste est servie en premier.
    let a = cash_duel(
        &["AAA-USD", "ZZZ-USD"],
        vec![("AAA-USD", same()), ("ZZZ-USD", same())],
    );
    assert_first_served(&a, 0);
    // Univers [ZZZ, AAA] : la première de la liste est ZZZ, qui est maintenant servie en premier.
    let b = cash_duel(
        &["ZZZ-USD", "AAA-USD"],
        vec![("AAA-USD", same()), ("ZZZ-USD", same())],
    );
    assert_first_served(&b, 0);
    assert_eq!(b.universe[0], "ZZZ-USD");
}
