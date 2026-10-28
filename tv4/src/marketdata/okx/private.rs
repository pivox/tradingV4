//! Client REST privé OKX v5, en lecture seule (docs/specs/s9.md §8.2 à §8.4).
//!
//! Ce module ne sert qu'à `tv4 instruments fetch` : trois requêtes `GET` (`public/time` non signée,
//! `account/config` et `account/instruments` signées), sans relance automatique. Il ne passe jamais
//! d'ordre.
//!
//! # Sécurité de la clé
//!
//! - [`Credentials`] a des champs privés, un `Debug` écrit à la main qui masque les trois valeurs, ni
//!   `Serialize`, ni `Display`, ni `Clone`.
//! - Une clé vient d'un fichier TOML hors dépôt ([`Credentials::from_file`]) ou de trois variables
//!   d'environnement ([`Credentials::from_env`]). Le fichier doit être un fichier régulier (pas un lien
//!   symbolique), en mode 0600 ou 0400, et aucun de ses dossiers ancêtres ne doit contenir `.git`.
//! - Les erreurs ([`CredentialsError`], [`PrivateApiError`]) ne contiennent ni clé, ni secret, ni
//!   passphrase, ni en-tête, ni URL signée, ni corps de requête, ni contenu du fichier de clé (pas même
//!   le message de l'analyseur TOML, qui recopie la ligne fautive). Elles contiennent la méthode, le
//!   chemin, le statut HTTP, le `code` et le `msg` d'OKX (ce dernier expurgé des valeurs de la clé).
//! - Le client ne suit aucune redirection : une redirection renverrait les en-têtes signés vers un
//!   autre hôte.
//! - Un schéma autre que `https://` est refusé, sauf `http://127.0.0.1` et `http://localhost`, réservés
//!   aux tests.
//!
//! La permission de la clé est contrôlée avant toute requête vers `account/instruments`
//! ([`check_read_only`]) : une clé qui peut trader ou retirer est refusée.

use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use super::sign::{iso_ms, sign};
use crate::model::Ts;

/// Variable d'environnement de la clé API.
pub const ENV_API_KEY: &str = "TV4_OKX_API_KEY";

/// Variable d'environnement du secret.
pub const ENV_SECRET_KEY: &str = "TV4_OKX_SECRET_KEY";

/// Variable d'environnement de la passphrase.
pub const ENV_PASSPHRASE: &str = "TV4_OKX_PASSPHRASE";

/// Taille maximale d'un fichier de clé : trois chaînes courtes tiennent très en deçà.
const MAX_KEY_FILE_BYTES: u64 = 16 * 1024;

/// Longueur maximale d'un message OKX recopié dans une erreur.
const MAX_MSG_IN_ERROR: usize = 200;

/// Chemin de l'heure du serveur.
const TIME_PATH: &str = "/api/v5/public/time";

/// Chemin de la configuration du compte (champ `perm`).
const CONFIG_PATH: &str = "/api/v5/account/config";

/// Chemin des instruments du compte.
const INSTRUMENTS_PATH: &str = "/api/v5/account/instruments";

