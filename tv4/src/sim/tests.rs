//! Tests unitaires du simulateur S6. Les chronologies sont écrites bougie par bougie : le signal émis à
//! la clôture de la 1m `i` porte `signal_ms = open_time[i] + 60 000` et, avec les défauts, n'est actif
//! qu'à l'open de la 1m `i + 2`.

use super::*;
use crate::model::ExchangeId;
use proptest::prelude::*;

const T0: Ts = 1_704_067_200_000;

fn okx() -> FeeSchedule {
    FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap()
}

fn bps(value: f64) -> Slippage {
    Slippage::from_bps(value).unwrap()
}

/// Tick 0,1 et lot 1e-8, sans minimum.
fn inst() -> InstrumentRules {
    InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).unwrap()
}

fn sim_with(rules: SimRules, slippage: Slippage, cash: f64) -> Simulator {
    Simulator::new(okx(), slippage, rules, inst(), cash, 16).unwrap()
}

fn sim_defaults(cash: f64) -> Simulator {
    sim_with(SimRules::default(), Slippage::ZERO, cash)
}

/// Règles à la S3 : fill à l'open de la bougie suivante, aucune limite de volume.
fn rules_s3() -> SimRules {
    SimRules {
        activation_delay_bars: 0,
        max_volume_share: 1e9,
        ..SimRules::default()
    }
}

/// 1m d'index `index`, volume 1e6 (la règle de volume ne gêne pas).
fn bar(index: i64, open: f64, high: f64, low: f64, close: f64) -> Candle {
    bar_v(index, open, high, low, close, 1e6)
}

fn bar_v(index: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Candle {
    Candle {
        open_time_ms: T0 + index * 60_000,
        open,
        high,
        low,
        close,
        volume,
        trades: None,
    }
}

fn flat(index: i64, price: f64) -> Candle {
    bar(index, price, price, price, price)
}

/// Contexte d'un signal émis à la clôture de la 1m `index`.
fn at(index: i64) -> SubmitContext {
    SubmitContext {
        signal_ms: T0 + (index + 1) * 60_000,
        equity: 1000.0,
        ref_price: 100.0,
    }
}

fn enter(quote: f64, stop: Option<f64>, tp: Option<f64>) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Market,
        stop,
        take_profit: tp,
        tag: "entree",
    }
}

fn exit_market() -> Instruction {
    Instruction::Exit {
        price: OrderPrice::Market,
        tag: "sortie",
    }
}

fn place(side: Side, price: f64, quote: f64, slot: u16, ttl: u32) -> Instruction {
    Instruction::Place {
        side,
        price,
        size: SizeSpec::Quote(quote),
        ttl_bars: ttl,
        slot,
        tag: "grille",
    }
}

fn close_to(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

// ---------------------------------------------------------------------------------------------
// Règles
// ---------------------------------------------------------------------------------------------

#[test]
fn regles_par_defaut_valides() {
    let r = SimRules::default();
    assert!(r.validate().is_ok());
    assert_eq!(r.version, "s6.1");
    assert_eq!(r.max_pending, 64);
    assert_eq!(r.market_buy_cash_margin, 0.05);
    assert_eq!(r.activation_delay_bars, 1);
    assert_eq!(r.limit_cross_ticks, 1);
    assert_eq!(r.intrabar, IntrabarRule::StopFirst);
    assert!(r.tp_ignored_on_fill_bar);
    assert!(r.zero_volume_no_fill);
    assert_eq!(r.max_volume_share, 0.005);
    assert_eq!(r.stop_stress_bps, 0.0);
}

#[test]
fn regles_invalides() {
    let d = SimRules::default();
    let invalide = |rules: SimRules| matches!(rules.validate(), Err(SimError::InvalidRules { .. }));
    assert!(invalide(SimRules {
        version: "s3-market-only",
        ..d
    }));
    for max_pending in [0, 257] {
        assert!(invalide(SimRules { max_pending, ..d }), "{max_pending}");
    }
    assert!(!invalide(SimRules {
        max_pending: 256,
        ..d
    }));
    for marge in [0.0, 1.0, f64::NAN, -0.1, 1.5, 1e-17, 1e-10] {
        assert!(
            invalide(SimRules {
                market_buy_cash_margin: marge,
                ..d
            }),
            "marge {marge}"
        );
    }
    assert!(invalide(SimRules {
        activation_delay_bars: 1441,
        ..d
    }));
    assert!(!invalide(SimRules {
        activation_delay_bars: 1440,
        ..d
    }));
    assert!(invalide(SimRules {
        limit_cross_ticks: 1001,
        ..d
    }));
    for part in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            invalide(SimRules {
                max_volume_share: part,
                ..d
            }),
            "part {part}"
        );
    }
    for stress in [-1.0, f64::NAN, 10_000.0, f64::INFINITY] {
        assert!(
            invalide(SimRules {
                stop_stress_bps: stress,
                ..d
            }),
            "stress {stress}"
        );
    }
    // Le cumul du slippage et du stress est contrôlé à la construction.
    let rules = SimRules {
        stop_stress_bps: 9_000.0,
        ..d
    };
    assert!(matches!(
        Simulator::new(okx(), bps(1_000.0), rules, inst(), 100.0, 1),
        Err(SimError::InvalidRules { .. })
    ));
    for cash in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            matches!(
                Simulator::new(okx(), Slippage::ZERO, d, inst(), cash, 1),
                Err(SimError::InvalidInitialCash { .. })
            ),
            "capital {cash}"
        );
    }
    let mauvais = InstrumentRules {
        tick_size: 0.3,
        ..inst()
    };
    assert!(matches!(
        Simulator::new(okx(), Slippage::ZERO, d, mauvais, 100.0, 1),
        Err(SimError::Model(ModelError::InvalidInstrument { .. }))
    ));
}

// ---------------------------------------------------------------------------------------------
// Frais, slippage, plafond de cash (valeurs à la main de S3, délai 0)
// ---------------------------------------------------------------------------------------------

#[test]
fn frais_et_slippage_a_la_main() {
    let mut sim = sim_with(rules_s3(), bps(10.0), 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    // Quote(100.1) à 100.1 (open 100 + 10 bps) : une unité exactement.
    sim.submit(enter(100.1, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    close_to(sim.cash(), 899.54965);
    assert_eq!(sim.qty(), 1.0);
    sim.submit(exit_market(), at(1)).unwrap();
    sim.on_bar(&flat(2, 110.0)).unwrap();
    let out = sim.finish(&flat(2, 110.0)).unwrap();
    assert_eq!(out.trades.len(), 1);
    let t = out.trades[0];
    close_to(t.entry_price, 100.1);
    close_to(t.entry_fee, 0.35035);
    close_to(t.exit_price, 109.89);
    close_to(t.exit_fee, 0.384615);
    close_to(out.cash, 1009.055035);
    close_to(t.pnl_net, 9.055035);
    close_to(t.entry_fee + t.exit_fee, 0.734965);
    close_to(t.entry_slippage + t.exit_slippage, 0.21);
    assert!(!t.capped_by_cash);
    assert_eq!(t.exit_reason, ExitReason::Signal);
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert!(!out.forced_close);
    assert_eq!(out.counters.fills, 2);
    assert_eq!(out.counters.fills_market, 2);
}

#[test]
fn le_fill_est_a_l_open_jamais_au_close() {
    let mut sim = sim_with(rules_s3(), bps(10.0), 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.1, None, None), at(0)).unwrap();
    let candle = bar(1, 100.0, 103.0, 99.0, 102.0);
    sim.on_bar(&candle).unwrap();
    sim.submit(exit_market(), at(1)).unwrap();
    sim.on_bar(&bar(2, 105.0, 106.0, 104.0, 105.5)).unwrap();
    let out = sim.finish(&flat(2, 105.0)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_ref_price, 100.0);
    assert_eq!(t.entry_price, 100.0 * (1.0 + bps(10.0).rate()));
    assert_ne!(t.entry_price, candle.close);
    assert_eq!(t.entry_ms, candle.open_time_ms);
}

#[test]
fn plafond_de_cash() {
    let fees = FeeSchedule::new(ExchangeId::Okx, 0, 0).unwrap();
    let mut sim = Simulator::new(fees, Slippage::ZERO, rules_s3(), inst(), 100.0, 4).unwrap();
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(500.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    close_to(sim.qty(), 0.95);
    close_to(sim.cash(), 5.0);
    assert_eq!(sim.counters().entries_capped_by_cash, 1);
}

// ---------------------------------------------------------------------------------------------
// Délai d'activation
// ---------------------------------------------------------------------------------------------

#[test]
fn achat_market_a_l_open_plus_un_delai() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    // 1m i+1 : l'ordre existe mais n'est pas actif.
    sim.on_bar(&flat(1, 101.0)).unwrap();
    assert_eq!(sim.qty(), 0.0, "jamais de fill à open[i+1]");
    assert_eq!(sim.pending_len(), 1);
    // 1m i+2 : fill à son open.
    sim.on_bar(&flat(2, 102.0)).unwrap();
    assert!(sim.qty() > 0.0);
    assert_eq!(sim.pending_len(), 0);
    let out = sim.finish(&flat(2, 102.0)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_ref_price, 102.0);
    assert_eq!(t.entry_ms, T0 + 2 * 60_000);
    assert_eq!(t.entry_signal_ms, T0 + 60_000);
}

#[test]
fn delai_par_le_temps_avec_trou() {
    // Trou : il n'y a pas de 1m d'index 2. L'ordre signalé à la clôture de 0 est actif à open[2] ; la
    // première 1m présente à ce moment-là est l'index 3.
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 101.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    sim.on_bar(&flat(3, 103.0)).unwrap();
    assert!(sim.qty() > 0.0);
    let out = sim.finish(&flat(3, 103.0)).unwrap();
    assert_eq!(out.trades[0].entry_ref_price, 103.0);
    assert_eq!(out.trades[0].entry_ms, T0 + 3 * 60_000);
}

// ---------------------------------------------------------------------------------------------
// Limits : traversée, gap, expiration
// ---------------------------------------------------------------------------------------------

#[test]
fn limit_achat_traverse_d_un_tick() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 99.0, 99.0, 1, 100), at(0))
        .unwrap();
    // Traversée avant l'activation : sans effet.
    sim.on_bar(&bar(1, 99.5, 99.8, 97.0, 99.0)).unwrap();
    assert_eq!(sim.pending_len(), 1);
    // Toucher exact (low == L) : pas de fill.
    sim.on_bar(&bar(2, 99.5, 99.8, 99.0, 99.4)).unwrap();
    assert_eq!(sim.pending_len(), 1);
    assert_eq!(sim.qty(), 0.0);
    // low == L − tick : ordre au repos, fill à L.
    sim.on_bar(&bar(3, 99.4, 99.5, 98.9, 99.0)).unwrap();
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.qty(), 1.0);
    let out = sim.finish(&flat(3, 99.0)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_price, 99.0);
    assert_eq!(t.entry_ref_price, 99.0);
    assert_eq!(t.entry_liquidity, Liquidity::Maker);
    close_to(t.entry_fee, 99.0 * 0.002);
    assert_eq!(t.entry_slippage, 0.0);
    assert_eq!(out.counters.fills_limit, 1);
}

/// `limit_cross_ticks = 0` : toucher exact du niveau, sans tolérance sous le low (F2 de la revue S6).
/// Avant la correction, le seuil valait `-1e-6` tick et une limit était remplie à `L` alors que le low
/// la dépassait de 5e-9, hors de `[low, high]`.
#[test]
fn limit_cross_ticks_zero_n_accepte_pas_un_low_hors_de_portee() {
    let rules = SimRules {
        limit_cross_ticks: 0,
        ..SimRules::default()
    };
    // Achat : low juste au-dessus de la limite, hors grille de tick.
    let mut sim = sim_with(rules, Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 99.0, 99.0, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&bar(2, 99.5, 99.8, 99.000000005, 99.4)).unwrap();
    assert_eq!(sim.pending_len(), 1);
    assert_eq!(sim.qty(), 0.0);
    // Toucher exact : rempli à L.
    sim.on_bar(&bar(3, 99.5, 99.8, 99.0, 99.4)).unwrap();
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.qty(), 1.0);
    let out = sim.finish(&flat(3, 99.4)).unwrap();
    assert_eq!(out.trades[0].entry_price, 99.0);

    // Vente : high juste sous la limite.
    let mut sim = sim_with(rules, Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim.submit(place(Side::Sell, 102.0, 102.0, 1, 100), at(2))
        .unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar(4, 100.0, 101.999999995, 99.9, 100.5))
        .unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim.on_bar(&bar(5, 100.0, 102.0, 99.9, 100.5)).unwrap();
    assert_eq!(sim.qty(), 0.0);
}

/// Limit d'achat à 99 posée à la clôture de la 1m 0, active à l'open de la 1m 2.
fn achat_a_99(slippage: Slippage, cash: f64) -> Simulator {
    let mut sim = sim_with(SimRules::default(), slippage, cash);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 99.0, 99.0, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim
}

