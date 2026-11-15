//! Bundles S14 natifs : octets possédés, projections et publication exclusive.
mod controls_common;
mod portfolio_common;

use std::collections::BTreeMap;
use std::{fs, path::Path};
use tv4::report::controls::identity::*;
use tv4::report::controls::{self, *};
use tv4::report::{self, stats, verify};
use tv4::research::controls::*;

fn source(dir: &Path, signal: i64) -> verify::VerifiedRun {
    source_kind(dir, signal, false)
}
fn source_kind(dir: &Path, signal: i64, protected: bool) -> verify::VerifiedRun {
    use portfolio_common::*;
    use tv4::engine::{PortfolioInputs, run_portfolio};
    use tv4::model::{ExchangeId, Timeframe, hash};
    use tv4::report::manifest::{ManifestS9Inputs, Timings};
    use tv4::report::metrics_s9::*;
    use tv4::report::stats_inputs::*;
    use tv4::research::stats::*;
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.)];
    let data = controls_common::dataset(vec![(universe[0].clone(), flat_series(0, 30, 100., 1e6))]);
    let fixture = controls_common::Fixture {
        signal_ms: T0 + signal * MINUTE,
        hold_ms: 2 * MINUTE,
        ..Default::default()
    };
    let research = controls_common::Research::new(T0, T0 + 30 * MINUTE);
    let inputs = PortfolioInputs {
        series: &data.series,
        strategies: fixture.set(1),
        config: config(&universe, &instruments, 300.),
    };
    let result = if protected {
        tv4::engine::run_portfolio_research(inputs, &research.spec()).unwrap()
    } else {
        run_portfolio(inputs).unwrap()
    };
    let metrics_inputs = MetricsS9Inputs {
        dataset: DatasetInfo {
            name: "controls-fixture".into(),
            exchange: ExchangeId::Okx,
            tf: Timeframe::M1,
            gaps_policy: "skip".into(),
            hash_format: "dsh-v1".into(),
            dataset_hash: hash::blake3_prefixed(b"controls-fixture-dataset"),
            series: vec![SeriesInfo {
                symbol: universe[0].clone(),
                rows: 30,
                first_ms: T0,
                last_ms: T0 + 29 * MINUTE,
            }],
        },
        instruments: InstrumentsInfo::toml(),
        okx_pairs: None,
        markets: vec![report::executability::LegMarket {
            open_time_ms: data.series[0].1.open_time_ms(),
            low: data.series[0].1.low(),
            close: data.series[0].1.close(),
            volume: data.series[0].1.volume(),
        }],
    };
    let metrics = if protected {
        MetricsS9::new_research(
            &result,
            metrics_inputs,
            &report::research::ResearchInputs {
                guard: report::research::Guard::None,
                warmup_start_ms: T0,
                universe: None,
                quality: &research.quality,
                subset: None,
            },
        )
        .unwrap()
    } else {
        MetricsS9::new(&result, metrics_inputs).unwrap()
    };
    let metrics_bytes = report::json_bytes(&metrics).unwrap();
    let trades = report::writers::trades_csv_legs(
        &result.trades,
        &result.trade_legs,
        &["X-USD"],
        30 * MINUTE,
    )
    .unwrap();
    let equity =
        report::equity::equity_csv(&result.grid_open_ms, &result.equity, 300., Timeframe::M1)
            .unwrap();
    let engine = report::EngineInfo {
        tv4_version: "fixture".into(),
        git_sha: "fixture-commit".into(),
        rustc: "fixture".into(),
        rustc_commit: "fixture".into(),
        target: "fixture".into(),
        target_cpu: "fixture".into(),
        profile: "fixture".into(),
        binary_hash: hash::blake3_prefixed(b"fixture-engine"),
    };
    let manifest_inputs = ManifestS9Inputs {
        instruments_file: None,
        engine,
        equity_step: Timeframe::M1,
        files: report::manifest::OutputFiles {
            metrics_json: hash::blake3_prefixed(&metrics_bytes),
            trades_csv: hash::blake3_prefixed(&trades),
            equity_csv: hash::blake3_prefixed(&equity),
        },
        started_at_ms: T0,
        timings: Timings {
            load_ms: 0,
            run_ms: 0,
            cli_cold_ms: 0,
        },
    };
    let mut manifest = if protected {
        report::ManifestS9::new_s11(&metrics, manifest_inputs, BTreeMap::new()).unwrap()
    } else {
        report::ManifestS9::new(&metrics, manifest_inputs).unwrap()
    };
    fs::create_dir_all(dir).unwrap();
    for (file, bytes) in [
        ("metrics.json", metrics_bytes),
        ("trades.csv", trades),
        ("equity.csv", equity),
        ("manifest.json", report::json_bytes(&manifest).unwrap()),
    ] {
        fs::write(dir.join(file), bytes).unwrap();
    }
    let run = verify::load_verified_run(dir).unwrap();
    let image = stats::snapshot(&run).unwrap();
    let packet = StatsInputs {
        schema: STATS_INPUTS_SCHEMA.into(),
        conventions: CONVENTIONS.into(),
        provenance: image.provenance,
        pair_symbols: image.pair_symbols,
        sizing: image.sizing,
        decision_timeframe_minutes: 1,
        closures: image
            .trades
            .iter()
            .map(|t| TradeClosure {
                row: t.row,
                at_ms: T0 + 30 * MINUTE,
            })
            .collect(),
        marks: vec![
            MarkPoint {
                at_ms: T0,
                equity: 300.,
                day_close: false,
            },
            MarkPoint {
                at_ms: T0 + 30 * MINUTE,
                equity: image.equity_final,
                day_close: false,
            },
        ],
        cash: vec![
            CashPoint {
                at_ms: T0,
                cash: 300.,
            },
            CashPoint {
                at_ms: T0 + 30 * MINUTE,
                cash: image.cash_final,
            },
        ],
        markets: vec![],
    };
    let bytes = report::json_bytes(&packet).unwrap();
    manifest.stats_inputs = Some(StatsInputsRef {
        file: STATS_INPUTS_FILE.into(),
        schema: STATS_INPUTS_SCHEMA.into(),
        hash: hash::blake3_prefixed(&bytes),
    });
    fs::write(dir.join(STATS_INPUTS_FILE), bytes).unwrap();
    fs::write(
        dir.join("manifest.json"),
        report::json_bytes(&manifest).unwrap(),
    )
    .unwrap();
    verify::load_verified_run(dir).unwrap()
}

