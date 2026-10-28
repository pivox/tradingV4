//! Horloge commune et trou de cotation (docs S9 §4) : la grille est l'union des minutes des paires, une
//! jambe sans minute n'avance pas, et une jambe en position sans minute active pendant `T` (7 jours)
//! sort de force au dernier close, en taker, avec `exit_reason = quote_gap`.
//!
//! Marché inventé : prix plats à 100 sauf mention contraire.

mod portfolio_common;

use std::sync::{Arc, Mutex};

use portfolio_common::{
    DAY, MINUTE, Scripted, Spy, T0, buy_quote, buy_risk, config, flat_bar, flat_series, instrument,
    names, run, sell_all,
};
use tv4::engine::PortfolioConfig;
use tv4::model::{Candle, InstrumentRules, Slippage};
use tv4::report::writers::trades_csv_legs;
use tv4::sim::{ExitReason, SizingMode};
use tv4::strategy::{OrderKind, Signal, Size, Strategy};

const GAP_DAYS: i64 = 7;

/// X est présente `[0, 1000)` puis `[1000 + gap_minutes, …)`. Y est présente tout du long.
fn x_with_gap(gap_minutes: i64, after: i64, price_after: f64) -> Vec<Candle> {
    let mut x = flat_series(0, 1000, 100.0, 1.0e6);
    x.extend(
        (1000 + gap_minutes..1000 + gap_minutes + after)
            .map(|m| flat_bar(T0 + m * MINUTE, price_after, 1.0e6)),
    );
    x
}

fn buyer_at(minute: i64, risk: f64, stop: f64) -> Box<dyn Strategy> {
    Box::new(Scripted::new(vec![(T0 + minute * MINUTE, buy_risk(risk, stop))]).0)
}

/// Achat limit de 100 de quote à `price`, valable `ttl_bars` minutes, au signal `at_minute`.
fn limit_buyer_at(at_minute: i64, price: f64, ttl_bars: u32) -> Box<dyn Strategy> {
    limit_buyer_quote(at_minute, 100.0, price, ttl_bars)
}

/// Achat limit de `quote` de quote à `price`, valable `ttl_bars` minutes, au signal `at_minute`.
fn limit_buyer_quote(at_minute: i64, quote: f64, price: f64, ttl_bars: u32) -> Box<dyn Strategy> {
    let signal = Signal::EnterLong {
        size: Size::Quote(quote),
        kind: OrderKind::Limit { price, ttl_bars },
        stop: None,
        take_profit: None,
        tag: "limite",
    };
    Box::new(Scripted::new(vec![(T0 + at_minute * MINUTE, signal)]).0)
}

/// Achat market de 100 de quote au signal `at_minute`.
fn market_buyer_at(at_minute: i64) -> Box<dyn Strategy> {
    Box::new(Scripted::new(vec![(T0 + at_minute * MINUTE, buy_quote(100.0))]).0)
}

/// Achat market de `quote` de quote au signal `at_minute`, avec l'espion de ce que la stratégie a vu.
fn market_buyer_quote_spy(at_minute: i64, quote: f64) -> (Box<dyn Strategy>, Arc<Mutex<Spy>>) {
    let (strategy, spy) = Scripted::new(vec![(T0 + at_minute * MINUTE, buy_quote(quote))]);
    (Box::new(strategy), spy)
}

fn idle() -> Box<dyn Strategy> {
    Box::new(Scripted::new(Vec::new()).0)
}

fn compounding<'a>(
    universe: &'a [String],
    instruments: &'a [InstrumentRules],
) -> PortfolioConfig<'a> {
    PortfolioConfig {
        sizing: SizingMode::Compounding,
        slippage: Slippage::from_bps(5.0).unwrap(),
        ..config(universe, instruments, 1000.0)
    }
}

