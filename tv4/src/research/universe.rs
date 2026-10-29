//! Calcul de l'univers trimestriel, liste figée des paires OKX tradables en USDC et règles d'instrument
//! dérivées du dump (docs/specs/s11.md §4.1 à §4.3, §4.7 et §7.1 à §7.2).
//!
//! Le module ne lit ni le zip ni le disque, sauf [`write_okx_usdc_subset`] : il travaille sur des
//! séries 1m **déjà contrôlées** par `marketdata::continuity::check_m1` (même fonction, même fin de
//! couverture que l'ingestion, §0). La CLI (lot F) ouvre le zip, boucle sur les paires et appelle, dans
//! l'ordre :
//!
//! 1. [`candidates_from_entries`] : paires candidates du zip, noms hérités écartés, collisions refusées ;
//! 2. **passe 1**, par paire : `rows_1m`, `check_m1`, puis [`scan_checked`] qui rend les
//!    [`PairStats`] (volumes par jour, derniers closes, trous de cotation, qualité) ou le rejet
//!    d'une série bloquante ; la série 1m est libérée avant la paire suivante ;
//! 3. [`select`] : éligibilité à chaque date `D`, choix USD contre USDT, classement, top 12 ;
//! 4. **passe 2**, sur les seules paires de l'union (et les éligibles des dates `--at`) :
//!    [`input_hash`], [`quality_hash`] et [`derive_instrument_rule`] ;
//! 5. [`universe_rules`], [`union_entries`] et [`Selected::to_csv_row`] assemblent
//!    `store::universe::UniverseBuild` ; l'écriture vérifiée est celle de `store::universe`.
//!
//! # Règle à la date `D` (§4.2)
//!
//! `D` est un minuit UTC (premier jour d'un trimestre, ou une date `--at`). Pour chaque paire :
//!
//! 1. **historique** : première minute active (volume > 0) `≤ D − 365 j` ;
//! 2. **continuité** : aucun trou de cotation `≥ T` (`DEFAULT_QUOTE_GAP_MS`, 7 jours) sur
//!    `[D − 365 j, D)`. Avec `a₁ < … < aₙ` les minutes actives de la fenêtre et `g(x) = x + 60 s`,
//!    les trous sont `a₁ − (D − 365 j)`, `aᵢ₊₁ − g(aᵢ)` et `D − g(aₙ)` ; une fenêtre sans minute active
//!    n'est pas continue ;
//! 3. **prix** : close de la dernière minute réelle d'`open_time < D`, strictement supérieur à 0,01
//!    (une paire USDT est lue comme un prix USD) ;
//! 4. **volume** : Σ `close × volume` des 1m des jours `D − 90 … D − 1`, accumulé jour par jour dans
//!    l'ordre chronologique puis sommé de zéro dans l'ordre des jours, comme `RankTable::build`
//!    (`engine/priority.rs`) : le score est identique au bit.
//!
//! Les règles 1 à 4 ne lisent que des minutes d'`open_time < D` : modifier une minute postérieure ne
//! change ni les membres ni leur rang (la colonne `excluded_tfs`, jugée sur les minutes du trimestre
//! lui-même, est la seule à pouvoir changer, §4.3). **Exception déclarée** (`rejection_uses_full_series`
//! dans les règles, écart n° 25 de §17) : le rejet d'une série bloquante est jugé sur la série complète,
//! donc un défaut postérieur à `D` retire la paire de tous les trimestres et la suivante du classement
//! la remplace.
//!
//! Pour une base donnée, la paire USD est retenue si elle est éligible, sinon la paire USDT si elle
//! l'est, jamais les deux et sans cumul de volume (ordre de préférence = `quotes` de
//! `config/universe.toml`). Le classement trie par volume décroissant (`f64::total_cmp`), puis
//! ancienneté (première minute active la plus ancienne d'abord), puis nom de paire Kraken en ordre
//! d'octets. Les 12 premiers sont les membres.
//!
//! # Mémoire
//!
//! [`PairStats`] ne garde ni les bougies ni les minutes actives : un statut par jour UTC, les trous de
//! cotation entre minutes actives (8 octets chacun, en minutes entières) et le résultat de la qualité.
//! Kraken n'écrit que les minutes avec transaction : pour une paire peu liquide, il y a presque un
//! trou par ligne. Toutes les paires candidates restent en mémoire jusqu'à la sélection, donc le pic
//! de la passe 1 est celui de la plus longue série **plus** 8 octets par trou de toutes les candidates
//! (de l'ordre de 0,3 à 1,2 Go sur le dump réel, à mesurer : la CLI imprime le total en fin de
//! passe 1). Les vecteurs sont compactés (`shrink_to_fit`) et [`PairStats::release_selection_data`]
//! libère jours, trous et maxima par bloc avant la passe 2.

use std::collections::BTreeMap;
use std::path::Path;

use thiserror::Error;

use crate::engine::{DEFAULT_QUOTE_GAP_MS, LIQUIDITY_WINDOW_DAYS};
use crate::marketdata::continuity::CheckedSeries;
use crate::marketdata::kraken_zip::{KrakenPair, KrakenZipError};
use crate::marketdata::okx::instruments::{
    EXCHANGE as OKX_EXCHANGE, InstrumentsError, InstrumentsFile,
};
use crate::model::universe::{
    PairSubset, SUBSET_KIND_OKX_USDC, SubsetRule, SubsetSource, UNIVERSE_RULES_VERSION,
    UniverseConfig, quarter_bounds,
};
use crate::model::{Candle, ModelError, Timeframe, Ts};
use crate::store::dsh;
use crate::store::time::{MS_PER_DAY, civil_from_days, format_rfc3339_utc};
use crate::store::universe::{
    CsvRow, InstrumentRule, UnionEntry, UniverseRules, write_pair_subset,
};
use crate::store::{
    Columns, GapsPolicy, QUALITY_MISSING_THRESHOLD, StoreError, Window, quality_report,
};

/// Nombre de membres d'un trimestre (§4.2 règle 7).
pub const TOP_N: usize = 12;

