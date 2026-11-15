//! Entrées numériques des post-traitements, sans accès aux artefacts du run.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TradeRecord {
    pub row: usize,
    pub lot_id: u64,
    pub symbol: String,
    pub entry_ms: i64,
    pub exit_ms: i64,
    pub qty: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub entry_fee: f64,
    pub exit_fee: f64,
    pub pnl_net: f64,
    pub r_multiple: Option<f64>,
    pub included: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkPoint {
    pub at_ms: i64,
    pub equity: f64,
    pub day_close: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CashPoint {
    pub at_ms: i64,
    pub cash: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PricePoint {
    pub at_ms: i64,
    pub close: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsSizing {
    Research,
    Compounding,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatsOptions {
    #[serde(with = "decimal_seed")]
    pub seed: u64,
    pub samples: u32,
    pub bootstrap_block: usize,
    pub correlation_threshold: f64,
}

impl StatsOptions {
    /// La CLI imposera séparément au moins 10 000 réplications ; les noyaux acceptent B>=2.
    pub const fn new(seed: u64) -> Self {
        Self {
            seed,
            samples: 10_000,
            bootstrap_block: 5,
            correlation_threshold: 0.7,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), StatsError> {
        if self.samples < 2
            || !(5..=10).contains(&self.bootstrap_block)
            || !self.correlation_threshold.is_finite()
            || !(0.0..1.0).contains(&self.correlation_threshold)
        {
            return Err(StatsError::InvalidOptions(
                "B>=2, bloc entre 5 et 10 et seuil de corrélation dans [0,1[ requis".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub availability: Availability,
    pub value: Option<f64>,
    pub reasons: Vec<String>,
}

impl Metric {
    pub(crate) fn available(value: f64) -> Result<Self, StatsError> {
        finite(value)?;
        Ok(Self {
            availability: Availability::Complete,
            value: Some(value),
            reasons: vec![],
        })
    }
    pub(crate) fn unavailable(reason: &str) -> Self {
        Self {
            availability: Availability::Unavailable,
            value: None,
            reasons: vec![reason.into()],
        }
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum StatsError {
    #[error("entrée statistique invalide : {0}")]
    InvalidInput(String),
    #[error("options statistiques invalides : {0}")]
    InvalidOptions(String),
    #[error("résultat statistique non fini")]
    NonFiniteResult,
}

pub(crate) fn finite(value: f64) -> Result<f64, StatsError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(StatsError::NonFiniteResult)
    }
}

/// Validation avant filtrage : aucune valeur NaN/infini n'est admise, même sur une ligne purgée.
pub(crate) fn sorted_trades(trades: &[TradeRecord]) -> Result<Vec<&TradeRecord>, StatsError> {
    for t in trades {
        for (name, value) in [
            ("qty", t.qty),
            ("entry_price", t.entry_price),
            ("exit_price", t.exit_price),
            ("entry_fee", t.entry_fee),
            ("exit_fee", t.exit_fee),
            ("pnl_net", t.pnl_net),
        ] {
            if !value.is_finite()
                || (matches!(name, "qty" | "entry_price" | "exit_price") && value <= 0.)
            {
                return Err(StatsError::InvalidInput(format!(
                    "ligne {} : {name} non fini ou non positif",
                    t.row
                )));
            }
        }
        if t.r_multiple.is_some_and(|r| !r.is_finite()) {
            return Err(StatsError::InvalidInput(format!(
                "ligne {} : R non fini",
                t.row
            )));
        }
    }
    let mut kept: Vec<_> = trades.iter().filter(|t| t.included).collect();
    kept.sort_by(|a, b| {
        (a.exit_ms, &a.symbol, a.lot_id, a.entry_ms, a.row)
            .cmp(&(b.exit_ms, &b.symbol, b.lot_id, b.entry_ms, b.row))
    });
    Ok(kept)
}

/// Décimal canonique sur toute la plage u64, lisible sans perte par les lecteurs PHP.
mod decimal_seed {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(seed: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&seed.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || !value.bytes().all(|b| b.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(D::Error::custom(
                "graine : chaîne décimale canonique u64 requise",
            ));
        }
        value
            .parse()
            .map_err(|_| D::Error::custom("graine hors plage u64"))
    }
}
