//! Rapports d'un run de recherche, génération s11 (docs/specs/s11.md §9) : le bloc `research` de
//! `metrics.json` (`tv4-metrics-s11`), la purge, et `lines.json` (`tv4-lines-v1`) qui résume les deux
//! lignes d'un rapport de validation ou de holdout.
//!
//! # Ce que le module fait
//!
//! * [`split_trades`] classe les trades du moteur : **conservés** ou **purgés** (§3.4). Un trade purgé est
//!   un trade encore ouvert à la fin de la fenêtre de décision, soldé par la clôture forcée
//!   (`exit_type = end_of_window`) ; son résultat est artificiel et dépendrait des données de l'autre côté
//!   de la frontière. Le moteur ne retire rien de son journal ni de son equity (il ne rejoue pas le
//!   portefeuille sans ces trades) : la classification est faite ici.
//! * [`ResearchBlock`] est le bloc `research` (§9.2), calculé depuis le [`ResearchResult`] du moteur et
//!   des [`ResearchInputs`] que la CLI fournit (garde, début de chauffe, calendrier d'univers, exclusions
//!   qualité, sous-ensemble). Le détail par trimestre (membres, jambes éligibles, exclusions qualité) est
//!   calculé **ici** depuis le calendrier d'univers et les exclusions : le moteur ne le fournit pas.
//! * [`render_research_run`] produit les octets de `metrics.json`, `trades.csv` et `equity.csv` d'un run de
//!   recherche (sans profils de coûts) ; les profils passent par
//!   [`render_profile_research`](super::profiles::render_profile_research).
//! * [`LinesReport`] produit `lines.json` : l'écart `full` moins `okx_usdc` est publié comme
//!   `residual_survivorship` (docs/04 §2.2).
//!
//! # Base des métriques (§9.2, point n° 5 de §16)
//!
//! * **Métriques de trades sur les trades conservés** : `n_trades`, `n_trades_forced` (zéro par
//!   construction, les fins de fenêtre étant purgées), espérances, `win_rate`, profit factor, durées,
//!   distribution, verdict, `funnel`, `funnel_by_pair` et `g1_executable`.
//! * **Métriques d'equity sur la fenêtre de décision** : PnL net, réalisé, marked, `equity_final`,
//!   rendement, drawdowns, exposition, frais, slippage, `approx`, jours de régime. Le PnL des trades
//!   purgés y est compris ; l'écart avec la somme des trades conservés est
//!   `research.purge.pnl_net_purged`.
//! * `trades.csv` garde **tous** les trades, purgés compris : `result_hash` couvre le journal complet, et un
//!   trade purgé se reconnaît à `exit_type = end_of_window`.
//!
//! # Écarts d'implémentation
//!
//! 1. L'entonnoir (`funnel`, `funnel_by_pair`) et G1 portent sur les trades conservés : les lots des trades
//!    purgés ne sont plus comptés dans `entry_fills`, donc `funnel.entry_not_filled_by_reason.other` les
//!    absorbe (c'est le complément `ordres − lots − refus au fill`). L'identité reste exacte.
//! 2. G0 (`executability`) garde les fractions de capital demandées par **toutes** les entrées du moteur :
//!    ces fractions ne sont pas rattachées à un trade (une entrée refusée au fill n'en a pas), on ne peut
//!    donc pas retirer celles des entrées purgées. G0 juge le notional d'une entrée, pas son issue.
//! 3. Les refus d'entrée propres à la recherche (`research.funnel`) ne sont ni dans
//!    `signals.rejected_by_reason` ni dans `funnel_by_pair[].rejected` : ils ont leur bloc. L'identité de
//!    l'entonnoir d'une paire est `entry_signals = entry_orders + refus à la soumission + refus de
//!    recherche`, contrôlée par `tv4 verify`.
//! 4. Hors univers trimestriel (mode `fixed`), `research.universe.quarters` est vide : l'exclusion qualité
//!    est portée par `research.quality_exclusions`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::{
    Line, PURGE_RULE, PortfolioResult, ResearchIdentity, ResearchResult, UniverseSpec,
};
use crate::model::blocks::{BLOCKS_VERSION, Block, is_purged};
use crate::model::hash;
use crate::model::universe::{PairSubset, QualitySchedule, QuarterExclusion, UniverseSchedule};
use crate::model::{Timeframe, Ts};
use crate::sim::{ExitReason, Trade};
use crate::store::time::format_rfc3339_utc;

