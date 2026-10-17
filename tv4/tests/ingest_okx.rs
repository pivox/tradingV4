//! Tests de `tv4 ingest okx` (docs/03-architecture.md §11, S7, deuxième partie) sur un serveur HTTP
//! mocké (`httpmock`) : aucun accès réseau réel.
//!
//! Le mock joue l'échange : il tient, par instrument, un jeu de bougies clôturées et un jeu de bougies
//! `confirm = "0"`, et répond à `after` par les `limit` plus récentes strictement plus anciennes, en ordre
//! décroissant. Chaque requête est journalisée. Les tests de bibliothèque appellent `ingest_okx` avec un
//! `OkxClient` injecté (limit réduit, espacement du bucket de 1 ms, retries de quelques millisecondes) ;
//! deux tests lancent le vrai exécutable avec l'argument caché `--base-url`.
//!
//! Les prix sont inventés : ils dépendent seulement de l'horodatage, pour qu'une bougie mal placée soit
//! visible.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use httpmock::prelude::*;
use httpmock::{HttpMockRequest, HttpMockResponse};
use serde_json::{Value, json};

use tv4::cli::{CliError, MonthStatus, OkxIngestOptions, OkxIngestReport, ingest_okx};
use tv4::marketdata::okx::{OkxClient, OkxConfig, OkxError};
use tv4::model::{Candle, ExchangeId, Timeframe, Ts};
use tv4::store::{
    FileManifest, Partition, Period, ReadOptions, Symbol, dsh, read_file_manifest, read_parquet,
    resolve_rel,
};

const MINUTE: Ts = 60_000;
const HALF_HOUR: Ts = 30 * MINUTE;
const DAY: Ts = 86_400_000;
/// Débuts de mois 2024 (UTC).
const JAN: Ts = 1_704_067_200_000;
const FEB: Ts = 1_706_745_600_000;
const MAR: Ts = 1_709_251_200_000;
const APR: Ts = 1_711_929_600_000;
const MAY: Ts = 1_714_521_600_000;
const JUN: Ts = 1_717_200_000_000;
const JUL: Ts = 1_719_792_000_000;
/// 2030-01-01 : « maintenant » des tests, après toutes les périodes.
const NOW: Ts = 1_893_456_000_000;

// --- Mock de l'échange ------------------------------------------------------------------------

/// Jeu de bougies d'un instrument.
#[derive(Clone, Default)]
struct Series {
    confirmed: Vec<Ts>,
    unclosed: Vec<Ts>,
}

/// `n` open_time consécutifs de pas `step` à partir de `first`.
fn run_of(first: Ts, step: Ts, n: i64) -> Vec<Ts> {
    (0..n).map(|i| first + i * step).collect()
}

/// Bougie de l'échange pour `ts` (prix inventés fonction de `ts`).
fn candle_of(ts: Ts) -> Candle {
    let base = 100.0 + ((ts / MINUTE) % 50) as f64;
    Candle::new(ts, base, base + 1.0, base - 1.0, base + 0.5, 12.5, None).unwrap()
}

fn row(ts: Ts, confirm: &str) -> Value {
    let base = 100.0 + ((ts / MINUTE) % 50) as f64;
    json!([
        ts.to_string(),
        base.to_string(),
        (base + 1.0).to_string(),
        (base - 1.0).to_string(),
        (base + 0.5).to_string(),
        "12.5",
        "1",
        "1250",
        confirm
    ])
}

fn body(rows: Vec<Value>) -> String {
    json!({ "code": "0", "msg": "", "data": rows }).to_string()
}

/// Réponse de l'échange à `query` pour les jeux `datasets`.
fn exchange_body(datasets: &BTreeMap<String, Series>, query: &BTreeMap<String, String>) -> String {
    let after: Ts = query["after"].parse().unwrap();
    let limit: usize = query["limit"].parse().unwrap();
    let empty = Series::default();
    let series = datasets.get(&query["instId"]).unwrap_or(&empty);
    let mut rows: Vec<(Ts, bool)> = series
        .confirmed
        .iter()
        .map(|ts| (*ts, true))
        .chain(series.unclosed.iter().map(|ts| (*ts, false)))
        .filter(|(ts, _)| *ts < after)
        .collect();
    rows.sort_unstable_by_key(|row| std::cmp::Reverse(row.0));
    rows.truncate(limit);
    body(
        rows.into_iter()
            .map(|(ts, closed)| row(ts, if closed { "1" } else { "0" }))
            .collect(),
    )
}

type Query = BTreeMap<String, String>;

/// Serveur mocké et journal des requêtes (paramètres de la requête).
struct Mocked {
    server: MockServer,
    log: Arc<Mutex<Vec<Query>>>,
}

