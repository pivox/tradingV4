//! Types partagés du moteur : `Candle`, `Timeframe`, `FeeSchedule`, `Slippage`, `Side`, `MarketEvent`
//! et erreurs (docs/03-architecture.md §2 et §6). Le JSON trié et le hash blake3 sont dans [`hash`].
//!
//! Règles de ce module : il ne dépend d'aucun autre module du moteur, n'utilise pas `chrono` (le
//! temps est un `i64` en millisecondes UTC, docs/03 §5) et les prix, quantités et volumes sont des
//! `f64` (docs/03 §5.1).

pub mod blocks;
pub mod costs;
pub mod hash;
pub mod instrument;
pub mod rng;
pub mod universe;

pub use blocks::Block;
pub use instrument::InstrumentRules;
pub use universe::{
    Member, PairSubset, QualitySchedule, QuarterExclusion, UniverseConfig, UniverseRef,
    UniverseSchedule,
};

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Horodatage : millisecondes depuis l'epoch Unix, en UTC.
pub type Ts = i64;

/// Un point de base (bp) vaut 0,01 % : 10 000 bps valent 100 %.
pub const BPS_PER_UNIT: f64 = 10_000.0;

/// Période maximale d'un indicateur (≈ 70 jours de 1m) : garde-fou contre une faute de frappe dans un
/// TOML. Partagée par `indicators` et `strategy`, qui ne se voient pas l'un l'autre.
pub const MAX_INDICATOR_PERIOD: usize = 100_000;

/// Valeur maximale acceptée pour des frais, en points de base (100 %). Le slippage a sa propre borne,
/// en `f64` (voir [`Slippage::from_bps`]).
pub const MAX_BPS: u32 = 10_000;

// ---------------------------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------------------------

/// Toutes les erreurs du module `model`.
#[derive(Debug, Error)]
pub enum ModelError {
    /// Un champ de bougie est NaN ou infini.
    #[error("bougie {open_time_ms} : le champ `{field}` n'est pas un nombre fini")]
    CandleNotFinite {
        open_time_ms: Ts,
        field: &'static str,
    },

    /// Un prix de bougie est nul ou négatif.
    #[error("bougie {open_time_ms} : le prix `{field}` doit être strictement positif")]
    NonPositivePrice {
        open_time_ms: Ts,
        field: &'static str,
    },

    /// Les quatre prix ne respectent pas `low <= open, close <= high`.
    #[error("bougie {open_time_ms} : prix OHLC incohérents (low <= open, close <= high exigé)")]
    InconsistentOhlc { open_time_ms: Ts },

    /// Volume négatif.
    #[error("bougie {open_time_ms} : volume négatif")]
    NegativeVolume { open_time_ms: Ts },

    /// Chaîne qui ne désigne aucun timeframe connu.
    #[error("timeframe inconnu : `{0}` (attendus : 1m, 5m, 15m, 30m, 1h, 4h, 12h, 1d)")]
    UnknownTimeframe(String),

    /// Un `open_time` ne tombe pas sur une frontière du timeframe (UTC).
    #[error("open_time {open_time_ms} non aligné sur une bougie {timeframe} (UTC)")]
    MisalignedOpenTime {
        timeframe: Timeframe,
        open_time_ms: Ts,
    },

    /// Chaîne qui ne désigne aucun exchange connu.
    #[error("exchange inconnu : `{0}` (attendus : kraken, okx, bitvavo_usdc)")]
    UnknownExchange(String),

    /// La section de l'exchange manque dans le fichier de frais.
    #[error("aucune grille de frais pour `{exchange}` dans la configuration")]
    FeesNotFound { exchange: ExchangeId },

