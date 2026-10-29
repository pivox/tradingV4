//! Lecture et écriture vérifiées des fichiers d'univers (docs/specs/s11.md §4.4, §4.5, §4.7 et §7.2).
//!
//! Un dossier d'univers contient :
//!
//! ```text
//! universe.json                 schéma tv4-universe-v1, écrit en dernier
//! universe_<AAAA>-Q<n>.csv      un fichier de 11 colonnes par trimestre entièrement couvert
//! eligible_<AAAA-MM-JJ>.csv     un fichier par date `--at` (tous les éligibles, sans troncature)
//! instruments_kraken.toml       règles d'instrument des paires de l'union (format de
//!                               `config/instruments.toml`)
//! ```
//!
//! # Ce que ce module fait, et ne fait pas
//!
//! Il ne calcule aucun univers : le calcul (éligibilité, classement, qualité) relève de
//! `research::universe`, qui remplit un [`UniverseBuild`]. Ici on rend les octets canoniques, on les
//! hache, on refuse de modifier un trimestre déjà figé ([`write_universe`]) et on relit tout en
//! vérifiant chaque hash ([`load`]) : `report_hash` de `universe.json`, `rules_hash`, `file_hash` de
//! chaque CSV et du fichier d'instruments.
//!
//! # Identité
//!
//! L'identité d'un univers, celle qui entre dans les manifestes de dataset et les runs, n'est faite que
//! des règles (`rules_hash`) et des fichiers des trimestres lus (`window_hash`, [`LoadedUniverse::window_hash`]).
//! La source, les dates `--at` et les diagnostics n'y entrent pas : ils sont dans `report_hash`, qui
//! n'est jamais recopié ailleurs.
//!
//! # Dépendances
//!
//! Comme tout `store`, ce module ne dépend que de `model`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::hash::{HASH_PREFIX, blake3_prefixed, hash_json};
use crate::model::universe::{UNIVERSE_RULES_VERSION, UniverseQuarter, quarter_bounds};
use crate::model::{Member, PairSubset, Timeframe, Ts, UniverseRef, UniverseSchedule};

use super::StoreError;
use super::dsh;
use super::durable::{io_error, write_bytes_durable};
use super::layout::Symbol;
use super::time::parse_rfc3339_utc;

/// Schéma de `universe.json`.
pub const UNIVERSE_SCHEMA: &str = "tv4-universe-v1";

/// Nom du fichier d'identité et de rapport d'un dossier d'univers.
pub const UNIVERSE_FILE: &str = "universe.json";

/// Nom du fichier des règles d'instrument dérivées du dump.
pub const INSTRUMENTS_FILE: &str = "instruments_kraken.toml";

/// En-tête des CSV d'univers (s11 §4.4) : onze colonnes.
pub const CSV_HEADER: &str = "rank,base,symbol,kraken_pair,quote,volume_quote_90d,first_active,longest_gap_365d_ms,last_close,excluded_tfs,input_hash";

/// Nombre de colonnes d'un CSV d'univers.
const CSV_COLUMNS: usize = 11;

/// Erreur « fichier d'univers invalide » qui nomme le fichier.
fn bad(path: &str, reason: impl Into<String>) -> StoreError {
    StoreError::UniverseFileInvalid {
        path: path.to_string(),
        reason: reason.into(),
    }
}

/// Erreur de sérialisation JSON nommant `path`.
fn json_error(path: &str, source: serde_json::Error) -> StoreError {
    StoreError::Json {
        path: path.to_string(),
        source,
    }
}

