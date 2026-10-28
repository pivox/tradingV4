//! Exécutabilité de la liste live (G0) et test G1 « au moins 95 % des trades exécutables au capital
//! réel » (docs S9 §10).
//!
//! Le calcul se fait après le run, hors de la boucle, à partir de ce que le moteur a enregistré
//! (fractions de capital demandées par les entrées, compteurs par paire) et des colonnes 1m du dataset.
//! Rien ici ne lit le fichier d'instruments ni la ligne de commande : l'appelant passe les minimums et
//! l'état OKX de chaque paire ([`OkxPair`]).
//!
//! # Règle G0 (docs/04 §5 G0, §3.1 point 4)
//!
//! Pour chaque paire de l'univers :
//!
//! 1. notional de référence au palier 100 % = 300 × médiane basse des fractions de capital demandées
//!    par les entrées du run, bornée par 300 (cash du palier 100 %, sans levier), dans les deux modes ;
//!    le N de `max_positions` du run n'y intervient pas : le live au palier 100 % n'a pas de plafond
//!    capital / N par ligne ;
//! 2. volume quote 1m médian = médiane basse de `volume × low` sur **toutes les minutes calendaires**
//!    de la fenêtre jouée, les minutes absentes comptant pour 0 (une minute absente ne permet aucun
//!    fill, docs/04 §10 décision 8) ;
//! 3. tests, dans l'ordre : (a) `0,005 × médiane ≥ notional`, sinon `rejected_volume` ; (b) `notional ≥
//!    minSz × close médian`, sinon `rejected_min_size` ; (c) état de l'instrument `live`, sinon
//!    `rejected_state`.
//!
//! Une paire sans entrée prend le verdict `indeterminate`, jamais `executable`. Sans règles OKX
//! (minimums lus dans le TOML de S6), le verdict est `not_applicable`.
//!
//! # Test G1 (docs S9 §10.2)
//!
//! `fills / (fills + rejets volume + rejets min_notional + rejets lot_step) ≥ 0,95`. Les rejets de
//! cash et de `max_positions` sont des contraintes de portefeuille : publiés à côté, non comptés.
//!
//! # Écart d'implémentation
//!
//! Les médianes sont des médianes **basses** (élément d'indice `(n − 1) / 2` de la série triée) :
//! elles rendent toujours une valeur observée, sans moyenne de deux valeurs voisines, et la médiane
//! d'une série dont plus de la moitié des minutes sont absentes vaut exactement 0.

use serde::Serialize;

use crate::model::Ts;

/// Capital du palier 100 % (décision du 13/10, docs/04 §5).
pub const REFERENCE_CAPITAL: f64 = 300.0;
/// Part du volume quote 1m qu'un ordre peut prendre (docs/04 §3.1 point 4).
pub const VOLUME_SHARE: f64 = 0.005;
/// Seuil de G1.
pub const G1_THRESHOLD: f64 = 0.95;
/// Nom de la règle, écrit dans `metrics.json`.
pub const RULE: &str = "g0_volume_then_min_size_then_state";
/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = 60_000;

/// Colonnes 1m d'une paire (le dataset entier : la fenêtre jouée est découpée ici).
#[derive(Clone, Copy, Debug)]
pub struct LegMarket<'a> {
    pub open_time_ms: &'a [Ts],
    pub low: &'a [f64],
    pub close: &'a [f64],
    pub volume: &'a [f64],
}

/// Minimums et état OKX de l'instrument d'une paire.
#[derive(Clone, Debug, PartialEq)]
pub struct OkxPair {
    /// Identifiant OKX (`BTC-USDC`).
    pub inst_id: String,
    /// Taille minimale d'un ordre, en monnaie de base (`minSz`).
    pub min_sz: f64,
    /// État de l'instrument (`live`, `suspend`, ...).
    pub state: String,
}

/// Ce que le calcul lit pour une paire.
#[derive(Clone, Debug)]
pub struct PairInput<'a> {
    pub symbol: &'a str,
    /// `None` sans fichier OKX.
    pub okx: Option<OkxPair>,
    /// Ordres d'entrée acceptés (affichage).
    pub entry_orders: u64,
    /// Fractions de capital demandées par les entrées (`LegResult::entry_fractions`).
    pub entry_fractions: &'a [f64],
    pub market: LegMarket<'a>,
}

