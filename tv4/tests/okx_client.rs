//! Tests du client OKX sur un serveur HTTP mocké (`httpmock`), docs/03-architecture.md §11, S7.
//!
//! Aucun accès réseau réel. Le serveur écoute sur `127.0.0.1` ; les clients de test désactivent le
//! proxy de l'environnement (`use_env_proxy: false`), le conteneur définissant `HTTPS_PROXY`. Toutes les
//! durées du client sont réduites à quelques millisecondes.
//!
//! Le mock se comporte comme l'échange : il tient un jeu de `open_time` et répond, pour un `after`
//! donné, aux `limit` bougies les plus récentes strictement plus anciennes, en ordre décroissant. Chaque
//! requête reçue est journalisée (paramètres de la requête et instant d'arrivée).

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use httpmock::prelude::*;
use httpmock::{HttpMockRequest, HttpMockResponse};
use serde_json::{Value, json};

use tv4::marketdata::okx::{
    OkxClient, OkxConfig, OkxError, OkxPage, WindowError, WindowFetch, fetch_window,
};
use tv4::model::{Timeframe, Ts};

/// 2024-01-01T00:00:00Z en millisecondes.
const T0: Ts = 1_704_067_200_000;
const MINUTE: Ts = 60_000;

/// Une requête reçue par le mock.
struct Hit {
    query: BTreeMap<String, String>,
    at: Instant,
}

type Log = Arc<Mutex<Vec<Hit>>>;

/// Serveur mocké et journal des requêtes reçues.
struct Mocked {
    server: MockServer,
    log: Log,
}

impl Mocked {
    /// Démarre un serveur dont la réponse à la n-ième requête (à partir de 0) est `handler(n, query)`.
    fn start(
        handler: impl Fn(usize, &BTreeMap<String, String>) -> (u16, String) + Send + Sync + 'static,
    ) -> Mocked {
        let server = MockServer::start();
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let journal = Arc::clone(&log);
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/api/v5/market/history-candles");
            then.respond_with(move |request: &HttpMockRequest| {
                let query: BTreeMap<String, String> = request.query_params().into_iter().collect();
                let index = {
                    let mut hits = journal.lock().unwrap();
                    hits.push(Hit {
                        query: query.clone(),
                        at: Instant::now(),
                    });
                    hits.len() - 1
                };
                let (status, body) = handler(index, &query);
                HttpMockResponse::builder()
                    .status(status)
                    .body(body)
                    .build()
            });
        });
        Mocked { server, log }
    }

    /// Serveur qui joue l'échange sur `dataset`, avec `unclosed` en bougies `confirm = "0"`.
    fn exchange(dataset: Vec<Ts>, unclosed: Vec<Ts>) -> Mocked {
        Mocked::start(move |_, query| (200, exchange_body(&dataset, &unclosed, query)))
    }

    fn client(&self, tweak: impl FnOnce(&mut OkxConfig)) -> OkxClient {
        let mut config = test_config(&self.server.base_url());
        tweak(&mut config);
        OkxClient::new(config)
    }

    fn count(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    /// Valeur d'un paramètre pour chaque requête reçue.
    fn params(&self, name: &str) -> Vec<Option<String>> {
        let hits = self.log.lock().unwrap();
        hits.iter()
            .map(|hit| hit.query.get(name).cloned())
            .collect()
    }

    fn arrivals(&self) -> Vec<Instant> {
        self.log.lock().unwrap().iter().map(|hit| hit.at).collect()
    }
}

fn test_config(base_url: &str) -> OkxConfig {
    OkxConfig {
        base_url: base_url.to_string(),
        limit: 10,
        timeout: Duration::from_secs(5),
        max_attempts: 5,
        backoff_min: Duration::from_millis(2),
        backoff_max: Duration::from_millis(8),
        slot_spacing: Duration::from_millis(1),
        rate_limit_pause: Duration::from_millis(40),
        use_env_proxy: false,
        jitter_seed: 1,
    }
}

