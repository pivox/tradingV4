//! Simulateur partagé du portefeuille (`sim::Book`, docs S9 §3, §4.3, §7) : cash commun, plafond de
//! positions, plafond de notional du mode recherche, trou de cotation, minimum d'ordre.
//!
//! Marché inventé : chaque jambe est plate à son prix, très liquide. Aucune donnée de marché.

use tv4::model::{Candle, ExchangeId, FeeSchedule, InstrumentRules, Side, Slippage};
use tv4::sim::{
    Book, BookConfig, BookRejection, ExitReason, Instruction, Liquidity, OrderPrice, Rejection,
    SimError, SimRules, SizeSpec, SizingMode, SubmitContext,
};

const T0: i64 = 1_704_067_200_000;
const MINUTE: i64 = 60_000;

fn fees() -> FeeSchedule {
    FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap()
}

/// Fill à l'open de la 1m suivante, aucune limite de volume : seul le portefeuille est en jeu.
fn rules() -> SimRules {
    SimRules {
        activation_delay_bars: 0,
        max_volume_share: 1e9,
        ..SimRules::default()
    }
}

fn instrument(min_qty: f64) -> InstrumentRules {
    InstrumentRules::new(0.1, 1e-8, min_qty, 0.0).unwrap()
}

fn config(mode: SizingMode, cash: f64, max_positions: u16) -> BookConfig {
    BookConfig {
        fees: fees(),
        slippage: Slippage::ZERO,
        rules: rules(),
        initial_cash: cash,
        max_positions,
        mode,
        min_order_bump_max_risk: None,
        trades_capacity: 16,
    }
}

fn book(mode: SizingMode, cash: f64, max_positions: u16, legs: usize) -> Book {
    Book::new(
        config(mode, cash, max_positions),
        &vec![instrument(0.0); legs],
    )
    .unwrap()
}

fn flat(index: i64, price: f64) -> Candle {
    Candle {
        open_time_ms: T0 + index * MINUTE,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: 1.0e6,
        trades: None,
    }
}

/// Contexte d'un signal émis à la clôture de la 1m `index`, equity `equity`.
fn at(index: i64, equity: f64) -> SubmitContext {
    SubmitContext {
        signal_ms: T0 + (index + 1) * MINUTE,
        equity,
        ref_price: 100.0,
    }
}

fn enter_quote(quote: f64) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Market,
        stop: None,
        take_profit: None,
        tag: "entree",
    }
}

fn enter_limit(quote: f64, price: f64, ttl_bars: u32) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Limit { price, ttl_bars },
        stop: None,
        take_profit: None,
        tag: "limite",
    }
}

fn exit_market() -> Instruction {
    Instruction::Exit {
        price: OrderPrice::Market,
        tag: "sortie",
    }
}

fn place_buy(price: f64, quote: f64, slot: u16) -> Instruction {
    Instruction::Place {
        side: Side::Buy,
        price,
        size: SizeSpec::Quote(quote),
        ttl_bars: 1000,
        slot,
        tag: "grille",
    }
}

/// Une 1m plate pour chaque jambe.
fn bars(book: &mut Book, index: i64, prices: &[f64]) {
    for (k, price) in prices.iter().enumerate() {
        book.on_bar(k, &flat(index, *price)).unwrap();
    }
}

