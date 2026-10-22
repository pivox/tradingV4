//! `tv4 ingest okx` : ingestion des bougies OKX `history-candles` (docs/03-architecture.md §11, S7,
//! deuxième partie).
//!
//! Le client (`marketdata::okx`, première partie) ne fait que lire. L'orchestration qui écrit des
//! fichiers vit ici, dans `cli`, comme `ingest kraken` : découpage en jobs, threads, tampon de pages,
//! fusion et écriture par `store::merge_period_with`, reprise.
//!
//! # Jobs et threads
//!
//! La fenêtre `[from, to)` est découpée par **période de fichier** du store : un mois pour les TF
//! inférieurs à 1h (1m, 5m, 15m, 30m), **une année** pour les TF de 1h et plus (4h, 1d), puisque le
//! store écrit un fichier annuel pour ceux-là (docs/03 §5.1). Un job est une période d'un instrument ;
//! ses sous-fenêtres (reprise) sont traitées l'une après l'autre dans le même job, donc par un seul
//! thread : aucune période n'est écrite par deux threads. Trois workers `std::thread::scope` tirent
//! les jobs d'une file commune (un compteur atomique) avec des clones du même `OkxClient`, donc le
//! même token bucket (8 requêtes par seconde pour l'ensemble). Les écritures de périodes différentes
//! d'une même partition se succèdent sous son verrou, attendu par `PartitionLock::acquire_wait`.
//!
//! # Tampon et écriture
//!
//! `fetch_window` remet les pages à `on_page`, du présent vers le passé. Le job les tamponne et, tous
//! les `batch_pages` pages (50 par défaut) puis à la fin de la dernière fenêtre, fusionne le tampon avec
//! le fichier de la période et le réécrit (fichier temporaire, `fsync`, `rename`, manifeste, sous le
//! verrou). Un arrêt perd au plus un lot (50 pages, soit 15 000 bougies 1m, environ 6 s de requêtes) ; le
//! fichier écrit reste toujours valide et contigu : il couvre `[x, fin)` après un arrêt, pas `[début, x)`.
//!
//! **Règle de contiguïté (S7-OKX-1).** Un lot intermédiaire n'est permis que si la pagination reste
//! collée aux données du fichier, c'est-à-dire si le premier lot écrit touche ou chevauche le fichier : la
//! période n'a pas de fichier, ou la fenêtre est une sous-fenêtre basse qui descend sous `ts_min` en
//! partant d'un point situé dans `[ts_min, ts_max + barre]` ([`window_is_glued`]). Sinon (sous-fenêtre
//! haute, ou fenêtre entière sur un fichier existant, avec ou sans `--resume`), la pagination part du
//! haut de la fenêtre, loin des données présentes : un premier lot écrit y laisserait, en cas d'échec
//! ensuite, un trou au milieu du fichier, invisible pour la reprise qui ne regarde que les extrémités. Le
//! job tamponne alors toute la fenêtre et fait **une seule écriture à la fin** : au plus un mois de 1m
//! (44 640 bougies, quelques Mo) ou un an de 4h ou de 1d. Un échec ne laisse alors que le fichier d'avant.
//! Les `ts` des bougies `confirm = "0"` vont dans `continuity.rejected_unclosed` du manifeste ; les
//! `duplicates` restent à 0 (le chevauchement d'une reprise est notre protocole, pas une anomalie de
//! la source), si bien que deux ingestions sans nouvelle donnée donnent des manifestes identiques.
//!
//! # Clôture d'un fichier
//!
//! Un fichier OKX n'est **clos** que si une ingestion a couvert toute sa période, ce qui n'est pas
//! « la période est passée » (règle D4 de S4, qui figerait un mois ingéré à moitié). Conditions : la
//! fenêtre demandée atteint la fin de la période (`to` au-delà de cette fin, bornée par l'instant présent)
//! **et** son début est établi, soit parce que `from` est au plus tôt le début de la période, soit parce
//! que la pagination a atteint une page vide sans descendre sous le début de la fenêtre (rien n'existe
//! avant : mois de cotation d'une paire), soit parce que le fichier contient déjà la première bougie de la
//! période, **et** aucune bougie `confirm = "0"` rencontrée dans la période ne manque encore au fichier
//! fusionné (S7-OKX-2 : le `rejected_unclosed` ajusté du manifeste vaut 0 ; sinon le fichier reste ouvert,
//! la bougie manquante pour toujours une fois clos, et une reprise ultérieure la récupère). Les lots
//! intermédiaires n'ont jamais `closed = true`. Un fichier clos n'est plus retéléchargé par `--resume`, ni
//! réécrit avec un contenu différent par le store.
//!
//! # Reprise (`--resume`)
//!
//! La pagination va vers le passé, donc un arrêt laisse un fichier qui couvre `[ts_min, ts_max]` avec
//! `ts_min` au milieu de la période. Une reprise par `max(open_time)` seule laisserait un trou en bas. La
//! règle, par période, à partir du manifeste du fichier (`page = limit × durée de la barre`, c'est la
//! page chevauchante retéléchargée ; la déduplication de la fusion absorbe le chevauchement) :
//!
//! - fichier absent : toute la fenêtre `[max(début de période, from), min(fin de période, to))` ;
//! - fichier clos : aucune requête ;
//! - sinon une sous-fenêtre basse `[début de fenêtre, ts_min + page)` si `ts_min` est après le début de la
//!   fenêtre, et une sous-fenêtre haute `[ts_max − page, fin de fenêtre)` si `ts_max` est avant la dernière
//!   bougie possible (`fin − barre`) ; si elles se touchent, une seule fenêtre entière.
//!
//! Sans `--resume`, toute la fenêtre est retéléchargée et fusionnée au fichier existant (le manifeste est
//! quand même lu : il décide des lots intermédiaires, de l'établissement du début et du compte rendu).
//! Limite assumée : la reprise ne répare que les extrémités ; un trou au milieu d'un fichier (bougies
//! absentes de la source, ou fichier édité) ne se comble qu'en relançant sans `--resume`. La règle de
//! contiguïté ci-dessus garantit que l'ingestion elle-même n'en crée pas.
//!
//! # Compte rendu
//!
//! Les lignes, la clôture et le `content_hash` d'un [`MonthReport`] viennent du manifeste relu sur le
//! disque après le job (S7-OKX-3), jamais d'un état supposé : un job en échec dont des lots sont déjà
//! écrits les rend, et un job qui n'écrit rien rend ceux du fichier présent. `Empty` ne signifie que
//! « aucun fichier ».

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use clap::Args;

