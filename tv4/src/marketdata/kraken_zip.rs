//! Dump OHLCVT complet de Kraken : un zip à plat, une entrée `<PAIRE>_<minutes>.csv` par paire et par
//! résolution (sans en-tête, format de `kraken.rs`), plus un `MANIFEST.json`.
//!
//! Le zip est lu sans extraction : SHA-256 du zip entier vérifié en flux AVANT toute lecture d'entrée,
//! puis `ZipArchive::by_name` (nom exact : `AIXBTUSD_1.csv` existe, une recherche par suffixe
//! confondrait les paires) et lecture CSV en flux. Seul le 1m (`<PAIRE>_1.csv`) est une source ; les
//! `_5` et `_60` servent d'oracle de test, jamais de source (docs/03 §2).
//!
//! # Manifeste du zip
//!
//! Le propriétaire a lu les valeurs de `MANIFEST.json` (product, release, couverture, nombre de paires,
//! de fichiers et de lignes, date de génération), pas les noms exacts de ses clés. Le lecteur est donc
//! tolérant : chaque champ est cherché sous plusieurs noms candidats, tous sont optionnels, et le JSON
//! brut est gardé pour le rapport (`raw`), ce qui révélera les vrais noms au premier run sur le dump.
//!
//! # Symbole canonique
//!
//! `XBTUSD` devient `BTC-USD`, `XDGUSD` devient `DOGE-USD` ; les autres bases sont inchangées. La
//! cotation est reconnue par le plus long suffixe de [`KNOWN_QUOTES`].

use std::borrow::Cow;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::str::FromStr;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zip::ZipArchive;
use zip::read::ZipFile;
use zip::result::ZipError;

use super::kraken::KrakenRawReader;

/// Nom de l'entrée du manifeste, à la racine du zip.
pub const MANIFEST_ENTRY: &str = "MANIFEST.json";

/// Taille maximale lue pour `MANIFEST.json` (garde-fou contre une entrée géante).
const MANIFEST_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Cotations connues, recherchées par le suffixe le plus long.
pub const KNOWN_QUOTES: [&str; 12] = [
    "USDT", "USDC", "EUR", "USD", "GBP", "CAD", "JPY", "CHF", "AUD", "ETH", "XBT", "DAI",
];

/// Erreurs de lecture du zip Kraken.
#[derive(Debug, Error)]
pub enum KrakenZipError {
    #[error("ouverture de `{path}` impossible : {source}")]
    Open {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("lecture de `{path}` impossible pendant le calcul du SHA-256 : {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "SHA-256 de `{path}` : {actual} calculé, {expected} attendu ; aucune entrée n'a été lue"
    )]
    Sha256Mismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("SHA-256 « {value} » invalide : 64 caractères hexadécimaux attendus")]
    InvalidSha256 { value: String },

    #[error("archive `{path}` illisible : {source}")]
    Zip {
        path: String,
        #[source]
        source: ZipError,
    },

    #[error("entrée `{entry}` absente du zip (recherche par nom exact)")]
    EntryMissing { entry: String },

    #[error("MANIFEST.json annonce has_header = true : lecture avec en-tête non prise en charge")]
    HeaderUnsupported,

    #[error("paire Kraken « {pair} » invalide : {reason}")]
    InvalidPair { pair: String, reason: &'static str },
}

// ---------------------------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------------------------

/// Rendu hexadécimal minuscule, sans crate `hex` (`LowerHex` n'existe pas sur le résultat de sha2 0.11).
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// SHA-256 d'un flux par tampon de 1 Mio : (hex minuscule, octets lus).
pub fn sha256_stream<R: Read>(mut reader: R) -> std::io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((hex_lower(hasher.finalize().as_slice()), total))
}

/// SHA-256 attendu : 64 caractères hexadécimaux, normalisés en minuscules. `FromStr` pour clap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sha256Hex(String);

impl Sha256Hex {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Sha256Hex {
    type Err = KrakenZipError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let valide = text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit());
        if valide {
            Ok(Sha256Hex(text.to_ascii_lowercase()))
        } else {
            Err(KrakenZipError::InvalidSha256 {
                value: text.to_string(),
            })
        }
    }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------------------------
// Paires et symboles canoniques
// ---------------------------------------------------------------------------------------------

/// Renommage Kraken vers le symbole canonique, par égalité exacte (jamais par sous-chaîne).
fn canonical_asset(asset: &str) -> &str {
    match asset {
        "XBT" => "BTC",
        "XDG" => "DOGE",
        autre => autre,
    }
}