/// `[A-Z0-9]+`, avec une longueur maximale.
fn is_upper_alnum(text: &str, max_len: usize) -> bool {
    !text.is_empty()
        && text.len() <= max_len
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Hash `blake3:<64 hex>` bien formé.
fn check_hash(path: &str, field: &str, value: &str) -> Result<(), StoreError> {
    dsh::strip_prefix(&format!("{path} ({field})"), value).map(|_| ())
}

// ---------------------------------------------------------------------------------------------
// CSV d'univers
// ---------------------------------------------------------------------------------------------

/// Une ligne d'un CSV d'univers (s11 §4.4).
#[derive(Clone, Debug, PartialEq)]
pub struct CsvRow {
    /// 1 pour le plus liquide ; consécutif dans un fichier.
    pub rank: u32,
    /// Base canonique (`BTC`).
    pub base: String,
    /// Symbole canonique du store (`BTC-USD`), égal à `base-quote`.
    pub symbol: String,
    /// Nom de la paire dans le zip (`XBTUSD`).
    pub kraken_pair: String,
    /// `USD` ou `USDT`.
    pub quote: String,
    pub volume_quote_90d: f64,
    /// RFC 3339 UTC de la première minute active.
    pub first_active: String,
    pub longest_gap_365d_ms: i64,
    pub last_close: f64,
    /// TF exclus par la qualité sur le trimestre, durée croissante, jamais le 1m.
    pub excluded_tfs: Vec<Timeframe>,
    /// `blake3:<hex>` du segment lu par la règle (s11 §4.4).
    pub input_hash: String,
}

impl CsvRow {
    fn validate(&self, shown: &str) -> Result<(), StoreError> {
        let ligne = |champ: &str, raison: String| {
            bad(shown, format!("rang {} : {champ} {raison}", self.rank))
        };
        if self.rank == 0 {
            return Err(bad(shown, "rang 0 : les rangs commencent à 1"));
        }
        if !is_upper_alnum(&self.base, 32) {
            return Err(ligne("base", format!("« {} » invalide", self.base)));
        }
        if !is_upper_alnum(&self.quote, 32) {
            return Err(ligne("quote", format!("« {} » invalide", self.quote)));
        }
        if self.symbol != format!("{}-{}", self.base, self.quote)
            || self.symbol.parse::<Symbol>().is_err()
        {
            return Err(ligne(
                "symbol",
                format!("« {} » n'est pas {}-{}", self.symbol, self.base, self.quote),
            ));
        }
        if !is_upper_alnum(&self.kraken_pair, 32) || self.kraken_pair.len() < 2 {
            return Err(ligne(
                "kraken_pair",
                format!("« {} » invalide", self.kraken_pair),
            ));
        }
        if !self.volume_quote_90d.is_finite() || self.volume_quote_90d < 0.0 {
            return Err(ligne(
                "volume_quote_90d",
                "doit être fini et positif".to_string(),
            ));
        }
        if !self.last_close.is_finite() {
            return Err(ligne("last_close", "doit être fini".to_string()));
        }
        if self.longest_gap_365d_ms < 0 {
            return Err(ligne(
                "longest_gap_365d_ms",
                "doit être positif".to_string(),
            ));
        }
        parse_rfc3339_utc(&self.first_active).map_err(|_| {
            ligne(
                "first_active",
                format!("« {} » illisible", self.first_active),
            )
        })?;
        if self.excluded_tfs.contains(&Timeframe::M1) {
            return Err(ligne(
                "excluded_tfs",
                "le 1m n'exclut jamais rien (décision 8)".to_string(),
            ));
        }
        if self.excluded_tfs.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ligne(
                "excluded_tfs",
                "à ranger par durée croissante, sans doublon".to_string(),
            ));
        }
        check_hash(shown, "input_hash", &self.input_hash)
    }
}

/// Rend `tfs` en `1h;4h` (vide : chaîne vide).
fn join_tfs(tfs: &[Timeframe]) -> String {
    tfs.iter()
        .map(|tf| tf.as_str())
        .collect::<Vec<_>>()
        .join(";")
}

/// Rend un CSV d'univers canonique : ASCII, LF, en-tête puis une ligne par membre. Les flottants sont
/// rendus par `{}` de Rust (plus court aller-retour exact). Les rangs doivent valoir 1, 2, 3...
pub fn render_csv(shown: &str, rows: &[CsvRow]) -> Result<String, StoreError> {
    let mut out = String::with_capacity(CSV_HEADER.len() + 1 + rows.len() * 160);
    out.push_str(CSV_HEADER);
    out.push('\n');
    for (index, row) in rows.iter().enumerate() {
        row.validate(shown)?;
        if usize::try_from(row.rank).ok() != Some(index + 1) {
            return Err(bad(
                shown,
                format!(
                    "rang {} à la ligne {} : les rangs doivent valoir 1, 2, 3...",
                    row.rank,
                    index + 1
                ),
            ));
        }
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            row.rank,
            row.base,
            row.symbol,
            row.kraken_pair,
            row.quote,
            row.volume_quote_90d,
            row.first_active,
            row.longest_gap_365d_ms,
            row.last_close,
            join_tfs(&row.excluded_tfs),
            row.input_hash,
        ));
    }
    Ok(out)
}

