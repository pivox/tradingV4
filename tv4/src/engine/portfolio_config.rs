//! Configuration du mode portefeuille et objet haché de son `config_hash` (docs S9 §11).
//!
//! [`PortfolioRunConfig`] est le pendant de [`RunConfig`](super::RunConfig) pour `run_portfolio`. Trois
//! différences :
//!
//! 1. **Le capital initial n'y est pas** (décision du propriétaire du 28/10, 13:52 UTC, carte « Hors du
//!    hash », docs/04 §4.2) : un même `config_hash` donne les mêmes trades à 300 € et à 3 000 €. Le
//!    capital est écrit à part (`metrics.config.initial_cash`, manifeste) et `tv4 verify` exige des
//!    capitaux égaux. `PortfolioRunConfig` n'a donc aucun champ de capital : si le propriétaire
//!    revenait sur ce choix (capital dans le hash, plus un second hash sans capital), seul ce fichier et
//!    [`PortfolioResult::config_hash`](super::PortfolioResult) changeraient.
//! 2. Le champ `schema` (`tv4-portfolio-config-v1`) distingue ce JSON de celui de `RunConfig` : un hash
//!    de portefeuille ne peut jamais égaler un hash S8.
//! 3. Le bloc `portfolio` porte l'univers **dans l'ordre donné** (départage des rangs égaux, docs S9
//!    §5.1), N, le mode de sizing, le trou de cotation, la règle de priorité et, quand une stratégie lit
//!    le régime, la paire de régime.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::{FeeSchedule, InstrumentRules, ModelError, Slippage, Timeframe, Ts, hash};
use crate::sim::{SimRules, SizingMode};
use crate::strategy::{REGIME_SMA_PERIOD, REGIME_TF, Strategy};

use super::config::StrategyRecord;

/// Identifiant du format de l'objet haché.
pub const PORTFOLIO_SCHEMA: &str = "tv4-portfolio-config-v1";
/// Règle de priorité des signaux d'une même minute : le rang de liquidité 90 j du jour UTC, puis l'ordre
/// de l'univers, jamais l'alphabet (docs/04 §3.1 point 5).
pub const PRIORITY_RULE: &str = "signal_time_then_liquidity_90d";
/// Fenêtre du rang de liquidité, en jours complets avant le jour courant.
pub const LIQUIDITY_WINDOW_DAYS: u32 = 90;
/// Nombre de positions simultanées par défaut (docs/04 §3.1 point 5).
pub const DEFAULT_MAX_POSITIONS: u16 = 8;
/// Trou de cotation par défaut : 7 jours, le seuil de continuité de l'univers (docs/04 §2.2).
pub const DEFAULT_QUOTE_GAP_MS: Ts = 7 * 86_400_000;
/// Nombre maximal de positions simultanées accepté par le moteur.
pub const MAX_POSITIONS_LIMIT: u16 = 64;

/// Règle `min_order_bump` telle que le hash la décrit (docs S9 §7.3).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct MinOrderBump {
    pub max_risk_pct: f64,
}

/// Paire de régime telle que le hash la décrit : symbole, période de la SMA et timeframe.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RegimeRecord {
    pub symbol: String,
    pub sma_period: usize,
    pub tf: Timeframe,
}

/// Bloc `portfolio` de l'objet haché.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PortfolioBlock {
    pub max_positions: u16,
    pub min_order_bump: Option<MinOrderBump>,
    pub priority: &'static str,
    pub liquidity_window_days: u32,
    pub quote_gap_ms: Ts,
    /// `None` (`null`) quand aucune stratégie ne lit le régime.
    pub regime: Option<RegimeRecord>,
    pub sizing: SizingMode,
    /// L'ordre donné par la configuration, non trié.
    pub universe: Vec<String>,
}

/// Objet haché du `config_hash` du mode portefeuille. **Sans capital initial.**
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PortfolioRunConfig {
    pub schema: &'static str,
    pub strategy: StrategyRecord,
    pub fees: FeeSchedule,
    pub slippage_bps: Slippage,
    pub warmup_bars: usize,
    pub tv4_version: &'static str,
    pub sim_rules: SimRules,
    /// Règles d'instrument par symbole de l'univers (clés triées par le JSON canonique).
    pub instruments: BTreeMap<String, InstrumentRules>,
    /// Hash du fichier d'instruments OKX, `null` sans fichier.
    pub instruments_hash: Option<String>,
    pub min_trades: u32,
    pub portfolio: PortfolioBlock,
}