/// `XBTUSD` → `BTC-USD`. Base et cotation : `XBT` devient `BTC`, `XDG` devient `DOGE`, le reste est
/// inchangé. La cotation est le plus long suffixe de [`KNOWN_QUOTES`] qui laisse une base non vide.
pub fn canonical_symbol(pair: &str) -> Result<String, KrakenZipError> {
    let invalid = |reason: &'static str| KrakenZipError::InvalidPair {
        pair: pair.to_string(),
        reason,
    };
    let valide = (2..=32).contains(&pair.len())
        && pair
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if !valide {
        return Err(invalid("majuscules et chiffres, 2 à 32 caractères"));
    }
    let quote = KNOWN_QUOTES
        .iter()
        .filter(|quote| pair.len() > quote.len() && pair.ends_with(**quote))
        .max_by_key(|quote| quote.len())
        .ok_or_else(|| invalid("cotation inconnue ou base vide"))?;
    let base = &pair[..pair.len() - quote.len()];
    Ok(format!(
        "{}-{}",
        canonical_asset(base),
        canonical_asset(quote)
    ))
}

/// Nom de paire Kraken validé (`[A-Z0-9]{2,32}`) et son symbole canonique. `FromStr` pour clap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KrakenPair {
    pair: String,
    symbol: String,
}

impl KrakenPair {
    /// Nom Kraken, par exemple `XBTUSD`.
    pub fn as_str(&self) -> &str {
        &self.pair
    }

    /// Symbole canonique, par exemple `BTC-USD`.
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Nom de l'entrée 1m dans le zip, par exemple `XBTUSD_1.csv`.
    pub fn entry_1m(&self) -> String {
        format!("{}_1.csv", self.pair)
    }
}

impl FromStr for KrakenPair {
    type Err = KrakenZipError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(KrakenPair {
            pair: text.to_string(),
            symbol: canonical_symbol(text)?,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Manifeste du zip
// ---------------------------------------------------------------------------------------------

/// Champs lus dans `MANIFEST.json`, tous optionnels (voir la doc du module). `raw` garde le JSON entier
/// pour le rapport.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct KrakenManifest {
    pub raw: Option<serde_json::Value>,
    /// Message si le JSON est illisible (le manifeste est alors ignoré, pas bloquant).
    pub parse_error: Option<String>,
    pub product: Option<String>,
    pub schema_version: Option<u64>,
    pub release: Option<String>,
    pub has_header: Option<bool>,
    pub coverage_start: Option<String>,
    pub coverage_end: Option<String>,
    pub pairs: Option<u64>,
    pub files: Option<u64>,
    pub rows: Option<u64>,
    pub generated_at: Option<String>,
}

/// Descend un chemin pointé (`coverage.end`) dans des objets JSON.
fn lookup<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.')
        .try_fold(value, |current, key| current.get(key))
}

/// Première valeur non nulle parmi les chemins candidats.
fn first_of<'a>(value: &'a serde_json::Value, paths: &[&str]) -> Option<&'a serde_json::Value> {
    paths
        .iter()
        .filter_map(|path| lookup(value, path))
        .find(|found| !found.is_null())
}

