//! `tv4 instruments fetch` sur un serveur HTTP mocké (docs/specs/s9.md §8.8). Aucun accès réseau : le
//! serveur écoute sur `127.0.0.1` (le client désactive alors le proxy de l'environnement).
//!
//! Le mock se comporte comme l'API privée d'OKX : il **recalcule** `OK-ACCESS-SIGN` avec un secret de
//! test et répond `50113` si la signature, la clé, la passphrase ou l'horodatage sont faux. Il tient
//! un journal des requêtes vues et du résultat de la vérification.
//!
//! Les clés de test sont des valeurs factices évidentes, choisies uniques pour qu'une fuite dans la
//! sortie standard, la sortie d'erreur, le fichier produit ou un `Debug` soit détectée par une simple
//! recherche de sous-chaîne.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use httpmock::prelude::*;
use httpmock::{HttpMockRequest, HttpMockResponse};
use serde_json::{Value, json};
use tv4::marketdata::okx::instruments::{InstrumentsError, InstrumentsFile};
use tv4::marketdata::okx::private::{
    Credentials, CredentialsError, PrivateApiError, PrivateClient, PrivateConfig,
};
use tv4::marketdata::okx::sign::sign;

/// Heure du serveur mocké : 2023-11-14T22:13:20.123Z (loin de l'horloge locale, pour prouver que
/// l'horodatage de signature vient de `public/time`).
const SERVER_TS: i64 = 1_700_000_000_123;

const TEST_KEY: &str = "CLE-NE-DOIT-PAS-FUIR-0001";
const TEST_SECRET: &str = "SECRET-NE-DOIT-PAS-FUIR-0002";
const TEST_PASSPHRASE: &str = "PASSPHRASE-NE-DOIT-PAS-FUIR-0003";
const SECRETS: [&str; 3] = [TEST_KEY, TEST_SECRET, TEST_PASSPHRASE];

/// Nom du fichier daté que la commande écrit pour `SERVER_TS`.
const DEFAULT_FILE_NAME: &str = "spot-20231114T221320Z.json";

// --- Serveur mocké ------------------------------------------------------------------------------

/// Une requête reçue.
struct Seen {
    /// Chemin et chaîne de requête, tels que reçus.
    uri: String,
    headers: Vec<(String, String)>,
    /// La signature et les autres en-têtes d'identification correspondent aux valeurs de test.
    signed_ok: bool,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Comportement du mock.
struct Plan {
    /// Valeur de `perm` rendue par `account/config` (`None` : champ absent).
    perm: Option<&'static str>,
    /// Réponse de `account/instruments` quand la signature est bonne.
    instruments: (u16, String),
    /// Heure rendue par `public/time`, en millisecondes.
    server_ts: i64,
}

impl Plan {
    fn standard() -> Plan {
        Plan {
            perm: Some("read_only"),
            instruments: (200, envelope(&okx_rows())),
            server_ts: SERVER_TS,
        }
    }
}

/// Requête signée vue par le mock : chemin, en-têtes, résultat de la vérification.
type SignedRequest = (String, Vec<(String, String)>, bool);

struct Api {
    server: MockServer,
    seen: Arc<Mutex<Vec<Seen>>>,
    time_hits: Arc<AtomicUsize>,
    config_hits: Arc<AtomicUsize>,
    instruments_hits: Arc<AtomicUsize>,
    redirect_hits: Arc<AtomicUsize>,
}

fn envelope(rows: &[Value]) -> String {
    json!({"code": "0", "msg": "", "data": rows}).to_string()
}

fn headers_of(request: &HttpMockRequest) -> Vec<(String, String)> {
    request.headers_vec().clone()
}

/// Chemin et chaîne de requête tels que reçus (le mock rend l'URI complète, hôte compris).
fn path_and_query(request: &HttpMockRequest) -> String {
    request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_default()
}

/// Vérifie une requête signée comme le fait OKX : en-têtes présents, clé et passphrase attendues,
/// signature recalculée sur `horodatage + GET + chemin?requête`.
fn signature_is_valid(request: &HttpMockRequest) -> bool {
    let headers = headers_of(request);
    let get = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let (Some(key), Some(signature), Some(timestamp), Some(passphrase)) = (
        get("OK-ACCESS-KEY"),
        get("OK-ACCESS-SIGN"),
        get("OK-ACCESS-TIMESTAMP"),
        get("OK-ACCESS-PASSPHRASE"),
    ) else {
        return false;
    };
    key == TEST_KEY
        && passphrase == TEST_PASSPHRASE
        && signature == sign(TEST_SECRET, timestamp, "GET", &path_and_query(request), "")
}

fn invalid_sign_response() -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(401)
        .body(json!({"code": "50113", "msg": "Invalid Sign", "data": []}).to_string())
        .build()
}

