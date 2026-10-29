//! Test de survivance du dump Kraken (docs/specs/s11.md §5, docs/04 §2.2).
//!
//! Le dump ne dit pas si les paires délistées y figurent. Ce module le mesure, sans rien deviner :
//!
//! - **motifs attendus** (`config/universe.toml`, `survivorship.expected`) : un motif `X*` correspond à
//!   toute entrée dont le **nom de paire commence** par `X`, toutes cotations et résolutions
//!   confondues (`AICNUSD` ne correspond pas à `ICN*`, `USDTUSD` ne correspond pas à `UST*`). Si les
//!   paires d'un motif ont plusieurs bases canoniques (`LUNAUSD` et `LUNA2USD`), l'entrée porte
//!   `review_needed` et la liste de ces bases : `present` n'est pas corrigé par le code, le pilote
//!   tranche sur la liste publiée, dates à l'appui (§5.1) ;
//! - **dernière bougie** de chaque paire, lue dans l'entrée de plus grande résolution présente parmi
//!   `1440, 720, 240, 60, 15, 5, 1` (§5.2) ;
//! - **délistées** : paires sans bougie dans les 90 derniers jours de la couverture, avec la plus
//!   ancienne dernière bougie (`earliest`) comme borne haute de la date de début du programme de
//!   dumps : une paire délistée avant ce début est absente du zip, une paire délistée après y figure.
//!
//! # Déroulé pour la CLI (lot F)
//!
//! [`plan_entries`] classe les noms d'entrées du zip ; [`span_of_rows`] lit les bornes d'une entrée ;
//! [`build_report`] assemble le rapport `tv4-survivorship-v1`, déterministe (aucun horodatage de
//! construction, listes triées en ordre d'octets, JSON à clés triées). [`survey_zip`] fait les trois
//! sur un zip ouvert par `KrakenZip::open_verified`.
//!
//! # Entrées non reconnues
//!
//! `unrecognized_entries` liste les entrées dont le nom n'a pas la forme `<PAIRE>_<n>.csv`, dont la
//! paire n'est pas valide ou est de forme héritée (`XXBTZUSD`, §4.1), et celles qui ne contiennent
//! aucune bougie ou qu'on n'a pas pu lire. `MANIFEST.json` n'est pas une entrée de paire. Ces entrées
//! ne participent ni aux motifs ni aux délistées.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::marketdata::kraken::{KrakenError, RawRow};
use crate::marketdata::kraken_zip::{KrakenPair, KrakenZip, KrakenZipError, MANIFEST_ENTRY};
use crate::model::hash::hash_json;
use crate::model::universe::UniverseConfig;
use crate::model::{ModelError, Ts};
use crate::store::StoreError;
use crate::store::time::{MS_PER_DAY, format_rfc3339_utc};

use super::universe::is_legacy_pair_name;

/// Schéma du rapport.
pub const SCHEMA: &str = "tv4-survivorship-v1";

/// Une paire sans bougie dans ce nombre de jours avant la fin de couverture est « probablement
/// délistée » (§5.2).
pub const DELISTED_THRESHOLD_DAYS: i64 = 90;

/// Résolutions candidates pour dater la dernière bougie, de la plus grande à la plus petite (§5.2).
pub const SOURCE_RESOLUTIONS: [u32; 7] = [1440, 720, 240, 60, 15, 5, 1];

/// Borne basse de la date de début du programme de dumps : une donnée externe de 04 §2.2, recopiée
/// dans le rapport comme hypothèse, pas comme mesure (§5.2).
pub const PROGRAM_START_HYPOTHESIS: &str = "2019-04 (BSV absent, docs/04 §2.2)";

/// Méthode d'estimation de la borne haute.
pub const PROGRAM_START_METHOD: &str = "earliest_delisted_present";

