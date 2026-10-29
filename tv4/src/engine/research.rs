//! Mode recherche du moteur portefeuille (docs S11 §8).
//!
//! Le mode recherche ajoute à la boucle de S9, sans la modifier hors recherche (les goldens
//! `portfolio_2legs` et `portfolio_profiles` le prouvent), quatre choses :
//!
//! 1. **Activation par jambe** (§8.2). La jambe `k` est vivante à partir de `live_k = max(fin de son
//!    warmup, prêt du régime)`. La grille commence à la plus précoce des jambes vivantes. Une jambe qui ne
//!    devient jamais vivante (listée trop tard pour son warmup) n'est pas une erreur : elle n'est jamais
//!    jouée, `live_from` est vide dans [`ResearchResult`].
//! 2. **Enregistrement à partir de `decision_start`** (§8.2). Les minutes d'`open_time < decision_start`
//!    sont jouées (stratégies, indicateurs, échéances) mais ni la grille, ni l'equity, ni
//!    `bars_in_position` ne les enregistrent : l'exposition, les jours de régime et l'annualisation
//!    portent sur la fenêtre de décision seule.
//! 3. **Filtre des entrées** (§8.3), à l'étape 5, à `now_ms` : fenêtre de décision, appartenance au
//!    trimestre, exclusion qualité du TF de décision. Un refus est compté, jamais silencieux. Les
//!    signaux de sortie, `MoveStop` et annulations ne sont jamais filtrés.
//! 4. **Annulation des entrées en attente** d'une jambe à plat qui sort de l'univers (§8.3), à l'étape 1,
//!    après les trous de cotation et avant tout `on_bar`, pour toutes les jambes dans l'ordre du rang.
//!
//! # Identité (§8.6)
//!
//! Le `config_hash` d'un run de recherche reste celui de la configuration gelée : le bloc `research` de
//! [`PortfolioRunConfig`](super::PortfolioRunConfig) ne porte ni bloc, ni fenêtre, ni membres, ni ligne,
//! et les tables par jambe y sont vidées. Ce qui distingue deux runs de même `config_hash` est
//! [`ResearchIdentity`], hachée à part ([`ResearchIdentity::hash`]).
//!
//! # Écarts à la spécification (voir aussi la doc de [`ResearchSpec`])
//!
//! 1. [`ResearchSpec`] porte un champ de plus, `universe_spec`, qui donne le bloc `universe_spec` du
//!    `config_hash` : le moteur ne peut pas le déduire (symboles demandés avant le filtre d'une ligne,
//!    `rules_hash` des règles de sélection).
//! 2. Le champ `legs_hash` de l'identité fournie est **ignoré** et recalculé par le moteur
//!    ([`legs_hash`]) : avec des profils de coûts, les valeurs de coûts par jambe sont résolues dans le
//!    moteur, après l'appelant. L'identité rendue dans [`ResearchResult`] porte la valeur du moteur.
//! 3. La fenêtre de décision est contrôlée de façon souple contre le bloc : début dans le bloc, fin au
//!    plus à la fin du bloc. `decision_window` (model) reste la source de la fenêtre côté CLI.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::blocks::Block;
use crate::model::costs::LegCostHashed;
use crate::model::universe::{QualitySchedule, UniverseSchedule};
use crate::model::{InstrumentRules, ModelError, Timeframe, Ts, hash};
use crate::store::time::civil_from_days;
use crate::strategy::Strategy;

use super::EngineError;
use super::portfolio_config::{PortfolioConfig, UniverseSpec};

/// Préfixe des hashes d'identité, comme `quality_hash` et `window_hash` (`blake3:` + hexadécimal).
const HASH_PREFIX: &str = "blake3:";

/// Durée d'un jour en millisecondes (le jour UTC n'a pas de saut).
const DAY_MS: Ts = 86_400_000;

/// Ligne d'un run de recherche (docs S11 §9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Line {
    /// Toutes les jambes de l'union.
    Full,
    /// Les jambes dont la base est tradable sur OKX en USDC (sous-ensemble figé).
    OkxUsdc,
}

impl Line {
    /// Nom écrit dans les rapports : `full` ou `okx_usdc`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Line::Full => "full",
            Line::OkxUsdc => "okx_usdc",
        }
    }
}