fn row(ts: Ts, confirm: &str) -> Value {
    json!([
        ts.to_string(),
        "100",
        "101",
        "99",
        "100.5",
        "12.5",
        "1",
        "1250",
        confirm
    ])
}

fn ok_body(rows: Vec<Value>) -> String {
    json!({ "code": "0", "msg": "", "data": rows }).to_string()
}

/// Réponse de l'échange à `query` : les `limit` plus récentes bougies de `dataset` avec `ts < after`.
fn exchange_body(dataset: &[Ts], unclosed: &[Ts], query: &BTreeMap<String, String>) -> String {
    let after: Ts = query["after"].parse().unwrap();
    let limit: usize = query["limit"].parse().unwrap();
    let mut older: Vec<Ts> = dataset.iter().copied().filter(|ts| *ts < after).collect();
    older.sort_unstable_by(|a, b| b.cmp(a));
    older.truncate(limit);
    ok_body(
        older
            .into_iter()
            .map(|ts| row(ts, if unclosed.contains(&ts) { "0" } else { "1" }))
            .collect(),
    )
}

/// `n` open_time consécutifs de `timeframe` à partir de `first`.
fn series(first: Ts, timeframe: Timeframe, n: i64) -> Vec<Ts> {
    (0..n).map(|i| first + i * timeframe.ms()).collect()
}

fn no_page(_: &OkxPage) -> Result<(), io::Error> {
    Ok(())
}

fn window(
    client: &OkxClient,
    timeframe: Timeframe,
    start: Ts,
    end: Ts,
) -> Result<WindowFetch, WindowError<io::Error>> {
    fetch_window(client, "BTC-USDC", timeframe, start, end, no_page)
}

fn open_times(fetch: &WindowFetch) -> Vec<Ts> {
    fetch.candles.iter().map(|c| c.open_time_ms).collect()
}

#[test]
fn le_client_est_send_sync_et_clonable() {
    fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
    assert_send_sync_clone::<OkxClient>();
}

// --- Pagination ---------------------------------------------------------------------------------

#[test]
fn after_exclusif_toujours_present_et_jamais_before() {
    let dataset = series(T0, Timeframe::M1, 25);
    let end = T0 + 25 * MINUTE;
    let mocked = Mocked::exchange(dataset.clone(), vec![]);
    let client = mocked.client(|_| {});

    let fetch = window(&client, Timeframe::M1, T0, end).unwrap();

    assert_eq!(open_times(&fetch), dataset);
    // Pages de 10, 10, 5 bougies, puis la page vide (le curseur est à `T0`, qui n'est pas < start).
    assert_eq!(mocked.count(), 4);
    let after: Vec<Option<String>> = [end, T0 + 15 * MINUTE, T0 + 5 * MINUTE, T0]
        .iter()
        .map(|ts| Some(ts.to_string()))
        .collect();
    assert_eq!(mocked.params("after"), after);
    assert!(mocked.params("before").iter().all(Option::is_none));
    assert!(
        mocked
            .params("instId")
            .iter()
            .all(|v| v.as_deref() == Some("BTC-USDC"))
    );
    assert!(
        mocked
            .params("bar")
            .iter()
            .all(|v| v.as_deref() == Some("1m"))
    );
    assert!(
        mocked
            .params("limit")
            .iter()
            .all(|v| v.as_deref() == Some("10"))
    );
    assert_eq!(
        (fetch.stats.pages, fetch.stats.requests, fetch.stats.retries),
        (4, 4, 0)
    );
}

#[test]
fn arret_sur_page_vide() {
    let mocked = Mocked::exchange(vec![], vec![]);
    let client = mocked.client(|_| {});
    let fetch = window(&client, Timeframe::M1, T0, T0 + 60 * MINUTE).unwrap();
    assert!(fetch.candles.is_empty());
    assert_eq!(mocked.count(), 1);
    assert_eq!(fetch.stats.pages, 1);
}

