//! Quotas, calendriers et budgets déterministes des contrôles S14.
//!
//! Les années et le statut conservé sont qualifiés par l'appelant à partir du vrai fill et de
//! `model::blocks::is_purged`. La libération n'est pas l'instant économique de sortie : ces types
//! ne tentent donc pas d'en déduire la purge ni de convertir un timestamp en année UTC.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnualQuota {
    pub year: i32,
    pub filled: u32,
    pub retained: u32,
}

/// Années strictement croissantes ; zéro est un quota valide et reste contraignant.
/// Un seul lot terminal purgé est possible sur toute la fenêtre d'une paire single_lot.
/// Une purge suivie d'une année à fills positifs contredit ce contrat et constitue une entrée
/// invalide, plutôt qu'un support vide. Les années suivantes à quota nul restent autorisées.
pub fn validate_quotas(quotas: &[AnnualQuota]) -> Result<(), PlanningFailure> {
    let mut previous_year = None;
    let mut purged = false;
    for quota in quotas {
        if previous_year.is_some_and(|year| quota.year <= year) {
            return Err(invalid(
                "les années des quotas doivent être strictement croissantes",
            ));
        }
        if quota.retained > quota.filled {
            return Err(invalid("le quota conservé dépasse le quota rempli"));
        }
        if purged && quota.filled > 0 {
            return Err(invalid(
                "aucun fill ne peut suivre l'année de la purge terminale",
            ));
        }
        let difference = quota.filled - quota.retained;
        if difference > 1 || (difference == 1 && purged) {
            return Err(invalid(
                "une paire ne peut avoir qu'une seule purge terminale",
            ));
        }
        purged |= difference == 1;
        previous_year = Some(quota.year);
    }
    Ok(())
}

/// Uniquement des lots réellement remplis. Les indices restent en mémoire ; la projection JSON
/// doit convertir les usize en u64 de façon vérifiée. Aucun poids de performance ici.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interval {
    pub id: u32,
    pub decision: usize,
    pub signal_ms: i64,
    pub entry_ms: i64,
    pub release_ms: i64,
    pub next_decision: usize,
    pub fill_year: i32,
    pub retained: bool,
    pub clocks_hash: String,
}