    /// Chaîne qui ne désigne aucune grille de frais nommée (`okx_xperps` n'existe pas : décision du
    /// 13/10, 18:35).
    #[error(
        "grille de frais inconnue : `{0}` (attendues : okx_regular, bitvavo_usdc ; alias : okx)"
    )]
    UnknownFeeGrid(String),

    /// La section de la grille manque dans le fichier de frais (une ancienne copie avec `[okx]` est
    /// refusée ici, sans repli silencieux).
    #[error("section `[{grid}]` absente de la configuration des frais")]
    FeeGridNotFound { grid: FeeGridId },

    /// `config/costs.toml` (ou une valeur résolue d'un profil de coûts) est invalide. `field` nomme
    /// le champ, et la jambe et le profil pour une valeur résolue.
    #[error("coûts : `{field}` invalide, {reason}")]
    CostsInvalid { field: String, reason: String },

    /// Le fichier de mesures `tv4-cost-measures-v1` est invalide.
    #[error("mesures de coûts : `{field}` invalide, {reason}")]
    MeasuresInvalid { field: String, reason: String },

    /// Chaîne qui ne désigne aucun bloc de recherche.
    #[error("bloc inconnu : `{0}` (attendus : DEV, VALIDATION, HOLDOUT, FORWARD)")]
    UnknownBlock(String),

    /// Fin de fenêtre incompatible avec le bloc (s11 §3.1, `decision_window`).
    #[error("bloc {block} : fin de fenêtre {window_end_ms} refusée, {reason}")]
    DecisionWindow {
        block: Block,
        window_end_ms: Ts,
        reason: &'static str,
    },

    /// Chauffe hors de 0 à 400 jours.
    #[error("chauffe de {days} jours hors des bornes autorisées (0 à 400)")]
    WarmupOutOfRange { days: i64 },

    /// `config/universe.toml`, un calendrier d'univers, des exclusions qualité ou une liste figée de
    /// paires est invalide. `field` nomme le champ.
    #[error("univers : `{field}` invalide, {reason}")]
    UniverseInvalid { field: String, reason: String },

    /// Symbole dont on ne peut pas tirer l'actif de base (pas de `-`, ou base vide).
    #[error("symbole `{symbol}` : l'actif de base est introuvable (forme attendue : BASE-QUOTE)")]
    CostsSymbol { symbol: String },

    /// Frais hors bornes.
    #[error("`{name}` vaut {value} bps, hors des bornes autorisées (0 à {max})")]
    BpsOutOfRange {
        name: &'static str,
        value: u32,
        max: u32,
    },

    /// Slippage non fini, négatif ou supérieur ou égal à 10 000 bps.
    #[error("`slippage_bps` vaut {value} : un nombre fini dans [0, 10 000[ est attendu")]
    SlippageOutOfRange { value: f64 },

    /// Texte de slippage qui n'est pas un nombre.
    #[error("`slippage_bps` : « {text} » n'est pas un nombre")]
    SlippageNotANumber { text: String },

    /// Aucune contrainte d'instrument pour ce couple exchange et symbole.
    #[error("aucun instrument `{symbol}` pour `{exchange}` dans `{path}`")]
    InstrumentNotFound {
        exchange: ExchangeId,
        symbol: String,
        path: String,
    },

    /// Contrainte d'instrument invalide (tick, lot ou minimum).
    #[error("instrument : `{field}` vaut {value}, {reason}")]
    InvalidInstrument {
        field: &'static str,
        value: f64,
        reason: &'static str,
    },

    /// Le fichier de configuration n'a pas pu être lu.
    #[error("lecture de `{path}` impossible : {source}")]
    ConfigRead {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Le contenu TOML est invalide.
    #[error("configuration TOML invalide : {0}")]
    ConfigToml(#[from] toml::de::Error),

    /// Un flottant NaN ou infini a été trouvé dans une valeur à hacher.
    #[error("flottant non fini (NaN ou infini) dans la valeur à hacher")]
    NonFiniteFloat,

    /// Sérialisation JSON impossible (par exemple une clé de table qui n'est pas une chaîne).
    #[error("sérialisation JSON impossible : {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------------------------
// Candle
// ---------------------------------------------------------------------------------------------

/// Bougie clôturée (docs/03 §5.1 et §6). `open_time_ms` est le début de la bougie, en UTC.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candle {
    pub open_time_ms: Ts,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Volume en monnaie de base.
    pub volume: f64,
    /// Nombre de trades : fourni par Kraken, pas par OKX.
    pub trades: Option<u32>,
}

impl Candle {
    /// Construit une bougie et la valide (voir [`Candle::validate`]).
    pub fn new(
        open_time_ms: Ts,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
        trades: Option<u32>,
    ) -> Result<Candle, ModelError> {
        let candle = Candle {
            open_time_ms,
            open,
            high,
            low,
            close,
            volume,
            trades,
        };
        candle.validate()?;
        Ok(candle)
    }

    /// Vérifie la bougie : tous les nombres sont finis, les prix sont strictement positifs,
    /// `low <= open, close <= high` et le volume n'est pas négatif.
    ///
    /// Un NaN est rejeté ici, à l'ingestion, pour ne jamais entrer dans un hash (docs/03 §7.4).
    pub fn validate(&self) -> Result<(), ModelError> {
        let t = self.open_time_ms;
        let fields = [
            ("open", self.open),
            ("high", self.high),
            ("low", self.low),
            ("close", self.close),
            ("volume", self.volume),
        ];
        for (field, value) in fields {
            if !value.is_finite() {
                return Err(ModelError::CandleNotFinite {
                    open_time_ms: t,
                    field,
                });
            }
        }
        for (field, value) in &fields[..4] {
            if *value <= 0.0 {
                return Err(ModelError::NonPositivePrice {
                    open_time_ms: t,
                    field,
                });
            }
        }
        let bas_ok = self.low <= self.open && self.low <= self.close && self.low <= self.high;
        let haut_ok = self.high >= self.open && self.high >= self.close;
        if !(bas_ok && haut_ok) {
            return Err(ModelError::InconsistentOhlc { open_time_ms: t });
        }
        if self.volume < 0.0 {
            return Err(ModelError::NegativeVolume { open_time_ms: t });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Timeframe
// ---------------------------------------------------------------------------------------------

/// Période d'une bougie. Les frontières sont alignées sur l'epoch Unix, donc sur minuit UTC.
///
/// L'ordre de déclaration est l'ordre des durées, ce qui donne `Ord` gratuitement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Timeframe {
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "30m")]
    M30,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "4h")]
    H4,
    #[serde(rename = "12h")]
    H12,
    #[serde(rename = "1d")]
    D1,
}

impl Timeframe {
    /// Tous les timeframes, du plus court au plus long.
    pub const ALL: [Timeframe; 8] = [
        Timeframe::M1,
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::H12,
        Timeframe::D1,
    ];

