//! Sous-commande `tv4 bench` (S8) : coût de la boucle par bougie 1m et coût moyen par appel de
//! `Strategy::on_bar` (docs/03-architecture.md §8 et §15.1).
//!
//! Mêmes entrées que `tv4 backtest` ([`RunInputArgs`]) ; rien n'est écrit sur disque. La CLI est un des
//! rares endroits qui lisent l'horloge : `engine` et `strategy` restent sans horloge, la boucle est
//! chronométrée par un [`LoopObserver`] et `on_bar` par une enveloppe de stratégie ([`TimedStrategy`]).
//!
//! Déroulement :
//!
//! 1. chargement et préparation une fois ;
//! 2. N runs (`--repeat`) de `run_cell_observed` : préparation (MTF, indicateurs), boucle et fin, en
//!    médiane (valeur basse pour N pair) ; `ns/1m` = boucle / bougies de la fenêtre ;
//! 3. N runs avec la stratégie enveloppée : durée cumulée et nombre d'appels de `on_bar` ;
//! 4. surcoût du chronomètre : coût moyen d'un intervalle vide `Instant::now()` / `elapsed()` sur un
//!    million de paires, soustrait du coût mesuré par appel ;
//! 5. contrôles : le nombre d'appels égale `funnel.evaluations`, et l'enveloppe ne change ni le
//!    `config_hash` ni le nombre de trades.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Args;

use super::CliError;
use super::backtest::{PreparedRun, RunInputArgs, prepare};
use crate::engine::{self, LoopObserver};
use crate::model::Timeframe;
use crate::strategy::{BarContext, IndicatorSpec, Signal, Strategy, StrategyFunnel};

/// Nombre de paires d'horloge utilisées pour mesurer le surcoût du chronomètre.
const CLOCK_PAIRS: u32 = 1_000_000;
/// Répétitions par défaut.
const DEFAULT_REPEAT: u32 = 5;

/// Arguments de `tv4 bench`.
#[derive(Debug, Args)]
pub struct BenchArgs {
    #[command(flatten)]
    pub inputs: RunInputArgs,
    /// Nombre de répétitions de chaque mesure (la médiane est affichée).
    #[arg(
        long,
        value_name = "N",
        default_value_t = DEFAULT_REPEAT,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub repeat: u32,
}

/// Durées d'un run : préparation, boucle et fin.
#[derive(Clone, Copy, Debug, Default)]
struct RunTimes {
    preparation: Duration,
    boucle: Duration,
    fin: Duration,
}

/// Observateur qui note l'instant de l'appel, du début et de la fin de la boucle.
struct PhaseTimer {
    appel: Instant,
    debut: Option<Instant>,
    fin: Option<Instant>,
}

impl PhaseTimer {
    fn new() -> PhaseTimer {
        PhaseTimer {
            appel: Instant::now(),
            debut: None,
            fin: None,
        }
    }

    /// Durées des trois phases, `retour` étant l'instant du retour de `run_cell_observed`.
    fn times(&self, retour: Instant) -> RunTimes {
        let (Some(debut), Some(fin)) = (self.debut, self.fin) else {
            return RunTimes::default();
        };
        RunTimes {
            preparation: debut.duration_since(self.appel),
            boucle: fin.duration_since(debut),
            fin: retour.duration_since(fin),
        }
    }
}

impl LoopObserver for PhaseTimer {
    fn loop_started(&mut self) {
        self.debut = Some(Instant::now());
    }

    fn loop_finished(&mut self) {
        self.fin = Some(Instant::now());
    }
}

/// Cumuls de [`TimedStrategy`], lisibles après que le moteur a pris la stratégie.
#[derive(Debug, Default)]
struct OnBarStats {
    calls: AtomicU64,
    nanos: AtomicU64,
}

/// Enveloppe qui délègue tout à la stratégie et chronomètre chaque `on_bar`.
///
/// L'accumulation se fait après la lecture de l'horloge de fin : elle n'entre pas dans l'intervalle
/// mesuré, dont le seul surcoût est celui du chronomètre lui-même.
struct TimedStrategy {
    inner: Box<dyn Strategy>,
    stats: Arc<OnBarStats>,
}

