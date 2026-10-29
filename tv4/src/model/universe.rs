//! Types de l'univers trimestriel et des sous-ensembles de paires (docs/specs/s11.md §4, §7 et §8.1).
//!
//! - [`UniverseConfig`] lit `config/universe.toml` (schéma `tv4-universe-config-v1`) : les listes de
//!   cotations, de bases exclues, de motifs de survivance et d'alias OKX. Aucune liste n'est en
//!   constante dans le code.
//! - [`UniverseSchedule`] est le calendrier des membres par trimestre que lit le moteur ; il est
//!   construit par `store::universe::load` depuis le dossier d'univers (hashes vérifiés par le store).
//! - [`QualitySchedule`] porte les exclusions qualité (trimestre, TF) du manifeste d'un dataset de
//!   bloc (décision 8 : le 1m n'exclut jamais rien).
//! - [`UniverseRef`] est la référence d'univers d'un manifeste (`rules_hash`, `window_hash`).
//! - [`PairSubset`] est la liste figée `tv4-pair-subset-v1` (bases tradables sur OKX en USDC).
//!
//! Les trimestres civils UTC (`2018-Q1`) se convertissent en intervalles `[début, fin)` par
//! [`quarter_bounds`] ; ce module ne dépend de rien hors de `model` et n'utilise pas `chrono`, donc
//! il porte sa propre conversion calendaire (recoupée avec `store::time` par `tests/blocks.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::hash::{HASH_PREFIX, blake3_hex, hash_json};
use super::{ModelError, Timeframe, Ts, read_config_text};

/// Schéma de `config/universe.toml`.
pub const UNIVERSE_CONFIG_SCHEMA: &str = "tv4-universe-config-v1";

/// Version des règles d'univers (`rules_version` de `universe.json`, s11 §4.4).
pub const UNIVERSE_RULES_VERSION: &str = "u1";

/// Schéma de la liste figée de paires.
pub const PAIR_SUBSET_SCHEMA: &str = "tv4-pair-subset-v1";

/// `kind` du sous-ensemble des bases tradables sur OKX en USDC (s11 §7.2).
pub const SUBSET_KIND_OKX_USDC: &str = "okx_usdc_tradable";

/// Millisecondes dans un jour.
const DAY_MS: Ts = 86_400_000;

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> ModelError {
    ModelError::UniverseInvalid {
        field: field.into(),
        reason: reason.into(),
    }
}

// ---------------------------------------------------------------------------------------------
// Trimestres civils
// ---------------------------------------------------------------------------------------------

/// Jours depuis 1970-01-01 du jour civil `(y, m, d)` (algorithme de Hinnant). Même résultat que
/// `store::time::days_from_civil`, que `tests/blocks.rs` recoupe.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Intervalle `[début, fin)` en millisecondes UTC du trimestre civil `label` (`AAAA-Qn`, année de
/// 1970 à 9999, `n` de 1 à 4).
pub fn quarter_bounds(label: &str) -> Result<(Ts, Ts), ModelError> {
    let bytes = label.as_bytes();
    let well_formed = bytes.len() == 7
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5] == b'Q'
        && (b'1'..=b'4').contains(&bytes[6]);
    if !well_formed {
        return Err(invalid(
            "quarter",
            format!("`{label}` : la forme attendue est AAAA-Qn (n de 1 à 4)"),
        ));
    }
    let year: i64 = label[..4]
        .parse()
        .map_err(|_| invalid("quarter", format!("`{label}` : année illisible")))?;
    if year < 1970 {
        return Err(invalid(
            "quarter",
            format!("`{label}` : l'année doit être au moins 1970"),
        ));
    }
    let quarter = i64::from(bytes[6] - b'0');
    let start = days_from_civil(year, 3 * (quarter - 1) + 1, 1) * DAY_MS;
    let end = if quarter == 4 {
        days_from_civil(year + 1, 1, 1)
    } else {
        days_from_civil(year, 3 * quarter + 1, 1)
    } * DAY_MS;
    Ok((start, end))
}

// ---------------------------------------------------------------------------------------------
// Calendrier des membres
// ---------------------------------------------------------------------------------------------

/// Un membre d'un trimestre : symbole canonique du store (`BTC-USD`) et rang (1 = plus liquide).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub symbol: String,
    pub rank: u8,
}

/// Un trimestre de l'univers : ses bornes `[start_ms, end_ms)` et ses membres triés par rang.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniverseQuarter {
    /// Libellé `AAAA-Qn`.
    pub quarter: String,
    pub start_ms: Ts,
    pub end_ms: Ts,
    pub members: Vec<Member>,
}

impl UniverseQuarter {
    /// Construit le trimestre `label` (les bornes se déduisent du libellé) et valide les membres :
    /// symboles non vides et distincts, rangs de 1 ou plus, strictement croissants.
    pub fn new(label: &str, members: Vec<Member>) -> Result<UniverseQuarter, ModelError> {
        let (start_ms, end_ms) = quarter_bounds(label)?;
        let mut symbols = BTreeSet::new();
        let mut previous_rank = 0u8;
        for member in &members {
            if member.symbol.is_empty() {
                return Err(invalid(
                    format!("{label}.members"),
                    "symbole vide".to_string(),
                ));
            }
            if !symbols.insert(member.symbol.as_str()) {
                return Err(invalid(
                    format!("{label}.members"),
                    format!("symbole `{}` en double", member.symbol),
                ));
            }
            if member.rank <= previous_rank {
                return Err(invalid(
                    format!("{label}.members"),
                    format!(
                        "le rang de `{}` ({}) doit dépasser le précédent ({previous_rank})",
                        member.symbol, member.rank
                    ),
                ));
            }
            previous_rank = member.rank;
        }
        Ok(UniverseQuarter {
            quarter: label.to_string(),
            start_ms,
            end_ms,
            members,
        })
    }

