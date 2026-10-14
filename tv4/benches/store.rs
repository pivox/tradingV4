//! Bench de lecture du store (docs/03 §8, S4) : 1 000 000 bougies 1m synthétiques (générateur de
//! `tests/common`, aucune donnée de marché) écrites en fichiers mensuels, relues avec la projection à
//! 6 colonnes. Cible < 0,3 s à chaud. `cargo bench --locked --bench store -- store_read_1m`.
//!
//! La préparation (génération et écriture des fichiers) est hors mesure. `cargo test --all-targets`
//! exécute aussi ce bench une fois, en mode test, préparation comprise.

#[path = "../tests/common/mod.rs"]
mod common;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use tv4::model::{ExchangeId, Timeframe};
use tv4::store::time::MAX_TS_MS;
use tv4::store::{Columns, Partition, Projection, ReadOptions, read_parquet, write_partition};

/// Nombre de bougies lues par itération.
const N: usize = 1_000_000;

fn store_read_1m(c: &mut Criterion) {
    // Symbole `SYN-USD` pour qu'aucun fichier ne passe pour une donnée réelle.
    let dossier = tempfile::tempdir().expect("dossier temporaire");
    let partition = Partition::new(
        ExchangeId::Kraken,
        "SYN-USD".parse().expect("symbole valide"),
        Timeframe::M1,
    );
    let candles = common::synthetic_candles(N, 42, common::T0, 0.0);
    let colonnes = Columns::from_candles(&candles);
    // Dernier instant représentable : tous les mois sont clos.
    let sorties = write_partition(
        dossier.path(),
        &partition,
        &colonnes,
        "synthetic",
        MAX_TS_MS - 1,
    )
    .expect("écriture des fichiers synthétiques");
    let fichiers: Vec<_> = sorties
        .iter()
        .map(|s| {
            s.file_rel
                .split('/')
                .fold(dossier.path().to_path_buf(), |p, c| p.join(c))
        })
        .collect();
    let options = ReadOptions {
        projection: Projection::NoTrades,
        window: None,
        batch_size: 8192,
    };

    let mut group = c.benchmark_group("store_read_1m");
    group.throughput(Throughput::Elements(N as u64));
    group.sample_size(10);
    group.bench_function("six_colonnes_fichiers_mensuels", |b| {
        b.iter(|| {
            let total: usize = fichiers
                .iter()
                .map(|f| read_parquet(black_box(f), &options).expect("lecture").len())
                .sum();
            assert_eq!(total, N);
            total
        })
    });
    group.finish();
}

criterion_group!(benches, store_read_1m);
criterion_main!(benches);
