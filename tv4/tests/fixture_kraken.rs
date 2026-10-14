//! Tests d'intégration du lecteur Kraken sur la fixture réelle XBTUSD (`fixtures/README.md`).
//!
//! Les fichiers sont des extraits du dump Kraken OHLCVT, jamais modifiés : chaque test s'appuie sur
//! leurs bornes et leurs effectifs, et une garde d'intégrité épingle leur empreinte blake3.

use std::fs;
use std::path::PathBuf;

use tv4::marketdata::kraken::KrakenCsvReader;
use tv4::model::hash::blake3_hex;
use tv4::model::{Candle, Timeframe};

const MINUTE_MS: i64 = 60_000;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures")
        .join(name)
}

fn read_fixture(name: &str, timeframe: Timeframe) -> Vec<Candle> {
    KrakenCsvReader::open(&fixture_path(name), timeframe)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn assert_strictly_increasing(candles: &[Candle]) {
    assert!(
        candles
            .windows(2)
            .all(|pair| pair[0].open_time_ms < pair[1].open_time_ms),
        "horodatages non strictement croissants"
    );
}

#[test]
fn le_1m_se_lit_en_entier() {
    let candles = read_fixture("kraken_xbtusd_1m.csv", Timeframe::M1);

    assert_eq!(candles.len(), 10_000);
    assert_eq!(candles[0].open_time_ms, 1_577_836_800_000);
    assert_eq!(candles[9_999].open_time_ms, 1_578_518_940_000);
    assert_strictly_increasing(&candles);
    assert!(candles.iter().all(|candle| candle.trades.is_some()));

    // Kraken n'écrit pas les minutes sans transaction : somme des écarts moins une minute chacun.
    let missing: i64 = candles
        .windows(2)
        .map(|pair| (pair[1].open_time_ms - pair[0].open_time_ms) / MINUTE_MS - 1)
        .sum();
    assert_eq!(missing, 1_370);
}

#[test]
fn le_5m_se_lit_en_entier() {
    let candles = read_fixture("kraken_xbtusd_5m.csv", Timeframe::M5);

    assert_eq!(candles.len(), 2_271);
    assert_eq!(candles[0].open_time_ms, 1_577_836_800_000);
    assert_eq!(candles[2_270].open_time_ms, 1_578_518_700_000);
    assert_strictly_increasing(&candles);
}

#[test]
fn le_1h_se_lit_en_entier() {
    let candles = read_fixture("kraken_xbtusd_1h.csv", Timeframe::H1);

    assert_eq!(candles.len(), 190);
    assert_eq!(candles[0].open_time_ms, 1_577_836_800_000);
    assert_eq!(candles[189].open_time_ms, 1_578_517_200_000);
    assert_strictly_increasing(&candles);
}

#[test]
fn les_fixtures_ne_sont_pas_modifiees() {
    const PINNED: [(&str, &str); 3] = [
        (
            "kraken_xbtusd_1m.csv",
            "3ed72356056ed8df57d6cc8ed0992ad4cbe50940ce08fe52db476842dbf56a4e",
        ),
        (
            "kraken_xbtusd_5m.csv",
            "5c0dd275b307ad1f113af17d9a91379a4e8c51a8dea36aac65137e03c40ebb44",
        ),
        (
            "kraken_xbtusd_1h.csv",
            "8cfbe46812271c5fb51855e89de8809262c32980ba41028ce98c2b0f4e0b5249",
        ),
    ];

    for (name, expected) in PINNED {
        let bytes = fs::read(fixture_path(name)).unwrap();
        assert_eq!(
            blake3_hex(&bytes),
            expected,
            "{name} a changé : une fixture ne se modifie jamais (un changement = nouveaux fichiers \
             et nouvelles empreintes, voir fixtures/README.md)"
        );
    }
}