impl Mocked {
    fn start(handler: impl Fn(&Query) -> (u16, String) + Send + Sync + 'static) -> Mocked {
        let server = MockServer::start();
        let log: Arc<Mutex<Vec<Query>>> = Arc::new(Mutex::new(Vec::new()));
        let journal = Arc::clone(&log);
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/api/v5/market/history-candles");
            then.respond_with(move |request: &HttpMockRequest| {
                let query: Query = request.query_params().into_iter().collect();
                journal.lock().unwrap().push(query.clone());
                let (status, text) = handler(&query);
                HttpMockResponse::builder()
                    .status(status)
                    .body(text)
                    .build()
            });
        });
        Mocked { server, log }
    }

    /// Échange sans panne sur un seul instrument, `BTC-USDC`.
    fn exchange(series: Series) -> Mocked {
        Mocked::exchange_of(BTreeMap::from([("BTC-USDC".to_string(), series)]))
    }

    fn exchange_of(datasets: BTreeMap<String, Series>) -> Mocked {
        Mocked::start(move |query| (200, exchange_body(&datasets, query)))
    }

    /// Échange qui répond `budget` requêtes puis échoue en 500 (budget négatif : jamais d'échec).
    fn with_budget(series: Series, budget: Arc<AtomicI64>) -> Mocked {
        let datasets = BTreeMap::from([("BTC-USDC".to_string(), series)]);
        Mocked::start(move |query| {
            loop {
                let current = budget.load(Ordering::SeqCst);
                if current < 0 {
                    break;
                }
                if current == 0 {
                    return (500, "{}".to_string());
                }
                if budget
                    .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    break;
                }
            }
            (200, exchange_body(&datasets, query))
        })
    }

    fn client(&self, limit: u32) -> OkxClient {
        OkxClient::new(OkxConfig {
            base_url: self.server.base_url(),
            limit,
            timeout: Duration::from_secs(5),
            max_attempts: 5,
            backoff_min: Duration::from_millis(2),
            backoff_max: Duration::from_millis(8),
            slot_spacing: Duration::from_millis(1),
            rate_limit_pause: Duration::from_millis(40),
            use_env_proxy: false,
            jitter_seed: 1,
        })
    }

    fn count(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    /// Valeurs du paramètre `name` des requêtes à partir de la `from`-ième.
    fn params_from(&self, from: usize, name: &str) -> Vec<Option<String>> {
        self.log.lock().unwrap()[from..]
            .iter()
            .map(|query| query.get(name).cloned())
            .collect()
    }
}

// --- Outils ------------------------------------------------------------------------------------

fn symbol(text: &str) -> Symbol {
    text.parse().unwrap()
}

fn options(root: &Path, insts: &[&str], tf: Timeframe, from: Ts, to: Ts) -> OkxIngestOptions {
    OkxIngestOptions::new(
        root.to_path_buf(),
        insts.iter().map(|i| symbol(i)).collect(),
        tf,
        from,
        to,
        NOW,
        false,
    )
}

fn month(year: i32, month: u32) -> Period {
    Period::Month { year, month }
}

fn partition(inst: &str, tf: Timeframe) -> Partition {
    Partition::new(ExchangeId::Okx, symbol(inst), tf)
}

fn manifest_path(root: &Path, inst: &str, tf: Timeframe, period: Period) -> PathBuf {
    resolve_rel(root, &partition(inst, tf).manifest_rel(period)).unwrap()
}

fn file_path(root: &Path, inst: &str, tf: Timeframe, period: Period) -> PathBuf {
    resolve_rel(root, &partition(inst, tf).file_rel(period)).unwrap()
}

/// Manifeste et bougies du fichier d'une période ; contrôle aussi que le `content_hash` du manifeste est
/// celui des lignes du Parquet.
fn read_period(
    root: &Path,
    inst: &str,
    tf: Timeframe,
    period: Period,
) -> (FileManifest, Vec<Candle>) {
    let manifest = read_file_manifest(&manifest_path(root, inst, tf, period)).unwrap();
    let columns =
        read_parquet(&file_path(root, inst, tf, period), &ReadOptions::default()).unwrap();
    let (hex, rows) = dsh::content_hash(&partition(inst, tf), period, &columns).unwrap();
    assert_eq!(manifest.content_hash, format!("blake3:{hex}"));
    assert_eq!(manifest.row_count, rows);
    (manifest, columns.to_candles())
}

fn expected(times: &[Ts]) -> Vec<Candle> {
    times.iter().map(|ts| candle_of(*ts)).collect()
}

fn run(client: &OkxClient, options: &OkxIngestOptions) -> OkxIngestReport {
    ingest_okx(client, options).unwrap()
}

fn statuses(report: &OkxIngestReport) -> Vec<MonthStatus> {
    report.months.iter().map(|m| m.status).collect()
}

