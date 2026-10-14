//! Sous-commandes `tv4 ingest ...` (vides au socle de S1).

use super::CliError;

/// `tv4 ingest kraken` : dumps OHLCVT Kraken, écrit à l'étape S5.
pub(super) fn kraken() -> Result<(), CliError> {
    Err(CliError::not_implemented("ingest kraken", "S5"))
}

/// `tv4 ingest okx` : API REST OKX `history-candles`, écrite à l'étape S7.
pub(super) fn okx() -> Result<(), CliError> {
    Err(CliError::not_implemented("ingest okx", "S7"))
}
