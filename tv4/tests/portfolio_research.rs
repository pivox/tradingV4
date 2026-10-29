//! Moteur en mode recherche (docs S11 §8 et §13) : activation par jambe, enregistrement à partir de
//! `decision_start`, filtre des entrées (fenêtre, univers, qualité), annulation des entrées en attente
//! d'une jambe qui sort de l'univers, identité de recherche et `config_hash`.
//!
//! Marché inventé : prix plats ou marche synthétique, dates dans FORWARD (2026-07-01 →) pour que la
//! fenêtre de décision soit valide sans données de 2018. Aucune donnée réelle.

mod common;
mod portfolio_common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use portfolio_common::{
    DAY, MINUTE, RegimeEntry, Scripted, buy_quote, config, flat_bar, instrument, names, sell_all,
};
use tv4::engine::{
    CostSetup, EngineError, Line, NoopObserver, OwnedPortfolioInputs, OwnedProfilesInputs,
    PortfolioConfig, PortfolioInputs, PortfolioResult, ProfilesInputs, ResearchIdentity,
    ResearchSpec, UniverseSpec, run_portfolio, run_portfolio_profiles_research,
    run_portfolio_profiles_research_owned, run_portfolio_research, run_portfolio_research_owned,
};
use tv4::model::Block;
use tv4::model::costs::CostsFile;
use tv4::model::universe::{Member, UniverseQuarter};
use tv4::model::{
    Candle, InstrumentRules, QualitySchedule, QuarterExclusion, Timeframe, Ts, UniverseSchedule,
};
use tv4::report::research::split_trades;
use tv4::sim::ExitReason;
use tv4::store::Columns;
use tv4::store::time::days_from_civil;
use tv4::store::{GapsPolicy, fill_gaps};
use tv4::strategy::{
    BarContext, IndicatorKind, IndicatorSpec, OrderKind, Signal, Size, Strategy, registry,
};

// ---------------------------------------------------------------------------------------------
// Aides
// ---------------------------------------------------------------------------------------------

/// Instant UTC en millisecondes.
fn at(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> Ts {
    days_from_civil(year, month, day) * DAY + hour * 3_600_000 + minute * MINUTE
}

/// `count` bougies 1m plates à partir de `first_ms`.
fn flat(first_ms: Ts, count: i64, price: f64) -> Vec<Candle> {
    (0..count)
        .map(|m| flat_bar(first_ms + m * MINUTE, price, 10.0))
        .collect()
}

fn data(series: Vec<(&str, Vec<Candle>)>) -> Vec<(String, Columns)> {
    series
        .into_iter()
        .map(|(symbol, candles)| (symbol.to_string(), Columns::from_candles(&candles)))
        .collect()
}

fn fixed(symbols: &[&str]) -> UniverseSpec {
    UniverseSpec::Fixed {
        symbols: names(symbols),
    }
}

fn quarter(label: &str, symbols: &[&str]) -> UniverseQuarter {
    let members = symbols
        .iter()
        .enumerate()
        .map(|(i, symbol)| Member {
            symbol: (*symbol).to_string(),
            rank: u8::try_from(i + 1).unwrap(),
        })
        .collect();
    UniverseQuarter::new(label, members).unwrap()
}

fn schedule(quarters: Vec<UniverseQuarter>) -> UniverseSchedule {
    UniverseSchedule::new(quarters).unwrap()
}

fn exclusions(entries: &[(&str, &str, &[Timeframe])]) -> QualitySchedule {
    let mut map: BTreeMap<String, Vec<QuarterExclusion>> = BTreeMap::new();
    for (symbol, label, tfs) in entries {
        map.entry((*symbol).to_string())
            .or_default()
            .push(QuarterExclusion {
                quarter: (*label).to_string(),
                tfs: tfs.to_vec(),
            });
    }
    QualitySchedule::from_exclusions(map).unwrap()
}

/// Identité de test : `legs_hash` vide, le moteur le recalcule.
fn identity(start: Ts, end: Ts, line: Line) -> ResearchIdentity {
    ResearchIdentity {
        block: Block::Forward,
        decision_start_ms: start,
        decision_end_ms: end,
        universe_window_hash: None,
        quality_hash: "blake3:test".to_string(),
        line,
        subset_hash: None,
        legs_hash: String::new(),
    }
}

/// Ce que `ResearchSpec` emprunte, pour une fenêtre `[start, end)` du bloc FORWARD.
struct Env {
    start: Ts,
    end: Ts,
    universe_spec: UniverseSpec,
    schedule: Option<UniverseSchedule>,
    quality: QualitySchedule,
    identity: ResearchIdentity,
}

impl Env {
    fn new(start: Ts, end: Ts, universe_spec: UniverseSpec) -> Env {
        Env {
            start,
            end,
            universe_spec,
            schedule: None,
            quality: QualitySchedule::default(),
            identity: identity(start, end, Line::Full),
        }
    }

    fn spec(&self) -> ResearchSpec<'_> {
        ResearchSpec {
            block: Block::Forward,
            decision_start_ms: self.start,
            decision_end_ms: self.end,
            universe: self.schedule.as_ref(),
            universe_spec: &self.universe_spec,
            quality: &self.quality,
            identity: &self.identity,
        }
    }
}

fn rules(count: usize) -> Vec<InstrumentRules> {
    vec![instrument(0.0); count]
}

/// Lance un portefeuille en mode recherche.
fn research(
    series: &[(String, Columns)],
    strategies: Vec<Box<dyn Strategy>>,
    universe: &[String],
    env: &Env,
) -> Result<PortfolioResult, EngineError> {
    let instruments = rules(universe.len());
    run_portfolio_research(
        PortfolioInputs {
            series,
            strategies,
            config: config(universe, &instruments, 300.0),
        },
        &env.spec(),
    )
}

// ---------------------------------------------------------------------------------------------
// Stratégie sonde
// ---------------------------------------------------------------------------------------------

/// `(now_ms, dernière SMA)` à chaque appel.
type Calls = Arc<Mutex<Vec<(Ts, f64)>>>;

/// Stratégie de test sur un seul timeframe `tf`, avec une SMA de `period` bougies. Elle note chaque appel
/// et, si `enter`, achète au marché pour 40 de cotation dès qu'elle est à plat, sans ordre en attente.
struct Probe {
    tfs: [Timeframe; 1],
    period: usize,
    enter: bool,
    /// Si présent, l'entrée est une limit d'achat à ce prix (loin sous le marché, donc en attente).
    limit: Option<f64>,
    calls: Calls,
}

impl Probe {
    fn boxed(tf: Timeframe, period: usize, enter: bool) -> (Box<dyn Strategy>, Calls) {
        let calls: Calls = Arc::default();
        (
            Box::new(Probe {
                tfs: [tf],
                period,
                enter,
                limit: None,
                calls: Arc::clone(&calls),
            }),
            calls,
        )
    }

    /// Comme `boxed(.., true)`, avec une limit d'achat à `price` valable 600 bougies 1m.
    fn limit(tf: Timeframe, period: usize, price: f64) -> Box<dyn Strategy> {
        Box::new(Probe {
            tfs: [tf],
            period,
            enter: true,
            limit: Some(price),
            calls: Arc::default(),
        })
    }
}

impl Strategy for Probe {
    fn id(&self) -> &'static str {
        "probe"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({
            "tf": self.tfs[0], "period": self.period, "enter": self.enter, "limit": self.limit
        })
    }
    fn timeframes(&self) -> &[Timeframe] {
        &self.tfs
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        vec![IndicatorSpec {
            name: "sma",
            tf: self.tfs[0],
            kind: IndicatorKind::Sma {
                period: self.period,
            },
        }]
    }
    fn warmup_bars(&self) -> usize {
        self.period
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        let sma = ctx
            .ind(self.tfs[0], "sma")
            .last()
            .copied()
            .unwrap_or(f64::NAN);
        self.calls.lock().unwrap().push((ctx.now_ms(), sma));
        if self.enter && ctx.position().is_none() && ctx.pending().is_empty() {
            out.push(Signal::EnterLong {
                size: Size::Quote(40.0),
                kind: match self.limit {
                    Some(price) => OrderKind::Limit {
                        price,
                        ttl_bars: 600,
                    },
                    None => OrderKind::Market,
                },
                stop: None,
                take_profit: None,
                tag: "probe",
            });
        }
    }
}

