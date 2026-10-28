//! Aides communes aux tests du rapport du portefeuille (S9, lot F) : datasets multi-séries synthétiques
//! construits par l'API du store, et lancement du binaire `tv4`. Marché inventé, aucune donnée réelle.
//!
//! Inclus par `mod report_s9_common;` dans chaque binaire de test ; chacun n'en utilise qu'une partie.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tv4::model::{Candle, ExchangeId, Timeframe};
use tv4::store::{
    Columns, GapsPolicy, MultiBuildRequest, Partition, Symbol, Window, build_dataset_multi,
    write_partition,
};

/// 2024-01-01T00:00:00Z.
pub const T0: i64 = 1_704_067_200_000;
pub const MINUTE: i64 = 60_000;
/// 2030-01-01T00:00:00Z : tous les mois des tests sont clos à cet instant.
pub const FUTUR: i64 = 1_893_456_000_000;

/// Marche multiplicative déterministe de `common::synthetic_candles`, dont les volumes sont multipliés
/// par `volume_scale`.
pub fn candles(
    n: usize,
    seed: u64,
    gap_prob: f64,
    volume_scale: f64,
    synthetic: fn(usize, u64, i64, f64) -> Vec<Candle>,
) -> Vec<Candle> {
    synthetic(n, seed, T0, gap_prob)
        .into_iter()
        .map(|mut c| {
            c.volume *= volume_scale;
            c
        })
        .collect()
}

pub fn symbol(text: &str) -> Symbol {
    text.parse().expect("symbole valide")
}

/// Écrit les partitions Kraken 1m de chaque paire puis construit le dataset `name` (politique `skip`)
/// sur `[T0, T0 + minutes)`. Rend le chemin du manifeste.
pub fn build_dataset(
    root: &Path,
    name: &str,
    series: &[(&str, Vec<Candle>)],
    minutes: i64,
) -> PathBuf {
    let mut symbols = Vec::new();
    for (sym, candles) in series {
        let partition = Partition::new(ExchangeId::Kraken, symbol(sym), Timeframe::M1);
        write_partition(
            root,
            &partition,
            &Columns::from_candles(candles),
            "synthetic",
            FUTUR,
        )
        .expect("écriture de la partition");
        symbols.push(symbol(sym));
    }
    let window = Window::new(T0, T0 + minutes * MINUTE).expect("fenêtre valide");
    build_dataset_multi(
        root,
        &MultiBuildRequest {
            name,
            exchange: ExchangeId::Kraken,
            symbols: &symbols,
            tf: Timeframe::M1,
            window,
            gaps_policy: GapsPolicy::Skip,
        },
        FUTUR,
    )
    .expect("build du dataset");
    root.join("datasets").join(format!("{name}.json"))
}

/// Chemin d'un fichier du dépôt, relatif à la racine.
pub fn repo(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

pub fn tv4(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(args)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

pub fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}
