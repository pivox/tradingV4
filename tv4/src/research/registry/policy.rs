//! Admissions groupées et quotas, sans effets de bord.
use std::collections::BTreeSet;

use crate::model::Ts;
use crate::model::blocks::Block;

use super::roles::{TrialRole, candidate_family, role_from_family};

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
        self.propose_admission_kind(request, false)
    }
    #[allow(dead_code, reason = "admission interne S14 branchée au lot CLI")]
    pub(crate) fn propose_controls_admission(
        &self,
        request: &Admission,
    ) -> Result<AdmissionBatch, RegistryError> {
        self.propose_admission_kind(request, true)
    }
    fn propose_admission_kind(
        &self,
        request: &Admission,
        controls: bool,
    ) -> Result<AdmissionBatch, RegistryError> {
        super::validate_window(request.decision_start_ms, request.decision_end_ms)?;
        if controls
            && (!matches!(request.block, None | Some(Block::Dev))
                || !request
                    .configs
                    .iter()
                    .any(|c| role_from_family(&c.family) == Ok(TrialRole::NullControl)))
        {
            return Err(invalid(
                "contrôles internes requis sur DEV effectif ou voie technique sans bloc",
            ));
        }
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
            if !controls {
                candidate_family(&config.family)?;
            } else {
                role_from_family(&config.family)?;
            }
            if !controls && &config.hypothesis != first_hypothesis {
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
        if controls {
            next.control_reads.insert(read_id);
        }
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
        candidate_family(family)?;
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
    /// Ajout de la seule association H à un null technique ; rôle/famille conservés.
    #[allow(dead_code, reason = "raccord interne de l'admission S14")]
    pub(crate) fn propose_control_link(
        &self,
        hashes: &[Digest],
        hypothesis: &HypothesisRef,
        date: &UtcDate,
    ) -> Result<Vec<Event>, RegistryError> {
        if hashes.is_empty() {
            return Err(invalid("liaison sans configuration"));
        }
        let mut next = self.clone();
        let mut events = Vec::new();
        for hash in hashes {
            let mut event = next.event_for(hash, date, Status::HypothesisLinked)?;
            if role_from_family(&event.family)? != TrialRole::NullControl {
                return Err(invalid("liaison interne réservée aux contrôles nuls"));
            }
            event.hypothesis = Some(hypothesis.clone());
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

#[cfg(test)]
mod controls_tests {
    use super::*;
    fn digest(i: u32) -> Digest {
        Digest::new(format!("{i:064x}")).unwrap()
    }
    fn hyp(i: u32) -> HypothesisRef {
        HypothesisRef::new(format!("H-{i:03}"), digest(999)).unwrap()
    }
    fn request() -> Admission {
        let date = UtcDate::new("2026-01-01T00:00:00Z").unwrap();
        Admission {
            configs: vec![
                ConfigSpec {
                    config_hash: digest(1),
                    family: "F1".into(),
                    hypothesis: Some(hyp(1)),
                },
                ConfigSpec {
                    config_hash: digest(2),
                    family: "F4".into(),
                    hypothesis: Some(hyp(2)),
                },
                ConfigSpec::null_control(digest(3), "f1", "1", Some(hyp(1))).unwrap(),
            ],
            dataset_hash: digest(900),
            block: Some(Block::Dev),
            time: AdmissionTime::new(date.clone(), date.quarter()).unwrap(),
            generation: 0,
            decision_start_ms: None,
            decision_end_ms: None,
        }
    }
    #[test]
    fn controls_multi_fiches_rejouent_et_terminent_tous_les_resultats() {
        let req = request();
        let reg = Registry::default();
        assert!(reg.propose_admission(&req).is_err());
        let batch = reg.propose_controls_admission(&req).unwrap();
        let reg = reg.with_events(&batch.events).unwrap();
        assert_eq!(reg.n(), 3);
        assert_eq!(reg.role_counts().null_controls, 1);
        let results = req
            .configs
            .iter()
            .enumerate()
            .map(|(i, c)| TrialResult {
                config_hash: c.config_hash.clone(),
                failed: i == 2,
                metrics: Metrics::default(),
            })
            .collect::<Vec<_>>();
        assert!(
            reg.propose_results(batch.read_id, &results[..2], req.time.date())
                .is_err()
        );
        let events = reg
            .propose_results(batch.read_id, &results, req.time.date())
            .unwrap();
        let reg = reg.with_events(&events).unwrap();
        assert!(reg.reads()[&batch.read_id].is_complete());
        let csv = [
            super::super::header_csv(),
            super::super::encode_events(reg.events()).unwrap(),
        ]
        .concat();
        assert_eq!(Registry::from_csv(&csv).unwrap(), reg);
        assert_eq!(reg.propose_controls_admission(&req).unwrap().n, 3);
        for block in [Block::Validation, Block::Holdout, Block::Forward] {
            let mut req = req.clone();
            req.block = Some(block);
            assert!(reg.propose_controls_admission(&req).is_err());
        }
    }
    #[test]
    fn registre_inclusif_admet_10800_nulls_et_27_parents_sans_plafond_cache() {
        let mut req = request();
        req.configs.clear();
        for parent in 1..=27 {
            req.configs.push(ConfigSpec {
                config_hash: digest(parent),
                family: format!("F{parent}"),
                hypothesis: Some(hyp(parent)),
            });
        }
        for i in 0..10800 {
            req.configs.push(
                ConfigSpec::null_control(digest(100 + i), "fixture", "1", Some(hyp(1 + i % 27)))
                    .unwrap(),
            );
        }
        let batch = Registry::default()
            .propose_controls_admission(&req)
            .unwrap();
        assert_eq!(batch.n, 10827);
        let reg = Registry::default().with_events(&batch.events).unwrap();
        assert_eq!(reg.role_counts().candidates, 27);
        assert_eq!(reg.role_counts().null_controls, 10800);
    }
    #[test]
    fn null_technique_peut_lier_h_seulement_sans_promotion_ni_claim() {
        let mut req = request();
        req.configs = vec![ConfigSpec::null_control(digest(3), "f1", "1", None).unwrap()];
        req.block = None;
        let batch = Registry::default()
            .propose_controls_admission(&req)
            .unwrap();
        let reg = Registry::default().with_events(&batch.events).unwrap();
        assert!(
            reg.propose_link(&[digest(3)], &hyp(1), "F1", req.time.date())
                .is_err()
        );
        let linked = reg
            .propose_control_link(&[digest(3)], &hyp(1), req.time.date())
            .unwrap();
        let mut forged = linked.clone();
        forged[0].family = "F1".into();
        assert!(reg.with_events(&forged).is_err());
        let reg = reg.with_events(&linked).unwrap();
        assert!(
            reg.propose_claims(&[digest(3)], &hyp(1), req.time.date())
                .is_err()
        );
        let mut forged = linked[0].clone();
        forged.event_id = reg.next_event_id().unwrap();
        forged.status = Status::Claim;
        forged.block = Some(Block::Holdout);
        forged.claim_holdout = Some(ClaimId(99));
        assert!(reg.with_events(&[forged]).is_err());
        assert!(ConfigSpec::null_control(digest(4), "f:1", "1", None).is_err());
        assert!(ConfigSpec::null_control(digest(4), "f1", "1:2", None).is_err());
    }
}