impl Api {
    fn start(plan: Plan) -> Api {
        let server = MockServer::start();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let counter = || Arc::new(AtomicUsize::new(0));
        let (time_hits, config_hits, instruments_hits, redirect_hits) =
            (counter(), counter(), counter(), counter());

        let hits = Arc::clone(&time_hits);
        let journal = Arc::clone(&seen);
        let ts = plan.server_ts;
        server.mock(|when, then| {
            when.method(GET).path("/api/v5/public/time");
            then.respond_with(move |request: &HttpMockRequest| {
                hits.fetch_add(1, Ordering::SeqCst);
                journal.lock().unwrap().push(Seen {
                    uri: path_and_query(request),
                    headers: headers_of(request),
                    signed_ok: false,
                });
                HttpMockResponse::builder()
                    .status(200)
                    .body(
                        json!({"code": "0", "msg": "", "data": [{"ts": ts.to_string()}]})
                            .to_string(),
                    )
                    .build()
            });
        });

        let hits = Arc::clone(&config_hits);
        let journal = Arc::clone(&seen);
        let perm = plan.perm;
        server.mock(|when, then| {
            when.method(GET).path("/api/v5/account/config");
            then.respond_with(move |request: &HttpMockRequest| {
                hits.fetch_add(1, Ordering::SeqCst);
                let signed_ok = signature_is_valid(request);
                journal.lock().unwrap().push(Seen {
                    uri: path_and_query(request),
                    headers: headers_of(request),
                    signed_ok,
                });
                if !signed_ok {
                    return invalid_sign_response();
                }
                let row = match perm {
                    Some(perm) => json!({"uid": "1", "perm": perm}),
                    None => json!({"uid": "1"}),
                };
                HttpMockResponse::builder()
                    .status(200)
                    .body(envelope(&[row]))
                    .build()
            });
        });

        let hits = Arc::clone(&instruments_hits);
        let journal = Arc::clone(&seen);
        let (status, body) = plan.instruments;
        server.mock(|when, then| {
            when.method(GET).path("/api/v5/account/instruments");
            then.respond_with(move |request: &HttpMockRequest| {
                hits.fetch_add(1, Ordering::SeqCst);
                let signed_ok = signature_is_valid(request);
                journal.lock().unwrap().push(Seen {
                    uri: path_and_query(request),
                    headers: headers_of(request),
                    signed_ok,
                });
                if !signed_ok {
                    return invalid_sign_response();
                }
                if status == 302 {
                    return HttpMockResponse::builder()
                        .status(302)
                        .header("Location", "/redirige-ici")
                        .body("")
                        .build();
                }
                HttpMockResponse::builder()
                    .status(status)
                    .body(body.clone())
                    .build()
            });
        });

        let hits = Arc::clone(&redirect_hits);
        server.mock(|when, then| {
            when.method(GET).path("/redirige-ici");
            then.respond_with(move |_: &HttpMockRequest| {
                hits.fetch_add(1, Ordering::SeqCst);
                HttpMockResponse::builder().status(200).body("{}").build()
            });
        });

        Api {
            server,
            seen,
            time_hits,
            config_hits,
            instruments_hits,
            redirect_hits,
        }
    }

    fn url(&self) -> String {
        self.server.base_url()
    }

    fn instruments_hits(&self) -> usize {
        self.instruments_hits.load(Ordering::SeqCst)
    }

    fn total_hits(&self) -> usize {
        self.time_hits.load(Ordering::SeqCst)
            + self.config_hits.load(Ordering::SeqCst)
            + self.instruments_hits.load(Ordering::SeqCst)
    }

    fn signed_requests(&self) -> Vec<SignedRequest> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.header("OK-ACCESS-SIGN").is_some())
            .map(|seen| (seen.uri.clone(), seen.headers.clone(), seen.signed_ok))
            .collect()
    }
}

/// Réponse OKX de test : instruments fictifs (valeurs inventées, pas celles d'OKX), dans le désordre.
fn okx_rows() -> Vec<Value> {
    vec![
        json!({
            "instId": "SOL-USDC", "baseCcy": "SOL", "quoteCcy": "USDC",
            "tickSz": "0.001", "lotSz": "0.000001", "minSz": "0.01",
            "state": "live", "listTime": "1755763200000", "ruleType": "normal",
            "tradeQuoteCcyList": ["USDC"],
            "upcChg": [{"param": "minSz", "newValue": "0.02", "effTime": "1798761600000"}]
        }),
        json!({
            "instId": "ETH-USDC", "baseCcy": "ETH", "quoteCcy": "USDC",
            "tickSz": "0.01", "lotSz": "0.000001", "minSz": "0.001",
            "state": "live", "listTime": "1755763200000", "ruleType": "normal",
            "tradeQuoteCcyList": ["USDC"], "upcChg": []
        }),
        json!({
            "instId": "BTC-USDC", "baseCcy": "BTC", "quoteCcy": "USDC",
            "tickSz": "0.1", "lotSz": "0.00000001", "minSz": "0.00001",
            "state": "live", "listTime": "1755763200000", "ruleType": "normal",
            "tradeQuoteCcyList": ["USDC"], "upcChg": []
        }),
        json!({
            "instId": "ADA-USDC", "baseCcy": "ADA", "quoteCcy": "USDC",
            "tickSz": "0.0001", "lotSz": "0.000001", "minSz": "1",
            "state": "suspend", "listTime": "1755763200000", "ruleType": "normal",
            "tradeQuoteCcyList": ["USDC"], "upcChg": []
        }),
    ]
}

