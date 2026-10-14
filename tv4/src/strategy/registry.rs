//! Registre des stratégies : un `match` explicite, sans plugin dynamique (docs/03 §6).

use super::{Strategy, StrategyError, sma_cross, validate_declaration};

/// Identifiants connus, pour le message d'erreur d'une stratégie inconnue.
pub const KNOWN_IDS: [&str; 1] = [sma_cross::ID];

/// Construit la stratégie `id` depuis son document TOML de paramètres.
///
/// `params` est le document entier, un `toml::Value::Table` obtenu par
/// `toml::from_str::<toml::Value>(&texte)` (surtout pas `texte.parse::<toml::Value>()`, qui échoue sur
/// un document). La lecture du fichier n'est pas faite ici : elle revient à `cli`, à `engine` et aux
/// tests. Une fonction `fn` est déjà `Fn + Send + Sync`, ce qui suffit pour la fabrique de rayon (1.5).
pub fn build(id: &str, params: &toml::Value) -> Result<Box<dyn Strategy>, StrategyError> {
    let strategy: Box<dyn Strategy> = match id {
        sma_cross::ID => Box::new(sma_cross::SmaCross::new(parse(sma_cross::ID, params)?)?),
        other => {
            return Err(StrategyError::UnknownStrategy {
                id: other.to_string(),
                known: KNOWN_IDS.join(", "),
            });
        }
    };
    validate_declaration(strategy.as_ref())?;
    Ok(strategy)
}

fn parse<T: serde::de::DeserializeOwned>(
    id: &'static str,
    params: &toml::Value,
) -> Result<T, StrategyError> {
    params
        .clone()
        .try_into::<T>()
        .map_err(|source| StrategyError::ParamsToml { id, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Timeframe, hash};
    use crate::strategy::{IndicatorKind, IndicatorSpec};

    const CONFIG: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../config/strategies/sma_cross.toml"
    );

    fn value(text: &str) -> toml::Value {
        toml::from_str::<toml::Value>(text).unwrap()
    }

    fn reference() -> toml::Value {
        value(&std::fs::read_to_string(CONFIG).unwrap())
    }

    fn params(fields: &str) -> toml::Value {
        value(fields)
    }

    #[test]
    fn construit_sma_cross_depuis_le_fichier_de_config() {
        let strategy = build("sma_cross", &reference()).unwrap();
        assert_eq!(strategy.id(), "sma_cross");
        assert_eq!(strategy.version(), "1.0.0");
        assert_eq!(strategy.timeframes(), [Timeframe::M15]);
        assert_eq!(
            strategy.indicators(),
            [
                IndicatorSpec {
                    name: "fast",
                    tf: Timeframe::M15,
                    kind: IndicatorKind::Sma { period: 20 }
                },
                IndicatorSpec {
                    name: "slow",
                    tf: Timeframe::M15,
                    kind: IndicatorKind::Sma { period: 50 }
                },
                IndicatorSpec {
                    name: "atr",
                    tf: Timeframe::M15,
                    kind: IndicatorKind::Atr { period: 14 }
                },
            ]
        );
        assert_eq!(strategy.warmup_bars(), 50);
    }

    #[test]
    fn params_json_est_canonique_et_stable() {
        let strategy = build("sma_cross", &reference()).unwrap();
        assert_eq!(
            hash::canonical_json(&strategy.params_json()).unwrap(),
            r#"{"fast":20,"slow":50,"stop_atr_mult":2.0,"tf":"15m"}"#
        );
        // Clés permutées : même hash.
        let permuted = params("stop_atr_mult = 2.0\nslow = 50\ntf = \"15m\"\nfast = 20");
        let other = build("sma_cross", &permuted).unwrap();
        assert_eq!(
            hash::hash_json(&strategy.params_json()).unwrap(),
            hash::hash_json(&other.params_json()).unwrap()
        );
        // Entier 2 ou flottant 2.0 : même hash.
        let integer = params("tf = \"15m\"\nfast = 20\nslow = 50\nstop_atr_mult = 2");
        let third = build("sma_cross", &integer).unwrap();
        assert_eq!(
            hash::hash_json(&strategy.params_json()).unwrap(),
            hash::hash_json(&third.params_json()).unwrap()
        );
    }

    #[test]
    fn strategie_inconnue() {
        let error = build("inconnue", &reference()).err().unwrap();
        assert!(matches!(error, StrategyError::UnknownStrategy { .. }));
        let message = error.to_string();
        assert!(
            message.contains("inconnue") && message.contains("sma_cross"),
            "{message}"
        );
    }

    #[test]
    fn parametres_toml_invalides() {
        let cases = [
            (
                "champ manquant",
                params("tf = \"15m\"\nfast = 20\nslow = 50"),
            ),
            (
                "champ inconnu",
                params("tf = \"15m\"\nfast = 20\nslow = 50\nstop_atr_mult = 2.0\nextra = 1"),
            ),
            (
                "tf inconnu",
                params("tf = \"2h\"\nfast = 20\nslow = 50\nstop_atr_mult = 2.0"),
            ),
            (
                "fast décimal",
                params("tf = \"15m\"\nfast = 20.5\nslow = 50\nstop_atr_mult = 2.0"),
            ),
            (
                "fast négatif",
                params("tf = \"15m\"\nfast = -1\nslow = 50\nstop_atr_mult = 2.0"),
            ),
            ("pas une table", toml::Value::Integer(3)),
        ];
        for (label, document) in cases {
            let error = build("sma_cross", &document).err().unwrap();
            assert!(
                matches!(error, StrategyError::ParamsToml { .. }),
                "{label} : {error:?}"
            );
        }
    }

    #[test]
    fn parametres_incoherents() {
        let cases = [
            "fast = 0\nslow = 50\nstop_atr_mult = 2.0",
            "fast = 50\nslow = 50\nstop_atr_mult = 2.0",
            "fast = 60\nslow = 50\nstop_atr_mult = 2.0",
            "fast = 20\nslow = 100001\nstop_atr_mult = 2.0",
            "fast = 20\nslow = 50\nstop_atr_mult = nan",
            "fast = 20\nslow = 50\nstop_atr_mult = inf",
            "fast = 20\nslow = 50\nstop_atr_mult = 0.0",
            "fast = 20\nslow = 50\nstop_atr_mult = -1.0",
        ];
        for fields in cases {
            let document = params(&format!("tf = \"15m\"\n{fields}"));
            let error = build("sma_cross", &document).err().unwrap();
            assert!(
                matches!(error, StrategyError::InvalidParams { .. }),
                "{fields} : {error:?}"
            );
        }
    }
}