/// Historique exigé avant `D`, en jours (§4.2 règles 1 et 2).
pub const HISTORY_DAYS: u32 = 365;

/// Prix minimal (strict) du dernier close avant `D` (§4.2 règle 3).
pub const MIN_PRICE: f64 = 0.01;

/// Premier trimestre produit par défaut (§4.4).
pub const DEFAULT_FIRST_QUARTER: &str = "2018-Q1";

/// TF dont la qualité est jugée, de 5m à 1d : le 1m, qui sert aux fills, n'exclut jamais rien
/// (docs/04 §2.5 et §10 décision 8). Même liste que `store::build_dataset_research`, qui compare ses
/// exclusions à celles des CSV de l'univers.
pub const QUALITY_TFS: [Timeframe; 7] = [
    Timeframe::M5,
    Timeframe::M15,
    Timeframe::M30,
    Timeframe::H1,
    Timeframe::H4,
    Timeframe::H12,
    Timeframe::D1,
];

/// Méthode de dérivation des règles d'instrument (§4.7), écrite dans `universe.json`.
pub const INSTRUMENTS_METHOD: &str = "decimals_v1";

/// Pas de lot des jambes Kraken de recherche : la précision des volumes vérifiée sur la fixture
/// (`config/instruments.toml`, §4.7).
pub const DERIVED_LOT_SIZE: f64 = 1e-8;

/// Nombre maximal de décimales d'un tick dérivé (§4.7).
pub const MAX_TICK_DECIMALS: u32 = 10;

/// Écart toléré à un multiple du tick pour juger qu'un prix « tombe » sur la décimale `d` (§4.7).
const TICK_TOLERANCE: f64 = 1e-6;

/// Source de la clé de hash `dsh-v1` des segments d'entrée (§4.4) : le nom d'exchange du store.
const HASH_EXCHANGE: &str = "kraken";

/// Durée d'une minute et d'un jour, en millisecondes.
const MINUTE_MS: Ts = 60_000;
const DAY_MS: Ts = MS_PER_DAY;

/// Taille des blocs du maximum de trous (requête de plus long trou en O(étendue / 256)).
const GAP_BLOCK: usize = 256;

/// Erreurs du calcul de l'univers.
#[derive(Debug, Error)]
pub enum UniverseError {
    /// Deux entrées du zip donnent le même symbole canonique (§4.1) : rien n'est écrit.
    #[error("symbole {symbol} produit par plusieurs entrées du zip : {pairs}")]
    SymbolCollision { symbol: String, pairs: String },

    /// La date d'évaluation n'est pas un minuit UTC.
    #[error("date {date_ms} ms : un minuit UTC est exigé (le rang de liquidité se lit au jour)")]
    DateNotMidnight { date_ms: Ts },

    /// Une série vide n'a ni statistiques ni règle d'instrument.
    #[error("série 1m vide : aucune statistique possible")]
    EmptySeries,

    /// Un instant sort de la plage que les trous de cotation savent représenter.
    #[error("instant {ts_ms} ms hors de la plage représentable des trous de cotation")]
    TimeRange { ts_ms: Ts },

    /// Le fichier d'instruments n'est pas un fichier OKX spot.
    #[error(
        "fichier d'instruments : exchange `{exchange}` et type `{inst_type}` lus, `okx` et `SPOT` attendus"
    )]
    NotOkxSpot { exchange: String, inst_type: String },

    /// Le chemin du fichier de sortie n'a pas de nom de fichier lisible.
    #[error("chemin `{path}` sans nom de fichier en UTF-8")]
    FileName { path: String },

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Model(#[from] ModelError),

    #[error(transparent)]
    Zip(#[from] KrakenZipError),

    #[error(transparent)]
    Instruments(#[from] InstrumentsError),
}

// ---------------------------------------------------------------------------------------------
// Trimestres
// ---------------------------------------------------------------------------------------------

/// Libellé `AAAA-Qn` du trimestre civil UTC qui contient `ts_ms`.
pub fn quarter_label_of(ts_ms: Ts) -> String {
    let (year, month, _) = civil_from_days(ts_ms.div_euclid(DAY_MS));
    format!("{year:04}-Q{}", (month - 1) / 3 + 1)
}

/// Trimestres entièrement couverts à partir de `first_label` : un par trimestre dont la fin est
/// `≤ coverage_end_ms` (§4.4, point n° 17 de §16 : la colonne qualité est alors complète). Vide si le
/// premier trimestre n'est pas entièrement couvert.
pub fn covered_quarters(first_label: &str, coverage_end_ms: Ts) -> Result<Vec<String>, ModelError> {
    let (mut start, _) = quarter_bounds(first_label)?;
    let mut labels = Vec::new();
    loop {
        let label = quarter_label_of(start);
        let (_, end) = quarter_bounds(&label)?;
        if end > coverage_end_ms {
            return Ok(labels);
        }
        labels.push(label);
        start = end;
    }
}

// ---------------------------------------------------------------------------------------------
// Paires candidates (§4.1)
// ---------------------------------------------------------------------------------------------

/// Une paire candidate du zip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Nom dans le zip (`XBTUSD`).
    pub kraken_pair: String,
    /// Symbole canonique du store (`BTC-USD`).
    pub symbol: String,
    /// Base canonique (`BTC`).
    pub base: String,
    /// Cotation canonique (`USD` ou `USDT`).
    pub quote: String,
}

/// Résultat de la recherche des candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSet {
    /// Candidates triées par nom de paire Kraken en ordre d'octets.
    pub candidates: Vec<Candidate>,
    /// Noms de forme héritée (`XXBTZUSD`), écartés et jamais devinés, triés en ordre d'octets.
    pub unrecognized_pairs: Vec<String>,
    /// Nombre d'entrées `<PAIRE>_1.csv` du zip (`pairs_in_zip` de `universe.json`).
    pub pairs_in_zip: u64,
}

