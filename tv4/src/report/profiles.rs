//! Rapports d'un dossier de profils de coûts (docs/specs/s10.md §12.4) : le rendu des fichiers d'un
//! profil, et `profiles.json` (`tv4-profiles-v1`) qui les résume.
//!
//! # Disposition d'un dossier de profils
//!
//! ```text
//! DIR/baseline/{metrics.json, trades.csv, equity.csv, manifest.json}
//! DIR/adverse/ ...
//! DIR/maker/ ...
//! DIR/profiles.json      (écrit en dernier, après les trois manifestes)
//! ```
//!
//! [`render_profile`] produit les octets de `metrics.json`, `trades.csv` et `equity.csv` d'un profil ; la
//! CLI y ajoute le manifeste ([`ManifestS9::new_s10`](super::ManifestS9::new_s10)), qui marque un run
//! complet. [`ProfilesReport::build`] lit les trois profils rendus et produit `profiles.json`, sans
//! horodatage ni durée : deux runs identiques l'écrivent à l'octet.
//!
//! # Ce que `profiles.json` juge, et ce qu'il ne juge pas
//!
//! * L'étiquette est jugée sur **baseline et adverse seuls** : le profil maker est informatif
//!   (docs/04 §3). Son statut est publié à part ([`MakerStatus`]).
//! * `fill_dependent` (« dépend du fill ») exige un maker positif **et** dont les entrées sont en
//!   majorité remplies en maker (`maker_entry_maker_share ≥ min_maker_entry_share`) : un maker positif
//!   qui prend surtout en taker (`positive_mostly_taker`) ne dit rien du taux de fill.
//! * Un profil maker sous `min_trades` n'empêche pas « tient les coûts » : il a par construction moins
//!   de trades (tirages ratés, ordres au repos jamais traversés).
//! * Aucun verdict de porte n'est prononcé : le fichier fournit les éléments de G1, qui applique ses
//!   propres seuils (`n_min`). `min_trades` est le seuil du run, et non celui de G1.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::ProfileRun;
use crate::model::costs::CostProfileName;
use crate::model::{FeeGridId, Timeframe, hash};

use super::ReportError;
use super::manifest::OutputFiles;
use super::manifest::profiles_id;
use super::metrics_s9::{CostsInputs, MetricsS9, MetricsS9Inputs};
use super::{equity, writers};

/// Identifiant du schéma de `profiles.json`.
pub const PROFILES_SCHEMA: &str = "tv4-profiles-v1";
/// Nom du fichier, à la racine du dossier de profils.
pub const PROFILES_FILE: &str = "profiles.json";

// ---------------------------------------------------------------------------------------------
// Rendu des fichiers d'un profil
// ---------------------------------------------------------------------------------------------

/// Les trois fichiers d'un profil, en octets, et les métriques dont ils viennent.
#[derive(Clone, Debug)]
pub struct RenderedProfile {
    pub metrics: MetricsS9,
    pub metrics_bytes: Vec<u8>,
    pub trades_bytes: Vec<u8>,
    pub equity_bytes: Vec<u8>,
}

impl RenderedProfile {
    /// Empreintes `blake3:<hex>` des trois fichiers (bloc `outputs.files` du manifeste).
    pub fn files(&self) -> OutputFiles {
        OutputFiles {
            equity_csv: hash::blake3_prefixed(&self.equity_bytes),
            metrics_json: hash::blake3_prefixed(&self.metrics_bytes),
            trades_csv: hash::blake3_prefixed(&self.trades_bytes),
        }
    }

    /// `result_hash` : `blake3:` + BLAKE3 des octets de `trades.csv`.
    pub fn result_hash(&self) -> String {
        hash::blake3_prefixed(&self.trades_bytes)
    }

    /// Écrit `metrics.json`, `trades.csv` et `equity.csv` dans `dir`, de façon atomique chacun. Le
    /// manifeste n'est pas écrit ici : il vient en dernier, une fois les trois profils écrits.
    pub fn write_files(&self, dir: &Path) -> Result<(), ReportError> {
        super::write_bytes_atomic(dir, "metrics.json", &self.metrics_bytes)?;
        super::write_bytes_atomic(dir, writers::TRADES_FILE, &self.trades_bytes)?;
        super::write_bytes_atomic(dir, equity::EQUITY_FILE, &self.equity_bytes)?;
        Ok(())
    }
}

