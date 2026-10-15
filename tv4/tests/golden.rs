//! Goldens du simulateur (docs/03-architecture.md §7.2 et §8, S6, décision E8).
//!
//! Deux familles :
//!
//! * dix cas écrits à la main sur des bougies 1m INVENTÉES (aucune donnée de marché), chacun avec son
//!   snapshot `insta` et des assertions de sens qui ne dépendent pas du snapshot ;
//! * une grille à 4 niveaux, stratégie de test de `tests/common/grille.rs`, sur une sinusoïde et sur une
//!   tendance baissière SYNTHÉTIQUES (`tests/common/sinus.rs`).
//!
//! Les snapshots sont dans `tests/snapshots/`. Hors CI, un snapshot absent ou différent écrit un
//! fichier `.snap.new` et fait échouer le test ; avec `CI=true` (GitHub Actions) ou `INSTA_UPDATE=no`,
//! l'échec est le même et rien n'est écrit : un snapshot oublié casse la CI. Création :
//! `INSTA_UPDATE=always cargo test --test golden`, puis relecture de chaque `.snap` avant commit.

mod common;

use std::fmt::Write as _;

use common::grille::{GrilleTest, NIVEAUX, QUOTE};
use common::scripte::Scripte;
use common::sinus::{sinusoide, tendance_baissiere};
use common::{T0, okx_fees, slippage};
use tv4::engine::{CellConfig, CellResult, run_cell};
use tv4::model::{Candle, InstrumentRules, Side};
use tv4::sim::{ExitReason, IntrabarRule, Liquidity, SimRules};
use tv4::strategy::{OrderKind, Signal, Size};

const MINUTE: i64 = 60_000;

/// Bougies 1m consécutives depuis `T0`, volume 1e6 : lignes `(open, high, low, close)`.
fn bougies(lignes: &[(f64, f64, f64, f64)]) -> Vec<Candle> {
    lignes
        .iter()
        .enumerate()
        .map(|(k, &(open, high, low, close))| Candle {
            open_time_ms: T0 + k as i64 * MINUTE,
            open,
            high,
            low,
            close,
            volume: 1.0e6,
            trades: None,
        })
        .collect()
}

type Ligne = (f64, f64, f64, f64);

const BASE: [Ligne; 6] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 101.0, 99.8, 100.8),
    (101.0, 102.0, 100.5, 101.5),
    (101.5, 103.0, 101.0, 102.5),
    (103.0, 104.0, 102.5, 103.5),
    (103.5, 104.0, 103.0, 103.8),
];

const ST: [Ligne; 5] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.6, 99.7, 100.2),
    (100.5, 101.0, 100.0, 100.4),
    (100.4, 100.5, 98.5, 98.8),
    (98.8, 99.0, 98.0, 98.5),
];

const ST2: [Ligne; 5] = [
    ST[0],
    ST[1],
    ST[2],
    (98.0, 98.4, 97.8, 98.2),
    (98.2, 98.5, 98.0, 98.3),
];

const BOTH: [Ligne; 5] = [
    ST[0],
    ST[1],
    ST[2],
    (100.4, 103.5, 98.8, 101.0),
    (101.0, 101.5, 100.5, 101.0),
];

const FB: [Ligne; 5] = [
    ST[0],
    ST[1],
    (100.5, 103.5, 100.3, 101.0),
    (101.0, 103.2, 100.8, 102.0),
    (102.0, 102.5, 101.5, 102.0),
];

const LIM: [Ligne; 7] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.4, 97.5, 99.0),
    (99.0, 99.5, 98.2, 98.5),
    (98.5, 98.9, 98.0, 98.4),
    (98.4, 98.8, 98.05, 98.6),
    (98.6, 98.7, 97.0, 97.5),
    (97.5, 98.0, 97.0, 97.8),
];

const GAP: [Ligne; 6] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.2, 99.6, 99.8),
    (98.0, 98.5, 97.5, 98.2),
    (98.2, 99.0, 98.0, 98.8),
    (103.0, 103.5, 102.6, 103.2),
    (103.2, 103.4, 103.0, 103.1),
];