use super::ReportError;
use super::metrics_s9::{MetricsS9, MetricsS9Inputs, WindowS9};
use super::profiles::RenderedProfile;
use super::{equity, writers};

/// Identifiant du schéma du manifeste s11 (même constante que `manifest::SCHEMA_S11`).
pub use super::manifest::SCHEMA_S11 as MANIFEST_SCHEMA_S11;
/// Identifiant du schéma des métriques s11 (même constante que `metrics_s9::SCHEMA_S11`).
pub use super::metrics_s9::SCHEMA_S11;

/// Identifiant du schéma de `lines.json`.
pub const LINES_SCHEMA: &str = "tv4-lines-v1";
/// Nom du fichier, à la racine du dossier de lignes.
pub const LINES_FILE: &str = "lines.json";
/// Valeur de `research.metrics_basis` : les métriques de trades portent sur les trades conservés.
pub const METRICS_BASIS: &str = "kept_trades";
/// Écart du simulateur propre à l'univers trimestriel (docs/specs/s11.md §9.2) : la qualité d'un trimestre
/// est jugée sur les minutes de ce trimestre.
pub const UNIVERSE_QUALITY_DEVIATION: &str = "universe_quality_uses_quarter_data";
/// Clé de `lines.json` (`config_hash`, `by_profile`, `residual_survivorship`) d'une ligne sans profils.
pub const SINGLE_PROFILE: &str = "single";
/// Clés des trois profils de coûts, dans l'ordre fixe de S10.
pub const COST_PROFILES: [&str; 3] = ["baseline", "adverse", "maker"];

fn series(detail: impl Into<String>) -> ReportError {
    ReportError::Series {
        detail: detail.into(),
    }
}

fn timestamp(ts_ms: Ts) -> Result<String, ReportError> {
    format_rfc3339_utc(ts_ms).map_err(|_| ReportError::Timestamp { ts_ms })
}

// ---------------------------------------------------------------------------------------------
// Entrées fournies par la CLI
// ---------------------------------------------------------------------------------------------

/// Garde provisoire avant S12 (docs/specs/s11.md §9.5), écrite dans `research.guard`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// Aucune lecture protégée.
    None,
    /// Lecture d'un bloc protégé avec l'option cachée `--unguarded-protected-read`.
    Unguarded,
}

impl Guard {
    /// Valeur écrite dans `metrics.json`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Guard::None => "none",
            Guard::Unguarded => "unguarded",
        }
    }
}

/// Sous-ensemble figé de la ligne `okx_usdc`, tel que la CLI l'a appliqué.
#[derive(Clone, Copy, Debug)]
pub struct SubsetInfo<'a> {
    /// Le fichier `tv4-pair-subset-v1` lu.
    pub subset: &'a PairSubset,
    /// Jambes de l'univers que le sous-ensemble retire de la ligne.
    pub legs_excluded: &'a [String],
}

/// Ce que la CLI fournit en plus du résultat du moteur pour écrire le bloc `research`.
#[derive(Clone, Copy, Debug)]
pub struct ResearchInputs<'a> {
    pub guard: Guard,
    /// Premier instant du dataset de bloc (début de la chauffe).
    pub warmup_start_ms: Ts,
    /// Calendrier d'univers lu du dossier `--universe` ; `None` en univers fixe.
    pub universe: Option<&'a UniverseSchedule>,
    /// Exclusions qualité du manifeste du dataset (univers fixe compris).
    pub quality: &'a QualitySchedule,
    /// Sous-ensemble de la ligne `okx_usdc` ; `None` pour `full`.
    pub subset: Option<SubsetInfo<'a>>,
}

// ---------------------------------------------------------------------------------------------
// Purge
// ---------------------------------------------------------------------------------------------

/// Les trades d'un run de recherche, classés par [`split_trades`].
#[derive(Clone, Debug)]
pub struct TradeSplit {
    /// Trades conservés, dans l'ordre du journal.
    pub kept: Vec<Trade>,
    /// Jambe de chaque trade conservé, alignée sur `kept`.
    pub kept_legs: Vec<u16>,
    /// Nombre de trades purgés.
    pub purged: u64,
    /// Somme des `pnl_net` des trades purgés, dans l'ordre du journal.
    pub pnl_net_purged: f64,
}

