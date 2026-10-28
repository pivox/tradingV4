//! Benchmarks du moteur (docs/03-architecture.md §8).
//!
//! `indicators_1m` : les cinq indicateurs sur 1 051 200 bougies 1m (2 ans), cible < 20 ms chacun.
//! Criterion ne fait aucune assertion de seuil : on relève les médianes avec
//! `cargo bench --locked --bench engine -- indicators_1m`.
//!
//! `engine_loop_2y_1m` (S6) : la boucle 1m complète, simulateur compris, sur 2 ans de 1m SYNTHÉTIQUES
//! (générateur de `tests/common`, sans trou, aucune donnée de marché).
//!
//! * `sma_cross_1m` : `sma_cross` 60/240, cible < 50 ns par 1m ;
//! * `pending_20` : 20 limits d'achat actives et jamais remplies, parcourues à chaque 1m (mesuré,
//!   indication < 150 ns par 1m).
//!
//! `engine_2y_1m` (S8) : la même série synthétique de 2 ans de 1m avec `sma_cross` en 15m, construit par
//! le registre depuis `config/strategies/sma_cross.toml` (la référence de S8). Deux mesures :
//!
//! * `sma_cross_15m_boucle` : la boucle seule, `Mtf` construit hors mesure. Seuil de la CI :
//!   < 50 ns par 1m (docs/03 §8), appliqué à la borne basse de l'intervalle de la médiane par
//!   `.github/scripts/criterion-gate.sh` ;
//! * `sma_cross_15m_cellule` : `run_cell_observed` complet (MTF, indicateurs, boucle, clôture
//!   forcée), informatif.
//!
//! `engine_portfolio_2y_1m_2legs` (S9, docs/specs/s9.md §16.2) : le moteur portefeuille sur deux séries
//! synthétiques de 2 ans de 1m (graines 42 et 43, mêmes horodatages, donc une grille de 1 051 200
//! minutes), `sma_cross` 15m sur chaque jambe, `Book` partagé, rang de liquidité et equity du portefeuille.
//! **Informatif, sans porte** : l'objectif publié est < 120 ns par minute de la grille pour deux jambes,
//! à confirmer sur la machine du propriétaire. Seule la boucle est chronométrée (`LoopObserver`) ;
//! `PortfolioInputs` est reconstruit à chaque itération (stratégies fraîches) et la préparation (MTF,
//! table de rang) est exclue de la mesure. Le débit est exprimé par minute de la grille.
//!
//! `engine_portfolio_profiles_2y_1m_2legs` (S10, docs/specs/s10.md §15.2) : les mêmes deux séries et la
//! même `sma_cross` 15m, jouées par `run_portfolio_profiles` (baseline, adverse, maker, une seule
//! préparation partagée, graine 1, `config/costs.toml`). **Informatif, sans porte** : aucun seuil n'est
//! publié, et la porte criterion de la CI reste celle de `engine_2y_1m`. Les trois boucles sont
//! chronométrées et additionnées (`ChronoCumul`), la préparation (`load` et `share`) est exclue de la
//! mesure, et le débit est exprimé par minute de la grille et par profil (trois fois la grille). Les
//! symboles `AAA-USD` et `BBB-USD` ne sont dans aucune liste de `config/costs.toml` : ils prennent la
//! classe `beyond` par repli, ce qui ne change rien au temps mesuré.
//!
//! Seule la boucle est chronométrée : un `LoopObserver` note l'instant à `loop_started` et à
//! `loop_finished`, la préparation (indicateurs, `Mtf`) est exclue. Commande :
//! `cargo bench --locked --bench engine -- engine_loop_2y_1m` (ou `engine_2y_1m`). `cargo test
//! --all-targets` exécute aussi ces benchs une fois, en mode test, sur 2 jours seulement.

#[path = "../tests/common/mod.rs"]
mod common;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::{Duration, Instant};
use tv4::engine::{
    CellConfig, CostSetup, DEFAULT_MAX_POSITIONS, DEFAULT_QUOTE_GAP_MS, LoopObserver, NoopObserver,
    PortfolioConfig, PortfolioInputs, ProfilesInputs, run_cell_mtf, run_cell_observed,
    run_portfolio_observed, run_portfolio_profiles,
};
use tv4::indicators::{Atr, Ema, Rsi, Sma, Vwap, batch};
use tv4::model::costs::CostsFile;
use tv4::model::{InstrumentRules, Side, Slippage, Timeframe};
use tv4::mtf::Mtf;
use tv4::sim::{SimRules, SizingMode};
use tv4::store::Columns;
use tv4::strategy::{BarContext, IndicatorSpec, Signal, Size, Strategy, registry};