#[test]
fn trois_jambes_n_egal_2_en_mode_recherche() {
    // Capital 1 000, N = 2, plafond de 450 par entrée (la marge de cash de 5 % d'un achat market empêche
    // la dernière place d'atteindre tout à fait capital / N : le plafond du test reste sous 500).
    let mut b = book(SizingMode::Research, 1000.0, 2, 3);
    let cap = Some(450.0);
    bars(&mut b, 0, &[100.0, 50.0, 10.0]);
    b.submit(0, enter_quote(900.0), at(0, 1000.0), cap).unwrap();
    b.submit(1, enter_quote(900.0), at(0, 1000.0), cap).unwrap();
    // La troisième entrée dépasse N = 2 : refus du portefeuille, avant la jambe.
    assert_eq!(
        b.submit(2, enter_quote(900.0), at(0, 1000.0), cap),
        Err(BookRejection::MaxPositions)
    );
    assert_eq!(b.positions_cap_hits(), 1);
    // Les signaux sans entrée ne sont pas concernés par N.
    assert_eq!(
        b.submit(2, exit_market(), at(0, 1000.0), cap),
        Err(BookRejection::Sim(Rejection::ExitWithoutPosition))
    );
    bars(&mut b, 1, &[100.0, 50.0, 10.0]);
    // Plafond de notional : 450 / 100 = 4,5 unités, 450 / 50 = 9 unités.
    assert_eq!(b.position(0).unwrap().qty, 4.5);
    assert_eq!(b.position(1).unwrap().qty, 9.0);
    assert!(b.position(2).is_none());
    assert_eq!(b.detail(0).unwrap().entries_capped_by_notional, 1);
    assert_eq!(b.detail(1).unwrap().entries_capped_by_notional, 1);
    // Cash commun : 1 000 moins deux achats de 450 et leurs frais taker.
    let attendu = 1000.0 - 2.0 * (450.0 + 450.0 * 0.0035);
    assert!((b.cash() - attendu).abs() < 1e-9, "{}", b.cash());
    // Equity : cash + positions au dernier close.
    assert!((b.equity() - (b.cash() + 450.0 + 450.0)).abs() < 1e-9);
    // Une troisième entrée reste impossible tant que deux positions sont ouvertes.
    assert_eq!(
        b.submit(2, enter_quote(100.0), at(1, 1000.0), cap),
        Err(BookRejection::MaxPositions)
    );
    // La jambe 0 sort : une place se libère pour la jambe 2.
    b.submit(0, exit_market(), at(1, 1000.0), cap).unwrap();
    bars(&mut b, 2, &[100.0, 50.0, 10.0]);
    assert!(b.position(0).is_none());
    b.submit(2, enter_quote(900.0), at(2, 1000.0), cap).unwrap();
    bars(&mut b, 3, &[100.0, 50.0, 10.0]);
    assert_eq!(b.position(2).unwrap().qty, 45.0, "450 / 10");
    // Fin de fenêtre : les positions restantes sont soldées jambe par jambe.
    let last = [
        Some(flat(3, 100.0)),
        Some(flat(3, 50.0)),
        Some(flat(3, 10.0)),
    ];
    b.close_all_at_end(&last).unwrap();
    let out = b.into_outcome();
    assert_eq!(out.max_open_positions, 2);
    assert!(
        out.max_cost_to_initial <= 1.0 + 1e-9 && out.max_cost_to_initial > 0.89,
        "{}",
        out.max_cost_to_initial
    );
    assert_eq!(out.max_positions_rejected, vec![0, 0, 2]);
    // Journal : trade de la jambe 0 (signal), puis les soldes de fin de fenêtre des jambes 1 et 2.
    assert_eq!(out.trades.len(), 3);
    assert_eq!(out.trade_legs, vec![0, 1, 2]);
    assert_eq!(out.trades[0].exit_reason, ExitReason::Signal);
    assert_eq!(out.trades[1].exit_reason, ExitReason::ForcedClose);
    assert!(out.forced_close);
    // Les lots sont numérotés sur tout le portefeuille, dans l'ordre des fills.
    let mut ids: Vec<u64> = out.trades.iter().map(|t| t.lot_id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![0, 1, 2]);
    // Les compteurs sont par jambe.
    assert_eq!(out.counters[0].fills, 2);
    assert_eq!(out.counters[1].fills, 2);
    assert_eq!(out.counters[2].fills, 2);
}

#[test]
fn le_cash_commun_borne_les_entrees_du_mode_capitalise() {
    // Capital 100, deux jambes, N = 2 : les deux market de 90 passent au signal, le second est plafonné
    // par le cash restant au fill.
    let mut b = book(SizingMode::Compounding, 100.0, 2, 2);
    bars(&mut b, 0, &[100.0, 100.0]);
    b.submit(0, enter_quote(90.0), at(0, 100.0), None).unwrap();
    b.submit(1, enter_quote(90.0), at(0, 100.0), None).unwrap();
    bars(&mut b, 1, &[100.0, 100.0]);
    assert!(b.cash() >= 0.0);
    let q0 = b.position(0).unwrap().qty;
    let q1 = b.position(1).unwrap().qty;
    assert_eq!(q0, 0.9);
    assert!(q1 > 0.0 && q1 < 0.1, "second achat plafonné : {q1}");
    assert_eq!(b.counters(0).unwrap().entries_capped_by_cash, 0);
    assert_eq!(b.counters(1).unwrap().entries_capped_by_cash, 1);
    b.close_all_at_end(&[Some(flat(1, 100.0)), Some(flat(1, 100.0))])
        .unwrap();
    let out = b.into_outcome();
    assert!(!out.trades[0].capped_by_cash);
    assert!(out.trades[1].capped_by_cash);
    assert_eq!(out.trade_legs, vec![0, 1]);
    assert!(out.cash > 0.0);
}