/// Classe les trades du moteur : purgés (§3.4, [`is_purged`]) ou conservés.
///
/// # Erreurs
///
/// Un trade purgé qui n'est pas `end_of_window` : le dataset dépasse la fenêtre de décision (un dataset de
/// bloc finit à `decision_end`), et `tv4 verify` ne pourrait plus reconnaître les trades purgés dans
/// `trades.csv`. Un trade `end_of_window` que la règle ne purge pas (entrée décidée à `decision_end` ou
/// après) est refusé de la même façon.
pub fn split_trades(
    result: &PortfolioResult,
    decision_end_ms: Ts,
) -> Result<TradeSplit, ReportError> {
    if result.trade_legs.len() != result.trades.len() {
        return Err(series(format!(
            "{} trades pour {} jambes de trade",
            result.trades.len(),
            result.trade_legs.len()
        )));
    }
    let mut split = TradeSplit {
        kept: Vec::with_capacity(result.trades.len()),
        kept_legs: Vec::with_capacity(result.trades.len()),
        purged: 0,
        pnl_net_purged: 0.0,
    };
    for (trade, &leg) in result.trades.iter().zip(&result.trade_legs) {
        let at_end = trade.exit_reason == ExitReason::ForcedClose;
        let purged = is_purged(
            trade.entry_signal_ms,
            trade.exit_ms,
            decision_end_ms,
            at_end,
        );
        if purged != at_end {
            return Err(series(format!(
                "le lot {} est {} par la règle de purge mais {} `end_of_window` : le dataset dépasse \
                 la fenêtre de décision",
                trade.lot_id,
                if purged { "purgé" } else { "conservé" },
                if at_end { "est" } else { "n'est pas" },
            )));
        }
        if purged {
            split.purged += 1;
            split.pnl_net_purged += trade.pnl_net;
        } else {
            split.kept.push(*trade);
            split.kept_legs.push(leg);
        }
    }
    Ok(split)
}

// ---------------------------------------------------------------------------------------------
// Bloc `research`
// ---------------------------------------------------------------------------------------------

/// Un trimestre de `research.universe.quarters`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QuarterRow {
    pub quarter: String,
    /// Membres du trimestre dans le calendrier d'univers.
    pub members: u64,
    /// Jambes jouées membres du trimestre et non exclues par la qualité pour le TF de décision.
    pub eligible_legs: u64,
    /// Jambes jouées membres du trimestre et exclues par la qualité pour le TF de décision, dans l'ordre
    /// du rang.
    pub excluded_quality: Vec<String>,
}

/// Bloc `research.universe`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UniverseBlock {
    /// `quarterly` ou `fixed`.
    pub mode: &'static str,
    /// Hash des règles de sélection (trimestriel) ; `null` en univers fixe.
    pub rules_hash: Option<String>,
    /// Hash des trimestres lus (trimestriel) ; `null` en univers fixe.
    pub window_hash: Option<String>,
    /// TF de décision dont les exclusions qualité sont lues.
    pub decision_tf: Timeframe,
    /// Symboles demandés (univers fixe, avant le filtre de ligne) ; `null` en trimestriel.
    pub symbols: Option<Vec<String>>,
    /// Trimestres qui recoupent la fenêtre de décision ; vide en univers fixe.
    pub quarters: Vec<QuarterRow>,
}

/// Bloc `research.subset`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SubsetBlock {
    pub kind: String,
    pub subset_hash: String,
    pub legs_excluded: Vec<String>,
}

/// Bloc `research.purge`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PurgeBlock {
    pub rule: &'static str,
    pub trades_kept: u64,
    pub trades_purged: u64,
    pub pnl_net_purged: f64,
}

/// Refus d'entrée de la recherche (§8.3), au total ou pour une jambe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ResearchFunnelBlock {
    pub entries_outside_decision_window: u64,
    pub entries_not_in_universe: u64,
    pub entries_excluded_quality: u64,
    pub entries_cancelled_universe: u64,
}

/// Une jambe de `research.legs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LegBlock {
    pub symbol: String,
    /// Première minute jouée de la jambe ; `null` si elle n'est jamais devenue vivante.
    pub live_from: Option<String>,
    #[serde(flatten)]
    pub funnel: ResearchFunnelBlock,
}

