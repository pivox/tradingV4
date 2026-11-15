//! Contrat pur du registre S12 : aucun fichier, aucune horloge ni donnée de marché.
use tv4::model::blocks::Block;
use tv4::research::registry::{
    Admission, AdmissionTime, ConfigSpec, Digest, HypothesisRef, Metrics, Quarter, Registry,
    Status, TrialResult, UtcDate, decode_csv, encode_events, header_csv,
};

fn digest(c: char) -> Digest {
    Digest::new(c.to_string().repeat(64)).expect("hash synthétique")
}
fn hypothesis(n: u32) -> HypothesisRef {
    HypothesisRef::new(format!("H-{n:03}"), digest('f')).expect("fiche synthétique")
}
fn date(quarter: u8) -> UtcDate {
    UtcDate::new(format!("2026-{:02}-01T00:00:00Z", 1 + (quarter - 1) * 3)).expect("date")
}
fn config(hash: char, candidate: u32) -> ConfigSpec {
    ConfigSpec {
        config_hash: digest(hash),
        family: "F1".into(),
        hypothesis: Some(hypothesis(candidate)),
    }
}
fn admission(configs: Vec<ConfigSpec>, block: Option<Block>, q: u8, generation: u32) -> Admission {
    Admission {
        configs,
        dataset_hash: digest('e'),
        block,
        time: AdmissionTime::new(date(q), Quarter::new(2026, q).expect("trimestre"))
            .expect("horloge"),
        generation,
        decision_start_ms: None,
        decision_end_ms: None,
    }
}
fn apply(registry: &mut Registry, events: &[tv4::research::registry::Event]) {
    *registry = registry.with_events(events).expect("événements valides");
}
fn seed(registry: &mut Registry, configs: Vec<ConfigSpec>) {
    for cfg in configs {
        let batch = registry
            .propose_admission(&admission(
                vec![cfg],
                Some(Block::Dev),
                1,
                registry.generation(),
            ))
            .expect("inscription DEV");
        apply(registry, &batch.events);
    }
}

#[test]
fn journal_vide_et_append_preservent_le_prefixe_et_reconstruisent_n() {
    let mut registry = Registry::from_csv(b"").expect("journal vide");
    assert_eq!(registry.n(), 0);
    let request = admission(vec![config('a', 1), config('b', 1)], Some(Block::Dev), 1, 0);
    let first = registry
        .propose_admission(&request)
        .expect("première admission");
    assert_eq!(registry.n(), 0, "une proposition ne publie aucun état");
    apply(&mut registry, &first.events);
    assert_eq!(registry.n(), 2, "les profils comptent séparément");
    let prefix = [header_csv(), encode_events(&first.events).expect("CSV")].concat();
    let second = registry
        .propose_admission(&request)
        .expect("répétition DEV");
    assert!(second.events.iter().all(|e| e.status != Status::Registered));
    assert_eq!(first.trial_ids, second.trial_ids);
    let bytes = [prefix.clone(), encode_events(&second.events).expect("CSV")].concat();
    assert!(bytes.starts_with(&prefix));
    let restored = Registry::from_csv(&bytes).expect("reconstruction");
    assert_eq!(restored.n(), 2);
    assert_eq!(restored.reads().len(), 2);
    assert_eq!(decode_csv(&bytes).expect("lecture"), restored.events());
}