/// 2 ans de bougies 1m (docs/03 §8).
const N: usize = 1_051_200;

fn indicators_1m(c: &mut Criterion) {
    // Données générées une fois, hors mesure ; le temps mesuré inclut l'allocation du `Vec` de sortie.
    let candles = common::synthetic_candles(N, 42, 1_704_067_200_000, 0.0);
    let closes: Vec<f64> = candles.iter().map(|k| k.close).collect();
    let mut group = c.benchmark_group("indicators_1m");
    group.throughput(Throughput::Elements(N as u64));
    group.sample_size(20);
    group.bench_function("sma_200", |b| {
        b.iter(|| {
            batch(
                Sma::new(200).expect("période valide"),
                black_box(&closes).iter().copied(),
            )
        })
    });
    group.bench_function("ema_200", |b| {
        b.iter(|| {
            batch(
                Ema::new(200).expect("période valide"),
                black_box(&closes).iter().copied(),
            )
        })
    });
    group.bench_function("rsi_14", |b| {
        b.iter(|| {
            batch(
                Rsi::new(14).expect("période valide"),
                black_box(&closes).iter().copied(),
            )
        })
    });
    group.bench_function("atr_14", |b| {
        b.iter(|| {
            batch(
                Atr::new(14).expect("période valide"),
                black_box(&candles).iter().copied(),
            )
        })
    });
    group.bench_function("vwap", |b| {
        b.iter(|| batch(Vwap::new(), black_box(&candles).iter().copied()))
    });
    group.finish();
}

/// Deux ans de 1m (docs/03 §8).
const N_DEUX_ANS: usize = 2 * 365 * 1440;

/// Taille de la série de la boucle : deux ans sous `cargo bench` (qui passe `--bench`), deux jours en
/// mode test.
fn taille_boucle() -> usize {
    if std::env::args().any(|a| a == "--bench") {
        N_DEUX_ANS
    } else {
        2 * 1440
    }
}

/// Chronomètre de la boucle : durée entre `loop_started` et `loop_finished`.
#[derive(Default)]
struct Chrono {
    debut: Option<Instant>,
    duree: Duration,
}

impl LoopObserver for Chrono {
    fn loop_started(&mut self) {
        self.debut = Some(Instant::now());
    }

    fn loop_finished(&mut self) {
        if let Some(debut) = self.debut.take() {
            self.duree = debut.elapsed();
        }
    }
}

/// Chronomètre cumulé : somme des durées de toutes les boucles d'un appel (trois pour les profils).
#[derive(Default)]
struct ChronoCumul {
    debut: Option<Instant>,
    duree: Duration,
}

impl LoopObserver for ChronoCumul {
    fn loop_started(&mut self) {
        self.debut = Some(Instant::now());
    }

    fn loop_finished(&mut self) {
        if let Some(debut) = self.debut.take() {
            self.duree += debut.elapsed();
        }
    }
}

/// Stratégie de bench : au premier appel, pose 20 limits d'achat à la moitié du close (jamais
/// remplies), puis ne fait plus rien. Le simulateur parcourt donc 20 ordres actifs à chaque 1m.
struct Pending20 {
    posee: bool,
}

impl Strategy for Pending20 {
    fn id(&self) -> &'static str {
        "pending_20"
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
        if self.posee {
            return;
        }
        self.posee = true;
        let Some(derniere) = ctx.last(Timeframe::M1) else {
            return;
        };
        for slot in 1..=20_u16 {
            out.push(Signal::PlaceLimit {
                side: Side::Buy,
                price: 0.5 * derniere.close,
                size: Size::Quote(1.0),
                ttl_bars: u32::MAX,
                slot,
                tag: "bench",
            });
        }
    }
}

