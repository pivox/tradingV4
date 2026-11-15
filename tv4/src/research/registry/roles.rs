//! Rôle persistant dérivé de la famille S12, sans nouvelle colonne CSV.
use super::{ConfigSpec, Digest, HypothesisRef, RegistryError, invalid};

pub const NULL_FAMILY_PREFIX: &str = "__tv4_null_v1__";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialRole {
    Candidate,
    NullControl,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoleCounts {
    pub candidates: usize,
    pub null_controls: usize,
}

pub fn role_from_family(family: &str) -> Result<TrialRole, RegistryError> {
    if !family.starts_with(NULL_FAMILY_PREFIX) {
        return Ok(TrialRole::Candidate);
    }
    let parts = family
        .strip_prefix("__tv4_null_v1__:")
        .ok_or_else(|| invalid("famille nulle réservée mal formée"))?
        .split(':')
        .collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|v| !valid_component(v)) {
        return Err(invalid("identité de stratégie nulle invalide"));
    }
    Ok(TrialRole::NullControl)
}
fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
pub(crate) fn candidate_family(family: &str) -> Result<(), RegistryError> {
    if role_from_family(family)? != TrialRole::Candidate {
        return Err(invalid("famille réservée aux contrôles internes"));
    }
    Ok(())
}
impl ConfigSpec {
    /// L'appelant interne fournit l'identité vérifiée du vrai parent, jamais le nom d'une fiche.
    #[allow(dead_code, reason = "raccord interne de la CLI S14 au lot suivant")]
    pub(crate) fn null_control(
        config_hash: Digest,
        strategy_id: &str,
        strategy_version: &str,
        hypothesis: Option<HypothesisRef>,
    ) -> Result<Self, RegistryError> {
        if !valid_component(strategy_id) || !valid_component(strategy_version) {
            return Err(invalid(
                "identité de stratégie ambiguë pour la famille nulle",
            ));
        }
        Ok(Self {
            config_hash,
            family: format!("{NULL_FAMILY_PREFIX}:{strategy_id}:{strategy_version}"),
            hypothesis,
        })
    }
}
