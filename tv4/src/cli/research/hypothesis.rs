//! Contrôles Git/fiche/candidat sans ouverture des données de marché.
use super::{ResearchError, binary_hash, io, refused, safe_path};
use crate::research::registry::{Digest, HypothesisRef, Registry, Status};
use crate::store::time::parse_rfc3339_utc;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

const FIELDS: [&str; 18] = [
    "H-NNN",
    "Date / auteur",
    "Famille",
    "Hypothèse économique",
    "Règle d'entrée",
    "Règle de sortie",
    "Paramètres",
    "Timeframe",
    "Univers",
    "Profil de coûts",
    "Faisabilité G0",
    "n_min / μ_cible",
    "Signaux attendus",
    "Contrôle",
    "Règle d'abandon",
    "Connaissances antérieures",
    "Claim holdout",
    "dataset_hash",
];

/// Identité pré-enregistrée calculée sur les octets exacts de la fiche commitée.
#[derive(Clone, Debug)]
pub(crate) struct Hypothesis {
    pub(crate) reference: HypothesisRef,
    pub(crate) family: String,
    bytes: Vec<u8>,
}
/// Candidat vérifié ; le gel est la date committer du commit référencé par le tag.
#[derive(Clone, Debug)]
pub(crate) struct FrozenCandidate {
    pub(crate) config_hashes: Vec<Digest>,
    pub(crate) okx_subset_hash: String,
    pub(crate) tag: String,
    pub(crate) frozen_at_ms: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateFile {
    schema: String,
    hypothesis: String,
    fiche_sha256: String,
    config_hashes: Vec<String>,
    engine_commit: String,
    engine_binary_hash: String,
    okx_subset_hash: String,
}

/// Pas de shell, ni de remplacement Git d'objets : les arguments d'objet proviennent d'un H-NNN strict.
pub(super) fn git(root: &Path, args: &[&str], reason: &str) -> Result<Vec<u8>, ResearchError> {
    let output = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| io(root, e))?;
    if !output.status.success() {
        return Err(refused(reason));
    }
    Ok(output.stdout)
}
fn validate_id(id: &str) -> Result<(), ResearchError> {
    if id.len() != 5
        || !id.starts_with("H-")
        || !id.as_bytes()[2..].iter().all(u8::is_ascii_digit)
        || id == "H-000"
    {
        return Err(refused(
            "fiche exécutable attendue : H-NNN (trois chiffres), différente de H-000",
        ));
    }
    Ok(())
}
fn read_current(root: &Path, rel: &str, label: &str) -> Result<Vec<u8>, ResearchError> {
    let path = safe_path(root, rel)?;
    match fs::read(&path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(refused(format!("{label} absent : {rel}")))
        }
        Err(error) => Err(io(&path, error)),
    }
}
fn blob(root: &Path, commit: &str, rel: &str, reason: &str) -> Result<Vec<u8>, ResearchError> {
    git(root, &["show", &format!("{commit}:{rel}")], reason)
}

pub(crate) fn load_hypothesis(root: &Path, id: &str) -> Result<Hypothesis, ResearchError> {
    validate_id(id)?;
    let rel = format!("research/{id}.md");
    let bytes = read_current(root, &rel, "fiche")?;
    let committed = blob(root, "HEAD", &rel, "fiche non commitée dans HEAD")?;
    if committed != bytes {
        return Err(refused("fiche modifiée depuis son commit HEAD"));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| refused("fiche non UTF-8"))?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let canonical = if key.starts_with("H-") { "H-NNN" } else { key };
        if !FIELDS.contains(&canonical) {
            continue;
        }
        if fields.insert(canonical, value.trim()).is_some() {
            return Err(refused(format!("fiche : champ {canonical} dupliqué")));
        }
        if canonical == "H-NNN"
            && (key != "H-NNN" && key != id || key == "H-NNN" && value.trim() != id)
        {
            return Err(refused("fiche : identité différente du nom H-NNN"));
        }
    }
    for field in FIELDS {
        if fields.get(field).is_none_or(|value| value.is_empty()) {
            return Err(refused(format!("fiche : champ {field} absent ou vide")));
        }
    }
    let family = fields
        .get("Famille")
        .ok_or_else(|| refused("fiche : Famille absente"))?
        .to_string();
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let reference = HypothesisRef::new(id.into(), Digest::new(sha)?)?;
    Ok(Hypothesis {
        reference,
        family,
        bytes,
    })
}

