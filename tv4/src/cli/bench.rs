//! Sous-commande `tv4 bench` (vide au socle de S1).

use super::CliError;

/// `tv4 bench` : coût moyen par `on_bar`, écrit à l'étape S8.
pub(super) fn run() -> Result<(), CliError> {
    Err(CliError::not_implemented("bench", "S8"))
}
