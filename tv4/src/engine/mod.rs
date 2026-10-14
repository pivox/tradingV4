//! Moteur de backtest mono-timeframe 1m (docs/03-architecture.md §2, §7.1 et §7.2).
//!
//! [`run_cell`] est une fonction pure : ni `static mut`, ni `thread_local!`, ni horloge, ni écriture
//! (c'est le rôle de `report`). Une cellule prend une stratégie fraîche construite par le registre,
//! calcule les colonnes d'indicateurs hors boucle, puis déroule la boucle 1m sans allocation.
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
//! 4. Les sept écarts de `sim` (voir la doc de ce module), dont `single_tf_1m` : une stratégie qui ne
//!    déclare pas exactement `[1m]` est refusée ([`EngineError::UnsupportedTimeframes`]) ; l'agrégation
//!    1m vers TF arrive en S5. La clôture forcée est comptée comme un trade (`n_trades`), avec
//!    `forced_close` et `n_trades_forced`, et son PnL est isolé dans `pnl_marked`.
//! 5. `ctx.pending()` est toujours vide (aucun ordre ne survit à l'étape 1) et la stratégie voit
//!    `position.stop == None` : le stop est mémorisé par le simulateur, jamais actif.

#![forbid(unsafe_code)]

pub mod config;
mod run;
pub mod signals;

pub use config::{CellConfig, RunConfig, StrategyRecord};
pub use run::{CellResult, run_cell, run_cell_observed};
pub use signals::SignalRejections;

use thiserror::Error;

use crate::indicators::IndicatorError;
use crate::model::{ModelError, Timeframe, Ts};
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

    #[error("bougie {open_time_ms} : le moteur S3 attend des bougies 1m alignées sur la minute")]
    NotOneMinute { open_time_ms: Ts },

    #[error(
        "bougie {index} : open_time {open_time_ms} non strictement croissant (précédent : {previous_ms})"
    )]
    NotIncreasing {
        index: usize,
        previous_ms: Ts,
        open_time_ms: Ts,
    },

    #[error(
        "la stratégie déclare {} ; S3 n'exécute qu'une stratégie mono-timeframe 1m sur les bougies 1m \
         (agrégation 1m → TF à l'étape S5, docs/03-architecture.md §11) ; pour la fixture : \
         config/strategies/sma_cross_1m.toml",
        timeframes_list(.declared)
    )]
    UnsupportedTimeframes { declared: Vec<Timeframe> },

    #[error("{bars} bougies pour un warmup de {warmup_bars} : il n'en reste aucune à rejouer")]
    NotEnoughBars { warmup_bars: usize, bars: usize },

    #[error(
        "l'indicateur `{name}` n'est pas encore valide à l'index {index} : le warmup déclaré est trop court"
    )]
    WarmupTooShort { name: &'static str, index: usize },
}

/// `1m, 15m` : liste lisible de timeframes pour les messages d'erreur.
fn timeframes_list(timeframes: &[Timeframe]) -> String {
    let names: Vec<&str> = timeframes.iter().map(|tf| tf.as_str()).collect();
    names.join(", ")
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
