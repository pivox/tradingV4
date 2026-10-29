//! Trois profils de coûts sur une préparation partagée (docs S10 §9).
//!
//! [`run_portfolio_profiles`] joue baseline, adverse et maker dans cet ordre, **un seul appel, une seule
//! préparation** : les bougies, les `Mtf` de chaque jambe (et celui du régime), les colonnes
//! d'indicateurs, le rang de liquidité et la série de régime sont construits une fois ([`load`] et
//! [`share`], comptés dans [`PrepStats`] par cette fonction), puis chaque profil reçoit un [`Book`] neuf,
//! un jeu de stratégies neuf et un curseur de régime neuf. Les trois profils tournent l'un après l'autre
//! et empruntent la même préparation : pas d'`Arc<Dataset>` ni de cache entre appels (docs S10 §9.3,
//! §19 n° 15 et §20 n° 4).
//!
//! # Une seule source pour la grille
//!
//! [`CostSetup`] ne porte pas de nom de grille : les taux viennent de `config.fees`, comme dans le chemin
//! hérité, et le nom haché (`costs.fee_grid.grid`) s'en déduit
//! ([`FeeGridId::from_legacy_exchange`]). Le bloc haché est donc toujours cohérent avec les fills.
//!
//! # Ce qui est refusé
//!
//! * un slippage scalaire non nul ([`EngineError::ScalarSlippageWithCosts`]) : il double le slippage market
//!   de chaque jambe ;
//! * `stop_stress_bps` non nul ([`EngineError::StopStressWithCosts`]) : le stress vit dans le profil ;
//! * un `decision_tf()` absent de `timeframes()` ([`EngineError::DecisionTfNotDeclared`]) ;
//! * des jeux de stratégies qui ne déclarent pas la même chose ([`EngineError::ProfileStrategiesDiffer`]),
//!   puisque les colonnes d'indicateurs sont calculées sur le jeu du profil baseline.
//!
//! # TF de décision
//!
//! Il vient de `Strategy::decision_tf()`, donc de la stratégie, et non de la jambe : les stratégies d'un
//! jeu ont toutes le même identifiant, la même version et les mêmes paramètres (`StrategiesDiffer`), donc
//! le même TF de décision. Le moteur le vérifie jambe par jambe, ce qui rend `costs.entry` du hash v2
//! unique sans perte.

use crate::model::costs::{
    CostMeasures, CostProfileName, CostsFile, LegCost, LegCostRecord, resolve_leg,
};
use crate::model::{FeeGridId, Slippage, Timeframe};
use crate::sim::{Book, CostCounters};
use crate::store::Columns;
use crate::strategy::Strategy;

use super::config::StrategyRecord;
use super::portfolio::{
    Checked, Loaded, PortfolioResult, PrepStats, Shared, book_config, check_config, load,
    play_profile, share,
};
use super::portfolio_config::{PortfolioConfig, PortfolioRunConfig};
use super::{EngineError, LoopObserver};

/// Entrées de [`run_portfolio_profiles`].
pub struct ProfilesInputs<'a> {
    /// Séries 1m du dataset, une par symbole (comme `PortfolioInputs::series`).
    pub series: &'a [(String, Columns)],
    /// Un jeu frais de stratégies par profil (baseline, adverse, maker), construit par l'appelant à
    /// partir du registre : les stratégies ont un état mutable, et rien ne passe d'un profil à l'autre.
    /// Chaque jeu a une stratégie par jambe, dans l'ordre de l'univers.
    pub strategies: [Vec<Box<dyn Strategy>>; 3],
    /// `config.slippage` doit valoir [`Slippage::ZERO`] et `config.sim_rules.stop_stress_bps` zéro.
    pub config: PortfolioConfig<'a>,
    pub costs: CostSetup<'a>,
}

/// Entrées de [`run_portfolio_profile`] : un seul profil, un seul jeu de stratégies.
pub struct ProfileInputs<'a> {
    pub series: &'a [(String, Columns)],
    pub strategies: Vec<Box<dyn Strategy>>,
    pub config: PortfolioConfig<'a>,
    pub costs: CostSetup<'a>,
}

/// Fichiers de coûts et graine d'un appel.
#[derive(Clone, Copy, Debug)]
pub struct CostSetup<'a> {
    pub file: &'a CostsFile,
    /// Mesures de carnet (S17), facultatives : elles remplacent les défauts paire par paire.
    pub measures: Option<&'a CostMeasures>,
    /// Graine des tirages, dans `[0, 2^53[` (docs S10 §8.4).
    pub seed: u64,
}