/// Rend `metrics.json` (`tv4-metrics-s10`), `trades.csv` (colonne `spread_cost` remplie) et `equity.csv`
/// d'un profil joué.
pub fn render_profile(
    run: &ProfileRun,
    inputs: MetricsS9Inputs<'_>,
    costs: CostsInputs<'_>,
    equity_step: Timeframe,
) -> Result<RenderedProfile, ReportError> {
    let metrics = MetricsS9::new_s10(run, inputs, costs)?;
    let result = &run.result;
    let symbols: Vec<&str> = result.universe.iter().map(String::as_str).collect();
    let metrics_bytes = super::json_bytes(&metrics)?;
    let trades_bytes = writers::trades_csv_legs_s10(
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
// profiles.json
// ---------------------------------------------------------------------------------------------

/// Étiquette d'un dossier de profils (docs/specs/s10.md §12.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileLabel {
    /// Baseline ou adverse a moins de `min_trades` trades.
    Indeterminate,
    /// Baseline et adverse sont positifs.
    PassesCosts,
    /// Baseline et adverse non positifs, maker positif et rempli surtout en maker.
    FillDependent,
    /// Baseline positif, adverse non positif.
    CostFragile,
    /// Tout le reste.
    Negative,
}

impl ProfileLabel {
    /// Rendu canonique, identique à la forme sérialisée.
    pub const fn as_str(self) -> &'static str {
        match self {
            ProfileLabel::Indeterminate => "indeterminate",
            ProfileLabel::PassesCosts => "passes_costs",
            ProfileLabel::FillDependent => "fill_dependent",
            ProfileLabel::CostFragile => "cost_fragile",
            ProfileLabel::Negative => "negative",
        }
    }

    /// Libellé français.
    pub const fn fr(self) -> &'static str {
        match self {
            ProfileLabel::Indeterminate => "indéterminé",
            ProfileLabel::PassesCosts => "tient les coûts",
            ProfileLabel::FillDependent => "dépend du fill",
            ProfileLabel::CostFragile => "fragile aux coûts",
            ProfileLabel::Negative => "négatif",
        }
    }
}

/// Statut du profil maker, séparé de l'étiquette (docs/specs/s10.md §12.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MakerStatus {
    /// Moins de `min_trades` trades.
    TooFewTrades,
    /// Espérance nette ≤ 0 (ou indéfinie).
    NonPositive,
    /// Espérance positive, mais `maker_entry_maker_share < min_maker_entry_share`.
    PositiveMostlyTaker,
    /// Espérance positive, y compris quand la part maker est indéfinie (aucune entrée convertie : la
    /// stratégie n'émet que des limits).
    Positive,
}

/// Statut du profil maker.
///
/// `share` est `maker_entry_maker_share` (`None` si aucune entrée convertie n'a abouti) et `min_share`
/// le seuil `min_maker_entry_share` du fichier de coûts.
pub fn maker_status(
    n_trades: u64,
    min_trades: u32,
    expectancy_net: Option<f64>,
    share: Option<f64>,
    min_share: f64,
) -> MakerStatus {
    if n_trades < u64::from(min_trades) {
        return MakerStatus::TooFewTrades;
    }
    if !expectancy_net.is_some_and(|e| e > 0.0) {
        return MakerStatus::NonPositive;
    }
    match share {
        Some(share) if share < min_share => MakerStatus::PositiveMostlyTaker,
        _ => MakerStatus::Positive,
    }
}

/// Étiquette, décidée dans l'ordre du tableau de docs/specs/s10.md §12.4, sur baseline et adverse seuls ;
/// le statut maker n'intervient qu'à la troisième ligne.
///
/// `enough_trades` vaut `true` si baseline **et** adverse ont au moins `min_trades` trades.
pub fn decide_label(
    enough_trades: bool,
    baseline_positive: bool,
    adverse_positive: bool,
    maker: MakerStatus,
) -> ProfileLabel {
    if !enough_trades {
        ProfileLabel::Indeterminate
    } else if baseline_positive && adverse_positive {
        ProfileLabel::PassesCosts
    } else if !baseline_positive && !adverse_positive && maker == MakerStatus::Positive {
        ProfileLabel::FillDependent
    } else if baseline_positive && !adverse_positive {
        ProfileLabel::CostFragile
    } else {
        ProfileLabel::Negative
    }
}

