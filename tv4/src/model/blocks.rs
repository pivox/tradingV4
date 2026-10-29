//! Blocs de recherche, embargos, chauffe et purge (docs/specs/s11.md §3, docs/04 §2.3).
//!
//! Les quatre blocs DEV, VALIDATION, HOLDOUT et FORWARD pavent le temps à partir du 2018-01-01 ; les
//! deux embargos de 59 jours (dates de 04 §2.3, pas « 60 j ») les séparent. Les intervalles du code
//! sont semi-ouverts `[début, fin)` en millisecondes UTC : la date de fin inclusive de 04 devient le
//! lendemain 00:00.
//!
//! **Une seule écriture.** Les six instants de frontière (début de DEV, fin de DEV, fin du premier
//! embargo, fin de VALIDATION, fin du second embargo, fin de HOLDOUT) ne sont écrits qu'ici dans le
//! code hors tests de `tv4/src` : `tests/architecture.rs` refuse leur littéral ailleurs, et
//! `tests/blocks.rs` les recalcule par une conversion calendaire indépendante (ce module ne dépend de
//! rien, il ne peut donc pas appeler `store::time`).
//!
//! Ce module ne dépend d'aucun autre module du moteur, n'utilise pas `chrono`, et n'alloue pas.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{ModelError, Ts};

/// Début de DEV : 2018-01-01 00:00 UTC.
const DEV_START_MS: Ts = 1_514_764_800_000;
/// Fin de DEV et début du premier embargo : 2023-01-01 00:00 UTC.
const EMBARGO_1_START_MS: Ts = 1_672_531_200_000;
/// Fin du premier embargo et début de VALIDATION : 2023-03-01 00:00 UTC.
const VALIDATION_START_MS: Ts = 1_677_628_800_000;
/// Fin de VALIDATION et début du second embargo : 2025-01-01 00:00 UTC.
const EMBARGO_2_START_MS: Ts = 1_735_689_600_000;
/// Fin du second embargo et début de HOLDOUT : 2025-03-01 00:00 UTC.
const HOLDOUT_START_MS: Ts = 1_740_787_200_000;
/// Fin de HOLDOUT et début de FORWARD : 2026-07-01 00:00 UTC.
const FORWARD_START_MS: Ts = 1_782_864_000_000;

/// Millisecondes dans un jour.
const DAY_MS: Ts = 86_400_000;

/// Chauffe par défaut avant le début d'un bloc : 200 jours de la SMA200 1d (la plus longue chauffe
/// admise, 04 §2.3 et §4.2) plus 10 jours de marge (s11 §3.3).
pub const WARMUP_PREFIX_DAYS_DEFAULT: i64 = 210;

/// Chauffe maximale acceptée (`--warmup-days`, 0 à 400).
pub const WARMUP_PREFIX_DAYS_MAX: i64 = 400;

/// Version des frontières et de la règle de purge : entre dans le `config_hash` d'un run de recherche
/// (s11 §8.6).
pub const BLOCKS_VERSION: &str = "b1";

/// Bloc de données de la recherche (04 §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Block {
    Dev,
    Validation,
    Holdout,
    Forward,
}

/// Intervalle `[start_ms, end_ms)` d'un bloc ; `end_ms` vaut `None` pour FORWARD (ouvert à droite).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSpan {
    pub start_ms: Ts,
    pub end_ms: Option<Ts>,
}

impl BlockSpan {
    /// `true` si `ts` est dans `[start_ms, end_ms)`.
    pub const fn contains(&self, ts: Ts) -> bool {
        ts >= self.start_ms
            && match self.end_ms {
                Some(end) => ts < end,
                None => true,
            }
    }
}

/// Les quatre blocs dans l'ordre chronologique.
pub const BLOCKS: [(Block, BlockSpan); 4] = [
    (
        Block::Dev,
        BlockSpan {
            start_ms: DEV_START_MS,
            end_ms: Some(EMBARGO_1_START_MS),
        },
    ),
    (
        Block::Validation,
        BlockSpan {
            start_ms: VALIDATION_START_MS,
            end_ms: Some(EMBARGO_2_START_MS),
        },
    ),
    (
        Block::Holdout,
        BlockSpan {
            start_ms: HOLDOUT_START_MS,
            end_ms: Some(FORWARD_START_MS),
        },
    ),
    (
        Block::Forward,
        BlockSpan {
            start_ms: FORWARD_START_MS,
            end_ms: None,
        },
    ),
];

/// `[début, fin)` de chaque embargo, dans l'ordre chronologique (59 jours chacun, s11 §3.2).
pub const EMBARGOS: [(Ts, Ts); 2] = [
    (EMBARGO_1_START_MS, VALIDATION_START_MS),
    (EMBARGO_2_START_MS, HOLDOUT_START_MS),
];

/// Où tombe un instant par rapport aux blocs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// Avant le 2018-01-01.
    BeforeDev,
    /// Dans un bloc.
    Block(Block),
    /// Dans l'embargo n° 1 (entre DEV et VALIDATION) ou n° 2 (entre VALIDATION et HOLDOUT).
    Embargo(u8),
}