#[test]
fn un_trou_de_8_jours_force_la_sortie_a_g0_plus_7_jours() {
    // X achète à la minute 100 (stop 90), puis n'a plus de cotation pendant 8 jours. Y cote tout du
    // long : le moteur franchit l'échéance sans X.
    let gap_minutes = 8 * 1440;
    let x = x_with_gap(gap_minutes, 100, 100.0);
    let y = flat_series(0, 1000 + gap_minutes + 100, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![buyer_at(100, 0.01, 90.0), idle()],
        compounding(&universe, &instruments),
    );
    assert_eq!(result.trades.len(), 1, "{:?}", result.trades);
    let trade = &result.trades[0];
    assert_eq!(result.trade_legs, vec![0]);
    assert_eq!(trade.exit_reason, ExitReason::QuoteGap);
    assert!(!trade.forced_close);
    // g0 = fin de la dernière minute active (la 999) ; l'échéance est g0 + 7 jours.
    let g0 = T0 + 1000 * MINUTE;
    assert_eq!(trade.exit_ms, g0 + GAP_DAYS * DAY);
    assert_eq!(trade.exit_signal_ms, trade.exit_ms);
    assert_eq!(trade.exit_order_ms, trade.exit_ms);
    // Dernier close (100) moins 5 bps de slippage, frais taker (35 bps), aucun fill dans le trou.
    assert_eq!(trade.exit_ref_price, 100.0);
    assert!((trade.exit_price - 100.0 * (1.0 - 0.0005)).abs() < 1e-9);
    assert!((trade.exit_fee - trade.qty * trade.exit_price * 0.0035).abs() < 1e-9);
    assert_eq!(
        trade.exit_liquidity,
        tv4::sim::Liquidity::Taker,
        "sortie en taker"
    );
    assert!(trade.entry_ms < g0);
    assert_eq!(result.legs[0].detail.quote_gap_exits, 1);
    assert_eq!(result.legs[1].detail.quote_gap_exits, 0);
    assert_eq!(result.quote_gap_exits, 1);
    // Le stop (90) n'a jamais été touché : X revient à 100.
    assert_eq!(result.legs[0].counters.stops_triggered, 0);
    // La sortie est comptée dans toutes les métriques (réalisée, pas marquée).
    assert_eq!(result.n_trades_forced, 0);
    assert!(!result.forced_close);
    assert_eq!(result.pnl_marked, 0.0);
    assert_eq!(result.pnl_realized.to_bits(), trade.pnl_net.to_bits());
    // Le cash est revenu : l'equity finale est le cash.
    assert!((result.cash_final - result.equity_final).abs() < 1e-12);
    // `trades.csv` écrit le vocabulaire de S9.
    let csv = trades_csv_legs(
        &result.trades,
        &result.trade_legs,
        &["X-USD", "Y-USD"],
        result.window_last_open_ms + MINUTE - result.window_first_open_ms,
    )
    .unwrap();
    let text = String::from_utf8(csv).unwrap();
    assert!(text.contains(",quote_gap,"), "{text}");
    assert!(text.contains(",gap_forced_exit,"), "{text}");
    assert!(text.contains(",X-USD,"), "{text}");
}

#[test]
fn un_trou_de_6_jours_ne_force_rien_et_retarde_seulement_les_fills() {
    // X achète à la minute 100, puis émet une vente market à la dernière minute avant le trou. L'ordre
    // n'a pas de minute pour remplir pendant 6 jours : il remplit à la première minute de retour.
    let gap_minutes = 6 * 1440;
    let x = x_with_gap(gap_minutes, 100, 100.0);
    let y = flat_series(0, 1000 + gap_minutes + 100, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let x_strategy = Scripted::new(vec![
        (T0 + 100 * MINUTE, buy_risk(0.01, 90.0)),
        (T0 + 1000 * MINUTE, sell_all()),
    ])
    .0;
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![Box::new(x_strategy), idle()],
        compounding(&universe, &instruments),
    );
    assert_eq!(result.trades.len(), 1);
    let trade = &result.trades[0];
    assert_eq!(trade.exit_reason, ExitReason::Signal);
    assert_eq!(result.quote_gap_exits, 0);
    // Le fill de sortie a lieu à la première minute de X après le trou.
    assert_eq!(trade.exit_ms, T0 + (1000 + gap_minutes) * MINUTE);
    assert_eq!(trade.exit_signal_ms, T0 + 1000 * MINUTE);
}