/// Une ligne de `profiles[]`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfileSummary {
    pub name: CostProfileName,
    /// Sous-dossier du profil : son nom.
    pub dir: &'static str,
    /// `gate` (baseline, adverse) ou `informative` (maker).
    pub role: &'static str,
    pub run_id: String,
    pub config_hash: String,
    pub result_hash: String,
    pub verdict: &'static str,
    pub n_trades: u64,
    pub expectancy_net: Option<f64>,
    pub expectancy_r: Option<f64>,
    pub pnl_net: f64,
    pub round_trip_cost_rate_mean: Option<f64>,
    pub cost_r_mean: Option<f64>,
    pub cost_r_pass: Option<bool>,
    pub cost_third_pass: bool,
    pub gross_median_rate: Option<f64>,
    /// `n_trades ≥ min_trades` et `expectancy_net > 0`.
    pub positive: bool,
}

/// Bloc `maker` : statut séparé et part des entrées remplies en maker.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MakerSummary {
    pub status: MakerStatus,
    pub n_trades: u64,
    pub expectancy_net: Option<f64>,
    pub entries_converted: u64,
    pub entries_taken_at_arrival: u64,
    pub entries_filled_maker: u64,
    pub maker_entry_maker_share: Option<f64>,
    pub min_maker_entry_share: f64,
}

/// Portes croisées : les éléments de G1, sans verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CostRGates {
    pub baseline: Option<bool>,
    pub adverse: Option<bool>,
}

/// Porte « 1/3 » de chaque profil de porte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CostThirdGates {
    pub baseline: bool,
    pub adverse: bool,
}

/// Bloc `gates`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfilesGates {
    pub cost_r: CostRGates,
    pub cost_third: CostThirdGates,
    /// Porte de docs/04 §3.3 (« gain brut médian requis ») : `gross_median_rate` de **baseline** ≥
    /// 2 × `round_trip_cost_rate_mean` de **baseline**. `null` si l'une des deux valeurs est indéfinie.
    pub gross_median_vs_2x_baseline_cost: Option<bool>,
    pub both_gate_profiles_positive: bool,
}

/// Contenu de `profiles.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfilesReport {
    pub schema: &'static str,
    /// [`profiles_id`] des trois `run_id`.
    pub profiles_id: String,
    pub dataset_hash: String,
    pub initial_cash: f64,
    pub seed: u64,
    pub fee_grid: FeeGridId,
    pub costs_file_hash: String,
    pub cost_measures_hash: Option<String>,
    pub stress_calibrated: bool,
    /// Baseline, adverse, maker, dans cet ordre.
    pub profiles: Vec<ProfileSummary>,
    pub maker: MakerSummary,
    pub gates: ProfilesGates,
    pub label: ProfileLabel,
    pub label_fr: &'static str,
}

/// Un profil rendu, tel que [`ProfilesReport::build`] le lit.
#[derive(Clone, Copy, Debug)]
pub struct ProfileInput<'a> {
    pub metrics: &'a MetricsS9,
    /// `result_hash` du profil (`blake3:` + BLAKE3 de son `trades.csv`).
    pub result_hash: &'a str,
}

/// Entrées de [`ProfilesReport::build`].
#[derive(Clone, Copy, Debug)]
pub struct ProfilesInput<'a> {
    pub baseline: ProfileInput<'a>,
    pub adverse: ProfileInput<'a>,
    pub maker: ProfileInput<'a>,
    /// `min_maker_entry_share` de `config/costs.toml` : il ne change que l'étiquette, et n'est donc pas
    /// dans le `config_hash`.
    pub min_maker_entry_share: f64,
}

fn series_error(detail: String) -> ReportError {
    ReportError::Series { detail }
}