/// Erreurs du test de survivance.
#[derive(Debug, Error)]
pub enum SurvivorshipError {
    #[error(transparent)]
    Zip(#[from] KrakenZipError),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Model(#[from] ModelError),

    #[error("rapport de survivance : JSON impossible : {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------------------------
// Classement des entrées
// ---------------------------------------------------------------------------------------------

/// Une paire du zip avec ses résolutions et l'entrée qui sert à dater sa dernière bougie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedPair {
    /// Nom dans le zip (`XBTUSD`).
    pub kraken_pair: String,
    /// Résolutions présentes (minutes), croissantes.
    pub resolutions: Vec<u32>,
    /// Plus grande résolution présente parmi [`SOURCE_RESOLUTIONS`] ; à défaut, la plus petite
    /// présente.
    pub source_resolution: u32,
    /// Nom de l'entrée de cette résolution (`XBTUSD_1440.csv`).
    pub source_entry: String,
}

/// Entrées du zip classées.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryPlan {
    /// Paires reconnues, triées par nom en ordre d'octets.
    pub pairs: Vec<PlannedPair>,
    /// Entrées non reconnues (voir la doc du module), triées en ordre d'octets.
    pub unrecognized_entries: Vec<String>,
}

/// Décompose `PAIRE_1440.csv` en (`PAIRE`, 1440).
fn split_entry(name: &str) -> Option<(&str, u32)> {
    let stem = name.strip_suffix(".csv")?;
    let (pair, resolution) = stem.rsplit_once('_')?;
    if pair.is_empty() || resolution.is_empty() || !resolution.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let resolution: u32 = resolution.parse().ok()?;
    (resolution > 0).then_some((pair, resolution))
}

/// Classe les noms d'entrées du zip, dans n'importe quel ordre (`entry_names()` rend l'ordre du
/// répertoire central, sans tri).
pub fn plan_entries(entry_names: &[String]) -> EntryPlan {
    let mut by_pair: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    let mut unrecognized: BTreeSet<String> = BTreeSet::new();
    for name in entry_names {
        if name == MANIFEST_ENTRY {
            continue;
        }
        match split_entry(name) {
            Some((pair, resolution))
                if !is_legacy_pair_name(pair) && pair.parse::<KrakenPair>().is_ok() =>
            {
                by_pair.entry(pair).or_default().insert(resolution);
            }
            _ => {
                unrecognized.insert(name.clone());
            }
        }
    }
    let pairs = by_pair
        .into_iter()
        .filter_map(|(pair, resolutions)| {
            let source_resolution = SOURCE_RESOLUTIONS
                .iter()
                .copied()
                .find(|r| resolutions.contains(r))
                .or_else(|| resolutions.iter().next().copied())?;
            Some(PlannedPair {
                kraken_pair: pair.to_string(),
                resolutions: resolutions.iter().copied().collect(),
                source_resolution,
                source_entry: format!("{pair}_{source_resolution}.csv"),
            })
        })
        .collect();
    EntryPlan {
        pairs,
        unrecognized_entries: unrecognized.into_iter().collect(),
    }
}

/// Première et dernière `open_time` (ms) d'un flux de lignes brutes, ou `None` s'il est vide. Le flux
/// n'a pas besoin d'être trié. La première erreur de lecture est rendue.
pub fn span_of_rows<I>(rows: I) -> Result<Option<(Ts, Ts)>, KrakenError>
where
    I: Iterator<Item = Result<RawRow, KrakenError>>,
{
    let mut span: Option<(Ts, Ts)> = None;
    for row in rows {
        let t = row?.open_time_ms;
        span = Some(span.map_or((t, t), |(first, last)| (first.min(t), last.max(t))));
    }
    Ok(span)
}

// ---------------------------------------------------------------------------------------------
// Rapport
// ---------------------------------------------------------------------------------------------

/// Observation d'une paire : ses résolutions et les bornes de l'entrée source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairObservation {
    pub kraken_pair: String,
    pub resolutions: Vec<u32>,
    pub source_resolution: u32,
    pub first_ms: Ts,
    pub last_ms: Ts,
}

/// Ce que le manifeste du zip apporte au rapport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipSummary {
    pub sha256: String,
    pub release: Option<String>,
    pub coverage_start: Option<String>,
    pub generated_at: Option<String>,
    /// Nombre d'entrées du zip, `MANIFEST.json` compris.
    pub entries: u64,
}

/// Section `zip` du rapport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZipSection {
    pub sha256: String,
    pub release: Option<String>,
    pub coverage_start: Option<String>,
    /// RFC 3339 UTC : fin exclusive de la couverture utilisée pour le seuil des 90 jours.
    pub coverage_end: String,
    pub generated_at: Option<String>,
    pub entries: u64,
    /// Nombre de paires reconnues.
    pub pairs: u64,
}

