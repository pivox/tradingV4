//! Aides communes aux tests des rapports de recherche s11 (docs/specs/s11.md §9) : un marché FORWARD
//! synthétique à trois jambes, un univers trimestriel de deux trimestres, une exclusion qualité en 4h et
//! une stratégie de test qui lit le régime BTC/SMA200. Les deux lignes (`full`, `okx_usdc`) sont jouées
//! sur le même marché. **Tout est inventé** : aucune donnée de marché réelle.
//!
//! Inclus par `mod research_common;` dans chaque binaire de test, à côté de `mod common;`,
//! `mod portfolio_common;` et `mod profiles_common;` ; chacun n'en utilise qu'une partie.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use tv4::engine::{
    CostSetup, Line, NoopObserver, PortfolioInputs, PortfolioResult, ProfilesInputs,
    ProfilesResult, ResearchIdentity, ResearchSpec, UniverseSpec, run_portfolio_profiles_research,
    run_portfolio_research,
};
use tv4::model::costs::CostsFile;
use tv4::model::universe::{Member, SubsetRule, SubsetSource, UniverseQuarter};
use tv4::model::{
    Block, ExchangeId, InstrumentRules, PairSubset, QualitySchedule, QuarterExclusion, Timeframe,
    Ts, UniverseSchedule,
};
use tv4::report::manifest::{ManifestS9Inputs, OutputFiles, Timings};
use tv4::report::metrics_s9::{
    CostsInputs, DatasetInfo, InstrumentsInfo, MetricsS9Inputs, SeriesInfo,
};
use tv4::report::profiles::{RenderedProfile, render_profile_research};
use tv4::report::research::{
    Guard, LineInput, LineRun, LinesReport, ResearchInputs, SINGLE_PROFILE, SubsetInfo,
    render_research_run,
};
use tv4::report::{ManifestS9, ManifestS10Inputs, profiles_id, write_json_atomic};
use tv4::store::Columns;
use tv4::store::time::days_from_civil;
use tv4::strategy::{BarContext, IndicatorSpec, OrderKind, Signal, Size, Strategy};

use crate::common;
use crate::portfolio_common::{DAY, MINUTE, config, instrument, names};
use crate::profiles_common::{COSTS_LABEL, engine_info, markets, profiles_report};

/// Nombre de jours de chauffe du régime avant le début de la fenêtre de décision (SMA200 + marge).
pub const WARMUP_DAYS: i64 = 210;
/// Hash de dataset des tests (le dataset est en mémoire, il n'a pas de vrai manifeste).
pub const DATASET_HASH: &str = "blake3:5e11";
/// Hash des règles de sélection de l'univers des tests.
pub const RULES_HASH: &str = "blake3:rules11";
/// Hash du fichier d'instruments dont dérive le sous-ensemble de test.
pub const SUBSET_SOURCE_HASH: &str = "blake3:ca11";

/// Instant UTC en millisecondes.
pub fn at(year: i64, month: u32, day: u32, hour: i64) -> Ts {
    days_from_civil(year, month, day) * DAY + hour * 3_600_000
}

/// Stratégie de test : décide en 4h, lit le régime BTC/SMA200 (`uses_regime()` vrai), entre au marché
/// avec un stop à 5 % sous la dernière clôture 4h quand le régime est haussier et qu'elle est à plat, et
/// sort quand il ne l'est plus.
pub struct RegimeH4 {
    tfs: [Timeframe; 1],
}

impl RegimeH4 {
    pub fn boxed() -> Box<dyn Strategy> {
        Box::new(RegimeH4 {
            tfs: [Timeframe::H4],
        })
    }
}