// --- Exécutable et fichiers de clé ---------------------------------------------------------------

const PROXY_VARIABLES: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

/// Lance `tv4`. L'environnement du sous-processus est maîtrisé : ni proxy, ni variable de clé sauf
/// celles de `env` (pas de `set_var` global dans ce processus de test).
fn tv4(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tv4"));
    command.args(args);
    for variable in PROXY_VARIABLES {
        command.env_remove(variable);
    }
    for variable in [
        "TV4_OKX_API_KEY",
        "TV4_OKX_SECRET_KEY",
        "TV4_OKX_PASSPHRASE",
    ] {
        command.env_remove(variable);
    }
    for (name, value) in env {
        command.env(name, value);
    }
    command
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Écrit un fichier de clé TOML avec les droits `mode`.
fn write_key_file(dir: &Path, name: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        format!(
            "api_key = \"{TEST_KEY}\"\nsecret_key = \"{TEST_SECRET}\"\npassphrase = \"{TEST_PASSPHRASE}\"\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

/// `tv4 instruments fetch --root <root> --credentials <key> --base-url <url>` plus `extra`.
fn fetch(root: &Path, key: &Path, api: &Api, extra: &[&str]) -> Output {
    let mut args = vec![
        "instruments".to_string(),
        "fetch".to_string(),
        "--root".to_string(),
        root.display().to_string(),
        "--credentials".to_string(),
        key.display().to_string(),
        "--base-url".to_string(),
        api.url(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_string()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    tv4(&refs, &[])
}

fn produced_file(root: &Path) -> PathBuf {
    root.join("instruments").join("okx").join(DEFAULT_FILE_NAME)
}

/// Aucune des trois valeurs de la clé, ni signature ni en-tête d'identification, dans `texts`.
fn assert_no_leak(api: &Api, texts: &[&str]) {
    let signatures: Vec<String> = api
        .signed_requests()
        .iter()
        .flat_map(|(_, headers, _)| {
            headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("OK-ACCESS-SIGN"))
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    for shown in texts {
        for secret in SECRETS {
            assert!(!shown.contains(secret), "fuite de {secret} dans :\n{shown}");
        }
        for signature in &signatures {
            assert!(
                !shown.contains(signature.as_str()),
                "signature dans :\n{shown}"
            );
        }
        assert!(
            !shown.contains("OK-ACCESS"),
            "nom d'en-tête dans :\n{shown}"
        );
    }
}

/// Instant en millisecondes d'un horodatage `AAAA-MM-JJTHH:MM:SS.mmmZ` (algorithme de Hinnant).
fn parse_iso_ms(iso: &str) -> i64 {
    assert_eq!(iso.len(), 24, "{iso}");
    assert!(iso.ends_with('Z'), "{iso}");
    let number = |range: std::ops::Range<usize>| iso[range].parse::<i64>().unwrap();
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days * 86_400_000
        + number(11..13) * 3_600_000
        + number(14..16) * 60_000
        + number(17..19) * 1_000
        + number(20..23)
}

// --- Signature, en-têtes, horodatage ------------------------------------------------------------

#[test]
fn en_tetes_et_signature_verifies_par_le_mock_avec_l_heure_du_serveur() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "okx-test.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));

    // Trois requêtes : public/time (non signée), puis account/config et account/instruments signées.
    assert_eq!(api.total_hits(), 3);
    let signed = api.signed_requests();
    assert_eq!(signed.len(), 2);
    assert_eq!(signed[0].0, "/api/v5/account/config");
    assert_eq!(signed[1].0, "/api/v5/account/instruments?instType=SPOT");
    for (uri, headers, signed_ok) in &signed {
        assert!(signed_ok, "signature fausse pour {uri}");
        let get = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("OK-ACCESS-KEY"), Some(TEST_KEY));
        assert_eq!(get("OK-ACCESS-PASSPHRASE"), Some(TEST_PASSPHRASE));
        assert_eq!(get("Content-Type"), Some("application/json"));
        // L'horodatage vient de `public/time` : à moins de 5 s de l'heure du serveur mocké (2023),
        // jamais de l'horloge locale.
        let timestamp = get("OK-ACCESS-TIMESTAMP").expect("horodatage");
        let ms = parse_iso_ms(timestamp);
        assert!(
            (SERVER_TS..SERVER_TS + 5_000).contains(&ms),
            "horodatage {timestamp} loin de l'heure du serveur"
        );
    }
    // La requête non signée ne porte aucun en-tête d'identification.
    let seen = api.seen.lock().unwrap();
    let time = seen
        .iter()
        .find(|seen| seen.uri == "/api/v5/public/time")
        .expect("public/time appelée");
    assert!(time.header("OK-ACCESS-KEY").is_none());
    assert!(time.header("OK-ACCESS-SIGN").is_none());
}

#[test]
fn le_mock_rejette_une_signature_fausse() {
    // Preuve que le mock vérifie vraiment : un client dont le secret diffère reçoit 50113.
    let api = Api::start(Plan::standard());
    let credentials = Credentials::new(
        TEST_KEY.to_string(),
        "AUTRE-SECRET-FACTICE".to_string(),
        TEST_PASSPHRASE.to_string(),
    )
    .unwrap();
    let config = PrivateConfig {
        base_url: api.url(),
        ..PrivateConfig::default()
    };
    let mut client = PrivateClient::new(&config, credentials).unwrap();
    let error = client.account_perm().unwrap_err();
    let shown = error.to_string();
    assert!(shown.contains("50113"), "{shown}");
    assert!(shown.contains("Invalid Sign"), "{shown}");
    assert!(!shown.contains("AUTRE-SECRET-FACTICE"));
    assert!(!api.signed_requests()[0].2);
}

// --- Permission de la clé -----------------------------------------------------------------------

#[test]
fn permission_lecture_seule_acceptee() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(api.instruments_hits(), 1);
}

#[test]
fn permission_trade_withdraw_inconnue_ou_absente_donne_le_code_2_sans_requete_d_instruments() {
    for perm in [
        Some("read_only,trade"),
        Some("read_only,withdraw"),
        Some("trade"),
        Some("read_only,autre"),
        None,
    ] {
        let api = Api::start(Plan {
            perm,
            ..Plan::standard()
        });
        let dir = tempfile::tempdir().unwrap();
        let key = write_key_file(dir.path(), "k.toml", 0o600);
        let out = fetch(dir.path(), &key, &api, &[]);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "perm {perm:?} : {stderr}");
        assert_eq!(api.instruments_hits(), 0, "perm {perm:?}");
        assert!(
            !dir.path().join("instruments").exists(),
            "perm {perm:?} : aucun fichier ne doit être écrit"
        );
        assert_no_leak(&api, &[&stderr, &text(&out.stdout)]);
    }
}

// --- Réponses en erreur -------------------------------------------------------------------------

#[test]
fn erreur_okx_dans_l_enveloppe_donne_le_code_1_avec_code_et_msg() {
    let body = json!({"code": "50113", "msg": "Invalid Sign", "data": []}).to_string();
    let api = Api::start(Plan {
        instruments: (200, body),
        ..Plan::standard()
    });
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("50113"), "{stderr}");
    assert!(stderr.contains("Invalid Sign"), "{stderr}");
    assert!(stderr.contains("/api/v5/account/instruments"), "{stderr}");
    assert!(!dir.path().join("instruments").exists());
    assert_no_leak(&api, &[&stderr]);
}

#[test]
fn statut_http_en_erreur_donne_le_code_1_avec_statut_code_et_msg() {
    let body = json!({"code": "50114", "msg": "Invalid Authorization", "data": []}).to_string();
    let api = Api::start(Plan {
        instruments: (401, body),
        ..Plan::standard()
    });
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("HTTP 401"), "{stderr}");
    assert!(stderr.contains("50114"), "{stderr}");
    assert_no_leak(&api, &[&stderr]);
}