use super::{CliError, now_ms};
use crate::marketdata::okx::{
    FetchStats, OkxClient, OkxConfig, OkxError, OkxPage, WindowError, fetch_window, okx_bar,
};
use crate::model::{Candle, ExchangeId, Timeframe, Ts};
use crate::store::time::{format_rfc3339_utc, parse_rfc3339_utc};
use crate::store::{
    self, Columns, ContinuityInput, Partition, Period, StoreError, Symbol, Window, WriteOptions,
    WriteOutcome, WriteStatus,
};

/// Source de provenance écrite dans les manifestes (`[a-z0-9-]{1,32}`).
const SOURCE_OKX: &str = "okx-history-candles";

/// Threads de téléchargement (docs/03 §3, ligne HTTP : 3 threads mesurés à 7,95 req/s).
pub const DEFAULT_WORKERS: usize = 3;

/// Pages tamponnées entre deux écritures du fichier de la période (docs/03 §11, S7).
pub const DEFAULT_BATCH_PAGES: usize = 50;

/// Attente maximale du verrou de partition par un worker. Une écriture dure quelques dizaines de
/// millisecondes : au-delà, un autre processus tient le verrou.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(120);

/// Date UTC `AAAA-MM-JJ` lue sur la ligne de commande : le jour à 00:00:00 UTC, en millisecondes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcDate(pub Ts);

impl FromStr for UtcDate {
    type Err = String;

    fn from_str(text: &str) -> Result<UtcDate, String> {
        let invalid = || format!("date « {text} » invalide : AAAA-MM-JJ (UTC) attendu");
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(invalid());
        }
        parse_rfc3339_utc(&format!("{text}T00:00:00Z"))
            .map(UtcDate)
            .map_err(|_| invalid())
    }
}

/// Arguments de `tv4 ingest okx`.
#[derive(Debug, Args)]
pub struct IngestOkxArgs {
    /// Instrument OKX spot (instId, par exemple BTC-USDC ou BTC-USDT) ; répétable.
    #[arg(long = "inst", value_name = "INSTID", required = true)]
    pub insts: Vec<Symbol>,
    /// Timeframe des bougies (1m par défaut ; 4h et 1d sont demandés à OKX en 4H et 1Dutc).
    #[arg(long, value_name = "TF", default_value = "1m")]
    pub tf: Timeframe,
    /// Début inclus, date UTC AAAA-MM-JJ (00:00 UTC).
    #[arg(long, value_name = "AAAA-MM-JJ")]
    pub from: UtcDate,
    /// Fin exclue, date UTC AAAA-MM-JJ (00:00 UTC) ; par défaut l'instant présent.
    #[arg(long, value_name = "AAAA-MM-JJ")]
    pub to: Option<UtcDate>,
    /// Reprend : ne télécharge que ce qui manque aux fichiers présents (extrémités), saute les fichiers
    /// clos. Sans cette option, toute la fenêtre est retéléchargée et fusionnée. Lançable par cron.
    #[arg(long)]
    pub resume: bool,
    /// Racine de travail (contient data/, state/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
    /// Racine de l'API, pour les tests de bout en bout sur un serveur local (défaut : eea.okx.com).
    #[arg(long, value_name = "URL", hide = true)]
    pub base_url: Option<String>,
}