#[test]
fn les_reservations_d_une_jambe_reduisent_le_cash_libre_des_autres() {
    let mut b = book(SizingMode::Compounding, 1000.0, 3, 2);
    bars(&mut b, 0, &[100.0, 100.0]);
    b.submit(0, place_buy(90.0, 600.0, 1), at(0, 1000.0), None)
        .unwrap();
    let libre = b.quote_free();
    assert!((libre - (1000.0 - 600.0 * 1.002)).abs() < 1e-3, "{libre}");
    // La jambe 1 ne peut pas réserver 500 : il ne reste que 398.
    assert_eq!(
        b.submit(1, place_buy(90.0, 500.0, 1), at(0, 1000.0), None),
        Err(BookRejection::Sim(Rejection::InsufficientQuote))
    );
    assert_eq!(b.quote_free(), libre, "un refus ne réserve rien");
    b.submit(1, place_buy(90.0, 300.0, 1), at(0, 1000.0), None)
        .unwrap();
    assert!(b.quote_free() < 100.0 && b.quote_free() > 0.0);
    // Un market de la jambe 1 est plafonné par le cash libre net des réservations de la jambe 0.
    let mut b = book(SizingMode::Compounding, 1000.0, 3, 2);
    bars(&mut b, 0, &[100.0, 100.0]);
    b.submit(0, place_buy(90.0, 900.0, 1), at(0, 1000.0), None)
        .unwrap();
    b.submit(1, enter_quote(500.0), at(0, 1000.0), None)
        .unwrap();
    bars(&mut b, 1, &[100.0, 100.0]);
    assert_eq!(b.counters(1).unwrap().entries_capped_by_cash, 1);
    assert!(b.position(1).unwrap().qty < 1.0);
    // Le quote réservé par la jambe 0 reste couvert.
    assert!(b.quote_free() >= -1e-9, "{}", b.quote_free());
    assert!(b.cash() >= 900.0 * 1.002 - 1e-9, "{}", b.cash());
}

#[test]
fn les_entrees_en_attente_comptent_pour_n() {
    let mut b = book(SizingMode::Compounding, 1000.0, 2, 3);
    bars(&mut b, 0, &[100.0, 100.0, 100.0]);
    // Une limit d'entrée en attente (jamais traversée) occupe une place, comme une position.
    b.submit(0, enter_limit(100.0, 50.0, 100), at(0, 1000.0), None)
        .unwrap();
    // Une `Place` d'achat aussi.
    b.submit(1, place_buy(50.0, 100.0, 1), at(0, 1000.0), None)
        .unwrap();
    assert_eq!(
        b.submit(2, enter_quote(100.0), at(0, 1000.0), None),
        Err(BookRejection::MaxPositions)
    );
    assert_eq!(
        b.submit(2, place_buy(50.0, 100.0, 1), at(0, 1000.0), None),
        Err(BookRejection::MaxPositions)
    );
    // Une jambe déjà occupée rend le refus de la jambe, pas celui du portefeuille (équivalence avec le
    // mono-paire à N = 1).
    assert_eq!(
        b.submit(0, enter_quote(100.0), at(0, 1000.0), None),
        Err(BookRejection::Sim(Rejection::EntryWhileLong))
    );
    // L'annulation libère la place une fois effective.
    b.submit(0, Instruction::CancelAll, at(0, 1000.0), None)
        .unwrap();
    bars(&mut b, 1, &[100.0, 100.0, 100.0]);
    assert_eq!(b.pending_len(0), 0);
    b.submit(2, enter_quote(100.0), at(1, 1000.0), None)
        .unwrap();
    assert_eq!(b.positions_cap_hits(), 2);
}

