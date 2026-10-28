//! Aides communes aux tests des rapports s10 (docs/specs/s10.md §12, lot E) : trois profils joués sur un
//! marché synthétique, rendus en fichiers, puis écrits en dossier de profils avec leurs manifestes.
//! Marché inventé : aucune donnée réelle.
//!
//! Inclus par `mod profiles_common;` dans chaque binaire de test, à côté de `mod common;`,
//! `mod portfolio_common;` et `mod report_s9_common;` dont il dépend ; chacun n'en utilise qu'une partie.

#![allow(dead_code)]

use std::path::Path;

use tv4::engine::{
    CostSetup, NoopObserver, ProfilesInputs, ProfilesResult, run_portfolio_profiles,
};
use tv4::model::costs::CostsFile;
use tv4::model::{ExchangeId, InstrumentRules, Timeframe};
use tv4::report::executability::LegMarket;
use tv4::report::manifest::{ManifestS9Inputs, OutputFiles, Timings};
use tv4::report::metrics_s9::{
    CostsInputs, DatasetInfo, InstrumentsInfo, MetricsS9Inputs, SeriesInfo,
};
use tv4::report::profiles::{
    ProfileInput, ProfilesInput, ProfilesReport, RenderedProfile, render_profile,
};
use tv4::report::{EngineInfo, ManifestS9, ManifestS10Inputs, profiles_id, write_json_atomic};
use tv4::store::Columns;
use tv4::strategy::{Strategy, registry};

use crate::common;
use crate::portfolio_common::{T0, config, instrument, names};

/// Paramètres d'un `sma_cross` 1m qui trade souvent sur une marche synthétique (warmup de 20 bougies).
pub const FAST_PARAMS: &str = "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0";
/// Étiquette du fichier de coûts dans les rapports de test.
pub const COSTS_LABEL: &str = "costs.toml";

pub fn costs_file() -> CostsFile {
    CostsFile::load(&common::repo_path("config/costs.toml")).expect("config/costs.toml")
}

pub fn sma_cross(text: &str) -> Box<dyn Strategy> {
    let value = toml::from_str::<toml::Value>(text).expect("TOML valide");
    registry::build("sma_cross", &value).expect("stratégie valide")
}

/// Un marché de deux jambes synthétiques et leurs règles d'instrument.
pub struct Scenario {
    pub series: Vec<(String, Columns)>,
    pub universe: Vec<String>,
    pub rules: Vec<InstrumentRules>,
}

/// `rows` minutes pour ETH-USD (graine 7) et SOL-USD (graine 8).
pub fn scenario(rows: usize) -> Scenario {
    let series: Vec<(String, Columns)> = [("ETH-USD", 7), ("SOL-USD", 8)]
        .into_iter()
        .map(|(symbol, seed)| {
            (
                symbol.to_string(),
                Columns::from_candles(&common::synthetic_candles(rows, seed, T0, 0.0)),
            )
        })
        .collect();
    Scenario {
        universe: names(&["ETH-USD", "SOL-USD"]),
        rules: vec![instrument(0.0); series.len()],
        series,
    }
}

/// Joue les trois profils de `sma_cross` (`params`) sur le scénario, mode recherche, capital 300.
pub fn run(scenario: &Scenario, file: &CostsFile, seed: u64, params: &str) -> ProfilesResult {
    let legs = scenario.universe.len();
    let set = || (0..legs).map(|_| sma_cross(params)).collect::<Vec<_>>();
    run_portfolio_profiles(
        ProfilesInputs {
            series: &scenario.series,
            strategies: [set(), set(), set()],
            config: config(&scenario.universe, &scenario.rules, 300.0),
            costs: CostSetup {
                file,
                measures: None,
                seed,
            },
        },
        &mut NoopObserver,
    )
    .expect("profils valides")
}

pub fn dataset_info(series: &[(String, Columns)]) -> DatasetInfo {
    DatasetInfo {
        name: "synthetique".to_string(),
        exchange: ExchangeId::Kraken,
        tf: Timeframe::M1,
        gaps_policy: "skip".to_string(),
        hash_format: "dsh-v1".to_string(),
        dataset_hash: "blake3:aa".to_string(),
        series: series
            .iter()
            .map(|(symbol, columns)| SeriesInfo {
                symbol: symbol.clone(),
                rows: columns.len() as u64,
                first_ms: columns.open_time_ms().first().copied().unwrap_or(0),
                last_ms: columns.open_time_ms().last().copied().unwrap_or(0),
            })
            .collect(),
    }
}

