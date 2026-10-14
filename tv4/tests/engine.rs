//! Tests d'intégration du moteur S3 sur des bougies écrites à la main ou synthétiques
//! (docs/03-architecture.md §7.2 et §7.5).

mod common;

use common::{T0, okx_fees, slippage};
use tv4::engine::{CellConfig, CellResult, EngineError, run_cell};
use tv4::model::{Candle, Side, Timeframe};
use tv4::sim::{ExitReason, SimRules};
use tv4::strategy::{BarContext, IndicatorKind, IndicatorSpec, OrderKind, Signal, Size, Strategy};

const MINUTE: i64 = 60_000;

/// Stratégie de test : émet les signaux du plan à l'appel numéro `calls`.
struct Scripted {
    plan: Vec<(usize, Signal)>,
    calls: usize,
    timeframes: Vec<Timeframe>,
    specs: Vec<IndicatorSpec>,
    warmup: usize,
}

impl Scripted {
    fn new(plan: Vec<(usize, Signal)>) -> Scripted {
        Scripted {
            plan,
            calls: 0,
            timeframes: vec![Timeframe::M1],
            specs: Vec::new(),
            warmup: 0,
        }
    }
}

impl Strategy for Scripted {
    fn id(&self) -> &'static str {
        "scripted"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &self.timeframes
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        self.specs.clone()
    }
    fn warmup_bars(&self) -> usize {
        self.warmup
    }
    fn on_bar(&mut self, _ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        for (call, signal) in &self.plan {
            if *call == self.calls {
                out.push(*signal);
            }
        }
        self.calls += 1;
    }
}

fn enter(stop: f64) -> Signal {
    Signal::EnterLong {
        size: Size::RiskPct(0.01),
        kind: OrderKind::Market,
        stop: Some(stop),
        take_profit: None,
        tag: "entree",
    }
}

fn exit() -> Signal {
    Signal::ExitLong {
        kind: OrderKind::Market,
        tag: "sortie",
    }
}

/// Cinq bougies 1m inventées (pas des données de marché), valeurs du jeu « main » de la spécification.
fn hand_candles() -> Vec<Candle> {
    let rows = [
        (99.0, 100.0, 98.0, 99.5),
        (100.0, 102.0, 99.0, 101.0),
        (105.0, 106.0, 104.0, 105.0),
        (110.0, 111.0, 109.0, 110.0),
        (110.0, 111.0, 109.0, 110.0),
    ];
    rows.iter()
        .enumerate()
        .map(|(i, &(open, high, low, close))| Candle {
            open_time_ms: T0 + i as i64 * MINUTE,
            open,
            high,
            low,
            close,
            volume: 1.0,
            trades: None,
        })
        .collect()
}

fn config(cash: f64) -> CellConfig<'static> {
    CellConfig {
        symbol: "TEST-USD",
        fees: okx_fees(),
        slippage: slippage(10.0),
        initial_cash: cash,
        sim_rules: SimRules::default(),
    }
}

fn run(plan: Vec<(usize, Signal)>, candles: &[Candle]) -> CellResult {
    run_cell(candles, Box::new(Scripted::new(plan)), &config(1000.0)).expect("run valide")
}