impl PortfolioRunConfig {
    /// Hash blake3 hexadécimal du JSON canonique de la configuration.
    pub fn hash(&self) -> Result<String, ModelError> {
        hash::hash_json(self)
    }

    /// JSON canonique (clés triées) de l'objet haché : la chaîne dont le hash est tiré.
    pub fn canonical_json(&self) -> Result<String, ModelError> {
        hash::canonical_json(self)
    }
}

/// Paramètres d'un run portefeuille : tout ce que `run_portfolio` reçoit en dehors des stratégies et des
/// données.
#[derive(Clone, Debug)]
pub struct PortfolioConfig<'a> {
    /// Univers tradé, dans l'ordre de la configuration (départage des rangs égaux).
    pub universe: &'a [String],
    /// Paire de régime (défaut de la CLI : `BTC-USD`). Lue seulement si une stratégie a `uses_regime()`.
    pub regime_symbol: &'a str,
    pub fees: FeeSchedule,
    pub slippage: Slippage,
    /// Capital initial, **hors** du `config_hash` : il est écrit à part dans les rapports.
    pub initial_cash: f64,
    pub sim_rules: SimRules,
    /// Règles d'instrument de chaque symbole de l'univers, dans le même ordre.
    pub instruments: &'a [InstrumentRules],
    /// Hash du fichier d'instruments OKX (`None` sans fichier).
    pub instruments_hash: Option<&'a str>,
    /// Nombre maximal de positions ouvertes plus entrées en attente (1 à [`MAX_POSITIONS_LIMIT`]).
    pub max_positions: u16,
    pub sizing: SizingMode,
    /// Règle `min_order_bump` (risque effectif maximal en part de la base), mode capitalisé seulement.
    pub min_order_bump_max_risk: Option<f64>,
    /// Trou de cotation au-delà duquel une jambe en position sort de force, en ms (> 0).
    pub quote_gap_ms: Ts,
    /// Seuil d'effectif du verdict.
    pub min_trades: u32,
}