#[test]
fn une_limit_echue_sur_une_jambe_sans_bougie_libere_sa_place_et_son_quote() {
    // N = 1, deux jambes. La jambe 0 pose une limit d'entrée sous le marché (TTL 2 minutes, jamais
    // traversée), puis n'a plus de bougie pendant 10 minutes : seule la jambe 1 avance.
    let mut b = book(SizingMode::Compounding, 1000.0, 1, 2);
    bars(&mut b, 0, &[100.0, 100.0]);
    b.submit(0, enter_limit(100.0, 50.0, 2), at(0, 1000.0), None)
        .unwrap();
    assert!(b.quote_free() < 900.0, "{}", b.quote_free());
    for index in 1..=10 {
        b.on_bar(1, &flat(index, 100.0)).unwrap();
    }
    // Sans l'application des échéances, l'ordre échu garde la seule place et son quote réservé.
    assert_eq!(b.pending_len(0), 1);
    assert_eq!(
        b.submit(1, enter_quote(100.0), at(10, 1000.0), None),
        Err(BookRejection::MaxPositions)
    );
    // Le moteur applique les échéances de la jambe sans bougie à la minute 10, sans fill.
    b.expire_due(0, T0 + 10 * MINUTE).unwrap();
    assert_eq!(b.pending_len(0), 0);
    assert_eq!(b.quote_free(), 1000.0);
    assert_eq!(b.counters(0).unwrap().limits_expired, 1);
    assert_eq!(b.counters(0).unwrap().orders_cancelled, 0);
    assert_eq!(b.counters(0).unwrap().fills, 0);
    assert!(b.position(0).is_none());
    b.submit(1, enter_quote(100.0), at(10, 1000.0), None)
        .unwrap();
    // Un second appel ne compte rien de plus (le prochain `on_bar` de la jambe non plus).
    b.expire_due(0, T0 + 10 * MINUTE).unwrap();
    b.on_bar(0, &flat(10, 100.0)).unwrap();
    assert_eq!(b.counters(0).unwrap().limits_expired, 1);
    assert_eq!(b.positions_cap_hits(), 1, "seul le refus d'avant l'appel");
}

#[test]
fn une_limit_echue_ne_rogne_plus_le_cash_libre_des_autres_jambes() {
    // N = 2 : le market de la jambe 1 est plafonné par le cash libre amputé du quote réservé par la limit
    // de la jambe 0 tant qu'elle n'a pas été retirée ; une fois retirée, il est rempli en entier.
    let run = |expire: bool| {
        let mut b = book(SizingMode::Compounding, 1000.0, 2, 2);
        bars(&mut b, 0, &[100.0, 100.0]);
        b.submit(0, enter_limit(900.0, 50.0, 2), at(0, 1000.0), None)
            .unwrap();
        for index in 1..=10 {
            b.on_bar(1, &flat(index, 100.0)).unwrap();
        }
        if expire {
            b.expire_due(0, T0 + 10 * MINUTE).unwrap();
        }
        b.submit(1, enter_quote(500.0), at(10, 1000.0), None)
            .unwrap();
        b.on_bar(1, &flat(11, 100.0)).unwrap();
        (
            b.counters(1).unwrap().entries_capped_by_cash,
            b.position(1).unwrap().qty,
        )
    };
    let (capped, qty) = run(false);
    assert_eq!(capped, 1);
    assert!(qty < 1.0, "{qty}");
    let (capped, qty) = run(true);
    assert_eq!(capped, 0);
    assert!((qty - 5.0).abs() < 1e-9, "{qty}");
}

#[test]
fn une_annulation_echue_sur_une_jambe_sans_bougie_est_appliquee_et_comptee() {
    let mut b = book(SizingMode::Compounding, 1000.0, 1, 2);
    bars(&mut b, 0, &[100.0, 100.0]);
    b.submit(0, enter_limit(100.0, 50.0, 1000), at(0, 1000.0), None)
        .unwrap();
    b.submit(0, Instruction::CancelAll, at(0, 1000.0), None)
        .unwrap();
    // Avant l'échéance de l'annulation (minute 1), rien ne bouge.
    b.expire_due(0, T0).unwrap();
    assert_eq!(b.pending_len(0), 1);
    b.expire_due(0, T0 + MINUTE).unwrap();
    assert_eq!(b.pending_len(0), 0);
    assert_eq!(b.counters(0).unwrap().orders_cancelled, 1);
    assert_eq!(b.counters(0).unwrap().limits_expired, 0);
    assert_eq!(b.quote_free(), 1000.0);
    // Une jambe inconnue est une erreur, une jambe sans ordre en attente ne fait rien.
    assert!(matches!(
        b.expire_due(7, T0),
        Err(SimError::UnknownLeg { leg: 7, legs: 2 })
    ));
    b.expire_due(1, T0 + 5 * MINUTE).unwrap();
}