/// Deux mois de bougies 30m : 25 en janvier, 25 en février plus une bougie `confirm = "0"` à la suite.
fn two_months() -> Series {
    Series {
        confirmed: [run_of(JAN, HALF_HOUR, 25), run_of(FEB, HALF_HOUR, 25)].concat(),
        unclosed: vec![FEB + 25 * HALF_HOUR],
    }
}

// --- Ingestion de plusieurs mois ----------------------------------------------------------------

#[test]
fn ingere_deux_mois_fichiers_manifestes_et_rejets() {
    let dir = tempfile::tempdir().unwrap();
    let mocked = Mocked::exchange(two_months());
    let client = mocked.client(10);

    let report = run(
        &client,
        &options(dir.path(), &["BTC-USDC"], Timeframe::M30, JAN, MAR),
    );

    assert_eq!(
        statuses(&report),
        [MonthStatus::Created, MonthStatus::Created]
    );
    let tf = Timeframe::M30;
    let (jan, jan_candles) = read_period(dir.path(), "BTC-USDC", tf, month(2024, 1));
    let (feb, feb_candles) = read_period(dir.path(), "BTC-USDC", tf, month(2024, 2));
    // Les bougies sont celles de l'échange, sans la bougie en formation.
    assert_eq!(jan_candles, expected(&run_of(JAN, HALF_HOUR, 25)));
    assert_eq!(feb_candles, expected(&run_of(FEB, HALF_HOUR, 25)));
    for (manifest, label) in [(&jan, "2024-01"), (&feb, "2024-02")] {
        assert_eq!(manifest.exchange, ExchangeId::Okx);
        assert_eq!(manifest.symbol.as_str(), "BTC-USDC");
        assert_eq!(manifest.tf, tf);
        assert_eq!(manifest.period, label);
        assert_eq!(manifest.source, "okx-history-candles");
        assert_eq!(manifest.hash_format, "dsh-v1");
        // Les deux mois sont passés et couverts de bout en bout : clos.
        assert!(manifest.closed, "{label}");
        let continuity = manifest.continuity.expect("bloc continuity");
        assert_eq!(continuity.duplicates, 0, "{label}");
        assert_eq!(continuity.ohlc_errors, 0, "{label}");
        assert_eq!(continuity.gaps, 0, "{label}");
    }
    assert_eq!((jan.ts_min, jan.ts_max), (JAN, JAN + 24 * HALF_HOUR));
    assert_eq!((feb.ts_min, feb.ts_max), (FEB, FEB + 24 * HALF_HOUR));
    // La bougie confirm = "0" de février est comptée dans son manifeste, pas dans celui de janvier.
    assert_eq!(jan.continuity.unwrap().rejected_unclosed, 0);
    assert_eq!(feb.continuity.unwrap().rejected_unclosed, 1);
    assert_eq!(report.months[1].rejected_unclosed, 1);

    // Protocole : `after` toujours présent, `before` jamais, barre 30m, instrument et limit demandés.
    let afters = mocked.params_from(0, "after");
    assert!(afters.iter().all(Option::is_some));
    assert!(mocked.params_from(0, "before").iter().all(Option::is_none));
    assert!(
        mocked
            .params_from(0, "bar")
            .iter()
            .all(|b| b.as_deref() == Some("30m"))
    );
    assert!(
        mocked
            .params_from(0, "instId")
            .iter()
            .all(|i| i.as_deref() == Some("BTC-USDC"))
    );
    let requests: u64 = report.months.iter().map(|m| m.requests).sum();
    assert_eq!(requests as usize, mocked.count());
}

#[test]
fn plusieurs_instruments_dans_la_meme_ingestion() {
    let dir = tempfile::tempdir().unwrap();
    let usdc = Series {
        confirmed: run_of(JAN, HALF_HOUR, 14),
        unclosed: vec![],
    };
    let usdt = Series {
        confirmed: run_of(JAN + DAY, HALF_HOUR, 9),
        unclosed: vec![],
    };
    let mocked = Mocked::exchange_of(BTreeMap::from([
        ("BTC-USDC".to_string(), usdc),
        ("BTC-USDT".to_string(), usdt),
    ]));
    let client = mocked.client(10);

    let report = run(
        &client,
        &options(
            dir.path(),
            &["BTC-USDC", "BTC-USDT"],
            Timeframe::M30,
            JAN,
            FEB,
        ),
    );

    let insts: Vec<&str> = report.months.iter().map(|m| m.inst.as_str()).collect();
    assert_eq!(insts, ["BTC-USDC", "BTC-USDT"]);
    let tf = Timeframe::M30;
    let (_, c1) = read_period(dir.path(), "BTC-USDC", tf, month(2024, 1));
    let (_, c2) = read_period(dir.path(), "BTC-USDT", tf, month(2024, 1));
    assert_eq!(c1, expected(&run_of(JAN, HALF_HOUR, 14)));
    assert_eq!(c2, expected(&run_of(JAN + DAY, HALF_HOUR, 9)));
}

