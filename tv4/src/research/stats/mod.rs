//! Post-traitements numériques purs du journal (S13).
//! Les entrées sont possédées par l'appelant ; aucun fichier, horloge ou moteur ici.
mod approx;
mod bootstrap;
mod concentration;
mod types;

pub use approx::quantile_type7;
pub use bootstrap::{
    BootstrapStats, STREAM_BOOTSTRAP, STREAM_MONTE_CARLO, STREAM_MONTHLY_BOOTSTRAP, STREAM_REMOVAL,
    bootstrap,
};
pub use concentration::{
    ConcentrationStats, GroupRemoval, TradeIdentity, TradeRemoval, concentration,
};
pub use types::{
    Availability, CashPoint, MarkPoint, Metric, PricePoint, StatsError, StatsOptions, StatsSizing,
    TradeRecord, Window,
};
