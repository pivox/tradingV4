//! Contraintes d'exchange d'un instrument : tick, lot, minimums (docs/04-protocole-recherche.md §3.1
//! point 6, docs/03-architecture.md §7.2).
//!
//! Les arrondis ne multiplient jamais par le pas : on divise par son inverse, qui doit être un entier
//! exact (`1.0 / 0.1 == 10.0`, `1.0 / 1e-8 == 1e8`), car `k / 10.0` vaut exactement le décimal le plus
//! proche alors que `k * 0.1` en diffère dans un cas sur trois. Un petit accrochage ([`STEP_SNAP`])
//! absorbe les erreurs de représentation (`0.29 * 1e8 = 28 999 999,999999996`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{ExchangeId, ModelError};

/// Tolérance d'accrochage d'un arrondi au pas, en pas : absorbe les erreurs de représentation sans
/// jamais déplacer un nombre de plus d'un millionième de pas.
pub const STEP_SNAP: f64 = 1e-6;

/// Plus grand nombre de pas représentable exactement (2^53).
pub const MAX_STEPS: f64 = 9_007_199_254_740_992.0;

/// Borne haute de l'inverse d'un pas : 1e12 pas par unité, très au-delà de 1e-8.
const MAX_INVERSE: f64 = 1e12;

/// Contraintes d'exchange d'un instrument, dans le `config_hash`.
///
/// Les prix sont arrondis au tick et les quantités au lot (sens d'arrondi dans la doc de `sim`).
/// `min_qty` est en monnaie de base, `min_notional` en monnaie de cotation ; 0 désactive le contrôle.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentRules {
    pub tick_size: f64,
    pub lot_size: f64,
    pub min_qty: f64,
    pub min_notional: f64,
}

/// Fichier `config/instruments.toml` : exchange, puis symbole canonique.
type InstrumentFile = BTreeMap<String, BTreeMap<String, InstrumentRules>>;

impl InstrumentRules {
    /// Construit et valide les contraintes.
    pub fn new(
        tick_size: f64,
        lot_size: f64,
        min_qty: f64,
        min_notional: f64,
    ) -> Result<InstrumentRules, ModelError> {
        let rules = InstrumentRules {
            tick_size,
            lot_size,
            min_qty,
            min_notional,
        };
        rules.validate()?;
        Ok(rules)
    }

    /// Contrôle dans l'ordre : pas finis et positifs, inverses entiers exacts, minimums finis et
    /// positifs ou nuls. La première erreur est rendue.
    pub fn validate(&self) -> Result<(), ModelError> {
        for (field, value) in [("tick_size", self.tick_size), ("lot_size", self.lot_size)] {
            if !(value.is_finite() && value > 0.0) {
                return Err(ModelError::InvalidInstrument {
                    field,
                    value,
                    reason: "un nombre fini strictement positif est attendu",
                });
            }
        }
        for (field, value) in [("tick_size", self.tick_size), ("lot_size", self.lot_size)] {
            let inv = 1.0 / value;
            if !(inv.is_finite() && inv >= 1.0 && inv.fract() == 0.0 && inv <= MAX_INVERSE) {
                return Err(ModelError::InvalidInstrument {
                    field,
                    value,
                    reason: "l'inverse du pas doit être un entier exact entre 1 et 1e12 (0.1, 0.01, 1e-8 conviennent)",
                });
            }
        }
        for (field, value) in [
            ("min_qty", self.min_qty),
            ("min_notional", self.min_notional),
        ] {
            if !(value.is_finite() && value >= 0.0) {
                return Err(ModelError::InvalidInstrument {
                    field,
                    value,
                    reason: "un nombre fini positif ou nul est attendu",
                });
            }
        }
        Ok(())
    }

    /// Inverse du tick (10.0 pour un tick de 0,1).
    pub fn inv_tick(&self) -> f64 {
        1.0 / self.tick_size
    }

    /// Inverse du lot (1e8 pour un lot de 1e-8).
    pub fn inv_lot(&self) -> f64 {
        1.0 / self.lot_size
    }

    /// Lots minimums : 0 si `min_qty == 0`, sinon `min_qty` arrondi au lot supérieur.
    pub fn min_lots(&self) -> u64 {
        if self.min_qty == 0.0 {
            0
        } else {
            ceil_steps(self.min_qty, self.inv_lot()).unwrap_or(u64::MAX)
        }
    }