// --- Écriture par lots --------------------------------------------------------------------------

#[test]
fn ecriture_intermediaire_par_lots_de_pages() {
    let dir = tempfile::tempdir().unwrap();
    let series = Series {
        confirmed: run_of(JAN, MINUTE, 95),
        unclosed: vec![],
    };
    // À chaque requête, le mock note le nombre de lignes du manifeste présent sur le disque : le fichier
    // doit exister, valide, dès le premier lot écrit.
    let observed: Arc<Mutex<Vec<Option<u64>>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&observed);
    let manifest = manifest_path(dir.path(), "BTC-USDC", Timeframe::M1, month(2024, 1));
    let datasets = BTreeMap::from([("BTC-USDC".to_string(), series)]);
    let mocked = Mocked::start(move |query| {
        let rows = read_file_manifest(&manifest).ok().map(|m| m.row_count);
        seen.lock().unwrap().push(rows);
        (200, exchange_body(&datasets, query))
    });
    let client = mocked.client(10);
    let mut opts = options(dir.path(), &["BTC-USDC"], Timeframe::M1, JAN, FEB);
    opts.workers = 1;
    opts.batch_pages = 2;

    let report = run(&client, &opts);

    // 95 bougies = 9 pages de 10 + 1 page de 5, puis la page vide : 11 requêtes. Une écriture après les
    // pages 2, 4, 6, 8 et 10 ; le fichier vu à la requête k a donc les lignes des pages écrites avant k.
    let rows = |n: u64| Some(n);
    assert_eq!(
        *observed.lock().unwrap(),
        [
            None,
            None,
            rows(20),
            rows(20),
            rows(40),
            rows(40),
            rows(60),
            rows(60),
            rows(80),
            rows(80),
            rows(95)
        ]
    );
    // Cinq lots plus la réécriture finale, qui clôt le mois (tampon vide, couverture établie).
    assert_eq!(report.months[0].writes, 6);
    let (manifest, candles) = read_period(dir.path(), "BTC-USDC", Timeframe::M1, month(2024, 1));
    assert_eq!(candles, expected(&run_of(JAN, MINUTE, 95)));
    assert!(manifest.closed);
}

// --- Arrêt en milieu de mois et reprise ---------------------------------------------------------

#[test]
fn arret_en_milieu_de_mois_puis_reprise_identique_a_une_ingestion_continue() {
    let series = Series {
        confirmed: run_of(JAN, MINUTE, 95),
        unclosed: vec![],
    };
    let tf = Timeframe::M1;
    let jan = month(2024, 1);

    // Référence : ingestion sans interruption.
    let reference_dir = tempfile::tempdir().unwrap();
    let reference_server = Mocked::exchange(series.clone());
    let reference_client = reference_server.client(10);
    let mut opts = options(reference_dir.path(), &["BTC-USDC"], tf, JAN, FEB);
    opts.workers = 1;
    opts.batch_pages = 2;
    run(&reference_client, &opts);
    let (reference, reference_candles) = read_period(reference_dir.path(), "BTC-USDC", tf, jan);

    // Ingestion interrompue : 5 pages servies, puis l'échange échoue durablement (500).
    let dir = tempfile::tempdir().unwrap();
    let budget = Arc::new(AtomicI64::new(5));
    let mocked = Mocked::with_budget(series, Arc::clone(&budget));
    let client = mocked.client(10);
    let mut opts = options(dir.path(), &["BTC-USDC"], tf, JAN, FEB);
    opts.workers = 1;
    opts.batch_pages = 2;
    let interrupted = run(&client, &opts);

    assert_eq!(statuses(&interrupted), [MonthStatus::Failed]);
    assert!(
        interrupted.months[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("abandon après 5 essais")),
        "{:?}",
        interrupted.months[0].error
    );
    // Deux lots écrits (pages 1-2 et 3-4) ; la page 5, tamponnée, est perdue. Le fichier couvre
    // [JAN+55 min, JAN+94 min], pas le début du mois, et n'est pas clos.
    let (partial, partial_candles) = read_period(dir.path(), "BTC-USDC", tf, jan);
    assert_eq!(
        partial_candles,
        expected(&run_of(JAN + 55 * MINUTE, MINUTE, 40))
    );
    assert!(!partial.closed);

    // Reprise : l'échange est rétabli.
    budget.store(-1, Ordering::SeqCst);
    let before = mocked.count();
    let mut opts = options(dir.path(), &["BTC-USDC"], tf, JAN, FEB);
    opts.workers = 1;
    opts.batch_pages = 2;
    opts.resume = true;
    let resumed = run(&client, &opts);

    assert_eq!(statuses(&resumed), [MonthStatus::Rewritten]);
    // page = 10 bougies. Moitié manquante : [JAN, ts_min + page = JAN+65 min) puis page vide ;
    // page chevauchante du haut : [ts_max − page = JAN+84 min, FEB). Rien d'autre n'est retéléchargé.
    let afters: Vec<Ts> = mocked
        .params_from(before, "after")
        .into_iter()
        .map(|a| a.unwrap().parse().unwrap())
        .collect();
    let minutes = |m: Ts| JAN + m * MINUTE;
    assert_eq!(
        afters,
        [
            minutes(65),
            minutes(55),
            minutes(45),
            minutes(35),
            minutes(25),
            minutes(15),
            minutes(5),
            minutes(0),
            FEB,
            minutes(85)
        ]
    );
    // Le mois final est complet, clos et identique à l'ingestion continue.
    let (final_manifest, final_candles) = read_period(dir.path(), "BTC-USDC", tf, jan);
    assert_eq!(final_candles, reference_candles);
    assert_eq!(final_manifest.content_hash, reference.content_hash);
    assert_eq!(final_manifest.row_count, 95);
    assert_eq!(final_manifest.continuity, reference.continuity);
    assert!(final_manifest.closed);
}

