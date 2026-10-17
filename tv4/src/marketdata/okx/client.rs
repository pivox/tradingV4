//! Client REST OKX : requête, retry, pagination vers le passé (docs/03-architecture.md §3.1 et §11, S7).

use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use thiserror::Error;

use super::bucket::{Jitter, TokenBucket};
use super::{OkxError, OkxPage, okx_bar, parse_page};
use crate::model::{Candle, Timeframe, Ts};

/// Domaine REST de l'EEE (docs/03 §11, S7). Les bougies publiques sont identiques sur `www.okx.com`.
pub const DEFAULT_BASE_URL: &str = "https://eea.okx.com";

/// Maximum accepté par `history-candles` (vérifié le 13/10 : défaut 100, maximum 300).
pub const MAX_LIMIT: u32 = 300;

/// Chemin de l'endpoint.
const HISTORY_CANDLES_PATH: &str = "/api/v5/market/history-candles";

/// Code OKX « trop de requêtes » : même sens qu'un HTTP 429.
const CODE_TOO_MANY_REQUESTS: &str = "50011";

/// Longueur maximale du corps d'erreur recopié dans un message.
const MAX_BODY_IN_ERROR: usize = 200;

/// Réglages du client. Toutes les durées sont injectables : les tests les réduisent à quelques
/// millisecondes, la valeur par défaut est celle de la production.
#[derive(Clone, Debug)]
pub struct OkxConfig {
    /// Racine de l'API, sans `/` final (`https://eea.okx.com`).
    pub base_url: String,
    /// Bougies demandées par requête (`limit`, 300 au maximum).
    pub limit: u32,
    /// Délai global d'une requête HTTP (connexion, envoi, lecture du corps).
    pub timeout: Duration,
    /// Essais au total par requête, le premier compris.
    pub max_attempts: u32,
    /// Attente avant le deuxième essai ; elle double à chaque essai.
    pub backoff_min: Duration,
    /// Plafond de l'attente.
    pub backoff_max: Duration,
    /// Espacement des créneaux du token bucket (125 ms soit 8 requêtes par seconde).
    pub slot_spacing: Duration,
    /// Pause de tout le bucket après un 429 ou un `code = "50011"`.
    pub rate_limit_pause: Duration,
    /// Vrai : l'agent lit `HTTPS_PROXY` et `NO_PROXY` dans l'environnement (défaut de `ureq`). Faux :
    /// connexion directe, ce qu'utilisent les tests sur `127.0.0.1`.
    pub use_env_proxy: bool,
    /// Graine du jitter. Fixe dans les tests ; par défaut tirée de l'horloge.
    pub jitter_seed: u64,
}

impl Default for OkxConfig {
    fn default() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(0));
        OkxConfig {
            base_url: DEFAULT_BASE_URL.to_string(),
            limit: MAX_LIMIT,
            timeout: Duration::from_secs(10),
            max_attempts: 5,
            backoff_min: Duration::from_millis(250),
            backoff_max: Duration::from_secs(8),
            slot_spacing: Duration::from_millis(125),
            rate_limit_pause: Duration::from_secs(2),
            use_env_proxy: true,
            jitter_seed: seed,
        }
    }
}

/// Client OKX. Clonable à bas coût et `Send + Sync` : les clones partagent le pool de connexions de
/// l'`Agent`, le token bucket et le générateur de jitter, donc la cadence globale de 8 requêtes par
/// seconde tient quel que soit le nombre de threads (`std::thread::scope` passe `&client`).
#[derive(Clone, Debug)]
pub struct OkxClient {
    agent: ureq::Agent,
    config: Arc<OkxConfig>,
    bucket: Arc<TokenBucket>,
    jitter: Arc<Jitter>,
}

/// Résultat d'un essai : une réponse utilisable, ou une erreur à retenter, ou une erreur définitive.
enum Attempt {
    Done(Vec<Value>),
    Retry { error: OkxError, rate_limited: bool },
    Fatal(OkxError),
}

impl OkxClient {
    /// Construit le client. L'agent lit le statut HTTP au lieu d'en faire une erreur
    /// (`http_status_as_error(false)`) pour pouvoir classer 429, 5xx et autres 4xx, et porte le délai
    /// global.
    pub fn new(config: OkxConfig) -> Self {
        let mut builder = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .http_status_as_error(false);
        if !config.use_env_proxy {
            builder = builder.proxy(None);
        }
        OkxClient {
            agent: ureq::Agent::new_with_config(builder.build()),
            bucket: Arc::new(TokenBucket::new(config.slot_spacing)),
            jitter: Arc::new(Jitter::new(config.jitter_seed)),
            config: Arc::new(config),
        }
    }