/// Résumé d'un profil ; refuse des métriques qui ne sont pas celles du profil attendu.
fn summarize(
    input: ProfileInput<'_>,
    expected: CostProfileName,
) -> Result<ProfileSummary, ReportError> {
    let m = input.metrics;
    let (Some(costs), Some(arithmetic)) = (m.costs.as_ref(), m.cost_arithmetic.as_ref()) else {
        return Err(series_error(format!(
            "les métriques du profil `{expected}` n'ont pas de blocs `costs` et `cost_arithmetic` : \
             ce ne sont pas celles d'un run s10"
        )));
    };
    if costs.profile != expected {
        return Err(series_error(format!(
            "les métriques du profil `{}` sont données à la place de celles du profil `{expected}`",
            costs.profile
        )));
    }
    Ok(ProfileSummary {
        name: expected,
        dir: expected.as_str(),
        role: if expected.gates() {
            "gate"
        } else {
            "informative"
        },
        run_id: m.run_id(),
        config_hash: m.config_hash.clone(),
        result_hash: input.result_hash.to_string(),
        verdict: m.verdict,
        n_trades: m.n_trades,
        expectancy_net: m.expectancy_net,
        expectancy_r: m.expectancy_r,
        pnl_net: m.pnl_net,
        round_trip_cost_rate_mean: arithmetic.round_trip_cost_rate_mean,
        cost_r_mean: arithmetic.cost_r_mean,
        cost_r_pass: arithmetic.cost_r_pass,
        cost_third_pass: arithmetic.cost_third_pass,
        gross_median_rate: arithmetic.gross_median_rate,
        positive: m.n_trades >= u64::from(m.min_trades)
            && m.expectancy_net.is_some_and(|e| e > 0.0),
    })
}

impl ProfilesReport {
    /// Construit `profiles.json` à partir des trois profils rendus.
    ///
    /// Refuse des profils qui ne décrivent pas le même run (données, capital, graine, grille, fichiers de
    /// coûts, seuil `min_trades`) : leur comparaison n'aurait pas de sens.
    pub fn build(input: &ProfilesInput<'_>) -> Result<ProfilesReport, ReportError> {
        let base = summarize(input.baseline, CostProfileName::Baseline)?;
        let adverse = summarize(input.adverse, CostProfileName::Adverse)?;
        let maker = summarize(input.maker, CostProfileName::Maker)?;

        let (mb, ma, mm) = (
            input.baseline.metrics,
            input.adverse.metrics,
            input.maker.metrics,
        );
        let (Some(cb), Some(ca), Some(cm)) =
            (mb.costs.as_ref(), ma.costs.as_ref(), mm.costs.as_ref())
        else {
            return Err(series_error("blocs `costs` absents".to_string()));
        };
        for (name, other, costs) in [("adverse", ma, ca), ("maker", mm, cm)] {
            let same = other.dataset_hash == mb.dataset_hash
                && other.config.initial_cash.to_bits() == mb.config.initial_cash.to_bits()
                && costs.seed == cb.seed
                && costs.fee_grid == cb.fee_grid
                && costs.costs_file_hash == cb.costs_file_hash
                && costs.cost_measures_hash == cb.cost_measures_hash
                && costs.stress_calibrated == cb.stress_calibrated
                && other.min_trades == mb.min_trades
                && other.data.window == mb.data.window;
            if !same {
                return Err(series_error(format!(
                    "le profil `{name}` ne partage pas les données, le capital, la fenêtre, la graine, \
                     la grille, les fichiers de coûts ou `min_trades` du profil baseline"
                )));
            }
        }

        let maker_counters = mm.cost_counters.as_ref().ok_or_else(|| {
            series_error("le profil maker n'a pas de bloc `cost_counters`".to_string())
        })?;
        let share = maker_counters.maker_entry_maker_share;
        let status = maker_status(
            maker.n_trades,
            mm.min_trades,
            maker.expectancy_net,
            share,
            input.min_maker_entry_share,
        );
        let enough = base.n_trades >= u64::from(mb.min_trades)
            && adverse.n_trades >= u64::from(ma.min_trades);
        let label = decide_label(enough, base.positive, adverse.positive, status);

        let gates = ProfilesGates {
            cost_r: CostRGates {
                baseline: base.cost_r_pass,
                adverse: adverse.cost_r_pass,
            },
            cost_third: CostThirdGates {
                baseline: base.cost_third_pass,
                adverse: adverse.cost_third_pass,
            },
            gross_median_vs_2x_baseline_cost: match (
                base.gross_median_rate,
                base.round_trip_cost_rate_mean,
            ) {
                (Some(gross), Some(cost)) => Some(gross >= 2.0 * cost),
                _ => None,
            },
            both_gate_profiles_positive: base.positive && adverse.positive,
        };
        let maker_summary = MakerSummary {
            status,
            n_trades: maker.n_trades,
            expectancy_net: maker.expectancy_net,
            entries_converted: maker_counters.total.maker_entries_converted,
            entries_taken_at_arrival: maker_counters.total.maker_entries_taken_at_arrival,
            entries_filled_maker: maker_counters.total.maker_entries_filled_maker,
            maker_entry_maker_share: share,
            min_maker_entry_share: input.min_maker_entry_share,
        };
        Ok(ProfilesReport {
            schema: PROFILES_SCHEMA,
            profiles_id: profiles_id(&base.run_id, &adverse.run_id, &maker.run_id),
            dataset_hash: mb.dataset_hash.clone(),
            initial_cash: mb.config.initial_cash,
            seed: cb.seed,
            fee_grid: cb.fee_grid,
            costs_file_hash: cb.costs_file_hash.clone(),
            cost_measures_hash: cb.cost_measures_hash.clone(),
            stress_calibrated: cb.stress_calibrated,
            profiles: vec![base, adverse, maker],
            maker: maker_summary,
            gates,
            label,
            label_fr: label.fr(),
        })
    }