/// `true` si `pair` a la forme héritée de l'API REST, `^[XZ][A-Z0-9]{3}[XZ](USD|USDT)$` (§4.1) : elle
/// n'est pas normalisée.
pub fn is_legacy_pair_name(pair: &str) -> bool {
    if !pair.is_ascii() {
        return false;
    }
    let bytes = pair.as_bytes();
    let (head, tail) = match bytes.len() {
        8 => (&bytes[..5], &bytes[5..]),
        9 => (&bytes[..5], &bytes[5..]),
        _ => return false,
    };
    let quote_ok = tail == b"USD" || tail == b"USDT";
    let is_xz = |b: u8| b == b'X' || b == b'Z';
    let alnum = |b: &u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    quote_ok && is_xz(head[0]) && head[1..4].iter().all(alnum) && is_xz(head[4])
}

/// Candidates d'après les noms d'entrées du zip (§4.1) : entrées `<PAIRE>_1.csv` dont la paire est
/// valide (`KrakenPair`), de cotation canonique dans `config.quotes`, de base hors de
/// `config.excluded_bases`, de forme non héritée. L'ordre des noms d'entrée est sans effet.
///
/// Une collision de symbole canonique est [`UniverseError::SymbolCollision`].
pub fn candidates_from_entries(
    entry_names: &[String],
    config: &UniverseConfig,
) -> Result<CandidateSet, UniverseError> {
    let mut pairs: Vec<&str> = entry_names
        .iter()
        .filter_map(|name| name.strip_suffix("_1.csv"))
        .filter(|pair| !pair.is_empty())
        .collect();
    pairs.sort_unstable();
    pairs.dedup();
    let pairs_in_zip = pairs.len() as u64;

    let mut candidates = Vec::new();
    let mut unrecognized = Vec::new();
    for pair in pairs {
        if is_legacy_pair_name(pair) {
            unrecognized.push(pair.to_string());
            continue;
        }
        let Ok(parsed) = pair.parse::<KrakenPair>() else {
            continue;
        };
        let Some((base, quote)) = parsed.symbol().split_once('-') else {
            continue;
        };
        if !config.quotes.iter().any(|q| q == quote) || config.is_excluded_base(base) {
            continue;
        }
        candidates.push(Candidate {
            kraken_pair: pair.to_string(),
            symbol: parsed.symbol().to_string(),
            base: base.to_string(),
            quote: quote.to_string(),
        });
    }
    let mut by_symbol: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for candidate in &candidates {
        by_symbol
            .entry(candidate.symbol.as_str())
            .or_default()
            .push(candidate.kraken_pair.as_str());
    }
    if let Some((symbol, names)) = by_symbol.iter().find(|(_, names)| names.len() > 1) {
        return Err(UniverseError::SymbolCollision {
            symbol: (*symbol).to_string(),
            pairs: names.join(", "),
        });
    }
    Ok(CandidateSet {
        candidates,
        unrecognized_pairs: unrecognized,
        pairs_in_zip,
    })
}

// ---------------------------------------------------------------------------------------------
// Statistiques d'une paire (passe 1)
// ---------------------------------------------------------------------------------------------

/// Volume quote et dernier close d'un jour UTC.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DayStat {
    /// Σ `volume × close` des 1m du jour, accumulé dans l'ordre chronologique.
    quote_volume: f64,
    /// Close de la dernière minute réelle du jour ; `None` pour un jour sans minute.
    last_close: Option<f64>,
}

/// Trou de cotation entre deux minutes actives consécutives, en minutes entières : `start = g(aᵢ)`,
/// `len = aᵢ₊₁ − g(aᵢ) > 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gap {
    start_min: u32,
    len_min: u32,
}

impl Gap {
    fn new(start_ms: Ts, end_ms: Ts) -> Result<Gap, UniverseError> {
        let to_min = |ms: Ts| {
            u32::try_from(ms.div_euclid(MINUTE_MS))
                .map_err(|_| UniverseError::TimeRange { ts_ms: ms })
        };
        Ok(Gap {
            start_min: to_min(start_ms)?,
            len_min: to_min(end_ms - start_ms)?,
        })
    }

    fn start_ms(self) -> Ts {
        Ts::from(self.start_min) * MINUTE_MS
    }

    fn end_ms(self) -> Ts {
        (Ts::from(self.start_min) + Ts::from(self.len_min)) * MINUTE_MS
    }
}

/// Statistiques d'une paire, calculées en une passe sur sa série 1m contrôlée ([`PairStats::from_m1`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PairStats {
    first_active_ms: Option<Ts>,
    last_active_ms: Option<Ts>,
    /// Jour UTC (depuis l'époque) de la première minute réelle.
    first_day: i64,
    /// Un statut par jour UTC, de `first_day` au jour de la dernière minute.
    days: Vec<DayStat>,
    /// Trous entre minutes actives, triés, sans recouvrement.
    gaps: Vec<Gap>,
    /// Plus long trou de chaque bloc de [`GAP_BLOCK`] trous.
    block_max: Vec<u32>,
    /// TF de 5m à 1d exclus par la qualité, par trimestre (`2018-Q1`) ; seuls les trimestres avec au
    /// moins un TF exclu y figurent. Durée croissante.
    excluded: BTreeMap<String, Vec<Timeframe>>,
}

/// Raison d'inéligibilité (§4.2), dans l'ordre où les règles sont jugées.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ineligible {
    /// Règle 1 : moins de 365 jours d'historique à `D` (ou aucune minute active).
    History,
    /// Règle 2 : un trou de cotation d'au moins 7 jours (ou aucune minute active) sur la fenêtre.
    QuoteGap,
    /// Règle 3 : dernier close absent ou `≤ 0,01`.
    Price,
}

/// Valeurs de la règle pour une paire éligible à `D`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EligibleMetrics {
    /// `open_time` de la première minute active de toute la série.
    pub first_active_ms: Ts,
    /// Plus long trou de cotation de `[D − 365 j, D)`, en millisecondes (règle 2).
    pub longest_gap_ms: Ts,
    /// Close de la dernière minute réelle d'`open_time < D`.
    pub last_close: f64,
    /// Volume quote des 90 jours précédents.
    pub volume_quote_90d: f64,
}