    /// Durée d'une bougie en millisecondes.
    pub const fn ms(self) -> i64 {
        match self {
            Timeframe::M1 => 60_000,
            Timeframe::M5 => 300_000,
            Timeframe::M15 => 900_000,
            Timeframe::M30 => 1_800_000,
            Timeframe::H1 => 3_600_000,
            Timeframe::H4 => 14_400_000,
            Timeframe::H12 => 43_200_000,
            Timeframe::D1 => 86_400_000,
        }
    }

    /// Bit du timeframe dans un masque `u8` (huit timeframes remplissent exactement un octet).
    ///
    /// Le `match` est explicite : le bit ne dépend pas de l'ordre de déclaration de l'énumération.
    /// Sert au masque `just_closed` de `BarContext` (docs/03 §6).
    pub const fn bit(self) -> u8 {
        match self {
            Timeframe::M1 => 1 << 0,
            Timeframe::M5 => 1 << 1,
            Timeframe::M15 => 1 << 2,
            Timeframe::M30 => 1 << 3,
            Timeframe::H1 => 1 << 4,
            Timeframe::H4 => 1 << 5,
            Timeframe::H12 => 1 << 6,
            Timeframe::D1 => 1 << 7,
        }
    }

    /// Rendu canonique (`1m`, `5m`, `15m`, `30m`, `1h`, `4h`, `12h`, `1d`), celui de
    /// `docs/dataset-hash.md` et des noms de dossiers Parquet.
    pub const fn as_str(self) -> &'static str {
        match self {
            Timeframe::M1 => "1m",
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::M30 => "30m",
            Timeframe::H1 => "1h",
            Timeframe::H4 => "4h",
            Timeframe::H12 => "12h",
            Timeframe::D1 => "1d",
        }
    }

    /// Vrai si `open_time_ms` est un début de bougie de ce timeframe (multiple de la durée).
    pub const fn is_aligned(self, open_time_ms: Ts) -> bool {
        open_time_ms.rem_euclid(self.ms()) == 0
    }

    /// Comme [`Timeframe::is_aligned`], mais renvoie une erreur qui nomme l'horodatage fautif.
    pub fn check_alignment(self, open_time_ms: Ts) -> Result<(), ModelError> {
        if self.is_aligned(open_time_ms) {
            Ok(())
        } else {
            Err(ModelError::MisalignedOpenTime {
                timeframe: self,
                open_time_ms,
            })
        }
    }

    /// Début de la bougie de ce timeframe qui contient l'instant `ts_ms`.
    pub const fn open_time_of(self, ts_ms: Ts) -> Ts {
        ts_ms - ts_ms.rem_euclid(self.ms())
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Timeframe {
    type Err = ModelError;

    fn from_str(text: &str) -> Result<Timeframe, ModelError> {
        Timeframe::ALL
            .into_iter()
            .find(|tf| tf.as_str() == text)
            .ok_or_else(|| ModelError::UnknownTimeframe(text.to_string()))
    }
}

// ---------------------------------------------------------------------------------------------
// Exchange, frais, slippage
// ---------------------------------------------------------------------------------------------

/// Identifiant canonique d'un lieu : celui des dossiers de données, des manifestes et des
/// sections de `config/fees.toml`. Minuscules, `[a-z0-9_]+` (docs/dataset-hash.md §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeId {
    /// Source des dumps OHLCVT historiques (aucune grille de frais : on n'y trade pas).
    Kraken,
    /// Lieu d'exécution (OKX Europe, paires USDC).
    Okx,
    /// Hypothèse de recherche pour la stratégie grid (docs/03 §15.3).
    BitvavoUsdc,
}

impl ExchangeId {
    /// Rendu canonique, identique à la forme sérialisée.
    pub const fn as_str(self) -> &'static str {
        match self {
            ExchangeId::Kraken => "kraken",
            ExchangeId::Okx => "okx",
            ExchangeId::BitvavoUsdc => "bitvavo_usdc",
        }
    }
}

impl fmt::Display for ExchangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExchangeId {
    type Err = ModelError;

    fn from_str(text: &str) -> Result<ExchangeId, ModelError> {
        match text {
            "kraken" => Ok(ExchangeId::Kraken),
            "okx" => Ok(ExchangeId::Okx),
            "bitvavo_usdc" => Ok(ExchangeId::BitvavoUsdc),
            autre => Err(ModelError::UnknownExchange(autre.to_string())),
        }
    }
}

/// Grille de frais nommée (docs/specs/s10.md §3) : le nom des sections de `config/fees.toml` et, dans
/// les runs s10, celui du bloc haché `costs.fee_grid.grid`.
///
/// `ExchangeId` reste l'identifiant des **données** (dossiers, `dataset_hash`) ; `FeeGridId` ne
/// désigne que des frais. `okx_xperps` n'est pas une variante : il tombe dans
/// [`ModelError::UnknownFeeGrid`] (décision du 13/10, 18:35).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeGridId {
    /// OKX Europe spot, grille Regular (0,20 % maker, 0,35 % taker).
    OkxRegular,
    /// Hypothèse de recherche pour la stratégie grid (docs/03 §15.3).
    BitvavoUsdc,
}