#[test]
fn controls_index_prime_sur_le_run_et_un_index_invalide_echoue() {
    let dir = tempfile::tempdir().unwrap();
    source(dir.path(), 1);
    fs::write(dir.path().join("controls-index.json"), b"{}\n").unwrap();
    assert!(
        verify::load_stats_leaves(dir.path()).is_err(),
        "un index présent invalide doit primer sur le run historique"
    );
}

fn experiment(
    k: u32,
    unavailable: bool,
) -> (
    ControlsContext,
    Vec<SourceMetadata>,
    Vec<verify::VerifiedRun>,
    tv4::engine::controls::BatchOutcome,
) {
    use portfolio_common::*;
    use tv4::engine::controls::*;
    let dir = tempfile::tempdir().unwrap();
    let runs = vec![
        source(&dir.path().join("a"), 1),
        source(&dir.path().join("b"), 2),
    ];
    let fixtures = [1, 2].map(|signal| controls_common::Fixture {
        signal_ms: T0 + signal * MINUTE,
        hold_ms: 2 * MINUTE,
        ..Default::default()
    });
    let metadata = runs
        .iter()
        .zip(&fixtures)
        .map(|(run, f)| SourceMetadata {
            config_hash: run.manifest()["config_hash"].as_str().unwrap().into(),
            geometry_hash: tv4::model::hash::hash_json(
                &tv4::strategy::controls::ControlGeometry::descriptor(f),
            )
            .unwrap(),
            has_btc_filter: false,
        })
        .collect();
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.)];
    let factories: Vec<_> = fixtures.iter().map(|f| move || Ok(f.set(1))).collect();
    let mut options = ControlOptions::new(u64::MAX);
    options.replications = k;
    if unavailable {
        options.budgets.probe_steps_per_geometry = 0;
    }
    let outcome = run_batch(
        BatchInputs {
            dataset: controls_common::dataset(vec![(
                universe[0].clone(),
                flat_series(0, 30, 100., 1e6),
            )]),
            sources: factories
                .iter()
                .map(|factory| SourceRuntime {
                    config: config(&universe, &instruments, 300.),
                    costs: None,
                    research: None,
                    quotas: BTreeMap::from([(
                        universe[0].clone(),
                        vec![AnnualQuota {
                            year: 2024,
                            filled: 1,
                            retained: 1,
                        }],
                    )]),
                    factory,
                })
                .collect(),
            options: options.clone(),
        },
        &mut |i, observed| {
            let ObservedRun::Plain(result) = observed else {
                panic!("plain")
            };
            assert_eq!(
                result.config_hash,
                runs[i].manifest()["config_hash"].as_str().unwrap()
            );
            assert_eq!(
                report::writers::trades_csv_legs(
                    &result.trades,
                    &result.trade_legs,
                    &["X-USD"],
                    30 * MINUTE
                )
                .unwrap(),
                runs[i].trades_bytes()
            );
            assert_eq!(
                report::equity::equity_csv(
                    &result.grid_open_ms,
                    &result.equity,
                    300.,
                    tv4::model::Timeframe::M1
                )
                .unwrap(),
                runs[i].equity_bytes()
            );
            Ok(ObservedValidation::Matched)
        },
    )
    .unwrap();
    let context = ControlsContext {
        engine: EngineIdentity {
            commit: "fixture-commit".into(),
            binary_hash: tv4::model::hash::blake3_prefixed(b"fixture-engine"),
        },
        dataset_hash: runs[0].manifest()["dataset_hash"].as_str().unwrap().into(),
        window: ControlsWindow {
            start_ms: T0,
            end_ms: T0 + 30 * MINUTE,
        },
        initial_cash: 300.,
        sizing: tv4::research::stats::StatsSizing::Research,
        line: None,
        profile: None,
        costs: None,
        options: ControlsOptions::from(&options),
        source_block: None,
        effective_block: None,
    };
    (context, metadata, runs, outcome)
}
fn admission(
    context: &ControlsContext,
    metadata: &[SourceMetadata],
    read_id: u64,
) -> CurrentAdmission {
    use tv4::report::governance::GovernanceContext;
    let mut hashes = metadata
        .iter()
        .map(|m| m.config_hash.clone())
        .collect::<Vec<_>>();
    for m in metadata {
        for mode in context
            .options
            .as_engine()
            .effective_modes(m.has_btc_filter)
        {
            for k in 0..context.options.replications {
                hashes.push(
                    NullProgramIdentity::new(
                        m.config_hash.clone(),
                        m.geometry_hash.clone(),
                        mode,
                        m.has_btc_filter,
                        context.options.seed,
                        k,
                        ProgramBudgets::from(&context.options.budgets),
                    )
                    .unwrap()
                    .config_hash()
                    .unwrap(),
                );
            }
        }
    }
    hashes.sort();
    hashes.dedup();
    let n = hashes.len() as u64;
    CurrentAdmission {
        candidates: metadata.len() as u64,
        null_controls: n - metadata.len() as u64,
        contexts: hashes
            .into_iter()
            .enumerate()
            .map(|(i, h)| {
                (
                    h.clone(),
                    GovernanceContext {
                        schema: "tv4-governance-v1".into(),
                        config_hash: h,
                        dataset_hash: context.dataset_hash.clone(),
                        trial_id: i as u64 + 1,
                        read_id,
                        hypothesis: context.source_block.map(|_| {
                            tv4::report::governance::HypothesisContext {
                                identifiant: "H-001".into(),
                                sha256: "0".repeat(64),
                            }
                        }),
                        frozen_tag: None,
                        frozen_at: None,
                        source_block: context.source_block,
                        effective_block: context.effective_block,
                        generation: 0,
                        claim_holdout: None,
                        noise: tv4::research::noise::noise_context(n, 1.).unwrap(),
                    },
                )
            })
            .collect(),
    }
}
fn bundle(k: u32, unavailable: bool, read_id: u64) -> ControlsBundleBytes {
    let (c, m, r, o) = experiment(k, unavailable);
    build_bundle(&c, &m, &r, &o, &admission(&c, &m, read_id)).unwrap()
}
fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn bundle_portable_projections_bijectives_et_feuilles_autonomes() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    let bytes = bundle(20, false, 1);
    publish(&out, &bytes).unwrap();
    let loaded = controls::verify::load_bundle(&out).unwrap();
    assert_eq!(loaded.leaves.len(), 2);
    assert_eq!(loaded.document.n_grid, 2);
    assert!(loaded.document.smoke);
    assert!(loaded.document.technical_only);
    assert_eq!(loaded.document.results.len(), 2);
    assert_eq!(verify::load_stats_leaves(&out).unwrap().leaves.len(), 2);
    for (dir, run) in &loaded.leaves {
        let projection = json(&dir.join("controls.json"));
        assert_eq!(projection["kind"], "projection");
        assert_eq!(projection["results"].as_array().unwrap().len(), 1);
        assert_eq!(projection["sources"].as_array().unwrap().len(), 2);
        assert_eq!(
            projection["statistics"][0]["maxima"]
                .as_array()
                .unwrap()
                .len(),
            20
        );
        assert_eq!(
            projection["statistics"][0]["sources"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let standalone = root.path().join(dir.file_name().unwrap());
        fs::create_dir(&standalone).unwrap();
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), standalone.join(entry.file_name())).unwrap();
        }
        assert!(
            verify::verify(&standalone, &standalone)
                .unwrap()
                .identical()
        );
        assert_eq!(
            verify::load_stats_leaves(&standalone)
                .unwrap()
                .layout_files
                .len(),
            2
        );
        assert_eq!(
            run.trades_bytes(),
            fs::read(standalone.join("trades.csv")).unwrap()
        );
        assert!(!dir.join("stats.json").exists());
    }
    assert!(verify::verify(&out, &out).unwrap().identical());
    assert!(publish(&out, &bytes).is_err());
}

