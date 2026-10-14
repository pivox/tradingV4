//! Sous-commandes `tv4 ingest ...` : `csv` depuis S4 ; `kraken` (S5) et `okx` (S7) restent vides.

use std::path::PathBuf;

use clap::Args;

use super::{CliError, now_ms};
use crate::marketdata::kraken::KrakenCsvReader;
use crate::model::{ExchangeId, Timeframe};
use crate::store::{self, Columns, Partition, Symbol, WriteStatus};

/// Source de provenance écrite dans les manifestes par `tv4 ingest csv`.
const SOURCE_KRAKEN_CSV: &str = "kraken-csv";

/// Arguments de `tv4 ingest csv`.
#[derive(Debug, Args)]
pub struct IngestCsvArgs {
    /// CSV au format OHLCVT Kraken, sans en-tête (timestamp en secondes,open,high,low,close,volume,trades).
    #[arg(long, value_name = "CSV")]
    pub csv: PathBuf,
    /// Exchange canonique des données (kraken, okx, bitvavo_usdc).
    #[arg(long, value_name = "EXCHANGE")]
    pub exchange: ExchangeId,
    /// Symbole canonique BASE-QUOTE (BTC-USD pour XBTUSD).
    #[arg(long, value_name = "SYMBOLE")]
    pub symbol: Symbol,
    /// Timeframe des bougies du CSV (1m, 5m, ..., 1d).
    #[arg(long, value_name = "TF")]
    pub tf: Timeframe,
    /// Racine de travail (contient data/, datasets/, state/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
}

/// `tv4 ingest kraken` : dumps OHLCVT Kraken, écrit à l'étape S5.
pub(super) fn kraken() -> Result<(), CliError> {
    Err(CliError::not_implemented("ingest kraken", "S5"))
}

/// `tv4 ingest okx` : API REST OKX `history-candles`, écrite à l'étape S7.
pub(super) fn okx() -> Result<(), CliError> {
    Err(CliError::not_implemented("ingest okx", "S7"))
}

/// `tv4 ingest csv` : lit un CSV Kraken (lecteur S2) et écrit les fichiers mensuels ou annuels du
/// store, avec verrou de partition et `_manifest.json`.
///
/// Le store refuse le désordre et les doublons (horodatages strictement croissants) ; la continuité
/// complète relève de S5.
pub(super) fn csv(args: &IngestCsvArgs) -> Result<(), CliError> {
    let candles = KrakenCsvReader::open(&args.csv, args.tf)?.collect::<Result<Vec<_>, _>>()?;
    let columns = Columns::from_candles(&candles);
    let partition = Partition::new(args.exchange, args.symbol.clone(), args.tf);
    let outcomes = store::write_partition(
        &args.root,
        &partition,
        &columns,
        SOURCE_KRAKEN_CSV,
        now_ms()?,
    )?;
    for outcome in &outcomes {
        let statut = match outcome.status {
            WriteStatus::Created => "créé",
            WriteStatus::Rewritten => "réécrit",
            WriteStatus::Unchanged => "inchangé",
        };
        let cloture = if outcome.closed {
            "période close"
        } else {
            "période non close"
        };
        println!(
            "{} : {} bougies, {statut} ({cloture}), content_hash {}",
            outcome.file_rel, outcome.row_count, outcome.content_hash
        );
    }
    Ok(())
}
