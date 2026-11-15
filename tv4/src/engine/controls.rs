//! Préparation et sondes natives S14. Les propriétaires Loaded/Shared vivent dans
//! `with_prepared_batch` ; sa continuation permet les DP et rejeux sans rechargement.
use std::collections::BTreeMap;
use std::sync::Arc;

use super::portfolio::{self, Checked, Loaded, Shared};
use super::{
    CostSetup, EngineError, PortfolioConfig, PortfolioResult, PrepStats, ProfileRun, ResearchSpec,
};
use crate::model::costs::CostProfileName;
use crate::model::{Timeframe, Ts};
use crate::research::controls::{
    AnnualQuota, ControlMode, ControlOptions, ControlsError, EngineWork, PairCatalogue,
    UnavailableReason,
};
use crate::sim::Trade;
use crate::store::MultiDataset;
use crate::strategy::controls::GeometryState;
use crate::strategy::{Signal, Strategy, StrategyError};

pub struct BatchInputs<'a> {
    pub dataset: Arc<MultiDataset>,
    pub sources: Vec<SourceRuntime<'a>>,
    pub options: ControlOptions,
}
pub struct SourceRuntime<'a> {
    pub config: PortfolioConfig<'a>,
    pub costs: Option<(CostSetup<'a>, CostProfileName)>,
    pub research: Option<&'a ResearchSpec<'a>>,
    pub quotas: BTreeMap<String, Vec<AnnualQuota>>,
    pub factory: &'a dyn Fn() -> Result<Vec<Box<dyn Strategy>>, StrategyError>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedValidation {
    Matched,
    Mismatched,
}
pub enum ObservedRun<'a> {
    Plain(&'a PortfolioResult),
    Profile(&'a ProfileRun),
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error(transparent)]
    Engine(EngineError),
    #[error(transparent)]
    Numeric(#[from] ControlsError),
    #[error("entrée de contrôle invalide : {0}")]
    InvalidInput(String),
    #[error("validation Observed : {0}")]
    Validation(String),
    #[error("contrôle indisponible : {0:?}")]
    Unavailable(UnavailableReason),
}
impl From<EngineError> for ControlError {
    fn from(value: EngineError) -> Self {
        match value {
            EngineError::ControlBudget(reason) => Self::Unavailable(reason),
            other => Self::Engine(other),
        }
    }
}
impl From<StrategyError> for ControlError {
    fn from(value: StrategyError) -> Self {
        Self::Engine(value.into())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlPhase {
    Preparation,
    Observed,
    Probes,
    Planning,
    Replay,
    Statistics,
}
/// Horloge et mesures restent chez l'appelant ; aucun appel par bougie.
pub trait PhaseObserver {
    fn phase_started(&mut self, _phase: ControlPhase) {}
    fn phase_finished(&mut self, _phase: ControlPhase) {}
}
pub struct NoopPhaseObserver;
impl PhaseObserver for NoopPhaseObserver {}

#[derive(Clone, Debug)]
pub struct PreparedSlot {
    pub signal_ms: Ts,
    pub open_ms: Ts,
    pub state: GeometryState,
    pub signal: Signal,
    /// Projection PreserveBtc de l'intention de base RemoveBtc.
    pub preserve_btc: bool,
}
#[derive(Clone, Debug)]
pub struct NativeTrace {
    pub leg: usize,
    pub trade: Trade,
    pub release_ms: Ts,
}
#[derive(Clone, Debug)]
pub struct ProbeOutcome {
    pub decision: usize,
    /// Bougies de jambes visitées, débitées avant chaque pas natif.
    pub steps: u64,
    pub trade: Option<Trade>,
    pub release_ms: Option<Ts>,
}
#[derive(Clone, Debug)]
pub struct PreparedMode {
    pub mode: ControlMode,
    pub alias_of: Option<ControlMode>,
    pub catalogues: Arc<Vec<PairCatalogue>>,
}
/// Données numériques conservées ; les courbes Full sont libérées après le callback.
pub struct PreparedSource<'p, 'a> {
    pub slots: Arc<Vec<Vec<PreparedSlot>>>,
    pub probes: Arc<Vec<Vec<ProbeOutcome>>>,
    pub modes: Vec<PreparedMode>,
    pub observed_trace: Vec<NativeTrace>,
    pub geometry_hash: String,
    /// Identité PortfolioRunConfig native calculée par le rejeu Observed.
    pub parent_config_hash: String,
    pub(super) runtime: &'p SourceRuntime<'a>,
    pub(super) shared: &'p Shared<'p>,
    pub(super) checked: &'p Checked,
    pub(super) declaration: &'p [Box<dyn Strategy>],
    pub(super) loaded: &'p Loaded,
    pub decision_tf: Timeframe,
}
pub struct PreparedBatch<'p, 'a> {
    pub sources: Vec<PreparedSource<'p, 'a>>,
    pub prep: PrepStats,
    pub work: &'p mut EngineWork,
    pub options: &'p ControlOptions,
    pub observer: &'p mut dyn PhaseObserver,
}

fn invalid(message: &str) -> ControlError {
    ControlError::InvalidInput(message.into())
}

/// Frontière lexicale réutilisée par le lot7 : la continuation peut emprunter les
/// préparations mais ne peut pas rendre de vues des bougies. Aucun résultat Full
/// n'est conservé. Un échec de callback reste technique ; Mismatched est numérique.
pub fn with_prepared_batch<T>(
    inputs: BatchInputs<'_>,
    validate_observed: &mut dyn FnMut(usize, ObservedRun<'_>) -> Result<ObservedValidation, String>,
    observer: &mut dyn PhaseObserver,
    continuation: impl FnOnce(&mut PreparedBatch<'_, '_>) -> Result<T, ControlError>,
) -> Result<T, ControlError> {
    let mut work = EngineWork::default();
    with_prepared_batch_with_work(inputs, &mut work, validate_observed, observer, continuation)
}

/// Même portée lexicale avec un compteur détenu par l'appelant : ses débits
/// partiels restent disponibles après un refus de budget ou un ObservedMismatch.
pub fn with_prepared_batch_with_work<T>(
    inputs: BatchInputs<'_>,
    work: &mut EngineWork,
    validate_observed: &mut dyn FnMut(usize, ObservedRun<'_>) -> Result<ObservedValidation, String>,
    observer: &mut dyn PhaseObserver,
    continuation: impl FnOnce(&mut PreparedBatch<'_, '_>) -> Result<T, ControlError>,
) -> Result<T, ControlError> {
    inputs.options.validate()?;
    if inputs
        .sources
        .iter()
        .any(|source| source.config.sizing != crate::sim::SizingMode::Research)
    {
        return Err(invalid("S14 exige le sizing Research à capital fixe"));
    }
    let first = inputs
        .sources
        .first()
        .ok_or_else(|| invalid("panneau vide"))?;
    let mut sets = Vec::with_capacity(inputs.sources.len());
    let mut checks = Vec::with_capacity(inputs.sources.len());
    let mut descriptors = Vec::with_capacity(inputs.sources.len());
    let mut tfs = Vec::with_capacity(inputs.sources.len());
    for source in &inputs.sources {
        check_homogeneous(first, source)?;
        let strategies = (source.factory)()?;
        let checked = portfolio::check_config(&strategies, &source.config)?;
        if let Some(research) = source.research {
            super::research::check_research(research, &source.config)?;
        }
        if source.costs.is_some() {
            super::profiles::check_cost_config(&source.config)?;
        }
        let tf = super::research::decision_tf_of(&strategies)?;
        let mut descriptor = None;
        for strategy in &strategies {
            let geometry = strategy.control_geometry()?;
            let caps = geometry.capabilities();
            if !caps.single_lot
                || caps.partial_exits
                || caps.quantity_dependent
                || !caps.reconstructible
            {
                return Err(invalid("géométrie non compatible avec single_lot"));
            }
            let value = geometry.descriptor();
            if descriptor
                .as_ref()
                .is_some_and(|previous| *previous != value)
            {
                return Err(invalid("géométries différentes entre jambes"));
            }
            descriptor = Some(value);
        }
        descriptors.push(descriptor.ok_or_else(|| invalid("géométrie absente"))?);
        checks.push(checked);
        sets.push(strategies);
        tfs.push(tf);
    }
    observer.phase_started(ControlPhase::Preparation);
    let prepared = (|| -> Result<_, ControlError> {
        let loaded = portfolio::load(
            &inputs.dataset.series,
            &first.config,
            checks.iter().any(|c| c.uses_regime),
        )?;
        // Toutes les conversions sont uniques ; chaque géométrie garde ses agrégats propres.
        let mut shared = Vec::new();
        let mut owners: Vec<usize> = Vec::new();
        let mut groups = Vec::new();
        let mut prep = PrepStats::default();
        for (index, source) in inputs.sources.iter().enumerate() {
            let group = owners.iter().position(|&owner| {
                descriptors[index] == descriptors[owner]
                    && super::profiles::same_declaration(&sets[index], &sets[owner])
            });
            if let Some(group) = group {
                groups.push(group);
                continue;
            }
            let views = if source.research.is_some() {
                portfolio::share_research(&loaded, &sets[index], &source.config, &checks[index])?
            } else {
                portfolio::share(&loaded, &sets[index], &source.config, &checks[index])?
            };
            let converted = prep.candles_converted;
            prep.record(&loaded, &views);
            if !shared.is_empty() {
                prep.candles_converted = converted;
            }
            groups.push(shared.len());
            owners.push(index);
            shared.push(views);
        }
        observer.phase_finished(ControlPhase::Preparation);
        let mut sources = Vec::new();
        for (index, runtime) in inputs.sources.iter().enumerate() {
            let group = groups[index];
            sources.push(PreparedSource {
                slots: Arc::default(),
                probes: Arc::default(),
                modes: Vec::new(),
                observed_trace: Vec::new(),
                parent_config_hash: String::new(),
                geometry_hash: crate::model::hash::hash_json(&descriptors[index])
                    .map_err(EngineError::from)?,
                runtime,
                shared: &shared[group],
                checked: &checks[owners[group]],
                loaded: &loaded,
                declaration: &sets[index],
                decision_tf: tfs[index],
            });
        }
        observer.phase_started(ControlPhase::Observed);
        let observed = (|| -> Result<(), ControlError> {
            for (index, source) in sources.iter_mut().enumerate() {
                let (trace, parent_config_hash) = portfolio::controls::observed(
                    source.runtime,
                    source.loaded,
                    source.shared,
                    source.checked,
                    source.declaration,
                    source.decision_tf,
                    work,
                    &inputs.options.budgets,
                    index,
                    validate_observed,
                )?;
                source.observed_trace = trace;
                source.parent_config_hash = parent_config_hash;
            }
            Ok(())
        })();
        observer.phase_finished(ControlPhase::Observed);
        observed?;
        observer.phase_started(ControlPhase::Probes);
        let probed = (|| -> Result<(), ControlError> {
            for index in 0..sources.len() {
                // Réutilisation uniquement à déclaration et géométrie identiques.
                if let Some(previous) = groups[..index]
                    .iter()
                    .position(|group| *group == groups[index])
                {
                    sources[index].slots = sources[previous].slots.clone();
                    sources[index].probes = sources[previous].probes.clone();
                    sources[index].modes = sources[previous].modes.clone();
                } else {
                    work.begin_geometry();
                    let source = &mut sources[index];
                    let (slots, probes, modes) = portfolio::controls::prepare_catalogues(
                        source.runtime,
                        source.loaded,
                        source.shared,
                        source.declaration,
                        source.decision_tf,
                        &inputs.options,
                        work,
                    )?;
                    source.slots = Arc::new(slots);
                    source.probes = Arc::new(probes);
                    source.modes = modes;
                }
            }
            Ok(())
        })();
        observer.phase_finished(ControlPhase::Probes);
        probed?;
        continuation(&mut PreparedBatch {
            sources,
            prep,
            work,
            options: &inputs.options,
            observer,
        })
    })();
    // Les propriétaires sortent seulement après la continuation.
    prepared
}

fn check_homogeneous(a: &SourceRuntime<'_>, b: &SourceRuntime<'_>) -> Result<(), ControlError> {
    let x = &a.config;
    let y = &b.config;
    if x.universe != y.universe
        || x.instruments != y.instruments
        || x.initial_cash.to_bits() != y.initial_cash.to_bits()
        || x.max_positions != y.max_positions
        || x.sizing != y.sizing
        || x.fees != y.fees
        || x.slippage != y.slippage
        || x.sim_rules != y.sim_rules
        || x.quote_gap_ms != y.quote_gap_ms
        || x.regime_symbol != y.regime_symbol
        || x.min_order_bump_max_risk != y.min_order_bump_max_risk
        || x.instruments_hash != y.instruments_hash
    {
        return Err(invalid(
            "panneau non homogène : portefeuille, coûts ou capital différents",
        ));
    }
    match (a.costs, b.costs) {
        (None, None) => {}
        (Some((x, p)), Some((y, q)))
            if p == q
                && x.seed == y.seed
                && x.file == y.file
                && match (x.measures, y.measures) {
                    (None, None) => true,
                    (Some(x), Some(y)) => x == y,
                    _ => false,
                } => {}
        _ => {
            return Err(invalid(
                "panneau non homogène : profil ou seed de coûts différent",
            ));
        }
    }
    match (a.research, b.research) {
        (None, None) => {}
        (Some(x), Some(y))
            if x.block == y.block
                && x.decision_start_ms == y.decision_start_ms
                && x.decision_end_ms == y.decision_end_ms
                && x.identity.line == y.identity.line
                && x.universe_spec == y.universe_spec
                && x.quality == y.quality
                && match (x.universe, y.universe) {
                    (None, None) => true,
                    (Some(x), Some(y)) => x == y,
                    _ => false,
                } => {}
        _ => {
            return Err(invalid(
                "panneau non homogène : ligne, fenêtre ou filtres de recherche différents",
            ));
        }
    }
    Ok(())
}
