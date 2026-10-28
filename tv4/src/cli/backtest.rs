//! Sous-commande `tv4 backtest` : backtest sur une fixture CSV Kraken 1m (S3) ou sur un dataset figé
//! du store (S4), qui écrit les quatre fichiers d'un run (S8) : `metrics.json`, `trades.csv`,
//! `equity.csv` puis `manifest.json`.
//!
//! Aucune logique métier ici (docs/03-architecture.md §2) : lecture des fichiers, appel du registre,
//! de `store`, de `engine::run_cell` et de `report`. Les entrées communes à `backtest` et à `bench`
//! (S8) sont dans [`RunInputArgs`] et préparées par [`prepare`].
//!
//! Avec `--symbols`, la commande bascule en mode portefeuille (S9, module `portfolio`) : plusieurs
//! paires sur un cash commun, `engine::run_portfolio`, schémas `tv4-metrics-s9` et `tv4-manifest-s9`.
//! Sans `--symbols`, le chemin de S8 ne change pas, et toute option du portefeuille est refusée
//! ([`CliError::PortfolioOptionWithoutSymbols`]) pour qu'aucun run S8 ne croie l'appliquer.
//!
//! Avec `--costs` (S10, module `profiles`, mode portefeuille seulement), trois runs de profils de coûts
//! sont écrits sous `--out` (`baseline/`, `adverse/`, `maker/`) puis `profiles.json`. Sans `--costs`, les
//! chemins de S8 et de S9 ne changent pas.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Args;

use super::{CliError, now_ms};
use crate::engine::{self, CellConfig, DEFAULT_MIN_TRADES};
use crate::marketdata::kraken;
use crate::model::hash;
use crate::model::{
    Candle, ExchangeId, FeeGridId, FeeSchedule, InstrumentRules, Slippage, Timeframe, Ts,
};
use crate::report::manifest::{ManifestInputs, OutputFiles, Timings};
use crate::report::{self, DataInfo, DatasetRef, EngineInfo, Manifest, Metrics, TradesContext};
use crate::sim::SimRules;
use crate::store::{self, Columns, GapsPolicy, Window, dsh};
use crate::strategy::{Strategy, registry};
use portfolio::PortfolioArgs;
pub use profiles::CostArgs;

mod portfolio;
mod profiles;

/// Symbole canonique de la fixture (XBTUSD Kraken, docs/03 §4).
const FIXTURE_SYMBOL: &str = "BTC-USD";
/// Exchange de la fixture : un dump Kraken, dont `config/instruments.toml` donne tick et lot.
const FIXTURE_EXCHANGE: ExchangeId = ExchangeId::Kraken;
/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = 60_000;
/// Manifeste d'un run : sa présence marque un run complet.
const MANIFEST_FILE: &str = "manifest.json";
/// Rapport HTML rendu par `tv4:report` à côté d'un run.
const REPORT_FILE: &str = "report.html";
/// Slippage scalaire du chemin hérité (S8 et S9) quand `--slippage-bps` n'est pas donné, en points de
/// base. Avec `--costs`, l'option est refusée : le slippage vient du profil de coûts (S10 §14.1).
const DEFAULT_SLIPPAGE_BPS: f64 = 5.0;
/// Taille du tampon de lecture du binaire pour son hash.
const BINARY_HASH_BUFFER: usize = 64 * 1024;