/// Identité de recherche (docs S11 §8.6) : ce qui distingue deux runs de même `config_hash` sur le même
/// `dataset_hash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResearchIdentity {
    pub block: Block,
    pub decision_start_ms: Ts,
    pub decision_end_ms: Ts,
    /// `window_hash` de l'univers du manifeste, `None` (`null`) en univers fixe.
    pub universe_window_hash: Option<String>,
    /// `blake3:` + `hash_json` de `quality_exclusions` du manifeste.
    pub quality_hash: String,
    pub line: Line,
    /// Hash du sous-ensemble de la ligne `okx_usdc`, `None` pour `full`.
    pub subset_hash: Option<String>,
    /// `blake3:` + hash des valeurs par jambe ([`legs_hash`]). Recalculé par le moteur.
    pub legs_hash: String,
}

impl ResearchIdentity {
    /// `"blake3:" + hash_json` de l'identité : l'`research_identity_hash` de `run_id_s11`.
    ///
    /// # Erreurs
    ///
    /// Flottant non fini (impossible ici) ou sérialisation impossible.
    pub fn hash(&self) -> Result<String, ModelError> {
        Ok(format!("{HASH_PREFIX}{}", hash::hash_json(self)?))
    }
}

/// Objet haché de [`legs_hash`] : les valeurs par jambe que le `config_hash` de recherche ne porte plus.
#[derive(Serialize)]
struct LegsHashed<'a> {
    /// L'ordre joué, non trié (il départage les rangs égaux).
    universe: &'a [String],
    instruments: BTreeMap<&'a str, &'a InstrumentRules>,
    instruments_hash: Option<&'a str>,
    costs_legs: &'a BTreeMap<String, LegCostHashed>,
}

/// `"blake3:" + hash_json` des valeurs par jambe d'un run de recherche : jambes dans l'ordre joué, règles
/// d'instrument par symbole, hash du fichier d'instruments et, avec des profils de coûts, valeurs de
/// coûts résolues par jambe (vide hors profils).
///
/// # Erreurs
///
/// Flottant non fini dans une règle, ou sérialisation impossible.
pub fn legs_hash(
    universe: &[String],
    instruments: &[InstrumentRules],
    instruments_hash: Option<&str>,
    costs_legs: &BTreeMap<String, LegCostHashed>,
) -> Result<String, ModelError> {
    let mut by_symbol: BTreeMap<&str, &InstrumentRules> = BTreeMap::new();
    for (symbol, rules) in universe.iter().zip(instruments) {
        by_symbol.insert(symbol.as_str(), rules);
    }
    let hashed = LegsHashed {
        universe,
        instruments: by_symbol,
        instruments_hash,
        costs_legs,
    };
    Ok(format!("{HASH_PREFIX}{}", hash::hash_json(&hashed)?))
}

/// Entrées de recherche des points d'entrée [`run_portfolio_research`](super::run_portfolio_research) et
/// [`run_portfolio_profiles_research`](super::run_portfolio_profiles_research). `PortfolioConfig` et
/// `PortfolioInputs` ne changent pas.
#[derive(Clone, Copy, Debug)]
pub struct ResearchSpec<'a> {
    pub block: Block,
    /// Début de la décision : aucune entrée n'est acceptée avant.
    pub decision_start_ms: Ts,
    /// Fin de la décision : aucune entrée n'est acceptée à `now_ms ≥ decision_end_ms`.
    pub decision_end_ms: Ts,
    /// `None` : univers fixe (les jambes de la config). `Some` : univers trimestriel, dont chaque
    /// trimestre qui recoupe la fenêtre de décision doit être présent.
    pub universe: Option<&'a UniverseSchedule>,
    /// Forme de l'univers pour le `config_hash` (écart n° 1 du module).
    pub universe_spec: &'a UniverseSpec,
    /// Exclusions qualité du manifeste, pour toutes les jambes, univers fixe compris.
    pub quality: &'a QualitySchedule,
    /// Identité de recherche calculée par l'appelant. Son `legs_hash` est ignoré et recalculé (écart
    /// n° 2 du module).
    pub identity: &'a ResearchIdentity,
}

/// Compteurs de recherche d'une jambe (docs S11 §8.3). `SignalRejections` et `LegResult` ne changent pas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResearchLegCounters {
    /// Entrées refusées parce que `now_ms` est hors de la fenêtre de décision.
    pub entries_outside_decision_window: u64,
    /// Entrées refusées parce que la jambe n'est pas membre du trimestre de `now_ms`.
    pub entries_not_in_universe: u64,
    /// Entrées refusées parce que le TF de décision est exclu par la qualité pour ce trimestre.
    pub entries_excluded_quality: u64,
    /// Ordres d'entrée en attente annulés quand la jambe, à plat, sort de l'univers (aussi comptés dans
    /// `orders_cancelled` du simulateur).
    pub entries_cancelled_universe: u64,
}

