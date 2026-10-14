//! Point d'entrée du binaire `tv4` : lit la ligne de commande, délègue à `tv4::cli`, traduit
//! l'erreur éventuelle en message sur stderr et en code de sortie non nul.

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
            ExitCode::FAILURE
        }
    }
}
