//! Tests d'intégration du moteur sur des bougies écrites à la main ou synthétiques
//! (docs/03-architecture.md §7.2 et §7.5).
//!
//! Les nombres à la main de S3 sont conservés grâce à des règles réglées « à la S3 » : activation
//! sans délai (fill à l'open de la 1m suivante), aucune limite de volume et une taille `Quote(100.1)`
//! qui donne exactement une unité à 100.1 (open 100 + 10 bps). Le chemin par défaut (délai d'une
//! minute, règle de volume) est couvert par `tests/sim_timing.rs` et par les tests du simulateur.

mod common;

use std::sync::{Arc, Mutex};

use common::{T0, okx_fees, slippage};
use tv4::engine::{CellConfig, CellResult, EngineError, run_cell, run_cell_mtf};
use tv4::model::{Candle, InstrumentRules, Side, Timeframe};
use tv4::mtf::Mtf;
use tv4::sim::{ExitReason, SimRules};
use tv4::strategy::{BarContext, IndicatorKind, IndicatorSpec, OrderKind, Signal, Size, Strategy};

const MINUTE: i64 = 60_000;

/// Ce que la stratégie a vu à un appel, pour chaque timeframe déclaré (dans l'ordre de déclaration).
#[derive(Clone, Debug)]
struct Appel {
    now_ms: i64,
    /// `ctx.just_closed(tf)`.
    vient_de_clore: Vec<bool>,
    /// Nombre de bougies visibles.
    visibles: Vec<usize>,
    /// `open_time` de la dernière bougie visible.
    derniere_ouverture: Vec<Option<i64>>,
}

/// Stratégie de test : émet les signaux du plan à l'appel numéro `calls`.
struct Scripted {
    plan: Vec<(usize, Signal)>,
    calls: usize,
    timeframes: Vec<Timeframe>,
    specs: Vec<IndicatorSpec>,
    warmup: usize,
    /// Journal des appels, lisible après le run.
    journal: Arc<Mutex<Vec<Appel>>>,
}

