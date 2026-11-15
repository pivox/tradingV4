//! S13 : image contrôlée des artefacts, sans accès aux bougies.
mod common;
mod portfolio_common;
mod profiles_common;
mod report_s9_common;
mod research_common;
use std::{fs, path::Path};
use tv4::report::stats::{read_run, snapshot};

fn backtest(dir: &Path) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let args = vec![
        "backtest".into(),
        "--fixture".into(),
        repo.join("fixtures/kraken_xbtusd_1m.csv")
            .display()
            .to_string(),
        "--strategy".into(),
        "sma_cross".into(),
        "--params".into(),
        repo.join("config/strategies/sma_cross.toml")
            .display()
            .to_string(),
        "--fees-file".into(),
        repo.join("config/fees.toml").display().to_string(),
        "--instruments-file".into(),
        repo.join("config/instruments.toml").display().to_string(),
        "--out".into(),
        dir.display().to_string(),
    ];
    let output = report_s9_common::tv4(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Isole explicitement le contrat historique sans paquet avant les mutations de sources.
fn legacy_backtest(dir: &Path) {
    backtest(dir);
    let path = dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("stats_inputs");
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

/// Les helpers de rapports utilisent des hashes abrégés ; ce lecteur exige le format public complet.
/// L'identité synthétique est remplacée avant génération des manifestes, sans toucher les helpers.
fn complete_dataset_hashes(rendered: &mut [tv4::report::profiles::RenderedProfile]) {
    let hash = tv4::model::hash::blake3_prefixed(b"stats-reader-dataset-synthetique");
    for profile in rendered {
        profile.metrics.dataset_hash = hash.clone();
        profile.metrics.data.dataset_hash = hash.clone();
        profile.metrics_bytes = tv4::report::json_bytes(&profile.metrics).unwrap();
    }
}

#[test]
fn ancien_run_et_image_possedee_resistent_a_une_mutation_du_disque() {
    let dir = tempfile::tempdir().unwrap();
    legacy_backtest(dir.path());
    let run = read_run(dir.path()).unwrap();
    let before = snapshot(&run).unwrap();
    assert!(!before.trades.is_empty());
    assert!(
        before.trades.iter().all(|t| t.included),
        "S8 conserve les clôtures forcées"
    );
    assert!(run.stats_inputs().is_none());
    assert_eq!(
        before.provenance.window.end_ms,
        run.manifest()["window"]["end_ms"].as_i64().unwrap()
    );
    let retained = run.manifest_bytes().to_vec();
    let retained_packet = packet(&run);
    fs::write(dir.path().join("trades.csv"), b"malforme\n").unwrap();
    fs::write(dir.path().join("manifest.json"), b"{}\n").unwrap();
    assert_eq!(snapshot(&run).unwrap(), before);
    retained_packet.validate_against(&before).unwrap();
    assert_eq!(run.manifest_bytes(), retained);
    assert!(read_run(dir.path()).is_err());
}

#[test]
fn journal_strict_meme_pour_colonnes_non_utilisees_et_r_net() {
    let dir = tempfile::tempdir().unwrap();
    backtest(dir.path());
    let run = read_run(dir.path()).unwrap();
    let original = run.trades_bytes();
    let parse = |bytes: &[u8]| {
        tv4::report::stats::parse_trades(bytes, false, snapshot(&run).unwrap().provenance.window)
    };
    let parsed = parse(original).unwrap();
    assert_eq!(parsed[0].row, 0);
    let extra_header = String::from_utf8(original.to_vec()).unwrap().replacen(
        "spread_cost\n",
        "spread_cost,extra\n",
        1,
    );
    assert!(parse(extra_header.as_bytes()).is_err());
    let first_line = String::from_utf8(original.to_vec())
        .unwrap()
        .lines()
        .take(2)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(parse(format!("{}\n{},extra\n", first_line[0], first_line[1]).as_bytes()).is_err());
    for (column, replacement) in [
        (0, "18446744073709551616"),
        (1, "NaN"),
        (3, "unknown"),
        (18, "NaN"),
        (20, "inf"),
        (21, "1"),
        (28, "unknown"),
        (30, "NaN"),
        (33, "99999"),
        (34, "invented"),
        (35, "-1"),
    ] {
        let mut reader = csv::Reader::from_reader(original);
        let mut rows: Vec<Vec<String>> = reader
            .records()
            .map(|r| r.unwrap().iter().map(str::to_owned).collect())
            .collect();
        rows[0][column] = replacement.into();
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer
            .write_record(tv4::report::writers::TRADES_COLUMNS)
            .unwrap();
        for row in rows {
            writer.write_record(row).unwrap();
        }
        assert!(
            parse(&writer.into_inner().unwrap()).is_err(),
            "colonne {column}"
        );
    }
    assert!(parse(b"lot_id\n1\n").is_err());
    let mut reader = csv::Reader::from_reader(original);
    let mut rows: Vec<Vec<String>> = reader
        .records()
        .map(|r| r.unwrap().iter().map(str::to_owned).collect())
        .collect();
    rows[0][33].clear();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(tv4::report::writers::TRADES_COLUMNS)
        .unwrap();
    for row in rows {
        writer.write_record(row).unwrap();
    }
    assert_eq!(
        parse(&writer.into_inner().unwrap()).unwrap()[0].r_multiple,
        None
    );
    let mut reader = csv::Reader::from_reader(original);
    let mut rows: Vec<Vec<String>> = reader
        .records()
        .map(|r| r.unwrap().iter().map(str::to_owned).collect())
        .collect();
    rows[0][10] = "forced_close".into();
    rows[0][22] = "true".into();
    rows[0][28] = "end_of_window".into();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(tv4::report::writers::TRADES_COLUMNS)
        .unwrap();
    for row in rows {
        writer.write_record(row).unwrap();
    }
    let forced = writer.into_inner().unwrap();
    let kept = parse(&forced).unwrap();
    let purged =
        tv4::report::stats::parse_trades(&forced, true, snapshot(&run).unwrap().provenance.window)
            .unwrap();
    assert!(
        purged
            .iter()
            .zip(&kept)
            .all(|(a, b)| a.pnl_net == b.pnl_net && a.r_multiple == b.r_multiple)
    );
    assert!(kept[0].included);
    assert!(!purged[0].included);
}

fn packet(run: &tv4::report::stats::VerifiedRun) -> tv4::report::stats_inputs::StatsInputs {
    use tv4::{report::stats_inputs::*, research::stats::*};
    let image = snapshot(run).unwrap();
    let w = image.provenance.window;
    let end = run.metrics()["equity_final"].as_f64().unwrap();
    StatsInputs {
        schema: STATS_INPUTS_SCHEMA.into(),
        conventions: CONVENTIONS.into(),
        provenance: image.provenance,
        pair_symbols: image.pair_symbols,
        sizing: image.sizing,
        decision_timeframe_minutes: (run.metrics()["config"]["strategy"]["params"]["tf"]
            .as_str()
            .unwrap()
            .parse::<tv4::model::Timeframe>()
            .unwrap()
            .ms()
            / 60_000) as u32,
        closures: image
            .trades
            .iter()
            .map(|t| TradeClosure {
                row: t.row,
                at_ms: w.end_ms,
            })
            .collect(),
        marks: vec![
            MarkPoint {
                at_ms: w.start_ms,
                equity: 300.,
                day_close: false,
            },
            MarkPoint {
                at_ms: w.end_ms,
                equity: end,
                day_close: false,
            },
        ],
        cash: vec![
            CashPoint {
                at_ms: w.start_ms,
                cash: 300.,
            },
            CashPoint {
                at_ms: w.end_ms,
                cash: run.metrics()["cash_final"].as_f64().unwrap(),
            },
        ],
        markets: vec![],
    }
}
fn install(dir: &Path, packet: &tv4::report::stats_inputs::StatsInputs) {
    use tv4::report::stats_inputs::*;
    let bytes = tv4::report::json_bytes(packet).unwrap();
    fs::write(dir.join(STATS_INPUTS_FILE), &bytes).unwrap();
    let path = dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["stats_inputs"] = serde_json::to_value(StatsInputsRef {
        file: STATS_INPUTS_FILE.into(),
        schema: STATS_INPUTS_SCHEMA.into(),
        hash: tv4::model::hash::blake3_prefixed(&bytes),
    })
    .unwrap();
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}
#[test]
fn manifeste_reference_le_paquet_une_fois_et_verify_le_controle() {
    use tv4::report::{stats_inputs::*, verify};
    let dir = tempfile::tempdir().unwrap();
    backtest(dir.path());
    let original = read_run(dir.path()).unwrap();
    let packet = packet(&original);
    packet.validate_for(&original).unwrap();
    install(dir.path(), &packet);
    let run = read_run(dir.path()).unwrap();
    assert_eq!(run.stats_inputs().unwrap().inputs, packet);
    let bytes = run.stats_inputs().unwrap().bytes.clone();
    fs::write(dir.path().join(STATS_INPUTS_FILE), b"{}").unwrap();
    assert_eq!(run.stats_inputs().unwrap().bytes, bytes);
    assert!(verify::verify(dir.path(), dir.path()).is_err());
    install(dir.path(), &packet);
    fs::remove_file(dir.path().join(STATS_INPUTS_FILE)).unwrap();
    assert!(read_run(dir.path()).is_err());
}
#[test]
fn paquet_externe_identites_cardinalites_et_marques_sont_recoupes() {
    use tv4::research::stats::*;
    let dir = tempfile::tempdir().unwrap();
    legacy_backtest(dir.path());
    let run = read_run(dir.path()).unwrap();
    let source = packet(&run);
    for case in 0..15 {
        let mut p = source.clone();
        match case {
            0 => p.provenance.initial_cash = 301.,
            1 => p.provenance.window.end_ms += 60_000,
            2 => p.provenance.profile = Some("maker".into()),
            3 => p.provenance.line = Some("full".into()),
            4 => p.provenance.files.equity_csv = format!("blake3:{}", "f".repeat(64)),
            5 => p.schema = "tv4-stats-inputs-v2".into(),
            6 => {
                p.closures.pop();
            }
            7 => p.closures[0].row = usize::MAX,
            8 => p.marks[0].equity = 301.,
            9 => p.marks[1].equity += 1.,
            10 => p.cash[1].cash += 1.,
            11 => p.marks[1].at_ms = p.marks[0].at_ms,
            12 => p.markets.push(MarketSeries {
                symbol: "BTC-USD".into(),
                daily_closes: vec![PricePoint {
                    at_ms: p.provenance.window.start_ms + 1,
                    close: 100.,
                }],
                valuations: vec![],
            }),
            13 => p.decision_timeframe_minutes = 0,
            _ => p.decision_timeframe_minutes = 7,
        }
        assert!(p.validate_for(&run).is_err(), "cas {case}");
    }
    // Un paquet orphelin n'est pas utilisé implicitement ; les données datées restent indisponibles.
    fs::write(dir.path().join("stats-inputs.json"), b"obsolete").unwrap();
    assert!(read_run(dir.path()).unwrap().stats_inputs().is_none());
    let mut changed = serde_json::to_value(&source).unwrap();
    changed.as_object_mut().unwrap().remove("cash");
    assert!(serde_json::from_value::<tv4::report::stats_inputs::StatsInputs>(changed).is_err());
}

#[test]
fn s11_purge_univers_joue_et_cloture_forcee_a_la_fin_globale() {
    use tv4::engine::Line;
    let fx = research_common::fixture();
    let mut rendered = fx.render(Line::Full, 300.);
    complete_dataset_hashes(std::slice::from_mut(&mut rendered));
    let dir = tempfile::tempdir().unwrap();
    fx.write_run(dir.path(), &rendered, Line::Full);
    let run = read_run(dir.path()).unwrap();
    let image = snapshot(&run).unwrap();
    assert!(image.trades.iter().any(|t| !t.included));
    assert!(image.trades.iter().any(|t| t.included));
    assert!(
        !image.pair_symbols.iter().any(|s| s == "BTC-USD"),
        "BTC est le contexte, pas une jambe jouée"
    );
    assert_eq!(
        image.trades.iter().filter(|t| t.included).count() as u64,
        run.metrics()["research"]["purge"]["trades_kept"]
            .as_u64()
            .unwrap()
    );
    let mut p = packet(&run);
    assert!(image.market_symbols.iter().any(|s| s == "BTC-USD"));
    p.markets.push(tv4::research::stats::MarketSeries {
        symbol: "BTC-USD".into(),
        daily_closes: vec![],
        valuations: vec![],
    });
    p.validate_for(&run).unwrap();
    // Simule un timestamp économique de jambe précédant la liquidation terminale globale.
    let row = image.trades.iter().find(|t| !t.included).unwrap().row;
    p.closures.iter_mut().find(|c| c.row == row).unwrap().at_ms = p.provenance.window.end_ms - 1;
    p.marks.insert(
        1,
        tv4::research::stats::MarkPoint {
            at_ms: p.provenance.window.end_ms - 1,
            equity: 300.,
            day_close: false,
        },
    );
    assert!(
        p.validate_for(&run).is_err(),
        "clôture forcée obligatoirement à la fin globale"
    );
}

#[test]
fn liquidation_terminal_global_meme_si_timestamp_economique_anterieur() {
    let dir = tempfile::tempdir().unwrap();
    legacy_backtest(dir.path());
    let original = fs::read(dir.path().join("trades.csv")).unwrap();
    let mut reader = csv::Reader::from_reader(original.as_slice());
    let mut rows: Vec<Vec<String>> = reader
        .records()
        .map(|r| r.unwrap().iter().map(str::to_owned).collect())
        .collect();
    rows[0][10] = "forced_close".into();
    rows[0][22] = "true".into();
    rows[0][28] = "end_of_window".into();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(tv4::report::writers::TRADES_COLUMNS)
        .unwrap();
    for row in rows {
        writer.write_record(row).unwrap();
    }
    let bytes = writer.into_inner().unwrap();
    fs::write(dir.path().join("trades.csv"), &bytes).unwrap();
    let path = dir.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let hash = tv4::model::hash::blake3_prefixed(&bytes);
    manifest["outputs"]["files"]["trades.csv"] = hash.clone().into();
    manifest["result_hash"] = hash.into();
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let run = read_run(dir.path()).unwrap();
    let image = snapshot(&run).unwrap();
    let mut p = packet(&run);
    assert!(
        image.trades[0].included,
        "S8 conserve end_of_window dans les statistiques de trades"
    );
    let at = image.trades[0].exit_ms;
    assert!(at < p.provenance.window.end_ms);
    p.closures[0].at_ms = at;
    p.marks.insert(
        1,
        tv4::research::stats::MarkPoint {
            at_ms: at,
            equity: 300.,
            day_close: false,
        },
    );
    assert!(
        p.validate_for(&run).is_err(),
        "la liquidation finale est réalisée à la fin globale, pas au timestamp de jambe"
    );
}

#[test]
fn profils_s10_frais_et_selection_adverse_restent_deja_nets() {
    let scenario = profiles_common::scenario(6000);
    let mut costs = profiles_common::costs_file();
    costs.profiles.maker.fill_probability = 1.;
    let result = profiles_common::run(
        &scenario,
        &costs,
        7,
        &profiles_common::FAST_PARAMS.replace("1m", "5m"),
    );
    let mut rendered = profiles_common::render_all(
        &result,
        &scenario,
        &profiles_common::dataset_info(&scenario.series),
        &costs,
    );
    assert!(
        rendered[2]
            .metrics
            .cost_counters
            .as_ref()
            .unwrap()
            .total
            .adverse_selection_total
            > 0.,
        "le scénario doit débiter la sélection adverse maker"
    );
    let dir = tempfile::tempdir().unwrap();
    complete_dataset_hashes(&mut rendered);
    profiles_common::write_profiles_dir(dir.path(), &rendered, &costs);
    for name in ["baseline", "adverse", "maker"] {
        let run = read_run(&dir.path().join(name)).unwrap();
        let image = snapshot(&run).unwrap();
        assert_eq!(image.provenance.profile.as_deref(), Some(name));
        assert!(
            image.trades.iter().all(|t| t.included),
            "S10 ne purge pas end_of_window"
        );
        packet(&run).validate_for(&run).unwrap();
    }
}

#[test]
fn paquet_externe_tolerance_ulp_et_manifest_historique_distinct() {
    let dir = tempfile::tempdir().unwrap();
    backtest(dir.path());
    let run = read_run(dir.path()).unwrap();
    let mut p = packet(&run);
    p.provenance.initial_cash = f64::from_bits(p.provenance.initial_cash.to_bits() + 1);
    p.marks[0].equity = p.provenance.initial_cash;
    p.validate_for(&run)
        .expect("l'aller-retour JSON peut déplacer le capital d'un ULP");
    let loaded = tv4::report::stats_inputs::LoadedStatsInputs::from_bytes(
        tv4::report::json_bytes(&p).unwrap(),
        &run,
    )
    .unwrap();
    assert_eq!(
        loaded.inputs.marks[0].equity,
        loaded.inputs.provenance.initial_cash
    );
    p = loaded.inputs;
    let path = dir.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["engine"]["git_sha"] = "commit-historique-distinct".into();
    manifest["timings"]["run_ms"] = 999.into();
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    p.validate_for(&read_run(dir.path()).unwrap()).unwrap();
}

#[test]
fn journal_accepte_plusieurs_ventes_partielles_du_meme_lot() {
    let dir = tempfile::tempdir().unwrap();
    backtest(dir.path());
    let run = read_run(dir.path()).unwrap();
    let mut reader = csv::Reader::from_reader(run.trades_bytes());
    let first = reader.records().next().unwrap().unwrap();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(tv4::report::writers::TRADES_COLUMNS)
        .unwrap();
    writer.write_record(&first).unwrap();
    writer.write_record(&first).unwrap();
    let trades = tv4::report::stats::parse_trades(
        &writer.into_inner().unwrap(),
        false,
        snapshot(&run).unwrap().provenance.window,
    )
    .unwrap();
    assert_eq!(trades.len(), 2);
    assert_eq!(trades[0].lot_id, trades[1].lot_id);
    assert_eq!(trades[1].row, 1);
}

#[test]
fn verify_refuse_reference_et_paquet_rehache_incoherents() {
    let dir = tempfile::tempdir().unwrap();
    backtest(dir.path());
    let run = read_run(dir.path()).unwrap();
    let source = packet(&run);
    for case in 0..6 {
        let mut p = source.clone();
        match case {
            0 => p.provenance.profile = Some("maker".into()),
            1 => p.provenance.line = Some("full".into()),
            2 => p.provenance.initial_cash += 1.,
            3 => p.provenance.window.end_ms += 60000,
            4 => p.provenance.files.metrics_json = format!("blake3:{}", "e".repeat(64)),
            _ => p.schema = "inconnu".into(),
        }
        install(dir.path(), &p);
        assert!(
            tv4::report::verify::verify(dir.path(), dir.path()).is_err(),
            "cas {case}"
        );
    }
    for (field, value) in [
        ("file", "../stats-inputs.json"),
        ("schema", "inconnu"),
        ("hash", "blake3:xyz"),
    ] {
        install(dir.path(), &source);
        let path = dir.path().join("manifest.json");
        let mut m: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        m["stats_inputs"][field] = value.into();
        fs::write(path, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(read_run(dir.path()).is_err(), "référence {field}");
    }
    install(dir.path(), &source);
    for file in ["metrics.json", "trades.csv", "equity.csv"] {
        let path = dir.path().join(file);
        let retained = fs::read(&path).unwrap();
        fs::write(&path, b"altere").unwrap();
        assert!(read_run(dir.path()).is_err(), "empreinte de {file}");
        fs::write(path, retained).unwrap();
    }
}

#[test]
fn un_parametre_tf_arbitraire_ne_devient_pas_le_timeframe_de_decision() {
    let dir = tempfile::tempdir().unwrap();
    legacy_backtest(dir.path());
    let path = dir.path().join("metrics.json");
    let mut metrics: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    metrics["config"]["strategy"]["id"] = "strategy_with_filter_tf".into();
    let bytes = serde_json::to_vec(&metrics).unwrap();
    fs::write(path, &bytes).unwrap();
    let path = dir.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["config"]["strategy"]["id"] = "strategy_with_filter_tf".into();
    manifest["outputs"]["files"]["metrics.json"] = tv4::model::hash::blake3_prefixed(&bytes).into();
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let run = read_run(dir.path()).unwrap();
    assert_eq!(snapshot(&run).unwrap().decision_timeframe_minutes, None);
    let mut p = packet(&run);
    p.decision_timeframe_minutes = 5;
    p.validate_for(&run).unwrap();
}

#[test]
fn cours_btc_non_admis_refuses_pour_un_run_eth_seul() {
    use tv4::research::stats::{MarketSeries, PricePoint};
    let mut scenario = profiles_common::scenario(4000);
    scenario.series.truncate(1);
    scenario.universe.truncate(1);
    scenario.rules.truncate(1);
    let costs = profiles_common::costs_file();
    let result = profiles_common::run(&scenario, &costs, 7, profiles_common::FAST_PARAMS);
    let mut rendered = profiles_common::render_all(
        &result,
        &scenario,
        &profiles_common::dataset_info(&scenario.series),
        &costs,
    );
    let dir = tempfile::tempdir().unwrap();
    complete_dataset_hashes(&mut rendered);
    profiles_common::write_profiles_dir(dir.path(), &rendered, &costs);
    let run = read_run(&dir.path().join("baseline")).unwrap();
    let mut p = packet(&run);
    assert_eq!(p.pair_symbols, vec!["ETH-USD"]);
    p.markets.push(MarketSeries {
        symbol: "BTC-USD".into(),
        daily_closes: vec![],
        valuations: vec![
            PricePoint {
                at_ms: p.provenance.window.start_ms,
                close: 100.,
            },
            PricePoint {
                at_ms: p.provenance.window.end_ms,
                close: 110.,
            },
        ],
    });
    assert!(
        p.validate_for(&run).is_err(),
        "BTC hors dataset ne peut activer un benchmark"
    );
}

#[test]
fn identites_malformees_refusees_meme_si_les_sources_correspondent() {
    let dir = tempfile::tempdir().unwrap();
    backtest(dir.path());
    let metrics_path = dir.path().join("metrics.json");
    let manifest_path = dir.path().join("manifest.json");
    let original_metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(&metrics_path).unwrap()).unwrap();
    let original_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    for key in ["run_id", "config_hash", "dataset_hash"] {
        let mut metrics = original_metrics.clone();
        let mut manifest = original_manifest.clone();
        metrics[key] = "identite-arbitraire".into();
        manifest[key] = "identite-arbitraire".into();
        let bytes = serde_json::to_vec(&metrics).unwrap();
        fs::write(&metrics_path, &bytes).unwrap();
        manifest["outputs"]["files"]["metrics.json"] =
            tv4::model::hash::blake3_prefixed(&bytes).into();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(read_run(dir.path()).is_err(), "format {key}");
    }
}