#[test]
fn le_trou_est_juge_avant_l_on_bar_de_la_jambe_qui_revient() {
    // Une seule paire, qui revient après 8 jours avec une 1m qui crève son stop (low 1). La sortie forcée
    // précède l'`on_bar` : le trade sort en `quote_gap` au dernier close, pas en `stop`.
    let gap_minutes = 8 * 1440;
    let mut x = flat_series(0, 1000, 100.0, 1.0e6);
    x.push(Candle {
        open_time_ms: T0 + (1000 + gap_minutes) * MINUTE,
        open: 100.0,
        high: 100.0,
        low: 1.0,
        close: 50.0,
        volume: 1.0e6,
        trades: None,
    });
    x.extend(flat_series(1001 + gap_minutes, 50, 50.0, 1.0e6));
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let result = run(
        &[("X-USD", x)],
        vec![buyer_at(100, 0.01, 90.0)],
        compounding(&universe, &instruments),
    );
    assert_eq!(result.trades.len(), 1);
    let trade = &result.trades[0];
    assert_eq!(trade.exit_reason, ExitReason::QuoteGap);
    assert_eq!(trade.exit_ms, T0 + 1000 * MINUTE + GAP_DAYS * DAY);
    assert_eq!(trade.exit_ref_price, 100.0);
    assert_eq!(result.legs[0].counters.stops_triggered, 0);
}

#[test]
fn une_paire_sans_autre_minute_ne_sort_qu_apres_le_trou() {
    // Le trou n'atteint pas 7 jours : le stop agit normalement au retour, la sortie n'est pas forcée.
    let gap_minutes = 6 * 1440;
    let mut x = flat_series(0, 1000, 100.0, 1.0e6);
    x.push(Candle {
        open_time_ms: T0 + (1000 + gap_minutes) * MINUTE,
        open: 100.0,
        high: 100.0,
        low: 1.0,
        close: 50.0,
        volume: 1.0e6,
        trades: None,
    });
    x.extend(flat_series(1001 + gap_minutes, 50, 50.0, 1.0e6));
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.0)];
    let result = run(
        &[("X-USD", x)],
        vec![buyer_at(100, 0.01, 90.0)],
        compounding(&universe, &instruments),
    );
    assert_eq!(result.trades.len(), 1);
    assert_eq!(result.trades[0].exit_reason, ExitReason::Stop);
    assert_eq!(result.quote_gap_exits, 0);
}

#[test]
fn la_cotation_qui_s_arrete_est_soldee_pendant_que_les_autres_paires_vivent() {
    // X ne revient jamais : ses données s'arrêtent. Y continue 9 jours : le trou franchit 7 jours en
    // cours de fenêtre, la sortie est un quote_gap et non une clôture forcée de fin de fenêtre.
    let x = flat_series(0, 1000, 100.0, 1.0e6);
    let y = flat_series(0, 1000 + 9 * 1440, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![buyer_at(100, 0.01, 90.0), idle()],
        compounding(&universe, &instruments),
    );
    assert_eq!(result.trades.len(), 1);
    assert_eq!(result.trades[0].exit_reason, ExitReason::QuoteGap);
    assert!(!result.forced_close);
    assert_eq!(
        result.trades[0].exit_ms,
        T0 + 1000 * MINUTE + GAP_DAYS * DAY
    );
}

#[test]
fn un_trou_encore_ouvert_en_fin_de_fenetre_se_juge_comme_en_cours_de_fenetre() {
    // X s'arrête ; Y s'arrête 3 jours plus tard : le trou de X (3 jours) est sous le seuil, la position
    // est soldée par la clôture forcée de fin de fenêtre, au dernier close de X.
    let x = flat_series(0, 1000, 100.0, 1.0e6);
    let y = flat_series(0, 1000 + 3 * 1440, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let short = run(
        &[("X-USD", x.clone()), ("Y-USD", y)],
        vec![buyer_at(100, 0.01, 90.0), idle()],
        compounding(&universe, &instruments),
    );
    assert_eq!(short.trades.len(), 1);
    assert_eq!(short.trades[0].exit_reason, ExitReason::ForcedClose);
    assert!(short.forced_close);
    assert_eq!(short.quote_gap_exits, 0);
    // Avec 8 jours de fenêtre après le dernier close de X, le seuil est atteint à la fin de fenêtre.
    let y = flat_series(0, 1000 + 8 * 1440, 50.0, 1.0e6);
    let long = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![buyer_at(100, 0.01, 90.0), idle()],
        compounding(&universe, &instruments),
    );
    assert_eq!(long.trades[0].exit_reason, ExitReason::QuoteGap);
    assert!(!long.forced_close);
}