const PLAT: [Ligne; 6] = [(100.0, 100.5, 99.5, 100.0); 6];

const FIFO: [Ligne; 7] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.2, 99.8, 100.0),
    (100.0, 100.2, 99.0, 99.5),
    (99.5, 100.0, 99.4, 99.8),
    (101.0, 102.0, 100.9, 101.5),
    (101.5, 103.5, 101.4, 103.0),
    (103.0, 103.2, 102.8, 103.0),
];

// --- Signaux ---

/// `EnterLong` : taille `Quote`, au marché ou en limite `(prix, ttl)`.
fn entree(quote: f64, limite: Option<(f64, u32)>, stop: Option<f64>, tp: Option<f64>) -> Signal {
    Signal::EnterLong {
        size: Size::Quote(quote),
        kind: match limite {
            Some((price, ttl_bars)) => OrderKind::Limit { price, ttl_bars },
            None => OrderKind::Market,
        },
        stop,
        take_profit: tp,
        tag: "entree",
    }
}

fn sortie(limite: Option<(f64, u32)>) -> Signal {
    Signal::ExitLong {
        kind: match limite {
            Some((price, ttl_bars)) => OrderKind::Limit { price, ttl_bars },
            None => OrderKind::Market,
        },
        tag: "sortie",
    }
}

/// `PlaceLimit` de tag `grille`, TTL 100.
fn pose(side: Side, price: f64, size: Size, slot: u16) -> Signal {
    Signal::PlaceLimit {
        side,
        price,
        size,
        ttl_bars: 100,
        slot,
        tag: "grille",
    }
}

// --- Exécution et résumé ---

fn config(rules: SimRules) -> CellConfig<'static> {
    CellConfig {
        symbol: "TEST-USD",
        fees: okx_fees(),
        slippage: slippage(10.0),
        initial_cash: 1000.0,
        sim_rules: rules,
        instrument: InstrumentRules::new(0.01, 1e-8, 0.0, 0.0).expect("instrument valide"),
    }
}

fn jouer(lignes: &[Ligne], plan: Vec<(usize, Signal)>, rules: SimRules) -> CellResult {
    run_cell(
        &bougies(lignes),
        Box::new(Scripte::new(plan)),
        &config(rules),
    )
    .expect("run valide")
}

/// Champs non nuls d'une structure de compteurs, dans l'ordre de déclaration : `serde_json::to_string`
/// d'une structure écrit les champs dans cet ordre (seul un `Value` les trierait).
fn champs_non_nuls<T: serde::Serialize>(valeur: &T) -> Vec<(String, u64)> {
    let json = serde_json::to_string(valeur).expect("compteurs sérialisables");
    json.trim_matches(|c| c == '{' || c == '}')
        .split(',')
        .filter_map(|paire| {
            let (nom, valeur) = paire.split_once(':')?;
            let valeur: u64 = valeur.parse().ok()?;
            (valeur != 0).then(|| (nom.trim_matches('"').to_string(), valeur))
        })
        .collect()
}

/// Index de 1m d'une date, depuis `T0`.
fn index_1m(ms: i64) -> i64 {
    (ms - T0) / MINUTE
}

/// Texte du snapshot : une ligne par grandeur, `{:?}` pour les flottants (plus court aller-retour,
/// signe de zéro visible).
fn resume(r: &CellResult, avec_trades: bool) -> String {
    let mut texte = String::new();
    writeln!(texte, "cash_final = {:?}", r.cash_final).unwrap();
    writeln!(texte, "pnl_net = {:?}", r.pnl_net).unwrap();
    writeln!(texte, "pnl_realized = {:?}", r.pnl_realized).unwrap();
    writeln!(texte, "pnl_marked = {:?}", r.pnl_marked).unwrap();
    writeln!(texte, "forced_close = {}", r.forced_close).unwrap();
    writeln!(texte, "n_trades = {}", r.trades.len()).unwrap();
    for (nom, valeur) in champs_non_nuls(&r.sim) {
        writeln!(texte, "sim.{nom} = {valeur}").unwrap();
    }
    for (nom, valeur) in champs_non_nuls(&r.rejections) {
        writeln!(texte, "rejet.{nom} = {valeur}").unwrap();
    }
    if avec_trades {
        for t in &r.trades {
            writeln!(
                texte,
                "trade lot={} entree={} {:?} ref={:?} prix={:?} sortie={} {:?} {:?} ref={:?} prix={:?} qty={:?} pnl_net={:?} mfe={:?} mae={:?}",
                t.lot_id,
                index_1m(t.entry_ms),
                t.entry_liquidity,
                t.entry_ref_price,
                t.entry_price,
                index_1m(t.exit_ms),
                t.exit_reason,
                t.exit_liquidity,
                t.exit_ref_price,
                t.exit_price,
                t.qty,
                t.pnl_net,
                t.mfe,
                t.mae,
            )
            .unwrap();
        }
    }
    texte
}