#[test]
fn admissions_actuelles_differentes_numeriques_identiques_et_octets_sources_possedes() {
    let root = tempfile::tempdir().unwrap();
    let (c, m, r, o) = experiment(20, false);
    let a = build_bundle(&c, &m, &r, &o, &admission(&c, &m, 1)).unwrap();
    let b = build_bundle(&c, &m, &r, &o, &admission(&c, &m, 2)).unwrap();
    let da = root.path().join("a");
    let db = root.path().join("b");
    publish(&da, &a).unwrap();
    publish(&db, &b).unwrap();
    assert_eq!(
        fs::read(da.join("controls.json")).unwrap(),
        fs::read(db.join("controls.json")).unwrap()
    );
    assert_ne!(
        fs::read(da.join("controls-manifest.json")).unwrap(),
        fs::read(db.join("controls-manifest.json")).unwrap()
    );
    assert!(verify::verify(&da, &db).unwrap().identical());
    let index = json(&da.join("controls-index.json"));
    let relative = index["sources"][0]["dir"].as_str().unwrap();
    assert!(
        verify::verify(&da.join(relative), &db.join(relative))
            .unwrap()
            .identical()
    );
    for run in r {
        let h = run.manifest()["config_hash"].as_str().unwrap();
        assert_eq!(
            run.manifest_bytes(),
            fs::read(da.join("runs").join(h).join("manifest.json")).unwrap()
        );
    }
}

#[test]
fn publication_echouee_ne_declare_jamais_complete_et_nettoie_temporaires() {
    let root = tempfile::tempdir().unwrap();
    let bytes = bundle(2, false, 1);
    for event in [PublishEvent::BeforeFile, PublishEvent::BeforeRename] {
        for fail_at in [0, 5, bytes.files().len()] {
            let out = root.path().join(format!("{event:?}-{fail_at}"));
            let mut seen = 0;
            assert!(
                publish_with(&out, &bytes, &mut |current, _| {
                    if current == event {
                        let n = seen;
                        seen += 1;
                        if n == fail_at {
                            return Err(std::io::Error::other("injection"));
                        }
                    }
                    Ok(())
                })
                .is_err()
            );
            assert!(!out.join("controls-index.json").exists());
            assert!(!out.exists(), "le nouveau dossier incomplet est supprimé");
        }
    }
}