impl FeeGridId {
    /// Rendu canonique, identique à la forme sérialisée et au nom de section de `fees.toml`.
    pub const fn as_str(self) -> &'static str {
        match self {
            FeeGridId::OkxRegular => "okx_regular",
            FeeGridId::BitvavoUsdc => "bitvavo_usdc",
        }
    }

    /// Identifiant de lieu que porte `FeeSchedule.exchange` : il garde la sérialisation héritée
    /// (`"exchange":"okx"`) et donc les hashes des chemins s8 et s9.
    pub const fn legacy_exchange(self) -> ExchangeId {
        match self {
            FeeGridId::OkxRegular => ExchangeId::Okx,
            FeeGridId::BitvavoUsdc => ExchangeId::BitvavoUsdc,
        }
    }

    /// Grille qui correspond à un lieu d'exécution ; `None` pour Kraken, qui n'a pas de grille.
    ///
    /// La correspondance est bijective tant que `FeeGridId` n'a que deux variantes (s10 §9.2).
    pub const fn from_legacy_exchange(exchange: ExchangeId) -> Option<FeeGridId> {
        match exchange {
            ExchangeId::Okx => Some(FeeGridId::OkxRegular),
            ExchangeId::BitvavoUsdc => Some(FeeGridId::BitvavoUsdc),
            ExchangeId::Kraken => None,
        }
    }
}

impl fmt::Display for FeeGridId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FeeGridId {
    type Err = ModelError;

    /// `okx` est l'alias d'analyse de `okx_regular` ; tout autre nom est refusé.
    fn from_str(text: &str) -> Result<FeeGridId, ModelError> {
        match text {
            "okx_regular" | "okx" => Ok(FeeGridId::OkxRegular),
            "bitvavo_usdc" => Ok(FeeGridId::BitvavoUsdc),
            autre => Err(ModelError::UnknownFeeGrid(autre.to_string())),
        }
    }
}

/// Vérifie qu'une valeur en points de base est dans `0..=max`.
fn check_bps(name: &'static str, value: u32, max: u32) -> Result<(), ModelError> {
    if value > max {
        Err(ModelError::BpsOutOfRange { name, value, max })
    } else {
        Ok(())
    }
}

/// Section d'un exchange dans `config/fees.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeeEntry {
    maker_bps: u32,
    taker_bps: u32,
}

/// Grille de frais d'un exchange, en points de base entiers (20 bps = 0,20 %).
///
/// Sérialisée, elle donne `{"exchange":"okx","maker_bps":20,"taker_bps":35}`, le bloc `fees` du
/// manifeste de run (docs/03 §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FeeSchedule {
    pub exchange: ExchangeId,
    pub maker_bps: u32,
    pub taker_bps: u32,
}

impl FeeSchedule {
    /// Construit une grille de frais ; chaque taux doit être compris entre 0 et 10 000 bps.
    pub fn new(
        exchange: ExchangeId,
        maker_bps: u32,
        taker_bps: u32,
    ) -> Result<FeeSchedule, ModelError> {
        check_bps("maker_bps", maker_bps, MAX_BPS)?;
        check_bps("taker_bps", taker_bps, MAX_BPS)?;
        Ok(FeeSchedule {
            exchange,
            maker_bps,
            taker_bps,
        })
    }

    /// Frais maker en fraction du notionnel (0,0020 pour 20 bps).
    pub fn maker_rate(&self) -> f64 {
        f64::from(self.maker_bps) / BPS_PER_UNIT
    }

    /// Frais taker en fraction du notionnel (0,0035 pour 35 bps).
    pub fn taker_rate(&self) -> f64 {
        f64::from(self.taker_bps) / BPS_PER_UNIT
    }

    /// Lit la section de `grid` dans un texte TOML au format de `config/fees.toml`.
    ///
    /// Le `FeeSchedule` produit porte `grid.legacy_exchange()` : sa sérialisation reste celle des
    /// chemins s8 et s9. Une section absente donne [`ModelError::FeeGridNotFound`].
    pub fn from_toml_str_grid(grid: FeeGridId, text: &str) -> Result<FeeSchedule, ModelError> {
        let sections: BTreeMap<String, FeeEntry> = toml::from_str(text)?;
        match sections.get(grid.as_str()) {
            Some(entree) => {
                FeeSchedule::new(grid.legacy_exchange(), entree.maker_bps, entree.taker_bps)
            }
            None => Err(ModelError::FeeGridNotFound { grid }),
        }
    }

    /// Lit la section de `grid` dans un fichier TOML (en pratique `config/fees.toml`).
    pub fn load_grid(grid: FeeGridId, path: &Path) -> Result<FeeSchedule, ModelError> {
        let text = read_config_text(path)?;
        FeeSchedule::from_toml_str_grid(grid, &text)
    }

    /// Lit la grille d'un lieu d'exécution dans un texte TOML : `Okx` lit `[okx_regular]`,
    /// `BitvavoUsdc` lit `[bitvavo_usdc]`, et `Kraken` (aucune grille : on n'y trade pas) donne
    /// [`ModelError::FeesNotFound`].
    pub fn from_toml_str(exchange: ExchangeId, text: &str) -> Result<FeeSchedule, ModelError> {
        match FeeGridId::from_legacy_exchange(exchange) {
            Some(grid) => FeeSchedule::from_toml_str_grid(grid, text),
            None => {
                // Le TOML est lu avant le refus, comme avant S10 : un fichier invalide reste une
                // erreur de TOML.
                let _: BTreeMap<String, FeeEntry> = toml::from_str(text)?;
                Err(ModelError::FeesNotFound { exchange })
            }
        }
    }