/// Erreurs de lecture ou de garde de la clé. Aucune ne contient une valeur de la clé.
#[derive(Debug, Error)]
pub enum CredentialsError {
    /// Ni fichier de clé donné, ni variable d'environnement.
    #[error(
        "clé OKX introuvable : donner --credentials <fichier TOML> ou définir {ENV_API_KEY}, {ENV_SECRET_KEY} et {ENV_PASSPHRASE}"
    )]
    Missing,

    /// Certaines variables sont définies, pas toutes (ou une est vide).
    #[error("clé OKX incomplète dans l'environnement : {missing} manque ou est vide")]
    EnvIncomplete { missing: String },

    /// Une des trois valeurs est vide.
    #[error("clé OKX : le champ `{field}` est vide")]
    EmptyField { field: &'static str },

    /// Le fichier de clé est un lien symbolique.
    #[error("fichier de clé `{path}` refusé : lien symbolique")]
    Symlink { path: String },

    /// Le fichier de clé n'est pas un fichier régulier.
    #[error("fichier de clé `{path}` refusé : ce n'est pas un fichier régulier")]
    NotRegular { path: String },

    /// Droits trop larges : seuls 0600 et 0400 sont acceptés.
    #[error(
        "fichier de clé `{path}` refusé : droits {mode:04o}, exiger 0600 ou 0400 (chmod 600 <fichier>)"
    )]
    UnsafeMode { path: String, mode: u32 },

    /// Un dossier ancêtre contient `.git` : un fichier de clé ne vit pas dans un dépôt.
    #[error("fichier de clé `{path}` refusé : il est sous un dépôt git (`{repo}`)")]
    InsideGitRepo { path: String, repo: String },

    /// Plateforme sans contrôle des droits : la garde ne peut pas être appliquée.
    #[error(
        "fichier de clé `{path}` refusé : contrôle des droits indisponible sur cette plateforme"
    )]
    UnsupportedPlatform { path: String },

    /// Le fichier de clé n'a pas pu être lu (le type d'erreur seul, jamais le contenu).
    #[error("fichier de clé `{path}` illisible : {kind}")]
    Unreadable { path: String, kind: String },

    /// Le fichier dépasse la taille raisonnable d'un fichier de clé.
    #[error("fichier de clé `{path}` trop gros (plus de {MAX_KEY_FILE_BYTES} octets)")]
    TooLarge { path: String },

    /// TOML invalide ou champs inattendus. Le détail de l'analyseur n'est pas recopié : il citerait
    /// la ligne fautive, donc possiblement la clé.
    #[error(
        "fichier de clé `{path}` invalide : TOML attendu avec les trois champs api_key, secret_key et passphrase, et aucun autre"
    )]
    Malformed { path: String },
}

impl CredentialsError {
    /// Vrai si la clé est refusée pour sa garde (fichier non sûr) : code de sortie 2 de la commande.
    pub fn is_unsafe_file(&self) -> bool {
        matches!(
            self,
            CredentialsError::Symlink { .. }
                | CredentialsError::NotRegular { .. }
                | CredentialsError::UnsafeMode { .. }
                | CredentialsError::InsideGitRepo { .. }
                | CredentialsError::UnsupportedPlatform { .. }
        )
    }
}

/// Contenu du fichier de clé. Pas de `Debug` : la struct porte les valeurs en clair.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    api_key: String,
    secret_key: String,
    passphrase: String,
}

/// Clé API OKX : clé, secret et passphrase.
///
/// Champs privés, `Debug` masqué, ni `Serialize` ni `Display` ni `Clone`.
pub struct Credentials {
    api_key: String,
    secret_key: String,
    passphrase: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("api_key", &"***")
            .field("secret_key", &"***")
            .field("passphrase", &"***")
            .finish()
    }
}

impl Credentials {
    /// Construit une clé. Aucune des trois valeurs ne peut être vide.
    pub fn new(
        api_key: String,
        secret_key: String,
        passphrase: String,
    ) -> Result<Credentials, CredentialsError> {
        for (field, value) in [
            ("api_key", &api_key),
            ("secret_key", &secret_key),
            ("passphrase", &passphrase),
        ] {
            if value.is_empty() {
                return Err(CredentialsError::EmptyField { field });
            }
        }
        Ok(Credentials {
            api_key,
            secret_key,
            passphrase,
        })
    }

    /// Lit la clé dans un fichier TOML après la garde du fichier (voir le module).
    pub fn from_file(path: &Path) -> Result<Credentials, CredentialsError> {
        let text = read_key_file(path)?;
        let parsed: KeyFile = toml::from_str(&text).map_err(|_| CredentialsError::Malformed {
            path: path.display().to_string(),
        })?;
        Credentials::new(parsed.api_key, parsed.secret_key, parsed.passphrase)
    }

    /// Lit la clé dans les trois variables d'environnement du processus.
    pub fn from_env() -> Result<Credentials, CredentialsError> {
        Credentials::from_env_with(|name| std::env::var(name).ok())
    }