/// Entrées d'un run, communes à `tv4 backtest` et à `tv4 bench`.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("donnees").required(true).args(["fixture", "dataset"])))]
pub struct RunInputArgs {
    /// CSV Kraken OHLCVT en 1m, sans en-tête (par exemple fixtures/kraken_xbtusd_1m.csv).
    /// Exclusif avec --dataset.
    #[arg(long, value_name = "CSV")]
    pub fixture: Option<PathBuf>,
    /// Dataset figé : un nom (`btcusd_2y`, résolu en <root>/datasets/<nom>.json) ou le chemin d'un
    /// manifeste (relatif à --root s'il est relatif). Exclusif avec --fixture.
    #[arg(long, value_name = "NOM|JSON")]
    pub dataset: Option<PathBuf>,
    /// Racine de travail pour --dataset (contient data/ et datasets/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
    /// Identifiant du registre (sma_cross).
    #[arg(long, value_name = "ID")]
    pub strategy: String,
    /// Fichier TOML des paramètres de la stratégie.
    #[arg(long, value_name = "TOML")]
    pub params: PathBuf,
    /// Grille de frais (section de --fees-file) : `okx_regular` (alias `okx`) ou `bitvavo_usdc`.
    #[arg(long, value_name = "GRILLE", default_value = "okx")]
    pub fees: FeeGridId,
    /// Fichier des grilles de frais, relatif au dossier courant.
    #[arg(long, value_name = "TOML", default_value = "config/fees.toml")]
    pub fees_file: PathBuf,
    /// Contraintes d'instrument (tick, lot, minimums) de l'exchange et du symbole des données, relatif
    /// au dossier courant.
    #[arg(long, value_name = "TOML", default_value = "config/instruments.toml")]
    pub instruments_file: PathBuf,
    /// Slippage en points de base (nombre fini dans [0, 10 000[), 5 par défaut. Refusé avec --costs.
    #[arg(long, value_name = "BPS", allow_negative_numbers = true)]
    pub slippage_bps: Option<Slippage>,
    /// Capital initial, dans la monnaie de cotation des données.
    #[arg(
        long,
        value_name = "MONTANT",
        default_value_t = 300.0,
        allow_negative_numbers = true
    )]
    pub capital: f64,
    /// Nombre minimal de trades en dessous duquel le verdict est `rejected_too_few_trades`.
    #[arg(
        long,
        value_name = "N",
        default_value_t = DEFAULT_MIN_TRADES,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub min_trades: u32,
}

impl RunInputArgs {
    /// Slippage scalaire du chemin hérité : celui de `--slippage-bps`, sinon [`DEFAULT_SLIPPAGE_BPS`].
    pub(super) fn legacy_slippage(&self) -> Result<Slippage, CliError> {
        match self.slippage_bps {
            Some(slippage) => Ok(slippage),
            None => Ok(Slippage::from_bps(DEFAULT_SLIPPAGE_BPS)?),
        }
    }
}

/// Arguments de `tv4 backtest`.
#[derive(Debug, Args)]
pub struct BacktestArgs {
    #[command(flatten)]
    pub inputs: RunInputArgs,
    /// Dossier de sortie (créé au besoin) ; défaut `<root>/runs/<run_id>`.
    #[arg(long, value_name = "DOSSIER")]
    pub out: Option<PathBuf>,
    /// Pas d'échantillonnage de `equity.csv` (1m, 5m, 15m, 30m, 1h, 4h, 12h, 1d).
    #[arg(long, value_name = "TF", default_value = "1h")]
    pub equity_step: Timeframe,
    /// Options du mode portefeuille (S9) : `--symbols` l'active.
    #[command(flatten)]
    pub portfolio: PortfolioArgs,
    /// Options des profils de coûts (S10) : `--costs` les active, exige `--symbols`.
    #[command(flatten)]
    pub costs: CostArgs,
}

/// Run préparé : données lues et vérifiées, paramètres lus, tout ce qu'il faut pour rejouer.
pub(super) struct PreparedRun {
    pub candles: Vec<Candle>,
    /// Symbole canonique des données.
    pub symbol: String,
    pub data: DataInfo,
    /// `blake3:<hex>` (`dsh-v1`).
    pub dataset_hash: String,
    pub hash_format: String,
    /// BLAKE3 hexadécimal nu des octets du CSV, en fixture seulement.
    pub fixture_blake3: Option<String>,
    pub fees: FeeSchedule,
    pub instrument: InstrumentRules,
    pub slippage: Slippage,
    pub capital: f64,
    pub min_trades: u32,
    strategy_id: String,
    params: toml::Value,
}

impl PreparedRun {
    /// Une stratégie fraîche, construite par le registre.
    pub fn build_strategy(&self) -> Result<Box<dyn Strategy>, CliError> {
        Ok(registry::build(&self.strategy_id, &self.params)?)
    }