/// Bloc `research` de `metrics.json` s11 (docs/specs/s11.md §9.2).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResearchBlock {
    pub block: Block,
    pub blocks_version: &'static str,
    pub guard: &'static str,
    pub decision_window: WindowS9,
    pub warmup_start: String,
    pub universe: UniverseBlock,
    pub line: Line,
    pub subset: Option<SubsetBlock>,
    pub regime_used: bool,
    pub identity: ResearchIdentity,
    /// `identity.hash()` : le `research_identity_hash` de `run_id_s11`.
    pub identity_hash: String,
    /// `quality_exclusions` du manifeste du dataset, tel quel : son hash est `identity.quality_hash`.
    pub quality_exclusions: BTreeMap<String, Vec<QuarterExclusion>>,
    pub purge: PurgeBlock,
    pub metrics_basis: &'static str,
    pub funnel: ResearchFunnelBlock,
    pub legs: Vec<LegBlock>,
}

fn funnel_block(c: &crate::engine::ResearchLegCounters) -> ResearchFunnelBlock {
    ResearchFunnelBlock {
        entries_outside_decision_window: c.entries_outside_decision_window,
        entries_not_in_universe: c.entries_not_in_universe,
        entries_excluded_quality: c.entries_excluded_quality,
        entries_cancelled_universe: c.entries_cancelled_universe,
    }
}

/// Détail par trimestre : membres, jambes éligibles et exclusions qualité (le moteur ne les fournit pas).
fn quarter_rows(
    schedule: &UniverseSchedule,
    quality: &QualitySchedule,
    played: &[String],
    decision_tf: Timeframe,
    start_ms: Ts,
    end_ms: Ts,
) -> Vec<QuarterRow> {
    schedule
        .overlapping(start_ms, end_ms)
        .iter()
        .map(|quarter| {
            let mut eligible = 0u64;
            let mut excluded = Vec::new();
            for member in &quarter.members {
                if !played.iter().any(|symbol| symbol == &member.symbol) {
                    continue;
                }
                if quality.is_excluded(&member.symbol, quarter.start_ms, decision_tf) {
                    excluded.push(member.symbol.clone());
                } else {
                    eligible += 1;
                }
            }
            QuarterRow {
                quarter: quarter.quarter.clone(),
                members: quarter.members.len() as u64,
                eligible_legs: eligible,
                excluded_quality: excluded,
            }
        })
        .collect()
}

