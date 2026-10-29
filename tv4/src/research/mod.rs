//! Outils de recherche hors simulation (docs/03-architecture.md §2, docs/specs/s10.md §11 et §15.1,
//! docs/specs/s11.md §12.1).
//!
//! - [`stress`] : calibration de la composante de stress des stops (S10) ;
//! - [`universe`] : calcul de l'univers trimestriel, hashes d'entrée, règles d'instrument dérivées du
//!   dump et liste figée des paires OKX tradables en USDC (S11) ;
//! - [`survivorship`] : test de survivance du dump Kraken (S11).
//!
//! Le module lit le modèle, les colonnes de bougies du `store`, `marketdata`, `mtf`, les indicateurs
//! et deux constantes de `engine` ; il ne produit aucun rapport, n'appelle pas la CLI et ne connaît
//! pas les stratégies (règle vérifiée par `tests/architecture.rs`). Les fonctions de S11 sont pures :
//! le zip est ouvert par la CLI.

pub mod stress;
pub mod survivorship;
pub mod universe;
