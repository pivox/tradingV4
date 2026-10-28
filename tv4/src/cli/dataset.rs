//! Sous-commandes `tv4 dataset ...` : `build` depuis S4 (multi-séries avec `--symbol` répété depuis S9).

use std::path::PathBuf;

use clap::Args;

use super::{CliError, now_ms};
use crate::model::{ExchangeId, Timeframe};
use crate::store::{
    self, BuildRequest, BuildStatus, GapsPolicy, MultiBuildRequest, Partition, Symbol, UtcMs,
    Window,
};

/// Arguments de `tv4 dataset build`.
#[derive(Debug, Args)]
pub struct DatasetBuildArgs {
    /// Racine de travail (contient data/, datasets/, state/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
    /// Nom du dataset : datasets/<nom>.json ([a-z0-9_], 1 à 64 caractères).
    #[arg(long, value_name = "NOM")]
    pub name: String,
    /// Exchange canonique des données (kraken, okx, bitvavo_usdc).
    #[arg(long, value_name = "EXCHANGE")]
    pub exchange: ExchangeId,
    /// Symbole canonique BASE-QUOTE ; l'option se répète pour un dataset multi-séries (S9), par
    /// exemple `--symbol ETH-USD --symbol BTC-USD`. L'ordre est sans effet sur le manifeste.
    #[arg(long = "symbol", value_name = "SYMBOLE", required = true)]
    pub symbols: Vec<Symbol>,
    /// Timeframe des bougies (1m, 5m, ..., 1d).
    #[arg(long, value_name = "TF")]
    pub tf: Timeframe,
    /// Début inclus, RFC 3339 UTC (2024-01-01T00:00:00Z).
    #[arg(long, value_name = "RFC3339")]
    pub from: UtcMs,
    /// Fin exclue, RFC 3339 UTC.
    #[arg(long, value_name = "RFC3339")]
    pub to: UtcMs,
    /// Politique de trous : skip (défaut), ffill, zero_volume_marked.
    #[arg(long, value_name = "POLITIQUE", default_value = "skip")]
    pub gaps_policy: GapsPolicy,
}

/// `tv4 dataset build` : manifeste et `dataset_hash` d'un dataset figé.
pub(super) fn build(args: &DatasetBuildArgs) -> Result<(), CliError> {
    let window = Window::new(args.from.0, args.to.0)?;
    // Un seul symbole : chemin mono-série inchangé du jalon 1. Plusieurs : dataset multi-séries.
    let outcome = match args.symbols.as_slice() {
        [symbol] => {
            let request = BuildRequest {
                name: &args.name,
                partition: Partition::new(args.exchange, symbol.clone(), args.tf),
                window,
                gaps_policy: args.gaps_policy,
            };
            store::build_dataset(&args.root, &request, now_ms()?)?
        }
        symbols => {
            let request = MultiBuildRequest {
                name: &args.name,
                exchange: args.exchange,
                symbols,
                tf: args.tf,
                window,
                gaps_policy: args.gaps_policy,
            };
            store::build_dataset_multi(&args.root, &request, now_ms()?)?
        }
    };
    let statut = match outcome.status {
        BuildStatus::Created => "créé",
        BuildStatus::Unchanged => "inchangé",
    };
    println!(
        "{} : {statut}, {} bougies, {} fichier(s) dont {} snapshot(s), dataset_hash {}",
        outcome.manifest_rel,
        outcome.manifest.row_count,
        outcome.manifest.files.len(),
        outcome.snapshots,
        outcome.manifest.dataset_hash
    );
    Ok(())
}