#[test]
fn enveloppe_illisible_et_instrument_incomplet_donnent_le_code_1() {
    for body in [
        "pas du json".to_string(),
        json!({"msg": "", "data": []}).to_string(),
        json!({"code": "0", "msg": ""}).to_string(),
        // `state` absent : champ obligatoire.
        envelope(&[json!({"instId": "BTC-USDC", "tickSz": "0.1", "lotSz": "1", "minSz": "1"})]),
    ] {
        let api = Api::start(Plan {
            instruments: (200, body.clone()),
            ..Plan::standard()
        });
        let dir = tempfile::tempdir().unwrap();
        let key = write_key_file(dir.path(), "k.toml", 0o600);
        let out = fetch(dir.path(), &key, &api, &[]);
        assert_eq!(out.status.code(), Some(1), "{body} : {}", text(&out.stderr));
        assert!(!dir.path().join("instruments").exists());
    }
}

#[test]
fn le_message_d_okx_qui_cite_la_cle_est_expurge() {
    // Un serveur (ou un proxy) qui renverrait la clé dans `msg` ne la fait pas apparaître.
    let body =
        json!({"code": "50111", "msg": format!("Invalid key {TEST_KEY}"), "data": []}).to_string();
    let api = Api::start(Plan {
        instruments: (200, body),
        ..Plan::standard()
    });
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr.contains("50111"), "{stderr}");
    assert!(stderr.contains("***"), "{stderr}");
    assert_no_leak(&api, &[&stderr, &text(&out.stdout)]);
}

#[test]
fn une_redirection_n_est_pas_suivie() {
    let api = Api::start(Plan {
        instruments: (302, String::new()),
        ..Plan::standard()
    });
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("HTTP 302"), "{stderr}");
    assert_eq!(
        api.redirect_hits.load(Ordering::SeqCst),
        0,
        "les en-têtes signés ne doivent pas suivre une redirection"
    );
}