#[test]
fn csv_refuse_troncature_schema_types_et_evenements_contradictoires() {
    let registry = Registry::default();
    let batch = registry
        .propose_admission(&admission(vec![config('a', 1)], Some(Block::Dev), 1, 0))
        .expect("admission");
    let bytes = [header_csv(), encode_events(&batch.events).expect("CSV")].concat();
    assert!(Registry::from_csv(&bytes[..bytes.len() - 1]).is_err());
    assert!(Registry::from_csv(b"mauvais,en-tete\n").is_err());
    let text = String::from_utf8(bytes.clone()).expect("UTF8");
    assert!(Registry::from_csv(text.replace("registered", "surprise").as_bytes()).is_err());
    assert!(
        Registry::from_csv(
            text.replace("2026-01-01T00:00:00Z", "2026-02-30T00:00:00Z")
                .as_bytes()
        )
        .is_err()
    );
    assert!(Registry::from_csv(text.replace(&"a".repeat(64), "pas-un-hash").as_bytes()).is_err());
    let mut duplicate = batch.events.clone();
    duplicate.push(batch.events[0].clone());
    assert!(registry.with_events(&duplicate).is_err());
    let mut inconsistent = batch.events.clone();
    inconsistent[1].trial_id.0 += 1;
    assert!(registry.with_events(&inconsistent).is_err());
    let mut non_finite = batch.events.clone();
    non_finite[0].metrics.mu_r = Some(f64::NAN);
    assert!(encode_events(&non_finite).is_err());
    assert!(Registry::from_csv(b"\n").is_err());
}

#[test]
fn inconnus_non_lies_et_trimestre_incoherent_sont_refuses_sur_bloc_protege() {
    let mut registry = Registry::default();
    let request = admission(vec![config('a', 1)], Some(Block::Validation), 1, 0);
    assert!(registry.propose_admission(&request).is_err());
    let mut unknown = config('a', 1);
    unknown.hypothesis = None;
    let demo = registry
        .propose_admission(&admission(vec![unknown], None, 1, 0))
        .expect("démo");
    apply(&mut registry, &demo.events);
    assert!(registry.propose_admission(&request).is_err());
    assert!(
        registry
            .trials()
            .get(&digest('a'))
            .expect("essai")
            .origin_unregistered
    );
    let link = registry
        .propose_link(&[digest('a')], &hypothesis(1), "F1", &date(1))
        .expect("rattachement");
    apply(&mut registry, &link);
    assert!(registry.propose_admission(&request).is_ok());
    assert!(
        registry
            .trials()
            .get(&digest('a'))
            .expect("essai")
            .origin_unregistered
    );
    assert!(AdmissionTime::new(date(1), Quarter::new(2026, 2).expect("Q2")).is_err());
    assert!(Quarter::new(2026, 0).is_err());
    assert!(HypothesisRef::new("H-000".into(), digest('a')).is_err());
}

#[test]
fn validation_compte_candidats_et_lectures_groupees_sans_remboursement() {
    let mut registry = Registry::default();
    seed(
        &mut registry,
        vec![
            config('a', 1),
            config('b', 1),
            config('c', 2),
            config('d', 3),
            config('5', 4),
        ],
    );
    let grouped = registry
        .propose_admission(&admission(
            vec![config('a', 1), config('b', 1)],
            Some(Block::Validation),
            2,
            0,
        ))
        .expect("profils groupés");
    apply(&mut registry, &grouped.events);
    assert_eq!(
        registry
            .quota(Quarter::new(2026, 2).expect("Q2"))
            .validation_reads,
        1
    );
    assert!(
        registry
            .propose_admission(&admission(
                vec![config('a', 1)],
                Some(Block::Validation),
                2,
                0
            ))
            .is_err()
    );
    for (hash, candidate) in [('c', 2), ('d', 3)] {
        let batch = registry
            .propose_admission(&admission(
                vec![config(hash, candidate)],
                Some(Block::Validation),
                2,
                0,
            ))
            .expect("quota disponible");
        apply(&mut registry, &batch.events);
    }
    assert!(
        registry
            .propose_admission(&admission(
                vec![config('5', 4)],
                Some(Block::Validation),
                2,
                0
            ))
            .is_err()
    );
    assert_eq!(
        registry.pending_reads().len(),
        8,
        "réservations interrompues conservées, DEV compris"
    );
    assert!(
        registry
            .propose_admission(&admission(
                vec![config('5', 4)],
                Some(Block::Validation),
                3,
                0
            ))
            .is_ok()
    );
}