/// Entrées du calcul G0.
#[derive(Clone, Debug)]
pub struct ExecutabilityInput<'a> {
    pub pairs: Vec<PairInput<'a>>,
    /// Première et dernière `open_time` de la fenêtre jouée.
    pub window_first_open_ms: Ts,
    pub window_last_open_ms: Ts,
    /// Vrai si les minimums viennent d'un fichier d'instruments OKX.
    pub okx_rules: bool,
    /// Vrai si les données sont celles du lieu d'exécution : dataset OKX dont chaque symbole est
    /// l'instrument USDC d'exécution.
    pub venue_matches_execution: bool,
}

/// Verdict G0 d'une paire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairVerdict {
    Executable,
    RejectedVolume,
    RejectedMinSize,
    RejectedState,
    /// Aucune entrée dans le run : le notional de référence n'existe pas.
    Indeterminate,
    /// Pas de minimums OKX : l'exécutabilité ne se juge pas.
    NotApplicable,
}

impl PairVerdict {
    /// Valeur écrite dans `metrics.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            PairVerdict::Executable => "executable",
            PairVerdict::RejectedVolume => "rejected_volume",
            PairVerdict::RejectedMinSize => "rejected_min_size",
            PairVerdict::RejectedState => "rejected_state",
            PairVerdict::Indeterminate => "indeterminate",
            PairVerdict::NotApplicable => "not_applicable",
        }
    }

    fn is_rejection(self) -> bool {
        matches!(
            self,
            PairVerdict::RejectedVolume | PairVerdict::RejectedMinSize | PairVerdict::RejectedState
        )
    }
}

/// Une ligne de `executability.pairs[]`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PairReport {
    pub symbol: String,
    pub okx_inst_id: Option<String>,
    pub entry_orders: u64,
    /// 300 × médiane basse des fractions demandées, bornée par 300 (cash du palier 100 %).
    pub notional_ref: Option<f64>,
    pub median_quote_volume_1m: Option<f64>,
    /// `0,005 × median_quote_volume_1m`.
    pub volume_cap: Option<f64>,
    pub min_sz: Option<f64>,
    pub median_close: Option<f64>,
    /// `min_sz × median_close`.
    pub min_order_notional: Option<f64>,
    pub state: Option<String>,
    pub verdict: PairVerdict,
}

/// Une paire écartée de la liste live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Rejection {
    pub symbol: String,
    pub reason: &'static str,
}

/// Bloc `executability` de `metrics.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Executability {
    pub reference_capital: f64,
    pub rule: &'static str,
    pub venue_matches_execution: bool,
    pub pairs: Vec<PairReport>,
    /// Paires `executable`, dans l'ordre de l'univers.
    pub live_list: Vec<String>,
    pub rejected: Vec<Rejection>,
}

/// Médiane basse (indice `(n − 1) / 2`) d'une série, réordonnée sur place ; `None` si elle est vide.
fn lower_median_in_place(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let index = (values.len() - 1) / 2;
    let (_, value, _) = values.select_nth_unstable_by(index, f64::total_cmp);
    Some(*value)
}

/// Médiane basse de `volume × low` sur `total` minutes calendaires dont `present.len()` ont une
/// bougie : les `total − present` minutes absentes valent 0 et sont placées en tête sans être
/// matérialisées. `present` est réordonné.
fn lower_median_with_zeros(present: &mut [f64], total: usize) -> Option<f64> {
    let total = total.max(present.len());
    if total == 0 {
        return None;
    }
    let absent = total - present.len();
    let index = (total - 1) / 2;
    if index < absent {
        return Some(0.0);
    }
    let (_, value, _) = present.select_nth_unstable_by(index - absent, f64::total_cmp);
    Some(*value)
}

