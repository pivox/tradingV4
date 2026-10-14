//! Bench d'ingestion Kraken (docs/03 §8, S5) : `tv4 ingest kraken` sur un zip **synthétique** de 2 ans
//! de 1m (1 051 200 lignes au format Kraken : secondes, prix à une décimale, volume à 8 décimales,
//! nombre de transactions entier) avec un `MANIFEST.json` synthétique. Aucune donnée de marché.
//! `cargo bench --locked --bench ingest`.
//!
//! La préparation du zip est hors mesure. Chaque itération ingère dans une racine temporaire neuve,
//! sans le SHA-256 déjà connu du zip (le calcul fait partie de l'ingestion mesurée). `cargo test
//! --all-targets` exécute aussi ce bench une fois, en mode test, sur 10 000 lignes seulement.

#[path = "../tests/common/mod.rs"]
mod common;

use std::fmt::Write as _;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tv4::cli::{IngestKrakenArgs, ingest_kraken};

/// Deux ans de 1m.
const N_DEUX_ANS: usize = 2 * 365 * 1440;

/// Début des lignes synthétiques : 2023-01-01T00:00:00Z, en secondes.
const DEBUT_S: i64 = 1_672_531_200;

fn taille() -> usize {
    if std::env::args().any(|a| a == "--bench") {
        N_DEUX_ANS
    } else {
        10_000
    }
}

/// CSV Kraken **synthétique** : une marche déterministe autour de 100, sans trou.
fn csv_synthetique(n: usize) -> String {
    let mut rng = common::SplitMix64(7);
    let mut texte = String::with_capacity(n * 50);
    let mut open = 100.0_f64;
    for k in 0..n {
        let close = ((open * (1.0 + (rng.next_f64() - 0.5) * 0.002)) * 10.0).round() / 10.0;
        let high = open.max(close) + 0.1;
        let low = (open.min(close) - 0.1).max(0.1);
        let volume = rng.next_f64() * 5.0;
        let trades = 1 + (rng.next_u64() % 40);
        let _ = writeln!(
            texte,
            "{},{open:.1},{high:.1},{low:.1},{close:.1},{volume:.8},{trades}",
            DEBUT_S + 60 * k as i64
        );
        open = close;
    }
    texte
}

fn ingest_kraken_zip_2y(c: &mut Criterion) {
    let n = taille();
    let dossier = tempfile::tempdir().expect("dossier temporaire");
    let zip = dossier.path().join("synthetique.zip");
    let csv = csv_synthetique(n);
    // Couverture au-delà de la dernière bougie : aucune non clôturée.
    let manifeste = common::manifeste_synthetique("2030-01-01");
    common::ecrire_zip(
        &zip,
        &[
            ("MANIFEST.json", &manifeste),
            ("SYNTHUSD_1.csv", csv.as_bytes()),
        ],
    );
    let sha = common::sha256_fichier(&zip);
    let now_ms = 1_900_000_000_000_i64;

    let mut group = c.benchmark_group("ingest_kraken_zip_2y");
    group.sample_size(10);
    group.bench_function("zip_synthetique_1m", |b| {
        b.iter_batched(
            || {
                let racine = tempfile::tempdir().expect("racine temporaire");
                let args = IngestKrakenArgs {
                    zip: zip.clone(),
                    sha256: sha.parse().expect("sha256 valide"),
                    pairs: vec!["SYNTHUSD".parse().expect("paire valide")],
                    root: racine.path().to_path_buf(),
                };
                (racine, args)
            },
            |(racine, args)| {
                let resume = ingest_kraken(&args, now_ms).expect("ingestion");
                assert_eq!(resume.pairs_ok, 1, "{:?}", resume.pairs_failed);
                racine
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

criterion_group!(benches, ingest_kraken_zip_2y);
criterion_main!(benches);
