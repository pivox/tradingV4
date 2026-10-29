//! Profil maker du simulateur (docs/specs/s10.md §7) : conversion d'une entrée market en limit sous le
//! bid du signal, un tirage de fill par ordre (`queue_missed`, `tp_queue_missed`), sélection adverse
//! débitée à part, réservation élargie.
//!
//! Bougies et paramètres de coût inventés pour le test (marché plat à 100, valeurs rondes). Les tirages
//! attendus sont recalculés ici par `model::rng`.

use tv4::model::costs::{EntryMode, LegCost, StopCost, StopSlippage, StopSlippageBps};
use tv4::model::rng::{self, STREAM_MAKER_FILL};
use tv4::model::{Candle, ExchangeId, FeeSchedule, InstrumentRules};
use tv4::sim::{
    ExitReason, Instruction, Liquidity, OrderPrice, Rejection, SimRules, Simulator, SizeSpec,
    SubmitContext,
};

const T0: i64 = 1_704_067_200_000;
const MINUTE: i64 = 60_000;
const SYMBOL: &str = "TEST-USD";

/// Demi-spread de 5 bp, slippage market de 3 bp, sélection adverse de 1,5 bp : valeurs rondes de test.
const H: f64 = 0.0005;
const S: f64 = 0.0003;
const ADVERSE: f64 = 0.00015;
const TTL: u32 = 30;
const MAKER_FEE: f64 = 0.0020;

fn fees() -> FeeSchedule {
    FeeSchedule::new(ExchangeId::Okx, 20, 35).expect("grille valide")
}

fn instrument() -> InstrumentRules {
    InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).expect("instrument valide")
}

fn rules() -> SimRules {
    SimRules {
        activation_delay_bars: 0,
        max_volume_share: 1e9,
        ..SimRules::default()
    }
}

fn maker_cost(fill_probability: f64, offset_rate: f64) -> LegCost {
    LegCost {
        half_spread_rate: H,
        market_slippage_rate: S,
        stop: StopCost::Drawn {
            dist: StopSlippage::from_quantiles(StopSlippageBps {
                p50: 5.0,
                p95: 50.0,
                p99: 200.0,
                p100: 200.0,
            }),
            stress_probability: 0.0,
            stress_drop: 0.03,
        },
        entry: EntryMode::Maker {
            ttl_minutes: TTL,
            fill_probability,
            offset_rate,
        },
        adverse_selection_rate: ADVERSE,
        leg_key: rng::leg_key(SYMBOL),
        seed: 1,
    }
}

fn sim(cost: LegCost) -> Simulator {
    Simulator::with_cost(fees(), cost, rules(), instrument(), 1_000.0, 16).expect("simulateur")
}

fn bar(i: i64, open: f64, high: f64, low: f64, close: f64) -> Candle {
    Candle {
        open_time_ms: T0 + i * MINUTE,
        open,
        high,
        low,
        close,
        volume: 1e6,
        trades: None,
    }
}

fn flat(i: i64) -> Candle {
    bar(i, 100.0, 100.0, 100.0, 100.0)
}

fn at(sim: &Simulator, i: i64, close: f64) -> SubmitContext {
    SubmitContext {
        signal_ms: T0 + (i + 1) * MINUTE,
        equity: sim.equity_at(close),
        ref_price: close,
    }
}

fn enter_market(quote: f64) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Market,
        stop: None,
        take_profit: None,
        tag: "entree",
    }
}

fn enter_limit(quote: f64, price: f64, take_profit: Option<f64>) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Limit {
            price,
            ttl_bars: 20,
        },
        stop: None,
        take_profit,
        tag: "entree",
    }
}

fn exit_limit(price: f64) -> Instruction {
    Instruction::Exit {
        price: OrderPrice::Limit {
            price,
            ttl_bars: 20,
        },
        tag: "sortie",
    }
}

fn close_to(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-9 * a.abs().max(1.0), "{a} != {b}");
}

