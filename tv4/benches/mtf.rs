//! Bench du multi-timeframe (docs/03 §8, S5) : resampling 1m vers 1h de 2 ans de bougies 1m
//! **synthétiques** (1 051 200 bougies sans trou, générateur de `tests/common`, aucune donnée de
//! marché). Cible < 30 ms. `cargo bench --locked --bench mtf`.
//!
//! * `mtf_resample_1h_2y` : `Mtf::build(&m1, &[H1])`, soit agrégation, `closed_count` et `just_closed`.
//! * `mtf_aggregate_1h_2y` : `aggregate(&m1, H1)` seul.
//!
//! `cargo test --all-targets` exécute aussi ce bench une fois, en mode test, sur 2 jours seulement.

#[path = "../tests/common/mod.rs"]
mod common;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use tv4::model::Timeframe;
use tv4::mtf::{Mtf, aggregate};

/// Deux ans de 1m.
const N_DEUX_ANS: usize = 2 * 365 * 1440;

/// Taille de la série : deux ans sous `cargo bench` (qui passe `--bench`), deux jours en mode test.
fn taille() -> usize {
    if std::env::args().any(|a| a == "--bench") {
        N_DEUX_ANS
    } else {
        2 * 1440
    }
}

fn mtf_resample(c: &mut Criterion) {
    let n = taille();
    let m1 = common::synthetic_candles(n, 42, common::T0, 0.0);
    let mut group = c.benchmark_group("mtf_resample_1h_2y");
    group.throughput(Throughput::Elements(n as u64));
    group.sample_size(30);
    group.bench_function("build_h1", |b| {
        b.iter(|| {
            let mtf = Mtf::build(black_box(&m1), &[Timeframe::H1]).expect("mtf");
            black_box(mtf.len())
        })
    });
    group.bench_function("aggregate_h1", |b| {
        b.iter(|| black_box(aggregate(black_box(&m1), Timeframe::H1).len()))
    });
    group.finish();
}

criterion_group!(benches, mtf_resample);
criterion_main!(benches);
