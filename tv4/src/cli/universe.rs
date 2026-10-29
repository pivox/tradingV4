//! Sous-commandes `tv4 universe ...` (S11, docs/specs/s11.md §4, §5, §7 et §11) : `build`, `verify`,
//! `survivorship` et `okx-subset`.
//!
//! Aucune logique métier ici : la CLI ouvre le zip (lecteur de S5, SHA-256 vérifié), enchaîne les
//! fonctions pures de [`crate::research::universe`] et [`crate::research::survivorship`] et passe par
//! l'écriture vérifiée de [`crate::store::universe`].
//!
//! # `build` : deux passes (§4.5)
//!
//! 1. **passe 1** : pour chaque paire candidate, lecture de l'entrée 1m, `check_m1` avec la même fin de
//!    couverture que l'ingestion, puis [`scan_checked`] qui garde les statistiques (volumes, derniers
//!    closes, trous de cotation, qualité) et libère la série avant la paire suivante ;
//! 2. sélection (`select`) à chaque début de trimestre entièrement couvert et à chaque date `--at` ;
//! 3. **passe 2** : relecture des seules paires qui ont besoin d'un hash ou d'une règle d'instrument
//!    (membres d'un trimestre, éligibles d'une date `--at`) pour `input_hash`, `quality_hash` et les
//!    règles `decimals_v1`, une série en mémoire à la fois ;
//! 4. écriture : CSV, `instruments_kraken.toml`, puis `universe.json` en dernier.
//!
//! **Sans fin de couverture lisible** (pas de `MANIFEST.json`, ou couverture illisible) et sans
//! `--coverage-end`, la commande échoue (code 1) et n'écrit rien : le repli de l'ingestion sur l'instant
//! présent rendrait les fichiers dépendants de l'heure d'exécution. Avec les deux, la plus petite des
//! deux est retenue.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Args, Subcommand};

use super::CliError;
use crate::marketdata::continuity::{self, CheckedSeries};
use crate::marketdata::kraken_zip::{
    KrakenManifest, KrakenPair, KrakenZip, KrakenZipError, Sha256Hex,
};
use crate::marketdata::okx::instruments::{InstrumentsError, InstrumentsFile};
use crate::model::universe::{UniverseConfig, quarter_bounds};
use crate::model::{Candle, Timeframe, Ts};
use crate::research::survivorship::survey_zip;
use crate::research::universe::{
    DEFAULT_FIRST_QUARTER, INSTRUMENTS_METHOD, PairEntry, PairScan, PairStats, QuarterSelection,
    Selected, candidates_from_entries, covered_quarters, derive_instrument_rule, input_hash,
    okx_usdc_subset, quality_hash, scan_checked, select, union_entries, universe_rules,
};
use crate::store::time::{MS_PER_DAY, format_rfc3339_utc, parse_coverage_end};
use crate::store::universe::{
    self, AtBuild, CsvRow, InstrumentRule, QuarterBuild, RejectedPair, UniverseBuild,
    UniverseSource, write_pair_subset,
};
use crate::store::{self, Partition, Symbol, UtcMs, quality_report};

/// Source de provenance de l'univers (`source.kind` de `universe.json`).
const SOURCE_KIND: &str = "kraken_ohlcvt_zip";

/// `release` écrite quand le zip n'a pas de manifeste lisible (la couverture vient de `--coverage-end`).
const UNKNOWN_RELEASE: &str = "unknown";

/// Taille moyenne d'une ligne Kraken en octets, pour dimensionner le `Vec<Candle>` (comme l'ingestion).
const BYTES_PER_ROW: u64 = 40;

/// Motifs de rejet d'une paire dont la série ne peut pas être classée (s11 §4.1, point n° 18).
const REASON_UNREADABLE: &str = "unreadable_entry";
const REASON_EMPTY: &str = "empty_series";

