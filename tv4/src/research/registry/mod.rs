//! Registre S12 pur : événements append-only, reconstruction et propositions d'admission.
//!
//! Aucun accès au disque, à Git ou à l'horloge. L'appelant relit le journal sous verrou, propose
//! une transaction, puis persiste ses octets avant de charger les séries. [`Registry::with_events`]
//! reconstruit une copie : une erreur ne modifie jamais l'état publié. Le numéro de génération est
//! une colonne explicite du CSV, accompagnée des deux bornes de décision ; le trimestre est dérivé de la date UTC canonique.
//! L'autorisation Git et le calcul du bloc effectif restent à la charge de l'appelant.

mod csv;
mod policy;
pub mod roles;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::model::Ts;
use crate::model::blocks::Block;
use crate::store::time::{civil_from_days, parse_rfc3339_utc};

pub use csv::{decode_csv, encode_events, header_csv};
pub use policy::{Admission, AdmissionBatch, ConfigSpec, Quota, TrialResult};
pub use roles::{RoleCounts, TrialRole, role_from_family};

/// Erreur explicite d'un journal ou d'une proposition ; aucun état n'est publié à l'échec.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("registre de recherche : {0}")]
pub struct RegistryError(pub String);

pub(crate) fn invalid(reason: impl Into<String>) -> RegistryError {
    RegistryError(reason.into())
}

/// Une fenêtre est absente (démo/historique) ou possède deux bornes ordonnées valides.
pub(crate) fn validate_window(start: Option<Ts>, end: Option<Ts>) -> Result<(), RegistryError> {
    match (start, end) {
        (None, None) => Ok(()),
        (Some(start), Some(end))
            if start >= 0 && end > start && end <= crate::store::time::MAX_TS_MS =>
        {
            Ok(())
        }
        _ => Err(invalid(
            "fenêtre de décision absente partiellement, vide ou hors limites",
        )),
    }
}

/// SHA-256 textuel canonique (64 chiffres hexadécimaux minuscules).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Digest(String);

impl Digest {
    pub fn new(value: impl Into<String>) -> Result<Self, RegistryError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid(
                "hash attendu : 64 chiffres hexadécimaux minuscules",
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identifiant stable d'une configuration inscrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrialId(pub u64);
/// Identifiant strictement croissant d'un événement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventId(pub u64);
/// Identifiant d'invocation partagé par tous ses profils et lignes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReadId(pub u64);
/// Identifiant unique d'un claim ; sa consommation n'est jamais remboursée.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaimId(pub u64);

/// Fiche identifiée et hachée ; sa présence dans Git est vérifiée par l'appelant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HypothesisRef {
    id: String,
    sha256: Digest,
}
impl HypothesisRef {
    pub fn new(id: String, sha256: Digest) -> Result<Self, RegistryError> {
        let suffix = id
            .strip_prefix("H-")
            .ok_or_else(|| invalid("hypothèse attendue : H-NNN"))?;
        if suffix.len() < 3
            || !suffix.bytes().all(|b| b.is_ascii_digit())
            || suffix.bytes().all(|b| b == b'0')
        {
            return Err(invalid(
                "hypothèse exécutable attendue : H-NNN, différente de H-000",
            ));
        }
        Ok(Self { id, sha256 })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn sha256(&self) -> &Digest {
        &self.sha256
    }
}

/// Trimestre civil UTC explicite fourni par la CLI ; aucun accès à l'horloge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Quarter {
    year: i32,
    number: u8,
}
impl Quarter {
    pub fn new(year: i32, number: u8) -> Result<Self, RegistryError> {
        if !(0..=9999).contains(&year) || !(1..=4).contains(&number) {
            return Err(invalid("année ou trimestre UTC invalide"));
        }
        Ok(Self { year, number })
    }
    pub fn year(self) -> i32 {
        self.year
    }
    pub fn number(self) -> u8 {
        self.number
    }
}
impl fmt::Display for Quarter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-Q{}", self.year, self.number)
    }
}

