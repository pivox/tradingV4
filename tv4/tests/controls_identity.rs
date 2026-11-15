//! Identités numériques des contrôles, indépendantes de l'admission courante.
use tv4::{
    model::hash::canonical_json,
    report::controls::identity::*,
    research::controls::{Budgets, ControlMode, ProgramBudgets},
};
fn hash(c: char) -> String {
    c.to_string().repeat(64)
}
fn program(index: u32) -> NullProgramIdentity {
    NullProgramIdentity::new(
        hash('a'),
        hash('b'),
        ControlMode::RemoveBtc,
        false,
        u64::MAX,
        index,
        ProgramBudgets::from(&Budgets::default()),
    )
    .unwrap()
}
#[test]
fn programme_alias_et_seed_max_sont_canoniques_et_stables_sur_extension_k() {
    let p = program(0);
    let json = canonical_json(&p).unwrap();
    assert!(json.contains("\"master_seed\":\"18446744073709551615\""));
    assert!(json.contains("\"effective_intervention\":\"preserve_btc\""));
    assert!(!json.contains("total_engine_steps"));
    assert_eq!(
        p.config_hash().unwrap(),
        NullProgramIdentity::new(
            hash('a'),
            hash('b'),
            ControlMode::PreserveBtc,
            true,
            u64::MAX,
            0,
            ProgramBudgets::from(&Budgets::default())
        )
        .unwrap()
        .config_hash()
        .unwrap()
    );
    let first = (0..200)
        .map(|i| program(i).config_hash().unwrap())
        .collect::<Vec<_>>();
    let extended = (0..1000)
        .map(|i| program(i).config_hash().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(first, extended[..200]);
}
#[test]
fn programme_refuse_seed_noncanonique_et_domaines_modifies() {
    let json = canonical_json(&program(0)).unwrap();
    for changed in [
        json.replace("18446744073709551615", "018446744073709551615"),
        json.replace("tv4-control-program-v1", "other"),
    ] {
        let parsed = serde_json::from_str::<NullProgramIdentity>(&changed);
        assert!(parsed.is_err() || parsed.unwrap().config_hash().is_err());
    }
    assert_ne!(
        program(0).config_hash().unwrap(),
        program(1).config_hash().unwrap()
    );
}
fn source(c: char) -> SourceIdentity {
    SourceIdentity {
        config_hash: hash(c),
        run_id: hash('c'),
        hashes: SourceHashes {
            metrics: hash('d'),
            trades: hash('e'),
            equity: hash('f'),
            stats_inputs: hash('1'),
            manifest: hash('2'),
        },
    }
}
fn run() -> ControlRunIdentity {
    use std::collections::BTreeMap;
    use tv4::research::{
        controls::AnnualQuota,
        stats::{StatsSizing, Window},
    };
    ControlRunIdentity {
        program_hashes: vec![
            program(0).config_hash().unwrap(),
            program(1).config_hash().unwrap(),
        ],
        sources: vec![source('a'), source('b')],
        catalogue_hash: hash('c'),
        quotas: BTreeMap::from([
            (
                hash('a'),
                BTreeMap::from([(
                    "BTC-USDC".into(),
                    vec![AnnualQuota {
                        year: 2021,
                        filled: 1,
                        retained: 1,
                    }],
                )]),
            ),
            (
                hash('b'),
                BTreeMap::from([(
                    "ETH-USDC".into(),
                    vec![AnnualQuota {
                        year: 2021,
                        filled: 2,
                        retained: 2,
                    }],
                )]),
            ),
        ]),
        dataset_hash: format!("blake3:{}", hash('3')),
        window: Window {
            start_ms: 0,
            end_ms: 60_000,
        },
        initial_cash: 300.0,
        sizing: StatsSizing::Research,
        profile: Some(ControlProfile::Baseline),
        line: Some(ControlLine::Full),
        costs: Some(CostIdentity {
            config_hash: hash('4'),
            seed: 1,
        }),
        engine: EngineIdentity {
            commit: "abc".into(),
            binary_hash: hash('5'),
        },
        total_engine_steps: 999,
    }
}
#[test]
fn run_lie_sources_peers_quotas_capital_exact_et_plafond_global_sans_ordre() {
    let a = run();
    let base = a.control_run_id().unwrap();
    let mut b = a.clone();
    b.sources.reverse();
    b.program_hashes.reverse();
    assert_eq!(base, b.control_run_id().unwrap());
    let mut changes = Vec::new();
    b = a.clone();
    b.initial_cash = f64::from_bits(a.initial_cash.to_bits() + 1);
    changes.push(b);
    b = a.clone();
    b.total_engine_steps += 1;
    changes.push(b);
    b = a.clone();
    b.dataset_hash = hash('6');
    changes.push(b);
    b = a.clone();
    b.sources[1].hashes.manifest = hash('7');
    changes.push(b);
    b = a.clone();
    b.quotas
        .get_mut(&hash('b'))
        .unwrap()
        .get_mut("ETH-USDC")
        .unwrap()[0]
        .filled = 3;
    b.quotas
        .get_mut(&hash('b'))
        .unwrap()
        .get_mut("ETH-USDC")
        .unwrap()[0]
        .retained = 3;
    changes.push(b);
    for b in changes {
        assert_ne!(base, b.control_run_id().unwrap());
        assert_eq!(a.program_hashes, b.program_hashes);
    }
    let replay = ReplayIdentity {
        control_run_id: base.clone(),
        program_hash: a.program_hashes[0].clone(),
        attempt: 0,
        replay_trace_hash: hash('8'),
    };
    let mut changed = replay.clone();
    changed.attempt = 1;
    assert_ne!(
        replay.replay_hash().unwrap(),
        changed.replay_hash().unwrap()
    );
    changed = replay.clone();
    changed.replay_trace_hash = hash('9');
    assert_ne!(
        replay.replay_hash().unwrap(),
        changed.replay_hash().unwrap()
    );
}

#[test]
fn programme_engage_chaque_option_propre_mais_pas_le_plafond_global() {
    let budgets = Budgets::default();
    let base = NullProgramIdentity::new(
        hash('a'),
        hash('b'),
        ControlMode::PreserveBtc,
        true,
        42,
        0,
        ProgramBudgets::from(&budgets),
    )
    .unwrap()
    .config_hash()
    .unwrap();
    let mut alternatives = vec![
        NullProgramIdentity::new(
            hash('a'),
            hash('b'),
            ControlMode::PreserveBtc,
            true,
            43,
            0,
            ProgramBudgets::from(&budgets),
        )
        .unwrap(),
        NullProgramIdentity::new(
            hash('a'),
            hash('b'),
            ControlMode::RemoveBtc,
            true,
            42,
            0,
            ProgramBudgets::from(&budgets),
        )
        .unwrap(),
        NullProgramIdentity::new(
            hash('a'),
            hash('c'),
            ControlMode::PreserveBtc,
            true,
            42,
            0,
            ProgramBudgets::from(&budgets),
        )
        .unwrap(),
        NullProgramIdentity::new(
            hash('c'),
            hash('b'),
            ControlMode::PreserveBtc,
            true,
            42,
            0,
            ProgramBudgets::from(&budgets),
        )
        .unwrap(),
    ];
    for i in 0..5 {
        let mut own = ProgramBudgets::from(&budgets);
        match i {
            0 => own.probe_steps_per_geometry += 1,
            1 => own.dp_cells_per_geometry += 1,
            2 => own.dp_bytes_per_geometry += 1,
            3 => own.attempts_per_replication += 1,
            _ => own.replay_steps_per_replication += 1,
        }
        alternatives.push(
            NullProgramIdentity::new(
                hash('a'),
                hash('b'),
                ControlMode::PreserveBtc,
                true,
                42,
                0,
                own,
            )
            .unwrap(),
        );
    }
    for alternative in alternatives {
        assert_ne!(base, alternative.config_hash().unwrap());
    }
    let mut global = budgets;
    global.total_engine_steps += 1;
    assert_eq!(
        base,
        NullProgramIdentity::new(
            hash('a'),
            hash('b'),
            ControlMode::PreserveBtc,
            true,
            42,
            0,
            ProgramBudgets::from(&global)
        )
        .unwrap()
        .config_hash()
        .unwrap()
    );
}
#[test]
fn run_refuse_sources_dupliquees_et_quotas_sans_parent_ou_manquants() {
    let mut r = run();
    r.sources.push(r.sources[0].clone());
    assert!(r.control_run_id().is_err());
    let mut r = run();
    r.quotas.remove(&hash('a'));
    assert!(r.control_run_id().is_err());
    let mut r = run();
    r.quotas.insert(hash('f'), Default::default());
    assert!(r.control_run_id().is_err());
}
#[test]
fn fixtures_canonique_et_empreintes_versionnees() {
    let p = program(0);
    let expected = format!(
        r#"{{"algorithm":"interval-year-dp-biguint-v1","effective_intervention":"preserve_btc","geometry_hash":"{}","master_seed":"18446744073709551615","parent_config_hash":"{}","program_budgets":{{"attempts_per_replication":128,"dp_bytes_per_geometry":268435456,"dp_cells_per_geometry":2000000,"probe_steps_per_geometry":10000000000,"replay_steps_per_replication":5000000000}},"quota_rule":"entry-fill-utc-filled-retained-v1","replication_index":0,"schema":"tv4-control-program-v1"}}"#,
        hash('b'),
        hash('a')
    );
    assert_eq!(canonical_json(&p).unwrap(), expected);
    let control_run_id = run().control_run_id().unwrap();
    let replay = ReplayIdentity {
        control_run_id: control_run_id.clone(),
        program_hash: p.config_hash().unwrap(),
        attempt: 0,
        replay_trace_hash: hash('8'),
    };
    assert_eq!(
        p.config_hash().unwrap(),
        "03b3444a9b54a0b84caaab30ca5c4aff8542edc15c8b9875c3dbdf7236c29225"
    );
    assert_eq!(
        control_run_id,
        "f0305c0f0fa5fe943206e7ba95237be54c0c7e427e9ee17a994fecd9a8a451a3"
    );
    assert_eq!(
        replay.replay_hash().unwrap(),
        "335ec70e0c93cccca60d50e56e52dd56dc6d08037a6d917b87b8726ce4ee8ab6"
    );
    assert_eq!(p.parent_config_hash(), hash('a'));
    assert_eq!(p.geometry_hash(), hash('b'));
    assert_eq!(p.effective_intervention(), ControlMode::PreserveBtc);
    assert_eq!(p.master_seed(), u64::MAX);
    assert_eq!(p.replication_index(), 0);
    assert_eq!(
        p.program_budgets(),
        ProgramBudgets::from(&Budgets::default())
    );
}

#[test]
fn budget_nul_reste_un_programme_identifiable_et_un_run_publiable() {
    let base = program(0).config_hash().unwrap();
    for i in 0..5 {
        let mut budgets = Budgets::default();
        match i {
            0 => budgets.probe_steps_per_geometry = 0,
            1 => budgets.dp_cells_per_geometry = 0,
            2 => budgets.dp_bytes_per_geometry = 0,
            3 => budgets.attempts_per_replication = 0,
            _ => budgets.replay_steps_per_replication = 0,
        }
        let mut options = tv4::research::controls::ControlOptions::new(u64::MAX);
        options.budgets = budgets;
        assert!(options.validate().is_ok());
        let p = NullProgramIdentity::new(
            hash('a'),
            hash('b'),
            ControlMode::PreserveBtc,
            true,
            u64::MAX,
            0,
            ProgramBudgets::from(&budgets),
        )
        .unwrap();
        assert_ne!(base, p.config_hash().unwrap());
    }
    let mut r = run();
    let base = r.control_run_id().unwrap();
    r.total_engine_steps = 0;
    assert_ne!(base, r.control_run_id().unwrap());
}