/// Sous-commandes de `tv4 universe`.
#[derive(Debug, Subcommand)]
pub enum UniverseAction {
    /// Calcule l'univers trimestriel depuis le zip Kraken : un CSV par trimestre entièrement couvert,
    /// les règles d'instrument des paires de l'union et `universe.json` (§4).
    Build(UniverseBuildArgs),
    /// Recalcule depuis le store les hashes et la première minute active de chaque membre (§4.5).
    Verify(UniverseVerifyArgs),
    /// Test de survivance : paires ICN, BSV, LUNA, UST, FTT, WAVES et date de début du programme (§5).
    Survivorship(UniverseSurvivorshipArgs),
    /// Liste figée des bases tradables sur OKX en USDC, dérivée d'un fichier d'instruments (§7).
    OkxSubset(UniverseOkxSubsetArgs),
}

/// Valide un libellé de trimestre `AAAA-Qn` pour clap (erreur d'usage, code 2).
fn parse_quarter(text: &str) -> Result<String, String> {
    quarter_bounds(text)
        .map(|_| text.to_string())
        .map_err(|e| e.to_string())
}

/// Arguments de `tv4 universe build`.
#[derive(Debug, Args)]
pub struct UniverseBuildArgs {
    /// Zip OHLCVT complet de Kraken.
    #[arg(long, value_name = "ZIP")]
    pub zip: PathBuf,
    /// SHA-256 publié par Kraken pour ce zip (64 caractères hexadécimaux), vérifié avant toute lecture.
    #[arg(long, value_name = "HEX")]
    pub sha256: Sha256Hex,
    /// Dossier de sortie (créé au besoin) ; un `universe.json` existant fige ses trimestres.
    #[arg(long, value_name = "DOSSIER")]
    pub out: PathBuf,
    /// Listes de l'univers (cotations, bases écartées, motifs de survivance, alias OKX).
    #[arg(long, value_name = "TOML", default_value = "config/universe.toml")]
    pub config: PathBuf,
    /// Premier trimestre produit, `AAAA-Qn` (défaut 2018-Q1).
    #[arg(long, value_name = "AAAA-Qn", value_parser = parse_quarter)]
    pub from_quarter: Option<String>,
    /// Dernier trimestre produit, `AAAA-Qn` (défaut : le dernier trimestre entièrement couvert).
    #[arg(long, value_name = "AAAA-Qn", value_parser = parse_quarter)]
    pub to_quarter: Option<String>,
    /// Date supplémentaire (minuit UTC, RFC 3339) dont tous les éligibles sont écrits dans
    /// `eligible_<date>.csv` ; répétable.
    #[arg(long = "at", value_name = "RFC3339")]
    pub at: Vec<UtcMs>,
    /// Fin de couverture exclusive (RFC 3339 UTC), obligatoire si le zip n'a pas de manifeste lisible ;
    /// la plus petite des deux est retenue si le manifeste en donne une.
    #[arg(long, value_name = "RFC3339")]
    pub coverage_end: Option<UtcMs>,
}

/// Arguments de `tv4 universe verify`.
#[derive(Debug, Args)]
pub struct UniverseVerifyArgs {
    /// Racine du store (contient data/).
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
    /// Dossier d'univers (`tv4 universe build`).
    #[arg(long, value_name = "DOSSIER")]
    pub universe: PathBuf,
}

/// Arguments de `tv4 universe survivorship`.
#[derive(Debug, Args)]
pub struct UniverseSurvivorshipArgs {
    /// Zip OHLCVT complet de Kraken.
    #[arg(long, value_name = "ZIP")]
    pub zip: PathBuf,
    /// SHA-256 publié par Kraken pour ce zip, vérifié avant toute lecture.
    #[arg(long, value_name = "HEX")]
    pub sha256: Sha256Hex,
    /// Fichier JSON du rapport ; un fichier existant au contenu différent n'est jamais écrasé.
    #[arg(long, value_name = "JSON")]
    pub out: PathBuf,
    /// Listes de l'univers (motifs de survivance).
    #[arg(long, value_name = "TOML", default_value = "config/universe.toml")]
    pub config: PathBuf,
    /// Fin de couverture exclusive (RFC 3339 UTC), comme pour `universe build`.
    #[arg(long, value_name = "RFC3339")]
    pub coverage_end: Option<UtcMs>,
}