    /// Comme [`Credentials::from_env`], avec un lecteur de variables injecté (les tests n'ont pas à
    /// modifier l'environnement du processus).
    ///
    /// Aucune variable définie : [`CredentialsError::Missing`]. Une ou deux seulement, ou une vide :
    /// [`CredentialsError::EnvIncomplete`], qui nomme les variables manquantes (jamais leur valeur).
    pub fn from_env_with(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Credentials, CredentialsError> {
        let get = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let (key, secret, passphrase) =
            (get(ENV_API_KEY), get(ENV_SECRET_KEY), get(ENV_PASSPHRASE));
        match (key, secret, passphrase) {
            (Some(key), Some(secret), Some(passphrase)) => {
                Credentials::new(key, secret, passphrase)
            }
            (None, None, None) => Err(CredentialsError::Missing),
            (key, secret, passphrase) => {
                let missing: Vec<&str> = [
                    (ENV_API_KEY, key.is_none()),
                    (ENV_SECRET_KEY, secret.is_none()),
                    (ENV_PASSPHRASE, passphrase.is_none()),
                ]
                .into_iter()
                .filter_map(|(name, absent)| absent.then_some(name))
                .collect();
                Err(CredentialsError::EnvIncomplete {
                    missing: missing.join(", "),
                })
            }
        }
    }

    /// Source de la clé : le fichier s'il est donné, sinon l'environnement.
    pub fn resolve(file: Option<&Path>) -> Result<Credentials, CredentialsError> {
        match file {
            Some(path) => Credentials::from_file(path),
            None => Credentials::from_env(),
        }
    }

    /// Remplace par `***` toute occurrence d'une des trois valeurs dans `text`.
    fn redact(&self, text: &str) -> String {
        let mut clean = text.to_string();
        for value in [&self.api_key, &self.secret_key, &self.passphrase] {
            if !value.is_empty() {
                clean = clean.replace(value.as_str(), "***");
            }
        }
        clean
    }
}

/// Garde du fichier de clé, puis lecture de son contenu (UTF-8, 16 Kio au plus).
///
/// Contrôles, dans l'ordre : le chemin n'est pas un lien symbolique ; le fichier est régulier ; sous
/// unix `mode & 0o077 == 0` ; aucun dossier ancêtre (chemin résolu) ne contient `.git`. Les droits et
/// le type sont relus sur le descripteur ouvert et comparés à l'inode vu avant l'ouverture, pour
/// qu'un remplacement entre le contrôle et la lecture ne passe pas.
fn read_key_file(path: &Path) -> Result<String, CredentialsError> {
    let shown = || path.display().to_string();
    let unreadable = |error: &std::io::Error| CredentialsError::Unreadable {
        path: shown(),
        kind: error.kind().to_string(),
    };

    let before = fs::symlink_metadata(path).map_err(|e| unreadable(&e))?;
    if before.file_type().is_symlink() {
        return Err(CredentialsError::Symlink { path: shown() });
    }
    if !before.is_file() {
        return Err(CredentialsError::NotRegular { path: shown() });
    }
    check_key_file_ancestors(path)?;

    let mut file = File::open(path).map_err(|e| unreadable(&e))?;
    let opened = file.metadata().map_err(|e| unreadable(&e))?;
    if !opened.is_file() {
        return Err(CredentialsError::NotRegular { path: shown() });
    }
    check_mode_and_identity(path, &before, &opened)?;
    if opened.len() > MAX_KEY_FILE_BYTES {
        return Err(CredentialsError::TooLarge { path: shown() });
    }
    let mut text = String::new();
    (&mut file)
        .take(MAX_KEY_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| unreadable(&e))?;
    if text.len() as u64 > MAX_KEY_FILE_BYTES {
        return Err(CredentialsError::TooLarge { path: shown() });
    }
    Ok(text)
}

#[cfg(unix)]
fn check_mode_and_identity(
    path: &Path,
    before: &fs::Metadata,
    opened: &fs::Metadata,
) -> Result<(), CredentialsError> {
    use std::os::unix::fs::MetadataExt;
    let shown = path.display().to_string();
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(CredentialsError::NotRegular { path: shown });
    }
    let mode = opened.mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(CredentialsError::UnsafeMode { path: shown, mode });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_mode_and_identity(
    path: &Path,
    _before: &fs::Metadata,
    _opened: &fs::Metadata,
) -> Result<(), CredentialsError> {
    Err(CredentialsError::UnsupportedPlatform {
        path: path.display().to_string(),
    })
}

/// Refuse un fichier de clé dont un dossier ancêtre contient `.git` (dépôt ou worktree). Le chemin est
/// d'abord résolu (`..` et liens des dossiers parents), pour juger l'emplacement réel du fichier.
fn check_key_file_ancestors(path: &Path) -> Result<(), CredentialsError> {
    let real = fs::canonicalize(path).map_err(|error| CredentialsError::Unreadable {
        path: path.display().to_string(),
        kind: error.kind().to_string(),
    })?;
    for dir in real.ancestors().skip(1) {
        if fs::symlink_metadata(dir.join(".git")).is_ok() {
            return Err(CredentialsError::InsideGitRepo {
                path: path.display().to_string(),
                repo: dir.display().to_string(),
            });
        }
    }
    Ok(())
}

/// Erreurs des requêtes privées. Aucune ne contient d'en-tête, d'URL signée, de corps de requête ni
/// de valeur de la clé : la méthode, le chemin sans hôte, le statut, le `code` et le `msg` d'OKX
/// suffisent à diagnostiquer.
#[derive(Debug, Error)]
pub enum PrivateApiError {
    /// URL de base refusée : schéma autre que `https://` (hors machine locale), identifiants dans
    /// l'URL, requête ou fragment. L'URL n'est pas recopiée.
    #[error(
        "URL de base refusée : https:// exigé (http:// seulement pour 127.0.0.1 et localhost), sans identifiants ni requête"
    )]
    BaseUrl,

    /// Erreur de transport. `reason` est une catégorie, pas le message de la bibliothèque.
    #[error("{method} {path} : transport : {reason}")]
    Transport {
        method: &'static str,
        path: String,
        reason: String,
    },

    /// Statut HTTP hors 2xx ; `detail` reprend `code` et `msg` quand le corps est une enveloppe OKX.
    #[error("{method} {path} : HTTP {status}{detail}")]
    Http {
        method: &'static str,
        path: String,
        status: u16,
        detail: String,
    },

    /// Enveloppe `code != "0"`.
    #[error("{method} {path} : OKX code {code} : {msg}")]
    Api {
        method: &'static str,
        path: String,
        code: String,
        msg: String,
    },

    /// Enveloppe illisible : JSON invalide, `code` ou `data` absent ou mal typé.
    #[error("{method} {path} : réponse illisible : {reason}")]
    Envelope {
        method: &'static str,
        path: String,
        reason: String,
    },

    /// La permission de la clé n'est pas lisible dans `account/config` : par prudence, la clé est
    /// refusée (code de sortie 2).
    #[error("permission de la clé illisible dans account/config : {reason}")]
    PermissionUnreadable { reason: String },

    /// La clé n'est pas en lecture seule (`perm` contient `trade` ou `withdraw`, ou une valeur
    /// inconnue) : refusée (code de sortie 2). La valeur de `perm` n'est pas un secret.
    #[error(
        "clé refusée : permission « {perm} » au lieu de « read_only » ; créer une clé en lecture seule, sans trade ni retrait"
    )]
    KeyNotReadOnly { perm: String },
}