#[test]
fn le_trou_de_cotation_d_une_jambe_a_plat_annule_ses_entrees_en_attente() {
    let mut b = book(SizingMode::Compounding, 1000.0, 3, 2);
    bars(&mut b, 0, &[100.0, 100.0]);
    // Jambe 0 : un achat market en attente (il ne réserve rien) ; jambe 1 : une `Place` d'achat limit
    // (elle réserve du quote). Toutes deux sont des entrées en attente d'une jambe à plat.
    b.submit(0, enter_quote(100.0), at(0, 1000.0), None)
        .unwrap();
    b.submit(1, place_buy(50.0, 100.0, 1), at(0, 1000.0), None)
        .unwrap();
    assert!(b.quote_free() < 900.0);
    assert_eq!(b.cancel_entries_quote_gap(0).unwrap(), 1);
    assert_eq!(b.cancel_entries_quote_gap(1).unwrap(), 1);
    // Rien à annuler la seconde fois.
    assert_eq!(b.cancel_entries_quote_gap(0).unwrap(), 0);
    assert_eq!(b.pending_len(0) + b.pending_len(1), 0);
    assert_eq!(b.quote_free(), 1000.0);
    assert_eq!(b.counters(0).unwrap().orders_cancelled, 1);
    assert_eq!(b.counters(1).unwrap().orders_cancelled, 1);
    assert!(b.cancel_entries_quote_gap(5).is_err());
}

#[test]
fn n_egal_1_equivaut_au_mono_paire_pour_une_jambe_occupee() {
    let mut b = book(SizingMode::Compounding, 1000.0, 1, 1);
    bars(&mut b, 0, &[100.0]);
    b.submit(0, enter_quote(100.0), at(0, 1000.0), None)
        .unwrap();
    assert_eq!(
        b.submit(0, enter_quote(100.0), at(0, 1000.0), None),
        Err(BookRejection::Sim(Rejection::EntryWhileLong))
    );
    assert_eq!(b.positions_cap_hits(), 0);
}

#[test]
fn sortie_forcee_pour_trou_de_cotation_d_une_jambe() {
    let mut b = book(SizingMode::Compounding, 1000.0, 2, 2);
    bars(&mut b, 0, &[100.0, 100.0]);
    b.submit(0, enter_quote(100.0), at(0, 1000.0), None)
        .unwrap();
    b.submit(1, enter_quote(100.0), at(0, 1000.0), None)
        .unwrap();
    bars(&mut b, 1, &[100.0, 100.0]);
    // La jambe 0 cote encore, la jambe 1 n'a plus de bougie : sortie forcée de la seule jambe 1.
    b.on_bar(0, &flat(2, 110.0)).unwrap();
    let exit_ms = T0 + 10_000 * MINUTE;
    assert!(b.force_exit_quote_gap(1, exit_ms).unwrap());
    assert!(!b.force_exit_quote_gap(1, exit_ms).unwrap(), "déjà à plat");
    assert!(b.position(1).is_none());
    assert!(b.position(0).is_some(), "la jambe 0 n'est pas touchée");
    assert_eq!(b.detail(1).unwrap().quote_gap_exits, 1);
    assert_eq!(b.detail(0).unwrap().quote_gap_exits, 0);
    let (trades, legs) = b.trades();
    assert_eq!(legs, &[1]);
    let t = trades[0];
    assert_eq!(t.exit_reason, ExitReason::QuoteGap);
    assert!(!t.forced_close);
    assert_eq!(t.exit_ms, exit_ms);
    assert_eq!(t.exit_price, 100.0, "dernier close, sans slippage");
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert!((t.exit_fee - 100.0 * 0.0035).abs() < 1e-9);
    // Elle repart quand ses bougies reviennent.
    b.on_bar(1, &flat(20_000, 100.0)).unwrap();
    b.submit(1, enter_quote(100.0), at(20_000, 1000.0), None)
        .unwrap();
}

#[test]
fn montee_au_minimum_d_ordre_dans_le_portefeuille() {
    let mut cfg = config(SizingMode::Compounding, 1000.0, 2);
    cfg.min_order_bump_max_risk = Some(0.015);
    // Minimum d'ordre de 0,1 : un risque de 0,01 % entre 100 et 98 donne 0,05 unité.
    let mut b = Book::new(cfg, &[instrument(0.1), instrument(0.1)]).unwrap();
    bars(&mut b, 0, &[100.0, 100.0]);
    let risque = Instruction::Enter {
        size: SizeSpec::RiskPct(0.0001),
        price: OrderPrice::Market,
        stop: Some(98.0),
        take_profit: None,
        tag: "risque",
    };
    b.submit(0, risque, at(0, 1000.0), None).unwrap();
    bars(&mut b, 1, &[100.0, 100.0]);
    assert_eq!(b.position(0).unwrap().qty, 0.1);
    assert_eq!(b.detail(0).unwrap().entries_bumped_to_min, 1);
    assert_eq!(b.detail(1).unwrap().entries_bumped_to_min, 0);
}