    /// Lit les contraintes de `exchange` et `symbol` dans un texte TOML au format de
    /// `config/instruments.toml`. Le champ `path` d'une erreur [`ModelError::InstrumentNotFound`] vaut
    /// `<texte>` ; [`InstrumentRules::load`] y met le vrai chemin.
    pub fn from_toml_str(
        exchange: ExchangeId,
        symbol: &str,
        text: &str,
    ) -> Result<InstrumentRules, ModelError> {
        let file: InstrumentFile = toml::from_str(text)?;
        let rules = file
            .get(exchange.as_str())
            .and_then(|symbols| symbols.get(symbol))
            .ok_or_else(|| ModelError::InstrumentNotFound {
                exchange,
                symbol: symbol.to_string(),
                path: "<texte>".to_string(),
            })?;
        rules.validate()?;
        Ok(*rules)
    }

    /// Lit les contraintes dans un fichier TOML (en pratique `config/instruments.toml`).
    pub fn load(
        exchange: ExchangeId,
        symbol: &str,
        path: &Path,
    ) -> Result<InstrumentRules, ModelError> {
        let text = std::fs::read_to_string(path).map_err(|source| ModelError::ConfigRead {
            path: path.display().to_string(),
            source,
        })?;
        InstrumentRules::from_toml_str(exchange, symbol, &text).map_err(|e| match e {
            ModelError::InstrumentNotFound {
                exchange, symbol, ..
            } => ModelError::InstrumentNotFound {
                exchange,
                symbol,
                path: path.display().to_string(),
            },
            autre => autre,
        })
    }
}

/// Convertit un nombre de pas flottant en entier, ou `None` s'il n'est pas fini, est négatif ou
/// dépasse [`MAX_STEPS`]. Un `-0.0` est ramené à 0.
fn steps_to_u64(k: f64) -> Option<u64> {
    if k.is_finite() && (0.0..=MAX_STEPS).contains(&k) {
        Some(k as u64)
    } else {
        None
    }
}

/// `floor(x × inv + STEP_SNAP)` en pas entiers ; `inv` est l'inverse du pas.
pub fn floor_steps(x: f64, inv: f64) -> Option<u64> {
    steps_to_u64((x * inv + STEP_SNAP).floor())
}