#[test]
fn preparation_indisponible_conserve_n_k_programmes_quotas_et_support_inconnu() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(3, true, 1)).unwrap();
    let panel = json(&out.join("controls.json"));
    assert_eq!(panel["n_grid"], 2);
    assert_eq!(panel["replications"], 3);
    assert_eq!(panel["status"], "inconclusive");
    assert_eq!(panel["results"].as_array().unwrap().len(), 2);
    assert_eq!(
        panel["results"][0]["modes"][0]["cells"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(panel["results"][0]["modes"][0]["pair_supports"][0]["cardinality"].is_null());
    assert_eq!(panel["sources"][0]["quotas"]["X-USD"][0]["filled"], 1);
    assert!(controls::verify::load_bundle(&out).is_ok());
}

#[test]
fn dto_strict_refuse_bool_entier_inconnu_et_support_non_canonique() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(2, false, 1)).unwrap();
    let original = json(&out.join("controls.json"));
    for (pointer, value) in [
        ("/smoke", serde_json::json!(1)),
        ("/n_grid", serde_json::json!(true)),
        ("/replications", serde_json::json!(2.0)),
        (
            "/results/0/modes/0/pair_supports/0/cardinality",
            serde_json::json!("01"),
        ),
        (
            "/context/options/seed",
            serde_json::json!(18446744073709551615_u64),
        ),
    ] {
        let mut value_doc = original.clone();
        *value_doc.pointer_mut(pointer).unwrap() = value;
        let parsed = serde_json::from_value::<ControlsDocument>(value_doc);
        assert!(
            parsed.is_err() || parsed.unwrap().validate().is_err(),
            "{pointer}"
        );
    }
    let mut changed = original;
    changed["unknown"] = serde_json::json!(0);
    assert!(serde_json::from_value::<ControlsDocument>(changed).is_err());
}

#[test]
fn en_tetes_ligne_profil_et_couts_doivent_provenir_des_sources() {
    let (c, m, r, o) = experiment(2, false);
    let mut changed = c.clone();
    // Chacun des en-têtes est une assertion de provenance, indépendamment du hash recalculé.
    changed.profile = Some(ControlProfile::Baseline);
    assert!(build_bundle(&changed, &m, &r, &o, &admission(&changed, &m, 1)).is_err());
    changed = c.clone();
    changed.line = Some(ControlLine::Full);
    assert!(build_bundle(&changed, &m, &r, &o, &admission(&changed, &m, 1)).is_err());
    changed = c.clone();
    changed.costs = Some(CostIdentity {
        config_hash: "f".repeat(64),
        seed: 12,
    });
    assert!(build_bundle(&changed, &m, &r, &o, &admission(&changed, &m, 1)).is_err());
}

#[test]
fn feuilles_alterees_et_liens_index_echanges_dupliques_ou_traversants_echouent() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(2, false, 1)).unwrap();
    let original = json(&out.join("controls-index.json"));
    for operation in 0..5 {
        let mut index = original.clone();
        match operation {
            0 => index["sources"][0]["dir"] = serde_json::json!("../escape"),
            1 => index["sources"][1] = index["sources"][0].clone(),
            2 => index["sources"].as_array_mut().unwrap().swap(0, 1),
            3 => {
                index["sources"][0]["controls"]["hash"] =
                    index["sources"][1]["controls"]["hash"].clone()
            }
            _ => index["complete"] = serde_json::json!(false),
        }
        fs::write(
            out.join("controls-index.json"),
            report::json_bytes(&index).unwrap(),
        )
        .unwrap();
        assert!(
            controls::verify::load_bundle(&out).is_err(),
            "opération {operation}"
        );
    }
    fs::write(
        out.join("controls-index.json"),
        report::json_bytes(&original).unwrap(),
    )
    .unwrap();
    let leaf = out.join(original["sources"][0]["dir"].as_str().unwrap());
    fs::write(leaf.join("controls.json"), b"{}\n").unwrap();
    assert!(verify::load_stats_leaves(&leaf).is_err());
    assert!(verify::verify(&leaf, &leaf).is_err());
    assert!(controls::verify::load_bundle(&out).is_err());
}

#[test]
fn stats_images_gardent_tous_sidecars_et_sources_sans_relecture() {
    let root = tempfile::tempdir().unwrap();
    let source_path = root.path().join("source");
    let run = source(&source_path, 1);
    let before = [
        run.manifest_bytes().to_vec(),
        run.metrics_bytes().to_vec(),
        run.trades_bytes().to_vec(),
        run.equity_bytes().to_vec(),
        run.stats_inputs().unwrap().bytes.clone(),
    ];
    for name in [
        "manifest.json",
        "metrics.json",
        "trades.csv",
        "equity.csv",
        "stats-inputs.json",
    ] {
        fs::write(source_path.join(name), b"remplace").unwrap();
    }
    assert_eq!(before[0], run.manifest_bytes());
    assert_eq!(before[4], run.stats_inputs().unwrap().bytes);
    // L'expérience n'a aucun propriétaire de chemin : les répertoires de ses sources ont déjà disparu.
    let (c, m, r, o) = experiment(2, false);
    let bundle = build_bundle(&c, &m, &r, &o, &admission(&c, &m, 1)).unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle).unwrap();
    let owned = verify::load_stats_leaves(&out).unwrap();
    assert_eq!(owned.layout_files.len(), 7);
    for (path, _) in &owned.layout_files {
        fs::write(path, b"remplace").unwrap();
    }
    for (path, run) in &owned.leaves {
        fs::write(path.join("trades.csv"), b"remplace").unwrap();
        assert!(!stats::snapshot(run).unwrap().trades.is_empty());
    }
    assert!(owned.layout_files.iter().all(|(_, b)| b.starts_with(b"{")));
}

#[test]
fn tri_conjoint_et_comparaison_des_controles_sur_feuille() {
    let root = tempfile::tempdir().unwrap();
    let (c, mut m, mut r, mut o) = experiment(3, false);
    let original = build_bundle(&c, &m, &r, &o, &admission(&c, &m, 1)).unwrap();
    m.reverse();
    r.reverse();
    o.sources.reverse();
    for stat in &mut o.statistics {
        stat.panel.sources.reverse();
    }
    let reordered = build_bundle(&c, &m, &r, &o, &admission(&c, &m, 1)).unwrap();
    assert_eq!(original.index_bytes(), reordered.index_bytes());
    for (a, b) in original.files().iter().zip(reordered.files()) {
        assert_eq!(a.relative_path, b.relative_path);
        assert_eq!(a.bytes, b.bytes);
    }
    let mut changed = c.clone();
    changed.options.budgets.total_engine_steps += 1;
    let changed = build_bundle(&changed, &m, &r, &o, &admission(&changed, &m, 1)).unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    publish(&a, &original).unwrap();
    publish(&b, &changed).unwrap();
    let index = json(&a.join("controls-index.json"));
    let leaf = index["sources"][0]["dir"].as_str().unwrap();
    assert!(
        !verify::verify(&a.join(leaf), &b.join(leaf))
            .unwrap()
            .identical()
    );
    assert!(!verify::verify(&a, &b).unwrap().identical());
    assert!(verify::verify(&a, &a.join(leaf)).is_err());
}