fn text_of(value: &serde_json::Value, paths: &[&str]) -> Option<String> {
    match first_of(value, paths)? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Un nombre peut être un entier JSON, une chaîne de chiffres, ou un tableau (sa longueur est alors
/// retenue : `"pairs": ["XBTUSD", ...]`).
fn count_of(value: &serde_json::Value, paths: &[&str]) -> Option<u64> {
    match first_of(value, paths)? {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        serde_json::Value::Array(items) => Some(items.len() as u64),
        _ => None,
    }
}

fn bool_of(value: &serde_json::Value, paths: &[&str]) -> Option<bool> {
    match first_of(value, paths)? {
        serde_json::Value::Bool(flag) => Some(*flag),
        serde_json::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

impl KrakenManifest {
    /// Extraction tolérante depuis les octets de `MANIFEST.json`. Un JSON illisible rend un manifeste
    /// vide avec `parse_error` rempli.
    pub fn from_json_bytes(bytes: &[u8]) -> KrakenManifest {
        let value = match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return KrakenManifest {
                    parse_error: Some(error.to_string()),
                    ..KrakenManifest::default()
                };
            }
        };
        KrakenManifest {
            product: text_of(&value, &["product"]),
            schema_version: count_of(&value, &["schema_version", "schemaVersion", "version"]),
            release: text_of(&value, &["release", "release_id"]),
            has_header: bool_of(&value, &["has_header", "hasHeader", "header"]),
            coverage_start: text_of(
                &value,
                &[
                    "coverage.start",
                    "coverage.from",
                    "coverage_start",
                    "start_date",
                    "start",
                    "from",
                ],
            ),
            coverage_end: text_of(
                &value,
                &[
                    "coverage.end",
                    "coverage.to",
                    "coverage_end",
                    "end_date",
                    "end",
                    "to",
                ],
            ),
            pairs: count_of(
                &value,
                &[
                    "pairs",
                    "pair_count",
                    "num_pairs",
                    "n_pairs",
                    "counts.pairs",
                ],
            ),
            files: count_of(
                &value,
                &["files", "file_count", "num_files", "counts.files"],
            ),
            rows: count_of(
                &value,
                &[
                    "rows",
                    "row_count",
                    "lines",
                    "num_rows",
                    "total_rows",
                    "counts.rows",
                ],
            ),
            generated_at: text_of(
                &value,
                &["generated_at", "generatedAt", "created_at", "generated"],
            ),
            raw: Some(value),
            parse_error: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Zip ouvert et vérifié
// ---------------------------------------------------------------------------------------------

/// Zip ouvert et dont le SHA-256 a été vérifié.
pub struct KrakenZip {
    archive: ZipArchive<File>,
    path: String,
    sha256: String,
    size: u64,
}

impl KrakenZip {
    /// Ouvre `path`, calcule le SHA-256 du fichier entier en flux et le compare à `expected` ; puis
    /// `seek(SeekFrom::Start(0))` sur LE MÊME descripteur et `ZipArchive::new`. Aucune entrée n'est lue
    /// avant la comparaison.
    pub fn open_verified(path: &Path, expected: &Sha256Hex) -> Result<KrakenZip, KrakenZipError> {
        let shown = path.display().to_string();
        let mut file = File::open(path).map_err(|source| KrakenZipError::Open {
            path: shown.clone(),
            source,
        })?;
        let (actual, size) = sha256_stream(&mut file).map_err(|source| KrakenZipError::Read {
            path: shown.clone(),
            source,
        })?;
        if actual != expected.as_str() {
            return Err(KrakenZipError::Sha256Mismatch {
                path: shown,
                expected: expected.as_str().to_string(),
                actual,
            });
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|source| KrakenZipError::Read {
                path: shown.clone(),
                source,
            })?;
        let archive = ZipArchive::new(file).map_err(|source| KrakenZipError::Zip {
            path: shown.clone(),
            source,
        })?;
        Ok(KrakenZip {
            archive,
            path: shown,
            sha256: actual,
            size,
        })
    }

    /// SHA-256 calculé du zip entier (égal à l'attendu : la vérification a réussi).
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Taille du zip en octets.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Nombre d'entrées du zip.
    pub fn entry_count(&self) -> usize {
        self.archive.len()
    }

    /// `MANIFEST.json` à la racine : `Ok(None)` s'il est absent ; JSON illisible : `Ok(Some(m))` avec
    /// `parse_error` rempli. Lecture bornée à 64 Mio.
    pub fn manifest(&mut self) -> Result<Option<KrakenManifest>, KrakenZipError> {
        let path = self.path.clone();
        let entry = match self.archive.by_name(MANIFEST_ENTRY) {
            Ok(entry) => entry,
            Err(ZipError::FileNotFound) => return Ok(None),
            Err(source) => return Err(KrakenZipError::Zip { path, source }),
        };
        let mut bytes = Vec::new();
        match entry.take(MANIFEST_MAX_BYTES).read_to_end(&mut bytes) {
            Ok(_) => Ok(Some(KrakenManifest::from_json_bytes(&bytes))),
            // Entrée corrompue : le manifeste est ignoré avec un message, la lecture des paires reste possible.
            Err(error) => Ok(Some(KrakenManifest {
                parse_error: Some(format!("lecture de {MANIFEST_ENTRY} impossible : {error}")),
                ..KrakenManifest::default()
            })),
        }
    }

    /// Taille décompressée de `<PAIRE>_1.csv` (indication pour dimensionner les tampons).
    pub fn entry_size_1m(&mut self, pair: &KrakenPair) -> Result<u64, KrakenZipError> {
        let name = pair.entry_1m();
        let path = self.path.clone();
        match self.archive.by_name(&name) {
            Ok(entry) => Ok(entry.size()),
            Err(ZipError::FileNotFound) => Err(KrakenZipError::EntryMissing { entry: name }),
            Err(source) => Err(KrakenZipError::Zip { path, source }),
        }
    }

    /// Lecteur en flux des lignes brutes de `<PAIRE>_1.csv` (`by_name`, nom exact).
    pub fn rows_1m(
        &mut self,
        pair: &KrakenPair,
    ) -> Result<KrakenRawReader<ZipFile<'_, File>>, KrakenZipError> {
        let name = pair.entry_1m();
        let path = self.path.clone();
        match self.archive.by_name(&name) {
            Ok(entry) => Ok(KrakenRawReader::new(entry)),
            Err(ZipError::FileNotFound) => Err(KrakenZipError::EntryMissing { entry: name }),
            Err(source) => Err(KrakenZipError::Zip { path, source }),
        }
    }

    /// Lecteur en flux des lignes brutes de l'entrée `name` (`by_name`, nom exact), pour les résolutions
    /// autres que le 1m (`XBTUSD_1440.csv`) : S11 s'en sert pour dater la dernière bougie d'une paire
    /// (docs/specs/s11.md §5.2). Même format que [`KrakenZip::rows_1m`].
    pub fn rows_entry(
        &mut self,
        name: &str,
    ) -> Result<KrakenRawReader<ZipFile<'_, File>>, KrakenZipError> {
        let path = self.path.clone();
        match self.archive.by_name(name) {
            Ok(entry) => Ok(KrakenRawReader::new(entry)),
            Err(ZipError::FileNotFound) => Err(KrakenZipError::EntryMissing {
                entry: name.to_string(),
            }),
            Err(source) => Err(KrakenZipError::Zip { path, source }),
        }
    }

    /// Noms des entrées (diagnostic) ; une entrée au nom illisible est ignorée.
    pub fn entry_names(&self) -> Vec<String> {
        self.archive
            .file_names()
            .filter_map(Result::ok)
            .map(Cow::into_owned)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    // Les contenus de ces tests sont des échantillons de format (prix inventés autour de 100), pas des
    // données de marché.
    const T0: i64 = 1_704_067_200;

    fn sample_csv(n: i64) -> String {
        (0..n)
            .map(|i| format!("{},100.0,101.0,99.0,100.5,1.5,3\n", T0 + 60 * i))
            .collect()
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().sync_all().unwrap();
    }

    fn sha_of(path: &Path) -> Sha256Hex {
        let (hex, _) = sha256_stream(File::open(path).unwrap()).unwrap();
        hex.parse().unwrap()
    }

    #[test]
    fn sha256_vecteur_connu_abc() {
        let (hex, lus) = sha256_stream(&b"abc"[..]).unwrap();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(lus, 3);
        // Le vecteur de la chaîne vide.
        let (vide, lus) = sha256_stream(&b""[..]).unwrap();
        assert_eq!(
            vide,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(lus, 0);
    }

    #[test]
    fn sha256_en_flux_egal_au_calcul_en_une_fois() {
        // Octets synthétiques autour de la taille du tampon (1 Mio).
        for taille in [0usize, 1, (1 << 20) - 1, 1 << 20, (1 << 20) + 1] {
            let octets: Vec<u8> = (0..taille).map(|i| (i % 251) as u8).collect();
            let (en_flux, lus) = sha256_stream(octets.as_slice()).unwrap();
            let en_une_fois = hex_lower(Sha256::digest(&octets).as_slice());
            assert_eq!(en_flux, en_une_fois, "taille {taille}");
            assert_eq!(lus, taille as u64);
        }
    }

    #[test]
    fn sha256hex_refuse_longueur_et_caracteres() {
        for mauvais in [
            "",
            "abc",
            &"a".repeat(63),
            &"a".repeat(65),
            &format!("{}g", "a".repeat(63)),
        ] {
            assert!(
                matches!(
                    mauvais.parse::<Sha256Hex>(),
                    Err(KrakenZipError::InvalidSha256 { .. })
                ),
                "« {mauvais} »"
            );
        }
        assert!("a".repeat(64).parse::<Sha256Hex>().is_ok());
    }

    #[test]
    fn sha256hex_normalise_en_minuscules() {
        let hex: Sha256Hex = "ABCDEF0123456789".repeat(4).parse().unwrap();
        assert_eq!(hex.as_str(), "abcdef0123456789".repeat(4));
    }

    #[test]
    fn symbole_canonique() {
        let cas = [
            ("XBTUSD", "BTC-USD"),
            ("XBTEUR", "BTC-EUR"),
            ("ETHUSD", "ETH-USD"),
            ("XDGUSD", "DOGE-USD"),
            ("ETHXBT", "ETH-BTC"),
            ("XBTUSDT", "BTC-USDT"),
            ("XBTUSDC", "BTC-USDC"),
            ("USDTUSD", "USDT-USD"),
            ("AIXBTUSD", "AIXBT-USD"),
        ];
        for (paire, attendu) in cas {
            assert_eq!(canonical_symbol(paire).unwrap(), attendu, "{paire}");
        }
        for refusee in ["USD", "XBTFOO", "xbtusd", "", &"A".repeat(33), "X"] {
            assert!(
                matches!(
                    canonical_symbol(refusee),
                    Err(KrakenZipError::InvalidPair { .. })
                ),
                "« {refusee} »"
            );
        }
        let paire: KrakenPair = "XBTUSD".parse().unwrap();
        assert_eq!(
            (paire.as_str(), paire.symbol(), paire.entry_1m().as_str()),
            ("XBTUSD", "BTC-USD", "XBTUSD_1.csv")
        );
    }

    #[test]
    fn manifeste_tolerant() {
        // Clés à plat.
        let plat = KrakenManifest::from_json_bytes(
            br#"{"product":"ohlcvt","schema_version":1,"release":"r1","has_header":false,
                "coverage_start":"2013-09-11","coverage_end":"2026-09-30","pairs":1760,
                "files":12350,"rows":598910464,"generated_at":"2026-10-01"}"#,
        );
        assert_eq!(plat.product.as_deref(), Some("ohlcvt"));
        assert_eq!(plat.schema_version, Some(1));
        assert_eq!(plat.release.as_deref(), Some("r1"));
        assert_eq!(plat.has_header, Some(false));
        assert_eq!(plat.coverage_end.as_deref(), Some("2026-09-30"));
        assert_eq!(plat.pairs, Some(1760));
        assert_eq!(plat.files, Some(12350));
        assert_eq!(plat.rows, Some(598_910_464));
        assert_eq!(plat.generated_at.as_deref(), Some("2026-10-01"));
        assert!(plat.raw.is_some() && plat.parse_error.is_none());

        // Imbriqué sous `coverage`, nombres en chaîne, `pairs` en tableau, clés inconnues ignorées.
        let autre = KrakenManifest::from_json_bytes(
            br#"{"coverage":{"start":"2013-09-11","end":"2026-09-30T00:00:00Z"},
                "pairs":["XBTUSD","ETHUSD","XDGUSD"],"rows":"598910464","files":"12350",
                "schemaVersion":"1","inconnue":{"x":1}}"#,
        );
        assert_eq!(autre.coverage_start.as_deref(), Some("2013-09-11"));
        assert_eq!(autre.coverage_end.as_deref(), Some("2026-09-30T00:00:00Z"));
        assert_eq!(autre.pairs, Some(3));
        assert_eq!(autre.rows, Some(598_910_464));
        assert_eq!(autre.files, Some(12350));
        assert_eq!(autre.schema_version, Some(1));
        assert_eq!(autre.product, None);

        // JSON illisible.
        let casse = KrakenManifest::from_json_bytes(b"{ pas du json");
        assert!(casse.parse_error.is_some());
        assert!(casse.raw.is_none() && casse.coverage_end.is_none());

        // Aucune clé connue : tout est None, le brut est gardé.
        let vide = KrakenManifest::from_json_bytes(b"{\"x\": 1}");
        assert!(vide.raw.is_some() && vide.parse_error.is_none());
        assert_eq!(vide.coverage_end, None);
    }

    #[test]
    fn manifeste_absent_du_zip() {
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("sans_manifeste.zip");
        write_zip(&chemin, &[("XBTUSD_1.csv", sample_csv(3).as_bytes())]);
        let mut zip = KrakenZip::open_verified(&chemin, &sha_of(&chemin)).unwrap();
        assert_eq!(zip.entry_count(), 1);
        assert!(zip.manifest().unwrap().is_none());
    }

    #[test]
    fn manifeste_lu_depuis_le_zip_et_lignes_en_flux() {
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("ok.zip");
        write_zip(
            &chemin,
            &[
                (
                    "MANIFEST.json",
                    br#"{"coverage":{"end":"2026-09-30"},"release":"r1"}"#,
                ),
                ("XBTUSD_1.csv", sample_csv(5).as_bytes()),
            ],
        );
        let mut zip = KrakenZip::open_verified(&chemin, &sha_of(&chemin)).unwrap();
        assert_eq!(zip.size(), std::fs::metadata(&chemin).unwrap().len());
        let manifeste = zip.manifest().unwrap().unwrap();
        assert_eq!(manifeste.coverage_end.as_deref(), Some("2026-09-30"));
        let paire: KrakenPair = "XBTUSD".parse().unwrap();
        assert!(zip.entry_size_1m(&paire).unwrap() > 0);
        let lignes: Vec<_> = zip
            .rows_1m(&paire)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(lignes.len(), 5);
        assert_eq!(lignes[0].open_time_ms, T0 * 1000);
    }

    #[test]
    fn entree_cherchee_par_nom_exact() {
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("piege.zip");
        write_zip(&chemin, &[("AIXBTUSD_1.csv", sample_csv(3).as_bytes())]);
        let mut zip = KrakenZip::open_verified(&chemin, &sha_of(&chemin)).unwrap();
        let xbt: KrakenPair = "XBTUSD".parse().unwrap();
        match zip.rows_1m(&xbt) {
            Err(KrakenZipError::EntryMissing { entry }) => assert_eq!(entry, "XBTUSD_1.csv"),
            Err(autre) => panic!("EntryMissing attendu : {autre:?}"),
            Ok(_) => panic!("AIXBTUSD_1.csv ne doit pas répondre à XBTUSD"),
        }
        assert!(matches!(
            zip.entry_size_1m(&xbt),
            Err(KrakenZipError::EntryMissing { .. })
        ));
        // La vraie paire du piège se lit normalement.
        let ai: KrakenPair = "AIXBTUSD".parse().unwrap();
        assert_eq!(zip.rows_1m(&ai).unwrap().count(), 3);
    }

    #[test]
    fn entree_generique_par_nom_exact() {
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("generique.zip");
        write_zip(
            &chemin,
            &[
                ("XBTUSD_1440.csv", sample_csv(2).as_bytes()),
                ("XBTUSD_1.csv", sample_csv(4).as_bytes()),
            ],
        );
        let mut zip = KrakenZip::open_verified(&chemin, &sha_of(&chemin)).unwrap();
        assert_eq!(zip.rows_entry("XBTUSD_1440.csv").unwrap().count(), 2);
        assert_eq!(zip.rows_entry("XBTUSD_1.csv").unwrap().count(), 4);
        match zip.rows_entry("XBTUSD_5.csv") {
            Err(KrakenZipError::EntryMissing { entry }) => assert_eq!(entry, "XBTUSD_5.csv"),
            Err(autre) => panic!("EntryMissing attendu : {autre:?}"),
            Ok(_) => panic!("XBTUSD_5.csv n'existe pas"),
        }
    }

    #[test]
    fn sha_faux_aucune_entree_lue() {
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("a.zip");
        write_zip(&chemin, &[("XBTUSD_1.csv", sample_csv(3).as_bytes())]);
        let faux: Sha256Hex = "00".repeat(32).parse().unwrap();
        match KrakenZip::open_verified(&chemin, &faux) {
            Err(KrakenZipError::Sha256Mismatch {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "00".repeat(32));
                assert_eq!(actual, sha_of(&chemin).as_str());
            }
            Err(autre) => panic!("Sha256Mismatch attendu : {autre:?}"),
            Ok(_) => panic!("un SHA-256 faux doit être refusé"),
        }
        let absent = KrakenZip::open_verified(&dir.path().join("absent.zip"), &faux);
        assert!(matches!(absent, Err(KrakenZipError::Open { .. })));
    }

    #[test]
    fn zip_tronque_illisible() {
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("a.zip");
        write_zip(&chemin, &[("XBTUSD_1.csv", sample_csv(50).as_bytes())]);
        let octets = std::fs::read(&chemin).unwrap();
        let tronque = dir.path().join("tronque.zip");
        std::fs::write(&tronque, &octets[..octets.len() / 2]).unwrap();
        // Le SHA-256 du fichier tronqué est le bon : c'est l'archive qui est illisible.
        match KrakenZip::open_verified(&tronque, &sha_of(&tronque)) {
            Err(KrakenZipError::Zip { .. }) => {}
            Err(autre) => panic!("Zip attendu : {autre:?}"),
            Ok(_) => panic!("un zip tronqué doit être refusé"),
        }
    }
}