/// Lit un CSV d'univers et refuse tout ce qui n'est pas sa forme canonique : le texte relu doit égaler
/// le rendu de [`render_csv`] octet pour octet.
pub fn parse_csv(shown: &str, text: &str) -> Result<Vec<CsvRow>, StoreError> {
    if !text.is_ascii() || text.contains('\r') {
        return Err(bad(shown, "ASCII et fins de ligne LF exigés"));
    }
    let mut lines = text.split('\n');
    if lines.next() != Some(CSV_HEADER) {
        return Err(bad(shown, "en-tête inattendu"));
    }
    let mut rows = Vec::new();
    for (index, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let numero = index + 2;
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != CSV_COLUMNS {
            return Err(bad(
                shown,
                format!(
                    "ligne {numero} : {} colonnes au lieu de {CSV_COLUMNS}",
                    fields.len()
                ),
            ));
        }
        let colonne = |nom: &str, texte: &str| {
            format!("ligne {numero} : colonne {nom} « {texte} » illisible")
        };
        let rank = fields[0]
            .parse::<u32>()
            .map_err(|_| bad(shown, colonne("rank", fields[0])))?;
        let volume_quote_90d = fields[5]
            .parse::<f64>()
            .map_err(|_| bad(shown, colonne("volume_quote_90d", fields[5])))?;
        let longest_gap_365d_ms = fields[7]
            .parse::<i64>()
            .map_err(|_| bad(shown, colonne("longest_gap_365d_ms", fields[7])))?;
        let last_close = fields[8]
            .parse::<f64>()
            .map_err(|_| bad(shown, colonne("last_close", fields[8])))?;
        let excluded_tfs = if fields[9].is_empty() {
            Vec::new()
        } else {
            fields[9]
                .split(';')
                .map(|tf| {
                    tf.parse::<Timeframe>()
                        .map_err(|_| bad(shown, colonne("excluded_tfs", fields[9])))
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        rows.push(CsvRow {
            rank,
            base: fields[1].to_string(),
            symbol: fields[2].to_string(),
            kraken_pair: fields[3].to_string(),
            quote: fields[4].to_string(),
            volume_quote_90d,
            first_active: fields[6].to_string(),
            longest_gap_365d_ms,
            last_close,
            excluded_tfs,
            input_hash: fields[10].to_string(),
        });
    }
    if render_csv(shown, &rows)? != text {
        return Err(bad(
            shown,
            "forme non canonique (rendu différent du texte lu)",
        ));
    }
    Ok(rows)
}

/// Nom du CSV d'un trimestre : `universe_2018-Q1.csv`.
pub fn quarter_file_name(quarter: &str) -> String {
    format!("universe_{quarter}.csv")
}

/// Nom du CSV d'une date `--at` (`2018-12-31T00:00:00Z`) : `eligible_2018-12-31.csv`.
pub fn at_file_name(date: &str) -> Result<String, StoreError> {
    parse_rfc3339_utc(date)?;
    Ok(format!("eligible_{}.csv", &date[..10]))
}

// ---------------------------------------------------------------------------------------------
// universe.json
// ---------------------------------------------------------------------------------------------

/// Origine de l'univers : le zip Kraken lu (s11 §4.4). N'entre ni dans le `rules_hash` ni dans le
/// `window_hash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseSource {
    pub kind: String,
    pub zip_sha256: String,
    pub release: String,
    /// RFC 3339 UTC : fin exclusive de la couverture.
    pub coverage_end: String,
    pub pairs_in_zip: u64,
    pub candidates: u64,
}

/// Règles de l'univers, recopiées dans `universe.json` : elles forment le `rules_hash`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseRules {
    pub top_n: u32,
    pub history_days: u32,
    pub quote_gap_ms: i64,
    pub liquidity_window_days: u32,
    pub min_price: f64,
    pub quotes: Vec<String>,
    pub quote_preference: Vec<String>,
    pub excluded_bases: Vec<String>,
    pub quality_threshold: f64,
    pub quality_tfs: Vec<Timeframe>,
    pub quality_uses_quarter_data: bool,
    /// `blake3:` des octets de `config/universe.toml`.
    pub config_file_hash: String,
    pub rules_version: String,
}

impl UniverseRules {
    /// `"blake3:" + hash_json(rules)` (s11 §4.4).
    pub fn hash(&self) -> Result<String, StoreError> {
        Ok(format!("{HASH_PREFIX}{}", hash_json(self)?))
    }

    fn validate(&self, shown: &str) -> Result<(), StoreError> {
        if self.rules_version != UNIVERSE_RULES_VERSION {
            return Err(bad(
                shown,
                format!(
                    "rules_version `{}` lue, `{UNIVERSE_RULES_VERSION}` attendue",
                    self.rules_version
                ),
            ));
        }
        if self.top_n == 0 {
            return Err(bad(shown, "top_n doit valoir au moins 1"));
        }
        if self.quality_tfs.contains(&Timeframe::M1)
            || self.quality_tfs.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(bad(
                shown,
                "quality_tfs : durée croissante, sans doublon, jamais le 1m",
            ));
        }
        check_hash(shown, "config_file_hash", &self.config_file_hash)
    }
}

/// Un trimestre de `universe.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarterEntry {
    /// `2018-Q1`
    pub quarter: String,
    pub date_ms: Ts,
    pub end_ms: Ts,
    /// `universe_2018-Q1.csv`
    pub file: String,
    /// `blake3:` des octets du CSV.
    pub file_hash: String,
    /// Paires éligibles avant la troncature au top N.
    pub eligible: u32,
    pub members: u32,
    /// Par membre, le hash `dsh-v1` du segment `[D, D_suivant)` lu par la qualité (s11 §4.4).
    pub quality_hashes: BTreeMap<String, String>,
}

/// Une date `--at` de `universe.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtEntry {
    /// RFC 3339 UTC.
    pub date: String,
    pub eligible: u32,
    pub file: String,
    pub file_hash: String,
}

/// Paire écartée parce que sa série est bloquante (s11 §4.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectedPair {
    pub kraken_pair: String,
    pub reasons: Vec<String>,
}

/// Une paire de l'union, avec son premier trimestre de présence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnionEntry {
    pub symbol: String,
    pub kraken_pair: String,
    pub first_quarter: String,
}

/// Référence au fichier des règles d'instrument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentsRef {
    pub file: String,
    pub file_hash: String,
    /// `decimals_v1` (s11 §4.7).
    pub method: String,
}

/// Contenu de `universe.json` (schéma `tv4-universe-v1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseFile {
    pub schema: String,
    pub source: UniverseSource,
    pub rules: UniverseRules,
    pub rules_hash: String,
    pub quarters: Vec<QuarterEntry>,
    pub at: Vec<AtEntry>,
    pub rejected_pairs: Vec<RejectedPair>,
    pub unrecognized_pairs: Vec<String>,
    pub union: Vec<UnionEntry>,
    pub instruments: InstrumentsRef,
    pub report_hash: String,
}

impl UniverseFile {
    /// JSON indenté à clés triées, `\n` final (forme des manifestes du store).
    pub fn to_json_pretty(&self) -> Result<String, StoreError> {
        let value = serde_json::to_value(self).map_err(|e| json_error(UNIVERSE_FILE, e))?;
        let mut text =
            serde_json::to_string_pretty(&value).map_err(|e| json_error(UNIVERSE_FILE, e))?;
        text.push('\n');
        Ok(text)
    }