#[test]
fn arret_quand_la_page_passe_sous_start_et_fenetre_respectee() {
    // Le jeu remonte bien en deçà de `start` ; la fenêtre est [T0 + 12 min, T0 + 27 min).
    let dataset = series(T0, Timeframe::M1, 40);
    let (start, end) = (T0 + 12 * MINUTE, T0 + 27 * MINUTE);
    let mocked = Mocked::exchange(dataset, vec![]);
    let client = mocked.client(|_| {});

    let fetch = window(&client, Timeframe::M1, start, end).unwrap();

    assert_eq!(open_times(&fetch), series(start, Timeframe::M1, 15));
    // after = end -> [26..17] ; after = 17 -> [16..7] contient ts < start : arrêt sans 3e requête.
    assert_eq!(mocked.count(), 2);
}

#[test]
fn fenetre_vide_n_envoie_aucune_requete() {
    let mocked = Mocked::exchange(series(T0, Timeframe::M1, 5), vec![]);
    let client = mocked.client(|_| {});
    let fetch = window(&client, Timeframe::M1, T0 + MINUTE, T0 + MINUTE).unwrap();
    assert!(fetch.candles.is_empty());
    assert_eq!(mocked.count(), 0);
}

#[test]
fn confirm_zero_est_exclue_et_comptee() {
    let dataset = series(T0, Timeframe::M1, 12);
    let encours = vec![T0 + 11 * MINUTE, T0 + 5 * MINUTE];
    let mocked = Mocked::exchange(dataset, encours.clone());
    let client = mocked.client(|_| {});

    let fetch = window(&client, Timeframe::M1, T0, T0 + 12 * MINUTE).unwrap();

    let attendu: Vec<Ts> = series(T0, Timeframe::M1, 12)
        .into_iter()
        .filter(|ts| !encours.contains(ts))
        .collect();
    assert_eq!(open_times(&fetch), attendu);
    assert_eq!(fetch.rejected_unclosed, [T0 + 5 * MINUTE, T0 + 11 * MINUTE]);
    assert_eq!(fetch.stats.rejected_unclosed, 2);
}

#[test]
fn on_page_recoit_chaque_page_non_vide_restreinte_a_la_fenetre() {
    let mocked = Mocked::exchange(series(T0, Timeframe::M1, 40), vec![]);
    let client = mocked.client(|_| {});
    let (start, end) = (T0 + 12 * MINUTE, T0 + 27 * MINUTE);
    let mut pages: Vec<Vec<Ts>> = Vec::new();
    let fetch = fetch_window(
        &client,
        "BTC-USDC",
        Timeframe::M1,
        start,
        end,
        |page: &OkxPage| {
            pages.push(page.candles.iter().map(|c| c.open_time_ms).collect());
            Ok::<(), io::Error>(())
        },
    )
    .unwrap();
    assert_eq!(pages.len(), 2);
    // Ordre reçu : décroissant, page la plus récente d'abord ; la seconde est rognée à `start`.
    assert_eq!(pages[0].first(), Some(&(T0 + 26 * MINUTE)));
    assert_eq!(pages[1].len(), 5);
    assert!(pages[1].iter().all(|ts| *ts >= start));
    assert_eq!(fetch.candles.len(), 15);
}

#[test]
fn l_erreur_de_on_page_arrete_la_recuperation() {
    let mocked = Mocked::exchange(series(T0, Timeframe::M1, 40), vec![]);
    let client = mocked.client(|_| {});
    let result = fetch_window(
        &client,
        "BTC-USDC",
        Timeframe::M1,
        T0,
        T0 + 40 * MINUTE,
        |_: &OkxPage| Err(io::Error::other("disque plein")),
    );
    assert!(matches!(result, Err(WindowError::OnPage(_))));
    assert_eq!(mocked.count(), 1);
}

// --- Contrôles de la page -----------------------------------------------------------------------