#[test]
fn configurations_invalides() {
    let un = [instrument(0.0)];
    let invalide = |cfg: BookConfig, instruments: &[InstrumentRules]| {
        matches!(
            Book::new(cfg, instruments),
            Err(SimError::InvalidRules { .. })
        )
    };
    assert!(invalide(config(SizingMode::Compounding, 1000.0, 1), &[]));
    assert!(invalide(config(SizingMode::Compounding, 1000.0, 0), &un));
    // `min_order_bump` : mode capitalisé seulement, risque dans ]0, 1].
    let mut cfg = config(SizingMode::Research, 1000.0, 1);
    cfg.min_order_bump_max_risk = Some(0.015);
    assert!(invalide(cfg, &un));
    for risque in [0.0, -0.1, 1.5, f64::NAN] {
        let mut cfg = config(SizingMode::Compounding, 1000.0, 1);
        cfg.min_order_bump_max_risk = Some(risque);
        assert!(invalide(cfg, &un), "{risque}");
    }
    let mut cfg = config(SizingMode::Compounding, 1000.0, 1);
    cfg.min_order_bump_max_risk = Some(0.015);
    assert!(Book::new(cfg, &un).is_ok());
    // Capital invalide : erreur du simulateur.
    assert!(matches!(
        Book::new(config(SizingMode::Compounding, 0.0, 1), &un),
        Err(SimError::InvalidInitialCash { .. })
    ));
}

#[test]
fn jambe_inconnue() {
    let mut b = book(SizingMode::Compounding, 1000.0, 1, 1);
    assert!(matches!(
        b.on_bar(3, &flat(0, 100.0)),
        Err(SimError::UnknownLeg { leg: 3, legs: 1 })
    ));
    assert_eq!(
        b.submit(3, enter_quote(10.0), at(0, 1000.0), None),
        Err(BookRejection::UnknownLeg)
    );
    assert!(b.force_exit_quote_gap(3, T0).is_err());
    assert!(b.position(3).is_none());
    assert_eq!(b.pending(3).count(), 0);
    assert_eq!(b.qty_free(3), 0.0);
    assert!(b.counters(3).is_none() && b.detail(3).is_none());
    assert!(b.last_close(3).is_none());
    // Une entrée par jambe à la clôture, ni plus ni moins.
    assert!(b.close_all_at_end(&[]).is_err());
    assert!(b.close_all_at_end(&[None, None]).is_err());
    assert!(b.close_all_at_end(&[None]).is_ok());
}

#[test]
fn le_chemin_mono_jambe_rend_les_memes_bits_que_le_simulateur() {
    // Scénario court écrit à la main (le rejeu aléatoire est dans `sim::tests`) : market, stop, sortie.
    use tv4::sim::Simulator;
    let mut sim =
        Simulator::new(fees(), Slippage::ZERO, rules(), instrument(0.0), 1000.0, 16).unwrap();
    let mut b = book(SizingMode::Compounding, 1000.0, 1, 1);
    let candles = [
        flat(0, 100.0),
        flat(1, 101.0),
        flat(2, 99.0),
        flat(3, 103.0),
    ];
    let entree = Instruction::Enter {
        size: SizeSpec::RiskPct(0.01),
        price: OrderPrice::Market,
        stop: Some(95.0),
        take_profit: None,
        tag: "e",
    };
    for (i, candle) in candles.iter().enumerate() {
        sim.on_bar(candle).unwrap();
        b.on_bar(0, candle).unwrap();
        assert_eq!(sim.cash().to_bits(), b.cash().to_bits());
        assert_eq!(
            sim.equity_at(candle.close).to_bits(),
            b.equity().to_bits(),
            "bougie {i}"
        );
        let ctx = at(i as i64, sim.equity_at(candle.close));
        let (a, c) = match i {
            0 => (
                sim.submit(entree, ctx).map_err(BookRejection::Sim),
                b.submit(0, entree, ctx, None),
            ),
            2 => (
                sim.submit(exit_market(), ctx).map_err(BookRejection::Sim),
                b.submit(0, exit_market(), ctx, None),
            ),
            _ => continue,
        };
        assert_eq!(a, c);
    }
    let last = candles[3];
    let out = sim.finish(&last).unwrap();
    b.close_all_at_end(&[Some(last)]).unwrap();
    let out_book = b.into_outcome();
    assert_eq!(out.trades, out_book.trades);
    assert_eq!(out.cash.to_bits(), out_book.cash.to_bits());
    assert_eq!(out.counters, out_book.counters[0]);
}