/// Réglages d'une ingestion, tous injectables : les tests réduisent le lot et fixent l'horloge.
#[derive(Clone, Debug)]
pub struct OkxIngestOptions {
    /// Racine de travail.
    pub root: PathBuf,
    /// Instruments (instId = symbole canonique), traités par la même file de jobs.
    pub insts: Vec<Symbol>,
    pub tf: Timeframe,
    /// Début inclus de la fenêtre.
    pub from: Ts,
    /// Fin exclue de la fenêtre (au plus `now_ms`).
    pub to: Ts,
    /// Instant présent injecté (`ingested_at`, clôture des fichiers).
    pub now_ms: Ts,
    pub resume: bool,
    /// Nombre de threads de téléchargement.
    pub workers: usize,
    /// Pages entre deux écritures du fichier d'une période.
    pub batch_pages: usize,
    /// Attente maximale du verrou de partition.
    pub lock_timeout: Duration,
}

impl OkxIngestOptions {
    /// Réglages de production : 3 workers, lot de 50 pages, `to` borné par `now_ms`.
    pub fn new(
        root: PathBuf,
        insts: Vec<Symbol>,
        tf: Timeframe,
        from: Ts,
        to: Ts,
        now_ms: Ts,
        resume: bool,
    ) -> OkxIngestOptions {
        OkxIngestOptions {
            root,
            insts,
            tf,
            from,
            to: to.min(now_ms),
            now_ms,
            resume,
            workers: DEFAULT_WORKERS,
            batch_pages: DEFAULT_BATCH_PAGES,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }
}

/// Issue d'un job (une période d'un instrument).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonthStatus {
    /// Premier fichier de la période.
    Created,
    /// Fichier existant réécrit (contenu, clôture ou continuité changés).
    Rewritten,
    /// Fichier existant sans changement (ou rien à télécharger).
    Unchanged,
    /// `--resume` : fichier clos, aucune requête.
    SkippedClosed,
    /// Aucune bougie dans la fenêtre et aucun fichier : rien n'est écrit.
    Empty,
    /// Échec après épuisement des essais ou erreur d'écriture ; les écritures déjà faites restent valides.
    Failed,
}

impl MonthStatus {
    const fn label(self) -> &'static str {
        match self {
            MonthStatus::Created => "créé",
            MonthStatus::Rewritten => "réécrit",
            MonthStatus::Unchanged => "inchangé",
            MonthStatus::SkippedClosed => "clos, ignoré",
            MonthStatus::Empty => "vide",
            MonthStatus::Failed => "ÉCHEC",
        }
    }
}

/// Compte rendu d'un job.
#[derive(Clone, Debug)]
pub struct MonthReport {
    /// instId.
    pub inst: String,
    pub period: Period,
    pub status: MonthStatus,
    /// Lignes du fichier après le job (0 sans fichier).
    pub rows: u64,
    /// Fichier clos après le job.
    pub closed: bool,
    pub content_hash: Option<String>,
    /// Sous-fenêtres téléchargées (0 : rien à télécharger).
    pub windows: usize,
    /// Réponses `code = "0"` reçues, pages vides finales comprises.
    pub pages: u64,
    /// Requêtes HTTP envoyées, retries compris.
    pub requests: u64,
    pub retries: u64,
    /// Bougies `confirm = "0"` rencontrées (distinctes).
    pub rejected_unclosed: u64,
    /// Écritures du fichier (lots intermédiaires et finale).
    pub writes: u64,
    /// Numéro du worker (0 à `workers - 1`) qui a traité le job.
    pub worker: usize,
    pub duration: Duration,
    /// Motif de l'échec.
    pub error: Option<String>,
}

/// Compte rendu d'une ingestion : un [`MonthReport`] par job, dans l'ordre des jobs (instrument puis
/// période croissante).
#[derive(Clone, Debug)]
pub struct OkxIngestReport {
    pub months: Vec<MonthReport>,
    pub duration: Duration,
}