impl ResearchBlock {
    /// Construit le bloc. `played` est l'univers du run (ordre des jambes), `split` la purge.
    ///
    /// # Erreurs
    ///
    /// Entrées incohérentes entre elles : univers trimestriel sans calendrier (ou l'inverse),
    /// `quality_hash` de l'identité différent du hash des exclusions fournies, ligne `okx_usdc` sans
    /// sous-ensemble (ou `full` avec un sous-ensemble), `subset_hash` de l'identité différent de celui du
    /// fichier, jambes du moteur qui ne suivent pas l'univers.
    pub fn build(
        engine: &ResearchResult,
        played: &[String],
        inputs: &ResearchInputs<'_>,
        split: &TradeSplit,
    ) -> Result<ResearchBlock, ReportError> {
        let identity = &engine.identity;
        let identity_hash = engine.identity.hash().map_err(ReportError::Json)?;
        if identity_hash != engine.identity_hash {
            return Err(series(
                "le hash d'identité du moteur diffère de celui de son identité",
            ));
        }
        if identity.block != engine.block
            || identity.decision_start_ms != engine.decision_start_ms
            || identity.decision_end_ms != engine.decision_end_ms
        {
            return Err(series(
                "l'identité de recherche contredit le résultat du moteur",
            ));
        }
        let quality_hash = inputs.quality.hash().map_err(ReportError::Json)?;
        if identity.quality_hash != quality_hash {
            return Err(series(format!(
                "quality_hash {} dans l'identité pour {quality_hash} recalculé depuis les exclusions",
                identity.quality_hash
            )));
        }
        if engine.legs.len() != played.len()
            || engine
                .legs
                .iter()
                .zip(played)
                .any(|(leg, symbol)| &leg.symbol != symbol)
        {
            return Err(series(
                "les jambes de recherche ne suivent pas l'univers du run",
            ));
        }

        let universe = match (&engine.universe_spec, inputs.universe) {
            (UniverseSpec::Quarterly { rules_hash }, Some(schedule)) => {
                let Some(window_hash) = identity.universe_window_hash.clone() else {
                    return Err(series(
                        "univers trimestriel sans `universe_window_hash` dans l'identité",
                    ));
                };
                UniverseBlock {
                    mode: "quarterly",
                    rules_hash: Some(rules_hash.clone()),
                    window_hash: Some(window_hash),
                    decision_tf: engine.decision_tf,
                    symbols: None,
                    quarters: quarter_rows(
                        schedule,
                        inputs.quality,
                        played,
                        engine.decision_tf,
                        engine.decision_start_ms,
                        engine.decision_end_ms,
                    ),
                }
            }
            (UniverseSpec::Fixed { symbols }, None) => {
                if identity.universe_window_hash.is_some() {
                    return Err(series(
                        "univers fixe avec un `universe_window_hash` dans l'identité",
                    ));
                }
                let mut sorted = symbols.clone();
                sorted.sort();
                UniverseBlock {
                    mode: "fixed",
                    rules_hash: None,
                    window_hash: None,
                    decision_tf: engine.decision_tf,
                    symbols: Some(sorted),
                    quarters: Vec::new(),
                }
            }
            (UniverseSpec::Quarterly { .. }, None) => {
                return Err(series("univers trimestriel sans calendrier d'univers"));
            }
            (UniverseSpec::Fixed { .. }, Some(_)) => {
                return Err(series("univers fixe avec un calendrier d'univers"));
            }
        };

        let subset = match (identity.line, inputs.subset) {
            (Line::Full, None) => {
                if identity.subset_hash.is_some() {
                    return Err(series("ligne `full` avec un `subset_hash` dans l'identité"));
                }
                None
            }
            (Line::OkxUsdc, Some(info)) => {
                if identity.subset_hash.as_deref() != Some(info.subset.subset_hash.as_str()) {
                    return Err(series(format!(
                        "subset_hash {:?} dans l'identité pour {} dans le fichier de sous-ensemble",
                        identity.subset_hash, info.subset.subset_hash
                    )));
                }
                Some(SubsetBlock {
                    kind: info.subset.kind.clone(),
                    subset_hash: info.subset.subset_hash.clone(),
                    legs_excluded: info.legs_excluded.to_vec(),
                })
            }
            (Line::OkxUsdc, None) => {
                return Err(series("la ligne `okx_usdc` exige un sous-ensemble"));
            }
            (Line::Full, Some(_)) => {
                return Err(series("la ligne `full` ne porte pas de sous-ensemble"));
            }
        };

        let mut legs = Vec::with_capacity(engine.legs.len());
        for leg in &engine.legs {
            legs.push(LegBlock {
                symbol: leg.symbol.clone(),
                live_from: leg.live_from_ms.map(timestamp).transpose()?,
                funnel: funnel_block(&leg.counters),
            });
        }
        Ok(ResearchBlock {
            block: engine.block,
            blocks_version: BLOCKS_VERSION,
            guard: inputs.guard.as_str(),
            decision_window: WindowS9 {
                start: timestamp(engine.decision_start_ms)?,
                end: timestamp(engine.decision_end_ms)?,
                start_ms: engine.decision_start_ms,
                end_ms: engine.decision_end_ms,
            },
            warmup_start: timestamp(inputs.warmup_start_ms)?,
            universe,
            line: identity.line,
            subset,
            regime_used: engine.regime_used,
            identity: identity.clone(),
            identity_hash,
            quality_exclusions: inputs.quality.exclusions().clone(),
            purge: PurgeBlock {
                rule: PURGE_RULE,
                trades_kept: split.kept.len() as u64,
                trades_purged: split.purged,
                pnl_net_purged: split.pnl_net_purged,
            },
            metrics_basis: METRICS_BASIS,
            funnel: funnel_block(&engine.totals),
            legs,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Rendu des fichiers d'un run de recherche
// ---------------------------------------------------------------------------------------------

/// Rend `metrics.json` (`tv4-metrics-s11`), `trades.csv` (tous les trades, purgés compris) et
/// `equity.csv` (depuis `decision_start`) d'un run de recherche sans profils de coûts.
///
/// # Erreurs
///
/// Celles de [`MetricsS9::new_research`] et de l'écriture des fichiers.
pub fn render_research_run(
    result: &PortfolioResult,
    inputs: MetricsS9Inputs<'_>,
    research: &ResearchInputs<'_>,
    equity_step: Timeframe,
) -> Result<RenderedProfile, ReportError> {
    let metrics = MetricsS9::new_research(result, inputs, research)?;
    let symbols: Vec<&str> = result.universe.iter().map(String::as_str).collect();
    let metrics_bytes = super::json_bytes(&metrics)?;
    let trades_bytes = writers::trades_csv_legs(
        &result.trades,
        &result.trade_legs,
        &symbols,
        metrics.span_ms(),
    )?;
    let equity_bytes = equity::equity_csv(
        &result.grid_open_ms,
        &result.equity,
        result.initial_cash,
        equity_step,
    )?;
    Ok(RenderedProfile {
        metrics,
        metrics_bytes,
        trades_bytes,
        equity_bytes,
    })
}

// ---------------------------------------------------------------------------------------------
// lines.json
// ---------------------------------------------------------------------------------------------

/// Un run (ou un profil) d'une ligne.
#[derive(Clone, Copy, Debug)]
pub struct LineRun<'a> {
    /// `single`, ou `baseline`, `adverse`, `maker`.
    pub profile: &'static str,
    pub metrics: &'a MetricsS9,
}

/// Une ligne : un run, ou les trois profils d'un dossier de profils (dans l'ordre baseline, adverse,
/// maker).
#[derive(Clone, Debug)]
pub struct LineInput<'a> {
    pub line: Line,
    pub runs: Vec<LineRun<'a>>,
}

impl LineInput<'_> {
    /// Vrai pour un dossier de profils.
    pub fn is_profiles(&self) -> bool {
        self.runs
            .first()
            .is_some_and(|r| r.profile != SINGLE_PROFILE)
    }

    /// `run_id` du run, ou `profiles_id` du dossier de profils.
    pub fn id(&self) -> Result<String, ReportError> {
        let ids: Vec<String> = self.runs.iter().map(|r| r.metrics.run_id()).collect();
        match ids.as_slice() {
            [single] if !self.is_profiles() => Ok(single.clone()),
            [baseline, adverse, maker] if self.is_profiles() => {
                Ok(super::manifest::profiles_id(baseline, adverse, maker))
            }
            _ => Err(series(
                "une ligne est un run seul ou trois profils (baseline, adverse, maker)",
            )),
        }
    }

    fn check_shape(&self) -> Result<(), ReportError> {
        let expected: &[&str] = if self.is_profiles() {
            &COST_PROFILES
        } else {
            &[SINGLE_PROFILE]
        };
        let found: Vec<&str> = self.runs.iter().map(|r| r.profile).collect();
        if found != expected {
            return Err(series(format!(
                "ligne `{}` : profils {found:?} pour {expected:?} attendus",
                self.line.as_str()
            )));
        }
        for run in &self.runs {
            let Some(research) = run.metrics.research.as_ref() else {
                return Err(series(format!(
                    "ligne `{}` : le profil `{}` n'est pas un run s11",
                    self.line.as_str(),
                    run.profile
                )));
            };
            if research.line != self.line {
                return Err(series(format!(
                    "ligne `{}` : le profil `{}` est un run de la ligne `{}`",
                    self.line.as_str(),
                    run.profile,
                    research.line.as_str()
                )));
            }
        }
        Ok(())
    }
}

/// Statistiques d'un run dans `lines[].by_profile`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfileStats {
    pub identity_hash: String,
    pub n_trades: u64,
    pub trades_purged: u64,
    pub expectancy_r: Option<f64>,
    pub expectancy_net: Option<f64>,
    pub profit_factor: Option<f64>,
    pub return_total: f64,
    /// `max_drawdown.h1.pct` (fraction).
    pub max_drawdown_1h: f64,
    pub verdict: &'static str,
}

