//! Lecture des données de marché brutes (docs/03-architecture.md §4).
//!
//! Le module ne dépend que de `model` (règle vérifiée par `tests/architecture.rs`). Il contient le
//! lecteur des dumps OHLCVT Kraken (`kraken`, S2), la lecture du zip complet avec vérification du
//! SHA-256 (`kraken_zip`, S5) et le contrôle de continuité qui produit le rapport bloquant
//! (`continuity`, S5). Le lecteur OKX (`okx`, S7 première partie : client `ureq`, token bucket, retry, pagination) lit
//! `history-candles` ; l'écriture, la reprise et `compare-sources` arrivent avec la suite de S7. L'écriture des fichiers et des rapports relève de
//! `store` et de `cli` : `marketdata` ne lit que.

pub mod continuity;
pub mod kraken;
pub mod kraken_zip;
pub mod okx;
