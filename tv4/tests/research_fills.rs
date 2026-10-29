//! Aucun fill dans une minute synthétique (S11, lot F, docs/specs/s11.md §6.4, critère 6) : avec
//! `zero_volume_marked`, `store::fill_gaps` insère des 1m à volume nul (masque `Columns::synthetic`), et
//! l'ordre d'un dataset de recherche ne peut être rempli que sur une 1m **réelle**.
//!
//! Trois scénarios déterministes (achat market avant un trou, limit au repos traversée par la première 1m
//! réelle après le trou, stop devenu actif pendant le trou), rejoués sur deux datasets identiques sauf la
//! politique de trous (`skip` contre `zero_volume_marked`) : mêmes fills, mêmes prix, au bit. Puis une
//! propriété (`proptest`, 256 cas) sur des séries à trous tirées au hasard et des ordres tirés au hasard.
//!
//! **Marché inventé** : bougies plates autour de 100, très liquides ; aucune donnée réelle. La stratégie
//! est indexée par l'instant de la bougie (et non par le rang de l'appel) : les appels diffèrent entre
//! `skip` et `zero_volume_marked`, le plan doit désigner les mêmes minutes réelles.

mod common;

use std::collections::BTreeSet;

use common::{T0, okx_fees, slippage};
use proptest::prelude::*;
use tv4::engine::{CellConfig, CellResult, run_cell};
use tv4::model::{Candle, InstrumentRules, Timeframe, Ts};
use tv4::sim::{ExitReason, SimRules};
use tv4::store::{Columns, GapsPolicy, fill_gaps};
use tv4::strategy::{BarContext, IndicatorSpec, OrderKind, Signal, Size, Strategy};

const MINUTE: Ts = 60_000;

/// Stratégie de test : émet chaque signal du plan à la clôture de la 1m qui s'ouvre à l'instant indiqué.
struct ParInstant {
    plan: Vec<(Ts, Signal)>,
}

impl Strategy for ParInstant {
    fn id(&self) -> &'static str {
        "par_instant"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M1]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        for (open_ms, signal) in &self.plan {
            if open_ms + MINUTE == ctx.now_ms() {
                out.push(*signal);
            }
        }
    }
}

/// Instant d'ouverture de la 1m numéro `i`.
fn ouverture(i: i64) -> Ts {
    T0 + i * MINUTE
}

/// Bougie plate inventée à l'instant `i` : open 100, close 100, extrêmes 99,8 et 101 (une limit d'achat à
/// 99,5 n'est pas traversée), très liquide.
fn plate(i: i64) -> Candle {
    Candle {
        open_time_ms: ouverture(i),
        open: 100.0,
        high: 101.0,
        low: 99.8,
        close: 100.0,
        volume: 1.0e6,
        trades: None,
    }
}

/// Bougie réelle à `i` avec ces open, high, low et close.
fn bougie(i: i64, open: f64, high: f64, low: f64, close: f64) -> Candle {
    Candle {
        open_time_ms: ouverture(i),
        open,
        high,
        low,
        close,
        volume: 1.0e6,
        trades: None,
    }
}

/// `n` bougies plates sans les minutes de `trou` ; `apres` remplace les bougies à partir de `trou.end`
/// quand il est donné.
fn serie(n: i64, trou: std::ops::Range<i64>, apres: Option<fn(i64) -> Candle>) -> Vec<Candle> {
    (0..n)
        .filter(|i| !trou.contains(i))
        .map(|i| match apres {
            Some(f) if i >= trou.end => f(i),
            _ => plate(i),
        })
        .collect()
}

fn config() -> CellConfig<'static> {
    CellConfig {
        symbol: "TEST-USD",
        fees: okx_fees(),
        slippage: slippage(0.0),
        initial_cash: 1_000.0,
        sim_rules: SimRules::default(),
        instrument: InstrumentRules::new(0.01, 1e-8, 0.0, 0.0).expect("instrument valide"),
    }
}

/// Politique de trous appliquée à des 1m réelles : `skip` rend les bougies telles quelles,
/// `zero_volume_marked` insère les minutes manquantes en synthétiques. Rend les bougies à jouer et
/// l'ensemble des instants synthétiques.
fn appliquer(reelles: &[Candle], politique: GapsPolicy) -> (Vec<Candle>, BTreeSet<Ts>) {
    let colonnes = fill_gaps(Columns::from_candles(reelles), Timeframe::M1, politique)
        .expect("politique de trous");
    let synthetiques = colonnes
        .open_time_ms()
        .iter()
        .zip(colonnes.synthetic())
        .filter(|(_, s)| **s)
        .map(|(t, _)| *t)
        .collect();
    (colonnes.to_candles(), synthetiques)
}