#[test]
fn schema_d_url_refuse_sans_aucune_requete() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    for url in [
        "http://eea.okx.com",
        "ftp://127.0.0.1",
        "https://u:p@eea.okx.com",
    ] {
        let out = tv4(
            &[
                "instruments",
                "fetch",
                "--root",
                &dir.path().display().to_string(),
                "--credentials",
                &key.display().to_string(),
                "--base-url",
                url,
            ],
            &[],
        );
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{url} : {stderr}");
        assert!(stderr.contains("URL de base refusée"), "{stderr}");
        // L'URL (et ses éventuels identifiants) n'est pas recopiée.
        assert!(!stderr.contains("u:p@"), "{stderr}");
    }
    assert_eq!(api.total_hits(), 0);
}

// --- Fichier produit ----------------------------------------------------------------------------

#[test]
fn fichier_produit_trie_chaines_intactes_hash_et_sortie_standard() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));

    let path = produced_file(dir.path());
    let raw = fs::read_to_string(&path).expect("fichier daté sous instruments/okx");
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["schema"], "tv4-instruments-v1");
    assert_eq!(value["exchange"], "okx");
    assert_eq!(value["site"], "other");
    assert_eq!(value["endpoint"], "/api/v5/account/instruments");
    assert_eq!(value["inst_type"], "SPOT");
    assert_eq!(value["fetched_at"], "2023-11-14T22:13:20.123Z");
    assert_eq!(value["server_time_ms"], SERVER_TS);
    // Triés par inst_id, valeurs gardées en chaînes telles qu'OKX les donne.
    let ids: Vec<&str> = value["instruments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["inst_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["ADA-USDC", "BTC-USDC", "ETH-USDC", "SOL-USDC"]);
    let btc = &value["instruments"][1];
    assert_eq!(btc["tick_sz"], "0.1");
    assert_eq!(btc["lot_sz"], "0.00000001");
    assert_eq!(btc["min_sz"], "0.00001");
    assert_eq!(btc["list_time"], "1755763200000");
    assert_eq!(btc["trade_quote_ccy_list"], json!(["USDC"]));
    // `upcChg` conservé tel quel.
    assert_eq!(
        value["instruments"][3]["upc_chg"],
        json!([{"param": "minSz", "newValue": "0.02", "effTime": "1798761600000"}])
    );

    // Le fichier se recharge, son hash est vérifié, et la sortie standard le rappelle.
    let loaded = InstrumentsFile::load(&path).unwrap();
    assert_eq!(value["instruments_hash"], loaded.instruments_hash.as_str());
    let stdout = text(&out.stdout);
    assert!(stdout.contains(&path.display().to_string()), "{stdout}");
    assert!(stdout.contains(&loaded.instruments_hash), "{stdout}");
    assert!(stdout.contains("instruments : 4"), "{stdout}");
    assert!(stdout.contains("BTC-USDC : listée, state live"), "{stdout}");
    assert!(stdout.contains("ETH-USDC : listée, state live"), "{stdout}");
    assert!(!stdout.contains("USDT"), "{stdout}");
    assert_no_leak(&api, &[&stdout, &text(&out.stderr), &raw]);
}

#[test]
fn la_sortie_signale_une_paire_usdt_et_une_paire_absente() {
    let mut rows = okx_rows();
    rows.retain(|row| row["instId"] != "ETH-USDC");
    rows.push(json!({
        "instId": "BTC-USDT", "baseCcy": "BTC", "quoteCcy": "USDT",
        "tickSz": "0.1", "lotSz": "0.00000001", "minSz": "0.00001", "state": "live"
    }));
    let api = Api::start(Plan {
        instruments: (200, envelope(&rows)),
        ..Plan::standard()
    });
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(stdout.contains("ETH-USDC : absente"), "{stdout}");
    assert!(stdout.contains("attention : 1 paire(s) USDT"), "{stdout}");
    assert!(stdout.contains("BTC-USDT"), "{stdout}");
}

#[test]
fn hash_stable_entre_deux_lectures_de_contenu_egal_et_different_si_un_min_sz_change() {
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let hash_of = |plan: Plan, out_name: &str| -> String {
        let api = Api::start(plan);
        let out_path = dir.path().join(out_name);
        let out = fetch(
            dir.path(),
            &key,
            &api,
            &["--out", &out_path.display().to_string()],
        );
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        InstrumentsFile::load(&out_path).unwrap().instruments_hash
    };
    let first = hash_of(Plan::standard(), "a.json");
    // Même contenu, autre heure de serveur et autre ordre de réponse : même hash.
    let mut shuffled = okx_rows();
    shuffled.reverse();
    let second = hash_of(
        Plan {
            instruments: (200, envelope(&shuffled)),
            server_ts: SERVER_TS + 3_600_000,
            ..Plan::standard()
        },
        "b.json",
    );
    assert_eq!(first, second);
    // Un min_sz modifié change le hash.
    let mut changed = okx_rows();
    for row in &mut changed {
        if row["instId"] == "BTC-USDC" {
            row["minSz"] = json!("0.0001");
        }
    }
    let third = hash_of(
        Plan {
            instruments: (200, envelope(&changed)),
            ..Plan::standard()
        },
        "c.json",
    );
    assert_ne!(first, third);
}