// --- Idempotence --------------------------------------------------------------------------------

#[test]
fn resume_sans_nouvelle_donnee_ne_fait_aucune_requete_sur_les_mois_clos() {
    let dir = tempfile::tempdir().unwrap();
    let mocked = Mocked::exchange(two_months());
    let client = mocked.client(10);
    let tf = Timeframe::M30;
    run(&client, &options(dir.path(), &["BTC-USDC"], tf, JAN, MAR));
    let snapshot = |period| {
        (
            fs::read(manifest_path(dir.path(), "BTC-USDC", tf, period)).unwrap(),
            fs::read(file_path(dir.path(), "BTC-USDC", tf, period)).unwrap(),
        )
    };
    let (jan_before, feb_before) = (snapshot(month(2024, 1)), snapshot(month(2024, 2)));
    let requests = mocked.count();

    let mut opts = options(dir.path(), &["BTC-USDC"], tf, JAN, MAR);
    opts.resume = true;
    let second = run(&client, &opts);

    assert_eq!(
        statuses(&second),
        [MonthStatus::SkippedClosed, MonthStatus::SkippedClosed]
    );
    assert_eq!(mocked.count(), requests, "aucune requête sur les mois clos");
    // Ni manifeste ni Parquet n'ont bougé d'un octet.
    assert_eq!(snapshot(month(2024, 1)), jan_before);
    assert_eq!(snapshot(month(2024, 2)), feb_before);
}

#[test]
fn relance_sans_resume_retelecharge_et_ne_change_aucun_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    let mocked = Mocked::exchange(two_months());
    let client = mocked.client(10);
    let tf = Timeframe::M30;
    run(&client, &options(dir.path(), &["BTC-USDC"], tf, JAN, MAR));
    let hashes = |root: &Path| -> Vec<String> {
        [month(2024, 1), month(2024, 2)]
            .iter()
            .map(|p| read_period(root, "BTC-USDC", tf, *p).0.content_hash)
            .collect()
    };
    let first = hashes(dir.path());
    let manifest = fs::read(manifest_path(dir.path(), "BTC-USDC", tf, month(2024, 2))).unwrap();
    let requests = mocked.count();

    // Une heure plus tard, sans nouvelle donnée.
    let mut opts = options(dir.path(), &["BTC-USDC"], tf, JAN, MAR);
    opts.now_ms = NOW + 3_600_000;
    let second = run(&client, &opts);

    assert!(
        mocked.count() > requests,
        "sans --resume tout est retéléchargé"
    );
    assert_eq!(
        statuses(&second),
        [MonthStatus::Unchanged, MonthStatus::Unchanged]
    );
    assert_eq!(hashes(dir.path()), first);
    // Le manifeste (dont `ingested_at`) est conservé tel quel.
    assert_eq!(
        fs::read(manifest_path(dir.path(), "BTC-USDC", tf, month(2024, 2))).unwrap(),
        manifest
    );
}

#[test]
fn mois_courant_la_reprise_ne_refait_que_la_sous_fenetre_haute() {
    let dir = tempfile::tempdir().unwrap();
    let tf = Timeframe::M30;
    let feb = month(2024, 2);
    let mocked = Mocked::exchange(two_months());
    let client = mocked.client(10);
    // `to` au milieu de février : le mois n'est pas couvert, donc pas clos.
    let to = FEB + 14 * DAY;
    let mut opts = options(dir.path(), &["BTC-USDC"], tf, FEB, to);
    run(&client, &opts);
    let (first, _) = read_period(dir.path(), "BTC-USDC", tf, feb);
    assert!(!first.closed);
    let manifest = fs::read(manifest_path(dir.path(), "BTC-USDC", tf, feb)).unwrap();
    let before = mocked.count();

    opts.resume = true;
    let second = run(&client, &opts);

    // ts_min = début de la fenêtre : pas de sous-fenêtre basse. Haut : [ts_max − page, to), soit la page
    // des 10 dernières lignes (dont la bougie en formation) puis la suivante qui passe sous le début.
    assert_eq!(statuses(&second), [MonthStatus::Unchanged]);
    assert_eq!(second.months[0].windows, 1);
    let afters: Vec<Ts> = mocked
        .params_from(before, "after")
        .into_iter()
        .map(|a| a.unwrap().parse().unwrap())
        .collect();
    assert_eq!(afters, [to, FEB + 16 * HALF_HOUR]);
    assert_eq!(
        read_period(dir.path(), "BTC-USDC", tf, feb).0.content_hash,
        first.content_hash
    );
    assert_eq!(
        fs::read(manifest_path(dir.path(), "BTC-USDC", tf, feb)).unwrap(),
        manifest
    );
    assert_eq!(second.months[0].rejected_unclosed, 1);
}