#[test]
fn claims_par_profil_admission_atomique_et_rotation_apres_trois_read_id() {
    let mut registry = Registry::default();
    seed(&mut registry, vec![config('a', 1), config('b', 1)]);
    let holdout = admission(
        vec![config('a', 1), config('b', 1)],
        Some(Block::Holdout),
        1,
        0,
    );
    assert!(registry.propose_admission(&holdout).is_err());
    let one = registry
        .propose_claims(&[digest('a')], &hypothesis(1), &date(1))
        .expect("claim partiel");
    apply(&mut registry, &one);
    assert!(
        registry
            .propose_claims(&[digest('a')], &hypothesis(1), &date(1))
            .is_err()
    );
    let before = registry.events().to_vec();
    assert!(registry.propose_admission(&holdout).is_err());
    assert_eq!(registry.events(), before);
    assert!(registry.claims().values().all(|c| c.consumed_by.is_none()));
    let other = registry
        .propose_claims(&[digest('b')], &hypothesis(1), &date(1))
        .expect("autre profil");
    apply(&mut registry, &other);
    for q in 1..=3 {
        if q > 1 {
            let claims = registry
                .propose_claims(&[digest('a'), digest('b')], &hypothesis(1), &date(q))
                .expect("nouveaux claims");
            apply(&mut registry, &claims);
        }
        let batch = registry
            .propose_admission(&admission(
                vec![config('a', 1), config('b', 1)],
                Some(Block::Holdout),
                q,
                0,
            ))
            .expect("HOLDOUT");
        assert_eq!(batch.claims_consumed.len(), 2);
        apply(&mut registry, &batch.events);
        assert!(
            registry
                .propose_admission(&admission(
                    vec![config('a', 1)],
                    Some(Block::Holdout),
                    q,
                    registry.generation()
                ))
                .is_err()
        );
    }
    assert_eq!(registry.generation(), 1);
    assert_eq!(registry.holdout_reads_by_generation().get(&0), Some(&3));
    assert_eq!(
        registry
            .claims()
            .values()
            .filter(|c| c.consumed_by.is_some())
            .count(),
        6
    );
    let claims = registry
        .propose_claims(&[digest('a')], &hypothesis(1), &date(4))
        .expect("claim génération 1");
    apply(&mut registry, &claims);
    assert!(
        registry
            .propose_admission(&admission(vec![config('a', 1)], Some(Block::Holdout), 4, 0))
            .is_err()
    );
    let new_generation = registry
        .propose_admission(&admission(vec![config('a', 1)], Some(Block::Holdout), 4, 1))
        .expect("génération suivante");
    apply(&mut registry, &new_generation.events);
    let bytes = [header_csv(), encode_events(registry.events()).expect("CSV")].concat();
    let restored = Registry::from_csv(&bytes).expect("rejeu rotation");
    assert_eq!(restored.generation(), 1);
    assert_eq!(restored.holdout_reads_by_generation().get(&1), Some(&1));
}

#[test]
fn resultats_ne_modifient_pas_n_et_terminent_la_lecture() {
    let mut registry = Registry::default();
    let batch = registry
        .propose_admission(&admission(
            vec![config('a', 1), config('b', 1)],
            Some(Block::Dev),
            1,
            0,
        ))
        .expect("admission");
    apply(&mut registry, &batch.events);
    let results = vec![
        TrialResult {
            config_hash: digest('a'),
            failed: false,
            metrics: Metrics {
                mu_r: Some(-0.25),
                n: Some(12),
                pf: Some(0.5),
            },
        },
        TrialResult {
            config_hash: digest('b'),
            failed: true,
            metrics: Metrics::default(),
        },
    ];
    let events = registry
        .propose_results(batch.read_id, &results, &date(1))
        .expect("résultats");
    apply(&mut registry, &events);
    assert_eq!(registry.n(), 2);
    assert!(registry.pending_reads().is_empty());
    assert!(
        registry
            .propose_results(batch.read_id, &results, &date(1))
            .is_err()
    );
    let bytes = [header_csv(), encode_events(registry.events()).expect("CSV")].concat();
    assert!(
        Registry::from_csv(&bytes)
            .expect("rejeu résultats")
            .pending_reads()
            .is_empty()
    );
}