    /// Configuration de cellule (sans la stratégie).
    pub fn cell_config(&self) -> CellConfig<'_> {
        CellConfig {
            symbol: &self.symbol,
            fees: self.fees,
            slippage: self.slippage,
            initial_cash: self.capital,
            sim_rules: SimRules::default(),
            instrument: self.instrument,
        }
    }
}

/// Lit les données (fixture ou dataset vérifié), les frais, l'instrument et les paramètres.
pub(super) fn prepare(args: &RunInputArgs) -> Result<PreparedRun, CliError> {
    let input = match (&args.fixture, &args.dataset) {
        (Some(fixture), None) => load_fixture(fixture)?,
        (None, Some(dataset)) => load_dataset(&args.root, dataset)?,
        _ => return Err(CliError::DataSource),
    };

    let fees = FeeSchedule::load_grid(args.fees, &args.fees_file)?;
    let instrument = InstrumentRules::load(input.exchange, &input.symbol, &args.instruments_file)?;

    let params_text = fs::read_to_string(&args.params).map_err(|source| CliError::Read {
        path: args.params.display().to_string(),
        source,
    })?;
    let params =
        toml::from_str::<toml::Value>(&params_text).map_err(|source| CliError::ParamsToml {
            path: args.params.display().to_string(),
            source,
        })?;
    // Construction d'essai : une stratégie inconnue ou des paramètres invalides échouent avant tout calcul.
    registry::build(&args.strategy, &params)?;

    Ok(PreparedRun {
        candles: input.candles,
        symbol: input.symbol,
        data: input.data,
        dataset_hash: input.dataset_hash,
        hash_format: input.hash_format,
        fixture_blake3: input.fixture_blake3,
        fees,
        instrument,
        slippage: args.legacy_slippage()?,
        capital: args.capital,
        min_trades: args.min_trades,
        strategy_id: args.strategy.clone(),
        params,
    })
}

/// Bougies à rejouer et description des données.
struct Input {
    candles: Vec<Candle>,
    exchange: ExchangeId,
    symbol: String,
    data: DataInfo,
    dataset_hash: String,
    hash_format: String,
    fixture_blake3: Option<String>,
}