    /// Rang de `symbol` dans ce trimestre, `None` s'il n'en est pas membre.
    pub fn rank_of(&self, symbol: &str) -> Option<u8> {
        self.members
            .iter()
            .find(|member| member.symbol == symbol)
            .map(|member| member.rank)
    }
}

/// Calendrier des trimestres d'un univers, triés et sans recouvrement (s11 §8.1).
///
/// Les trimestres ne sont pas forcément contigus (le store peut n'en charger qu'une partie) : un
/// trou se détecte par [`UniverseSchedule::first_gap`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UniverseSchedule {
    quarters: Vec<UniverseQuarter>,
}

impl UniverseSchedule {
    /// Valide l'ordre (début strictement croissant, aucun recouvrement, bornes non vides).
    pub fn new(quarters: Vec<UniverseQuarter>) -> Result<UniverseSchedule, ModelError> {
        for quarter in &quarters {
            if quarter.start_ms >= quarter.end_ms {
                return Err(invalid(
                    "quarters",
                    format!("trimestre {} : bornes vides ou inversées", quarter.quarter),
                ));
            }
        }
        for pair in quarters.windows(2) {
            if pair[1].start_ms < pair[0].end_ms {
                return Err(invalid(
                    "quarters",
                    format!(
                        "les trimestres {} et {} ne sont pas triés ou se recouvrent",
                        pair[0].quarter, pair[1].quarter
                    ),
                ));
            }
        }
        Ok(UniverseSchedule { quarters })
    }

    /// Les trimestres, dans l'ordre chronologique.
    pub fn quarters(&self) -> &[UniverseQuarter] {
        &self.quarters
    }

    /// `true` sans aucun trimestre.
    pub fn is_empty(&self) -> bool {
        self.quarters.is_empty()
    }

    /// Trimestre qui contient `ts` (intervalle demi-ouvert), `None` s'il n'y en a pas.
    pub fn quarter_at(&self, ts: Ts) -> Option<&UniverseQuarter> {
        let index = self.quarters.partition_point(|q| q.end_ms <= ts);
        self.quarters.get(index).filter(|q| q.start_ms <= ts)
    }

    /// Trimestres qui recoupent `[start_ms, end_ms)`, dans l'ordre. Vide si la fenêtre est vide.
    pub fn overlapping(&self, start_ms: Ts, end_ms: Ts) -> &[UniverseQuarter] {
        let first = self.quarters.partition_point(|q| q.end_ms <= start_ms);
        let last = self.quarters.partition_point(|q| q.start_ms < end_ms);
        if first < last {
            &self.quarters[first..last]
        } else {
            &[]
        }
    }

    /// Premier instant de `[start_ms, end_ms)` qu'aucun trimestre ne couvre, `None` si la fenêtre
    /// est entièrement couverte (ou vide). Le moteur en tire `UniverseQuarterMissing`.
    pub fn first_gap(&self, start_ms: Ts, end_ms: Ts) -> Option<Ts> {
        let mut cursor = start_ms;
        for quarter in self.overlapping(start_ms, end_ms) {
            if quarter.start_ms > cursor {
                return Some(cursor);
            }
            cursor = cursor.max(quarter.end_ms);
        }
        (cursor < end_ms).then_some(cursor)
    }

    /// Rang de `symbol` dans le trimestre qui contient `ts`, `None` hors univers.
    pub fn rank_at(&self, ts: Ts, symbol: &str) -> Option<u8> {
        self.quarter_at(ts)?.rank_of(symbol)
    }

    /// Symboles des trimestres qui recoupent `[start_ms, end_ms)`, dédoublonnés, dans l'ordre de
    /// première apparition (trimestre, puis rang) : l'ordre des jambes d'un run de recherche
    /// (s11 §4.4, `union`).
    pub fn union(&self, start_ms: Ts, end_ms: Ts) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        let mut symbols = Vec::new();
        for quarter in self.overlapping(start_ms, end_ms) {
            for member in &quarter.members {
                if seen.insert(member.symbol.as_str()) {
                    symbols.push(member.symbol.as_str());
                }
            }
        }
        symbols
    }
}

/// Référence d'univers d'un manifeste de dataset de bloc (s11 §4.4 et §6.1) : les règles et les
/// trimestres lus, sous forme de hashes `blake3:…`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseRef {
    pub rules_hash: String,
    pub window_hash: String,
}

// ---------------------------------------------------------------------------------------------
// Exclusions qualité
// ---------------------------------------------------------------------------------------------

/// Trimestre exclu d'un symbole tel que le manifeste l'écrit : le libellé et les TF de décision
/// exclus (5m à 1d, par durée croissante). Le 1m n'y figure jamais.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarterExclusion {
    pub quarter: String,
    pub tfs: Vec<Timeframe>,
}