impl PortfolioRunConfig {
    /// Construit l'objet haché. `uses_regime` dit si au moins une stratégie lit le régime : le bloc
    /// `regime` n'est écrit que dans ce cas.
    pub fn new(
        strategy: &dyn Strategy,
        config: &PortfolioConfig<'_>,
        warmup_bars: usize,
        uses_regime: bool,
    ) -> PortfolioRunConfig {
        let mut instruments = BTreeMap::new();
        for (symbol, rules) in config.universe.iter().zip(config.instruments) {
            instruments.insert(symbol.clone(), *rules);
        }
        PortfolioRunConfig {
            schema: PORTFOLIO_SCHEMA,
            strategy: StrategyRecord::of(strategy),
            fees: config.fees,
            slippage_bps: config.slippage,
            warmup_bars,
            tv4_version: env!("CARGO_PKG_VERSION"),
            sim_rules: config.sim_rules,
            instruments,
            instruments_hash: config.instruments_hash.map(str::to_string),
            min_trades: config.min_trades,
            portfolio: PortfolioBlock {
                max_positions: config.max_positions,
                min_order_bump: config
                    .min_order_bump_max_risk
                    .map(|max_risk_pct| MinOrderBump { max_risk_pct }),
                priority: PRIORITY_RULE,
                liquidity_window_days: LIQUIDITY_WINDOW_DAYS,
                quote_gap_ms: config.quote_gap_ms,
                regime: uses_regime.then(|| RegimeRecord {
                    symbol: config.regime_symbol.to_string(),
                    sma_period: REGIME_SMA_PERIOD,
                    tf: REGIME_TF,
                }),
                sizing: config.sizing,
                universe: config.universe.to_vec(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ExchangeId;
    use crate::strategy::registry;

    const BASE: &str = "tf = \"1m\"\nfast = 60\nslow = 240\nstop_atr_mult = 2.0";

    fn strategy(text: &str) -> Box<dyn Strategy> {
        let value = toml::from_str::<toml::Value>(text).unwrap();
        registry::build("sma_cross", &value).unwrap()
    }

    fn universe() -> Vec<String> {
        vec!["BTC-USD".to_string(), "ETH-USD".to_string()]
    }

    fn rules() -> Vec<InstrumentRules> {
        vec![
            InstrumentRules::new(0.1, 1e-8, 0.0001, 0.0).unwrap(),
            InstrumentRules::new(0.01, 1e-8, 0.001, 0.0).unwrap(),
        ]
    }

    fn config<'a>(universe: &'a [String], rules: &'a [InstrumentRules]) -> PortfolioConfig<'a> {
        PortfolioConfig {
            universe,
            regime_symbol: "BTC-USD",
            fees: FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
            slippage: Slippage::from_bps(5.0).unwrap(),
            initial_cash: 300.0,
            sim_rules: SimRules::default(),
            instruments: rules,
            instruments_hash: None,
            max_positions: DEFAULT_MAX_POSITIONS,
            sizing: SizingMode::Research,
            min_order_bump_max_risk: None,
            quote_gap_ms: DEFAULT_QUOTE_GAP_MS,
            min_trades: 30,
        }
    }

    fn hash_of(config: &PortfolioConfig<'_>, uses_regime: bool) -> String {
        let s = strategy(BASE);
        PortfolioRunConfig::new(s.as_ref(), config, s.warmup_bars(), uses_regime)
            .hash()
            .unwrap()
    }

    /// JSON canonique complet de la configuration de référence (sizing de recherche, N = 8, trou de
    /// 7 jours, deux paires, pas de fichier OKX, pas de régime), et son hash. Les valeurs sont produites
    /// par le code du dépôt au moment de l'implémentation de S9 ; ce ne sont pas des sorties du
    /// simulateur sur des données de marché.
    #[test]
    fn le_json_canonique_de_reference_et_son_hash() {
        let universe = universe();
        let rules = rules();
        let s = strategy(BASE);
        let run = PortfolioRunConfig::new(
            s.as_ref(),
            &config(&universe, &rules),
            s.warmup_bars(),
            false,
        );
        assert_eq!(run.canonical_json().unwrap(), REFERENCE_JSON);
        assert_eq!(run.hash().unwrap(), REFERENCE_HASH);
    }

    /// Hash recoupé avec le module Python `blake3` sur cette chaîne.
    const REFERENCE_HASH: &str = "36bd64fdd20e6ca6b040c535a358fa7721d8a90bcd50a0a6c5cb08d912f55b79";
    const REFERENCE_JSON: &str = concat!(
        r#"{"fees":{"exchange":"okx","maker_bps":20,"taker_bps":35},"#,
        r#""instruments":{"BTC-USD":{"lot_size":1e-8,"min_notional":0.0,"min_qty":0.0001,"tick_size":0.1},"#,
        r#""ETH-USD":{"lot_size":1e-8,"min_notional":0.0,"min_qty":0.001,"tick_size":0.01}},"#,
        r#""instruments_hash":null,"min_trades":30,"#,
        r#""portfolio":{"liquidity_window_days":90,"max_positions":8,"min_order_bump":null,"#,
        r#""priority":"signal_time_then_liquidity_90d","quote_gap_ms":604800000,"regime":null,"#,
        r#""sizing":"research","universe":["BTC-USD","ETH-USD"]},"schema":"tv4-portfolio-config-v1","#,
        r#""sim_rules":{"activation_delay_bars":1,"intrabar":"stop_first","limit_cross_ticks":1,"#,
        r#""market_buy_cash_margin":0.05,"max_pending":64,"max_volume_share":0.005,"#,
        r#""stop_stress_bps":0.0,"tp_ignored_on_fill_bar":true,"version":"s6.1","#,
        r#""zero_volume_no_fill":true},"slippage_bps":5.0,"strategy":{"id":"sma_cross","#,
        r#""params":{"fast":60,"slow":240,"stop_atr_mult":2.0,"tf":"1m"},"version":"1.1.0"},"#,
        r#""tv4_version":"0.1.0","warmup_bars":240}"#
    );

    #[test]
    fn le_capital_n_entre_pas_dans_le_hash() {
        let universe = universe();
        let rules = rules();
        let base = config(&universe, &rules);
        let reference = hash_of(&base, false);
        for capital in [3000.0, 1.0, 60.0] {
            let autre = PortfolioConfig {
                initial_cash: capital,
                ..base.clone()
            };
            assert_eq!(reference, hash_of(&autre, false), "capital {capital}");
        }
        // Le JSON canonique ne contient aucune trace du capital.
        let s = strategy(BASE);
        let json = PortfolioRunConfig::new(s.as_ref(), &base, s.warmup_bars(), false)
            .canonical_json()
            .unwrap();
        assert!(!json.contains("initial_cash"), "{json}");
    }

    #[test]
    fn chaque_autre_champ_change_le_hash() {
        let universe = universe();
        let rules = rules();
        let base = config(&universe, &rules);
        let reference = hash_of(&base, false);
        let swapped = vec!["ETH-USD".to_string(), "BTC-USD".to_string()];
        let swapped_rules = vec![rules[1], rules[0]];
        let other_rules = vec![
            InstrumentRules::new(0.1, 1e-8, 0.0002, 0.0).unwrap(),
            rules[1],
        ];
        let variants: Vec<(&str, PortfolioConfig<'_>)> = vec![
            (
                "fees",
                PortfolioConfig {
                    fees: FeeSchedule::new(ExchangeId::Okx, 10, 20).unwrap(),
                    ..base.clone()
                },
            ),
            (
                "slippage",
                PortfolioConfig {
                    slippage: Slippage::from_bps(6.0).unwrap(),
                    ..base.clone()
                },
            ),
            (
                "sim_rules",
                PortfolioConfig {
                    sim_rules: SimRules {
                        activation_delay_bars: 0,
                        ..SimRules::default()
                    },
                    ..base.clone()
                },
            ),
            (
                "instruments",
                PortfolioConfig {
                    instruments: &other_rules,
                    ..base.clone()
                },
            ),
            (
                "instruments_hash",
                PortfolioConfig {
                    instruments_hash: Some("blake3:abc"),
                    ..base.clone()
                },
            ),
            (
                "max_positions",
                PortfolioConfig {
                    max_positions: 7,
                    ..base.clone()
                },
            ),
            (
                "sizing",
                PortfolioConfig {
                    sizing: SizingMode::Compounding,
                    ..base.clone()
                },
            ),
            (
                "min_order_bump",
                PortfolioConfig {
                    sizing: SizingMode::Compounding,
                    min_order_bump_max_risk: Some(0.015),
                    ..base.clone()
                },
            ),
            (
                "quote_gap_ms",
                PortfolioConfig {
                    quote_gap_ms: 8 * 86_400_000,
                    ..base.clone()
                },
            ),
            (
                "min_trades",
                PortfolioConfig {
                    min_trades: 31,
                    ..base.clone()
                },
            ),
            (
                "ordre de l'univers",
                PortfolioConfig {
                    universe: &swapped,
                    instruments: &swapped_rules,
                    ..base.clone()
                },
            ),
        ];
        let mut seen = vec![reference.clone()];
        for (label, variant) in &variants {
            let h = hash_of(variant, false);
            assert!(
                !seen.contains(&h),
                "{label} ne change pas le hash (ou collision)"
            );
            seen.push(h);
        }
        // Le régime n'entre dans le hash que si une stratégie le lit, et son symbole y est.
        let with_regime = hash_of(&base, true);
        assert_ne!(reference, with_regime);
        let other_regime = PortfolioConfig {
            regime_symbol: "ETH-USD",
            ..base.clone()
        };
        assert_ne!(with_regime, hash_of(&other_regime, true));
        assert_eq!(reference, hash_of(&other_regime, false));
        // La stratégie et son paramétrage.
        let other = strategy("tf = \"1m\"\nfast = 61\nslow = 240\nstop_atr_mult = 2.0");
        let h = PortfolioRunConfig::new(other.as_ref(), &base, other.warmup_bars(), false)
            .hash()
            .unwrap();
        assert_ne!(reference, h);
    }

    #[test]
    fn un_hash_de_portefeuille_n_est_jamais_un_hash_s8() {
        let universe = vec!["BTC-USD".to_string()];
        let rules = vec![InstrumentRules::new(0.1, 1e-8, 0.0, 0.0).unwrap()];
        let s = strategy(BASE);
        let run = PortfolioRunConfig::new(
            s.as_ref(),
            &config(&universe, &rules),
            s.warmup_bars(),
            false,
        );
        assert!(run.canonical_json().unwrap().contains(PORTFOLIO_SCHEMA));
        assert_eq!(run.schema, "tv4-portfolio-config-v1");
    }
}
