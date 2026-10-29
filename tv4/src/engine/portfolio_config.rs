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
//!
//! # Deux générations (docs S10 §2 et §10)
//!
//! Le schéma `tv4-portfolio-config-v1` (chemin hérité) porte `fees` et `slippage_bps` de premier niveau et
//! n'a pas de bloc `costs` : son JSON canonique, donc ses hashes, n'ont pas bougé depuis S9. Le schéma
//! `tv4-portfolio-config-v2` (profils de coûts de S10, [`PortfolioRunConfig::with_costs`]) n'a ni `fees`
//! ni `slippage_bps` : les frais et les coûts sont dans le bloc `costs` ([`CostsHashed`]), qui porte le
//! profil, la grille nommée, la graine, le TF de décision, le mode d'entrée et les valeurs résolues de
//! chaque jambe. Les champs absents de chaque génération ne sont pas sérialisés (`None`).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::costs::{
    COSTS_RULES_VERSION, CostProfileName, EntryHashed, LegCostHashed, LegCostRecord,
};
use crate::model::{
    FeeGridId, FeeSchedule, InstrumentRules, ModelError, Slippage, Timeframe, Ts, hash,
};
use crate::sim::{SimRules, SizingMode};
use crate::strategy::{REGIME_SMA_PERIOD, REGIME_TF, Strategy};

use super::EngineError;
use super::config::StrategyRecord;

/// Identifiant du format de l'objet haché du chemin hérité.
pub const PORTFOLIO_SCHEMA: &str = "tv4-portfolio-config-v1";
/// Identifiant du format de l'objet haché des runs s10 (profils de coûts).
pub const PORTFOLIO_SCHEMA_V2: &str = "tv4-portfolio-config-v2";
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

/// Grille de frais telle que le hash la décrit : son nom, déduit de `PortfolioConfig::fees` (docs S10
/// §9.2), et les taux de la grille chargée.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FeeGridHashed {
    pub grid: FeeGridId,
    pub maker_bps: u32,
    pub taker_bps: u32,
}

/// Bloc `costs` de l'objet haché d'un run s10 (docs S10 §10). Il porte tout ce qui détermine les trades
/// côté coûts : profil, grille, graine, TF de décision et mode d'entrée, valeurs résolues par jambe.
/// Les **sources** des valeurs (`measured`, `default`, `fallback`) et `min_maker_entry_share` n'y sont
/// pas : ils ne changent aucun trade.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CostsHashed {
    pub entry: EntryHashed,
    pub fee_grid: FeeGridHashed,
    /// Valeurs résolues de chaque jambe, par symbole (clés triées par le JSON canonique).
    pub legs: BTreeMap<String, LegCostHashed>,
    pub profile: CostProfileName,
    pub rules_version: &'static str,
    pub seed: u64,
}