/// Verdict de l'éligibilité d'une paire à `D`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Eligibility {
    Eligible(EligibleMetrics),
    Ineligible(Ineligible),
}

impl PairStats {
    /// Statistiques d'une série 1m contrôlée (triée, sans doublon, alignée sur la minute ; rien ne
    /// doit être synthétique). La qualité est calculée par `store::quality_report` sur **toutes** les
    /// minutes réelles, y compris celles à volume nul, comme l'ingestion.
    ///
    /// Erreurs : série vide, désordre ou désalignement (rendus par `quality_report`), instant hors de
    /// la plage représentable.
    pub fn from_m1(candles: &[Candle]) -> Result<PairStats, UniverseError> {
        let Some(first) = candles.first() else {
            return Err(UniverseError::EmptySeries);
        };
        let open_times: Vec<Ts> = candles.iter().map(|c| c.open_time_ms).collect();
        let report = quality_report(&open_times, &QUALITY_TFS)?;
        drop(open_times);
        // Lignes triées par (TF croissant, trimestre) : les TF d'un trimestre s'ajoutent par durée
        // croissante.
        let mut excluded: BTreeMap<String, Vec<Timeframe>> = BTreeMap::new();
        for row in report.rows.iter().filter(|row| row.excluded) {
            excluded
                .entry(row.quarter.clone())
                .or_default()
                .push(row.tf);
        }

        let first_day = first.open_time_ms.div_euclid(DAY_MS);
        let mut days: Vec<DayStat> = Vec::new();
        let mut gaps: Vec<Gap> = Vec::new();
        let mut first_active: Option<Ts> = None;
        let mut previous_active: Option<Ts> = None;
        for candle in candles {
            let index = usize::try_from(candle.open_time_ms.div_euclid(DAY_MS) - first_day)
                .map_err(|_| UniverseError::TimeRange {
                    ts_ms: candle.open_time_ms,
                })?;
            if days.len() <= index {
                days.resize(
                    index + 1,
                    DayStat {
                        quote_volume: 0.0,
                        last_close: None,
                    },
                );
            }
            if let Some(day) = days.get_mut(index) {
                day.quote_volume += candle.volume * candle.close;
                day.last_close = Some(candle.close);
            }
            if candle.volume > 0.0 {
                first_active.get_or_insert(candle.open_time_ms);
                if let Some(previous) = previous_active {
                    let start = previous + MINUTE_MS;
                    if candle.open_time_ms > start {
                        gaps.push(Gap::new(start, candle.open_time_ms)?);
                    }
                }
                previous_active = Some(candle.open_time_ms);
            }
        }
        let block_max: Vec<u32> = gaps
            .chunks(GAP_BLOCK)
            .map(|block| block.iter().map(|gap| gap.len_min).max().unwrap_or(0))
            .collect();
        // Les `push` laissent jusqu'à deux fois la longueur en capacité : toutes les paires candidates
        // restent en mémoire jusqu'à la sélection, la capacité non rendue compterait double.
        days.shrink_to_fit();
        gaps.shrink_to_fit();
        Ok(PairStats {
            first_active_ms: first_active,
            last_active_ms: previous_active,
            first_day,
            days,
            gaps,
            block_max,
            excluded,
        })
    }

    /// `open_time` de la première minute active (volume > 0) de toute la série.
    pub fn first_active_ms(&self) -> Option<Ts> {
        self.first_active_ms
    }

    /// `open_time` de la dernière minute active.
    pub fn last_active_ms(&self) -> Option<Ts> {
        self.last_active_ms
    }

    /// Nombre de trous de cotation (d'au moins une minute) entre minutes actives.
    pub fn gap_count(&self) -> usize {
        self.gaps.len()
    }

    /// Octets occupés par les trous de cotation (8 par trou).
    pub fn gap_bytes(&self) -> usize {
        self.gaps.len() * std::mem::size_of::<Gap>()
    }

    /// Libère les tableaux que seule la sélection lit (jours, trous, maxima par bloc). À appeler une
    /// fois [`select`] terminé, avant la passe 2, qui ne relit plus que [`PairStats::excluded_tfs`] :
    /// le pic de la passe 2 redevient celui de la plus longue série. Après cet appel,
    /// [`PairStats::eligibility`] ne rend plus rien de juste.
    pub fn release_selection_data(&mut self) {
        self.days = Vec::new();
        self.gaps = Vec::new();
        self.block_max = Vec::new();
    }

    /// TF de 5m à 1d exclus par la qualité pendant le trimestre `quarter` (`2018-Q1`), par durée
    /// croissante ; vide si aucun. Jamais le 1m.
    pub fn excluded_tfs(&self, quarter: &str) -> &[Timeframe] {
        self.excluded.get(quarter).map_or(&[], Vec::as_slice)
    }

    /// Trimestres avec au moins un TF exclu, par ordre chronologique.
    pub fn excluded_quarters(&self) -> impl Iterator<Item = (&str, &[Timeframe])> {
        self.excluded
            .iter()
            .map(|(quarter, tfs)| (quarter.as_str(), tfs.as_slice()))
    }

    /// Volume quote du jour UTC `day` (jours depuis l'époque) ; 0 hors de la série ou sans minute.
    fn day_volume(&self, day: i64) -> f64 {
        usize::try_from(day - self.first_day)
            .ok()
            .and_then(|index| self.days.get(index))
            .map_or(0.0, |stat| stat.quote_volume)
    }

    /// Volume quote 90 j à `date_ms` : Σ des volumes quote des jours `D − 90 … D − 1`, de zéro, dans
    /// l'ordre des jours. `date_ms` doit être un minuit (voir [`PairStats::eligibility`]).
    pub fn volume_quote_90d(&self, date_ms: Ts) -> f64 {
        let day = date_ms.div_euclid(DAY_MS);
        let mut sum = 0.0_f64;
        for j in (day - i64::from(LIQUIDITY_WINDOW_DAYS))..day {
            sum += self.day_volume(j);
        }
        sum
    }