fn jouer(candles: &[Candle], plan: Vec<(Ts, Signal)>) -> CellResult {
    run_cell(candles, Box::new(ParInstant { plan }), &config()).expect("run valide")
}

fn achat_market(stop: Option<f64>) -> Signal {
    Signal::EnterLong {
        size: Size::Quote(100.0),
        kind: OrderKind::Market,
        stop,
        take_profit: None,
        tag: "entree",
    }
}

fn achat_limit(price: f64) -> Signal {
    Signal::EnterLong {
        size: Size::Quote(100.0),
        kind: OrderKind::Limit {
            price,
            ttl_bars: 60,
        },
        stop: None,
        take_profit: None,
        tag: "entree",
    }
}

const POLITIQUES: [GapsPolicy; 2] = [GapsPolicy::Skip, GapsPolicy::ZeroVolumeMarked];

/// Aucun fill de trade (hors clôture forcée, qui a lieu à la dernière 1m) sur une minute synthétique.
fn aucun_fill_synthetique(r: &CellResult, synthetiques: &BTreeSet<Ts>, contexte: &str) {
    for t in &r.trades {
        assert!(
            !synthetiques.contains(&t.entry_ms),
            "{contexte} : entrée sur la synthétique {}",
            t.entry_ms
        );
        if !t.forced_close {
            assert!(
                !synthetiques.contains(&t.exit_ms),
                "{contexte} : sortie sur la synthétique {}",
                t.exit_ms
            );
        }
    }
}

#[test]
fn un_achat_market_juste_avant_un_trou_est_rempli_a_la_premiere_1m_reelle_apres() {
    // Trou [30, 40) ; décision à la clôture de la 1m 28, premier fill possible à la 1m 30 (dans le trou).
    let reelles = serie(60, 30..40, None);
    let mut resultats = Vec::new();
    for politique in POLITIQUES {
        let (candles, synthetiques) = appliquer(&reelles, politique);
        let r = jouer(&candles, vec![(ouverture(28), achat_market(None))]);
        assert_eq!(r.trades.len(), 1, "{politique}");
        assert_eq!(r.trades[0].entry_ms, ouverture(40), "{politique}");
        assert_eq!(r.trades[0].entry_order_ms, ouverture(30), "{politique}");
        aucun_fill_synthetique(&r, &synthetiques, &politique.to_string());
        assert_eq!(
            synthetiques.len(),
            if politique == GapsPolicy::Skip { 0 } else { 10 }
        );
        resultats.push(r.trades);
    }
    assert_eq!(
        resultats[0], resultats[1],
        "skip et zvm : mêmes fills au bit"
    );
}

#[test]
fn une_limit_au_repos_est_remplie_a_son_prix_par_la_premiere_1m_reelle_qui_la_traverse() {
    // Avant le trou, aucune 1m ne traverse 99,5 (low 99,8). La première 1m réelle après le trou descend à
    // 99,0 : fill à 99,5 exactement, à la 1m 40.
    fn apres(i: i64) -> Candle {
        if i == 40 {
            bougie(i, 100.0, 100.5, 99.0, 100.0)
        } else {
            plate(i)
        }
    }
    let reelles = serie(60, 30..40, Some(apres));
    let mut resultats = Vec::new();
    for politique in POLITIQUES {
        let (candles, synthetiques) = appliquer(&reelles, politique);
        let r = jouer(&candles, vec![(ouverture(20), achat_limit(99.5))]);
        assert_eq!(r.trades.len(), 1, "{politique}");
        let t = r.trades[0];
        assert_eq!(t.entry_ms, ouverture(40), "{politique}");
        assert_eq!(t.entry_ref_price, 99.5, "{politique}");
        aucun_fill_synthetique(&r, &synthetiques, &politique.to_string());
        resultats.push(r.trades);
    }
    assert_eq!(
        resultats[0], resultats[1],
        "skip et zvm : mêmes fills au bit"
    );
}