    /// Réglages du client.
    pub fn config(&self) -> &OkxConfig {
        &self.config
    }

    /// Token bucket partagé (les tests le lisent ; la partie suivante pourrait y prendre des créneaux).
    pub fn bucket(&self) -> &TokenBucket {
        &self.bucket
    }

    /// Une page : bougies strictement plus anciennes que `after` (borne exclusive, toujours envoyée).
    /// Renvoie la page décodée et le nombre d'essais utilisés (1 sans retry).
    ///
    /// Chaque essai prend un créneau du bucket. Un 429 ou un `code = "50011"` met en plus tout le
    /// bucket en pause. Seuls les échecs de transport et de statut/enveloppe sont retentés ; une page
    /// reçue mais fausse ([`parse_page`]) échoue tout de suite.
    pub fn fetch_page(
        &self,
        inst_id: &str,
        timeframe: Timeframe,
        after: Ts,
    ) -> Result<(OkxPage, u32), OkxError> {
        let bar = okx_bar(timeframe)?;
        let max_attempts = self.config.max_attempts.max(1);
        let mut attempt = 1;
        loop {
            self.bucket.acquire();
            match self.try_once(inst_id, bar, after) {
                Attempt::Done(data) => {
                    return parse_page(&data, timeframe, after).map(|page| (page, attempt));
                }
                Attempt::Fatal(error) => return Err(error),
                Attempt::Retry {
                    error,
                    rate_limited,
                } => {
                    if rate_limited {
                        self.bucket.pause(self.config.rate_limit_pause);
                    }
                    if attempt >= max_attempts {
                        return Err(OkxError::RetriesExhausted {
                            attempts: attempt,
                            last: Box::new(error),
                        });
                    }
                    thread::sleep(self.backoff(attempt));
                    attempt += 1;
                }
            }
        }
    }

    /// Attente avant l'essai suivant l'essai `attempt` (à partir de 1) : `backoff_min × 2^(attempt-1)`
    /// plafonné à `backoff_max`, puis jitter « équitable » : la moitié fixe plus un tirage dans l'autre
    /// moitié. L'attente reste donc entre `base/2` et `base`, et deux threads ne se resynchronisent pas.
    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.saturating_sub(1).min(20);
        let base = self
            .config
            .backoff_min
            .checked_mul(factor)
            .unwrap_or(self.config.backoff_max)
            .min(self.config.backoff_max);
        let half = base / 2;
        half + self.jitter.up_to(base - half)
    }

    /// Un essai HTTP, sans retry.
    fn try_once(&self, inst_id: &str, bar: &str, after: Ts) -> Attempt {
        let url = format!("{}{HISTORY_CANDLES_PATH}", self.config.base_url);
        let result = self
            .agent
            .get(&url)
            .query("instId", inst_id)
            .query("bar", bar)
            .query("after", after.to_string())
            .query("limit", self.config.limit.to_string())
            .call();
        let mut response = match result {
            Ok(response) => response,
            // Une adresse impossible à lire ne s'arrangera pas en la redemandant.
            Err(error @ ureq::Error::BadUri(_)) => {
                return Attempt::Fatal(OkxError::Transport {
                    message: error.to_string(),
                });
            }
            Err(error) => return transport_retry(&error),
        };
        let status = response.status().as_u16();
        let body = match response.body_mut().read_to_string() {
            Ok(body) => body,
            Err(error) => return transport_retry(&error),
        };
        match status {
            200..=299 => parse_envelope(&body),
            429 => Attempt::Retry {
                error: http_error(status, &body),
                rate_limited: true,
            },
            500..=599 => Attempt::Retry {
                error: http_error(status, &body),
                rate_limited: false,
            },
            _ => Attempt::Fatal(http_error(status, &body)),
        }
    }
}

fn transport_retry(error: &ureq::Error) -> Attempt {
    Attempt::Retry {
        error: OkxError::Transport {
            message: error.to_string(),
        },
        rate_limited: false,
    }
}

fn http_error(status: u16, body: &str) -> OkxError {
    OkxError::Http {
        status,
        body: body.chars().take(MAX_BODY_IN_ERROR).collect(),
    }
}

/// Lit l'enveloppe `{"code","msg","data"}`. `code != "0"` est retenté (cas typique `50011`, qui met le
/// bucket en pause) ; une enveloppe illisible l'est aussi, car un corps tronqué est un incident de
/// transport.
fn parse_envelope(body: &str) -> Attempt {
    let envelope_error = |reason: &str| Attempt::Retry {
        error: OkxError::Envelope {
            reason: reason.to_string(),
        },
        rate_limited: false,
    };
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return envelope_error("JSON invalide");
    };
    let Some(code) = value.get("code").and_then(Value::as_str) else {
        return envelope_error("champ `code` absent");
    };
    if code != "0" {
        let msg = value.get("msg").and_then(Value::as_str).unwrap_or("");
        return Attempt::Retry {
            error: OkxError::Api {
                code: code.to_string(),
                msg: msg.to_string(),
            },
            rate_limited: code == CODE_TOO_MANY_REQUESTS,
        };
    }
    match value.get("data").and_then(Value::as_array) {
        Some(data) => Attempt::Done(data.clone()),
        None => envelope_error("champ `data` absent ou n'est pas un tableau"),
    }
}

