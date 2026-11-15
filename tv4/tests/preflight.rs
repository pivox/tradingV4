//! Les configurations d'admission sont construites sans bougies et égales à celles exécutées.
mod common;
mod portfolio_common;

use portfolio_common::{MINUTE, config, fees, instrument, names, rules};
use tv4::engine::{
    CellConfig, CostSetup, EngineError, Line, NoopObserver, PortfolioInputs, ProfilesInputs,
    ResearchIdentity, ResearchSpec, UniverseSpec, preflight, run_cell, run_portfolio,
    run_portfolio_profiles, run_portfolio_profiles_research, run_portfolio_research,
};
use tv4::model::costs::{CostProfileName, CostsFile};
use tv4::model::{Block, QualitySchedule, Slippage};
use tv4::store::Columns;
use tv4::store::time::days_from_civil;
use tv4::strategy::{Strategy, registry};

fn strategies(count: usize) -> Vec<Box<dyn Strategy>> {
    let params = toml::from_str("tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0").unwrap();
    (0..count)
        .map(|_| registry::build("sma_cross", &params).unwrap())
        .collect()
}
fn series(symbols: &[String], start: i64) -> Vec<(String, Columns)> {
    symbols
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                s.clone(),
                Columns::from_candles(&common::synthetic_candles(400, 42 + i as u64, start, 0.0)),
            )
        })
        .collect()
}

#[test]
fn cellule_sans_bougies_et_resultat_executé_portent_le_meme_hash() {
    let config = CellConfig {
        symbol: "BTC-USD",
        fees: fees(),
        slippage: Slippage::ZERO,
        initial_cash: 300.0,
        sim_rules: tv4::sim::SimRules {
            extra_entry_delay_bars: 1,
            ..rules()
        },
        instrument: instrument(0.0),
    };
    let strategy = strategies(1).remove(0);
    let expected = preflight::cell(strategy.as_ref(), &config, 23).unwrap();
    let bars = common::synthetic_candles(400, 42, portfolio_common::T0, 0.0);
    let actual = run_cell(&bars, strategy, &config)
        .unwrap()
        .with_min_trades(23)
        .unwrap();
    assert_eq!(expected, actual.config);
    assert_eq!(expected.hash().unwrap(), actual.config_hash);
    assert!(matches!(
        preflight::cell(strategies(1)[0].as_ref(), &config, 0),
        Err(EngineError::InvalidMinTrades)
    ));
}

