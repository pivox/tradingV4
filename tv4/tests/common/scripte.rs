//! Stratégie scriptée partagée par les tests d'intégration : émet des signaux fixés à l'avance.
//!
//! M1 seulement, warmup 0, aucun indicateur. Avec warmup 0 et un seul timeframe M1, l'appel `k` de
//! `on_bar` est la 1m d'index `k` de la fenêtre : le plan `(k, signal)` désigne donc la 1m dont la
//! clôture déclenche le signal.

use tv4::model::Timeframe;
use tv4::strategy::{BarContext, IndicatorSpec, Signal, Strategy};

/// Stratégie de test qui rejoue un plan `(index de 1m, signal)`.
pub struct Scripte {
    plan: Vec<(usize, Signal)>,
    appel: usize,
}

impl Scripte {
    /// Plan : les signaux dont l'index vaut `k` sont émis à l'appel `k`, dans l'ordre du plan.
    pub fn new(plan: Vec<(usize, Signal)>) -> Scripte {
        Scripte { plan, appel: 0 }
    }
}

impl Strategy for Scripte {
    fn id(&self) -> &'static str {
        "scripte"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M1]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn on_bar(&mut self, _ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        for (appel, signal) in &self.plan {
            if *appel == self.appel {
                out.push(*signal);
            }
        }
        self.appel += 1;
    }
}