    /// Close de la dernière minute réelle d'`open_time < date_ms` ; `None` sans minute avant.
    pub fn last_close_before(&self, date_ms: Ts) -> Option<f64> {
        let last_day = usize::try_from(date_ms.div_euclid(DAY_MS) - 1 - self.first_day).ok()?;
        let top = last_day.min(self.days.len().checked_sub(1)?);
        self.days
            .get(..=top)?
            .iter()
            .rev()
            .find_map(|stat| stat.last_close)
    }

    /// Trou de cotation qui contient l'instant `t_ms` : `(début, fin)` avec `début ≤ t < fin`.
    fn gap_containing(&self, t_ms: Ts) -> Option<(Ts, Ts)> {
        let index = self.gaps.partition_point(|gap| gap.start_ms() <= t_ms);
        let gap = self.gaps.get(index.checked_sub(1)?)?;
        (t_ms < gap.end_ms()).then(|| (gap.start_ms(), gap.end_ms()))
    }

    /// Plus long trou (en minutes) parmi `gaps[lo..hi]`.
    fn max_len_min(&self, lo: usize, hi: usize) -> u32 {
        let mut best = 0_u32;
        let mut index = lo;
        while index < hi {
            if index.is_multiple_of(GAP_BLOCK) && index + GAP_BLOCK <= hi {
                best = best.max(self.block_max.get(index / GAP_BLOCK).copied().unwrap_or(0));
                index += GAP_BLOCK;
            } else {
                best = best.max(self.gaps.get(index).map_or(0, |gap| gap.len_min));
                index += 1;
            }
        }
        best
    }

    /// Plus long trou de cotation de `[window_start, window_end)` (règle 2), en millisecondes, ou
    /// `None` si la fenêtre ne contient aucune minute active. Les bornes sont des instants alignés sur
    /// la minute.
    pub fn longest_gap_ms(&self, window_start: Ts, window_end: Ts) -> Option<Ts> {
        let (first, last) = (self.first_active_ms?, self.last_active_ms?);
        if window_start >= window_end || window_end <= first || window_start > last {
            return None;
        }
        // Première minute active ≥ window_start.
        let a1 = if window_start <= first {
            first
        } else {
            self.gap_containing(window_start)
                .map_or(window_start, |(_, end)| end)
        };
        if a1 >= window_end {
            return None;
        }
        // Dernière minute active < window_end.
        let before = window_end - MINUTE_MS;
        let an = if before >= last {
            last
        } else {
            self.gap_containing(before)
                .map_or(before, |(start, _)| start - MINUTE_MS)
        };
        let initial = a1 - window_start;
        let final_gap = window_end - (an + MINUTE_MS);
        let lo = self
            .gaps
            .partition_point(|gap| gap.start_ms() < a1 + MINUTE_MS);
        let hi = self.gaps.partition_point(|gap| gap.end_ms() <= an);
        let inner = Ts::from(self.max_len_min(lo, hi)) * MINUTE_MS;
        Some(initial.max(final_gap).max(inner))
    }

    /// Éligibilité de la paire à `date_ms` (§4.2 règles 1 à 3) et valeurs de la règle. `date_ms` doit
    /// être un minuit UTC.
    pub fn eligibility(&self, date_ms: Ts) -> Result<Eligibility, UniverseError> {
        if date_ms.rem_euclid(DAY_MS) != 0 {
            return Err(UniverseError::DateNotMidnight { date_ms });
        }
        let window_start = date_ms - Ts::from(HISTORY_DAYS) * DAY_MS;
        let Some(first_active_ms) = self.first_active_ms.filter(|&f| f <= window_start) else {
            return Ok(Eligibility::Ineligible(Ineligible::History));
        };
        let longest_gap_ms = match self.longest_gap_ms(window_start, date_ms) {
            Some(gap) if gap < DEFAULT_QUOTE_GAP_MS => gap,
            _ => return Ok(Eligibility::Ineligible(Ineligible::QuoteGap)),
        };
        let Some(last_close) = self.last_close_before(date_ms).filter(|&c| c > MIN_PRICE) else {
            return Ok(Eligibility::Ineligible(Ineligible::Price));
        };
        Ok(Eligibility::Eligible(EligibleMetrics {
            first_active_ms,
            longest_gap_ms,
            last_close,
            volume_quote_90d: self.volume_quote_90d(date_ms),
        }))
    }
}

/// Résultat de la passe 1 pour une paire.
#[derive(Clone, Debug, PartialEq)]
pub enum PairScan {
    /// Série utilisable.
    Accepted(Box<PairStats>),
    /// Série bloquante (doublon divergent, OHLC incohérent, horodatage non aligné, valeur non
    /// finie, série vide) : la paire est écartée et listée dans `rejected_pairs` (§4.1, point n° 18
    /// de §16). Les motifs sont les noms stables de `BlockingKind`, triés.
    Rejected { reasons: Vec<String> },
}

/// Passe 1 d'une paire sur sa série contrôlée par `check_m1`.
pub fn scan_checked(checked: &CheckedSeries) -> Result<PairScan, UniverseError> {
    if checked.report.is_blocking() {
        return Ok(PairScan::Rejected {
            reasons: checked
                .report
                .blocking
                .iter()
                .map(|kind| kind.as_str().to_string())
                .collect(),
        });
    }
    Ok(PairScan::Accepted(Box::new(PairStats::from_m1(
        &checked.candles,
    )?)))
}

// ---------------------------------------------------------------------------------------------
// Sélection (§4.2 règles 4 à 7)
// ---------------------------------------------------------------------------------------------

/// Une paire candidate et ses statistiques.
#[derive(Clone, Debug, PartialEq)]
pub struct PairEntry {
    pub candidate: Candidate,
    pub stats: PairStats,
}

/// Une paire éligible à une date, avec son rang.
#[derive(Clone, Debug, PartialEq)]
pub struct Selected {
    /// 1 pour le plus liquide, consécutif.
    pub rank: u32,
    pub candidate: Candidate,
    pub metrics: EligibleMetrics,
}