/// Arguments de `tv4 universe okx-subset`.
#[derive(Debug, Args)]
pub struct UniverseOkxSubsetArgs {
    /// Fichier d'instruments OKX (`tv4 instruments fetch`), dont le hash est vérifié.
    #[arg(long, value_name = "JSON")]
    pub instruments: PathBuf,
    /// Fichier `tv4-pair-subset-v1` produit ; un fichier existant n'est jamais écrasé.
    #[arg(long, value_name = "JSON")]
    pub out: PathBuf,
    /// Listes de l'univers (table d'alias OKX).
    #[arg(long, value_name = "TOML", default_value = "config/universe.toml")]
    pub config: PathBuf,
}

// ---------------------------------------------------------------------------------------------
// Zip et fin de couverture
// ---------------------------------------------------------------------------------------------

/// Zip ouvert et vérifié, son manifeste et la fin de couverture retenue.
struct OpenedZip {
    zip: KrakenZip,
    manifest: Option<KrakenManifest>,
    coverage_end: Ts,
}

/// Ouvre le zip, lit son manifeste et fixe la fin de couverture (§4.5) : la plus petite de celle du
/// manifeste et de `--coverage-end`. Sans l'une ni l'autre, [`CliError::CoverageUnknown`].
fn open_zip(
    path: &Path,
    sha256: &Sha256Hex,
    coverage_override: Option<UtcMs>,
) -> Result<OpenedZip, CliError> {
    let started = Instant::now();
    let mut zip = KrakenZip::open_verified(path, sha256)?;
    println!(
        "zip : {} ({} octets, {} entrées), SHA-256 vérifié : {} ({:.2} s)",
        path.display(),
        zip.size(),
        zip.entry_count(),
        zip.sha256(),
        started.elapsed().as_secs_f64()
    );
    let manifest = zip.manifest()?;
    if manifest.as_ref().and_then(|m| m.has_header) == Some(true) {
        return Err(KrakenZipError::HeaderUnsupported.into());
    }
    let from_manifest = match manifest.as_ref().and_then(|m| m.coverage_end.as_deref()) {
        Some(text) => {
            let parsed = parse_coverage_end(text);
            if parsed.is_none() {
                println!(
                    "avertissement : couverture « {text} » illisible (AAAA-MM-JJ ou AAAA-MM-JJTHH:MM:SSZ attendu), ignorée"
                );
            }
            parsed
        }
        None => {
            println!("avertissement : pas de fin de couverture dans MANIFEST.json");
            None
        }
    };
    let coverage_end = match (from_manifest, coverage_override.map(|c| c.0)) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return Err(CliError::CoverageUnknown),
    };
    println!(
        "fin de couverture retenue : {}",
        format_rfc3339_utc(coverage_end)?
    );
    Ok(OpenedZip {
        zip,
        manifest,
        coverage_end,
    })
}

/// Lit l'entrée 1m d'une paire et la contrôle avec `check_m1` (même fonction et même fin de couverture
/// que l'ingestion, §0).
fn read_checked(
    zip: &mut KrakenZip,
    pair: &KrakenPair,
    coverage_end: Ts,
) -> Result<CheckedSeries, KrakenZipError> {
    let size = zip.entry_size_1m(pair)?;
    let capacity = usize::try_from(size / BYTES_PER_ROW).unwrap_or(0);
    let rows = zip.rows_1m(pair)?;
    Ok(continuity::check_m1_with_capacity(
        rows,
        coverage_end,
        capacity,
    ))
}

// ---------------------------------------------------------------------------------------------
// tv4 universe build
// ---------------------------------------------------------------------------------------------

/// Ce que la passe 2 calcule pour une paire.
#[derive(Default)]
struct Need {
    symbol: String,
    /// Dates dont il faut l'`input_hash` (débuts de trimestre et dates `--at`).
    input_dates: BTreeSet<Ts>,
    /// Trimestres dont il faut le `quality_hash` (la paire y est membre).
    quarters: BTreeSet<String>,
    /// Vrai si la paire est dans l'union : sa règle d'instrument est dérivée.
    rule: bool,
}

/// Résultats de la passe 2.
#[derive(Default)]
struct PassTwo {
    input_hashes: BTreeMap<(Ts, String), String>,
    quality_hashes: BTreeMap<(String, String), String>,
    rules: BTreeMap<String, InstrumentRule>,
}

