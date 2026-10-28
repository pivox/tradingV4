//! Allocateur compteur des trois profils de coûts (docs/specs/s10.md §15.2) : la boucle de la grille de
//! **chacun** des trois profils, sur deux jambes, ne fait pas plus d'allocations que celle du portefeuille de
//! S9 (`portfolio_allocations.rs`) : au plus 1 pour 10 000 minutes de la grille. Les tirages (stop, fill
//! maker) n'allouent pas : les clés sont des `[u64; 4]` sur la pile et la sélection adverse est un calcul.
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
    CostSetup, LoopObserver, PortfolioConfig, ProfilesInputs, run_portfolio_profiles,
};
use tv4::model::costs::CostsFile;
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

/// Compte les allocations faites par le thread courant entre `loop_started` et `loop_finished`, une
/// mesure par boucle : `run_portfolio_profiles` appelle l'observateur autour de chacun des trois profils.
#[derive(Default)]
struct Probe {
    per_loop: Vec<u64>,
}

impl LoopObserver for Probe {
    fn loop_started(&mut self) {
        COUNT.with(|count| count.set(0));
        COUNTING.with(|flag| flag.set(true));
    }

    fn loop_finished(&mut self) {
        COUNTING.with(|flag| flag.set(false));
        self.per_loop.push(COUNT.with(Cell::get));
    }
}

fn strategy(text: &str) -> Box<dyn Strategy> {
    let value = toml::from_str::<toml::Value>(text).expect("TOML valide");
    registry::build("sma_cross", &value).expect("stratégie valide")
}

/// Deux jambes de 10 240 minutes (warmup de 240), `sma_cross` ; rend (allocations de la boucle de chaque
/// profil, minutes de la grille, trades de chaque profil).
fn measure(
    mode: SizingMode,
    params: &str,
    warmup: usize,
    rows: usize,
) -> (Vec<u64>, usize, [usize; 3]) {
    let universe = names(&["BTC-USD", "ETH-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let series = vec![
        (
            "BTC-USD".to_string(),
            Columns::from_candles(&common::synthetic_candles(
                rows + warmup,
                7,
                common::T0,
                0.0,
            )),
        ),
        (
            "ETH-USD".to_string(),
            Columns::from_candles(&common::synthetic_candles(
                rows + warmup,
                8,
                common::T0,
                0.0,
            )),
        ),
    ];
    let file = CostsFile::load(&common::repo_path("config/costs.toml")).expect("config/costs.toml");
    let set = || vec![strategy(params), strategy(params)];
    let mut probe = Probe::default();
    let result = run_portfolio_profiles(
        ProfilesInputs {
            series: &series,
            strategies: [set(), set(), set()],
            config: PortfolioConfig {
                sizing: mode,
                ..config(&universe, &instruments, 300.0)
            },
            costs: CostSetup {
                file: &file,
                measures: None,
                seed: 1,
            },
        },
        &mut probe,
    )
    .expect("profils valides");
    let trades = [
        result.runs[0].result.trades.len(),
        result.runs[1].result.trades.len(),
        result.runs[2].result.trades.len(),
    ];
    (probe.per_loop, result.runs[0].result.grid_minutes, trades)
}

#[test]
fn les_trois_boucles_allouent_au_plus_une_fois_en_recherche() {
    let (allocations, grid, trades) = measure(
        SizingMode::Research,
        "tf = \"1m\"\nfast = 60\nslow = 240\nstop_atr_mult = 2.0",
        240,
        10_000,
    );
    assert_eq!(grid, 10_000);
    assert_eq!(allocations.len(), 3, "une mesure par profil");
    // Le chemin des fills, des stops tirés et des tirages doit être exercé (baseline et adverse).
    assert!(trades[0] > 0 && trades[1] > 0, "trades : {trades:?}");
    println!(
        "allocations par boucle (baseline, adverse, maker), recherche : {allocations:?}, trades {trades:?}"
    );
    for (profile, count) in allocations.iter().enumerate() {
        assert!(
            *count <= 1,
            "profil {profile} : {count} allocations dans la boucle"
        );
    }
}

#[test]
fn les_trois_boucles_allouent_au_plus_une_fois_en_capitalise() {
    let (allocations, grid, trades) = measure(
        SizingMode::Compounding,
        "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0",
        20,
        10_000,
    );
    assert_eq!(grid, 10_000);
    assert_eq!(allocations.len(), 3);
    assert!(trades[0] > 0 && trades[1] > 0, "trades : {trades:?}");
    println!(
        "allocations par boucle (baseline, adverse, maker), capitalisé : {allocations:?}, trades {trades:?}"
    );
    for (profile, count) in allocations.iter().enumerate() {
        assert!(
            *count <= 1,
            "profil {profile} : {count} allocations dans la boucle"
        );
    }
}

#[test]
fn les_trois_boucles_multi_timeframe_allouent_au_plus_une_fois() {
    // 15m + filtre de tendance 1h : deux timeframes déclarés par jambe.
    let (allocations, grid, _) = measure(
        SizingMode::Research,
        "tf = \"15m\"\nfast = 4\nslow = 12\nstop_atr_mult = 2.0\n[trend]\ntf = \"1h\"\nsma = 6",
        0,
        10_000,
    );
    assert!(grid > 5_000, "grille de {grid} minutes");
    assert_eq!(allocations.len(), 3);
    println!("allocations par boucle (baseline, adverse, maker), 15m + 1h : {allocations:?}");
    for (profile, count) in allocations.iter().enumerate() {
        assert!(
            *count <= 1,
            "profil {profile} : {count} allocations dans la boucle"
        );
    }
}