impl Interval {
    /// `signal_ms` est l'horloge du slot décisionnel ; la libération à la même clôture qu'une
    /// décision suivante est permise. Les indices n'ont pas besoin d'être contigus.
    pub fn validate(&self) -> Result<(), PlanningFailure> {
        if self.signal_ms > self.entry_ms || self.entry_ms > self.release_ms {
            return Err(invalid(
                "les horloges doivent respecter signal ≤ entrée ≤ libération",
            ));
        }
        if self.next_decision <= self.decision {
            return Err(invalid(
                "la décision suivante doit suivre strictement la décision",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairCatalogue {
    pub symbol: String,
    pub intervals: Vec<Interval>,
}

impl PairCatalogue {
    /// Un candidat par décision, IDs et horloges de signal strictement croissants. Les fills et
    /// leurs années peuvent être non monotones parmi les candidats, notamment en maker.
    /// `next_decision` et `release_ms` doivent rendre compatible le même premier candidat ; les
    /// slots absents du catalogue ne contraignent pas la valeur exacte de l'indice suivant.
    pub fn validate(&self) -> Result<(), PlanningFailure> {
        if self.symbol.trim().is_empty() {
            return Err(invalid("le symbole du catalogue est vide"));
        }
        for interval in &self.intervals {
            interval.validate()?;
        }
        for pair in self.intervals.windows(2) {
            if pair[0].id >= pair[1].id
                || pair[0].decision >= pair[1].decision
                || pair[0].signal_ms >= pair[1].signal_ms
            {
                return Err(invalid(
                    "IDs, décisions et signaux du catalogue doivent être strictement croissants",
                ));
            }
        }
        for interval in &self.intervals {
            let by_decision = self
                .intervals
                .partition_point(|candidate| candidate.decision < interval.next_decision);
            let by_release = self
                .intervals
                .partition_point(|candidate| candidate.signal_ms < interval.release_ms);
            // Même en cas de fill/libération instantanés, une décision ne peut pas se reprendre
            // elle-même : seules les décisions strictement postérieures sont des continuations.
            let after_current = self
                .intervals
                .partition_point(|candidate| candidate.decision <= interval.decision);
            if by_decision != by_release.max(after_current) {
                return Err(invalid(
                    "la décision suivante est incohérente avec l'horloge de libération",
                ));
            }
            if !interval.retained && by_decision < self.intervals.len() {
                return Err(invalid(
                    "un intervalle purgé terminal ne peut avoir de continuation",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Calendar {
    pub interval_ids: Vec<u32>,
}

impl Calendar {
    /// Vérifie le calendrier choisi : IDs connus ordonnés, paire libre avant chaque décision,
    /// fills annuels chronologiques et quotas exacts. Toute purge choisie doit être terminale.
    /// Les calendriers vides satisfont uniquement des quotas nuls (ou absents).
    pub fn validate(
        &self,
        catalogue: &PairCatalogue,
        quotas: &[AnnualQuota],
    ) -> Result<(), PlanningFailure> {
        catalogue.validate()?;
        validate_quotas(quotas)?;
        let mut actual = vec![(0_u32, 0_u32); quotas.len()];
        let mut previous: Option<&Interval> = None;
        for id in &self.interval_ids {
            let index = catalogue
                .intervals
                .binary_search_by_key(id, |item| item.id)
                .map_err(|_| invalid("le calendrier référence un intervalle absent"))?;
            let interval = &catalogue.intervals[index];
            if let Some(prior) = previous
                && (interval.id <= prior.id
                    || interval.decision < prior.next_decision
                    || interval.signal_ms < prior.release_ms
                    || interval.fill_year < prior.fill_year
                    || !prior.retained)
            {
                return Err(invalid(
                    "le calendrier contient un chevauchement, un ordre invalide ou une purge non terminale",
                ));
            }
            let year_index = quotas
                .binary_search_by_key(&interval.fill_year, |quota| quota.year)
                .map_err(|_| invalid("l'année du fill ne possède aucun quota"))?;
            let counts = &mut actual[year_index];
            counts.0 = counts
                .0
                .checked_add(1)
                .ok_or_else(|| invalid("le compte des fills déborde"))?;
            if interval.retained {
                counts.1 = counts
                    .1
                    .checked_add(1)
                    .ok_or_else(|| invalid("le compte des lots conservés déborde"))?;
            }
            previous = Some(interval);
        }
        if quotas
            .iter()
            .zip(actual)
            .any(|(quota, (filled, retained))| quota.filled != filled || quota.retained != retained)
        {
            return Err(invalid(
                "le calendrier ne respecte pas les quotas remplis et conservés",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub probe_steps_per_geometry: u64,
    pub dp_cells_per_geometry: u64,
    pub dp_bytes_per_geometry: u64,
    pub attempts_per_replication: u32,
    pub replay_steps_per_replication: u64,
    pub total_engine_steps: u64,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            probe_steps_per_geometry: 10_000_000_000,
            dp_cells_per_geometry: 2_000_000,
            dp_bytes_per_geometry: 268_435_456,
            attempts_per_replication: 128,
            replay_steps_per_replication: 5_000_000_000,
            total_engine_steps: 500_000_000_000,
        }
    }
}

/// Identité du programme : les cinq budgets propres, sans le plafond global de l'invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramBudgets {
    pub probe_steps_per_geometry: u64,
    pub dp_cells_per_geometry: u64,
    pub dp_bytes_per_geometry: u64,
    pub attempts_per_replication: u32,
    pub replay_steps_per_replication: u64,
}

impl From<&Budgets> for ProgramBudgets {
    fn from(value: &Budgets) -> Self {
        Self {
            probe_steps_per_geometry: value.probe_steps_per_geometry,
            dp_cells_per_geometry: value.dp_cells_per_geometry,
            dp_bytes_per_geometry: value.dp_bytes_per_geometry,
            attempts_per_replication: value.attempts_per_replication,
            replay_steps_per_replication: value.replay_steps_per_replication,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    PreserveBtc,
    RemoveBtc,
}

impl ControlMode {
    /// Sans filtre BTC, les deux modes désignent exactement le même programme.
    pub const fn effective(self, has_btc_filter: bool) -> Self {
        if has_btc_filter {
            self
        } else {
            Self::PreserveBtc
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlOptions {
    pub seed: u64,
    pub replications: u32,
    pub modes: Vec<ControlMode>,
    pub budgets: Budgets,
}

impl ControlOptions {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            replications: 200,
            modes: vec![ControlMode::PreserveBtc, ControlMode::RemoveBtc],
            budgets: Budgets::default(),
        }
    }

    /// Les budgets nuls sont autorisés : leur épuisement est une indisponibilité explicite.
    pub fn validate(&self) -> Result<(), ControlsError> {
        if self.replications == 0 || self.modes.is_empty() {
            return Err(ControlsError::InvalidInput(
                "K doit être positif et au moins un mode est requis".into(),
            ));
        }
        let mut modes = self.modes.clone();
        modes.sort_unstable();
        modes.dedup();
        if modes.len() != self.modes.len() {
            return Err(ControlsError::InvalidInput(
                "les modes demandés doivent être distincts".into(),
            ));
        }
        Ok(())
    }

    /// Ordre canonique et dédoublonnage après résolution des aliases ; les modes demandés restent
    /// disponibles dans `modes` pour publier leur lien d'alias explicite.
    pub fn effective_modes(&self, has_btc_filter: bool) -> Vec<ControlMode> {
        let mut modes: Vec<_> = self
            .modes
            .iter()
            .map(|mode| mode.effective(has_btc_filter))
            .collect();
        modes.sort_unstable();
        modes.dedup();
        modes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellAvailability {
    Complete,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelStatus {
    Complete,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    EmptySupport,
    PreparationBudget,
    PlanningBudget,
    AttemptsBudget,
    ReplayBudget,
    GlobalEngineBudget,
    ObservedMismatch,
    ObservedOutsideSupport,
    MissingPositiveRisk,
    NoRetainedTrades,
    InsufficientMonths,
    BootstrapZeroDenominator,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlanningFailure {
    #[error("entrée de planification invalide : {0}")]
    InvalidInput(String),
    #[error("aucun calendrier compatible")]
    EmptySupport,
    /// Compteurs effectivement réservés avant l'opération refusée, sans saturation.
    #[error(
        "budget de planification épuisé après {cells} cellules et {logical_bytes} octets logiques"
    )]
    Budget { cells: u64, logical_bytes: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ControlsError {
    #[error("entrée de contrôle invalide : {0}")]
    InvalidInput(String),
    #[error("valeur numérique non finie")]
    NonFinite,
    #[error(transparent)]
    Planning(#[from] PlanningFailure),
}

fn invalid(message: &str) -> PlanningFailure {
    PlanningFailure::InvalidInput(message.into())
}

/// Un compteur par géométrie, partagé entre ses paires et modes effectifs. L'appelant débite avant
/// d'allouer/calculer les comptes. Une nouvelle géométrie reçoit un nouveau compteur.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanningWork {
    pub cells: u64,
    pub logical_bytes: u64,
}

impl PlanningWork {
    /// Réservation transactionnelle : les deux additions et plafonds passent avant mutation.
    pub fn charge(
        &mut self,
        cells: u64,
        logical_bytes: u64,
        budgets: &Budgets,
    ) -> Result<(), PlanningFailure> {
        let failure = || PlanningFailure::Budget {
            cells: self.cells,
            logical_bytes: self.logical_bytes,
        };
        let next_cells = self.cells.checked_add(cells).ok_or_else(failure)?;
        let next_bytes = self
            .logical_bytes
            .checked_add(logical_bytes)
            .ok_or_else(failure)?;
        if next_cells > budgets.dp_cells_per_geometry || next_bytes > budgets.dp_bytes_per_geometry
        {
            return Err(failure());
        }
        self.cells = next_cells;
        self.logical_bytes = next_bytes;
        Ok(())
    }
}

/// Cumuls d'invocation et compteurs locaux distincts. Aucun temps machine n'entre dans les budgets.
/// Les appels `charge_*` doivent précéder l'opération, jamais la mesurer après exécution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineWork {
    pub probe_steps: u64,
    pub replay_steps: u64,
    pub total_steps: u64,
    pub planning_cells: u64,
    pub planning_logical_bytes: u64,
    geometry_probe_steps: u64,
    replication_replay_steps: u64,
    replication_attempts: u32,
}

impl EngineWork {
    /// Au début d'une géométrie, avant toutes ses sondes ; les cumuls restent intacts.
    pub fn begin_geometry(&mut self) {
        self.geometry_probe_steps = 0;
    }

    /// Au début d'une réplication, avant sa première tentative. Ne pas appeler entre tentatives.
    pub fn begin_replication(&mut self) {
        self.replication_replay_steps = 0;
        self.replication_attempts = 0;
    }

    pub fn charge_probe(&mut self, steps: u64, budgets: &Budgets) -> Result<(), UnavailableReason> {
        let local = bounded_add(
            self.geometry_probe_steps,
            steps,
            budgets.probe_steps_per_geometry,
            UnavailableReason::PreparationBudget,
        )?;
        let cumulative = bounded_add(
            self.probe_steps,
            steps,
            u64::MAX,
            UnavailableReason::PreparationBudget,
        )?;
        let total = bounded_add(
            self.total_steps,
            steps,
            budgets.total_engine_steps,
            UnavailableReason::GlobalEngineBudget,
        )?;
        self.geometry_probe_steps = local;
        self.probe_steps = cumulative;
        self.total_steps = total;
        Ok(())
    }

    pub fn charge_replay(
        &mut self,
        steps: u64,
        budgets: &Budgets,
    ) -> Result<(), UnavailableReason> {
        let local = bounded_add(
            self.replication_replay_steps,
            steps,
            budgets.replay_steps_per_replication,
            UnavailableReason::ReplayBudget,
        )?;
        let cumulative = bounded_add(
            self.replay_steps,
            steps,
            u64::MAX,
            UnavailableReason::ReplayBudget,
        )?;
        let total = bounded_add(
            self.total_steps,
            steps,
            budgets.total_engine_steps,
            UnavailableReason::GlobalEngineBudget,
        )?;
        self.replication_replay_steps = local;
        self.replay_steps = cumulative;
        self.total_steps = total;
        Ok(())
    }

    /// Pas moteur hors sonde/rejeu contrôlé, notamment Observed : consomme seulement le plafond
    /// global, sans fausser les diagnostics de phase ni les quotas locaux.
    pub fn charge_engine(
        &mut self,
        steps: u64,
        budgets: &Budgets,
    ) -> Result<(), UnavailableReason> {
        self.total_steps = bounded_add(
            self.total_steps,
            steps,
            budgets.total_engine_steps,
            UnavailableReason::GlobalEngineBudget,
        )?;
        Ok(())
    }

    pub fn charge_attempt(&mut self, budgets: &Budgets) -> Result<(), UnavailableReason> {
        let next = self
            .replication_attempts
            .checked_add(1)
            .ok_or(UnavailableReason::AttemptsBudget)?;
        if next > budgets.attempts_per_replication {
            return Err(UnavailableReason::AttemptsBudget);
        }
        self.replication_attempts = next;
        Ok(())
    }
}

fn bounded_add(
    current: u64,
    debit: u64,
    limit: u64,
    reason: UnavailableReason,
) -> Result<u64, UnavailableReason> {
    current
        .checked_add(debit)
        .filter(|next| *next <= limit)
        .ok_or(reason)
}

/// Réduction possédée d'un journal ; les mois sans trade restent présents.
#[derive(Clone, Debug, PartialEq)]
pub struct MonthMoment {
    pub year: i32,
    pub month: u8,
    pub r_sum: f64,
    pub count: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonthCoverageStatus {
    Complete,
    Partial,
    Excluded,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthCoverage {
    pub year: i32,
    pub month: u8,
    pub status: MonthCoverageStatus,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairQuotaCheck {
    pub symbol: String,
    pub expected: Vec<AnnualQuota>,
    pub actual: Vec<AnnualQuota>,
    pub conformant: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayRejection {
    OccupiedPair,
    TraceMismatch,
    QuotaMismatch,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRejection {
    pub attempt: u32,
    pub reason: ReplayRejection,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CellSummary {
    pub replication: u32,
    pub accepted_attempt: Option<u32>,
    pub attempts: u32,
    pub availability: CellAvailability,
    pub reason: Option<UnavailableReason>,
    pub score: Option<f64>,
    pub months: Vec<MonthMoment>,
    pub quota_checks: Vec<PairQuotaCheck>,
    pub quantity_changed: bool,
    pub replay_trace_hash: String,
    pub rejections: Vec<AttemptRejection>,
}
/// Cardinalité exacte en décimal canonique, sans conserver les tables DP.
/// `Some("0")` prouve un support vide ; `None` signifie un calcul non terminé ou non atteint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairSupportSummary {
    pub symbol: String,
    pub cardinality: Option<String>,
}
/// Les aliases partagent physiquement catalogues, supports et cellules ; le DTO sérialise leurs tranches.
#[derive(Clone, Debug, PartialEq)]
pub struct ModeSummary {
    pub mode: ControlMode,
    pub alias_of: Option<ControlMode>,
    pub catalogues: std::sync::Arc<Vec<PairCatalogue>>,
    pub pair_supports: std::sync::Arc<Vec<PairSupportSummary>>,
    /// Échec conjoint de planification ; les cardinalités antérieures restent disponibles.
    pub planning_reason: Option<UnavailableReason>,
    pub cells: std::sync::Arc<Vec<CellSummary>>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SourceSummary {
    pub quotas: std::collections::BTreeMap<String, Vec<AnnualQuota>>,
    pub observed_score: Option<f64>,
    pub observed_reason: Option<UnavailableReason>,
    pub observed_months: Vec<MonthMoment>,
    pub month_coverage: Vec<MonthCoverage>,
    pub modes: Vec<ModeSummary>,
    pub planning_work: PlanningWork,
}

/// Estimation temporelle approximative : tous les fragments mensuels restent inclus.
/// `point` reste descriptif quand la couverture interdit l'estimation d'incertitude.
#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapSummary {
    pub available: bool,
    pub reason: Option<UnavailableReason>,
    pub approximate: bool,
    pub samples: u32,
    pub months: u32,
    pub complete_months: u32,
    pub point: Option<f64>,
    pub standard_error: Option<f64>,
    pub lb90: Option<f64>,
    pub lb90_positive: Option<bool>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SourceStatistics {
    pub score: Option<f64>,
    pub control_mean: Option<f64>,
    pub delta: Option<f64>,
    pub mc_standard_error: Option<f64>,
    pub constant_controls: bool,
    pub exceedances: Option<u32>,
    pub ties: Option<u32>,
    pub p_value: Option<f64>,
    pub delta_at_least_010: Option<bool>,
    pub above_critical: Option<bool>,
    /// Les modes aliases d'un parent partagent la même estimation physique.
    pub temporal: Option<std::sync::Arc<BootstrapSummary>>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PanelStatistics {
    pub status: PanelStatus,
    pub n_grid: u32,
    pub replications: u32,
    pub smoke: bool,
    /// Admissibilité numérique seulement ; jamais un verdict G1.
    pub numeric_admissible: bool,
    pub resolution: f64,
    /// Rang base 1, calculé en entiers, même lorsque ce rang dépasse K.
    pub critical_rank: u64,
    pub critical_value: Option<f64>,
    /// Vide si le panneau requis est incomplet ; aucun maximum sur un sous-panneau.
    pub maxima: Vec<f64>,
    pub family_score: Option<f64>,
    pub family_exceedances: Option<u32>,
    pub family_p_value: Option<f64>,
    pub sources: Vec<SourceStatistics>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ModeStatistics {
    pub mode: ControlMode,
    pub alias_of: Option<ControlMode>,
    pub panel: PanelStatistics,
}
