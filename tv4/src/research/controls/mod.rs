//! Contrôles aléatoires : contrats numériques purs, sans simulation ni I/O (S14).

pub mod bootstrap;
pub mod calendar;
pub mod random;
pub mod statistics;
pub mod types;

pub use calendar::CalendarTable;
pub use types::*;