    /// Octets de `profiles.json` : JSON canonique suivi d'un `\n`.
    pub fn bytes(&self) -> Result<Vec<u8>, ReportError> {
        super::json_bytes(self)
    }

    /// Écrit `profiles.json` dans `dir`, de façon atomique. À appeler en dernier, après les trois
    /// manifestes.
    pub fn write(&self, dir: &Path) -> Result<PathBuf, ReportError> {
        super::write_json_atomic(dir, PROFILES_FILE, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_tableau_d_etiquettes_est_decide_dans_l_ordre() {
        use MakerStatus::{NonPositive, Positive, PositiveMostlyTaker, TooFewTrades};
        use ProfileLabel::{CostFragile, FillDependent, Indeterminate, Negative, PassesCosts};
        // 1. trop peu de trades sur baseline ou adverse : indéterminé, quoi que fasse le reste.
        assert_eq!(decide_label(false, true, true, Positive), Indeterminate);
        assert_eq!(decide_label(false, false, false, Positive), Indeterminate);
        // 2. baseline et adverse positifs : le maker n'y change rien.
        for maker in [TooFewTrades, NonPositive, PositiveMostlyTaker, Positive] {
            assert_eq!(decide_label(true, true, true, maker), PassesCosts);
        }
        // 3. aucun des deux positif, maker positif rempli en maker.
        assert_eq!(decide_label(true, false, false, Positive), FillDependent);
        // maker positif mais pris en taker : pas « dépend du fill ».
        assert_eq!(
            decide_label(true, false, false, PositiveMostlyTaker),
            Negative
        );
        assert_eq!(decide_label(true, false, false, TooFewTrades), Negative);
        assert_eq!(decide_label(true, false, false, NonPositive), Negative);
        // 4. baseline positif, adverse non.
        assert_eq!(decide_label(true, true, false, Positive), CostFragile);
        // 5. adverse positif sans baseline : « sinon ».
        assert_eq!(decide_label(true, false, true, Positive), Negative);
    }

    #[test]
    fn les_quatre_statuts_maker() {
        assert_eq!(
            maker_status(29, 30, Some(1.0), Some(0.9), 0.5),
            MakerStatus::TooFewTrades
        );
        assert_eq!(
            maker_status(30, 30, Some(0.0), Some(0.9), 0.5),
            MakerStatus::NonPositive
        );
        assert_eq!(
            maker_status(30, 30, None, Some(0.9), 0.5),
            MakerStatus::NonPositive
        );
        assert_eq!(
            maker_status(30, 30, Some(0.1), Some(0.49), 0.5),
            MakerStatus::PositiveMostlyTaker
        );
        assert_eq!(
            maker_status(30, 30, Some(0.1), Some(0.5), 0.5),
            MakerStatus::Positive
        );
        assert_eq!(
            maker_status(30, 30, Some(0.1), None, 0.5),
            MakerStatus::Positive,
            "aucune entrée convertie : la part est indéfinie, le statut reste positif"
        );
    }

    #[test]
    fn les_libelles_francais_sont_ceux_de_la_spec() {
        assert_eq!(ProfileLabel::FillDependent.fr(), "dépend du fill");
        assert_eq!(ProfileLabel::PassesCosts.fr(), "tient les coûts");
        assert_eq!(ProfileLabel::CostFragile.fr(), "fragile aux coûts");
        assert_eq!(ProfileLabel::Indeterminate.fr(), "indéterminé");
        assert_eq!(ProfileLabel::Negative.fr(), "négatif");
        let json = hash::canonical_json(&ProfileLabel::FillDependent).unwrap();
        assert_eq!(json, "\"fill_dependent\"");
        assert_eq!(ProfileLabel::FillDependent.as_str(), "fill_dependent");
    }
}
