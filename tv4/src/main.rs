//! Point d'entrée du binaire `tv4` : lit la ligne de commande, délègue à `tv4::cli`, traduit
//! l'erreur éventuelle en message sur stderr et en code de sortie non nul : 1 par défaut, 2 pour une
//! erreur d'usage (clap) ou une clé OKX refusée par `tv4 instruments fetch` (permission autre que
//! lecture, fichier de clé non sûr), 3 pour deux runs différents (`tv4 verify`) ; détail dans
//! [`tv4::cli::CliError::exit_code`].

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;
use tv4::cli::{Cli, run};

fn main() -> ExitCode {
    // clap gère lui-même `--help`, `--version` et les erreurs d'usage (code de sortie 2).
    let cli = Cli::parse();
    match run(&cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(erreur) => {
            eprintln!("erreur : {erreur}");
            ExitCode::from(erreur.exit_code())
        }
    }
}
