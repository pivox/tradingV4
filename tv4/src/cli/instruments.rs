//! Sous-commande `tv4 instruments fetch` (docs/specs/s9.md §8.1 et §8.2).
//!
//! Lit les instruments autorisés pour le compte (`GET /api/v5/account/instruments?instType=SPOT` sur
//! `https://eea.okx.com`) avec une clé **en lecture seule**, et écrit un fichier daté et hashé
//! `<root>/instruments/okx/spot-<AAAAMMJJ>T<HHMMSS>Z.json` (schéma `tv4-instruments-v1`).
//!
//! Séquence : `public/time` (heure du serveur, non signée), `account/config` (contrôle de `perm`),
//! `account/instruments`. Aucune relance automatique. La clé vient de `--credentials` ou des variables
//! `TV4_OKX_API_KEY`, `TV4_OKX_SECRET_KEY` et `TV4_OKX_PASSPHRASE` ; elle n'est jamais écrite ni
//! journalisée, et la sortie standard ne montre ni clé, ni signature, ni en-tête.
//!
//! Codes de sortie : 0 succès, 1 erreur (réseau, réponse, fichier), 2 clé refusée (permission autre
//! que lecture, fichier de clé non sûr).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use super::CliError;
use crate::marketdata::okx::DEFAULT_BASE_URL;
use crate::marketdata::okx::instruments::{InstrumentsFile, file_name, site_of};
use crate::marketdata::okx::private::{Credentials, PrivateClient, PrivateConfig, check_read_only};
use crate::store::StoreError;

/// Opérations sur les instruments.
#[derive(Debug, Subcommand)]
pub enum InstrumentsAction {
    /// Lit les instruments OKX du compte (clé en lecture seule) et écrit un fichier daté et hashé.
    Fetch(FetchArgs),
}

/// Arguments de `tv4 instruments fetch`.
#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Racine de travail : le fichier est écrit sous `<root>/instruments/okx/`.
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
    /// Fichier TOML de la clé (api_key, secret_key, passphrase), hors dépôt, en mode 0600 ou 0400.
    /// Sans lui : variables TV4_OKX_API_KEY, TV4_OKX_SECRET_KEY et TV4_OKX_PASSPHRASE.
    #[arg(long, value_name = "TOML")]
    pub credentials: Option<PathBuf>,
    /// Racine de l'API (https://eea.okx.com par défaut). Réservée aux tests : http:// n'est accepté
    /// que pour 127.0.0.1 et localhost.
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,
    /// Type d'instruments (SPOT seulement).
    #[arg(long, value_name = "TYPE", default_value = "SPOT", value_parser = ["SPOT"])]
    pub inst_type: String,
    /// Fichier de sortie. Défaut : `<root>/instruments/okx/spot-<horodatage UTC du serveur>.json`.
    /// Un fichier existant n'est jamais écrasé.
    #[arg(long, value_name = "JSON")]
    pub out: Option<PathBuf>,
}

/// Paires dont la présence est rappelée à l'écran : le périmètre live du cycle 1.
const WATCHED: [&str; 2] = ["BTC-USDC", "ETH-USDC"];

/// `tv4 instruments fetch`.
pub(super) fn fetch(args: &FetchArgs) -> Result<(), CliError> {
    // Échec rapide : un fichier de sortie donné et déjà présent, avant toute requête.
    if let Some(out) = &args.out {
        refuse_existing(out)?;
    }
    let credentials = Credentials::resolve(args.credentials.as_deref())?;
    let base_url = args.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
    let config = PrivateConfig {
        base_url: base_url.to_string(),
        ..PrivateConfig::default()
    };
    let mut client = PrivateClient::new(&config, credentials)?;

    let server_time_ms = client.server_time_ms()?;
    // La permission est contrôlée avant toute requête vers `account/instruments`.
    check_read_only(client.account_perm()?.as_deref())?;
    let data = client.account_instruments(&args.inst_type)?;

    let file =
        InstrumentsFile::from_response(site_of(base_url), &args.inst_type, server_time_ms, &data)?;
    let path = match &args.out {
        Some(out) => out.clone(),
        None => args
            .root
            .join("instruments")
            .join("okx")
            .join(file_name(&args.inst_type, server_time_ms)),
    };
    refuse_existing(&path)?;
    write_new_durable(&path, &file.to_bytes()?)?;
    print_summary(&path, &file);
    Ok(())
}

/// Refuse d'écraser un fichier existant (lien symbolique compris).
fn refuse_existing(path: &Path) -> Result<(), CliError> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(CliError::OutputExists {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

/// Écrit `path` de façon durable : `<nom>.tmp`, `sync_all`, `rename`, `sync_all` du dossier
/// (docs/03 §5.1). Le dossier est créé au besoin.
fn write_new_durable(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let io = |at: &Path| {
        let shown = at.display().to_string();
        move |source: std::io::Error| {
            CliError::Store(StoreError::Io {
                path: shown,
                source,
            })
        }
    };
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(io(dir))?;
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let written = (|| {
        let mut file = File::create(&tmp).map_err(io(&tmp))?;
        file.write_all(bytes).map_err(io(&tmp))?;
        file.sync_all().map_err(io(&tmp))?;
        fs::rename(&tmp, path).map_err(io(path))
    })();
    if let Err(error) = written {
        // Au mieux : l'erreur d'origine prime.
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    File::open(dir).and_then(|d| d.sync_all()).map_err(io(dir))
}

/// Sortie standard : chemin, hash, nombre d'instruments, paires suivies, paires USDT éventuelles.
/// Aucune valeur de clé, de signature ni d'en-tête.
fn print_summary(path: &Path, file: &InstrumentsFile) {
    println!("fichier : {}", path.display());
    println!("instruments_hash : {}", file.instruments_hash);
    println!("instruments : {}", file.instruments.len());
    for inst_id in WATCHED {
        match file.get(inst_id) {
            Some(instrument) => println!("{inst_id} : listée, state {}", instrument.state),
            None => println!("{inst_id} : absente"),
        }
    }
    let usdt: Vec<&str> = file
        .instruments
        .iter()
        .filter(|instrument| instrument.inst_id.ends_with("-USDT"))
        .map(|instrument| instrument.inst_id.as_str())
        .collect();
    if !usdt.is_empty() {
        println!(
            "attention : {} paire(s) USDT listée(s) (l'API du compte ne devrait en rendre aucune pour un résident de l'EEE), par exemple {}",
            usdt.len(),
            usdt[0]
        );
    }
}