/// Objet haché du `config_hash` du mode portefeuille. **Sans capital initial.**
///
/// `fees` et `slippage_bps` n'existent que dans le schéma v1 (chemin hérité) ; `costs` n'existe que dans
/// le schéma v2 (s10). Un champ `None` n'est pas sérialisé : le JSON v1 est celui de S9, à l'octet.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PortfolioRunConfig {
    pub schema: &'static str,
    pub strategy: StrategyRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fees: Option<FeeSchedule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slippage_bps: Option<Slippage>,
    /// Profil de coûts de S10, `None` dans le chemin hérité.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub costs: Option<CostsHashed>,
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
            fees: Some(config.fees),
            slippage_bps: Some(config.slippage),
            costs: None,
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

    /// Objet haché d'un run s10 (docs S10 §10) : schéma v2, sans `fees` ni `slippage_bps`, avec le bloc
    /// `costs`.
    ///
    /// * `legs` : un [`LegCostRecord`] par jambe, dans l'ordre de l'univers, tous du même `profile` ;
    /// * le nom de la grille se déduit de `config.fees` (une seule source pour la grille, docs S10 §9.2),
    ///   les taux sont ceux de la grille chargée ;
    /// * `entry` est celui de la première jambe : toutes les jambes d'un même jeu de stratégies ont le
    ///   même TF de décision (le moteur le vérifie) et le même mode d'entrée.
    ///
    /// # Erreurs
    ///
    /// [`EngineError::NoFeeGrid`] si le lieu de `config.fees` n'a pas de grille nommée,
    /// [`EngineError::EmptyUniverse`] sans jambe.
    pub fn with_costs(
        strategy: &dyn Strategy,
        config: &PortfolioConfig<'_>,
        warmup_bars: usize,
        uses_regime: bool,
        seed: u64,
        profile: CostProfileName,
        legs: &[LegCostRecord],
    ) -> Result<PortfolioRunConfig, EngineError> {
        let grid = FeeGridId::from_legacy_exchange(config.fees.exchange).ok_or(
            EngineError::NoFeeGrid {
                exchange: config.fees.exchange,
            },
        )?;
        let entry = legs
            .first()
            .map(LegCostRecord::entry_hashed)
            .ok_or(EngineError::EmptyUniverse)?;
        let mut hashed_legs = BTreeMap::new();
        for leg in legs {
            hashed_legs.insert(leg.symbol.clone(), leg.hashed());
        }
        let mut run = PortfolioRunConfig::new(strategy, config, warmup_bars, uses_regime);
        run.schema = PORTFOLIO_SCHEMA_V2;
        run.fees = None;
        run.slippage_bps = None;
        run.costs = Some(CostsHashed {
            entry,
            fee_grid: FeeGridHashed {
                grid,
                maker_bps: config.fees.maker_bps,
                taker_bps: config.fees.taker_bps,
            },
            legs: hashed_legs,
            profile,
            rules_version: COSTS_RULES_VERSION,
            seed,
        });
        Ok(run)
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

    // -----------------------------------------------------------------------------------------
    // Génération s10 (schéma v2, bloc `costs`)
    // -----------------------------------------------------------------------------------------

    use crate::model::costs::{CostMeasures, CostsFile, resolve_leg};

    /// Copie figée des défauts de §4.1 : la référence v2 ne dépend pas de `config/costs.toml`, que la
    /// calibration du stress (§11.3) réécrit.
    fn costs_file() -> CostsFile {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/costs_c1.toml");
        CostsFile::load(&path).unwrap()
    }

    /// Valeurs résolues de chaque jambe de l'univers de test.
    fn legs_of(
        file: &CostsFile,
        measures: Option<&CostMeasures>,
        profile: CostProfileName,
        decision_tf: Timeframe,
        seed: u64,
    ) -> Vec<LegCostRecord> {
        universe()
            .iter()
            .map(|symbol| {
                resolve_leg(file, measures, profile, symbol, decision_tf, seed)
                    .unwrap()
                    .1
            })
            .collect()
    }

    /// Objet haché v2 de la stratégie de base, profil `profile`, graine 1, TF de décision 1m.
    fn run_v2(
        config: &PortfolioConfig<'_>,
        file: &CostsFile,
        profile: CostProfileName,
    ) -> PortfolioRunConfig {
        run_v2_with(
            config,
            &legs_of(file, None, profile, Timeframe::M1, 1),
            1,
            profile,
        )
    }

    fn run_v2_with(
        config: &PortfolioConfig<'_>,
        legs: &[LegCostRecord],
        seed: u64,
        profile: CostProfileName,
    ) -> PortfolioRunConfig {
        let s = strategy(BASE);
        PortfolioRunConfig::with_costs(
            s.as_ref(),
            config,
            s.warmup_bars(),
            false,
            seed,
            profile,
            legs,
        )
        .unwrap()
    }

    fn hash_v2(run: &PortfolioRunConfig) -> String {
        run.hash().unwrap()
    }

    /// Même configuration que `REFERENCE_JSON`, profil baseline, défauts de la copie figée
    /// `tests/fixtures/costs_c1.toml`, graine 1, TF de décision 1m. La chaîne et son hash sont
    /// produits par le code du dépôt (aucune donnée de marché) ; ce test, et non l'exemple de la spec,
    /// fait foi pour un recalcul en PHP. La section `costs` porte `rules_version`, la grille déduite
    /// de `config.fees` (`okx` → `okx_regular`), les valeurs résolues de chaque jambe et la graine ;
    /// `fees` et `slippage_bps` n'y sont plus.
    #[test]
    fn le_json_canonique_v2_de_reference_et_son_hash() {
        let universe = universe();
        let rules = rules();
        let file = costs_file();
        let run = run_v2(&config(&universe, &rules), &file, CostProfileName::Baseline);
        assert_eq!(run.canonical_json().unwrap(), REFERENCE_JSON_V2);
        assert_eq!(run.hash().unwrap(), REFERENCE_HASH_V2);
        assert_eq!(run.schema, "tv4-portfolio-config-v2");
        assert!(run.fees.is_none() && run.slippage_bps.is_none());
    }

    /// Hash recoupé avec le module Python `blake3` sur cette chaîne.
    const REFERENCE_HASH_V2: &str =
        "f1ab17a2291b8f5d4e4380fe134601b607e494d3fefae6f743eb97e6c89ca515";
    const REFERENCE_JSON_V2: &str = concat!(
        r#"{"costs":{"entry":{"decision_tf":"1m","mode":"taker"},"fee_grid":{"grid":"okx_regular","#,
        r#""maker_bps":20,"taker_bps":35},"legs":{"BTC-USD":{"adverse_selection_bps":0.0,"class":"major","#,
        r#""half_spread_bps":0.5,"market_slippage_bps":1.0,"stop_slippage_bps":{"p100":200.0,"p50":5.0,"#,
        r#""p95":50.0,"p99":200.0},"stress_drop":0.03,"stress_probability":0.03},"#,
        r#""ETH-USD":{"adverse_selection_bps":0.0,"class":"major","half_spread_bps":0.5,"#,
        r#""market_slippage_bps":1.0,"stop_slippage_bps":{"p100":200.0,"p50":5.0,"p95":50.0,"p99":200.0},"#,
        r#""stress_drop":0.03,"stress_probability":0.03}},"profile":"baseline","rules_version":"c1","#,
        r#""seed":1},"instruments":{"BTC-USD":{"lot_size":1e-8,"min_notional":0.0,"min_qty":0.0001,"#,
        r#""tick_size":0.1},"ETH-USD":{"lot_size":1e-8,"min_notional":0.0,"min_qty":0.001,"#,
        r#""tick_size":0.01}},"instruments_hash":null,"min_trades":30,"#,
        r#""portfolio":{"liquidity_window_days":90,"max_positions":8,"min_order_bump":null,"#,
        r#""priority":"signal_time_then_liquidity_90d","quote_gap_ms":604800000,"regime":null,"#,
        r#""sizing":"research","universe":["BTC-USD","ETH-USD"]},"schema":"tv4-portfolio-config-v2","#,
        r#""sim_rules":{"activation_delay_bars":1,"intrabar":"stop_first","limit_cross_ticks":1,"#,
        r#""market_buy_cash_margin":0.05,"max_pending":64,"max_volume_share":0.005,"stop_stress_bps":0.0,"#,
        r#""tp_ignored_on_fill_bar":true,"version":"s6.1","zero_volume_no_fill":true},"#,
        r#""strategy":{"id":"sma_cross","params":{"fast":60,"slow":240,"stop_atr_mult":2.0,"tf":"1m"},"#,
        r#""version":"1.1.0"},"tv4_version":"0.1.0","warmup_bars":240}"#
    );

    #[test]
    fn le_json_v1_ne_porte_aucune_trace_de_la_generation_s10() {
        let universe = universe();
        let rules = rules();
        let s = strategy(BASE);
        let run = PortfolioRunConfig::new(
            s.as_ref(),
            &config(&universe, &rules),
            s.warmup_bars(),
            false,
        );
        let json = run.canonical_json().unwrap();
        for absent in ["costs", "fee_grid", "okx_regular", "rules_version", "seed"] {
            assert!(!json.contains(absent), "{absent} dans {json}");
        }
        assert!(json.contains(r#""fees":{"exchange":"okx""#), "{json}");
        assert!(json.contains(r#""slippage_bps":5.0"#), "{json}");
        assert!(run.costs.is_none());
    }

    #[test]
    fn deux_profils_deux_config_hash() {
        let universe = universe();
        let rules = rules();
        let file = costs_file();
        let base = config(&universe, &rules);
        let hashes: Vec<String> = CostProfileName::ALL
            .iter()
            .map(|&profile| hash_v2(&run_v2(&base, &file, profile)))
            .collect();
        assert_eq!(hashes.len(), 3);
        for (i, a) in hashes.iter().enumerate() {
            for b in &hashes[i + 1..] {
                assert_ne!(a, b, "deux profils donnent le même config_hash");
            }
        }
        // Un profil n'est jamais le run hérité de même configuration.
        assert!(!hashes.contains(&hash_of(&base, false)));
        // Le profil est haché sous son nom.
        let json = run_v2(&base, &file, CostProfileName::Adverse)
            .canonical_json()
            .unwrap();
        assert!(json.contains(r#""profile":"adverse""#), "{json}");
        let json = run_v2(&base, &file, CostProfileName::Maker)
            .canonical_json()
            .unwrap();
        assert!(json.contains(r#""mode":"maker""#), "{json}");
        assert!(json.contains(r#""adverse_selection_bps":1.5"#), "{json}");
    }

    #[test]
    fn la_graine_les_valeurs_par_jambe_et_le_tf_de_decision_sont_hachees() {
        let universe = universe();
        let rules = rules();
        let file = costs_file();
        let base = config(&universe, &rules);
        let profile = CostProfileName::Maker;
        let legs = |tf, seed| legs_of(&file, None, profile, tf, seed);
        let reference = hash_v2(&run_v2_with(&base, &legs(Timeframe::D1, 1), 1, profile));
        // Graine.
        let autre = hash_v2(&run_v2_with(&base, &legs(Timeframe::D1, 2), 2, profile));
        assert_ne!(reference, autre, "graine");
        // TF de décision : il change aussi le TTL maker, mais il est haché sous son nom.
        let intraday = run_v2_with(&base, &legs(Timeframe::H4, 1), 1, profile);
        assert_ne!(reference, hash_v2(&intraday), "tf de décision");
        let json = intraday.canonical_json().unwrap();
        assert!(json.contains(r#""decision_tf":"4h""#), "{json}");
        assert!(json.contains(r#""ttl_minutes":60"#), "{json}");
        // Valeur résolue d'une jambe.
        let mut modifie = legs(Timeframe::D1, 1);
        modifie[1].half_spread_bps += 1.0;
        assert_ne!(
            reference,
            hash_v2(&run_v2_with(&base, &modifie, 1, profile)),
            "valeur par jambe"
        );
        // Décalage de la limit maker.
        let mut decale = file.clone();
        decale.profiles.maker.entry_offset_bps = 2.0;
        assert_ne!(
            reference,
            hash_v2(&run_v2_with(
                &base,
                &legs_of(&decale, None, profile, Timeframe::D1, 1),
                1,
                profile
            )),
            "entry_offset_bps"
        );
    }

    #[test]
    fn le_seuil_de_part_maker_les_sources_et_le_capital_ne_sont_pas_haches() {
        let universe = universe();
        let rules = rules();
        let file = costs_file();
        let base = config(&universe, &rules);
        let profile = CostProfileName::Maker;
        let reference = hash_v2(&run_v2(&base, &file, profile));
        // `min_maker_entry_share` ne change que l'étiquette.
        let mut autre_seuil = file.clone();
        autre_seuil.profiles.maker.min_maker_entry_share = 0.9;
        assert_eq!(reference, hash_v2(&run_v2(&base, &autre_seuil, profile)));
        // Le capital reste hors du hash.
        let autre_capital = PortfolioConfig {
            initial_cash: 3000.0,
            ..base.clone()
        };
        assert_eq!(reference, hash_v2(&run_v2(&autre_capital, &file, profile)));
        // Une mesure qui redonne exactement les défauts change les sources, pas les trades : même hash.
        let mesures = CostMeasures::from_json_str(
            r#"{"schema":"tv4-cost-measures-v1","venue":"okx","captured_from_ms":0,"captured_to_ms":172800000,
"bases":{"BTC":{"inst_id":"BTC-USDC","hours":48.0,"half_spread_bps":{"median":0.5,"p95":3.0},
"market_slippage_bps":{"median":1.0}}}}"#,
        )
        .unwrap();
        let mesure = legs_of(&file, Some(&mesures), profile, Timeframe::M1, 1);
        let defaut = legs_of(&file, None, profile, Timeframe::M1, 1);
        assert_ne!(mesure[0].spread_source, defaut[0].spread_source);
        assert_eq!(
            reference,
            hash_v2(&run_v2_with(&base, &mesure, 1, profile)),
            "les sources n'entrent pas dans le hash"
        );
        // Le JSON haché n'a pas de trace des sources, du seuil ni du capital.
        let json = run_v2(&base, &file, profile).canonical_json().unwrap();
        for absent in [
            "measured",
            "fallback",
            "min_maker_entry_share",
            "initial_cash",
            "class_source",
        ] {
            assert!(!json.contains(absent), "{absent} dans {json}");
        }
    }

    #[test]
    fn la_grille_hachee_se_deduit_de_config_fees() {
        let universe = universe();
        let rules = rules();
        let file = costs_file();
        let okx = config(&universe, &rules);
        let json = run_v2(&okx, &file, CostProfileName::Baseline)
            .canonical_json()
            .unwrap();
        assert!(
            json.contains(r#""fee_grid":{"grid":"okx_regular","maker_bps":20,"taker_bps":35}"#),
            "{json}"
        );
        let bitvavo = PortfolioConfig {
            fees: FeeSchedule::new(ExchangeId::BitvavoUsdc, 15, 25).unwrap(),
            ..okx.clone()
        };
        let json = run_v2(&bitvavo, &file, CostProfileName::Baseline)
            .canonical_json()
            .unwrap();
        assert!(
            json.contains(r#""fee_grid":{"grid":"bitvavo_usdc","maker_bps":15,"taker_bps":25}"#),
            "{json}"
        );
        // Un lieu sans grille nommée est refusé, jamais deviné.
        let kraken = PortfolioConfig {
            fees: FeeSchedule::new(ExchangeId::Kraken, 25, 40).unwrap(),
            ..okx.clone()
        };
        let s = strategy(BASE);
        let legs = legs_of(&file, None, CostProfileName::Baseline, Timeframe::M1, 1);
        let refus = PortfolioRunConfig::with_costs(
            s.as_ref(),
            &kraken,
            s.warmup_bars(),
            false,
            1,
            CostProfileName::Baseline,
            &legs,
        );
        assert!(matches!(
            refus,
            Err(EngineError::NoFeeGrid {
                exchange: ExchangeId::Kraken
            })
        ));
        // Sans jambe, pas d'objet haché.
        let sans_jambe = PortfolioRunConfig::with_costs(
            s.as_ref(),
            &okx,
            s.warmup_bars(),
            false,
            1,
            CostProfileName::Baseline,
            &[],
        );
        assert!(matches!(sans_jambe, Err(EngineError::EmptyUniverse)));
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
