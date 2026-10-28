//! Outils de recherche hors simulation (docs/03-architecture.md §2, docs/specs/s10.md §11 et §15.1).
//!
//! Pour l'instant, un seul outil : la calibration de la composante de stress des stops
//! ([`stress`]). Le module lit le modèle, les colonnes de bougies du `store`, `mtf` et les
//! indicateurs ; il ne produit aucun rapport, n'appelle pas la CLI et ne connaît pas les stratégies
//! (règle vérifiée par `tests/architecture.rs`).

pub mod stress;