    /// Lit la grille d'un lieu d'exécution dans un fichier TOML (en pratique `config/fees.toml`).
    pub fn load(exchange: ExchangeId, path: &Path) -> Result<FeeSchedule, ModelError> {
        let text = read_config_text(path)?;
        FeeSchedule::from_toml_str(exchange, &text)
    }
}

/// Lit un fichier de configuration en texte, avec l'erreur de lecture du module.
pub(crate) fn read_config_text(path: &Path) -> Result<String, ModelError> {
    std::fs::read_to_string(path).map_err(|source| ModelError::ConfigRead {
        path: path.display().to_string(),
        source,
    })
}

/// Slippage scalaire en points de base, en `f64` (docs/03 §7.2 : `slippage_bps` du manifeste).
///
/// Il pénalise toujours le trade : un achat market se remplit plus haut, une vente market ou un
/// stop plus bas. Sérialisé, il donne simplement le nombre de bps (`5.0`, `2.5`).
///
/// Le type du slippage (entier ou flottant) est une décision en attente du propriétaire (carte du
/// 14/10) : le moteur n'utilise que [`Slippage::buy_price`], [`Slippage::sell_price`] et
/// [`Slippage::rate`], donc seul ce type change si elle est tranchée autrement.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Slippage {
    bps: f64,
}

impl Slippage {
    /// Aucun slippage.
    pub const ZERO: Slippage = Slippage { bps: 0.0 };

    /// Construit un slippage ; il doit être fini, positif ou nul, et strictement inférieur à
    /// 10 000 bps, sinon un prix de vente `open × (1 − s)` deviendrait nul ou négatif. `-0.0` est
    /// normalisé en `0.0`.
    pub fn from_bps(bps: f64) -> Result<Slippage, ModelError> {
        if !(bps.is_finite() && bps >= 0.0 && bps < f64::from(MAX_BPS)) {
            return Err(ModelError::SlippageOutOfRange { value: bps });
        }
        // `-0.0 == 0.0` : cette garde attrape les deux zéros.
        let bps = if bps == 0.0 { 0.0 } else { bps };
        Ok(Slippage { bps })
    }

    /// Valeur en points de base.
    pub const fn bps(self) -> f64 {
        self.bps
    }

    /// Slippage en fraction du prix (0,0005 pour 5 bps).
    pub fn rate(self) -> f64 {
        self.bps / BPS_PER_UNIT
    }

    /// Prix payé pour un achat au prix de référence `price` : `price × (1 + slippage)`.
    pub fn buy_price(self, price: f64) -> f64 {
        price * (1.0 + self.rate())
    }

    /// Prix reçu pour une vente au prix de référence `price` : `price × (1 − slippage)`.
    pub fn sell_price(self, price: f64) -> f64 {
        price * (1.0 - self.rate())
    }
}

/// Lecture d'un nombre de bps (`5`, `5.0`, `2.5`) : permet à clap d'utiliser `Slippage` comme type
/// d'argument.
impl FromStr for Slippage {
    type Err = ModelError;

    fn from_str(text: &str) -> Result<Slippage, ModelError> {
        let bps = text
            .parse::<f64>()
            .map_err(|_| ModelError::SlippageNotANumber {
                text: text.to_string(),
            })?;
        Slippage::from_bps(bps)
    }
}

// ---------------------------------------------------------------------------------------------
// Side, MarketEvent
// ---------------------------------------------------------------------------------------------