impl Strategy for RegimeH4 {
    fn id(&self) -> &'static str {
        "regime_h4_test"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({"tf": "4h", "stop_pct": 0.05, "risk_pct": 0.01})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &self.tfs
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn uses_regime(&self) -> bool {
        true
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        if !ctx.just_closed(Timeframe::H4) {
            return;
        }
        let bull = ctx.regime().and_then(|view| view.bull());
        if ctx.position().is_none() && ctx.pending().is_empty() && bull == Some(true) {
            let Some(last) = ctx.last(Timeframe::H4) else {
                return;
            };
            out.push(Signal::EnterLong {
                size: Size::RiskPct(0.01),
                kind: OrderKind::Market,
                stop: Some(last.close * 0.95),
                take_profit: None,
                tag: "bull",
            });
        } else if ctx.position().is_some() && bull != Some(true) {
            out.push(Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "bear",
            });
        }
    }
}

/// Une ligne qui est un run seul.
pub fn line_input(line: Line, rendered: &RenderedProfile) -> LineInput<'_> {
    LineInput {
        line,
        runs: vec![LineRun {
            profile: SINGLE_PROFILE,
            metrics: &rendered.metrics,
        }],
    }
}

/// Une ligne qui est un dossier de trois profils.
pub fn profiles_line_input(line: Line, rendered: &[RenderedProfile]) -> LineInput<'_> {
    LineInput {
        line,
        runs: rendered
            .iter()
            .zip(["baseline", "adverse", "maker"])
            .map(|(r, profile)| LineRun {
                profile,
                metrics: &r.metrics,
            })
            .collect(),
    }
}

/// Le marché, l'univers, les exclusions et le sous-ensemble des deux lignes.
pub struct Fixture {
    /// BTC-USD (régime, hors univers) puis AAA-USD, BBB-USD, CCC-USD.
    pub series: Vec<(String, Columns)>,
    pub schedule: UniverseSchedule,
    pub quality: QualitySchedule,
    /// Sous-ensemble de la ligne `okx_usdc` : AAA et CCC (BBB n'est pas tradable).
    pub subset: PairSubset,
    pub start_ms: Ts,
    pub end_ms: Ts,
    pub warmup_start_ms: Ts,
    pub universe_spec: UniverseSpec,
}

const FULL: [&str; 3] = ["AAA-USD", "BBB-USD", "CCC-USD"];
const OKX: [&str; 2] = ["AAA-USD", "CCC-USD"];

fn member(symbol: &str, rank: u8) -> Member {
    Member {
        symbol: symbol.to_string(),
        rank,
    }
}

/// Marché FORWARD synthétique, fenêtre de décision `[2026-09-28, 2026-10-05)` à cheval sur deux
/// trimestres. BTC-USD : plat à 100 pendant la chauffe, 110 de J-3 à J+3 (régime haussier), 90 de J+3 à
/// J+5 (baissier) puis 115 (haussier). Les jambes sont des marches aléatoires déterministes qui
/// commencent à J-3.
pub fn fixture() -> Fixture {
    fixture_with_warmup(WARMUP_DAYS)
}