// --- Threads ------------------------------------------------------------------------------------

#[test]
fn trois_workers_ecrivent_chaque_mois_une_seule_fois() {
    let confirmed: Vec<Ts> = [JAN, FEB, MAR, APR, MAY, JUN]
        .iter()
        .flat_map(|start| run_of(*start, HALF_HOUR, 12))
        .collect();
    let series = Series {
        confirmed,
        unclosed: vec![],
    };
    let tf = Timeframe::M30;

    let dir = tempfile::tempdir().unwrap();
    let mocked = Mocked::exchange(series.clone());
    let client = mocked.client(5);
    let mut opts = options(dir.path(), &["BTC-USDC"], tf, JAN, JUL);
    opts.batch_pages = 1;
    assert_eq!(opts.workers, 3);
    let report = run(&client, &opts);

    // Un compte rendu par mois, une seule fois, dans l'ordre ; aucun échec (pas de verrou refusé).
    let periods: Vec<Period> = report.months.iter().map(|m| m.period).collect();
    let attendus: Vec<Period> = (1..=6).map(|m| month(2024, m)).collect();
    assert_eq!(periods, attendus);
    assert_eq!(statuses(&report), vec![MonthStatus::Created; 6]);
    assert!(report.months.iter().all(|m| m.worker < 3));
    for (index, period) in attendus.iter().enumerate() {
        let start = [JAN, FEB, MAR, APR, MAY, JUN][index];
        let (manifest, candles) = read_period(dir.path(), "BTC-USDC", tf, *period);
        assert_eq!(
            candles,
            expected(&run_of(start, HALF_HOUR, 12)),
            "{period:?}"
        );
        assert!(manifest.closed);
    }

    // Même résultat, à l'octet des hashes, avec un seul worker.
    let single_dir = tempfile::tempdir().unwrap();
    let single_server = Mocked::exchange(series);
    let single_client = single_server.client(5);
    let mut opts = options(single_dir.path(), &["BTC-USDC"], tf, JAN, JUL);
    opts.workers = 1;
    run(&single_client, &opts);
    for period in &attendus {
        assert_eq!(
            read_period(dir.path(), "BTC-USDC", tf, *period)
                .0
                .content_hash,
            read_period(single_dir.path(), "BTC-USDC", tf, *period)
                .0
                .content_hash
        );
    }
}

// --- Clôture ------------------------------------------------------------------------------------

#[test]
fn mois_de_cotation_clos_quand_le_debut_de_l_historique_est_etabli() {
    let tf = Timeframe::M30;
    let listed = JAN + 3 * DAY;
    let series = Series {
        confirmed: run_of(listed, HALF_HOUR, 20),
        unclosed: vec![],
    };
    let mocked = Mocked::exchange(series);
    let client = mocked.client(10);

    // `from` au début du mois : la fenêtre couvre la période entière.
    let dir = tempfile::tempdir().unwrap();
    run(&client, &options(dir.path(), &["BTC-USDC"], tf, JAN, FEB));
    assert!(
        read_period(dir.path(), "BTC-USDC", tf, month(2024, 1))
            .0
            .closed
    );

    // `from` au milieu du mois, avant la cotation : la pagination s'arrête sur une page vide sans
    // descendre sous `from`, donc rien n'existe avant. Clos, comme le mois de cotation de BTC-USDC.
    let dir = tempfile::tempdir().unwrap();
    run(
        &client,
        &options(dir.path(), &["BTC-USDC"], tf, JAN + DAY, FEB),
    );
    assert!(
        read_period(dir.path(), "BTC-USDC", tf, month(2024, 1))
            .0
            .closed
    );
}

