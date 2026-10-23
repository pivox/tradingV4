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
mod run;
pub mod signals;

pub use config::{CellConfig, RunConfig, StrategyRecord};
pub use run::{CellResult, run_cell, run_cell_mtf, run_cell_observed};
pub use signals::SignalRejections;

use thiserror::Error;

use crate::indicators::IndicatorError;
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