#[test]
fn csv_echappe_les_champs_et_rejette_les_guillemets_mal_fermes() {
    let registry = Registry::default();
    let mut cfg = config('a', 1);
    cfg.family = "F1, \"cassure\"".into();
    let batch = registry
        .propose_admission(&admission(vec![cfg], Some(Block::Dev), 1, 0))
        .expect("admission");
    let bytes = [header_csv(), encode_events(&batch.events).expect("CSV")].concat();
    assert_eq!(Registry::from_csv(&bytes).expect("CSV échappé").n(), 1);
    let malformed = [header_csv(), b"\"inacheve\n".to_vec()].concat();
    assert!(Registry::from_csv(&malformed).is_err());
}

#[test]
fn refuse_la_reouverture_d_un_read_id_apres_une_autre_lecture() {
    let mut registry = Registry::default();
    seed(&mut registry, vec![config('a', 1), config('b', 1)]);
    let first = registry
        .propose_admission(&admission(
            vec![config('a', 1)],
            Some(Block::Validation),
            2,
            0,
        ))
        .expect("VALIDATION");
    apply(&mut registry, &first.events);
    let intervening = registry
        .propose_admission(&admission(vec![config('a', 1)], Some(Block::Dev), 2, 0))
        .expect("DEV");
    apply(&mut registry, &intervening.events);
    let mut extra = first.events.last().expect("réservation").clone();
    extra.event_id.0 = registry.events().last().expect("événement").event_id.0 + 1;
    extra.config_hash = digest('b');
    extra.trial_id = registry
        .trials()
        .get(&digest('b'))
        .expect("profil connu")
        .id;
    assert!(
        registry.with_events(&[extra]).is_err(),
        "un read_id ne doit pas regrouper des invocations ultérieures"
    );
}

#[test]
fn termine_un_run_historique_apres_rattachement_de_la_fiche() {
    let mut registry = Registry::default();
    let mut cfg = config('a', 1);
    cfg.hypothesis = None;
    let batch = registry
        .propose_admission(&admission(vec![cfg], None, 1, 0))
        .expect("démo");
    apply(&mut registry, &batch.events);
    let link = registry
        .propose_link(&[digest('a')], &hypothesis(1), "F1", &date(1))
        .expect("fiche");
    apply(&mut registry, &link);
    let results = [TrialResult {
        config_hash: digest('a'),
        failed: true,
        metrics: Metrics::default(),
    }];
    let events = registry
        .propose_results(batch.read_id, &results, &date(1))
        .expect("l'ancienne provenance de la lecture reste valide");
    apply(&mut registry, &events);
    assert!(registry.pending_reads().is_empty());
}

#[test]
fn rejette_les_profils_incoherents_et_champs_non_applicables() {
    let mut registry = Registry::default();
    seed(&mut registry, vec![config('a', 1), config('b', 1)]);
    let grouped = registry
        .propose_admission(&admission(
            vec![config('a', 1), config('b', 1)],
            Some(Block::Validation),
            2,
            0,
        ))
        .expect("lot");
    for variant in 0..5 {
        let mut events = grouped.events.clone();
        let event = events.last_mut().expect("profil");
        match variant {
            0 => event.generation += 1,
            1 => event.dataset_hash = Some(digest('d')),
            2 => event.hypothesis = Some(hypothesis(2)),
            3 => event.claim_holdout = Some(tv4::research::registry::ClaimId(123)),
            _ => event.date = date(3),
        }
        assert!(registry.with_events(&events).is_err());
    }
    let mut mixed = admission(
        vec![config('a', 1), config('b', 2)],
        Some(Block::Validation),
        2,
        0,
    );
    assert!(registry.propose_admission(&mixed).is_err());
    mixed.configs[1] = config('a', 1);
    assert!(registry.propose_admission(&mixed).is_err());
    let claims = registry
        .propose_claims(&[digest('a')], &hypothesis(1), &date(2))
        .expect("claim");
    let mut bad_claim = claims[0].clone();
    bad_claim.metrics.n = Some(0);
    assert!(registry.with_events(&[bad_claim]).is_err());
    apply(&mut registry, &claims);
    let holdout = registry
        .propose_admission(&admission(vec![config('a', 1)], Some(Block::Holdout), 2, 0))
        .expect("HOLDOUT");
    let mut mismatch = holdout.events.clone();
    mismatch[0].claim_holdout = Some(tv4::research::registry::ClaimId(999));
    assert!(registry.with_events(&mismatch).is_err());
    apply(&mut registry, &holdout.events);
    let mut reused = holdout.events[0].clone();
    reused.read_id = Some(tv4::research::registry::ReadId(holdout.read_id.0 + 1));
    reused.event_id.0 += 1;
    reused.date = date(3);
    assert!(registry.with_events(&[reused]).is_err());
    let next = registry.propose_admission(&admission(
        vec![config('b', 1), config('c', 1)],
        Some(Block::Holdout),
        3,
        0,
    ));
    assert!(next.is_err());
    assert_eq!(registry.n(), 2, "profil inconnu non inscrit par erreur");
}

