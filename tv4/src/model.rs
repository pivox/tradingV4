//! Types partagés du moteur : `Candle`, `Timeframe`, `FeeSchedule`, `Slippage`, `Side`, `MarketEvent`
//! et erreurs (docs/03-architecture.md §2 et §6). Le JSON trié et le hash blake3 sont dans [`hash`].
//!
//! Règles de ce module : il ne dépend d'aucun autre module du moteur, n'utilise pas `chrono` (le
//! temps est un `i64` en millisecondes UTC, docs/03 §5) et les prix, quantités et volumes sont des
//! `f64` (docs/03 §5.1).

pub mod hash;

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

/// Valeur maximale acceptée pour des frais ou un slippage, en points de base (100 %).
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

    /// Frais ou slippage hors bornes.
    #[error("`{name}` vaut {value} bps, hors des bornes autorisées (0 à {max})")]
    BpsOutOfRange {
        name: &'static str,
        value: u32,
        max: u32,
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

    /// Lit la section de `exchange` dans un texte TOML au format de `config/fees.toml`.
    pub fn from_toml_str(exchange: ExchangeId, text: &str) -> Result<FeeSchedule, ModelError> {
        let sections: BTreeMap<String, FeeEntry> = toml::from_str(text)?;
        match sections.get(exchange.as_str()) {
            Some(entree) => FeeSchedule::new(exchange, entree.maker_bps, entree.taker_bps),
            None => Err(ModelError::FeesNotFound { exchange }),
        }
    }

    /// Lit la section de `exchange` dans un fichier TOML (en pratique `config/fees.toml`).
    pub fn load(exchange: ExchangeId, path: &Path) -> Result<FeeSchedule, ModelError> {
        let text = std::fs::read_to_string(path).map_err(|source| ModelError::ConfigRead {
            path: path.display().to_string(),
            source,
        })?;
        FeeSchedule::from_toml_str(exchange, &text)
    }
}

/// Slippage scalaire en points de base (docs/03 §7.2 : `slippage_bps` du manifeste).
///
/// Il pénalise toujours le trade : un achat market se remplit plus haut, une vente market ou un
/// stop plus bas. Sérialisé, il donne simplement le nombre de bps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Slippage {
    bps: u32,
}

impl Slippage {
    /// Aucun slippage.
    pub const ZERO: Slippage = Slippage { bps: 0 };

    /// Construit un slippage ; il doit rester strictement inférieur à 10 000 bps, sinon un prix de
    /// vente deviendrait nul ou négatif.
    pub fn from_bps(bps: u32) -> Result<Slippage, ModelError> {
        check_bps("slippage_bps", bps, MAX_BPS - 1)?;
        Ok(Slippage { bps })
    }

    /// Valeur en points de base.
    pub const fn bps(self) -> u32 {
        self.bps
    }

    /// Slippage en fraction du prix (0,0005 pour 5 bps).
    pub fn rate(self) -> f64 {
        f64::from(self.bps) / BPS_PER_UNIT
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
    fn fichier_de_frais_absent_ou_invalide() {
        let absent = FeeSchedule::load(ExchangeId::Okx, Path::new("/n/existe/pas.toml"));
        assert!(matches!(absent, Err(ModelError::ConfigRead { .. })));
        let invalide = FeeSchedule::from_toml_str(ExchangeId::Okx, "[okx]\nmaker_bps = \"vingt\"");
        assert!(matches!(invalide, Err(ModelError::ConfigToml(_))));
        let incomplet = FeeSchedule::from_toml_str(ExchangeId::Okx, "[okx]\nmaker_bps = 20");
        assert!(matches!(incomplet, Err(ModelError::ConfigToml(_))));
        let inconnu = FeeSchedule::from_toml_str(
            ExchangeId::Okx,
            "[okx]\nmaker_bps = 20\ntaker_bps = 35\nrebate_bps = 1",
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
        let toml =
            FeeSchedule::from_toml_str(ExchangeId::Okx, "[okx]\nmaker_bps = 99999\ntaker_bps = 1");
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
        let s = Slippage::from_bps(5).unwrap();
        assert_eq!(s.bps(), 5);
        assert!((s.rate() - 0.0005).abs() < 1e-15);
        assert!((s.buy_price(100.0) - 100.05).abs() < 1e-12);
        assert!((s.sell_price(100.0) - 99.95).abs() < 1e-12);
        assert_eq!(Slippage::ZERO.buy_price(123.0), 123.0);
        assert_eq!(Slippage::ZERO.sell_price(123.0), 123.0);
    }

    #[test]
    fn slippage_hors_bornes_et_serialisation() {
        assert!(Slippage::from_bps(9_999).is_ok());
        assert!(matches!(
            Slippage::from_bps(10_000),
            Err(ModelError::BpsOutOfRange { .. })
        ));
        assert_eq!(
            serde_json::to_string(&Slippage::from_bps(5).unwrap()).unwrap(),
            "5"
        );
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