#[test]
fn from_en_milieu_de_mois_avec_des_donnees_avant_ne_clot_pas() {
    let tf = Timeframe::M30;
    let series = Series {
        confirmed: run_of(JAN, HALF_HOUR, 200),
        unclosed: vec![],
    };
    let mocked = Mocked::exchange(series);
    let client = mocked.client(10);
    let dir = tempfile::tempdir().unwrap();
    // Des bougies existent avant `from` : le début du mois n'est pas ingéré, le fichier reste ouvert.
    run(
        &client,
        &options(dir.path(), &["BTC-USDC"], tf, JAN + DAY, FEB),
    );
    let (manifest, candles) = read_period(dir.path(), "BTC-USDC", tf, month(2024, 1));
    assert!(!manifest.closed);
    assert_eq!(candles.first().map(|c| c.open_time_ms), Some(JAN + DAY));
    // Un `--resume` avec `from` au début du mois complète la moitié manquante et clôt.
    let mut opts = options(dir.path(), &["BTC-USDC"], tf, JAN, FEB);
    opts.resume = true;
    run(&client, &opts);
    let (manifest, candles) = read_period(dir.path(), "BTC-USDC", tf, month(2024, 1));
    assert!(manifest.closed);
    assert_eq!(candles, expected(&run_of(JAN, HALF_HOUR, 200)));
}

#[test]
fn tf_de_1h_et_plus_fichier_annuel_barres_okx() {
    let dir = tempfile::tempdir().unwrap();
    let dec_2023 = JAN - 3 * DAY;
    let series = Series {
        confirmed: run_of(dec_2023, DAY, 6),
        unclosed: vec![],
    };
    let mocked = Mocked::exchange(series);
    let client = mocked.client(4);
    let tf = Timeframe::D1;

    let report = run(
        &client,
        &options(
            dir.path(),
            &["BTC-USDC"],
            tf,
            JAN - 30 * DAY,
            JAN + 366 * DAY,
        ),
    );

    // Un job par année de fichier (2023 et 2024), jamais par mois.
    let periods: Vec<Period> = report.months.iter().map(|m| m.period).collect();
    assert_eq!(
        periods,
        [Period::Year { year: 2023 }, Period::Year { year: 2024 }]
    );
    let (y2023, c2023) = read_period(dir.path(), "BTC-USDC", tf, Period::Year { year: 2023 });
    let (y2024, c2024) = read_period(dir.path(), "BTC-USDC", tf, Period::Year { year: 2024 });
    assert_eq!(c2023, expected(&run_of(dec_2023, DAY, 3)));
    assert_eq!(c2024, expected(&run_of(JAN, DAY, 3)));
    assert_eq!(y2023.period, "2023");
    assert_eq!(y2024.period, "2024");
    assert!(y2023.closed);
    // 2024 va jusqu'à JAN + 366 jours = 2025-01-01, fin de l'année bissextile : close aussi.
    assert!(y2024.closed);
    // Toute bougie 1d est demandée en `1Dutc`, jamais en `1D`.
    assert!(
        mocked
            .params_from(0, "bar")
            .iter()
            .all(|b| b.as_deref() == Some("1Dutc"))
    );
}

// --- Échecs -------------------------------------------------------------------------------------

#[test]
fn un_mois_en_echec_laisse_les_autres_valides_et_la_reprise_le_complete() {
    let tf = Timeframe::M30;
    let broken = Arc::new(AtomicI64::new(1));
    let datasets = BTreeMap::from([("BTC-USDC".to_string(), two_months())]);
    let flag = Arc::clone(&broken);
    let mocked = Mocked::start(move |query| {
        // Tant que `broken` vaut 1, la fenêtre de février (after = MAR) échoue en 500.
        if flag.load(Ordering::SeqCst) == 1 && query["after"] == MAR.to_string() {
            return (500, "{}".to_string());
        }
        (200, exchange_body(&datasets, query))
    });
    let client = mocked.client(10);
    let dir = tempfile::tempdir().unwrap();

    let report = run(&client, &options(dir.path(), &["BTC-USDC"], tf, JAN, MAR));

    assert_eq!(
        statuses(&report),
        [MonthStatus::Created, MonthStatus::Failed]
    );
    assert_eq!(report.failures().len(), 1);
    assert_eq!(report.failures()[0].period, month(2024, 2));
    // Janvier, écrit, est valide ; février n'a pas de fichier.
    let (jan, _) = read_period(dir.path(), "BTC-USDC", tf, month(2024, 1));
    assert!(jan.closed);
    assert!(!file_path(dir.path(), "BTC-USDC", tf, month(2024, 2)).exists());

    // L'échange revient : la reprise saute janvier (clos) et écrit février.
    broken.store(0, Ordering::SeqCst);
    let mut opts = options(dir.path(), &["BTC-USDC"], tf, JAN, MAR);
    opts.resume = true;
    let report = run(&client, &opts);
    assert_eq!(
        statuses(&report),
        [MonthStatus::SkippedClosed, MonthStatus::Created]
    );
    assert!(
        read_period(dir.path(), "BTC-USDC", tf, month(2024, 2))
            .0
            .closed
    );
}