impl Strategy for TimedStrategy {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn version(&self) -> &'static str {
        self.inner.version()
    }

    fn params_json(&self) -> serde_json::Value {
        self.inner.params_json()
    }

    fn timeframes(&self) -> &[Timeframe] {
        self.inner.timeframes()
    }

    fn indicators(&self) -> Vec<IndicatorSpec> {
        self.inner.indicators()
    }

    fn warmup_bars(&self) -> usize {
        self.inner.warmup_bars()
    }

    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        let debut = Instant::now();
        self.inner.on_bar(ctx, out);
        let ecoule = debut.elapsed();
        self.stats.calls.fetch_add(1, Ordering::Relaxed);
        self.stats.nanos.fetch_add(
            u64::try_from(ecoule.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn funnel(&self) -> Option<StrategyFunnel> {
        self.inner.funnel()
    }
}

/// Résultat d'un run chronométré.
struct TimedRun {
    times: RunTimes,
    config_hash: String,
    n_trades: usize,
    evaluations: u64,
    bars: usize,
}

/// Un run non enveloppé, avec les durées de ses trois phases.
fn timed_run(prepared: &PreparedRun) -> Result<TimedRun, CliError> {
    let strategy = prepared.build_strategy()?;
    let config = prepared.cell_config();
    let mut timer = PhaseTimer::new();
    let result = engine::run_cell_observed(&prepared.candles, strategy, &config, &mut timer)?
        .with_min_trades(prepared.min_trades)?;
    let times = timer.times(Instant::now());
    Ok(TimedRun {
        times,
        config_hash: result.config_hash,
        n_trades: result.trades.len(),
        evaluations: result.funnel.evaluations,
        bars: result.bars_in_window,
    })
}

/// Résultat d'un run avec la stratégie enveloppée.
struct WrappedRun {
    calls: u64,
    nanos: u64,
    config_hash: String,
    n_trades: usize,
    evaluations: u64,
}

/// Un run avec la stratégie enveloppée.
fn wrapped_run(prepared: &PreparedRun) -> Result<WrappedRun, CliError> {
    let stats = Arc::new(OnBarStats::default());
    let strategy = Box::new(TimedStrategy {
        inner: prepared.build_strategy()?,
        stats: Arc::clone(&stats),
    });
    let config = prepared.cell_config();
    let result = engine::run_cell(&prepared.candles, strategy, &config)?
        .with_min_trades(prepared.min_trades)?;
    Ok(WrappedRun {
        calls: stats.calls.load(Ordering::Relaxed),
        nanos: stats.nanos.load(Ordering::Relaxed),
        config_hash: result.config_hash,
        n_trades: result.trades.len(),
        evaluations: result.funnel.evaluations,
    })
}

/// Médiane d'une liste non vide (valeur basse pour une longueur paire).
fn median<T: Copy + Ord>(values: &mut [T]) -> Option<T> {
    values.sort();
    values.get(values.len().saturating_sub(1) / 2).copied()
}

/// Coût moyen, en nanosecondes, d'un intervalle vide entre `Instant::now()` et `elapsed()`.
fn clock_overhead_ns() -> f64 {
    let mut total = Duration::ZERO;
    for _ in 0..CLOCK_PAIRS {
        let debut = black_box(Instant::now());
        total += debut.elapsed();
    }
    total.as_secs_f64() * 1e9 / f64::from(CLOCK_PAIRS)
}

/// Millisecondes (fractionnaires) d'une durée.
fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

/// `tv4 bench`.
pub(super) fn run(args: &BenchArgs) -> Result<(), CliError> {
    let t_load = Instant::now();
    let prepared = prepare(&args.inputs)?;
    let load = t_load.elapsed();
    let repeat = usize::try_from(args.repeat).unwrap_or(usize::MAX);

    // Boucle seule : N runs non enveloppés.
    let mut runs = Vec::with_capacity(repeat);
    for _ in 0..repeat {
        runs.push(timed_run(&prepared)?);
    }
    let reference = runs.first().ok_or_else(|| CliError::BenchInconsistent {
        detail: "aucune répétition".to_string(),
    })?;
    let (config_hash, n_trades, evaluations, bars) = (
        reference.config_hash.clone(),
        reference.n_trades,
        reference.evaluations,
        reference.bars,
    );
    for run in &runs {
        if run.config_hash != config_hash || run.n_trades != n_trades {
            return Err(CliError::BenchInconsistent {
                detail: "deux répétitions du même run donnent un résultat différent".to_string(),
            });
        }
    }
    let mut preparations: Vec<Duration> = runs.iter().map(|r| r.times.preparation).collect();
    let mut boucles: Vec<Duration> = runs.iter().map(|r| r.times.boucle).collect();
    let mut fins: Vec<Duration> = runs.iter().map(|r| r.times.fin).collect();
    let preparation = median(&mut preparations).unwrap_or_default();
    let boucle = median(&mut boucles).unwrap_or_default();
    let fin = median(&mut fins).unwrap_or_default();
    let ns_per_bar = boucle.as_secs_f64() * 1e9 / bars.max(1) as f64;

    // on_bar : N runs avec la stratégie enveloppée.
    let mut per_call_ns: Vec<u64> = Vec::with_capacity(repeat);
    let mut calls_seen = 0u64;
    for _ in 0..repeat {
        let wrapped = wrapped_run(&prepared)?;
        let (calls, nanos) = (wrapped.calls, wrapped.nanos);
        if calls != wrapped.evaluations || calls != evaluations {
            return Err(CliError::BenchInconsistent {
                detail: format!(
                    "{calls} appels de on_bar chronométrés pour {evaluations} évaluations de l'entonnoir"
                ),
            });
        }
        if wrapped.config_hash != config_hash || wrapped.n_trades != n_trades {
            return Err(CliError::BenchInconsistent {
                detail: "l'enveloppe chronométrée change le résultat du run".to_string(),
            });
        }
        calls_seen = calls;
        // Nanosecondes par appel au centième près (entier, pour trier sans flottant).
        per_call_ns.push(nanos.saturating_mul(100) / calls.max(1));
    }
    let measured = median(&mut per_call_ns).unwrap_or_default() as f64 / 100.0;
    let overhead = clock_overhead_ns();
    let net = (measured - overhead).max(0.0);

    let strategy = prepared.build_strategy()?;
    println!(
        "tv4 bench : {} {}, {bars} bougies 1m dans la fenêtre, {} répétitions (médianes)",
        strategy.id(),
        strategy.version(),
        args.repeat
    );
    println!("chargement : {:.0} ms", ms(load));
    println!(
        "préparation : {:.1} ms (MTF et indicateurs)   boucle : {:.1} ms soit {ns_per_bar:.1} ns/1m   fin : {:.1} ms",
        ms(preparation),
        ms(boucle),
        ms(fin)
    );
    println!(
        "on_bar : {calls_seen} appels, {measured:.1} ns/appel mesurés, surcoût du chronomètre {overhead:.1} ns/appel, {net:.1} ns/appel net estimé"
    );
    Ok(())
}
