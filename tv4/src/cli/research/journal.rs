//! Transaction disque S12 : un verrou permanent protège toute la vie du Journal.
use super::{ResearchError, io, refused, safe_path};
use crate::model::blocks::Block;
#[cfg(test)]
use crate::research::registry::{Admission, AdmissionBatch};
use crate::research::registry::{
    Event, ReadId, Registry, TrialResult, UtcDate, encode_events, header_csv,
};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Timeout court et explicite : le verrou ne doit couvrir aucun calcul ni lecture de bougies.
const LOCK_TIMEOUT: Duration = Duration::from_millis(500);
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Transaction sérialisée. Drop déverrouille explicitement le fichier permanent.
///
/// `append` valide une copie, écrit et synchronise le CSV, publie la copie puis remplace le README.
/// Une écriture CSV en échec empoisonne l'instance ; rouvrir exige de relire le journal strictement.
/// Un échec du README après sync du CSV ne rembourse jamais l'opération déjà durable.
pub(crate) struct Journal {
    root: PathBuf,
    lock: File,
    registry: Registry,
    empty: bool,
    poisoned: bool,
}
impl Journal {
    pub(crate) fn open(root: &Path) -> Result<Self, ResearchError> {
        let lock_path = safe_path(root, "state/locks/research.lock")?;
        let parent = root.join("state/locks");
        fs::create_dir_all(&parent).map_err(|e| io(&parent, e))?;
        let lock = File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| io(&lock_path, e))?;
        let start = Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if start.elapsed() < LOCK_TIMEOUT => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(refused(format!(
                        "verrou de recherche occupé après {} ms : {}",
                        LOCK_TIMEOUT.as_millis(),
                        lock_path.display()
                    )));
                }
                Err(TryLockError::Error(error)) => return Err(io(&lock_path, error)),
            }
        }
        // Construire la garde avant toute lecture : même un refus de CSV déclenche son Drop.
        let mut journal = Self {
            root: root.to_path_buf(),
            lock,
            registry: Registry::default(),
            empty: true,
            poisoned: false,
        };
        let path = safe_path(root, "research/trials.csv")?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(io(&path, error)),
        };
        journal.empty = bytes.is_empty();
        journal.registry = Registry::from_csv(&bytes)?;
        Ok(journal)
    }
    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    pub(crate) fn append(&mut self, events: &[Event]) -> Result<(), ResearchError> {
        if self.poisoned {
            return Err(refused(
                "journal après échec d'écriture : rouvrir et valider le CSV",
            ));
        }
        let next = self.registry.with_events(events)?;
        if events.is_empty() {
            return Ok(());
        }
        let mut bytes = if self.empty { header_csv() } else { Vec::new() };
        bytes.extend(encode_events(events)?);
        let path = safe_path(&self.root, "research/trials.csv")?;
        let parent = self.root.join("research");
        fs::create_dir_all(&parent).map_err(|e| io(&parent, e))?;
        self.poisoned = true;
        let mut file = File::options()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| io(&path, e))?;
        file.write_all(&bytes).map_err(|e| io(&path, e))?;
        file.sync_all().map_err(|e| io(&path, e))?;
        // Rend durable l'entrée de répertoire lorsque trials.csv vient d'être créé.
        File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|e| io(&parent, e))?;
        self.registry = next;
        self.empty = false;
        self.poisoned = false;
        if let Some(event) = events.last() {
            self.refresh_readme(&event.date)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn reserve(&mut self, request: &Admission) -> Result<AdmissionBatch, ResearchError> {
        let batch = self.registry.propose_admission(request)?;
        self.append(&batch.events)?;
        Ok(batch)
    }
    pub(crate) fn finish(
        &mut self,
        read_id: ReadId,
        results: &[TrialResult],
        date: &UtcDate,
    ) -> Result<(), ResearchError> {
        let events = self.registry.propose_results(read_id, results, date)?;
        self.append(&events)
    }

    pub(crate) fn refresh_readme(&self, date: &UtcDate) -> Result<String, ResearchError> {
        if self.poisoned {
            return Err(refused(
                "journal après échec d'écriture : projection refusée",
            ));
        }
        let text = self.summary(date);
        let path = safe_path(&self.root, "research/README.md")?;
        let parent = self.root.join("research");
        fs::create_dir_all(&parent).map_err(|e| io(&parent, e))?;
        let temp = parent.join(format!(
            ".README.md.{}.{}.tmp",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = File::options()
                .create_new(true)
                .write(true)
                .open(&temp)
                .map_err(|e| io(&temp, e))?;
            file.write_all(text.as_bytes()).map_err(|e| io(&temp, e))?;
            file.sync_all().map_err(|e| io(&temp, e))?;
            fs::rename(&temp, &path).map_err(|e| io(&path, e))?;
            File::open(&parent)
                .and_then(|file| file.sync_all())
                .map_err(|e| io(&parent, e))?;
            Ok::<_, ResearchError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        Ok(text)
    }

    fn summary(&self, date: &UtcDate) -> String {
        let reg = &self.registry;
        let dev_reads = reg
            .reads()
            .values()
            .filter(|read| read.block == Some(Block::Dev))
            .count();
        let dev_trials = reg
            .trials()
            .values()
            .filter(|trial| trial.initial_block == Some(Block::Dev))
            .count();
        let active = reg
            .claims()
            .values()
            .filter(|claim| claim.consumed_by.is_none())
            .count();
        let mut text = format!(
            "# Registre de recherche\n\nProjection du journal `trials.csv`, source de vérité append-only.\nActualisé : {} (UTC).\n\nN = {} configurations distinctes ; {} événements historiques.\nEssais DEV : {dev_trials} ; lectures DEV : {dev_reads}.\nClaims actifs : {active} ; consommés : {}.\nLectures sans résultat complet : {}.\nGénération active : {}.\n\n## Lectures et quotas par trimestre UTC\n\nVALIDATION : 3 candidats au plus, une lecture par candidat et par trimestre.\nHOLDOUT : 1 lecture par trimestre, 3 par génération, réservations comprises.\n\n| Trimestre | VALIDATION / 3 | Candidats | HOLDOUT / 1 |\n| --- | ---: | --- | ---: |\n",
            date.as_str(),
            reg.n(),
            reg.events().len(),
            reg.claims().len() - active,
            reg.pending_reads().len(),
            reg.generation()
        );
        let mut quarters = BTreeSet::from([date.quarter()]);
        quarters.extend(reg.reads().values().map(|read| read.quarter()));
        for quarter in quarters {
            let quota = reg.quota(quarter);
            let _ = writeln!(
                text,
                "| {quarter} | {} | {} | {} |",
                quota.validation_reads,
                quota
                    .validation_candidates
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", "),
                quota.holdout_reads
            );
        }
        text.push_str("\n## Historique des générations HOLDOUT\n\n| Génération | Lectures / 3 |\n| --- | ---: |\n");
        for generation in 0..=reg.generation() {
            let count = reg
                .holdout_reads_by_generation()
                .get(&generation)
                .copied()
                .unwrap_or(0);
            let _ = writeln!(text, "| {generation} | {count} |");
        }
        let _ = writeln!(
            text,
            "\nTotal HOLDOUT historique : {}. Rotation après trois réservations par génération.\n",
            reg.holdout_reads_by_generation().values().sum::<usize>()
        );
        text.push_str("## Lectures réservées sans résultat complet\n\n");
        if reg.pending_reads().is_empty() {
            text.push_str("Aucune.\n");
        }
        for read in reg.pending_reads() {
            let _ = writeln!(
                text,
                "- read_id {} : {}, {}, génération {} ; {}/{} profils terminés.",
                read.id.0,
                read.date.as_str(),
                read.block.map_or("historique", Block::as_str),
                read.generation,
                read.completed_hashes.len(),
                read.config_hashes.len()
            );
        }
        text.push_str("\nLes réservations interrompues restent consommées. Aucun compteur historique n'est remis à zéro.\n");
        text
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::blocks::Block;
    use crate::research::registry::{AdmissionTime, ConfigSpec, Digest, HypothesisRef, Metrics};
    fn admission() -> Admission {
        let date = UtcDate::new("2026-01-01T00:00:00Z").expect("date");
        Admission {
            configs: vec![ConfigSpec {
                config_hash: Digest::new("a".repeat(64)).expect("hash"),
                family: "F1".into(),
                hypothesis: Some(
                    HypothesisRef::new("H-001".into(), Digest::new("b".repeat(64)).expect("sha"))
                        .expect("fiche"),
                ),
            }],
            dataset_hash: Digest::new("c".repeat(64)).expect("dataset"),
            block: Some(Block::Dev),
            time: AdmissionTime::new(date.clone(), date.quarter()).expect("time"),
            generation: 0,
            decision_start_ms: None,
            decision_end_ms: None,
        }
    }
    #[test]
    fn reservation_interrompue_reste_inscrite_et_resultat_ajoute_sans_modification_du_prefixe() {
        let dir = tempfile::tempdir().expect("tmp");
        let request = admission();
        let read = {
            let mut journal = Journal::open(dir.path()).expect("open");
            journal.reserve(&request).expect("reserve").read_id
        };
        let prefix = fs::read(dir.path().join("research/trials.csv")).expect("prefix");
        let mut journal = Journal::open(dir.path()).expect("reopen");
        assert_eq!(journal.registry().n(), 1);
        assert_eq!(journal.registry().pending_reads().len(), 1);
        journal
            .finish(
                read,
                &[TrialResult {
                    config_hash: request.configs[0].config_hash.clone(),
                    failed: false,
                    metrics: Metrics {
                        mu_r: Some(-0.2),
                        n: Some(20),
                        pf: Some(0.5),
                    },
                }],
                request.time.date(),
            )
            .expect("finish");
        assert!(
            fs::read(dir.path().join("research/trials.csv"))
                .expect("csv")
                .starts_with(&prefix)
        );
        assert!(journal.registry().pending_reads().is_empty());
    }
    #[test]
    fn append_invalide_ne_modifie_ni_csv_ni_projection() {
        let dir = tempfile::tempdir().expect("tmp");
        let mut journal = Journal::open(dir.path()).expect("open");
        let batch = journal.reserve(&admission()).expect("reserve");
        let csv = fs::read(dir.path().join("research/trials.csv")).expect("csv");
        let readme = fs::read(dir.path().join("research/README.md")).expect("readme");
        assert!(journal.append(&batch.events).is_err());
        assert_eq!(
            csv,
            fs::read(dir.path().join("research/trials.csv")).expect("csv")
        );
        assert_eq!(
            readme,
            fs::read(dir.path().join("research/README.md")).expect("readme")
        );
        assert_eq!(journal.registry().n(), 1);
    }

    // Ce point de synchronisation n’existe que dans le binaire de tests. Il permet
    // un vrai SIGKILL après sync_all et Drop du verrou, sans fenêtre temporelle.
    #[test]
    fn child_reserve_then_wait() {
        let Some(root) = std::env::var_os("TV4_TEST_JOURNAL_KILL_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let mut request = admission();
        {
            let mut journal = Journal::open(&root).expect("open");
            let seed = journal.reserve(&request).expect("seed");
            journal
                .finish(
                    seed.read_id,
                    &[TrialResult {
                        config_hash: request.configs[0].config_hash.clone(),
                        failed: false,
                        metrics: Metrics::default(),
                    }],
                    request.time.date(),
                )
                .expect("seed result");
            let claims = journal
                .registry()
                .propose_claims(
                    &[request.configs[0].config_hash.clone()],
                    request.configs[0].hypothesis.as_ref().expect("fiche"),
                    request.time.date(),
                )
                .expect("claim");
            journal.append(&claims).expect("append claim");
            request.block = Some(Block::Holdout);
            journal.reserve(&request).expect("protected reservation");
        }
        fs::write(root.join("ready"), b"reservation synchronisee").expect("ready");
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn tuer_apres_reservation_conserve_claim_quota_et_lecture_en_attente() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "cli::research::journal::tests::child_reserve_then_wait",
            ])
            .env("TV4_TEST_JOURNAL_KILL_ROOT", root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("child");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.join("ready").exists() && Instant::now() < deadline {
            if child.try_wait().expect("status").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let ready = root.join("ready").exists();
        let _ = child.kill();
        child.wait().expect("wait");
        assert!(
            ready,
            "l’enfant doit avoir synchronisé sa réservation avant SIGKILL"
        );
        let csv = root.join("research/trials.csv");
        let prefix = fs::read(&csv).expect("csv");
        let mut journal = Journal::open(root).expect("lock released after SIGKILL");
        assert_eq!(journal.registry().pending_reads().len(), 1);
        assert_eq!(
            journal
                .registry()
                .claims()
                .values()
                .filter(|c| c.consumed_by.is_some())
                .count(),
            1
        );
        let mut request = admission();
        request.block = Some(Block::Holdout);
        assert!(
            journal.reserve(&request).is_err(),
            "quota et claim ne sont pas remboursés"
        );
        assert_eq!(fs::read(&csv).expect("csv unchanged"), prefix);
        assert!(
            fs::read_to_string(root.join("research/README.md"))
                .expect("README")
                .contains("Lectures sans résultat complet : 1")
        );
    }
}
