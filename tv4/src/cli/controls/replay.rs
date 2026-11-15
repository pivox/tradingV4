//! Préflight et rendu Observed via les mêmes configurations et renderers que backtest.
use super::{
    error,
    plan::LoadedPlan,
    sources::{Sources, string},
    usage,
};
use crate::{
    cli::{
        CliError,
        backtest::portfolio::{Instruments, market_of, okx_instruments_owned},
    },
    engine::{
        self, CostSetup, Line, PortfolioConfig, ResearchIdentity, ResearchSpec, UniverseSpec,
        controls::{BatchInputs, ObservedRun, ObservedValidation, SourceRuntime},
    },
    model::{
        FeeSchedule, InstrumentRules, Slippage, Timeframe,
        costs::{CostMeasures, CostProfileName, CostsFile},
        universe::PairSubset,
    },
    report::{
        self,
        controls::{SourceMetadata, bundle::source_quotas},
        metrics_s9::{CostsInputs, DatasetInfo, InstrumentsInfo, MetricsS9Inputs, SeriesInfo},
        profiles::RenderedProfile,
        research::{Guard, ResearchInputs, SubsetInfo},
    },
    sim::{SimRules, SizingMode},
    store::{self, Columns, DatasetManifest, ResearchInfo, universe::LoadedUniverse},
    strategy::{Strategy, StrategyError, registry},
};
use std::{collections::BTreeMap, path::Path, sync::Arc};
pub struct Recipe {
    pub params: Vec<toml::Value>,
    pub strategies: Vec<String>,
    pub manifest: DatasetManifest,
    pub info: Option<ResearchInfo>,
    pub universe: Option<LoadedUniverse>,
    pub universe_spec: UniverseSpec,
    pub subset: Option<PairSubset>,
    pub excluded: Vec<String>,
    pub symbols: Vec<String>,
    pub instruments: Instruments,
    pub fees: FeeSchedule,
    pub costs: Option<CostsFile>,
    pub measures: Option<CostMeasures>,
    pub metadata: Vec<SourceMetadata>,
    pub hashes: Vec<String>,
    pub records: Vec<engine::StrategyRecord>,
    pub tfs: Vec<Timeframe>,
    pub identity: Option<ResearchIdentity>,
}
fn profile(p: report::controls::identity::ControlProfile) -> CostProfileName {
    match p {
        report::controls::identity::ControlProfile::Baseline => CostProfileName::Baseline,
        report::controls::identity::ControlProfile::Adverse => CostProfileName::Adverse,
        report::controls::identity::ControlProfile::Maker => CostProfileName::Maker,
    }
}
impl Recipe {
    pub fn prepare(plan: &LoadedPlan, sources: &Sources) -> Result<Self, CliError> {
        let p = &plan.plan;
        let manifest: DatasetManifest =
            serde_json::from_slice(plan.data(&p.dataset_manifest)?).map_err(usage)?;
        let info = store::research_meta(&manifest).map_err(usage)?;
        if manifest.dataset_hash != sources.context.dataset_hash
            || manifest.tfs != [Timeframe::M1]
            || manifest.block != sources.context.source_block
        {
            return Err(usage("dataset/TF/bloc de recette différent des sources"));
        }
        if let Some(info) = &info {
            for run in &sources.runs {
                if run
                    .metrics()
                    .pointer("/research/identity/decision_start_ms")
                    .and_then(|v| v.as_i64())
                    != Some(info.decision_start_ms)
                    || run
                        .metrics()
                        .pointer("/research/identity/decision_end_ms")
                        .and_then(|v| v.as_i64())
                        != Some(info.decision_end_ms)
                {
                    return Err(usage(
                        "fenêtre décisionnelle historique différente du dataset",
                    ));
                }
            }
        }
        if info.is_none() && (p.line.is_some() || p.universe.is_some() || p.subset.is_some()) {
            return Err(usage("ligne/univers réservés au dataset de recherche"));
        }
        let universe = p
            .universe
            .as_ref()
            .map(|u| {
                let parent = Path::new(&u.file.file)
                    .parent()
                    .ok_or_else(|| usage("dossier univers absent"))?;
                let mut files = BTreeMap::new();
                for f in std::iter::once(&u.file).chain(&u.files) {
                    let rel = Path::new(&f.file)
                        .strip_prefix(parent)
                        .map_err(|_| usage("fichier univers hors de son dossier déclaré"))?
                        .to_str()
                        .ok_or_else(|| usage("fichier univers non UTF-8"))?;
                    files.insert(rel.into(), plan.data(f)?.to_vec());
                }
                store::universe::load_from_bytes(Path::new("univers_commite"), &files)
                    .map_err(usage)
            })
            .transpose()?;
        let universe_spec = universe.as_ref().map_or_else(
            || UniverseSpec::Fixed {
                symbols: p.symbols.clone(),
            },
            |u| UniverseSpec::Quarterly {
                rules_hash: u.rules_hash().into(),
            },
        );
        let subset = p
            .subset
            .as_ref()
            .map(|s| PairSubset::from_json_str(plan.text(s)?).map_err(usage))
            .transpose()?;
        let all = if let (Some(u), Some(i)) = (&universe, &info) {
            let wh = u
                .window_hash(i.decision_start_ms, i.decision_end_ms)
                .map_err(usage)?;
            if i.universe
                .as_ref()
                .is_none_or(|r| r.rules_hash != u.rules_hash() || r.window_hash != wh)
            {
                return Err(usage("univers différent du dataset"));
            }
            u.union_symbols(i.decision_start_ms, i.decision_end_ms)
        } else {
            p.symbols.clone()
        };
        let (symbols, excluded): (Vec<_>, Vec<_>) = all.into_iter().partition(|s| {
            p.line != Some(report::controls::identity::ControlLine::OkxUsdc)
                || subset
                    .as_ref()
                    .is_some_and(|x| x.contains_base(s.split_once('-').map_or(s.as_str(), |v| v.0)))
        });
        if symbols != sources.snapshots[0].pair_symbols || symbols.is_empty() {
            return Err(usage("jambes de recette différentes de la ligne source"));
        }
        let instruments = match p.instruments_format.as_str() {
            "toml" => {
                let text = plan.text(&p.instruments)?;
                if universe
                    .as_ref()
                    .is_some_and(|u| u.instruments_text() != text)
                {
                    return Err(usage("instruments différents de l'univers"));
                }
                Instruments {
                    rules: symbols
                        .iter()
                        .map(|s| InstrumentRules::from_toml_str(manifest.exchange, s, text))
                        .collect::<Result<_, _>>()
                        .map_err(usage)?,
                    info: InstrumentsInfo::toml(),
                    okx_pairs: None,
                    hash: None,
                    file_as_given: None,
                }
            }
            "okx" => {
                let file = crate::marketdata::okx::instruments::InstrumentsFile::from_json_str(
                    plan.text(&p.instruments)?,
                    &p.instruments.file,
                )
                .map_err(usage)?;
                let label = sources.runs[0]
                    .metrics()
                    .pointer("/instruments/file")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| usage("fichier instruments source absent"))?;
                okx_instruments_owned(file, Path::new(label), &symbols, "USDC").map_err(usage)?
            }
            _ => return Err(usage("format instruments inconnu")),
        };
        let fees = FeeSchedule::from_toml_str_grid(
            p.fees_grid.parse().map_err(usage)?,
            plan.text(&p.fees)?,
        )
        .map_err(usage)?;
        let costs = p
            .costs
            .as_ref()
            .map(|c| CostsFile::from_toml_str(plan.text(&c.file)?).map_err(usage))
            .transpose()?;
        let measures = p
            .costs
            .as_ref()
            .and_then(|c| c.measures.as_ref())
            .map(|m| CostMeasures::from_json_str(plan.text(m)?).map_err(usage))
            .transpose()?;
        if let Some(c) = &costs
            && sources.context.costs.as_ref().is_none_or(|x| {
                x.config_hash != c.file_hash()
                    || Some(x.seed) != p.costs.as_ref().map(|c| c.cost_seed)
            })
        {
            return Err(usage("coûts ou cost_seed différents de la source"));
        }
        for run in &sources.runs {
            let source_measures = run
                .metrics()
                .pointer("/costs/cost_measures_hash")
                .and_then(|v| v.as_str());
            if source_measures != measures.as_ref().map(CostMeasures::file_hash) {
                return Err(usage("mesures de coûts différentes des recettes source"));
            }
        }
        let identity = info
            .as_ref()
            .map(|i| {
                Ok::<_, CliError>(ResearchIdentity {
                    block: i.block,
                    decision_start_ms: i.decision_start_ms,
                    decision_end_ms: i.decision_end_ms,
                    universe_window_hash: universe
                        .as_ref()
                        .map(|u| {
                            u.window_hash(i.decision_start_ms, i.decision_end_ms)
                                .map_err(usage)
                        })
                        .transpose()?,
                    quality_hash: i.quality.hash().map_err(usage)?,
                    line: match p.line {
                        Some(report::controls::identity::ControlLine::OkxUsdc) => Line::OkxUsdc,
                        _ => Line::Full,
                    },
                    subset_hash: if p.line == Some(report::controls::identity::ControlLine::OkxUsdc)
                    {
                        subset.as_ref().map(|s| s.subset_hash.clone())
                    } else {
                        None
                    },
                    legs_hash: String::new(),
                })
            })
            .transpose()?;
        let mut recipe = Self {
            params: Vec::new(),
            strategies: Vec::new(),
            manifest,
            info,
            universe,
            universe_spec,
            subset,
            excluded,
            symbols,
            instruments,
            fees,
            costs,
            measures,
            metadata: Vec::new(),
            hashes: Vec::new(),
            records: Vec::new(),
            tfs: Vec::new(),
            identity,
        };
        for (i, parent) in p.parents.iter().enumerate() {
            let params =
                toml::from_str::<toml::Value>(plan.text(&parent.params)?).map_err(usage)?;
            let strategy = string(sources.runs[i].metrics(), "/config/strategy/id")?;
            recipe.params.push(params);
            recipe.strategies.push(strategy);
            let strategies = recipe.factory(i).map_err(usage)?;
            let geometry = engine::controls::preflight_geometry(&strategies).map_err(usage)?;
            let packet = &sources.runs[i]
                .stats_inputs()
                .ok_or_else(|| usage("paquet manquant"))?
                .inputs;
            if packet.decision_timeframe_minutes != (geometry.decision_tf.ms() / 60_000) as u32
                || sources.snapshots[i]
                    .decision_timeframe_minutes
                    .is_some_and(|v| v != packet.decision_timeframe_minutes)
            {
                return Err(usage("TF déclaré différent du vrai TF de géométrie"));
            }
            if recipe
                .tfs
                .first()
                .is_some_and(|tf| *tf != geometry.decision_tf)
            {
                return Err(usage("TF de décision hétérogènes entre parents"));
            }
            recipe.tfs.push(geometry.decision_tf);
            let config = recipe.config(plan, sources, i)?;
            let run_config = engine::preflight::portfolio(
                &strategies,
                &config,
                recipe.cost_setup(plan),
                recipe.info.as_ref().map(|_| &recipe.universe_spec),
            )
            .map_err(usage)?;
            if run_config.hash()? != parent.config_hash {
                return Err(usage("configuration préflight différente du parent source"));
            }
            if let Some(identity) = &recipe.identity {
                // S11 retire les règles par jambe du config_hash : recouper aussi son identité native.
                let native = engine::preflight::portfolio(
                    &strategies,
                    &config,
                    recipe.cost_setup(plan),
                    None,
                )
                .map_err(usage)?;
                let empty_costs = BTreeMap::new();
                let mut expected = identity.clone();
                expected.legs_hash = engine::legs_hash(
                    config.universe,
                    config.instruments,
                    config.instruments_hash,
                    native
                        .costs
                        .as_ref()
                        .map_or(&empty_costs, |costs| &costs.legs),
                )?;
                if sources.runs[i].metrics().pointer("/research/identity")
                    != Some(&serde_json::to_value(&expected).map_err(error)?)
                {
                    return Err(usage(
                        "identité de recherche historique différente des recettes",
                    ));
                }
            }
            recipe.records.push(run_config.strategy);
            recipe.hashes.push(parent.config_hash.clone());
            recipe.metadata.push(SourceMetadata {
                config_hash: parent.config_hash.clone(),
                geometry_hash: geometry.geometry_hash,
                has_btc_filter: geometry.has_btc_filter,
            });
        }
        Ok(recipe)
    }
    pub fn factory(&self, index: usize) -> Result<Vec<Box<dyn Strategy>>, StrategyError> {
        (0..self.instruments.rules.len())
            .map(|_| registry::build(&self.strategies[index], &self.params[index]))
            .collect()
    }
    fn config<'a>(
        &'a self,
        plan: &'a LoadedPlan,
        sources: &Sources,
        index: usize,
    ) -> Result<PortfolioConfig<'a>, CliError> {
        let p = &plan.plan;
        let rules = SimRules {
            extra_entry_delay_bars: p.entry_delay_bars * (self.tfs[index].ms() / 60_000) as u32,
            ..SimRules::default()
        };
        let quote_gap_ms = sources.runs[index]
            .metrics()
            .pointer("/config/portfolio/quote_gap_ms")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| usage("quote_gap_ms absent"))?;
        Ok(PortfolioConfig {
            universe: &self.symbols,
            regime_symbol: &p.regime_symbol,
            fees: self.fees,
            slippage: if self.costs.is_some() {
                Slippage::ZERO
            } else {
                Slippage::from_bps(p.slippage_bps).map_err(usage)?
            },
            initial_cash: sources.snapshots[index].provenance.initial_cash,
            sim_rules: rules,
            instruments: &self.instruments.rules,
            instruments_hash: self.instruments.hash.as_deref(),
            max_positions: p.max_positions,
            sizing: SizingMode::Research,
            min_order_bump_max_risk: None,
            quote_gap_ms,
            min_trades: p.min_trades,
        })
    }
    fn cost_setup(&self, plan: &LoadedPlan) -> Option<(CostSetup<'_>, CostProfileName)> {
        self.costs.as_ref().zip(plan.plan.profile).map(|(file, p)| {
            (
                CostSetup {
                    file,
                    measures: self.measures.as_ref(),
                    seed: plan.plan.costs.as_ref().map_or(1, |c| c.cost_seed),
                },
                profile(p),
            )
        })
    }
    fn spec(&self) -> Option<ResearchSpec<'_>> {
        self.info
            .as_ref()
            .zip(self.identity.as_ref())
            .map(|(info, identity)| ResearchSpec {
                block: info.block,
                decision_start_ms: info.decision_start_ms,
                decision_end_ms: info.decision_end_ms,
                universe: self.universe.as_ref().map(|u| u.schedule()),
                universe_spec: &self.universe_spec,
                quality: &info.quality,
                identity,
            })
    }
    pub fn run(
        &self,
        plan: &LoadedPlan,
        sources: &Sources,
        dataset: Arc<store::MultiDataset>,
    ) -> Result<engine::controls::BatchOutcome, CliError> {
        let factories = (0..self.params.len())
            .map(|i| move || self.factory(i))
            .collect::<Vec<_>>();
        let spec = self.spec();
        let runtime = factories
            .iter()
            .enumerate()
            .map(|(i, f)| {
                Ok(SourceRuntime {
                    config: self.config(plan, sources, i)?,
                    costs: self.cost_setup(plan),
                    research: spec.as_ref(),
                    quotas: source_quotas(&sources.runs[i]).map_err(error)?,
                    factory: f,
                })
            })
            .collect::<Result<Vec<_>, CliError>>()?;
        let mut observed = |i: usize, run: ObservedRun<'_>| -> Result<ObservedValidation, String> {
            self.render(plan, sources, i, run, &dataset.series)
                .map(|r| {
                    if r.files().metrics_json == sources.snapshots[i].provenance.files.metrics_json
                        && r.files().trades_csv == sources.snapshots[i].provenance.files.trades_csv
                        && r.files().equity_csv == sources.snapshots[i].provenance.files.equity_csv
                    {
                        ObservedValidation::Matched
                    } else {
                        ObservedValidation::Mismatched
                    }
                })
                .map_err(|e| e.to_string())
        };
        engine::controls::run_batch(
            BatchInputs {
                dataset: Arc::clone(&dataset),
                sources: runtime,
                options: plan.options.clone(),
            },
            &mut observed,
        )
        .map_err(error)
    }
    fn render(
        &self,
        plan: &LoadedPlan,
        sources: &Sources,
        index: usize,
        run: ObservedRun<'_>,
        series: &[(String, Columns)],
    ) -> Result<RenderedProfile, CliError> {
        let result = match &run {
            ObservedRun::Plain(r) => *r,
            ObservedRun::Profile(r) => &r.result,
        };
        let dataset = DatasetInfo {
            name: self.manifest.name.clone(),
            exchange: self.manifest.exchange,
            tf: Timeframe::M1,
            gaps_policy: self.manifest.gaps_policy.as_str().into(),
            hash_format: self.manifest.hash_format.clone(),
            dataset_hash: self.manifest.dataset_hash.clone(),
            series: series
                .iter()
                .filter(|(s, _)| sources.snapshots[index].market_symbols.contains(s))
                .map(|(s, c)| SeriesInfo {
                    symbol: s.clone(),
                    rows: c.len() as u64,
                    first_ms: c.open_time_ms().first().copied().unwrap_or(0),
                    last_ms: c.open_time_ms().last().copied().unwrap_or(0),
                })
                .collect(),
        };
        let inputs = MetricsS9Inputs {
            dataset,
            instruments: self.instruments.info.clone(),
            okx_pairs: self.instruments.okx_pairs.clone(),
            markets: result
                .universe
                .iter()
                .map(|s| market_of(series, s))
                .collect(),
        };
        let research = self.info.as_ref().map(|i| ResearchInputs {
            guard: match sources.runs[index]
                .metrics()
                .pointer("/research/guard")
                .and_then(|v| v.as_str())
            {
                Some("guarded") => Guard::Guarded,
                Some("unguarded") => Guard::Unguarded,
                _ => Guard::None,
            },
            warmup_start_ms: store::time::parse_rfc3339_utc(&self.manifest.window[0]).unwrap_or(0),
            universe: self.universe.as_ref().map(|u| u.schedule()),
            quality: &i.quality,
            subset: self
                .subset
                .as_ref()
                .filter(|_| {
                    plan.plan.line == Some(report::controls::identity::ControlLine::OkxUsdc)
                })
                .map(|subset| SubsetInfo {
                    subset,
                    legs_excluded: &self.excluded,
                }),
        });
        let costs = self.costs.as_ref().map(|file| CostsInputs {
            file,
            file_label: sources.runs[index]
                .metrics()
                .pointer("/costs/costs_file")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            measures: self.measures.as_ref(),
            measures_label: sources.runs[index]
                .metrics()
                .pointer("/costs/cost_measures_file")
                .and_then(|v| v.as_str()),
        });
        Ok(match (run, research, costs) {
            (ObservedRun::Plain(r), Some(ref re), _) => {
                report::research::render_research_run(r, inputs, re, plan.plan.equity_step)?
            }
            (ObservedRun::Plain(r), None, _) => {
                crate::cli::backtest::portfolio::render_portfolio(r, inputs, plan.plan.equity_step)?
            }
            (ObservedRun::Profile(r), Some(ref re), Some(c)) => {
                report::profiles::render_profile_research(r, inputs, c, re, plan.plan.equity_step)?
            }
            (ObservedRun::Profile(r), None, Some(c)) => {
                report::profiles::render_profile(r, inputs, c, plan.plan.equity_step)?
            }
            _ => return Err(error("rendu Observed incohérent")),
        })
    }
}
