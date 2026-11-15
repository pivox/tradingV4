//! Contrat causal des géométries utilisées par les contrôles de timing S14.
//!
//! Le moteur capture l'état juste avant chaque évaluation effectivement jouée, calcule l'intention
//! d'entrée puis appelle `advance_flat`. Une sonde restaure ce seed en O(1), sans relire le préfixe.

use super::{BarContext, Signal, StrategyError};

/// Intervention sur le seul filtre BTC ; sans filtre BTC, les deux modes sont identiques.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryIntervention {
    PreserveBtc,
    RemoveBtc,
}

/// Capacités à vérifier avant de construire un catalogue single_lot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryCapabilities {
    pub single_lot: bool,
    pub partial_exits: bool,
    pub quantity_dependent: bool,
    pub reconstructible: bool,
    pub has_btc_filter: bool,
}

/// État de prix fermé aux géométries admises ; aucun état de portefeuille ou de données futures.
#[derive(Clone, Debug, PartialEq)]
pub enum GeometryState {
    /// Géométrie exogène sans mémoire de prix (fixtures).
    Stateless,
    /// Écart de la dernière clôture valide effectivement évaluée, pas de la colonne précédente.
    SmaCross { prev_diff: Option<f64> },
}

/// Géométrie indépendante du déclencheur Observed et du portefeuille.
pub trait ControlGeometry: Send {
    /// Description stable du contrat, de la configuration et des horloges, sans état de run.
    fn descriptor(&self) -> serde_json::Value;
    fn capabilities(&self) -> GeometryCapabilities;
    fn capture_state(&self) -> GeometryState;
    /// Restauration O(1) ; un état d'une autre géométrie est refusé sans mutation.
    fn restore_state(&mut self, state: &GeometryState) -> Result<(), StrategyError>;
    /// Avance uniquement la mémoire de prix lors d'une évaluation effective, même sans entrée.
    fn advance_flat(&mut self, ctx: &BarContext<'_>);
    /// Remplace le seul déclencheur d'entrée. Ne lit ni cash, ni quantité, ni position.
    fn entry(&self, ctx: &BarContext<'_>, mode: EntryIntervention) -> Option<Signal>;
    /// Évalue les sorties historiques et avance la mémoire de prix comme Observed.
    fn exits(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>);
}