#[test]
fn ordre_croissant_refuse_sans_retry() {
    let mocked = Mocked::start(|_, _| (200, ok_body(vec![row(T0, "1"), row(T0 + MINUTE, "1")])));
    let client = mocked.client(|_| {});
    let result = window(&client, Timeframe::M1, T0, T0 + 10 * MINUTE);
    assert!(matches!(
        result,
        Err(WindowError::Okx(OkxError::NotDescending { row: 2, .. }))
    ));
    assert_eq!(mocked.count(), 1);
}

#[test]
fn ts_non_aligne_et_bougie_invalide_refuses_sans_retry() {
    let desaligne = Mocked::start(|_, _| (200, ok_body(vec![row(T0 + 7, "1")])));
    let result = window(&desaligne.client(|_| {}), Timeframe::M1, T0, T0 + MINUTE);
    assert!(matches!(
        result,
        Err(WindowError::Okx(OkxError::MisalignedTs { .. }))
    ));
    assert_eq!(desaligne.count(), 1);

    let incoherente = json!([T0.to_string(), "100", "98", "99", "100", "1", "1", "1", "1"]);
    let invalide = Mocked::start(move |_, _| (200, ok_body(vec![incoherente.clone()])));
    let result = window(&invalide.client(|_| {}), Timeframe::M1, T0, T0 + MINUTE);
    assert!(matches!(
        result,
        Err(WindowError::Okx(OkxError::InvalidCandle { .. }))
    ));
    assert_eq!(invalide.count(), 1);
}

#[test]
fn serveur_qui_ignore_after_est_detecte_au_lieu_de_boucler() {
    // Toujours la même page : à la 2e requête, ses ts dépassent la borne exclusive.
    let mocked = Mocked::start(|_, _| {
        let rows = (0..10).rev().map(|i| row(T0 + i * MINUTE, "1")).collect();
        (200, ok_body(rows))
    });
    let client = mocked.client(|_| {});
    let result = window(&client, Timeframe::M1, T0 - 100 * MINUTE, T0 + 10 * MINUTE);
    assert!(matches!(
        result,
        Err(WindowError::Okx(OkxError::NotBeforeCursor { .. }))
    ));
    assert_eq!(mocked.count(), 2);
}

// --- Barres -------------------------------------------------------------------------------------

#[test]
fn barres_envoyees_1dutc_4h_et_timeframe_refuse() {
    let jours = Mocked::exchange(series(T0, Timeframe::D1, 3), vec![]);
    let fetch = window(
        &jours.client(|_| {}),
        Timeframe::D1,
        T0,
        T0 + 3 * Timeframe::D1.ms(),
    )
    .unwrap();
    assert_eq!(fetch.candles.len(), 3);
    assert!(
        jours
            .params("bar")
            .iter()
            .all(|v| v.as_deref() == Some("1Dutc"))
    );

    let quatre_heures = Mocked::exchange(series(T0, Timeframe::H4, 3), vec![]);
    window(
        &quatre_heures.client(|_| {}),
        Timeframe::H4,
        T0,
        T0 + 3 * Timeframe::H4.ms(),
    )
    .unwrap();
    assert!(
        quatre_heures
            .params("bar")
            .iter()
            .all(|v| v.as_deref() == Some("4H"))
    );

    let douze = Mocked::exchange(vec![], vec![]);
    let result = window(
        &douze.client(|_| {}),
        Timeframe::H12,
        T0,
        T0 + Timeframe::D1.ms(),
    );
    assert!(matches!(
        result,
        Err(WindowError::Okx(OkxError::UnsupportedTimeframe { .. }))
    ));
    assert_eq!(douze.count(), 0);
}

// --- Retry --------------------------------------------------------------------------------------

/// Serveur qui répond `(status, body)` aux `failures` premières requêtes, puis par l'échange.
fn failing_then_ok(failures: usize, status: u16, body: &'static str, dataset: Vec<Ts>) -> Mocked {
    Mocked::start(move |index, query| {
        if index < failures {
            (status, body.to_string())
        } else {
            (200, exchange_body(&dataset, &[], query))
        }
    })
}