/// Trimestre exclu résolu : bornes `[start_ms, end_ms)` et masque des TF exclus (`Timeframe::bit()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExcludedQuarter {
    pub start_ms: Ts,
    pub end_ms: Ts,
    pub tf_mask: u8,
}

/// Exclusions qualité par symbole (`quality_exclusions` du manifeste), résolues en intervalles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QualitySchedule {
    exclusions: BTreeMap<String, Vec<QuarterExclusion>>,
    intervals: BTreeMap<String, Vec<ExcludedQuarter>>,
}

impl QualitySchedule {
    /// Résout les exclusions du manifeste. Pour chaque symbole : trimestres triés, distincts, avec au
    /// moins un TF, TF triés par durée croissante sans doublon, jamais le 1m.
    pub fn from_exclusions(
        exclusions: BTreeMap<String, Vec<QuarterExclusion>>,
    ) -> Result<QualitySchedule, ModelError> {
        let mut intervals = BTreeMap::new();
        for (symbol, list) in &exclusions {
            let field = format!("quality_exclusions.{symbol}");
            let mut resolved: Vec<ExcludedQuarter> = Vec::with_capacity(list.len());
            for exclusion in list {
                let (start_ms, end_ms) = quarter_bounds(&exclusion.quarter)?;
                if resolved
                    .last()
                    .is_some_and(|last| start_ms <= last.start_ms)
                {
                    return Err(invalid(
                        &field,
                        format!(
                            "trimestre {} : l'ordre chronologique strict est exigé",
                            exclusion.quarter
                        ),
                    ));
                }
                if exclusion.tfs.is_empty() {
                    return Err(invalid(
                        &field,
                        format!("trimestre {} : aucun TF exclu", exclusion.quarter),
                    ));
                }
                if exclusion.tfs.contains(&Timeframe::M1) {
                    return Err(invalid(
                        &field,
                        format!(
                            "trimestre {} : le 1m n'exclut jamais rien (décision 8)",
                            exclusion.quarter
                        ),
                    ));
                }
                if exclusion.tfs.windows(2).any(|pair| pair[0] >= pair[1]) {
                    return Err(invalid(
                        &field,
                        format!(
                            "trimestre {} : TF à ranger par durée croissante, sans doublon",
                            exclusion.quarter
                        ),
                    ));
                }
                let tf_mask = exclusion.tfs.iter().fold(0u8, |mask, tf| mask | tf.bit());
                resolved.push(ExcludedQuarter {
                    start_ms,
                    end_ms,
                    tf_mask,
                });
            }
            intervals.insert(symbol.clone(), resolved);
        }
        Ok(QualitySchedule {
            exclusions,
            intervals,
        })
    }

    /// Les exclusions telles que le manifeste les écrit.
    pub fn exclusions(&self) -> &BTreeMap<String, Vec<QuarterExclusion>> {
        &self.exclusions
    }

    /// Trimestres exclus du symbole, triés ; vide pour un symbole sans exclusion.
    pub fn intervals(&self, symbol: &str) -> &[ExcludedQuarter] {
        self.intervals.get(symbol).map_or(&[], Vec::as_slice)
    }

    /// Masque des TF exclus pour `symbol` à l'instant `ts` (0 hors d'un trimestre exclu).
    pub fn mask_at(&self, symbol: &str, ts: Ts) -> u8 {
        let list = self.intervals(symbol);
        let index = list.partition_point(|q| q.end_ms <= ts);
        list.get(index)
            .filter(|q| q.start_ms <= ts)
            .map_or(0, |q| q.tf_mask)
    }

    /// `true` si `tf` est exclu pour `symbol` à l'instant `ts`. Le 1m ne l'est jamais.
    pub fn is_excluded(&self, symbol: &str, ts: Ts, tf: Timeframe) -> bool {
        tf != Timeframe::M1 && self.mask_at(symbol, ts) & tf.bit() != 0
    }

    /// `"blake3:" + hash_json` des exclusions du manifeste (`quality_hash` de l'identité de
    /// recherche, s11 §8.6). Deux plannings vides ont le même hash.
    pub fn hash(&self) -> Result<String, ModelError> {
        Ok(format!("{HASH_PREFIX}{}", hash_json(&self.exclusions)?))
    }
}

// ---------------------------------------------------------------------------------------------
// Sous-ensemble figé de paires
// ---------------------------------------------------------------------------------------------

/// Origine d'un sous-ensemble : le fichier d'instruments dont il dérive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubsetSource {
    pub instruments_file: String,
    pub instruments_hash: String,
    pub site: String,
    pub fetched_at: String,
}

/// Règle appliquée pour tirer le sous-ensemble ; elle entre dans le `subset_hash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubsetRule {
    pub quote: String,
    pub state: String,
    pub trade_quote_ccy: bool,
    /// Alias explicites base Kraken → base OKX (vide par défaut, aucun alias n'est deviné).
    pub aliases: BTreeMap<String, String>,
}

/// Liste figée de bases, schéma `tv4-pair-subset-v1`, hachée.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairSubset {
    pub schema: String,
    pub kind: String,
    pub source: SubsetSource,
    pub rule: SubsetRule,
    /// Bases canoniques, triées en ordre d'octets, sans doublon.
    pub bases: Vec<String>,
    pub subset_hash: String,
}