/// Une paire qui correspond à un motif.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedPair {
    pub kraken_pair: String,
    pub resolutions: Vec<u32>,
    /// RFC 3339 UTC de la première bougie de l'entrée source.
    pub first: String,
    /// RFC 3339 UTC de la dernière bougie de l'entrée source.
    pub last: String,
    pub source_resolution: u32,
}

/// Un motif attendu (`ICN*`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedEntry {
    pub pattern: String,
    pub present: bool,
    /// Plusieurs bases canoniques derrière le même préfixe : le pilote tranche (§5.1).
    pub review_needed: bool,
    /// Bases canoniques des paires trouvées, triées en ordre d'octets.
    pub bases: Vec<String>,
    pub pairs: Vec<ExpectedPair>,
}

/// Une paire délistée (probablement) et sa dernière bougie.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelistedPair {
    pub kraken_pair: String,
    pub last: String,
}

/// Section `delisted`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delisted {
    pub threshold_days: i64,
    pub count: u64,
    /// La plus ancienne dernière bougie (départage : nom de paire en ordre d'octets).
    pub earliest: Option<DelistedPair>,
    /// Toutes les délistées, triées par nom de paire en ordre d'octets.
    pub pairs: Vec<DelistedPair>,
}

/// Section `program_start` : une borne haute mesurée et une borne basse hypothétique (§5.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramStart {
    pub at_most: Option<String>,
    pub at_least_hypothesis: String,
    pub method: String,
}

/// Rapport `tv4-survivorship-v1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurvivorshipReport {
    pub schema: String,
    pub zip: ZipSection,
    pub expected: Vec<ExpectedEntry>,
    /// Motifs sans aucune paire (`ICN*`), dans l'ordre de la configuration.
    pub absent: Vec<String>,
    pub delisted: Delisted,
    pub program_start: ProgramStart,
    pub unrecognized_entries: Vec<String>,
    /// `"blake3:" + hash_json` de l'objet sans `report_hash`.
    pub report_hash: String,
}

impl SurvivorshipReport {
    /// JSON indenté à clés triées, `\n` final (forme des manifestes du store).
    pub fn to_json_pretty(&self) -> Result<String, SurvivorshipError> {
        let value = serde_json::to_value(self)?;
        let mut text = serde_json::to_string_pretty(&value)?;
        text.push('\n');
        Ok(text)
    }

    /// Hash de l'objet entier sans `report_hash`.
    pub fn compute_report_hash(&self) -> Result<String, SurvivorshipError> {
        let mut value = serde_json::to_value(self)?;
        if let Value::Object(fields) = &mut value {
            fields.remove("report_hash");
        }
        Ok(format!("blake3:{}", hash_json(&value)?))
    }
}

/// Base canonique d'un nom de paire Kraken (`XBTUSD` → `BTC`), `None` si le nom n'est pas valide.
fn base_of(pair: &str) -> Option<String> {
    let parsed: KrakenPair = pair.parse().ok()?;
    parsed
        .symbol()
        .split_once('-')
        .map(|(base, _)| base.to_string())
}

