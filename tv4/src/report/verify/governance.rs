//! Validation locale de la provenance S12, jamais comparée aux compteurs d'un autre run.
use crate::{model::Block, store::time::parse_rfc3339_utc};
use serde_json::Value;

fn invalid(path: &str, reason: &str) -> String {
    format!(
        "manifest.json : governance{} : {reason}",
        if path.is_empty() {
            String::new()
        } else {
            format!(".{path}")
        }
    )
}
fn field<'a>(g: &'a Value, path: &str) -> Result<&'a Value, String> {
    path.split('.').try_fold(g, |value, key| {
        value
            .get(key)
            .ok_or_else(|| invalid(path, "champ obligatoire absent"))
    })
}
fn string<'a>(g: &'a Value, path: &str) -> Result<&'a str, String> {
    field(g, path)?
        .as_str()
        .ok_or_else(|| invalid(path, "chaîne attendue"))
}
fn optional_string<'a>(g: &'a Value, path: &str) -> Result<Option<&'a str>, String> {
    let value = field(g, path)?;
    if value.is_null() {
        Ok(None)
    } else {
        string(g, path).map(Some)
    }
}
fn unsigned(g: &Value, path: &str) -> Result<u64, String> {
    field(g, path)?
        .as_u64()
        .ok_or_else(|| invalid(path, "entier positif ou nul attendu"))
}
fn optional_unsigned(g: &Value, path: &str) -> Result<Option<u64>, String> {
    if field(g, path)?.is_null() {
        Ok(None)
    } else {
        unsigned(g, path).map(Some)
    }
}
fn positive(value: u64, path: &str) -> Result<(), String> {
    if value == 0 {
        Err(invalid(path, "entier strictement positif attendu"))
    } else {
        Ok(())
    }
}
fn optional_block(g: &Value, path: &str) -> Result<Option<Block>, String> {
    optional_string(g, path)?
        .map(|text| {
            Block::ALL
                .into_iter()
                .find(|b| b.as_str() == text)
                .ok_or_else(|| invalid(path, "bloc canonique inconnu"))
        })
        .transpose()
}
fn hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}
fn check_date(text: &str) -> Result<(), String> {
    let error = || invalid("frozen_at", "date et heure UTC invalides");
    if !text.is_ascii() {
        return Err(error());
    }
    let core = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix("+00:00"))
        .ok_or_else(error)?;
    if core.len() < 19 {
        return Err(error());
    }
    if core.len() > 19 {
        let fraction = core
            .strip_prefix(&core[..19])
            .and_then(|v| v.strip_prefix('.'))
            .ok_or_else(error)?;
        if fraction.is_empty()
            || fraction.len() > 9
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(error());
        }
    }
    parse_rfc3339_utc(&format!("{}Z", &core[..19])).map_err(|_| error())?;
    Ok(())
}