/// Colonnes de chaque jambe de l'univers, dans l'ordre (calcul de G0).
pub fn markets<'a>(series: &'a [(String, Columns)], universe: &[String]) -> Vec<LegMarket<'a>> {
    universe
        .iter()
        .map(|symbol| {
            let (_, columns) = series
                .iter()
                .find(|(name, _)| name == symbol)
                .expect("série de la jambe");
            LegMarket {
                open_time_ms: columns.open_time_ms(),
                low: columns.low(),
                close: columns.close(),
                volume: columns.volume(),
            }
        })
        .collect()
}

/// Rend les trois profils joués (baseline, adverse, maker) en fichiers, avec un dataset d'essai.
pub fn render_all(
    result: &ProfilesResult,
    scenario: &Scenario,
    dataset: &DatasetInfo,
    file: &CostsFile,
) -> Vec<RenderedProfile> {
    result
        .runs
        .iter()
        .map(|run| {
            render_profile(
                run,
                MetricsS9Inputs {
                    dataset: dataset.clone(),
                    instruments: InstrumentsInfo::toml(),
                    okx_pairs: None,
                    markets: markets(&scenario.series, &scenario.universe),
                },
                CostsInputs {
                    file,
                    file_label: COSTS_LABEL,
                    measures: None,
                    measures_label: None,
                },
                Timeframe::H1,
            )
            .expect("rendu du profil")
        })
        .collect()
}

/// `profiles.json` des trois profils rendus.
pub fn profiles_report(rendered: &[RenderedProfile], file: &CostsFile) -> ProfilesReport {
    let hashes: Vec<String> = rendered.iter().map(RenderedProfile::result_hash).collect();
    let input = |i: usize| ProfileInput {
        metrics: &rendered[i].metrics,
        result_hash: &hashes[i],
    };
    ProfilesReport::build(&ProfilesInput {
        baseline: input(0),
        adverse: input(1),
        maker: input(2),
        min_maker_entry_share: file.profiles.maker.min_maker_entry_share,
    })
    .expect("profiles.json")
}

/// Moteur factice des manifestes de test.
pub fn engine_info() -> EngineInfo {
    EngineInfo {
        tv4_version: "0.0.0".to_string(),
        git_sha: "test".to_string(),
        rustc: "test".to_string(),
        rustc_commit: "test".to_string(),
        target: "test".to_string(),
        target_cpu: "test".to_string(),
        profile: "test".to_string(),
        binary_hash: "blake3:00".to_string(),
    }
}

/// Manifeste d'un profil rendu, avec le `profiles_id` du dossier.
pub fn manifest_of(rendered: &RenderedProfile, id: &str) -> ManifestS9 {
    ManifestS9::new_s10(
        &rendered.metrics,
        ManifestS10Inputs {
            base: ManifestS9Inputs {
                instruments_file: None,
                engine: engine_info(),
                equity_step: Timeframe::H1,
                files: OutputFiles {
                    equity_csv: tv4::model::hash::blake3_prefixed(&rendered.equity_bytes),
                    metrics_json: tv4::model::hash::blake3_prefixed(&rendered.metrics_bytes),
                    trades_csv: tv4::model::hash::blake3_prefixed(&rendered.trades_bytes),
                },
                started_at_ms: T0,
                timings: Timings {
                    load_ms: 1,
                    run_ms: 2,
                    cli_cold_ms: 3,
                },
            },
            costs_file: COSTS_LABEL.to_string(),
            cost_measures_file: None,
            profiles_id: id.to_string(),
        },
    )
    .expect("manifeste s10")
}

/// Écrit un dossier de profils complet : trois sous-dossiers avec manifeste, puis `profiles.json`.
pub fn write_profiles_dir(dir: &Path, rendered: &[RenderedProfile], file: &CostsFile) {
    let report = profiles_report(rendered, file);
    let ids: Vec<String> = rendered.iter().map(|r| r.metrics.run_id()).collect();
    let id = profiles_id(&ids[0], &ids[1], &ids[2]);
    assert_eq!(id, report.profiles_id);
    for (profile, name) in rendered.iter().zip(["baseline", "adverse", "maker"]) {
        let sub = dir.join(name);
        profile.write_files(&sub).expect("fichiers du profil");
        write_json_atomic(&sub, "manifest.json", &manifest_of(profile, &id)).expect("manifeste");
    }
    report.write(dir).expect("profiles.json");
}