/// Une entrée de `lines[]`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LineEntry {
    pub line: Line,
    /// Sous-dossier de la ligne : son nom.
    pub dir: &'static str,
    /// `run_id` d'un run seul ; absent pour un dossier de profils.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// `profiles_id` d'un dossier de profils ; absent pour un run seul.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profiles_id: Option<String>,
    /// Jambes jouées, dans l'ordre du run.
    pub legs: Vec<String>,
    pub by_profile: BTreeMap<String, ProfileStats>,
}

/// Sous-ensemble de `lines.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinesSubset {
    pub kind: String,
    pub subset_hash: String,
    pub bases: Vec<String>,
}

/// Écart `full − okx_usdc` d'un profil (docs/04 §2.2 : la survivance résiduelle).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Residual {
    pub expectancy_r: Option<f64>,
    pub expectancy_net: Option<f64>,
    pub return_total: Option<f64>,
    pub max_drawdown_1h: Option<f64>,
    pub n_trades: i64,
}

/// Contenu de `lines.json` (`tv4-lines-v1`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LinesReport {
    pub schema: &'static str,
    pub block: Block,
    pub dataset_hash: String,
    pub universe_window_hash: Option<String>,
    /// `config_hash` de chaque profil, commun aux deux lignes.
    pub config_hash: BTreeMap<String, String>,
    pub subset: LinesSubset,
    pub initial_cash: f64,
    /// `full` puis `okx_usdc`.
    pub lines: Vec<LineEntry>,
    pub residual_survivorship: BTreeMap<String, Residual>,
    /// BLAKE3 hexadécimal de `"{id_full}|{id_okx_usdc}"`.
    pub lines_id: String,
}

