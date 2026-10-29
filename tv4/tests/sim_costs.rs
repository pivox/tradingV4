//! Prix de chaque fill sous un `LegCost` de la génération s10 (docs/specs/s10.md §5.2 et §6).
//!
//! Toutes les bougies et tous les paramètres de coût sont inventés pour le test (marché plat à 100, valeurs
//! rondes), jamais des données de marché : seules les formules sont en jeu. Les tirages attendus sont
//! recalculés ici par `model::rng`, jamais lus dans le simulateur.

use tv4::model::costs::{EntryMode, LegCost, StopCost, StopSlippage, StopSlippageBps};
use tv4::model::rng::{self, STREAM_STOP_SLIPPAGE, STREAM_STOP_STRESS};
use tv4::model::{Candle, ExchangeId, FeeSchedule, InstrumentRules, Side, Slippage};
use tv4::report::writers::{trades_csv_legs, trades_csv_legs_s10};
use tv4::sim::{
    Book, BookConfig, ExitReason, Instruction, Liquidity, OrderPrice, SimError, SimRules,
    Simulator, SizeSpec, SizingMode, SubmitContext, Trade,
};

const T0: i64 = 1_704_067_200_000;
const MINUTE: i64 = 60_000;
const SYMBOL: &str = "TEST-USD";

/// Demi-spread de 5 bp, slippage market de 3 bp : valeurs rondes de test.
const H: f64 = 0.0005;
const S: f64 = 0.0003;

fn fees() -> FeeSchedule {
    FeeSchedule::new(ExchangeId::Okx, 20, 35).expect("grille valide")
}

fn instrument() -> InstrumentRules {
    InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).expect("instrument valide")
}

/// Fill à l'open de la 1m qui suit le signal, aucune limite de volume.
fn rules() -> SimRules {
    SimRules {
        activation_delay_bars: 0,
        max_volume_share: 1e9,
        ..SimRules::default()
    }
}

fn dist() -> StopSlippage {
    StopSlippage::from_quantiles(StopSlippageBps {
        p50: 5.0,
        p95: 50.0,
        p99: 200.0,
        p100: 200.0,
    })
}

fn cost(stress_probability: f64, seed: u64) -> LegCost {
    LegCost {
        half_spread_rate: H,
        market_slippage_rate: S,
        stop: StopCost::Drawn {
            dist: dist(),
            stress_probability,
            stress_drop: 0.03,
        },
        entry: EntryMode::Taker,
        adverse_selection_rate: 0.0,
        leg_key: rng::leg_key(SYMBOL),
        seed,
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

/// Contexte d'un signal émis à la clôture de la 1m `i`.
fn at(sim: &Simulator, i: i64, close: f64) -> SubmitContext {
    SubmitContext {
        signal_ms: T0 + (i + 1) * MINUTE,
        equity: sim.equity_at(close),
        ref_price: close,
    }
}

fn enter_market(quote: f64, stop: Option<f64>) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Market,
        stop,
        take_profit: None,
        tag: "entree",
    }
}

fn enter_limit(quote: f64, price: f64) -> Instruction {
    Instruction::Enter {
        size: SizeSpec::Quote(quote),
        price: OrderPrice::Limit {
            price,
            ttl_bars: 20,
        },
        stop: None,
        take_profit: None,
        tag: "entree",
    }
}

fn exit_market() -> Instruction {
    Instruction::Exit {
        price: OrderPrice::Market,
        tag: "sortie",
    }
}

fn close_to(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-9 * a.abs().max(1.0), "{a} != {b}");
}

/// Entrée market à la 1m 1 sur marché plat, puis la suite de bougies donnée (à partir de l'indice 2).
fn run_market_entry(cost: LegCost, stop: Option<f64>, rest: &[Candle]) -> Simulator {
    let mut sim = sim(cost);
    sim.on_bar(&flat(0)).expect("bougie 0");
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0, stop), ctx).expect("entrée");
    sim.on_bar(&flat(1)).expect("bougie 1");
    for candle in rest {
        sim.on_bar(candle).expect("bougie");
    }
    sim
}

fn only_trade(sim: Simulator, last: &Candle) -> Trade {
    let out = sim.finish(last).expect("fin de run");
    assert_eq!(out.trades.len(), 1);
    out.trades[0]
}

