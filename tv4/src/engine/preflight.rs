//! Construction de l'identité de configuration avant accès aux bougies (S12).
//!
//! Réutilise les validateurs, la résolution des coûts et les constructeurs canoniques
//! du moteur. Aucun hash n'est reconstitué par une formule propre à la CLI.

use crate::model::costs::CostProfileName;
use crate::strategy::{Strategy, validate_declaration};

use super::portfolio::check_config;
use super::profiles::{check_cost_config, resolve_legs};
use super::research::decision_tf_of;
use super::{
    CellConfig, CostSetup, EngineError, PortfolioConfig, PortfolioRunConfig, ResearchHashed,
    RunConfig, UniverseSpec,
};

/// Configuration d'une cellule sans charger ses séries.
///
/// # Erreurs
/// Déclaration de stratégie, règles, instrument ou seuil d'effectif invalides ;
/// une stratégie qui lit le régime exige le mode portefeuille.
pub fn cell(
    strategy: &dyn Strategy,
    config: &CellConfig<'_>,
    min_trades: u32,
) -> Result<RunConfig, EngineError> {
    if strategy.uses_regime() {
        return Err(EngineError::RegimeNeedsPortfolio);
    }
    config.sim_rules.validate()?;
    config.instrument.validate()?;
    validate_declaration(strategy)?;
    RunConfig::new(strategy, config, strategy.warmup_bars()).with_min_trades(min_trades)
}

/// Configuration portefeuille, éventuellement profilée et/ou de recherche, sans
/// accès aux séries. `universe` est la règle de recherche gelée, pas ses membres
/// actifs sur un bloc. Les champs par jambe suivent exactement `into_research`.
///
/// # Erreurs
/// Celles des validateurs de configuration et de la résolution des coûts du moteur.
pub fn portfolio(
    strategies: &[Box<dyn Strategy>],
    config: &PortfolioConfig<'_>,
    costs: Option<(CostSetup<'_>, CostProfileName)>,
    universe: Option<&UniverseSpec>,
) -> Result<PortfolioRunConfig, EngineError> {
    let checked = check_config(strategies, config)?;
    let strategy = strategies.first().ok_or(EngineError::EmptyUniverse)?;
    let mut result = match costs {
        Some((setup, profile)) => {
            check_cost_config(config)?;
            let tf = decision_tf_of(strategies)?;
            let (_, legs) = resolve_legs(config, &setup, profile, tf)?;
            PortfolioRunConfig::with_costs(
                strategy.as_ref(),
                config,
                checked.warmup_bars,
                checked.uses_regime,
                setup.seed,
                profile,
                &legs,
            )?
        }
        None => PortfolioRunConfig::new(
            strategy.as_ref(),
            config,
            checked.warmup_bars,
            checked.uses_regime,
        ),
    };
    if let Some(universe) = universe {
        result = result.into_research(ResearchHashed::new(
            universe.clone(),
            decision_tf_of(strategies)?,
        ));
    }
    Ok(result)
}