impl Selected {
    /// Ligne de CSV d'univers (§4.4). `excluded_tfs` (qualité du trimestre) et `input_hash` (passe 2)
    /// viennent de l'appelant.
    pub fn to_csv_row(
        &self,
        excluded_tfs: Vec<Timeframe>,
        input_hash: String,
    ) -> Result<CsvRow, UniverseError> {
        Ok(CsvRow {
            rank: self.rank,
            base: self.candidate.base.clone(),
            symbol: self.candidate.symbol.clone(),
            kraken_pair: self.candidate.kraken_pair.clone(),
            quote: self.candidate.quote.clone(),
            volume_quote_90d: self.metrics.volume_quote_90d,
            first_active: format_rfc3339_utc(self.metrics.first_active_ms)?,
            longest_gap_365d_ms: self.metrics.longest_gap_ms,
            last_close: self.metrics.last_close,
            excluded_tfs,
            input_hash,
        })
    }
}

/// Les éligibles d'une date `D`, classés (une paire par base).
#[derive(Clone, Debug, PartialEq)]
pub struct QuarterSelection {
    pub date_ms: Ts,
    /// Tous les éligibles, rang 1 à n.
    pub eligible: Vec<Selected>,
}

impl QuarterSelection {
    /// Les membres : les [`TOP_N`] premiers (tous, avec moins de 12 éligibles).
    pub fn members(&self) -> &[Selected] {
        &self.eligible[..self.eligible.len().min(TOP_N)]
    }

    /// Nombre d'éligibles avant la troncature (`eligible` de `universe.json`).
    pub fn eligible_count(&self) -> u32 {
        u32::try_from(self.eligible.len()).unwrap_or(u32::MAX)
    }
}

/// Éligibles de la date `date_ms` : règles 1 à 4, choix USD contre USDT par base (ordre de
/// `config.quotes`), classement (volume décroissant, ancienneté, nom d'octets). Les bases de
/// `config.excluded_bases` et les cotations hors de `config.quotes` sont ignorées.
pub fn select_at(
    pairs: &[PairEntry],
    date_ms: Ts,
    config: &UniverseConfig,
) -> Result<QuarterSelection, UniverseError> {
    // Meilleure paire éligible par base : la cotation la plus préférée.
    let mut best: BTreeMap<&str, (usize, &PairEntry, EligibleMetrics)> = BTreeMap::new();
    for entry in pairs {
        let candidate = &entry.candidate;
        if config.is_excluded_base(&candidate.base) {
            continue;
        }
        let Some(preference) = config.quotes.iter().position(|q| *q == candidate.quote) else {
            continue;
        };
        let Eligibility::Eligible(metrics) = entry.stats.eligibility(date_ms)? else {
            continue;
        };
        let already_better = best
            .get(candidate.base.as_str())
            .is_some_and(|(current, ..)| *current <= preference);
        if !already_better {
            best.insert(candidate.base.as_str(), (preference, entry, metrics));
        }
    }
    let mut chosen: Vec<(&PairEntry, EligibleMetrics)> = best
        .into_values()
        .map(|(_, entry, metrics)| (entry, metrics))
        .collect();
    chosen.sort_by(|(a, ma), (b, mb)| {
        mb.volume_quote_90d
            .total_cmp(&ma.volume_quote_90d)
            .then(ma.first_active_ms.cmp(&mb.first_active_ms))
            .then_with(|| {
                a.candidate
                    .kraken_pair
                    .as_bytes()
                    .cmp(b.candidate.kraken_pair.as_bytes())
            })
    });
    let eligible = chosen
        .into_iter()
        .enumerate()
        .map(|(index, (entry, metrics))| Selected {
            rank: u32::try_from(index + 1).unwrap_or(u32::MAX),
            candidate: entry.candidate.clone(),
            metrics,
        })
        .collect();
    Ok(QuarterSelection { date_ms, eligible })
}

/// [`select_at`] pour chaque date de `dates_ms`, dans l'ordre donné.
pub fn select(
    pairs: &[PairEntry],
    dates_ms: &[Ts],
    config: &UniverseConfig,
) -> Result<Vec<QuarterSelection>, UniverseError> {
    dates_ms
        .iter()
        .map(|&date_ms| select_at(pairs, date_ms, config))
        .collect()
}

/// Paires de l'union, ordonnées par (premier trimestre de présence, rang dans ce trimestre) : l'ordre
/// des jambes d'un run de recherche (§4.4). `quarters` est chronologique, chaque trimestre avec son
/// libellé `AAAA-Qn`.
pub fn union_entries(quarters: &[(String, &QuarterSelection)]) -> Vec<UnionEntry> {
    let mut union: Vec<UnionEntry> = Vec::new();
    for (label, selection) in quarters {
        for member in selection.members() {
            if !union
                .iter()
                .any(|entry| entry.symbol == member.candidate.symbol)
            {
                union.push(UnionEntry {
                    symbol: member.candidate.symbol.clone(),
                    kraken_pair: member.candidate.kraken_pair.clone(),
                    first_quarter: label.clone(),
                });
            }
        }
    }
    union
}