impl PrivateApiError {
    /// Vrai si la clé est refusée pour sa permission : code de sortie 2 de la commande.
    pub fn is_key_refused(&self) -> bool {
        matches!(
            self,
            PrivateApiError::PermissionUnreadable { .. } | PrivateApiError::KeyNotReadOnly { .. }
        )
    }
}

/// Contrôle de la permission lue dans `account/config`.
///
/// `perm` est une liste séparée par des virgules. Seul `read_only` est accepté : `trade`, `withdraw`
/// et toute valeur inconnue donnent [`PrivateApiError::KeyNotReadOnly`] ; une valeur absente ou vide
/// donne [`PrivateApiError::PermissionUnreadable`].
pub fn check_read_only(perm: Option<&str>) -> Result<(), PrivateApiError> {
    let Some(perm) = perm.map(str::trim).filter(|perm| !perm.is_empty()) else {
        return Err(PrivateApiError::PermissionUnreadable {
            reason: "champ `perm` absent ou vide".to_string(),
        });
    };
    let tokens: Vec<&str> = perm.split(',').map(str::trim).collect();
    if tokens.iter().all(|token| *token == "read_only") {
        return Ok(());
    }
    Err(PrivateApiError::KeyNotReadOnly {
        perm: sanitize(perm),
    })
}

