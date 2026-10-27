//! Configuration d'une cellule de backtest et objet haché du `config_hash` (docs/03-architecture.md
//! §5.3).

use serde::Serialize;

use crate::model::{FeeSchedule, InstrumentRules, ModelError, Slippage, hash};
use crate::sim::SimRules;
use crate::strategy::Strategy;

/// Nombre minimal de trades en dessous duquel le verdict est `rejected_too_few_trades` (docs/03 §5.3).
pub const DEFAULT_MIN_TRADES: u32 = 30;

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
    /// Tick, lot et minimums de l'instrument (docs/04 §3.1 point 6) : dans le `config_hash`.
    pub instrument: InstrumentRules,
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
/// instrument, min_trades, warmup_bars, tv4_version }` (composition définitive depuis S8).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunConfig {
    pub strategy: StrategyRecord,
    pub fees: FeeSchedule,
    pub slippage_bps: Slippage,
    pub initial_cash: f64,
    pub warmup_bars: usize,
    pub tv4_version: &'static str,
    pub sim_rules: SimRules,
    pub instrument: InstrumentRules,
    /// Seuil d'effectif du verdict (S8).
    pub min_trades: u32,
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
            instrument: config.instrument,
            min_trades: DEFAULT_MIN_TRADES,
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
            instrument: InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).unwrap(),
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
    fn les_regles_et_l_instrument_changent_le_hash() {
        let s = build(BASE);
        let base = config(5.0, 300.0);
        let ref_hash = hash_of(s.as_ref(), &base);
        let mut autre = base;
        autre.sim_rules.activation_delay_bars = 0;
        assert_ne!(ref_hash, hash_of(s.as_ref(), &autre));
        let mut autre = base;
        autre.sim_rules.max_pending = 32;
        assert_ne!(ref_hash, hash_of(s.as_ref(), &autre));
        let mut autre = base;
        autre.sim_rules.intrabar = crate::sim::IntrabarRule::TpFirst;
        assert_ne!(ref_hash, hash_of(s.as_ref(), &autre));
        let mut autre = base;
        autre.sim_rules.stop_stress_bps = 10.0;
        assert_ne!(ref_hash, hash_of(s.as_ref(), &autre));
        let mut autre = base;
        autre.instrument = InstrumentRules::new(0.01, 1e-8, 0.0, 0.0).unwrap();
        assert_ne!(ref_hash, hash_of(s.as_ref(), &autre));
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
        assert!(json.contains(r#""min_trades":30"#), "{json}");
        assert!(json.contains(r#""version":"s6.1""#), "{json}");
        assert!(json.contains(r#""activation_delay_bars":1"#), "{json}");
        assert!(json.contains(r#""intrabar":"stop_first""#), "{json}");
        assert!(json.contains(r#""max_volume_share":0.005"#), "{json}");
        assert!(json.contains(r#""stop_stress_bps":0.0"#), "{json}");
        assert!(
            json.contains(
                r#""instrument":{"lot_size":1e-8,"min_notional":0.0,"min_qty":0.0,"tick_size":0.1}"#
            ),
            "{json}"
        );
    }

    /// JSON canonique complet de `sma_cross` 1m avec les défauts du CLI (docs S6 §B.6.4), et son hash.
    /// Le hash est calculé par le code du dépôt sur cette chaîne ; ce n'est pas une sortie du simulateur.
    #[test]
    fn le_json_canonique_1m_complet_et_son_hash() {
        let s = build(BASE);
        let run = RunConfig::new(s.as_ref(), &config(5.0, 300.0), s.warmup_bars());
        let json = hash::canonical_json(&run).unwrap();
        assert_eq!(
            json,
            concat!(
                r#"{"fees":{"exchange":"okx","maker_bps":20,"taker_bps":35},"initial_cash":300.0,"#,
                r#""instrument":{"lot_size":1e-8,"min_notional":0.0,"min_qty":0.0,"tick_size":0.1},"#,
                r#""min_trades":30,"sim_rules":{"activation_delay_bars":1,"intrabar":"stop_first","limit_cross_ticks":1,"#,
                r#""market_buy_cash_margin":0.05,"max_pending":64,"max_volume_share":0.005,"#,
                r#""stop_stress_bps":0.0,"tp_ignored_on_fill_bar":true,"version":"s6.1","#,
                r#""zero_volume_no_fill":true},"slippage_bps":5.0,"strategy":{"id":"sma_cross","#,
                r#""params":{"fast":60,"slow":240,"stop_atr_mult":2.0,"tf":"1m"},"version":"1.1.0"},"#,
                r#""tv4_version":"0.1.0","warmup_bars":240}"#
            )
        );
        // S8 : `"min_trades":30` s'insère entre `instrument` et `sim_rules`. Valeur recoupée avec le
        // module Python `blake3` ; sans cette clé la chaîne redonne `6ea48993…` (valeur de S6).
        assert_eq!(
            run.hash().unwrap(),
            "00fbee51dfb4bad5be96083449df30220e155a5d14cf1f29944fa58e3e046cc5"
        );
    }
}