#[test]
fn la_grille_est_l_union_des_minutes_a_partir_du_depart_commun() {
    // X : minutes 0, 1, 2, 5 et 6. Y : minutes 1, 2, 3 et 4. Toutes les jambes démarrent à la même
    // minute, la plus tardive des premières (la 1) : la minute 0 de X n'est pas jouée. Union : 1 à 6.
    let x: Vec<Candle> = [0, 1, 2, 5, 6]
        .iter()
        .map(|m| flat_bar(T0 + m * MINUTE, 100.0, 1.0e6))
        .collect();
    let y: Vec<Candle> = [1, 2, 3, 4]
        .iter()
        .map(|m| flat_bar(T0 + m * MINUTE, 200.0, 1.0e6))
        .collect();
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![idle(), idle()],
        compounding(&universe, &instruments),
    );
    assert_eq!(result.window_start_ms, T0 + MINUTE);
    let expected: Vec<i64> = (1..=6).map(|m| T0 + m * MINUTE).collect();
    assert_eq!(result.grid_open_ms, expected);
    assert_eq!(result.equity.len(), 6);
    assert_eq!(result.grid_minutes, 6);
    assert_eq!(result.legs[0].rows, 4);
    assert_eq!(result.legs[0].start_index, 1);
    assert_eq!(result.legs[1].rows, 4);
    assert_eq!(result.legs[1].start_index, 0);
    // Sans position, l'equity reste le capital.
    assert!(result.equity.iter().all(|e| *e == 1000.0));
    assert_eq!(result.bars_in_position, 0);
}

// ---------------------------------------------------------------------------------------------
// Entrées en attente d'une jambe à plat : échéances de TTL et trou de cotation (docs S9 §4.2, §4.3)
// ---------------------------------------------------------------------------------------------

/// Config du duel X / Y : une seule place (`max_positions = 1`), mode capitalisé.
fn one_slot<'a>(universe: &'a [String], instruments: &'a [InstrumentRules]) -> PortfolioConfig<'a> {
    PortfolioConfig {
        max_positions: 1,
        ..compounding(universe, instruments)
    }
}

#[test]
fn un_achat_market_en_attente_est_annule_a_g0_plus_t_sur_une_jambe_a_plat() {
    // (a) X achète au marché à la dernière minute avant un trou de 8 jours. Aucune minute ne permet de
    // remplir pendant le trou, et un ordre market n'expire pas : à g0 + 7 jours, l'entrée en attente est
    // annulée. Y entre pendant le trou, après l'échéance, sans être refusée par `max_positions`, et X ne
    // trade pas à son retour sur un signal vieux de 8 jours.
    let gap_minutes = 8 * 1440;
    let x = x_with_gap(gap_minutes, 100, 200.0);
    let y = flat_series(0, 1000 + gap_minutes + 100, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    // Y entre 5 minutes après l'échéance g0 + 7 jours (la 1m 1000 + 7 × 1440).
    let y_signal = 1000 + 7 * 1440 + 5;
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![market_buyer_at(1000), market_buyer_at(y_signal)],
        one_slot(&universe, &instruments),
    );
    assert_eq!(result.trade_legs, vec![1], "{:?}", result.trades);
    assert_eq!(result.max_positions_rejected, 0);
    assert_eq!(result.legs[0].max_positions_rejected, 0);
    assert_eq!(result.legs[1].max_positions_rejected, 0);
    // L'ordre de X est compté annulé, et l'entonnoir garde son identité : un ordre, aucun fill.
    let x_leg = &result.legs[0];
    assert_eq!(x_leg.counters.orders_cancelled, 1);
    assert_eq!(x_leg.counters.fills, 0);
    assert_eq!(x_leg.counters.orders_unfilled_at_end, 0);
    assert_eq!(x_leg.funnel.entry_signals, 1);
    assert_eq!(x_leg.funnel.entry_orders, 1);
    assert_eq!(x_leg.funnel.entry_rejections.total(), 0);
    assert_eq!(result.quote_gap_exits, 0);
    // Y est entrée à la 1m qui suit son signal, bien avant le retour de X.
    assert!(result.trades[0].entry_ms < T0 + (1000 + gap_minutes) * MINUTE);
}

