//! Admissions groupées et quotas, sans effets de bord.
use std::collections::BTreeSet;

use crate::model::Ts;
use crate::model::blocks::Block;

use super::{
    AdmissionTime, ClaimId, Digest, Event, HypothesisRef, Metrics, Quarter, ReadId, Registry,
    RegistryError, Status, TrialId, UtcDate, invalid,
};

/// Identité canonique d'un profil ; le hash est calculé par les helpers du moteur en amont.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigSpec {
    pub config_hash: Digest,
    pub family: String,
    pub hypothesis: Option<HypothesisRef>,
}
/// Une invocation pour un candidat et un bloc effectif déjà autorisés par la CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub configs: Vec<ConfigSpec>,
    pub dataset_hash: Digest,
    pub block: Option<Block>,
    pub time: AdmissionTime,
    pub generation: u32,
    pub decision_start_ms: Option<Ts>,
    pub decision_end_ms: Option<Ts>,
}
/// Proposition complète à persister sous verrou avant calcul ; aucun état d'origine n'est modifié.
#[derive(Clone, Debug, PartialEq)]
pub struct AdmissionBatch {
    pub events: Vec<Event>,
    pub trial_ids: Vec<TrialId>,
    pub read_id: ReadId,
    pub claims_consumed: Vec<ClaimId>,
    pub n: usize,
    /// Génération utilisée par cette lecture, y compris celle qui provoque une rotation.
    pub generation: u32,
    /// Génération qui devient active après la persistance de tout le lot.
    pub next_generation: u32,
}
/// Mesures finales ou échec explicite d'un profil de la lecture.
#[derive(Clone, Debug, PartialEq)]
pub struct TrialResult {
    pub config_hash: Digest,
    pub failed: bool,
    pub metrics: Metrics,
}
/// Compteurs historiques d'un trimestre UTC, indépendants du dataset et du dossier de sortie.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quota {
    pub validation_reads: usize,
    pub validation_candidates: BTreeSet<String>,
    pub holdout_reads: usize,
}
impl Registry {
    pub fn quota(&self, quarter: Quarter) -> Quota {
        let mut quota = Quota::default();
        for read in self.reads.values().filter(|r| r.quarter() == quarter) {
            match read.block {
                Some(Block::Validation) => {
                    quota.validation_reads += 1;
                    if let Some(hypothesis) = &read.hypothesis {
                        quota
                            .validation_candidates
                            .insert(hypothesis.id().to_owned());
                    }
                }
                Some(Block::Holdout) => quota.holdout_reads += 1,
                _ => {}
            }
        }
        quota
    }
    pub(crate) fn check_quota(
        &self,
        block: Option<Block>,
        hypothesis: Option<&HypothesisRef>,
        quarter: Quarter,
        generation: u32,
    ) -> Result<(), RegistryError> {
        let quota = self.quota(quarter);
        match block {
            Some(Block::Validation) => {
                let candidate = hypothesis.ok_or_else(|| invalid("VALIDATION sans hypothèse"))?;
                if quota.validation_candidates.contains(candidate.id()) {
                    return Err(invalid("candidat déjà lu en VALIDATION ce trimestre UTC"));
                }
                if quota.validation_reads >= 3 {
                    return Err(invalid(
                        "quota VALIDATION épuisé : trois candidats par trimestre UTC",
                    ));
                }
            }
            Some(Block::Holdout) => {
                if hypothesis.is_none() {
                    return Err(invalid("HOLDOUT sans hypothèse"));
                }
                if quota.holdout_reads >= 1 {
                    return Err(invalid(
                        "quota HOLDOUT épuisé : une lecture par trimestre UTC",
                    ));
                }
                if self.holdout_reads.get(&generation).copied().unwrap_or(0) >= 3 {
                    return Err(invalid(
                        "quota HOLDOUT épuisé : trois lectures par génération",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }
    /// Prépare inscriptions et réservations indivisibles. Une erreur sur le dernier profil annule
    /// toute la proposition, y compris les inscriptions et consommations préparées sur une copie.
    pub fn propose_admission(&self, request: &Admission) -> Result<AdmissionBatch, RegistryError> {
        super::validate_window(request.decision_start_ms, request.decision_end_ms)?;
        if request.configs.is_empty() {
            return Err(invalid("admission sans configuration"));
        }
        if request.generation != self.generation {
            return Err(invalid("génération d'admission obsolète"));
        }
        let first_hypothesis = &request.configs[0].hypothesis;
        let mut seen = BTreeSet::new();
        for config in &request.configs {
            if !seen.insert(&config.config_hash) {
                return Err(invalid("hash dupliqué dans l'admission"));
            }
            if &config.hypothesis != first_hypothesis {
                return Err(invalid("plusieurs candidats dans une même invocation"));
            }
            if request.block.is_some() && config.hypothesis.is_none() {
                return Err(invalid(
                    "fiche pré-enregistrée obligatoire pour un bloc de recherche",
                ));
            }
            if let Some(trial) = self.trials.get(&config.config_hash) {
                if trial.family != config.family || trial.hypothesis != config.hypothesis {
                    return Err(invalid(
                        "configuration connue associée à une autre famille ou fiche ; liaison explicite requise",
                    ));
                }
            } else if request.block.is_some_and(Block::is_protected) {
                return Err(invalid("configuration inconnue sur un bloc protégé"));
            }
        }
        self.check_quota(
            request.block,
            first_hypothesis.as_ref(),
            request.time.quarter(),
            request.generation,
        )?;
        let mut next = self.clone();
        let mut events = Vec::new();
        let mut trial_ids = Vec::new();
        let mut claims_consumed = Vec::new();
        let read_id = self.next_read_id()?;
        for config in &request.configs {
            if !next.trials.contains_key(&config.config_hash) {
                let event = Event {
                    trial_id: next.next_trial_id()?,
                    family: config.family.clone(),
                    config_hash: config.config_hash.clone(),
                    dataset_hash: Some(request.dataset_hash.clone()),
                    block: request.block,
                    date: request.time.date().clone(),
                    metrics: Metrics::default(),
                    status: Status::Registered,
                    claim_holdout: None,
                    event_id: next.next_event_id()?,
                    read_id: None,
                    hypothesis: config.hypothesis.clone(),
                    generation: request.generation,
                    decision_start_ms: None,
                    decision_end_ms: None,
                };
                next.push(event, &mut events)?;
            }
        }
        for config in &request.configs {
            let mut event = next.event_for(
                &config.config_hash,
                request.time.date(),
                Status::ReadReserved,
            )?;
            trial_ids.push(event.trial_id);
            event.dataset_hash = Some(request.dataset_hash.clone());
            event.block = request.block;
            event.read_id = Some(read_id);
            event.generation = request.generation;
            event.decision_start_ms = request.decision_start_ms;
            event.decision_end_ms = request.decision_end_ms;
            if request.block == Some(Block::Holdout) {
                let claim = next
                    .claims
                    .values()
                    .find(|c| c.config_hash == config.config_hash && c.consumed_by.is_none())
                    .ok_or_else(|| invalid("claim HOLDOUT préalable absent pour un profil"))?;
                event.claim_holdout = Some(claim.id);
                claims_consumed.push(claim.id);
            }
            next.push(event, &mut events)?;
        }
        Ok(AdmissionBatch {
            events,
            trial_ids,
            read_id,
            claims_consumed,
            n: next.n(),
            generation: request.generation,
            next_generation: next.generation,
        })
    }
    /// Rattache explicitement une fiche à des essais de démonstration, sans effacer leur origine.
    pub fn propose_link(
        &self,
        hashes: &[Digest],
        hypothesis: &HypothesisRef,
        family: &str,
        date: &UtcDate,
    ) -> Result<Vec<Event>, RegistryError> {
        if hashes.is_empty() {
            return Err(invalid("liaison sans configuration"));
        }
        let mut next = self.clone();
        let mut events = Vec::new();
        for hash in hashes {
            let mut event = next.event_for(hash, date, Status::HypothesisLinked)?;
            event.hypothesis = Some(hypothesis.clone());
            event.family = family.to_owned();
            next.push(event, &mut events)?;
        }
        Ok(events)
    }
    /// Inscrit des claims préalables par profil. Aucun claim actif ne peut être doublé et les
    /// identifiants consommés ne sont jamais recyclés.
    pub fn propose_claims(
        &self,
        hashes: &[Digest],
        hypothesis: &HypothesisRef,
        date: &UtcDate,
    ) -> Result<Vec<Event>, RegistryError> {
        if hashes.is_empty() {
            return Err(invalid("claim sans configuration"));
        }
        let mut next = self.clone();
        let mut events = Vec::new();
        for hash in hashes {
            let mut event = next.event_for(hash, date, Status::Claim)?;
            if event.hypothesis.as_ref() != Some(hypothesis) {
                return Err(invalid(
                    "claim sur une configuration non liée à cette fiche",
                ));
            }
            event.block = Some(Block::Holdout);
            event.claim_holdout = Some(ClaimId(event.event_id.0));
            next.push(event, &mut events)?;
        }
        Ok(events)
    }
    /// Termine tous les profils d'une lecture dans un seul lot. Un résultat absent reste inconnu.
    pub fn propose_results(
        &self,
        read_id: ReadId,
        results: &[TrialResult],
        date: &UtcDate,
    ) -> Result<Vec<Event>, RegistryError> {
        let read = self
            .reads
            .get(&read_id)
            .ok_or_else(|| invalid("lecture inconnue"))?;
        let hashes: BTreeSet<_> = results.iter().map(|r| r.config_hash.clone()).collect();
        if hashes != read.config_hashes
            || hashes.len() != results.len()
            || !read.resulted_hashes.is_empty()
        {
            return Err(invalid("résultats incomplets, dupliqués ou déjà inscrits"));
        }
        let mut next = self.clone();
        let mut events = Vec::new();
        for result in results {
            let reservation = self
                .events
                .iter()
                .find(|e| {
                    e.status == Status::ReadReserved
                        && e.read_id == Some(read_id)
                        && e.config_hash == result.config_hash
                })
                .ok_or_else(|| invalid("réservation manquante"))?;
            let mut event = reservation.clone();
            event.event_id = next.next_event_id()?;
            event.date = date.clone();
            event.status = if result.failed {
                Status::Failed
            } else {
                Status::Completed
            };
            event.metrics = result.metrics.clone();
            next.push(event, &mut events)?;
        }
        for result in results {
            let reservation = self
                .events
                .iter()
                .find(|e| {
                    e.status == Status::ReadReserved
                        && e.read_id == Some(read_id)
                        && e.config_hash == result.config_hash
                })
                .ok_or_else(|| invalid("réservation manquante"))?;
            let mut event = reservation.clone();
            event.event_id = next.next_event_id()?;
            event.date = date.clone();
            event.status = Status::ReadCompleted;
            next.push(event, &mut events)?;
        }
        Ok(events)
    }
}