#[test]
fn achat_et_vente_market_au_taux_compose() {
    let mut sim = sim(cost(0.0, 1));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0, None), ctx).unwrap();
    sim.on_bar(&flat(1)).unwrap();
    let ctx = at(&sim, 1, 100.0);
    sim.submit(exit_market(), ctx).unwrap();
    sim.on_bar(&flat(2)).unwrap();
    let out = sim.finish(&flat(2)).unwrap();
    assert_eq!(out.trades.len(), 1);
    let t = out.trades[0];

    let rate = H + S;
    assert_eq!(t.entry_price, 100.0 * (1.0 + rate));
    assert_eq!(t.exit_price, 100.0 * (1.0 - rate));
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    assert_eq!(t.exit_liquidity, Liquidity::Taker);
    assert_eq!(t.entry_ref_price, 100.0);
    assert_eq!(t.exit_ref_price, 100.0);
    assert_eq!(t.entry_fee, t.qty * t.entry_price * 0.0035);
    assert_eq!(t.exit_fee, t.qty * t.exit_price * 0.0035);

    // Décomposition : slippage + spread = impact, spread = qty × open × h (l'impact le dépasse).
    let entry_impact = t.qty * (t.entry_price - 100.0);
    let exit_impact = t.qty * (100.0 - t.exit_price);
    close_to(t.entry_slippage + t.entry_spread, entry_impact);
    close_to(t.exit_slippage + t.exit_spread, exit_impact);
    close_to(t.entry_spread, t.qty * 100.0 * H);
    close_to(t.exit_spread, t.qty * 100.0 * H);
    assert!(t.entry_slippage > 0.0 && t.exit_slippage > 0.0);
    // PnL : produit moins frais moins coût, le coût de marché est déjà dans les prix.
    close_to(
        t.pnl_net,
        (t.qty * t.exit_price - t.exit_fee) - (t.qty * t.entry_price + t.entry_fee),
    );
}

#[test]
fn les_compteurs_de_spread_somment_les_parts_de_spread_des_fills_taker() {
    let mut sim = sim(cost(0.0, 1));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0, None), ctx).unwrap();
    sim.on_bar(&flat(1)).unwrap();
    let ctx = at(&sim, 1, 100.0);
    sim.submit(exit_market(), ctx).unwrap();
    sim.on_bar(&flat(2)).unwrap();
    let counters = *sim.cost_counters();
    let out = sim.finish(&flat(2)).unwrap();
    let t = out.trades[0];
    close_to(counters.spread_cost_total, t.entry_spread + t.exit_spread);
    assert_eq!(counters.adverse_selection_total, 0.0);
    assert_eq!(counters.stop_fills_drawn, 0);
    assert_eq!(
        counters.maker_fill_draws, 0,
        "aucun tirage hors stop et profil maker"
    );
}

#[test]
fn limit_marketable_plafonnee_par_sa_limite() {
    let mut sim = sim(cost(0.0, 1));
    sim.on_bar(&flat(0)).unwrap();
    // Limite 100,0 ; l'open suivant à 99,98 est sous la limite : marketable à l'arrivée. Le prix
    // composé 99,98 × 1,0008 dépasse la limite : il est plafonné à `L`.
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_limit(500.0, 100.0), ctx).unwrap();
    sim.on_bar(&bar(1, 99.98, 100.2, 99.9, 100.0)).unwrap();
    let t = only_trade(sim, &flat(1));
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    assert_eq!(t.entry_price, 100.0);
    assert_eq!(t.entry_ref_price, 99.98);
    // Impact `qty × (L − open)` plus petit que `qty × open × h` : tout est du spread.
    close_to(t.entry_spread, t.qty * (100.0 - 99.98));
    assert_eq!(t.entry_slippage, 0.0);
}

#[test]
fn limit_marketable_sous_le_plafond_paie_le_taux_compose() {
    let mut sim = sim(cost(0.0, 1));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_limit(500.0, 100.5), ctx).unwrap();
    sim.on_bar(&bar(1, 99.0, 100.2, 98.9, 100.0)).unwrap();
    let t = only_trade(sim, &flat(1));
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    assert_eq!(t.entry_price, 99.0 * (1.0 + (H + S)));
    close_to(t.entry_spread, t.qty * 99.0 * H);
}

