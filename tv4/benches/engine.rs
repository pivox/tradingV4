//! Benchmarks du moteur (docs/03-architecture.md §8).
//!
//! `indicators_1m` : les cinq indicateurs sur 1 051 200 bougies 1m (2 ans), cible < 20 ms chacun.
//! Criterion ne fait aucune assertion de seuil : on relève les médianes avec
//! `cargo bench --locked --bench engine -- indicators_1m`.

#[path = "../tests/common/mod.rs"]
mod common;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use tv4::indicators::{Atr, Ema, Rsi, Sma, Vwap, batch};

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

criterion_group!(benches, indicators_1m);
criterion_main!(benches);