impl ResearchLegCounters {
    /// Somme des trois refus, c'est-à-dire ce qu'ajoute la recherche à l'identité de l'entonnoir
    /// d'entrée : `entry_signals = entry_orders + entry_rejections.total() + max_positions_rejected +
    /// refused()`.
    pub fn refused(&self) -> u64 {
        self.entries_outside_decision_window
            + self.entries_not_in_universe
            + self.entries_excluded_quality
    }

    fn add(&mut self, other: &ResearchLegCounters) {
        self.entries_outside_decision_window += other.entries_outside_decision_window;
        self.entries_not_in_universe += other.entries_not_in_universe;
        self.entries_excluded_quality += other.entries_excluded_quality;
        self.entries_cancelled_universe += other.entries_cancelled_universe;
    }
}

/// Une jambe dans [`ResearchResult`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResearchLegResult {
    pub symbol: String,
    /// `open_time` de la première minute jouée de la jambe, `None` si elle n'est jamais devenue vivante.
    pub live_from_ms: Option<Ts>,
    pub counters: ResearchLegCounters,
}

/// Ce que le moteur rend d'un run de recherche (docs S11 §8.4), dans `PortfolioResult::research`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResearchResult {
    pub block: Block,
    pub decision_start_ms: Ts,
    pub decision_end_ms: Ts,
    /// TF de décision de la stratégie, celui dont les exclusions qualité ont été lues.
    pub decision_tf: Timeframe,
    /// Forme de l'univers, telle que le `config_hash` la porte.
    pub universe_spec: UniverseSpec,
    /// Identité de recherche, `legs_hash` recalculé par le moteur.
    pub identity: ResearchIdentity,
    /// `identity.hash()`.
    pub identity_hash: String,
    /// Une entrée par jambe, dans l'ordre de l'univers du run.
    pub legs: Vec<ResearchLegResult>,
    /// Somme des compteurs des jambes.
    pub totals: ResearchLegCounters,
    /// Vrai si une stratégie lit le régime (`Strategy::uses_regime`).
    pub regime_used: bool,
}

// ---------------------------------------------------------------------------------------------
// Points d'entrée : validation
// ---------------------------------------------------------------------------------------------

/// TF de décision d'un jeu de stratégies : celui de la première, déclaré dans ses timeframes, et le même
/// pour toutes les jambes (docs S10 §4.5).
///
/// # Erreurs
///
/// Jeu vide, TF de décision absent des timeframes déclarés, ou TF de décision différent d'une jambe à
/// l'autre ([`EngineError::StrategiesDiffer`]).
pub(super) fn decision_tf_of(strategies: &[Box<dyn Strategy>]) -> Result<Timeframe, EngineError> {
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

/// Contrôles de la recherche, hors boucle : règle de volume nul, fenêtre de décision contre le bloc,
/// trimestres de l'univers présents sur toute la fenêtre.
///
/// # Erreurs
///
/// [`EngineError::ResearchNeedsZeroVolumeRule`], [`EngineError::ResearchWindow`],
/// [`EngineError::UniverseQuarterMissing`].
pub(super) fn check_research(
    spec: &ResearchSpec<'_>,
    config: &PortfolioConfig<'_>,
) -> Result<(), EngineError> {
    if !config.sim_rules.zero_volume_no_fill {
        return Err(EngineError::ResearchNeedsZeroVolumeRule);
    }
    let span = spec.block.span();
    let window = |reason: &'static str| EngineError::ResearchWindow {
        block: spec.block,
        reason,
    };
    if spec.decision_end_ms <= spec.decision_start_ms {
        return Err(window("la fenêtre de décision est vide ou inversée"));
    }
    if spec.decision_start_ms < span.start_ms {
        return Err(window("la fenêtre de décision commence avant le bloc"));
    }
    if span.end_ms.is_some_and(|end| spec.decision_end_ms > end) {
        return Err(window("la fenêtre de décision finit après la fin du bloc"));
    }
    if let Some(schedule) = spec.universe
        && let Some(gap) = schedule.first_gap(spec.decision_start_ms, spec.decision_end_ms)
    {
        return Err(EngineError::UniverseQuarterMissing {
            quarter: quarter_label(gap),
        });
    }
    Ok(())
}

/// Libellé `AAAA-Qn` du trimestre civil UTC qui contient `ts`.
fn quarter_label(ts: Ts) -> String {
    let (year, month, _) = civil_from_days(ts.div_euclid(DAY_MS));
    format!("{year}-Q{}", (month - 1) / 3 + 1)
}

