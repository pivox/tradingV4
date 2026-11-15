//! Construction sans I/O puis publication exclusive, index installé en dernier.
use super::{ControlsError, identity::*, invalid, types::*};
use crate::report::governance::GovernanceContext;
use crate::{
    engine::controls::BatchOutcome,
    model::hash,
    report::{self, stats, verify::VerifiedRun},
    research::{controls::*, stats::Window},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Path, PathBuf},
};

pub struct BundleFile {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
}
pub struct ControlsBundleBytes {
    files: Vec<BundleFile>,
    index: Vec<u8>,
}
impl ControlsBundleBytes {
    /// Mesures de l'appelant, sérialisées avant mutation et jamais référencées par les identités.
    pub fn with_measurements<T: serde::Serialize>(
        mut self,
        value: &T,
    ) -> Result<Self, ControlsError> {
        let path = PathBuf::from("controls-measurements.json");
        if self.files.iter().any(|file| file.relative_path == path) {
            return Err(invalid("mesures déjà présentes"));
        }
        let bytes = bytes(value)?;
        self.files.push(BundleFile {
            relative_path: path,
            bytes,
        });
        Ok(self)
    }
    pub fn files(&self) -> &[BundleFile] {
        &self.files
    }
    pub fn index_bytes(&self) -> &[u8] {
        &self.index
    }
}
pub(super) fn bytes<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ControlsError> {
    report::json_bytes(value).map_err(|e| invalid(e.to_string()))
}
pub(super) fn identity_source(run: &VerifiedRun) -> Result<SourceIdentity, ControlsError> {
    let s = stats::snapshot(run).map_err(invalid)?;
    let packet = run.stats_inputs().ok_or_else(|| {
        invalid("les cinq sources vérifiées, dont stats-inputs.json, sont requises")
    })?;
    Ok(SourceIdentity {
        config_hash: s.provenance.config_hash,
        run_id: s.provenance.run_id,
        hashes: SourceHashes {
            metrics: hash::blake3_prefixed(run.metrics_bytes()),
            trades: hash::blake3_prefixed(run.trades_bytes()),
            equity: hash::blake3_prefixed(run.equity_bytes()),
            stats_inputs: hash::blake3_prefixed(&packet.bytes),
            manifest: hash::blake3_prefixed(run.manifest_bytes()),
        },
    })
}
fn costs_source(run: &VerifiedRun) -> Result<Option<CostIdentity>, ControlsError> {
    let Some(value) = run.metrics().get("costs").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    Ok(Some(CostIdentity {
        config_hash: value
            .get("costs_file_hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("hash de coûts source absent"))?
            .into(),
        seed: value
            .get("seed")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| invalid("seed de coûts source absent"))?,
    }))
}
fn historical(run: &VerifiedRun) -> Result<Option<AdmissionRecord>, ControlsError> {
    run.manifest()
        .get("governance")
        .filter(|g| !g.is_null())
        .map(|g| {
            serde_json::from_value::<GovernanceContext>(g.clone())
                .map(|g| AdmissionRecord::from(&g))
                .map_err(|e| invalid(e.to_string()))
        })
        .transpose()
}
/// Recoupement commun au producteur et aux lecteurs de feuilles, uniquement sur les octets possédés.
pub(super) fn validate_run_source(
    run: &VerifiedRun,
    source: &ControlsSource,
    c: &ControlsContext,
) -> Result<(), ControlsError> {
    let image = stats::snapshot(run).map_err(invalid)?;
    let profile = c.profile.map(|p| match p {
        ControlProfile::Baseline => "baseline",
        ControlProfile::Adverse => "adverse",
        ControlProfile::Maker => "maker",
    });
    let line = c.line.map(|l| match l {
        ControlLine::Full => "full",
        ControlLine::OkxUsdc => "okx_usdc",
    });
    let admission = historical(run)?;
    let block = run
        .metrics()
        .pointer("/research/block")
        .filter(|b| !b.is_null())
        .map(|b| {
            serde_json::from_value::<crate::model::Block>(b.clone())
                .map_err(|e| invalid(e.to_string()))
        })
        .transpose()?;
    let effective = admission.as_ref().map_or(block, |g| g.effective_block);
    let original = admission.as_ref().map_or(block, |g| g.source_block);
    if identity_source(run)? != source.identity
        || image.provenance.dataset_hash != c.dataset_hash
        || image.provenance.initial_cash.to_bits() != c.initial_cash.to_bits()
        || image.provenance.window.start_ms != c.window.start_ms
        || image.provenance.window.end_ms != c.window.end_ms
        || image.sizing != c.sizing
        || image.provenance.profile.as_deref() != profile
        || image.provenance.line.as_deref() != line
        || costs_source(run)? != c.costs
        || source.cost_seed != costs_source(run)?.map(|cost| cost.seed)
        || source.historical_admission != admission
        || source.quotas != source_quotas(run)?
        || effective != c.effective_block
        || original != c.source_block
        || block.is_some_and(|b| Some(b) != effective)
        || run
            .manifest()
            .pointer("/engine/git_sha")
            .and_then(serde_json::Value::as_str)
            != Some(c.engine.commit.as_str())
        || run
            .manifest()
            .pointer("/engine/binary_hash")
            .and_then(serde_json::Value::as_str)
            != Some(c.engine.binary_hash.as_str())
    {
        return Err(invalid(
            "contexte moteur/dataset/fenêtre/capital/coûts/ligne/bloc différent des octets source",
        ));
    }
    Ok(())
}
fn year(at: i64) -> Result<i32, ControlsError> {
    i32::try_from(crate::store::time::civil_from_days(at.div_euclid(86_400_000)).0)
        .map_err(|_| invalid("année hors borne"))
}
/// Quotas des fills réels, y compris années nulles et lots ensuite purgés.
pub fn source_quotas(
    run: &VerifiedRun,
) -> Result<BTreeMap<String, Vec<AnnualQuota>>, ControlsError> {
    let s = stats::snapshot(run).map_err(invalid)?;
    let first = year(s.provenance.window.start_ms)?;
    let last = year(s.provenance.window.end_ms - 1)?;
    let mut out: BTreeMap<_, BTreeMap<_, _>> = s
        .pair_symbols
        .into_iter()
        .map(|symbol| {
            (
                symbol,
                (first..=last).map(|y| (y, (0_u32, 0_u32))).collect(),
            )
        })
        .collect();
    let mut lots = BTreeSet::new();
    for t in s.trades {
        if !lots.insert((t.symbol.clone(), t.lot_id)) {
            return Err(invalid("sortie partielle ou lot source dupliqué"));
        }
        let counts = out
            .get_mut(&t.symbol)
            .and_then(|q| q.get_mut(&year(t.entry_ms).ok()?))
            .ok_or_else(|| invalid("fill source hors fenêtre ou univers"))?;
        counts.0 = counts
            .0
            .checked_add(1)
            .ok_or_else(|| invalid("quota rempli débordé"))?;
        if t.included {
            counts.1 = counts
                .1
                .checked_add(1)
                .ok_or_else(|| invalid("quota conservé débordé"))?;
        }
    }
    out.into_iter()
        .map(|(symbol, years)| {
            let q = years
                .into_iter()
                .map(|(year, (filled, retained))| AnnualQuota {
                    year,
                    filled,
                    retained,
                })
                .collect::<Vec<_>>();
            validate_quotas(&q).map_err(|e| invalid(e.to_string()))?;
            Ok((symbol, q))
        })
        .collect()
}
pub(super) fn program(
    source: &ControlsSource,
    c: &ControlsContext,
    mode: ControlMode,
    k: u32,
) -> Result<NullProgramIdentity, ControlsError> {
    NullProgramIdentity::new(
        source.identity.config_hash.clone(),
        source.geometry_hash.clone(),
        mode,
        source.has_btc_filter,
        c.options.seed,
        k,
        ProgramBudgets::from(&c.options.budgets),
    )
    .map_err(|e| invalid(e.to_string()))
}
pub(super) fn program_hashes(
    sources: &[ControlsSource],
    c: &ControlsContext,
) -> Result<Vec<String>, ControlsError> {
    let mut hashes = BTreeSet::new();
    for source in sources {
        for mode in c.options.as_engine().effective_modes(source.has_btc_filter) {
            for k in 0..c.options.replications {
                hashes.insert(
                    program(source, c, mode, k)?
                        .config_hash()
                        .map_err(|e| invalid(e.to_string()))?,
                );
            }
        }
    }
    Ok(hashes.into_iter().collect())
}
pub(super) fn run_identity(document: &ControlsDocument, hashes: Vec<String>) -> ControlRunIdentity {
    let c = &document.context;
    ControlRunIdentity {
        program_hashes: hashes,
        sources: document
            .sources
            .iter()
            .map(|s| s.identity.clone())
            .collect(),
        catalogue_hash: document.catalogue_hash.clone(),
        quotas: document
            .sources
            .iter()
            .map(|s| (s.identity.config_hash.clone(), s.quotas.clone()))
            .collect(),
        dataset_hash: c.dataset_hash.clone(),
        window: Window {
            start_ms: c.window.start_ms,
            end_ms: c.window.end_ms,
        },
        initial_cash: c.initial_cash,
        sizing: c.sizing,
        profile: c.profile,
        line: c.line,
        costs: c.costs.clone(),
        engine: c.engine.clone(),
        total_engine_steps: c.options.budgets.total_engine_steps,
    }
}
fn current_manifest(
    document: &ControlsDocument,
    numeric: &[u8],
    admission: &CurrentAdmission,
    projection: Option<String>,
) -> Result<ControlsManifest, ControlsError> {
    let admissions: Vec<_> = admission
        .contexts
        .values()
        .map(AdmissionRecord::from)
        .collect();
    let first = admissions
        .first()
        .ok_or_else(|| invalid("admission actuelle absente"))?;
    let manifest = ControlsManifest {
        schema: MANIFEST_SCHEMA.into(),
        kind: document.kind.clone(),
        control_run_id: document.control_run_id.clone(),
        numeric: FileRef {
            file: CONTROLS_FILE.into(),
            hash: hash::blake3_prefixed(numeric),
        },
        sources: document
            .sources
            .iter()
            .map(|s| s.identity.clone())
            .collect(),
        read_id: first.read_id,
        n_registre: admission
            .candidates
            .checked_add(admission.null_controls)
            .ok_or_else(|| invalid("N registre débordé"))?,
        role_counts: RoleCountsDto {
            candidates: admission.candidates,
            null_controls: admission.null_controls,
        },
        admissions,
        panel_hash: document.panel_hash.clone(),
        projection,
    };
    super::verify::validate_manifest(&manifest, document, numeric)?;
    Ok(manifest)
}
fn cell(
    source: &ControlsSource,
    c: &ControlsContext,
    mode: ControlMode,
    k: u32,
    run_id: &str,
    cell: Option<&CellSummary>,
    reason: Option<UnavailableReason>,
) -> Result<CellDto, ControlsError> {
    let p = program(source, c, mode, k)?;
    let config_hash = p.config_hash().map_err(|e| invalid(e.to_string()))?;
    let trace = cell
        .filter(|v| !v.replay_trace_hash.is_empty())
        .map(|v| v.replay_trace_hash.clone());
    let replay_hash = cell
        .and_then(|v| v.accepted_attempt)
        .zip(trace.as_ref())
        .map(|(attempt, trace)| {
            ReplayIdentity {
                control_run_id: run_id.into(),
                program_hash: config_hash.clone(),
                attempt,
                replay_trace_hash: trace.clone(),
            }
            .replay_hash()
            .map_err(|e| invalid(e.to_string()))
        })
        .transpose()?;
    Ok(CellDto {
        replication: k,
        program: p,
        config_hash,
        control_run_id: run_id.into(),
        replay_hash,
        accepted_attempt: cell.and_then(|v| v.accepted_attempt),
        attempts: cell.map_or(0, |v| v.attempts),
        availability: cell.map_or(CellAvailability::Unavailable, |v| v.availability),
        reason: cell.map_or(reason, |v| v.reason),
        score: cell.and_then(|v| v.score),
        months: cell.map_or_else(Vec::new, |v| v.months.iter().map(Into::into).collect()),
        quota_checks: cell.map_or_else(Vec::new, |v| {
            v.quota_checks.iter().map(Into::into).collect()
        }),
        quantity_changed: cell.is_some_and(|v| v.quantity_changed),
        replay_trace_hash: trace,
        rejections: cell.map_or_else(Vec::new, |v| v.rejections.iter().map(Into::into).collect()),
    })
}
pub(super) fn projection(
    panel: &ControlsDocument,
    source: &str,
    panel_hash: &str,
) -> ControlsDocument {
    let mut value = panel.clone();
    value.kind = DocumentKind::Projection;
    value.panel_hash = Some(panel_hash.into());
    value.results.retain(|r| r.config_hash == source);
    for stat in &mut value.statistics {
        stat.sources.retain(|s| s.config_hash == source);
    }
    value
}