#[test]
fn limit_au_repos_remplie_a_l_exactement_sans_spread_meme_a_travers_un_gap() {
    // Limite 99,0 sous l'open d'arrivée (100,0) : au repos. La 1m 1 ne la traverse pas ; la 1m 2 la
    // traverse, au premier cas à l'intérieur de la bougie, au second par un gap qui place toute la 1m sous
    // `L` : le prix reste `L`.
    for (open, high, low) in [(100.0, 100.0, 98.0), (97.0, 97.5, 96.5)] {
        let mut sim = sim(cost(0.0, 1));
        sim.on_bar(&flat(0)).unwrap();
        let ctx = at(&sim, 0, 100.0);
        sim.submit(enter_limit(500.0, 99.0), ctx).unwrap();
        sim.on_bar(&bar(1, 100.0, 100.0, 99.5, 99.6)).unwrap();
        assert_eq!(sim.pending_len(), 1, "pas encore traversée");
        sim.on_bar(&bar(2, open, high, low, 98.5)).unwrap();
        assert_eq!(sim.pending_len(), 0);
        let counters = *sim.cost_counters();
        let t = only_trade(sim, &flat(2));
        assert_eq!(t.entry_liquidity, Liquidity::Maker);
        assert_eq!(t.entry_price, 99.0);
        assert_eq!(t.entry_ref_price, 99.0);
        assert_eq!(t.entry_slippage, 0.0);
        assert_eq!(
            t.entry_spread, 0.0,
            "pas de spread sur un fill au repos baseline"
        );
        assert_eq!(t.entry_fee, t.qty * 99.0 * 0.0020);
        assert_eq!(
            counters.maker_fill_draws, 0,
            "baseline : aucun tirage de fill"
        );
        assert_eq!(counters.adverse_selection_total, 0.0);
    }
}

/// Prix de stop attendu, recalculé par `model::rng` (les mêmes mots que s10 §5.2).
fn expected_stop_price(c: &LegCost, stop: f64, open: f64, open_time_ms: i64) -> (f64, bool) {
    let StopCost::Drawn {
        dist,
        stress_probability,
        stress_drop,
    } = c.stop
    else {
        panic!("stop tiré attendu");
    };
    let ts = rng::ts_word(open_time_ms);
    let u1 = rng::unit(rng::draw(c.seed, [c.leg_key, STREAM_STOP_SLIPPAGE, ts, 0]));
    let u2 = rng::unit(rng::draw(c.seed, [c.leg_key, STREAM_STOP_STRESS, ts, 0]));
    let reference = stop.min(open);
    let rate = c.half_spread_rate + dist.quantile_bps(u1) / 10_000.0;
    let mut price = reference * (1.0 - rate);
    let stressed = u2 < stress_probability;
    if stressed {
        price = price.min(stop * (1.0 - stress_drop));
    }
    (price, stressed)
}

#[test]
fn stop_tire_au_prix_recalcule_par_le_rng() {
    for p in [0.0, 0.5, 1.0] {
        for seed in 1..=40u64 {
            let c = cost(p, seed);
            // Stop à 95 ; la 1m 3 traverse le stop à l'intérieur de la bougie (open 100).
            let hit = bar(3, 100.0, 100.0, 94.0, 96.0);
            let sim = run_market_entry(c, Some(95.0), &[flat(2), hit]);
            let counters_before = *sim.cost_counters();
            assert_eq!(counters_before.stop_fills_drawn, 1);
            let t = only_trade(sim, &hit);
            assert_eq!(t.exit_reason, ExitReason::Stop);
            assert_eq!(t.exit_liquidity, Liquidity::Taker);
            let (price, stressed) = expected_stop_price(&c, 95.0, 100.0, hit.open_time_ms);
            assert_eq!(
                t.exit_price.to_bits(),
                price.to_bits(),
                "p {p} graine {seed}"
            );
            assert_eq!(t.exit_ref_price, 95.0);
            assert_eq!(counters_before.stop_stress_fills, u64::from(stressed));
            // Jamais meilleur que `ref × (1 − h)` : le slippage de stop n'est jamais favorable.
            assert!(t.exit_price <= 95.0 * (1.0 - H));
            // Décomposition : spread = qty × ref × h, slippage = reste.
            let impact = t.qty * (95.0 - t.exit_price);
            close_to(t.exit_slippage + t.exit_spread, impact);
            close_to(t.exit_spread, t.qty * 95.0 * H);
        }
    }
}

#[test]
fn le_stress_prend_le_pire_des_deux_prix() {
    // Probabilité 1 : le prix de stress `stop × (1 − 3 %)` est toujours atteint, il est plus bas que le
    // pire slippage de la loi (h + 200 bp = 2,05 %).
    let c = cost(1.0, 1);
    let hit = bar(3, 100.0, 100.0, 94.0, 96.0);
    let sim = run_market_entry(c, Some(95.0), &[flat(2), hit]);
    assert_eq!(sim.cost_counters().stop_stress_fills, 1);
    let t = only_trade(sim, &hit);
    assert_eq!(t.exit_price, 95.0 * (1.0 - 0.03));

    // Gap sous le stop : la référence est l'open (90), déjà sous le prix de stress (92,15) : le min
    // garde le prix du slippage tiré, plus bas.
    let gap = bar(3, 90.0, 91.0, 89.0, 90.0);
    let sim = run_market_entry(c, Some(95.0), &[flat(2), gap]);
    let t = only_trade(sim, &gap);
    let (price, stressed) = expected_stop_price(&c, 95.0, 90.0, gap.open_time_ms);
    assert!(stressed);
    assert_eq!(t.exit_price.to_bits(), price.to_bits());
    assert_eq!(t.exit_ref_price, 90.0);
    assert!(t.exit_price < 95.0 * 0.97);
    assert!(t.exit_price <= 90.0 * (1.0 - H));
}

