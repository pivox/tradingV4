//! Sous-commande `tv4 backtest` : backtest minimal sur une fixture CSV Kraken 1m (S3) ou sur un
//! dataset figé du store (S4).
//!
//! Aucune logique métier ici (docs/03-architecture.md §2) : lecture des fichiers, appel du registre,
//! de `store`, de `engine::run_cell` et de `report`.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;

use super::CliError;
use crate::engine::{self, CellConfig};
use crate::marketdata::kraken;
use crate::model::hash;
use crate::model::{Candle, ExchangeId, FeeSchedule, InstrumentRules, Slippage, Timeframe};
use crate::report::{self, DataInfo, DatasetRef, EngineInfo, Metrics};
use crate::sim::SimRules;
use crate::store;
use crate::strategy::registry;

/// Symbole canonique de la fixture (XBTUSD Kraken, docs/03 §4).
const FIXTURE_SYMBOL: &str = "BTC-USD";
/// Exchange de la fixture : un dump Kraken, dont `config/instruments.toml` donne tick et lot.
const FIXTURE_EXCHANGE: ExchangeId = ExchangeId::Kraken;

/// Arguments de `tv4 backtest`.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("donnees").required(true).args(["fixture", "dataset"])))]
pub struct BacktestArgs {
    /// CSV Kraken OHLCVT en 1m, sans en-tête (par exemple fixtures/kraken_xbtusd_1m.csv).
    /// Exclusif avec --dataset.
    #[arg(long, value_name = "CSV")]
    pub fixture: Option<PathBuf>,
    /// Manifeste de dataset figé (datasets/<nom>.json), relatif à --root s'il est relatif.
    /// Exclusif avec --fixture.
    #[arg(long, value_name = "JSON")]
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
    /// Grille de frais (section de --fees-file).
    #[arg(long, value_name = "EXCHANGE", default_value = "okx")]
    pub fees: ExchangeId,
    /// Fichier des grilles de frais, relatif au dossier courant.
    #[arg(long, value_name = "TOML", default_value = "config/fees.toml")]
    pub fees_file: PathBuf,
    /// Contraintes d'instrument (tick, lot, minimums) de l'exchange et du symbole des données, relatif
    /// au dossier courant.
    #[arg(long, value_name = "TOML", default_value = "config/instruments.toml")]
    pub instruments_file: PathBuf,
    /// Slippage en points de base (nombre fini dans [0, 10 000[).
    #[arg(
        long,
        value_name = "BPS",
        default_value = "5",
        allow_negative_numbers = true
    )]
    pub slippage_bps: Slippage,
    /// Capital initial, dans la monnaie de cotation des données.
    #[arg(
        long,
        value_name = "MONTANT",
        default_value_t = 300.0,
        allow_negative_numbers = true
    )]
    pub capital: f64,
    /// Dossier de sortie (créé au besoin) ; metrics.json et trades.csv y sont écrits.
    #[arg(long, value_name = "DOSSIER")]
    pub out: PathBuf,
}

/// Bougies à rejouer et description des données pour `metrics.json`.
struct Input {
    candles: Vec<Candle>,
    /// Exchange et symbole canonique des données : ils choisissent la ligne de `--instruments-file`.
    exchange: ExchangeId,
    symbol: String,
    data: DataInfo,
}

/// `tv4 backtest` : lit les données (fixture ou dataset vérifié) et les paramètres, rejoue la
/// stratégie, écrit `metrics.json`.
pub(super) fn run(args: &BacktestArgs) -> Result<(), CliError> {
    let input = match (&args.fixture, &args.dataset) {
        (Some(fixture), None) => load_fixture(fixture)?,
        (None, Some(dataset)) => load_dataset(&args.root, dataset)?,
        _ => return Err(CliError::DataSource),
    };

    let fees = FeeSchedule::load(args.fees, &args.fees_file)?;
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
    let strategy = registry::build(&args.strategy, &params)?;

    let config = CellConfig {
        symbol: &input.symbol,
        fees,
        slippage: args.slippage_bps,
        initial_cash: args.capital,
        sim_rules: SimRules::default(),
        instrument,
    };
    let result = engine::run_cell(&input.candles, strategy, &config)?;

    let engine_info = EngineInfo {
        tv4_version: env!("CARGO_PKG_VERSION"),
        git_sha: crate::GIT_SHA,
    };
    let metrics = Metrics::new(&result, input.data, engine_info);
    report::write_json_atomic(&args.out, "metrics.json", &metrics)?;
    report::write_trades_csv(&args.out, &result.trades)?;
    println!(
        "metrics.json et trades.csv écrits dans {} : {} trades, PnL net {:.2}",
        args.out.display(),
        result.trades.len(),
        result.pnl_net
    );
    Ok(())
}

/// Chemin S3, inchangé : CSV Kraken 1m lu en entier, hash de ses octets.
fn load_fixture(fixture: &Path) -> Result<Input, CliError> {
    let bytes = fs::read(fixture).map_err(|source| CliError::Read {
        path: fixture.display().to_string(),
        source,
    })?;
    let fixture_blake3 = hash::blake3_hex(&bytes);
    let candles = kraken::read_all(bytes.as_slice(), Timeframe::M1)?;
    let data = DataInfo::fixture(fixture_blake3, FIXTURE_SYMBOL, &candles)?;
    Ok(Input {
        candles,
        exchange: FIXTURE_EXCHANGE,
        symbol: FIXTURE_SYMBOL.to_string(),
        data,
    })
}

/// Chemin S4 : dataset figé. Le store recalcule le `content_hash` de chaque fichier listé puis le
/// `dataset_hash` et refuse de démarrer au moindre écart, en nommant le fichier.
fn load_dataset(root: &Path, dataset: &Path) -> Result<Input, CliError> {
    // Un chemin absolu remplace la racine (comportement de `Path::join`).
    let path = root.join(dataset);
    // Les messages d'erreur citent le chemin tel que l'utilisateur l'a passé à --dataset.
    let shown = dataset.display().to_string();
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
        dataset_hash: loaded.manifest.dataset_hash.clone(),
        hash_format: loaded.manifest.hash_format.clone(),
        gaps_policy: loaded.manifest.gaps_policy.as_str().to_string(),
        synthetic_bars: loaded.columns.synthetic_count(),
    };
    let data = DataInfo::dataset(reference, &symbol, Timeframe::M1, &candles)?;
    Ok(Input {
        candles,
        exchange: manifest.exchange,
        symbol,
        data,
    })
}