impl Block {
    /// Les quatre blocs, dans l'ordre chronologique.
    pub const ALL: [Block; 4] = [
        Block::Dev,
        Block::Validation,
        Block::Holdout,
        Block::Forward,
    ];

    /// Rendu canonique (`DEV`, `VALIDATION`, `HOLDOUT`, `FORWARD`), celui du manifeste de dataset.
    pub const fn as_str(self) -> &'static str {
        match self {
            Block::Dev => "DEV",
            Block::Validation => "VALIDATION",
            Block::Holdout => "HOLDOUT",
            Block::Forward => "FORWARD",
        }
    }

    /// Intervalle `[début, fin)` du bloc.
    pub const fn span(self) -> BlockSpan {
        match self {
            Block::Dev => BLOCKS[0].1,
            Block::Validation => BLOCKS[1].1,
            Block::Holdout => BLOCKS[2].1,
            Block::Forward => BLOCKS[3].1,
        }
    }

    /// Vrai pour VALIDATION et HOLDOUT, dont la lecture est comptée (lu par S12).
    pub const fn is_protected(self) -> bool {
        matches!(self, Block::Validation | Block::Holdout)
    }

    /// Début de la chauffe d'un dataset du bloc : `start_ms − warmup_days` jours (s11 §3.3).
    ///
    /// `warmup_days` doit être dans `0..=WARMUP_PREFIX_DAYS_MAX`.
    pub fn warmup_start_ms(self, warmup_days: i64) -> Result<Ts, ModelError> {
        if !(0..=WARMUP_PREFIX_DAYS_MAX).contains(&warmup_days) {
            return Err(ModelError::WarmupOutOfRange { days: warmup_days });
        }
        Ok(self.span().start_ms - warmup_days * DAY_MS)
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Block {
    type Err = ModelError;

    /// Lit le rendu canonique, sans tenir compte de la casse (`dev`, `Holdout`...).
    fn from_str(text: &str) -> Result<Block, ModelError> {
        Block::ALL
            .into_iter()
            .find(|block| block.as_str().eq_ignore_ascii_case(text))
            .ok_or_else(|| ModelError::UnknownBlock(text.to_string()))
    }
}

/// Zone de l'instant `ts` : avant DEV, dans un bloc ou dans un embargo (intervalles semi-ouverts).
pub fn zone_of(ts: Ts) -> Zone {
    if ts < DEV_START_MS {
        Zone::BeforeDev
    } else if ts < EMBARGO_1_START_MS {
        Zone::Block(Block::Dev)
    } else if ts < VALIDATION_START_MS {
        Zone::Embargo(1)
    } else if ts < EMBARGO_2_START_MS {
        Zone::Block(Block::Validation)
    } else if ts < HOLDOUT_START_MS {
        Zone::Embargo(2)
    } else if ts < FORWARD_START_MS {
        Zone::Block(Block::Holdout)
    } else {
        Zone::Block(Block::Forward)
    }
}

/// Fenêtre de décision `[début du bloc, min(fin du bloc, window_end_ms))` d'un dataset de bloc.
///
/// Refuse une fenêtre qui se termine au plus tard au début du bloc (fenêtre de décision vide) et, pour
/// un bloc fermé, une fenêtre qui se termine après la fin du bloc (elle recouvrirait l'embargo ou le
/// bloc suivant).
pub fn decision_window(block: Block, window_end_ms: Ts) -> Result<(Ts, Ts), ModelError> {
    let span = block.span();
    if window_end_ms <= span.start_ms {
        return Err(ModelError::DecisionWindow {
            block,
            window_end_ms,
            reason: "la fenêtre se termine au plus tard au début du bloc",
        });
    }
    match span.end_ms {
        Some(end) if window_end_ms > end => Err(ModelError::DecisionWindow {
            block,
            window_end_ms,
            reason: "la fenêtre se termine après la fin du bloc",
        }),
        Some(end) => Ok((span.start_ms, window_end_ms.min(end))),
        None => Ok((span.start_ms, window_end_ms)),
    }
}

/// Un trade est **purgé** (s11 §3.4) si son entrée a été décidée avant `decision_end_ms` et s'il est
/// soldé par la fin de fenêtre (`end_of_window`) ou sort à `exit_ms ≥ decision_end_ms`.
///
/// Dans un run de recherche, les deux conditions de sortie coïncident (la fenêtre s'arrête à
/// `decision_end_ms`) ; la seconde n'est qu'une garde. Une entrée décidée à `decision_end_ms` ou
/// après n'existe pas dans un run de recherche (le moteur la refuse) : elle n'est jamais « purgée ».
pub const fn is_purged(
    entry_signal_ms: Ts,
    exit_ms: Ts,
    decision_end_ms: Ts,
    end_of_window: bool,
) -> bool {
    entry_signal_ms < decision_end_ms && (end_of_window || exit_ms >= decision_end_ms)
}
