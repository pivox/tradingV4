//! Moteur de backtest multi-timeframe (docs/03-architecture.md §2, §7.1 et §7.2).
//!
//! [`run_cell`] est une fonction pure : ni `static mut`, ni `thread_local!`, ni horloge, ni écriture
//! (c'est le rôle de `report`). Une cellule prend une stratégie fraîche construite par le registre,
//! agrège le 1m vers les timeframes qu'elle déclare ([`crate::mtf`]), calcule les colonnes
//! d'indicateurs de chaque timeframe hors boucle, puis déroule la boucle 1m sans allocation.
//!
//! Écarts d'implémentation (S3) à docs/03 :
//!
//! 1. `run_cell(&[Candle], Box<dyn Strategy>, &CellConfig)` au lieu de
//!    `(&Dataset, &IndicatorCache, &CellConfig)`, jusqu'à S4 et au jalon 1.5. Le crochet
//!    [`LoopObserver`], appelé deux fois par run, sert au test de l'allocateur et servira à `tv4
//!    bench` (S8), qui chronométrera la boucle de l'extérieur.
//! 2. Convention du warmup : les bougies `[0, warmup_bars)` précèdent `from`, la première bougie
//!    jouée est l'index `warmup_bars`, et le refus de démarrer porte sur `colonne[warmup_bars]` non
//!    finie.
//! 3. `BarState.equity = cash + qty × close[i]`, la clôture de la 1m courante (étape 3, même
//!    instant).
//! 4. Les cinq écarts de `sim` encore ouverts après S6 (`SIM_DEVIATIONS`, voir la doc de ce module) ;
//!    `single_tf_1m` est levé en S5. La clôture forcée est comptée comme un trade (`n_trades`), avec
//!    `forced_close` et `n_trades_forced`, et son PnL est isolé dans `pnl_marked`.
//! 5. Depuis S6, `ctx.pending()` rend les ordres en attente du simulateur (copie dans un tampon alloué
//!    avant la boucle) et `position.stop` le stop en vigueur : le stop est actif.
//!
//! Ajouts de S8 (sans effet sur un fill, un prix ni un compteur du simulateur) :
//!
//! 1. `RunConfig.min_trades` (défaut [`DEFAULT_MIN_TRADES`]) entre dans le `config_hash` ;
//!    [`CellResult::with_min_trades`] le remplace et recalcule le hash. `CellConfig` ne change pas.
//! 2. [`EngineFunnel`] : compteurs d'évaluations et de signaux par classe (entrée, sortie, autre),
//!    acceptés et refusés par motif ; `CellResult.bars_in_position` compte les 1m où la position est
//!    non nulle après `Simulator::on_bar`. La somme des rejets des classes redonne `rejections` (S6).
//! 3. `Strategy::funnel()` est lu une fois après la boucle.
//!
//! Ajouts de S9 (portefeuille multi-paires, docs S9) :
//!
//! 1. [`run_portfolio`] rejoue plusieurs paires sur un cash commun ([`Book`](crate::sim::Book)), une
//!    stratégie par paire, sur la grille union des minutes des paires. Le chemin de [`run_cell`] est
//!    inchangé : le golden de S8 reste identique à l'octet. Sans `--symbols`, la CLI suit toujours
//!    `run_cell`.
//! 2. [`PortfolioRunConfig`] est l'objet haché du mode portefeuille : **sans capital initial** (un même
//!    `config_hash` donne les mêmes trades à 300 € et à 3 000 €, décision du 28/10).
//! 3. [`RankTable`] (priorité : horodatage, puis rang de liquidité 90 j, jamais l'alphabet) et la série
//!    de régime BTC 1d / SMA200 exposée aux stratégies par `BarContext::regime`.
//! 4. `run_cell` refuse une stratégie qui lit le régime ([`EngineError::RegimeNeedsPortfolio`]).
//!
//! Ajouts de S10 (profils de coûts, docs S10 §9 et §10) :
//!
//! 1. [`run_portfolio_profiles`] joue baseline, adverse et maker sur une préparation construite une fois
//!    (bougies, `Mtf`, colonnes d'indicateurs, série de régime, rang), chaque profil ayant son `Book`, ses
//!    stratégies et son curseur de régime. [`PrepStats`] est compté par l'appelant de la préparation.
//! 2. [`PortfolioRunConfig`] porte une seconde génération (`tv4-portfolio-config-v2`, bloc `costs`) ; le
//!    JSON v1 du chemin hérité est inchangé à l'octet.
//! 3. `Strategy::decision_tf()` (défaut : le plus grand TF déclaré) fixe le TTL maker et entre dans le hash
//!    v2 seulement.
//!
//! Ajouts de S11 (mode recherche, docs S11 §8) :
//!
//! 1. [`run_portfolio_research`] et [`run_portfolio_profiles_research`] jouent un portefeuille sur un
//!    dataset de bloc : activation par jambe, enregistrement de la grille à partir de `decision_start`,
//!    filtre des entrées (fenêtre de décision, trimestre de l'univers, exclusion qualité du TF de
//!    décision) avec refus comptés, annulation des entrées en attente d'une jambe qui sort de l'univers.
//!    Les points d'entrée existants gardent leur comportement au bit (goldens `portfolio_2legs` et
//!    `portfolio_profiles`).
//! 2. [`ResearchSpec`] est séparé de [`PortfolioConfig`] et de [`PortfolioInputs`], qui ne changent pas ;
//!    `PortfolioResult::research` porte le [`ResearchResult`].
//! 3. Le `config_hash` d'un run de recherche reste celui de la configuration gelée : bloc `research`
//!    facultatif de [`PortfolioRunConfig`] et trois tables par jambe vidées. Ce qui distingue deux runs
//!    est la [`ResearchIdentity`], hachée à part. `REFERENCE_JSON` des générations v1 et v2 est inchangé.
//!
//! Écarts d'implémentation (S6) à S3 :
//!
//! 1. Le `CellConfig` porte l'`InstrumentRules` (tick, lot, minimums), qui entre dans le `config_hash`
//!    avec les `SimRules` (délai d'activation, traversée d'un tick, `IntrabarRule`, `max_pending`).
//! 2. Les signaux sont traduits en `Instruction` du simulateur par `engine::signals::dispatch`, qui
//!    compte les rejets par motif ; un signal ne produit plus de `MarketOrder` direct.
//! 3. La boucle appelle `Simulator::on_bar` à chaque 1m (étapes 0 à 6) puis la stratégie, puis
//!    `submit` pour chaque signal ; un ordre décidé à la clôture de `i` n'est actif qu'à `i + 2` par
//!    défaut (I3).
//!
//! Écarts d'implémentation (S5) à S3 :
//!
//! 1. Warmup multi-timeframe. Il se compte en bougies du plus grand timeframe déclaré. La première 1m
//!    jouée est la plus petite `i` telle que `closed_count[tf_max][i - 1] >= warmup_bars` (avec 0 pour
//!    `i = 0`) : les `warmup_bars` bougies sont toutes clôturées à la fin de la 1m qui précède. C'est
//!    la convention S3 généralisée ; pour une stratégie 1m elle redonne `start = warmup_bars` au bit.
//! 2. Refus de démarrer. Pour chaque indicateur, dans l'ordre de `indicators()`, avec
//!    `k = closed_count[tf][start]` : [`EngineError::WarmupTooShort`] si `k == 0` (index 0) ou si la
//!    colonne n'est pas finie à `k - 1`. L'index est celui de la série du timeframe de l'indicateur.
//! 3. Appel de la stratégie. `on_bar` est appelé à la 1m `i` si `just_closed[i]` contient un timeframe
//!    déclaré, jamais autrement. Les fills, l'observation intrabar et l'equity ont lieu à chaque 1m.
//! 4. Garde-fou. [`BarContext::new`](crate::strategy::BarContext::new) refuse une bougie visible non
//!    clôturée ([`StrategyError::LookAhead`], S2) : avec un `closed_count` fautif, le run échoue
//!    avant d'appeler la stratégie (cinquième ligne de défense de docs/03 §7.5).
//! 5. `single_tf_1m` est levé : une stratégie peut déclarer n'importe quel sous-ensemble de
//!    `Timeframe::ALL`. [`run_cell_mtf`] rejoue sur un [`Mtf`](crate::mtf::Mtf) déjà construit.