#[test]
fn refus_d_ecraser_un_fichier_existant() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    assert_eq!(fetch(dir.path(), &key, &api, &[]).status.code(), Some(0));
    let path = produced_file(dir.path());
    let before = fs::read(&path).unwrap();

    // Même heure de serveur donc même nom de fichier : refus, contenu intact.
    let hits = api.total_hits();
    let again = fetch(dir.path(), &key, &api, &[]);
    assert_eq!(again.status.code(), Some(1));
    assert!(
        text(&again.stderr).contains("existe déjà"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(api.total_hits() > hits);

    // Avec --out sur un fichier existant : échec avant toute requête.
    let hits = api.total_hits();
    let explicit = fetch(
        dir.path(),
        &key,
        &api,
        &["--out", &path.display().to_string()],
    );
    assert_eq!(explicit.status.code(), Some(1));
    assert_eq!(api.total_hits(), hits);
    assert_eq!(fs::read(&path).unwrap(), before);
    // Aucun fichier temporaire ne reste.
    let names: Vec<String> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [DEFAULT_FILE_NAME]);
}

// --- Absence de fuite ---------------------------------------------------------------------------

#[test]
fn aucune_fuite_en_succes_en_erreur_et_dans_les_debug() {
    // Succès : sortie standard, sortie d'erreur et fichier produit.
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = fetch(dir.path(), &key, &api, &[]);
    let file = fs::read_to_string(produced_file(dir.path())).unwrap();
    assert_no_leak(&api, &[&text(&out.stdout), &text(&out.stderr), &file]);

    // Debug de `Credentials`, du client et d'erreurs réelles.
    let credentials = Credentials::from_file(&key).unwrap();
    let mut shown = vec![format!("{credentials:?}"), format!("{credentials:#?}")];
    let config = PrivateConfig {
        base_url: api.url(),
        ..PrivateConfig::default()
    };
    let mut client = PrivateClient::new(&config, credentials).unwrap();
    shown.push(format!("{client:?}"));
    // Une erreur d'API dont le `msg` cite la clé.
    let echo = Api::start(Plan {
        instruments: (
            200,
            json!({"code": "50111", "msg": format!("{TEST_KEY} {TEST_SECRET} {TEST_PASSPHRASE}")})
                .to_string(),
        ),
        ..Plan::standard()
    });
    let mut echo_client = PrivateClient::new(
        &PrivateConfig {
            base_url: echo.url(),
            ..PrivateConfig::default()
        },
        Credentials::from_file(&key).unwrap(),
    )
    .unwrap();
    let error = echo_client.account_instruments("SPOT").unwrap_err();
    shown.push(format!("{error}"));
    shown.push(format!("{error:?}"));
    // Une erreur de transport : port local fermé (listener lié puis relâché).
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut closed_client = PrivateClient::new(
        &PrivateConfig {
            base_url: format!("http://127.0.0.1:{closed_port}"),
            ..PrivateConfig::default()
        },
        Credentials::from_file(&key).unwrap(),
    )
    .unwrap();
    let error = closed_client.account_instruments("SPOT").unwrap_err();
    shown.push(format!("{error}"));
    shown.push(format!("{error:?}"));
    assert!(matches!(error, PrivateApiError::Transport { .. }));
    // Perm refusée.
    shown.push(format!("{:?}", client.server_time_ms()));
    let refused = tv4::marketdata::okx::private::check_read_only(Some("read_only,trade"));
    shown.push(format!("{:?}", refused.unwrap_err()));
    for text in &shown {
        for secret in SECRETS {
            assert!(!text.contains(secret), "fuite de {secret} dans :\n{text}");
        }
    }
}

// --- Garde du fichier de clé --------------------------------------------------------------------

#[test]
fn fichier_de_cle_en_mode_0644_refuse_avec_le_code_2_sans_aucune_requete() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o644);
    let out = fetch(dir.path(), &key, &api, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("0644"), "{stderr}");
    assert_eq!(api.total_hits(), 0);
    assert_no_leak(&api, &[&stderr, &text(&out.stdout)]);
    // Même refus pour des droits de groupe seuls (0640) ou d'écriture (0602).
    for mode in [0o640, 0o602, 0o660, 0o604] {
        fs::set_permissions(&key, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            fetch(dir.path(), &key, &api, &[]).status.code(),
            Some(2),
            "{mode:o}"
        );
    }
    assert_eq!(api.total_hits(), 0);
}