/// `tv4 backtest` : prépare le run, rejoue la stratégie, écrit `metrics.json`, `trades.csv`,
/// `equity.csv` puis `manifest.json`.
pub(super) fn run(args: &BacktestArgs) -> Result<(), CliError> {
    // Les refus d'usage de S10 viennent avant toute lecture de fichier.
    args.costs.check(&args.portfolio, &args.inputs)?;
    if args.costs.is_costs() {
        return profiles::run(args);
    }
    if args.portfolio.is_portfolio() {
        return portfolio::run(args);
    }
    args.portfolio.reject_without_symbols()?;
    let t0 = Instant::now();
    let started_at_ms = now_ms()?;

    let prepared = prepare(&args.inputs)?;
    let strategy = prepared.build_strategy()?;
    let config = prepared.cell_config();
    let load_ms = elapsed_ms(t0);

    let t_run = Instant::now();
    let result = engine::run_cell(&prepared.candles, strategy, &config)?
        .with_min_trades(prepared.min_trades)?;
    let run_ms = elapsed_ms(t_run);

    let window_candles = prepared
        .candles
        .get(result.start_index..)
        .ok_or(report::ReportError::NoCandles)?;
    let open_times: Vec<Ts> = window_candles.iter().map(|c| c.open_time_ms).collect();
    let metrics = Metrics::new(
        &result,
        &open_times,
        prepared.data.clone(),
        prepared.dataset_hash.clone(),
    )?;
    let context = TradesContext {
        symbol: &prepared.symbol,
        span_ms: metrics.window.span_ms(),
    };
    let metrics_bytes = report::json_bytes(&metrics)?;
    let trades_bytes = report::writers::trades_csv(&result.trades, &context)?;
    let equity_bytes = report::equity::equity_csv(
        &open_times,
        &result.equity,
        result.config.initial_cash,
        args.equity_step,
    )?;
    let files = OutputFiles {
        equity_csv: hash::blake3_prefixed(&equity_bytes),
        metrics_json: hash::blake3_prefixed(&metrics_bytes),
        trades_csv: hash::blake3_prefixed(&trades_bytes),
    };

    let end_ms = metrics.window.last_open_time_ms + MINUTE_MS;
    let run_id = report::run_id(
        &metrics.config_hash,
        &metrics.dataset_hash,
        metrics.window.first_open_time_ms,
        end_ms,
    );
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| args.inputs.root.join("runs").join(&run_id));

    // Un dossier de sortie existant (même `run_id` sous un autre commit) ne doit pas garder l'ancien
    // manifeste, qui marque un run complet, pendant que les autres fichiers sont réécrits : une
    // interruption laisserait des fichiers du nouveau run sous le manifeste de l'ancien. Le manifeste
    // est réécrit en dernier ; le rapport HTML d'un rendu précédent est périmé lui aussi.
    for stale in [MANIFEST_FILE, REPORT_FILE] {
        remove_stale(&out, stale)?;
    }
    report::write_bytes_atomic(&out, "metrics.json", &metrics_bytes)?;
    report::write_bytes_atomic(&out, report::writers::TRADES_FILE, &trades_bytes)?;
    report::write_bytes_atomic(&out, report::equity::EQUITY_FILE, &equity_bytes)?;
    let cli_cold_ms = elapsed_ms(t0);

    // Hash du binaire après l'arrêt du chronomètre ; un run sans hash de binaire n'est pas un résultat
    // valide : il échoue avant le manifeste.
    let engine_info = EngineInfo::current(binary_hash()?);
    let manifest = Manifest::new(
        &metrics,
        ManifestInputs {
            hash_format: prepared.hash_format.clone(),
            fixture_blake3: prepared.fixture_blake3.clone(),
            engine: engine_info,
            equity_step: args.equity_step,
            files,
            started_at_ms,
            timings: Timings {
                load_ms,
                run_ms,
                cli_cold_ms,
            },
        },
    )?;
    debug_assert_eq!(manifest.run_id, run_id);
    report::write_json_atomic(&out, MANIFEST_FILE, &manifest)?;
    println!(
        "run {run_id} écrit dans {} : {} trades, PnL net {:.2}, verdict {}",
        out.display(),
        result.trades.len(),
        result.pnl_net,
        metrics.verdict
    );
    Ok(())
}