#[derive(serde::Serialize)]
struct CatalogueIntervalIdentity {
    id: u32,
    decision: u64,
    signal_ms: i64,
    entry_ms: i64,
    release_ms: i64,
    next_decision: u64,
    fill_year: i32,
    retained: bool,
    clocks_hash: String,
}
impl TryFrom<&Interval> for CatalogueIntervalIdentity {
    type Error = ControlsError;
    fn try_from(i: &Interval) -> Result<Self, Self::Error> {
        Ok(Self {
            id: i.id,
            decision: u64::try_from(i.decision)
                .map_err(|_| invalid("index de décision hors u64"))?,
            signal_ms: i.signal_ms,
            entry_ms: i.entry_ms,
            release_ms: i.release_ms,
            next_decision: u64::try_from(i.next_decision)
                .map_err(|_| invalid("index de continuation hors u64"))?,
            fill_year: i.fill_year,
            retained: i.retained,
            clocks_hash: i.clocks_hash.clone(),
        })
    }
}
#[derive(serde::Serialize)]
struct CataloguePairIdentity {
    symbol: String,
    intervals: Vec<CatalogueIntervalIdentity>,
}
#[derive(serde::Serialize)]
struct CatalogueModeIdentity {
    mode: ControlMode,
    alias_of: Option<ControlMode>,
    catalogues: Vec<CataloguePairIdentity>,
}
#[derive(serde::Serialize)]
struct CatalogueSourceIdentity {
    source: SourceIdentity,
    available: bool,
    modes: Option<Vec<CatalogueModeIdentity>>,
    unavailable_reasons: Vec<UnavailableReason>,
}
fn catalogue_identity(
    source: &ControlsSource,
    summary: Option<&SourceSummary>,
    reasons: &[UnavailableReason],
) -> Result<CatalogueSourceIdentity, ControlsError> {
    let modes = summary
        .map(|summary| {
            summary
                .modes
                .iter()
                .map(|mode| {
                    let catalogues = mode
                        .catalogues
                        .iter()
                        .map(|pair| {
                            Ok(CataloguePairIdentity {
                                symbol: pair.symbol.clone(),
                                intervals: pair
                                    .intervals
                                    .iter()
                                    .map(CatalogueIntervalIdentity::try_from)
                                    .collect::<Result<_, _>>()?,
                            })
                        })
                        .collect::<Result<_, ControlsError>>()?;
                    Ok(CatalogueModeIdentity {
                        mode: mode.mode,
                        alias_of: mode.alias_of,
                        catalogues,
                    })
                })
                .collect::<Result<Vec<_>, ControlsError>>()
        })
        .transpose()?;
    Ok(CatalogueSourceIdentity {
        source: source.identity.clone(),
        available: summary.is_some(),
        modes,
        unavailable_reasons: if summary.is_some() {
            Vec::new()
        } else {
            reasons.to_vec()
        },
    })
}
/// Les sources/metadata/outcome suivent le même ordre runtime ; le document est trié conjointement.
pub fn build_bundle(
    c: &ControlsContext,
    metadata: &[SourceMetadata],
    runs: &[VerifiedRun],
    outcome: &BatchOutcome,
    admission: &CurrentAdmission,
) -> Result<ControlsBundleBytes, ControlsError> {
    c.validate()?;
    if runs.len() != metadata.len()
        || runs.len() != outcome.requested_sources as usize
        || !(1..=27).contains(&runs.len())
        || outcome.requested_replications != c.options.replications
        || (!outcome.sources.is_empty() && outcome.sources.len() != runs.len())
    {
        return Err(invalid("cardinalités sources/moteur/options différentes"));
    }
    let mut ordered = Vec::new();
    for (i, (run, m)) in runs.iter().zip(metadata).enumerate() {
        let identity = identity_source(run)?;
        if identity.config_hash != m.config_hash {
            return Err(invalid("metadata déliée du parent source"));
        }
        let historical_admission = historical(run)?;
        let quotas = source_quotas(run)?;
        if outcome.sources.get(i).is_some_and(|s| s.quotas != quotas) {
            return Err(invalid("quotas moteur différents du journal source"));
        }
        let source = ControlsSource {
            identity,
            geometry_hash: m.geometry_hash.clone(),
            has_btc_filter: m.has_btc_filter,
            cost_seed: costs_source(run)?.map(|v| v.seed),
            regime_conventions: REGIME_CONVENTIONS.into(),
            quotas,
            historical_admission,
        };
        validate_run_source(run, &source, c)?;
        ordered.push((i, run, source));
    }
    ordered.sort_by(|a, b| a.2.identity.config_hash.cmp(&b.2.identity.config_hash));
    if ordered
        .windows(2)
        .any(|v| v[0].2.identity.config_hash == v[1].2.identity.config_hash)
    {
        return Err(invalid("configuration parent dupliquée"));
    }
    // Un échec avant les catalogues n'est jamais encodé comme un catalogue/support vide.
    let catalogue = ordered
        .iter()
        .map(|(i, _, source)| catalogue_identity(source, outcome.sources.get(*i), &outcome.reasons))
        .collect::<Result<Vec<_>, _>>()?;
    let catalogue_hash = hash::hash_json(&("tv4-controls-catalogue-v1", catalogue))
        .map_err(|e| invalid(e.to_string()))?;
    let sources = ordered
        .iter()
        .map(|(_, _, s)| s.clone())
        .collect::<Vec<_>>();
    let canonical_reference = sources[0].identity.config_hash.clone();
    let mut document = ControlsDocument {
        schema: SCHEMA.into(),
        kind: DocumentKind::Panel,
        conventions: CONVENTIONS.into(),
        control_run_id: String::new(),
        panel_hash: None,
        canonical_reference,
        context: c.clone(),
        sources,
        status: outcome.status,
        reasons: outcome.reasons.clone(),
        n_grid: outcome.requested_sources,
        replications: c.options.replications,
        smoke: c.options.replications < 200,
        technical_only: c.effective_block.is_none(),
        catalogue_hash,
        results: Vec::new(),
        statistics: Vec::new(),
    };
    let programs = program_hashes(&document.sources, c)?;
    document.control_run_id = run_identity(&document, programs)
        .control_run_id()
        .map_err(|e| invalid(e.to_string()))?;
    for (index, _, source) in &ordered {
        let summary = outcome.sources.get(*index);
        let reason = outcome.reasons.first().copied();
        let mut modes = Vec::new();
        for &mode in &c.options.modes {
            let value = summary.and_then(|s| s.modes.iter().find(|m| m.mode == mode));
            if summary.is_some() && value.is_none() {
                return Err(invalid("mode moteur absent"));
            }
            if value.is_some_and(|v| v.cells.len() != c.options.replications as usize) {
                return Err(invalid("matrice moteur sans K cellules"));
            }
            let alias_of = (mode.effective(source.has_btc_filter) != mode)
                .then_some(mode.effective(source.has_btc_filter));
            modes.push(ModeResult {
                mode,
                alias_of,
                pair_supports: value.map_or_else(
                    || {
                        source
                            .quotas
                            .keys()
                            .map(|symbol| PairSupportDto {
                                symbol: symbol.clone(),
                                cardinality: None,
                            })
                            .collect()
                    },
                    |v| v.pair_supports.iter().map(Into::into).collect(),
                ),
                planning_reason: value.map_or(reason, |v| v.planning_reason),
                cells: (0..c.options.replications)
                    .map(|k| {
                        cell(
                            source,
                            c,
                            mode,
                            k,
                            &document.control_run_id,
                            value.and_then(|v| v.cells.get(k as usize)),
                            reason,
                        )
                    })
                    .collect::<Result<_, _>>()?,
            });
        }
        document.results.push(SourceResult {
            config_hash: source.identity.config_hash.clone(),
            observed: ObservedDto {
                score: summary.and_then(|s| s.observed_score),
                reason: summary.map_or(reason, |s| s.observed_reason),
                months: summary.map_or_else(Vec::new, |s| {
                    s.observed_months.iter().map(Into::into).collect()
                }),
            },
            month_coverage: summary.map_or_else(Vec::new, |s| {
                s.month_coverage.iter().map(Into::into).collect()
            }),
            modes,
        });
    }
    for &mode in &c.options.modes {
        let p = outcome.statistics.iter().find(|s| s.mode == mode);
        if p.is_some_and(|s| s.panel.sources.len() != runs.len()) {
            return Err(invalid("statistiques moteur décalées"));
        }
        let sources = ordered
            .iter()
            .map(|(i, _, source)| {
                if let Some(p) = p {
                    SourceStatisticsDto::from_source(
                        source.identity.config_hash.clone(),
                        &p.panel.sources[*i],
                    )
                } else {
                    SourceStatisticsDto {
                        config_hash: source.identity.config_hash.clone(),
                        score: None,
                        control_mean: None,
                        delta: None,
                        mc_standard_error: None,
                        constant_controls: false,
                        exceedances: None,
                        ties: None,
                        p_value: None,
                        delta_at_least_010: None,
                        above_critical: None,
                        temporal: None,
                    }
                }
            })
            .collect();
        let k = document.replications;
        document.statistics.push(ModeStatisticsDto {
            mode,
            alias_of: p.and_then(|v| v.alias_of),
            status: p.map_or(PanelStatus::Inconclusive, |v| v.panel.status),
            n_grid: document.n_grid,
            replications: k,
            smoke: document.smoke,
            numeric_admissible: p.is_some_and(|v| v.panel.numeric_admissible),
            resolution: 1. / (f64::from(k) + 1.),
            critical_rank: (95 * (u64::from(k) + 1)).div_ceil(100),
            critical_value: p.and_then(|v| v.panel.critical_value),
            maxima: p.map_or_else(Vec::new, |v| v.panel.maxima.clone()),
            family_score: p.and_then(|v| v.panel.family_score),
            family_exceedances: p.and_then(|v| v.panel.family_exceedances),
            family_p_value: p.and_then(|v| v.panel.family_p_value),
            sources,
        });
    }
    document.validate()?;
    let panel_bytes = bytes(&document)?;
    let panel_hash = hash::blake3_prefixed(&panel_bytes);
    let manifest = current_manifest(&document, &panel_bytes, admission, None)?;
    let manifest_bytes = bytes(&manifest)?;
    let mut files = Vec::new();
    let mut manifests = Vec::new();
    let mut entries = Vec::new();
    files.push(BundleFile {
        relative_path: CONTROLS_FILE.into(),
        bytes: panel_bytes,
    });
    for (_, run, source) in ordered {
        let dir = format!("runs/{}", source.identity.config_hash);
        for (name, value) in [
            ("manifest.json", run.manifest_bytes()),
            ("metrics.json", run.metrics_bytes()),
            ("trades.csv", run.trades_bytes()),
            ("equity.csv", run.equity_bytes()),
            (
                "stats-inputs.json",
                run.stats_inputs()
                    .ok_or_else(|| invalid("paquet manquant"))?
                    .bytes
                    .as_slice(),
            ),
        ] {
            files.push(BundleFile {
                relative_path: Path::new(&dir).join(name),
                bytes: value.to_vec(),
            });
        }
        let local = projection(&document, &source.identity.config_hash, &panel_hash);
        let local_bytes = bytes(&local)?;
        let local_manifest = current_manifest(
            &local,
            &local_bytes,
            admission,
            Some(source.identity.config_hash.clone()),
        )?;
        let local_manifest_bytes = bytes(&local_manifest)?;
        entries.push(IndexSource {
            config_hash: source.identity.config_hash,
            dir: dir.clone(),
            controls: FileRef {
                file: CONTROLS_FILE.into(),
                hash: hash::blake3_prefixed(&local_bytes),
            },
            manifest: FileRef {
                file: MANIFEST_FILE.into(),
                hash: hash::blake3_prefixed(&local_manifest_bytes),
            },
        });
        files.push(BundleFile {
            relative_path: Path::new(&dir).join(CONTROLS_FILE),
            bytes: local_bytes,
        });
        manifests.push(BundleFile {
            relative_path: Path::new(&dir).join(MANIFEST_FILE),
            bytes: local_manifest_bytes,
        });
    }
    files.push(BundleFile {
        relative_path: MANIFEST_FILE.into(),
        bytes: manifest_bytes.clone(),
    });
    files.extend(manifests);
    let index = ControlsIndex {
        schema: INDEX_SCHEMA.into(),
        complete: true,
        control_run_id: document.control_run_id,
        canonical_reference: document.canonical_reference,
        panel: FileRef {
            file: CONTROLS_FILE.into(),
            hash: panel_hash,
        },
        manifest: FileRef {
            file: MANIFEST_FILE.into(),
            hash: hash::blake3_prefixed(&manifest_bytes),
        },
        sources: entries,
    };
    Ok(ControlsBundleBytes {
        files,
        index: bytes(&index)?,
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishEvent {
    BeforeFile,
    BeforeRename,
}
pub fn publish(out: &Path, bundle: &ControlsBundleBytes) -> Result<(), ControlsError> {
    publish_with(out, bundle, &mut |_, _| Ok(()))
}
/// Le callback ne change pas les octets ; il peut injecter une erreur d'I/O avant une étape.
pub fn publish_with(
    out: &Path,
    bundle: &ControlsBundleBytes,
    hook: &mut dyn FnMut(PublishEvent, &Path) -> std::io::Result<()>,
) -> Result<(), ControlsError> {
    fs::create_dir(out).map_err(|e| invalid(format!("nouveau dossier {} : {e}", out.display())))?;
    let result = (|| {
        for (path, content) in bundle
            .files
            .iter()
            .map(|f| (f.relative_path.as_path(), f.bytes.as_slice()))
            .chain(std::iter::once((
                Path::new(INDEX_FILE),
                bundle.index.as_slice(),
            )))
        {
            let dest = out.join(path);
            let dir = dest
                .parent()
                .ok_or_else(|| invalid("parent de fichier absent"))?;
            hook(PublishEvent::BeforeFile, &dest).map_err(|e| invalid(e.to_string()))?;
            fs::create_dir_all(dir).map_err(|e| invalid(e.to_string()))?;
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| invalid("nom de fichier invalide"))?;
            let staged = format!("{name}.stage");
            let stage = report::write_bytes_atomic(dir, &staged, content)
                .map_err(|e| invalid(e.to_string()))?;
            hook(PublishEvent::BeforeRename, &dest).map_err(|e| invalid(e.to_string()))?;
            fs::rename(stage, &dest).map_err(|e| invalid(e.to_string()))?;
            File::open(dir)
                .and_then(|f| f.sync_all())
                .map_err(|e| invalid(e.to_string()))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(out);
    }
    result
}