fn engine_loop_2y_1m(c: &mut Criterion) {
    let n = taille_boucle();
    let candles = common::synthetic_candles(n, 42, common::T0, 0.0);
    let mtf = Mtf::build(&candles, &[Timeframe::M1]).expect("mtf");
    let config = CellConfig {
        symbol: "SYNTH",
        fees: common::okx_fees(),
        slippage: common::slippage(5.0),
        initial_cash: 1.0e6,
        // Volumes synthétiques minuscules : la règle de part de volume rejetterait tous les achats.
        sim_rules: SimRules {
            max_volume_share: 1.0e9,
            ..SimRules::default()
        },
        instrument: InstrumentRules::new(1e-4, 1e-8, 0.0, 0.0).expect("instrument valide"),
    };
    // Garde-fou, hors mesure : les 20 limits sont bien acceptées et restent en attente jusqu'à la fin.
    {
        let courte = Mtf::build(&candles[..2000], &[Timeframe::M1]).expect("mtf court");
        let resultat = run_cell_mtf(
            &courte,
            Box::new(Pending20 { posee: false }),
            &config,
            &mut Chrono::default(),
        )
        .expect("run valide");
        assert_eq!(resultat.rejections.total(), 0);
        assert_eq!(resultat.sim.orders_unfilled_at_end, 20);
        assert_eq!(resultat.sim.fills, 0);
    }
    let mut group = c.benchmark_group("engine_loop_2y_1m");
    group.throughput(Throughput::Elements(n as u64));
    group.sample_size(20);
    group.bench_function("sma_cross_1m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut chrono = Chrono::default();
                let resultat = run_cell_mtf(&mtf, common::sma_cross_1m(), &config, &mut chrono)
                    .expect("run valide");
                black_box(resultat.trades.len());
                total += chrono.duree;
            }
            total
        })
    });
    group.bench_function("pending_20", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut chrono = Chrono::default();
                let resultat = run_cell_mtf(
                    &mtf,
                    Box::new(Pending20 { posee: false }),
                    &config,
                    &mut chrono,
                )
                .expect("run valide");
                black_box(resultat.sim.orders_unfilled_at_end);
                total += chrono.duree;
            }
            total
        })
    });
    group.finish();
}

/// Paramètres de référence de S8 : le fichier lu par `tv4 backtest --params`.
const SMA_CROSS_TOML: &str = include_str!("../../config/strategies/sma_cross.toml");

/// Une `sma_cross` fraîche, construite par le registre depuis `config/strategies/sma_cross.toml`.
fn sma_cross_reference() -> Box<dyn Strategy> {
    let params: toml::Value = toml::from_str(SMA_CROSS_TOML).expect("sma_cross.toml valide");
    registry::build("sma_cross", &params).expect("sma_cross construite par le registre")
}

fn engine_2y_1m(c: &mut Criterion) {
    let n = taille_boucle();
    let candles = common::synthetic_candles(n, 42, common::T0, 0.0);
    let declared = sma_cross_reference().timeframes().to_vec();
    assert_eq!(declared, [Timeframe::M15], "la référence de S8 est en 15m");
    let mtf = Mtf::build(&candles, &declared).expect("mtf");
    let config = CellConfig {
        symbol: "SYNTH",
        fees: common::okx_fees(),
        slippage: common::slippage(5.0),
        initial_cash: 1.0e6,
        // Volumes synthétiques minuscules : la règle de part de volume rejetterait tous les achats.
        sim_rules: SimRules {
            max_volume_share: 1.0e9,
            ..SimRules::default()
        },
        instrument: InstrumentRules::new(1e-4, 1e-8, 0.0, 0.0).expect("instrument valide"),
    };
    let mut group = c.benchmark_group("engine_2y_1m");
    group.throughput(Throughput::Elements(n as u64));
    group.sample_size(20);
    group.bench_function("sma_cross_15m_boucle", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut chrono = Chrono::default();
                let resultat = run_cell_mtf(&mtf, sma_cross_reference(), &config, &mut chrono)
                    .expect("run valide");
                black_box(resultat.trades.len());
                total += chrono.duree;
            }
            total
        })
    });
    group.bench_function("sma_cross_15m_cellule", |b| {
        b.iter(|| {
            let resultat = run_cell_observed(
                black_box(&candles),
                sma_cross_reference(),
                &config,
                &mut NoopObserver,
            )
            .expect("run valide");
            black_box(resultat.trades.len())
        })
    });
    group.finish();
}