// ---------------------------------------------------------------------------------------------
// Propriété : suites aléatoires sur trois jambes, invariants du portefeuille
// ---------------------------------------------------------------------------------------------

mod proprietes {
    use super::*;
    use proptest::prelude::*;

    const LEGS: usize = 3;

    #[derive(Clone, Debug)]
    enum Step {
        Bar {
            leg: usize,
            open: f64,
            up: f64,
            down: f64,
            close: f64,
            volume: f64,
        },
        EnterMarket {
            leg: usize,
            quote: f64,
            stop: Option<f64>,
        },
        EnterLimit {
            leg: usize,
            quote: f64,
            price: f64,
            ttl: u32,
        },
        Exit {
            leg: usize,
        },
        PlaceBuy {
            leg: usize,
            quote: f64,
            price: f64,
            slot: u16,
        },
        PlaceSell {
            leg: usize,
            quote: f64,
            price: f64,
            slot: u16,
        },
        CancelAll {
            leg: usize,
        },
        GapExit {
            leg: usize,
        },
    }

    fn steps() -> impl Strategy<Value = Vec<Step>> {
        let leg = 0..LEGS;
        let step = prop_oneof![
            4 => (leg.clone(), 50.0f64..150.0, 0.0f64..0.04, 0.0f64..0.04, 0.0f64..1.0,
                  prop_oneof![Just(0.0f64), 1.0f64..1e7])
                .prop_map(|(leg, open, up, down, close, volume)| Step::Bar {
                    leg, open, up, down, close, volume
                }),
            2 => (leg.clone(), 1.0f64..2000.0, proptest::option::of(10.0f64..140.0))
                .prop_map(|(leg, quote, stop)| Step::EnterMarket { leg, quote, stop }),
            1 => (leg.clone(), 1.0f64..2000.0, 50.0f64..150.0, 1u32..10)
                .prop_map(|(leg, quote, price, ttl)| Step::EnterLimit { leg, quote, price, ttl }),
            2 => leg.clone().prop_map(|leg| Step::Exit { leg }),
            1 => (leg.clone(), 1.0f64..2000.0, 50.0f64..150.0, 1u16..4)
                .prop_map(|(leg, quote, price, slot)| Step::PlaceBuy { leg, quote, price, slot }),
            1 => (leg.clone(), 1.0f64..2000.0, 50.0f64..150.0, 1u16..4)
                .prop_map(|(leg, quote, price, slot)| Step::PlaceSell { leg, quote, price, slot }),
            1 => leg.clone().prop_map(|leg| Step::CancelAll { leg }),
            1 => leg.prop_map(|leg| Step::GapExit { leg }),
        ];
        proptest::collection::vec(step, 1..120)
    }