/// Calcule le bloc `executability`.
pub fn compute(input: &ExecutabilityInput<'_>) -> Executability {
    let total_minutes = usize::try_from(
        (input.window_last_open_ms - input.window_first_open_ms).div_euclid(MINUTE_MS) + 1,
    )
    .unwrap_or(0);
    // Tampons réutilisés d'une paire à l'autre.
    let mut quote_volumes: Vec<f64> = Vec::new();
    let mut closes: Vec<f64> = Vec::new();
    let mut fractions: Vec<f64> = Vec::new();

    let mut pairs = Vec::with_capacity(input.pairs.len());
    let mut live_list = Vec::new();
    let mut rejected = Vec::new();
    for pair in &input.pairs {
        let m = &pair.market;
        let from = m
            .open_time_ms
            .partition_point(|&t| t < input.window_first_open_ms);
        let to = m
            .open_time_ms
            .partition_point(|&t| t <= input.window_last_open_ms);
        quote_volumes.clear();
        closes.clear();
        for i in from..to {
            if let (Some(&volume), Some(&low), Some(&close)) =
                (m.volume.get(i), m.low.get(i), m.close.get(i))
            {
                quote_volumes.push(volume * low);
                closes.push(close);
            }
        }
        let median_quote_volume = lower_median_with_zeros(&mut quote_volumes, total_minutes);
        let median_close = lower_median_in_place(&mut closes);

        fractions.clear();
        fractions.extend_from_slice(pair.entry_fractions);
        // Au palier 100 % le live n'a pas de plafond capital / N par ligne : le notional est borné par
        // le cash du palier (sans levier), identique en recherche et en capitalisé.
        let notional_ref = lower_median_in_place(&mut fractions)
            .map(|fraction| (REFERENCE_CAPITAL * fraction).min(REFERENCE_CAPITAL));

        let volume_cap = median_quote_volume.map(|median| VOLUME_SHARE * median);
        let min_sz = pair.okx.as_ref().map(|okx| okx.min_sz);
        let min_order_notional = match (min_sz, median_close) {
            (Some(min_sz), Some(close)) => Some(min_sz * close),
            _ => None,
        };
        let verdict = if !input.okx_rules {
            PairVerdict::NotApplicable
        } else {
            match (&pair.okx, notional_ref, volume_cap, min_order_notional) {
                (Some(okx), Some(notional), Some(cap), Some(min_order)) => {
                    if cap < notional {
                        PairVerdict::RejectedVolume
                    } else if notional < min_order {
                        PairVerdict::RejectedMinSize
                    } else if okx.state != "live" {
                        PairVerdict::RejectedState
                    } else {
                        PairVerdict::Executable
                    }
                }
                _ => PairVerdict::Indeterminate,
            }
        };
        if verdict == PairVerdict::Executable {
            live_list.push(pair.symbol.to_string());
        }
        if verdict.is_rejection() {
            rejected.push(Rejection {
                symbol: pair.symbol.to_string(),
                reason: verdict.as_str(),
            });
        }
        pairs.push(PairReport {
            symbol: pair.symbol.to_string(),
            okx_inst_id: pair.okx.as_ref().map(|okx| okx.inst_id.clone()),
            entry_orders: pair.entry_orders,
            notional_ref,
            median_quote_volume_1m: median_quote_volume,
            volume_cap,
            min_sz,
            median_close,
            min_order_notional,
            state: pair.okx.as_ref().map(|okx| okx.state.clone()),
            verdict,
        });
    }
    Executability {
        reference_capital: REFERENCE_CAPITAL,
        rule: RULE,
        venue_matches_execution: input.venue_matches_execution,
        pairs,
        live_list,
        rejected,
    }
}

/// Compteurs de G1, sommés sur les paires.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct G1Counts {
    pub fills: u64,
    pub rejected_volume: u64,
    pub rejected_min_notional: u64,
    pub rejected_lot_step: u64,
    pub rejected_cash: u64,
    pub rejected_max_positions: u64,
}

/// Bloc `g1_executable` de `metrics.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct G1Executable {
    pub fills: u64,
    pub rejected_volume: u64,
    pub rejected_min_notional: u64,
    pub rejected_lot_step: u64,
    pub rejected_cash: u64,
    pub rejected_max_positions: u64,
    /// `fills / (fills + volume + min_notional + lot_step)` ; `null` si le dénominateur est nul.
    pub ratio: Option<f64>,
    pub threshold: f64,
    /// `pass`, `fail`, `no_data` ou `not_applicable`.
    pub verdict: &'static str,
    /// Vrai si le run est au capital de référence (300) : G1 ne se lit que dans ce cas.
    pub at_reference_capital: bool,
}