// --- Dix cas à la main ---

#[test]
fn golden_gagnant() {
    let r = jouer(
        &BASE,
        vec![
            (0, entree(500.0, None, Some(95.0), None)),
            (2, sortie(None)),
        ],
        SimRules::default(),
    );
    // Signal à la clôture de la 1m 0, fill à l'open de la 1m 2 (délai d'une minute) ; sortie signalée
    // à la clôture de la 1m 2, fill à l'open de la 1m 4.
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(index_1m(t.entry_ms), 2);
    assert_eq!(index_1m(t.exit_ms), 4);
    assert_eq!(t.exit_reason, ExitReason::Signal);
    assert_eq!(t.entry_liquidity, Liquidity::Taker);
    assert!(r.pnl_net > 0.0 && !r.forced_close);
    insta::assert_snapshot!("gagnant", resume(&r, true));
}

#[test]
fn golden_stop() {
    let r = jouer(
        &ST,
        vec![(0, entree(500.0, None, Some(99.0), None))],
        SimRules::default(),
    );
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.exit_reason, ExitReason::Stop);
    assert_eq!(index_1m(t.exit_ms), 3);
    assert_eq!(t.exit_ref_price, 99.0);
    assert!(r.pnl_net < 0.0);
    insta::assert_snapshot!("stop", resume(&r, true));
}

#[test]
fn golden_stop_gap() {
    let r = jouer(
        &ST2,
        vec![(0, entree(500.0, None, Some(99.0), None))],
        SimRules::default(),
    );
    // La 1m 3 ouvre à 98,0 sous le stop : le fill se fait au pire de (stop, open), soit l'open.
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.exit_reason, ExitReason::Stop);
    assert_eq!(t.exit_ref_price, 98.0);
    insta::assert_snapshot!("stop_gap", resume(&r, true));
}

#[test]
fn golden_stop_et_tp_dans_la_meme_1m_stop_first() {
    let r = jouer(
        &BOTH,
        vec![(0, entree(500.0, None, Some(99.0), Some(103.0)))],
        SimRules::default(),
    );
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_reason, ExitReason::Stop);
    assert_eq!(r.sim.stop_tp_same_bar, 1);
    insta::assert_snapshot!("stop_tp_stop_first", resume(&r, true));
}

#[test]
fn golden_stop_et_tp_dans_la_meme_1m_tp_first() {
    let r = jouer(
        &BOTH,
        vec![(0, entree(500.0, None, Some(99.0), Some(103.0)))],
        SimRules {
            intrabar: IntrabarRule::TpFirst,
            ..SimRules::default()
        },
    );
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    assert_eq!(t.exit_liquidity, Liquidity::Maker);
    assert_eq!(t.exit_ref_price, 103.0);
    assert!(r.pnl_net > 0.0);
    insta::assert_snapshot!("stop_tp_tp_first", resume(&r, true));
}

#[test]
fn golden_tp_touche_dans_la_bougie_de_fill_ignore() {
    let r = jouer(
        &FB,
        vec![(0, entree(500.0, None, Some(99.0), Some(103.0)))],
        SimRules::default(),
    );
    // La 1m 2 est la bougie de fill (high 103,5 >= 103) : le TP y est ignoré, il est exécuté à la 1m 3.
    assert_eq!(r.sim.take_profits_ignored_fill_bar, 1);
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(index_1m(t.entry_ms), 2);
    assert_eq!(index_1m(t.exit_ms), 3);
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    insta::assert_snapshot!("tp_bougie_de_fill", resume(&r, true));
}