/// Sens d'un ordre. Spot seulement : pas de vente à découvert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// Entrée du simulateur (docs/03 §1 principe 1 et §6).
///
/// Jalon 1 : seul `ClosedBar` est émis. Les variantes `Trade { ts, px, qty }` et
/// `BookTop { ts, bid, ask }` s'ajouteront au jalon 3 (paper) : le compilateur signalera alors
/// chaque `match` à compléter, ce qui est voulu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarketEvent {
    /// Une bougie vient de se clôturer.
    ClosedBar(Candle),
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FEES_TOML: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/fees.toml");

    fn healthy_candle() -> Candle {
        Candle::new(1_704_067_200_000, 100.0, 110.0, 95.0, 105.0, 12.5, Some(42)).unwrap()
    }

    fn with_mutation(mutation: impl Fn(&mut Candle)) -> Result<(), ModelError> {
        let mut c = healthy_candle();
        mutation(&mut c);
        c.validate()
    }

    // --- Candle ---

    #[test]
    fn une_bougie_saine_est_acceptee() {
        assert!(healthy_candle().validate().is_ok());
        assert!(
            Candle::new(0, 1.0, 1.0, 1.0, 1.0, 0.0, None).is_ok(),
            "bougie plate, volume nul"
        );
    }

    #[test]
    fn une_bougie_refuse_nan_et_infini() {
        for (name, value) in [
            ("open", f64::NAN),
            ("high", f64::INFINITY),
            ("low", f64::NAN),
        ] {
            let result = with_mutation(|c| match name {
                "open" => c.open = value,
                "high" => c.high = value,
                _ => c.low = value,
            });
            assert!(
                matches!(result, Err(ModelError::CandleNotFinite { field, .. }) if field == name),
                "{name} : {result:?}"
            );
        }
        let volume = with_mutation(|c| c.volume = f64::NEG_INFINITY);
        assert!(matches!(
            volume,
            Err(ModelError::CandleNotFinite {
                field: "volume",
                ..
            })
        ));
    }

    #[test]
    fn une_bougie_refuse_un_prix_nul_ou_negatif() {
        let zero = with_mutation(|c| c.low = 0.0);
        assert!(matches!(
            zero,
            Err(ModelError::NonPositivePrice { field: "low", .. })
        ));
        let negative = with_mutation(|c| c.open = -1.0);
        assert!(matches!(
            negative,
            Err(ModelError::NonPositivePrice { field: "open", .. })
        ));
    }

    #[test]
    fn une_bougie_refuse_un_ohlc_incoherent() {
        assert!(matches!(
            with_mutation(|c| c.high = 99.0),
            Err(ModelError::InconsistentOhlc { .. })
        ));
        assert!(matches!(
            with_mutation(|c| c.low = 101.0),
            Err(ModelError::InconsistentOhlc { .. })
        ));
        assert!(matches!(
            with_mutation(|c| c.close = 120.0),
            Err(ModelError::InconsistentOhlc { .. })
        ));
        assert!(matches!(
            with_mutation(|c| c.open = 90.0),
            Err(ModelError::InconsistentOhlc { .. })
        ));
    }

    #[test]
    fn une_bougie_refuse_un_volume_negatif() {
        assert!(matches!(
            with_mutation(|c| c.volume = -0.1),
            Err(ModelError::NegativeVolume { .. })
        ));
        assert!(
            with_mutation(|c| c.volume = -0.0).is_ok(),
            "-0.0 n'est pas négatif"
        );
    }

    #[test]
    fn le_message_d_erreur_nomme_l_horodatage() {
        let error = with_mutation(|c| c.volume = -1.0).unwrap_err();
        assert!(error.to_string().contains("1704067200000"), "{error}");
    }

    // --- Timeframe ---

    #[test]
    fn les_durees_sont_celles_de_docs_03() {
        assert_eq!(Timeframe::M1.ms(), 60_000);
        assert_eq!(Timeframe::M5.ms(), 300_000);
        assert_eq!(Timeframe::H1.ms(), 3_600_000);
        assert_eq!(Timeframe::H4.ms(), 14_400_000);
        assert_eq!(Timeframe::D1.ms(), 86_400_000);
    }

    #[test]
    fn les_timeframes_sont_ranges_par_duree() {
        let durees: Vec<i64> = Timeframe::ALL.iter().map(|tf| tf.ms()).collect();
        assert!(durees.windows(2).all(|paire| paire[0] < paire[1]));
        assert!(Timeframe::M1 < Timeframe::D1);
    }

    #[test]
    fn aller_retour_timeframe_chaine() {
        for tf in Timeframe::ALL {
            assert_eq!(tf.as_str().parse::<Timeframe>().unwrap(), tf);
            assert_eq!(tf.to_string(), tf.as_str());
        }
        assert!(matches!(
            "2h".parse::<Timeframe>(),
            Err(ModelError::UnknownTimeframe(_))
        ));
        assert!("1M".parse::<Timeframe>().is_err(), "la casse compte");
        assert!("".parse::<Timeframe>().is_err());
    }

    #[test]
    fn aller_retour_timeframe_json() {
        for tf in Timeframe::ALL {
            let json = serde_json::to_string(&tf).unwrap();
            assert_eq!(
                json,
                format!("\"{}\"", tf.as_str()),
                "serde et as_str doivent concorder"
            );
            assert_eq!(serde_json::from_str::<Timeframe>(&json).unwrap(), tf);
        }
    }

    #[test]
    fn les_bits_des_timeframes_sont_distincts_et_remplissent_un_octet() {
        let mut masque = 0u8;
        for (i, tf) in Timeframe::ALL.into_iter().enumerate() {
            assert_eq!(tf.bit(), 1u8 << i, "{tf}");
            assert_eq!(masque & tf.bit(), 0, "bit déjà pris par un autre : {tf}");
            masque |= tf.bit();
        }
        assert_eq!(masque, 0xFF);
    }

    #[test]
    fn la_periode_maximale_d_un_indicateur() {
        assert_eq!(MAX_INDICATOR_PERIOD, 100_000);
    }

    #[test]
    fn alignement_des_open_time() {
        // 2024-01-01T00:00:00Z
        let minuit = 1_704_067_200_000;
        for tf in Timeframe::ALL {
            assert!(tf.is_aligned(minuit), "{tf}");
            assert!(tf.is_aligned(0));
            assert!(tf.check_alignment(minuit + tf.ms()).is_ok());
            assert!(matches!(
                tf.check_alignment(minuit + 1),
                Err(ModelError::MisalignedOpenTime { .. })
            ));
        }
        assert!(Timeframe::H1.is_aligned(minuit + 3_600_000));
        assert!(!Timeframe::H1.is_aligned(minuit + 60_000));
        assert!(Timeframe::M1.is_aligned(minuit + 60_000));
        assert!(Timeframe::H1.is_aligned(-3_600_000), "avant 1970 aussi");
        assert!(!Timeframe::H1.is_aligned(-1));
    }

    #[test]
    fn debut_de_la_bougie_qui_contient_un_instant() {
        let minuit = 1_704_067_200_000;
        assert_eq!(
            Timeframe::H4.open_time_of(minuit + 5 * 3_600_000 + 17),
            minuit + 4 * 3_600_000
        );
        assert_eq!(Timeframe::M1.open_time_of(minuit + 59_999), minuit);
        assert_eq!(Timeframe::D1.open_time_of(-1), -86_400_000);
    }

    // --- Exchange, frais, slippage ---

    #[test]
    fn aller_retour_exchange() {
        for id in [ExchangeId::Kraken, ExchangeId::Okx, ExchangeId::BitvavoUsdc] {
            assert_eq!(id.as_str().parse::<ExchangeId>().unwrap(), id);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{}\"", id.as_str()));
        }
        assert!(matches!(
            "binance".parse::<ExchangeId>(),
            Err(ModelError::UnknownExchange(_))
        ));
    }

    #[test]
    fn config_fees_toml_okx() {
        let frais = FeeSchedule::load(ExchangeId::Okx, Path::new(FEES_TOML)).unwrap();
        assert_eq!((frais.maker_bps, frais.taker_bps), (20, 35));
        assert!((frais.maker_rate() - 0.0020).abs() < 1e-15);
        assert!((frais.taker_rate() - 0.0035).abs() < 1e-15);
    }

    #[test]
    fn config_fees_toml_bitvavo_usdc() {
        let frais = FeeSchedule::load(ExchangeId::BitvavoUsdc, Path::new(FEES_TOML)).unwrap();
        assert_eq!((frais.maker_bps, frais.taker_bps), (5, 5));
        assert_eq!(frais.exchange, ExchangeId::BitvavoUsdc);
    }

    #[test]
    fn exchange_sans_grille_de_frais() {
        let error = FeeSchedule::load(ExchangeId::Kraken, Path::new(FEES_TOML)).unwrap_err();
        assert!(matches!(
            error,
            ModelError::FeesNotFound {
                exchange: ExchangeId::Kraken
            }
        ));
    }

    #[test]
    fn grilles_nommees_et_alias() {
        for (texte, attendu) in [
            ("okx_regular", FeeGridId::OkxRegular),
            ("okx", FeeGridId::OkxRegular),
            ("bitvavo_usdc", FeeGridId::BitvavoUsdc),
        ] {
            assert_eq!(texte.parse::<FeeGridId>().unwrap(), attendu, "{texte}");
        }
        assert_eq!(FeeGridId::OkxRegular.as_str(), "okx_regular");
        assert_eq!(
            serde_json::to_string(&FeeGridId::BitvavoUsdc).unwrap(),
            "\"bitvavo_usdc\""
        );
        assert_eq!(FeeGridId::OkxRegular.legacy_exchange(), ExchangeId::Okx);
        assert_eq!(
            FeeGridId::from_legacy_exchange(ExchangeId::BitvavoUsdc),
            Some(FeeGridId::BitvavoUsdc)
        );
        assert_eq!(FeeGridId::from_legacy_exchange(ExchangeId::Kraken), None);
    }

    #[test]
    fn grilles_inconnues_refusees_avec_la_liste_attendue() {
        for nom in ["okx_xperps", "kraken", "binance", "", "OKX"] {
            let erreur = nom.parse::<FeeGridId>().unwrap_err();
            assert!(matches!(erreur, ModelError::UnknownFeeGrid(_)), "{nom}");
            let message = erreur.to_string();
            assert!(message.contains(&format!("`{nom}`")), "{message}");
            assert!(
                message.contains("okx_regular, bitvavo_usdc") && message.contains("alias : okx"),
                "{message}"
            );
        }
    }

    #[test]
    fn config_fees_toml_a_la_section_okx_regular_et_pas_okx() {
        let texte = std::fs::read_to_string(FEES_TOML).unwrap();
        let sections: BTreeMap<String, toml::Value> = toml::from_str(&texte).unwrap();
        assert!(sections.contains_key("okx_regular"));
        assert!(sections.contains_key("bitvavo_usdc"));
        assert!(
            !sections.contains_key("okx"),
            "l'ancienne section est renommée"
        );
    }

    #[test]
    fn la_grille_okx_regular_garde_la_serialisation_heritee() {
        let par_grille =
            FeeSchedule::load_grid(FeeGridId::OkxRegular, Path::new(FEES_TOML)).unwrap();
        let par_lieu = FeeSchedule::load(ExchangeId::Okx, Path::new(FEES_TOML)).unwrap();
        assert_eq!(par_grille, par_lieu);
        assert_eq!(
            serde_json::to_string(&par_lieu).unwrap(),
            r#"{"exchange":"okx","maker_bps":20,"taker_bps":35}"#
        );
    }

    #[test]
    fn ancienne_section_okx_refusee_sans_repli() {
        let ancienne = "[okx]\nmaker_bps = 20\ntaker_bps = 35\n";
        let erreur = FeeSchedule::from_toml_str_grid(FeeGridId::OkxRegular, ancienne).unwrap_err();
        assert!(matches!(
            erreur,
            ModelError::FeeGridNotFound {
                grid: FeeGridId::OkxRegular
            }
        ));
        assert!(erreur.to_string().contains("[okx_regular]"));
        let par_lieu = FeeSchedule::from_toml_str(ExchangeId::Okx, ancienne);
        assert!(matches!(par_lieu, Err(ModelError::FeeGridNotFound { .. })));
    }

    #[test]
    fn fichier_de_frais_absent_ou_invalide() {
        let absent = FeeSchedule::load(ExchangeId::Okx, Path::new("/n/existe/pas.toml"));
        assert!(matches!(absent, Err(ModelError::ConfigRead { .. })));
        let invalide =
            FeeSchedule::from_toml_str(ExchangeId::Okx, "[okx_regular]\nmaker_bps = \"vingt\"");
        assert!(matches!(invalide, Err(ModelError::ConfigToml(_))));
        let incomplet =
            FeeSchedule::from_toml_str(ExchangeId::Okx, "[okx_regular]\nmaker_bps = 20");
        assert!(matches!(incomplet, Err(ModelError::ConfigToml(_))));
        let inconnu = FeeSchedule::from_toml_str(
            ExchangeId::Okx,
            "[okx_regular]\nmaker_bps = 20\ntaker_bps = 35\nrebate_bps = 1",
        );
        assert!(
            matches!(inconnu, Err(ModelError::ConfigToml(_))),
            "champ inconnu refusé"
        );
    }

    #[test]
    fn frais_hors_bornes() {
        assert!(FeeSchedule::new(ExchangeId::Okx, 10_000, 0).is_ok());
        let trop = FeeSchedule::new(ExchangeId::Okx, 20, 10_001);
        assert!(matches!(
            trop,
            Err(ModelError::BpsOutOfRange {
                name: "taker_bps",
                ..
            })
        ));
        let toml = FeeSchedule::from_toml_str(
            ExchangeId::Okx,
            "[okx_regular]\nmaker_bps = 99999\ntaker_bps = 1",
        );
        assert!(matches!(
            toml,
            Err(ModelError::BpsOutOfRange {
                name: "maker_bps",
                ..
            })
        ));
    }

    #[test]
    fn les_frais_se_serialisent_comme_dans_le_manifeste() {
        let frais = FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap();
        let json = serde_json::to_string(&frais).unwrap();
        assert_eq!(json, r#"{"exchange":"okx","maker_bps":20,"taker_bps":35}"#);
    }

    #[test]
    fn slippage_ajuste_les_prix_contre_le_trade() {
        let s = Slippage::from_bps(5.0).unwrap();
        assert_eq!(s.bps(), 5.0);
        assert!((s.rate() - 0.0005).abs() < 1e-15);
        assert!((s.buy_price(100.0) - 100.05).abs() < 1e-12);
        assert!((s.sell_price(100.0) - 99.95).abs() < 1e-12);
        assert_eq!(Slippage::ZERO.buy_price(123.0), 123.0);
        assert_eq!(Slippage::ZERO.sell_price(123.0), 123.0);
    }

    #[test]
    fn slippage_fractionnaire() {
        let s = Slippage::from_bps(2.5).unwrap();
        assert_eq!(s.rate(), 0.00025);
        assert!((s.buy_price(100.0) - 100.025).abs() < 1e-12);
    }

    #[test]
    fn slippage_hors_bornes() {
        for refuse in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.1,
            10_000.0,
            1e300,
        ] {
            assert!(
                matches!(
                    Slippage::from_bps(refuse),
                    Err(ModelError::SlippageOutOfRange { .. })
                ),
                "{refuse}"
            );
        }
        assert!(Slippage::from_bps(0.0).is_ok());
        assert!(Slippage::from_bps(9_999.5).is_ok());
        let zero_negatif = Slippage::from_bps(-0.0).unwrap();
        assert_eq!(zero_negatif.bps().to_bits(), 0, "-0.0 est normalisé");
    }

    #[test]
    fn slippage_se_serialise_en_nombre() {
        let s = Slippage::from_bps(5.0).unwrap();
        assert_eq!(serde_json::to_string(&s).unwrap(), "5.0");
        assert_eq!(hash::canonical_json(&s).unwrap(), "5.0");
        let demi = Slippage::from_bps(2.5).unwrap();
        assert_eq!(hash::canonical_json(&demi).unwrap(), "2.5");
    }

    #[test]
    fn slippage_depuis_une_chaine() {
        assert_eq!(
            "5".parse::<Slippage>().unwrap(),
            "5.0".parse::<Slippage>().unwrap()
        );
        assert!(matches!(
            "abc".parse::<Slippage>(),
            Err(ModelError::SlippageNotANumber { .. })
        ));
        for hors in ["-1", "nan"] {
            assert!(
                matches!(
                    hors.parse::<Slippage>(),
                    Err(ModelError::SlippageOutOfRange { .. })
                ),
                "{hors}"
            );
        }
    }

    // --- Side, MarketEvent ---

    #[test]
    fn un_evenement_porte_sa_bougie() {
        let bougie = healthy_candle();
        let MarketEvent::ClosedBar(lue) = MarketEvent::ClosedBar(bougie);
        assert_eq!(lue, bougie);
        assert_ne!(Side::Buy, Side::Sell);
    }
}