#[test]
fn deux_graines_donnent_au_moins_un_prix_de_stop_different() {
    let price_of = |seed: u64, minute: i64| {
        let c = cost(0.0, seed);
        let hit = bar(minute, 100.0, 100.0, 94.0, 96.0);
        // Entrée à la 1m 1, stop touché à la minute `minute` (le prix tiré dépend de l'instant).
        let mut rest: Vec<Candle> = (2..minute).map(flat).collect();
        rest.push(hit);
        only_trade(run_market_entry(c, Some(95.0), &rest), &hit).exit_price
    };
    let mut differing = 0;
    for minute in 3..40 {
        let a = price_of(1, minute);
        assert_eq!(
            a.to_bits(),
            price_of(1, minute).to_bits(),
            "même graine, même prix"
        );
        if a.to_bits() != price_of(2, minute).to_bits() {
            differing += 1;
        }
    }
    assert!(differing > 0, "deux graines doivent changer des prix");
}

#[test]
fn fin_de_fenetre_et_trou_de_cotation_au_taux_compose() {
    let rate = H + S;
    // Fin de fenêtre : close × (1 − (h + s)), règle de volume ignorée, pas de tirage.
    let last = bar(3, 100.0, 101.0, 99.0, 100.5);
    let sim = run_market_entry(cost(1.0, 1), None, &[flat(2), last]);
    assert_eq!(sim.cost_counters().stop_fills_drawn, 0);
    let t = only_trade(sim, &last);
    assert_eq!(t.exit_reason, ExitReason::ForcedClose);
    assert_eq!(t.exit_price, 100.5 * (1.0 - rate));
    close_to(t.exit_spread, t.qty * 100.5 * H);
    close_to(
        t.exit_slippage + t.exit_spread,
        t.qty * (100.5 - t.exit_price),
    );

    // Trou de cotation : dernier close × (1 − (h + s)), sans stress (ce n'est pas un stop).
    let mut sim = run_market_entry(cost(1.0, 1), None, &[flat(2), last]);
    assert!(sim.force_exit_quote_gap(T0 + 10 * MINUTE).unwrap());
    assert_eq!(sim.cost_counters().stop_stress_fills, 0);
    let t = only_trade(sim, &last);
    assert_eq!(t.exit_reason, ExitReason::QuoteGap);
    assert_eq!(t.exit_price, 100.5 * (1.0 - rate));
    assert_eq!(t.exit_ref_price, 100.5);
}

#[test]
fn le_sizing_au_risque_se_fait_sur_le_prix_de_fill() {
    // Un coût plus haut change la quantité : `RiskPct` est dimensionné sur `open × (1 + (h + s))`.
    let size = |c: LegCost| {
        let mut sim = sim(c);
        sim.on_bar(&flat(0)).unwrap();
        let ctx = at(&sim, 0, 100.0);
        let instruction = Instruction::Enter {
            size: SizeSpec::RiskPct(0.01),
            price: OrderPrice::Market,
            stop: Some(95.0),
            take_profit: None,
            tag: "entree",
        };
        sim.submit(instruction, ctx).unwrap();
        sim.on_bar(&flat(1)).unwrap();
        sim.qty()
    };
    let cheap = size(LegCost {
        half_spread_rate: 0.0,
        market_slippage_rate: 0.0,
        ..cost(0.0, 1)
    });
    let dear = size(LegCost {
        half_spread_rate: 0.002,
        market_slippage_rate: 0.002,
        ..cost(0.0, 1)
    });
    assert!(dear < cheap, "{dear} < {cheap}");
}

#[test]
fn with_cost_refuse_un_stress_de_regles_avec_un_stop_tire() {
    let bad_rules = SimRules {
        stop_stress_bps: 10.0,
        ..rules()
    };
    let result = Simulator::with_cost(fees(), cost(0.0, 1), bad_rules, instrument(), 1_000.0, 4);
    assert!(matches!(result, Err(SimError::InvalidRules { .. })));
    // Le chemin hérité garde son stress de règles dans le taux fixe.
    let legacy = LegCost::legacy(Slippage::from_bps(1.0).unwrap(), 10.0);
    assert!(Simulator::with_cost(fees(), legacy, bad_rules, instrument(), 1_000.0, 4).is_ok());
}

