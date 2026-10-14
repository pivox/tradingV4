//! Ligne de commande `tv4` (clap, dérivation). Aucune logique métier ici : chaque sous-commande est
//! une fonction qui appelle les autres modules (docs/03-architecture.md §2).
//!
//! Au socle de S1, toutes les sous-commandes sont vides : elles sortent en erreur en nommant l'étape
//! du jalon 1 qui les implémentera (docs/03 §11). `grid` arrive au jalon 1.5.

use clap::{Parser, Subcommand};

mod backtest;
mod bench;
mod dataset;
mod ingest;
mod verify;

/// Erreurs de la couche CLI.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// La sous-commande existe dans l'interface mais n'est pas encore codée.
    #[error(
        "la sous-commande « {command} » n'est pas encore implémentée : elle sera écrite à l'étape {step} du jalon 1 (docs/03-architecture.md §11)"
    )]
    NotImplemented {
        /// Nom complet tel que tapé par l'utilisateur, par exemple `ingest okx`.
        command: &'static str,
        /// Étape du jalon 1 qui l'implémentera, de `S2` à `S8`.
        step: &'static str,
    },
}

impl CliError {
    /// Raccourci pour les sous-commandes vides.
    pub(crate) fn not_implemented(command: &'static str, step: &'static str) -> Self {
        CliError::NotImplemented { command, step }
    }
}

/// Arguments de la ligne de commande `tv4`.
#[derive(Debug, Parser)]
#[command(
    name = "tv4",
    version = crate::LONG_VERSION,
    about = "tradingV4 : moteur de backtest crypto spot",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Sous-commande à exécuter.
    #[command(subcommand)]
    pub command: Command,
}

/// Sous-commandes de `tv4` (docs/03 §2, module `cli`).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Ingère des bougies depuis une source de marché.
    Ingest {
        /// Source des bougies.
        #[command(subcommand)]
        source: IngestSource,
    },
    /// Opérations sur les datasets figés.
    Dataset {
        /// Opération demandée.
        #[command(subcommand)]
        action: DatasetAction,
    },
    /// Lance un backtest.
    Backtest,
    /// Compare deux runs à l'octet.
    Verify,
    /// Mesure le coût moyen par bougie.
    Bench,
}

/// Sources d'ingestion.
#[derive(Debug, Subcommand)]
pub enum IngestSource {
    /// Dumps OHLCVT Kraken.
    Kraken,
    /// API REST OKX `history-candles`.
    Okx,
}

/// Opérations sur les datasets.
#[derive(Debug, Subcommand)]
pub enum DatasetAction {
    /// Construit un dataset figé (manifeste et `dataset_hash`).
    Build,
}

/// Exécute la sous-commande demandée.
pub fn run(command: &Command) -> Result<(), CliError> {
    match command {
        Command::Ingest { source } => match source {
            IngestSource::Kraken => ingest::kraken(),
            IngestSource::Okx => ingest::okx(),
        },
        Command::Dataset { action } => match action {
            DatasetAction::Build => dataset::build(),
        },
        Command::Backtest => backtest::run(),
        Command::Verify => verify::run(),
        Command::Bench => bench::run(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Chaque sous-commande vide, la commande tapée et l'étape annoncée.
    const EMPTY_COMMANDS: [(&[&str], &str, &str); 6] = [
        (&["tv4", "ingest", "kraken"], "ingest kraken", "S5"),
        (&["tv4", "ingest", "okx"], "ingest okx", "S7"),
        (&["tv4", "dataset", "build"], "dataset build", "S4"),
        (&["tv4", "backtest"], "backtest", "S3"),
        (&["tv4", "verify"], "verify", "S8"),
        (&["tv4", "bench"], "bench", "S8"),
    ];

    #[test]
    fn la_definition_clap_est_coherente() {
        Cli::command().debug_assert();
    }

    #[test]
    fn chaque_sous_commande_vide_nomme_son_etape() {
        for (args, command, step) in EMPTY_COMMANDS {
            let cli = Cli::try_parse_from(args).expect("la ligne de commande doit être valide");
            let message = match run(&cli.command) {
                Ok(()) => panic!("« {command} » devrait échouer tant qu'elle est vide"),
                Err(error) => error.to_string(),
            };
            assert!(message.contains(command), "{message}");
            assert!(message.contains(step), "{message}");
            assert!(
                message.contains("n'est pas encore implémentée"),
                "{message}"
            );
        }
    }

    #[test]
    fn grid_n_existe_pas_encore() {
        assert!(Cli::try_parse_from(["tv4", "grid"]).is_err());
    }

    #[test]
    fn sans_argument_clap_affiche_l_aide() {
        assert!(Cli::try_parse_from(["tv4"]).is_err());
    }
}
