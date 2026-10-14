//! Lecture des données de marché brutes (docs/03-architecture.md §4).
//!
//! Le module ne dépend que de `model` (règle vérifiée par `tests/architecture.rs`). S2 ne contient que
//! le lecteur des dumps OHLCVT Kraken ; le lecteur OKX arrive en S7, le contrôle de continuité en S5.

pub mod kraken;