/// Supprime `dir/name` s'il existe ; une absence (fichier ou dossier) n'est pas une erreur.
fn remove_stale(dir: &Path, name: &str) -> Result<(), report::ReportError> {
    let path = dir.join(name);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(report::ReportError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// Millisecondes écoulées depuis `since`, saturées.
fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `blake3:<hex>` de l'exécutable en cours, lu en flux.
fn binary_hash() -> Result<String, CliError> {
    let path = std::env::current_exe().map_err(|source| CliError::BinaryHash {
        path: "<exécutable courant>".to_string(),
        source,
    })?;
    let fail = |source| CliError::BinaryHash {
        path: path.display().to_string(),
        source,
    };
    let mut file = File::open(&path).map_err(fail)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; BINARY_HASH_BUFFER];
    loop {
        let read = file.read(&mut buffer).map_err(fail)?;
        if read == 0 {
            break;
        }
        if let Some(chunk) = buffer.get(..read) {
            hasher.update(chunk);
        }
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// Chemin S3 : CSV Kraken 1m lu en entier, hash de ses octets. Le `dataset_hash` (`dsh-v1`) est celui
/// d'un segment unique (kraken, BTC-USD, 1m, fenêtre des bougies lues, politique `skip`) : il diffère
/// de celui d'un dataset construit sur les mêmes bougies avec une autre fenêtre.
fn load_fixture(fixture: &Path) -> Result<Input, CliError> {
    let bytes = fs::read(fixture).map_err(|source| CliError::Read {
        path: fixture.display().to_string(),
        source,
    })?;
    let fixture_blake3 = hash::blake3_hex(&bytes);
    let candles = kraken::read_all(bytes.as_slice(), Timeframe::M1)?;
    let data = DataInfo::fixture(FIXTURE_EXCHANGE, FIXTURE_SYMBOL, &candles)?;
    let window = Window::new(
        data.first_open_time_ms,
        data.last_open_time_ms.saturating_add(MINUTE_MS),
    )?;
    let columns = Columns::from_candles(&candles);
    let (hex, _rows) = dsh::dataset_hash(
        FIXTURE_EXCHANGE.as_str(),
        window,
        GapsPolicy::Skip,
        &[(FIXTURE_SYMBOL, Timeframe::M1, &columns)],
    )?;
    Ok(Input {
        candles,
        exchange: FIXTURE_EXCHANGE,
        symbol: FIXTURE_SYMBOL.to_string(),
        data,
        dataset_hash: dsh::prefixed(&hex),
        hash_format: dsh::HASH_FORMAT.to_string(),
        fixture_blake3: Some(fixture_blake3),
    })
}

/// Vrai si la valeur de `--dataset` est un nom (ni séparateur de chemin, ni suffixe `.json`).
fn is_bare_name(value: &str) -> bool {
    !value.contains('/') && !value.contains('\\') && !value.ends_with(".json")
}

/// Nom de dataset valide : `[a-z0-9_]{1,64}`, règle de `tv4 dataset build`.
fn is_valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Chemin du manifeste désigné par `--dataset`. Un nom nu se résout en `<root>/datasets/<nom>.json` ;
/// sinon un chemin, où un chemin absolu remplace la racine (comportement de `Path::join`).
pub(super) fn resolve_dataset_path(root: &Path, dataset: &Path) -> Result<PathBuf, CliError> {
    let shown = dataset.display().to_string();
    if is_bare_name(&shown) {
        if !is_valid_name(&shown) {
            return Err(store::StoreError::InvalidDatasetName(shown).into());
        }
        Ok(root.join("datasets").join(format!("{shown}.json")))
    } else {
        Ok(root.join(dataset))
    }
}

/// Chemin S4 : dataset figé. Le store recalcule le `content_hash` de chaque fichier listé puis le
/// `dataset_hash` et refuse de démarrer au moindre écart, en nommant le fichier.
fn load_dataset(root: &Path, dataset: &Path) -> Result<Input, CliError> {
    // Les messages d'erreur citent le chemin tel que l'utilisateur l'a passé à --dataset.
    let shown = dataset.display().to_string();
    let path = resolve_dataset_path(root, dataset)?;
    let manifest = store::read_dataset_manifest(&path)?;
    // Un manifeste sans symbole ou sans timeframe (ou avec plusieurs) n'est pas rejouable : message
    // de série, pas de repli silencieux sur le 1m.
    let ([symbol], [tf]) = (manifest.symbols.as_slice(), manifest.tfs.as_slice()) else {
        return Err(store::StoreError::MultiSeriesUnsupported {
            symbols: manifest.symbols.len(),
            tfs: manifest.tfs.len(),
        }
        .into());
    };
    if *tf != Timeframe::M1 {
        return Err(CliError::DatasetTimeframe {
            path: shown,
            tf: *tf,
        });
    }
    let symbol = symbol.as_str().to_string();

    let loaded = store::load_dataset(root, &path)?;
    let candles = loaded.columns.to_candles();
    let reference = DatasetRef {
        name: loaded.manifest.name.clone(),
        gaps_policy: loaded.manifest.gaps_policy.as_str().to_string(),
        synthetic_bars: loaded.columns.synthetic_count(),
    };
    let data = DataInfo::dataset(
        reference,
        manifest.exchange,
        &symbol,
        Timeframe::M1,
        &candles,
    )?;
    Ok(Input {
        candles,
        exchange: manifest.exchange,
        symbol,
        data,
        dataset_hash: loaded.manifest.dataset_hash.clone(),
        hash_format: loaded.manifest.hash_format.clone(),
        fixture_blake3: None,
    })
}
