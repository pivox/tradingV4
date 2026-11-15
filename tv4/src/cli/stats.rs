//! S13 : orchestration hors marché des artefacts possédés et vérifiés.
mod corpus;
mod selection;
use super::{CliError, research};
use crate::{
    report::{
        stats::{self, output::*},
        stats_inputs::LoadedStatsInputs,
        verify,
    },
    research::stats::*,
};
use clap::Args;
use std::{fs, path::PathBuf};

#[derive(Debug, Args)]
pub struct StatsArgs {
    pub run_dir: PathBuf,
    #[arg(long)]
    pub seed: u64,
    #[arg(long, default_value_t=10_000, value_parser=clap::value_parser!(u32).range(10_000..))]
    pub samples: u32,
    #[arg(long, default_value_t=5, value_parser=clap::value_parser!(u8).range(5..=10))]
    pub bootstrap_block: u8,
    #[arg(long, default_value_t=0.7, value_parser=correlation)]
    pub correlation_threshold: f64,
    #[arg(long)]
    pub inputs: Option<PathBuf>,
    #[arg(long)]
    pub delayed_run: Option<PathBuf>,
    #[arg(long)]
    pub peer_run: Vec<PathBuf>,
    #[arg(long)]
    pub research_root: Option<PathBuf>,
    #[arg(long)]
    pub btc_symbol: Option<String>,
    #[arg(long)]
    pub eth_symbol: Option<String>,
}
fn correlation(value: &str) -> Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite() && (0.0..1.0).contains(x))
        .ok_or_else(|| "seuil de corrélation fini dans [0,1[ requis".into())
}
fn error(message: impl ToString) -> CliError {
    CliError::Stats(message.to_string())
}
fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage {
        option: "stats".into(),
        detail: message.into(),
    }
}