/// Date UTC stricte, exactement `YYYY-MM-DDTHH:MM:SSZ`, validée par le calendrier du store.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UtcDate(String);
impl UtcDate {
    pub fn new(value: impl Into<String>) -> Result<Self, RegistryError> {
        let value = value.into();
        parse_rfc3339_utc(&value)
            .map_err(|_| invalid("date UTC invalide, attendu YYYY-MM-DDTHH:MM:SSZ"))?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn quarter(&self) -> Quarter {
        // La construction a validé les chiffres et les bornes calendaires de ces deux champs.
        let b = self.0.as_bytes();
        let year = b[..4]
            .iter()
            .fold(0_i32, |n, c| n * 10 + i32::from(c - b'0'));
        let month = (b[5] - b'0') * 10 + b[6] - b'0';
        Quarter {
            year,
            number: (month - 1) / 3 + 1,
        }
    }
    /// Construction depuis l'instant injecté par l'appelant ; aucune lecture de l'horloge.
    pub fn from_timestamp_ms(timestamp_ms: i64) -> Result<Self, RegistryError> {
        crate::store::time::format_rfc3339_utc(timestamp_ms)
            .map_err(|_| invalid("horodatage UTC hors limites"))
            .and_then(Self::new)
    }
    /// Année et trimestre de l'instant injecté, sans conversion textuelle à la CLI.
    pub fn quarter_from_timestamp_ms(timestamp_ms: i64) -> Result<Quarter, RegistryError> {
        let (year, month, _) = civil_from_days(timestamp_ms.div_euclid(86_400_000));
        Quarter::new(
            i32::try_from(year).map_err(|_| invalid("année hors limites"))?,
            ((month - 1) / 3 + 1) as u8,
        )
    }
}

/// Paire date/trimestre vérifiée à la frontière de l'API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionTime {
    date: UtcDate,
    quarter: Quarter,
}
impl AdmissionTime {
    pub fn new(date: UtcDate, quarter: Quarter) -> Result<Self, RegistryError> {
        if date.quarter() != quarter {
            return Err(invalid(
                "trimestre incompatible avec la date UTC d'admission",
            ));
        }
        Ok(Self { date, quarter })
    }
    pub fn date(&self) -> &UtcDate {
        &self.date
    }
    pub fn quarter(&self) -> Quarter {
        self.quarter
    }
}

/// Mesures inconnues représentées par `None`, jamais par un zéro de remplacement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    pub mu_r: Option<f64>,
    pub n: Option<u64>,
    pub pf: Option<f64>,
}
impl Metrics {
    pub(crate) fn validate(&self) -> Result<(), RegistryError> {
        if self.mu_r.is_some_and(|v| !v.is_finite())
            || self.pf.is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            return Err(invalid("métrique non finie ou profit factor négatif"));
        }
        Ok(())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.mu_r.is_none() && self.n.is_none() && self.pf.is_none()
    }
}

/// Vocabulaire fermé des événements S12.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Registered,
    Completed,
    Failed,
    Claim,
    HypothesisLinked,
    ReadReserved,
    ReadCompleted,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Claim => "claim",
            Self::HypothesisLinked => "hypothesis_linked",
            Self::ReadReserved => "read_reserved",
            Self::ReadCompleted => "read_completed",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, RegistryError> {
        match value {
            "registered" => Ok(Self::Registered),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "claim" => Ok(Self::Claim),
            "hypothesis_linked" => Ok(Self::HypothesisLinked),
            "read_reserved" => Ok(Self::ReadReserved),
            "read_completed" => Ok(Self::ReadCompleted),
            _ => Err(invalid(format!("statut inconnu : {value}"))),
        }
    }
}

/// Une ligne du journal. Tous les champs restent validés lors du rejeu, même construits en Rust.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub trial_id: TrialId,
    pub family: String,
    pub config_hash: Digest,
    pub dataset_hash: Option<Digest>,
    pub block: Option<Block>,
    pub date: UtcDate,
    pub metrics: Metrics,
    pub status: Status,
    pub claim_holdout: Option<ClaimId>,
    pub event_id: EventId,
    pub read_id: Option<ReadId>,
    pub hypothesis: Option<HypothesisRef>,
    pub generation: u32,
    pub decision_start_ms: Option<Ts>,
    pub decision_end_ms: Option<Ts>,
}