/// Remplace les caractères de contrôle par une espace et borne la longueur, pour qu'un texte reçu du
/// serveur ne casse ni une ligne de journal ni un message.
fn sanitize(text: &str) -> String {
    text.chars()
        .take(MAX_MSG_IN_ERROR)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Valide l'URL de base et la rend sans `/` final. `https://` partout ; `http://` seulement pour
/// l'hôte `127.0.0.1` ou `localhost`. Les identifiants (`@`), la requête (`?`) et le fragment (`#`)
/// sont refusés. Retourne aussi si l'hôte est local.
fn validate_base_url(url: &str) -> Result<(String, bool), PrivateApiError> {
    let (is_https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(PrivateApiError::BaseUrl);
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty()
        || url.contains(['@', '?', '#'])
        || authority.chars().any(char::is_whitespace)
    {
        return Err(PrivateApiError::BaseUrl);
    }
    let host = authority.split(':').next().unwrap_or("");
    let local = host == "127.0.0.1" || host == "localhost";
    if !is_https && !local {
        return Err(PrivateApiError::BaseUrl);
    }
    Ok((url.trim_end_matches('/').to_string(), local))
}

/// Réglages du client privé.
#[derive(Clone, Debug)]
pub struct PrivateConfig {
    /// Racine de l'API (`https://eea.okx.com` par défaut), sans `/` final.
    pub base_url: String,
    /// Délai global d'une requête.
    pub timeout: Duration,
    /// Vrai : l'agent lit `HTTPS_PROXY` et `NO_PROXY`. Ignoré (faux) pour une machine locale, où se
    /// trouvent les serveurs de test.
    pub use_env_proxy: bool,
}

impl Default for PrivateConfig {
    fn default() -> Self {
        PrivateConfig {
            base_url: super::DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(10),
            use_env_proxy: true,
        }
    }
}

/// Horloge du serveur : l'heure lue dans `public/time`, avancée du temps écoulé localement. Elle sert
/// d'horodatage de signature, et l'écart de 30 s toléré par OKX (erreur 50102) ne dépend alors plus de
/// l'horloge de la machine.
#[derive(Clone, Copy, Debug)]
struct ServerClock {
    server_ms: Ts,
    read_at: Instant,
}

impl ServerClock {
    fn now_ms(&self) -> Ts {
        let elapsed = Ts::try_from(self.read_at.elapsed().as_millis()).unwrap_or(0);
        self.server_ms.saturating_add(elapsed)
    }
}

/// Client des requêtes privées en lecture. Voir le module.
pub struct PrivateClient {
    agent: ureq::Agent,
    base_url: Arc<str>,
    credentials: Credentials,
    clock: Option<ServerClock>,
}

impl fmt::Debug for PrivateClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateClient")
            .field("base_url", &self.base_url)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl PrivateClient {
    /// Construit le client. L'URL de base est validée ([`PrivateApiError::BaseUrl`]).
    pub fn new(
        config: &PrivateConfig,
        credentials: Credentials,
    ) -> Result<PrivateClient, PrivateApiError> {
        let (base_url, local) = validate_base_url(&config.base_url)?;
        let mut builder = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .http_status_as_error(false)
            // Aucune redirection : elle ferait suivre les en-têtes signés vers un autre hôte.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .https_only(!local);
        if local || !config.use_env_proxy {
            builder = builder.proxy(None);
        }
        Ok(PrivateClient {
            agent: ureq::Agent::new_with_config(builder.build()),
            base_url: Arc::from(base_url),
            credentials,
            clock: None,
        })
    }

    /// `GET /api/v5/public/time`, non signée : heure du serveur en millisecondes. Elle fixe l'horloge
    /// de signature des requêtes suivantes.
    pub fn server_time_ms(&mut self) -> Result<Ts, PrivateApiError> {
        let data = self.get("GET", TIME_PATH, None, false)?;
        let ts = data
            .first()
            .and_then(|row| row.get("ts"))
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<Ts>().ok())
            .filter(|ts| *ts > 0)
            .ok_or_else(|| envelope_error(TIME_PATH, "`data[0].ts` absent ou non numérique"))?;
        self.clock = Some(ServerClock {
            server_ms: ts,
            read_at: Instant::now(),
        });
        Ok(ts)
    }

    /// `GET /api/v5/account/config`, signée : valeur du champ `perm` (`None` si absent).
    pub fn account_perm(&mut self) -> Result<Option<String>, PrivateApiError> {
        let data = self.get("GET", CONFIG_PATH, None, true)?;
        Ok(data
            .first()
            .and_then(|row| row.get("perm"))
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// `GET /api/v5/account/instruments?instType=<inst_type>`, signée : le tableau `data`.
    pub fn account_instruments(&mut self, inst_type: &str) -> Result<Vec<Value>, PrivateApiError> {
        let query = format!("instType={inst_type}");
        self.get("GET", INSTRUMENTS_PATH, Some(&query), true)
    }

    /// Une requête GET. Le chemin signé est exactement celui qui est envoyé, requête comprise.
    fn get(
        &mut self,
        method: &'static str,
        path: &str,
        query: Option<&str>,
        signed: bool,
    ) -> Result<Vec<Value>, PrivateApiError> {
        let path_and_query = match query {
            Some(query) => format!("{path}?{query}"),
            None => path.to_string(),
        };
        let url = format!("{}{path_and_query}", self.base_url);
        let mut request = self.agent.get(&url);
        if signed {
            // Sans horloge du serveur, on prend d'abord l'heure : jamais l'horloge locale.
            if self.clock.is_none() {
                self.server_time_ms()?;
            }
            let clock = self
                .clock
                .ok_or_else(|| envelope_error(path, "horloge du serveur indisponible"))?;
            let timestamp = iso_ms(clock.now_ms());
            let signature = sign(
                &self.credentials.secret_key,
                &timestamp,
                method,
                &path_and_query,
                "",
            );
            request = request
                .header("OK-ACCESS-KEY", &self.credentials.api_key)
                .header("OK-ACCESS-SIGN", &signature)
                .header("OK-ACCESS-TIMESTAMP", &timestamp)
                .header("OK-ACCESS-PASSPHRASE", &self.credentials.passphrase)
                .header("Content-Type", "application/json");
        }
        let mut response = request
            .call()
            .map_err(|error| transport_error(method, path, &error))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| transport_error(method, path, &error))?;
        self.parse_response(method, path, status, &body)
    }

    /// Décode la réponse : statut, puis enveloppe `{"code","msg","data"}`.
    fn parse_response(
        &self,
        method: &'static str,
        path: &str,
        status: u16,
        body: &str,
    ) -> Result<Vec<Value>, PrivateApiError> {
        let parsed = serde_json::from_str::<Value>(body).ok();
        let code_and_msg = parsed.as_ref().and_then(|value| {
            let code = value.get("code")?.as_str()?;
            let msg = value.get("msg").and_then(Value::as_str).unwrap_or("");
            // Expurgation du texte entier, puis troncature et neutralisation des contrôles : dans
            // l'ordre inverse, une valeur de la clé à cheval sur la coupure ne serait plus reconnue
            // et son préfixe sortirait en clair. `code` suit le même chemin que `msg` : il vient du
            // serveur, et un `code` recopié brut injecterait ses séquences de contrôle dans le terminal.
            Some((
                sanitize(&self.credentials.redact(code)),
                sanitize(&self.credentials.redact(msg)),
            ))
        });
        if !(200..300).contains(&status) {
            let detail = code_and_msg
                .map(|(code, msg)| format!(" : OKX code {code} : {msg}"))
                .unwrap_or_default();
            return Err(PrivateApiError::Http {
                method,
                path: path.to_string(),
                status,
                detail,
            });
        }
        let envelope = |reason: &str| PrivateApiError::Envelope {
            method,
            path: path.to_string(),
            reason: reason.to_string(),
        };
        let Some(value) = parsed else {
            return Err(envelope("JSON invalide"));
        };
        let Some((code, msg)) = code_and_msg else {
            return Err(envelope("champ `code` absent"));
        };
        if code != "0" {
            return Err(PrivateApiError::Api {
                method,
                path: path.to_string(),
                code,
                msg,
            });
        }
        match value.get("data").and_then(Value::as_array) {
            Some(data) => Ok(data.clone()),
            None => Err(envelope("champ `data` absent ou n'est pas un tableau")),
        }
    }
}

fn envelope_error(path: &str, reason: &str) -> PrivateApiError {
    PrivateApiError::Envelope {
        method: "GET",
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

/// Catégorie d'une erreur `ureq`, sans recopier son message (qui peut citer l'URL).
fn transport_error(method: &'static str, path: &str, error: &ureq::Error) -> PrivateApiError {
    let reason = match error {
        ureq::Error::Timeout(_) => "délai dépassé".to_string(),
        ureq::Error::HostNotFound => "hôte introuvable".to_string(),
        ureq::Error::ConnectionFailed => "connexion impossible".to_string(),
        ureq::Error::Tls(_) | ureq::Error::Pem(_) | ureq::Error::Rustls(_) => {
            "erreur TLS".to_string()
        }
        ureq::Error::Io(io) => format!("erreur d'entrée-sortie ({})", io.kind()),
        ureq::Error::BadUri(_) => "adresse invalide".to_string(),
        ureq::Error::InvalidProxyUrl => "proxy invalide".to_string(),
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            "redirection refusée".to_string()
        }
        _ => "erreur HTTP".to_string(),
    };
    PrivateApiError::Transport {
        method,
        path: path.to_string(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn debug_masque_les_trois_valeurs() {
        let credentials = Credentials::new(
            "CLE-NE-DOIT-PAS-FUIR".to_string(),
            "SECRET-NE-DOIT-PAS-FUIR".to_string(),
            "PASSPHRASE-NE-DOIT-PAS-FUIR".to_string(),
        )
        .unwrap();
        let shown = format!("{credentials:?}");
        assert!(!shown.contains("NE-DOIT-PAS-FUIR"), "{shown}");
        assert!(shown.contains("***"));
    }

    #[test]
    fn valeurs_vides_refusees() {
        let error = Credentials::new("a".into(), String::new(), "c".into()).unwrap_err();
        assert!(matches!(
            error,
            CredentialsError::EmptyField {
                field: "secret_key"
            }
        ));
    }

    #[test]
    fn environnement_complet_incomplet_et_absent() {
        let complet = [
            (ENV_API_KEY, "k"),
            (ENV_SECRET_KEY, "s"),
            (ENV_PASSPHRASE, "p"),
        ];
        assert!(Credentials::from_env_with(lookup(&complet)).is_ok());
        assert!(matches!(
            Credentials::from_env_with(lookup(&[])),
            Err(CredentialsError::Missing)
        ));
        let sans_secret = [(ENV_API_KEY, "k"), (ENV_PASSPHRASE, "p")];
        let error = Credentials::from_env_with(lookup(&sans_secret)).unwrap_err();
        assert!(
            matches!(&error, CredentialsError::EnvIncomplete { missing } if missing == ENV_SECRET_KEY)
        );
        // Une variable vide vaut une variable absente.
        let vide = [
            (ENV_API_KEY, "k"),
            (ENV_SECRET_KEY, ""),
            (ENV_PASSPHRASE, "p"),
        ];
        assert!(matches!(
            Credentials::from_env_with(lookup(&vide)),
            Err(CredentialsError::EnvIncomplete { .. })
        ));
    }

    #[test]
    fn permission_seule_la_lecture_passe() {
        assert!(check_read_only(Some("read_only")).is_ok());
        assert!(check_read_only(Some(" read_only ")).is_ok());
        for refusee in [
            "read_only,trade",
            "read_only,withdraw",
            "trade",
            "read_only,autre",
        ] {
            let error = check_read_only(Some(refusee)).unwrap_err();
            assert!(
                matches!(error, PrivateApiError::KeyNotReadOnly { .. }),
                "{refusee}"
            );
            assert!(error.is_key_refused());
        }
        for illisible in [None, Some(""), Some("  ")] {
            let error = check_read_only(illisible).unwrap_err();
            assert!(matches!(
                error,
                PrivateApiError::PermissionUnreadable { .. }
            ));
            assert!(error.is_key_refused());
        }
    }

    #[test]
    fn url_de_base_https_ou_machine_locale() {
        assert_eq!(
            validate_base_url("https://eea.okx.com/").unwrap(),
            ("https://eea.okx.com".to_string(), false)
        );
        assert_eq!(
            validate_base_url("http://127.0.0.1:8080").unwrap(),
            ("http://127.0.0.1:8080".to_string(), true)
        );
        assert!(validate_base_url("http://localhost:1").unwrap().1);
        for refusee in [
            "http://eea.okx.com",
            "ftp://127.0.0.1",
            "eea.okx.com",
            "https://user:pass@eea.okx.com",
            "https://eea.okx.com?x=1",
            "https://",
            "http://127.0.0.1.evil.example",
        ] {
            assert!(
                matches!(validate_base_url(refusee), Err(PrivateApiError::BaseUrl)),
                "{refusee}"
            );
        }
    }

    #[test]
    fn redaction_des_valeurs_de_la_cle() {
        let credentials =
            Credentials::new("AAAA1111".into(), "BBBB2222".into(), "CCCC3333".into()).unwrap();
        assert_eq!(
            credentials.redact("clé AAAA1111 et BBBB2222 puis CCCC3333"),
            "clé *** et *** puis ***"
        );
    }

    /// Client de test : aucune requête n'est émise, seul `parse_response` est exercé.
    fn client_de_test() -> PrivateClient {
        let credentials = Credentials::new(
            "CLE-API-SECRETE-0123456789".into(),
            "SECRET-0123456789".into(),
            "PASSPHRASE-0123456789".into(),
        )
        .unwrap();
        let config = PrivateConfig {
            base_url: "http://127.0.0.1:1".into(),
            ..PrivateConfig::default()
        };
        PrivateClient::new(&config, credentials).unwrap()
    }

    /// Plus longue sous-chaîne commune, de la clé API à `texte`, par préfixe de la clé.
    fn plus_long_prefixe_de_la_cle(texte: &str) -> usize {
        let cle = "CLE-API-SECRETE-0123456789";
        (1..=cle.len())
            .rev()
            .find(|n| texte.contains(&cle[..*n]))
            .unwrap_or(0)
    }

    #[test]
    fn la_cle_a_cheval_sur_la_coupure_de_200_caracteres_est_expurgee() {
        let client = client_de_test();
        // 190 caractères de bourrage puis la clé : sans expurgation préalable, la coupure à 200
        // laisserait passer les 5 premiers caractères de la clé.
        for bourrage in [150, 190, 195, 199, 200] {
            let msg = format!(
                "{} key=CLE-API-SECRETE-0123456789 fin",
                "x".repeat(bourrage)
            );
            let body = serde_json::json!({"code": "50113", "msg": msg}).to_string();
            let error = client.parse_response("GET", "/p", 200, &body).unwrap_err();
            let texte = error.to_string();
            assert!(
                plus_long_prefixe_de_la_cle(&texte) < 3,
                "bourrage {bourrage} : {texte}"
            );
            // Même chemin pour la branche statut non 2xx.
            let error = client.parse_response("GET", "/p", 500, &body).unwrap_err();
            let texte = error.to_string();
            assert!(
                plus_long_prefixe_de_la_cle(&texte) < 3,
                "bourrage {bourrage}, HTTP : {texte}"
            );
        }
    }

    #[test]
    fn le_code_http_non_2xx_est_neutralise() {
        let client = client_de_test();
        let body = serde_json::json!({"code": "5\u{1b}[31m\n0", "msg": "m"}).to_string();
        let error = client.parse_response("GET", "/p", 500, &body).unwrap_err();
        let PrivateApiError::Http { detail, .. } = &error else {
            panic!("attendu Http : {error:?}");
        };
        assert!(!detail.contains('\u{1b}'), "{detail:?}");
        assert!(!detail.contains('\n'), "{detail:?}");
        assert!(detail.contains("OKX code 5 [31m 0"), "{detail:?}");
    }

    #[test]
    fn un_code_egal_a_la_passphrase_est_expurge() {
        let client = client_de_test();
        let body = serde_json::json!({"code": "PASSPHRASE-0123456789", "msg": "m"}).to_string();
        for status in [200, 500] {
            let error = client
                .parse_response("GET", "/p", status, &body)
                .unwrap_err();
            let texte = error.to_string();
            assert!(texte.contains("***"), "{texte}");
            assert!(!texte.contains("PASSPHRASE-0123456789"), "{texte}");
        }
    }

    #[test]
    fn sanitize_borne_et_neutralise_les_controles() {
        assert_eq!(sanitize("a\nb\u{1b}c"), "a b c");
        assert_eq!(sanitize(&"x".repeat(500)).len(), MAX_MSG_IN_ERROR);
    }
}