#[test]
fn rattache_la_famille_reelle_sans_effacer_l_origine_demo() {
    let mut registry = Registry::default();
    let mut cfg = config('a', 1);
    cfg.hypothesis = None;
    cfg.family = "unregistered_hypothesis".into();
    let demo = registry
        .propose_admission(&admission(vec![cfg], None, 1, 0))
        .expect("démo");
    apply(&mut registry, &demo.events);
    let linked = registry
        .propose_link(&[digest('a')], &hypothesis(1), "F1", &date(1))
        .expect("rattachement explicite de la famille");
    apply(&mut registry, &linked);
    assert!(
        registry
            .propose_admission(&admission(
                vec![config('a', 1)],
                Some(Block::Validation),
                2,
                0
            ))
            .is_ok()
    );
    let trial = registry.trials().get(&digest('a')).expect("essai");
    assert_eq!(trial.family, "F1");
    assert!(trial.origin_unregistered);
    assert_eq!(registry.events()[0].family, "unregistered_hypothesis");
    let results = [TrialResult {
        config_hash: digest('a'),
        failed: true,
        metrics: Metrics::default(),
    }];
    let events = registry
        .propose_results(demo.read_id, &results, &date(2))
        .expect("résultat ancienne famille");
    apply(&mut registry, &events);
    let bytes = [header_csv(), encode_events(registry.events()).expect("CSV")].concat();
    assert_eq!(Registry::from_csv(&bytes).expect("rejeu").n(), 1);
}

#[test]
fn les_fenetres_de_decision_sont_conservees_par_lecture_et_recoupees_au_rejeu() {
    let mut registry = Registry::default();
    let mut request = admission(vec![config('a', 1), config('b', 1)], Some(Block::Dev), 1, 0);
    request.decision_start_ms = Some(1_000);
    assert!(
        registry.propose_admission(&request).is_err(),
        "fenetre partielle refusee"
    );
    request.decision_end_ms = Some(1_000);
    assert!(
        registry.propose_admission(&request).is_err(),
        "fenetre vide refusee"
    );
    request.decision_end_ms = Some(2_000);
    let batch = registry
        .propose_admission(&request)
        .expect("fenetre valide");
    apply(&mut registry, &batch.events);
    let read = &registry.reads()[&batch.read_id];
    assert_eq!(read.decision_start_ms, Some(1_000));
    assert_eq!(read.decision_end_ms, Some(2_000));
    let bytes = [header_csv(), encode_events(registry.events()).expect("CSV")].concat();
    assert_eq!(Registry::from_csv(&bytes).unwrap(), registry);
    let mut corrupted = batch.events.clone();
    let last = corrupted.last_mut().unwrap();
    last.decision_end_ms = Some(3_000);
    assert!(
        Registry::default().with_events(&corrupted).is_err(),
        "profils du meme read_id divergents"
    );
    let result = TrialResult {
        config_hash: digest('a'),
        failed: false,
        metrics: Metrics::default(),
    };
    let other = TrialResult {
        config_hash: digest('b'),
        failed: true,
        metrics: Metrics::default(),
    };
    let mut results = registry
        .propose_results(batch.read_id, &[result, other], &date(1))
        .unwrap();
    results[0].decision_start_ms = Some(500);
    assert!(
        registry.with_events(&results).is_err(),
        "resultat sur une autre fenetre"
    );
}
