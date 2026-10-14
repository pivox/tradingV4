//! Sous-commande `tv4 verify` (vide au socle de S1).

use super::CliError;

/// `tv4 verify` : comparaison de deux runs à l'octet, écrite à l'étape S8.
pub(super) fn run() -> Result<(), CliError> {
    Err(CliError::not_implemented("verify", "S8"))
}