#[test]
fn un_trou_sous_le_seuil_laisse_l_achat_market_remplir_au_retour() {
    // Contre-épreuve : avec un trou de 6 jours (sous T), rien n'est annulé et l'achat remplit à la
    // première minute de retour, comme la vente market du test des 6 jours.
    let gap_minutes = 6 * 1440;
    let x = x_with_gap(gap_minutes, 100, 200.0);
    let y = flat_series(0, 1000 + gap_minutes + 100, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![market_buyer_at(1000), idle()],
        one_slot(&universe, &instruments),
    );
    assert_eq!(result.trade_legs, vec![0]);
    assert_eq!(result.legs[0].counters.orders_cancelled, 0);
    assert_eq!(
        result.trades[0].entry_ms,
        T0 + (1000 + gap_minutes) * MINUTE
    );
}

#[test]
fn une_limit_d_entree_echue_pendant_un_trou_court_libere_sa_place() {
    // (b) X pose une limit d'achat à 90 (jamais traversée) avec un TTL de 60 minutes, puis n'a plus de
    // cotation pendant 2 jours (sous T : pas de trou de cotation). Le TTL est échu à la 1m 1061 ; Y entre
    // une heure plus tard et n'est pas refusée par `max_positions`. X ne remplit pas à son retour.
    let gap_minutes = 2 * 1440;
    let x = x_with_gap(gap_minutes, 100, 100.0);
    let y = flat_series(0, 1000 + gap_minutes + 100, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![
            limit_buyer_at(1000, 90.0, 60),
            market_buyer_at(1000 + 60 + 60),
        ],
        one_slot(&universe, &instruments),
    );
    assert_eq!(result.trade_legs, vec![1], "{:?}", result.trades);
    assert_eq!(result.max_positions_rejected, 0);
    let x_leg = &result.legs[0];
    assert_eq!(x_leg.counters.limits_expired, 1);
    assert_eq!(x_leg.counters.orders_cancelled, 0);
    assert_eq!(x_leg.counters.orders_unfilled_at_end, 0);
    assert_eq!(x_leg.funnel.entry_orders, 1);
    assert_eq!(result.quote_gap_exits, 0);
}

#[test]
fn la_limit_echue_d_une_paire_delistee_ne_bloque_pas_les_autres() {
    // X pose une limit d'entrée puis ses données s'arrêtent (délistage) ; Y continue 3 jours et entre
    // bien après l'échéance du TTL. Sans l'application des échéances aux jambes sans bougie, la place et
    // la réservation de X resteraient bloquées jusqu'à la fin de la fenêtre.
    let x = flat_series(0, 1000, 100.0, 1.0e6);
    let y = flat_series(0, 1000 + 3 * 1440, 50.0, 1.0e6);
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let result = run(
        &[("X-USD", x), ("Y-USD", y)],
        vec![limit_buyer_at(1000, 90.0, 5), market_buyer_at(1000 + 120)],
        one_slot(&universe, &instruments),
    );
    assert_eq!(result.trade_legs, vec![1], "{:?}", result.trades);
    assert_eq!(result.max_positions_rejected, 0);
    // L'ordre sort en expiration au fil de la fenêtre, pas en « abandonné à la fin ».
    let x_leg = &result.legs[0];
    assert_eq!(x_leg.counters.limits_expired, 1);
    assert_eq!(x_leg.counters.orders_unfilled_at_end, 0);
}