    /// `"blake3:" + hash_json` de l'objet entier sans `report_hash` : intégrité du fichier, jamais
    /// recopiée dans un manifeste ni dans un run (s11 §4.4).
    pub fn compute_report_hash(&self) -> Result<String, StoreError> {
        let mut value = serde_json::to_value(self).map_err(|e| json_error(UNIVERSE_FILE, e))?;
        if let Value::Object(fields) = &mut value {
            fields.remove("report_hash");
        }
        Ok(format!("{HASH_PREFIX}{}", hash_json(&value)?))
    }
}

// ---------------------------------------------------------------------------------------------
// instruments_kraken.toml
// ---------------------------------------------------------------------------------------------

/// Règle d'instrument d'une jambe de recherche Kraken (s11 §4.7).
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentRule {
    /// Symbole canonique (`BTC-USD`).
    pub symbol: String,
    pub tick_size: f64,
    pub lot_size: f64,
    pub min_qty: f64,
    pub min_notional: f64,
}

/// Rend `instruments_kraken.toml` au format de `config/instruments.toml` : une table
/// `[kraken.<SYMBOLE>]` par règle, symboles et clés triés, avec en tête la méthode et la mention
/// « dérivé du dump, minimums non mesurés ».
pub fn render_instruments_toml(
    method: &str,
    rules: &[InstrumentRule],
) -> Result<String, StoreError> {
    let shown = INSTRUMENTS_FILE;
    if method.is_empty()
        || !method
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(bad(shown, format!("méthode « {method} » invalide")));
    }
    let mut sorted: Vec<&InstrumentRule> = rules.iter().collect();
    sorted.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    if let Some(pair) = sorted.windows(2).find(|p| p[0].symbol == p[1].symbol) {
        return Err(bad(shown, format!("symbole {} en double", pair[0].symbol)));
    }
    let mut out = String::new();
    out.push_str(
        "# Règles d'instrument des jambes de recherche Kraken (docs/specs/s11.md §4.7).\n",
    );
    out.push_str(&format!(
        "# Méthode : {method}. Dérivé du dump, minimums non mesurés.\n"
    ));
    out.push_str("# Même format que config/instruments.toml. Son BLAKE3 est écrit dans universe.json : ne pas l'éditer.\n");
    for rule in sorted {
        rule.symbol
            .parse::<Symbol>()
            .map_err(|_| bad(shown, format!("symbole « {} » invalide", rule.symbol)))?;
        for (nom, valeur, strict) in [
            ("tick_size", rule.tick_size, true),
            ("lot_size", rule.lot_size, true),
            ("min_qty", rule.min_qty, false),
            ("min_notional", rule.min_notional, false),
        ] {
            let valide = valeur.is_finite() && (valeur > 0.0 || (!strict && valeur == 0.0));
            if !valide {
                return Err(bad(
                    shown,
                    format!(
                        "{} : {nom} vaut {valeur}, hors des valeurs admises",
                        rule.symbol
                    ),
                ));
            }
        }
        out.push_str(&format!(
            "\n[kraken.{}]\nlot_size = {:?}\nmin_notional = {:?}\nmin_qty = {:?}\ntick_size = {:?}\n",
            rule.symbol, rule.lot_size, rule.min_notional, rule.min_qty, rule.tick_size
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Écriture
// ---------------------------------------------------------------------------------------------

/// Un trimestre à écrire : ses membres (lignes du CSV), le nombre d'éligibles avant la troncature et le
/// hash de qualité de chaque membre.
#[derive(Clone, Debug, PartialEq)]
pub struct QuarterBuild {
    /// `2018-Q1`
    pub quarter: String,
    pub rows: Vec<CsvRow>,
    pub eligible: u32,
    /// Une entrée par membre, `blake3:`.
    pub quality_hashes: BTreeMap<String, String>,
}

/// Une date `--at` à écrire : tous les éligibles à cette date.
#[derive(Clone, Debug, PartialEq)]
pub struct AtBuild {
    /// RFC 3339 UTC.
    pub date: String,
    pub rows: Vec<CsvRow>,
}

/// Tout ce qu'il faut pour écrire un dossier d'univers.
#[derive(Clone, Debug, PartialEq)]
pub struct UniverseBuild {
    pub source: UniverseSource,
    pub rules: UniverseRules,
    pub quarters: Vec<QuarterBuild>,
    pub at: Vec<AtBuild>,
    pub rejected_pairs: Vec<RejectedPair>,
    pub unrecognized_pairs: Vec<String>,
    pub union: Vec<UnionEntry>,
    pub instruments: Vec<InstrumentRule>,
    /// `decimals_v1`
    pub instruments_method: String,
}

/// Écrit si le fichier est absent ou différent.
fn write_if_changed(path: &Path, text: &str) -> Result<(), StoreError> {
    if fs::read(path).is_ok_and(|existing| existing == text.as_bytes()) {
        return Ok(());
    }
    write_bytes_durable(path, text.as_bytes())
}

/// Écrit un dossier d'univers : les CSV, `instruments_kraken.toml`, puis `universe.json` en dernier.
/// Rend le contenu de `universe.json` avec ses hashes.
///
/// **Immutabilité (s11 §4.5).** Si `dir` contient déjà un `universe.json`, il est relu et vérifié, puis :
/// les règles doivent être identiques ([`StoreError::UniverseRulesChanged`]), et chaque trimestre déjà
/// présent doit être reproduit à l'octet, CSV et entrée de `quarters` ([`StoreError::FrozenQuarterChanged`]).
/// Un dump plus récent n'ajoute donc que des trimestres. Rien n'est écrit si une de ces vérifications
/// échoue. `instruments_kraken.toml`, les dates `--at` et les diagnostics ne sont pas figés.
///
/// Les listes `rejected_pairs` et `unrecognized_pairs` sont triées en ordre d'octets à l'écriture ;
/// l'ordre de `union` est celui donné par l'appelant (s11 §4.4).
pub fn write_universe(dir: &Path, build: &UniverseBuild) -> Result<UniverseFile, StoreError> {
    let shown = dir.join(UNIVERSE_FILE).display().to_string();
    build.rules.validate(&shown)?;
    parse_rfc3339_utc(&build.source.coverage_end)?;
    if build.source.zip_sha256.len() != 64
        || !build
            .source
            .zip_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad(
            &shown,
            "source.zip_sha256 : 64 chiffres hexadécimaux minuscules",
        ));
    }

    // Trimestres : CSV rendus, hachés, entrées de `universe.json`.
    let mut files: Vec<(String, String)> = Vec::new();
    let mut quarters: Vec<QuarterEntry> = Vec::with_capacity(build.quarters.len());
    for item in &build.quarters {
        let (start_ms, end_ms) = quarter_bounds(&item.quarter)?;
        if quarters
            .last()
            .is_some_and(|previous| start_ms <= previous.date_ms)
        {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : ordre chronologique strict exigé",
                    item.quarter
                ),
            ));
        }
        let file = quarter_file_name(&item.quarter);
        let csv = render_csv(&file, &item.rows)?;
        check_quarter_content(&shown, &build.rules, &item.quarter, &item.rows)?;
        let members = u32::try_from(item.rows.len()).map_err(|_| {
            bad(
                &shown,
                format!("trimestre {} : trop de membres", item.quarter),
            )
        })?;
        if members > build.rules.top_n || item.eligible < members {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : {members} membres pour top_n {} et {} éligibles",
                    item.quarter, build.rules.top_n, item.eligible
                ),
            ));
        }
        let symbols: BTreeSet<&str> = item.rows.iter().map(|r| r.symbol.as_str()).collect();
        let hashed: BTreeSet<&str> = item.quality_hashes.keys().map(String::as_str).collect();
        if symbols != hashed || symbols.len() != item.rows.len() {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : quality_hashes doit porter exactement les membres, sans doublon",
                    item.quarter
                ),
            ));
        }
        for (symbol, value) in &item.quality_hashes {
            check_hash(&shown, &format!("quality_hashes.{symbol}"), value)?;
        }
        quarters.push(QuarterEntry {
            quarter: item.quarter.clone(),
            date_ms: start_ms,
            end_ms,
            file: file.clone(),
            file_hash: blake3_prefixed(csv.as_bytes()),
            eligible: item.eligible,
            members,
            quality_hashes: item.quality_hashes.clone(),
        });
        files.push((file, csv));
    }

    // Dates `--at`.
    let mut at: Vec<AtEntry> = Vec::with_capacity(build.at.len());
    for item in &build.at {
        let file = at_file_name(&item.date)?;
        if at.iter().any(|entry| entry.file == file) {
            return Err(bad(
                &shown,
                format!("deux dates --at donnent le même fichier {file}"),
            ));
        }
        let csv = render_csv(&file, &item.rows)?;
        let eligible = u32::try_from(item.rows.len())
            .map_err(|_| bad(&shown, format!("{file} : trop de lignes")))?;
        at.push(AtEntry {
            date: item.date.clone(),
            eligible,
            file: file.clone(),
            file_hash: blake3_prefixed(csv.as_bytes()),
        });
        files.push((file, csv));
    }
    at.sort_by(|a, b| a.date.cmp(&b.date));

    // Listes diagnostiques triées en ordre d'octets.
    let mut rejected_pairs = build.rejected_pairs.clone();
    for pair in &mut rejected_pairs {
        pair.reasons.sort();
    }
    rejected_pairs.sort_by(|a, b| a.kraken_pair.cmp(&b.kraken_pair));
    let mut unrecognized_pairs = build.unrecognized_pairs.clone();
    unrecognized_pairs.sort();

    // L'ordre de l'union est celui de l'appelant ; on ne contrôle que la forme.
    let mut seen = BTreeSet::new();
    for entry in &build.union {
        entry.symbol.parse::<Symbol>().map_err(|_| {
            bad(
                &shown,
                format!("union : symbole « {} » invalide", entry.symbol),
            )
        })?;
        quarter_bounds(&entry.first_quarter)?;
        if !seen.insert(entry.symbol.as_str()) {
            return Err(bad(&shown, format!("union : {} en double", entry.symbol)));
        }
    }

    let instruments_text = render_instruments_toml(&build.instruments_method, &build.instruments)?;
    let mut file = UniverseFile {
        schema: UNIVERSE_SCHEMA.to_string(),
        source: build.source.clone(),
        rules: build.rules.clone(),
        rules_hash: build.rules.hash()?,
        quarters,
        at,
        rejected_pairs,
        unrecognized_pairs,
        union: build.union.clone(),
        instruments: InstrumentsRef {
            file: INSTRUMENTS_FILE.to_string(),
            file_hash: blake3_prefixed(instruments_text.as_bytes()),
            method: build.instruments_method.clone(),
        },
        report_hash: String::new(),
    };
    file.report_hash = file.compute_report_hash()?;

    // Immutabilité des trimestres déjà figés.
    if dir.join(UNIVERSE_FILE).is_file() {
        let old = load(dir)?;
        if old.file.rules != file.rules {
            return Err(StoreError::UniverseRulesChanged);
        }
        for entry in &old.file.quarters {
            if file.quarters.iter().find(|q| q.quarter == entry.quarter) != Some(entry) {
                return Err(StoreError::FrozenQuarterChanged {
                    quarter: entry.quarter.clone(),
                });
            }
        }
    }

    for (name, text) in &files {
        write_if_changed(&dir.join(name), text)?;
    }
    write_if_changed(&dir.join(INSTRUMENTS_FILE), &instruments_text)?;
    write_bytes_durable(&dir.join(UNIVERSE_FILE), file.to_json_pretty()?.as_bytes())?;
    Ok(file)
}