/// Compteurs d'une récupération de fenêtre.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FetchStats {
    /// Réponses `code = "0"` reçues, la page vide finale comprise.
    pub pages: u64,
    /// Requêtes HTTP envoyées, retries compris : `requests == pages + retries`.
    pub requests: u64,
    /// Essais supplémentaires (429, 5xx, timeout, `code != "0"`).
    pub retries: u64,
    /// Bougies `confirm = "0"` exclues, dans la fenêtre.
    pub rejected_unclosed: u64,
}

/// Résultat de [`fetch_window`].
#[derive(Clone, Debug, PartialEq)]
pub struct WindowFetch {
    /// Bougies clôturées de `[start, end)`, par `open_time` croissant, sans doublon.
    pub candles: Vec<Candle>,
    /// `open_time` des bougies `confirm = "0"` rejetées dans la fenêtre, croissants : à reporter dans
    /// `rejected_unclosed` du `_manifest.json`.
    pub rejected_unclosed: Vec<Ts>,
    pub stats: FetchStats,
}

/// Erreur de [`fetch_window`] : celle du client OKX, ou celle de `on_page`.
#[derive(Debug, Error)]
pub enum WindowError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Okx(#[from] OkxError),
    #[error("traitement d'une page : {0}")]
    OnPage(#[source] E),
}

/// Récupère les bougies de `[start, end)` en remontant le temps.
///
/// Première requête avec `after = end`, puis `after = ts` de la plus ancienne ligne de la page reçue
/// (bougies non clôturées comprises, pour que le curseur avance toujours). Arrêt sur page vide ou dès
/// qu'une ligne a `ts < start`. Seules les bougies de la fenêtre sont gardées. `before` n'est jamais
/// envoyé.
///
/// `on_page` reçoit chaque page non vide dans l'ordre de réception (donc de plus en plus ancienne),
/// restreinte à la fenêtre : la partie suivante y tamponne les pages par lots de 50. Son erreur arrête
/// la récupération ([`WindowError::OnPage`]). Les bougies sont aussi accumulées dans le résultat, ce qui
/// convient à une fenêtre d'un an de 1m (≈ 525 600 bougies, 30 Mo).
pub fn fetch_window<E, F>(
    client: &OkxClient,
    inst_id: &str,
    timeframe: Timeframe,
    start: Ts,
    end: Ts,
    mut on_page: F,
) -> Result<WindowFetch, WindowError<E>>
where
    E: std::error::Error + 'static,
    F: FnMut(&OkxPage) -> Result<(), E>,
{
    // Le refus d'un timeframe inconnu doit venir avant tout test de fenêtre.
    okx_bar(timeframe)?;
    let mut candles: Vec<Candle> = Vec::new();
    let mut rejected: Vec<Ts> = Vec::new();
    let mut stats = FetchStats::default();
    let window = start..end;
    let mut after = end;
    while !window.is_empty() {
        let (mut page, attempts) = client.fetch_page(inst_id, timeframe, after)?;
        stats.pages += 1;
        stats.requests += u64::from(attempts);
        stats.retries += u64::from(attempts - 1);
        let Some(oldest) = page.oldest_ts else {
            break;
        };
        page.candles.retain(|c| window.contains(&c.open_time_ms));
        page.rejected_unclosed.retain(|ts| window.contains(ts));
        on_page(&page).map_err(WindowError::OnPage)?;
        candles.extend_from_slice(&page.candles);
        rejected.extend_from_slice(&page.rejected_unclosed);
        if oldest < start {
            break;
        }
        after = oldest;
    }
    // Pages reçues du plus récent au plus ancien : on remet l'ordre croissant. Le curseur strict rend
    // un doublon impossible ; la déduplication reste la garantie de sortie (la reprise de la partie
    // suivante retélécharge une page chevauchante).
    candles.sort_by_key(|c| c.open_time_ms);
    candles.dedup_by_key(|c| c.open_time_ms);
    rejected.sort_unstable();
    rejected.dedup();
    stats.rejected_unclosed = rejected.len() as u64;
    Ok(WindowFetch {
        candles,
        rejected_unclosed: rejected,
        stats,
    })
}