#[test]
fn golden_limit_expire() {
    let r = jouer(
        &LIM,
        vec![(0, entree(300.0, Some((98.0, 3)), None, None))],
        SimRules::default(),
    );
    // Actif à partir de la 1m 2 pour 3 minutes (1m 2, 3, 4). La 1m 1 traverse le niveau avant
    // l'activation, la 1m 3 le touche exactement (traversée de 0 tick, pas de fill), la 1m 5 arrive
    // après l'expiration.
    assert_eq!(r.sim.limits_expired, 1);
    assert!(r.trades.is_empty());
    assert_eq!(r.cash_final, 1000.0);
    insta::assert_snapshot!("limit_expire", resume(&r, true));
}

#[test]
fn golden_gap_sous_la_limite() {
    let r = jouer(
        &GAP,
        vec![
            (0, entree(490.0, Some((99.0, 10)), None, None)),
            (2, sortie(Some((102.0, 10)))),
        ],
        SimRules::default(),
    );
    // Achat limit à 99,0 : la 1m 2 ouvre à 98,0, le fill se fait à min(limite, open) = 98,0. La vente
    // limit à 102,0 est remplie à max(limite, open) = 103,0 à l'ouverture de la 1m 4.
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.entry_liquidity, Liquidity::Maker);
    assert_eq!(t.entry_price, 98.0);
    assert_eq!(t.exit_price, 103.0);
    assert_eq!(index_1m(t.entry_ms), 2);
    assert_eq!(index_1m(t.exit_ms), 4);
    insta::assert_snapshot!("gap", resume(&r, true));
}

#[test]
fn golden_cash_insuffisant() {
    let r = jouer(
        &PLAT,
        vec![
            (0, pose(Side::Buy, 90.0, Size::Quote(600.0), 1)),
            (0, pose(Side::Buy, 89.0, Size::Quote(500.0), 2)),
            (0, pose(Side::Buy, 88.0, Size::Quote(300.0), 1)),
            (0, pose(Side::Buy, 88.0, Size::RiskPct(0.01), 3)),
            (1, entree(5000.0, None, None, None)),
        ],
        SimRules::default(),
    );
    assert_eq!(r.rejections.insufficient_quote, 1);
    assert_eq!(r.rejections.slot_already_open, 1);
    assert_eq!(r.rejections.place_limit_risk_pct, 1);
    assert_eq!(r.sim.entries_capped_by_cash, 1);
    assert!(r.forced_close);
    insta::assert_snapshot!("cash_insuffisant", resume(&r, true));
}

#[test]
fn golden_fifo() {
    let r = jouer(
        &FIFO,
        vec![
            (0, pose(Side::Buy, 100.0, Size::Quote(100.0), 1)),
            (0, pose(Side::Buy, 99.5, Size::Quote(99.5), 2)),
            (3, pose(Side::Sell, 103.0, Size::Quote(154.5), 3)),
        ],
        SimRules::default(),
    );
    // Deux lots d'une unité (à 100 et à 99,5). La vente de 1,5 unité ferme le lot le plus ancien en
    // entier, puis la moitié du second ; le reste est fermé par la clôture forcée.
    assert!(r.forced_close);
    assert_eq!(r.trades[0].lot_id, 0);
    assert_eq!(r.trades[0].qty, 1.0);
    assert_eq!(r.trades[1].lot_id, 1);
    assert_eq!(r.trades[1].qty, 0.5);
    assert_eq!(r.trades[1].exit_reason, ExitReason::LimitSell);
    assert_eq!(r.trades[2].lot_id, 1);
    assert_eq!(r.trades[2].exit_reason, ExitReason::ForcedClose);
    insta::assert_snapshot!("fifo", resume(&r, true));
}

// --- Grille synthétique ---

/// Frais maker de la grille OKX, en fraction (20 bps).
const MAKER: f64 = 0.002;
const TICK: f64 = 1e-4;