    fn rejouer(mode: SizingMode, n: u16, cash: f64, steps: &[Step]) -> Result<(), TestCaseError> {
        let mut b = Book::new(config(mode, cash, n), &[instrument(0.0); LEGS]).unwrap();
        // Plafond du mode recherche : capital / N.
        let cap = (mode == SizingMode::Research).then(|| cash / f64::from(n));
        let mut index = [0i64; LEGS];
        let mut last: [Option<Candle>; LEGS] = [None; LEGS];
        for step in steps {
            match step.clone() {
                Step::Bar {
                    leg,
                    open,
                    up,
                    down,
                    close,
                    volume,
                } => {
                    let high = open * (1.0 + up);
                    let low = open * (1.0 - down);
                    let close = low + (high - low) * close;
                    let candle = Candle {
                        open_time_ms: T0 + index[leg] * MINUTE,
                        open,
                        high,
                        low,
                        close,
                        volume,
                        trades: None,
                    };
                    index[leg] += 1;
                    last[leg] = Some(candle);
                    prop_assert!(b.on_bar(leg, &candle).is_ok());
                }
                Step::GapExit { leg } => {
                    let exit_ms = T0 + index[leg] * MINUTE;
                    prop_assert!(b.force_exit_quote_gap(leg, exit_ms).is_ok());
                }
                other => {
                    let (leg, instruction) = match other {
                        Step::EnterMarket { leg, quote, stop } => (
                            leg,
                            Instruction::Enter {
                                size: SizeSpec::Quote(quote),
                                price: OrderPrice::Market,
                                stop,
                                take_profit: None,
                                tag: "e",
                            },
                        ),
                        Step::EnterLimit {
                            leg,
                            quote,
                            price,
                            ttl,
                        } => (leg, enter_limit(quote, price, ttl)),
                        Step::Exit { leg } => (leg, exit_market()),
                        // Une grille de `Place` d'achat n'est pas plafonnée au total : hors du mode
                        // recherche, où le notional cumulé est borné par le plafond de chaque entrée.
                        Step::PlaceBuy {
                            leg,
                            quote,
                            price,
                            slot,
                        } if mode == SizingMode::Compounding => {
                            (leg, place_buy(price, quote, slot))
                        }
                        Step::PlaceBuy { leg, .. } | Step::CancelAll { leg } => {
                            (leg, Instruction::CancelAll)
                        }
                        Step::PlaceSell {
                            leg,
                            quote,
                            price,
                            slot,
                        } => (
                            leg,
                            Instruction::Place {
                                side: Side::Sell,
                                price,
                                size: SizeSpec::Quote(quote),
                                ttl_bars: 50,
                                slot,
                                tag: "v",
                            },
                        ),
                        Step::Bar { .. } | Step::GapExit { .. } => continue,
                    };
                    let ctx = SubmitContext {
                        signal_ms: T0 + index[leg] * MINUTE,
                        equity: b.equity(),
                        ref_price: b.last_close(leg).unwrap_or(100.0),
                    };
                    let _ = b.submit(leg, instruction, ctx, cap);
                }
            }
            b.check_invariants();
            prop_assert!(b.cash().is_finite() && b.cash() >= 0.0);
            prop_assert!(b.quote_free() >= -1e-6, "{}", b.quote_free());
            // Une jambe peut porter un ordre en attente sans position ni entrée (sortie, vente
            // limit) : la borne de N ne vaut que pour les positions ouvertes.
            let en_position = (0..LEGS).filter(|&k| b.position(k).is_some()).count();
            prop_assert!(en_position <= usize::from(n), "{en_position} > {n}");
            let (trades, legs) = b.trades();
            prop_assert_eq!(trades.len(), legs.len());
        }
        b.close_all_at_end(&last).unwrap();
        let out = b.into_outcome();
        prop_assert_eq!(out.trades.len(), out.trade_legs.len());
        prop_assert!(out.trade_legs.iter().all(|&k| usize::from(k) < LEGS));
        prop_assert!(out.max_open_positions <= n);
        if mode == SizingMode::Research {
            prop_assert!(
                out.max_cost_to_initial <= 1.0 + 1e-9,
                "{}",
                out.max_cost_to_initial
            );
        }
        let mut ids: Vec<u64> = out.trades.iter().map(|t| t.lot_id).collect();
        ids.sort_unstable();
        let total = ids.len();
        ids.dedup();
        // Un lot vendu en plusieurs fois donne plusieurs trades de même `lot_id` : jamais deux lots
        // distincts avec le même identifiant, donc l'identifiant d'une jambe ne se retrouve pas ailleurs.
        prop_assert!(ids.len() <= total);
        for id in ids {
            let jambes: Vec<u16> = out
                .trades
                .iter()
                .zip(&out.trade_legs)
                .filter(|(t, _)| t.lot_id == id)
                .map(|(_, &k)| k)
                .collect();
            prop_assert!(
                jambes.windows(2).all(|w| w[0] == w[1]),
                "lot {id} : {jambes:?}"
            );
        }
        prop_assert!(out.cash.is_finite() && out.cash >= 0.0);
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        #[test]
        fn invariants_en_mode_recherche(n in 1u16..=3, cash in 100.0f64..1e5, s in steps()) {
            rejouer(SizingMode::Research, n, cash, &s)?;
        }

        #[test]
        fn invariants_en_mode_capitalise(n in 1u16..=3, cash in 100.0f64..1e5, s in steps()) {
            rejouer(SizingMode::Compounding, n, cash, &s)?;
        }
    }
}