// ---------------------------------------------------------------------------------------------
// État de la boucle
// ---------------------------------------------------------------------------------------------

/// Intervalles `[début, fin)` triés et disjoints, lus par un curseur qui avance (le temps est croissant) :
/// lecture O(1) amortie.
#[derive(Clone, Debug, Default)]
struct Spans {
    spans: Vec<(Ts, Ts)>,
    cursor: usize,
}

impl Spans {
    /// Ajoute `[start, end)` en fusionnant avec le dernier intervalle s'il le touche. Les appels se
    /// font dans l'ordre chronologique.
    fn push(&mut self, start: Ts, end: Ts) {
        if let Some(last) = self.spans.last_mut()
            && last.1 >= start
        {
            last.1 = last.1.max(end);
            return;
        }
        self.spans.push((start, end));
    }

    /// Vrai si `ts` est dans un intervalle. `ts` ne doit pas décroître d'un appel à l'autre.
    fn contains(&mut self, ts: Ts) -> bool {
        while let Some(&(_, end)) = self.spans.get(self.cursor) {
            if end <= ts {
                self.cursor += 1;
            } else {
                break;
            }
        }
        self.spans
            .get(self.cursor)
            .is_some_and(|&(start, _)| start <= ts)
    }

    /// Lecture sans curseur (construction de la table des changements).
    fn contains_at(&self, ts: Ts) -> bool {
        let index = self.spans.partition_point(|&(_, end)| end <= ts);
        self.spans.get(index).is_some_and(|&(start, _)| start <= ts)
    }
}

/// Éligibilité d'une jambe : appartenance à l'univers et exclusion qualité du TF de décision.
#[derive(Clone, Debug)]
struct LegEligibility {
    /// Univers fixe : toujours membre, `member` est alors vide et inutilisé.
    always_member: bool,
    member: Spans,
    /// Trimestres où le TF de décision est exclu pour la jambe.
    excluded: Spans,
    /// Changements de l'éligibilité `(instant, éligible)` dans l'ordre : lus aux minutes de la grille
    /// (étape 1) pour détecter la sortie de l'univers, une comparaison par jambe et par minute.
    changes: Vec<(Ts, bool)>,
    next_change: usize,
    /// Éligibilité à la dernière minute de grille traitée.
    eligible: bool,
}

/// Motif d'un refus d'entrée.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    OutsideWindow,
    NotInUniverse,
    ExcludedQuality,
}

impl ResearchLegCounters {
    /// Compte un refus.
    pub(super) fn record(&mut self, refusal: Refusal) {
        match refusal {
            Refusal::OutsideWindow => self.entries_outside_decision_window += 1,
            Refusal::NotInUniverse => self.entries_not_in_universe += 1,
            Refusal::ExcludedQuality => self.entries_excluded_quality += 1,
        }
    }
}

/// État de recherche de la boucle : fenêtre de décision et éligibilité de chaque jambe. Construit avant la
/// boucle (les seules allocations), lu sans allocation dans la boucle.
pub(super) struct ResearchState {
    decision_start_ms: Ts,
    decision_end_ms: Ts,
    legs: Vec<LegEligibility>,
}

impl ResearchState {
    /// Table d'éligibilité de chaque jambe de `universe` : intervalles d'appartenance (trimestres qui
    /// recoupent la fenêtre de décision) et intervalles d'exclusion qualité du `decision_tf`.
    pub(super) fn new(
        spec: &ResearchSpec<'_>,
        universe: &[String],
        decision_tf: Timeframe,
    ) -> ResearchState {
        let mut legs = Vec::with_capacity(universe.len());
        for symbol in universe {
            let mut member = Spans::default();
            if let Some(schedule) = spec.universe {
                for quarter in schedule.overlapping(spec.decision_start_ms, spec.decision_end_ms) {
                    if quarter.rank_of(symbol).is_some() {
                        member.push(quarter.start_ms, quarter.end_ms);
                    }
                }
            }
            let mut excluded = Spans::default();
            if decision_tf != Timeframe::M1 {
                for interval in spec.quality.intervals(symbol) {
                    if interval.tf_mask & decision_tf.bit() != 0 {
                        excluded.push(interval.start_ms, interval.end_ms);
                    }
                }
            }
            let always_member = spec.universe.is_none();
            let mut boundaries: Vec<Ts> = member
                .spans
                .iter()
                .chain(&excluded.spans)
                .flat_map(|&(start, end)| [start, end])
                .collect();
            boundaries.sort_unstable();
            boundaries.dedup();
            let eligible_at =
                |ts: Ts| (always_member || member.contains_at(ts)) && !excluded.contains_at(ts);
            let mut changes: Vec<(Ts, bool)> = Vec::new();
            let initial = eligible_at(Ts::MIN);
            let mut previous = initial;
            for &boundary in &boundaries {
                let now = eligible_at(boundary);
                if now != previous {
                    changes.push((boundary, now));
                    previous = now;
                }
            }
            legs.push(LegEligibility {
                always_member,
                member,
                excluded,
                changes,
                next_change: 0,
                eligible: initial,
            });
        }
        ResearchState {
            decision_start_ms: spec.decision_start_ms,
            decision_end_ms: spec.decision_end_ms,
            legs,
        }
    }