impl OkxIngestReport {
    /// Jobs en échec.
    pub fn failures(&self) -> Vec<&MonthReport> {
        self.months
            .iter()
            .filter(|m| m.status == MonthStatus::Failed)
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Planification
// ---------------------------------------------------------------------------------------------

/// Une période d'un instrument à ingérer.
struct Job {
    inst: Symbol,
    partition: Partition,
    period: Period,
    /// `[max(début de période, from), min(fin de période, to))`.
    request: Range<Ts>,
}

/// Jobs de la fenêtre : pour chaque instrument, les périodes de fichier qui la recoupent.
fn plan_jobs(options: &OkxIngestOptions) -> Result<Vec<Job>, CliError> {
    let mut jobs = Vec::new();
    for inst in &options.insts {
        let partition = Partition::new(ExchangeId::Okx, inst.clone(), options.tf);
        let mut period = Period::containing(options.tf, options.from)?;
        loop {
            let window = period.window();
            if window.start_ms() >= options.to {
                break;
            }
            jobs.push(Job {
                inst: inst.clone(),
                partition: partition.clone(),
                period,
                request: window.start_ms().max(options.from)..window.end_ms().min(options.to),
            });
            period = period.next();
        }
    }
    Ok(jobs)
}

/// Sous-fenêtres à télécharger pour `request` d'après le fichier présent `(ts_min, ts_max)`, du plus
/// ancien au plus récent. Règle de reprise détaillée dans la documentation du module ; `bar` est la
/// durée de la barre et `page` vaut `limit × bar`. Un fichier clos est traité avant l'appel.
fn plan_windows(request: &Range<Ts>, file: Option<(Ts, Ts)>, bar: Ts, page: Ts) -> Vec<Range<Ts>> {
    let Some((ts_min, ts_max)) = file else {
        return vec![request.clone()];
    };
    // Moitié ancienne manquante (arrêt en milieu de période, ou période de cotation).
    let low = (ts_min > request.start)
        .then(|| request.start..request.end.min(ts_min.saturating_add(page)));
    // Moitié récente manquante (période courante, ou fenêtre élargie).
    let high = (ts_max < request.end - bar)
        .then(|| request.start.max(ts_max.saturating_sub(page))..request.end);
    match (low, high) {
        (Some(low), Some(high)) if low.end >= high.start => vec![request.clone()],
        (Some(low), Some(high)) => vec![low, high],
        (Some(window), None) | (None, Some(window)) => vec![window],
        (None, None) => Vec::new(),
    }
}

/// Vrai si le premier lot écrit pour `window` serait collé au fichier `(ts_min, ts_max)` : la fenêtre
/// descend sous `ts_min` (sous-fenêtre basse) et sa borne haute, d'où part la pagination, tombe entre
/// `ts_min` et `ts_max + bar`, donc la première page chevauche ou touche les données présentes.
/// Sans fichier, la fenêtre est la première donnée de la période : collée par définition.
fn window_is_glued(window: &Range<Ts>, file: Option<(Ts, Ts)>, bar: Ts) -> bool {
    match file {
        None => true,
        Some((ts_min, ts_max)) => {
            window.start < ts_min
                && ts_min <= window.end
                && window.end <= ts_max.saturating_add(bar)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Exécution d'un job
// ---------------------------------------------------------------------------------------------

/// Erreur d'un job : celle du client OKX ou celle du store.
#[derive(Debug, thiserror::Error)]
enum JobError {
    #[error(transparent)]
    Okx(#[from] OkxError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<WindowError<StoreError>> for JobError {
    fn from(error: WindowError<StoreError>) -> JobError {
        match error {
            WindowError::Okx(error) => JobError::Okx(error),
            WindowError::OnPage(error) => JobError::Store(error),
        }
    }
}

/// Résultat d'un job réussi.
struct JobDone {
    status: MonthStatus,
    rows: u64,
    closed: bool,
    content_hash: Option<String>,
    windows: usize,
    stats: FetchStats,
    rejected_unclosed: u64,
    writes: u64,
}

/// Paramètres communs à tous les jobs.
struct Context<'a> {
    options: &'a OkxIngestOptions,
    /// Durée de la barre en millisecondes.
    bar: Ts,
    /// `limit × bar` : la page chevauchante retéléchargée par une reprise.
    page: Ts,
}

/// État d'un job : tampon de pages et suivi des écritures.
struct JobState<'a> {
    ctx: &'a Context<'a>,
    job: &'a Job,
    buffer: Vec<Candle>,
    rejected: BTreeSet<Ts>,
    pages_since_write: usize,
    /// Vrai : la fenêtre en cours est collée au fichier ([`window_is_glued`]), les lots intermédiaires
    /// sont permis. Faux : tout est tamponné jusqu'à la fin de la fenêtre.
    glued: bool,
    /// Plus ancienne ligne reçue dans la fenêtre en cours (bougies non clôturées comprises).
    min_oldest: Option<Ts>,
    created: bool,
    rewritten: bool,
    writes: u64,
    last: Option<WriteOutcome>,
}

impl JobState<'_> {
    /// `on_page` de `fetch_window` : tamponne la page et écrit le lot quand il est plein.
    fn on_page(&mut self, page: &OkxPage) -> Result<(), StoreError> {
        self.buffer.extend_from_slice(&page.candles);
        self.rejected.extend(page.rejected_unclosed.iter().copied());
        if let Some(oldest) = page.oldest_ts {
            self.min_oldest = Some(self.min_oldest.map_or(oldest, |m| m.min(oldest)));
        }
        self.pages_since_write += 1;
        if self.glued && self.pages_since_write >= self.ctx.options.batch_pages.max(1) {
            // Lot intermédiaire, collé au fichier : jamais clos.
            self.write(0, false)?;
        }
        Ok(())
    }

    /// Fusionne le tampon avec le fichier de la période et l'écrit. `closed_until_ms` est la borne de
    /// clôture du store (0 : ne clôt rien). `force` écrit même un tampon vide (clôture d'un fichier dont
    /// la couverture vient d'être établie).
    fn write(&mut self, closed_until_ms: Ts, force: bool) -> Result<(), StoreError> {
        self.pages_since_write = 0;
        if self.buffer.is_empty() && !force {
            return Ok(());
        }
        self.buffer.sort_by_key(|c| c.open_time_ms);
        self.buffer.dedup_by_key(|c| c.open_time_ms);
        let columns = Columns::from_candles(&self.buffer);
        let rejected: Vec<Ts> = self.rejected.iter().copied().collect();
        let options = WriteOptions {
            source: SOURCE_OKX,
            now_ms: self.ctx.options.now_ms,
            closed_until_ms,
            continuity: Some(ContinuityInput {
                duplicates: &[],
                rejected_unclosed: &rejected,
            }),
        };
        let outcome = store::merge_period_with(
            &self.ctx.options.root,
            &self.job.partition,
            self.job.period,
            &columns,
            &options,
            self.ctx.options.lock_timeout,
        )?;
        self.buffer.clear();
        if let Some(outcome) = outcome {
            self.writes += 1;
            match outcome.status {
                WriteStatus::Created => self.created = true,
                WriteStatus::Rewritten => self.rewritten = true,
                WriteStatus::Unchanged => {}
            }
            self.last = Some(outcome);
        }
        Ok(())
    }
}

/// Un job : plan de reprise, téléchargement des sous-fenêtres, écritures, clôture.
fn run_job(client: &OkxClient, ctx: &Context<'_>, job: &Job) -> Result<JobDone, JobError> {
    let options = ctx.options;
    let period_window = job.period.window();
    let (start, end) = (job.request.start, job.request.end);

    // Manifeste du fichier présent, toujours lu : `--resume` décide de ce qu'on en fait, mais la
    // contiguïté des lots, l'établissement du début et le compte rendu en dépendent aussi sans lui.
    let manifest_path = store::resolve_rel(&options.root, &job.partition.manifest_rel(job.period))?;
    let existing = if manifest_path.is_file() {
        Some(store::read_file_manifest(&manifest_path)?)
    } else {
        None
    };
    if let Some(manifest) = existing.as_ref().filter(|m| options.resume && m.closed) {
        return Ok(JobDone {
            status: MonthStatus::SkippedClosed,
            rows: manifest.row_count,
            closed: true,
            content_hash: Some(manifest.content_hash.clone()),
            windows: 0,
            stats: FetchStats::default(),
            rejected_unclosed: 0,
            writes: 0,
        });
    }
    let file_range = existing.as_ref().map(|m| (m.ts_min, m.ts_max));
    let windows = if options.resume {
        plan_windows(&job.request, file_range, ctx.bar, ctx.page)
    } else {
        vec![job.request.clone()]
    };

    let mut state = JobState {
        ctx,
        job,
        buffer: Vec::new(),
        rejected: BTreeSet::new(),
        pages_since_write: 0,
        glued: true,
        min_oldest: None,
        created: false,
        rewritten: false,
        writes: 0,
        last: None,
    };
    let mut stats = FetchStats::default();
    // Début de l'historique établi par la pagination : la fenêtre qui commence à `start` s'est arrêtée
    // sur une page vide sans descendre sous `start`, donc rien n'existe avant (période de cotation).
    let mut history_start_proven = false;
    for (index, window) in windows.iter().enumerate() {
        state.min_oldest = None;
        state.glued = window_is_glued(window, file_range, ctx.bar);
        let fetch = fetch_window(
            client,
            job.inst.as_str(),
            options.tf,
            window.start,
            window.end,
            |page| state.on_page(page),
        )?;
        stats.pages += fetch.stats.pages;
        stats.requests += fetch.stats.requests;
        stats.retries += fetch.stats.retries;
        if window.start == start {
            history_start_proven = state.min_oldest.is_none_or(|oldest| oldest >= start);
        }
        // Fin de fenêtre : le tampon est écrit ; la dernière écriture est celle de la clôture.
        if index + 1 < windows.len() {
            state.write(0, false)?;
        }
    }

    // Clôture : la fenêtre atteint la fin de la période, son début est établi, et aucune bougie
    // `confirm = "0"` de la période ne manque encore.
    let start_established = start == period_window.start_ms()
        || history_start_proven
        || existing
            .as_ref()
            .is_some_and(|m| m.ts_min <= period_window.start_ms());
    let mut covered = end == period_window.end_ms() && start_established;
    if covered && !state.rejected.is_empty() {
        // Écriture sans clôture d'abord : le store retire des rejets les bougies présentes dans le
        // résultat fusionné ; il en reste (compte ajusté > 0) si une bougie de la période manque encore.
        state.write(0, true)?;
        covered = state
            .last
            .as_ref()
            .and_then(|outcome| outcome.continuity)
            .is_some_and(|continuity| continuity.rejected_unclosed == 0);
    }
    if covered || !state.buffer.is_empty() {
        let closed_until_ms = if covered { options.now_ms } else { 0 };
        state.write(closed_until_ms, true)?;
    }

    // État du disque après le job : l'écriture finale si le job en a fait une, sinon le manifeste relu.
    let (status, rows, closed, content_hash) = match (&state.last, disk_manifest(ctx, job)) {
        (Some(last), _) => {
            let status = if state.created {
                MonthStatus::Created
            } else if state.rewritten {
                MonthStatus::Rewritten
            } else {
                MonthStatus::Unchanged
            };
            (
                status,
                last.row_count,
                last.closed,
                Some(last.content_hash.clone()),
            )
        }
        (None, Some(manifest)) => (
            MonthStatus::Unchanged,
            manifest.row_count,
            manifest.closed,
            Some(manifest.content_hash),
        ),
        (None, None) => (MonthStatus::Empty, 0, false, None),
    };
    Ok(JobDone {
        status,
        rows,
        closed,
        content_hash,
        windows: windows.len(),
        stats,
        rejected_unclosed: state.rejected.len() as u64,
        writes: state.writes,
    })
}

/// Manifeste du fichier de la période tel qu'il est sur le disque maintenant (`None` : pas de fichier, ou
/// manifeste illisible, ce que la ligne de compte rendu ne peut pas réparer).
fn disk_manifest(ctx: &Context<'_>, job: &Job) -> Option<store::FileManifest> {
    let path =
        store::resolve_rel(&ctx.options.root, &job.partition.manifest_rel(job.period)).ok()?;
    if path.is_file() {
        store::read_file_manifest(&path).ok()
    } else {
        None
    }
}

fn month_report(
    ctx: &Context<'_>,
    job: &Job,
    worker: usize,
    result: Result<JobDone, JobError>,
    started: Instant,
) -> MonthReport {
    let base = MonthReport {
        inst: job.inst.to_string(),
        period: job.period,
        status: MonthStatus::Failed,
        rows: 0,
        closed: false,
        content_hash: None,
        windows: 0,
        pages: 0,
        requests: 0,
        retries: 0,
        rejected_unclosed: 0,
        writes: 0,
        worker,
        duration: started.elapsed(),
        error: None,
    };
    match result {
        Ok(done) => MonthReport {
            status: done.status,
            rows: done.rows,
            closed: done.closed,
            content_hash: done.content_hash,
            windows: done.windows,
            pages: done.stats.pages,
            requests: done.stats.requests,
            retries: done.stats.retries,
            rejected_unclosed: done.rejected_unclosed,
            writes: done.writes,
            ..base
        },
        // Échec : des lots peuvent déjà être écrits, le compte rendu décrit le disque.
        Err(error) => {
            let disk = disk_manifest(ctx, job);
            MonthReport {
                rows: disk.as_ref().map_or(0, |m| m.row_count),
                closed: disk.as_ref().is_some_and(|m| m.closed),
                content_hash: disk.map(|m| m.content_hash),
                error: Some(error.to_string()),
                ..base
            }
        }
    }
}

fn seconds(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64())
}

fn print_month(tf: Timeframe, month: &MonthReport) {
    let tete = format!("okx {} {tf} {}", month.inst, month.period.label());
    if let Some(error) = &month.error {
        println!("{tete} : ÉCHEC : {error}");
        return;
    }
    let cloture = if month.closed { "clos" } else { "non clos" };
    println!(
        "{tete} : {}, {} bougies, {} rejets confirm = 0, {} requêtes ({} retries), {cloture}, {} s",
        month.status.label(),
        month.rows,
        month.rejected_unclosed,
        month.requests,
        month.retries,
        seconds(month.duration)
    );
}

/// Résumé d'un instrument : mois par statut, bougies en fichier, rejets, requêtes.
fn print_summary(tf: Timeframe, inst: &Symbol, months: &[&MonthReport], duration: Duration) {
    let count = |status| months.iter().filter(|m| m.status == status).count();
    let sum = |field: fn(&MonthReport) -> u64| months.iter().map(|m| field(m)).sum::<u64>();
    println!(
        "{inst} {tf} : {} périodes ({} créées, {} réécrites, {} inchangées, {} closes ignorées, {} vides, {} en échec) ; {} bougies en fichier ; {} rejets confirm = 0 ; {} requêtes dont {} retries ; {} s",
        months.len(),
        count(MonthStatus::Created),
        count(MonthStatus::Rewritten),
        count(MonthStatus::Unchanged),
        count(MonthStatus::SkippedClosed),
        count(MonthStatus::Empty),
        count(MonthStatus::Failed),
        sum(|m| m.rows),
        sum(|m| m.rejected_unclosed),
        sum(|m| m.requests),
        sum(|m| m.retries),
        seconds(duration)
    );
}

/// Ingestion OKX d'une fenêtre pour un ou plusieurs instruments ; imprime un compte rendu par période
/// et un résumé par instrument.
///
/// Rend `Err` seulement pour une demande invalide (timeframe sans barre OKX UTC, fenêtre vide,
/// instrument en double). Un job en échec n'arrête pas les autres : son motif est dans le
/// [`MonthReport`] (statut [`MonthStatus::Failed`]), les fichiers déjà écrits restent valides, et
/// l'appelant décide du code de sortie ([`OkxIngestReport::failures`]).
pub fn ingest_okx(
    client: &OkxClient,
    options: &OkxIngestOptions,
) -> Result<OkxIngestReport, CliError> {
    let started = Instant::now();
    let bar = options.tf.ms();
    okx_bar(options.tf)?;
    Window::new(options.from, options.to)?;
    let mut vus = BTreeSet::new();
    for inst in &options.insts {
        if !vus.insert(inst.as_str()) {
            return Err(CliError::DuplicatePair {
                pair: inst.to_string(),
            });
        }
    }

    let jobs = plan_jobs(options)?;
    let ctx = Context {
        options,
        bar,
        page: i64::from(client.config().limit) * bar,
    };
    println!(
        "okx : {} instrument(s), {}, fenêtre [{}, {}), {} période(s) de fichier, {} workers{}",
        options.insts.len(),
        options.tf,
        format_rfc3339_utc(options.from)?,
        format_rfc3339_utc(options.to)?,
        jobs.len(),
        options.workers.clamp(1, jobs.len().max(1)),
        if options.resume { ", reprise" } else { "" }
    );

    // File de jobs : un compteur atomique. Chaque indice n'est servi qu'une fois, donc une période
    // n'est jamais traitée par deux threads.
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, MonthReport)>> = Mutex::new(Vec::with_capacity(jobs.len()));
    let workers = options.workers.clamp(1, jobs.len().max(1));
    let panicked = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let client = client.clone();
                let (jobs, ctx, next, results) = (&jobs, &ctx, &next, &results);
                scope.spawn(move || {
                    loop {
                        let index = next.fetch_add(1, Ordering::SeqCst);
                        let Some(job) = jobs.get(index) else { break };
                        let job_started = Instant::now();
                        let result = run_job(&client, ctx, job);
                        let report = month_report(ctx, job, worker, result, job_started);
                        print_month(ctx.options.tf, &report);
                        results
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push((index, report));
                    }
                })
            })
            .collect();
        // Tous les threads sont joints, même après une panique : `scope` les attendrait de toute façon.
        let panics: Vec<bool> = handles
            .into_iter()
            .map(|handle| handle.join().is_err())
            .collect();
        panics.contains(&true)
    });
    if panicked {
        return Err(CliError::OkxWorkerPanicked);
    }

    let mut months = results.into_inner().unwrap_or_else(PoisonError::into_inner);
    months.sort_by_key(|(index, _)| *index);
    let months: Vec<MonthReport> = months.into_iter().map(|(_, report)| report).collect();
    let duration = started.elapsed();
    for inst in &options.insts {
        let of_inst: Vec<&MonthReport> =
            months.iter().filter(|m| m.inst == inst.as_str()).collect();
        let inst_duration = of_inst.iter().map(|m| m.duration).sum::<Duration>();
        print_summary(options.tf, inst, &of_inst, inst_duration);
    }
    println!("okx : durée totale {} s", seconds(duration));
    Ok(OkxIngestReport { months, duration })
}