/// Projection d'un essai, avec conservation de sa première origine sans fiche.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trial {
    pub id: TrialId,
    pub family: String,
    pub hypothesis: Option<HypothesisRef>,
    pub origin_unregistered: bool,
    pub initial_block: Option<Block>,
}
/// Projection d'un claim individuel par profil.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub id: ClaimId,
    pub config_hash: Digest,
    pub hypothesis: HypothesisRef,
    pub date: UtcDate,
    pub consumed_by: Option<ReadId>,
}
/// Projection d'une lecture, éventuellement réservée sans aucun résultat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub id: ReadId,
    pub config_hashes: BTreeSet<Digest>,
    pub dataset_hash: Digest,
    pub block: Option<Block>,
    pub hypothesis: Option<HypothesisRef>,
    pub date: UtcDate,
    pub generation: u32,
    pub decision_start_ms: Option<Ts>,
    pub decision_end_ms: Option<Ts>,
    pub completed_hashes: BTreeSet<Digest>,
    pub(crate) resulted_hashes: BTreeSet<Digest>,
}
impl Read {
    pub fn is_complete(&self) -> bool {
        self.completed_hashes == self.config_hashes
    }
    pub fn quarter(&self) -> Quarter {
        self.date.quarter()
    }
}

/// Projection déterministe du journal complet ; ses champs ne sont modifiés que sur une copie.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registry {
    events: Vec<Event>,
    trials: BTreeMap<Digest, Trial>,
    claims: BTreeMap<ClaimId, Claim>,
    reads: BTreeMap<ReadId, Read>,
    generation: u32,
    control_reads: BTreeSet<ReadId>,
    holdout_reads: BTreeMap<u32, usize>,
}
impl Registry {
    pub fn from_csv(bytes: &[u8]) -> Result<Self, RegistryError> {
        Self::default().with_events(&decode_csv(bytes)?)
    }
    pub fn events(&self) -> &[Event] {
        &self.events
    }
    pub fn role_counts(&self) -> RoleCounts {
        let mut counts = RoleCounts::default();
        for trial in self.trials.values() {
            if role_from_family(&trial.family) == Ok(TrialRole::NullControl) {
                counts.null_controls += 1;
            } else {
                counts.candidates += 1;
            }
        }
        counts
    }
    pub fn n(&self) -> usize {
        self.trials.len()
    }
    pub fn trials(&self) -> &BTreeMap<Digest, Trial> {
        &self.trials
    }
    pub fn claims(&self) -> &BTreeMap<ClaimId, Claim> {
        &self.claims
    }
    pub fn reads(&self) -> &BTreeMap<ReadId, Read> {
        &self.reads
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }
    pub fn holdout_reads_by_generation(&self) -> &BTreeMap<u32, usize> {
        &self.holdout_reads
    }
    pub fn pending_reads(&self) -> Vec<&Read> {
        self.reads.values().filter(|r| !r.is_complete()).collect()
    }
    /// Applique un lot à une copie et retourne l'état complet validé, sans mutation à l'échec.
    pub fn with_events(&self, events: &[Event]) -> Result<Self, RegistryError> {
        let mut next = self.clone();
        for event in events {
            if event.status == Status::ReadReserved
                && matches!(event.block, None | Some(Block::Dev))
                && role_from_family(&event.family)? == TrialRole::NullControl
                && let Some(id) = event.read_id
            {
                next.control_reads.insert(id);
            }
        }
        for event in events {
            next.apply(event)?;
        }
        Ok(next)
    }
    pub(crate) fn next_event_id(&self) -> Result<EventId, RegistryError> {
        self.events
            .last()
            .map_or(Some(1), |e| e.event_id.0.checked_add(1))
            .map(EventId)
            .ok_or_else(|| invalid("identifiants d'événements épuisés"))
    }
    pub(crate) fn next_trial_id(&self) -> Result<TrialId, RegistryError> {
        self.trials
            .values()
            .map(|t| t.id.0)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .map(TrialId)
            .ok_or_else(|| invalid("identifiants d'essais épuisés"))
    }
    pub(crate) fn next_read_id(&self) -> Result<ReadId, RegistryError> {
        self.reads
            .keys()
            .next_back()
            .map_or(0, |r| r.0)
            .checked_add(1)
            .map(ReadId)
            .ok_or_else(|| invalid("identifiants de lectures épuisés"))
    }
    pub(crate) fn event_for(
        &self,
        hash: &Digest,
        date: &UtcDate,
        status: Status,
    ) -> Result<Event, RegistryError> {
        let trial = self
            .trials
            .get(hash)
            .ok_or_else(|| invalid("configuration inconnue"))?;
        Ok(Event {
            trial_id: trial.id,
            family: trial.family.clone(),
            config_hash: hash.clone(),
            dataset_hash: None,
            block: None,
            date: date.clone(),
            metrics: Metrics::default(),
            status,
            claim_holdout: None,
            event_id: self.next_event_id()?,
            read_id: None,
            hypothesis: trial.hypothesis.clone(),
            generation: self.generation,
            decision_start_ms: None,
            decision_end_ms: None,
        })
    }
    pub(crate) fn push(
        &mut self,
        event: Event,
        batch: &mut Vec<Event>,
    ) -> Result<(), RegistryError> {
        self.apply(&event)?;
        batch.push(event);
        Ok(())
    }
    fn apply(&mut self, event: &Event) -> Result<(), RegistryError> {
        event.metrics.validate()?;
        let role = role_from_family(&event.family)?;
        if role == TrialRole::NullControl
            && (event.block.is_some_and(|b| b != Block::Dev) || event.status == Status::Claim)
        {
            return Err(invalid(
                "contrôle nul interdit sur un bloc protégé ou dans un claim",
            ));
        }
        validate_window(event.decision_start_ms, event.decision_end_ms)?;
        if !matches!(
            event.status,
            Status::ReadReserved | Status::Completed | Status::Failed | Status::ReadCompleted
        ) && (event.decision_start_ms.is_some() || event.decision_end_ms.is_some())
        {
            return Err(invalid("fenêtre de décision portée hors d'une lecture"));
        }
        if event.event_id.0 == 0
            || self
                .events
                .last()
                .is_some_and(|e| event.event_id <= e.event_id)
        {
            return Err(invalid("event_id nul, dupliqué ou non croissant"));
        }
        if event.trial_id.0 == 0
            || event.family.trim().is_empty()
            || event.family.chars().any(char::is_control)
        {
            return Err(invalid("identifiant d'essai nul ou famille vide/invalide"));
        }
        if !matches!(event.status, Status::Completed) && !event.metrics.is_empty() {
            return Err(invalid("métriques autorisées seulement sur completed"));
        }
        if event.status == Status::Registered {
            if self.trials.contains_key(&event.config_hash)
                || self.trials.values().any(|t| t.id == event.trial_id)
            {
                return Err(invalid("configuration ou trial_id déjà inscrit"));
            }
            if event.read_id.is_some()
                || event.claim_holdout.is_some()
                || event.block.is_some_and(Block::is_protected)
                || (event.block.is_some() && event.hypothesis.is_none())
                || event.generation != self.generation
            {
                return Err(invalid(
                    "inscription incohérente ou fiche obligatoire absente",
                ));
            }
            self.trials.insert(
                event.config_hash.clone(),
                Trial {
                    id: event.trial_id,
                    family: event.family.clone(),
                    hypothesis: event.hypothesis.clone(),
                    origin_unregistered: event.hypothesis.is_none(),
                    initial_block: event.block,
                },
            );
        } else {
            let trial = self
                .trials
                .get(&event.config_hash)
                .ok_or_else(|| invalid("événement sur configuration inconnue"))?;
            if (role == TrialRole::NullControl
                || role_from_family(&trial.family)? == TrialRole::NullControl)
                && trial.family != event.family
            {
                return Err(invalid("rôle et famille nulle immuables"));
            }
            let is_result = matches!(
                event.status,
                Status::Completed | Status::Failed | Status::ReadCompleted
            );
            if trial.id != event.trial_id
                || (!is_result
                    && event.status != Status::HypothesisLinked
                    && trial.family != event.family)
            {
                return Err(invalid("référence d'essai incohérente"));
            }
            if !is_result
                && event.status != Status::HypothesisLinked
                && trial.hypothesis != event.hypothesis
            {
                return Err(invalid("fiche incohérente avec l'essai"));
            }
            match event.status {
                Status::Registered => return Err(invalid("inscription inattendue")),
                Status::HypothesisLinked => self.apply_link(event)?,
                Status::Claim => self.apply_claim(event)?,
                Status::ReadReserved => self.apply_reservation(event)?,
                Status::Completed | Status::Failed | Status::ReadCompleted => {
                    self.apply_result(event)?
                }
            }
        }
        self.events.push(event.clone());
        Ok(())
    }
    fn apply_link(&mut self, event: &Event) -> Result<(), RegistryError> {
        if event.hypothesis.is_none()
            || event.read_id.is_some()
            || event.claim_holdout.is_some()
            || event.block.is_some()
            || event.dataset_hash.is_some()
            || event.generation != self.generation
        {
            return Err(invalid("liaison d'hypothèse incohérente"));
        }
        let trial = self
            .trials
            .get_mut(&event.config_hash)
            .ok_or_else(|| invalid("essai inconnu"))?;
        if trial.hypothesis.is_some() {
            return Err(invalid("essai déjà associé à une fiche"));
        }
        trial.hypothesis = event.hypothesis.clone();
        trial.family = event.family.clone();
        Ok(())
    }
    fn apply_claim(&mut self, event: &Event) -> Result<(), RegistryError> {
        let id = event
            .claim_holdout
            .ok_or_else(|| invalid("identifiant de claim absent"))?;
        let hypothesis = event
            .hypothesis
            .clone()
            .ok_or_else(|| invalid("claim sans fiche"))?;
        if id.0 == 0
            || self.claims.contains_key(&id)
            || event.read_id.is_some()
            || event.block != Some(Block::Holdout)
            || event.dataset_hash.is_some()
            || event.generation != self.generation
            || self
                .claims
                .values()
                .any(|c| c.config_hash == event.config_hash && c.consumed_by.is_none())
        {
            return Err(invalid(
                "claim réutilisé, doublon actif ou événement incohérent",
            ));
        }
        self.claims.insert(
            id,
            Claim {
                id,
                config_hash: event.config_hash.clone(),
                hypothesis,
                date: event.date.clone(),
                consumed_by: None,
            },
        );
        Ok(())
    }
    fn apply_reservation(&mut self, event: &Event) -> Result<(), RegistryError> {
        let id = event
            .read_id
            .filter(|id| id.0 > 0)
            .ok_or_else(|| invalid("read_id absent ou nul"))?;
        let dataset_hash = event
            .dataset_hash
            .clone()
            .ok_or_else(|| invalid("réservation sans dataset_hash"))?;
        if event.block.is_some() && event.hypothesis.is_none() {
            return Err(invalid("lecture de recherche sans hypothèse"));
        }
        if let Some(read) = self.reads.get(&id) {
            if !self.events.last().is_some_and(|previous| {
                previous.status == Status::ReadReserved && previous.read_id == Some(id)
            }) {
                return Err(invalid(
                    "réouverture d'une lecture après la fin de son lot de réservations",
                ));
            }
            if read.dataset_hash != dataset_hash
                || read.block != event.block
                || (read.hypothesis != event.hypothesis && !self.control_reads.contains(&id))
                || read.date != event.date
                || read.generation != event.generation
                || read.decision_start_ms != event.decision_start_ms
                || read.decision_end_ms != event.decision_end_ms
                || read.config_hashes.contains(&event.config_hash)
                || !read.resulted_hashes.is_empty()
            {
                return Err(invalid(
                    "profils de lecture incohérents ou réservation dupliquée/tardive",
                ));
            }
        } else {
            if event.generation != self.generation
                || self
                    .reads
                    .keys()
                    .next_back()
                    .is_some_and(|last| id <= *last)
            {
                return Err(invalid("génération obsolète ou read_id non croissant"));
            }
            self.check_quota(
                event.block,
                event.hypothesis.as_ref(),
                event.date.quarter(),
                event.generation,
            )?;
        }
        let claim_id = if event.block == Some(Block::Holdout) {
            let claim_id = event
                .claim_holdout
                .ok_or_else(|| invalid("claim HOLDOUT absent"))?;
            let claim = self
                .claims
                .get(&claim_id)
                .ok_or_else(|| invalid("claim HOLDOUT inconnu"))?;
            if claim.config_hash != event.config_hash
                || Some(&claim.hypothesis) != event.hypothesis.as_ref()
                || claim.consumed_by.is_some()
                || claim.date > event.date
            {
                return Err(invalid(
                    "claim HOLDOUT incompatible, déjà consommé ou postérieur",
                ));
            }
            Some(claim_id)
        } else {
            if event.claim_holdout.is_some() {
                return Err(invalid("claim sur une lecture hors HOLDOUT"));
            }
            None
        };
        let new_read = !self.reads.contains_key(&id);
        let read = self.reads.entry(id).or_insert_with(|| Read {
            id,
            config_hashes: BTreeSet::new(),
            dataset_hash,
            block: event.block,
            hypothesis: event.hypothesis.clone(),
            date: event.date.clone(),
            generation: event.generation,
            decision_start_ms: event.decision_start_ms,
            decision_end_ms: event.decision_end_ms,
            completed_hashes: BTreeSet::new(),
            resulted_hashes: BTreeSet::new(),
        });
        read.config_hashes.insert(event.config_hash.clone());
        if let Some(claim_id) = claim_id
            && let Some(claim) = self.claims.get_mut(&claim_id)
        {
            claim.consumed_by = Some(id);
        }
        if new_read && event.block == Some(Block::Holdout) {
            let count = self.holdout_reads.entry(event.generation).or_default();
            *count += 1;
            if *count == 3 {
                self.generation = self
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| invalid("générations épuisées"))?;
            }
        }
        Ok(())
    }
    fn apply_result(&mut self, event: &Event) -> Result<(), RegistryError> {
        let id = event
            .read_id
            .ok_or_else(|| invalid("résultat sans read_id"))?;
        let read = self
            .reads
            .get_mut(&id)
            .ok_or_else(|| invalid("résultat sans réservation"))?;
        if !read.config_hashes.contains(&event.config_hash)
            || read.dataset_hash
                != *event
                    .dataset_hash
                    .as_ref()
                    .ok_or_else(|| invalid("résultat sans dataset_hash"))?
            || read.block != event.block
            || read.generation != event.generation
            || read.decision_start_ms != event.decision_start_ms
            || read.decision_end_ms != event.decision_end_ms
            || event.date < read.date
            || read.completed_hashes.contains(&event.config_hash)
        {
            return Err(invalid("résultat incompatible ou lecture déjà terminée"));
        }
        let reservation = self
            .events
            .iter()
            .find(|e| {
                e.status == Status::ReadReserved
                    && e.read_id == Some(id)
                    && e.config_hash == event.config_hash
            })
            .ok_or_else(|| invalid("réservation absente"))?;
        if event.claim_holdout != reservation.claim_holdout
            || event.family != reservation.family
            || event.hypothesis != reservation.hypothesis
        {
            return Err(invalid("claim du résultat incohérent"));
        }
        if event.status == Status::ReadCompleted {
            if !read.resulted_hashes.contains(&event.config_hash) {
                return Err(invalid("lecture terminée sans résultat ni échec"));
            }
            read.completed_hashes.insert(event.config_hash.clone());
        } else if !read.resulted_hashes.insert(event.config_hash.clone()) {
            return Err(invalid("résultat dupliqué pour ce profil et cette lecture"));
        }
        Ok(())
    }
}