/// Portefeuille à deux jambes (S9 §16.2) : informatif, sans porte.
fn engine_portfolio_2y_1m_2legs(c: &mut Criterion) {
    let n = taille_boucle();
    let universe = vec!["AAA-USD".to_string(), "BBB-USD".to_string()];
    let series = vec![
        (
            universe[0].clone(),
            Columns::from_candles(&common::synthetic_candles(n, 42, common::T0, 0.0)),
        ),
        (
            universe[1].clone(),
            Columns::from_candles(&common::synthetic_candles(n, 43, common::T0, 0.0)),
        ),
    ];
    let instruments =
        vec![InstrumentRules::new(1e-4, 1e-8, 0.0, 0.0).expect("instrument valide"); 2];
    let config = || PortfolioConfig {
        universe: &universe,
        regime_symbol: "BTC-USD",
        fees: common::okx_fees(),
        slippage: common::slippage(5.0),
        initial_cash: 1.0e6,
        // Volumes synthétiques minuscules : la règle de part de volume rejetterait tous les achats.
        sim_rules: SimRules {
            max_volume_share: 1.0e9,
            ..SimRules::default()
        },
        instruments: &instruments,
        instruments_hash: None,
        max_positions: DEFAULT_MAX_POSITIONS,
        sizing: SizingMode::Research,
        min_order_bump_max_risk: None,
        quote_gap_ms: DEFAULT_QUOTE_GAP_MS,
        min_trades: 30,
    };
    let mut group = c.benchmark_group("engine_portfolio_2y_1m_2legs");
    group.throughput(Throughput::Elements(n as u64));
    group.sample_size(10);
    group.bench_function("sma_cross_15m_boucle", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut chrono = Chrono::default();
                let resultat = run_portfolio_observed(
                    PortfolioInputs {
                        series: &series,
                        strategies: vec![sma_cross_reference(), sma_cross_reference()],
                        config: config(),
                    },
                    &mut chrono,
                )
                .expect("run valide");
                black_box(resultat.trades.len());
                total += chrono.duree;
            }
            total
        })
    });
    group.finish();
}

/// Trois profils de coûts sur deux jambes (S10 §15.2) : informatif, sans porte.
fn engine_portfolio_profiles_2y_1m_2legs(c: &mut Criterion) {
    let n = taille_boucle();
    let universe = vec!["AAA-USD".to_string(), "BBB-USD".to_string()];
    let series = vec![
        (
            universe[0].clone(),
            Columns::from_candles(&common::synthetic_candles(n, 42, common::T0, 0.0)),
        ),
        (
            universe[1].clone(),
            Columns::from_candles(&common::synthetic_candles(n, 43, common::T0, 0.0)),
        ),
    ];
    let instruments =
        vec![InstrumentRules::new(1e-4, 1e-8, 0.0, 0.0).expect("instrument valide"); 2];
    let costs = CostsFile::from_toml_str(include_str!("../../config/costs.toml"))
        .expect("config/costs.toml valide");
    let config = || PortfolioConfig {
        universe: &universe,
        regime_symbol: "BTC-USD",
        fees: common::okx_fees(),
        // Le slippage scalaire est refusé avec les profils : le slippage vient du fichier de coûts.
        slippage: Slippage::ZERO,
        initial_cash: 1.0e6,
        // Volumes synthétiques minuscules : la règle de part de volume rejetterait tous les achats.
        sim_rules: SimRules {
            max_volume_share: 1.0e9,
            ..SimRules::default()
        },
        instruments: &instruments,
        instruments_hash: None,
        max_positions: DEFAULT_MAX_POSITIONS,
        sizing: SizingMode::Research,
        min_order_bump_max_risk: None,
        quote_gap_ms: DEFAULT_QUOTE_GAP_MS,
        min_trades: 30,
    };
    let jeu = || vec![sma_cross_reference(), sma_cross_reference()];
    let mut group = c.benchmark_group("engine_portfolio_profiles_2y_1m_2legs");
    // Trois boucles par appel : le débit est rapporté par minute de la grille et par profil.
    group.throughput(Throughput::Elements(3 * n as u64));
    group.sample_size(10);
    group.bench_function("sma_cross_15m_boucles", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut chrono = ChronoCumul::default();
                let resultat = run_portfolio_profiles(
                    ProfilesInputs {
                        series: &series,
                        strategies: [jeu(), jeu(), jeu()],
                        config: config(),
                        costs: CostSetup {
                            file: &costs,
                            measures: None,
                            seed: 1,
                        },
                    },
                    &mut chrono,
                )
                .expect("run valide");
                black_box(resultat.runs[0].result.trades.len());
                total += chrono.duree;
            }
            total
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    indicators_1m,
    engine_loop_2y_1m,
    engine_2y_1m,
    engine_portfolio_2y_1m_2legs,
    engine_portfolio_profiles_2y_1m_2legs
);
criterion_main!(benches);