/// Une 1m qui traverse `L = 99,9` en profondeur (open au-dessus, plus bas sous le niveau).
fn crossing(i: i64) -> Candle {
    bar(i, 100.0, 100.0, 99.0, 99.5)
}

#[test]
fn l_entree_market_devient_une_limit_sous_le_bid_du_signal() {
    // ref 100,037 : 100,037 × (1 − 5 bp) = 99,987 → arrondi au tick vers le bas : 99,9. TTL de 30 min.
    let mut sim = sim(maker_cost(1.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.037);
    sim.submit(enter_market(500.0), ctx).unwrap();
    let pending: Vec<_> = sim.pending().collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].limit, Some((99.9, TTL)));
    assert_eq!(
        pending[0].expires_at_ms,
        Some(T0 + MINUTE + i64::from(TTL) * MINUTE),
        "TTL en 1m à partir de l'activation"
    );
    assert!(pending[0].qty > 0.0);
    assert_eq!(sim.cost_counters().maker_entries_converted, 1);

    // Une seconde entrée est refusée : le compteur ne compte que les ordres acceptés.
    let ctx = at(&sim, 0, 100.037);
    assert_eq!(
        sim.submit(enter_market(500.0), ctx),
        Err(Rejection::EntryWhileLong)
    );
    assert_eq!(sim.cost_counters().maker_entries_converted, 1);
}

#[test]
fn le_decalage_abaisse_la_limit_au_dela_du_demi_spread() {
    // o = 20 bp : taux h + o = 25 bp, 100,037 × 0,9975 = 99,787 → 99,7.
    let mut sim = sim(maker_cost(1.0, 0.002));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.037);
    sim.submit(enter_market(500.0), ctx).unwrap();
    let view = sim.pending().next().unwrap();
    assert_eq!(view.limit, Some((99.7, TTL)));
    // Arrondi vers le bas, jamais vers le haut : 100,0 × (1 − 5 bp) = 99,95 → 99,9.
    let mut sim2 = self::sim(maker_cost(1.0, 0.0));
    sim2.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim2, 0, 100.0);
    sim2.submit(enter_market(500.0), ctx).unwrap();
    assert_eq!(sim2.pending().next().unwrap().limit, Some((99.9, TTL)));
}