fn stats_of(metrics: &MetricsS9, research: &ResearchBlock) -> ProfileStats {
    ProfileStats {
        identity_hash: research.identity_hash.clone(),
        n_trades: metrics.n_trades,
        trades_purged: research.purge.trades_purged,
        expectancy_r: metrics.expectancy_r,
        expectancy_net: metrics.expectancy_net,
        profit_factor: metrics.profit_factor,
        return_total: metrics.return_total,
        max_drawdown_1h: metrics.max_drawdown.h1.pct,
        verdict: metrics.verdict,
    }
}

/// `a − b`, `None` si l'une des deux valeurs est indéfinie.
fn minus(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a - b),
        _ => None,
    }
}

impl LinesReport {
    /// Construit `lines.json` des deux lignes d'un rapport de validation ou de holdout.
    ///
    /// # Erreurs
    ///
    /// Les deux lignes ne décrivent pas la même chose : bloc, dataset, univers, capital, profils ou
    /// `config_hash` différents (une dérive du code ferait de deux lectures d'une même configuration deux
    /// configurations), ligne `okx_usdc` dont le sous-ensemble n'est pas celui du fichier donné.
    pub fn build(
        full: &LineInput<'_>,
        okx: &LineInput<'_>,
        subset: &PairSubset,
    ) -> Result<LinesReport, ReportError> {
        if full.line != Line::Full || okx.line != Line::OkxUsdc {
            return Err(series(
                "lines.json : les lignes sont `full` puis `okx_usdc`",
            ));
        }
        full.check_shape()?;
        okx.check_shape()?;
        if full.is_profiles() != okx.is_profiles() {
            return Err(series(
                "lines.json : les deux lignes sont des runs seuls, ou deux dossiers de profils",
            ));
        }
        let (Some(first_full), Some(first_okx)) = (full.runs.first(), okx.runs.first()) else {
            return Err(series("lines.json : une ligne sans run"));
        };
        let (Some(rf), Some(ro)) = (
            first_full.metrics.research.as_ref(),
            first_okx.metrics.research.as_ref(),
        ) else {
            return Err(series("lines.json : une ligne n'est pas un run s11"));
        };
        let common_ok = rf.block == ro.block
            && first_full.metrics.dataset_hash == first_okx.metrics.dataset_hash
            && rf.identity.universe_window_hash == ro.identity.universe_window_hash
            && first_full.metrics.config.initial_cash.to_bits()
                == first_okx.metrics.config.initial_cash.to_bits();
        if !common_ok {
            return Err(series(
                "lines.json : les deux lignes n'ont pas le même bloc, dataset, univers ou capital",
            ));
        }
        match &ro.subset {
            Some(s) if s.kind == subset.kind && s.subset_hash == subset.subset_hash => {}
            _ => {
                return Err(series(
                    "lines.json : le sous-ensemble de la ligne `okx_usdc` n'est pas celui du fichier donné",
                ));
            }
        }

        let mut config_hash = BTreeMap::new();
        let mut by_full = BTreeMap::new();
        let mut by_okx = BTreeMap::new();
        let mut residual = BTreeMap::new();
        for (a, b) in full.runs.iter().zip(&okx.runs) {
            let (Some(ra), Some(rb)) = (a.metrics.research.as_ref(), b.metrics.research.as_ref())
            else {
                return Err(series("lines.json : un profil n'est pas un run s11"));
            };
            if a.metrics.config_hash != b.metrics.config_hash {
                return Err(series(format!(
                    "lines.json : le profil `{}` n'a pas le même config_hash sur les deux lignes \
                     ({} contre {})",
                    a.profile, a.metrics.config_hash, b.metrics.config_hash
                )));
            }
            if a.metrics.dataset_hash != first_full.metrics.dataset_hash
                || b.metrics.dataset_hash != first_full.metrics.dataset_hash
                || a.metrics.config.initial_cash.to_bits()
                    != first_full.metrics.config.initial_cash.to_bits()
                || b.metrics.config.initial_cash.to_bits()
                    != first_full.metrics.config.initial_cash.to_bits()
            {
                return Err(series(
                    "lines.json : les profils ne partagent pas le dataset ou le capital",
                ));
            }
            config_hash.insert(a.profile.to_string(), a.metrics.config_hash.clone());
            let (sa, sb) = (stats_of(a.metrics, ra), stats_of(b.metrics, rb));
            residual.insert(
                a.profile.to_string(),
                Residual {
                    expectancy_r: minus(sa.expectancy_r, sb.expectancy_r),
                    expectancy_net: minus(sa.expectancy_net, sb.expectancy_net),
                    return_total: Some(sa.return_total - sb.return_total),
                    max_drawdown_1h: Some(sa.max_drawdown_1h - sb.max_drawdown_1h),
                    n_trades: i64::try_from(sa.n_trades).map_err(|_| ReportError::Overflow {
                        context: "residual_survivorship.n_trades",
                    })? - i64::try_from(sb.n_trades).map_err(|_| {
                        ReportError::Overflow {
                            context: "residual_survivorship.n_trades",
                        }
                    })?,
                },
            );
            by_full.insert(a.profile.to_string(), sa);
            by_okx.insert(b.profile.to_string(), sb);
        }

        let entry = |input: &LineInput<'_>,
                     by_profile: BTreeMap<String, ProfileStats>|
         -> Result<LineEntry, ReportError> {
            let id = input.id()?;
            let first = input
                .runs
                .first()
                .ok_or_else(|| series("lines.json : une ligne sans run"))?;
            Ok(LineEntry {
                line: input.line,
                dir: input.line.as_str(),
                run_id: (!input.is_profiles()).then(|| id.clone()),
                profiles_id: input.is_profiles().then_some(id),
                legs: first.metrics.data.symbols.clone(),
                by_profile,
            })
        };
        let entry_full = entry(full, by_full)?;
        let entry_okx = entry(okx, by_okx)?;
        let lines_id = hash::blake3_hex(format!("{}|{}", full.id()?, okx.id()?).as_bytes());
        Ok(LinesReport {
            schema: LINES_SCHEMA,
            block: rf.block,
            dataset_hash: first_full.metrics.dataset_hash.clone(),
            universe_window_hash: rf.identity.universe_window_hash.clone(),
            config_hash,
            subset: LinesSubset {
                kind: subset.kind.clone(),
                subset_hash: subset.subset_hash.clone(),
                bases: subset.bases.clone(),
            },
            initial_cash: first_full.metrics.config.initial_cash,
            lines: vec![entry_full, entry_okx],
            residual_survivorship: residual,
            lines_id,
        })
    }

