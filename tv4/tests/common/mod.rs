//! Générateur synthétique déterministe, partagé par les tests d'intégration et le bench criterion.
//!
//! Aucune crate aléatoire et aucune donnée de marché : une marche multiplicative pilotée par un
//! SplitMix64. Les prix sont inventés (autour de 100).

// Inclus par plusieurs crates de test et par le bench : chacun n'utilise qu'une partie.
#![allow(dead_code)]

use tv4::model::Candle;

/// SplitMix64 standard : générateur de 64 bits, déterministe, sans dépendance.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Valeur dans [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }
}

/// `n` bougies 1m synthétiques valides, avec des trous si `gap_prob > 0` et des volumes nuls (2 %).
///
/// `open` = close précédent (100,0 au départ) ; `close = open × (1 + (u - 0,5) × 0,002)` ;
/// `high` et `low` débordent de 0 à 5 bps ; `open_time` avance d'une minute, plus 1 à 30 minutes
/// quand un trou est tiré.
pub fn synthetic_candles(n: usize, seed: u64, start_ms: i64, gap_prob: f64) -> Vec<Candle> {
    let mut rng = SplitMix64(seed);
    let mut candles = Vec::with_capacity(n);
    let mut open = 100.0_f64;
    let mut open_time_ms = start_ms;
    for _ in 0..n {
        let u = rng.next_f64();
        let u2 = rng.next_f64();
        let u3 = rng.next_f64();
        let u4 = rng.next_f64();
        let u5 = rng.next_f64();
        // Longueur du trou tirée à part : `u5 < gap_prob` borne u5 et ne peut pas servir deux fois.
        let u6 = rng.next_f64();
        let close = open * (1.0 + (u - 0.5) * 0.002);
        let high = open.max(close) * (1.0 + u2 * 0.0005);
        let low = open.min(close) * (1.0 - u3 * 0.0005);
        let volume = if u4 < 0.02 { 0.0 } else { u4 * 5.0 };
        candles.push(Candle {
            open_time_ms,
            open,
            high,
            low,
            close,
            volume,
            trades: None,
        });
        open = close;
        open_time_ms += 60_000;
        if u5 < gap_prob {
            open_time_ms += (1 + (u6 * 30.0) as i64) * 60_000;
        }
    }
    candles
}

// --- Aides communes aux tests du moteur (S3) ---

use std::path::PathBuf;
use tv4::model::{ExchangeId, FeeSchedule, Slippage, Timeframe};
use tv4::strategy::{Strategy, registry};

/// 2024-01-01T00:00:00Z : origine des bougies écrites à la main.
pub const T0: i64 = 1_704_067_200_000;

/// Chemin d'un fichier du dépôt, relatif à la racine.
pub fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

/// Stratégie `id` construite par le registre depuis un fichier TOML du dépôt.
pub fn strategy_from_file(id: &str, relative: &str) -> Box<dyn Strategy> {
    let text = std::fs::read_to_string(repo_path(relative)).expect("fichier de paramètres lisible");
    let value = toml::from_str::<toml::Value>(&text).expect("TOML valide");
    registry::build(id, &value).expect("stratégie valide")
}

/// `sma_cross` 60/240 en 1m (`config/strategies/sma_cross_1m.toml`).
pub fn sma_cross_1m() -> Box<dyn Strategy> {
    strategy_from_file("sma_cross", "config/strategies/sma_cross_1m.toml")
}

/// Grille OKX (20 / 35 bps), lue dans `config/fees.toml`.
pub fn okx_fees() -> FeeSchedule {
    FeeSchedule::load(ExchangeId::Okx, &repo_path("config/fees.toml")).expect("frais okx")
}

pub fn slippage(bps: f64) -> Slippage {
    Slippage::from_bps(bps).expect("slippage valide")
}