#[test]
fn une_limit_de_la_strategie_garde_son_prix_et_son_ttl() {
    let mut sim = sim(maker_cost(1.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_limit(500.0, 99.0, None), ctx).unwrap();
    assert_eq!(sim.pending().next().unwrap().limit, Some((99.0, 20)));
    assert_eq!(sim.cost_counters().maker_entries_converted, 0);
}

#[test]
fn marketable_a_l_arrivee_part_en_taker_et_se_compte() {
    // L = 99,9 ; l'open d'arrivée (99,0) est sous la limite : prise en taker à min(open × (1 + h + s), L).
    let mut sim = sim(maker_cost(0.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0), ctx).unwrap();
    sim.on_bar(&bar(1, 99.0, 99.5, 98.5, 99.0)).unwrap();
    let counters = *sim.cost_counters();
    let sim_counters = *sim.counters();
    let out = sim.finish(&flat(1)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    assert_eq!(t.entry_price, 99.0 * (1.0 + (H + S)));
    assert_eq!(counters.maker_entries_taken_at_arrival, 1);
    assert_eq!(counters.maker_entries_filled_maker, 0);
    assert_eq!(
        counters.maker_fill_draws, 0,
        "pas de tirage pour un fill taker"
    );
    assert_eq!(
        counters.adverse_selection_total, 0.0,
        "pas de sélection adverse en taker"
    );
    assert_eq!(sim_counters.limits_marketable_at_arrival, 1);
    assert_eq!(t.entry_fee, t.qty * t.entry_price * 0.0035);
}

#[test]
fn au_repos_un_tirage_reussi_remplit_a_l_en_maker_avec_selection_adverse() {
    let mut sim = sim(maker_cost(1.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0), ctx).unwrap();
    sim.on_bar(&crossing(1)).unwrap();
    let counters = *sim.cost_counters();
    assert_eq!(counters.maker_fill_draws, 1);
    assert_eq!(counters.maker_fills, 1);
    assert_eq!(counters.maker_fill_missed, 0);
    assert_eq!(counters.maker_entries_filled_maker, 1);
    assert_eq!(counters.maker_entries_taken_at_arrival, 0);
    let cash = sim.cash();
    let out = sim.finish(&flat(1)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.entry_liquidity, Liquidity::Maker);
    assert_eq!(t.entry_price, 99.9, "le prix reste L (décision 9)");
    assert_eq!(t.entry_slippage, 0.0);
    let cost = t.qty * 99.9;
    let debit = cost * ADVERSE;
    assert_eq!(
        t.entry_spread, debit,
        "la sélection adverse est portée par entry_spread"
    );
    assert_eq!(
        t.entry_fee,
        cost * MAKER_FEE,
        "les frais restent ceux de l'exchange"
    );
    close_to(counters.adverse_selection_total, debit);
    assert_eq!(counters.spread_cost_total, 0.0);
    // Le débit est retiré du cash au fill.
    close_to(cash, 1_000.0 - (cost + cost * MAKER_FEE + debit));
}

#[test]
fn la_selection_adverse_est_dans_le_pnl_et_dans_spread_cost() {
    let mut sim = sim(maker_cost(1.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0), ctx).unwrap();
    sim.on_bar(&crossing(1)).unwrap();
    let ctx = at(&sim, 1, 100.0);
    sim.submit(
        Instruction::Exit {
            price: OrderPrice::Market,
            tag: "sortie",
        },
        ctx,
    )
    .unwrap();
    sim.on_bar(&flat(2)).unwrap();
    let out = sim.finish(&flat(2)).unwrap();
    let t = out.trades[0];
    let entry_debit = t.qty * 99.9 * ADVERSE;
    // Sortie market taker : part de spread dans le prix, aucun débit.
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert_eq!(t.exit_price, 100.0 * (1.0 - (H + S)));
    close_to(t.exit_spread, t.qty * 100.0 * H);
    let expected = (t.qty * t.exit_price - t.exit_fee) - (t.qty * 99.9 + t.entry_fee + entry_debit);
    close_to(t.pnl_net, expected);
    // Sans le débit, le PnL serait plus haut : il est bien dedans.
    let without = (t.qty * t.exit_price - t.exit_fee) - (t.qty * 99.9 + t.entry_fee);
    assert!(t.pnl_net < without);
    close_to(without - t.pnl_net, entry_debit);
}

#[test]
fn un_tirage_rate_marque_l_ordre_qui_ne_se_remplit_plus_et_expire() {
    let mut sim = sim(maker_cost(0.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let cash = sim.cash();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0), ctx).unwrap();
    let reserved = cash - sim.quote_free();
    assert!(reserved > 500.0);
    // Plusieurs traversées profondes : un seul tirage, jamais de fill.
    for i in 1..=5 {
        sim.on_bar(&crossing(i)).unwrap();
    }
    let counters = *sim.cost_counters();
    assert_eq!(
        counters.maker_fill_draws, 1,
        "un tirage par ordre, pas un par 1m traversée"
    );
    assert_eq!(counters.maker_fill_missed, 1);
    assert_eq!(counters.maker_fills, 0);
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.pending_len(), 1);
    assert_eq!(
        cash - sim.quote_free(),
        reserved,
        "l'ordre manqué garde sa réservation jusqu'à son expiration"
    );
    // Expiration au TTL : 30 minutes après l'activation (1m 1).
    for i in 6..=40 {
        sim.on_bar(&flat(i)).unwrap();
    }
    assert_eq!(sim.pending_len(), 0);
    assert_eq!(sim.counters().limits_expired, 1);
    assert_eq!(sim.quote_free(), sim.cash(), "la réservation est libérée");
    assert_eq!(sim.cost_counters().maker_entries_filled_maker, 0);
}

#[test]
fn le_tirage_de_fill_suit_la_cle_de_l_ordre() {
    let p = 0.5;
    let mut successes = 0;
    for i in 0..60i64 {
        let mut sim = sim(maker_cost(p, 0.0));
        for k in 0..=i {
            sim.on_bar(&flat(k)).unwrap();
        }
        let ctx = at(&sim, i, 100.0);
        sim.submit(enter_market(500.0), ctx).unwrap();
        sim.on_bar(&crossing(i + 1)).unwrap();
        // Clé : [leg_key, flux 3, active_from_ms, L.to_bits() ^ (côté achat << 63)].
        let active_from = T0 + (i + 1) * MINUTE;
        let u = rng::unit(rng::draw(
            1,
            [
                rng::leg_key(SYMBOL),
                STREAM_MAKER_FILL,
                rng::ts_word(active_from),
                99.9f64.to_bits(),
            ],
        ));
        let expected = u < p;
        successes += usize::from(expected);
        assert_eq!(sim.qty() > 0.0, expected, "ordre {i}");
        let counters = sim.cost_counters();
        assert_eq!(counters.maker_fill_draws, 1);
        assert_eq!(counters.maker_fills, u64::from(expected));
        assert_eq!(counters.maker_fill_missed, u64::from(!expected));
    }
    assert!(
        (1..60).contains(&successes),
        "les deux issues doivent se produire sur 60 ordres ({successes})"
    );
}

#[test]
fn la_reservation_couvre_la_selection_adverse() {
    let mut sim = sim(maker_cost(1.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    // Une grille qui réserve presque tout le cash : Quote 997,0 à 100 → réservé 997,0 × 1,00215.
    sim.submit(
        Instruction::Place {
            side: tv4::model::Side::Buy,
            price: 100.0,
            size: SizeSpec::Quote(997.0),
            ttl_bars: 20,
            slot: 1,
            tag: "grille",
        },
        ctx,
    )
    .unwrap();
    let reserved = sim.cash() - sim.quote_free();
    let cost = (997.0f64 / 100.0) * 100.0;
    assert!(
        reserved > cost * (1.0 + MAKER_FEE),
        "la sélection adverse est réservée"
    );
    close_to(reserved, cost * (1.0 + MAKER_FEE + ADVERSE));
    // Le fill au repos laisse un cash positif ou nul : le débit était déjà réservé.
    sim.on_bar(&bar(1, 100.0, 100.0, 99.0, 99.5)).unwrap();
    assert!(sim.qty() > 0.0);
    assert!(sim.cash() >= 0.0, "cash {}", sim.cash());
    let counters = sim.cost_counters();
    close_to(counters.adverse_selection_total, cost * ADVERSE);

    // 997,95 passerait avec la seule réservation maker (999,9459 < 1 000) mais pas avec la sélection
    // adverse (1 000,0956) : refusé à la pose, jamais de cash négatif au fill.
    let mut sim = self::sim(maker_cost(1.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    let result = sim.submit(
        Instruction::Place {
            side: tv4::model::Side::Buy,
            price: 100.0,
            size: SizeSpec::Quote(997.95),
            ttl_bars: 20,
            slot: 1,
            tag: "grille",
        },
        ctx,
    );
    assert_eq!(result, Err(Rejection::InsufficientQuote));
}

/// Entrée limit native marketable à l'arrivée (taker, donc sans tirage) avec un take-profit à 101,
/// signalée à la clôture de la 1m `s`. Rend le simulateur après la 1m de fill (`s + 1`).
fn long_with_take_profit(p: f64, s: i64) -> Simulator {
    let mut sim = sim(maker_cost(p, 0.0));
    for k in 0..=s {
        sim.on_bar(&flat(k)).unwrap();
    }
    let ctx = at(&sim, s, 100.0);
    sim.submit(enter_limit(500.0, 100.5, Some(101.0)), ctx)
        .unwrap();
    sim.on_bar(&flat(s + 1)).unwrap();
    assert!(sim.qty() > 0.0, "entrée taker remplie à l'arrivée");
    sim
}

/// 1m qui traverse le take-profit à 101.
fn tp_cross(i: i64) -> Candle {
    bar(i, 100.0, 102.0, 99.95, 101.0)
}

#[test]
fn le_tirage_du_take_profit_suit_la_cle_tp_active_from() {
    let p = 0.5;
    let mut successes = 0;
    for s in 0..60i64 {
        let mut sim = long_with_take_profit(p, s);
        // Le TP arrive à la 1m qui suit le fill : `tp_active_from_ms` = open de la 1m `s + 2`.
        let tp_active_from = T0 + (s + 2) * MINUTE;
        sim.on_bar(&tp_cross(s + 2)).unwrap();
        let u = rng::unit(rng::draw(
            1,
            [
                rng::leg_key(SYMBOL),
                STREAM_MAKER_FILL,
                rng::ts_word(tp_active_from),
                101.0f64.to_bits() ^ (1 << 63),
            ],
        ));
        let expected = u < p;
        successes += usize::from(expected);
        assert_eq!(
            sim.counters().take_profits_triggered,
            u64::from(expected),
            "s {s}"
        );
        assert_eq!(sim.cost_counters().maker_fill_draws, 1);
        assert_eq!(sim.cost_counters().maker_fills, u64::from(expected));
        assert_eq!(sim.cost_counters().maker_fill_missed, u64::from(!expected));
        assert_eq!(sim.qty() == 0.0, expected);
    }
    assert!(
        (1..60).contains(&successes),
        "les deux issues ({successes})"
    );
}

#[test]
fn le_take_profit_reussi_se_remplit_au_prix_en_maker_avec_selection_adverse() {
    let mut sim = long_with_take_profit(1.0, 0);
    sim.on_bar(&tp_cross(2)).unwrap();
    let out = sim.finish(&flat(2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    assert_eq!(t.exit_liquidity, Liquidity::Maker);
    assert_eq!(t.exit_price, 101.0);
    assert_eq!(t.exit_slippage, 0.0);
    let proceeds = t.qty * 101.0;
    close_to(t.exit_spread, proceeds * ADVERSE);
    assert_eq!(t.exit_fee, proceeds * MAKER_FEE);
}

#[test]
fn un_take_profit_manque_n_est_pas_rearme_par_un_move_stop_et_se_retire_au_suivant() {
    let mut sim = long_with_take_profit(0.0, 0);
    sim.on_bar(&tp_cross(2)).unwrap();
    assert_eq!(sim.cost_counters().maker_fill_missed, 1);
    assert_eq!(sim.counters().take_profits_triggered, 0);
    assert!(sim.qty() > 0.0, "la position reste ouverte");
    // D'autres traversées : aucun nouveau tirage, aucun fill.
    sim.on_bar(&tp_cross(3)).unwrap();
    // Un MoveStop ne réarme pas le take-profit.
    let ctx = at(&sim, 3, 101.0);
    sim.submit(Instruction::MoveStop { stop: 99.0 }, ctx)
        .unwrap();
    sim.on_bar(&tp_cross(4)).unwrap();
    sim.on_bar(&tp_cross(5)).unwrap();
    assert_eq!(sim.cost_counters().maker_fill_draws, 1);
    assert_eq!(sim.counters().take_profits_triggered, 0);
    assert_eq!(sim.counters().move_stops_applied, 1);
    // Le stop sort de la position (taker, tirage de stop).
    sim.on_bar(&bar(6, 100.0, 100.0, 98.0, 98.5)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    assert_eq!(sim.cost_counters().stop_fills_drawn, 1);
    // Position suivante : un take-profit neuf, un tirage neuf (`tp_queue_missed` est remis à faux).
    sim.on_bar(&flat(7)).unwrap();
    let ctx = at(&sim, 7, 100.0);
    sim.submit(enter_limit(500.0, 100.5, Some(101.0)), ctx)
        .unwrap();
    sim.on_bar(&flat(8)).unwrap();
    assert!(sim.qty() > 0.0);
    sim.on_bar(&tp_cross(9)).unwrap();
    assert_eq!(
        sim.cost_counters().maker_fill_draws,
        2,
        "le TP suivant est tiré à son tour"
    );
    assert_eq!(sim.cost_counters().maker_fill_missed, 2);
}

#[test]
fn une_vente_limit_au_repos_tire_son_fill() {
    // p = 1 : remplie à `L` en maker, avec sélection adverse.
    let mut sim = long_with_take_profit(1.0, 0);
    let ctx = at(&sim, 1, 100.0);
    sim.submit(exit_limit(101.0), ctx).unwrap();
    sim.on_bar(&bar(2, 100.0, 101.5, 99.95, 100.5)).unwrap();
    let out = sim.finish(&flat(2)).unwrap();
    let t = out.trades[0];
    assert_eq!(t.exit_liquidity, Liquidity::Maker);
    assert_eq!(t.exit_price, 101.0);
    close_to(t.exit_spread, t.qty * 101.0 * ADVERSE);

    // p = 0 : l'ordre est manqué, reste en file jusqu'à l'expiration, la position reste ouverte.
    let mut sim = long_with_take_profit(0.0, 0);
    // Sans take-profit déclenché : la limite de sortie est posée au-dessus de la traversée du TP.
    let ctx = at(&sim, 1, 100.0);
    sim.submit(exit_limit(100.6), ctx).unwrap();
    sim.on_bar(&bar(2, 100.0, 100.7, 99.95, 100.2)).unwrap();
    sim.on_bar(&bar(3, 100.0, 100.7, 99.95, 100.2)).unwrap();
    assert_eq!(sim.cost_counters().maker_fill_draws, 1);
    assert_eq!(sim.cost_counters().maker_fill_missed, 1);
    assert!(sim.qty() > 0.0);
    assert_eq!(
        sim.counters().fills_limit,
        1,
        "seule l'entrée limit est un fill limit"
    );
    assert_eq!(sim.pending_len(), 1);
    for i in 4..=30 {
        sim.on_bar(&flat(i)).unwrap();
    }
    assert_eq!(sim.pending_len(), 0);
    assert!(sim.counters().limits_expired >= 1);
}

#[test]
fn la_part_maker_des_entrees_converties_se_lit_dans_les_compteurs() {
    // Deux entrées converties : l'une prise en taker à l'arrivée, l'autre au repos (tirage réussi).
    let mut sim = sim(maker_cost(1.0, 0.0));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(300.0), ctx).unwrap();
    sim.on_bar(&bar(1, 99.0, 99.5, 98.5, 99.0)).unwrap();
    let ctx = at(&sim, 1, 99.0);
    sim.submit(
        Instruction::Exit {
            price: OrderPrice::Market,
            tag: "sortie",
        },
        ctx,
    )
    .unwrap();
    sim.on_bar(&flat(2)).unwrap();
    assert_eq!(sim.qty(), 0.0);
    let ctx = at(&sim, 2, 100.0);
    sim.submit(enter_market(300.0), ctx).unwrap();
    sim.on_bar(&crossing(3)).unwrap();
    let counters = sim.cost_counters();
    assert_eq!(counters.maker_entries_converted, 2);
    assert_eq!(counters.maker_entries_taken_at_arrival, 1);
    assert_eq!(counters.maker_entries_filled_maker, 1);
}