/// Limits pessimistes : un achat au repos (P_arr = 100 >= L) qu'un gap traverse est rempli à `L`
/// exactement, en maker, même si l'open de la 1m est déjà sous `L` et que le fill sort de `[low, high]`.
#[test]
fn limit_achat_au_repos_traverse_par_un_gap_rempli_a_la_limite_en_maker() {
    let mut sim = achat_a_99(bps(10.0), 1000.0);
    // 1m d'activation : open 100, l'ordre n'est pas marketable.
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    // Gap : toute la 1m est sous la limite.
    sim.on_bar(&bar(3, 98.0, 98.5, 97.5, 98.2)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    assert_eq!(sim.counters().fills_limit, 1);
    let out = sim.finish(&flat(3, 98.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_price, 99.0);
    assert_eq!(t.entry_ref_price, 99.0);
    assert_eq!(t.entry_liquidity, Liquidity::Maker);
    assert_eq!(t.entry_slippage, 0.0);
    close_to(t.entry_fee, 99.0 * 0.002);
}

/// Limits pessimistes : un achat marketable à l'arrivée (open d'activation 98 < L = 99) prend la
/// liquidité, à `open × (1 + s)`, en taker.
#[test]
fn limit_achat_marketable_a_l_arrivee_rempli_a_l_open_en_taker() {
    let mut sim = achat_a_99(bps(10.0), 1000.0);
    sim.on_bar(&bar(2, 98.0, 98.5, 97.5, 98.2)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    assert_eq!(sim.counters().fills_limit, 1);
    assert_eq!(sim.counters().fills_market, 0);
    let out = sim.finish(&flat(2, 98.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_price, 98.0 * (1.0 + 10.0 / 10_000.0));
    assert_eq!(t.entry_ref_price, 98.0);
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    close_to(t.entry_slippage, 98.0 * 0.001);
    close_to(t.entry_fee, t.entry_price * 0.0035);
}

/// Sans slippage, un achat marketable est pris à l'open (gap sous la limite), en taker.
#[test]
fn limit_achat_marketable_sans_slippage_a_l_open() {
    let mut sim = achat_a_99(Slippage::ZERO, 1000.0);
    sim.on_bar(&bar(2, 98.0, 98.5, 97.5, 98.2)).unwrap();
    let out = sim.finish(&flat(2, 98.2)).unwrap();
    assert_eq!(out.trades[0].entry_price, 98.0);
    assert_eq!(out.trades[0].entry_liquidity, Liquidity::Taker);
    assert_eq!(out.trades[0].entry_slippage, 0.0);
}

/// Le prix d'un achat marketable est plafonné par `L` quand `open × (1 + s) > L`.
#[test]
fn limit_achat_marketable_plafonne_par_la_limite() {
    let mut sim = sim_with(SimRules::default(), bps(20.0), 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 98.1, 98.1, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    // 98.0 × 1,002 = 98.196 > 98.1 : plafonné à la limite, toujours taker.
    sim.on_bar(&bar(2, 98.0, 98.5, 97.5, 98.2)).unwrap();
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    let out = sim.finish(&flat(2, 98.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_price, 98.1);
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    close_to(t.entry_slippage, 0.1);
    close_to(t.entry_fee, 98.1 * 0.0035);
}

/// P_arr est le close de la dernière 1m antérieure quand la minute d'activation manque (la 1m 2 est
/// absente de la série).
#[test]
fn prix_d_arrivee_pris_sur_le_close_precedent_quand_la_minute_manque() {
    // Close de la 1m 1 à 100 >= L : au repos. La 1m 3 ouvre à 98 (sous L) mais l'ordre n'est pas
    // marketable : rempli à L en maker.
    let mut sim = achat_a_99(Slippage::ZERO, 1000.0);
    sim.on_bar(&bar(3, 98.0, 98.5, 97.5, 98.2)).unwrap();
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    let out = sim.finish(&flat(3, 98.2)).unwrap();
    assert_eq!(out.trades[0].entry_price, 99.0);
    assert_eq!(out.trades[0].entry_liquidity, Liquidity::Maker);

    // Close de la 1m 1 à 98.5 < L : marketable. La 1m 3 ouvre à 98 : taker à l'open.
    let mut sim = sim_with(SimRules::default(), Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 99.0, 99.0, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&bar(1, 100.0, 100.0, 98.5, 98.5)).unwrap();
    sim.on_bar(&bar(3, 98.0, 98.5, 97.5, 98.2)).unwrap();
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    let out = sim.finish(&flat(3, 98.2)).unwrap();
    assert_eq!(out.trades[0].entry_price, 98.0);
    assert_eq!(out.trades[0].entry_liquidity, Liquidity::Taker);
}

/// Marketable à l'arrivée, mais à la première 1m à volume l'open ne franchit plus `L` : l'ordre passe
/// au repos pour de bon (rempli à `L` en maker, jamais en taker à une 1m ultérieure).
#[test]
fn limit_marketable_dont_l_open_ne_franchit_plus_devient_au_repos() {
    let mut sim = achat_a_99(bps(10.0), 1000.0);
    // 1m d'activation à volume nul : open 98 < L, donc marketable à l'arrivée, mais rien n'est exécuté.
    sim.on_bar(&bar_v(2, 98.0, 98.0, 98.0, 98.0, 0.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.pending_len(), 1);
    // Première 1m à volume : open 100 >= L, le plus bas ne traverse pas : au repos.
    sim.on_bar(&bar(3, 100.0, 100.5, 99.5, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.pending_len(), 1);
    // Gap sous la limite ensuite : rempli à L en maker, pas à l'open en taker.
    sim.on_bar(&bar(4, 98.5, 98.8, 98.0, 98.4)).unwrap();
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    let out = sim.finish(&flat(4, 98.4)).unwrap();
    assert_eq!(out.trades[0].entry_price, 99.0);
    assert_eq!(out.trades[0].entry_liquidity, Liquidity::Maker);
}

/// Marketable à l'arrivée, première 1m à volume après une 1m à volume nul : l'open la franchit encore,
/// taker à l'open de cette 1m.
#[test]
fn limit_marketable_apres_une_minute_a_volume_nul_rempli_en_taker() {
    let mut sim = achat_a_99(Slippage::ZERO, 1000.0);
    sim.on_bar(&bar_v(2, 98.0, 98.0, 98.0, 98.0, 0.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    sim.on_bar(&bar(3, 97.0, 97.5, 96.5, 97.2)).unwrap();
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    let out = sim.finish(&flat(3, 97.2)).unwrap();
    assert_eq!(out.trades[0].entry_price, 97.0);
    assert_eq!(out.trades[0].entry_liquidity, Liquidity::Taker);
}

/// Un achat marketable est soumis à la règle de volume comme un achat market.
#[test]
fn limit_achat_marketable_soumis_a_la_regle_de_volume() {
    let mut sim = achat_a_99(Slippage::ZERO, 1000.0);
    // Volume 1 : 0,5 % de 1 × 97,5 = 0,4875 < 98.
    sim.on_bar(&bar_v(2, 98.0, 98.5, 97.5, 98.2, 1.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().buys_rejected_volume_share, 1);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    assert_eq!(sim.pending_len(), 0);
}

/// La réservation couvre les frais maker : un fill taker que le cash libre ne couvre pas est abandonné
/// et compté comme un achat market sans cash.
#[test]
fn limit_achat_marketable_sans_cash_pour_les_frais_taker_est_abandonne() {
    // 99 × 1,002 = 99.198 réservés sur 99.2 de cash ; un fill à 98.9 coûte 98.9 × 1,0035 = 99.246.
    let mut sim = achat_a_99(Slippage::ZERO, 99.2);
    sim.on_bar(&bar(2, 98.9, 99.0, 98.5, 98.9)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().buys_rejected_insufficient_cash, 1);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    assert_eq!(sim.cash(), 99.2);
    assert_eq!(sim.pending_len(), 0);
    // Un open plus bas laisse assez de cash : 98.0 × 1,0035 = 98.343.
    let mut sim = achat_a_99(Slippage::ZERO, 99.2);
    sim.on_bar(&bar(2, 98.0, 98.5, 97.5, 98.2)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
}

/// Position d'une unité achetée au marché à l'open de la 1m 2, sans slippage.
fn long_one() -> Simulator {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim
}

#[test]
fn limit_vente_symetrique() {
    // Au repos : toucher exact sans fill, puis traversée d'un tick : fill à L, maker.
    let mut sim = long_one();
    sim.submit(
        Instruction::Exit {
            price: OrderPrice::Limit {
                price: 102.0,
                ttl_bars: 50,
            },
            tag: "sortie",
        },
        at(2),
    )
    .unwrap();
    // Active à open[4] ; la 1m 3 ne la voit pas.
    sim.on_bar(&bar(3, 100.0, 105.0, 100.0, 104.0)).unwrap();
    assert_eq!(sim.pending_len(), 1);
    sim.on_bar(&bar(4, 101.0, 102.0, 100.5, 101.5)).unwrap();
    assert_eq!(sim.pending_len(), 1, "toucher exact : pas de fill");
    sim.on_bar(&bar(5, 101.5, 102.1, 101.0, 101.8)).unwrap();
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(5, 101.8)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_price, 102.0);
    assert_eq!(t.exit_liquidity, Liquidity::Maker);
    assert_eq!(t.exit_reason, ExitReason::Signal);
    close_to(t.exit_fee, 102.0 * 0.002);
}

/// Position d'une unité, comme [`long_one`], avec le slippage donné (entrée market à 100 × (1 + s)).
fn long_one_with(slippage: Slippage) -> Simulator {
    let mut sim = sim_with(SimRules::default(), slippage, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(slippage.buy_price(100.0), None, None), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim
}

fn sortie_limit(sim: &mut Simulator, price: f64) {
    sim.submit(
        Instruction::Exit {
            price: OrderPrice::Limit {
                price,
                ttl_bars: 50,
            },
            tag: "sortie",
        },
        at(2),
    )
    .unwrap();
}

/// Vente au repos (P_arr = 100 <= L = 102) qu'un gap traverse : remplie à `L`, maker.
#[test]
fn limit_vente_au_repos_traversee_par_un_gap_remplie_a_la_limite_en_maker() {
    let mut sim = long_one();
    sortie_limit(&mut sim, 102.0);
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&flat(4, 100.0)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim.on_bar(&bar(5, 103.0, 103.5, 102.6, 103.2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    let out = sim.finish(&flat(5, 103.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_price, 102.0);
    assert_eq!(t.exit_ref_price, 102.0);
    assert_eq!(t.exit_liquidity, Liquidity::Maker);
    assert_eq!(t.exit_slippage, 0.0);
    close_to(t.exit_fee, 102.0 * 0.002);
}

/// Vente marketable à l'arrivée (open d'activation 103 > L = 102) : taker à `open × (1 − s)`.
#[test]
fn limit_vente_marketable_a_l_arrivee_remplie_a_l_open_en_taker() {
    let mut sim = long_one_with(bps(10.0));
    sortie_limit(&mut sim, 102.0);
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar(4, 103.0, 103.5, 102.6, 103.2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    assert_eq!(sim.counters().fills_limit, 1);
    let out = sim.finish(&flat(4, 103.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_price, 103.0 * (1.0 - 10.0 / 10_000.0));
    assert_eq!(t.exit_ref_price, 103.0);
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert_eq!(t.exit_reason, ExitReason::Signal);
    close_to(t.exit_slippage, 103.0 * 0.001);
    close_to(t.exit_fee, t.exit_price * 0.0035);
}

/// Le prix d'une vente marketable est plafonné par `L` (plancher du prix reçu) quand `open × (1 − s) < L`.
#[test]
fn limit_vente_marketable_plafonnee_par_la_limite() {
    let mut sim = long_one_with(bps(20.0));
    // 103.0 × 0,998 = 102.794 < 102.9 : la limite est le prix.
    sortie_limit(&mut sim, 102.9);
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar(4, 103.0, 103.5, 102.6, 103.2)).unwrap();
    let out = sim.finish(&flat(4, 103.2)).unwrap();
    assert_eq!(out.trades[0].exit_price, 102.9);
    assert_eq!(out.trades[0].exit_liquidity, Liquidity::Taker);
    close_to(out.trades[0].exit_slippage, 0.1);
}

/// Vente marketable (P_arr = 103 > L) dont l'open ne franchit plus `L` à la première 1m à volume :
/// elle passe au repos et se remplit à `L` en maker.
#[test]
fn limit_vente_marketable_dont_l_open_ne_franchit_plus_devient_au_repos() {
    let mut sim = long_one();
    sortie_limit(&mut sim, 102.0);
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar_v(4, 103.0, 103.0, 103.0, 103.0, 0.0))
        .unwrap();
    assert_eq!(sim.qty(), 1.0);
    // Open 101 <= L : au repos ; le plus haut ne la traverse pas.
    sim.on_bar(&bar(5, 101.0, 101.5, 100.5, 101.0)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    // Gap au-dessus ensuite : rempli à L en maker, pas à l'open.
    sim.on_bar(&bar(6, 104.0, 104.5, 103.5, 104.2)).unwrap();
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    let out = sim.finish(&flat(6, 104.2)).unwrap();
    assert_eq!(out.trades[0].exit_price, 102.0);
    assert_eq!(out.trades[0].exit_liquidity, Liquidity::Maker);
}

/// Take-profit au repos (P_arr = open de la 1m qui suit le fill = 101 <= 102) qu'un gap traverse :
/// rempli au TP, maker.
#[test]
fn tp_au_repos_traverse_par_un_gap_rempli_au_tp_en_maker() {
    let mut sim = entree_avec(SimRules::default(), bps(5.0), 95.0, Some(102.0));
    sim.on_bar(&bar(2, 100.0, 100.5, 99.5, 100.2)).unwrap();
    sim.on_bar(&bar(3, 101.0, 101.5, 100.8, 101.2)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim.on_bar(&bar(4, 103.0, 103.5, 102.6, 103.2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().take_profits_triggered, 1);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    let out = sim.finish(&flat(4, 103.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    assert_eq!(t.exit_price, 102.0);
    assert_eq!(t.exit_ref_price, 102.0);
    assert_eq!(t.exit_liquidity, Liquidity::Maker);
    assert_eq!(t.exit_slippage, 0.0);
}

/// Take-profit marketable à l'arrivée (open de la 1m qui suit le fill = 103 > 102) : taker à
/// `open × (1 − s)`.
#[test]
fn tp_marketable_a_l_arrivee_rempli_a_l_open_en_taker() {
    let mut sim = entree_avec(SimRules::default(), bps(5.0), 95.0, Some(102.0));
    sim.on_bar(&bar(2, 100.0, 100.5, 99.5, 100.2)).unwrap();
    sim.on_bar(&bar(3, 103.0, 103.5, 102.6, 103.2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().take_profits_triggered, 1);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    assert_eq!(
        sim.counters().fills_limit,
        0,
        "le TP n'est pas dans fills_limit"
    );
    let out = sim.finish(&flat(3, 103.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    assert_eq!(t.exit_price, 103.0 * (1.0 - 5.0 / 10_000.0));
    assert_eq!(t.exit_ref_price, 103.0);
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    close_to(t.exit_slippage, 103.0 * 0.0005);
}

/// Take-profit marketable à l'arrivée mais l'open de la première 1m à volume ne le franchit plus : au
/// repos, maker à `tp`.
#[test]
fn tp_marketable_dont_l_open_ne_franchit_plus_devient_au_repos() {
    let mut sim = entree_avec(SimRules::default(), bps(5.0), 95.0, Some(102.0));
    sim.on_bar(&bar(2, 100.0, 100.5, 99.5, 100.2)).unwrap();
    sim.on_bar(&bar_v(3, 103.0, 103.0, 103.0, 103.0, 0.0))
        .unwrap();
    assert_eq!(sim.qty(), 1.0);
    // Open 101, le plus haut franchit le TP : maker à 102, pas taker.
    sim.on_bar(&bar(4, 101.0, 102.5, 100.5, 102.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    let out = sim.finish(&flat(4, 102.0)).unwrap();
    assert_eq!(out.trades[0].exit_price, 102.0);
    assert_eq!(out.trades[0].exit_liquidity, Liquidity::Maker);
}

/// Le take-profit passe au repos dès la 1m où l'open ne franchit plus son niveau, même quand le touché
/// de cette 1m est ignoré parce qu'un achat y est rempli (la règle 3 « évalue déjà j » sans exception) :
/// le touché ignoré est compté, puis la sortie de la 1m suivante est maker à `tp`, pas taker.
#[test]
fn tp_marketable_demote_au_repos_dans_une_1m_ou_son_touche_est_ignore() {
    let mut sim = entree_avec(SimRules::default(), bps(5.0), 90.0, Some(102.0));
    sim.on_bar(&bar(2, 100.0, 100.5, 99.5, 100.2)).unwrap();
    // Limit d'achat à 99, active à open[4].
    sim.submit(place(Side::Buy, 99.0, 99.0, 1, 50), at(2))
        .unwrap();
    // Le TP arrive à open[3] = 103 > 102 : marketable (volume 0, aucun fill).
    sim.on_bar(&bar_v(3, 103.0, 103.0, 103.0, 103.0, 0.0))
        .unwrap();
    // open[4] = 101 ne franchit plus le TP : au repos. L'achat à 99 est rempli (low 98.5), donc le
    // touché du TP (high 102.5) est ignoré et compté.
    sim.on_bar(&bar(4, 101.0, 102.5, 98.5, 101.5)).unwrap();
    assert_eq!(sim.counters().take_profits_ignored_fill_bar, 1);
    assert_eq!(sim.counters().fills_limit, 1);
    assert!(sim.qty() > 1.0);
    // open[5] = 103 > 102 : TP au repos touché, maker à 102 (gap), jamais taker.
    sim.on_bar(&bar(5, 103.0, 103.5, 102.6, 103.2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    let out = sim.finish(&flat(5, 103.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    assert_eq!(t.exit_price, 102.0);
    assert_eq!(t.exit_liquidity, Liquidity::Maker);
}

/// Avec `tp_ignored_on_fill_bar = false`, le take-profit arrive à l'open de la 1m de fill de l'entrée
/// (écart 11, lecture retenue) : marketable si cet open est au-dessus du TP, il sort alors en taker à
/// `max(open × (1 − s), tp)` et non en maker à `tp` (un prix sous le low de la 1m).
#[test]
fn tp_sans_ignore_arrive_a_l_open_de_la_1m_de_fill() {
    let rules = SimRules {
        tp_ignored_on_fill_bar: false,
        ..SimRules::default()
    };
    let mut sim = entree_avec(rules, bps(5.0), 90.0, Some(102.0));
    // Entrée à l'open 103 (+ slippage) ; le TP à 102 est déjà sous le marché à son arrivée.
    sim.on_bar(&bar(2, 103.0, 103.5, 102.6, 103.2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    assert_eq!(sim.counters().take_profits_ignored_fill_bar, 0);
    let out = sim.finish(&flat(2, 103.2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    assert_eq!(t.exit_ref_price, 103.0);
    assert_eq!(t.exit_price, 103.0 * (1.0 - 5.0 / 10_000.0));
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert_eq!(t.exit_ms, t.entry_ms, "sortie dans la 1m de fill");
}

/// Avec `tp_ignored_on_fill_bar = false`, une entrée limit AU REPOS remplie en cours de 1m (maker) pose son
/// take-profit après le fill : il n'est jamais vendu à l'open de cette 1m (avant l'achat, prix optimiste),
/// son touché y est ignoré et compté, et il arrive à la 1m suivante (lecture pessimiste de l'écart 11).
#[test]
fn tp_sans_ignore_entree_limit_au_repos_arrive_a_la_1m_suivante() {
    let rules = SimRules {
        tp_ignored_on_fill_bar: false,
        ..SimRules::default()
    };
    let mut sim = sim_with(rules, bps(10.0), 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(
        Instruction::Enter {
            size: SizeSpec::Quote(500.0),
            price: OrderPrice::Limit {
                price: 98.0,
                ttl_bars: 10,
            },
            stop: Some(90.0),
            take_profit: Some(99.5),
            tag: "entree",
        },
        at(0),
    )
    .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    // 1m 2 : P_arr = 100 > 98, au repos ; low 97,5 traverse L : fill maker à 98 exactement, en cours de 1m.
    // Le TP (99,5) est sous l'open de cette 1m mais n'existait pas à l'open : ni taker à l'open, ni touché.
    sim.on_bar(&bar(2, 100.0, 100.2, 97.5, 99.0)).unwrap();
    assert!(
        sim.qty() > 0.0,
        "position ouverte, TP pas pris dans la 1m de fill"
    );
    assert_eq!(sim.counters().fills_limit, 1);
    assert_eq!(sim.counters().take_profits_ignored_fill_bar, 1);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 0);
    // 1m 3 : le TP arrive, P_arr = open = 100,1 > 99,5 : marketable, open > TP : taker.
    sim.on_bar(&bar(3, 100.1, 100.3, 99.8, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    let out = sim.finish(&flat(3, 100.0)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_price, 98.0);
    assert_eq!(t.entry_liquidity, Liquidity::Maker);
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    assert_eq!(t.exit_ref_price, 100.1);
    assert_eq!(t.exit_price, 100.1 * (1.0 - 10.0 / 10_000.0));
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert_eq!(t.exit_ms, T0 + 3 * 60_000);
}

/// Contrôle : une entrée limit marketable à l'arrivée (taker à l'open) garde un TP qui arrive à l'open de la
/// 1m de fill quand `tp_ignored_on_fill_bar` est faux, comme une entrée au marché.
#[test]
fn tp_sans_ignore_entree_limit_taker_arrive_a_l_open_de_la_1m_de_fill() {
    let rules = SimRules {
        tp_ignored_on_fill_bar: false,
        ..SimRules::default()
    };
    let mut sim = sim_with(rules, bps(10.0), 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(
        Instruction::Enter {
            size: SizeSpec::Quote(500.0),
            price: OrderPrice::Limit {
                price: 100.0,
                ttl_bars: 10,
            },
            stop: Some(90.0),
            take_profit: Some(99.0),
            tag: "entree",
        },
        at(0),
    )
    .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    // 1m 2 : open 99,5, P_arr < L = 100 : marketable, open < L : taker à min(99,5 x 1,001, 100) ; le TP à 99
    // est sous l'open : il sort dans la même 1m en taker.
    sim.on_bar(&bar(2, 99.5, 99.8, 98.5, 99.2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().take_profits_ignored_fill_bar, 0);
    assert_eq!(sim.counters().limits_marketable_at_arrival, 2);
}

/// Le compteur additionne les fills taker de limits : achat, vente.
#[test]
fn compteur_limits_marketables_a_l_arrivee() {
    let mut sim = achat_a_99(Slippage::ZERO, 1000.0);
    sim.on_bar(&bar(2, 98.0, 98.5, 97.5, 98.2)).unwrap();
    assert_eq!(sim.counters().limits_marketable_at_arrival, 1);
    // Vente limit à 97, active à open[4] ; open[4] = 99 > 97 : taker.
    sim.submit(
        Instruction::Exit {
            price: OrderPrice::Limit {
                price: 97.0,
                ttl_bars: 50,
            },
            tag: "sortie",
        },
        at(2),
    )
    .unwrap();
    sim.on_bar(&flat(3, 98.0)).unwrap();
    sim.on_bar(&bar(4, 99.0, 99.5, 98.5, 99.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(4, 99.0)).unwrap();
    assert_eq!(out.counters.limits_marketable_at_arrival, 2);
    assert_eq!(out.counters.fills_limit, 2);
    assert_eq!(out.trades[0].exit_price, 99.0);
    assert_eq!(out.trades[0].exit_liquidity, Liquidity::Taker);
}

#[test]
fn ttl_compte_en_1m() {
    // Placé à la clôture de 0, actif à open[2], TTL 3 : remplissable dans les 1m 2, 3 et 4 ; retiré à
    // l'étape 0 de la 1m 5.
    let traverse = |index: i64| {
        let mut sim = sim_defaults(1000.0);
        sim.on_bar(&flat(0, 100.0)).unwrap();
        sim.submit(place(Side::Buy, 98.0, 98.0, 1, 3), at(0))
            .unwrap();
        for i in 1..=5 {
            let c = if i == index {
                bar(i, 98.5, 98.6, 97.0, 97.5)
            } else {
                bar(i, 99.0, 99.5, 98.5, 99.0)
            };
            sim.on_bar(&c).unwrap();
        }
        sim
    };
    for index in [2, 3, 4] {
        let sim = traverse(index);
        assert_eq!(sim.qty(), 1.0, "fill en {index}");
        assert_eq!(sim.counters().limits_expired, 0);
    }
    let sim = traverse(5);
    assert_eq!(sim.qty(), 0.0, "expiré avant la 1m 5");
    assert_eq!(sim.counters().limits_expired, 1);
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.quote_free(), 1000.0, "la réservation est libérée");
    // Une traversée avant l'activation (1m 1) ne remplit pas non plus.
    let sim = traverse(1);
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().limits_expired, 1);
}

// ---------------------------------------------------------------------------------------------
// Réservations, slots, file
// ---------------------------------------------------------------------------------------------

#[test]
fn reservation_quote_et_refus() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 90.0, 600.0, 1, 100), at(0))
        .unwrap();
    // Environ 600 de notional et 1,2 de frais maker estimés.
    let libre = sim.quote_free();
    assert!((libre - 398.8).abs() < 1e-4, "{libre}");
    assert_eq!(
        sim.submit(place(Side::Buy, 89.0, 500.0, 2, 100), at(0)),
        Err(Rejection::InsufficientQuote)
    );
    assert_eq!(sim.quote_free(), libre, "un refus ne réserve rien");
    // 300 tient dans le cash libre (398,8).
    sim.submit(place(Side::Buy, 88.0, 300.0, 2, 100), at(0))
        .unwrap();
    assert_eq!(sim.pending_len(), 2);
    assert!(sim.quote_free() < 100.0);
    assert!(sim.quote_free() > 0.0);
}

#[test]
fn reservation_quantite() {
    let mut sim = long_one();
    sim.submit(place(Side::Sell, 105.0, 1e6, 1, 100), at(2))
        .unwrap();
    assert_eq!(sim.qty_free(), 0.0, "toute la quantité est réservée");
    assert_eq!(sim.qty(), 1.0);
    assert_eq!(
        sim.submit(place(Side::Sell, 106.0, 10.0, 2, 100), at(2)),
        Err(Rejection::SellWithoutFreeQty)
    );
    assert_eq!(
        sim.submit(
            Instruction::Exit {
                price: OrderPrice::Limit {
                    price: 106.0,
                    ttl_bars: 5
                },
                tag: "x"
            },
            at(2)
        ),
        Err(Rejection::SellWithoutFreeQty)
    );
    // Une vente partielle laisse du libre.
    let mut sim = long_one();
    sim.submit(place(Side::Sell, 105.0, 42.0, 1, 100), at(2))
        .unwrap();
    close_to(sim.qty_free(), 1.0 - 0.4);
    let vue: Vec<PendingView> = sim.pending().collect();
    assert_eq!(vue.len(), 1);
    assert_eq!(vue[0].side, Side::Sell);
    assert_eq!(vue[0].limit, Some((105.0, 100)));
    close_to(vue[0].qty, 0.4);
    assert_eq!(vue[0].slot, 1);
}

#[test]
fn slot_occupe_jusqu_a_l_annulation_effective() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.submit(Instruction::CancelSlot { slot: 1 }, at(1))
        .unwrap();
    // L'annulation n'est pas effective : le slot reste occupé.
    assert_eq!(
        sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(1)),
        Err(Rejection::SlotAlreadyOpen)
    );
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(
        sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(2)),
        Err(Rejection::SlotAlreadyOpen)
    );
    // Annulation effective à open[3] : l'ordre disparaît et libère sa réservation.
    sim.on_bar(&flat(3, 100.0)).unwrap();
    assert_eq!(sim.counters().orders_cancelled, 1);
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.quote_free(), 1000.0);
    sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(3))
        .unwrap();
    assert_eq!(sim.pending_len(), 1);
}

#[test]
fn annulation_totale_retardee() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(0))
        .unwrap();
    sim.submit(place(Side::Buy, 89.0, 50.0, 2, 100), at(0))
        .unwrap();
    sim.submit(Instruction::CancelAll, at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.pending_len(), 2);
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.counters().orders_cancelled, 2);
    // CancelAll sans ordre : accepté, sans effet.
    sim.submit(Instruction::CancelAll, at(2)).unwrap();
}

#[test]
fn file_pleine() {
    let mut sim = sim_defaults(100_000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    for slot in 1..=64 {
        sim.submit(place(Side::Buy, 50.0, 10.0, slot, 100), at(0))
            .unwrap();
    }
    assert_eq!(sim.pending_len(), 64);
    assert_eq!(
        sim.submit(place(Side::Buy, 50.0, 10.0, 65, 100), at(0)),
        Err(Rejection::PendingFull)
    );
    assert_eq!(
        sim.submit(enter(10.0, None, None), at(0)),
        Err(Rejection::PendingFull)
    );
}

#[test]
fn la_capacite_de_la_file_survit_aux_fills() {
    let mut sim = sim_defaults(1000.0);
    let capacite = sim.pending.capacity();
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(sim.pending.capacity(), capacite);
}

// ---------------------------------------------------------------------------------------------
// Lots FIFO
// ---------------------------------------------------------------------------------------------

/// Position agrégée de deux lots : un `Enter` market (stop 90, TP 120 par défaut) et un `Place` d'achat
/// à 99, tous deux remplis à la 1m 2.
fn position_de_deux_lots(tp: Option<f64>) -> Simulator {
    let mut sim = sim_with(SimRules::default(), bps(10.0), 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, Some(90.0), tp), at(0)).unwrap();
    sim.submit(place(Side::Buy, 99.0, 99.0, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&bar(2, 100.0, 100.5, 98.5, 99.5)).unwrap();
    let pos = sim.position().unwrap();
    assert!(
        (pos.qty - 2.0).abs() < 0.01,
        "deux lots d'environ une unité"
    );
    assert!(
        pos.avg_price > 99.0 && pos.avg_price < 100.2,
        "{}",
        pos.avg_price
    );
    assert_eq!(pos.stop, Some(90.0));
    sim
}

/// F8 de la revue S6 : `MoveStop` et le take-profit valent pour la position agrégée, lots `Enter` et
/// `Place` confondus.
#[test]
fn stop_et_tp_sur_la_position_agregee() {
    // MoveStop effectif à open[4] : un seul stop ferme les deux lots, au pire de (98, open).
    let mut sim = position_de_deux_lots(Some(120.0));
    sim.submit(Instruction::MoveStop { stop: 98.0 }, at(2))
        .unwrap();
    sim.on_bar(&flat(3, 99.5)).unwrap();
    sim.on_bar(&bar(4, 99.5, 99.8, 97.0, 97.5)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(4, 97.5)).unwrap();
    assert_eq!(out.counters.stops_triggered, 1);
    assert_eq!(out.counters.move_stops_applied, 1);
    assert_eq!(out.trades.len(), 2);
    for (t, lot) in out.trades.iter().zip(0u64..) {
        assert_eq!(t.lot_id, lot);
        assert_eq!(t.exit_reason, ExitReason::Stop);
        assert_eq!(t.exit_ref_price, 98.0);
        close_to(t.exit_price, 98.0 * (1.0 - 0.001));
        assert_eq!(t.stop_at_exit, Some(98.0));
    }

    // Take-profit : un seul déclenchement ferme aussi les deux lots, au prix du TP, en maker.
    let mut sim = position_de_deux_lots(Some(105.0));
    sim.on_bar(&flat(3, 99.5)).unwrap();
    sim.on_bar(&bar(4, 100.0, 106.0, 99.9, 105.5)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(4, 105.5)).unwrap();
    assert_eq!(out.counters.stops_triggered, 0);
    assert_eq!(out.trades.len(), 2);
    for t in &out.trades {
        assert_eq!(t.exit_reason, ExitReason::TakeProfit);
        assert_eq!(t.exit_ref_price, 105.0);
        assert_eq!(t.exit_price, 105.0);
        assert_eq!(t.exit_liquidity, Liquidity::Maker);
    }
}

#[test]
fn fifo_par_lot() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&bar(0, 100.0, 100.5, 99.5, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 100.0, 100.0, 1, 100), at(0))
        .unwrap();
    sim.submit(place(Side::Buy, 99.5, 99.5, 2, 100), at(0))
        .unwrap();
    sim.on_bar(&bar(1, 100.0, 100.2, 99.8, 100.0)).unwrap();
    // Les deux limits sont franchies à la 1m 2, dans l'ordre de la file.
    sim.on_bar(&bar(2, 100.0, 100.2, 99.0, 99.5)).unwrap();
    assert_eq!(sim.qty(), 2.0);
    let pos = sim.position().unwrap();
    close_to(pos.avg_price, 99.75);
    assert_eq!(pos.opened_at_ms, T0 + 2 * 60_000);
    sim.submit(place(Side::Sell, 103.0, 154.5, 3, 100), at(2))
        .unwrap();
    sim.on_bar(&bar(3, 99.5, 100.0, 99.4, 99.8)).unwrap();
    sim.on_bar(&bar(4, 101.0, 102.0, 100.9, 101.5)).unwrap();
    sim.on_bar(&bar(5, 101.5, 103.5, 101.4, 103.0)).unwrap();
    // 1,5 vendu : le lot 0 en entier, la moitié du lot 1.
    close_to(sim.qty(), 0.5);
    let out = sim.finish(&flat(5, 103.0)).unwrap();
    assert_eq!(out.trades.len(), 3);
    let (a, b, c) = (out.trades[0], out.trades[1], out.trades[2]);
    assert_eq!((a.lot_id, b.lot_id, c.lot_id), (0, 1, 1));
    assert_eq!(a.qty, 1.0);
    close_to(b.qty, 0.5);
    close_to(c.qty, 0.5);
    assert_eq!(a.exit_reason, ExitReason::LimitSell);
    assert_eq!(c.exit_reason, ExitReason::ForcedClose);
    assert!(c.forced_close && !a.forced_close);
    // Frais d'entrée au prorata du lot : tout le lot 0, la moitié du lot 1 pour chacune de ses parts.
    close_to(a.entry_fee, 100.0 * 0.002);
    close_to(b.entry_fee, 99.5 * 0.002 * 0.5);
    close_to(c.entry_fee, 99.5 * 0.002 * 0.5);
    // Frais de sortie de la vente : 154,5 × 0,2 % répartis 1 / 1,5 et 0,5 / 1,5.
    close_to(a.exit_fee, 154.5 * 0.002 * (1.0 / 1.5));
    close_to(b.exit_fee, 154.5 * 0.002 * (0.5 / 1.5));
    close_to(a.pnl_net, (103.0 - 100.0) - 100.0 * 0.002 - 103.0 * 0.002);
    // Les lots se vendent dans l'ordre d'entrée.
    assert!(a.entry_ms <= b.entry_ms);
    assert_eq!(a.entry_ref_price, 100.0);
    assert_eq!(b.entry_ref_price, 99.5);
}

// ---------------------------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------------------------

#[test]
fn regle_de_volume_achat_rejete_vente_executee() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    // Volume 10 à 100 : 0,5 % de 1000 = 5. Un achat de 100 est rejeté.
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&bar_v(2, 100.0, 100.0, 100.0, 100.0, 10.0))
        .unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().buys_rejected_volume_share, 1);
    assert_eq!(sim.cash(), 1000.0);
    // Un achat de 4 passe.
    sim.submit(enter(4.0, None, None), at(2)).unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar_v(4, 100.0, 100.0, 100.0, 100.0, 10.0))
        .unwrap();
    assert!(sim.qty() > 0.0);
    // Une sortie au-delà de la part est exécutée et comptée.
    let qty = sim.qty();
    sim.submit(
        Instruction::Exit {
            price: OrderPrice::Market,
            tag: "s",
        },
        at(4),
    )
    .unwrap();
    sim.on_bar(&flat(5, 100.0)).unwrap();
    sim.on_bar(&bar_v(6, 100.0, 100.0, 100.0, 100.0, 0.001))
        .unwrap();
    assert_eq!(sim.qty(), 0.0, "{qty}");
    assert_eq!(sim.counters().exits_over_volume_share, 1);
}

/// Le volume quote d'une 1m vaut `volume × low` (écart 5, le choix le plus pessimiste), pas
/// `volume × close` ni `volume × high` : aucun test sur la fixture ne distingue ces trois définitions,
/// les rejets y étant loin du seuil.
#[test]
fn volume_quote_vaut_volume_fois_low() {
    // Volume 10, low 50 : 0,5 % de 500 = 2,5 (avec le close, 0,5 % de 1000 vaudrait 5).
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(4.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&bar_v(2, 100.0, 110.0, 50.0, 100.0, 10.0))
        .unwrap();
    assert_eq!(sim.qty(), 0.0, "4 > 2,5 : rejeté");
    assert_eq!(sim.counters().buys_rejected_volume_share, 1);
    // Un achat de 2 tient sous 2,5.
    sim.submit(enter(2.0, None, None), at(2)).unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar_v(4, 100.0, 110.0, 50.0, 100.0, 10.0))
        .unwrap();
    assert!(sim.qty() > 0.0);
    assert_eq!(sim.counters().buys_rejected_volume_share, 1);
}

#[test]
fn volume_nul_bloque_tout() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    sim.submit(place(Side::Buy, 95.0, 50.0, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    // 1m active mais sans volume : ni market, ni limit (même franchie).
    sim.on_bar(&bar_v(2, 100.0, 100.0, 90.0, 95.0, 0.0))
        .unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.pending_len(), 2);
    assert_eq!(sim.counters().bars_zero_volume_blocked, 1);
    // La 1m suivante remplit le market à son open.
    sim.on_bar(&flat(3, 101.0)).unwrap();
    assert!(sim.qty() > 0.0);
    let out = sim.finish(&flat(3, 101.0)).unwrap();
    assert_eq!(out.trades[0].entry_ref_price, 101.0);
}

#[test]
fn volume_nul_sans_ordre_actif_n_est_pas_compte() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&bar_v(0, 100.0, 100.0, 100.0, 100.0, 0.0))
        .unwrap();
    assert_eq!(sim.counters().bars_zero_volume_blocked, 0);
}

#[test]
fn entree_ignoree_si_deja_long() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(place(Side::Buy, 100.0, 100.0, 1, 100), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    // Enter market accepté (à plat), actif à open[3].
    sim.submit(enter(100.0, None, None), at(1)).unwrap();
    // La limit est remplie à la 1m 2.
    sim.on_bar(&bar(2, 100.0, 100.2, 99.0, 99.5)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim.on_bar(&flat(3, 100.0)).unwrap();
    assert_eq!(sim.counters().entries_dropped_while_long, 1);
    assert_eq!(sim.qty(), 1.0);
}

// ---------------------------------------------------------------------------------------------
// Ordre des contrôles de submit
// ---------------------------------------------------------------------------------------------

fn limit(price: f64, ttl_bars: u32) -> OrderPrice {
    OrderPrice::Limit { price, ttl_bars }
}

fn enter_full(
    size: SizeSpec,
    price: OrderPrice,
    stop: Option<f64>,
    tp: Option<f64>,
) -> Instruction {
    Instruction::Enter {
        size,
        price,
        stop,
        take_profit: tp,
        tag: "t",
    }
}

/// F4 de la revue S6 : un `Enter` market ne doit pas être accepté quand des limits d'achat ont réservé
/// presque tout le cash, il serait abandonné au fill sans entrer dans `signals_rejected`.
#[test]
fn enter_market_refuse_quand_le_quote_libre_ne_couvre_pas_un_lot() {
    // Lot d'une unité : une limit d'achat à 90 de 10 lots réserve 901,8, il reste 98,2 de quote libre,
    // soit 0,93 lot au prix de référence 100 après la marge de 5 % et le taker.
    let instrument = InstrumentRules::new(0.1, 1.0, 0.0, 0.0).unwrap();
    let nouveau = || {
        let mut sim = Simulator::new(
            okx(),
            Slippage::ZERO,
            SimRules::default(),
            instrument,
            1000.0,
            16,
        )
        .unwrap();
        sim.on_bar(&flat(0, 100.0)).unwrap();
        sim
    };
    let mut sim = nouveau();
    sim.submit(place(Side::Buy, 90.0, 900.0, 1, 100), at(0))
        .unwrap();
    assert_eq!(
        sim.submit(enter(50.0, None, None), at(0)),
        Err(Rejection::InsufficientQuote)
    );
    assert_eq!(
        sim.pending_len(),
        1,
        "le market refusé n'occupe aucune place"
    );
    // Avec du quote libre pour un lot au moins, le market est accepté (plafonné au fill s'il le faut).
    let mut sim = nouveau();
    sim.submit(place(Side::Buy, 90.0, 450.0, 1, 100), at(0))
        .unwrap();
    sim.submit(enter(500.0, None, None), at(0)).unwrap();
    assert_eq!(sim.pending_len(), 2);
}

#[test]
fn ordre_des_controles_de_submit() {
    let nan = f64::NAN;
    let quote = SizeSpec::Quote(100.0);
    let risk = SizeSpec::RiskPct(0.01);
    let ctx = at(0);
    let mut sim = sim_defaults(1000.0);

    // --- Enter : un cas par ligne, y compris les cumuls (le premier motif gagne) ---
    let cas = [
        // prix de limite invalide avant le TTL nul
        (
            enter_full(quote, limit(nan, 0), None, None),
            Rejection::InvalidPrice,
        ),
        // stop invalide avant le TTL nul
        (
            enter_full(quote, limit(99.0, 0), Some(-1.0), None),
            Rejection::InvalidPrice,
        ),
        (
            enter_full(quote, OrderPrice::Market, None, Some(nan)),
            Rejection::InvalidPrice,
        ),
        // TTL nul avant la taille invalide
        (
            enter_full(SizeSpec::Quote(nan), limit(99.0, 0), None, None),
            Rejection::InvalidTtl,
        ),
        // taille invalide avant risque sans stop
        (
            enter_full(SizeSpec::RiskPct(0.0), OrderPrice::Market, None, None),
            Rejection::InvalidSize,
        ),
        (
            enter_full(SizeSpec::PctEquity(1.5), OrderPrice::Market, None, None),
            Rejection::InvalidSize,
        ),
        (
            enter_full(risk, OrderPrice::Market, None, None),
            Rejection::RiskWithoutStop,
        ),
        // stop au-dessus de la référence
        (
            enter_full(risk, OrderPrice::Market, Some(101.0), None),
            Rejection::InvalidRiskSizing,
        ),
        (
            enter_full(risk, limit(99.0, 5), Some(120.0), None),
            Rejection::InvalidRiskSizing,
        ),
        // limite sous le tick
        (
            enter_full(quote, limit(0.04, 5), None, None),
            Rejection::InvalidPrice,
        ),
    ];
    for (instruction, attendu) in cas {
        assert_eq!(
            sim.submit(instruction, ctx),
            Err(attendu),
            "{instruction:?}"
        );
    }
    // Entrée acceptée, puis refus car une entrée est en attente.
    sim.submit(enter_full(quote, OrderPrice::Market, None, None), ctx)
        .unwrap();
    assert_eq!(
        sim.submit(enter_full(quote, OrderPrice::Market, None, None), ctx),
        Err(Rejection::EntryWhileLong)
    );

    // --- Exit ---
    let mut sim = sim_defaults(1000.0);
    let exit_limit = |price, ttl| Instruction::Exit {
        price: limit(price, ttl),
        tag: "x",
    };
    assert_eq!(
        sim.submit(exit_limit(nan, 0), ctx),
        Err(Rejection::InvalidPrice)
    );
    assert_eq!(
        sim.submit(exit_limit(99.0, 0), ctx),
        Err(Rejection::InvalidTtl)
    );
    assert_eq!(
        sim.submit(exit_market(), ctx),
        Err(Rejection::ExitWithoutPosition)
    );
    // File pleine avant « sans position ».
    let rules = SimRules {
        max_pending: 1,
        ..SimRules::default()
    };
    let mut petit = sim_with(rules, Slippage::ZERO, 1000.0);
    petit
        .submit(place(Side::Buy, 90.0, 10.0, 1, 5), ctx)
        .unwrap();
    assert_eq!(
        petit.submit(exit_market(), ctx),
        Err(Rejection::PendingFull)
    );
    // Doublon de sortie.
    let mut sim = long_one();
    sim.submit(exit_market(), at(2)).unwrap();
    assert_eq!(
        sim.submit(exit_market(), at(2)),
        Err(Rejection::DuplicateExit)
    );
    assert_eq!(
        sim.submit(exit_limit(101.0, 5), at(2)),
        Err(Rejection::DuplicateExit)
    );

    // --- Place ---
    let mut sim = sim_defaults(1000.0);
    let p = |price, size, slot, ttl| Instruction::Place {
        side: Side::Buy,
        price,
        size,
        ttl_bars: ttl,
        slot,
        tag: "p",
    };
    assert_eq!(
        sim.submit(p(nan, SizeSpec::Quote(10.0), 1, 0), ctx),
        Err(Rejection::InvalidPrice)
    );
    assert_eq!(
        sim.submit(p(90.0, SizeSpec::Quote(nan), 0, 0), ctx),
        Err(Rejection::InvalidTtl)
    );
    assert_eq!(
        sim.submit(p(90.0, SizeSpec::Quote(nan), 0, 3), ctx),
        Err(Rejection::InvalidSize)
    );
    assert_eq!(
        sim.submit(p(90.0, risk, 0, 3), ctx),
        Err(Rejection::InvalidSlot)
    );
    assert_eq!(
        sim.submit(p(90.0, risk, 1, 3), ctx),
        Err(Rejection::PlaceLimitRiskPct)
    );
    assert_eq!(
        sim.submit(p(0.04, SizeSpec::Quote(10.0), 1, 3), ctx),
        Err(Rejection::InvalidPrice)
    );
    assert_eq!(
        sim.submit(p(90.0, SizeSpec::Quote(5000.0), 1, 3), ctx),
        Err(Rejection::InsufficientQuote)
    );
    assert_eq!(
        sim.submit(
            Instruction::Place {
                side: Side::Sell,
                price: 90.0,
                size: SizeSpec::Quote(10.0),
                ttl_bars: 3,
                slot: 1,
                tag: "p"
            },
            ctx
        ),
        Err(Rejection::SellWithoutFreeQty)
    );
    // File pleine avant slot déjà ouvert.
    let mut petit = sim_with(rules, Slippage::ZERO, 1000.0);
    petit
        .submit(p(90.0, SizeSpec::Quote(10.0), 1, 3), ctx)
        .unwrap();
    assert_eq!(
        petit.submit(p(90.0, SizeSpec::Quote(10.0), 1, 3), ctx),
        Err(Rejection::PendingFull)
    );
    let mut sim = sim_defaults(1000.0);
    sim.submit(p(90.0, SizeSpec::Quote(10.0), 1, 3), ctx)
        .unwrap();
    assert_eq!(
        sim.submit(p(91.0, SizeSpec::Quote(10.0), 1, 3), ctx),
        Err(Rejection::SlotAlreadyOpen)
    );

    // --- MoveStop, CancelSlot ---
    let mut sim = sim_defaults(1000.0);
    assert_eq!(
        sim.submit(Instruction::MoveStop { stop: nan }, ctx),
        Err(Rejection::InvalidPrice)
    );
    assert_eq!(
        sim.submit(Instruction::MoveStop { stop: 95.0 }, ctx),
        Err(Rejection::MoveStopWithoutPosition)
    );
    assert_eq!(
        sim.submit(Instruction::CancelSlot { slot: 0 }, ctx),
        Err(Rejection::InvalidSlot)
    );
    assert_eq!(
        sim.submit(Instruction::CancelSlot { slot: 9 }, ctx),
        Err(Rejection::CancelUnknownSlot)
    );
}

#[test]
fn entree_limit_plafonnee_par_le_cash() {
    let mut sim = sim_defaults(100.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    // 500 voulus à 100 avec 100 de cash : plafonné à cash × 0,95 / (L × 1,002).
    sim.submit(
        Instruction::Enter {
            size: SizeSpec::Quote(500.0),
            price: limit(100.0, 5),
            stop: None,
            take_profit: None,
            tag: "e",
        },
        at(0),
    )
    .unwrap();
    assert!(sim.quote_free() > 0.0);
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&bar(2, 100.0, 100.0, 99.0, 99.5)).unwrap();
    assert_eq!(sim.counters().entries_capped_by_cash, 1);
    assert!((sim.qty() - 0.95 / 1.002).abs() < 1e-7, "{}", sim.qty());
    let out = sim.finish(&flat(2, 99.5)).unwrap();
    assert!(out.trades[0].capped_by_cash);
}

// ---------------------------------------------------------------------------------------------
// Minimums d'instrument
// ---------------------------------------------------------------------------------------------

#[test]
fn minimums() {
    let instrument = InstrumentRules::new(0.1, 1e-8, 0.0001, 10.0).unwrap();
    let mut sim = Simulator::new(
        okx(),
        Slippage::ZERO,
        SimRules::default(),
        instrument,
        1000.0,
        4,
    )
    .unwrap();
    sim.on_bar(&flat(0, 100.0)).unwrap();
    // 5 de notional : sous min_notional.
    assert_eq!(
        sim.submit(place(Side::Buy, 100.0, 5.0, 1, 5), at(0)),
        Err(Rejection::BelowMinimum)
    );
    // 1e-5 unité : sous min_qty (le notional de 1e-3 est aussi trop petit).
    assert_eq!(
        sim.submit(place(Side::Buy, 100.0, 0.001, 1, 5), at(0)),
        Err(Rejection::BelowMinimum)
    );
    // Assez gros : accepté.
    sim.submit(place(Side::Buy, 100.0, 50.0, 1, 5), at(0))
        .unwrap();
    // Un market est dimensionné au fill : accepté à la pose, rejeté au fill.
    let mut sim = Simulator::new(
        okx(),
        Slippage::ZERO,
        SimRules::default(),
        instrument,
        1000.0,
        4,
    )
    .unwrap();
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(5.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim.on_bar(&flat(2, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().buys_rejected_below_minimum, 1);
}

// ---------------------------------------------------------------------------------------------
// Stop, take-profit, IntrabarRule
// ---------------------------------------------------------------------------------------------

/// Entrée au marché à l'open de la 1m 2 (100, slippage 5 bps : 100.05), stop et TP donnés.
fn entree_avec(rules: SimRules, slippage: Slippage, stop: f64, tp: Option<f64>) -> Simulator {
    let mut sim = sim_with(rules, slippage, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.05, Some(stop), tp), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    sim
}

#[test]
fn stop_execute_au_pire_du_niveau_et_de_l_open() {
    let s = bps(5.0);
    let mut sim = entree_avec(SimRules::default(), s, 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    assert_eq!(sim.position().unwrap().stop, Some(99.0));
    // Niveau traversé, open au-dessus : fill au niveau, moins le slippage.
    sim.on_bar(&bar(3, 100.5, 101.0, 99.0, 99.5)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(3, 99.5)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::Stop);
    assert_eq!(t.exit_ref_price, 99.0);
    assert_eq!(t.exit_price, 99.0 * (1.0 - 0.0005));
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert_eq!(t.stop_at_exit, Some(99.0));
    assert_eq!(t.exit_ms, T0 + 3 * 60_000);
    assert_eq!(
        t.exit_signal_ms,
        T0 + 60_000,
        "signal de l'entrée qui a posé le stop"
    );
    assert_eq!(t.exit_tag, "stop");
    assert_eq!(out.counters.stops_triggered, 1);

    // Gap sous le stop : fill à l'open, moins le slippage.
    let mut sim = entree_avec(SimRules::default(), s, 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    sim.on_bar(&bar(3, 98.0, 98.5, 97.5, 98.2)).unwrap();
    let out = sim.finish(&flat(3, 98.2)).unwrap();
    assert_eq!(out.trades[0].exit_ref_price, 98.0);
    assert_eq!(out.trades[0].exit_price, 98.0 * (1.0 - 0.0005));
}

#[test]
fn stop_stress_ajoute() {
    let rules = SimRules {
        stop_stress_bps: 100.0,
        ..SimRules::default()
    };
    let mut sim = entree_avec(rules, bps(5.0), 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    sim.on_bar(&bar(3, 100.5, 101.0, 98.9, 99.5)).unwrap();
    let out = sim.finish(&flat(3, 99.5)).unwrap();
    // (5 + 100) / 10 000 : une seule division.
    assert_eq!(out.trades[0].exit_price, 99.0 * (1.0 - 105.0 / 10_000.0));
    close_to(out.trades[0].exit_slippage, 99.0 * 0.0105);
}

#[test]
fn stop_et_tp_meme_bougie_deux_regles() {
    let barre = bar(3, 100.4, 103.5, 98.8, 101.0);
    for (intrabar, raison) in [
        (IntrabarRule::StopFirst, ExitReason::Stop),
        (IntrabarRule::TpFirst, ExitReason::TakeProfit),
    ] {
        let rules = SimRules {
            intrabar,
            ..SimRules::default()
        };
        let mut sim = entree_avec(rules, bps(5.0), 99.0, Some(103.0));
        sim.on_bar(&bar(2, 100.0, 100.5, 99.5, 100.2)).unwrap();
        sim.on_bar(&barre).unwrap();
        assert_eq!(sim.qty(), 0.0);
        assert_eq!(sim.counters().stop_tp_same_bar, 1, "{intrabar:?}");
        let out = sim.finish(&flat(3, 101.0)).unwrap();
        assert_eq!(out.trades.len(), 1);
        let t = out.trades[0];
        assert_eq!(t.exit_reason, raison);
        match raison {
            ExitReason::Stop => assert_eq!(t.exit_ref_price, 99.0),
            _ => {
                assert_eq!(t.exit_price, 103.0);
                assert_eq!(t.exit_liquidity, Liquidity::Maker);
                assert_eq!(t.exit_tag, "take_profit");
                assert_eq!(out.counters.take_profits_triggered, 1);
            }
        }
    }
}

#[test]
fn tp_ignore_dans_la_bougie_de_fill() {
    let mut sim = entree_avec(SimRules::default(), Slippage::ZERO, 95.0, Some(102.0));
    // La 1m de fill franchit déjà le take-profit : ignoré et compté.
    sim.on_bar(&bar(2, 100.0, 103.5, 100.3, 101.0)).unwrap();
    assert_eq!(sim.counters().take_profits_ignored_fill_bar, 1);
    assert!(sim.qty() > 0.0);
    // La suivante le remplit, maker.
    sim.on_bar(&bar(3, 101.0, 103.2, 100.8, 102.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(3, 102.0)).unwrap();
    assert_eq!(out.trades[0].exit_reason, ExitReason::TakeProfit);
    assert_eq!(out.trades[0].exit_price, 102.0);
    assert_eq!(out.counters.take_profits_ignored_fill_bar, 1);
    // Désactivée par la règle : rempli dans la bougie de fill.
    let rules = SimRules {
        tp_ignored_on_fill_bar: false,
        ..SimRules::default()
    };
    let mut sim = entree_avec(rules, Slippage::ZERO, 95.0, Some(102.0));
    sim.on_bar(&bar(2, 100.0, 103.5, 100.3, 101.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
}

#[test]
fn stop_actif_dans_la_bougie_de_fill() {
    let mut sim = entree_avec(SimRules::default(), Slippage::ZERO, 99.0, None);
    // Entrée à 100 et franchissement du stop dans la même 1m : perte immédiate.
    sim.on_bar(&bar(2, 100.0, 100.5, 98.5, 99.5)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(2, 99.5)).unwrap();
    assert_eq!(out.trades.len(), 1);
    assert_eq!(out.trades[0].exit_reason, ExitReason::Stop);
    assert_eq!(out.trades[0].entry_ms, out.trades[0].exit_ms);
}

#[test]
fn move_stop_retarde_et_abandonne_si_plat() {
    let mut sim = entree_avec(SimRules::default(), Slippage::ZERO, 95.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    // Signal à la clôture de 2, effectif à open[4].
    sim.submit(Instruction::MoveStop { stop: 99.0 }, at(2))
        .unwrap();
    assert_eq!(sim.position().unwrap().stop, Some(95.0));
    sim.on_bar(&bar(3, 100.5, 101.0, 98.0, 100.0)).unwrap();
    assert!(
        sim.qty() > 0.0,
        "le stop déplacé n'est pas encore en vigueur"
    );
    assert_eq!(sim.position().unwrap().stop, Some(95.0));
    sim.on_bar(&bar(4, 100.0, 100.5, 98.5, 99.0)).unwrap();
    assert_eq!(sim.counters().move_stops_applied, 1);
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(4, 99.0)).unwrap();
    assert_eq!(out.trades[0].exit_reason, ExitReason::Stop);
    assert_eq!(out.trades[0].exit_ref_price, 99.0);

    // Position fermée avant l'effet : MoveStop abandonné et compté.
    let mut sim = entree_avec(SimRules::default(), Slippage::ZERO, 95.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    sim.submit(exit_market(), at(2)).unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.submit(Instruction::MoveStop { stop: 99.0 }, at(3))
        .unwrap();
    sim.on_bar(&flat(4, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    sim.on_bar(&flat(5, 100.0)).unwrap();
    assert_eq!(sim.counters().move_stops_dropped_flat, 1);
    assert_eq!(sim.counters().move_stops_applied, 0);
}

#[test]
fn stop_annule_les_ventes_limit() {
    let mut sim = entree_avec(SimRules::default(), Slippage::ZERO, 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    sim.submit(place(Side::Sell, 110.0, 50.0, 1, 100), at(2))
        .unwrap();
    assert!(sim.qty_free() < sim.qty());
    sim.on_bar(&bar(3, 100.0, 101.0, 99.5, 100.5)).unwrap();
    // Stop touché en même temps que la vente est active, pas encore franchie.
    sim.on_bar(&bar(4, 100.0, 100.5, 98.0, 99.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.counters().sells_cancelled_by_exit, 1);
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.reserved_lots, 0);
    assert_eq!(sim.qty_free(), 0.0);
}

#[test]
fn vente_limit_et_stop_meme_bougie_stop_d_abord() {
    // StopFirst : la vente limit franchie dans la même 1m que le stop est annulée, pas remplie.
    let mut sim = entree_avec(SimRules::default(), Slippage::ZERO, 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    sim.submit(place(Side::Sell, 104.0, 50.0, 1, 100), at(2))
        .unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar(4, 100.0, 105.0, 98.0, 99.0)).unwrap();
    let out = sim.finish(&flat(4, 99.0)).unwrap();
    assert_eq!(out.trades.len(), 1);
    assert_eq!(out.trades[0].exit_reason, ExitReason::Stop);
    assert_eq!(out.counters.sells_cancelled_by_exit, 1);
    // TpFirst : la vente limit passe d'abord, puis le stop sur le reste.
    let rules = SimRules {
        intrabar: IntrabarRule::TpFirst,
        ..SimRules::default()
    };
    let mut sim = entree_avec(rules, Slippage::ZERO, 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    sim.submit(place(Side::Sell, 104.0, 50.0, 1, 100), at(2))
        .unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&bar(4, 100.0, 105.0, 98.0, 99.0)).unwrap();
    let out = sim.finish(&flat(4, 99.0)).unwrap();
    assert_eq!(out.trades.len(), 2);
    assert_eq!(out.trades[0].exit_reason, ExitReason::LimitSell);
    assert_eq!(out.trades[1].exit_reason, ExitReason::Stop);
}

// ---------------------------------------------------------------------------------------------
// MFE, MAE, clôture forcée
// ---------------------------------------------------------------------------------------------

#[test]
fn mfe_mae() {
    // Sortie market : la 1m de sortie (b5, excursions extrêmes) est exclue.
    let mut sim = entree_avec(SimRules::default(), bps(5.0), 50.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.8, 100.5)).unwrap();
    sim.on_bar(&bar(3, 100.5, 103.0, 100.0, 102.0)).unwrap();
    sim.submit(exit_market(), at(3)).unwrap();
    sim.on_bar(&bar(4, 102.0, 102.5, 98.5, 99.0)).unwrap();
    sim.on_bar(&bar(5, 99.0, 110.0, 90.0, 95.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let out = sim.finish(&flat(5, 95.0)).unwrap();
    let t = out.trades[0];
    close_to(t.mfe, 103.0 - 100.05);
    close_to(t.mae, 98.5 - 100.05);
    assert_eq!(t.exit_price, 99.0 * (1.0 - 0.0005));

    // Un stop inclut sa 1m : le plus bas de la 1m du stop compte.
    let mut sim = entree_avec(SimRules::default(), bps(5.0), 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.8, 100.5)).unwrap();
    sim.on_bar(&bar(3, 100.5, 103.0, 97.0, 98.0)).unwrap();
    let out = sim.finish(&flat(3, 98.0)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::Stop);
    close_to(t.mfe, 103.0 - 100.05);
    close_to(t.mae, 97.0 - 100.05);
}

#[test]
fn finish_sans_position_et_ordre_en_attente() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.0, None, None), at(0)).unwrap();
    sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(0))
        .unwrap();
    assert_eq!(sim.pending_len(), 2);
    let out = sim.finish(&flat(0, 100.0)).unwrap();
    assert_eq!(out.counters.orders_unfilled_at_end, 2);
    assert!(out.trades.is_empty());
    assert!(!out.forced_close);
    assert_eq!(out.cash, 1000.0);
}

#[test]
fn finish_cloture_forcee() {
    let s = bps(10.0);
    let mut sim = sim_with(rules_s3(), s, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.1, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    let last = bar(2, 100.0, 112.0, 99.0, 110.0);
    sim.on_bar(&last).unwrap();
    let out = sim.finish(&last).unwrap();
    assert!(out.forced_close);
    assert_eq!(out.qty, 0.0);
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::ForcedClose);
    assert!(t.forced_close);
    assert_eq!(t.exit_tag, "forced_close");
    assert_eq!(t.exit_price, 110.0 * (1.0 - s.rate()));
    assert_eq!(t.exit_ref_price, 110.0);
    assert_eq!(t.exit_ms, last.open_time_ms + 60_000);
    assert_eq!(t.exit_signal_ms, last.open_time_ms + 60_000);
    assert_eq!(out.counters.fills, 2);
}

#[test]
fn pas_de_short() {
    let mut sim = sim_defaults(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    assert_eq!(
        sim.submit(exit_market(), at(0)),
        Err(Rejection::ExitWithoutPosition)
    );
    let mut sim = long_one();
    sim.submit(exit_market(), at(2)).unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&flat(4, 101.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(
        sim.submit(exit_market(), at(4)),
        Err(Rejection::ExitWithoutPosition)
    );
}

#[test]
fn sortie_ignoree_si_deja_a_plat() {
    // Un stop ferme la position avant que la sortie market en attente soit active.
    let mut sim = entree_avec(SimRules::default(), Slippage::ZERO, 99.0, None);
    sim.on_bar(&bar(2, 100.0, 101.0, 99.5, 100.5)).unwrap();
    sim.submit(exit_market(), at(2)).unwrap();
    sim.on_bar(&bar(3, 100.0, 100.5, 98.0, 98.5)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    sim.on_bar(&flat(4, 98.0)).unwrap();
    assert_eq!(sim.counters().exits_dropped_flat, 1);
}

// ---------------------------------------------------------------------------------------------
// Invariants
// ---------------------------------------------------------------------------------------------

#[test]
fn invariants_actifs_en_debug() {
    if !cfg!(debug_assertions) {
        panic!("les tests doivent tourner avec les assertions de debug");
    }
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "quote_reserved")]
fn invariant_viole_panique() {
    let mut sim = sim_defaults(1000.0);
    sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(0))
        .unwrap();
    sim.cash = 0.0;
    sim.check_invariants();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "slot en double")]
fn slot_en_double_panique() {
    let mut sim = sim_defaults(1000.0);
    sim.submit(place(Side::Buy, 90.0, 50.0, 1, 100), at(0))
        .unwrap();
    sim.submit(place(Side::Buy, 89.0, 50.0, 2, 100), at(0))
        .unwrap();
    if let Some(order) = sim.pending.get_mut(1) {
        order.slot = 1;
    }
    sim.check_invariants();
}

// ---------------------------------------------------------------------------------------------
// Portefeuille (S9) : cash étranger, plafond de notional, minimum d'ordre, trou de cotation
// ---------------------------------------------------------------------------------------------

/// Tick 0,1, lot 1e-8, quantité minimale `min_qty` et notional minimal `min_notional`.
fn inst_min(min_qty: f64, min_notional: f64) -> InstrumentRules {
    InstrumentRules::new(0.1, 1e-8, min_qty, min_notional).unwrap()
}

fn sim_min(min_qty: f64, min_notional: f64, cash: f64) -> Simulator {
    Simulator::new(
        okx(),
        Slippage::ZERO,
        rules_s3(),
        inst_min(min_qty, min_notional),
        cash,
        8,
    )
    .unwrap()
}

fn enter_risk(pct: f64, stop: f64) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::RiskPct(pct),
        price: OrderPrice::Market,
        stop: Some(stop),
        take_profit: None,
        tag: "risque",
    }
}

fn enter_limit(quote: f64, price: f64, stop: Option<f64>) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Limit { price, ttl_bars: 5 },
        stop,
        take_profit: None,
        tag: "limite",
    }
}

#[test]
fn le_quote_des_autres_jambes_reduit_le_cash_libre() {
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.foreign_reserved = 400.0;
    assert_eq!(sim.quote_free(), 600.0);
    // 700 + frais ne tient pas dans 600 ; 500 tient.
    assert_eq!(
        sim.submit(place(Side::Buy, 90.0, 700.0, 1, 100), at(0)),
        Err(Rejection::InsufficientQuote)
    );
    sim.submit(place(Side::Buy, 90.0, 500.0, 1, 100), at(0))
        .unwrap();
    let libre = sim.quote_free();
    assert!((libre - (600.0 - 500.0 * 1.002)).abs() < 1e-4, "{libre}");
}

#[test]
fn un_achat_market_est_plafonne_par_le_cash_libre_net_du_quote_etranger() {
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.foreign_reserved = 900.0;
    sim.submit(enter(500.0, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.counters().entries_capped_by_cash, 1);
    assert!(sim.qty() > 0.0 && sim.qty() < 1.0, "{}", sim.qty());
    assert!(
        sim.cash() >= 900.0,
        "le quote étranger reste intact : {}",
        sim.cash()
    );
}

#[test]
fn un_market_sans_cash_libre_net_du_quote_etranger_est_refuse_au_signal() {
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.foreign_reserved = 1000.0;
    assert_eq!(
        sim.submit(enter(500.0, None, None), at(0)),
        Err(Rejection::InsufficientQuote)
    );
}

#[test]
fn plafond_de_notional_market_au_fill() {
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit_capped(enter(500.0, None, None), at(0), Some(200.0))
        .unwrap();
    assert_eq!(sim.detail().entries_capped_by_notional, 0, "compté au fill");
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.qty(), 2.0);
    assert_eq!(sim.detail().entries_capped_by_notional, 1);
    assert_eq!(sim.counters().entries_capped_by_cash, 0);
    // Un plafond qui ne mord pas ne compte pas, et `None` rend `submit`.
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit_capped(enter(150.0, None, None), at(0), Some(200.0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.qty(), 1.5);
    assert_eq!(sim.detail().entries_capped_by_notional, 0);
}

#[test]
fn plafond_de_notional_limit_a_la_soumission() {
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit_capped(enter_limit(500.0, 100.0, None), at(0), Some(200.0))
        .unwrap();
    let vue: Vec<PendingView> = sim.pending().collect();
    assert_eq!(vue.len(), 1);
    assert_eq!(vue[0].qty, 2.0, "plafonné avant la réservation");
    assert_eq!(sim.detail().entries_capped_by_notional, 1);
    // La réservation suit la quantité plafonnée : 200 + frais maker.
    let libre = sim.quote_free();
    assert!((libre - (1000.0 - 200.0 * 1.002)).abs() < 1e-9, "{libre}");
    // Place d'achat : même plafond.
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.submit_capped(place(Side::Buy, 100.0, 500.0, 1, 10), at(0), Some(250.0))
        .unwrap();
    assert_eq!(sim.pending().next().unwrap().qty, 2.5);
    assert_eq!(sim.detail().entries_capped_by_notional, 1);
}

/// Simulateur au pas de quantité 0,01 (`inv_lot = 100`), pour placer le plafond à moins d'un
/// `STEP_SNAP` de lot sous un nombre entier de lots.
fn sim_lot_001(cash: f64) -> Simulator {
    let instrument = InstrumentRules::new(0.1, 0.01, 0.0, 0.0).unwrap();
    Simulator::new(okx(), Slippage::ZERO, rules_s3(), instrument, cash, 8).unwrap()
}

/// Plafond de 200 moins 5e-7 : à 100, `cap_q` = 199,9999995 lots, que `floor_steps` accroche à 200 lots
/// (`+ STEP_SNAP`). Le notional du fill dépasse alors le plafond de 5e-7, soit 2,5e-9 en relatif, bien
/// au-delà du `1e-12` littéral de docs S9 §7.4.
const CAP_SOUS_UN_LOT: f64 = 200.0 - 5e-7;

#[test]
fn plafond_de_notional_au_fill_ne_panique_pas_a_l_accrochage_d_un_lot() {
    // Achat market plafonné : 2 unités remplies pour un plafond de 199,9999995.
    let mut sim = sim_lot_001(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit_capped(enter(500.0, None, None), at(0), Some(CAP_SOUS_UN_LOT))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.qty(), 2.0);
    assert!(sim.qty() * 100.0 > CAP_SOUS_UN_LOT * (1.0 + 1e-12));
    // Limit d'entrée au repos : même arrondi à la soumission, même tolérance au fill.
    let mut sim = sim_lot_001(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit_capped(
        enter_limit(500.0, 100.0, None),
        at(0),
        Some(CAP_SOUS_UN_LOT),
    )
    .unwrap();
    sim.on_bar(&bar(1, 100.0, 100.0, 99.0, 99.5)).unwrap();
    assert_eq!(sim.qty(), 2.0);
    // `Place` d'achat au repos : le plafond est lui aussi porté par l'ordre.
    let mut sim = sim_lot_001(1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit_capped(
        place(Side::Buy, 100.0, 500.0, 1, 10),
        at(0),
        Some(CAP_SOUS_UN_LOT),
    )
    .unwrap();
    sim.on_bar(&bar(1, 100.0, 100.0, 99.0, 99.5)).unwrap();
    assert_eq!(sim.qty(), 2.0);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "plafond de notional")]
fn plafond_de_notional_depasse_au_fill_panique() {
    // Une limit de 2 unités à 100 est acceptée sous un plafond de 200, puis son plafond est ramené à 100
    // dans la file : au fill, le notional (200) dépasse le plafond.
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit_capped(enter_limit(500.0, 100.0, None), at(0), Some(200.0))
        .unwrap();
    assert_eq!(sim.pending().next().unwrap().qty, 2.0);
    sim.pending[0].max_notional = Some(100.0);
    let _ = sim.on_bar(&bar(1, 100.0, 100.0, 99.0, 99.5));
}

#[test]
fn le_plafond_de_notional_ne_touche_pas_les_ventes() {
    let mut sim = long_one();
    sim.submit_capped(exit_market(), at(2), Some(1.0)).unwrap();
    sim.on_bar(&flat(3, 100.0)).unwrap();
    sim.on_bar(&flat(4, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.detail().entries_capped_by_notional, 0);
}

#[test]
fn les_refus_sous_le_minimum_sont_decomposes() {
    // minSz 0,01 : 0,5 de quote à 100 donne 0,005, un nombre entier de lots mais sous le minimum.
    let mut sim = sim_min(0.01, 0.0, 1000.0);
    assert_eq!(
        sim.submit(enter_limit(0.5, 100.0, None), at(0)),
        Err(Rejection::BelowMinimum)
    );
    assert_eq!(sim.detail().below_min_min_qty, 1);
    // Quantité sous un lot : zéro lot.
    assert_eq!(
        sim.submit(enter_limit(1e-9, 100.0, None), at(0)),
        Err(Rejection::BelowMinimum)
    );
    assert_eq!(sim.detail().below_min_zero_lot, 1);
    // Place d'achat.
    assert_eq!(
        sim.submit(place(Side::Buy, 100.0, 0.5, 1, 5), at(0)),
        Err(Rejection::BelowMinimum)
    );
    assert_eq!(sim.detail().below_min_min_qty, 2);
    // Achat market refusé au fill : les deux compteurs bougent.
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(0.5, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.counters().buys_rejected_below_minimum, 1);
    assert_eq!(sim.detail().below_min_min_qty, 3);
    // Notional minimal : 5 de quote sous un minimum de 10.
    let mut sim = sim_min(0.0, 10.0, 1000.0);
    assert_eq!(
        sim.submit(enter_limit(5.0, 100.0, None), at(0)),
        Err(Rejection::BelowMinimum)
    );
    assert_eq!(sim.detail().below_min_notional, 1);
    assert_eq!(sim.detail().below_min_exits, 0);
}

#[test]
fn un_refus_de_vente_sous_le_minimum_va_dans_les_sorties() {
    // Position de 0,011 ; une vente limit libre de 0,011 passe, une vente limit partielle de 0,005 non.
    let mut sim = sim_min(0.01, 0.0, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(1.1, None, None), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert!(sim.qty() > 0.0);
    assert_eq!(
        sim.submit(place(Side::Sell, 110.0, 0.5, 1, 5), at(1)),
        Err(Rejection::BelowMinimum)
    );
    assert_eq!(sim.detail().below_min_exits, 1);
    assert_eq!(sim.detail().below_min_min_qty, 0, "les achats sont à part");
}

#[test]
fn montee_au_minimum_d_ordre_market() {
    // Risque 0,01 % de 1000 = 0,1 entre 100 et 98 : 0,05 unité, sous le minimum de 0,1.
    let mut sim = sim_min(0.1, 0.0, 1000.0);
    sim.bump_max_risk_pct = Some(0.015);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter_risk(0.0001, 98.0), at(0)).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.1, "portée à minSz");
    assert_eq!(sim.detail().entries_bumped_to_min, 1);
    assert_eq!(sim.counters().buys_rejected_below_minimum, 0);
    // Risque effectif 0,1 × 2 = 0,2 ≤ 1,5 % de 1000 : accepté. Avec une base de 10, la borne tombe à
    // 0,15 et la montée est refusée.
    let mut sim = sim_min(0.1, 0.0, 1000.0);
    sim.bump_max_risk_pct = Some(0.015);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    let ctx = SubmitContext {
        equity: 10.0,
        ..at(0)
    };
    sim.submit(enter_risk(0.0001, 98.0), ctx).unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.detail().entries_bumped_to_min, 0);
    assert_eq!(sim.counters().buys_rejected_below_minimum, 1);
    assert_eq!(
        sim.detail().below_min_zero_lot + sim.detail().below_min_min_qty,
        1
    );
}

#[test]
fn montee_au_minimum_d_ordre_limit_et_garde_fous() {
    let mut sim = sim_min(0.1, 0.0, 1000.0);
    sim.bump_max_risk_pct = Some(0.015);
    let limite = Instruction::Enter {
        size: SizeSpec::RiskPct(0.0001),
        price: OrderPrice::Limit {
            price: 100.0,
            ttl_bars: 5,
        },
        stop: Some(98.0),
        take_profit: None,
        tag: "l",
    };
    sim.submit(limite, at(0)).unwrap();
    assert_eq!(sim.pending().next().unwrap().qty, 0.1);
    assert_eq!(sim.detail().entries_bumped_to_min, 1);
    // Sans règle : refus.
    let mut sim = sim_min(0.1, 0.0, 1000.0);
    assert_eq!(sim.submit(limite, at(0)), Err(Rejection::BelowMinimum));
    // Sans stop, pas de risque calculable : refus (la taille en quote donne 0,05 < minSz).
    let mut sim = sim_min(0.1, 0.0, 1000.0);
    sim.bump_max_risk_pct = Some(0.015);
    assert_eq!(
        sim.submit(enter_limit(5.0, 100.0, None), at(0)),
        Err(Rejection::BelowMinimum)
    );
    // Le cash libre ne couvre pas le minimum (10 de cash, minimum 0,1 à 100 = 10 + frais) : pas de montée.
    let mut sim = sim_min(0.1, 0.0, 10.0);
    sim.bump_max_risk_pct = Some(0.5);
    assert!(sim.submit(limite, at(0)).is_err());
    assert_eq!(sim.detail().entries_bumped_to_min, 0);
    // Un plafond de notional sous le minimum d'ordre interdit aussi la montée.
    let mut sim = sim_min(0.1, 0.0, 1000.0);
    sim.bump_max_risk_pct = Some(0.015);
    assert_eq!(
        sim.submit_capped(limite, at(0), Some(5.0)),
        Err(Rejection::BelowMinimum)
    );
    assert_eq!(sim.detail().entries_bumped_to_min, 0);
}

/// Position longue d'une unité avec slippage de 10 bps, un stop à 90 et un take-profit à 120.
fn long_with_orders() -> Simulator {
    let mut sim = sim_with(rules_s3(), bps(10.0), 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter(100.1, Some(90.0), Some(120.0)), at(0))
        .unwrap();
    sim.on_bar(&flat(1, 100.0)).unwrap();
    assert_eq!(sim.qty(), 1.0);
    sim.submit(place(Side::Sell, 130.0, 50.0, 1, 1000), at(1))
        .unwrap();
    sim
}

#[test]
fn sortie_forcee_pour_trou_de_cotation() {
    let mut sim = long_with_orders();
    sim.on_bar(&flat(2, 101.0)).unwrap();
    assert_eq!(sim.pending_len(), 1);
    // Un `MoveStop` pas encore effectif est abandonné avec la position.
    sim.submit(Instruction::MoveStop { stop: 95.0 }, at(5))
        .unwrap();
    let exit_ms = T0 + 10_000 * 60_000;
    assert!(sim.force_exit_quote_gap(exit_ms).unwrap());
    assert_eq!(sim.qty(), 0.0);
    assert!(sim.position().is_none());
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.quote_free(), sim.cash());
    assert_eq!(sim.counters().sells_cancelled_by_exit, 1);
    assert_eq!(sim.detail().quote_gap_exits, 1);
    // Idempotent : à plat, plus rien.
    assert!(!sim.force_exit_quote_gap(exit_ms).unwrap());
    assert_eq!(sim.detail().quote_gap_exits, 1);
    let out = sim.finish(&flat(2, 101.0)).unwrap();
    assert_eq!(out.trades.len(), 1);
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::QuoteGap);
    assert!(!t.forced_close, "pas une clôture de fin de fenêtre");
    assert_eq!(t.exit_ms, exit_ms);
    assert_eq!(t.exit_signal_ms, exit_ms);
    assert_eq!(t.exit_order_ms, exit_ms);
    assert_eq!(t.exit_ref_price, 101.0, "dernier close");
    assert_eq!(t.exit_price, 101.0 * (1.0 - bps(10.0).rate()));
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    close_to(t.exit_fee, t.exit_price * 0.0035);
    close_to(t.exit_slippage, 101.0 - t.exit_price);
    assert_eq!(t.exit_tag, "quote_gap");
    assert_eq!(
        t.stop_at_exit,
        Some(90.0),
        "le MoveStop en attente est abandonné"
    );
    assert!(!t.stop_moved);
    // PnL réalisé : produit net moins coût net.
    close_to(
        t.pnl_net,
        (t.exit_price - t.exit_fee) - (t.entry_price + t.entry_fee),
    );
    assert!(!out.forced_close);
}

#[test]
fn sortie_forcee_sans_position_laisse_les_entrees_en_attente() {
    let mut sim = sim_with(rules_s3(), Slippage::ZERO, 1000.0);
    sim.on_bar(&flat(0, 100.0)).unwrap();
    sim.submit(enter_limit(100.0, 90.0, None), at(0)).unwrap();
    assert!(!sim.force_exit_quote_gap(T0 + 99 * 60_000).unwrap());
    assert_eq!(sim.pending_len(), 1, "l'entrée expirera par son TTL");
    assert_eq!(sim.detail().quote_gap_exits, 0);
}

#[test]
fn sortie_forcee_annule_aussi_les_achats_limit_en_attente() {
    let mut sim = long_one();
    sim.submit(place(Side::Buy, 80.0, 100.0, 1, 1000), at(2))
        .unwrap();
    sim.submit(place(Side::Sell, 150.0, 20.0, 2, 1000), at(2))
        .unwrap();
    assert!(sim.quote_free() < sim.cash());
    assert!(sim.force_exit_quote_gap(T0 + 50 * 60_000).unwrap());
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.counters().orders_cancelled, 1);
    assert_eq!(sim.counters().sells_cancelled_by_exit, 1);
    assert_eq!(sim.quote_free(), sim.cash());
}

#[test]
fn close_at_end_est_la_partie_de_finish_qui_solde() {
    let mut a = long_with_orders();
    a.on_bar(&flat(2, 101.0)).unwrap();
    let mut b = long_with_orders();
    b.on_bar(&flat(2, 101.0)).unwrap();
    let out = a.finish(&flat(2, 101.0)).unwrap();
    assert!(b.close_at_end(&flat(2, 101.0)).unwrap());
    assert!(!b.close_at_end(&flat(2, 101.0)).unwrap(), "rien à solder");
    assert_eq!(out.trades, b.trades);
    assert_eq!(out.cash.to_bits(), b.cash().to_bits());
    assert_eq!(out.counters, *b.counters());
    assert_eq!(out.trades[0].exit_reason, ExitReason::ForcedClose);
    assert_eq!(b.counters().orders_unfilled_at_end, 1);
    assert_eq!(b.pending_len(), 0);
}

// ---------------------------------------------------------------------------------------------
// Un `Book` à une jambe rejoue le simulateur au bit
// ---------------------------------------------------------------------------------------------

/// Exécute la suite d'opérations sur un `Simulator` et sur un `Book` à une jambe (mode capitalisé),
/// en parallèle, et exige des états identiques au bit après chaque opération.
fn comparer_book_et_simulateur(
    operations: &[Op],
    fees: FeeSchedule,
    rules: SimRules,
    cash: f64,
    slippage: f64,
) -> Result<(), TestCaseError> {
    let mut sim = Simulator::new(fees, bps(slippage), rules, inst(), cash, 8).unwrap();
    let config = BookConfig {
        fees,
        slippage: bps(slippage),
        rules,
        initial_cash: cash,
        max_positions: 1,
        mode: SizingMode::Compounding,
        min_order_bump_max_risk: None,
        trades_capacity: 8,
    };
    let mut book = Book::new(config, &[inst()]).unwrap();
    let mut index = 0i64;
    let mut last = flat(0, 100.0);
    for op in operations {
        match op {
            Op::Bar {
                open,
                up,
                down,
                close,
                volume,
            } => {
                let high = open * (1.0 + up);
                let low = open * (1.0 - down);
                let close = low + (high - low) * close;
                last = bar_v(index, *open, high, low, close, *volume);
                index += 1;
                prop_assert_eq!(
                    sim.on_bar(&last).map_err(|e| e.to_string()),
                    book.on_bar(0, &last).map_err(|e| e.to_string())
                );
                prop_assert_eq!(sim.cash().to_bits(), book.cash().to_bits());
                prop_assert_eq!(sim.equity_at(last.close).to_bits(), book.equity().to_bits());
                prop_assert_eq!(sim.quote_free().to_bits(), book.quote_free().to_bits());
                prop_assert_eq!(
                    sim.qty().to_bits(),
                    book.position(0).map_or(0.0, |p| p.qty).to_bits()
                );
                prop_assert_eq!(sim.qty_free().to_bits(), book.qty_free(0).to_bits());
                prop_assert_eq!(sim.pending_len(), book.pending_len(0));
                prop_assert!(book.pending(0).eq(sim.pending()));
                book.check_invariants();
            }
            other => {
                let ctx = SubmitContext {
                    signal_ms: T0 + index * 60_000,
                    equity: sim.equity_at(last.close),
                    ref_price: last.close,
                };
                let instruction = match other.clone() {
                    Op::Enter {
                        size,
                        amount,
                        limit,
                        stop,
                        tp,
                    } => {
                        let size = match size {
                            0 => SizeSpec::Quote(amount),
                            1 => SizeSpec::PctEquity((amount / 1e5).clamp(1e-6, 1.0)),
                            _ => SizeSpec::RiskPct((amount / 1e5).clamp(1e-6, 1.0)),
                        };
                        Instruction::Enter {
                            size,
                            price: limit.map_or(OrderPrice::Market, |(price, ttl_bars)| {
                                OrderPrice::Limit { price, ttl_bars }
                            }),
                            stop,
                            take_profit: tp,
                            tag: "e",
                        }
                    }
                    Op::Exit(limit) => Instruction::Exit {
                        price: limit.map_or(OrderPrice::Market, |(price, ttl_bars)| {
                            OrderPrice::Limit { price, ttl_bars }
                        }),
                        tag: "x",
                    },
                    Op::Place {
                        buy,
                        price,
                        amount,
                        slot,
                        ttl,
                    } => Instruction::Place {
                        side: if buy { Side::Buy } else { Side::Sell },
                        price,
                        size: SizeSpec::Quote(amount),
                        ttl_bars: ttl,
                        slot,
                        tag: "p",
                    },
                    Op::CancelSlot(slot) => Instruction::CancelSlot { slot },
                    Op::CancelAll => Instruction::CancelAll,
                    Op::MoveStop(stop) => Instruction::MoveStop { stop },
                    Op::Bar { .. } => continue,
                };
                let a = sim.submit(instruction, ctx);
                let b = book.submit(0, instruction, ctx, None);
                prop_assert_eq!(a.map_err(BookRejection::Sim), b);
            }
        }
    }
    let out = sim.finish(&last).unwrap();
    book.close_all_at_end(&[Some(last)]).unwrap();
    let book_out = book.into_outcome();
    prop_assert_eq!(out.cash.to_bits(), book_out.cash.to_bits());
    prop_assert_eq!(&out.trades, &book_out.trades);
    prop_assert!(book_out.trade_legs.iter().all(|&leg| leg == 0));
    prop_assert_eq!(book_out.trade_legs.len(), out.trades.len());
    prop_assert_eq!(out.counters, book_out.counters[0]);
    prop_assert_eq!(out.forced_close, book_out.forced_close);
    prop_assert_eq!(book_out.max_positions_rejected[0], 0);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Propriétés
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    Enter {
        size: u8,
        amount: f64,
        limit: Option<(f64, u32)>,
        stop: Option<f64>,
        tp: Option<f64>,
    },
    Exit(Option<(f64, u32)>),
    Place {
        buy: bool,
        price: f64,
        amount: f64,
        slot: u16,
        ttl: u32,
    },
    CancelSlot(u16),
    CancelAll,
    MoveStop(f64),
    Bar {
        open: f64,
        up: f64,
        down: f64,
        close: f64,
        volume: f64,
    },
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    let size = 0u8..3;
    let limit = proptest::option::of((1.0f64..500.0, 1u32..12));
    let op = prop_oneof![
        (
            size,
            1e-3f64..1e5,
            limit.clone(),
            proptest::option::of(0.5f64..400.0),
            proptest::option::of(1.0f64..600.0)
        )
            .prop_map(|(size, amount, limit, stop, tp)| Op::Enter {
                size,
                amount,
                limit,
                stop,
                tp
            }),
        limit.prop_map(Op::Exit),
        (
            any::<bool>(),
            1.0f64..500.0,
            1e-3f64..1e5,
            0u16..6,
            0u32..12
        )
            .prop_map(|(buy, price, amount, slot, ttl)| Op::Place {
                buy,
                price,
                amount,
                slot,
                ttl
            }),
        (0u16..6).prop_map(Op::CancelSlot),
        Just(Op::CancelAll),
        (0.5f64..500.0).prop_map(Op::MoveStop),
        (
            50.0f64..250.0,
            0.0f64..0.05,
            0.0f64..0.05,
            0.0f64..1.0,
            prop_oneof![Just(0.0f64), 1.0f64..1e7]
        )
            .prop_map(|(open, up, down, close, volume)| Op::Bar {
                open,
                up,
                down,
                close,
                volume
            }),
    ];
    proptest::collection::vec(op, 1..80)
}

/// Rejoue une suite d'opérations et rend l'issue et les empreintes binaires.
fn rejouer(
    operations: &[Op],
    fees: FeeSchedule,
    rules: SimRules,
    cash: f64,
    slippage: f64,
) -> Result<(SimOutcome, Vec<u64>), TestCaseError> {
    let mut sim = Simulator::new(fees, bps(slippage), rules, inst(), cash, 8).unwrap();
    let mut index = 0i64;
    let mut last = flat(0, 100.0);
    let mut empreintes = Vec::new();
    for op in operations {
        match op {
            Op::Bar {
                open,
                up,
                down,
                close,
                volume,
            } => {
                let high = open * (1.0 + up);
                let low = open * (1.0 - down);
                let close = low + (high - low) * close;
                last = bar_v(index, *open, high, low, close, *volume);
                index += 1;
                let result = sim.on_bar(&last);
                prop_assert!(result.is_ok(), "{result:?}");
                prop_assert!(sim.cash().is_finite() && sim.cash() >= 0.0);
                prop_assert!(sim.quote_free() >= 0.0 || sim.quote_free().abs() < 1e-6);
                prop_assert!(sim.qty() >= 0.0 && sim.qty_free() >= 0.0);
                prop_assert!(sim.qty_free() <= sim.qty());
                prop_assert!(sim.pending_len() <= rules.max_pending);
                empreintes.push(sim.cash().to_bits());
                empreintes.push(sim.qty().to_bits());
            }
            other => {
                let ctx = SubmitContext {
                    signal_ms: T0 + index * 60_000,
                    equity: sim.equity_at(last.close),
                    ref_price: last.close,
                };
                let instruction = match other.clone() {
                    Op::Enter {
                        size,
                        amount,
                        limit,
                        stop,
                        tp,
                    } => {
                        let size = match size {
                            0 => SizeSpec::Quote(amount),
                            1 => SizeSpec::PctEquity((amount / 1e5).clamp(1e-6, 1.0)),
                            _ => SizeSpec::RiskPct((amount / 1e5).clamp(1e-6, 1.0)),
                        };
                        Instruction::Enter {
                            size,
                            price: limit.map_or(OrderPrice::Market, |(price, ttl_bars)| {
                                OrderPrice::Limit { price, ttl_bars }
                            }),
                            stop,
                            take_profit: tp,
                            tag: "e",
                        }
                    }
                    Op::Exit(limit) => Instruction::Exit {
                        price: limit.map_or(OrderPrice::Market, |(price, ttl_bars)| {
                            OrderPrice::Limit { price, ttl_bars }
                        }),
                        tag: "x",
                    },
                    Op::Place {
                        buy,
                        price,
                        amount,
                        slot,
                        ttl,
                    } => Instruction::Place {
                        side: if buy { Side::Buy } else { Side::Sell },
                        price,
                        size: SizeSpec::Quote(amount),
                        ttl_bars: ttl,
                        slot,
                        tag: "p",
                    },
                    Op::CancelSlot(slot) => Instruction::CancelSlot { slot },
                    Op::CancelAll => Instruction::CancelAll,
                    Op::MoveStop(stop) => Instruction::MoveStop { stop },
                    Op::Bar { .. } => continue,
                };
                let accepted = sim.submit(instruction, ctx).is_ok();
                empreintes.push(u64::from(accepted));
            }
        }
    }
    let out = sim.finish(&last);
    prop_assert!(out.is_ok(), "{out:?}");
    let out = out.map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(out.qty, 0.0);
    Ok((out, empreintes))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn cash_et_quantite_jamais_negatifs(
        taker in 0u32..=500,
        maker in 0u32..=500,
        slippage in 0.0f64..200.0,
        cash in 1.0f64..1e7,
        delay in 0u32..=3,
        cross in 0u32..=3,
        tp_first in any::<bool>(),
        operations in ops(),
    ) {
        let rules = SimRules {
            activation_delay_bars: delay,
            limit_cross_ticks: cross,
            intrabar: if tp_first { IntrabarRule::TpFirst } else { IntrabarRule::StopFirst },
            ..SimRules::default()
        };
        let fees = FeeSchedule::new(ExchangeId::Okx, maker, taker).unwrap();
        let (a, ea) = rejouer(&operations, fees, rules, cash, slippage)?;
        // Déterminisme : deux exécutions identiques au bit.
        let (b, eb) = rejouer(&operations, fees, rules, cash, slippage)?;
        prop_assert_eq!(ea, eb);
        prop_assert_eq!(a.cash.to_bits(), b.cash.to_bits());
        prop_assert_eq!(a.trades.len(), b.trades.len());
        for (x, y) in a.trades.iter().zip(&b.trades) {
            prop_assert_eq!(x, y);
        }
        prop_assert_eq!(a.counters, b.counters);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Un portefeuille à une jambe en mode capitalisé ne change pas l'arithmétique : mêmes trades, même
    /// cash, même equity, mêmes compteurs, au bit (docs S9 §2.3).
    #[test]
    fn un_book_a_une_jambe_rejoue_le_simulateur_au_bit(
        taker in 0u32..=500,
        maker in 0u32..=500,
        slippage in 0.0f64..200.0,
        cash in 1.0f64..1e7,
        delay in 0u32..=3,
        cross in 0u32..=3,
        tp_first in any::<bool>(),
        operations in ops(),
    ) {
        let rules = SimRules {
            activation_delay_bars: delay,
            limit_cross_ticks: cross,
            intrabar: if tp_first { IntrabarRule::TpFirst } else { IntrabarRule::StopFirst },
            ..SimRules::default()
        };
        let fees = FeeSchedule::new(ExchangeId::Okx, maker, taker).unwrap();
        comparer_book_et_simulateur(&operations, fees, rules, cash, slippage)?;
    }
}
