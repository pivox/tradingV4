//! Post-traitements numériques purs du journal (S13).
//! Les entrées sont possédées par l'appelant ; aucun fichier, horloge ou moteur ici.
mod approx;
mod benchmarks;
mod bootstrap;
mod clustering;
mod concentration;
mod daily;
mod monte_carlo;
mod random;
mod regimes;
mod types;

pub use approx::quantile_type7;
pub use bootstrap::{BootstrapStats, bootstrap};
pub use concentration::{
    ConcentrationStats, GroupRemoval, TradeIdentity, TradeRemoval, concentration,
};
pub use monte_carlo::{MonteCarloStats, TradeClosure, monte_carlo};
pub use random::{STREAM_BOOTSTRAP, STREAM_MONTE_CARLO, STREAM_MONTHLY_BOOTSTRAP, STREAM_REMOVAL};
pub use types::{
    Availability, CashPoint, MarkPoint, Metric, PricePoint, StatsError, StatsOptions, StatsSizing,
    TradeRecord, Window,
};

pub use benchmarks::*;
pub use clustering::*;
pub use daily::{DailyReturns, DatedReturn, MarketSeries, daily_returns};
pub use regimes::*;