/// Flux haché d'un sous-ensemble : sans `fetched_at` ni nom de fichier (s11 §7.2).
#[derive(Serialize)]
struct SubsetHashed<'a> {
    schema: &'a str,
    kind: &'a str,
    source: SubsetSourceHashed<'a>,
    rule: &'a SubsetRule,
    bases: &'a [String],
}

#[derive(Serialize)]
struct SubsetSourceHashed<'a> {
    instruments_hash: &'a str,
}

impl PairSubset {
    /// Construit un sous-ensemble : les bases sont triées en ordre d'octets, sans doublon ni base
    /// vide, et le `subset_hash` est calculé.
    pub fn new(
        kind: &str,
        source: SubsetSource,
        rule: SubsetRule,
        mut bases: Vec<String>,
    ) -> Result<PairSubset, ModelError> {
        bases.sort();
        let mut subset = PairSubset {
            schema: PAIR_SUBSET_SCHEMA.to_string(),
            kind: kind.to_string(),
            source,
            rule,
            bases,
            subset_hash: String::new(),
        };
        subset.validate_content()?;
        subset.subset_hash = subset.compute_hash()?;
        Ok(subset)
    }

    /// Lit un sous-ensemble : schéma, contenu et `subset_hash` sont vérifiés.
    pub fn from_json_str(text: &str) -> Result<PairSubset, ModelError> {
        let subset: PairSubset = serde_json::from_str(text)?;
        if subset.schema != PAIR_SUBSET_SCHEMA {
            return Err(invalid(
                "schema",
                format!("`{PAIR_SUBSET_SCHEMA}` attendu, `{}` lu", subset.schema),
            ));
        }
        subset.validate_content()?;
        let expected = subset.compute_hash()?;
        if subset.subset_hash != expected {
            return Err(invalid(
                "subset_hash",
                format!("`{expected}` attendu, `{}` lu", subset.subset_hash),
            ));
        }
        Ok(subset)
    }

    /// JSON indenté à clés triées, `\n` final (forme des manifestes du store).
    pub fn to_json_pretty(&self) -> Result<String, ModelError> {
        let value = serde_json::to_value(self)?;
        let mut text = serde_json::to_string_pretty(&value)?;
        text.push('\n');
        Ok(text)
    }

    /// `"blake3:" + hash_json` de `{schema, kind, source: {instruments_hash}, rule, bases}`.
    pub fn compute_hash(&self) -> Result<String, ModelError> {
        let hashed = SubsetHashed {
            schema: &self.schema,
            kind: &self.kind,
            source: SubsetSourceHashed {
                instruments_hash: &self.source.instruments_hash,
            },
            rule: &self.rule,
            bases: &self.bases,
        };
        Ok(format!("{HASH_PREFIX}{}", hash_json(&hashed)?))
    }

    /// `true` si `base` (canonique Kraken, après `XBT` → `BTC`) est dans la liste.
    pub fn contains_base(&self, base: &str) -> bool {
        self.bases
            .binary_search_by(|candidate| candidate.as_str().cmp(base))
            .is_ok()
    }

    /// `true` sans aucune base.
    pub fn is_empty(&self) -> bool {
        self.bases.is_empty()
    }