pub(super) fn check(metrics: &Value, manifest: &Value) -> Result<(), String> {
    let research = metrics.get("research").filter(|r| !r.is_null());
    let guard = research
        .and_then(|r| r.get("guard"))
        .and_then(Value::as_str);
    if research.is_some() && !matches!(guard, Some("none" | "unguarded" | "guarded")) {
        return Err("metrics.json : research.guard absent ou inconnu".into());
    }
    let Some(g) = manifest.get("governance").filter(|g| !g.is_null()) else {
        return if guard == Some("guarded") {
            Err(invalid("", "contexte requis pour research.guard = guarded"))
        } else {
            Ok(())
        };
    };
    if !g.is_object() {
        return Err(invalid("", "objet attendu"));
    }
    if string(g, "schema")? != "tv4-governance-v1" {
        return Err(invalid("schema", "schéma inconnu"));
    }
    for key in ["config_hash", "dataset_hash"] {
        let hash = string(g, key)?;
        if !hex64(hash.strip_prefix("blake3:").unwrap_or(hash)) {
            return Err(invalid(key, "hash de 64 caractères hexadécimaux attendu"));
        }
        if manifest.get(key).and_then(Value::as_str) != Some(hash) {
            return Err(invalid(key, "différent de celui du manifeste"));
        }
    }
    for path in ["trial_id", "read_id", "noise.n"] {
        positive(unsigned(g, path)?, path)?;
    }
    let n = unsigned(g, "noise.n")?;
    if let Some(n_eff) = optional_unsigned(g, "noise.n_eff")? {
        positive(n_eff, "noise.n_eff")?;
        if n_eff > n {
            return Err(invalid("noise.n_eff", "ne peut pas dépasser N"));
        }
    }
    field(g, "noise.selection_years")?
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| {
            invalid(
                "noise.selection_years",
                "durée finie strictement positive attendue",
            )
        })?;
    // Le producteur a calculé les repères : conserver ses arrondis et ses absences. Le lecteur
    // vérifie les types et la finitude, sans réévaluer une formule approximative ni remplacer T.
    for path in ["noise.expected", "noise.approx_q90", "noise.q95"] {
        let value = field(g, path)?;
        if !value.is_null() && value.as_f64().is_none_or(|v| !v.is_finite()) {
            return Err(invalid(path, "nombre fini ou null attendu"));
        }
    }
    let hypothesis = field(g, "hypothesis")?;
    let id = if hypothesis.is_null() {
        None
    } else {
        let id = string(g, "hypothesis.identifiant")?;
        let suffix = id
            .strip_prefix("H-")
            .ok_or_else(|| invalid("hypothesis.identifiant", "fiche H-NNN exécutable attendue"))?;
        if suffix.len() != 3
            || !suffix.bytes().all(|b| b.is_ascii_digit())
            || suffix.bytes().all(|b| b == b'0')
        {
            return Err(invalid(
                "hypothesis.identifiant",
                "fiche H-NNN différente de H-000 attendue",
            ));
        }
        if !hex64(string(g, "hypothesis.sha256")?) {
            return Err(invalid(
                "hypothesis.sha256",
                "SHA-256 de 64 caractères hexadécimaux attendu",
            ));
        }
        Some(id)
    };
    let frozen_tag = optional_string(g, "frozen_tag")?;
    let frozen_at = optional_string(g, "frozen_at")?;
    if let Some(date) = frozen_at {
        check_date(date)?;
    }
    let source = optional_block(g, "source_block")?;
    let effective = optional_block(g, "effective_block")?;
    let generation = unsigned(g, "generation")?;
    if generation > u64::from(u32::MAX) {
        return Err(invalid("generation", "entier hors limites"));
    }
    let expected = match (source, generation) {
        (Some(Block::Holdout), 1..) => Some(Block::Validation),
        (Some(Block::Forward), 1..) => Some(Block::Holdout),
        _ => source,
    };
    if effective != expected {
        return Err(invalid(
            "effective_block",
            "incohérent avec source_block et generation",
        ));
    }
    if effective.map(Block::as_str)
        != research
            .and_then(|r| r.get("block"))
            .and_then(Value::as_str)
    {
        return Err(invalid(
            "effective_block",
            "différent de research.block des métriques",
        ));
    }
    let claim = optional_unsigned(g, "claim_holdout")?;
    if let Some(id) = claim {
        positive(id, "claim_holdout")?;
    }
    if (effective == Some(Block::Holdout)) != claim.is_some() {
        return Err(invalid(
            "claim_holdout",
            "requis uniquement pour le bloc effectif HOLDOUT",
        ));
    }
    if effective.is_some_and(Block::is_protected) {
        let id =
            id.ok_or_else(|| invalid("hypothesis", "fiche requise pour une lecture protégée"))?;
        if frozen_tag != Some(format!("frozen/{id}").as_str()) {
            return Err(invalid(
                "frozen_tag",
                "tag frozen/H-NNN correspondant à la fiche requis",
            ));
        }
        if frozen_at.is_none() {
            return Err(invalid("frozen_at", "date UTC de gel requise"));
        }
        if guard != Some("guarded") {
            return Err(
                "metrics.json : research.guard = guarded requis pour une lecture protégée".into(),
            );
        }
    }
    Ok(())
}