/// `tv4 ingest okx` : enrobage de [`ingest_okx`] ; code de sortie non nul si une période a échoué.
pub(super) fn okx(args: &IngestOkxArgs) -> Result<(), CliError> {
    let now = now_ms()?;
    let to = args.to.map_or(now, |date| date.0);
    let mut config = OkxConfig::default();
    if let Some(url) = &args.base_url {
        config.base_url = url.trim_end_matches('/').to_string();
    }
    let client = OkxClient::new(config);
    let options = OkxIngestOptions::new(
        args.root.clone(),
        args.insts.clone(),
        args.tf,
        args.from.0,
        to,
        now,
        args.resume,
    );
    let report = ingest_okx(&client, &options)?;
    let failures = report.failures();
    if failures.is_empty() {
        return Ok(());
    }
    let failed: Vec<String> = failures
        .iter()
        .map(|m| {
            format!(
                "{} {} ({})",
                m.inst,
                m.period.label(),
                m.error.as_deref().unwrap_or("?")
            )
        })
        .collect();
    Err(CliError::OkxIncomplete {
        failed: failed.join(" ; "),
    })
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // une sous-fenêtre = une plage
mod tests {
    use super::*;

    const MIN: Ts = 60_000;

    #[test]
    fn date_utc_aaaa_mm_jj() {
        assert_eq!("2025-08-21".parse(), Ok(UtcDate(1_755_734_400_000)));
        assert_eq!("1970-01-01".parse(), Ok(UtcDate(0)));
        for illisible in [
            "",
            "2025-8-21",
            "2025/08/21",
            "2025-13-01",
            "2025-02-30",
            "2025-08-21T00:00:00Z",
            "demain",
        ] {
            assert!(illisible.parse::<UtcDate>().is_err(), "« {illisible} »");
        }
    }

    #[test]
    fn fichier_absent_toute_la_fenetre() {
        let request = 1_000 * MIN..5_000 * MIN;
        assert_eq!(plan_windows(&request, None, MIN, 300 * MIN), [request]);
    }

    #[test]
    fn arret_en_milieu_de_mois_page_chevauchante_et_moitie_manquante() {
        // Fichier [3 000, 4 900] minutes dans la fenêtre [0, 5 000) : bas [0, 3 300), haut [4 600, 5 000).
        let request = 0..5_000 * MIN;
        let windows = plan_windows(&request, Some((3_000 * MIN, 4_900 * MIN)), MIN, 300 * MIN);
        assert_eq!(windows, [0..3_300 * MIN, 4_600 * MIN..5_000 * MIN]);
    }

    #[test]
    fn fichier_complet_aucune_requete() {
        // ts_min au début, ts_max = fin − barre.
        let request = 0..5_000 * MIN;
        assert!(plan_windows(&request, Some((0, 4_999 * MIN)), MIN, 300 * MIN).is_empty());
    }

    #[test]
    fn periode_courante_seule_la_sous_fenetre_haute() {
        let request = 0..5_000 * MIN;
        let windows = plan_windows(&request, Some((0, 4_000 * MIN)), MIN, 300 * MIN);
        assert_eq!(windows, [3_700 * MIN..5_000 * MIN]);
    }

    #[test]
    fn periode_de_cotation_sous_fenetre_basse_seule() {
        // La paire est cotée à partir de 2 000 : ts_min > début, ts_max = fin − barre.
        let request = 0..5_000 * MIN;
        let windows = plan_windows(&request, Some((2_000 * MIN, 4_999 * MIN)), MIN, 300 * MIN);
        assert_eq!(windows, [0..2_300 * MIN]);
    }

    #[test]
    fn sous_fenetres_qui_se_touchent_fusionnees() {
        // Fichier court [100, 200] : bas [0, 400) et haut [-100 → 0, 5 000) se recouvrent : une seule fenêtre.
        let request = 0..5_000 * MIN;
        let windows = plan_windows(&request, Some((100 * MIN, 200 * MIN)), MIN, 300 * MIN);
        assert_eq!(windows, [request]);
    }

    #[test]
    fn fichier_hors_de_la_fenetre_demandee() {
        let request = 10_000 * MIN..11_000 * MIN;
        // Fichier entièrement avant la fenêtre : tout est à télécharger (haut = fenêtre entière).
        let avant = plan_windows(&request, Some((0, 5_000 * MIN)), MIN, 300 * MIN);
        assert_eq!(avant, std::slice::from_ref(&request));
        // Fichier entièrement après : bas = fenêtre entière.
        let apres = plan_windows(&request, Some((20_000 * MIN, 21_000 * MIN)), MIN, 300 * MIN);
        assert_eq!(apres, [request]);
    }

    #[test]
    fn lots_intermediaires_seulement_quand_la_fenetre_est_collee_au_fichier() {
        let request = 0..5_000 * MIN;
        let file = Some((3_000 * MIN, 4_900 * MIN));
        // Fichier absent : collé.
        assert!(window_is_glued(&request, None, MIN));
        // Sous-fenêtre basse [0, 3 300) : descend sous ts_min, part de l'intérieur du fichier.
        assert!(window_is_glued(&(0..3_300 * MIN), file, MIN));
        // Sous-fenêtre haute [4 600, 5 000) : part du haut, au-dessus de ts_max + barre.
        assert!(!window_is_glued(&(4_600 * MIN..5_000 * MIN), file, MIN));
        // Fenêtre entière sur un fichier court : la pagination part de 5 000, loin de [100, 200].
        assert!(!window_is_glued(
            &request,
            Some((100 * MIN, 200 * MIN)),
            MIN
        ));
        // Fenêtre entière qui finit juste après le fichier (ts_max = fin − barre) : collée.
        assert!(window_is_glued(
            &request,
            Some((3_000 * MIN, 4_999 * MIN)),
            MIN
        ));
        // Fenêtre entièrement avant le fichier : le lot ne toucherait pas le fichier.
        assert!(!window_is_glued(&(0..1_000 * MIN), file, MIN));
        // Toute sous-fenêtre basse du plan de reprise est collée, toute sous-fenêtre haute ne l'est pas.
        for (ts_min, ts_max) in [(3_000, 4_900), (2_000, 4_999), (1_000, 1_100), (400, 4_000)] {
            let (ts_min, ts_max) = (ts_min * MIN, ts_max * MIN);
            let windows = plan_windows(&request, Some((ts_min, ts_max)), MIN, 300 * MIN);
            if let [low, high] = windows.as_slice() {
                assert!(window_is_glued(low, Some((ts_min, ts_max)), MIN));
                assert!(!window_is_glued(high, Some((ts_min, ts_max)), MIN));
            }
        }
    }

    #[test]
    fn jobs_par_mois_pour_le_1m_et_par_annee_pour_le_4h() {
        let janvier = 1_704_067_200_000;
        let inst: Symbol = "BTC-USDC".parse().unwrap();
        let options = |tf, from, to| {
            OkxIngestOptions::new(
                PathBuf::from("."),
                vec![inst.clone()],
                tf,
                from,
                to,
                to,
                false,
            )
        };
        // 15 janvier au 10 mars 2024 en 1m : trois mois, premier et dernier tronqués.
        let from = janvier + 14 * 86_400_000;
        let to = 1_709_251_200_000 + 9 * 86_400_000;
        let jobs = plan_jobs(&options(Timeframe::M1, from, to)).unwrap();
        let labels: Vec<String> = jobs.iter().map(|j| j.period.label()).collect();
        assert_eq!(labels, ["2024-01", "2024-02", "2024-03"]);
        assert_eq!(jobs[0].request.start, from);
        assert_eq!(jobs[0].request.end, 1_706_745_600_000);
        assert_eq!(jobs[2].request.end, to);
        // En 4h, le store écrit un fichier annuel : un seul job pour la même fenêtre.
        let jobs = plan_jobs(&options(Timeframe::H4, from, to)).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].period.label(), "2024");
        assert_eq!(jobs[0].request, from..to);
    }
}
