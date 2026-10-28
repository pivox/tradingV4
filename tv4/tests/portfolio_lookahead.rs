//! Look-ahead du moteur portefeuille (docs S9 §17, I2) : deux jambes, un espion qui enregistre ce que
//! chaque stratégie voit, et des perturbations du futur d'une jambe.
//!
//! * chaque stratégie ne voit que des bougies closes à `now_ms`, sur ses propres données (symbole et
//!   prix de sa jambe, jamais ceux de l'autre) ;
//! * modifier les 1m d'une jambe après un instant `T_p` ne change rien de ce que l'autre jambe a vu, ni
//!   rien de ce que la jambe modifiée a vu avant `T_p` ;
//! * en mode recherche, les trades de l'autre jambe ne changent pas du tout.

mod common;
mod portfolio_common;

use std::sync::{Arc, Mutex};

use portfolio_common::{MINUTE, T0, config, instrument, names, run};
use tv4::engine::PortfolioConfig;
use tv4::model::{Candle, Timeframe, Ts};
use tv4::strategy::{BarContext, IndicatorSpec, Signal, Strategy, registry};

/// Ce qu'une stratégie espion a vu à un appel.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    symbol: String,
    now_ms: Ts,
    /// Dernière 1m visible : (open_time, close).
    last_m1: Option<(Ts, f64)>,
    /// Dernière 5m visible : (open_time, close).
    last_m5: Option<(Ts, f64)>,
    just_closed_m5: bool,
}

/// Espion M1 + M5 : n'émet aucun signal, enregistre ses vues et vérifie la clôture des bougies.
struct Probe {
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Strategy for Probe {
    fn id(&self) -> &'static str {
        "probe"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M1, Timeframe::M5]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        2
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, _out: &mut Vec<Signal>) {
        let now = ctx.now_ms();
        let last_m1 = ctx.last(Timeframe::M1).map(|c| (c.open_time_ms, c.close));
        let last_m5 = ctx.last(Timeframe::M5).map(|c| (c.open_time_ms, c.close));
        // I2 : toute bougie visible est close à `now_ms`.
        if let Some((open, _)) = last_m1 {
            assert!(open + Timeframe::M1.ms() <= now, "1m non close");
        }
        if let Some((open, _)) = last_m5 {
            assert!(open + Timeframe::M5.ms() <= now, "5m non close");
        }
        self.seen.lock().unwrap().push(Seen {
            symbol: ctx.symbol().to_string(),
            now_ms: now,
            last_m1,
            last_m5,
            just_closed_m5: ctx.just_closed(Timeframe::M5),
        });
    }
}

fn series(seed: u64, start_price_scale: f64, count: usize) -> Vec<Candle> {
    common::synthetic_candles(count, seed, T0, 0.0)
        .into_iter()
        .map(|c| Candle {
            open: c.open * start_price_scale,
            high: c.high * start_price_scale,
            low: c.low * start_price_scale,
            close: c.close * start_price_scale,
            ..c
        })
        .collect()
}

type SharedSeen = Arc<Mutex<Vec<Seen>>>;

fn spies() -> (Vec<Box<dyn Strategy>>, Vec<SharedSeen>) {
    let seen: Vec<SharedSeen> = (0..2).map(|_| Arc::new(Mutex::new(Vec::new()))).collect();
    let strategies: Vec<Box<dyn Strategy>> = seen
        .iter()
        .map(|s| {
            Box::new(Probe {
                seen: Arc::clone(s),
            }) as Box<dyn Strategy>
        })
        .collect();
    (strategies, seen)
}

const N: usize = 2_000;

