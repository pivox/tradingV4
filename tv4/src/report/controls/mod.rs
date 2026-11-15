//! Rapports S14 portables : identités pures, DTO possédés, publication et lecture vérifiée.
pub mod bundle;
pub mod identity;
pub mod types;
pub mod verify;
pub use bundle::{ControlsBundleBytes, PublishEvent, build_bundle, publish, publish_with};
pub use types::*;

#[derive(Debug, thiserror::Error)]
#[error("contrôles S14 : {0}")]
pub struct ControlsError(pub String);
pub(super) fn invalid(reason: impl Into<String>) -> ControlsError {
    ControlsError(reason.into())
}