impl Scripted {
    fn new(plan: Vec<(usize, Signal)>) -> Scripted {
        Scripted {
            plan,
            calls: 0,
            timeframes: vec![Timeframe::M1],
            specs: Vec::new(),
            warmup: 0,
            journal: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Scripted sur ces timeframes, sans signal.
    fn sur(timeframes: &[Timeframe], warmup: usize) -> Scripted {
        let mut strategie = Scripted::new(Vec::new());
        strategie.timeframes = timeframes.to_vec();
        strategie.warmup = warmup;
        strategie
    }

    fn journal(&self) -> Arc<Mutex<Vec<Appel>>> {
        Arc::clone(&self.journal)
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
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        self.journal.lock().unwrap().push(Appel {
            now_ms: ctx.now_ms(),
            vient_de_clore: self
                .timeframes
                .iter()
                .map(|tf| ctx.just_closed(*tf))
                .collect(),
            visibles: self
                .timeframes
                .iter()
                .map(|tf| ctx.candles(*tf).len())
                .collect(),
            derniere_ouverture: self
                .timeframes
                .iter()
                .map(|tf| ctx.last(*tf).map(|c| c.open_time_ms))
                .collect(),
        });
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
        size: Size::Quote(100.1),
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
        sim_rules: SimRules {
            activation_delay_bars: 0,
            max_volume_share: 1e9,
            ..SimRules::default()
        },
        instrument: InstrumentRules::new(0.01, 1e-8, 0.0, 0.0).expect("instrument valide"),
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
fn signaux_s6_acceptes_et_refus_comptes() {
    let entree = |size, kind, stop, take_profit| Signal::EnterLong {
        size,
        kind,
        stop,
        take_profit,
        tag: "t",
    };
    let place = |side, price, size, slot| Signal::PlaceLimit {
        side,
        price,
        size,
        ttl_bars: 2,
        slot,
        tag: "t",
    };
    let plan = vec![
        // invalid_risk_sizing : stop au-dessus du close de b0 (99,5)
        (
            0,
            entree(Size::RiskPct(0.01), OrderKind::Market, Some(150.0), None),
        ),
        // risk_without_stop
        (
            0,
            entree(Size::RiskPct(0.01), OrderKind::Market, None, None),
        ),
        // invalid_size
        (0, entree(Size::Quote(-1.0), OrderKind::Market, None, None)),
        // invalid_ttl
        (
            0,
            entree(
                Size::Quote(10.0),
                OrderKind::Limit {
                    price: 99.0,
                    ttl_bars: 0,
                },
                None,
                None,
            ),
        ),
        // invalid_price
        (
            0,
            entree(
                Size::Quote(10.0),
                OrderKind::Limit {
                    price: f64::NAN,
                    ttl_bars: 2,
                },
                None,
                None,
            ),
        ),
        // move_stop_without_position
        (0, Signal::MoveStop { stop: 95.0 }),
        // acceptés : annulation sans ordre, limit de grille
        (0, Signal::CancelPending),
        (0, place(Side::Buy, 99.0, Size::Quote(10.0), 1)),
        // slot_already_open
        (0, place(Side::Buy, 98.0, Size::Quote(10.0), 1)),
        // invalid_slot
        (0, place(Side::Buy, 98.0, Size::Quote(10.0), 0)),
        // place_limit_risk_pct
        (0, place(Side::Buy, 98.0, Size::RiskPct(0.01), 2)),
        // sell_without_free_qty : aucune position
        (0, place(Side::Sell, 101.0, Size::Quote(10.0), 3)),
        // accepté, puis cancel_unknown_slot
        (0, Signal::CancelSlot { slot: 1 }),
        (0, Signal::CancelSlot { slot: 7 }),
        // exit_without_position
        (0, exit()),
        // insufficient_quote : 5 000 pour 1 000 de cash
        (0, place(Side::Buy, 98.0, Size::Quote(5_000.0), 2)),
        // accepté, puis entry_while_long
        (0, enter(89.5)),
        (0, enter(89.5)),
    ];
    let result = run(plan, &hand_candles());
    let r = result.rejections;
    assert_eq!(r.invalid_risk_sizing, 1);
    assert_eq!(r.risk_without_stop, 1);
    assert_eq!(r.invalid_size, 1);
    assert_eq!(r.invalid_ttl, 1);
    assert_eq!(r.invalid_price, 1);
    assert_eq!(r.move_stop_without_position, 1);
    assert_eq!(r.slot_already_open, 1);
    assert_eq!(r.invalid_slot, 1);
    assert_eq!(r.place_limit_risk_pct, 1);
    assert_eq!(r.sell_without_free_qty, 1);
    assert_eq!(r.cancel_unknown_slot, 1);
    assert_eq!(r.exit_without_position, 1);
    assert_eq!(r.insufficient_quote, 1);
    assert_eq!(r.entry_while_long, 1);
    assert_eq!(r.total(), 14);
    assert_eq!(result.signals_emitted, 18);
    assert_eq!(result.signals_accepted, 4);
    assert_eq!(result.signals_emitted, result.signals_accepted + r.total());
    // L'entrée market est remplie à b1 ; la limit de grille a été annulée avant de pouvoir l'être.
    assert_eq!(result.trades.len(), 1);
    assert_eq!(result.sim.orders_cancelled, 1);
    assert_eq!(result.sim.fills_limit, 0);
}

#[test]
fn sortie_limit_et_rejets_du_simulateur_comptes() {
    let sortie_limit = Signal::ExitLong {
        kind: OrderKind::Limit {
            price: 105.0,
            ttl_bars: 5,
        },
        tag: "limit",
    };
    let plan = vec![
        // Deux entrées sur la même bougie : la seconde est refusée par le simulateur.
        (0, enter(89.5)),
        (0, enter(89.5)),
        // Entrée alors que la position est longue (remplie à b1).
        (1, enter(89.5)),
        // Sortie limit à 105, puis une sortie market : doublon.
        (1, sortie_limit),
        (1, exit()),
        // La limit est remplie à b2 : plus de position.
        (2, exit()),
    ];
    let result = run(plan, &hand_candles());
    let r = result.rejections;
    assert_eq!(r.entry_while_long, 2);
    assert_eq!(r.duplicate_exit, 1);
    assert_eq!(r.exit_without_position, 1);
    assert_eq!(result.signals_emitted, result.signals_accepted + r.total());
    // Sortie limit exécutée : maker, au prix max(105, open b2 = 105).
    assert_eq!(result.trades.len(), 1);
    let t = result.trades[0];
    assert_eq!(t.exit_price, 105.0);
    assert_eq!(t.exit_ref_price, 105.0);
    assert_eq!(t.exit_ms, T0 + 2 * MINUTE);
    assert_eq!(t.exit_reason, ExitReason::Signal);
    assert_eq!(result.sim.fills_limit, 1);
    close_to(t.exit_fee, 105.0 * 0.002);
    assert!(!result.forced_close);
}

#[test]
fn strategie_multi_tf_acceptee() {
    let candles = common::synthetic_candles(2_000, 1, T0, 0.0);
    let strategy = common::strategy_from_file("sma_cross", "config/strategies/sma_cross.toml");
    let result = run_cell(&candles, strategy, &config(300.0)).expect("sma_cross 15m acceptée");
    // 50 bougies 15m de warmup, 15 minutes chacune, sans trou : la 1m 750 est la première jouée.
    assert_eq!(result.start_index, 50 * 15);
    assert_eq!(result.bars_in_window, 2_000 - 750);

    let deux = Scripted::sur(&[Timeframe::M1, Timeframe::M15], 0);
    let result = run_cell(&candles, Box::new(deux), &config(300.0)).expect("[1m, 15m] acceptée");
    assert_eq!(result.start_index, 0);
}

#[test]
fn timeframe_absent_du_mtf_refuse() {
    let candles = common::synthetic_candles(300, 1, T0, 0.0);
    let mtf = Mtf::build(&candles, &[Timeframe::M5]).unwrap();
    let strategie = Scripted::sur(&[Timeframe::H1], 0);
    let erreur = run_cell_mtf(
        &mtf,
        Box::new(strategie),
        &config(300.0),
        &mut tv4::engine::NoopObserver,
    )
    .unwrap_err();
    assert!(
        matches!(erreur, EngineError::MissingTimeframe { tf: Timeframe::H1 }),
        "{erreur:?}"
    );
    assert!(erreur.to_string().contains("1h"), "{erreur}");
}

/// Débuts de seaux `tf` distincts, calculés par division euclidienne des horodatages (sans `mtf`).
fn seaux(candles: &[Candle], tf: Timeframe) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for c in candles {
        let b = c.open_time_ms - c.open_time_ms.rem_euclid(tf.ms());
        if out.last() != Some(&b) {
            out.push(b);
        }
    }
    out
}

/// Nombre de seaux clôturés à l'instant `now_ms`, par recherche dichotomique sur les horodatages.
fn clos_a(seaux: &[i64], tf: Timeframe, now_ms: i64) -> usize {
    seaux.partition_point(|&b| b + tf.ms() <= now_ms)
}

#[test]
fn on_bar_appele_seulement_a_la_cloture_du_tf() {
    // Données synthétiques avec des trous (5 % des minutes sont suivies d'un trou).
    let candles = common::synthetic_candles(3_000, 11, T0, 0.05);
    assert!(
        candles
            .windows(2)
            .any(|w| w[1].open_time_ms - w[0].open_time_ms > MINUTE),
        "le jeu de test doit contenir des trous"
    );
    let strategie = Scripted::sur(&[Timeframe::M15], 0);
    let journal = strategie.journal();
    let result = run_cell(&candles, Box::new(strategie), &config(300.0)).unwrap();
    assert_eq!(result.start_index, 0);

    // Attendu, recalculé par horodatages : une 1m est un appel si le nombre de 15m clôturées y augmente.
    let b15 = seaux(&candles, Timeframe::M15);
    let mut attendus = Vec::new();
    let mut avant = 0;
    for c in &candles {
        let now = c.open_time_ms + MINUTE;
        let n = clos_a(&b15, Timeframe::M15, now);
        if n > avant {
            attendus.push(now);
        }
        avant = n;
    }
    let obtenus: Vec<i64> = journal.lock().unwrap().iter().map(|a| a.now_ms).collect();
    assert_eq!(obtenus, attendus);
    assert!(attendus.len() > 100, "{} appels", attendus.len());
    // Chaque appel annonce bien la clôture du 15m.
    assert!(journal.lock().unwrap().iter().all(|a| a.vient_de_clore[0]));
}

#[test]
fn warmup_en_bougies_du_plus_grand_tf() {
    // 1 500 minutes avec quelques trous : une vingtaine de H1.
    let candles = common::synthetic_candles(1_500, 5, T0, 0.02);
    let strategie = Scripted::sur(&[Timeframe::M5, Timeframe::H1], 3);
    let result = run_cell(&candles, Box::new(strategie), &config(300.0)).unwrap();

    // Attendu : plus petit `i` dont les 3 H1 sont clôturées à la fin de la 1m `i - 1`.
    let b1h = seaux(&candles, Timeframe::H1);
    let attendu = (1..candles.len())
        .find(|&i| clos_a(&b1h, Timeframe::H1, candles[i - 1].open_time_ms + MINUTE) >= 3)
        .expect("trois H1 dans les données");
    assert_eq!(result.start_index, attendu);
    assert_eq!(result.warmup_bars, 3);
}

#[test]
fn warmup_multi_tf_indicateur_nan_refuse() {
    // 700 minutes sans trou, alignées sur l'heure : la H1 numéro 5 se clôt à la 1m 299.
    let candles = common::synthetic_candles(700, 2, T0, 0.0);
    let mut strategie = Scripted::sur(&[Timeframe::H1], 5);
    strategie.specs = vec![IndicatorSpec {
        name: "sma10",
        tf: Timeframe::H1,
        kind: IndicatorKind::Sma { period: 10 },
    }];
    let erreur = run_cell(&candles, Box::new(strategie), &config(300.0)).unwrap_err();
    assert!(
        matches!(
            erreur,
            EngineError::WarmupTooShort {
                name: "sma10",
                index: 4
            }
        ),
        "{erreur:?}"
    );

    // Avec un warmup de 10 bougies H1, la SMA 10 est finie à l'index 9 : le run démarre.
    let mut assez = Scripted::sur(&[Timeframe::H1], 10);
    assez.specs = vec![IndicatorSpec {
        name: "sma10",
        tf: Timeframe::H1,
        kind: IndicatorKind::Sma { period: 10 },
    }];
    let result = run_cell(&candles, Box::new(assez), &config(300.0)).unwrap();
    assert_eq!(result.start_index, 600);
}

#[test]
fn bougie_tf_incomplete_jamais_vue() {
    // 2 h 38 de données (minutes 0 à 157) : la H1 02:00 n'est jamais complète, la 15m 02:30 non plus.
    let candles = common::synthetic_candles(158, 4, T0, 0.0);
    let strategie = Scripted::sur(&[Timeframe::M15, Timeframe::H1], 0);
    let journal = strategie.journal();
    run_cell(&candles, Box::new(strategie), &config(300.0)).unwrap();
    let appels = journal.lock().unwrap().clone();
    let derniere_h1 = T0 + 3_600_000;
    let derniere_15m = T0 + 2 * 3_600_000 + 900_000;
    for appel in &appels {
        assert!(
            appel.visibles[1] <= 2,
            "H1 visibles : {}",
            appel.visibles[1]
        );
        assert!(
            appel.visibles[0] <= 10,
            "15m visibles : {}",
            appel.visibles[0]
        );
        if let Some(ouverture) = appel.derniere_ouverture[1] {
            assert!(
                ouverture <= derniere_h1,
                "H1 {ouverture} vue à {}",
                appel.now_ms
            );
        }
        if let Some(ouverture) = appel.derniere_ouverture[0] {
            assert!(
                ouverture <= derniere_15m,
                "15m {ouverture} vue à {}",
                appel.now_ms
            );
        }
    }
    let dernier = appels.last().unwrap();
    assert_eq!(dernier.visibles, [10, 2]);
}

#[test]
fn just_closed_transmis_au_contexte() {
    let candles = common::synthetic_candles(60, 6, T0, 0.0);
    let strategie = Scripted::sur(&[Timeframe::M5, Timeframe::M15], 0);
    let journal = strategie.journal();
    run_cell(&candles, Box::new(strategie), &config(300.0)).unwrap();
    let appels = journal.lock().unwrap().clone();
    let a = |minute: i64| {
        appels
            .iter()
            .find(|appel| appel.now_ms == T0 + (minute + 1) * MINUTE)
            .unwrap_or_else(|| panic!("aucun appel à la 1m {minute}"))
    };
    // 1m hh:14 : la 5m 00:10 et la 15m 00:00 viennent de se clôturer.
    assert_eq!(a(14).vient_de_clore, [true, true]);
    assert_eq!(a(14).visibles, [3, 1]);
    // 1m hh:09 : seule la 5m 00:05.
    assert_eq!(a(9).vient_de_clore, [true, false]);
    assert_eq!(a(9).visibles, [2, 0]);
    // 1m hh:07 : aucune clôture, la stratégie n'est pas appelée.
    assert!(appels.iter().all(|appel| appel.now_ms != T0 + 8 * MINUTE));
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