fn close_to(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

#[test]
fn frais_et_slippage_exacts_sur_un_cas_a_la_main() {
    let result = run(vec![(0, enter(89.5)), (2, exit())], &hand_candles());
    assert_eq!(result.trades.len(), 1);
    let t = result.trades[0];
    close_to(t.qty, 1.0);
    close_to(t.entry_price, 100.1);
    close_to(t.entry_fee, 0.35035);
    close_to(t.exit_price, 109.89);
    close_to(t.exit_fee, 0.384615);
    close_to(t.pnl_net, 9.055035);
    close_to(result.cash_final, 1009.055035);
    close_to(result.fees_total, 0.734965);
    close_to(result.slippage_total, 0.21);
    close_to(result.pnl_net, result.cash_final - 1000.0);
    assert_eq!(t.exit_reason, ExitReason::Signal);
    assert!(!t.capped_by_cash);
    let attendu = [1000.0, 1000.54965, 1004.54965, 1009.055035, 1009.055035];
    assert_eq!(result.equity.len(), attendu.len());
    for (obtenu, attendu) in result.equity.iter().zip(attendu) {
        close_to(*obtenu, attendu);
    }
}

#[test]
fn le_fill_est_a_l_open_suivant_jamais_au_close() {
    let candles = hand_candles();
    let result = run(vec![(0, enter(89.5)), (2, exit())], &candles);
    let t = result.trades[0];
    // Signal émis à la clôture de b0, fill à l'open de b1.
    assert_eq!(t.entry_signal_ms, T0 + MINUTE);
    assert_eq!(t.entry_ms, T0 + MINUTE);
    assert_eq!(t.entry_ref_price, 100.0, "open de b1");
    assert_ne!(t.entry_ref_price, candles[0].close);
    assert_ne!(t.entry_price, candles[0].close * 1.001);
    assert!(t.entry_ref_price >= candles[1].low && t.entry_ref_price <= candles[1].high);
    // Sortie : signal à la clôture de b2, fill à l'open de b3.
    assert_eq!(t.exit_signal_ms, T0 + 3 * MINUTE);
    assert_eq!(t.exit_ms, T0 + 3 * MINUTE);
    assert_eq!(t.exit_ref_price, 110.0, "open de b3");
    assert!(t.exit_ref_price >= candles[3].low && t.exit_ref_price <= candles[3].high);
    for trade in &result.trades {
        assert!(trade.entry_ms >= trade.entry_signal_ms);
        assert!(trade.exit_ms >= trade.exit_signal_ms);
    }
}

#[test]
fn signal_sur_la_derniere_bougie_aucun_fill() {
    let result = run(vec![(4, enter(100.0))], &hand_candles());
    assert!(result.trades.is_empty());
    assert_eq!(result.sim.orders_unfilled_at_end, 1);
    assert_eq!(result.cash_final, 1000.0);
    assert_eq!(result.signals_accepted, 1);
}

#[test]
fn pas_de_short() {
    let result = run(vec![(0, exit()), (1, exit())], &hand_candles());
    assert_eq!(result.rejections.exit_without_position, 2);
    assert!(result.trades.is_empty());
    assert_eq!(result.qty_final, 0.0);
    assert_eq!(result.cash_final, 1000.0);
}

#[test]
fn cash_jamais_negatif() {
    // Risque de 100 % de l'equity avec un stop à 0,1 du prix : la quantité demandée est énorme.
    let signal = Signal::EnterLong {
        size: Size::RiskPct(1.0),
        kind: OrderKind::Market,
        stop: Some(99.4),
        take_profit: None,
        tag: "gros",
    };
    let result = run(vec![(0, signal)], &hand_candles());
    assert_eq!(result.sim.entries_capped_by_cash, 1);
    assert!(result.cash_final >= 0.0);
    assert!(result.equity.iter().all(|e| e.is_finite() && *e > 0.0));
    assert!(result.trades[0].capped_by_cash);
}

#[test]
fn cloture_forcee_comptee() {
    let result = run(vec![(0, enter(89.5))], &hand_candles());
    assert!(result.forced_close);
    assert_eq!(result.trades.len(), 1);
    assert_eq!(result.n_trades_forced, 1);
    assert_eq!(result.pnl_realized, 0.0);
    assert_eq!(result.pnl_marked, result.trades[0].pnl_net);
    let t = result.trades[0];
    assert_eq!(t.exit_reason, ExitReason::ForcedClose);
    assert_eq!(t.exit_ref_price, 110.0, "close de b4");
    assert_eq!(result.qty_final, 0.0);
    assert_eq!(result.equity_final, result.cash_final);
}

#[test]
fn signaux_non_geres_rejetes_et_comptes() {
    let plan = vec![
        // limit_order_unsupported
        (
            0,
            Signal::EnterLong {
                size: Size::RiskPct(0.01),
                kind: OrderKind::Limit {
                    price: 99.0,
                    ttl_bars: 3,
                },
                stop: Some(95.0),
                take_profit: None,
                tag: "t",
            },
        ),
        // take_profit_unsupported
        (
            0,
            Signal::EnterLong {
                size: Size::RiskPct(0.01),
                kind: OrderKind::Market,
                stop: Some(95.0),
                take_profit: Some(120.0),
                tag: "t",
            },
        ),
        // size_unsupported
        (
            0,
            Signal::EnterLong {
                size: Size::PctEquity(0.2),
                kind: OrderKind::Market,
                stop: Some(95.0),
                take_profit: None,
                tag: "t",
            },
        ),
        // risk_without_stop
        (
            0,
            Signal::EnterLong {
                size: Size::RiskPct(0.01),
                kind: OrderKind::Market,
                stop: None,
                take_profit: None,
                tag: "t",
            },
        ),
        // invalid_risk_sizing : stop au-dessus du close de b0 (99,5)
        (0, enter(150.0)),
        (0, Signal::MoveStop { stop: 95.0 }),
        (0, Signal::CancelPending),
        (
            0,
            Signal::PlaceLimit {
                side: Side::Buy,
                price: 99.0,
                size: Size::Quote(10.0),
                ttl_bars: 2,
                slot: 1,
                tag: "t",
            },
        ),
        (0, Signal::CancelSlot { slot: 1 }),
    ];
    let result = run(plan, &hand_candles());
    let r = result.rejections;
    assert_eq!(r.limit_order_unsupported, 1);
    assert_eq!(r.take_profit_unsupported, 1);
    assert_eq!(r.size_unsupported, 1);
    assert_eq!(r.risk_without_stop, 1);
    assert_eq!(r.invalid_risk_sizing, 1);
    assert_eq!(r.move_stop_unsupported, 1);
    assert_eq!(r.cancel_pending_unsupported, 1);
    assert_eq!(r.place_limit_unsupported, 1);
    assert_eq!(r.cancel_slot_unsupported, 1);
    assert_eq!(r.total(), 9);
    assert!(result.trades.is_empty());
    assert_eq!(result.signals_emitted, 9);
    assert_eq!(result.signals_emitted, result.signals_accepted + r.total());
}

#[test]
fn sortie_limit_et_rejets_du_simulateur_comptes() {
    let plan = vec![
        (
            0,
            Signal::ExitLong {
                kind: OrderKind::Limit {
                    price: 101.0,
                    ttl_bars: 2,
                },
                tag: "t",
            },
        ),
        // Deux entrées sur la même bougie : la seconde est refusée par le simulateur.
        (0, enter(89.5)),
        (0, enter(89.5)),
        // Entrée alors que la position est longue.
        (2, enter(89.5)),
        // Deux sorties : la seconde est un doublon.
        (3, exit()),
        (3, exit()),
    ];
    let result = run(plan, &hand_candles());
    let r = result.rejections;
    assert_eq!(r.limit_order_unsupported, 1);
    assert_eq!(r.entry_while_long, 2);
    assert_eq!(r.duplicate_exit, 1);
    assert_eq!(result.signals_emitted, result.signals_accepted + r.total());
}

#[test]
fn regle_mono_tf() {
    let candles = common::synthetic_candles(400, 1, T0, 0.0);
    let strategy = common::strategy_from_file("sma_cross", "config/strategies/sma_cross.toml");
    let erreur = run_cell(&candles, strategy, &config(300.0)).unwrap_err();
    assert!(
        matches!(erreur, EngineError::UnsupportedTimeframes { .. }),
        "{erreur:?}"
    );
    let message = erreur.to_string();
    assert!(message.contains("S5"), "{message}");
    assert!(message.contains("sma_cross_1m.toml"), "{message}");
    assert!(message.contains("15m"), "{message}");

    let mut deux = Scripted::new(Vec::new());
    deux.timeframes = vec![Timeframe::M1, Timeframe::M15];
    let erreur = run_cell(&candles, Box::new(deux), &config(300.0)).unwrap_err();
    assert!(
        matches!(erreur, EngineError::UnsupportedTimeframes { .. }),
        "{erreur:?}"
    );
}

#[test]
fn donnees_refusees() {
    let neuf = || Box::new(Scripted::new(Vec::new()));
    let cfg = config(300.0);
    assert!(matches!(
        run_cell(&[], neuf(), &cfg),
        Err(EngineError::EmptyData)
    ));

    let mut doublon = hand_candles();
    doublon[2].open_time_ms = doublon[1].open_time_ms;
    assert!(matches!(
        run_cell(&doublon, neuf(), &cfg),
        Err(EngineError::NotIncreasing { index: 2, .. })
    ));

    let mut desordre = hand_candles();
    desordre.swap(1, 3);
    assert!(matches!(
        run_cell(&desordre, neuf(), &cfg),
        Err(EngineError::NotIncreasing { .. })
    ));

    let mut decale = hand_candles();
    decale[1].open_time_ms += 1_000;
    assert!(matches!(
        run_cell(&decale, neuf(), &cfg),
        Err(EngineError::NotOneMinute { .. })
    ));

    let mut invalide = hand_candles();
    invalide[0].high = 1.0;
    assert!(matches!(
        run_cell(&invalide, neuf(), &cfg),
        Err(EngineError::Model(_))
    ));
}

#[test]
fn warmup() {
    let candles = common::synthetic_candles(20, 1, T0, 0.0);
    let cfg = config(300.0);

    let mut court = Scripted::new(Vec::new());
    court.specs = vec![IndicatorSpec {
        name: "sma10",
        tf: Timeframe::M1,
        kind: IndicatorKind::Sma { period: 10 },
    }];
    court.warmup = 5;
    let erreur = run_cell(&candles, Box::new(court), &cfg).unwrap_err();
    assert!(
        matches!(
            erreur,
            EngineError::WarmupTooShort {
                name: "sma10",
                index: 5
            }
        ),
        "{erreur:?}"
    );

    let mut trop_long = Scripted::new(Vec::new());
    trop_long.warmup = 20;
    assert!(matches!(
        run_cell(&candles, Box::new(trop_long), &cfg),
        Err(EngineError::NotEnoughBars {
            warmup_bars: 20,
            bars: 20
        })
    ));
}

#[test]
fn capital_invalide() {
    for cash in [0.0, -1.0, f64::NAN] {
        let erreur = run_cell(
            &hand_candles(),
            Box::new(Scripted::new(Vec::new())),
            &config(cash),
        )
        .unwrap_err();
        assert!(matches!(erreur, EngineError::Sim(_)), "{erreur:?}");
    }
}

#[test]
fn trous_toleres() {
    let candles = common::synthetic_candles(2_000, 3, T0, 0.05);
    assert!(
        candles
            .windows(2)
            .any(|w| w[1].open_time_ms - w[0].open_time_ms > MINUTE),
        "le jeu de test doit contenir des trous"
    );
    let result = run_cell(&candles, common::sma_cross_1m(), &config(300.0)).unwrap();
    assert_eq!(result.start_index, 240);
    assert_eq!(result.equity.len(), candles.len() - result.start_index);
    assert_eq!(result.bars_in_window, result.equity.len());
}