#![forbid(unsafe_code)]

pub mod config;
mod portfolio;
pub mod portfolio_config;
pub mod priority;
pub mod profiles;
mod regime;
pub mod research;
mod run;
pub mod signals;

pub use config::{CellConfig, DEFAULT_MIN_TRADES, RunConfig, StrategyRecord};
pub use portfolio::{
    LegResult, OwnedPortfolioInputs, PortfolioInputs, PortfolioResult, PrepStats, PriorityStats,
    RegimeStats, run_portfolio, run_portfolio_counted, run_portfolio_observed,
    run_portfolio_research, run_portfolio_research_counted, run_portfolio_research_owned,
};
pub use portfolio_config::{
    CostsHashed, DEFAULT_MAX_POSITIONS, DEFAULT_QUOTE_GAP_MS, FeeGridHashed, LIQUIDITY_WINDOW_DAYS,
    MAX_POSITIONS_LIMIT, MinOrderBump, PORTFOLIO_SCHEMA, PORTFOLIO_SCHEMA_V2, PRIORITY_RULE,
    PURGE_RULE, PortfolioBlock, PortfolioConfig, PortfolioRunConfig, RegimeRecord, ResearchHashed,
    UniverseSpec,
};
pub use priority::RankTable;
pub use profiles::{
    CostSetup, OwnedProfilesInputs, ProfileInputs, ProfileRun, ProfilesInputs, ProfilesResult,
    run_portfolio_profile, run_portfolio_profiles, run_portfolio_profiles_research,
    run_portfolio_profiles_research_owned,
};
pub use research::{
    Line, ResearchIdentity, ResearchLegCounters, ResearchLegResult, ResearchResult, ResearchSpec,
    legs_hash,
};
pub use run::{CellResult, run_cell, run_cell_mtf, run_cell_observed};
pub use signals::{EngineFunnel, SignalClass, SignalRejections, StrategyFunnelCounts};

