//! Configuration d'une cellule de backtest et objet haché du `config_hash` (docs/03-architecture.md
//! §5.3).

use serde::Serialize;

use crate::model::{FeeSchedule, ModelError, Slippage, hash};
use crate::sim::SimRules;
use crate::strategy::Strategy;

/// Paramètres d'une cellule : tout ce que `run_cell` reçoit en dehors de la stratégie et des données.
#[derive(Clone, Copy, Debug)]
pub struct CellConfig<'a> {
    /// Symbole canonique des données (`BTC-USD`). Il décrit les données, pas le run : il n'entre pas
    /// dans le `config_hash`.
    pub symbol: &'a str,
    pub fees: FeeSchedule,
    pub slippage: Slippage,
    /// Capital initial, en monnaie de cotation des données.
    pub initial_cash: f64,
    pub sim_rules: SimRules,
}

/// Stratégie telle que le manifeste la décrit : identifiant, version et paramètres.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StrategyRecord {
    pub id: &'static str,
    pub version: &'static str,
    pub params: serde_json::Value,
}

impl StrategyRecord {
    pub fn of(strategy: &dyn Strategy) -> StrategyRecord {
        StrategyRecord {
            id: strategy.id(),
            version: strategy.version(),
            params: strategy.params_json(),
        }
    }
}

/// Objet haché du `config_hash` : `{ strategy, fees, slippage_bps, initial_cash, sim_rules,
/// warmup_bars, tv4_version }`.
///
/// La composition est provisoire jusqu'à S8 (`min_trades` arrivera avec le verdict ; aucun run n'est
/// encore archivé).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunConfig {
    pub strategy: StrategyRecord,
    pub fees: FeeSchedule,
    pub slippage_bps: Slippage,
    pub initial_cash: f64,
    pub warmup_bars: usize,
    pub tv4_version: &'static str,
    pub sim_rules: SimRules,
}

impl RunConfig {
    pub fn new(strategy: &dyn Strategy, config: &CellConfig<'_>, warmup_bars: usize) -> RunConfig {
        RunConfig {
            strategy: StrategyRecord::of(strategy),
            fees: config.fees,
            slippage_bps: config.slippage,
            initial_cash: config.initial_cash,
            warmup_bars,
            tv4_version: env!("CARGO_PKG_VERSION"),
            sim_rules: config.sim_rules,
        }
    }

    /// Hash blake3 hexadécimal du JSON canonique de la configuration.
    pub fn hash(&self) -> Result<String, ModelError> {
        hash::hash_json(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ExchangeId;
    use crate::strategy::registry;

    fn build(text: &str) -> Box<dyn Strategy> {
        let value = toml::from_str::<toml::Value>(text).unwrap();
        registry::build("sma_cross", &value).unwrap()
    }

    fn config(slippage: f64, cash: f64) -> CellConfig<'static> {
        CellConfig {
            symbol: "BTC-USD",
            fees: FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
            slippage: Slippage::from_bps(slippage).unwrap(),
            initial_cash: cash,
            sim_rules: SimRules::default(),
        }
    }

    fn hash_of(strategy: &dyn Strategy, config: &CellConfig<'_>) -> String {
        RunConfig::new(strategy, config, strategy.warmup_bars())
            .hash()
            .unwrap()
    }

    const BASE: &str = "tf = \"1m\"\nfast = 60\nslow = 240\nstop_atr_mult = 2.0";

    #[test]
    fn le_hash_est_stable_par_permutation_des_parametres() {
        let a = build(BASE);
        let b = build("stop_atr_mult = 2\nslow = 240\nfast = 60\ntf = \"1m\"");
        let c = config(5.0, 300.0);
        assert_eq!(hash_of(a.as_ref(), &c), hash_of(b.as_ref(), &c));
    }

    #[test]
    fn slippage_et_capital_changent_le_hash() {
        let s = build(BASE);
        let ref_hash = hash_of(s.as_ref(), &config(5.0, 300.0));
        assert_ne!(ref_hash, hash_of(s.as_ref(), &config(6.0, 300.0)));
        assert_ne!(ref_hash, hash_of(s.as_ref(), &config(5.0, 301.0)));
        assert_eq!(ref_hash, hash_of(s.as_ref(), &config(5.0, 300.0)));
    }

    #[test]
    fn les_parametres_de_la_strategie_changent_le_hash() {
        let a = build(BASE);
        let b = build("tf = \"1m\"\nfast = 61\nslow = 240\nstop_atr_mult = 2.0");
        let c = config(5.0, 300.0);
        assert_ne!(hash_of(a.as_ref(), &c), hash_of(b.as_ref(), &c));
    }

    #[test]
    fn le_json_canonique_porte_les_champs_attendus() {
        let s = build(BASE);
        let json =
            hash::canonical_json(&RunConfig::new(s.as_ref(), &config(5.0, 300.0), 240)).unwrap();
        assert!(json.contains(r#""slippage_bps":5.0"#), "{json}");
        assert!(json.contains(r#""initial_cash":300.0"#), "{json}");
        assert!(json.contains(r#""warmup_bars":240"#), "{json}");
        assert!(json.contains(r#""stops":"inactive""#), "{json}");
        assert!(json.contains(r#""version":"s3-market-only""#), "{json}");
    }
}
