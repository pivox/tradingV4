//! Paquet numérique S13, distinct des trois artefacts immuables de simulation.
//! Aucun hash du manifeste : le paquet est écrit avant lui et peut accompagner un ancien run.
use super::{stats, verify::VerifiedRun};
use crate::research::stats::{
    CashPoint, MarkPoint, MarketSeries, StatsSizing, TradeClosure, Window,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const STATS_INPUTS_FILE: &str = "stats-inputs.json";
pub const STATS_INPUTS_SCHEMA: &str = "tv4-stats-inputs-v1";
/// Version des horloges : clôtures disponibles au minuit final ; cash après événements à gauche.
pub const CONVENTIONS: &str = "utc-close-availability_cash-left_marks-close-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsInputsRef {
    pub file: String,
    pub schema: String,
    pub hash: String,
}
impl StatsInputsRef {
    pub fn validate(&self) -> Result<(), String> {
        if self.file != STATS_INPUTS_FILE
            || self.schema != STATS_INPUTS_SCHEMA
            || !valid_hash(&self.hash)
        {
            return Err("référence stats_inputs : nom fixe, schéma v1 et BLAKE3 requis".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationHashes {
    #[serde(rename = "metrics.json")]
    pub metrics_json: String,
    #[serde(rename = "trades.csv")]
    pub trades_csv: String,
    #[serde(rename = "equity.csv")]
    pub equity_csv: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsProvenance {
    pub run_id: String,
    pub config_hash: String,
    pub dataset_hash: String,
    pub result_hash: String,
    pub initial_cash: f64,
    pub window: Window,
    pub profile: Option<String>,
    pub line: Option<String>,
    pub files: SimulationHashes,
}
impl StatsProvenance {
    pub fn validate(&self) -> Result<(), String> {
        validate_window(self.window)?;
        if !valid_hex(&self.run_id)
            || !valid_hex(&self.config_hash)
            || !valid_hash(&self.dataset_hash)
            || !self.initial_cash.is_finite()
            || self.initial_cash <= 0.
        {
            return Err("provenance : run_id/config_hash 64 hex minuscules, dataset_hash BLAKE3 et capital positif fini requis".into());
        }
        if [
            &self.result_hash,
            &self.files.metrics_json,
            &self.files.trades_csv,
            &self.files.equity_csv,
        ]
        .into_iter()
        .any(|h| !valid_hash(h))
            || self.result_hash != self.files.trades_csv
        {
            return Err("provenance : empreintes BLAKE3 de simulation incohérentes".into());
        }
        if self
            .profile
            .as_ref()
            .is_some_and(|v| !["baseline", "adverse", "maker"].contains(&v.as_str()))
            || self
                .line
                .as_ref()
                .is_some_and(|v| !["full", "okx_usdc"].contains(&v.as_str()))
        {
            return Err("provenance : profil ou ligne inconnu".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsInputs {
    pub schema: String,
    pub conventions: String,
    pub provenance: StatsProvenance,
    /// Univers joué, distinct des marchés chargés uniquement comme contexte.
    pub pair_symbols: Vec<String>,
    pub sizing: StatsSizing,
    pub decision_timeframe_minutes: u32,
    /// Une correspondance pour chaque ligne du journal, y compris les lignes purgées.
    pub closures: Vec<TradeClosure>,
    /// Points explicites uniquement : l'absence d'une ancre ou d'un minuit reste un trou.
    pub marks: Vec<MarkPoint>,
    pub cash: Vec<CashPoint>,
    pub markets: Vec<MarketSeries>,
}

/// Le lecteur retient les octets exacts utilisés pour l'empreinte, sans seconde lecture.
#[derive(Clone, Debug)]
pub struct LoadedStatsInputs {
    pub bytes: Vec<u8>,
    pub inputs: StatsInputs,
}
impl LoadedStatsInputs {
    pub fn from_bytes(bytes: Vec<u8>, run: &VerifiedRun) -> Result<Self, String> {
        let inputs: StatsInputs =
            serde_json::from_slice(&bytes).map_err(|e| format!("paquet statistique JSON : {e}"))?;
        inputs.validate_for(run)?;
        Ok(Self { bytes, inputs })
    }
}

fn valid_hex(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn valid_hash(hash: &str) -> bool {
    hash.strip_prefix("blake3:").is_some_and(valid_hex)
}

pub(crate) fn validate_window(window: Window) -> Result<(), String> {
    if !(0..crate::store::time::MAX_TS_MS).contains(&window.start_ms)
        || window.end_ms >= crate::store::time::MAX_TS_MS
        || window.end_ms <= window.start_ms
    {
        return Err("fenêtre statistique invalide".into());
    }
    Ok(())
}
fn unique_times(points: impl Iterator<Item = i64>) -> bool {
    let mut previous = None;
    for at in points {
        if !(0..crate::store::time::MAX_TS_MS).contains(&at) || previous.is_some_and(|p| p >= at) {
            return false;
        }
        previous = Some(at);
    }
    true
}
impl StatsInputs {
    /// Contrôles purs utilisables avant écriture et après désérialisation.
    pub fn validate(&self) -> Result<(), String> {
        self.provenance.validate()?;
        if self.schema != STATS_INPUTS_SCHEMA || self.conventions != CONVENTIONS {
            return Err("schéma ou conventions du paquet statistique inconnus".into());
        }
        if !crate::model::Timeframe::ALL
            .iter()
            .any(|tf| tf.ms() / 60_000 == i64::from(self.decision_timeframe_minutes))
        {
            return Err("timeframe de décision non supporté".into());
        }
        let symbols: BTreeSet<_> = self.pair_symbols.iter().collect();
        if symbols.is_empty()
            || symbols.len() != self.pair_symbols.len()
            || symbols.iter().any(|s| s.is_empty())
        {
            return Err("univers joué vide ou dupliqué".into());
        }
        let w = self.provenance.window;
        if !unique_times(self.marks.iter().map(|p| p.at_ms))
            || self.marks.iter().any(|p| {
                p.at_ms < w.start_ms
                    || p.at_ms > w.end_ms
                    || !p.equity.is_finite()
                    || p.equity < 0.
                    || (p.day_close && p.at_ms.rem_euclid(86_400_000) != 0)
            })
        {
            return Err("marquage : horloge, montant ou clôture UTC invalide".into());
        }
        let mut zero = false;
        for p in &self.marks {
            if zero && p.equity > 0. {
                return Err("marquage source : résurrection après zéro".into());
            }
            zero |= p.equity == 0.;
        }
        if self
            .marks
            .first()
            .is_some_and(|p| p.at_ms == w.start_ms && p.equity != self.provenance.initial_cash)
        {
            return Err("marquage initial différent du capital".into());
        }
        if !unique_times(self.cash.iter().map(|p| p.at_ms))
            || self.cash.iter().any(|p| {
                p.at_ms < w.start_ms || p.at_ms > w.end_ms || !p.cash.is_finite() || p.cash < 0.
            })
            || self.cash.first().is_some_and(|p| p.at_ms != w.start_ms)
        {
            return Err("cash : trace gauche finie, positive et ancrée au début requise".into());
        }
        let mut rows = BTreeSet::new();
        if self
            .closures
            .iter()
            .any(|p| !rows.insert(p.row) || p.at_ms < w.start_ms || p.at_ms > w.end_ms)
        {
            return Err("table de réalisation : row dupliquée ou instant hors fenêtre".into());
        }
        let mut market_symbols = BTreeSet::new();
        for market in &self.markets {
            if market.symbol.is_empty() || !market_symbols.insert(&market.symbol) {
                return Err("marché vide ou dupliqué".into());
            }
            for (points, daily) in [(&market.daily_closes, true), (&market.valuations, false)] {
                if !unique_times(points.iter().map(|p| p.at_ms))
                    || points.iter().any(|p| {
                        !p.close.is_finite()
                            || p.close <= 0.
                            || if daily {
                                p.at_ms.rem_euclid(86_400_000) != 0 || p.at_ms > w.end_ms
                            } else {
                                p.at_ms < w.start_ms
                                    || p.at_ms > w.end_ms
                                    || (p.at_ms != w.start_ms
                                        && p.at_ms != w.end_ms
                                        && p.at_ms.rem_euclid(86_400_000) != 0)
                            }
                    })
                {
                    return Err(
                        "prix : valeurs positives, instants uniques et grille UTC exacte requis"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    /// Admission d'un paquet externe sur l'image contrôlée des trois simulations.
    /// Les métadonnées historiques de gouvernance/moteur du manifeste ne sont pas dans ce contrat.
    pub fn validate_for(&self, run: &VerifiedRun) -> Result<(), String> {
        self.validate_against(&stats::snapshot(run)?)
    }

    /// Contrôle pur pour le producteur, avant publication du manifeste final.
    pub fn validate_against(&self, image: &stats::RunSnapshot) -> Result<(), String> {
        self.validate()?;
        let mut expected = image.provenance.clone();
        expected.initial_cash = self.provenance.initial_cash;
        if self.provenance != expected
            || !stats::capital_matches(self.provenance.initial_cash, image.provenance.initial_cash)
        {
            return Err("provenance du paquet différente du run (identité, capital, fenêtre, profil, ligne ou empreintes)".into());
        }
        if image
            .decision_timeframe_minutes
            .is_some_and(|tf| tf != self.decision_timeframe_minutes)
        {
            return Err("timeframe de décision différent du run".into());
        }
        if self
            .markets
            .iter()
            .any(|m| !image.market_symbols.contains(&m.symbol))
        {
            return Err("marché du paquet absent des séries admises du run".into());
        }
        if self.sizing != image.sizing || self.pair_symbols != image.pair_symbols {
            return Err("sizing ou univers joué différent du run".into());
        }
        if self.closures.len() != image.trades.len() {
            return Err("table de réalisation incomplète pour le journal".into());
        }
        let closures: BTreeMap<_, _> = self.closures.iter().map(|c| (c.row, c)).collect();
        let mark_times: BTreeSet<_> = self.marks.iter().map(|p| p.at_ms).collect();
        let terminal_rows: BTreeSet<_> = image.terminal_rows.iter().copied().collect();
        for trade in &image.trades {
            let closure = closures
                .get(&trade.row)
                .ok_or("row du journal absente de la table de réalisation")?;
            if terminal_rows.contains(&trade.row) && closure.at_ms != self.provenance.window.end_ms
            {
                return Err("liquidation terminale réalisée hors de la fin globale".into());
            }
            if closure.at_ms < trade.exit_ms {
                return Err("réalisation antérieure à la sortie économique".into());
            }
            if !mark_times.contains(&closure.at_ms) {
                return Err("réalisation sans marquage exact".into());
            }
        }
        let equity = image.equity_final;
        let cash = image.cash_final;
        if !equity.is_finite() || equity < 0. || !cash.is_finite() || cash < 0. {
            return Err("soldes finaux attendus non finis ou négatifs".into());
        }
        let mut ledger = self.provenance.initial_cash;
        for t in &image.trades {
            ledger += t.pnl_net;
        }
        if !stats::close(cash, ledger) {
            return Err(
                "cash final incohérent avec tous les flux du journal, purges incluses".into(),
            );
        }
        if self.marks.last().is_some_and(|p| {
            p.at_ms == self.provenance.window.end_ms && !stats::close(p.equity, equity)
        }) {
            return Err("marquage final différent de equity_final".into());
        }
        if self.cash.last().is_some_and(|p| {
            p.at_ms == self.provenance.window.end_ms && !stats::close(p.cash, cash)
        }) {
            return Err("cash terminal différent de cash_final".into());
        }
        Ok(())
    }
}