#[test]
fn modes_0600_et_0400_acceptes() {
    for (mode, name) in [(0o600, "a.toml"), (0o400, "b.toml")] {
        let api = Api::start(Plan::standard());
        let dir = tempfile::tempdir().unwrap();
        let key = write_key_file(dir.path(), name, mode);
        let out = fetch(dir.path(), &key, &api, &[]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{mode:o} : {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn lien_symbolique_vers_la_cle_refuse() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let real = write_key_file(dir.path(), "reel.toml", 0o600);
    let link = dir.path().join("lien.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let out = fetch(dir.path(), &link, &api, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("lien symbolique"), "{stderr}");
    assert_eq!(api.total_hits(), 0);
}

#[test]
fn fichier_de_cle_qui_n_est_pas_un_fichier_regulier_refuse() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("dossier.toml");
    fs::create_dir(&folder).unwrap();
    let out = fetch(dir.path(), &folder, &api, &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(api.total_hits(), 0);
}

#[test]
fn fichier_de_cle_sous_un_dossier_qui_contient_dot_git_refuse() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    // `.git` en dossier, puis en fichier (worktree), à deux niveaux au-dessus de la clé.
    for dot_git_is_dir in [true, false] {
        let repo = tempfile::tempdir_in(dir.path()).unwrap();
        let dot_git = repo.path().join(".git");
        if dot_git_is_dir {
            fs::create_dir(&dot_git).unwrap();
        } else {
            fs::write(&dot_git, "gitdir: /ailleurs\n").unwrap();
        }
        let nested = repo.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let key = write_key_file(&nested, "k.toml", 0o600);
        let out = fetch(dir.path(), &key, &api, &[]);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("dépôt git"), "{stderr}");
        assert_no_leak(&api, &[&stderr]);
    }
    assert_eq!(api.total_hits(), 0);
}

#[test]
fn fichier_de_cle_malforme_ou_incomplet_sans_citer_son_contenu() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        // Champ inconnu.
        format!(
            "api_key = \"{TEST_KEY}\"\nsecret_key = \"{TEST_SECRET}\"\npassphrase = \"{TEST_PASSPHRASE}\"\nextra = \"x\"\n"
        ),
        // Champ manquant.
        format!("api_key = \"{TEST_KEY}\"\nsecret_key = \"{TEST_SECRET}\"\n"),
        // TOML invalide, avec un secret sur la ligne fautive.
        format!("api_key = {TEST_KEY}\n"),
        // Valeur vide.
        format!(
            "api_key = \"{TEST_KEY}\"\nsecret_key = \"\"\npassphrase = \"{TEST_PASSPHRASE}\"\n"
        ),
    ];
    for (index, content) in cases.iter().enumerate() {
        let path = dir.path().join(format!("m{index}.toml"));
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let out = fetch(dir.path(), &path, &api, &[]);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "cas {index} : {stderr}");
        assert_no_leak(&api, &[&stderr, &text(&out.stdout)]);
    }
    assert_eq!(api.total_hits(), 0);
}

#[test]
fn fichier_de_cle_inexistant_refuse_sans_requete() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let out = fetch(dir.path(), &dir.path().join("absent.toml"), &api, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(api.total_hits(), 0);
}

// --- Variables d'environnement ------------------------------------------------------------------

fn fetch_with_env(root: &Path, api: &Api, env: &[(&str, &str)]) -> Output {
    tv4(
        &[
            "instruments",
            "fetch",
            "--root",
            &root.display().to_string(),
            "--base-url",
            &api.url(),
        ],
        env,
    )
}

#[test]
fn cle_lue_dans_les_trois_variables_d_environnement() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let out = fetch_with_env(
        dir.path(),
        &api,
        &[
            ("TV4_OKX_API_KEY", TEST_KEY),
            ("TV4_OKX_SECRET_KEY", TEST_SECRET),
            ("TV4_OKX_PASSPHRASE", TEST_PASSPHRASE),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(api.instruments_hits(), 1);
    assert!(api.signed_requests().iter().all(|(_, _, ok)| *ok));
    assert_no_leak(&api, &[&text(&out.stdout), &text(&out.stderr)]);
}

#[test]
fn une_variable_manquante_ou_vide_refuse_sans_requete_et_sans_fuite() {
    let all = [
        ("TV4_OKX_API_KEY", TEST_KEY),
        ("TV4_OKX_SECRET_KEY", TEST_SECRET),
        ("TV4_OKX_PASSPHRASE", TEST_PASSPHRASE),
    ];
    for missing in 0..3 {
        let api = Api::start(Plan::standard());
        let dir = tempfile::tempdir().unwrap();
        let env: Vec<(&str, &str)> = all
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != missing)
            .map(|(_, pair)| *pair)
            .collect();
        let out = fetch_with_env(dir.path(), &api, &env);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains(all[missing].0), "{stderr}");
        assert_eq!(api.total_hits(), 0);
        assert_no_leak(&api, &[&stderr, &text(&out.stdout)]);
    }
    // Variable vide : même refus.
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let out = fetch_with_env(
        dir.path(),
        &api,
        &[
            ("TV4_OKX_API_KEY", TEST_KEY),
            ("TV4_OKX_SECRET_KEY", ""),
            ("TV4_OKX_PASSPHRASE", TEST_PASSPHRASE),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(api.total_hits(), 0);
}

#[test]
fn aucune_source_de_cle_nomme_les_deux_sources() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let out = fetch_with_env(dir.path(), &api, &[]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("--credentials"), "{stderr}");
    assert!(stderr.contains("TV4_OKX_API_KEY"), "{stderr}");
    assert_eq!(api.total_hits(), 0);
}