/// Contrôles de contenu d'un trimestre : symboles distincts, TF exclus parmi `quality_tfs`.
fn check_quarter_content(
    shown: &str,
    rules: &UniverseRules,
    quarter: &str,
    rows: &[CsvRow],
) -> Result<(), StoreError> {
    let mut symbols = BTreeSet::new();
    for row in rows {
        if !symbols.insert(row.symbol.as_str()) {
            return Err(bad(
                shown,
                format!("trimestre {quarter} : {} en double", row.symbol),
            ));
        }
        if let Some(tf) = row
            .excluded_tfs
            .iter()
            .find(|tf| !rules.quality_tfs.contains(tf))
        {
            return Err(bad(
                shown,
                format!(
                    "trimestre {quarter} : {} exclu en {tf}, hors de quality_tfs",
                    row.symbol
                ),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------------------------

/// Exclusions qualité attendues par les CSV : pour chaque (trimestre, symbole membre), les TF exclus
/// (vide : aucun). `build_dataset_research` les compare à ce que le store calcule (s11 §4.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QualityExpectation {
    entries: BTreeMap<(String, String), Vec<Timeframe>>,
}

impl QualityExpectation {
    /// Construit depuis des triplets (trimestre `AAAA-Qn`, symbole, TF exclus). Trimestre valide,
    /// aucun doublon, TF par durée croissante sans le 1m.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (String, String, Vec<Timeframe>)>,
    ) -> Result<QualityExpectation, StoreError> {
        let mut map = BTreeMap::new();
        for (quarter, symbol, tfs) in entries {
            quarter_bounds(&quarter)?;
            if tfs.contains(&Timeframe::M1) || tfs.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(bad(
                    "qualité attendue",
                    format!("{quarter} {symbol} : TF à ranger par durée croissante, jamais le 1m"),
                ));
            }
            if map.insert((quarter.clone(), symbol.clone()), tfs).is_some() {
                return Err(bad(
                    "qualité attendue",
                    format!("{quarter} {symbol} en double"),
                ));
            }
        }
        Ok(QualityExpectation { entries: map })
    }

    /// Les triplets, triés par (trimestre, symbole).
    pub fn entries(&self) -> &BTreeMap<(String, String), Vec<Timeframe>> {
        &self.entries
    }
}

/// Un trimestre relu : son entrée de `universe.json` et ses lignes.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedQuarter {
    pub entry: QuarterEntry,
    pub rows: Vec<CsvRow>,
}

/// Dossier d'univers relu, tous hashes vérifiés.
#[derive(Clone, Debug)]
pub struct LoadedUniverse {
    dir: PathBuf,
    file: UniverseFile,
    quarters: Vec<LoadedQuarter>,
    schedule: UniverseSchedule,
    instruments_text: String,
}

impl LoadedUniverse {
    /// Dossier lu.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Contenu de `universe.json`.
    pub fn file(&self) -> &UniverseFile {
        &self.file
    }

    /// `rules_hash` (`blake3:...`).
    pub fn rules_hash(&self) -> &str {
        &self.file.rules_hash
    }

    /// Les trimestres relus, dans l'ordre chronologique.
    pub fn quarters(&self) -> &[LoadedQuarter] {
        &self.quarters
    }

    /// Calendrier des membres, lu par le moteur.
    pub fn schedule(&self) -> &UniverseSchedule {
        &self.schedule
    }

    /// Texte vérifié de `instruments_kraken.toml` (BLAKE3 égal à `instruments.file_hash`). Les règles
    /// d'une jambe s'en tirent par `InstrumentRules::from_toml_str(ExchangeId::Kraken, symbole, texte)`.
    pub fn instruments_text(&self) -> &str {
        &self.instruments_text
    }

    /// Chemin de `instruments_kraken.toml`.
    pub fn instruments_path(&self) -> PathBuf {
        self.dir.join(&self.file.instruments.file)
    }

    /// Symboles de l'union des trimestres qui recoupent `[start_ms, end_ms)`, dédoublonnés, dans
    /// l'ordre (trimestre, puis rang) : l'ordre des jambes d'un run de recherche.
    pub fn union_symbols(&self, start_ms: Ts, end_ms: Ts) -> Vec<String> {
        self.schedule
            .union(start_ms, end_ms)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// Identité de l'univers lu sur `[start_ms, end_ms)` (s11 §4.4) :
    /// `"blake3:" + hash_json({ rules_hash, quarters: [{ quarter, file_hash }…] })`, sur les seuls
    /// trimestres qui recoupent la fenêtre, dans l'ordre chronologique.
    ///
    /// [`StoreError::UniverseWindowNotCovered`] si un instant de la fenêtre n'est couvert par aucun
    /// trimestre : un univers qui ne couvre pas la fenêtre n'en est pas l'identité.
    pub fn window_hash(&self, start_ms: Ts, end_ms: Ts) -> Result<String, StoreError> {
        if let Some(missing_ms) = self.schedule.first_gap(start_ms, end_ms) {
            return Err(StoreError::UniverseWindowNotCovered {
                start_ms,
                end_ms,
                missing_ms,
            });
        }
        #[derive(Serialize)]
        struct Item<'a> {
            quarter: &'a str,
            file_hash: &'a str,
        }
        #[derive(Serialize)]
        struct Hashed<'a> {
            rules_hash: &'a str,
            quarters: Vec<Item<'a>>,
        }
        let quarters = self
            .quarters
            .iter()
            .filter(|q| q.entry.date_ms < end_ms && q.entry.end_ms > start_ms)
            .map(|q| Item {
                quarter: &q.entry.quarter,
                file_hash: &q.entry.file_hash,
            })
            .collect();
        let hashed = Hashed {
            rules_hash: &self.file.rules_hash,
            quarters,
        };
        Ok(format!("{HASH_PREFIX}{}", hash_json(&hashed)?))
    }

    /// Référence écrite dans le manifeste d'un dataset de bloc : `rules_hash` et `window_hash`.
    pub fn reference(&self, start_ms: Ts, end_ms: Ts) -> Result<UniverseRef, StoreError> {
        Ok(UniverseRef {
            rules_hash: self.file.rules_hash.clone(),
            window_hash: self.window_hash(start_ms, end_ms)?,
        })
    }

    /// Exclusions qualité des CSV pour les trimestres qui recoupent `[start_ms, end_ms)`.
    pub fn quality_expectation(&self, start_ms: Ts, end_ms: Ts) -> QualityExpectation {
        let entries = self
            .quarters
            .iter()
            .filter(|q| q.entry.date_ms < end_ms && q.entry.end_ms > start_ms)
            .flat_map(|q| {
                q.rows.iter().map(|row| {
                    (
                        (q.entry.quarter.clone(), row.symbol.clone()),
                        row.excluded_tfs.clone(),
                    )
                })
            })
            .collect();
        QualityExpectation { entries }
    }
}