/// Nombre de jours d'un instant, pour valider qu'une date est un minuit UTC.
fn is_midnight(ts_ms: Ts) -> bool {
    ts_ms.rem_euclid(MS_PER_DAY) == 0
}

/// Passe 2 d'une paire : hashes et règle d'instrument depuis la série contrôlée.
fn compute_pass_two(need: &Need, candles: &[Candle], out: &mut PassTwo) -> Result<(), CliError> {
    for &date in &need.input_dates {
        out.input_hashes.insert(
            (date, need.symbol.clone()),
            input_hash(&need.symbol, candles, date)?,
        );
    }
    for quarter in &need.quarters {
        out.quality_hashes.insert(
            (quarter.clone(), need.symbol.clone()),
            quality_hash(&need.symbol, candles, quarter)?,
        );
    }
    if need.rule {
        out.rules.insert(
            need.symbol.clone(),
            derive_instrument_rule(&need.symbol, candles)?,
        );
    }
    Ok(())
}

/// Ligne de CSV d'un éligible : `excluded` vient des statistiques de la passe 1, `input_hash` de la
/// passe 2.
fn csv_row(
    selected: &Selected,
    stats: &BTreeMap<&str, &PairStats>,
    quarter: Option<&str>,
    pass_two: &PassTwo,
    date_ms: Ts,
) -> Result<CsvRow, CliError> {
    let symbol = selected.candidate.symbol.as_str();
    let excluded = match (quarter, stats.get(symbol)) {
        (Some(label), Some(stats)) => stats.excluded_tfs(label).to_vec(),
        _ => Vec::new(),
    };
    let input = pass_two
        .input_hashes
        .get(&(date_ms, symbol.to_string()))
        .cloned()
        .ok_or_else(|| CliError::Usage {
            option: "universe build".to_string(),
            detail: format!("input_hash de {symbol} absent de la passe 2 (bug)"),
        })?;
    Ok(selected.to_csv_row(excluded, input)?)
}

