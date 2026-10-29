//! Sous-commandes `tv4 dataset ...` : `build` depuis S4 (multi-séries avec `--symbol` répété depuis S9,
//! datasets de recherche par bloc avec `--block` depuis S11).
//!
//! # Avec `--block` (docs/specs/s11.md §6.3)
//!
//! * `--from` est interdit : la fenêtre commence à `début du bloc − --warmup-days` (210 jours par
//!   défaut) ; `--to` est interdit sauf pour FORWARD, où il est obligatoire (fin des données voulue) ;
//!   les autres blocs finissent à la fin du bloc ;
//! * `--gaps-policy` vaut `zero_volume_marked` par défaut, toute autre valeur est refusée (code 2) ;
//!   `--tf` doit valoir `1m` ;
//! * `--universe <DOSSIER>` (facultatif) vérifie `universe.json`, prend comme symboles l'**union
//!   dédoublonnée** des membres des trimestres qui recoupent la fenêtre de décision et des `--symbol`
//!   donnés, et écrit `universe` (`rules_hash`, `window_hash`) dans le manifeste ; sans lui,
//!   `--symbol` est obligatoire (univers fixe) ;
//! * les exclusions qualité par symbole sont calculées par le store sur la série complète (§4.3).
//!
//! Sans `--block`, rien ne change : `--symbol`, `--from` et `--to` sont exigés (code 2 sinon, comme
//! l'exigeait clap) et la politique de trous vaut `skip` par défaut. Toute option de recherche donnée
//! sans `--block` est refusée.

use std::path::PathBuf;

use clap::Args;

use super::{CliError, now_ms};
use crate::model::blocks::{WARMUP_PREFIX_DAYS_DEFAULT, WARMUP_PREFIX_DAYS_MAX, decision_window};
use crate::model::{Block, ExchangeId, Timeframe};
use crate::store::{
    self, BuildRequest, BuildStatus, GapsPolicy, MultiBuildRequest, Partition, ResearchMeta,
    Symbol, UtcMs, Window,
};

/// Arguments de `tv4 dataset build`.
#[derive(Debug, Args)]
pub struct DatasetBuildArgs {
    /// Racine de travail (contient data/, datasets/, state/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
    /// Nom du dataset : datasets/<nom>.json ([a-z0-9_], 1 à 64 caractères).
    #[arg(long, value_name = "NOM")]
    pub name: String,
    /// Exchange canonique des données (kraken, okx, bitvavo_usdc).
    #[arg(long, value_name = "EXCHANGE")]
    pub exchange: ExchangeId,
    /// Symbole canonique BASE-QUOTE ; l'option se répète pour un dataset multi-séries (S9), par
    /// exemple `--symbol ETH-USD --symbol BTC-USD`. L'ordre est sans effet sur le manifeste. Exigée
    /// sans `--block` et sans `--universe` ; avec les deux, elle s'ajoute à l'union de l'univers.
    #[arg(long = "symbol", value_name = "SYMBOLE")]
    pub symbols: Vec<Symbol>,
    /// Timeframe des bougies (1m, 5m, ..., 1d). `1m` avec `--block`.
    #[arg(long, value_name = "TF")]
    pub tf: Timeframe,
    /// Début inclus, RFC 3339 UTC (2024-01-01T00:00:00Z). Exigé sans `--block`, interdit avec.
    #[arg(long, value_name = "RFC3339")]
    pub from: Option<UtcMs>,
    /// Fin exclue, RFC 3339 UTC. Exigée sans `--block` et pour FORWARD, interdite pour les autres blocs.
    #[arg(long, value_name = "RFC3339")]
    pub to: Option<UtcMs>,
    /// Politique de trous : skip (défaut sans bloc), ffill, zero_volume_marked (seule valeur, et défaut,
    /// avec `--block`).
    #[arg(long, value_name = "POLITIQUE")]
    pub gaps_policy: Option<GapsPolicy>,
    /// Bloc de recherche (DEV, VALIDATION, HOLDOUT, FORWARD) : construit un dataset de recherche
    /// (docs/specs/s11.md §6).
    #[arg(long, value_name = "BLOC")]
    pub block: Option<Block>,
    /// Dossier d'univers trimestriel (`tv4 universe build`) : les symboles sont l'union des membres des
    /// trimestres de la fenêtre de décision. Exige `--block`.
    #[arg(long, value_name = "DOSSIER", requires = "block")]
    pub universe: Option<PathBuf>,
    /// Jours de chauffe avant le début du bloc (0 à 400, défaut 210). Exige `--block`.
    #[arg(
        long,
        value_name = "JOURS",
        requires = "block",
        value_parser = clap::value_parser!(i64).range(0..=WARMUP_PREFIX_DAYS_MAX)
    )]
    pub warmup_days: Option<i64>,
}

fn usage(option: &str, detail: impl Into<String>) -> CliError {
    CliError::Usage {
        option: option.to_string(),
        detail: detail.into(),
    }
}

