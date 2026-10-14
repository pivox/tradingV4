//! Sous-commande `tv4 backtest` (vide au socle de S1).

use super::CliError;

/// `tv4 backtest` : première version sur fixture à l'étape S3, version complète à l'étape S8.
pub(super) fn run() -> Result<(), CliError> {
    Err(CliError::not_implemented("backtest", "S3"))
}