/// Assemble le rapport. `coverage_end_ms` est la fin exclusive de la couverture (même règle que
/// `universe build`, §4.5) ; elle fixe le seuil des 90 jours des délistées. `observations` est dans
/// n'importe quel ordre : toutes les listes du rapport sont triées.
pub fn build_report(
    zip: &ZipSummary,
    coverage_end_ms: Ts,
    observations: &[PairObservation],
    unrecognized_entries: &[String],
    config: &UniverseConfig,
) -> Result<SurvivorshipReport, SurvivorshipError> {
    let mut sorted: Vec<&PairObservation> = observations.iter().collect();
    sorted.sort_by(|a, b| a.kraken_pair.as_bytes().cmp(b.kraken_pair.as_bytes()));

    let mut expected = Vec::with_capacity(config.survivorship.expected.len());
    let mut absent = Vec::new();
    for prefix in &config.survivorship.expected {
        let pattern = format!("{prefix}*");
        let mut pairs = Vec::new();
        let mut bases = BTreeSet::new();
        for observation in sorted
            .iter()
            .filter(|o| o.kraken_pair.starts_with(prefix.as_str()))
        {
            if let Some(base) = base_of(&observation.kraken_pair) {
                bases.insert(base);
            }
            pairs.push(ExpectedPair {
                kraken_pair: observation.kraken_pair.clone(),
                resolutions: observation.resolutions.clone(),
                first: format_rfc3339_utc(observation.first_ms)?,
                last: format_rfc3339_utc(observation.last_ms)?,
                source_resolution: observation.source_resolution,
            });
        }
        let present = !pairs.is_empty();
        if !present {
            absent.push(pattern.clone());
        }
        expected.push(ExpectedEntry {
            pattern,
            present,
            review_needed: bases.len() > 1,
            bases: bases.into_iter().collect(),
            pairs,
        });
    }

    let threshold_ms = coverage_end_ms - DELISTED_THRESHOLD_DAYS * MS_PER_DAY;
    let mut delisted_pairs = Vec::new();
    let mut earliest: Option<(Ts, &str)> = None;
    for observation in sorted.iter().filter(|o| o.last_ms < threshold_ms) {
        delisted_pairs.push(DelistedPair {
            kraken_pair: observation.kraken_pair.clone(),
            last: format_rfc3339_utc(observation.last_ms)?,
        });
        let candidate = (observation.last_ms, observation.kraken_pair.as_str());
        if earliest.is_none_or(|current| candidate < current) {
            earliest = Some(candidate);
        }
    }
    let earliest_pair = earliest
        .map(|(last_ms, pair)| {
            format_rfc3339_utc(last_ms).map(|last| DelistedPair {
                kraken_pair: pair.to_string(),
                last,
            })
        })
        .transpose()?;

    let mut unrecognized = unrecognized_entries.to_vec();
    unrecognized.sort();
    unrecognized.dedup();

    let mut report = SurvivorshipReport {
        schema: SCHEMA.to_string(),
        zip: ZipSection {
            sha256: zip.sha256.clone(),
            release: zip.release.clone(),
            coverage_start: zip.coverage_start.clone(),
            coverage_end: format_rfc3339_utc(coverage_end_ms)?,
            generated_at: zip.generated_at.clone(),
            entries: zip.entries,
            pairs: observations.len() as u64,
        },
        expected,
        absent,
        delisted: Delisted {
            threshold_days: DELISTED_THRESHOLD_DAYS,
            count: delisted_pairs.len() as u64,
            earliest: earliest_pair.clone(),
            pairs: delisted_pairs,
        },
        program_start: ProgramStart {
            at_most: earliest_pair.map(|pair| pair.last),
            at_least_hypothesis: PROGRAM_START_HYPOTHESIS.to_string(),
            method: PROGRAM_START_METHOD.to_string(),
        },
        unrecognized_entries: unrecognized,
        report_hash: String::new(),
    };
    report.report_hash = report.compute_report_hash()?;
    Ok(report)
}

/// Rapport d'un zip ouvert et vérifié : classe les entrées, lit les bornes de l'entrée source de
/// chaque paire (flux, sans extraction), assemble [`build_report`].
///
/// Une entrée source illisible ou sans aucune bougie va dans `unrecognized_entries` au lieu de faire
/// échouer le rapport : c'est un diagnostic, pas une source de trading (`check_m1` n'est pas appliqué).
pub fn survey_zip(
    zip: &mut KrakenZip,
    config: &UniverseConfig,
    coverage_end_ms: Ts,
) -> Result<SurvivorshipReport, SurvivorshipError> {
    let manifest = zip.manifest()?;
    let names = zip.entry_names();
    let plan = plan_entries(&names);
    let mut unrecognized = plan.unrecognized_entries.clone();
    let mut observations = Vec::with_capacity(plan.pairs.len());
    for pair in &plan.pairs {
        let rows = zip.rows_entry(&pair.source_entry)?;
        match span_of_rows(rows).ok().flatten() {
            Some((first_ms, last_ms)) => observations.push(PairObservation {
                kraken_pair: pair.kraken_pair.clone(),
                resolutions: pair.resolutions.clone(),
                source_resolution: pair.source_resolution,
                first_ms,
                last_ms,
            }),
            None => unrecognized.push(pair.source_entry.clone()),
        }
    }
    let summary = ZipSummary {
        sha256: zip.sha256().to_string(),
        release: manifest.as_ref().and_then(|m| m.release.clone()),
        coverage_start: manifest.as_ref().and_then(|m| m.coverage_start.clone()),
        generated_at: manifest.as_ref().and_then(|m| m.generated_at.clone()),
        entries: names.len() as u64,
    };
    build_report(
        &summary,
        coverage_end_ms,
        &observations,
        &unrecognized,
        config,
    )
}