    fn validate_content(&self) -> Result<(), ModelError> {
        if self.kind.is_empty() {
            return Err(invalid("kind", "valeur vide".to_string()));
        }
        if self.bases.iter().any(String::is_empty) {
            return Err(invalid("bases", "base vide".to_string()));
        }
        if self.bases.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(invalid(
                "bases",
                "bases à ranger en ordre d'octets, sans doublon".to_string(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// config/universe.toml
// ---------------------------------------------------------------------------------------------

/// Motifs de survivance (s11 §5.1).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurvivorshipConfig {
    /// Préfixes de noms de paire attendus dans le dump (`ICN` s'écrit `ICN*` dans le rapport).
    pub expected: Vec<String>,
}

/// `config/universe.toml`, schéma `tv4-universe-config-v1`, validé.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseConfig {
    pub schema: String,
    pub rules_version: String,
    /// Cotations canoniques admises, par ordre de préférence (la paire USD avant la paire USDT).
    pub quotes: Vec<String>,
    /// Bases écartées : monnaies fiat et stablecoins.
    pub excluded_bases: Vec<String>,
    pub survivorship: SurvivorshipConfig,
    /// Alias explicites base Kraken → base OKX (vide par défaut).
    pub okx_base_aliases: BTreeMap<String, String>,
    #[serde(skip)]
    file_hash: String,
}

/// `true` pour un actif en majuscules ASCII et chiffres, non vide.
fn is_asset_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

impl UniverseConfig {
    /// Lit et valide un texte au format de `config/universe.toml`. Le BLAKE3 des octets du texte est
    /// gardé ([`UniverseConfig::file_hash`]).
    pub fn from_toml_str(text: &str) -> Result<UniverseConfig, ModelError> {
        let mut config: UniverseConfig = toml::from_str(text)?;
        config.validate()?;
        config.file_hash = blake3_hex(text.as_bytes());
        Ok(config)
    }

    /// Lit et valide un fichier (en pratique `config/universe.toml`).
    pub fn load(path: &Path) -> Result<UniverseConfig, ModelError> {
        UniverseConfig::from_toml_str(&read_config_text(path)?)
    }

    /// BLAKE3 des octets du fichier lu, en hexadécimal, **sans** préfixe : l'appelant ajoute
    /// `blake3:` pour `config_file_hash` de `universe.json`.
    pub fn file_hash(&self) -> &str {
        &self.file_hash
    }

    /// `true` si `base` est une monnaie fiat ou un stablecoin écarté.
    pub fn is_excluded_base(&self, base: &str) -> bool {
        self.excluded_bases.iter().any(|b| b == base)
    }

    /// Bases écartées en ordre d'octets (forme de `universe.json`).
    pub fn sorted_excluded_bases(&self) -> Vec<&str> {
        let mut bases: Vec<&str> = self.excluded_bases.iter().map(String::as_str).collect();
        bases.sort_unstable();
        bases
    }

    /// Base OKX d'une base Kraken canonique : l'alias explicite s'il existe, sinon la base elle-même
    /// (égalité stricte, s11 §7.1).
    pub fn okx_base<'a>(&'a self, base: &'a str) -> &'a str {
        self.okx_base_aliases.get(base).map_or(base, String::as_str)
    }

    fn validate(&self) -> Result<(), ModelError> {
        if self.schema != UNIVERSE_CONFIG_SCHEMA {
            return Err(invalid(
                "schema",
                format!("`{UNIVERSE_CONFIG_SCHEMA}` attendu, `{}` lu", self.schema),
            ));
        }
        if self.rules_version != UNIVERSE_RULES_VERSION {
            return Err(invalid(
                "rules_version",
                format!(
                    "`{UNIVERSE_RULES_VERSION}` attendu, `{}` lu",
                    self.rules_version
                ),
            ));
        }
        if self.quotes.is_empty() {
            return Err(invalid("quotes", "au moins une cotation".to_string()));
        }
        for (name, list) in [
            ("quotes", &self.quotes),
            ("excluded_bases", &self.excluded_bases),
            ("survivorship.expected", &self.survivorship.expected),
        ] {
            let mut seen = BTreeSet::new();
            for item in list {
                if !is_asset_name(item) {
                    return Err(invalid(
                        name,
                        format!("`{item}` : majuscules et chiffres ASCII attendus, non vide"),
                    ));
                }
                if !seen.insert(item.as_str()) {
                    return Err(invalid(name, format!("`{item}` en double")));
                }
            }
        }
        if self.survivorship.expected.is_empty() {
            return Err(invalid(
                "survivorship.expected",
                "au moins un motif".to_string(),
            ));
        }
        for (kraken, okx) in &self.okx_base_aliases {
            if !is_asset_name(kraken) || !is_asset_name(okx) {
                return Err(invalid(
                    "okx_base_aliases",
                    format!("`{kraken}` = `{okx}` : majuscules et chiffres ASCII attendus"),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
schema = "tv4-universe-config-v1"
rules_version = "u1"
quotes = ["USD", "USDT"]
excluded_bases = ["USDT", "EUR", "USD"]

[survivorship]
expected = ["ICN", "FTT"]

[okx_base_aliases]
"#;

    fn member(symbol: &str, rank: u8) -> Member {
        Member {
            symbol: symbol.to_string(),
            rank,
        }
    }

    fn quarter(label: &str, symbols: &[&str]) -> UniverseQuarter {
        let members = symbols
            .iter()
            .enumerate()
            .map(|(i, s)| member(s, u8::try_from(i + 1).unwrap()))
            .collect();
        UniverseQuarter::new(label, members).unwrap()
    }

    fn schedule() -> UniverseSchedule {
        UniverseSchedule::new(vec![
            quarter("2018-Q1", &["BTC-USD", "ETH-USD"]),
            quarter("2018-Q2", &["ETH-USD", "LTC-USD", "BTC-USD"]),
            quarter("2018-Q4", &["XRP-USD"]),
        ])
        .unwrap()
    }

    // Les bornes de trimestres sont des constantes de calendrier sans rapport avec le marché.
    #[test]
    fn bornes_de_trimestre() {
        // 2018-01-01 est le jour 17 532 depuis l'epoch, 2018-04-01 le jour 17 622.
        assert_eq!(
            quarter_bounds("2018-Q1").unwrap(),
            (17_532 * DAY_MS, 17_622 * DAY_MS)
        );
        let (q4_start, q4_end) = quarter_bounds("2019-Q4").unwrap();
        let (next_start, _) = quarter_bounds("2020-Q1").unwrap();
        assert_eq!(q4_end, next_start);
        assert_eq!((q4_end - q4_start) / DAY_MS, 92);
        // 2020 est bissextile : le premier trimestre compte 91 jours.
        let (a, b) = quarter_bounds("2020-Q1").unwrap();
        assert_eq!((b - a) / DAY_MS, 91);
    }

    #[test]
    fn libelles_de_trimestre_refuses() {
        for label in [
            "", "2018", "2018-Q0", "2018-Q5", "2018-q1", "18-Q1", "2018Q1x", "1969-Q4", "é018-Q1",
            "2018-Q1 ",
        ] {
            assert!(quarter_bounds(label).is_err(), "`{label}` accepté");
        }
    }

    #[test]
    fn trimestre_valide_ses_membres() {
        assert!(UniverseQuarter::new("2018-Q1", vec![member("", 1)]).is_err());
        assert!(
            UniverseQuarter::new("2018-Q1", vec![member("A-USD", 1), member("A-USD", 2)]).is_err()
        );
        assert!(
            UniverseQuarter::new("2018-Q1", vec![member("A-USD", 2), member("B-USD", 1)]).is_err()
        );
        assert!(UniverseQuarter::new("2018-Q1", vec![member("A-USD", 0)]).is_err());
        assert!(UniverseQuarter::new("2018-Qx", vec![]).is_err());
        let q = quarter("2018-Q1", &["BTC-USD", "ETH-USD"]);
        assert_eq!(q.rank_of("ETH-USD"), Some(2));
        assert_eq!(q.rank_of("SOL-USD"), None);
    }

    #[test]
    fn calendrier_refuse_desordre_et_recouvrement() {
        let q1 = quarter("2018-Q1", &["A-USD"]);
        let q2 = quarter("2018-Q2", &["A-USD"]);
        assert!(UniverseSchedule::new(vec![q2.clone(), q1.clone()]).is_err());
        assert!(UniverseSchedule::new(vec![q1.clone(), q1.clone()]).is_err());
        let mut vide = q1.clone();
        vide.end_ms = vide.start_ms;
        assert!(UniverseSchedule::new(vec![vide]).is_err());
        assert!(UniverseSchedule::new(vec![q1, q2]).is_ok());
        assert!(UniverseSchedule::default().is_empty());
    }

    #[test]
    fn trimestre_a_l_instant_demi_ouvert() {
        let s = schedule();
        let (q2_start, q2_end) = quarter_bounds("2018-Q2").unwrap();
        assert_eq!(s.quarter_at(q2_start).unwrap().quarter, "2018-Q2");
        assert_eq!(s.quarter_at(q2_start - 1).unwrap().quarter, "2018-Q1");
        // Q3 manque : la fin de Q2 n'appartient à aucun trimestre du calendrier.
        assert!(s.quarter_at(q2_end).is_none());
        assert!(s.quarter_at(q2_end - 1).is_some());
        assert!(s.quarter_at(0).is_none());
        assert_eq!(s.rank_at(q2_start, "LTC-USD"), Some(2));
        assert_eq!(s.rank_at(q2_start - 1, "LTC-USD"), None);
        assert_eq!(s.rank_at(q2_end, "BTC-USD"), None);
    }

    #[test]
    fn trimestres_qui_recoupent_une_fenetre() {
        let s = schedule();
        let (q1_start, q1_end) = quarter_bounds("2018-Q1").unwrap();
        let (_, q2_end) = quarter_bounds("2018-Q2").unwrap();
        let labels = |a, b| -> Vec<String> {
            s.overlapping(a, b)
                .iter()
                .map(|q| q.quarter.clone())
                .collect()
        };
        assert_eq!(labels(q1_start, q1_end), ["2018-Q1"]);
        assert_eq!(labels(q1_end - 1, q1_end + 1), ["2018-Q1", "2018-Q2"]);
        assert_eq!(labels(q1_start, q2_end), ["2018-Q1", "2018-Q2"]);
        assert!(labels(q1_end, q1_end).is_empty());
        assert!(labels(q1_end, q1_start).is_empty());
        assert!(labels(0, q1_start).is_empty());
    }

    #[test]
    fn premier_trou_du_calendrier() {
        let s = schedule();
        let (q1_start, _) = quarter_bounds("2018-Q1").unwrap();
        let (q3_start, q3_end) = quarter_bounds("2018-Q3").unwrap();
        let (_, q4_end) = quarter_bounds("2018-Q4").unwrap();
        assert_eq!(s.first_gap(q1_start, q3_start), None);
        assert_eq!(s.first_gap(q1_start, q3_end), Some(q3_start));
        assert_eq!(s.first_gap(q3_start, q3_end), Some(q3_start));
        assert_eq!(s.first_gap(q1_start - 5, q1_start + 5), Some(q1_start - 5));
        assert_eq!(s.first_gap(q3_end, q4_end), None);
        assert_eq!(s.first_gap(q3_end, q4_end + 1), Some(q4_end));
        assert_eq!(s.first_gap(q3_end, q3_end), None);
        assert_eq!(UniverseSchedule::default().first_gap(5, 10), Some(5));
    }

    #[test]
    fn union_dans_l_ordre_de_premiere_apparition() {
        let s = schedule();
        let (q1_start, _) = quarter_bounds("2018-Q1").unwrap();
        let (_, q4_end) = quarter_bounds("2018-Q4").unwrap();
        assert_eq!(
            s.union(q1_start, q4_end),
            ["BTC-USD", "ETH-USD", "LTC-USD", "XRP-USD"]
        );
        let (q2_start, q2_end) = quarter_bounds("2018-Q2").unwrap();
        assert_eq!(s.union(q2_start, q2_end), ["ETH-USD", "LTC-USD", "BTC-USD"]);
        assert!(s.union(q2_end, q2_end).is_empty());
    }

    fn exclusions() -> BTreeMap<String, Vec<QuarterExclusion>> {
        let mut map = BTreeMap::new();
        map.insert(
            "ETH-USD".to_string(),
            vec![
                QuarterExclusion {
                    quarter: "2018-Q1".to_string(),
                    tfs: vec![Timeframe::H1, Timeframe::H4, Timeframe::D1],
                },
                QuarterExclusion {
                    quarter: "2018-Q3".to_string(),
                    tfs: vec![Timeframe::H4],
                },
            ],
        );
        map
    }

    #[test]
    fn exclusions_qualite_par_tf_et_par_instant() {
        let q = QualitySchedule::from_exclusions(exclusions()).unwrap();
        let (q1_start, q1_end) = quarter_bounds("2018-Q1").unwrap();
        let (q3_start, _) = quarter_bounds("2018-Q3").unwrap();
        assert!(q.is_excluded("ETH-USD", q1_start, Timeframe::H4));
        assert!(q.is_excluded("ETH-USD", q1_end - 1, Timeframe::D1));
        assert!(!q.is_excluded("ETH-USD", q1_end, Timeframe::H4));
        assert!(!q.is_excluded("ETH-USD", q1_start - 1, Timeframe::H4));
        // Le 1m, et un TF absent de la liste, ne sont jamais exclus.
        assert!(!q.is_excluded("ETH-USD", q1_start, Timeframe::M1));
        assert!(!q.is_excluded("ETH-USD", q1_start, Timeframe::M15));
        assert!(!q.is_excluded("ETH-USD", q3_start, Timeframe::D1));
        assert!(q.is_excluded("ETH-USD", q3_start, Timeframe::H4));
        assert!(!q.is_excluded("BTC-USD", q1_start, Timeframe::H4));
        assert_eq!(
            q.mask_at("ETH-USD", q1_start),
            Timeframe::H1.bit() | Timeframe::H4.bit() | Timeframe::D1.bit()
        );
        assert_eq!(q.mask_at("BTC-USD", q1_start), 0);
        assert_eq!(q.intervals("ETH-USD").len(), 2);
        assert!(q.intervals("BTC-USD").is_empty());
        assert_eq!(q.exclusions(), &exclusions());
    }

    #[test]
    fn exclusions_qualite_mal_formees_refusees() {
        let with = |list: Vec<QuarterExclusion>| {
            let mut map = BTreeMap::new();
            map.insert("A-USD".to_string(), list);
            QualitySchedule::from_exclusions(map)
        };
        let ex = |quarter: &str, tfs: &[Timeframe]| QuarterExclusion {
            quarter: quarter.to_string(),
            tfs: tfs.to_vec(),
        };
        assert!(with(vec![ex("2018-Q1", &[])]).is_err());
        assert!(with(vec![ex("2018-Q1", &[Timeframe::M1])]).is_err());
        assert!(with(vec![ex("2018-Q1", &[Timeframe::H4, Timeframe::H1])]).is_err());
        assert!(with(vec![ex("2018-Q1", &[Timeframe::H4, Timeframe::H4])]).is_err());
        assert!(
            with(vec![
                ex("2018-Q2", &[Timeframe::H4]),
                ex("2018-Q1", &[Timeframe::H4])
            ])
            .is_err()
        );
        assert!(
            with(vec![
                ex("2018-Q1", &[Timeframe::H4]),
                ex("2018-Q1", &[Timeframe::D1])
            ])
            .is_err()
        );
        assert!(with(vec![ex("T1", &[Timeframe::H4])]).is_err());
        assert!(with(vec![]).is_ok());
    }

    #[test]
    fn hash_des_exclusions_est_stable_et_distingue() {
        let vide = QualitySchedule::default();
        let a = QualitySchedule::from_exclusions(exclusions()).unwrap();
        let hash = vide.hash().unwrap();
        assert!(hash.starts_with("blake3:"));
        assert_eq!(hash.len(), "blake3:".len() + 64);
        assert_eq!(
            hash,
            QualitySchedule::from_exclusions(BTreeMap::new())
                .unwrap()
                .hash()
                .unwrap()
        );
        assert_ne!(hash, a.hash().unwrap());
        assert_eq!(a.hash().unwrap(), a.clone().hash().unwrap());
    }

    #[test]
    fn exclusion_serde_refuse_un_champ_inconnu() {
        let ok: QuarterExclusion =
            serde_json::from_str(r#"{"quarter":"2018-Q1","tfs":["1h","4h"]}"#).unwrap();
        assert_eq!(ok.tfs, [Timeframe::H1, Timeframe::H4]);
        assert!(
            serde_json::from_str::<QuarterExclusion>(r#"{"quarter":"2018-Q1","tfs":[],"x":1}"#)
                .is_err()
        );
        let reference = UniverseRef {
            rules_hash: "blake3:a".to_string(),
            window_hash: "blake3:b".to_string(),
        };
        let json = serde_json::to_string(&reference).unwrap();
        assert_eq!(
            json,
            r#"{"rules_hash":"blake3:a","window_hash":"blake3:b"}"#
        );
        assert_eq!(
            serde_json::from_str::<UniverseRef>(&json).unwrap(),
            reference
        );
        assert!(serde_json::from_str::<UniverseRef>(r#"{"rules_hash":"a"}"#).is_err());
    }

    fn subset() -> PairSubset {
        PairSubset::new(
            SUBSET_KIND_OKX_USDC,
            SubsetSource {
                instruments_file: "spot-synthetique.json".to_string(),
                instruments_hash: "blake3:abc".to_string(),
                site: "eea".to_string(),
                fetched_at: "2026-01-01T00:00:00Z".to_string(),
            },
            SubsetRule {
                quote: "USDC".to_string(),
                state: "live".to_string(),
                trade_quote_ccy: true,
                aliases: BTreeMap::new(),
            },
            vec!["SOL".to_string(), "BTC".to_string(), "ETH".to_string()],
        )
        .unwrap()
    }

    #[test]
    fn sous_ensemble_trie_hache_et_relu() {
        let s = subset();
        assert_eq!(s.bases, ["BTC", "ETH", "SOL"]);
        assert!(s.contains_base("ETH"));
        assert!(!s.contains_base("ADA"));
        assert!(!s.is_empty());
        let text = s.to_json_pretty().unwrap();
        assert!(text.ends_with("}\n"));
        assert_eq!(PairSubset::from_json_str(&text).unwrap(), s);
        assert_eq!(s.subset_hash, s.compute_hash().unwrap());
    }

    #[test]
    fn hash_du_sous_ensemble_ignore_date_et_nom_de_fichier() {
        let a = subset();
        let mut b = subset();
        b.source.fetched_at = "2027-02-02T00:00:00Z".to_string();
        b.source.instruments_file = "autre.json".to_string();
        assert_eq!(a.compute_hash().unwrap(), b.compute_hash().unwrap());
        b.source.instruments_hash = "blake3:def".to_string();
        assert_ne!(a.compute_hash().unwrap(), b.compute_hash().unwrap());
        let mut c = subset();
        c.rule.aliases.insert("XDG".to_string(), "DOGE".to_string());
        assert_ne!(a.compute_hash().unwrap(), c.compute_hash().unwrap());
    }

    #[test]
    fn sous_ensemble_modifie_a_la_main_refuse() {
        let text = subset().to_json_pretty().unwrap();
        let altere = text.replace("\"SOL\"", "\"ADA\"");
        assert!(PairSubset::from_json_str(&altere).is_err());
        let schema = text.replace("tv4-pair-subset-v1", "tv4-pair-subset-v2");
        assert!(PairSubset::from_json_str(&schema).is_err());
        let champ = text.replace("\"bases\"", "\"extra\": 1,\n  \"bases\"");
        assert!(PairSubset::from_json_str(&champ).is_err());
        assert!(PairSubset::from_json_str("pas du json").is_err());
    }

    #[test]
    fn sous_ensemble_refuse_doublon_et_base_vide() {
        let build = |bases: Vec<&str>| {
            let s = subset();
            PairSubset::new(
                &s.kind,
                s.source.clone(),
                s.rule.clone(),
                bases.into_iter().map(str::to_string).collect(),
            )
        };
        assert!(build(vec!["BTC", "BTC"]).is_err());
        assert!(build(vec!["BTC", ""]).is_err());
        assert!(build(vec![]).unwrap().is_empty());
        let s = subset();
        assert!(PairSubset::new("", s.source, s.rule, vec![]).is_err());
    }

    #[test]
    fn configuration_valide() {
        let config = UniverseConfig::from_toml_str(CONFIG).unwrap();
        assert_eq!(config.quotes, ["USD", "USDT"]);
        assert!(config.is_excluded_base("EUR"));
        assert!(!config.is_excluded_base("BTC"));
        assert_eq!(config.sorted_excluded_bases(), ["EUR", "USD", "USDT"]);
        assert_eq!(config.okx_base("BTC"), "BTC");
        assert_eq!(config.file_hash(), blake3_hex(CONFIG.as_bytes()));
        assert_eq!(config.survivorship.expected, ["ICN", "FTT"]);
    }

    #[test]
    fn alias_okx_explicite() {
        let text = CONFIG.replace(
            "[okx_base_aliases]\n",
            "[okx_base_aliases]\nLUNA = \"LUNC\"\n",
        );
        let config = UniverseConfig::from_toml_str(&text).unwrap();
        assert_eq!(config.okx_base("LUNA"), "LUNC");
        assert_eq!(config.okx_base("BTC"), "BTC");
    }

    #[test]
    fn configuration_invalide_refusee() {
        let cas = [
            CONFIG.replace("tv4-universe-config-v1", "tv4-universe-config-v2"),
            CONFIG.replace("\"u1\"", "\"u2\""),
            CONFIG.replace("quotes = [\"USD\", \"USDT\"]", "quotes = []"),
            CONFIG.replace("\"USDT\", \"EUR\"", "\"USDT\", \"USDT\""),
            CONFIG.replace("\"EUR\"", "\"eur\""),
            CONFIG.replace("expected = [\"ICN\", \"FTT\"]", "expected = []"),
            CONFIG.replace("\"ICN\"", "\"ICN*\""),
            CONFIG.replace(
                "[okx_base_aliases]\n",
                "[okx_base_aliases]\nluna = \"LUNC\"\n",
            ),
            CONFIG.replace("[okx_base_aliases]\n", "[okx_base_aliases]\nLUNA = \"\"\n"),
            CONFIG.replace(
                "rules_version = \"u1\"",
                "rules_version = \"u1\"\ninconnu = 1",
            ),
            CONFIG.replace("[okx_base_aliases]\n", ""),
            "n'importe quoi =".to_string(),
        ];
        for (i, text) in cas.iter().enumerate() {
            assert!(
                UniverseConfig::from_toml_str(text).is_err(),
                "cas {i} accepté"
            );
        }
    }

    #[test]
    fn lecture_d_un_fichier_absent_est_une_erreur() {
        let erreur = UniverseConfig::load(Path::new("/nonexistent/universe.toml")).unwrap_err();
        assert!(matches!(erreur, ModelError::ConfigRead { .. }));
    }
}