/// Un profil joué.
#[derive(Clone, Debug)]
pub struct ProfileRun {
    pub profile: CostProfileName,
    pub result: PortfolioResult,
    /// Valeurs résolues de chaque jambe, dans l'ordre de l'univers.
    pub legs: Vec<LegCostRecord>,
    /// Compteurs de coûts de chaque jambe, dans l'ordre de l'univers.
    pub cost_by_leg: Vec<CostCounters>,
}

/// Les trois profils et le coût de la préparation partagée.
#[derive(Clone, Debug)]
pub struct ProfilesResult {
    /// Dans l'ordre fixe : baseline, adverse, maker.
    pub runs: [ProfileRun; 3],
    pub prep: PrepStats,
}

/// Refus de la configuration propre aux profils : slippage scalaire, stress de simulateur, grille sans
/// nom.
fn check_cost_config(config: &PortfolioConfig<'_>) -> Result<(), EngineError> {
    if config.slippage != Slippage::ZERO {
        return Err(EngineError::ScalarSlippageWithCosts {
            bps: config.slippage.bps(),
        });
    }
    if config.sim_rules.stop_stress_bps != 0.0 {
        return Err(EngineError::StopStressWithCosts {
            stop_stress_bps: config.sim_rules.stop_stress_bps,
        });
    }
    if FeeGridId::from_legacy_exchange(config.fees.exchange).is_none() {
        return Err(EngineError::NoFeeGrid {
            exchange: config.fees.exchange,
        });
    }
    Ok(())
}

/// TF de décision d'un jeu de stratégies : celui de la première, déclaré dans ses timeframes, et le même
/// pour toutes les jambes.
fn decision_tf_of(strategies: &[Box<dyn Strategy>]) -> Result<Timeframe, EngineError> {
    let first = strategies.first().ok_or(EngineError::EmptyUniverse)?;
    let tf = first.decision_tf();
    for strategy in strategies {
        let own = strategy.decision_tf();
        if !strategy.timeframes().contains(&own) {
            return Err(EngineError::DecisionTfNotDeclared {
                id: strategy.id(),
                tf: own,
            });
        }
        if own != tf {
            return Err(EngineError::StrategiesDiffer);
        }
    }
    Ok(tf)
}

/// Vrai si deux jeux déclarent la même chose : identité de la stratégie, indicateurs, timeframes,
/// régime, warmup et TF de décision. Les colonnes d'indicateurs du premier servent au second.
fn same_declaration(a: &[Box<dyn Strategy>], b: &[Box<dyn Strategy>]) -> bool {
    let (Some(x), Some(y)) = (a.first(), b.first()) else {
        return a.is_empty() && b.is_empty();
    };
    a.len() == b.len()
        && StrategyRecord::of(x.as_ref()) == StrategyRecord::of(y.as_ref())
        && x.indicators() == y.indicators()
        && x.timeframes() == y.timeframes()
        && x.uses_regime() == y.uses_regime()
        && x.warmup_bars() == y.warmup_bars()
        && x.decision_tf() == y.decision_tf()
}

/// Résout les coûts de chaque jambe pour un profil.
fn resolve_legs(
    config: &PortfolioConfig<'_>,
    costs: &CostSetup<'_>,
    profile: CostProfileName,
    decision_tf: Timeframe,
) -> Result<(Vec<LegCost>, Vec<LegCostRecord>), EngineError> {
    let mut cost = Vec::with_capacity(config.universe.len());
    let mut records = Vec::with_capacity(config.universe.len());
    for symbol in config.universe {
        let (leg, record) = resolve_leg(
            costs.file,
            costs.measures,
            profile,
            symbol,
            decision_tf,
            costs.seed,
        )?;
        cost.push(leg);
        records.push(record);
    }
    Ok((cost, records))
}

/// Ce que `play_cost_profile` lit sans le modifier : la préparation et la configuration.
struct Prepared<'p, 'a> {
    loaded: &'p Loaded,
    shared: &'p Shared<'a>,
    checked: &'p Checked,
    config: &'p PortfolioConfig<'p>,
    costs: &'p CostSetup<'p>,
}