/// Comme [`fixture`], avec `warmup_days` jours de chauffe du régime (au moins 201 pour que la SMA200 soit
/// prête) ; les données de la fenêtre de décision sont les mêmes quelle que soit la chauffe.
pub fn fixture_with_warmup(warmup_days: i64) -> Fixture {
    let start_ms = at(2026, 9, 28, 0);
    let end_ms = at(2026, 10, 5, 0);
    let warmup_start_ms = start_ms - warmup_days * DAY;
    let leg_start = start_ms - 3 * DAY;
    let leg_rows = ((end_ms - leg_start) / MINUTE) as usize;

    let mut btc = Vec::new();
    let mut ts = warmup_start_ms;
    while ts < end_ms {
        let day = (ts - start_ms).div_euclid(DAY);
        let price = match day {
            ..=-4 => 100.0,
            -3..=2 => 110.0,
            3..=4 => 90.0,
            _ => 115.0,
        };
        btc.push(tv4::model::Candle {
            open_time_ms: ts,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 10.0,
            trades: None,
        });
        ts += MINUTE;
    }
    let mut series = vec![("BTC-USD".to_string(), Columns::from_candles(&btc))];
    for (symbol, seed) in FULL.iter().zip([11u64, 12, 13]) {
        series.push((
            (*symbol).to_string(),
            Columns::from_candles(&common::synthetic_candles(leg_rows, seed, leg_start, 0.0)),
        ));
    }

    let q3 = UniverseQuarter::new("2026-Q3", vec![member("AAA-USD", 1), member("BBB-USD", 2)])
        .expect("trimestre valide");
    let q4 = UniverseQuarter::new(
        "2026-Q4",
        vec![
            member("AAA-USD", 1),
            member("BBB-USD", 2),
            member("CCC-USD", 3),
        ],
    )
    .expect("trimestre valide");
    let schedule = UniverseSchedule::new(vec![q3, q4]).expect("calendrier valide");
    let mut exclusions = BTreeMap::new();
    exclusions.insert(
        "BBB-USD".to_string(),
        vec![QuarterExclusion {
            quarter: "2026-Q4".to_string(),
            tfs: vec![Timeframe::H4],
        }],
    );
    let quality = QualitySchedule::from_exclusions(exclusions).expect("exclusions valides");
    let subset = PairSubset::new(
        "okx_usdc",
        SubsetSource {
            instruments_file: "synthetic-instruments.json".to_string(),
            instruments_hash: SUBSET_SOURCE_HASH.to_string(),
            site: "synthetic".to_string(),
            fetched_at: "2026-09-01T00:00:00Z".to_string(),
        },
        SubsetRule {
            quote: "USDC".to_string(),
            state: "live".to_string(),
            trade_quote_ccy: true,
            aliases: BTreeMap::new(),
        },
        vec!["AAA".to_string(), "CCC".to_string()],
    )
    .expect("sous-ensemble valide");
    Fixture {
        series,
        schedule,
        quality,
        subset,
        start_ms,
        end_ms,
        warmup_start_ms,
        universe_spec: UniverseSpec::Quarterly {
            rules_hash: RULES_HASH.to_string(),
        },
    }
}

impl Fixture {
    /// Jambes jouées par une ligne, dans l'ordre de l'univers.
    pub fn played(&self, line: Line) -> Vec<String> {
        match line {
            Line::Full => names(&FULL),
            Line::OkxUsdc => names(&OKX),
        }
    }

    /// Jambes que le sous-ensemble retire de la ligne `okx_usdc`.
    pub fn excluded(&self, line: Line) -> Vec<String> {
        match line {
            Line::Full => Vec::new(),
            Line::OkxUsdc => names(&["BBB-USD"]),
        }
    }

    /// Séries de la ligne : le régime, puis les jambes jouées.
    pub fn line_series(&self, line: Line) -> Vec<(String, Columns)> {
        let played = self.played(line);
        self.series
            .iter()
            .filter(|(symbol, _)| symbol == "BTC-USD" || played.contains(symbol))
            .cloned()
            .collect()
    }

    /// Identité de recherche de la ligne (le moteur recalcule `legs_hash`).
    pub fn identity(&self, line: Line) -> ResearchIdentity {
        ResearchIdentity {
            block: Block::Forward,
            decision_start_ms: self.start_ms,
            decision_end_ms: self.end_ms,
            universe_window_hash: Some("blake3:window11".to_string()),
            quality_hash: self.quality.hash().expect("hash des exclusions"),
            line,
            subset_hash: match line {
                Line::Full => None,
                Line::OkxUsdc => Some(self.subset.subset_hash.clone()),
            },
            legs_hash: String::new(),
        }
    }

    /// Entrées de rapport de la ligne.
    pub fn inputs<'a>(&'a self, excluded: &'a [String], line: Line) -> ResearchInputs<'a> {
        ResearchInputs {
            guard: Guard::None,
            warmup_start_ms: self.warmup_start_ms,
            universe: Some(&self.schedule),
            quality: &self.quality,
            subset: match line {
                Line::Full => None,
                Line::OkxUsdc => Some(SubsetInfo {
                    subset: &self.subset,
                    legs_excluded: excluded,
                }),
            },
        }
    }

