//! Recettes préenregistrées : chaque lecture est possédée et recoupée avec HEAD.
use super::{error, usage};
use crate::{
    cli::{CliError, research},
    model::Timeframe,
    report::controls::{
        ControlsOptions, ControlsWindow,
        identity::{ControlLine, ControlProfile},
    },
    research::controls::ControlOptions,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    pub file: String,
    pub hash: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parent {
    pub config_hash: String,
    pub params: FileRef,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Costs {
    pub file: FileRef,
    pub measures: Option<FileRef>,
    pub cost_seed: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Universe {
    pub file: FileRef,
    pub files: Vec<FileRef>,
}
fn k_default() -> u32 {
    200
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    seed: String,
    #[serde(default = "k_default")]
    replications: u32,
    modes: Vec<crate::research::controls::ControlMode>,
    budgets: crate::research::controls::Budgets,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    pub parents: Vec<Parent>,
    pub dataset_manifest: FileRef,
    pub instruments: FileRef,
    pub instruments_format: String,
    pub fees: FileRef,
    pub fees_grid: String,
    pub costs: Option<Costs>,
    pub universe: Option<Universe>,
    pub subset: Option<FileRef>,
    pub line: Option<ControlLine>,
    pub profile: Option<ControlProfile>,
    pub window: ControlsWindow,
    pub initial_cash: f64,
    pub symbols: Vec<String>,
    pub regime_symbol: String,
    pub max_positions: u16,
    pub slippage_bps: f64,
    pub entry_delay_bars: u32,
    pub equity_step: Timeframe,
    pub min_trades: u32,
    pub technical_nonqualifying: bool,
    options: Options,
}
pub struct LoadedPlan {
    pub plan: Plan,
    pub root: PathBuf,
    pub base: PathBuf,
    pub bytes: BTreeMap<String, Vec<u8>>,
    pub options: ControlOptions,
}
fn relative(value: &str) -> Result<(), CliError> {
    if value.is_empty()
        || value.contains('\\')
        || Path::new(value)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(usage("chemin de recette relatif sans traversée requis"));
    }
    Ok(())
}
fn committed(root: &Path, path: &Path) -> Result<Vec<u8>, CliError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| usage("recette hors du checkout du plan"))?
        .to_str()
        .ok_or_else(|| usage("chemin non UTF-8"))?;
    relative(rel)?;
    let safe = research::safe_path(root, rel)?;
    let bytes = fs::read(&safe).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            usage(format!("plan ou recette absent : {}", safe.display()))
        } else {
            error(format!("{} : {e}", safe.display()))
        }
    })?;
    if research::hypothesis::blob(root, "HEAD", rel, "plan ou recette non commité dans HEAD")?
        != bytes
    {
        return Err(usage("plan ou recette modifié depuis HEAD"));
    }
    Ok(bytes)
}
impl LoadedPlan {
    pub fn load(path: &Path, seed: u64) -> Result<Self, CliError> {
        // Refuser les liens avant canonicalisation.
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(error)?.join(path)
        };
        let base = absolute
            .parent()
            .ok_or_else(|| usage("parent du plan absent"))?
            .to_path_buf();
        let root = research::resolve_root(Some(&base))?;
        let bytes = committed(&root, &absolute)?;
        let plan: Plan = serde_json::from_slice(&bytes)
            .map_err(|e| usage(format!("plan JSON invalide : {e}")))?;
        let parsed_seed = plan
            .options
            .seed
            .parse::<u64>()
            .map_err(|_| usage("seed du plan invalide"))?;
        if plan.schema != "tv4-controls-plan-v1"
            || parsed_seed != seed
            || parsed_seed.to_string() != plan.options.seed
        {
            return Err(usage("schéma ou graine explicite différents du plan"));
        }
        let mut options = ControlOptions {
            seed,
            replications: plan.options.replications,
            modes: plan.options.modes.clone(),
            budgets: plan.options.budgets,
        };
        options.modes.sort();
        if !(1..=1000).contains(&options.replications) {
            return Err(usage("K de la CLI v1 doit être dans 1..=1000"));
        }
        options.validate().map_err(usage)?;
        if !(1..=27).contains(&plan.parents.len())
            || plan
                .parents
                .windows(2)
                .any(|p| p[0].config_hash >= p[1].config_hash)
            || plan.min_trades == 0
            || !plan.initial_cash.is_finite()
            || plan.initial_cash <= 0.
            || plan.entry_delay_bars > 1
        {
            return Err(usage(
                "roster trié distinct, capital, effectif ou délai invalides",
            ));
        }
        if !(1..=crate::engine::MAX_POSITIONS_LIMIT).contains(&plan.max_positions) {
            return Err(usage("max_positions du plan hors limites"));
        }
        for p in &plan.parents {
            research::digest(&p.config_hash)?;
            if p.config_hash.starts_with("blake3:") {
                return Err(usage("hash parent nu requis"));
            }
        }
        if plan.profile.is_some() != plan.costs.is_some()
            || plan.line == Some(ControlLine::OkxUsdc) && plan.subset.is_none()
        {
            return Err(usage("profil/coûts ou ligne/sous-ensemble incohérents"));
        }
        let mut loaded = Self {
            plan,
            root,
            base,
            bytes: BTreeMap::new(),
            options,
        };
        let mut refs = vec![
            &loaded.plan.dataset_manifest,
            &loaded.plan.instruments,
            &loaded.plan.fees,
        ];
        refs.extend(loaded.plan.parents.iter().map(|p| &p.params));
        if let Some(c) = &loaded.plan.costs {
            refs.push(&c.file);
            refs.extend(c.measures.iter());
        }
        if let Some(u) = &loaded.plan.universe {
            refs.push(&u.file);
            refs.extend(&u.files);
        }
        refs.extend(loaded.plan.subset.iter());
        for reference in refs {
            relative(&reference.file)?;
            let bytes = committed(&loaded.root, &loaded.base.join(&reference.file))?;
            if crate::model::hash::blake3_prefixed(&bytes) != reference.hash {
                return Err(usage(format!(
                    "empreinte de recette différente : {}",
                    reference.file
                )));
            }
            if let Some(old) = loaded.bytes.insert(reference.file.clone(), bytes.clone())
                && old != bytes
            {
                return Err(usage("recette dupliquée incohérente"));
            }
        }
        Ok(loaded)
    }
    pub fn text(&self, file: &FileRef) -> Result<&str, CliError> {
        std::str::from_utf8(self.data(file)?).map_err(usage)
    }
    pub fn data(&self, file: &FileRef) -> Result<&[u8], CliError> {
        self.bytes
            .get(&file.file)
            .map(Vec::as_slice)
            .ok_or_else(|| usage("recette absente"))
    }
    pub fn report_options(&self) -> ControlsOptions {
        ControlsOptions::from(&self.options)
    }
}