#[test]
fn le_fichier_est_prefere_aux_variables() {
    let api = Api::start(Plan::standard());
    let dir = tempfile::tempdir().unwrap();
    let key = write_key_file(dir.path(), "k.toml", 0o600);
    let out = tv4(
        &[
            "instruments",
            "fetch",
            "--root",
            &dir.path().display().to_string(),
            "--credentials",
            &key.display().to_string(),
            "--base-url",
            &api.url(),
        ],
        &[("TV4_OKX_API_KEY", "AUTRE-CLE-FACTICE")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

// --- Lecture du fichier par le run (utilisée par F) ---------------------------------------------

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("okx_instruments_spot.json")
}

#[test]
fn la_fixture_est_celle_que_produit_le_code() {
    // La fixture est un fichier d'instruments de test, FAUX (valeurs inventées) et sans secret. Elle
    // doit rester identique à ce que `from_response` produit sur `okx_rows()` : si le format change,
    // la régénérer avec TV4_FIXTURE_UPDATE=1 puis relire le diff.
    let file = InstrumentsFile::from_response("eea", "SPOT", SERVER_TS, &okx_rows()).unwrap();
    let expected = file.to_bytes().unwrap();
    if std::env::var_os("TV4_FIXTURE_UPDATE").is_some() {
        fs::create_dir_all(fixture_path().parent().unwrap()).unwrap();
        fs::write(fixture_path(), &expected).unwrap();
    }
    let committed = fs::read(fixture_path()).expect("fixture présente");
    assert_eq!(committed, expected, "fixture périmée");
}

#[test]
fn chargement_de_la_fixture_et_conversion_en_regles() {
    let file = InstrumentsFile::load(&fixture_path()).unwrap();
    assert_eq!(file.instruments.len(), 4);
    let btc = file.to_rules("BTC-USDC").unwrap();
    assert_eq!(
        (btc.tick_size, btc.lot_size, btc.min_qty, btc.min_notional),
        (0.1, 1e-8, 0.00001, 0.0)
    );
    let eth = file.to_rules("ETH-USDC").unwrap();
    assert_eq!(
        (eth.tick_size, eth.lot_size, eth.min_qty),
        (0.01, 1e-6, 0.001)
    );
    assert!(file.to_rules("SOL-USDC").is_ok());
    // Une paire suspendue est convertible : elle est signalée par G0, pas refusée ici.
    assert_eq!(file.get("ADA-USDC").unwrap().state, "suspend");
    assert!(file.to_rules("ADA-USDC").is_ok());
    assert!(matches!(
        file.to_rules("DOGE-USDC"),
        Err(InstrumentsError::NotListed { .. })
    ));
    let changes = file.upcoming_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].inst_id, "SOL-USDC");
    assert_eq!(changes[0].param, "minSz");
}

#[test]
fn un_pas_dont_l_inverse_n_est_pas_entier_exact_est_refuse_par_le_modele() {
    // Limite connue de `InstrumentRules::validate` (docs/specs/s9.md, point ouvert) : 1 / 0,00001 vaut
    // 99999,99999999999 en flottant, donc un tick OKX de 0,00001 est refusé à la conversion, avec
    // le nom de l'instrument. Les ticks de BTC-USDC (0,1) et ETH-USDC (0,01) ne sont pas concernés.
    let mut row = okx_rows()[3].clone();
    row["tickSz"] = json!("0.00001");
    let file = InstrumentsFile::from_response("eea", "SPOT", SERVER_TS, &[row]).unwrap();
    let error = file.to_rules("ADA-USDC").unwrap_err();
    assert!(
        matches!(&error, InstrumentsError::InvalidRules { inst_id, .. } if inst_id == "ADA-USDC"),
        "{error}"
    );
}

#[test]
fn fichier_modifie_a_la_main_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let raw = fs::read_to_string(fixture_path()).unwrap();
    let altered = dir.path().join("modifie.json");
    fs::write(&altered, raw.replace("\"0.00001\"", "\"0.0000001\"")).unwrap();
    assert!(matches!(
        InstrumentsFile::load(&altered),
        Err(InstrumentsError::HashMismatch { .. })
    ));
    let absent = dir.path().join("absent.json");
    assert!(matches!(
        InstrumentsFile::load(&absent),
        Err(InstrumentsError::Read { .. })
    ));
    // Un fichier trié autrement (ou en double) est refusé même si le hash est refait.
    let mut file = InstrumentsFile::load(&fixture_path()).unwrap();
    file.instruments.swap(0, 1);
    file.instruments_hash = file.compute_hash().unwrap();
    let unsorted = dir.path().join("desordre.json");
    fs::write(&unsorted, file.to_bytes().unwrap()).unwrap();
    assert!(matches!(
        InstrumentsFile::load(&unsorted),
        Err(InstrumentsError::Unsorted { .. })
    ));
}

#[test]
fn erreurs_de_cle_sans_valeur() {
    // Display des erreurs de lecture : jamais de valeur de clé, même quand le fichier en contient.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("k.toml");
    fs::write(&path, format!("api_key = {TEST_KEY}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let error = Credentials::from_file(&path).unwrap_err();
    assert!(matches!(error, CredentialsError::Malformed { .. }));
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains(TEST_KEY), "{shown}");
    }
}