use thiserror::Error;

use crate::indicators::IndicatorError;
use crate::model::blocks::Block;
use crate::model::{ModelError, Timeframe, Ts};
use crate::mtf::MtfError;
use crate::sim::SimError;
use crate::strategy::StrategyError;

/// Erreurs d'une cellule de backtest.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Sim(#[from] SimError),

    #[error(transparent)]
    Strategy(#[from] StrategyError),

    #[error(transparent)]
    Indicator(#[from] IndicatorError),

    #[error(transparent)]
    Model(#[from] ModelError),

    #[error("aucune bougie à rejouer")]
    EmptyData,

    #[error("`min_trades` doit valoir au moins 1")]
    InvalidMinTrades,

    #[error("bougie {open_time_ms} : le moteur attend des bougies 1m alignées sur la minute")]
    NotOneMinute { open_time_ms: Ts },

    #[error(
        "bougie {index} : open_time {open_time_ms} non strictement croissant (précédent : {previous_ms})"
    )]
    NotIncreasing {
        index: usize,
        previous_ms: Ts,
        open_time_ms: Ts,
    },

    #[error(transparent)]
    Mtf(#[from] MtfError),

    #[error("le multi-timeframe fourni ne contient pas le timeframe {tf} déclaré par la stratégie")]
    MissingTimeframe { tf: Timeframe },

    #[error(
        "index de clôture absent à la bougie {index} : le multi-timeframe est incohérent avec les données"
    )]
    MissingClosedCount { index: usize },

    #[error("{bars} bougies pour un warmup de {warmup_bars} : il n'en reste aucune à rejouer")]
    NotEnoughBars { warmup_bars: usize, bars: usize },

    #[error(
        "l'indicateur `{name}` n'est pas encore valide à l'index {index} : le warmup déclaré est trop court"
    )]
    WarmupTooShort { name: &'static str, index: usize },

    // --- Mode portefeuille (S9) ---
    #[error(
        "une stratégie lit le régime : `run_cell` ne charge aucune série de régime, utiliser le mode portefeuille (`--symbols`)"
    )]
    RegimeNeedsPortfolio,

    #[error(
        "la série de régime `{symbol}` est absente du dataset : la reconstruire avec `--symbol {symbol}`"
    )]
    RegimeSeriesMissing { symbol: String },

    #[error(
        "la série de régime `{symbol}` n'a jamais 200 bougies 1d closes : la SMA200 n'existe pas, la fenêtre doit commencer au moins 200 jours avant la période jouée"
    )]
    RegimeNotReady { symbol: String },

    #[error("l'univers du portefeuille est vide")]
    EmptyUniverse,

    #[error("{legs} paires : un portefeuille en compte au plus 65 535")]
    TooManyLegs { legs: usize },

    #[error("le symbole `{symbol}` figure deux fois dans l'univers")]
    DuplicateSymbol { symbol: String },

    #[error("le symbole `{symbol}` de l'univers est absent du dataset")]
    SymbolNotInDataset { symbol: String },

    #[error("la série de `{symbol}` ne contient aucune bougie")]
    LegWithoutBars { symbol: String },

    #[error("{instruments} règles d'instrument pour {legs} paires : une par paire est attendue")]
    InstrumentCountMismatch { instruments: usize, legs: usize },

    #[error("{strategies} stratégies pour {legs} paires : une par paire est attendue")]
    StrategyCountMismatch { strategies: usize, legs: usize },

    #[error(
        "les stratégies de l'univers diffèrent (identifiant, version ou paramètres) : le config_hash n'en décrit qu'une"
    )]
    StrategiesDiffer,

    #[error("`max_positions` vaut {max_positions} : un entier de 1 à 64 est attendu")]
    InvalidMaxPositions { max_positions: u16 },

    #[error("trou de cotation de {quote_gap_ms} ms : une durée strictement positive est attendue")]
    InvalidQuoteGap { quote_gap_ms: Ts },

    #[error(
        "`{symbol}` : {bars} bougies pour un warmup de {warmup_bars} : il n'en reste aucune à rejouer à la minute de départ commune"
    )]
    LegNotEnoughBars {
        symbol: String,
        warmup_bars: usize,
        bars: usize,
    },

    // --- Profils de coûts (S10) ---
    #[error(
        "slippage scalaire non nul ({bps} bp) avec des profils de coûts : le slippage market vit dans le profil de chaque jambe, `PortfolioConfig::slippage` doit valoir zéro"
    )]
    ScalarSlippageWithCosts { bps: f64 },

    #[error(
        "`stop_stress_bps` vaut {stop_stress_bps} avec des profils de coûts : le stress vit dans le profil (`config/costs.toml`), `SimRules::stop_stress_bps` doit valoir zéro"
    )]
    StopStressWithCosts { stop_stress_bps: f64 },

    #[error(
        "la grille de frais du lieu `{exchange}` n'a pas de nom de grille : un run s10 exige une grille nommée (okx_regular ou bitvavo_usdc)"
    )]
    NoFeeGrid { exchange: crate::model::ExchangeId },

    #[error(
        "le timeframe de décision {tf} de la stratégie `{id}` n'est pas dans ses timeframes déclarés"
    )]
    DecisionTfNotDeclared { id: &'static str, tf: Timeframe },

    #[error(
        "les trois jeux de stratégies des profils diffèrent (identifiant, version, paramètres, indicateurs, timeframes, timeframe de décision, régime ou warmup) : les colonnes d'indicateurs sont calculées une fois, sur le jeu du profil baseline"
    )]
    ProfileStrategiesDiffer,

    // --- Mode recherche (S11) ---
    #[error(
        "le mode recherche exige `zero_volume_no_fill = true` : une bougie à volume nul (synthétique ou réelle) ne doit jamais remplir"
    )]
    ResearchNeedsZeroVolumeRule,

    #[error(
        "le trimestre {quarter} recoupe la fenêtre de décision mais n'est pas dans l'univers fourni"
    )]
    UniverseQuarterMissing { quarter: String },

    #[error("fenêtre de décision invalide pour le bloc {block} : {reason}")]
    ResearchWindow { block: Block, reason: &'static str },

    #[error(
        "aucune jambe ne devient vivante : le warmup de chaque jambe (et la SMA200 du régime) dépasse les données du dataset"
    )]
    ResearchNoLiveLeg,
}

/// Crochet appelé exactement deux fois par run, juste avant et juste après la boucle 1m. Il permet
/// de mesurer la boucle de l'extérieur (allocateur compteur, chronomètre) sans mettre d'horloge dans
/// `engine`.
pub trait LoopObserver {
    fn loop_started(&mut self);
    fn loop_finished(&mut self);
}

/// Observateur qui ne fait rien : celui de [`run_cell`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopObserver;

impl LoopObserver for NoopObserver {
    fn loop_started(&mut self) {}
    fn loop_finished(&mut self) {}
}