#[test]
fn retry_sur_429_puis_succes_et_pause_du_bucket() {
    let mocked = failing_then_ok(1, 429, "{}", series(T0, Timeframe::M1, 3));
    let client = mocked.client(|c| c.rate_limit_pause = Duration::from_millis(150));
    let begin = Instant::now();

    let fetch = window(&client, Timeframe::M1, T0, T0 + 3 * MINUTE).unwrap();

    assert_eq!(fetch.candles.len(), 3);
    assert_eq!(fetch.stats.retries, 1);
    // 2 essais pour la 1re page + 1 page vide.
    assert_eq!((fetch.stats.pages, fetch.stats.requests), (2, 3));
    // Le 429 met le bucket en pause : le 2e essai part au plus tôt 150 ms après le 1er.
    let arrivals = mocked.arrivals();
    assert!(arrivals[1] - arrivals[0] >= Duration::from_millis(140));
    assert!(begin.elapsed() >= Duration::from_millis(150));
}

#[test]
fn retry_sur_500_puis_succes_sans_pause_du_bucket() {
    let mocked = failing_then_ok(2, 500, "boom", series(T0, Timeframe::M1, 3));
    let client = mocked.client(|c| c.rate_limit_pause = Duration::from_secs(30));
    let fetch = window(&client, Timeframe::M1, T0, T0 + 3 * MINUTE).unwrap();
    assert_eq!(fetch.candles.len(), 3);
    assert_eq!(fetch.stats.retries, 2);
    // Un 5xx n'est pas un 429 : la pause de 30 s n'a pas eu lieu (le test finirait en timeout).
    assert_eq!(
        fetch.stats.requests,
        fetch.stats.pages + fetch.stats.retries
    );
}

#[test]
fn retry_sur_code_50011_puis_succes_et_pause_du_bucket() {
    let body = r#"{"code":"50011","msg":"Too Many Requests","data":[]}"#;
    let mocked = failing_then_ok(1, 200, body, series(T0, Timeframe::M1, 3));
    let client = mocked.client(|c| c.rate_limit_pause = Duration::from_millis(150));
    let fetch = window(&client, Timeframe::M1, T0, T0 + 3 * MINUTE).unwrap();
    assert_eq!(fetch.candles.len(), 3);
    assert_eq!(fetch.stats.retries, 1);
    let arrivals = mocked.arrivals();
    assert!(arrivals[1] - arrivals[0] >= Duration::from_millis(140));
}

#[test]
fn retry_sur_enveloppe_illisible_puis_succes() {
    let mocked = failing_then_ok(1, 200, "<html>proxy</html>", series(T0, Timeframe::M1, 3));
    let fetch = window(&mocked.client(|_| {}), Timeframe::M1, T0, T0 + 3 * MINUTE).unwrap();
    assert_eq!(fetch.candles.len(), 3);
    assert_eq!(fetch.stats.retries, 1);
}

#[test]
fn retry_sur_timeout_puis_succes() {
    let dataset = series(T0, Timeframe::M1, 3);
    let mocked = Mocked::start(move |index, query| {
        if index == 0 {
            std::thread::sleep(Duration::from_millis(500));
        }
        (200, exchange_body(&dataset, &[], query))
    });
    let client = mocked.client(|c| c.timeout = Duration::from_millis(150));
    let fetch = window(&client, Timeframe::M1, T0, T0 + 3 * MINUTE).unwrap();
    assert_eq!(fetch.candles.len(), 3);
    assert!(fetch.stats.retries >= 1);
}

#[test]
fn abandon_apres_cinq_essais() {
    let mocked = Mocked::start(|_, _| (500, "boom".to_string()));
    let client = mocked.client(|_| {});
    let result = window(&client, Timeframe::M1, T0, T0 + 3 * MINUTE);
    match result {
        Err(WindowError::Okx(OkxError::RetriesExhausted { attempts, last })) => {
            assert_eq!(attempts, 5);
            assert!(matches!(*last, OkxError::Http { status: 500, .. }));
        }
        other => panic!("abandon attendu, obtenu {other:?}"),
    }
    assert_eq!(mocked.count(), 5);
}