/// Duel de cash à l'échéance d'un TTL (docs S9 §4.2, étape 2a), mode capitalisé, N = 8, cash 1 000.
///
/// Les deux paires ont une 1m à chaque minute. ZZZ est la plus liquide, donc rangée avant AAA (le rang du
/// jour 2 lit les volumes des jours 0 et 1) : son `on_bar` précède celui d'AAA. AAA pose à la minute 3000
/// une limit d'achat de 700 de quote à 90 (jamais remplie : le marché est plat à 100), valable
/// `ttl_bars` minutes, qui réserve 701,4 de quote. ZZZ achète 500 au marché, au signal de la minute
/// 3005, et le fill tombe à la 1m 3006, minute où le TTL de 5 bars d'AAA est échu (activation 3001,
/// échéance 3006).
///
/// Sans la limit d'AAA, le quote libre de ZZZ est 1 000 : 500 entrent en entier (le plafond de cash est
/// de 9,47 lots, pour 5 demandés). Avec elle, le quote libre est 298,6 et le plafond tombe à 2,83 lots.
/// Rend le résultat et ce que la stratégie de ZZZ a vu à son signal.
fn ttl_expiry_cash_duel(ttl_bars: u32) -> (tv4::engine::PortfolioResult, Arc<Mutex<Spy>>) {
    let minutes = 2 * 1440 + 180;
    let universe = names(&["AAA-USD", "ZZZ-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let (buyer, spy) = market_buyer_quote_spy(3005, 500.0);
    let result = run(
        &[
            ("AAA-USD", flat_series(0, minutes, 100.0, 5.0e5)),
            ("ZZZ-USD", flat_series(0, minutes, 100.0, 1.0e6)),
        ],
        vec![limit_buyer_quote(3000, 700.0, 90.0, ttl_bars), buyer],
        PortfolioConfig {
            max_positions: 8,
            ..compounding(&universe, &instruments)
        },
    );
    (result, spy)
}

#[test]
fn une_limit_echue_a_t_ne_plafonne_pas_l_achat_market_d_une_jambe_rangee_avant() {
    let (result, spy) = ttl_expiry_cash_duel(5);
    // Les deux jambes ont une bougie à chaque minute, et ZZZ (indice 1) est rangée avant AAA.
    assert_eq!(result.legs[0].rows, result.legs[1].rows);
    // À son signal (minute 3005), ZZZ voit la réservation d'AAA : 1 000 − 701,4 de quote libre.
    let free = spy
        .lock()
        .unwrap()
        .states
        .iter()
        .find(|(now_ms, ..)| *now_ms == T0 + 3005 * MINUTE)
        .map(|(.., quote_free)| *quote_free)
        .expect("ZZZ est appelée à la minute 3005");
    // (La quantité de la limit est arrondie au pas de lot : 1e-8 lot, soit quelques 1e-6 de quote.)
    assert!(
        (free - (1000.0 - 700.0 * 1.002)).abs() < 1e-5,
        "libre = {free}"
    );
    // Le fill de ZZZ tombe à la minute de l'échéance du TTL d'AAA : l'échéance passe avant l'`on_bar` de
    // ZZZ, qui n'est plus plafonnée par une réservation morte.
    assert_eq!(result.trade_legs, vec![1], "{:?}", result.trades);
    let trade = &result.trades[0];
    assert_eq!(trade.entry_ms, T0 + 3006 * MINUTE);
    assert!(!trade.capped_by_cash, "plafonnée par une limit échue");
    // Quantité demandée en entier : 500 de quote au prix de l'open majoré de 5 bps de slippage.
    let full = 500.0 / (100.0 * (1.0 + 5e-4));
    assert!((trade.qty - full).abs() < 1e-7, "qty = {}", trade.qty);
    assert_eq!(result.legs[1].counters.entries_capped_by_cash, 0);
    // La limit d'AAA sort en expiration, sans fill, avec son compteur.
    let aaa = &result.legs[0];
    assert_eq!(aaa.counters.limits_expired, 1);
    assert_eq!(aaa.counters.orders_cancelled, 0);
    assert_eq!(aaa.counters.fills, 0);
    assert_eq!(aaa.counters.orders_unfilled_at_end, 0);
    assert_eq!(result.max_positions_rejected, 0);
}

#[test]
fn la_meme_limit_encore_valable_a_t_plafonne_bien_l_achat_market() {
    // Contre-épreuve : avec un TTL d'un bar de plus, la limit d'AAA est encore valable à la minute du
    // fill de ZZZ (échéance 3007) et son quote réservé plafonne l'achat. Le test précédent mesure donc
    // bien l'effet de l'échéance, pas un plafond qui n'existerait pas.
    let (result, _spy) = ttl_expiry_cash_duel(6);
    assert_eq!(result.trade_legs, vec![1], "{:?}", result.trades);
    let trade = &result.trades[0];
    assert_eq!(trade.entry_ms, T0 + 3006 * MINUTE);
    assert!(trade.capped_by_cash);
    assert!(trade.qty < 5.0 && trade.qty > 2.0, "qty = {}", trade.qty);
    assert_eq!(result.legs[1].counters.entries_capped_by_cash, 1);
    // La limit expire une minute plus tard, comme dans le premier cas.
    assert_eq!(result.legs[0].counters.limits_expired, 1);
}