fn config_grille() -> CellConfig<'static> {
    CellConfig {
        symbol: "SYNTH",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: 1000.0,
        sim_rules: SimRules::default(),
        instrument: InstrumentRules::new(TICK, 1e-8, 0.0, 0.0).expect("instrument valide"),
    }
}

fn jouer_grille(candles: &[Candle], espacement: f64) -> (CellResult, u64, usize, f64) {
    let (grille, suivi) = GrilleTest::new(espacement);
    let b_n = grille.prix_achat(NIVEAUX);
    let r = run_cell(candles, Box::new(grille), &config_grille()).expect("run de la grille valide");
    let suivi = *suivi.lock().expect("suivi lisible");
    (r, suivi.cycles, suivi.tenus, b_n)
}

#[test]
fn grille_sinusoide_espacements() {
    let candles = sinusoide(2460, 0.06);
    assert_eq!(candles.len(), 2460);
    for s in [0.003, 0.0035, 0.004, 0.005, 0.01] {
        let (r, cycles, tenus, b_n) = jouer_grille(&candles, s);
        let bps = (s * 10_000.0_f64).round() as u32;
        let mut texte = resume(&r, false);
        writeln!(texte, "cycles = {cycles}").unwrap();
        insta::assert_snapshot!(format!("sinusoide_{bps}bps"), texte);

        assert!(cycles > 0, "espacement {s} : aucun cycle complet");
        assert!(!r.forced_close, "espacement {s} : clôture forcée");
        assert_eq!(tenus, 0, "espacement {s} : niveaux encore tenus");
        assert_eq!(r.sim.limits_expired, 0, "espacement {s}");
        assert_eq!(r.rejections.total(), 0, "espacement {s}");
        assert_eq!(r.sim.buys_rejected_volume_share, 0, "espacement {s}");
        assert_eq!(r.sim.buys_rejected_insufficient_cash, 0, "espacement {s}");

        // PnL réalisé attendu : par cycle, achat de Q / B_k à B_k puis vente à B_{k-1} = B_k (1 + s)
        // au tick près, soit Q (s − 2 m − m s) avec des frais maker m sur les deux jambes. Écart
        // toléré : l'arrondi au tick des deux prix, de l'ordre de 2 ticks / B_N par cycle.
        let attendu = cycles as f64 * QUOTE * (s - 2.0 * MAKER - MAKER * s);
        let tolerance = cycles as f64 * QUOTE * 2.0 * TICK / b_n;
        assert!(
            (r.pnl_realized - attendu).abs() <= tolerance,
            "espacement {s} : pnl_realized {} contre {attendu} (tolérance {tolerance})",
            r.pnl_realized
        );
        // À 0,20 % maker, un espacement de 0,40 % perd encore (terme m s) : le seuil est à 0,5 %.
        if s <= 0.004 {
            assert!(r.pnl_realized < 0.0, "espacement {s} devrait perdre");
        } else {
            assert!(r.pnl_realized > 0.0, "espacement {s} devrait gagner");
        }
    }
}

#[test]
fn grille_tendance_baissiere() {
    let candles = tendance_baissiere(1440, 720, 0.03, 0.12);
    assert_eq!(candles.len(), 1440);
    let (r, cycles, tenus, _) = jouer_grille(&candles, 0.005);
    let mut texte = resume(&r, true);
    writeln!(texte, "cycles = {cycles}").unwrap();
    insta::assert_snapshot!("tendance_baissiere", texte);

    // La baisse de 12 % laisse les 4 niveaux achetés sans revente possible : l'inventaire est soldé
    // au dernier close, et le PnL marqué est négatif.
    assert!(r.forced_close);
    assert_eq!(tenus, NIVEAUX);
    assert_eq!(r.sim.orders_unfilled_at_end, NIVEAUX as u64);
    assert!(r.pnl_marked < 0.0);
    assert!(r.pnl_marked < r.pnl_net);
    assert!(r.pnl_realized > 0.0);
    assert_eq!(r.sim.limits_expired, 0);
    assert_eq!(r.rejections.total(), 0);
}
