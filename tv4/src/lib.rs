//! Moteur tradingV4 : bibliothèque du paquet `tv4`.
//!
//! Chaque module est une future crate (docs/03-architecture.md §2). La règle de dépendance entre
//! modules est vérifiée par `tests/architecture.rs` :
//!
//! - `model` ne dépend de rien ;
//! - `strategy` ne dépend que de `model` ;
//! - `sim` ne dépend pas de `strategy` ;
//! - `engine` produit un résultat pur : il n'écrit rien (c'est `report`) et ne remonte pas vers `cli` ;
//! - `report` lit les résultats de `engine` et `sim` : il ne connaît pas `strategy` et ne remonte pas
//!   vers `cli` ;
//! - `indicators` ne dépend que de `model` ;
//! - `marketdata` ne dépend que de `model` ;
//! - `mtf` ne dépend que de `model` ;
//! - `store` ne dépend que de `model` ; `arrow` et `parquet` n'apparaissent que dans
//!   `store/parquet.rs` ;
//! - `cli` n'a aucune logique métier ;
//! - `chrono` est absent de `model`, `engine`, `sim` et `strategy` (en interne, le temps est un `i64`
//!   en millisecondes UTC).

// Aucun `unsafe` dans le moteur : le bornage anti look-ahead repose sur ce garde-fou (docs/03 §1.2).
#![forbid(unsafe_code)]
// Pas de `unwrap()` dans la lib : un `Result` remonte l'erreur. Toléré dans les tests (clippy.toml).
#![deny(clippy::unwrap_used)]

pub mod cli;
pub mod engine;
pub mod indicators;
pub mod marketdata;
pub mod model;
pub mod mtf;
pub mod report;
pub mod sim;
pub mod store;
pub mod strategy;

/// Git sha du commit compilé, fourni par `build.rs` (`unknown` sans git).
pub const GIT_SHA: &str = env!("TV4_GIT_SHA");

/// Version du paquet suivie du git sha, par exemple `0.1.0 (a1b2c3d4e5f6)`.
pub const LONG_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("TV4_GIT_SHA"), ")");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_version_longue_contient_le_sha() {
        assert!(LONG_VERSION.contains(GIT_SHA));
        assert!(LONG_VERSION.starts_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn le_sha_est_hexadecimal_ou_unknown() {
        // Forme attendue : `a1b2c3d4e5f6`, `a1b2c3d4e5f6-dirty` (modifications non commitées) ou `unknown`.
        let hexa_part = GIT_SHA.strip_suffix("-dirty").unwrap_or(GIT_SHA);
        let hexa = !hexa_part.is_empty() && hexa_part.chars().all(|c| c.is_ascii_hexdigit());
        assert!(hexa || GIT_SHA == "unknown", "sha inattendu : {GIT_SHA}");
    }
}
