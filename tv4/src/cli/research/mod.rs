//! Stockage et autorisations Git S12. Le CSV reste l'unique source d'état.
//!
//! Pour un backtest : résoudre `ResearchArgs`, ouvrir `Journal`, vérifier fiche et gel sous
//! verrou, réserver, puis laisser tomber le journal AVANT de charger les bougies.
pub(crate) mod admission;
pub(crate) mod hypothesis;
pub(crate) mod journal;

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::CliError;
use crate::research::registry::{Digest, RegistryError, UtcDate};
use clap::{Args, Subcommand};
use hypothesis::{FrozenCandidate, Hypothesis, git, load_frozen, load_hypothesis};
use journal::Journal;

/// Erreurs de politique (code 2) distinguées des erreurs d'écriture/lecture (code 1).
#[derive(Debug, thiserror::Error)]
pub enum ResearchError {
    #[error("recherche refusée : {0}")]
    Refused(String),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("recherche : accès à `{path}` impossible : {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
impl ResearchError {
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::Io { .. } => 1,
            _ => 2,
        }
    }
}
pub(crate) fn refused(reason: impl Into<String>) -> ResearchError {
    ResearchError::Refused(reason.into())
}
pub(crate) fn io(path: &Path, source: std::io::Error) -> ResearchError {
    ResearchError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Arguments communs à l'admission des futurs chemins backtest.
#[derive(Clone, Debug, Default, Args)]
pub(crate) struct ResearchArgs {
    /// Checkout Git du projet de recherche (par défaut : racine Git du répertoire courant).
    #[arg(long)]
    pub(crate) research_root: Option<PathBuf>,
    /// Fiche pré-enregistrée, par exemple H-001.
    #[arg(long)]
    pub(crate) hypothesis: Option<String>,
}
impl ResearchArgs {
    pub(crate) fn resolve_root(&self) -> Result<PathBuf, ResearchError> {
        resolve_root(self.research_root.as_deref())
    }
    /// La fiche doit être relue et vérifiée pendant la transaction qui l'utilise.
    pub(crate) fn load_hypothesis(&self, root: &Path) -> Result<Hypothesis, ResearchError> {
        let id = self
            .hypothesis
            .as_deref()
            .ok_or_else(|| refused("fiche --hypothesis obligatoire"))?;
        load_hypothesis(root, id)
    }
}

/// Gestion des compteurs de recherche ; aucune commande ne lit des bougies.
#[derive(Debug, Subcommand)]
pub enum ResearchAction {
    /// Affiche le registre et régénère sa projection README.
    Status {
        #[arg(long)]
        research_root: Option<PathBuf>,
    },
    /// Inscrit les claims HOLDOUT du candidat gelé.
    Claim {
        #[arg(long)]
        hypothesis: String,
        #[arg(long)]
        research_root: Option<PathBuf>,
    },
}

pub(super) fn run(action: &ResearchAction) -> Result<(), CliError> {
    let (root, id) = match action {
        ResearchAction::Status { research_root } => (research_root, None),
        ResearchAction::Claim {
            research_root,
            hypothesis,
        } => (research_root, Some(hypothesis.clone())),
    };
    let args = ResearchArgs {
        research_root: root.clone(),
        hypothesis: id,
    };
    let root = args.resolve_root()?;
    let mut journal = Journal::open(&root)?;
    if args.hypothesis.is_some() {
        let fiche = args.load_hypothesis(&root)?;
        let frozen: FrozenCandidate = load_frozen(&root, &fiche, journal.registry())?;
        let date = UtcDate::from_timestamp_ms(super::now_ms()?).map_err(ResearchError::from)?;
        let events = journal
            .registry()
            .propose_claims(&frozen.config_hashes, &fiche.reference, &date)
            .map_err(ResearchError::from)?;
        journal.append(&events)?;
        println!(
            "Claims HOLDOUT inscrits pour {} : {} configuration(s).",
            fiche.reference.id(),
            frozen.config_hashes.len()
        );
    } else {
        let date = UtcDate::from_timestamp_ms(super::now_ms()?).map_err(ResearchError::from)?;
        let summary = journal.refresh_readme(&date)?;
        print!("{summary}");
    }
    Ok(())
}

/// Résout toujours le checkout depuis cwd/--research-root, indépendamment du store de données.
pub(crate) fn resolve_root(explicit: Option<&Path>) -> Result<PathBuf, ResearchError> {
    let start = match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|e| io(Path::new("."), e))?,
    };
    let start = fs::canonicalize(&start).map_err(|e| io(&start, e))?;
    let bytes = git(
        &start,
        &["rev-parse", "--show-toplevel"],
        "checkout Git de recherche introuvable",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| refused("racine Git non UTF-8"))?
        .trim_end_matches(['\n', '\r']);
    fs::canonicalize(text).map_err(|e| io(Path::new(text), e))
}

/// Normalisation de la frontière dsh-v1/registre (le registre stocke 64 caractères nus).
pub(crate) fn digest(value: &str) -> Result<Digest, ResearchError> {
    Ok(Digest::new(value.strip_prefix("blake3:").unwrap_or(value))?)
}

/// Refuse aussi les parents symboliques avant de lire/créer le fichier final.
pub(crate) fn safe_path(root: &Path, relative: &str) -> Result<PathBuf, ResearchError> {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(refused(format!(
                    "lien symbolique interdit : {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(&path, error)),
        }
    }
    Ok(path)
}

/// BLAKE3 en flux du binaire courant, également utilisé par la vérification de gel.
pub(crate) fn binary_hash() -> Result<String, ResearchError> {
    let path = std::env::current_exe().map_err(|e| io(Path::new("binaire courant"), e))?;
    let mut file = File::open(&path).map_err(|e| io(&path, e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| io(&path, e))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}