/// Calcule le bloc `g1_executable`. `okx_rules` dit si les minimums viennent d'un fichier OKX.
pub fn g1(counts: G1Counts, initial_cash: f64, okx_rules: bool) -> G1Executable {
    let denominator = counts.fills
        + counts.rejected_volume
        + counts.rejected_min_notional
        + counts.rejected_lot_step;
    let ratio = (denominator > 0).then(|| counts.fills as f64 / denominator as f64);
    let verdict = if !okx_rules {
        "not_applicable"
    } else {
        match ratio {
            None => "no_data",
            Some(r) if r >= G1_THRESHOLD => "pass",
            Some(_) => "fail",
        }
    };
    G1Executable {
        fills: counts.fills,
        rejected_volume: counts.rejected_volume,
        rejected_min_notional: counts.rejected_min_notional,
        rejected_lot_step: counts.rejected_lot_step,
        rejected_cash: counts.rejected_cash,
        rejected_max_positions: counts.rejected_max_positions,
        ratio,
        threshold: G1_THRESHOLD,
        verdict,
        at_reference_capital: initial_cash == REFERENCE_CAPITAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mediane_basse_d_une_serie_paire_et_impaire() {
        assert_eq!(lower_median_in_place(&mut []), None);
        assert_eq!(lower_median_in_place(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(lower_median_in_place(&mut [4.0, 1.0, 3.0, 2.0]), Some(2.0));
    }

    #[test]
    fn les_minutes_absentes_comptent_pour_zero() {
        // 3 minutes présentes (1, 2, 3) sur 5 : série triée 0, 0, 1, 2, 3, médiane 1.
        assert_eq!(lower_median_with_zeros(&mut [3.0, 1.0, 2.0], 5), Some(1.0));
        // 3 présentes sur 7 : 0, 0, 0, 0, 1, 2, 3, médiane (indice 3) = 0.
        assert_eq!(lower_median_with_zeros(&mut [3.0, 1.0, 2.0], 7), Some(0.0));
        // Aucune absente : médiane basse ordinaire.
        assert_eq!(
            lower_median_with_zeros(&mut [3.0, 1.0, 2.0, 4.0], 4),
            Some(2.0)
        );
        // Aucune minute.
        assert_eq!(lower_median_with_zeros(&mut [], 0), None);
    }

    #[test]
    fn g1_a_19_fills_et_un_rejet_de_volume_passe() {
        let counts = G1Counts {
            fills: 19,
            rejected_volume: 1,
            ..G1Counts::default()
        };
        let g = g1(counts, 300.0, true);
        assert_eq!(g.ratio, Some(0.95));
        assert_eq!(g.verdict, "pass");
        assert!(g.at_reference_capital);
    }

    #[test]
    fn g1_a_18_fills_et_deux_rejets_echoue() {
        let counts = G1Counts {
            fills: 18,
            rejected_volume: 1,
            rejected_lot_step: 1,
            ..G1Counts::default()
        };
        let g = g1(counts, 300.0, true);
        assert_eq!(g.ratio, Some(0.9));
        assert_eq!(g.verdict, "fail");
    }

    #[test]
    fn g1_ignore_cash_et_max_positions() {
        let counts = G1Counts {
            fills: 19,
            rejected_volume: 1,
            rejected_cash: 50,
            rejected_max_positions: 50,
            ..G1Counts::default()
        };
        assert_eq!(g1(counts, 300.0, true).verdict, "pass");
    }

    #[test]
    fn g1_sans_donnee_et_sans_regles_okx() {
        assert_eq!(g1(G1Counts::default(), 300.0, true).verdict, "no_data");
        assert_eq!(g1(G1Counts::default(), 300.0, true).ratio, None);
        let counts = G1Counts {
            fills: 1,
            ..G1Counts::default()
        };
        assert_eq!(g1(counts, 300.0, false).verdict, "not_applicable");
        assert!(!g1(counts, 3000.0, true).at_reference_capital);
    }
}
