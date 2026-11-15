//! Admission sérialisée, sans verrou conservé pendant la lecture ou le calcul.
use super::{
    ResearchArgs, ResearchError, digest, hypothesis::load_frozen, journal::Journal, refused,
};
use crate::{
    cli::{CliError, now_ms},
    model::{Block, Ts},
    report::governance::{GovernanceContext, HypothesisContext},
    research::{
        noise::noise_context,
        registry::{
            Admission, AdmissionTime, ConfigSpec, Metrics, ReadId, Registry, TrialResult, UtcDate,
        },
    },
    store::time::{civil_from_days, days_from_civil, format_rfc3339_utc},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub(crate) struct Session {
    root: PathBuf,
    read_id: ReadId,
    contexts: BTreeMap<String, GovernanceContext>,
    block: Option<Block>,
    window: Option<(Ts, Ts)>,
    generation: u32,
    finished: bool,
}
impl Session {
    pub(crate) fn begin(
        args: &ResearchArgs,
        config_hashes: &[String],
        dataset_hash: &str,
        source_block: Option<Block>,
        decision_window: Option<(Ts, Ts)>,
        subset_hash: Option<&str>,
    ) -> Result<Self, CliError> {
        Self::begin_with_prepare(
            args,
            config_hashes,
            dataset_hash,
            source_block,
            decision_window,
            subset_hash,
            |_, _| Ok(()),
        )
    }
    /// Termine les métadonnées dépendantes de la rotation avant de publier la réservation.
    /// Le callback ne doit jamais charger des séries ; il est exécuté sous le verrou du journal.
    pub(crate) fn begin_with_prepare(
        args: &ResearchArgs,
        config_hashes: &[String],
        dataset_hash: &str,
        source_block: Option<Block>,
        decision_window: Option<(Ts, Ts)>,
        subset_hash: Option<&str>,
        prepare: impl FnOnce(Option<Block>, Option<(Ts, Ts)>) -> Result<(), CliError>,
    ) -> Result<Self, CliError> {
        let root = args.resolve_root()?;
        let mut journal = Journal::open(&root)?;
        let date = UtcDate::from_timestamp_ms(now_ms()?).map_err(ResearchError::from)?;
        let fiche = if source_block.is_some() || args.hypothesis.is_some() {
            Some(args.load_hypothesis(&root)?)
        } else {
            None
        };
        let hashes = config_hashes
            .iter()
            .map(|h| digest(h))
            .collect::<Result<Vec<_>, _>>()?;
        let mut staged = journal.registry().clone();
        let mut events = Vec::new();
        if let Some(fiche) = &fiche {
            let links = hashes
                .iter()
                .filter(|h| {
                    staged
                        .trials()
                        .get(*h)
                        .is_some_and(|t| t.hypothesis.is_none())
                })
                .cloned()
                .collect::<Vec<_>>();
            if !links.is_empty() {
                let linked = staged
                    .propose_link(&links, &fiche.reference, &fiche.family, &date)
                    .map_err(ResearchError::from)?;
                staged = staged.with_events(&linked).map_err(ResearchError::from)?;
                events.extend(linked);
            }
        }
        let generation = staged.generation();
        let block = match (source_block, generation) {
            (Some(Block::Holdout), 1..) => Some(Block::Validation),
            (Some(Block::Forward), 1..) => Some(Block::Holdout),
            _ => source_block,
        };
        let frozen = if block.is_some_and(Block::is_protected) {
            let fiche = fiche
                .as_ref()
                .ok_or_else(|| refused("fiche --hypothesis obligatoire pour un bloc protégé"))?;
            let frozen = load_frozen(&root, fiche, &staged)?;
            let requested: BTreeSet<_> = hashes.iter().collect();
            let selected: BTreeSet<_> = frozen.config_hashes.iter().collect();
            if requested != selected || requested.len() != hashes.len() {
                return Err(refused("ensemble exact des configurations gelées requis").into());
            }
            if subset_hash != Some(frozen.okx_subset_hash.as_str()) {
                return Err(refused("sous-ensemble OKX absent ou différent du gel").into());
            }
            Some(frozen)
        } else {
            None
        };
        let window = resolve_window(
            &staged,
            source_block,
            decision_window,
            frozen.as_ref().map(|f| f.frozen_at_ms),
        )?;
        prepare(block, window)?;
        let configs = hashes
            .iter()
            .map(|hash| {
                let (family, hypothesis) = match (&fiche, staged.trials().get(hash)) {
                    (Some(fiche), _) => (fiche.family.clone(), Some(fiche.reference.clone())),
                    (None, Some(trial)) => (trial.family.clone(), trial.hypothesis.clone()),
                    (None, None) => ("unregistered_hypothesis".into(), None),
                };
                ConfigSpec {
                    config_hash: hash.clone(),
                    family,
                    hypothesis,
                }
            })
            .collect::<Vec<_>>();
        if let Some(first) = configs.first()
            && configs.iter().any(|c| c.hypothesis != first.hypothesis)
        {
            return Err(refused(
                "associations de fiches ambiguës : donner --hypothesis explicitement",
            )
            .into());
        }
        let request = Admission {
            configs,
            dataset_hash: digest(dataset_hash)?,
            block,
            time: AdmissionTime::new(date.clone(), date.quarter()).map_err(ResearchError::from)?,
            generation,
            decision_start_ms: window.map(|w| w.0),
            decision_end_ms: window.map(|w| w.1),
        };
        let batch = staged
            .propose_admission(&request)
            .map_err(ResearchError::from)?;
        events.extend(batch.events.clone());
        journal.append(&events)?;
        let noise = noise_context(batch.n as u64, 5.0).map_err(|e| refused(e.to_string()))?;
        let mut contexts = BTreeMap::new();
        for (hash, trial_id) in config_hashes.iter().zip(&batch.trial_ids) {
            let reservation = batch.events.iter().find(|e| {
                e.read_id == Some(batch.read_id)
                    && e.config_hash.as_str() == hash.strip_prefix("blake3:").unwrap_or(hash)
            });
            contexts.insert(
                hash.clone(),
                GovernanceContext {
                    schema: "tv4-governance-v1".into(),
                    config_hash: hash.clone(),
                    dataset_hash: dataset_hash.into(),
                    trial_id: trial_id.0,
                    read_id: batch.read_id.0,
                    hypothesis: request
                        .configs
                        .iter()
                        .find(|c| {
                            c.config_hash.as_str() == hash.strip_prefix("blake3:").unwrap_or(hash)
                        })
                        .and_then(|c| c.hypothesis.as_ref())
                        .map(|h| HypothesisContext {
                            identifiant: h.id().into(),
                            sha256: h.sha256().to_string(),
                        }),
                    frozen_tag: frozen.as_ref().map(|f| f.tag.clone()),
                    frozen_at: frozen
                        .as_ref()
                        .map(|f| format_rfc3339_utc(f.frozen_at_ms))
                        .transpose()
                        .map_err(|e| refused(e.to_string()))?,
                    source_block,
                    effective_block: block,
                    generation,
                    claim_holdout: reservation.and_then(|e| e.claim_holdout.map(|c| c.0)),
                    noise: noise.clone(),
                },
            );
        }
        drop(journal);
        Ok(Self {
            root,
            read_id: batch.read_id,
            contexts,
            block,
            window,
            generation,
            finished: false,
        })
    }
    pub(crate) fn context(&self, hash: &str) -> Result<GovernanceContext, CliError> {
        self.contexts
            .get(hash)
            .cloned()
            .ok_or_else(|| refused("configuration absente de l'admission").into())
    }
    pub(crate) fn effective_block(&self) -> Option<Block> {
        self.block
    }
    pub(crate) fn decision_window(&self) -> Option<(Ts, Ts)> {
        self.window
    }
    pub(crate) fn generation(&self) -> u32 {
        self.generation
    }
    pub(crate) fn finish(&mut self, results: &[TrialResult]) -> Result<(), CliError> {
        let date = UtcDate::from_timestamp_ms(now_ms()?).map_err(ResearchError::from)?;
        let mut journal = Journal::open(&self.root)?;
        journal.finish(self.read_id, results, &date)?;
        self.finished = true;
        Ok(())
    }
    pub(crate) fn fail(&mut self) -> Result<(), CliError> {
        if self.finished {
            return Ok(());
        }
        let results = self
            .contexts
            .keys()
            .map(|h| {
                Ok(TrialResult {
                    config_hash: digest(h)?,
                    failed: true,
                    metrics: Metrics::default(),
                })
            })
            .collect::<Result<Vec<_>, ResearchError>>()?;
        self.finish(&results)
    }
    pub(crate) fn print_context(&self) {
        for context in self.contexts.values() {
            let n = &context.noise;
            let shown =
                |v: Option<f64>| v.map_or_else(|| "non calculé".to_string(), |v| format!("{v:.4}"));
            println!(
                "Essais et lectures : N = {}, N_eff = non calculé (S13), T = {} ans ; espérance {}, approximation q90 {}, q95 {} ; essai {}, lecture {}, génération {}",
                n.n,
                n.selection_years,
                shown(n.expected),
                shown(n.approx_q90),
                shown(n.q95),
                context.trial_id,
                context.read_id,
                self.generation()
            );
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.fail()
        {
            eprintln!("échec de journalisation : {error}");
        }
    }
}

fn resolve_window(
    registry: &Registry,
    source: Option<Block>,
    window: Option<(Ts, Ts)>,
    frozen_at: Option<Ts>,
) -> Result<Option<(Ts, Ts)>, ResearchError> {
    let Some(source) = source else {
        return Ok(window);
    };
    let (mut start, end) = window.ok_or_else(|| refused("fenêtre de décision absente"))?;
    if end <= start
        || !source.span().contains(start)
        || source.span().end_ms.is_some_and(|limit| end > limit)
    {
        return Err(refused(
            "fenêtre de décision incompatible avec le bloc source",
        ));
    }
    if source == Block::Forward && registry.generation() > 0 {
        let frozen_at =
            frozen_at.ok_or_else(|| refused("gel requis pour FORWARD devenu HOLDOUT"))?;
        let (year, month, _) = civil_from_days(frozen_at.div_euclid(86_400_000));
        let quarter_month = ((month - 1) / 3) * 3 + 1;
        let (next_year, next_month) = if quarter_month == 10 {
            (year + 1, 1)
        } else {
            (year, quarter_month + 3)
        };
        start = start.max(days_from_civil(next_year, next_month, 1) * 86_400_000);
        let previous = registry.generation() - 1;
        let mut cutoff = Block::Holdout
            .span()
            .end_ms
            .ok_or_else(|| refused("fin du HOLDOUT historique absente"))?;
        for read in registry
            .reads()
            .values()
            .filter(|r| r.block == Some(Block::Holdout) && r.generation == previous)
        {
            match read.decision_end_ms {
                Some(end) => cutoff = cutoff.max(end),
                None if previous == 0 => {}
                None => {
                    return Err(refused(
                        "fenêtre historique absente : borne de génération indérivable",
                    ));
                }
            }
        }
        if previous > 0
            && !registry
                .reads()
                .values()
                .any(|r| r.block == Some(Block::Holdout) && r.generation == previous)
        {
            return Err(refused(
                "aucune fenêtre historique pour la génération précédente",
            ));
        }
        start = start.max(cutoff);
        if !quarter_boundary(start) || !quarter_boundary(end) || end <= start {
            return Err(refused(
                "HOLDOUT renouvelé : trimestres complets postérieurs au gel requis",
            ));
        }
    }
    Ok(Some((start, end)))
}
fn quarter_boundary(ts: Ts) -> bool {
    let (_, month, day) = civil_from_days(ts.div_euclid(86_400_000));
    ts.rem_euclid(86_400_000) == 0 && day == 1 && [1, 4, 7, 10].contains(&month)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::registry::{Digest, HypothesisRef};
    fn ts(text: &str) -> Ts {
        crate::store::time::parse_rfc3339_utc(text).expect("date")
    }
    fn reg() -> Registry {
        let date = UtcDate::new("2025-01-01T00:00:00Z").unwrap();
        let batch = Registry::default()
            .propose_admission(&Admission {
                configs: vec![ConfigSpec {
                    config_hash: Digest::new("a".repeat(64)).unwrap(),
                    family: "F1".into(),
                    hypothesis: Some(
                        HypothesisRef::new("H-001".into(), Digest::new("b".repeat(64)).unwrap())
                            .unwrap(),
                    ),
                }],
                dataset_hash: Digest::new("c".repeat(64)).unwrap(),
                block: Some(Block::Dev),
                time: AdmissionTime::new(date.clone(), date.quarter()).unwrap(),
                generation: 0,
                decision_start_ms: Some(Block::Dev.span().start_ms),
                decision_end_ms: Block::Dev.span().end_ms,
            })
            .unwrap();
        Registry::default().with_events(&batch.events).unwrap()
    }
    fn rotate(mut reg: Registry, dates: [&str; 3], window: Option<(Ts, Ts)>) -> Registry {
        for date in dates {
            let date = UtcDate::new(date).unwrap();
            let (hash, trial) = reg.trials().first_key_value().unwrap();
            let spec = ConfigSpec {
                config_hash: hash.clone(),
                family: trial.family.clone(),
                hypothesis: trial.hypothesis.clone(),
            };
            let claims = reg
                .propose_claims(
                    std::slice::from_ref(hash),
                    trial.hypothesis.as_ref().unwrap(),
                    &date,
                )
                .unwrap();
            reg = reg.with_events(&claims).unwrap();
            let request = Admission {
                configs: vec![spec],
                dataset_hash: Digest::new("c".repeat(64)).unwrap(),
                block: Some(Block::Holdout),
                time: AdmissionTime::new(date.clone(), date.quarter()).unwrap(),
                generation: reg.generation(),
                decision_start_ms: window.map(|w| w.0),
                decision_end_ms: window.map(|w| w.1),
            };
            let batch = reg.propose_admission(&request).unwrap();
            reg = reg.with_events(&batch.events).unwrap();
        }
        reg
    }
    fn first_rotation() -> Registry {
        rotate(
            reg(),
            [
                "2026-01-01T00:00:00Z",
                "2026-04-01T00:00:00Z",
                "2026-07-01T00:00:00Z",
            ],
            Some((
                Block::Holdout.span().start_ms,
                Block::Holdout.span().end_ms.unwrap(),
            )),
        )
    }
    #[test]
    fn forward_historique_est_reduit_au_premier_trimestre_entier_postgel() {
        let r = first_rotation();
        assert_eq!(r.generation(), 1);
        assert_eq!(
            resolve_window(
                &r,
                Some(Block::Forward),
                Some((Block::Forward.span().start_ms, ts("2027-04-01T00:00:00Z"))),
                Some(ts("2026-11-15T12:00:00Z"))
            )
            .unwrap(),
            Some((ts("2027-01-01T00:00:00Z"), ts("2027-04-01T00:00:00Z")))
        );
    }
    #[test]
    fn forward_incomplet_ou_anterieur_au_gel_est_refuse() {
        let r = first_rotation();
        for end in ["2027-02-01T00:00:00Z", "2026-10-01T00:00:00Z"] {
            assert!(
                resolve_window(
                    &r,
                    Some(Block::Forward),
                    Some((Block::Forward.span().start_ms, ts(end))),
                    Some(ts("2026-11-15T12:00:00Z"))
                )
                .is_err()
            );
        }
    }
    #[test]
    fn generation_suivante_ne_rejoue_pas_les_trimestres_consommes() {
        let r = rotate(
            first_rotation(),
            [
                "2026-10-01T00:00:00Z",
                "2027-01-01T00:00:00Z",
                "2027-04-01T00:00:00Z",
            ],
            Some((ts("2027-01-01T00:00:00Z"), ts("2027-04-01T00:00:00Z"))),
        );
        assert_eq!(r.generation(), 2);
        assert_eq!(
            resolve_window(
                &r,
                Some(Block::Forward),
                Some((Block::Forward.span().start_ms, ts("2027-07-01T00:00:00Z"))),
                Some(ts("2026-11-15T12:00:00Z"))
            )
            .unwrap(),
            Some((ts("2027-04-01T00:00:00Z"), ts("2027-07-01T00:00:00Z")))
        );
        assert!(
            resolve_window(
                &r,
                Some(Block::Forward),
                Some((Block::Forward.span().start_ms, ts("2027-04-01T00:00:00Z"))),
                Some(ts("2026-11-15T12:00:00Z"))
            )
            .is_err()
        );
    }
    #[test]
    fn ancienne_generation_sans_fenetre_ne_permet_pas_de_reset() {
        let r = rotate(
            first_rotation(),
            [
                "2026-10-01T00:00:00Z",
                "2027-01-01T00:00:00Z",
                "2027-04-01T00:00:00Z",
            ],
            None,
        );
        assert!(
            resolve_window(
                &r,
                Some(Block::Forward),
                Some((Block::Forward.span().start_ms, ts("2028-01-01T00:00:00Z"))),
                Some(ts("2026-11-15T12:00:00Z"))
            )
            .is_err()
        );
    }
    #[test]
    fn replay_horsbloc_preserve_la_fiche_deja_liee_sans_reinscription() {
        let root = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.invalid"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(root.path())
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        std::fs::create_dir_all(root.path().join("research")).unwrap();
        let mut csv = crate::research::registry::header_csv();
        csv.extend(crate::research::registry::encode_events(reg().events()).unwrap());
        std::fs::write(root.path().join("research/trials.csv"), csv).unwrap();
        let args = ResearchArgs {
            research_root: Some(root.path().into()),
            hypothesis: None,
        };
        let hash = "a".repeat(64);
        let mut session = Session::begin(
            &args,
            std::slice::from_ref(&hash),
            &"c".repeat(64),
            None,
            None,
            None,
        )
        .unwrap();
        let context = session.context(&hash).unwrap();
        assert_eq!(context.hypothesis.unwrap().identifiant, "H-001");
        assert_eq!(context.noise.n, 1);
        assert!(context.frozen_tag.is_none());
        session.fail().unwrap();
        let journal = Journal::open(root.path()).unwrap();
        assert_eq!(journal.registry().n(), 1);
        assert_eq!(journal.registry().reads().len(), 2);
    }
    #[test]
    fn refus_de_preparation_rotation_ne_publie_aucune_reservation() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root.path())
                .args(["init", "-q"])
                .output()
                .unwrap()
                .status
                .success()
        );
        std::fs::create_dir_all(root.path().join("research")).unwrap();
        let mut csv = crate::research::registry::header_csv();
        csv.extend(crate::research::registry::encode_events(reg().events()).unwrap());
        let path = root.path().join("research/trials.csv");
        std::fs::write(&path, &csv).unwrap();
        let args = ResearchArgs {
            research_root: Some(root.path().into()),
            hypothesis: None,
        };
        let result = Session::begin_with_prepare(
            &args,
            &["a".repeat(64)],
            &"c".repeat(64),
            None,
            None,
            None,
            |_, _| Err(refused("union admise vide").into()),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), csv);
    }
}