#[test]
fn validation_semantique_des_matrices_statistiques_supports_et_programmes() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(3, false, 1)).unwrap();
    let original = json(&out.join("controls.json"));
    for (pointer, value) in [
        ("/schema", serde_json::json!("ancien")),
        ("/canonical_reference", serde_json::json!("0".repeat(64))),
        ("/context/initial_cash", serde_json::json!(-1)),
        ("/context/sizing", serde_json::json!("compounding")),
        ("/context/effective_block", serde_json::json!("HOLDOUT")),
        (
            "/context/options/modes",
            serde_json::json!(["preserve_btc", "preserve_btc"]),
        ),
        ("/control_run_id", serde_json::json!("0".repeat(64))),
        (
            "/sources/0/geometry_hash",
            serde_json::json!("z".repeat(64)),
        ),
        (
            "/sources/0/identity/hashes/manifest",
            serde_json::json!("0".repeat(64)),
        ),
        ("/sources/0/quotas/X-USD/0/retained", serde_json::json!(2)),
        (
            "/results/0/modes/0/cells/0/replication",
            serde_json::json!(1),
        ),
        (
            "/results/0/modes/0/cells/0/config_hash",
            serde_json::json!("0".repeat(64)),
        ),
        (
            "/results/0/modes/0/cells/0/program/replication_index",
            serde_json::json!(1),
        ),
        (
            "/results/0/modes/0/cells/0/replay_hash",
            serde_json::json!("0".repeat(64)),
        ),
        ("/results/0/modes/0/cells/0/attempts", serde_json::json!(0)),
        (
            "/results/0/modes/0/cells/0/quota_checks/0/conformant",
            serde_json::json!(false),
        ),
        (
            "/results/0/modes/0/cells/0/availability",
            serde_json::json!("unavailable"),
        ),
        (
            "/results/0/modes/0/pair_supports/0/cardinality",
            serde_json::json!("0"),
        ),
        ("/results/0/modes/1/alias_of", serde_json::json!(null)),
        ("/statistics/0/resolution", serde_json::json!(0.5)),
        ("/statistics/0/numeric_admissible", serde_json::json!(true)),
        ("/statistics/0/maxima", serde_json::json!([])),
        ("/statistics/0/family_p_value", serde_json::json!(2)),
        (
            "/statistics/0/sources/0/temporal/samples",
            serde_json::json!(2),
        ),
        (
            "/statistics/0/sources/0/temporal/available",
            serde_json::json!(true),
        ),
    ] {
        let mut document = original.clone();
        *document.pointer_mut(pointer).unwrap() = value;
        let parsed = serde_json::from_value::<ControlsDocument>(document);
        assert!(
            parsed.is_err() || parsed.unwrap().validate().is_err(),
            "{pointer}"
        );
    }
    for pointer in [
        "/panel_hash",
        "/results/0/observed/reason",
        "/statistics/0/critical_value",
    ] {
        let mut value = original.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        value
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            serde_json::from_value::<ControlsDocument>(value).is_err(),
            "nullable requis : {pointer}"
        );
    }
}

#[test]
fn admission_exacte_et_cardinalites_sans_reduction_implicite() {
    let (c, m, r, o) = experiment(2, false);
    let mut a = admission(&c, &m, 1);
    a.contexts.pop_first();
    assert!(build_bundle(&c, &m, &r, &o, &a).is_err());
    let mut a = admission(&c, &m, 1);
    a.null_controls = 0;
    assert!(build_bundle(&c, &m, &r, &o, &a).is_err());
    let mut a = admission(&c, &m, 1);
    a.contexts.values_mut().next().unwrap().read_id = 2;
    assert!(build_bundle(&c, &m, &r, &o, &a).is_err());
    let mut bad = o.clone();
    bad.requested_sources = 1;
    assert!(build_bundle(&c, &m, &r, &bad, &admission(&c, &m, 1)).is_err());
    let mut bad = o.clone();
    bad.sources[0].quotas.get_mut("X-USD").unwrap()[0].filled = 2;
    assert!(build_bundle(&c, &m, &r, &bad, &admission(&c, &m, 1)).is_err());
    let mut bad = o.clone();
    bad.sources[0].modes.clear();
    assert!(build_bundle(&c, &m, &r, &bad, &admission(&c, &m, 1)).is_err());
    let mut bad = o.clone();
    bad.statistics[0].panel.sources.pop();
    assert!(build_bundle(&c, &m, &r, &bad, &admission(&c, &m, 1)).is_err());
    let mut metadata = m.clone();
    metadata[0].config_hash = "0".repeat(64);
    assert!(build_bundle(&c, &metadata, &r, &o, &admission(&c, &m, 1)).is_err());
}

#[test]
fn complete_k200_reste_technique_et_incertitude_temporelle_est_separee() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(200, false, 1)).unwrap();
    let loaded = controls::verify::load_bundle(&out).unwrap();
    assert_eq!(loaded.document.status, PanelStatus::Complete);
    assert!(!loaded.document.smoke);
    assert!(loaded.document.technical_only);
    for mode in &loaded.document.statistics {
        assert!(mode.numeric_admissible);
        assert_eq!(mode.maxima.len(), 200);
        assert!(!mode.sources[0].temporal.as_ref().unwrap().available);
        assert_eq!(
            mode.sources[0].temporal.as_ref().unwrap().reason,
            Some(UnavailableReason::InsufficientMonths)
        );
    }
}