/// Les 10 000 bougies 1m de `fixtures/kraken_xbtusd_1m.csv`.
pub fn fixture_1m() -> Vec<Candle> {
    let file = std::fs::File::open(repo_path("fixtures/kraken_xbtusd_1m.csv")).expect("fixture");
    tv4::marketdata::kraken::read_all(file, Timeframe::M1).expect("fixture lisible")
}

// --- Zips Kraken synthétiques et constantes des preuves S4/S5 (S5) ---

use std::io::Write as _;
use std::path::Path;

/// `content_hash` du mois 2020-01 de la fixture (identique pour `ingest csv` et `ingest kraken`).
pub const HASH_FIXTURE_MOIS: &str =
    "blake3:d94f2eb46e2c4b6738b7ef4ed2441f3928db379e5b29f48f95532ba18c56f353";

/// `dataset_hash` de la fixture en `skip` sur les 8 premiers jours de janvier 2020 (preuve S4).
pub const DATASET_HASH_SKIP: &str =
    "blake3:943075a3c2fe27072d3c2a9a1113580fbfcb04542c6ae2e7a82db44aaf54a495";

/// Écrit un zip déterministe : `MANIFEST.json` en `Stored`, les autres entrées en `Deflated`. Aucun zip
/// n'est commité : les tests les construisent à la volée (zip 9.0.2 donne deux fois le même SHA-256).
pub fn ecrire_zip(chemin: &Path, entrees: &[(&str, &[u8])]) {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};
    let mut zip = ZipWriter::new(std::fs::File::create(chemin).expect("création du zip"));
    for (nom, octets) in entrees {
        let methode = if *nom == "MANIFEST.json" {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        zip.start_file(
            *nom,
            SimpleFileOptions::default().compression_method(methode),
        )
        .expect("entrée du zip");
        zip.write_all(octets).expect("écriture de l'entrée");
    }
    zip.finish()
        .expect("fin du zip")
        .sync_all()
        .expect("synchronisation du zip");
}

/// SHA-256 hexadécimal minuscule d'un fichier (même fonction que la CLI).
pub fn sha256_fichier(chemin: &Path) -> String {
    let fichier = std::fs::File::open(chemin).expect("fichier à hacher");
    tv4::marketdata::kraken_zip::sha256_stream(fichier)
        .expect("SHA-256")
        .0
}

/// `MANIFEST.json` **synthétique** qui reprend les valeurs lues par le propriétaire sur le dump réel
/// (product `ohlcvt`, schema_version 1, release `r1`, has_header false, couverture 2013-09-11 à `fin`,
/// 1 760 paires, 12 350 fichiers, 598 910 464 lignes, généré le 2026-10-01). Les NOMS de clés sont une
/// hypothèse de test : ceux du manifeste réel ne sont pas connus.
pub fn manifeste_synthetique(fin: &str) -> Vec<u8> {
    format!(
        r#"{{"product":"ohlcvt","schema_version":1,"release":"r1","has_header":false,"coverage":{{"start":"2013-09-11","end":"{fin}"}},"pairs":1760,"files":12350,"rows":598910464,"generated_at":"2026-10-01"}}"#
    )
    .into_bytes()
}

/// Texte de `fixtures/kraken_xbtusd_1m.csv` (10 000 lignes réelles).
pub fn texte_fixture_1m() -> String {
    std::fs::read_to_string(repo_path("fixtures/kraken_xbtusd_1m.csv")).expect("fixture 1m")
}

/// Trois lignes **synthétiques** (prix inventés, sans rapport avec un marché) pour l'entrée piège
/// `AIXBTUSD_1.csv` : si une recherche par suffixe lisait cette entrée pour XBTUSD, les prix le diraient.
pub const LIGNES_AIXBT_SYNTHETIQUES: &str = "\
1577836800,5.0,5.5,4.5,5.2,10.0,3
1577836860,5.2,5.4,5.1,5.3,12.0,4
1577836920,5.3,5.3,5.0,5.1,8.0,2
";