    /// Octets de `lines.json` : JSON canonique suivi d'un `\n`.
    pub fn bytes(&self) -> Result<Vec<u8>, ReportError> {
        super::json_bytes(self)
    }

    /// Écrit `lines.json` dans `dir`, de façon atomique. À appeler en dernier, après les deux lignes.
    pub fn write(&self, dir: &Path) -> Result<PathBuf, ReportError> {
        super::write_json_atomic(dir, LINES_FILE, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_garde_s_ecrit_comme_la_spec() {
        assert_eq!(Guard::None.as_str(), "none");
        assert_eq!(Guard::Unguarded.as_str(), "unguarded");
    }

    #[test]
    fn la_difference_est_indefinie_si_une_valeur_l_est() {
        assert_eq!(minus(Some(3.0), Some(1.0)), Some(2.0));
        assert_eq!(minus(None, Some(1.0)), None);
        assert_eq!(minus(Some(1.0), None), None);
    }

    #[test]
    fn le_bloc_de_lignes_n_a_que_run_id_ou_profiles_id() {
        let entry = LineEntry {
            line: Line::Full,
            dir: "full",
            run_id: Some("aa".to_string()),
            profiles_id: None,
            legs: vec!["BTC-USD".to_string()],
            by_profile: BTreeMap::new(),
        };
        let json = hash::canonical_json(&entry).unwrap();
        assert_eq!(
            json,
            r#"{"by_profile":{},"dir":"full","legs":["BTC-USD"],"line":"full","run_id":"aa"}"#
        );
    }
}