/// Bloc `rules` de `universe.json` : les constantes de ce module, la fenêtre de rang et le trou de
/// S9, et les listes de `config/universe.toml` (§4.2). `quote_preference` est l'ordre de `quotes`.
pub fn universe_rules(config: &UniverseConfig) -> UniverseRules {
    UniverseRules {
        top_n: TOP_N as u32,
        history_days: HISTORY_DAYS,
        quote_gap_ms: DEFAULT_QUOTE_GAP_MS,
        liquidity_window_days: LIQUIDITY_WINDOW_DAYS,
        min_price: MIN_PRICE,
        quotes: config.quotes.clone(),
        quote_preference: config.quotes.clone(),
        excluded_bases: config
            .sorted_excluded_bases()
            .into_iter()
            .map(str::to_string)
            .collect(),
        quality_threshold: QUALITY_MISSING_THRESHOLD,
        quality_tfs: QUALITY_TFS.to_vec(),
        quality_uses_quarter_data: true,
        rejection_uses_full_series: true,
        config_file_hash: format!("blake3:{}", config.file_hash()),
        rules_version: UNIVERSE_RULES_VERSION.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Hashes d'entrée (passe 2, §4.4)
// ---------------------------------------------------------------------------------------------

/// Hash `dsh-v1` du segment `[start, end)` de `candles` : celui d'un dataset mono-série `skip`
/// (`docs/dataset-hash.md` §5.2), exchange `kraken`, 1m. `candles` est trié.
fn segment_hash(
    symbol: &str,
    candles: &[Candle],
    start_ms: Ts,
    end_ms: Ts,
) -> Result<String, UniverseError> {
    let lo = candles.partition_point(|c| c.open_time_ms < start_ms);
    let hi = candles.partition_point(|c| c.open_time_ms < end_ms);
    let columns = Columns::from_candles(&candles[lo..hi]);
    let window = Window::new(start_ms, end_ms)?;
    let (hex, _rows) = dsh::dataset_hash(
        HASH_EXCHANGE,
        window,
        GapsPolicy::Skip,
        &[(symbol, Timeframe::M1, &columns)],
    )?;
    Ok(dsh::prefixed(&hex))
}

/// `input_hash` d'un membre à `date_ms` : hash `dsh-v1` du segment `[D − 365 j, D)`, c'est-à-dire des
/// données lues par les règles 2 à 4. Il ne couvre pas `first_active`, qui vient d'avant la fenêtre
/// (§4.4). `candles` est la série 1m contrôlée de `symbol`.
pub fn input_hash(symbol: &str, candles: &[Candle], date_ms: Ts) -> Result<String, UniverseError> {
    segment_hash(
        symbol,
        candles,
        date_ms - Ts::from(HISTORY_DAYS) * DAY_MS,
        date_ms,
    )
}

/// `quality_hash` d'un membre pour le trimestre `quarter` (`AAAA-Qn`) : hash `dsh-v1` du segment
/// `[D, D_suivant)` lu par la qualité (§4.4).
pub fn quality_hash(
    symbol: &str,
    candles: &[Candle],
    quarter: &str,
) -> Result<String, UniverseError> {
    let (start_ms, end_ms) = quarter_bounds(quarter)?;
    segment_hash(symbol, candles, start_ms, end_ms)
}

// ---------------------------------------------------------------------------------------------
// Règles d'instrument dérivées (§4.7)
// ---------------------------------------------------------------------------------------------

/// `10^d` par multiplications successives, exactes pour `d ≤ 22` : pas d'appel à la libm
/// (`tests/architecture.rs`, docs/03 §7.4).
fn pow10(decimals: u32) -> f64 {
    (0..decimals).fold(1.0_f64, |acc, _| acc * 10.0)
}

/// Vrai si `price × 10^d` est à moins de 1e-6 d'un entier.
fn fits_decimals(price: f64, decimals: u32) -> bool {
    let scaled = price * pow10(decimals);
    (scaled - scaled.round()).abs() <= TICK_TOLERANCE
}

/// Nombre de décimales de la méthode `decimals_v1` : le plus petit `d` de 0 à 10 tel que chaque prix
/// (open, high, low, close) de `candles` vérifie `|p·10^d − round(p·10^d)| ≤ 1e-6`, ou 10 si aucun ne
/// convient.
pub fn tick_decimals(candles: &[Candle]) -> u32 {
    (0..=MAX_TICK_DECIMALS)
        .find(|&decimals| {
            candles.iter().all(|c| {
                [c.open, c.high, c.low, c.close]
                    .iter()
                    .all(|&price| fits_decimals(price, decimals))
            })
        })
        .unwrap_or(MAX_TICK_DECIMALS)
}

/// Règle d'instrument d'une jambe de recherche (`decimals_v1`, §4.7) : `tick_size = 10^−d`,
/// `lot_size = 1e-8`, minimums à 0. Un tick « le plus fin observé » est sans danger pour la
/// simulation (les prix ne sont qu'arrondis plus finement), un tick trop grossier ne le serait pas.
pub fn derive_instrument_rule(
    symbol: &str,
    candles: &[Candle],
) -> Result<InstrumentRule, UniverseError> {
    if candles.is_empty() {
        return Err(UniverseError::EmptySeries);
    }
    let decimals = tick_decimals(candles);
    // 1 / 10^d est la division correctement arrondie : 0,1 pour d = 1, 1e-8 pour d = 8.
    let tick_size = 1.0 / pow10(decimals);
    Ok(InstrumentRule {
        symbol: symbol.to_string(),
        tick_size,
        lot_size: DERIVED_LOT_SIZE,
        min_qty: 0.0,
        min_notional: 0.0,
    })
}

// ---------------------------------------------------------------------------------------------
// Sous-ensemble OKX en USDC (§7)
// ---------------------------------------------------------------------------------------------

/// Quote OKX de la règle du sous-ensemble.
const OKX_QUOTE: &str = "USDC";

/// État OKX d'un instrument tradable.
const OKX_STATE_LIVE: &str = "live";

/// Liste figée des bases tradables sur OKX en USDC (§7.1), tirée d'un fichier `tv4-instruments-v1`.
///
/// Un instrument est tradable si son état est `live` et si sa cotation est l'USDC ou si
/// `trade_quote_ccy_list` contient l'USDC (forme du carnet USD unifié). Une base Kraken `k` est dans
/// la liste si la base OKX `config.okx_base(k)` a un instrument tradable : l'égalité est stricte et la
/// table `okx_base_aliases` (vide par défaut) est la seule correspondance admise, aucun alias n'est
/// deviné. Une base Kraken déclarée comme source d'un alias n'est donc tradable que par son alias.
///
/// `instruments_file_name` est le nom du fichier (sans dossier), écrit dans `source`. La liste peut
/// être vide ; refuser une liste vide relève de l'appelant (code 1 de la CLI, §13).
pub fn okx_usdc_subset(
    instruments: &InstrumentsFile,
    instruments_file_name: &str,
    config: &UniverseConfig,
) -> Result<PairSubset, UniverseError> {
    if instruments.exchange != OKX_EXCHANGE || instruments.inst_type != "SPOT" {
        return Err(UniverseError::NotOkxSpot {
            exchange: instruments.exchange.clone(),
            inst_type: instruments.inst_type.clone(),
        });
    }
    let tradable: std::collections::BTreeSet<&str> = instruments
        .instruments
        .iter()
        .filter(|instrument| {
            instrument.state == OKX_STATE_LIVE
                && !instrument.base_ccy.is_empty()
                && (instrument.quote_ccy == OKX_QUOTE
                    || instrument
                        .trade_quote_ccy_list
                        .iter()
                        .any(|quote| quote == OKX_QUOTE))
        })
        .map(|instrument| instrument.base_ccy.as_str())
        .collect();
    // Bases Kraken candidates : les bases OKX tradables elles-mêmes et les sources d'alias.
    let mut candidates: std::collections::BTreeSet<&str> = tradable.clone();
    candidates.extend(config.okx_base_aliases.keys().map(String::as_str));
    let bases: Vec<String> = candidates
        .into_iter()
        .filter(|kraken_base| tradable.contains(config.okx_base(kraken_base)))
        .map(str::to_string)
        .collect();
    let subset = PairSubset::new(
        SUBSET_KIND_OKX_USDC,
        SubsetSource {
            instruments_file: instruments_file_name.to_string(),
            instruments_hash: instruments.instruments_hash.clone(),
            site: instruments.site.clone(),
            fetched_at: instruments.fetched_at.clone(),
        },
        SubsetRule {
            quote: OKX_QUOTE.to_string(),
            state: OKX_STATE_LIVE.to_string(),
            trade_quote_ccy: true,
            aliases: config.okx_base_aliases.clone(),
        },
        bases,
    )?;
    Ok(subset)
}

/// Lit le fichier d'instruments `instruments_path` (schéma, tri et hash vérifiés par
/// `InstrumentsFile::load`), calcule la liste et l'écrit dans `out` par l'écriture vérifiée de
/// `store::universe` : un fichier existant n'est jamais écrasé (`StoreError::SubsetExists`).
pub fn write_okx_usdc_subset(
    instruments_path: &Path,
    config: &UniverseConfig,
    out: &Path,
) -> Result<PairSubset, UniverseError> {
    let instruments = InstrumentsFile::load(instruments_path)?;
    let name = instruments_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| UniverseError::FileName {
            path: instruments_path.display().to_string(),
        })?;
    let subset = okx_usdc_subset(&instruments, name, config)?;
    write_pair_subset(out, &subset)?;
    Ok(subset)
}

// Séries synthétiques (instants, prix et volumes inventés) : aucune valeur de marché.
#[cfg(test)]
mod tests {
    use super::*;

    /// 2020-01-01T00:00:00Z.
    const DEBUT: Ts = 1_577_836_800_000;

    /// 1 000 minutes actives, chacune suivie d'une minute vide : un trou par intervalle.
    fn serie_a_trous() -> Vec<Candle> {
        (0..1000)
            .map(|i| Candle {
                open_time_ms: DEBUT + 2 * i * MINUTE_MS,
                open: 10.0,
                high: 10.0,
                low: 10.0,
                close: 10.0,
                volume: 1.0,
                trades: Some(1),
            })
            .collect()
    }

    #[test]
    fn les_tableaux_de_la_passe_1_sont_compactes() {
        let stats = PairStats::from_m1(&serie_a_trous()).unwrap();
        assert_eq!(stats.gap_count(), 999);
        assert_eq!(stats.gaps.capacity(), stats.gaps.len());
        assert_eq!(stats.days.capacity(), stats.days.len());
        assert_eq!(stats.gap_bytes(), 999 * 8);
    }

    #[test]
    fn le_compactage_ne_change_pas_la_selection() {
        let compacte = PairStats::from_m1(&serie_a_trous()).unwrap();
        let mut large = compacte.clone();
        large.gaps.reserve(10_000);
        large.days.reserve(10_000);
        assert!(large.gaps.capacity() > large.gaps.len());
        assert_eq!(compacte, large);

        let config = UniverseConfig::load(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../config/universe.toml"
        )))
        .unwrap();
        let candidate = candidates_from_entries(&["AAAUSD_1.csv".to_string()], &config)
            .unwrap()
            .candidates
            .remove(0);
        let entrees = |stats: PairStats| {
            vec![PairEntry {
                candidate: candidate.clone(),
                stats,
            }]
        };
        // La série tient en 2 jours : l'historique de 365 jours manque, donc aucune paire éligible, mais
        // les deux sélections doivent être identiques à toute date.
        let dates = [DEBUT + 400 * DAY_MS, DEBUT + 800 * DAY_MS];
        assert_eq!(
            select(&entrees(compacte), &dates, &config).unwrap(),
            select(&entrees(large), &dates, &config).unwrap()
        );
    }

    #[test]
    fn la_liberation_apres_selection_garde_la_qualite() {
        // Même série avec un grand trou (400 minutes absentes de plus) : la qualité exclut des TF courts.
        let troue: Vec<Candle> = serie_a_trous()
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !(300..500).contains(i))
            .map(|(_, candle)| candle)
            .collect();
        let mut stats = PairStats::from_m1(&troue).unwrap();
        let qualite: Vec<(String, Vec<Timeframe>)> = stats
            .excluded_quarters()
            .map(|(quarter, tfs)| (quarter.to_string(), tfs.to_vec()))
            .collect();
        assert!(!qualite.is_empty());
        stats.release_selection_data();
        assert_eq!(stats.gap_count(), 0);
        assert!(stats.days.is_empty() && stats.block_max.is_empty());
        let apres: Vec<(String, Vec<Timeframe>)> = stats
            .excluded_quarters()
            .map(|(quarter, tfs)| (quarter.to_string(), tfs.to_vec()))
            .collect();
        assert_eq!(apres, qualite);
    }
}