fn book_config() -> BookConfig {
    BookConfig {
        fees: fees(),
        slippage: Slippage::ZERO,
        rules: rules(),
        initial_cash: 1_000.0,
        max_positions: 2,
        mode: SizingMode::Compounding,
        min_order_bump_max_risk: None,
        trades_capacity: 8,
    }
}

#[test]
fn le_portefeuille_porte_un_cout_par_jambe() {
    let wide = LegCost {
        half_spread_rate: 0.003,
        ..cost(0.0, 1)
    };
    let tight = cost(0.0, 1);
    let mut book =
        Book::with_costs(book_config(), &[instrument(), instrument()], &[wide, tight]).unwrap();
    for leg in 0..2 {
        book.on_bar(leg, &flat(0)).unwrap();
        let ctx = SubmitContext {
            signal_ms: T0 + MINUTE,
            equity: book.equity(),
            ref_price: 100.0,
        };
        book.submit(leg, enter_market(200.0, None), ctx, None)
            .unwrap();
        book.on_bar(leg, &flat(1)).unwrap();
    }
    let last = [Some(flat(1)), Some(flat(1))];
    book.close_all_at_end(&last).unwrap();
    let out = book.into_outcome();
    assert_eq!(out.cost_by_leg.len(), 2);
    assert_eq!(out.trades.len(), 2);
    let price_of = |leg: u16| {
        let k = out.trade_legs.iter().position(|&l| l == leg).unwrap();
        out.trades[k].entry_price
    };
    assert_eq!(price_of(0), 100.0 * (1.0 + (0.003 + S)));
    assert_eq!(price_of(1), 100.0 * (1.0 + (H + S)));
    assert!(out.cost_by_leg[0].spread_cost_total > out.cost_by_leg[1].spread_cost_total);
}

#[test]
fn with_costs_refuse_un_nombre_de_couts_faux_et_un_slippage_scalaire() {
    let one = [cost(0.0, 1)];
    assert!(matches!(
        Book::with_costs(book_config(), &[instrument(), instrument()], &one),
        Err(SimError::InvalidRules { .. })
    ));
    let config = BookConfig {
        slippage: Slippage::from_bps(1.0).unwrap(),
        ..book_config()
    };
    assert!(matches!(
        Book::with_costs(config, &[instrument()], &one),
        Err(SimError::InvalidRules { .. })
    ));
    assert!(Book::with_costs(book_config(), &[instrument()], &one).is_ok());
    let _ = Side::Buy;
}

#[test]
fn spread_cost_s10_somme_les_deux_parts_et_s8_reste_vide() {
    let mut sim = sim(cost(0.0, 1));
    sim.on_bar(&flat(0)).unwrap();
    let ctx = at(&sim, 0, 100.0);
    sim.submit(enter_market(500.0, None), ctx).unwrap();
    sim.on_bar(&flat(1)).unwrap();
    let ctx = at(&sim, 1, 100.0);
    sim.submit(exit_market(), ctx).unwrap();
    sim.on_bar(&flat(2)).unwrap();
    let out = sim.finish(&flat(2)).unwrap();
    let legs = [0u16];
    let symbols = [SYMBOL];

    let s10 =
        String::from_utf8(trades_csv_legs_s10(&out.trades, &legs, &symbols, 3 * MINUTE).unwrap())
            .unwrap();
    let old = String::from_utf8(trades_csv_legs(&out.trades, &legs, &symbols, 3 * MINUTE).unwrap())
        .unwrap();
    let row = |text: &str| -> Vec<String> {
        text.lines()
            .nth(1)
            .unwrap()
            .split(',')
            .map(str::to_string)
            .collect()
    };
    let (a, b) = (row(&s10), row(&old));
    assert_eq!(a.len(), 36);
    assert_eq!(
        a[..35],
        b[..35],
        "les 35 autres colonnes sont celles de s9, à l'octet"
    );
    assert_eq!(b[35], "", "s8 et s9 : colonne vide");
    let t = out.trades[0];
    assert_eq!(a[35], (t.entry_spread + t.exit_spread).to_string());
    assert!(t.entry_spread + t.exit_spread > 0.0);
    // L'en-tête est identique.
    assert_eq!(s10.lines().next(), old.lines().next());
}