    pub(super) fn decision_start_ms(&self) -> Ts {
        self.decision_start_ms
    }

    /// Pose l'éligibilité de chaque jambe à la première minute de grille `t`, sans rien annuler : aucun
    /// ordre n'existe encore.
    pub(super) fn init_at(&mut self, t: Ts) {
        for leg in &mut self.legs {
            while let Some(&(at, eligible)) = leg.changes.get(leg.next_change) {
                if at > t {
                    break;
                }
                leg.eligible = eligible;
                leg.next_change += 1;
            }
        }
    }

    /// Avance l'éligibilité de la jambe `k` à la minute de grille `t` (étape 1). Rend vrai si la jambe
    /// était éligible à la minute précédente et ne l'est plus : ses entrées en attente doivent être
    /// annulées si elle est à plat.
    pub(super) fn leaves_at(&mut self, k: usize, t: Ts) -> bool {
        let Some(leg) = self.legs.get_mut(k) else {
            return false;
        };
        let before = leg.eligible;
        while let Some(&(at, eligible)) = leg.changes.get(leg.next_change) {
            if at > t {
                break;
            }
            leg.eligible = eligible;
            leg.next_change += 1;
        }
        before && !leg.eligible
    }

    /// Filtre d'une entrée de la jambe `k` décidée à `now_ms` (étape 5). `None` si elle est acceptée.
    /// L'ordre des contrôles est celui de la spécification : fenêtre, univers, qualité.
    pub(super) fn refusal(&mut self, k: usize, now_ms: Ts) -> Option<Refusal> {
        if now_ms < self.decision_start_ms || now_ms >= self.decision_end_ms {
            return Some(Refusal::OutsideWindow);
        }
        let leg = self.legs.get_mut(k)?;
        if !leg.always_member && !leg.member.contains(now_ms) {
            return Some(Refusal::NotInUniverse);
        }
        if leg.excluded.contains(now_ms) {
            return Some(Refusal::ExcludedQuality);
        }
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Résultat
// ---------------------------------------------------------------------------------------------

/// Ce qu'une jambe a joué, pour [`build_result`].
pub(super) struct LegOutcome<'a> {
    pub(super) symbol: &'a str,
    pub(super) live_from_ms: Option<Ts>,
    pub(super) counters: ResearchLegCounters,
}

/// Assemble le [`ResearchResult`] d'un run : recalcule `legs_hash`, hache l'identité, somme les
/// compteurs.
///
/// # Erreurs
///
/// [`EngineError::Model`] si une règle d'instrument ou un coût n'est pas sérialisable.
pub(super) fn build_result(
    spec: &ResearchSpec<'_>,
    config: &PortfolioConfig<'_>,
    decision_tf: Timeframe,
    costs_legs: &BTreeMap<String, LegCostHashed>,
    regime_used: bool,
    legs: &[LegOutcome<'_>],
) -> Result<ResearchResult, EngineError> {
    let mut identity = spec.identity.clone();
    identity.legs_hash = legs_hash(
        config.universe,
        config.instruments,
        config.instruments_hash,
        costs_legs,
    )?;
    let identity_hash = identity.hash()?;
    let mut totals = ResearchLegCounters::default();
    let mut results = Vec::with_capacity(legs.len());
    for leg in legs {
        totals.add(&leg.counters);
        results.push(ResearchLegResult {
            symbol: leg.symbol.to_string(),
            live_from_ms: leg.live_from_ms,
            counters: leg.counters,
        });
    }
    let mut universe_spec = spec.universe_spec.clone();
    if let UniverseSpec::Fixed { symbols } = &mut universe_spec {
        symbols.sort();
    }
    Ok(ResearchResult {
        block: spec.block,
        decision_start_ms: spec.decision_start_ms,
        decision_end_ms: spec.decision_end_ms,
        decision_tf,
        universe_spec,
        identity,
        identity_hash,
        legs: results,
        totals,
        regime_used,
    })
}