#[test]
fn un_panneau_complet_exige_les_p_values_mc_et_leurs_effectifs() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(200, false, 1)).unwrap();
    let original = json(&out.join("controls.json"));
    assert_eq!(original["statistics"][0]["family_exceedances"], 200);
    assert_eq!(original["statistics"][0]["sources"][0]["exceedances"], 200);
    let mut accepted = Vec::new();
    for (pointer, value) in [
        ("/statistics/0/family_p_value", serde_json::json!(0.)),
        ("/statistics/0/family_p_value", serde_json::json!(0.5)),
        ("/statistics/0/family_p_value", serde_json::json!(1. / 402.)),
        ("/statistics/0/family_p_value", serde_json::json!(null)),
        ("/statistics/0/family_exceedances", serde_json::json!(null)),
        ("/statistics/0/family_exceedances", serde_json::json!(201)),
        ("/statistics/0/critical_value", serde_json::json!(null)),
        ("/statistics/0/sources/0/p_value", serde_json::json!(0.)),
        ("/statistics/0/sources/0/p_value", serde_json::json!(0.5)),
        ("/statistics/0/sources/0/p_value", serde_json::json!(null)),
        (
            "/statistics/0/sources/0/exceedances",
            serde_json::json!(null),
        ),
        (
            "/statistics/0/sources/0/exceedances",
            serde_json::json!(201),
        ),
        ("/statistics/0/sources/0/ties", serde_json::json!(null)),
        ("/statistics/0/sources/0/ties", serde_json::json!(201)),
    ] {
        let mut document = original.clone();
        *document.pointer_mut(pointer).unwrap() = value.clone();
        let parsed: ControlsDocument = serde_json::from_value(document).unwrap();
        if parsed.validate().is_ok() {
            accepted.push(format!("{pointer} = {value}"));
        }
    }
    assert!(accepted.is_empty(), "mutations acceptées : {accepted:?}");
}

#[test]
fn k1_complet_reste_smoke_et_exige_un_seuil_critique_indisponible() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(1, false, 1)).unwrap();
    let loaded = controls::verify::load_bundle(&out).unwrap();
    for stat in &loaded.document.statistics {
        assert_eq!(stat.status, PanelStatus::Complete);
        assert!(stat.smoke);
        assert!(!stat.numeric_admissible);
        assert!(stat.critical_rank > 1);
        assert!(stat.critical_value.is_none());
    }
    let mut changed = loaded.document;
    changed.statistics[0].critical_value = Some(0.);
    assert!(changed.validate().is_err());
}

#[test]
fn projection_numerique_rehachee_doit_rester_identique_a_la_marginale_du_panneau() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(3, false, 1)).unwrap();
    let mut index = json(&out.join("controls-index.json"));
    let leaf = out.join(index["sources"][0]["dir"].as_str().unwrap());
    let mut projection = json(&leaf.join("controls.json"));
    projection["statistics"][0]["sources"][0]["mc_standard_error"] = serde_json::json!(0.123);
    let bytes = report::json_bytes(&projection).unwrap();
    let hash = tv4::model::hash::blake3_prefixed(&bytes);
    fs::write(leaf.join("controls.json"), bytes).unwrap();
    let mut manifest = json(&leaf.join("controls-manifest.json"));
    manifest["numeric"]["hash"] = hash.clone().into();
    let bytes = report::json_bytes(&manifest).unwrap();
    index["sources"][0]["controls"]["hash"] = hash.into();
    index["sources"][0]["manifest"]["hash"] = tv4::model::hash::blake3_prefixed(&bytes).into();
    fs::write(leaf.join("controls-manifest.json"), bytes).unwrap();
    fs::write(
        out.join("controls-index.json"),
        report::json_bytes(&index).unwrap(),
    )
    .unwrap();
    assert!(
        controls::verify::load_bundle(&out).is_err(),
        "une projection plausible isolément n'est pas une autre marginale autorisée"
    );
}

#[test]
fn une_matrice_complete_ne_peut_publier_des_maxima_inventes() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bundle(3, false, 1)).unwrap();
    let mut value = json(&out.join("controls.json"));
    value["statistics"][0]["maxima"][0] = serde_json::json!(123.);
    let parsed: ControlsDocument = serde_json::from_value(value).unwrap();
    assert!(parsed.validate().is_err());
}

#[test]
fn un_run_historique_protege_ne_devient_jamais_une_source_technique_sans_bloc() {
    let root = tempfile::tempdir().unwrap();
    let run = source_kind(root.path(), 1, true);
    assert_eq!(run.metrics()["research"]["block"], "VALIDATION");
    let (mut c, mut m, _, mut o) = experiment(2, true);
    c.line = Some(ControlLine::Full);
    m.truncate(1);
    m[0].config_hash = run.manifest()["config_hash"].as_str().unwrap().into();
    o.requested_sources = 1;
    let error = build_bundle(&c, &m, &[run], &o, &admission(&c, &m, 1))
        .err()
        .unwrap();
    assert!(error.to_string().contains("bloc"));
}

#[test]
fn mesures_separees_ne_changent_aucune_identite_ni_aucun_manifest() {
    let root = tempfile::tempdir().unwrap();
    let a = bundle(2, false, 1)
        .with_measurements(&serde_json::json!({"load_ms":1}))
        .unwrap();
    let b = bundle(2, false, 1)
        .with_measurements(&serde_json::json!({"load_ms":99}))
        .unwrap();
    assert_eq!(a.index_bytes(), b.index_bytes());
    for (a, b) in a.files().iter().zip(b.files()) {
        assert_eq!(a.relative_path, b.relative_path);
        if a.relative_path != Path::new("controls-measurements.json") {
            assert_eq!(a.bytes, b.bytes);
        } else {
            assert_ne!(a.bytes, b.bytes);
        }
    }
    assert!(b.with_measurements(&serde_json::json!({})).is_err());
    assert!(bundle(2, false, 1).with_measurements(&f64::NAN).is_err());
    let out = root.path().join("bundle");
    publish(&out, &a).unwrap();
    assert!(controls::verify::load_bundle(&out).is_ok());
}