struct Leaf {
    dir: PathBuf,
    run: stats::VerifiedRun,
    image: stats::RunSnapshot,
    external: Option<LoadedStatsInputs>,
    layouts: Vec<LayoutFile>,
}
impl Leaf {
    fn packet(&self) -> Option<&LoadedStatsInputs> {
        self.external.as_ref().or_else(|| self.run.stats_inputs())
    }
    fn source(&self) -> Result<Source, CliError> {
        let mut source = Source::from_run(&self.run, self.packet().map(|p| p.bytes.as_slice()))
            .map_err(error)?;
        source.layout_files = self.layouts.clone();
        Ok(source)
    }
    fn daily(&self) -> Result<DailyReturns, CliError> {
        daily_returns(
            self.packet().map_or(&[], |p| p.inputs.marks.as_slice()),
            self.image.provenance.window,
        )
        .map_err(error)
    }
}
fn load(dir: &std::path::Path) -> Result<(Vec<Leaf>, Vec<LayoutFile>), CliError> {
    let tree = verify::load_stats_leaves(dir)?;
    let layouts: Vec<LayoutFile> = tree
        .layout_files
        .into_iter()
        .map(|(p, b)| LayoutFile {
            file: p
                .strip_prefix(dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned(),
            hash: crate::model::hash::blake3_prefixed(&b),
        })
        .collect();
    let leaves = tree
        .leaves
        .into_iter()
        .map(|(dir, run)| {
            let image = stats::snapshot(&run).map_err(error)?;
            Ok(Leaf {
                dir,
                run,
                image,
                external: None,
                layouts: layouts.clone(),
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    Ok((leaves, layouts))
}
fn partition(leaf: &Leaf) -> ResearchPartition {
    let m = leaf.run.metrics();
    let data = m.pointer("/data/block").and_then(serde_json::Value::as_str);
    if m.pointer("/data/source")
        .and_then(serde_json::Value::as_str)
        != Some("dataset")
        || data
            != m.pointer("/research/block")
                .and_then(serde_json::Value::as_str)
    {
        return ResearchPartition::Unclassified;
    }
    let Some(block) = data.and_then(|b| b.parse::<crate::model::Block>().ok()) else {
        return ResearchPartition::Unclassified;
    };
    // Le lecteur partagé a déjà contrôlé le contrat S12 source/effectif/génération.
    // Après rotation, les dates restent celles du bloc source et la partition
    // statistique reste le bloc effectif. Sans cette provenance, garder la
    // géométrie statique du bloc annoncé : aucune reclassification implicite.
    let geometry_block = leaf
        .run
        .manifest()
        .pointer("/governance/source_block")
        .and_then(serde_json::Value::as_str)
        .and_then(|source| source.parse::<crate::model::Block>().ok())
        .filter(|_| {
            leaf.run
                .manifest()
                .pointer("/governance/effective_block")
                .and_then(serde_json::Value::as_str)
                == data
        })
        .unwrap_or(block);
    let span = geometry_block.span();
    let window = leaf.image.provenance.window;
    if !span.contains(window.start_ms)
        || !span.contains(window.end_ms - 1)
        || leaf
            .run
            .manifest()
            .pointer("/governance/effective_block")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|b| Some(b) != data)
    {
        return ResearchPartition::Unclassified;
    }
    match data {
        Some("DEV") => ResearchPartition::Dev,
        Some("VALIDATION") => ResearchPartition::Validation,
        Some("HOLDOUT") => ResearchPartition::Holdout,
        Some("FORWARD") => ResearchPartition::Forward,
        _ => ResearchPartition::Unclassified,
    }
}
pub(super) fn run(args: &StatsArgs) -> Result<(), CliError> {
    let options = StatsOptions {
        seed: args.seed,
        samples: args.samples,
        bootstrap_block: usize::from(args.bootstrap_block),
        correlation_threshold: args.correlation_threshold,
    };
    let (mut leaves, layout_files) = load(&args.run_dir)?;
    if let Some(path) = &args.inputs {
        if leaves.len() != 1 {
            return Err(usage("--inputs exige une seule feuille explicite"));
        }
        let bytes = fs::read(path).map_err(|e| error(format!("{} : {e}", path.display())))?;
        let packet = LoadedStatsInputs::from_bytes(bytes, &leaves[0].run).map_err(error)?;
        if let Some(original) = leaves[0].run.stats_inputs()
            && original.bytes != packet.bytes
        {
            return Err(error("paquet externe différent du paquet référencé"));
        }
        leaves[0].external = Some(packet);
    }
    let delayed = if let Some(dir) = &args.delayed_run {
        let (other, layouts) = load(dir)?;
        if other.len() != leaves.len()
            || layouts.iter().map(|l| &l.file).collect::<Vec<_>>()
                != layout_files.iter().map(|l| &l.file).collect::<Vec<_>>()
        {
            return Err(usage("layout du rejeu décalé différent"));
        }
        Some(other)
    } else {
        None
    };
    let mut peers = Vec::new();
    // L'ordre CLI n'entre pas dans la sortie ; le corpus est trié par identité et dédupliqué.
    for dir in &args.peer_run {
        let (mut loaded, _) = load(dir)?;
        let matches = |l: &Leaf| {
            leaves
                .iter()
                .any(|base| base.image.provenance.line == l.image.provenance.line)
        };
        if !loaded.iter().any(matches) {
            return Err(usage(
                "corpus : peer sans ligne correspondante dans le run courant",
            ));
        }
        loaded.retain(matches);
        peers.append(&mut loaded);
    }
    peers.sort_by_cached_key(|l| {
        (
            crate::model::hash::blake3_prefixed(l.run.manifest_bytes()),
            l.layouts
                .iter()
                .map(|f| (f.file.clone(), f.hash.clone()))
                .collect::<Vec<_>>(),
        )
    });
    let registry = corpus::registry(args)?;
    let mut outputs = Vec::new();
    for leaf in &leaves {
        let image = &leaf.image;
        let p = leaf.packet().map(|p| &p.inputs);
        let marks = p.map_or(&[][..], |p| p.marks.as_slice());
        let markets = p.map_or(&[][..], |p| p.markets.as_slice());
        let (selection, symbols) = selection::resolve(args, &image.pair_symbols, markets)?;
        let partition = partition(leaf);
        let corpus = corpus::build(leaf, &leaves, &peers, &registry, &options)?;
        let mut current_noise = corpus
            .declared_verified_n
            .map(|n| crate::research::noise::noise_context(n as u64, 5.))
            .transpose()
            .map_err(error)?;
        if let (Some(noise), Some(clusters)) = (&mut current_noise, &corpus.clustering.value)
            && clusters.global_n_eff.value.is_some()
        {
            noise.n_eff = Some(clusters.panel_n_eff as u64);
        }
        let (delayed_entries, delayed_source) = if let Some(others) = &delayed {
            let matches = others
                .iter()
                .filter(|d| {
                    d.image.provenance.line == image.provenance.line
                        && d.image.provenance.profile == image.provenance.profile
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(usage(
                    "feuille de rejeu décalé absente ou ambiguë pour profil/ligne",
                ));
            }
            let other = matches[0];
            (
                Section::available(
                    stats::compare_delayed_entries(&leaf.run, &other.run).map_err(usage)?,
                ),
                Some(other.source()?),
            )
        } else {
            (
                Section::unavailable(
                    "aucun rejeu causal fourni : utiliser --delayed-run sur un backtest explicite",
                ),
                None,
            )
        };
        let output = StatsOutput {
            schema: SCHEMA.into(),
            options: options.clone(),
            source: leaf.source()?,
            layout_files: layout_files.clone(),
            partition,
            selection,
            bootstrap: bootstrap(&image.trades, &options).map_err(error)?,
            concentration: concentration(&image.trades).map_err(error)?,
            monte_carlo: monte_carlo(
                &image.trades,
                p.map_or(&[], |p| p.closures.as_slice()),
                marks,
                image.provenance.window,
                image.provenance.initial_cash,
                image.sizing,
                &options,
            )
            .map_err(error)?,
            regimes: regimes(
                &image.trades,
                markets.iter().find(|m| m.symbol == symbols.btc),
                marks,
                image.provenance.window,
            )
            .map_err(error)?,
            benchmarks: benchmarks(
                BenchmarkInput {
                    trades: &image.trades,
                    marks,
                    cash: p.map_or(&[], |p| p.cash.as_slice()),
                    markets,
                    capital: image.provenance.initial_cash,
                    window: image.provenance.window,
                    pair_symbols: &image.pair_symbols,
                    symbols: &symbols,
                    partition,
                },
                &options,
            )
            .map_err(error)?,
            delayed_entries,
            delayed_source,
            registry: registry.clone(),
            current_noise,
            corpus,
            methods: vec![
                "s13-v1_splitmix64_separate_streams_type7".into(),
                "bootstrap_circular_trade_blocks".into(),
                "monte_carlo_circular_temporal_blocks_min20_marked".into(),
                "regimes_causal_sma200_vol30_expanding_median".into(),
                "benchmarks_initial_weights_cash_simple_prorata".into(),
                "pearson_strict_connected_components_v1".into(),
            ],
        };
        outputs.push((leaf.dir.clone(), output.canonical_bytes().map_err(error)?));
    }
    publish(&outputs).map_err(error)
}