/// Entonnoir d'entrée de la recherche : `entry_signals = ordres + rejets + max_positions + refus`.
fn assert_funnel_identity(result: &PortfolioResult) {
    for (leg, research_leg) in result
        .legs
        .iter()
        .zip(&result.research.as_ref().unwrap().legs)
    {
        let f = &leg.funnel;
        assert_eq!(
            f.entry_signals,
            f.entry_orders
                + f.entry_rejections.total()
                + leg.max_positions_rejected
                + research_leg.counters.refused(),
            "identité de l'entonnoir de {}",
            leg.symbol
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Enregistrement à partir de decision_start, aucune entrée avant
// ---------------------------------------------------------------------------------------------

#[test]
fn la_grille_et_l_equity_commencent_a_decision_start() {
    let t0 = at(2026, 8, 10, 0, 0);
    let start = at(2026, 8, 11, 0, 0);
    let end = at(2026, 8, 12, 0, 0);
    let series = data(vec![("AAA-USD", flat(t0, 2 * 1440, 100.0))]);
    let universe = names(&["AAA-USD"]);
    let (strategy, calls) = Probe::boxed(Timeframe::M1, 30, false);
    let env = Env::new(start, end, fixed(&["AAA-USD"]));
    let result = research(&series, vec![strategy], &universe, &env).unwrap();

    // La chauffe est jouée (la stratégie est appelée avant decision_start), mais pas enregistrée.
    let first_call = calls.lock().unwrap().first().unwrap().0;
    assert!(
        first_call < start,
        "la stratégie doit être appelée dans la chauffe"
    );
    assert_eq!(result.window_first_open_ms, start);
    assert_eq!(result.grid_minutes, 1440);
    assert_eq!(result.grid_open_ms.len(), 1440);
    assert_eq!(result.equity.len(), 1440);
    assert_eq!(result.grid_open_ms[0], start);
    assert_eq!(result.equity[0], 300.0);
    // La fenêtre jouée, elle, commence à la fin du warmup de la jambe.
    assert_eq!(result.window_start_ms, t0 + 30 * MINUTE);
    let research = result.research.as_ref().unwrap();
    assert_eq!(research.legs[0].live_from_ms, Some(t0 + 30 * MINUTE));
    assert_eq!(research.decision_start_ms, start);
    assert_eq!(research.block, Block::Forward);
}

#[test]
fn aucune_entree_avant_decision_start_et_les_refus_sont_comptes() {
    let t0 = at(2026, 8, 10, 22, 0);
    let start = at(2026, 8, 11, 0, 0);
    let end = at(2026, 8, 11, 4, 0);
    let series = data(vec![("AAA-USD", flat(t0, 6 * 60, 100.0))]);
    let universe = names(&["AAA-USD"]);
    let (strategy, _) = Probe::boxed(Timeframe::M1, 5, true);
    let env = Env::new(start, end, fixed(&["AAA-USD"]));
    let result = research(&series, vec![strategy], &universe, &env).unwrap();

    let counters = result.research.as_ref().unwrap().legs[0].counters;
    // La sonde entre à chaque appel à plat : toutes les tentatives de la chauffe sont refusées.
    assert!(
        counters.entries_outside_decision_window > 100,
        "{counters:?}"
    );
    assert_eq!(counters.entries_not_in_universe, 0);
    assert_eq!(counters.entries_excluded_quality, 0);
    assert!(
        !result.trades.is_empty(),
        "l'entrée de la fenêtre est acceptée"
    );
    for trade in &result.trades {
        assert!(
            trade.entry_signal_ms >= start,
            "entrée décidée à {} avant decision_start",
            trade.entry_signal_ms
        );
    }
    assert_funnel_identity(&result);
}

#[test]
fn une_entree_a_decision_end_est_refusee() {
    let t0 = at(2026, 8, 10, 22, 0);
    let start = at(2026, 8, 11, 0, 0);
    let end = at(2026, 8, 11, 1, 0);
    // Les données vont bien au-delà de la fin de décision : les appels de [end, ...) sont refusés.
    let series = data(vec![("AAA-USD", flat(t0, 5 * 60, 100.0))]);
    let universe = names(&["AAA-USD"]);
    let (strategy, _) = Probe::boxed(Timeframe::M1, 5, true);
    let env = Env::new(start, end, fixed(&["AAA-USD"]));
    let result = research(&series, vec![strategy], &universe, &env).unwrap();
    for trade in &result.trades {
        assert!(trade.entry_signal_ms < end);
    }
    assert!(
        result.research.as_ref().unwrap().legs[0]
            .counters
            .entries_outside_decision_window
            > 0
    );
    assert_funnel_identity(&result);
}

// ---------------------------------------------------------------------------------------------
// La chauffe sert
// ---------------------------------------------------------------------------------------------

#[test]
fn la_chauffe_est_lue_les_indicateurs_a_decision_start_sont_identiques() {
    let start = at(2026, 8, 11, 0, 0);
    let end = at(2026, 8, 11, 6, 0);
    let universe = names(&["AAA-USD"]);
    // Même marche synthétique sur la fenêtre, préfixe de 2 h ou de 3 jours : la SMA(30) est identique
    // dès qu'elle est chaude, donc à chaque appel de la fenêtre de décision.
    let walk = common::synthetic_candles(4 * 1440, 11, at(2026, 8, 8, 0, 0), 0.0);
    let cut = |from: Ts| -> Vec<Candle> {
        walk.iter()
            .filter(|c| c.open_time_ms >= from)
            .copied()
            .collect()
    };
    let run = |prefix: Ts| {
        let series = data(vec![("AAA-USD", cut(start - prefix))]);
        let (strategy, calls) = Probe::boxed(Timeframe::M1, 30, false);
        let env = Env::new(start, end, fixed(&["AAA-USD"]));
        research(&series, vec![strategy], &universe, &env).unwrap();
        let guard = calls.lock().unwrap();
        guard.clone()
    };
    let short = run(2 * 3_600_000);
    let long = run(3 * DAY);
    let in_window = |calls: &[(Ts, f64)]| -> Vec<(Ts, f64)> {
        calls
            .iter()
            .filter(|(now, _)| *now >= start)
            .copied()
            .collect()
    };
    let (short_w, long_w) = (in_window(&short), in_window(&long));
    assert!(!short_w.is_empty());
    assert_eq!(short_w.len(), long_w.len());
    for (a, b) in short_w.iter().zip(&long_w) {
        assert_eq!(a.0, b.0);
        assert!(a.1.is_finite(), "SMA non chaude à {}", a.0);
        assert_eq!(a.1.to_bits(), b.1.to_bits(), "SMA différente à {}", a.0);
    }
    // Dès l'instant decision_start, la valeur existe (la chauffe a servi) et la stratégie a été appelée
    // avant.
    assert_eq!(short_w[0].0, start);
    assert!(short.iter().any(|(now, _)| *now < start));
}

/// Marché de l'exposition et du régime : BTC-USD (paire de régime, hors univers) a 210 jours de prix
/// plat à 100 puis 3 jours à 110 (haussier) et 3 jours à 90 (baissier) ; AAA-USD est plat à 50.
fn regime_market(aaa_prefix_days: i64, start: Ts) -> Vec<(String, Columns)> {
    let btc_from = start - 210 * DAY;
    let mut btc = flat(btc_from, 210 * 1440, 100.0);
    btc.extend(flat(start, 3 * 1440, 110.0));
    btc.extend(flat(start + 3 * DAY, 3 * 1440, 90.0));
    let aaa_from = start - aaa_prefix_days * DAY;
    let aaa = flat(aaa_from, aaa_prefix_days * 1440 + 6 * 1440, 50.0);
    data(vec![("BTC-USD", btc), ("AAA-USD", aaa)])
}

#[test]
fn exposition_et_jours_de_regime_identiques_avec_une_chauffe_de_0_ou_de_210_jours() {
    let start = at(2026, 8, 11, 0, 0);
    let end = start + 6 * DAY;
    let universe = names(&["AAA-USD"]);
    let run = |prefix_days: i64| {
        let series = regime_market(prefix_days, start);
        let (strategy, _) = Scripted::with_regime(RegimeEntry {
            every_ms: 3_600_000,
            signal: buy_quote(60.0),
        });
        let env = Env::new(start, end, fixed(&["AAA-USD"]));
        let instruments = rules(1);
        run_portfolio_research(
            PortfolioInputs {
                series: &series,
                strategies: vec![Box::new(strategy)],
                config: config(&universe, &instruments, 300.0),
            },
            &env.spec(),
        )
        .unwrap()
    };
    let cold = run(0);
    let warm = run(210);
    // La chauffe de 210 jours est jouée mais pas enregistrée.
    assert_eq!(cold.window_start_ms, start);
    assert!(warm.window_start_ms < start);
    assert_eq!(cold.window_first_open_ms, start);
    assert_eq!(warm.window_first_open_ms, start);
    assert_eq!(cold.grid_minutes, 6 * 1440);
    assert_eq!(warm.grid_minutes, cold.grid_minutes);
    // Mêmes trades, même exposition, mêmes jours de régime, même equity.
    assert!(!cold.trades.is_empty(), "des trades doivent exister");
    assert_eq!(cold.trades.len(), warm.trades.len());
    for (a, b) in cold.trades.iter().zip(&warm.trades) {
        assert_eq!(a.entry_ms, b.entry_ms);
        assert_eq!(a.exit_ms, b.exit_ms);
        assert_eq!(a.pnl_net.to_bits(), b.pnl_net.to_bits());
    }
    assert_eq!(cold.bars_in_position, warm.bars_in_position);
    assert!(cold.bars_in_position > 0);
    let (rc, rw) = (cold.regime.as_ref().unwrap(), warm.regime.as_ref().unwrap());
    assert_eq!(rc, rw);
    assert!(rc.days_bull > 0 && rc.days_bear > 0, "{rc:?}");
    assert_eq!(rc.days_bull + rc.days_bear, 6);
    assert_eq!(
        cold.equity.iter().map(|e| e.to_bits()).collect::<Vec<_>>(),
        warm.equity.iter().map(|e| e.to_bits()).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------------------------
// Activation par jambe
// ---------------------------------------------------------------------------------------------

#[test]
fn chaque_jambe_demarre_a_sa_propre_minute_et_une_jambe_trop_courte_n_est_pas_une_erreur() {
    let t0 = at(2026, 8, 10, 0, 0);
    let late = at(2026, 8, 11, 12, 0);
    let start = at(2026, 8, 10, 12, 0);
    let end = at(2026, 8, 12, 0, 0);
    let series = data(vec![
        ("AAA-USD", flat(t0, 2 * 1440, 100.0)),
        ("BBB-USD", flat(late, 12 * 60, 50.0)),
        ("CCC-USD", flat(late, 10, 20.0)),
    ]);
    let universe = names(&["AAA-USD", "BBB-USD", "CCC-USD"]);
    let make = || {
        (0..3)
            .map(|_| Probe::boxed(Timeframe::M1, 30, false).0)
            .collect::<Vec<_>>()
    };
    let env = Env::new(start, end, fixed(&["AAA-USD", "BBB-USD", "CCC-USD"]));
    let result = research(&series, make(), &universe, &env).unwrap();
    let live: Vec<Option<Ts>> = result
        .research
        .as_ref()
        .unwrap()
        .legs
        .iter()
        .map(|leg| leg.live_from_ms)
        .collect();
    assert_eq!(live[0], Some(t0 + 30 * MINUTE));
    assert_eq!(live[1], Some(late + 30 * MINUTE));
    assert_eq!(live[2], None, "CCC n'atteint jamais son warmup");
    assert_eq!(result.legs[2].rows, 0);
    assert_eq!(result.window_start_ms, t0 + 30 * MINUTE);
    // Le départ commun de S9 repousse tout au départ de la jambe la plus tardive, et refuse CCC.
    let instruments = rules(3);
    let common_start = run_portfolio(PortfolioInputs {
        series: &series,
        strategies: make(),
        config: config(&universe, &instruments, 300.0),
    });
    assert!(matches!(
        common_start,
        Err(EngineError::NotEnoughBars { .. })
    ));
    let universe2 = names(&["AAA-USD", "BBB-USD"]);
    let instruments2 = rules(2);
    let common_start = run_portfolio(PortfolioInputs {
        series: &series,
        strategies: make().into_iter().take(2).collect(),
        config: config(&universe2, &instruments2, 300.0),
    })
    .unwrap();
    assert_eq!(common_start.window_start_ms, late + 30 * MINUTE);
}

#[test]
fn aucune_jambe_vivante_est_une_erreur() {
    let t0 = at(2026, 8, 10, 0, 0);
    let series = data(vec![("AAA-USD", flat(t0, 10, 100.0))]);
    let universe = names(&["AAA-USD"]);
    let (strategy, _) = Probe::boxed(Timeframe::M1, 30, false);
    let env = Env::new(t0, t0 + DAY, fixed(&["AAA-USD"]));
    let error = research(&series, vec![strategy], &universe, &env).unwrap_err();
    assert!(matches!(error, EngineError::ResearchNoLiveLeg), "{error}");
}

// ---------------------------------------------------------------------------------------------
// Annulation à D - 60 s
// ---------------------------------------------------------------------------------------------

fn boundary_market() -> Vec<(String, Columns)> {
    let from = at(2026, 9, 30, 22, 0);
    data(vec![
        ("AAA-USD", flat(from, 4 * 60, 100.0)),
        ("BBB-USD", flat(from, 4 * 60, 100.0)),
    ])
}

fn run_boundary(next_quarter_members: &[&str]) -> PortfolioResult {
    let d = at(2026, 10, 1, 0, 0);
    let series = boundary_market();
    let universe = names(&["AAA-USD", "BBB-USD"]);
    // AAA : un achat décidé à D - 60 s, donc actif à D. BBB ne fait rien.
    let (aaa, _) = Scripted::new(vec![(d - MINUTE, buy_quote(50.0))]);
    let (bbb, _) = Scripted::new(Vec::new());
    let mut env = Env::new(
        at(2026, 9, 30, 23, 0),
        at(2026, 10, 1, 2, 0),
        UniverseSpec::Quarterly {
            rules_hash: "blake3:rules".to_string(),
        },
    );
    env.schedule = Some(schedule(vec![
        quarter("2026-Q3", &["AAA-USD", "BBB-USD"]),
        quarter("2026-Q4", next_quarter_members),
    ]));
    research(&series, vec![Box::new(aaa), Box::new(bbb)], &universe, &env).unwrap()
}

#[test]
fn une_entree_decidee_a_d_moins_60_s_est_annulee_quand_la_jambe_sort_de_l_univers() {
    let d = at(2026, 10, 1, 0, 0);
    // Témoin : AAA reste membre, l'ordre remplit à D.
    let kept = run_boundary(&["AAA-USD", "BBB-USD"]);
    assert_eq!(kept.trades.len(), 1);
    assert_eq!(kept.trades[0].entry_ms, d);
    assert_eq!(kept.sim.orders_cancelled, 0);
    let counters = kept.research.as_ref().unwrap().legs[0].counters;
    assert_eq!(counters.entries_cancelled_universe, 0);

    // AAA sort de l'univers au trimestre de D : aucun fill, une annulation comptée.
    let left = run_boundary(&["BBB-USD"]);
    assert!(left.trades.is_empty(), "aucun fill après la sortie");
    assert_eq!(left.legs[0].counters.fills, 0);
    assert_eq!(left.sim.orders_cancelled, 1);
    assert_eq!(
        left.legs[0].funnel.entry_orders, 1,
        "l'entrée avait été acceptée"
    );
    let research = left.research.as_ref().unwrap();
    assert_eq!(research.legs[0].counters.entries_cancelled_universe, 1);
    assert_eq!(research.legs[1].counters.entries_cancelled_universe, 0);
    assert_eq!(research.totals.entries_cancelled_universe, 1);
    assert_funnel_identity(&left);
}

#[test]
fn une_jambe_en_position_qui_sort_de_l_univers_va_a_son_terme() {
    let d = at(2026, 10, 1, 0, 0);
    let series = boundary_market();
    let universe = names(&["AAA-USD", "BBB-USD"]);
    // AAA entre une heure avant D et sort 30 minutes après : la sortie n'est pas filtrée.
    let (aaa, _) = Scripted::new(vec![
        (d - 60 * MINUTE, buy_quote(50.0)),
        (d + 30 * MINUTE, sell_all()),
    ]);
    let (bbb, _) = Scripted::new(Vec::new());
    let mut env = Env::new(
        at(2026, 9, 30, 23, 0),
        at(2026, 10, 1, 2, 0),
        UniverseSpec::Quarterly {
            rules_hash: "blake3:rules".to_string(),
        },
    );
    env.schedule = Some(schedule(vec![
        quarter("2026-Q3", &["AAA-USD", "BBB-USD"]),
        quarter("2026-Q4", &["BBB-USD"]),
    ]));
    let result = research(&series, vec![Box::new(aaa), Box::new(bbb)], &universe, &env).unwrap();
    assert_eq!(result.trades.len(), 1);
    assert!(!result.trades[0].forced_close);
    assert!(result.trades[0].exit_ms >= d + 30 * MINUTE);
    assert_eq!(result.sim.orders_cancelled, 0);
    assert_eq!(
        result
            .research
            .as_ref()
            .unwrap()
            .totals
            .entries_not_in_universe,
        0
    );
}

/// Limit d'achat autonome (`PlaceLimit`, slot 1) à `price`, pour 30 de cotation, valable très longtemps.
fn place_buy(price: f64) -> Signal {
    Signal::PlaceLimit {
        side: tv4::model::Side::Buy,
        price,
        size: Size::Quote(30.0),
        ttl_bars: 1_000_000,
        slot: 1,
        tag: "place",
    }
}

/// Marché de la frontière D = 2026-10-01 pour AAA (100 puis 85 à partir de D + 20 min) et BBB (plat).
fn boundary_market_drop() -> Vec<(String, Columns)> {
    let from = at(2026, 9, 30, 22, 0);
    let d = at(2026, 10, 1, 0, 0);
    let drop = d + 20 * MINUTE;
    let aaa: Vec<Candle> = (0..4 * 60)
        .map(|m| {
            let open_ms = from + m * MINUTE;
            flat_bar(open_ms, if open_ms >= drop { 85.0 } else { 100.0 }, 10.0)
        })
        .collect();
    data(vec![
        ("AAA-USD", aaa),
        ("BBB-USD", flat(from, 4 * 60, 100.0)),
    ])
}

/// AAA achète au marché à D - 60 min et pose une limit d'achat à 90 à D - 30 min ; si `sell_after`,
/// elle vend tout à D + 10 min. AAA est absente du trimestre de D, BBB reste membre.
fn run_place_buy_left(sell_after: bool) -> PortfolioResult {
    let d = at(2026, 10, 1, 0, 0);
    let series = boundary_market_drop();
    let universe = names(&["AAA-USD", "BBB-USD"]);
    let mut plan = vec![
        (d - 60 * MINUTE, buy_quote(50.0)),
        (d - 30 * MINUTE, place_buy(90.0)),
    ];
    if sell_after {
        plan.push((d + 10 * MINUTE, sell_all()));
    }
    let (aaa, _) = Scripted::new(plan);
    let (bbb, _) = Scripted::new(Vec::new());
    let mut env = Env::new(
        at(2026, 9, 30, 23, 0),
        at(2026, 10, 1, 2, 0),
        UniverseSpec::Quarterly {
            rules_hash: "blake3:rules".to_string(),
        },
    );
    env.schedule = Some(schedule(vec![
        quarter("2026-Q3", &["AAA-USD", "BBB-USD"]),
        quarter("2026-Q4", &["BBB-USD"]),
    ]));
    research(&series, vec![Box::new(aaa), Box::new(bbb)], &universe, &env).unwrap()
}

#[test]
fn les_limits_d_achat_d_une_jambe_en_position_qui_sort_de_l_univers_sont_annulees() {
    let d = at(2026, 10, 1, 0, 0);
    // La limit d'achat à 90 serait remplie à D + 20 min (prix 85) : elle est annulée à D, la position
    // ouverte à D - 60 min va à son terme (clôture forcée), aucune entrée ne remplit après D.
    let result = run_place_buy_left(false);
    assert!(
        result.trades.iter().all(|trade| trade.entry_ms < d),
        "aucune entrée remplie hors univers : {:?}",
        result.trades.iter().map(|t| t.entry_ms).collect::<Vec<_>>()
    );
    assert_eq!(result.trades.len(), 1);
    assert_eq!(result.trades[0].entry_ms, d - 59 * MINUTE);
    assert_eq!(result.sim.orders_cancelled, 1);
    let research = result.research.as_ref().unwrap();
    assert_eq!(research.legs[0].counters.entries_cancelled_universe, 1);
    assert_eq!(research.totals.entries_cancelled_universe, 1);
    assert_funnel_identity(&result);
}

#[test]
fn la_limit_d_achat_annulee_n_ouvre_pas_de_position_neuve_apres_la_vente() {
    let d = at(2026, 10, 1, 0, 0);
    // Variante : la position est vendue à D + 10 min, la limit (annulée à D) ne remplit pas à D + 20 min.
    let result = run_place_buy_left(true);
    assert_eq!(result.trades.len(), 1, "aucune position neuve");
    assert_eq!(result.trades[0].entry_ms, d - 59 * MINUTE);
    assert!(!result.trades[0].forced_close);
    assert_eq!(result.sim.orders_cancelled, 1);
    assert_eq!(
        result
            .research
            .as_ref()
            .unwrap()
            .totals
            .entries_cancelled_universe,
        1
    );
    assert_funnel_identity(&result);
}

/// Stratégie 1h qui rejoue un plan de signaux, chacun émis une fois au premier appel de `now_ms` au
/// moins égal à sa date.
struct PlannedH1 {
    tfs: [Timeframe; 1],
    plan: Vec<(Ts, Signal)>,
    fired: Vec<bool>,
}

impl PlannedH1 {
    fn boxed(plan: Vec<(Ts, Signal)>) -> Box<dyn Strategy> {
        let fired = vec![false; plan.len()];
        Box::new(PlannedH1 {
            tfs: [Timeframe::H1],
            plan,
            fired,
        })
    }
}

impl Strategy for PlannedH1 {
    fn id(&self) -> &'static str {
        "planned_h1"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &self.tfs
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        for ((at, signal), fired) in self.plan.iter().zip(self.fired.iter_mut()) {
            if !*fired && ctx.now_ms() >= *at {
                *fired = true;
                out.push(*signal);
            }
        }
    }
}

#[test]
fn une_exclusion_qualite_annule_les_limits_d_achat_d_une_jambe_en_position() {
    // Variante qualité : AAA reste membre mais son 1h est exclu en 2026-Q4. Elle achète au marché et pose
    // sa limit d'achat à 23:00 (D - 60 min), le prix baisse à D + 20 min : la limit est annulée à D.
    let d = at(2026, 10, 1, 0, 0);
    let series = boundary_market_drop();
    let universe = names(&["AAA-USD", "BBB-USD"]);
    let run = |excluded: bool| {
        // Les deux signaux de AAA partent au même appel de 23:00.
        let strategies: Vec<Box<dyn Strategy>> = vec![
            PlannedH1::boxed(vec![
                (d - 60 * MINUTE, buy_quote(50.0)),
                (d - 60 * MINUTE, place_buy(90.0)),
            ]),
            PlannedH1::boxed(Vec::new()),
        ];
        let mut env = Env::new(
            at(2026, 9, 30, 23, 0),
            at(2026, 10, 1, 2, 0),
            fixed(&["AAA-USD", "BBB-USD"]),
        );
        if excluded {
            env.quality = exclusions(&[("AAA-USD", "2026-Q4", &[Timeframe::H1])]);
        }
        research(&series, strategies, &universe, &env).unwrap()
    };
    // Témoin : sans exclusion, la limit remplit à D + 20 min.
    let kept = run(false);
    assert_eq!(kept.trades.len(), 2);
    assert_eq!(kept.sim.orders_cancelled, 0);

    let excluded = run(true);
    assert!(
        excluded.trades.iter().all(|trade| trade.entry_ms < d),
        "aucune entrée remplie après l'exclusion"
    );
    assert_eq!(excluded.sim.orders_cancelled, 1);
    assert_eq!(
        excluded
            .research
            .as_ref()
            .unwrap()
            .totals
            .entries_cancelled_universe,
        1
    );
    assert_funnel_identity(&excluded);
}

/// Fenêtre de décision [2026-07-10, 2026-07-20) : AAA est plate et cotée jusqu'à la dernière minute de la
/// fenêtre ; BBB entre au marché au début, puis sa dernière bougie ouvre à `decision_end - 7 j - 60 s +
/// delta`, si bien que l'échéance de son trou de cotation tombe à `decision_end + delta`.
fn run_gap_at_window_end(delta: Ts) -> PortfolioResult {
    let start = at(2026, 7, 10, 0, 0);
    let end = at(2026, 7, 20, 0, 0);
    let last_open = end - 7 * DAY - MINUTE + delta;
    let series = data(vec![
        ("AAA-USD", flat(start, (end - start) / MINUTE, 100.0)),
        (
            "BBB-USD",
            flat(start, (last_open - start) / MINUTE + 1, 100.0),
        ),
    ]);
    let universe = names(&["AAA-USD", "BBB-USD"]);
    let (aaa, _) = Scripted::new(Vec::new());
    let (bbb, _) = Scripted::new(vec![(start + 10 * MINUTE, buy_quote(50.0))]);
    let env = Env::new(start, end, fixed(&["AAA-USD", "BBB-USD"]));
    research(&series, vec![Box::new(aaa), Box::new(bbb)], &universe, &env).unwrap()
}

#[test]
fn une_echeance_de_trou_a_decision_end_donne_une_cloture_forcee_purgee() {
    let end = at(2026, 7, 20, 0, 0);
    // delta = -60 s : l'échéance tombe à decision_end - 60 s, dans la grille. Sortie par trou de
    // cotation, trade conservé.
    let before = run_gap_at_window_end(-MINUTE);
    assert_eq!(before.trades.len(), 1);
    assert_eq!(before.trades[0].exit_reason, ExitReason::QuoteGap);
    assert_eq!(before.trades[0].exit_ms, end - MINUTE);
    let split = split_trades(&before, end).expect("trade conservé");
    assert_eq!((split.kept.len(), split.purged), (1, 0));

    // delta = 0 et +60 s : le trou n'est pas soldé en `quote_gap` à decision_end (ni après). La position
    // passe à la clôture forcée de fin de fenêtre, que la règle de purge retire ; plus d'erreur.
    for delta in [0, MINUTE] {
        let result = run_gap_at_window_end(delta);
        assert_eq!(result.trades.len(), 1, "delta = {delta}");
        assert_eq!(result.trades[0].exit_reason, ExitReason::ForcedClose);
        let split = split_trades(&result, end).expect("trade purgé, sans erreur");
        assert_eq!((split.kept.len(), split.purged), (0, 1), "delta = {delta}");
        assert_funnel_identity(&result);
    }
}

// ---------------------------------------------------------------------------------------------
// Aucun fill dans une minute synthétique (docs S11 §6.4, critère 6)
// ---------------------------------------------------------------------------------------------

/// Minutes synthétiques du trou : `[GAP_FROM, GAP_TO)` minutes après le début de la fenêtre.
const GAP_FROM: i64 = 30;
const GAP_TO: i64 = 40;

/// Bougie réelle de la minute `i` après `first_ms`, volume 10.
fn real_bar(first_ms: Ts, i: i64, high: f64, low: f64) -> Candle {
    Candle {
        open_time_ms: first_ms + i * MINUTE,
        open: 100.0,
        high,
        low,
        close: 100.0,
        volume: 10.0,
        trades: None,
    }
}

/// Dataset de 60 minutes autour de 100, sans les minutes `[GAP_FROM, GAP_TO)`, passé par
/// `zero_volume_marked` : les minutes du trou sont insérées plates au dernier close, à volume nul, et
/// marquées synthétiques. `high_before` et `high_after` sont les plus hauts des bougies réelles avant et
/// après le trou. Rend les colonnes et les instants synthétiques.
fn gapped_market(first_ms: Ts, high_before: f64, high_after: f64) -> (Columns, Vec<Ts>) {
    let candles: Vec<Candle> = (0..60)
        .filter(|i| !(GAP_FROM..GAP_TO).contains(i))
        .map(|i| {
            let high = if i < GAP_FROM {
                high_before
            } else {
                high_after
            };
            real_bar(first_ms, i, high, 99.8)
        })
        .collect();
    let columns = fill_gaps(
        Columns::from_candles(&candles),
        Timeframe::M1,
        GapsPolicy::ZeroVolumeMarked,
    )
    .expect("politique de trous");
    let synthetic: Vec<Ts> = columns
        .open_time_ms()
        .iter()
        .zip(columns.synthetic())
        .filter(|(_, flag)| **flag)
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(synthetic.len() as i64, GAP_TO - GAP_FROM);
    (columns, synthetic)
}

/// Joue `plan` sur AAA-USD (marché à trou) en mode recherche et vérifie qu'aucun fill, d'entrée comme de
/// sortie, n'a eu lieu dans une minute synthétique (la clôture forcée de fin de fenêtre est exclue : elle
/// a lieu à la dernière minute, réelle).
fn play_gapped(
    high_before: f64,
    high_after: f64,
    plan: Vec<(Ts, Signal)>,
) -> (PortfolioResult, Vec<Ts>) {
    let first_ms = at(2026, 7, 10, 0, 0);
    let (columns, synthetic) = gapped_market(first_ms, high_before, high_after);
    let series = vec![("AAA-USD".to_string(), columns)];
    let universe = names(&["AAA-USD"]);
    let (strategy, _) = Scripted::new(plan);
    let env = Env::new(first_ms, first_ms + 60 * MINUTE, fixed(&["AAA-USD"]));
    let result = research(&series, vec![Box::new(strategy)], &universe, &env).unwrap();
    for trade in &result.trades {
        assert!(
            !synthetic.contains(&trade.entry_ms),
            "entrée dans la minute synthétique {}",
            trade.entry_ms
        );
        if !trade.forced_close {
            assert!(
                !synthetic.contains(&trade.exit_ms),
                "sortie dans la minute synthétique {}",
                trade.exit_ms
            );
        }
    }
    (result, synthetic)
}

/// Entrée au marché de 50 de cotation, avec stop et take-profit éventuels.
fn entry_with(stop: Option<f64>, take_profit: Option<f64>) -> Signal {
    Signal::EnterLong {
        size: Size::Quote(50.0),
        kind: OrderKind::Market,
        stop,
        take_profit,
        tag: "entree",
    }
}

#[test]
fn le_mode_recherche_ne_remplit_jamais_dans_une_minute_synthetique() {
    let first_ms = at(2026, 7, 10, 0, 0);
    let minute = |i: i64| first_ms + i * MINUTE;

    // Entrée : décidée à la clôture de la minute 28, elle serait active à la minute 30 (synthétique) ; le
    // premier fill possible est la première 1m réelle après le trou.
    let (entry, _) = play_gapped(100.2, 100.2, vec![(minute(29), buy_quote(50.0))]);
    assert_eq!(entry.trades.len(), 1);
    assert_eq!(entry.trades[0].entry_order_ms, minute(30));
    assert_eq!(entry.trades[0].entry_ms, minute(GAP_TO));

    // Stop : entrée à la minute 22, stop initial à 99,0 (jamais touché). Un `MoveStop` à 100,5, au-dessus
    // du prix plat des minutes synthétiques (100), devient actif à la minute 30, dans le trou : il serait
    // franchi par une synthétique. La sortie a lieu à la première 1m réelle, au stop.
    let (stop, _) = play_gapped(
        100.2,
        100.2,
        vec![
            (minute(21), entry_with(Some(99.0), None)),
            (minute(29), Signal::MoveStop { stop: 100.5 }),
        ],
    );
    assert_eq!(stop.trades.len(), 1);
    let trade = stop.trades[0];
    assert_eq!(trade.entry_ms, minute(22));
    assert_eq!(trade.exit_reason, ExitReason::Stop);
    assert_eq!(trade.exit_ms, minute(GAP_TO));

    // Take-profit : entrée à la minute 22 avec un take-profit à 100,5, jamais atteint avant le trou
    // (plus haut 100,2) ; les synthétiques sont plates à 100. La première 1m réelle après le trou monte à
    // 101 : sortie au take-profit à cette minute, et à aucune minute du trou.
    let (tp, _) = play_gapped(
        100.2,
        101.0,
        vec![(minute(21), entry_with(None, Some(100.5)))],
    );
    assert_eq!(tp.trades.len(), 1);
    let trade = tp.trades[0];
    assert_eq!(trade.entry_ms, minute(22));
    assert_eq!(trade.exit_reason, ExitReason::TakeProfit);
    assert_eq!(trade.exit_ms, minute(GAP_TO));

    // Sortie limit marketable (prix 99,0 sous le marché, comme un take-profit déjà franchi) décidée à la
    // minute 29, active à la minute 30 : elle ne part ni à l'open d'une synthétique ni dans le trou, mais
    // à la première 1m réelle.
    let exit_limit = Signal::ExitLong {
        kind: OrderKind::Limit {
            price: 99.0,
            ttl_bars: 600,
        },
        tag: "sortie",
    };
    let (exit, _) = play_gapped(
        100.2,
        100.2,
        vec![
            (minute(21), entry_with(None, None)),
            (minute(29), exit_limit),
        ],
    );
    assert_eq!(exit.trades.len(), 1);
    assert_eq!(exit.trades[0].entry_ms, minute(22));
    assert_eq!(exit.trades[0].exit_ms, minute(GAP_TO));
    assert!(!exit.trades[0].forced_close);
}

#[test]
fn le_filtre_d_univers_refuse_une_entree_d_une_jambe_non_membre() {
    let from = at(2026, 8, 10, 0, 0);
    let series = data(vec![
        ("AAA-USD", flat(from, 1440, 100.0)),
        ("BBB-USD", flat(from, 1440, 100.0)),
    ]);
    let universe = names(&["AAA-USD", "BBB-USD"]);
    let make = || {
        (0..2)
            .map(|_| Probe::boxed(Timeframe::M1, 5, true).0)
            .collect::<Vec<_>>()
    };
    let mut env = Env::new(
        from,
        from + DAY,
        UniverseSpec::Quarterly {
            rules_hash: "blake3:rules".to_string(),
        },
    );
    env.schedule = Some(schedule(vec![quarter("2026-Q3", &["BBB-USD"])]));
    let result = research(&series, make(), &universe, &env).unwrap();
    let research = result.research.as_ref().unwrap();
    assert!(research.legs[0].counters.entries_not_in_universe > 0);
    assert_eq!(research.legs[1].counters.entries_not_in_universe, 0);
    assert_eq!(result.legs[0].trades, 0, "AAA n'a aucun trade");
    assert!(result.legs[1].trades > 0 || result.legs[1].counters.fills > 0);
    assert_funnel_identity(&result);
}

// ---------------------------------------------------------------------------------------------
// Exclusion qualité par TF de décision
// ---------------------------------------------------------------------------------------------

/// Une paire exclue en 4h et gardée en 1d pour 2026-Q3, sur 4 jours de prix plats.
fn quality_run(tf: Timeframe, excluded: &[Timeframe], quarterly: bool) -> PortfolioResult {
    let from = at(2026, 8, 10, 0, 0);
    let series = data(vec![("AAA-USD", flat(from, 4 * 1440, 100.0))]);
    let universe = names(&["AAA-USD"]);
    let (strategy, _) = Probe::boxed(tf, 1, true);
    let mut env = if quarterly {
        let mut env = Env::new(
            from,
            from + 4 * DAY,
            UniverseSpec::Quarterly {
                rules_hash: "blake3:rules".to_string(),
            },
        );
        env.schedule = Some(schedule(vec![quarter("2026-Q3", &["AAA-USD"])]));
        env
    } else {
        Env::new(from, from + 4 * DAY, fixed(&["AAA-USD"]))
    };
    if !excluded.is_empty() {
        env.quality = exclusions(&[("AAA-USD", "2026-Q3", excluded)]);
    }
    research(&series, vec![strategy], &universe, &env).unwrap()
}

#[test]
fn la_paire_exclue_en_4h_n_entre_pas_en_4h_mais_entre_en_1d() {
    for quarterly in [false, true] {
        let mode = if quarterly { "trimestriel" } else { "fixe" };
        // Témoin : sans exclusion, la stratégie 4h entre.
        let control = quality_run(Timeframe::H4, &[], quarterly);
        assert!(!control.trades.is_empty(), "{mode} : témoin sans trade");
        assert_eq!(
            control
                .research
                .as_ref()
                .unwrap()
                .totals
                .entries_excluded_quality,
            0
        );
        // Exclue en 4h : aucun trade, des refus comptés.
        let excluded = [Timeframe::H4];
        let h4 = quality_run(Timeframe::H4, &excluded, quarterly);
        assert!(
            h4.trades.is_empty(),
            "{mode} : trade en 4h malgré l'exclusion"
        );
        assert!(
            h4.research
                .as_ref()
                .unwrap()
                .totals
                .entries_excluded_quality
                > 0
        );
        assert_funnel_identity(&h4);
        // Gardée en 1d : la même exclusion ne touche pas la stratégie 1d.
        let d1 = quality_run(Timeframe::D1, &excluded, quarterly);
        assert!(
            !d1.trades.is_empty(),
            "{mode} : la stratégie 1d doit entrer"
        );
        assert_eq!(
            d1.research
                .as_ref()
                .unwrap()
                .totals
                .entries_excluded_quality,
            0
        );
        // Le TF de décision lu est celui de la stratégie.
        assert_eq!(d1.research.as_ref().unwrap().decision_tf, Timeframe::D1);
        assert_eq!(h4.research.as_ref().unwrap().decision_tf, Timeframe::H4);
    }
}

#[test]
fn le_1m_n_est_jamais_filtre_par_la_qualite() {
    // Le manifeste ne porte jamais le 1m, mais même une exclusion de tous les autres TF laisse passer
    // une stratégie dont le TF de décision est le 1m.
    let all = [
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::H12,
        Timeframe::D1,
    ];
    let result = quality_run(Timeframe::M1, &all, false);
    assert!(!result.trades.is_empty());
    assert_eq!(
        result
            .research
            .as_ref()
            .unwrap()
            .totals
            .entries_excluded_quality,
        0
    );
}

#[test]
fn une_exclusion_qualite_en_cours_de_route_annule_les_entrees_en_attente() {
    // Une limit d'achat posée à 23:00 par une stratégie 1h reste en attente (son prix est loin sous le
    // marché). À D = 2026-10-01, le 1h devient exclu pour AAA (2026-Q4) : l'ordre est annulé comme à une
    // sortie de l'univers. Sans exclusion, il reste en attente jusqu'à la fin.
    let from = at(2026, 9, 30, 21, 0);
    let series = data(vec![("AAA-USD", flat(from, 5 * 60, 100.0))]);
    let universe = names(&["AAA-USD"]);
    let run = |excluded: bool| {
        let mut env = Env::new(
            at(2026, 9, 30, 23, 0),
            at(2026, 10, 1, 2, 0),
            fixed(&["AAA-USD"]),
        );
        if excluded {
            env.quality = exclusions(&[("AAA-USD", "2026-Q4", &[Timeframe::H1])]);
        }
        research(
            &series,
            vec![Probe::limit(Timeframe::H1, 1, 50.0)],
            &universe,
            &env,
        )
        .unwrap()
    };
    let kept = run(false);
    assert_eq!(kept.sim.orders_cancelled, 0);
    assert_eq!(
        kept.research
            .as_ref()
            .unwrap()
            .totals
            .entries_cancelled_universe,
        0
    );
    assert_eq!(kept.legs[0].funnel.entry_orders, 1);

    let cancelled = run(true);
    assert_eq!(cancelled.legs[0].funnel.entry_orders, 1);
    assert_eq!(cancelled.sim.orders_cancelled, 1);
    let research = cancelled.research.as_ref().unwrap();
    assert_eq!(research.totals.entries_cancelled_universe, 1);
    // Après l'exclusion, la stratégie 1h qui revient à plat voudrait réentrer : ces entrées sont refusées
    // par la qualité.
    assert!(research.totals.entries_excluded_quality > 0);
    assert_funnel_identity(&cancelled);
}

// ---------------------------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------------------------

#[test]
fn les_controles_de_la_recherche_refusent_une_configuration_invalide() {
    let from = at(2026, 8, 10, 0, 0);
    let series = data(vec![("AAA-USD", flat(from, 1440, 100.0))]);
    let universe = names(&["AAA-USD"]);
    let strategy = || vec![Probe::boxed(Timeframe::M1, 5, false).0];
    let instruments = rules(1);

    // Règle de volume nul désactivée.
    let env = Env::new(from, from + DAY, fixed(&["AAA-USD"]));
    let mut cfg: PortfolioConfig<'_> = config(&universe, &instruments, 300.0);
    cfg.sim_rules.zero_volume_no_fill = false;
    let error = run_portfolio_research(
        PortfolioInputs {
            series: &series,
            strategies: strategy(),
            config: cfg,
        },
        &env.spec(),
    )
    .unwrap_err();
    assert!(matches!(error, EngineError::ResearchNeedsZeroVolumeRule));

    // Fenêtre vide, avant le bloc, ou après sa fin.
    for (start, end) in [(from + DAY, from + DAY), (at(2026, 6, 1, 0, 0), from + DAY)] {
        let env = Env::new(start, end, fixed(&["AAA-USD"]));
        let error = research(&series, strategy(), &universe, &env).unwrap_err();
        assert!(
            matches!(error, EngineError::ResearchWindow { .. }),
            "{error}"
        );
    }
    let env = Env {
        identity: identity(at(2025, 4, 1, 0, 0), at(2026, 7, 2, 0, 0), Line::Full),
        ..Env::new(
            at(2025, 4, 1, 0, 0),
            at(2026, 7, 2, 0, 0),
            fixed(&["AAA-USD"]),
        )
    };
    let spec = ResearchSpec {
        block: Block::Holdout,
        ..env.spec()
    };
    let error = run_portfolio_research(
        PortfolioInputs {
            series: &series,
            strategies: strategy(),
            config: config(&universe, &instruments, 300.0),
        },
        &spec,
    )
    .unwrap_err();
    assert!(
        matches!(error, EngineError::ResearchWindow { .. }),
        "{error}"
    );

    // Trimestre absent de l'univers : le dernier trimestre de la fenêtre n'est pas fourni.
    let mut env = Env::new(
        from,
        at(2026, 10, 2, 0, 0),
        UniverseSpec::Quarterly {
            rules_hash: "blake3:rules".to_string(),
        },
    );
    env.schedule = Some(schedule(vec![quarter("2026-Q3", &["AAA-USD"])]));
    let error = research(&series, strategy(), &universe, &env).unwrap_err();
    match error {
        EngineError::UniverseQuarterMissing { quarter } => assert_eq!(quarter, "2026-Q4"),
        other => panic!("erreur inattendue : {other}"),
    }
}

// ---------------------------------------------------------------------------------------------
// config_hash et identité
// ---------------------------------------------------------------------------------------------

#[test]
fn le_config_hash_de_recherche_ne_depend_ni_du_bloc_ni_de_la_ligne() {
    let from = at(2026, 8, 10, 0, 0);
    let series = data(vec![
        ("AAA-USD", flat(from, 1440, 100.0)),
        ("BBB-USD", flat(from, 1440, 50.0)),
    ]);
    let make = |n: usize| {
        (0..n)
            .map(|_| Probe::boxed(Timeframe::M1, 5, false).0)
            .collect::<Vec<_>>()
    };
    let spec_symbols = fixed(&["AAA-USD", "BBB-USD"]);

    // Ligne `full` : les deux jambes.
    let full_universe = names(&["AAA-USD", "BBB-USD"]);
    let full_env = Env::new(from, from + DAY, spec_symbols.clone());
    let full = research(&series, make(2), &full_universe, &full_env).unwrap();

    // Ligne `okx_usdc` : une seule jambe, même configuration gelée (mêmes symboles demandés).
    let mut okx_env = Env::new(from, from + DAY, spec_symbols.clone());
    okx_env.identity = ResearchIdentity {
        line: Line::OkxUsdc,
        subset_hash: Some("blake3:subset".to_string()),
        ..identity(from, from + DAY, Line::Full)
    };
    let okx_universe = names(&["AAA-USD"]);
    let okx = research(&series[..1], make(1), &okx_universe, &okx_env).unwrap();
    assert_eq!(
        full.config_hash, okx.config_hash,
        "même configuration gelée"
    );
    let (full_id, okx_id) = (
        &full.research.as_ref().unwrap().identity_hash,
        &okx.research.as_ref().unwrap().identity_hash,
    );
    assert_ne!(full_id, okx_id, "deux lignes, deux identités");
    assert!(full_id.starts_with("blake3:"));

    // Un autre bloc (HOLDOUT, fenêtre dans le bloc) : même config_hash, autre identité.
    let holdout_start = at(2025, 3, 1, 0, 0);
    let holdout_series = data(vec![
        ("AAA-USD", flat(holdout_start, 1440, 100.0)),
        ("BBB-USD", flat(holdout_start, 1440, 50.0)),
    ]);
    let mut holdout_env = Env::new(holdout_start, holdout_start + DAY, spec_symbols.clone());
    holdout_env.identity.block = Block::Holdout;
    let holdout_spec = ResearchSpec {
        block: Block::Holdout,
        ..holdout_env.spec()
    };
    let instruments = rules(2);
    let holdout = run_portfolio_research(
        PortfolioInputs {
            series: &holdout_series,
            strategies: make(2),
            config: config(&full_universe, &instruments, 300.0),
        },
        &holdout_spec,
    )
    .unwrap();
    assert_eq!(full.config_hash, holdout.config_hash);
    assert_ne!(full_id, &holdout.research.as_ref().unwrap().identity_hash);

    // Les tables par jambe sont vidées dans l'objet haché ; le hash diffère de celui du chemin hérité.
    let json = full.config.canonical_json().unwrap();
    assert!(
        json.contains(r#""research":{"blocks_version":"b1""#),
        "{json}"
    );
    assert!(json.contains(r#""instruments":{}"#), "{json}");
    assert!(json.contains(r#""universe":[]"#), "{json}");
    assert!(
        json.contains(r#""purge_rule":"end_of_window_v1""#),
        "{json}"
    );
    assert!(json.contains(r#""universe_spec":{"mode":"fixed","symbols":["AAA-USD","BBB-USD"]}"#));
    let instruments = rules(2);
    let legacy = run_portfolio(PortfolioInputs {
        series: &series,
        strategies: make(2),
        config: config(&full_universe, &instruments, 300.0),
    })
    .unwrap();
    assert_ne!(legacy.config_hash, full.config_hash);
    assert!(legacy.research.is_none());
    assert!(
        !legacy
            .config
            .canonical_json()
            .unwrap()
            .contains("blocks_version")
    );

    // L'identité distingue aussi les valeurs par jambe (ici les règles d'instrument).
    let other_rules = vec![
        instrument(0.0),
        InstrumentRules::new(0.5, 1e-8, 0.0, 0.0).unwrap(),
    ];
    let other = run_portfolio_research(
        PortfolioInputs {
            series: &series,
            strategies: make(2),
            config: config(&full_universe, &other_rules, 300.0),
        },
        &full_env.spec(),
    )
    .unwrap();
    assert_eq!(other.config_hash, full.config_hash);
    assert_ne!(
        other.research.as_ref().unwrap().identity.legs_hash,
        full.research.as_ref().unwrap().identity.legs_hash
    );
}

// ---------------------------------------------------------------------------------------------
// Profils de coûts
// ---------------------------------------------------------------------------------------------

#[test]
fn les_trois_profils_de_recherche_ont_leurs_compteurs_et_leur_identite() {
    let from = at(2026, 8, 10, 0, 0);
    let series = data(vec![
        ("ETH-USD", common::synthetic_candles(3 * 1440, 7, from, 0.0)),
        ("SOL-USD", common::synthetic_candles(3 * 1440, 8, from, 0.0)),
    ]);
    let file = CostsFile::load(&common::costs_c1_path()).unwrap();
    let universe = names(&["ETH-USD", "SOL-USD"]);
    let instruments = rules(2);
    let params = "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0";
    let set = || {
        (0..2)
            .map(|_| {
                let value = toml::from_str::<toml::Value>(params).unwrap();
                registry::build("sma_cross", &value).unwrap()
            })
            .collect::<Vec<_>>()
    };
    // La décision commence 1 jour après le début des données : la chauffe est jouée.
    let env = Env::new(from + DAY, from + 3 * DAY, fixed(&["ETH-USD", "SOL-USD"]));
    let result = run_portfolio_profiles_research(
        ProfilesInputs {
            series: &series,
            strategies: [set(), set(), set()],
            config: config(&universe, &instruments, 300.0),
            costs: CostSetup {
                file: &file,
                measures: None,
                seed: 42,
            },
        },
        &env.spec(),
        &mut NoopObserver,
    )
    .unwrap();
    let mut identities = Vec::new();
    for run in &result.runs {
        let r = &run.result;
        let research = r.research.as_ref().expect("résultat de recherche");
        assert_eq!(r.window_first_open_ms, from + DAY);
        assert!(research.totals.entries_outside_decision_window > 0);
        assert_funnel_identity(r);
        // Tables par jambe vidées dans le hash s10 aussi.
        let json = r.config.canonical_json().unwrap();
        assert!(json.contains(r#""instruments":{}"#));
        assert!(json.contains(r#""legs":{}"#), "{json}");
        for trade in &r.trades {
            assert!(trade.entry_signal_ms >= from + DAY);
        }
        identities.push(research.identity.legs_hash.clone());
    }
    // Chaque profil a ses valeurs de coûts par jambe, donc son `legs_hash`.
    assert_ne!(identities[0], identities[1]);
    assert_ne!(identities[0], identities[2]);
    assert_ne!(identities[1], identities[2]);
}

// ---------------------------------------------------------------------------------------------
// Consommation des colonnes (docs S11 §12.2, parade 1)
// ---------------------------------------------------------------------------------------------

/// Séries synthétiques de deux jambes (marche inventée) en FORWARD.
fn walk_market(from: Ts) -> Vec<(String, Columns)> {
    data(vec![
        ("ETH-USD", common::synthetic_candles(3 * 1440, 7, from, 0.0)),
        ("SOL-USD", common::synthetic_candles(3 * 1440, 8, from, 0.0)),
    ])
}

fn sma_cross_fast() -> Box<dyn Strategy> {
    let value =
        toml::from_str::<toml::Value>("tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0")
            .unwrap();
    registry::build("sma_cross", &value).unwrap()
}

#[test]
fn la_variante_possedee_donne_le_meme_resultat_au_bit() {
    let from = at(2026, 8, 10, 0, 0);
    let universe = names(&["ETH-USD", "SOL-USD"]);
    let instruments = rules(2);
    let env = Env::new(from + DAY, from + 3 * DAY, fixed(&["ETH-USD", "SOL-USD"]));
    let borrowed_series = walk_market(from);
    let borrowed = run_portfolio_research(
        PortfolioInputs {
            series: &borrowed_series,
            strategies: vec![sma_cross_fast(), sma_cross_fast()],
            config: config(&universe, &instruments, 300.0),
        },
        &env.spec(),
    )
    .unwrap();
    let owned = run_portfolio_research_owned(
        OwnedPortfolioInputs {
            series: walk_market(from),
            strategies: vec![sma_cross_fast(), sma_cross_fast()],
            config: config(&universe, &instruments, 300.0),
        },
        &env.spec(),
    )
    .unwrap();
    assert!(!borrowed.trades.is_empty());
    assert_eq!(borrowed.config_hash, owned.config_hash);
    assert_eq!(borrowed.research, owned.research);
    assert_eq!(borrowed.trades.len(), owned.trades.len());
    assert_eq!(
        borrowed
            .equity
            .iter()
            .map(|e| e.to_bits())
            .collect::<Vec<_>>(),
        owned.equity.iter().map(|e| e.to_bits()).collect::<Vec<_>>()
    );

    // Même chose avec les trois profils de coûts.
    let file = CostsFile::load(&common::costs_c1_path()).unwrap();
    let set = || vec![sma_cross_fast(), sma_cross_fast()];
    let costs = || CostSetup {
        file: &file,
        measures: None,
        seed: 42,
    };
    let profiles_borrowed = run_portfolio_profiles_research(
        ProfilesInputs {
            series: &borrowed_series,
            strategies: [set(), set(), set()],
            config: config(&universe, &instruments, 300.0),
            costs: costs(),
        },
        &env.spec(),
        &mut NoopObserver,
    )
    .unwrap();
    let profiles_owned = run_portfolio_profiles_research_owned(
        OwnedProfilesInputs {
            series: walk_market(from),
            strategies: [set(), set(), set()],
            config: config(&universe, &instruments, 300.0),
            costs: costs(),
        },
        &env.spec(),
        &mut NoopObserver,
    )
    .unwrap();
    for (a, b) in profiles_borrowed.runs.iter().zip(&profiles_owned.runs) {
        assert_eq!(a.result.config_hash, b.result.config_hash);
        assert_eq!(a.result.research, b.result.research);
        assert_eq!(a.result.pnl_net.to_bits(), b.result.pnl_net.to_bits());
    }
}

#[test]
fn la_variante_possedee_garde_les_erreurs_de_chargement() {
    let from = at(2026, 8, 10, 0, 0);
    let universe = names(&["AAA-USD", "ZZZ-USD"]);
    let instruments = rules(2);
    let env = Env::new(from, from + DAY, fixed(&["AAA-USD", "ZZZ-USD"]));
    let error = run_portfolio_research_owned(
        OwnedPortfolioInputs {
            series: data(vec![("AAA-USD", flat(from, 1440, 100.0))]),
            strategies: (0..2)
                .map(|_| Probe::boxed(Timeframe::M1, 5, false).0)
                .collect(),
            config: config(&universe, &instruments, 300.0),
        },
        &env.spec(),
    )
    .unwrap_err();
    assert!(
        matches!(error, EngineError::SymbolNotInDataset { ref symbol } if symbol == "ZZZ-USD"),
        "{error}"
    );
}