/// Lit un fichier du dossier et vérifie son BLAKE3 contre `expected`.
fn read_verified_text(dir: &Path, name: &str, expected: &str) -> Result<String, StoreError> {
    let path = dir.join(name);
    let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
    let actual = blake3_prefixed(&bytes);
    if actual != expected {
        return Err(StoreError::UniverseHashMismatch {
            path: path.display().to_string(),
            field: "file_hash",
            expected: expected.to_string(),
            actual,
        });
    }
    String::from_utf8(bytes).map_err(|_| bad(&path.display().to_string(), "UTF-8 attendu"))
}

/// Relit un dossier d'univers en vérifiant tout : schéma, `report_hash`, `rules_hash`, `file_hash` de
/// chaque CSV (trimestres et dates `--at`) et du fichier d'instruments, forme canonique des CSV,
/// cohérence des entrées (bornes du trimestre, nombre de membres, `quality_hashes`), listes triées.
pub fn load(dir: &Path) -> Result<LoadedUniverse, StoreError> {
    let json_path = dir.join(UNIVERSE_FILE);
    let shown = json_path.display().to_string();
    let bytes = fs::read(&json_path).map_err(|source| io_error(&json_path, source))?;
    let file: UniverseFile =
        serde_json::from_slice(&bytes).map_err(|source| json_error(&shown, source))?;
    if file.schema != UNIVERSE_SCHEMA {
        return Err(bad(
            &shown,
            format!("schéma `{}` lu, `{UNIVERSE_SCHEMA}` attendu", file.schema),
        ));
    }
    check_hash(&shown, "report_hash", &file.report_hash)?;
    let report = file.compute_report_hash()?;
    if report != file.report_hash {
        return Err(StoreError::UniverseHashMismatch {
            path: shown,
            field: "report_hash",
            expected: file.report_hash,
            actual: report,
        });
    }
    file.rules.validate(&shown)?;
    let rules_hash = file.rules.hash()?;
    if rules_hash != file.rules_hash {
        return Err(StoreError::UniverseHashMismatch {
            path: shown,
            field: "rules_hash",
            expected: file.rules_hash,
            actual: rules_hash,
        });
    }
    parse_rfc3339_utc(&file.source.coverage_end)?;

    let mut quarters: Vec<LoadedQuarter> = Vec::with_capacity(file.quarters.len());
    let mut schedule_quarters = Vec::with_capacity(file.quarters.len());
    for entry in &file.quarters {
        let (start_ms, end_ms) = quarter_bounds(&entry.quarter)?;
        if entry.date_ms != start_ms || entry.end_ms != end_ms {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : bornes différentes du libellé",
                    entry.quarter
                ),
            ));
        }
        if quarters.last().is_some_and(|q| q.entry.date_ms >= start_ms) {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : ordre chronologique strict exigé",
                    entry.quarter
                ),
            ));
        }
        if entry.file != quarter_file_name(&entry.quarter) {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : fichier `{}` inattendu",
                    entry.quarter, entry.file
                ),
            ));
        }
        check_hash(&shown, "file_hash", &entry.file_hash)?;
        let text = read_verified_text(dir, &entry.file, &entry.file_hash)?;
        let rows = parse_csv(&entry.file, &text)?;
        check_quarter_content(&shown, &file.rules, &entry.quarter, &rows)?;
        let members = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        if entry.members != members || members > file.rules.top_n || entry.eligible < members {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : {} membres déclarés, {members} lignes, top_n {}, {} éligibles",
                    entry.quarter, entry.members, file.rules.top_n, entry.eligible
                ),
            ));
        }
        let symbols: BTreeSet<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        let hashed: BTreeSet<&str> = entry.quality_hashes.keys().map(String::as_str).collect();
        if symbols != hashed {
            return Err(bad(
                &shown,
                format!(
                    "trimestre {} : quality_hashes ne porte pas exactement les membres",
                    entry.quarter
                ),
            ));
        }
        for (symbol, value) in &entry.quality_hashes {
            check_hash(&shown, &format!("quality_hashes.{symbol}"), value)?;
        }
        let mut members_list = Vec::with_capacity(rows.len());
        for row in &rows {
            let rank = u8::try_from(row.rank).map_err(|_| {
                bad(
                    &shown,
                    format!("trimestre {} : rang {} trop grand", entry.quarter, row.rank),
                )
            })?;
            members_list.push(Member {
                symbol: row.symbol.clone(),
                rank,
            });
        }
        schedule_quarters.push(UniverseQuarter::new(&entry.quarter, members_list)?);
        quarters.push(LoadedQuarter {
            entry: entry.clone(),
            rows,
        });
    }
    let schedule = UniverseSchedule::new(schedule_quarters)?;

    for entry in &file.at {
        if entry.file != at_file_name(&entry.date)? {
            return Err(bad(
                &shown,
                format!("date {} : fichier `{}` inattendu", entry.date, entry.file),
            ));
        }
        check_hash(&shown, "file_hash", &entry.file_hash)?;
        let text = read_verified_text(dir, &entry.file, &entry.file_hash)?;
        let rows = parse_csv(&entry.file, &text)?;
        if u32::try_from(rows.len()).ok() != Some(entry.eligible) {
            return Err(bad(
                &shown,
                format!(
                    "date {} : {} éligibles déclarés, {} lignes",
                    entry.date,
                    entry.eligible,
                    rows.len()
                ),
            ));
        }
    }
    if file.at.windows(2).any(|pair| pair[0].date >= pair[1].date) {
        return Err(bad(
            &shown,
            "at : dates à ranger par ordre croissant, sans doublon",
        ));
    }
    if file
        .rejected_pairs
        .windows(2)
        .any(|pair| pair[0].kraken_pair >= pair[1].kraken_pair)
        || file
            .unrecognized_pairs
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(bad(
            &shown,
            "rejected_pairs et unrecognized_pairs : ordre d'octets strict exigé",
        ));
    }
    let mut seen = BTreeSet::new();
    for entry in &file.union {
        quarter_bounds(&entry.first_quarter)?;
        if !seen.insert(entry.symbol.as_str()) {
            return Err(bad(&shown, format!("union : {} en double", entry.symbol)));
        }
    }

    if file.instruments.file != INSTRUMENTS_FILE {
        return Err(bad(
            &shown,
            format!("instruments.file : `{}` inattendu", file.instruments.file),
        ));
    }
    check_hash(&shown, "instruments.file_hash", &file.instruments.file_hash)?;
    let instruments_text =
        read_verified_text(dir, &file.instruments.file, &file.instruments.file_hash)?;

    Ok(LoadedUniverse {
        dir: dir.to_path_buf(),
        file,
        quarters,
        schedule,
        instruments_text,
    })
}

// ---------------------------------------------------------------------------------------------
// Sous-ensemble figé de paires (tv4-pair-subset-v1)
// ---------------------------------------------------------------------------------------------

/// Lit une liste figée de paires en vérifiant son schéma, son contenu et son `subset_hash`.
pub fn read_pair_subset(path: &Path) -> Result<PairSubset, StoreError> {
    let text = fs::read_to_string(path).map_err(|source| io_error(path, source))?;
    Ok(PairSubset::from_json_str(&text)?)
}

/// Écrit une liste figée de paires de façon durable. Un fichier existant n'est jamais écrasé
/// ([`StoreError::SubsetExists`]) ; le sous-ensemble est relu avant l'écriture, ce qui contrôle son
/// `subset_hash`.
pub fn write_pair_subset(path: &Path, subset: &PairSubset) -> Result<(), StoreError> {
    let text = subset.to_json_pretty()?;
    PairSubset::from_json_str(&text)?;
    if path.try_exists().map_err(|source| io_error(path, source))? {
        return Err(StoreError::SubsetExists {
            path: path.display().to_string(),
        });
    }
    write_bytes_durable(path, text.as_bytes())
}