pub(crate) fn load_frozen(
    root: &Path,
    fiche: &Hypothesis,
    registry: &Registry,
) -> Result<FrozenCandidate, ResearchError> {
    let id = fiche.reference.id();
    let rel = format!("research/frozen/{id}.json");
    let bytes = read_current(root, &rel, "candidat gelé")?;
    let candidate: CandidateFile = serde_json::from_slice(&bytes)
        .map_err(|e| refused(format!("candidat gelé JSON invalide : {e}")))?;
    if candidate.schema != "tv4-frozen-candidate-v1"
        || candidate.hypothesis != id
        || candidate.fiche_sha256 != fiche.reference.sha256().as_str()
    {
        return Err(refused(
            "candidat gelé : schéma, hypothèse ou hash de fiche incohérent",
        ));
    }
    if !matches!(candidate.config_hashes.len(), 1 | 3) {
        return Err(refused("candidat gelé : un ou trois config_hashes requis"));
    }
    let mut seen = BTreeSet::new();
    let mut config_hashes = Vec::new();
    for value in &candidate.config_hashes {
        let hash = Digest::new(value.clone())?;
        if !seen.insert(hash.clone()) {
            return Err(refused("candidat gelé : config_hash dupliqué"));
        }
        let trial = registry
            .trials()
            .get(&hash)
            .ok_or_else(|| refused("candidat gelé : config_hash inconnu du registre"))?;
        if trial.hypothesis.as_ref() != Some(&fiche.reference) || trial.family != fiche.family {
            return Err(refused(
                "candidat gelé : config_hash associé à une autre fiche ou famille",
            ));
        }
        config_hashes.push(hash);
    }
    for (label, value) in [
        ("binaire", &candidate.engine_binary_hash),
        ("sous-ensemble OKX", &candidate.okx_subset_hash),
    ] {
        let hash = value.strip_prefix("blake3:").ok_or_else(|| {
            refused(format!(
                "candidat gelé : hash {label} attendu avec préfixe blake3:"
            ))
        })?;
        Digest::new(hash)?;
    }
    if candidate.engine_commit != crate::GIT_SHA {
        return Err(refused(
            "candidat gelé : commit moteur différent du binaire courant",
        ));
    }
    if candidate.engine_binary_hash != binary_hash()? {
        return Err(refused(
            "candidat gelé : empreinte du binaire moteur différente",
        ));
    }
    if blob(root, "HEAD", &rel, "candidat gelé non commité dans HEAD")? != bytes {
        return Err(refused("candidat gelé modifié depuis HEAD"));
    }
    let tag = format!("frozen/{id}");
    let commit = git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
        "tag de gel absent ou ne désignant pas un commit",
    )?;
    let commit = std::str::from_utf8(&commit)
        .map_err(|_| refused("tag de gel invalide"))?
        .trim();
    let fiche_rel = format!("research/{id}.md");
    if blob(root, commit, &fiche_rel, "fiche absente du gel")? != fiche.bytes
        || blob(root, commit, &rel, "candidat absent du gel")? != bytes
    {
        return Err(refused(
            "gel différent des octets courants et HEAD de la fiche ou du candidat",
        ));
    }
    let seconds = git(
        root,
        &["show", "-s", "--format=%ct", commit],
        "date du gel introuvable",
    )?;
    let seconds = std::str::from_utf8(&seconds)
        .map_err(|_| refused("date de gel invalide"))?
        .trim()
        .parse::<i64>()
        .map_err(|_| refused("date de gel invalide"))?;
    let frozen_at_ms = seconds
        .checked_mul(1000)
        .ok_or_else(|| refused("date de gel hors limites"))?;
    // L'état courant ne suffit pas : l'inscription et l'événement qui établit l'association
    // doivent déjà exister à la date du gel. L'égalité est admise à la précision seconde de la CLI.
    for hash in &config_hashes {
        let mut registered = false;
        let mut associated = false;
        for event in registry.events().iter().filter(|event| {
            &event.config_hash == hash
                && matches!(event.status, Status::Registered | Status::HypothesisLinked)
        }) {
            let event_at_ms = parse_rfc3339_utc(event.date.as_str())
                .map_err(|_| refused("candidat gelé : date d'événement du registre invalide"))?;
            if event_at_ms <= frozen_at_ms {
                registered |= event.status == Status::Registered;
                associated |= event.hypothesis.as_ref() == Some(&fiche.reference)
                    && event.family == fiche.family;
            }
        }
        if !registered || !associated {
            return Err(refused(
                "candidat gelé : inscription ou association fiche/famille inexistante à la date du gel",
            ));
        }
    }
    Ok(FrozenCandidate {
        config_hashes,
        okx_subset_hash: candidate.okx_subset_hash,
        tag,
        frozen_at_ms,
    })
}