#[test]
fn portefeuille_herit_e_et_profils_partagent_les_constructeurs_de_hash() {
    let symbols = names(&["BTC-USD", "ETH-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let mut config = config(&symbols, &instruments, 300.0);
    config.sim_rules.extra_entry_delay_bars = 1;
    let data = series(&symbols, portfolio_common::T0);
    let set = strategies(2);
    let expected = preflight::portfolio(&set, &config, None, None).unwrap();
    let actual = run_portfolio(PortfolioInputs {
        series: &data,
        strategies: set,
        config: config.clone(),
    })
    .unwrap();
    assert_eq!(expected, actual.config);
    let file = CostsFile::load(&common::costs_c1_path()).unwrap();
    let costs = CostSetup {
        file: &file,
        measures: None,
        seed: 42,
    };
    let sets = [strategies(2), strategies(2), strategies(2)];
    let expected: Vec<_> = CostProfileName::ALL
        .into_iter()
        .map(|p| preflight::portfolio(&sets[0], &config, Some((costs, p)), None).unwrap())
        .collect();
    let actual = run_portfolio_profiles(
        ProfilesInputs {
            series: &data,
            strategies: sets,
            config,
            costs,
        },
        &mut NoopObserver,
    )
    .unwrap();
    for (expected, actual) in expected.iter().zip(actual.runs) {
        assert_eq!(expected, &actual.result.config);
        assert_eq!(expected.hash().unwrap(), actual.result.config_hash);
    }
}

#[test]
fn recherche_et_ses_profils_sont_identifiables_avant_chargement_des_series() {
    let symbols = names(&["BTC-USD", "ETH-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let mut config = config(&symbols, &instruments, 300.0);
    config.sim_rules.extra_entry_delay_bars = 1;
    let start = days_from_civil(2026, 7, 1) * 86_400_000;
    let data = series(&symbols, start);
    let universe = UniverseSpec::Fixed {
        symbols: symbols.clone(),
    };
    let quality = QualitySchedule::default();
    let identity = ResearchIdentity {
        block: Block::Forward,
        decision_start_ms: start,
        decision_end_ms: start + 400 * MINUTE,
        universe_window_hash: None,
        quality_hash: "blake3:test".into(),
        line: Line::Full,
        subset_hash: None,
        legs_hash: String::new(),
    };
    let research = ResearchSpec {
        block: Block::Forward,
        decision_start_ms: start,
        decision_end_ms: identity.decision_end_ms,
        universe: None,
        universe_spec: &universe,
        quality: &quality,
        identity: &identity,
    };
    let set = strategies(2);
    let expected = preflight::portfolio(&set, &config, None, Some(&universe)).unwrap();
    let actual = run_portfolio_research(
        PortfolioInputs {
            series: &data,
            strategies: set,
            config: config.clone(),
        },
        &research,
    )
    .unwrap();
    assert_eq!(expected, actual.config);
    let file = CostsFile::load(&common::costs_c1_path()).unwrap();
    let costs = CostSetup {
        file: &file,
        measures: None,
        seed: 42,
    };
    let sets = [strategies(2), strategies(2), strategies(2)];
    let expected: Vec<_> = CostProfileName::ALL
        .into_iter()
        .map(|p| {
            preflight::portfolio(&sets[0], &config, Some((costs, p)), Some(&universe)).unwrap()
        })
        .collect();
    let actual = run_portfolio_profiles_research(
        ProfilesInputs {
            series: &data,
            strategies: sets,
            config,
            costs,
        },
        &research,
        &mut NoopObserver,
    )
    .unwrap();
    for (expected, actual) in expected.iter().zip(actual.runs) {
        assert_eq!(expected, &actual.result.config);
        assert_eq!(expected.hash().unwrap(), actual.result.config_hash);
    }
}

#[test]
fn une_configuration_incoherente_est_refusee_sans_series() {
    let symbols = names(&["BTC-USD"]);
    let instruments = [instrument(0.0)];
    let mut config = config(&symbols, &instruments, 300.0);
    config.sim_rules.extra_entry_delay_bars = 1;
    assert!(preflight::portfolio(&[], &config, None, None).is_err());
    let file = CostsFile::load(&common::costs_c1_path()).unwrap();
    let mut invalid = config.clone();
    invalid.slippage = Slippage::from_bps(1.0).unwrap();
    assert!(matches!(
        preflight::portfolio(
            &strategies(1),
            &invalid,
            Some((
                CostSetup {
                    file: &file,
                    measures: None,
                    seed: 42
                },
                CostProfileName::Baseline
            )),
            None
        ),
        Err(EngineError::ScalarSlippageWithCosts { .. })
    ));
}

#[test]
fn le_chargement_utilise_le_manifeste_admis_sans_relire_un_manifeste_remplace() {
    use tv4::model::{ExchangeId, Timeframe};
    use tv4::store::{GapsPolicy, MultiBuildRequest, Partition, Window};
    let root = tempfile::tempdir().unwrap();
    let start = portfolio_common::T0;
    let candles = common::synthetic_candles(400, 42, start, 0.0);
    let symbol = "BTC-USD".parse().unwrap();
    let partition = Partition::new(ExchangeId::Kraken, symbol, Timeframe::M1);
    let now = days_from_civil(2030, 1, 1) * 86_400_000;
    tv4::store::write_partition(
        root.path(),
        &partition,
        &Columns::from_candles(&candles),
        "synthetic",
        now,
    )
    .unwrap();
    let symbols = ["BTC-USD".parse().unwrap()];
    let built = tv4::store::build_dataset_multi(
        root.path(),
        &MultiBuildRequest {
            name: "admis",
            exchange: ExchangeId::Kraken,
            symbols: &symbols,
            tf: Timeframe::M1,
            window: Window::new(start, start + 400 * MINUTE).unwrap(),
            gaps_policy: GapsPolicy::Skip,
        },
        now,
    )
    .unwrap();
    let path = root.path().join(&built.manifest_rel);
    let before = tv4::store::load_dataset_multi(root.path(), &path).unwrap();
    std::fs::write(&path, b"manifeste remplace pendant le run").unwrap();
    let after = tv4::store::load_dataset_multi_manifest(root.path(), &path, built.manifest.clone())
        .unwrap();
    assert_eq!(before.manifest, after.manifest);
    assert_eq!(before.series, after.series);
    let mut wrong = built.manifest;
    wrong.dataset_hash = format!("blake3:{}", "0".repeat(64));
    assert!(
        tv4::store::load_dataset_multi_manifest(root.path(), &path, wrong).is_err(),
        "les bougies restent verifiees"
    );
}