#[test]
fn code_api_persistant_abandonne_avec_le_dernier_code() {
    let body = r#"{"code":"51001","msg":"Instrument ID does not exist","data":[]}"#;
    let mocked = Mocked::start(move |_, _| (200, body.to_string()));
    let result = window(&mocked.client(|_| {}), Timeframe::M1, T0, T0 + MINUTE);
    match result {
        Err(WindowError::Okx(OkxError::RetriesExhausted { attempts: 5, last })) => {
            assert!(matches!(*last, OkxError::Api { ref code, .. } if code == "51001"));
        }
        other => panic!("abandon attendu, obtenu {other:?}"),
    }
    assert_eq!(mocked.count(), 5);
}

#[test]
fn autre_4xx_echoue_sans_retry() {
    let mocked = Mocked::start(|_, _| {
        (
            400,
            r#"{"code":"51000","msg":"Parameter error"}"#.to_string(),
        )
    });
    let result = window(&mocked.client(|_| {}), Timeframe::M1, T0, T0 + MINUTE);
    assert!(matches!(
        result,
        Err(WindowError::Okx(OkxError::Http { status: 400, .. }))
    ));
    assert_eq!(mocked.count(), 1);
}

// --- Token bucket -------------------------------------------------------------------------------

#[test]
fn le_bucket_espace_les_requetes_et_les_retries_le_consomment() {
    // 3 requêtes dont les 2 premières échouent en 500 ; le backoff (1 ms) est bien plus court que
    // l'espacement : c'est le bucket qui fixe la cadence des retries.
    let spacing = Duration::from_millis(40);
    let mocked = failing_then_ok(2, 500, "boom", vec![]);
    let client = mocked.client(|c| {
        c.slot_spacing = spacing;
        c.backoff_min = Duration::from_millis(1);
        c.backoff_max = Duration::from_millis(1);
    });
    let begin = Instant::now();
    let fetch = window(&client, Timeframe::M1, T0, T0 + MINUTE).unwrap();
    assert_eq!(fetch.stats.requests, 3);
    let arrivals = mocked.arrivals();
    // Marge de 10 ms pour la latence des requêtes.
    for pair in arrivals.windows(2) {
        assert!(pair[1] - pair[0] >= spacing - Duration::from_millis(10));
    }
    assert!(begin.elapsed() >= 2 * spacing);
}

#[test]
fn trois_threads_partagent_le_bucket() {
    let spacing = Duration::from_millis(20);
    let mocked = Mocked::exchange(series(T0, Timeframe::M1, 100), vec![]);
    let client = mocked.client(|c| c.slot_spacing = spacing);
    let begin = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                for _ in 0..4 {
                    client
                        .fetch_page("BTC-USDC", Timeframe::M1, T0 + 100 * MINUTE)
                        .unwrap();
                }
            });
        }
    });
    // 12 requêtes, 11 intervalles : borne basse certaine, quel que soit le nombre de threads.
    assert_eq!(mocked.count(), 12);
    assert!(begin.elapsed() >= 11 * spacing);
    let mut arrivals = mocked.arrivals();
    arrivals.sort();
    for pair in arrivals.windows(2) {
        assert!(pair[1] - pair[0] >= spacing - Duration::from_millis(10));
    }
}

#[test]
fn la_pause_s_applique_a_tous_les_threads() {
    let pause = Duration::from_millis(200);
    let mocked = Mocked::exchange(series(T0, Timeframe::M1, 100), vec![]);
    let client = mocked.client(|_| {});
    let begin = Instant::now();
    client.bucket().pause(pause);
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                client
                    .fetch_page("BTC-USDC", Timeframe::M1, T0 + 100 * MINUTE)
                    .unwrap();
            });
        }
    });
    // Aucune requête des trois threads n'a atteint le serveur avant la fin de la pause.
    let arrivals = mocked.arrivals();
    assert_eq!(arrivals.len(), 3);
    assert!(arrivals.iter().all(|at| *at - begin >= pause));
}