/// `tv4 dataset build` : manifeste et `dataset_hash` d'un dataset figé.
pub(super) fn build(args: &DatasetBuildArgs) -> Result<(), CliError> {
    let outcome = match args.block {
        Some(block) => build_research(args, block)?,
        None => build_plain(args)?,
    };
    let statut = match outcome.status {
        BuildStatus::Created => "créé",
        BuildStatus::Unchanged => "inchangé",
    };
    println!(
        "{} : {statut}, {} bougies, {} fichier(s) dont {} snapshot(s), dataset_hash {}",
        outcome.manifest_rel,
        outcome.manifest.row_count,
        outcome.manifest.files.len(),
        outcome.snapshots,
        outcome.manifest.dataset_hash
    );
    if let (Some(block), Some(window)) = (outcome.manifest.block, &outcome.manifest.decision_window)
    {
        println!(
            "bloc {block} : fenêtre de décision [{}, {}), {} symbole(s), fenêtre du dataset [{}, {})",
            window[0],
            window[1],
            outcome.manifest.symbols.len(),
            outcome.manifest.window[0],
            outcome.manifest.window[1]
        );
    }
    Ok(())
}

/// Chemin de S4 et de S9, inchangé : `--symbol`, `--from` et `--to` exigés.
fn build_plain(args: &DatasetBuildArgs) -> Result<store::BuildOutcome, CliError> {
    let (Some(from), Some(to)) = (args.from, args.to) else {
        return Err(usage(
            if args.from.is_none() {
                "--from"
            } else {
                "--to"
            },
            "obligatoire sans --block",
        ));
    };
    if args.symbols.is_empty() {
        return Err(usage("--symbol", "obligatoire sans --block"));
    }
    let window = Window::new(from.0, to.0)?;
    let gaps_policy = args.gaps_policy.unwrap_or(GapsPolicy::Skip);
    // Un seul symbole : chemin mono-série inchangé du jalon 1. Plusieurs : dataset multi-séries.
    Ok(match args.symbols.as_slice() {
        [symbol] => {
            let request = BuildRequest {
                name: &args.name,
                partition: Partition::new(args.exchange, symbol.clone(), args.tf),
                window,
                gaps_policy,
            };
            store::build_dataset(&args.root, &request, now_ms()?)?
        }
        symbols => {
            let request = MultiBuildRequest {
                name: &args.name,
                exchange: args.exchange,
                symbols,
                tf: args.tf,
                window,
                gaps_policy,
            };
            store::build_dataset_multi(&args.root, &request, now_ms()?)?
        }
    })
}

/// Dataset de recherche d'un bloc (docs/specs/s11.md §6.2 et §6.3).
fn build_research(args: &DatasetBuildArgs, block: Block) -> Result<store::BuildOutcome, CliError> {
    if args.from.is_some() {
        return Err(usage(
            "--from",
            "interdit avec --block : la fenêtre commence à début du bloc − --warmup-days",
        ));
    }
    if args.tf != Timeframe::M1 {
        return Err(usage(
            "--tf",
            format!(
                "{} : un dataset de recherche se construit en 1m, les autres timeframes sont dérivés",
                args.tf
            ),
        ));
    }
    if let Some(policy) = args.gaps_policy
        && policy != GapsPolicy::ZeroVolumeMarked
    {
        return Err(usage(
            "--gaps-policy",
            format!("{policy} : avec --block, seule zero_volume_marked est admise"),
        ));
    }
    let window_end = match (block, args.to) {
        (Block::Forward, Some(to)) => to.0,
        (Block::Forward, None) => {
            return Err(usage(
                "--to",
                "obligatoire pour FORWARD (fin des données voulue)",
            ));
        }
        (_, Some(_)) => {
            return Err(usage(
                "--to",
                format!("interdit pour {block} : la fenêtre finit à la fin du bloc"),
            ));
        }
        (_, None) => block.span().end_ms.ok_or_else(|| {
            usage(
                "--block",
                format!("{block} est ouvert à droite : --to obligatoire"),
            )
        })?,
    };
    let warmup_days = args.warmup_days.unwrap_or(WARMUP_PREFIX_DAYS_DEFAULT);
    let window_start = block.warmup_start_ms(warmup_days)?;
    let (decision_start, decision_end) = decision_window(block, window_end)?;
    let window = Window::new(window_start, window_end)?;

    // Symboles : union dédoublonnée des membres de la fenêtre de décision et des --symbol.
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut universe_ref = None;
    let mut expectation = None;
    match args.universe.as_deref() {
        Some(dir) => {
            let loaded = store::universe::load(dir)?;
            universe_ref = Some(loaded.reference(decision_start, decision_end)?);
            expectation = Some(loaded.quality_expectation(decision_start, decision_end));
            for symbol in loaded.union_symbols(decision_start, decision_end) {
                symbols.push(symbol.parse::<Symbol>()?);
            }
        }
        None if args.symbols.is_empty() => {
            return Err(usage(
                "--symbol",
                "obligatoire avec --block sans --universe (univers fixe)",
            ));
        }
        None => {}
    }
    for symbol in &args.symbols {
        if !symbols.contains(symbol) {
            symbols.push(symbol.clone());
        }
    }
    let request = MultiBuildRequest {
        name: &args.name,
        exchange: args.exchange,
        symbols: &symbols,
        tf: args.tf,
        window,
        gaps_policy: GapsPolicy::ZeroVolumeMarked,
    };
    let research = ResearchMeta {
        block,
        universe: universe_ref,
        universe_quality: expectation,
    };
    Ok(store::build_dataset_research(
        &args.root,
        &request,
        &research,
        now_ms()?,
    )?)
}
