//! Allocateur compteur : la boucle 1m ne doit presque rien allouer (docs/03-architecture.md §7.2 :
//! au plus 1 allocation pour 10 000 bougies).
//!
//! Binaire de test dédié : le `#[global_allocator]` ne touche que lui. `unsafe` y est permis (le
//! `#![forbid(unsafe_code)]` de la lib ne s'applique pas aux crates de test). Le compteur et le
//! drapeau sont par thread, car le harnais lance les tests en parallèle ; `realloc` compte comme une
//! allocation, puisque la croissance d'un `Vec` passe par lui.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::{T0, okx_fees, slippage};
use tv4::engine::{CellConfig, LoopObserver, run_cell_observed};
use tv4::model::Timeframe;
use tv4::sim::SimRules;
use tv4::strategy::{BarContext, IndicatorKind, IndicatorSpec, OrderKind, Signal, Size, Strategy};

struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

/// `try_with` : jamais de panique pendant la destruction des variables locales au thread.
fn record() {
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: chaque méthode délègue telle quelle à `System`, avec les mêmes arguments, après avoir
// seulement incrémenté un compteur local au thread (sans allocation : les `thread_local!` sont des
// `Cell` initialisées en `const`).
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: le contrat de `GlobalAlloc::alloc` est transmis à `System` sans modification.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: idem, contrat de `alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        // SAFETY: idem, `ptr` et `layout` viennent d'une allocation faite par ce même allocateur,
        // c'est-à-dire par `System`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: idem, `ptr` et `layout` viennent d'une allocation faite par `System`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Compte les allocations faites par le thread courant entre `loop_started` et `loop_finished`.
#[derive(Default)]
struct Probe {
    during_loop: u64,
}

impl LoopObserver for Probe {
    fn loop_started(&mut self) {
        COUNT.with(|count| count.set(0));
        COUNTING.with(|flag| flag.set(true));
    }

    fn loop_finished(&mut self) {
        COUNTING.with(|flag| flag.set(false));
        self.during_loop = COUNT.with(Cell::get);
    }
}

fn config() -> CellConfig<'static> {
    CellConfig {
        symbol: "TEST",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: 300.0,
        sim_rules: SimRules::default(),
    }
}

#[test]
fn la_boucle_sur_10_000_bougies_alloue_au_plus_une_fois() {
    // Warmup 240 : exactement 10 000 bougies dans la boucle.
    let candles = common::synthetic_candles(10_240, 7, T0, 0.0);
    let mut probe = Probe::default();
    let result = run_cell_observed(&candles, common::sma_cross_1m(), &config(), &mut probe)
        .expect("run valide");
    assert_eq!(result.bars_in_window, 10_000);
    assert!(
        !result.trades.is_empty(),
        "le chemin des fills doit être exercé"
    );
    println!(
        "allocations dans la boucle (synthétique) : {}",
        probe.during_loop
    );
    assert!(
        probe.during_loop <= 1,
        "{} allocations dans la boucle de 10 000 bougies",
        probe.during_loop
    );
}

#[test]
fn la_boucle_sur_la_fixture_alloue_au_plus_une_fois() {
    let candles = common::fixture_1m();
    let mut probe = Probe::default();
    let result = run_cell_observed(&candles, common::sma_cross_1m(), &config(), &mut probe)
        .expect("run valide");
    assert_eq!(result.bars_in_window, 9_760);
    assert_eq!(result.trades.len(), 23);
    println!(
        "allocations dans la boucle (fixture) : {}",
        probe.during_loop
    );
    assert!(
        probe.during_loop <= 1,
        "{} allocations dans la boucle de la fixture",
        probe.during_loop
    );
}

/// Témoin mutant : alloue à chaque bougie, pour prouver que le compteur voit les allocations.
struct AllocatingStrategy;

impl Strategy for AllocatingStrategy {
    fn id(&self) -> &'static str {
        "allocating"
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
    fn on_bar(&mut self, _ctx: &BarContext<'_>, _out: &mut Vec<Signal>) {
        std::hint::black_box(Vec::<u8>::with_capacity(8));
    }
}

#[test]
fn le_compteur_detecte_une_allocation_par_bougie() {
    let candles = common::synthetic_candles(10_000, 7, T0, 0.0);
    let mut probe = Probe::default();
    run_cell_observed(
        &candles,
        Box::new(AllocatingStrategy),
        &config(),
        &mut probe,
    )
    .expect("run valide");
    assert!(
        probe.during_loop >= 10_000,
        "le compteur n'a vu que {} allocations",
        probe.during_loop
    );
}

/// Stratégie multi-timeframe de test sur `[5m, 1h]` : lit les deux vues et ses colonnes, entre quand
/// elle est à plat et sort quand elle est en position, à chaque clôture de l'un des deux timeframes.
/// Elle n'alloue pas elle-même.
struct MultiTf;

impl Strategy for MultiTf {
    fn id(&self) -> &'static str {
        "multi_tf"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M5, Timeframe::H1]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        vec![
            IndicatorSpec {
                name: "sma3",
                tf: Timeframe::M5,
                kind: IndicatorKind::Sma { period: 3 },
            },
            IndicatorSpec {
                name: "atr3",
                tf: Timeframe::H1,
                kind: IndicatorKind::Atr { period: 3 },
            },
        ]
    }
    fn warmup_bars(&self) -> usize {
        3
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        let (Some(sma), Some(atr), Some(last)) = (
            ctx.ind(Timeframe::M5, "sma3").last(),
            ctx.ind(Timeframe::H1, "atr3").last(),
            ctx.last(Timeframe::M5),
        ) else {
            return;
        };
        std::hint::black_box((sma, atr, ctx.candles(Timeframe::H1).len()));
        if ctx.position().is_none() {
            out.push(Signal::EnterLong {
                size: Size::RiskPct(0.01),
                kind: OrderKind::Market,
                stop: Some(last.close * 0.99),
                take_profit: None,
                tag: "entree",
            });
        } else {
            out.push(Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "sortie",
            });
        }
    }
}

#[test]
fn la_boucle_multi_tf_alloue_au_plus_une_fois() {
    // Données synthétiques sans trou : 10 000 minutes jouées après un warmup de 3 H1 (180 minutes) ;
    // `Scripted`-like `MultiTf` sur [5m, 1h] avec une SMA 5m et un ATR 1h.
    let candles = common::synthetic_candles(10_180, 9, T0, 0.0);
    let mut probe = Probe::default();
    let result = run_cell_observed(&candles, Box::new(MultiTf), &config(), &mut probe)
        .expect("run multi-TF valide");
    assert_eq!(result.start_index, 180);
    assert_eq!(result.bars_in_window, 10_000);
    assert!(
        !result.trades.is_empty(),
        "le chemin des fills doit être exercé"
    );
    println!(
        "allocations dans la boucle ([5m, 1h] synthétique) : {}",
        probe.during_loop
    );
    assert!(
        probe.during_loop <= 1,
        "{} allocations dans la boucle multi-TF de 10 000 bougies",
        probe.during_loop
    );
}

#[test]
fn la_boucle_sma_cross_15m_sur_la_fixture_alloue_au_plus_une_fois() {
    let candles = common::fixture_1m();
    let strategy = common::strategy_from_file("sma_cross", "config/strategies/sma_cross.toml");
    let mut probe = Probe::default();
    let result =
        run_cell_observed(&candles, strategy, &config(), &mut probe).expect("run 15m valide");
    assert!(!result.trades.is_empty());
    println!(
        "allocations dans la boucle (fixture, 15m) : {}",
        probe.during_loop
    );
    assert!(
        probe.during_loop <= 1,
        "{} allocations dans la boucle de la fixture en 15m",
        probe.during_loop
    );
}