/// `ceil(x × inv − STEP_SNAP)` en pas entiers ; `inv` est l'inverse du pas.
pub fn ceil_steps(x: f64, inv: f64) -> Option<u64> {
    steps_to_u64((x * inv - STEP_SNAP).ceil())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FICHIER: &str = include_str!("../../../config/instruments.toml");

    #[test]
    fn arrondi_prix_achat_vers_le_bas_vente_vers_le_haut() {
        let inv = InstrumentRules::new(0.1, 1e-8, 0.0, 0.0)
            .unwrap()
            .inv_tick();
        assert_eq!(inv, 10.0);
        assert_eq!(floor_steps(100.07, inv), Some(1000));
        assert_eq!(ceil_steps(100.07, inv), Some(1001));
        // Un prix déjà sur le tick reste inchangé dans les deux sens.
        assert_eq!(floor_steps(100.1, inv), Some(1001));
        assert_eq!(ceil_steps(100.1, inv), Some(1001));
        assert_eq!(1000.0 / inv, 100.0);
        assert_eq!(1001.0 / inv, 100.1);
    }

    #[test]
    fn accrochage_des_quantites() {
        // 0.29 * 1e8 = 28 999 999,999999996 : sans l'accrochage ce serait 28 999 999.
        assert!(std::hint::black_box(0.29_f64) * 1e8 < 29_000_000.0);
        assert_eq!(floor_steps(0.29, 1e8), Some(29_000_000));
        assert_eq!(floor_steps(1.15, 1e8), Some(115_000_000));
        assert_eq!(floor_steps(0.57, 1e8), Some(57_000_000));
        assert_eq!(floor_steps(0.1 + 0.2, 1e8), Some(30_000_000));
        assert_eq!(floor_steps(f64::INFINITY, 1e8), None);
        assert_eq!(floor_steps(f64::NAN, 1e8), None);
        assert_eq!(floor_steps(-1.0, 1e8), None);
        assert_eq!(floor_steps(1e300, 1e8), None);
        assert_eq!(floor_steps(0.0, 1e8), Some(0));
        // Plafond par le bas : la partie en dessous du lot est perdue.
        assert_eq!(floor_steps(0.123456789, 1e8), Some(12_345_678));
        assert_eq!(ceil_steps(0.123456781, 1e8), Some(12_345_679));
        // `ceil` d'un petit négatif ramène à 0 (jamais -0 converti en u64 négatif).
        assert_eq!(ceil_steps(0.0, 1e8), Some(0));
    }

    #[test]
    fn validation() {
        let champ = |r: Result<InstrumentRules, ModelError>| match r {
            Err(ModelError::InvalidInstrument { field, .. }) => field,
            autre => panic!("attendu InvalidInstrument, obtenu {autre:?}"),
        };
        assert_eq!(
            champ(InstrumentRules::new(0.0, 1e-8, 0.0, 0.0)),
            "tick_size"
        );
        assert_eq!(
            champ(InstrumentRules::new(0.1, -1e-8, 0.0, 0.0)),
            "lot_size"
        );
        assert_eq!(
            champ(InstrumentRules::new(0.3, 1e-8, 0.0, 0.0)),
            "tick_size"
        );
        assert_eq!(champ(InstrumentRules::new(0.1, 3e-8, 0.0, 0.0)), "lot_size");
        assert_eq!(
            champ(InstrumentRules::new(2.0, 1e-8, 0.0, 0.0)),
            "tick_size"
        );
        assert_eq!(
            champ(InstrumentRules::new(0.1, 1e-8, f64::NAN, 0.0)),
            "min_qty"
        );
        assert_eq!(
            champ(InstrumentRules::new(0.1, 1e-8, 0.0, -1.0)),
            "min_notional"
        );
        assert_eq!(
            champ(InstrumentRules::new(f64::NAN, 1e-8, 0.0, 0.0)),
            "tick_size"
        );
        assert!(InstrumentRules::new(0.01, 1e-8, 0.0001, 5.0).is_ok());
        assert!(InstrumentRules::new(1e-4, 1e-8, 0.0, 0.0).is_ok());
    }

    #[test]
    fn minimums_en_lots() {
        let rules = InstrumentRules::new(0.1, 1e-8, 0.0001, 0.0).unwrap();
        assert_eq!(rules.min_lots(), 10_000);
        let rules = InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).unwrap();
        assert_eq!(rules.min_lots(), 0);
        // Un minimum non aligné sur le lot est relevé.
        let rules = InstrumentRules::new(0.1, 1e-4, 0.00015, 0.0).unwrap();
        assert_eq!(rules.min_lots(), 2);
    }

    #[test]
    fn lecture_toml() {
        let rules = InstrumentRules::from_toml_str(ExchangeId::Kraken, "BTC-USD", FICHIER).unwrap();
        assert_eq!(rules.tick_size, 0.1);
        assert_eq!(rules.lot_size, 1e-8);
        assert_eq!(rules.min_qty, 0.0);
        assert_eq!(rules.min_notional, 0.0);
        assert!(matches!(
            InstrumentRules::from_toml_str(ExchangeId::Kraken, "ETH-USD", FICHIER),
            Err(ModelError::InstrumentNotFound { .. })
        ));
        assert!(matches!(
            InstrumentRules::from_toml_str(ExchangeId::Okx, "BTC-USD", FICHIER),
            Err(ModelError::InstrumentNotFound { .. })
        ));
        let inconnu = "[kraken.BTC-USD]\ntick_size = 0.1\nlot_size = 1e-8\nmin_qty = 0.0\nmin_notional = 0.0\nautre = 1\n";
        assert!(matches!(
            InstrumentRules::from_toml_str(ExchangeId::Kraken, "BTC-USD", inconnu),
            Err(ModelError::ConfigToml(_))
        ));
        let invalide = "[kraken.BTC-USD]\ntick_size = 0.3\nlot_size = 1e-8\nmin_qty = 0.0\nmin_notional = 0.0\n";
        assert!(matches!(
            InstrumentRules::from_toml_str(ExchangeId::Kraken, "BTC-USD", invalide),
            Err(ModelError::InvalidInstrument {
                field: "tick_size",
                ..
            })
        ));
    }

    #[test]
    fn chemin_dans_l_erreur_de_load() {
        let chemin = std::path::Path::new("../config/instruments.toml");
        assert!(InstrumentRules::load(ExchangeId::Kraken, "BTC-USD", chemin).is_ok());
        match InstrumentRules::load(ExchangeId::Okx, "BTC-USDC", chemin) {
            Err(ModelError::InstrumentNotFound { path, .. }) => {
                assert!(path.ends_with("instruments.toml"), "{path}");
            }
            autre => panic!("{autre:?}"),
        }
        assert!(matches!(
            InstrumentRules::load(
                ExchangeId::Kraken,
                "BTC-USD",
                std::path::Path::new("absent.toml")
            ),
            Err(ModelError::ConfigRead { .. })
        ));
    }
}
