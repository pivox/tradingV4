//! Lecture unique des sources, projections et index ; aucune résolution de chemin historique.
use super::{ControlsError, bundle, identity::*, invalid, types::*};
use crate::{
    model::{Block, hash},
    report::verify::{VerifiedRun, VerifyReport},
    research::{
        controls::*,
        stats::{StatsSizing, Window},
    },
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(hex)
}
fn check(condition: bool, reason: &str) -> Result<(), ControlsError> {
    if condition {
        Ok(())
    } else {
        Err(invalid(reason))
    }
}
fn finite(value: Option<f64>) -> bool {
    value.is_none_or(f64::is_finite)
}
fn probability(value: Option<f64>) -> bool {
    value.is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v))
}
impl ControlsContext {
    pub fn validate(&self) -> Result<(), ControlsError> {
        self.options
            .as_engine()
            .validate()
            .map_err(|e| invalid(e.to_string()))?;
        check(
            self.options.modes.windows(2).all(|m| m[0] < m[1]),
            "modes distincts et triés requis",
        )?;
        crate::report::stats_inputs::validate_window(Window {
            start_ms: self.window.start_ms,
            end_ms: self.window.end_ms,
        })
        .map_err(invalid)?;
        check(
            digest(&self.dataset_hash)
                && digest(&self.engine.binary_hash)
                && !self.engine.commit.is_empty()
                && self.initial_cash.is_finite()
                && self.initial_cash > 0.
                && self.sizing == StatsSizing::Research,
            "contexte moteur/dataset/capital/Research invalide",
        )?;
        check(
            self.source_block == self.effective_block
                && matches!(self.effective_block, None | Some(Block::Dev)),
            "S14 exige DEV effectif ou une voie technique sans bloc",
        )?;
        Ok(())
    }
}
fn validate_source(source: &ControlsSource, c: &ControlsContext) -> Result<(), ControlsError> {
    let s = &source.identity;
    check(
        hex(&s.config_hash)
            && hex(&s.run_id)
            && hex(&source.geometry_hash)
            && source.regime_conventions == REGIME_CONVENTIONS,
        "identité ou convention source invalide",
    )?;
    check(
        [
            &s.hashes.metrics,
            &s.hashes.trades,
            &s.hashes.equity,
            &s.hashes.stats_inputs,
            &s.hashes.manifest,
        ]
        .iter()
        .all(|h| digest(h)),
        "les cinq hashes source BLAKE3 sont requis",
    )?;
    check(
        source.cost_seed == c.costs.as_ref().map(|v| v.seed) && !source.quotas.is_empty(),
        "coûts ou quotas source incohérents",
    )?;
    for (symbol, q) in &source.quotas {
        check(!symbol.is_empty(), "symbole de quota vide")?;
        validate_quotas(q).map_err(|e| invalid(e.to_string()))?;
    }
    if let Some(g) = &source.historical_admission {
        validate_admission(g)?;
        check(
            g.config_hash == s.config_hash && g.dataset_hash == c.dataset_hash,
            "admission historique déliée de la source",
        )?;
    }
    Ok(())
}
fn validate_admission(a: &AdmissionRecord) -> Result<(), ControlsError> {
    check(
        a.schema == "tv4-governance-v1"
            && hex(&a.config_hash)
            && digest(&a.dataset_hash)
            && a.trial_id > 0
            && a.read_id > 0
            && a.noise.n > 0,
        "admission invalide",
    )?;
    check(
        a.noise.selection_years.is_finite()
            && a.noise.selection_years > 0.
            && finite(a.noise.expected)
            && finite(a.noise.approx_q90)
            && finite(a.noise.q95)
            && a.noise.n_eff.is_none_or(|n| n > 0 && n <= a.noise.n),
        "repères de bruit invalides",
    )?;
    if let Some(h) = &a.hypothesis {
        check(
            h.identifiant.starts_with("H-")
                && h.identifiant.len() == 5
                && h.identifiant[2..].bytes().all(|b| b.is_ascii_digit())
                && h.identifiant != "H-000"
                && hex(&h.sha256),
            "fiche d'admission invalide",
        )?;
    }
    check(
        a.source_block.is_none() || a.hypothesis.is_some(),
        "une fiche S12 est obligatoire dès qu'un bloc est déclaré",
    )?;
    if !a.effective_block.is_some_and(Block::is_protected) {
        check(
            a.claim_holdout.is_none() && a.frozen_tag.is_none() && a.frozen_at.is_none(),
            "gel/claim réservés aux lectures protégées",
        )?;
    }
    Ok(())
}
fn validate_months(months: &[MonthMomentDto]) -> Result<(), ControlsError> {
    check(
        months
            .iter()
            .all(|m| (1..=12).contains(&m.month) && m.r_sum.is_finite())
            && months
                .windows(2)
                .all(|m| (m[0].year, m[0].month) < (m[1].year, m[1].month)),
        "moments mensuels invalides",
    )
}
fn validate_cell(
    cell: &CellDto,
    source: &ControlsSource,
    c: &ControlsContext,
    mode: ControlMode,
    k: u32,
    run_id: &str,
) -> Result<(), ControlsError> {
    let p = bundle::program(source, c, mode, k)?;
    check(
        cell.replication == k
            && cell.program == p
            && cell.config_hash == p.config_hash().map_err(|e| invalid(e.to_string()))?
            && cell.control_run_id == run_id,
        "programme/cellule délié de son parent ou de sa réplication",
    )?;
    let available = cell.availability == CellAvailability::Complete;
    check(
        finite(cell.score)
            && available == cell.score.is_some()
            && available == cell.reason.is_none(),
        "score, statut et raison incohérents",
    )?;
    check(
        cell.attempts <= c.options.budgets.attempts_per_replication
            && cell.accepted_attempt.is_none_or(|v| v < cell.attempts)
            && (!available || cell.accepted_attempt.is_some()),
        "tentative acceptée ou budget invalide",
    )?;
    check(
        cell.replay_trace_hash
            .as_ref()
            .is_none_or(|v| hex(v) || digest(v))
            && cell.accepted_attempt.is_some() == cell.replay_trace_hash.is_some(),
        "trace native absente ou invalide",
    )?;
    let expected = cell
        .accepted_attempt
        .zip(cell.replay_trace_hash.as_ref())
        .map(|(attempt, trace)| {
            ReplayIdentity {
                control_run_id: run_id.into(),
                program_hash: cell.config_hash.clone(),
                attempt,
                replay_trace_hash: trace.clone(),
            }
            .replay_hash()
            .map_err(|e| invalid(e.to_string()))
        })
        .transpose()?;
    check(expected == cell.replay_hash, "replay_hash incorrect")?;
    validate_months(&cell.months)?;
    check(
        cell.rejections.iter().all(|r| r.attempt < cell.attempts)
            && cell
                .rejections
                .windows(2)
                .all(|r| r[0].attempt < r[1].attempt),
        "rejets de tentatives incohérents",
    )?;
    let mut symbols = BTreeSet::new();
    for q in &cell.quota_checks {
        check(
            symbols.insert(&q.symbol)
                && source.quotas.get(&q.symbol) == Some(&q.expected)
                && q.conformant == (q.expected == q.actual)
                && (!cell.accepted_attempt.is_some() || q.conformant),
            "quotas de cellule non conformes ou dupliqués",
        )?;
        validate_quotas(&q.actual).map_err(|e| invalid(e.to_string()))?;
    }
    check(
        cell.accepted_attempt.is_none() || symbols.len() == source.quotas.len(),
        "quotas d'un rejeu accepté incomplets",
    )
}
fn validate_result(
    result: &SourceResult,
    source: &ControlsSource,
    document: &ControlsDocument,
) -> Result<(), ControlsError> {
    let c = &document.context;
    check(
        finite(result.observed.score)
            && result.observed.score.is_some() == result.observed.reason.is_none(),
        "Observed score/raison incohérents",
    )?;
    validate_months(&result.observed.months)?;
    check(
        result
            .month_coverage
            .iter()
            .all(|m| (1..=12).contains(&m.month))
            && result
                .month_coverage
                .windows(2)
                .all(|m| (m[0].year, m[0].month) < (m[1].year, m[1].month)),
        "couverture mensuelle invalide",
    )?;
    check(
        result
            .modes
            .iter()
            .map(|m| m.mode)
            .eq(c.options.modes.iter().copied()),
        "modes incomplets ou décalés",
    )?;
    for mode in &result.modes {
        let effective = mode.mode.effective(source.has_btc_filter);
        check(
            mode.alias_of == (mode.mode != effective).then_some(effective)
                && mode.cells.len() == document.replications as usize,
            "alias ou K local invalide",
        )?;
        check(
            mode.pair_supports
                .iter()
                .map(|p| &p.symbol)
                .collect::<BTreeSet<_>>()
                == source.quotas.keys().collect::<BTreeSet<_>>()
                && mode.pair_supports.len() == source.quotas.len(),
            "supports incomplets ou dupliqués",
        )?;
        for pair in &mode.pair_supports {
            if let Some(n) = &pair.cardinality {
                check(
                    !n.is_empty()
                        && n.bytes().all(|b| b.is_ascii_digit())
                        && (n == "0" || !n.starts_with('0')),
                    "cardinalité décimale non canonique",
                )?;
                check(
                    n != "0" || mode.planning_reason == Some(UnavailableReason::EmptySupport),
                    "support zéro sans preuve EmptySupport",
                )?;
            } else {
                check(
                    mode.planning_reason.is_some(),
                    "support inconnu sans raison",
                )?;
            }
        }
        for (k, cell) in mode.cells.iter().enumerate() {
            validate_cell(
                cell,
                source,
                c,
                mode.mode,
                k as u32,
                &document.control_run_id,
            )?;
        }
        if let Some(alias) = mode
            .alias_of
            .and_then(|a| result.modes.iter().find(|m| m.mode == a))
        {
            check(
                mode.cells == alias.cells
                    && mode.pair_supports == alias.pair_supports
                    && mode.planning_reason == alias.planning_reason,
                "alias différent de sa réalisation effective",
            )?;
        }
    }
    Ok(())
}
fn validate_stat(
    stat: &ModeStatisticsDto,
    document: &ControlsDocument,
) -> Result<(), ControlsError> {
    let k = document.replications;
    let complete = stat.status == PanelStatus::Complete;
    let local_complete = document.results.iter().all(|r| {
        r.observed.score.is_some()
            && r.modes
                .iter()
                .find(|m| m.mode == stat.mode)
                .is_some_and(|m| {
                    m.cells
                        .iter()
                        .all(|c| c.availability == CellAvailability::Complete)
                })
    });
    check(
        (!complete || local_complete)
            && (document.kind != DocumentKind::Panel || complete == local_complete),
        "statut statistique différent des cellules N×K requises",
    )?;
    check(
        stat.n_grid == document.n_grid
            && stat.replications == k
            && stat.smoke == document.smoke
            && stat.numeric_admissible == (complete && !document.smoke)
            && stat.resolution == 1. / (f64::from(k) + 1.)
            && stat.critical_rank == (95 * (u64::from(k) + 1)).div_ceil(100),
        "N/K/résolution/qualification numérique incohérents",
    )?;
    check(
        stat.sources
            .iter()
            .map(|s| &s.config_hash)
            .eq(document.results.iter().map(|r| &r.config_hash)),
        "statistiques sources décalées",
    )?;
    check(
        stat.maxima.iter().all(|s| s.is_finite())
            && if complete {
                stat.maxima.len() == k as usize
                    && stat.family_score.is_some()
                    && stat.family_p_value.is_some()
                    && stat.family_exceedances.is_some()
            } else {
                stat.maxima.is_empty()
                    && stat.family_p_value.is_none()
                    && stat.family_exceedances.is_none()
                    && stat.critical_value.is_none()
            },
        "maxima ou p-value d'un panneau incomplet",
    )?;
    check(
        finite(stat.critical_value)
            && finite(stat.family_score)
            && probability(stat.family_p_value)
            && stat.family_exceedances.is_none_or(|n| n <= k)
            && stat.critical_value.is_some() == (complete && stat.critical_rank <= u64::from(k)),
        "statistiques de famille invalides",
    )?;
    if complete {
        check(
            stat.family_exceedances
                .zip(stat.family_p_value)
                .is_some_and(|(b, p)| {
                    p >= stat.resolution && p == (1. + f64::from(b)) / (f64::from(k) + 1.)
                }),
            "p-value de famille différente de (1+b)/(K+1)",
        )?;
    }
    if complete && document.kind == DocumentKind::Panel {
        for (column, &maximum) in stat.maxima.iter().enumerate() {
            let actual = document
                .results
                .iter()
                .filter_map(|r| {
                    r.modes
                        .iter()
                        .find(|m| m.mode == stat.mode)
                        .and_then(|m| m.cells[column].score)
                })
                .reduce(f64::max);
            check(
                actual == Some(maximum),
                "maximum publié différent de la colonne du panneau",
            )?;
        }
        check(
            document
                .results
                .iter()
                .filter_map(|r| r.observed.score)
                .reduce(f64::max)
                == stat.family_score,
            "score de famille différent des sources observées",
        )?;
    }
    for (s, r) in stat.sources.iter().zip(&document.results) {
        check(
            s.score == r.observed.score
                && [s.score, s.control_mean, s.delta, s.mc_standard_error]
                    .into_iter()
                    .all(finite)
                && probability(s.p_value)
                && s.exceedances.is_none_or(|n| n <= k)
                && s.ties.is_none_or(|n| n <= k)
                && s.mc_standard_error.is_none_or(|v| v >= 0.),
            "statistique marginale invalide",
        )?;
        check(
            complete
                || (s.p_value.is_none()
                    && s.exceedances.is_none()
                    && s.ties.is_none()
                    && s.above_critical.is_none()),
            "p-value marginale sur panneau incomplet",
        )?;
        if complete {
            check(
                s.ties.is_some()
                    && s.exceedances.zip(s.p_value).is_some_and(|(b, p)| {
                        p >= stat.resolution && p == (1. + f64::from(b)) / (f64::from(k) + 1.)
                    }),
                "p-value marginale ou effectifs différents de (1+b)/(K+1)",
            )?;
        }
        if let Some(t) = &s.temporal {
            check(
                t.approximate
                    && t.samples == 10_000
                    && t.complete_months <= t.months
                    && [t.point, t.standard_error, t.lb90].into_iter().all(finite)
                    && t.standard_error.is_none_or(|v| v >= 0.),
                "bootstrap temporel invalide",
            )?;
            check(
                if t.available {
                    t.reason.is_none()
                        && t.standard_error.is_some()
                        && t.lb90.is_some()
                        && t.lb90_positive.is_some()
                        && t.complete_months >= 2
                } else {
                    t.reason.is_some()
                        && t.standard_error.is_none()
                        && t.lb90.is_none()
                        && t.lb90_positive.is_none()
                },
                "disponibilité temporelle incohérente",
            )?;
        }
    }
    Ok(())
}
impl ControlsDocument {
    pub fn validate(&self) -> Result<(), ControlsError> {
        self.context.validate()?;
        hash::canonical_json(self).map_err(|e| invalid(e.to_string()))?;
        check(
            self.schema == SCHEMA
                && self.conventions == CONVENTIONS
                && hex(&self.control_run_id)
                && hex(&self.catalogue_hash),
            "schéma/conventions/identité numérique invalide",
        )?;
        check(
            (1..=27).contains(&self.n_grid)
                && self.sources.len() == self.n_grid as usize
                && self.replications == self.context.options.replications
                && self.smoke == (self.replications < 200)
                && self.technical_only == self.context.effective_block.is_none(),
            "N/K/voie technique incohérents",
        )?;
        check(
            self.sources
                .windows(2)
                .all(|s| s[0].identity.config_hash < s[1].identity.config_hash)
                && self.canonical_reference == self.sources[0].identity.config_hash,
            "roster trié distinct et référence firsthash requis",
        )?;
        for source in &self.sources {
            validate_source(source, &self.context)?;
        }
        check(
            match self.kind {
                DocumentKind::Panel => {
                    self.panel_hash.is_none() && self.results.len() == self.sources.len()
                }
                DocumentKind::Projection => {
                    self.panel_hash.as_ref().is_some_and(|h| digest(h)) && self.results.len() == 1
                }
            },
            "forme panneau/projection invalide",
        )?;
        check(
            self.results
                .windows(2)
                .all(|r| r[0].config_hash < r[1].config_hash),
            "résultats sources dupliqués/désordonnés",
        )?;
        for result in &self.results {
            let source = self
                .sources
                .iter()
                .find(|s| s.identity.config_hash == result.config_hash)
                .ok_or_else(|| invalid("résultat sans provenance source"))?;
            validate_result(result, source, self)?;
        }
        check(
            self.statistics
                .iter()
                .map(|s| s.mode)
                .eq(self.context.options.modes.iter().copied()),
            "statistiques modes incomplètes/décalées",
        )?;
        for stat in &self.statistics {
            validate_stat(stat, self)?;
        }
        check(
            (self.status == PanelStatus::Complete)
                == self
                    .statistics
                    .iter()
                    .all(|s| s.status == PanelStatus::Complete),
            "statut global incohérent",
        )?;
        let id = bundle::run_identity(self, bundle::program_hashes(&self.sources, &self.context)?)
            .control_run_id()
            .map_err(|e| invalid(e.to_string()))?;
        check(id == self.control_run_id, "control_run_id incorrect")
    }
}
pub(super) fn validate_manifest(
    m: &ControlsManifest,
    d: &ControlsDocument,
    numeric: &[u8],
) -> Result<(), ControlsError> {
    check(
        m.schema == MANIFEST_SCHEMA
            && m.kind == d.kind
            && m.control_run_id == d.control_run_id
            && m.numeric.file == CONTROLS_FILE
            && m.numeric.hash == hash::blake3_prefixed(numeric)
            && m.sources
                == d.sources
                    .iter()
                    .map(|s| s.identity.clone())
                    .collect::<Vec<_>>()
            && m.panel_hash == d.panel_hash,
        "manifeste numérique/provenance délié",
    )?;
    check(
        match d.kind {
            DocumentKind::Panel => m.projection.is_none(),
            DocumentKind::Projection => {
                m.projection.as_ref() == d.results.first().map(|r| &r.config_hash)
            }
        },
        "identité projection du manifeste incorrecte",
    )?;
    check(
        m.n_registre > 0
            && m.role_counts
                .candidates
                .checked_add(m.role_counts.null_controls)
                == Some(m.n_registre)
            && m.read_id > 0,
        "N courant/comptes par rôle invalides",
    )?;
    let programs = bundle::program_hashes(&d.sources, &d.context)?;
    let expected = programs
        .iter()
        .chain(d.sources.iter().map(|s| &s.identity.config_hash))
        .collect::<BTreeSet<_>>();
    check(
        m.admissions
            .iter()
            .map(|a| &a.config_hash)
            .eq(expected.iter().copied())
            && m.role_counts.candidates >= d.n_grid as u64
            && m.role_counts.null_controls >= programs.len() as u64,
        "admission parents/nulls non bijective ou comptes insuffisants",
    )?;
    let mut trials = BTreeSet::new();
    for a in &m.admissions {
        validate_admission(a)?;
        check(
            a.read_id == m.read_id
                && a.dataset_hash == d.context.dataset_hash
                && a.noise.n == m.n_registre
                && a.source_block == d.context.source_block
                && a.effective_block == d.context.effective_block
                && trials.insert(a.trial_id),
            "admission courante incohérente ou trial dupliqué",
        )?;
    }
    Ok(())
}
fn read(dir: &Path, name: &str) -> Result<Vec<u8>, ControlsError> {
    let path = dir.join(name);
    let meta =
        fs::symlink_metadata(&path).map_err(|e| invalid(format!("{} : {e}", path.display())))?;
    check(
        meta.is_file() && !meta.file_type().is_symlink(),
        "fichier de contrôle régulier requis",
    )?;
    fs::read(&path).map_err(|e| invalid(format!("{} : {e}", path.display())))
}
/// La présence d'un lien pendant reste une présence : son contenu sera refusé, jamais ignoré.
pub(crate) fn present(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}
/// Garde S14 avant la première lecture source ; les anciens runs sans contrôles gardent leur lecteur.
pub(crate) fn check_leaf_paths(dir: &Path) -> Result<(), ControlsError> {
    if !present(&dir.join(CONTROLS_FILE)) && !present(&dir.join(MANIFEST_FILE)) {
        return Ok(());
    }
    for name in [
        CONTROLS_FILE,
        MANIFEST_FILE,
        "manifest.json",
        "metrics.json",
        "trades.csv",
        "equity.csv",
        "stats-inputs.json",
    ] {
        let path = dir.join(name);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| invalid(format!("{} : {e}", path.display())))?;
        check(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "copies S14 régulières requises, aucun lien symbolique",
        )?;
    }
    Ok(())
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ControlsError> {
    serde_json::from_slice(bytes).map_err(|e| invalid(format!("JSON strict : {e}")))
}
fn linked(reference: &FileRef, name: &str, bytes: &[u8]) -> Result<(), ControlsError> {
    check(
        reference.file == name && reference.hash == hash::blake3_prefixed(bytes),
        "référence de fichier fixe/hash incorrect",
    )
}
#[derive(Debug)]
pub struct LoadedProjection {
    pub document: ControlsDocument,
    pub manifest: ControlsManifest,
    pub files: Vec<(String, Vec<u8>)>,
}
/// La feuille ne suit aucun lien vers le panneau : les autres sources ne sont que des empreintes.
pub fn load_projection(
    dir: &Path,
    run: &VerifiedRun,
) -> Result<Option<LoadedProjection>, ControlsError> {
    if !present(&dir.join(CONTROLS_FILE)) && !present(&dir.join(MANIFEST_FILE)) {
        return Ok(None);
    }
    let document_bytes = read(dir, CONTROLS_FILE)?;
    let document: ControlsDocument = parse(&document_bytes)?;
    document.validate()?;
    check(
        document.kind == DocumentKind::Projection,
        "une feuille exige une projection",
    )?;
    let manifest_bytes = read(dir, MANIFEST_FILE)?;
    let manifest: ControlsManifest = parse(&manifest_bytes)?;
    validate_manifest(&manifest, &document, &document_bytes)?;
    let identity = bundle::identity_source(run)?;
    check(
        document.results[0].config_hash == identity.config_hash
            && document.sources.iter().any(|s| s.identity == identity),
        "projection échangée ou source altérée",
    )?;
    let source = document
        .sources
        .iter()
        .find(|s| s.identity == identity)
        .ok_or_else(|| invalid("provenance locale absente"))?;
    bundle::validate_run_source(run, source, &document.context)?;
    Ok(Some(LoadedProjection {
        document,
        manifest,
        files: vec![
            (CONTROLS_FILE.into(), document_bytes),
            (MANIFEST_FILE.into(), manifest_bytes),
        ],
    }))
}
pub struct LoadedControlsBundle {
    pub document: ControlsDocument,
    pub manifest: ControlsManifest,
    pub index: ControlsIndex,
    pub leaves: Vec<(PathBuf, VerifiedRun)>,
    pub layout_files: Vec<(PathBuf, Vec<u8>)>,
}
pub fn load_bundle(dir: &Path) -> Result<LoadedControlsBundle, ControlsError> {
    let index_bytes = read(dir, INDEX_FILE)?;
    let index: ControlsIndex = parse(&index_bytes)?;
    check(
        index.schema == INDEX_SCHEMA && index.complete,
        "index S14 complet requis",
    )?;
    let panel_bytes = read(dir, CONTROLS_FILE)?;
    linked(&index.panel, CONTROLS_FILE, &panel_bytes)?;
    let document: ControlsDocument = parse(&panel_bytes)?;
    document.validate()?;
    check(
        document.kind == DocumentKind::Panel,
        "panneau racine requis",
    )?;
    let manifest_bytes = read(dir, MANIFEST_FILE)?;
    linked(&index.manifest, MANIFEST_FILE, &manifest_bytes)?;
    let manifest: ControlsManifest = parse(&manifest_bytes)?;
    validate_manifest(&manifest, &document, &panel_bytes)?;
    check(
        index.control_run_id == document.control_run_id
            && index.canonical_reference == document.canonical_reference
            && index.sources.len() == document.sources.len(),
        "index/panneau déliés",
    )?;
    let mut leaves = Vec::new();
    let mut layout_files = vec![
        (dir.join(INDEX_FILE), index_bytes),
        (dir.join(CONTROLS_FILE), panel_bytes),
        (dir.join(MANIFEST_FILE), manifest_bytes),
    ];
    for (entry, source) in index.sources.iter().zip(&document.sources) {
        check(
            entry.config_hash == source.identity.config_hash
                && entry.dir == format!("runs/{}", source.identity.config_hash),
            "chemin fixe runs/<hash> requis ; doublon ou échange",
        )?;
        let path = dir.join(&entry.dir);
        for parent in [dir.join("runs"), path.clone()] {
            let meta = fs::symlink_metadata(&parent).map_err(|e| invalid(e.to_string()))?;
            check(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "dossier de copie régulier requis",
            )?;
        }
        let run =
            crate::report::verify::load_verified_run(&path).map_err(|e| invalid(e.to_string()))?;
        // load_verified_run possède et valide déjà les deux sidecars de cette feuille.
        let local = run
            .controls()
            .ok_or_else(|| invalid("projection de feuille absente"))?;
        let local_bytes = &local.files[0].1;
        let local_manifest_bytes = &local.files[1].1;
        linked(&entry.controls, CONTROLS_FILE, local_bytes)?;
        linked(&entry.manifest, MANIFEST_FILE, local_manifest_bytes)?;
        check(
            bundle::identity_source(&run)? == source.identity,
            "source copiée différente du panneau",
        )?;
        check(
            local.document == bundle::projection(&document, &entry.config_hash, &index.panel.hash),
            "projection non bijective au panneau",
        )?;
        check(
            local.manifest.admissions == manifest.admissions
                && local.manifest.role_counts == manifest.role_counts
                && local.manifest.read_id == manifest.read_id,
            "admissions du bundle incohérentes",
        )?;
        layout_files.extend(
            local
                .files
                .iter()
                .map(|(name, b)| (path.join(name), b.clone())),
        );
        leaves.push((path, run));
    }
    Ok(LoadedControlsBundle {
        document,
        manifest,
        index,
        leaves,
        layout_files,
    })
}
/// Seuls les fichiers numériques et les cinq sources historiques décident du verdict.
pub fn compare_bundles(a: &Path, b: &Path) -> Result<VerifyReport, ControlsError> {
    let a = load_bundle(a)?;
    let b = load_bundle(b)?;
    let mut different_files = usize::from(a.layout_files[1].1 != b.layout_files[1].1);
    let mut lines = vec![if different_files == 0 {
        "controls.json : identique".into()
    } else {
        "controls.json : DIFFÉRENT".into()
    }];
    if a.manifest != b.manifest {
        lines.push("admissions actuelles : différentes (information)".into());
    }
    if a.leaves.len() != b.leaves.len() {
        different_files += 1;
    } else {
        for ((_, run_a), (_, run_b)) in a.leaves.iter().zip(&b.leaves) {
            for (name, aa, bb) in [
                (
                    "manifest.json",
                    run_a.manifest_bytes(),
                    run_b.manifest_bytes(),
                ),
                ("metrics.json", run_a.metrics_bytes(), run_b.metrics_bytes()),
                ("trades.csv", run_a.trades_bytes(), run_b.trades_bytes()),
                ("equity.csv", run_a.equity_bytes(), run_b.equity_bytes()),
                (
                    "stats-inputs.json",
                    run_a.stats_inputs().map_or(&[][..], |p| p.bytes.as_slice()),
                    run_b.stats_inputs().map_or(&[][..], |p| p.bytes.as_slice()),
                ),
            ] {
                if aa != bb {
                    different_files += 1;
                    lines.push(format!("source {name} : DIFFÉRENT"));
                }
            }
        }
    }
    lines.push(if different_files == 0 {
        "verdict : identiques à l'octet".into()
    } else {
        format!("verdict : DIFFÉRENTS ({different_files} fichier(s))")
    });
    Ok(VerifyReport {
        lines,
        different_files,
    })
}