/// Résout les coûts d'un profil, construit son objet haché, son `Book`, puis le joue sur la préparation
/// partagée.
fn play_cost_profile(
    prepared: &Prepared<'_, '_>,
    profile: CostProfileName,
    strategies: Vec<Box<dyn Strategy>>,
    decision_tf: Timeframe,
    observer: &mut dyn LoopObserver,
) -> Result<ProfileRun, EngineError> {
    let config = prepared.config;
    let (leg_costs, legs) = resolve_legs(config, prepared.costs, profile, decision_tf)?;
    let run_config = PortfolioRunConfig::with_costs(
        strategies
            .first()
            .map(|s| s.as_ref())
            .ok_or(EngineError::EmptyUniverse)?,
        config,
        prepared.checked.warmup_bars,
        prepared.checked.uses_regime,
        prepared.costs.seed,
        profile,
        &legs,
    )?;
    let config_hash = run_config.hash()?;
    let book = Book::with_costs(
        book_config(config, prepared.loaded.window_max()),
        config.instruments,
        &leg_costs,
    )?;
    let (result, cost_by_leg) = play_profile(
        prepared.shared,
        strategies,
        book,
        config,
        run_config,
        config_hash,
        observer,
    )?;
    Ok(ProfileRun {
        profile,
        result,
        legs,
        cost_by_leg,
    })
}

/// Joue baseline, adverse et maker sur une préparation construite une fois.
///
/// L'observateur est appelé autour de la boucle de **chaque** profil (trois fois `loop_started` puis
/// `loop_finished`), pas autour des trois.
///
/// # Erreurs
///
/// Celles de `run_portfolio`, plus [`EngineError::ScalarSlippageWithCosts`],
/// [`EngineError::StopStressWithCosts`], [`EngineError::NoFeeGrid`],
/// [`EngineError::DecisionTfNotDeclared`], [`EngineError::ProfileStrategiesDiffer`] et les erreurs de
/// résolution des coûts ([`EngineError::Model`]).
pub fn run_portfolio_profiles(
    inputs: ProfilesInputs<'_>,
    observer: &mut dyn LoopObserver,
) -> Result<ProfilesResult, EngineError> {
    let ProfilesInputs {
        series,
        strategies: [baseline, adverse, maker],
        config,
        costs,
    } = inputs;
    check_cost_config(&config)?;
    let checked = check_config(&baseline, &config)?;
    for other in [&adverse, &maker] {
        check_config(other, &config)?;
        if !same_declaration(&baseline, other) {
            return Err(EngineError::ProfileStrategiesDiffer);
        }
    }
    let decision_tfs = [
        decision_tf_of(&baseline)?,
        decision_tf_of(&adverse)?,
        decision_tf_of(&maker)?,
    ];

    // Préparation : une seule fois, comptée ici et nulle part ailleurs.
    let loaded = load(series, &config, checked.uses_regime)?;
    let shared = share(&loaded, &baseline, &config, &checked)?;
    let mut prep = PrepStats::default();
    prep.record(&loaded, &shared);

    let prepared = Prepared {
        loaded: &loaded,
        shared: &shared,
        checked: &checked,
        config: &config,
        costs: &costs,
    };
    let [tf_baseline, tf_adverse, tf_maker] = decision_tfs;
    let run_baseline = play_cost_profile(
        &prepared,
        CostProfileName::Baseline,
        baseline,
        tf_baseline,
        observer,
    )?;
    let run_adverse = play_cost_profile(
        &prepared,
        CostProfileName::Adverse,
        adverse,
        tf_adverse,
        observer,
    )?;
    let run_maker =
        play_cost_profile(&prepared, CostProfileName::Maker, maker, tf_maker, observer)?;
    Ok(ProfilesResult {
        runs: [run_baseline, run_adverse, run_maker],
        prep,
    })
}

/// Joue **un seul** profil, avec sa propre préparation : la référence d'un run isolé (docs S10 §16,
/// « chaque profil identique au bit à un run isolé avec les mêmes coûts »). Rend aussi les compteurs de
/// la préparation, pour la comparaison avec [`run_portfolio_profiles`].
///
/// # Erreurs
///
/// Celles de [`run_portfolio_profiles`], hors `ProfileStrategiesDiffer`.
pub fn run_portfolio_profile(
    inputs: ProfileInputs<'_>,
    profile: CostProfileName,
    observer: &mut dyn LoopObserver,
) -> Result<(ProfileRun, PrepStats), EngineError> {
    let ProfileInputs {
        series,
        strategies,
        config,
        costs,
    } = inputs;
    check_cost_config(&config)?;
    let checked = check_config(&strategies, &config)?;
    let decision_tf = decision_tf_of(&strategies)?;
    let loaded = load(series, &config, checked.uses_regime)?;
    let shared = share(&loaded, &strategies, &config, &checked)?;
    let mut prep = PrepStats::default();
    prep.record(&loaded, &shared);
    let prepared = Prepared {
        loaded: &loaded,
        shared: &shared,
        checked: &checked,
        config: &config,
        costs: &costs,
    };
    let run = play_cost_profile(&prepared, profile, strategies, decision_tf, observer)?;
    Ok((run, prep))
}
