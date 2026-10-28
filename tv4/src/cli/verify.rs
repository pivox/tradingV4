//! Sous-commande `tv4 verify` (S8) : compare deux dossiers de run à l'octet.
//!
//! Le compte rendu vient de [`crate::report::verify`] ; la CLI l'imprime sur la sortie standard et
//! traduit le verdict en code de sortie : 0 identiques, 3 différents ([`CliError::VerifyMismatch`]),
//! 1 pour un run illisible ou incohérent.

use std::path::PathBuf;

use clap::Args;

use super::CliError;
use crate::report::verify;

/// Arguments de `tv4 verify`.
#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Premier dossier de run (celui de `tv4 backtest --out`).
    #[arg(value_name = "RUN_A")]
    pub run_a: PathBuf,
    /// Second dossier de run.
    #[arg(value_name = "RUN_B")]
    pub run_b: PathBuf,
}

/// `tv4 verify <RUN_A> <RUN_B>`.
pub(super) fn run(args: &VerifyArgs) -> Result<(), CliError> {
    let report = verify::verify(&args.run_a, &args.run_b)?;
    for line in &report.lines {
        println!("{line}");
    }
    if report.identical() {
        Ok(())
    } else {
        Err(CliError::VerifyMismatch {
            files: report.different_files,
        })
    }
}
