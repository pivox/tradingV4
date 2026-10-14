//! Tests d'intégration du lecteur Kraken sur un vrai fichier.
//!
//! Les lignes ci-dessous sont un échantillon de format, valeurs inventées (prix ronds autour de 100),
//! pas des données de marché. Les tests sur la vraie fixture XBTUSD sont dans `fixture_kraken.rs`.

use std::fs;
use std::path::Path;

use tv4::marketdata::kraken::{KrakenCsvReader, KrakenError};
use tv4::model::{Candle, Timeframe};

const SAMPLE: &str = "\
1704067200,100.0,101.0,99.0,100.5,12.5,10
1704067260,100.5,102.0,100.0,101.0,8.25,7
1704067320,101.0,101.5,100.5,100.75,0.0,0
";

#[test]
fn lecture_en_flux_d_un_fichier() {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("echantillon_kraken_format.csv");
    fs::write(&path, SAMPLE).unwrap();

    let candles: Vec<Candle> = KrakenCsvReader::open(&path, Timeframe::M1)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(candles.len(), 3);
    assert_eq!(
        candles[0],
        Candle {
            open_time_ms: 1_704_067_200_000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 12.5,
            trades: Some(10),
        }
    );
    assert_eq!(candles[1].open_time_ms, 1_704_067_260_000);
    assert_eq!(candles[1].volume, 8.25);
    assert_eq!(candles[2].trades, Some(0));
}

#[test]
fn ouverture_d_un_chemin_inexistant() {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("n_existe_pas/absent.csv");
    let error = KrakenCsvReader::open(&path, Timeframe::M1).err().unwrap();
    assert!(matches!(&error, KrakenError::Open { path: p, .. } if p.contains("absent.csv")));
    assert!(error.to_string().contains("absent.csv"), "{error}");
}
