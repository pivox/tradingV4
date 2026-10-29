//! Allocateur compteur du moteur portefeuille (docs S9 §16.2) : la boucle de la grille sur deux jambes ne
//! doit presque rien allouer (au plus 1 allocation pour 10 000 minutes de la grille : tampons de signaux
//! par jambe, table de rang, vecteurs d'equity et de grille préalloués).
//!
//! Binaire de test dédié : le `#[global_allocator]` ne touche que lui. `unsafe` y est permis (le
//! `#![forbid(unsafe_code)]` de la lib ne s'applique pas aux crates de test). Le compteur est par thread,
//! car le harnais lance les tests en parallèle ; `realloc` compte comme une allocation.

mod common;
mod portfolio_common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use portfolio_common::{config, instrument, names};
use tv4::engine::{
    Line, LoopObserver, PortfolioConfig, PortfolioInputs, ResearchIdentity, ResearchSpec,
    UniverseSpec, run_portfolio_observed, run_portfolio_research_counted,
};
use tv4::model::universe::{Member, UniverseQuarter};
use tv4::model::{Block, QualitySchedule, UniverseSchedule};
use tv4::sim::SizingMode;
use tv4::store::Columns;
use tv4::strategy::{Strategy, registry};

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

fn strategy(text: &str) -> Box<dyn Strategy> {
    let value = toml::from_str::<toml::Value>(text).expect("TOML valide");
    registry::build("sma_cross", &value).expect("stratégie valide")
}

