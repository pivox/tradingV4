//! Timing des fills (I3, docs/03-architecture.md §7.5) : un ordre décidé à la clôture de la 1m `i` n'est
//! jamais rempli à `i + 1`, et le premier fill possible est à l'open de la 1m `i + 2` par défaut
//! (`activation_delay_bars = 1`, docs/04 §3.1 point 1).
//!
//! Toutes les bougies sont inventées (marché plat à 100, grande liquidité), jamais des données de
//! marché : seul le timing est en jeu, pas les prix.

mod common;

use common::scripte::Scripte;
use common::{T0, okx_fees, slippage};
use tv4::engine::{CellConfig, CellResult, run_cell};
use tv4::model::{Candle, InstrumentRules, Timeframe};
use tv4::sim::{ExitReason, SimRules};
use tv4::strategy::{BarContext, OrderKind, Signal, Size, Strategy};

const MINUTE: i64 = 60_000;

/// Marché plat inventé : open 100, high 101, low 99, close 100, très liquide. Une limit d'achat à 99,5
/// et une limit de vente à 100,5 sont traversées par chaque bougie.
fn bougies_plates(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| Candle {
            open_time_ms: T0 + i as i64 * MINUTE,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 1.0e6,
            trades: None,
        })
        .collect()
}

fn config(delai: u32) -> CellConfig<'static> {
    CellConfig {
        symbol: "TEST-USD",
        fees: okx_fees(),
        slippage: slippage(0.0),
        initial_cash: 1_000.0,
        sim_rules: SimRules {
            activation_delay_bars: delai,
            ..SimRules::default()
        },
        instrument: InstrumentRules::new(0.01, 1e-8, 0.0, 0.0).expect("instrument valide"),
    }
}

fn achat_market() -> Signal {
    Signal::EnterLong {
        size: Size::Quote(100.0),
        kind: OrderKind::Market,
        stop: None,
        take_profit: None,
        tag: "entree",
    }
}

fn achat_limit() -> Signal {
    Signal::EnterLong {
        size: Size::Quote(100.0),
        kind: OrderKind::Limit {
            price: 99.5,
            ttl_bars: 10,
        },
        stop: None,
        take_profit: None,
        tag: "entree",
    }
}

fn vente_limit() -> Signal {
    Signal::ExitLong {
        kind: OrderKind::Limit {
            price: 100.5,
            ttl_bars: 10,
        },
        tag: "sortie",
    }
}

fn jouer(plan: Vec<(usize, Signal)>, n: usize, delai: u32) -> CellResult {
    run_cell(
        &bougies_plates(n),
        Box::new(Scripte::new(plan)),
        &config(delai),
    )
    .expect("run valide")
}

/// Instant d'ouverture de la 1m numéro `i`.
fn ouverture(i: i64) -> i64 {
    T0 + i * MINUTE
}

#[test]
fn ordre_pose_a_i_jamais_rempli_a_i_plus_1() {
    let i = 3_i64;
    // Market et limit d'achat : le fill d'entrée est à i + 2, jamais à i + 1.
    for (nom, signal) in [("market", achat_market()), ("limit achat", achat_limit())] {
        let r = jouer(vec![(i as usize, signal)], 12, 1);
        assert_eq!(r.trades.len(), 1, "{nom}");
        let t = r.trades[0];
        assert_ne!(t.entry_ms, ouverture(i + 1), "{nom} : fill à i + 1");
        assert_eq!(t.entry_ms, ouverture(i + 2), "{nom}");
        assert_eq!(t.entry_signal_ms, ouverture(i) + MINUTE, "{nom}");
    }

    // Limit de vente : entrée au market à 1, sortie décidée à 5, fill à 7 et pas à 6.
    let r = jouer(vec![(1, achat_market()), (5, vente_limit())], 12, 1);
    assert_eq!(r.trades.len(), 1);
    let t = r.trades[0];
    assert_eq!(t.entry_ms, ouverture(3));
    assert_ne!(t.exit_ms, ouverture(6), "vente limit remplie à i + 1");
    assert_eq!(t.exit_ms, ouverture(7));
    assert_eq!(t.exit_reason, ExitReason::Signal);
    assert!(!t.forced_close);
}

#[test]
fn signal_sur_la_derniere_bougie_zero_fill() {
    // Signal à la dernière 1m jouée : l'ordre n'a pas de bougie suivante, aucun fill n'a lieu.
    let n = 8;
    for signal in [achat_market(), achat_limit()] {
        let r = jouer(vec![(n - 1, signal)], n, 1);
        assert!(r.trades.is_empty());
        assert_eq!(r.qty_final, 0.0);
        assert_eq!(r.cash_final, 1_000.0);
    }
    // Un signal à l'avant-dernière : avec le délai par défaut le fill serait à i + 2 = n, hors fenêtre.
    let r = jouer(vec![(n - 2, achat_market())], n, 1);
    assert!(r.trades.is_empty());
}

#[test]
fn prix_de_fill_dans_la_bougie_de_fill() {
    let r = jouer(vec![(2, achat_market()), (5, vente_limit())], 12, 1);
    let bougies = bougies_plates(12);
    let t = r.trades[0];
    let entree = &bougies[((t.entry_ms - T0) / MINUTE) as usize];
    let sortie = &bougies[((t.exit_ms - T0) / MINUTE) as usize];
    assert!(t.entry_price >= entree.low && t.entry_price <= entree.high);
    assert!(t.exit_price >= sortie.low && t.exit_price <= sortie.high);
    // Limit de vente à 100,5 avec un open à 100 : ordre au repos, fill à la limite 100,5.
    assert_eq!(t.exit_price, 100.5);
}

#[test]
fn delai_zero_reproduit_le_fill_s3() {
    let i = 3_i64;
    for (nom, signal) in [("market", achat_market()), ("limit achat", achat_limit())] {
        let r = jouer(vec![(i as usize, signal)], 12, 0);
        assert_eq!(r.trades.len(), 1, "{nom}");
        assert_eq!(r.trades[0].entry_ms, ouverture(i + 1), "{nom}");
    }
    let r = jouer(vec![(1, achat_market()), (5, vente_limit())], 12, 0);
    assert_eq!(r.trades[0].entry_ms, ouverture(2));
    assert_eq!(r.trades[0].exit_ms, ouverture(6));
}

/// Signal émis à la clôture de la première 15m : entrée au market.
struct Entree15m {
    emis: bool,
    timeframes: Vec<Timeframe>,
}

impl Strategy for Entree15m {
    fn id(&self) -> &'static str {
        "entree_15m"
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
    fn indicators(&self) -> Vec<tv4::strategy::IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        if !self.emis && ctx.just_closed(Timeframe::M15) {
            self.emis = true;
            out.push(achat_market());
        }
    }
}

#[test]
fn strategie_15m_fill_deux_minutes_apres_la_cloture() {
    // T0 est aligné sur 15 minutes. La première 15m se clôture à T0 + 15 min : la décision est prise à
    // la clôture de la 1m T0 + 14 min (`signal_ms = T0 + 15 min`) et le fill a lieu à l'open de la 1m
    // `open_time = clôture + 60 000`.
    let strategie = Box::new(Entree15m {
        emis: false,
        timeframes: vec![Timeframe::M15],
    });
    let r = run_cell(&bougies_plates(40), strategie, &config(1)).expect("run valide");
    assert_eq!(r.trades.len(), 1);
    let t = r.trades[0];
    let cloture = T0 + 15 * MINUTE;
    assert_eq!(t.entry_signal_ms, cloture);
    assert_eq!(t.entry_ms, cloture + MINUTE);
}