#[test]
fn un_stop_deja_franchi_quand_il_devient_actif_pendant_le_trou_sort_sur_une_1m_reelle() {
    // Entrée au market décidée à 20 (fill à 22) avec un stop initial à 99,0, jamais touché avant le trou
    // (low 99,8). Un MoveStop à 99,5 décidé à 28 devient actif à 30, dans le trou. La première 1m réelle
    // après le trou ouvre à 97 : le stop est franchi, la sortie a lieu à la 1m 40.
    fn apres(i: i64) -> Candle {
        bougie(i, 97.0, 98.0, 96.0, 97.0)
    }
    let reelles = serie(60, 30..40, Some(apres));
    let mut resultats = Vec::new();
    for politique in POLITIQUES {
        let (candles, synthetiques) = appliquer(&reelles, politique);
        let r = jouer(
            &candles,
            vec![
                (ouverture(20), achat_market(Some(99.0))),
                (ouverture(28), Signal::MoveStop { stop: 99.5 }),
            ],
        );
        assert_eq!(r.trades.len(), 1, "{politique}");
        let t = r.trades[0];
        assert_eq!(t.entry_ms, ouverture(22), "{politique}");
        assert_eq!(t.exit_ms, ouverture(40), "{politique}");
        assert_eq!(t.exit_reason, ExitReason::Stop, "{politique}");
        assert!(!t.forced_close, "{politique}");
        aucun_fill_synthetique(&r, &synthetiques, &politique.to_string());
        resultats.push(r.trades);
    }
    assert_eq!(
        resultats[0], resultats[1],
        "skip et zvm : mêmes fills au bit"
    );
}

// ---------------------------------------------------------------------------------------------
// Propriété
// ---------------------------------------------------------------------------------------------

const N_PROP: usize = 90;

fn signal_de(genre: u8, niveau: f64, prix: f64) -> Signal {
    match genre {
        0 => achat_market(None),
        1 => achat_market(Some(prix * niveau.min(0.99))),
        2 => achat_limit(prix * niveau),
        3 => Signal::ExitLong {
            kind: OrderKind::Market,
            tag: "sortie",
        },
        _ => Signal::MoveStop {
            stop: prix * niveau,
        },
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Séries à trous tirées au hasard (première et dernière 1m réelles), marche aléatoire, ordres
    /// tirés au hasard : aucun fill (entrée, ou sortie hors clôture forcée) sur une 1m synthétique, que
    /// les trous soient laissés vides (`skip`) ou comblés (`zero_volume_marked`).
    #[test]
    fn aucun_fill_sur_une_minute_synthetique(
        presence in prop::collection::vec(prop::bool::weighted(0.6), N_PROP),
        pas in prop::collection::vec(-0.01f64..0.01, N_PROP),
        ordres in prop::collection::vec((0..N_PROP, 0u8..5, 0.95f64..1.05), 0..6),
    ) {
        let mut reelles = Vec::new();
        let mut prix = 100.0_f64;
        for (i, (present, pas)) in presence.iter().zip(&pas).enumerate() {
            let dernier = i == N_PROP - 1;
            let ouvert = prix;
            prix *= 1.0 + pas;
            if *present || i == 0 || dernier {
                reelles.push(bougie(
                    i as i64,
                    ouvert,
                    ouvert.max(prix) * 1.002,
                    ouvert.min(prix) * 0.998,
                    prix,
                ));
            }
        }
        // Les ordres ne sont décidés qu'à la clôture d'une 1m réelle.
        let instants_reels: BTreeSet<Ts> = reelles.iter().map(|c| c.open_time_ms).collect();
        let plan = || -> Vec<(Ts, Signal)> {
            ordres
                .iter()
                .filter(|(rang, _, _)| instants_reels.contains(&ouverture(*rang as i64)))
                .map(|(rang, genre, niveau)| {
                    let reference = reelles
                        .iter()
                        .find(|c| c.open_time_ms == ouverture(*rang as i64))
                        .map_or(100.0, |c| c.close);
                    (ouverture(*rang as i64), signal_de(*genre, *niveau, reference))
                })
                .collect()
        };
        for politique in POLITIQUES {
            let (candles, synthetiques) = appliquer(&reelles, politique);
            prop_assert!(
                synthetiques.iter().all(|t| !instants_reels.contains(t)),
                "une synthétique recouvre une 1m réelle"
            );
            let r = run_cell(&candles, Box::new(ParInstant { plan: plan() }), &config());
            let r = r.map_err(|e| TestCaseError::fail(format!("{politique} : {e}")))?;
            for t in &r.trades {
                prop_assert!(
                    !synthetiques.contains(&t.entry_ms),
                    "{politique} : entrée sur la synthétique {}",
                    t.entry_ms
                );
                if !t.forced_close {
                    prop_assert!(
                        !synthetiques.contains(&t.exit_ms),
                        "{politique} : sortie sur la synthétique {}",
                        t.exit_ms
                    );
                }
            }
        }
    }
}