/// Joue A (prix ~100) et B (prix ~1000) avec l'espion ; `perturb` modifie les 1m de B à partir de
/// `from_minute`.
fn play_spies(perturb: Option<usize>) -> (Vec<Seen>, Vec<Seen>) {
    let a = series(11, 1.0, N);
    let mut b = series(23, 10.0, N);
    if let Some(from) = perturb {
        for c in &mut b[from..] {
            c.open *= 1.3;
            c.high *= 1.3;
            c.low *= 1.3;
            c.close *= 1.3;
        }
    }
    let universe = names(&["A-USD", "B-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let (strategies, seen) = spies();
    run(
        &[("A-USD", a), ("B-USD", b)],
        strategies,
        config(&universe, &instruments, 1000.0),
    );
    let take = |k: usize| seen[k].lock().unwrap().clone();
    (take(0), take(1))
}

#[test]
fn chaque_jambe_ne_voit_que_ses_propres_bougies_closes() {
    let (a, b) = play_spies(None);
    assert!(a.len() > 300 && b.len() > 300, "{} {}", a.len(), b.len());
    for seen in &a {
        assert_eq!(seen.symbol, "A-USD");
        // Prix de A autour de 100, jamais ceux de B (autour de 1000).
        assert!(seen.last_m1.unwrap().1 < 200.0, "{seen:?}");
        assert!(seen.last_m5.unwrap().1 < 200.0, "{seen:?}");
    }
    for seen in &b {
        assert_eq!(seen.symbol, "B-USD");
        assert!(seen.last_m1.unwrap().1 > 500.0, "{seen:?}");
        assert!(seen.last_m5.unwrap().1 > 500.0, "{seen:?}");
    }
    // La 5m ne clôt qu'aux minutes multiples de 5 (now_ms aligné sur 5 minutes).
    assert!(
        a.iter()
            .all(|s| s.just_closed_m5 == (s.now_ms.rem_euclid(5 * MINUTE) == 0))
    );
}

#[test]
fn modifier_le_futur_de_b_ne_change_rien_de_ce_que_a_a_vu() {
    let from = 1_200;
    let (a0, b0) = play_spies(None);
    let (a1, b1) = play_spies(Some(from));
    // A : vue identique à chaque appel, même après l'instant de la perturbation.
    assert_eq!(a0, a1);
    // B : identique jusqu'à l'instant de la perturbation (la 1m d'index `from` ouvre à
    // T0 + from minutes, et c'est la dernière clôture qui la précède), différente ensuite.
    let t_p = T0 + i64::try_from(from).unwrap() * MINUTE;
    let before = |v: &[Seen]| {
        v.iter()
            .filter(|s| s.now_ms <= t_p)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(before(&b0), before(&b1));
    assert!(before(&b0).len() > 100);
    assert_ne!(b0, b1, "la perturbation est visible après coup");
}

#[test]
fn en_recherche_les_trades_de_a_ne_dependent_pas_du_futur_de_b() {
    // `sma_cross` sur les deux jambes. Mode recherche : la base de sizing est le capital initial, le
    // plafond `capital / N` aussi ; le cash est abondant. Les trades de A sont donc ceux d'A seule.
    let play = |perturb: Option<usize>| {
        let a = series(11, 1.0, N + 400);
        let mut b = series(23, 10.0, N + 400);
        if let Some(from) = perturb {
            for c in &mut b[from..] {
                c.open *= 0.7;
                c.high *= 0.7;
                c.low *= 0.7;
                c.close *= 0.7;
            }
        }
        let universe = names(&["A-USD", "B-USD"]);
        let instruments = vec![instrument(0.0); 2];
        let strategies: Vec<Box<dyn Strategy>> = (0..2)
            .map(|_| {
                let text = "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0";
                let value = toml::from_str::<toml::Value>(text).unwrap();
                registry::build("sma_cross", &value).unwrap()
            })
            .collect();
        run(
            &[("A-USD", a), ("B-USD", b)],
            strategies,
            PortfolioConfig {
                max_positions: 8,
                ..config(&universe, &instruments, 1000.0)
            },
        )
    };
    let base = play(None);
    let changed = play(Some(1_500));
    let of_leg = |r: &tv4::engine::PortfolioResult, leg: u16| -> Vec<tv4::sim::Trade> {
        r.trades
            .iter()
            .zip(&r.trade_legs)
            .filter(|(_, l)| **l == leg)
            .map(|(t, _)| *t)
            .collect()
    };
    let a0 = of_leg(&base, 0);
    assert!(a0.len() >= 5, "assez de trades sur A : {}", a0.len());
    // Les `lot_id` sont globaux (compteur commun) : on compare les trades à `lot_id` près.
    let strip = |trades: Vec<tv4::sim::Trade>| {
        trades
            .into_iter()
            .map(|mut t| {
                t.lot_id = 0;
                t
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(a0), strip(of_leg(&changed, 0)));
    // Avant la perturbation, B aussi est identique.
    let b0: Vec<_> = of_leg(&base, 1)
        .into_iter()
        .filter(|t| t.exit_ms < T0 + 1_500 * MINUTE)
        .collect();
    let b1: Vec<_> = of_leg(&changed, 1)
        .into_iter()
        .filter(|t| t.exit_ms < T0 + 1_500 * MINUTE)
        .collect();
    assert_eq!(strip(b0), strip(b1));
}