    /// Joue la ligne en mode recherche, capital `cash`.
    pub fn play(&self, line: Line, cash: f64) -> PortfolioResult {
        let played = self.played(line);
        let series = self.line_series(line);
        let identity = self.identity(line);
        let spec = ResearchSpec {
            block: Block::Forward,
            decision_start_ms: self.start_ms,
            decision_end_ms: self.end_ms,
            universe: Some(&self.schedule),
            universe_spec: &self.universe_spec,
            quality: &self.quality,
            identity: &identity,
        };
        let instruments: Vec<InstrumentRules> = vec![instrument(0.0); played.len()];
        run_portfolio_research(
            PortfolioInputs {
                series: &series,
                strategies: played.iter().map(|_| RegimeH4::boxed()).collect(),
                config: config(&played, &instruments, cash),
            },
            &spec,
        )
        .expect("run de recherche valide")
    }

    /// Informations de dataset de la ligne (un segment par série fournie au run).
    pub fn dataset_info(&self, line: Line) -> DatasetInfo {
        let series = self.line_series(line);
        DatasetInfo {
            name: "synthetique-forward".to_string(),
            exchange: ExchangeId::Kraken,
            tf: Timeframe::M1,
            // Politique d'un dataset de bloc : les trous intérieurs sont remplis et marqués sans volume.
            gaps_policy: "zero_volume_marked".to_string(),
            hash_format: "dsh-v1".to_string(),
            dataset_hash: DATASET_HASH.to_string(),
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

    /// Joue puis rend la ligne en fichiers (capital `cash`).
    pub fn render(&self, line: Line, cash: f64) -> RenderedProfile {
        let result = self.play(line, cash);
        self.render_result(&result, line)
    }

    /// Rend un résultat déjà joué.
    pub fn render_result(&self, result: &PortfolioResult, line: Line) -> RenderedProfile {
        let series = self.line_series(line);
        let played = self.played(line);
        let excluded = self.excluded(line);
        render_research_run(
            result,
            MetricsS9Inputs {
                dataset: self.dataset_info(line),
                instruments: InstrumentsInfo::toml(),
                okx_pairs: None,
                markets: markets(&series, &played),
            },
            &self.inputs(&excluded, line),
            Timeframe::H1,
        )
        .expect("rendu du run de recherche")
    }

    /// Manifeste s11 d'un run rendu.
    pub fn manifest(&self, rendered: &RenderedProfile, line: Line) -> ManifestS9 {
        ManifestS9::new_s11(
            &rendered.metrics,
            self.manifest_inputs(rendered),
            self.research_inputs(line),
        )
        .expect("manifeste s11")
    }

    /// Écrit les trois fichiers et le manifeste d'un run dans `dir`.
    pub fn write_run(&self, dir: &Path, rendered: &RenderedProfile, line: Line) {
        rendered.write_files(dir).expect("fichiers du run");
        write_json_atomic(dir, "manifest.json", &self.manifest(rendered, line)).expect("manifeste");
    }

    /// Le `lines.json` des deux lignes rendues.
    pub fn lines_report(&self, full: &RenderedProfile, okx: &RenderedProfile) -> LinesReport {
        LinesReport::build(
            &line_input(Line::Full, full),
            &line_input(Line::OkxUsdc, okx),
            &self.subset,
        )
        .expect("lines.json")
    }

    /// Joue les trois profils de coûts de la ligne en mode recherche, capital 300.
    pub fn play_profiles(&self, line: Line, file: &CostsFile, seed: u64) -> ProfilesResult {
        let played = self.played(line);
        let series = self.line_series(line);
        let identity = self.identity(line);
        let spec = ResearchSpec {
            block: Block::Forward,
            decision_start_ms: self.start_ms,
            decision_end_ms: self.end_ms,
            universe: Some(&self.schedule),
            universe_spec: &self.universe_spec,
            quality: &self.quality,
            identity: &identity,
        };
        let instruments: Vec<InstrumentRules> = vec![instrument(0.0); played.len()];
        let set = || played.iter().map(|_| RegimeH4::boxed()).collect::<Vec<_>>();
        run_portfolio_profiles_research(
            ProfilesInputs {
                series: &series,
                strategies: [set(), set(), set()],
                config: config(&played, &instruments, 300.0),
                costs: CostSetup {
                    file,
                    measures: None,
                    seed,
                },
            },
            &spec,
            &mut NoopObserver,
        )
        .expect("profils de recherche valides")
    }

    /// Rend les trois profils de la ligne (baseline, adverse, maker) en fichiers s11.
    pub fn render_profiles(&self, line: Line, file: &CostsFile, seed: u64) -> Vec<RenderedProfile> {
        let result = self.play_profiles(line, file, seed);
        let series = self.line_series(line);
        let played = self.played(line);
        let excluded = self.excluded(line);
        result
            .runs
            .iter()
            .map(|run| {
                render_profile_research(
                    run,
                    MetricsS9Inputs {
                        dataset: self.dataset_info(line),
                        instruments: InstrumentsInfo::toml(),
                        okx_pairs: None,
                        markets: markets(&series, &played),
                    },
                    CostsInputs {
                        file,
                        file_label: COSTS_LABEL,
                        measures: None,
                        measures_label: None,
                    },
                    &self.inputs(&excluded, line),
                    Timeframe::H1,
                )
                .expect("rendu du profil de recherche")
            })
            .collect()
    }

    /// Écrit un dossier de profils s11 : trois sous-dossiers avec manifeste, puis `profiles.json`.
    pub fn write_profiles_dir(
        &self,
        dir: &Path,
        rendered: &[RenderedProfile],
        file: &CostsFile,
        line: Line,
    ) {
        let report = profiles_report(rendered, file);
        let ids: Vec<String> = rendered.iter().map(|r| r.metrics.run_id()).collect();
        let id = profiles_id(&ids[0], &ids[1], &ids[2]);
        assert_eq!(id, report.profiles_id);
        for (profile, name) in rendered.iter().zip(["baseline", "adverse", "maker"]) {
            let sub = dir.join(name);
            profile.write_files(&sub).expect("fichiers du profil");
            let manifest = ManifestS9::new_s11_profile(
                &profile.metrics,
                ManifestS10Inputs {
                    base: self.manifest_inputs(profile),
                    costs_file: COSTS_LABEL.to_string(),
                    cost_measures_file: None,
                    profiles_id: id.clone(),
                },
                self.research_inputs(line),
            )
            .expect("manifeste s11 de profil");
            write_json_atomic(&sub, "manifest.json", &manifest).expect("manifeste");
        }
        report.write(dir).expect("profiles.json");
    }

    fn research_inputs(&self, line: Line) -> BTreeMap<String, String> {
        let mut research_inputs = BTreeMap::new();
        research_inputs.insert("universe".to_string(), "synthetic-universe".to_string());
        if line == Line::OkxUsdc {
            research_inputs.insert(
                "okx_subset".to_string(),
                "synthetic-subset.json".to_string(),
            );
        }
        research_inputs
    }

    fn manifest_inputs(&self, rendered: &RenderedProfile) -> ManifestS9Inputs {
        ManifestS9Inputs {
            instruments_file: None,
            engine: engine_info(),
            equity_step: Timeframe::H1,
            files: OutputFiles {
                equity_csv: tv4::model::hash::blake3_prefixed(&rendered.equity_bytes),
                metrics_json: tv4::model::hash::blake3_prefixed(&rendered.metrics_bytes),
                trades_csv: tv4::model::hash::blake3_prefixed(&rendered.trades_bytes),
            },
            started_at_ms: 1_790_000_000_000,
            timings: Timings {
                load_ms: 1,
                run_ms: 2,
                cli_cold_ms: 3,
            },
        }
    }

    /// Écrit un dossier de lignes complet : `full/` et `okx_usdc/` avec leurs manifestes, puis
    /// `lines.json`.
    pub fn write_lines_dir(&self, dir: &Path, cash: f64) {
        let full = self.render(Line::Full, cash);
        let okx = self.render(Line::OkxUsdc, cash);
        self.write_run(&dir.join("full"), &full, Line::Full);
        self.write_run(&dir.join("okx_usdc"), &okx, Line::OkxUsdc);
        self.lines_report(&full, &okx)
            .write(dir)
            .expect("écriture de lines.json");
    }
}