/// Deux jambes de 10 240 minutes (warmup de 240), `sma_cross` ; rend (allocations dans la boucle,
/// minutes de la grille, trades).
fn measure(mode: SizingMode, params: &str, warmup: usize, rows: usize) -> (u64, usize, usize) {
    let universe = names(&["AAA-USD", "BBB-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let series = vec![
        (
            "AAA-USD".to_string(),
            Columns::from_candles(&common::synthetic_candles(
                rows + warmup,
                7,
                common::T0,
                0.0,
            )),
        ),
        (
            "BBB-USD".to_string(),
            Columns::from_candles(&common::synthetic_candles(
                rows + warmup,
                8,
                common::T0,
                0.0,
            )),
        ),
    ];
    let mut probe = Probe::default();
    let result = run_portfolio_observed(
        PortfolioInputs {
            series: &series,
            strategies: vec![strategy(params), strategy(params)],
            config: PortfolioConfig {
                sizing: mode,
                ..config(&universe, &instruments, 300.0)
            },
        },
        &mut probe,
    )
    .expect("run valide");
    (probe.during_loop, result.grid_minutes, result.trades.len())
}

#[test]
fn la_boucle_a_deux_jambes_alloue_au_plus_une_fois_en_recherche() {
    let (allocations, grid, trades) = measure(
        SizingMode::Research,
        "tf = \"1m\"\nfast = 60\nslow = 240\nstop_atr_mult = 2.0",
        240,
        10_000,
    );
    assert_eq!(grid, 10_000);
    assert!(trades > 0, "le chemin des fills doit être exercé");
    println!("allocations dans la boucle (2 jambes, recherche) : {allocations}");
    assert!(allocations <= 1, "{allocations} allocations dans la boucle");
}

#[test]
fn la_boucle_a_deux_jambes_alloue_au_plus_une_fois_en_capitalise() {
    let (allocations, grid, trades) = measure(
        SizingMode::Compounding,
        "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0",
        20,
        10_000,
    );
    assert_eq!(grid, 10_000);
    assert!(trades > 0);
    println!("allocations dans la boucle (2 jambes, capitalisé) : {allocations}");
    assert!(allocations <= 1, "{allocations} allocations dans la boucle");
}

#[test]
fn la_boucle_multi_timeframe_a_deux_jambes_alloue_au_plus_une_fois() {
    // 15m + filtre de tendance 1h : deux timeframes déclarés par jambe.
    let (allocations, grid, _) = measure(
        SizingMode::Research,
        "tf = \"15m\"\nfast = 4\nslow = 12\nstop_atr_mult = 2.0\n[trend]\ntf = \"1h\"\nsma = 6",
        0,
        10_000,
    );
    assert!(grid > 5_000, "grille de {grid} minutes");
    println!("allocations dans la boucle (2 jambes, 15m + 1h) : {allocations}");
    assert!(allocations <= 1, "{allocations} allocations dans la boucle");
}

/// Mode recherche (docs S11 §12.2) : deux jambes de 10 240 minutes posées en FORWARD, univers
/// trimestriel dont BBB sort au 2026-10-01 (l'annulation des entrées en attente est sur le chemin),
/// fenêtre de décision qui commence après la chauffe (le filtre refuse des entrées dans le préfixe).
/// Rend (allocations dans la boucle, minutes enregistrées, trades, refus hors fenêtre, refus univers).
fn measure_research() -> (u64, usize, usize, u64, u64) {
    // 2026-09-28T00:00:00Z : la fenêtre de 7 jours traverse le 2026-10-01.
    let from: i64 = tv4::store::time::days_from_civil(2026, 9, 28) * 86_400_000;
    let rows = 10_000usize;
    let warmup = 240usize;
    let universe = names(&["AAA-USD", "BBB-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let series = vec![
        (
            "AAA-USD".to_string(),
            Columns::from_candles(&common::synthetic_candles(rows + warmup, 7, from, 0.0)),
        ),
        (
            "BBB-USD".to_string(),
            Columns::from_candles(&common::synthetic_candles(rows + warmup, 8, from, 0.0)),
        ),
    ];
    let member = |symbol: &str, rank: u8| Member {
        symbol: symbol.to_string(),
        rank,
    };
    let schedule = UniverseSchedule::new(vec![
        UniverseQuarter::new("2026-Q3", vec![member("AAA-USD", 1), member("BBB-USD", 2)]).unwrap(),
        UniverseQuarter::new("2026-Q4", vec![member("AAA-USD", 1)]).unwrap(),
    ])
    .unwrap();
    let quality = QualitySchedule::default();
    let start = from + 2_000 * 60_000;
    let end = from + (rows + warmup) as i64 * 60_000;
    let universe_spec = UniverseSpec::Quarterly {
        rules_hash: "blake3:rules".to_string(),
    };
    let identity = ResearchIdentity {
        block: Block::Forward,
        decision_start_ms: start,
        decision_end_ms: end,
        universe_window_hash: Some("blake3:window".to_string()),
        quality_hash: "blake3:quality".to_string(),
        line: Line::Full,
        subset_hash: None,
        legs_hash: String::new(),
    };
    let spec = ResearchSpec {
        block: Block::Forward,
        decision_start_ms: start,
        decision_end_ms: end,
        universe: Some(&schedule),
        universe_spec: &universe_spec,
        quality: &quality,
        identity: &identity,
    };
    let params = "tf = \"1m\"\nfast = 60\nslow = 240\nstop_atr_mult = 2.0";
    let mut probe = Probe::default();
    let (result, _) = run_portfolio_research_counted(
        PortfolioInputs {
            series: &series,
            strategies: vec![strategy(params), strategy(params)],
            config: config(&universe, &instruments, 300.0),
        },
        &spec,
        &mut probe,
    )
    .expect("run valide");
    let research = result.research.as_ref().expect("résultat de recherche");
    (
        probe.during_loop,
        result.grid_minutes,
        result.trades.len(),
        research.totals.entries_outside_decision_window,
        research.totals.entries_not_in_universe,
    )
}

#[test]
fn la_boucle_de_recherche_a_deux_jambes_alloue_au_plus_une_fois() {
    let (allocations, grid, trades, outside, not_member) = measure_research();
    assert!(grid > 7_000, "grille enregistrée de {grid} minutes");
    assert!(trades > 0, "le chemin des fills doit être exercé");
    assert!(outside > 0, "le filtre de fenêtre doit être exercé");
    println!(
        "allocations dans la boucle (2 jambes, recherche S11) : {allocations} (refus hors fenêtre {outside}, hors univers {not_member})"
    );
    assert!(allocations <= 1, "{allocations} allocations dans la boucle");
}
