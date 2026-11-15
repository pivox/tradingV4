//! Gardes publiques du rôle réservé ; le journal historique garde son schéma.
use tv4::model::Block;
use tv4::research::registry::{
    Admission, AdmissionTime, ConfigSpec, Digest, HypothesisRef, Registry, UtcDate,
};
fn digest(c: char) -> Digest {
    Digest::new(c.to_string().repeat(64)).unwrap()
}
fn hypothesis() -> HypothesisRef {
    HypothesisRef::new("H-001".into(), digest('f')).unwrap()
}
fn request(family: &str) -> Admission {
    let date = UtcDate::new("2026-01-01T00:00:00Z").unwrap();
    Admission {
        configs: vec![ConfigSpec {
            config_hash: digest('a'),
            family: family.into(),
            hypothesis: Some(hypothesis()),
        }],
        dataset_hash: digest('e'),
        block: Some(Block::Dev),
        time: AdmissionTime::new(date.clone(), date.quarter()).unwrap(),
        generation: 0,
        decision_start_ms: None,
        decision_end_ms: None,
    }
}
#[test]
fn admission_candidate_refuse_la_famille_reservee_meme_forgee() {
    for family in [
        "__tv4_null_v1__:f1:1",
        "__tv4_null_v1__",
        "__tv4_null_v1__:f1:1:extra",
    ] {
        assert!(
            Registry::default()
                .propose_admission(&request(family))
                .is_err(),
            "{family}"
        );
    }
}
#[test]
fn liaison_publique_ne_peut_introduire_un_role_null() {
    let mut request = request("unregistered_hypothesis");
    request.block = None;
    request.configs[0].hypothesis = None;
    let batch = Registry::default().propose_admission(&request).unwrap();
    let registry = Registry::default().with_events(&batch.events).unwrap();
    assert!(
        registry
            .propose_link(
                &[digest('a')],
                &hypothesis(),
                "__tv4_null_v1__:f1:1",
                request.time.date()
            )
            .is_err()
    );
}