/// `tv4 universe build`.
pub(super) fn build(args: &UniverseBuildArgs) -> Result<(), CliError> {
    let started = Instant::now();
    let config = UniverseConfig::load(&args.config)?;
    // Contrôles des dates avant d'ouvrir le zip (SHA-256, passes) : une erreur de saisie ne coûte rien.
    // Avec la règle du minuit, deux dates du même jour sont deux instants égaux.
    let mut seen_days = BTreeSet::new();
    for at in &args.at {
        if !is_midnight(at.0) {
            return Err(CliError::Usage {
                option: "--at".to_string(),
                detail: format!(
                    "{} : un minuit UTC est exigé (le rang de liquidité se lit au jour)",
                    format_rfc3339_utc(at.0)?
                ),
            });
        }
        if !seen_days.insert(at.0.div_euclid(MS_PER_DAY)) {
            return Err(CliError::Usage {
                option: "--at".to_string(),
                detail: format!(
                    "{} : date donnée deux fois (un fichier eligible_<jour>.csv par jour)",
                    format_rfc3339_utc(at.0)?
                ),
            });
        }
    }
    let OpenedZip {
        mut zip,
        manifest,
        coverage_end,
    } = open_zip(&args.zip, &args.sha256, args.coverage_end)?;
    // La sélection à `D` ne lit que `[D − 365 j, D)` : une date postérieure à la fin de couverture
    // compterait des jours sans données comme des volumes nuls. La borne exclusive elle-même est permise.
    for at in &args.at {
        if at.0 > coverage_end {
            return Err(CliError::Usage {
                option: "--at".to_string(),
                detail: format!(
                    "{} : postérieure à la fin de couverture du dump ({}), éligibilité non mesurable",
                    format_rfc3339_utc(at.0)?,
                    format_rfc3339_utc(coverage_end)?
                ),
            });
        }
    }

    // Trimestres entièrement couverts (point n° 17 de §16), dans la plage demandée.
    let first = args
        .from_quarter
        .as_deref()
        .unwrap_or(DEFAULT_FIRST_QUARTER);
    let mut labels = covered_quarters(first, coverage_end)?;
    if let Some(last) = args.to_quarter.as_deref() {
        labels.retain(|label| label.as_str() <= last);
    }
    if labels.is_empty() {
        return Err(CliError::UniverseNoQuarter {
            from: first.to_string(),
            coverage_end: format_rfc3339_utc(coverage_end)?,
        });
    }

    // Paires candidates.
    let names = zip.entry_names();
    let set = candidates_from_entries(&names, &config)?;
    println!(
        "{} paires dans le zip, {} candidates (USD et USDT, hors fiat et stablecoins), {} noms de forme héritée écartés",
        set.pairs_in_zip,
        set.candidates.len(),
        set.unrecognized_pairs.len()
    );

    // Passe 1.
    let mut entries: Vec<PairEntry> = Vec::with_capacity(set.candidates.len());
    let mut rejected: Vec<RejectedPair> = Vec::new();
    for candidate in &set.candidates {
        let pair: KrakenPair = candidate.kraken_pair.parse()?;
        let reject = |reasons: Vec<String>| RejectedPair {
            kraken_pair: candidate.kraken_pair.clone(),
            reasons,
        };
        match read_checked(&mut zip, &pair, coverage_end) {
            Err(error) => {
                println!(
                    "{} : entrée illisible ({error}), paire écartée",
                    candidate.kraken_pair
                );
                rejected.push(reject(vec![REASON_UNREADABLE.to_string()]));
            }
            Ok(checked) if checked.candles.is_empty() && !checked.report.is_blocking() => {
                println!(
                    "{} : aucune bougie clôturée, paire écartée",
                    candidate.kraken_pair
                );
                rejected.push(reject(vec![REASON_EMPTY.to_string()]));
            }
            Ok(checked) => match scan_checked(&checked)? {
                PairScan::Accepted(stats) => entries.push(PairEntry {
                    candidate: candidate.clone(),
                    stats: *stats,
                }),
                PairScan::Rejected { reasons } => {
                    println!(
                        "{} : série bloquante ({}), paire écartée",
                        candidate.kraken_pair,
                        reasons.join(", ")
                    );
                    rejected.push(reject(reasons));
                }
            },
        }
    }
    // Mémoire de la passe 1 : les trous de cotation de toutes les candidates restent en mémoire
    // jusqu'à la sélection (s11 §4.5 point 2) ; ce relevé se compare au pic de `/usr/bin/time -l`.
    let total_gaps: usize = entries.iter().map(|e| e.stats.gap_count()).sum();
    let total_gap_bytes: usize = entries.iter().map(|e| e.stats.gap_bytes()).sum();
    println!(
        "passe 1 : {} paires classées, {} écartées, {total_gaps} trous de cotation en mémoire ({:.1} Mo) ({:.1} s)",
        entries.len(),
        rejected.len(),
        total_gap_bytes as f64 / 1_000_000.0,
        started.elapsed().as_secs_f64()
    );

    // Sélection : débuts de trimestre puis dates --at.
    let mut dates: Vec<Ts> = Vec::with_capacity(labels.len() + args.at.len());
    for label in &labels {
        dates.push(quarter_bounds(label)?.0);
    }
    dates.extend(args.at.iter().map(|at| at.0));
    let selections = select(&entries, &dates, &config)?;
    let (quarter_selections, at_selections) = selections.split_at(labels.len());
    // La sélection est faite : l'assemblage ne relit plus que `excluded_tfs`. On rend les tableaux de
    // jours et de trous avant la passe 2, dont le pic redevient celui de la plus longue série.
    for entry in &mut entries {
        entry.stats.release_selection_data();
    }

    // Besoins de la passe 2, par paire Kraken.
    let mut needs: BTreeMap<String, Need> = BTreeMap::new();
    for (label, selection) in labels.iter().zip(quarter_selections) {
        for member in selection.members() {
            let need = needs
                .entry(member.candidate.kraken_pair.clone())
                .or_default();
            need.symbol.clone_from(&member.candidate.symbol);
            need.input_dates.insert(selection.date_ms);
            need.quarters.insert(label.clone());
            need.rule = true;
        }
    }
    for selection in at_selections {
        // Une date --at demande l'`input_hash` de tous les éligibles, pas seulement des 12 premiers.
        for eligible in &selection.eligible {
            let need = needs
                .entry(eligible.candidate.kraken_pair.clone())
                .or_default();
            need.symbol.clone_from(&eligible.candidate.symbol);
            need.input_dates.insert(selection.date_ms);
        }
    }

    // Passe 2.
    let mut pass_two = PassTwo::default();
    for (kraken_pair, need) in &needs {
        let pair: KrakenPair = kraken_pair.parse()?;
        let checked = read_checked(&mut zip, &pair, coverage_end)?;
        if checked.report.is_blocking() || checked.candles.is_empty() {
            return Err(CliError::Usage {
                option: "universe build".to_string(),
                detail: format!(
                    "{kraken_pair} : la série lue en passe 2 diffère de la passe 1 (bug)"
                ),
            });
        }
        compute_pass_two(need, &checked.candles, &mut pass_two)?;
    }
    println!(
        "passe 2 : {} paires relues ({:.1} s)",
        needs.len(),
        started.elapsed().as_secs_f64()
    );

    // Assemblage.
    let stats: BTreeMap<&str, &PairStats> = entries
        .iter()
        .map(|entry| (entry.candidate.symbol.as_str(), &entry.stats))
        .collect();
    let mut quarters: Vec<QuarterBuild> = Vec::with_capacity(labels.len());
    for (label, selection) in labels.iter().zip(quarter_selections) {
        let mut rows = Vec::with_capacity(selection.members().len());
        let mut quality_hashes = BTreeMap::new();
        for member in selection.members() {
            rows.push(csv_row(
                member,
                &stats,
                Some(label),
                &pass_two,
                selection.date_ms,
            )?);
            let symbol = member.candidate.symbol.clone();
            let hash = pass_two
                .quality_hashes
                .get(&(label.clone(), symbol.clone()))
                .cloned()
                .ok_or_else(|| CliError::Usage {
                    option: "universe build".to_string(),
                    detail: format!("quality_hash de {symbol} {label} absent de la passe 2 (bug)"),
                })?;
            quality_hashes.insert(symbol, hash);
        }
        quarters.push(QuarterBuild {
            quarter: label.clone(),
            rows,
            eligible: selection.eligible_count(),
            quality_hashes,
        });
    }
    let mut at: Vec<AtBuild> = Vec::with_capacity(at_selections.len());
    for selection in at_selections {
        let mut rows = Vec::with_capacity(selection.eligible.len());
        for eligible in &selection.eligible {
            rows.push(csv_row(
                eligible,
                &stats,
                None,
                &pass_two,
                selection.date_ms,
            )?);
        }
        at.push(AtBuild {
            date: format_rfc3339_utc(selection.date_ms)?,
            rows,
        });
    }
    let labelled: Vec<(String, &QuarterSelection)> = labels
        .iter()
        .cloned()
        .zip(quarter_selections.iter())
        .collect();
    let union = union_entries(&labelled);
    let mut instruments: Vec<InstrumentRule> = Vec::with_capacity(union.len());
    for entry in &union {
        let rule = pass_two
            .rules
            .get(&entry.symbol)
            .cloned()
            .ok_or_else(|| CliError::Usage {
                option: "universe build".to_string(),
                detail: format!(
                    "règle d'instrument de {} absente de la passe 2 (bug)",
                    entry.symbol
                ),
            })?;
        instruments.push(rule);
    }
    instruments.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    let build = UniverseBuild {
        source: UniverseSource {
            kind: SOURCE_KIND.to_string(),
            zip_sha256: zip.sha256().to_string(),
            release: manifest
                .as_ref()
                .and_then(|m| m.release.clone())
                .unwrap_or_else(|| UNKNOWN_RELEASE.to_string()),
            coverage_end: format_rfc3339_utc(coverage_end)?,
            pairs_in_zip: set.pairs_in_zip,
            candidates: set.candidates.len() as u64,
        },
        rules: universe_rules(&config),
        quarters,
        at,
        rejected_pairs: rejected,
        unrecognized_pairs: set.unrecognized_pairs.clone(),
        union,
        instruments,
        instruments_method: INSTRUMENTS_METHOD.to_string(),
    };
    let file = universe::write_universe(&args.out, &build)?;

    // Compte rendu.
    println!("univers écrit dans {}", args.out.display());
    for quarter in &file.quarters {
        println!(
            "{} : {} membres sur {} éligibles",
            quarter.quarter, quarter.members, quarter.eligible
        );
    }
    for entry in &file.at {
        println!("--at {} : {} éligibles", entry.date, entry.eligible);
    }
    println!(
        "{} paires écartées (série bloquante ou illisible), {} noms de forme héritée non reconnus",
        file.rejected_pairs.len(),
        file.unrecognized_pairs.len()
    );
    println!(
        "{} paires dans l'union, {} règles d'instrument écrites ({INSTRUMENTS_METHOD})",
        file.union.len(),
        build.instruments.len()
    );
    println!("rules_hash {}", file.rules_hash);
    println!("report_hash {}", file.report_hash);
    println!("durée totale {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// tv4 universe verify
// ---------------------------------------------------------------------------------------------

/// `tv4 universe verify` : recalcule depuis le store chaque `input_hash`, chaque `quality_hash`, chaque
/// `first_active` et chaque liste `excluded_tfs` des trimestres, après la vérification des hashes de
/// fichiers que fait `store::universe::load`.
pub(super) fn verify(args: &UniverseVerifyArgs) -> Result<(), CliError> {
    let loaded = universe::load(&args.universe)?;
    println!(
        "univers {} : schéma, report_hash, rules_hash et file_hash de {} trimestre(s) vérifiés",
        args.universe.display(),
        loaded.quarters().len()
    );

    // Les membres à contrôler, groupés par symbole : une série du store en mémoire à la fois.
    let mut by_symbol: BTreeMap<&str, Vec<(&universe::LoadedQuarter, &CsvRow)>> = BTreeMap::new();
    for quarter in loaded.quarters() {
        for row in &quarter.rows {
            by_symbol
                .entry(row.symbol.as_str())
                .or_default()
                .push((quarter, row));
        }
    }
    let mut problems: Vec<String> = Vec::new();
    let mut checked_rows = 0_usize;
    for (symbol, members) in &by_symbol {
        let parsed: Symbol = symbol.parse()?;
        let partition = Partition::new(crate::model::ExchangeId::Kraken, parsed, Timeframe::M1);
        let columns = store::read_series_window(&args.root, &partition, None)?;
        if columns.is_empty() {
            problems.push(format!(
                "{symbol} : aucune bougie 1m dans le store sous {} (ingérer l'union : tv4 ingest kraken --pairs-from-universe)",
                args.root.display()
            ));
            continue;
        }
        let candles = columns.to_candles();
        let first_active = candles
            .iter()
            .find(|candle| candle.volume > 0.0)
            .map(|candle| candle.open_time_ms);
        let quality = quality_report(columns.open_time_ms(), &Timeframe::ALL)?;
        for (quarter, row) in members {
            checked_rows += 1;
            let label = quarter.entry.quarter.as_str();
            match first_active.map(format_rfc3339_utc) {
                Some(Ok(found)) if found == row.first_active => {}
                Some(Ok(found)) => problems.push(format!(
                    "{label} {symbol} : first_active {} dans le CSV, {found} dans le store",
                    row.first_active
                )),
                _ => problems.push(format!(
                    "{label} {symbol} : aucune minute active dans le store"
                )),
            }
            let expected_input = input_hash(symbol, &candles, quarter.entry.date_ms)?;
            if expected_input != row.input_hash {
                problems.push(format!(
                    "{label} {symbol} : input_hash {} dans le CSV, {expected_input} recalculé",
                    row.input_hash
                ));
            }
            let expected_quality = quality_hash(symbol, &candles, label)?;
            match quarter.entry.quality_hashes.get(*symbol) {
                Some(found) if *found == expected_quality => {}
                found => problems.push(format!(
                    "{label} {symbol} : quality_hash {} dans universe.json, {expected_quality} recalculé",
                    found.map_or("absent", String::as_str)
                )),
            }
            let excluded: Vec<Timeframe> = quality
                .rows
                .iter()
                .filter(|r| r.quarter == label && r.excluded && r.tf != Timeframe::M1)
                .map(|r| r.tf)
                .collect();
            if excluded != row.excluded_tfs {
                problems.push(format!(
                    "{label} {symbol} : excluded_tfs {:?} dans le CSV, {excluded:?} recalculé",
                    row.excluded_tfs
                ));
            }
        }
    }
    for problem in &problems {
        println!("écart : {problem}");
    }
    if problems.is_empty() {
        println!(
            "universe verify : {checked_rows} membre(s) de trimestre sur {} symbole(s), tout est identique au store",
            by_symbol.len()
        );
        Ok(())
    } else {
        Err(CliError::UniverseVerifyFailed {
            path: args.universe.display().to_string(),
            count: problems.len(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// tv4 universe survivorship
// ---------------------------------------------------------------------------------------------

/// `tv4 universe survivorship` : rapport `tv4-survivorship-v1`, déterministe.
pub(super) fn survivorship(args: &UniverseSurvivorshipArgs) -> Result<(), CliError> {
    let config = UniverseConfig::load(&args.config)?;
    let OpenedZip {
        mut zip,
        coverage_end,
        ..
    } = open_zip(&args.zip, &args.sha256, args.coverage_end)?;
    let report = survey_zip(&mut zip, &config, coverage_end)?;
    let text = report.to_json_pretty()?;

    let (dir, name) = split_out_path(&args.out)?;
    match std::fs::read(&args.out) {
        Ok(existing) if existing == text.as_bytes() => {
            println!("{} : inchangé", args.out.display());
        }
        Ok(_) => {
            return Err(CliError::OutputExists {
                path: args.out.display().to_string(),
            });
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            crate::report::write_bytes_atomic(&dir, &name, text.as_bytes())?;
            println!("{} : écrit", args.out.display());
        }
        Err(source) => {
            return Err(CliError::Read {
                path: args.out.display().to_string(),
                source,
            });
        }
    }

    for entry in &report.expected {
        let state = if entry.present { "présent" } else { "absent" };
        let review = if entry.review_needed {
            " (plusieurs bases : à revoir)"
        } else {
            ""
        };
        println!("{} : {state}{review}", entry.pattern);
    }
    println!(
        "{} paires dans le zip, {} délistées (dernière bougie à plus de {} jours de la fin de couverture)",
        report.zip.pairs, report.delisted.count, report.delisted.threshold_days
    );
    match &report.program_start.at_most {
        Some(at_most) => println!("début du programme de dumps au plus tard : {at_most}"),
        None => println!("début du programme de dumps : aucune paire délistée présente"),
    }
    println!("report_hash {}", report.report_hash);
    Ok(())
}

/// Dossier parent et nom de fichier d'un chemin de sortie.
fn split_out_path(path: &Path) -> Result<(PathBuf, String), CliError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CliError::Usage {
            option: "--out".to_string(),
            detail: format!("{} : un nom de fichier est attendu", path.display()),
        })?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((dir, name.to_string()))
}

// ---------------------------------------------------------------------------------------------
// tv4 universe okx-subset
// ---------------------------------------------------------------------------------------------

/// `tv4 universe okx-subset` : liste figée des bases tradables en USDC (`tv4-pair-subset-v1`).
pub(super) fn okx_subset(args: &UniverseOkxSubsetArgs) -> Result<(), CliError> {
    let config = UniverseConfig::load(&args.config)?;
    let instruments = InstrumentsFile::load(&args.instruments).map_err(|error| match error {
        InstrumentsError::HashMismatch { .. } => CliError::InstrumentsHashMismatch {
            path: args.instruments.display().to_string(),
        },
        other => CliError::Instruments(other),
    })?;
    let (_, name) = split_out_path(&args.instruments)?;
    let subset = okx_usdc_subset(&instruments, &name, &config)?;
    if subset.is_empty() {
        return Err(CliError::SubsetEmpty {
            what: format!("fichier d'instruments `{}`", args.instruments.display()),
        });
    }
    write_pair_subset(&args.out, &subset)?;
    println!(
        "{} : {} bases tradables en USDC ({}), subset_hash {}",
        args.out.display(),
        subset.bases.len(),
        subset.bases.join(", "),
        subset.subset_hash
    );
    Ok(())
}
