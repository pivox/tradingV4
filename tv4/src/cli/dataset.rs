//! Sous-commandes `tv4 dataset ...` (vides au socle de S1).

use super::CliError;

/// `tv4 dataset build` : manifeste et `dataset_hash`, écrit à l'étape S4.
pub(super) fn build() -> Result<(), CliError> {
    Err(CliError::not_implemented("dataset build", "S4"))
}