#[test]
fn stats_cli_posttraite_seulement_les_copies_et_preserve_les_controles() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("bundle");
    let bytes = bundle(2, false, 1);
    publish(&out, &bytes).unwrap();
    let before = verify::load_stats_leaves(&out).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root.path())
            .status()
            .unwrap()
            .success()
    );
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_tv4"))
        .arg("stats")
        .arg(&out)
        .args(["--seed", "1", "--research-root"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for (path, bytes) in before.layout_files {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    for (dir, run) in before.leaves {
        assert!(dir.join("stats.json").is_file());
        assert_eq!(
            run.manifest_bytes(),
            fs::read(dir.join("manifest.json")).unwrap()
        );
        let stats = json(&dir.join("stats.json"));
        assert_eq!(stats["source"]["layout_files"].as_array().unwrap().len(), 7);
    }
}

#[test]
#[cfg(unix)]
fn les_controles_presents_en_liens_pendants_ne_sont_jamais_absents() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    source(&run, 1);
    for file in [
        "controls.json",
        "controls-manifest.json",
        "controls-index.json",
    ] {
        symlink(root.path().join("absent"), run.join(file)).unwrap();
        assert!(
            verify::load_stats_leaves(&run).is_err(),
            "{file} présent doit échouer, même si le lien est pendant"
        );
        fs::remove_file(run.join(file)).unwrap();
    }
    let out = root.path().join("bundle");
    publish(&out, &bundle(2, false, 1)).unwrap();
    let index = json(&out.join("controls-index.json"));
    let leaf = out.join(index["sources"][0]["dir"].as_str().unwrap());
    let external = root.path().join("copie-externe");
    fs::rename(leaf.join("trades.csv"), &external).unwrap();
    symlink(&external, leaf.join("trades.csv")).unwrap();
    assert!(verify::load_stats_leaves(&out).is_err());
    assert!(verify::load_stats_leaves(&leaf).is_err());
}

#[test]
fn panneau_s11_dev_baseline_natif_conserve_ligne_couts_et_admission() {
    use portfolio_common::*;
    use tv4::engine::controls::*;
    use tv4::engine::{
        CostSetup, NoopObserver, ProfileRun, ProfilesInputs, run_portfolio_profiles_research,
    };
    use tv4::model::{
        Block, Timeframe,
        costs::{CostProfileName, CostsFile},
        hash,
    };
    use tv4::report::{
        manifest::*,
        metrics_s9::*,
        profiles::render_profile_research,
        research::{Guard, ResearchInputs},
        stats_inputs::*,
    };
    let t: i64 = 1_640_995_200_000;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("source");
    let base = source(&root.path().join("base"), 1);
    let universe = names(&["X-USD"]);
    let instruments = vec![instrument(0.)];
    let data = controls_common::dataset(vec![(
        universe[0].clone(),
        flat_series(0, 30, 100., 1e6)
            .into_iter()
            .map(|mut c| {
                c.open_time_ms += t - portfolio_common::T0;
                c
            })
            .collect(),
    )]);
    let fixture = controls_common::Fixture {
        signal_ms: t + MINUTE,
        hold_ms: 2 * MINUTE,
        ..Default::default()
    };
    let costs = CostsFile::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/costs_c1.toml"),
    )
    .unwrap();
    let costsetup = CostSetup {
        file: &costs,
        measures: None,
        seed: 7,
    };
    let mut research = controls_common::Research::new(t, t + 30 * MINUTE);
    research.identity.block = Block::Dev;
    let mut spec = research.spec();
    spec.block = Block::Dev;
    let played = run_portfolio_profiles_research(
        ProfilesInputs {
            series: &data.series,
            strategies: [fixture.set(1), fixture.set(1), fixture.set(1)],
            config: config(&universe, &instruments, 300.),
            costs: costsetup,
        },
        &spec,
        &mut NoopObserver,
    )
    .unwrap();
    let render = |played: &ProfileRun| {
        render_profile_research(
            played,
            MetricsS9Inputs {
                dataset: DatasetInfo {
                    name: "controls-fixture".into(),
                    exchange: tv4::model::ExchangeId::Okx,
                    tf: Timeframe::M1,
                    gaps_policy: "skip".into(),
                    hash_format: "dsh-v1".into(),
                    dataset_hash: hash::blake3_prefixed(b"controls-fixture-dataset"),
                    series: vec![SeriesInfo {
                        symbol: universe[0].clone(),
                        rows: 30,
                        first_ms: t,
                        last_ms: t + 29 * MINUTE,
                    }],
                },
                instruments: InstrumentsInfo::toml(),
                okx_pairs: None,
                markets: vec![report::executability::LegMarket {
                    open_time_ms: data.series[0].1.open_time_ms(),
                    low: data.series[0].1.low(),
                    close: data.series[0].1.close(),
                    volume: data.series[0].1.volume(),
                }],
            },
            CostsInputs {
                file: &costs,
                file_label: "costs_c1.toml",
                measures: None,
                measures_label: None,
            },
            &ResearchInputs {
                guard: Guard::None,
                warmup_start_ms: t,
                universe: None,
                quality: &research.quality,
                subset: None,
            },
            Timeframe::M1,
        )
        .unwrap()
    };
    let rendered = render(&played.runs[0]);
    let mut engine = report::EngineInfo::current(hash::blake3_prefixed(b"fixture-engine"));
    engine.git_sha = "fixture-commit".into();
    let mut manifest = report::ManifestS9::new_s11_profile(
        &rendered.metrics,
        report::ManifestS10Inputs {
            base: ManifestS9Inputs {
                instruments_file: None,
                engine,
                equity_step: Timeframe::M1,
                files: rendered.files(),
                started_at_ms: t,
                timings: Timings {
                    load_ms: 0,
                    run_ms: 0,
                    cli_cold_ms: 0,
                },
            },
            costs_file: "costs_c1.toml".into(),
            cost_measures_file: None,
            profiles_id: "0".repeat(64),
        },
        BTreeMap::new(),
    )
    .unwrap();
    rendered.write_files(&dir).unwrap();
    report::write_json_atomic(&dir, "manifest.json", &manifest).unwrap();
    let bare = verify::load_verified_run(&dir).unwrap();
    let image = stats::snapshot(&bare).unwrap();
    let mut packet = base.stats_inputs().unwrap().inputs.clone();
    packet.provenance = image.provenance;
    packet.marks.last_mut().unwrap().equity = image.equity_final;
    packet.cash.last_mut().unwrap().cash = image.cash_final;
    let shift = t - portfolio_common::T0;
    for m in &mut packet.marks {
        m.at_ms += shift;
    }
    for c in &mut packet.cash {
        c.at_ms += shift;
    }
    for c in &mut packet.closures {
        c.at_ms += shift;
    }
    let packet_bytes = report::json_bytes(&packet).unwrap();
    manifest.stats_inputs = Some(StatsInputsRef {
        file: STATS_INPUTS_FILE.into(),
        schema: STATS_INPUTS_SCHEMA.into(),
        hash: hash::blake3_prefixed(&packet_bytes),
    });
    fs::write(dir.join(STATS_INPUTS_FILE), packet_bytes).unwrap();
    report::write_json_atomic(&dir, "manifest.json", &manifest).unwrap();
    let run = verify::load_verified_run(&dir).unwrap();
    let factory = || Ok(fixture.set(1));
    let mut options = ControlOptions::new(42);
    options.replications = 2;
    let outcome = run_batch(
        BatchInputs {
            dataset: data.clone(),
            sources: vec![SourceRuntime {
                config: config(&universe, &instruments, 300.),
                costs: Some((costsetup, CostProfileName::Baseline)),
                research: Some(&spec),
                quotas: controls::bundle::source_quotas(&run).unwrap(),
                factory: &factory,
            }],
            options: options.clone(),
        },
        &mut |_, observed| {
            let ObservedRun::Profile(observed) = observed else {
                panic!("profile")
            };
            let native = render(observed);
            assert_eq!(native.metrics_bytes, run.metrics_bytes());
            assert_eq!(native.trades_bytes, run.trades_bytes());
            assert_eq!(native.equity_bytes, run.equity_bytes());
            Ok(ObservedValidation::Matched)
        },
    )
    .unwrap();
    let context = ControlsContext {
        engine: EngineIdentity {
            commit: "fixture-commit".into(),
            binary_hash: hash::blake3_prefixed(b"fixture-engine"),
        },
        dataset_hash: run.manifest()["dataset_hash"].as_str().unwrap().into(),
        window: ControlsWindow {
            start_ms: t,
            end_ms: t + 30 * MINUTE,
        },
        initial_cash: 300.,
        sizing: tv4::research::stats::StatsSizing::Research,
        line: Some(ControlLine::Full),
        profile: Some(ControlProfile::Baseline),
        costs: Some(CostIdentity {
            config_hash: costs.file_hash().into(),
            seed: 7,
        }),
        options: (&options).into(),
        source_block: Some(Block::Dev),
        effective_block: Some(Block::Dev),
    };
    let metadata = vec![SourceMetadata {
        config_hash: run.manifest()["config_hash"].as_str().unwrap().into(),
        geometry_hash: hash::hash_json(&tv4::strategy::controls::ControlGeometry::descriptor(
            &fixture,
        ))
        .unwrap(),
        has_btc_filter: false,
    }];
    let runs = vec![run];
    let admitted = admission(&context, &metadata, 1);
    let bytes = build_bundle(&context, &metadata, &runs, &outcome, &admitted).unwrap();
    let out = root.path().join("bundle");
    publish(&out, &bytes).unwrap();
    let loaded = controls::verify::load_bundle(&out).unwrap();
    assert!(!loaded.document.technical_only);
    assert_eq!(loaded.document.sources[0].cost_seed, Some(7));
    assert_eq!(
        loaded.document.context.costs.as_ref().unwrap().config_hash,
        costs.file_hash()
    );
    assert!(
        verify::verify(&loaded.leaves[0].0, &loaded.leaves[0].0)
            .unwrap()
            .identical()
    );
    for change in 0..2 {
        let mut bad = context.clone();
        if change == 0 {
            bad.costs.as_mut().unwrap().seed += 1;
        } else {
            bad.costs.as_mut().unwrap().config_hash = "0".repeat(64);
        }
        assert!(
            build_bundle(
                &bad,
                &metadata,
                &runs,
                &outcome,
                &admission(&bad, &metadata, 1)
            )
            .is_err()
        );
    }
    let mut missing = admission(&context, &metadata, 1);
    missing.contexts.values_mut().next().unwrap().hypothesis = None;
    assert!(
        build_bundle(&context, &metadata, &runs, &outcome, &missing).is_err(),
        "DEV exige une fiche S12 pour chaque parent et null"
    );
}

#[test]
#[ignore = "export explicite de petites fixtures natives pour le lecteur PHP"]
fn export_php_controls_fixture() {
    let destination =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../control-plane/tests/fixtures/controls-rust");
    fs::create_dir_all(&destination).unwrap();
    for (name, k, inconclusive) in [
        ("smoke", 20, false),
        ("complete", 200, false),
        ("inconclusive", 20, true),
    ] {
        let path = destination.join(name);
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        publish(&path, &bundle(k, inconclusive, 1)).unwrap();
        controls::verify::load_bundle(&path).unwrap();
    }
}