#[test]
fn demandes_invalides_refusees_avant_toute_requete() {
    let dir = tempfile::tempdir().unwrap();
    let mocked = Mocked::exchange(Series::default());
    let client = mocked.client(10);
    let base = || options(dir.path(), &["BTC-USDC"], Timeframe::M1, JAN, FEB);

    // 12h : pas de barre OKX UTC vérifiée.
    let mut opts = base();
    opts.tf = Timeframe::H12;
    assert!(matches!(
        ingest_okx(&client, &opts),
        Err(CliError::Okx(OkxError::UnsupportedTimeframe { .. }))
    ));
    // Fenêtre vide.
    let mut opts = base();
    opts.to = opts.from;
    assert!(matches!(
        ingest_okx(&client, &opts),
        Err(CliError::Store(_))
    ));
    // Instrument en double.
    let opts = options(
        dir.path(),
        &["BTC-USDC", "BTC-USDC"],
        Timeframe::M1,
        JAN,
        FEB,
    );
    assert!(matches!(
        ingest_okx(&client, &opts),
        Err(CliError::DuplicatePair { .. })
    ));
    assert_eq!(mocked.count(), 0);
}

// --- Exécutable ---------------------------------------------------------------------------------

fn tv4_ingest(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tv4"));
    command.arg("ingest").arg("okx").args(args);
    // Le conteneur définit un proxy : le serveur de test est sur 127.0.0.1.
    for variable in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(variable);
    }
    command
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

#[test]
fn commande_de_bout_en_bout_sur_un_serveur_local() {
    let dir = tempfile::tempdir().unwrap();
    let series = Series {
        confirmed: run_of(JAN, DAY, 4),
        unclosed: vec![JAN + 4 * DAY],
    };
    let mocked = Mocked::exchange(series);
    let base_url = mocked.server.base_url();
    let root = dir.path().display().to_string();
    let args = |extra: &[&'static str]| -> Vec<String> {
        let mut args: Vec<String> = [
            "--inst",
            "BTC-USDC",
            "--tf",
            "1d",
            "--from",
            "2024-01-01",
            "--to",
            "2024-01-06",
            "--root",
            &root,
            "--base-url",
            &base_url,
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        args.extend(extra.iter().map(|s| (*s).to_string()));
        args
    };
    let run_cli = |extra: &[&'static str]| {
        let owned = args(extra);
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        tv4_ingest(&refs)
    };

    let output = run_cli(&[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("okx BTC-USDC 1d 2024"), "{stdout}");
    assert!(stdout.contains("1 rejets confirm = 0"), "{stdout}");
    assert!(
        stdout.contains("BTC-USDC 1d : 1 périodes (1 créées"),
        "{stdout}"
    );
    let tf = Timeframe::D1;
    let (manifest, candles) = read_period(dir.path(), "BTC-USDC", tf, Period::Year { year: 2024 });
    assert_eq!(candles, expected(&run_of(JAN, DAY, 4)));
    assert_eq!(manifest.continuity.unwrap().rejected_unclosed, 1);
    assert!(!manifest.closed);
    assert!(
        mocked
            .params_from(0, "bar")
            .iter()
            .all(|b| b.as_deref() == Some("1Dutc"))
    );

    // Relance avec --resume : mêmes données, content_hash inchangé.
    let hash = manifest.content_hash.clone();
    let output = run_cli(&["--resume"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_period(dir.path(), "BTC-USDC", tf, Period::Year { year: 2024 })
            .0
            .content_hash,
        hash
    );
}

#[test]
fn commande_en_echec_code_de_sortie_non_nul_et_motif() {
    let dir = tempfile::tempdir().unwrap();
    // 400 : erreur définitive, pas de retry.
    let mocked = Mocked::start(|_| (400, "{}".to_string()));
    let output = tv4_ingest(&[
        "--inst",
        "BTC-USDC",
        "--tf",
        "1d",
        "--from",
        "2024-01-01",
        "--to",
        "2024-01-06",
        "--root",
        &dir.path().display().to_string(),
        "--base-url",
        &mocked.server.base_url(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ingestion OKX incomplète"), "{stderr}");
    assert!(stderr.contains("BTC-USDC 2024"), "{stderr}");
    assert!(stderr.contains("HTTP 400"), "{stderr}");
}

#[test]
fn arguments_invalides_erreur_d_usage() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().display().to_string();
    for args in [
        vec!["--inst", "BTC-USDC", "--from", "2024-01-01T00:00:00Z"],
        vec!["--inst", "btc-usdc", "--from", "2024-01-01"],
        vec!["--inst", "BTC-USDC-SWAP", "--from", "2024-01-01"],
        vec!["--from", "2024-01-01"],
        vec!["--inst", "BTC-USDC"],
    ] {
        let mut args = args;
        args.extend(["--root", &root]);
        let output = tv4_ingest(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}
