//! Calibration de la composante de stress des stops (docs/specs/s10.md §11, docs/04 §3.1 point 3).
//!
//! # Ce que mesure la calibration
//!
//! Le simulateur applique à un stop, avec la probabilité `stress_probability`, un fill à
//! `stop × (1 − stress_drop)` (§5.2). Cette probabilité n'est pas devinée : [`calibrate`] la mesure
//! sur l'historique 1m, par des positions **synthétiques** :
//!
//! 1. 1m → `tf` par [`Mtf::build`], bougies **closes** seulement ;
//! 2. ATR14 de Wilder sur `tf` ([`Atr`]) ;
//! 3. à la clôture d'une bougie `tf` d'indice `j` où l'ATR est défini et où aucune position
//!    synthétique n'est ouverte, `E = close_j` et `S = E − k × ATR_j`. Le stop est actif à partir de
//!    `close_time_j + 60 000` ms, où `close_time_j = open_j + durée(tf)` est l'instant du signal
//!    (délai d'activation de 04 §3.1 point 1, [`ACTIVATION_DELAY_MS`]) ;
//! 4. la première 1m active à volume > 0 dont `low ≤ S` est le **déclenchement** ; sans déclenchement
//!    avant `close_time_j + horizon × durée(tf)`, la position **expire** ;
//! 5. stress `m1`, **causal** et retenu : `high_c − low_c > range_mult × ATR14_1m(c − 1)`, l'ATR de
//!    Wilder des 1m antérieures à `c` ;
//! 6. stress `tf`, informatif et non causal : le range complet de la bougie `tf` qui contient `c`
//!    dépasse `range_mult × ATR_j`.
//!
//! # Conventions précisées par l'implémentation
//!
//! - **Les 1m à volume nul ne comptent pas.** Elles ne déclenchent rien (décision 8 de 04 §10) et
//!   n'entrent pas dans l'ATR14 des 1m : leur range nul l'écraserait et gonflerait la part de stress.
//!   Elles sont sautées, et le TR de la 1m suivante à volume > 0 prend le close de la dernière 1m à
//!   volume > 0 (c'est ce que fait [`Atr`] quand on ne lui fournit pas la bougie). Elles participent
//!   en revanche à l'agrégation vers `tf`, comme dans le moteur ([`Mtf`]).
//! - **Non-chevauchement.** Une nouvelle position ne s'ouvre qu'à une clôture `tf` qui tombe à la
//!   fin de la 1m de déclenchement ou après (`close_time ≥ open_c + 60 000`), ou à la fin de la
//!   fenêtre d'une position expirée (`close_time ≥ close_time_j + horizon × durée(tf)`).
//! - **Position censurée.** Une position sans déclenchement dont la fenêtre dépasse la fin des
//!   données n'est ni expirée ni déclenchée : elle n'est pas comptée, et c'est la dernière de la
//!   série. Ainsi `positions = triggered + expired`.
//! - **ATR1m indéfini.** Tant que 14 minutes à volume > 0 n'ont pas été vues, l'ATR14 des 1m est
//!   indéfini : le déclenchement compte dans `triggered`, mais pas dans `stress_m1`.
//! - **Stop non positif.** Si `S ≤ 0`, le stop ne peut pas être touché : la position expire.
//! - **Stress `tf` d'une bougie non close.** Si la bougie `tf` qui contient `c` n'est pas close à la
//!   fin des données, son range est incomplet : elle ne compte pas dans `stress_tf`.
//! - **Profondeur.** Pour chaque déclenchement en stress `m1`, `(S − low_c) / S` et
//!   `(S − close_c) / S` (ce dernier est négatif quand la 1m reclôture au-dessus du stop). Médiane et
//!   p95 sont calculés ici ([`median`], [`p95_nearest_rank`]) : `research` ne dépend pas de `report`.
//!
//! # Sortie
//!
//! [`StressCalibration`] est `Serialize` et ne porte ni `created_at` ni `commit` : son
//! `measurement_hash` est le BLAKE3 du JSON canonique du reste, donc deux appels sur les mêmes
//! données donnent le même hash. Le fichier de sortie ajoute ces deux champs par [`StressFile`].
//! `tv4_version` fait partie de ce qui est haché (comme le prévoit §11.2).
//!
//! Le module ne dépend que de `model`, `mtf`, `indicators` et `store` (docs/specs/s10.md §15.1).

use serde::Serialize;
use thiserror::Error;

use crate::indicators::{Atr, Indicator, IndicatorError};
use crate::model::costs::{CostsFile, PairClass, base_asset};
use crate::model::hash::{blake3_hex, canonical_json};
use crate::model::{Candle, ModelError, Timeframe, Ts};
use crate::mtf::{Mtf, MtfError};
use crate::store::Columns;

/// Schéma du fichier de calibration (§11.2).
pub const STRESS_SCHEMA: &str = "tv4-stress-calibration-v1";

/// Délai d'activation d'un stop synthétique après la clôture de décision (04 §3.1 point 1).
pub const ACTIVATION_DELAY_MS: i64 = 60_000;

/// Période de l'ATR de Wilder, du TF de décision comme des 1m.
pub const ATR_PERIOD: usize = 14;

const MINUTE_MS: i64 = 60_000;

/// Paramètres de la calibration (`--tf`, `--k`, `--horizon-bars`, `--range-mult` de la CLI).
#[derive(Clone, Debug, PartialEq)]
pub struct StressParams {
    /// TF de décision étudiés, sans doublon et différents de 1m. Défaut : 1d puis 4h.
    pub tfs: Vec<Timeframe>,
    /// Multiples d'ATR du stop synthétique (la plage de 04 §3.3). Défaut : 1,5 ; 2 ; 3.
    pub k: Vec<f64>,
    /// Durée de vie d'une position, en bougies `tf`. Défaut : 30.
    pub horizon_bars: usize,
    /// Multiple d'ATR au-delà duquel une bougie est en stress. Défaut : 5.
    pub range_mult: f64,
}

impl Default for StressParams {
    fn default() -> Self {
        StressParams {
            tfs: vec![Timeframe::D1, Timeframe::H4],
            k: vec![1.5, 2.0, 3.0],
            horizon_bars: 30,
            range_mult: 5.0,
        }
    }
}

/// Paramètres tels qu'ils sont écrits dans le fichier (`params` de §11.2).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StressParamsRecord {
    pub activation_delay_ms: i64,
    pub atr_period: usize,
    pub horizon_bars: usize,
    pub k: Vec<f64>,
    pub range_mult: f64,
    pub tfs: Vec<Timeframe>,
}

impl StressParamsRecord {
    fn from_params(params: &StressParams) -> StressParamsRecord {
        StressParamsRecord {
            activation_delay_ms: ACTIVATION_DELAY_MS,
            atr_period: ATR_PERIOD,
            horizon_bars: params.horizon_bars,
            k: params.k.clone(),
            range_mult: params.range_mult,
            tfs: params.tfs.clone(),
        }
    }
}

/// Erreurs de la calibration.
#[derive(Debug, Error)]
pub enum StressError {
    #[error("paramètre de calibration `{field}` invalide : {reason}")]
    InvalidParams { field: &'static str, reason: String },
    #[error("aucune série à calibrer")]
    NoSeries,
    #[error("série `{symbol}` : aucune bougie 1m")]
    EmptySeries { symbol: String },
    #[error("série `{symbol}` fournie deux fois")]
    DuplicateSymbol { symbol: String },
    #[error("série `{symbol}` : {source}")]
    Mtf {
        symbol: String,
        #[source]
        source: MtfError,
    },
    #[error("ATR : {0}")]
    Indicator(#[from] IndicatorError),
    #[error(transparent)]
    Model(#[from] ModelError),
}

/// Classe de regroupement de la calibration : `major` si la base figure dans `classes.major` de
/// `config/costs.toml`, `alt` sinon (`top20` et `beyond` se confondent, §11.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StressClass {
    Major,
    Alt,
}

/// Médiane et p95 d'une série de profondeurs ; `None` quand elle est vide.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct DepthStat {
    pub median: Option<f64>,
    pub p95: Option<f64>,
}

impl DepthStat {
    fn of(values: &[f64]) -> DepthStat {
        DepthStat {
            median: median(values),
            p95: p95_nearest_rank(values),
        }
    }
}

/// Une ligne de `results` : une (série, TF de décision, k).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StressResult {
    pub symbol: String,
    pub class: StressClass,
    pub tf: Timeframe,
    pub k: f64,
    pub positions: usize,
    pub triggered: usize,
    pub expired: usize,
    pub stress_m1: usize,
    pub share_m1: Option<f64>,
    pub stress_tf: usize,
    pub share_tf: Option<f64>,
    pub stress_depth_low: DepthStat,
    pub stress_depth_close: DepthStat,
}

/// Une ligne de `pooled` : une (classe, TF de décision), toutes séries et tous k confondus.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StressPooled {
    pub class: StressClass,
    pub tf: Timeframe,
    pub triggered: usize,
    pub stress_m1: usize,
    pub share_m1: Option<f64>,
    pub stress_tf: usize,
    pub share_tf: Option<f64>,
    pub stress_depth_low: DepthStat,
    pub stress_depth_close: DepthStat,
}

/// Un dataset lu par la calibration (`datasets` de §11.2) : renseigné par l'appelant, qui connaît
/// le `dataset_hash` ([`StressCalibration::with_datasets`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DatasetRef {
    pub name: String,
    pub dataset_hash: String,
}

/// Résultat de [`calibrate`], sans `created_at` ni `commit` : tout ce qui est écrit ici entre dans
/// `measurement_hash`, sauf `measurement_hash` lui-même.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StressCalibration {
    pub schema: String,
    pub tv4_version: String,
    /// BLAKE3 de `config/costs.toml` : les classes en viennent.
    pub costs_file_hash: String,
    pub params: StressParamsRecord,
    pub datasets: Vec<DatasetRef>,
    pub results: Vec<StressResult>,
    pub pooled: Vec<StressPooled>,
    pub measurement_hash: String,
}

/// Les champs hachés de [`StressCalibration`] : tout, sauf `measurement_hash`.
#[derive(Serialize)]
struct HashedBody<'a> {
    schema: &'a str,
    tv4_version: &'a str,
    costs_file_hash: &'a str,
    params: &'a StressParamsRecord,
    datasets: &'a [DatasetRef],
    results: &'a [StressResult],
    pooled: &'a [StressPooled],
}

impl StressCalibration {
    /// Renseigne les datasets lus (triés par nom, pour que l'ordre de l'appelant ne change pas le
    /// hash) et recalcule `measurement_hash`.
    pub fn with_datasets(mut self, mut datasets: Vec<DatasetRef>) -> Result<Self, StressError> {
        datasets.sort_by(|a, b| a.name.cmp(&b.name));
        self.datasets = datasets;
        self.measurement_hash = self.compute_hash()?;
        Ok(self)
    }

    /// BLAKE3 du JSON canonique des champs hachés (hexadécimal, 64 caractères).
    pub fn compute_hash(&self) -> Result<String, StressError> {
        let body = HashedBody {
            schema: &self.schema,
            tv4_version: &self.tv4_version,
            costs_file_hash: &self.costs_file_hash,
            params: &self.params,
            datasets: &self.datasets,
            results: &self.results,
            pooled: &self.pooled,
        };
        Ok(blake3_hex(canonical_json(&body)?.as_bytes()))
    }

    /// Forme écrite sur disque : `created_at` et `commit` s'ajoutent, sans entrer dans le hash.
    pub fn file<'a>(&'a self, created_at: &'a str, commit: &'a str) -> StressFile<'a> {
        StressFile {
            created_at,
            commit,
            calibration: self,
        }
    }
}

/// Contenu du fichier `stress-<AAAAMMJJTHHMMSSZ>.json` (§11.2).
#[derive(Serialize)]
pub struct StressFile<'a> {
    pub created_at: &'a str,
    pub commit: &'a str,
    #[serde(flatten)]
    calibration: &'a StressCalibration,
}

/// Médiane, avec la convention de S8 : tri par [`f64::total_cmp`], moyenne des deux valeurs centrales
/// pour un effectif pair ; `None` pour une série vide.
pub fn median(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let upper = *sorted.get(n / 2)?;
    if n % 2 == 1 {
        Some(upper)
    } else {
        let lower = *sorted.get(n / 2 - 1)?;
        Some((lower + upper) / 2.0)
    }
}

/// p95 par la règle du rang le plus proche : la valeur triée d'indice `ceil(0,95 × n) − 1`, sans
/// interpolation. L'indice se calcule en entiers, `(95 × n).div_ceil(100) − 1` (soit `(95 × n + 99) / 100 − 1`) ; `None` pour une série
/// vide.
pub fn p95_nearest_rank(values: &[f64]) -> Option<f64> {
    let n = values.len();
    if n == 0 {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get((95 * n).div_ceil(100) - 1).copied()
}

impl StressParams {
    fn validate(&self) -> Result<(), StressError> {
        let invalid =
            |field: &'static str, reason: String| StressError::InvalidParams { field, reason };
        if self.tfs.is_empty() {
            return Err(invalid(
                "tfs",
                "au moins un TF de décision est requis".into(),
            ));
        }
        for (i, tf) in self.tfs.iter().enumerate() {
            if *tf == Timeframe::M1 {
                return Err(invalid(
                    "tfs",
                    "le TF de décision ne peut pas être 1m (le déclenchement se lit en 1m)".into(),
                ));
            }
            if self.tfs.get(..i).is_some_and(|before| before.contains(tf)) {
                return Err(invalid("tfs", format!("TF {tf} donné deux fois")));
            }
        }
        if self.k.is_empty() {
            return Err(invalid("k", "au moins un multiple d'ATR est requis".into()));
        }
        for (i, k) in self.k.iter().enumerate() {
            if !k.is_finite() || *k <= 0.0 {
                return Err(invalid(
                    "k",
                    format!("{k} : un nombre fini strictement positif est attendu"),
                ));
            }
            if self
                .k
                .get(..i)
                .is_some_and(|before| before.iter().any(|other| other.to_bits() == k.to_bits()))
            {
                return Err(invalid("k", format!("valeur {k} donnée deux fois")));
            }
        }
        if self.horizon_bars == 0 {
            return Err(invalid(
                "horizon_bars",
                "au moins une bougie est requise".into(),
            ));
        }
        // Le plus grand horizon doit tenir en millisecondes sur un `i64`.
        let widest = self.tfs.iter().map(|tf| tf.ms()).max().unwrap_or(0);
        let fits = i64::try_from(self.horizon_bars)
            .ok()
            .and_then(|bars| bars.checked_mul(widest))
            .is_some();
        if !fits {
            return Err(invalid(
                "horizon_bars",
                format!("{} bougies débordent", self.horizon_bars),
            ));
        }
        if !self.range_mult.is_finite() || self.range_mult <= 0.0 {
            return Err(invalid(
                "range_mult",
                format!(
                    "{} : un nombre fini strictement positif est attendu",
                    self.range_mult
                ),
            ));
        }
        Ok(())
    }
}

/// Compteurs et profondeurs d'une (série, TF, k), avant mise en forme.
#[derive(Default)]
struct Cell {
    triggered: usize,
    expired: usize,
    stress_m1: usize,
    stress_tf: usize,
    depth_low: Vec<f64>,
    depth_close: Vec<f64>,
}

/// Accumulateur de `pooled` pour une (classe, TF).
#[derive(Default)]
struct PoolAcc {
    present: bool,
    triggered: usize,
    stress_m1: usize,
    stress_tf: usize,
    depth_low: Vec<f64>,
    depth_close: Vec<f64>,
}

fn share(part: usize, whole: usize) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

/// ATR14 de Wilder des 1m **antérieures** à chaque 1m : `out[i]` ne dépend que de `candles[..i]`,
/// et seules les 1m à volume > 0 y entrent. `NaN` tant que l'ATR est indéfini.
fn atr_1m_before(candles: &[Candle]) -> Result<Vec<f64>, StressError> {
    let mut atr = Atr::new(ATR_PERIOD)?;
    let mut last = f64::NAN;
    let mut out = Vec::with_capacity(candles.len());
    for candle in candles {
        out.push(last);
        if candle.volume > 0.0 {
            last = atr.update(*candle);
        }
    }
    Ok(out)
}

/// Une série 1m préparée pour un TF : bougies `tf` closes et leur ATR14.
struct TfView<'a> {
    tf: Timeframe,
    closed: &'a [Candle],
    atr: Vec<f64>,
}

fn tf_view<'a>(mtf: &'a Mtf<'a>, tf: Timeframe) -> Result<TfView<'a>, StressError> {
    let all = mtf.candles(tf).unwrap_or(&[]);
    let closed_count = mtf
        .closed_counts(tf)
        .and_then(|counts| counts.last().copied())
        .unwrap_or(0);
    let closed = all.get(..closed_count).unwrap_or(all);
    let mut atr = Atr::new(ATR_PERIOD)?;
    let values = closed.iter().map(|bar| atr.update(*bar)).collect();
    Ok(TfView {
        tf,
        closed,
        atr: values,
    })
}

/// Joue les positions synthétiques d'une (série, TF, k) (§11.1 points 3 à 8).
fn run_cell(
    m1: &[Candle],
    atr_1m: &[f64],
    view: &TfView<'_>,
    k: f64,
    params: &StressParams,
) -> Cell {
    let mut cell = Cell::default();
    let Some(last) = m1.last() else {
        return cell;
    };
    let data_end = last.open_time_ms.saturating_add(MINUTE_MS);
    let tf_ms = view.tf.ms();
    // Le validateur a vérifié que `horizon_bars × durée(tf)` tient dans un `i64`.
    let horizon_ms = i64::try_from(params.horizon_bars)
        .unwrap_or(i64::MAX)
        .saturating_mul(tf_ms);
    // Plus petit `close_time` auquel une nouvelle position peut s'ouvrir.
    let mut next_allowed: Ts = Ts::MIN;
    for (j, bar) in view.closed.iter().enumerate() {
        let close_time = bar.open_time_ms.saturating_add(tf_ms);
        if close_time < next_allowed {
            continue;
        }
        let atr_j = view.atr.get(j).copied().unwrap_or(f64::NAN);
        if !atr_j.is_finite() {
            continue;
        }
        let stop = bar.close - k * atr_j;
        let active_from = close_time.saturating_add(ACTIVATION_DELAY_MS);
        let expiry = close_time.saturating_add(horizon_ms);
        let first = m1.partition_point(|c| c.open_time_ms < active_from);
        // Balayage des 1m actives de la fenêtre ; un stop non positif ne peut pas être touché.
        let trigger = if stop > 0.0 {
            m1.get(first..)
                .unwrap_or(&[])
                .iter()
                .take_while(|c| c.open_time_ms < expiry)
                .position(|c| c.volume > 0.0 && c.low <= stop)
                .map(|offset| first + offset)
        } else {
            None
        };
        match trigger {
            Some(index) => {
                let Some(c) = m1.get(index) else { break };
                cell.triggered += 1;
                let before = atr_1m.get(index).copied().unwrap_or(f64::NAN);
                if before.is_finite() && c.high - c.low > params.range_mult * before {
                    cell.stress_m1 += 1;
                    cell.depth_low.push((stop - c.low) / stop);
                    cell.depth_close.push((stop - c.close) / stop);
                }
                // Stress `tf`, informatif : la bougie close qui contient `c`.
                let container = view.tf.open_time_of(c.open_time_ms);
                if let Ok(position) = view
                    .closed
                    .binary_search_by_key(&container, |b| b.open_time_ms)
                    && let Some(container_bar) = view.closed.get(position)
                    && container_bar.high - container_bar.low > params.range_mult * atr_j
                {
                    cell.stress_tf += 1;
                }
                next_allowed = c.open_time_ms.saturating_add(MINUTE_MS);
            }
            None if expiry > data_end => break,
            None => {
                cell.expired += 1;
                next_allowed = expiry;
            }
        }
    }
    cell
}

/// Calibre la composante de stress sur des séries 1m (§11.1).
///
/// `series` : une paire (symbole, colonnes 1m) par série, dans l'ordre de sortie de `results`. Le
/// symbole (`BTC-USD`) donne l'actif de base, donc la classe, d'après `costs`. Les colonnes
/// doivent être des 1m alignées et strictement croissantes ([`Mtf::build`] le contrôle). Sortie :
/// `results` par (série, TF de `params.tfs`, k de `params.k`), puis `pooled` par classe (`major`
/// puis `alt`) et TF. `datasets` est vide : voir [`StressCalibration::with_datasets`].
pub fn calibrate(
    series: &[(String, Columns)],
    costs: &CostsFile,
    params: &StressParams,
) -> Result<StressCalibration, StressError> {
    params.validate()?;
    if series.is_empty() {
        return Err(StressError::NoSeries);
    }
    let mut results: Vec<StressResult> =
        Vec::with_capacity(series.len() * params.tfs.len() * params.k.len());
    // `[major, alt]` × TF.
    let mut pools: Vec<[PoolAcc; 2]> = params.tfs.iter().map(|_| Default::default()).collect();

    for (position, (symbol, columns)) in series.iter().enumerate() {
        if series
            .get(..position)
            .is_some_and(|before| before.iter().any(|(other, _)| other == symbol))
        {
            return Err(StressError::DuplicateSymbol {
                symbol: symbol.clone(),
            });
        }
        if columns.is_empty() {
            return Err(StressError::EmptySeries {
                symbol: symbol.clone(),
            });
        }
        let class = match costs.class_of(base_asset(symbol)?).0 {
            PairClass::Major => StressClass::Major,
            PairClass::Top20 | PairClass::Beyond => StressClass::Alt,
        };
        let class_index = usize::from(class == StressClass::Alt);
        let m1 = columns.to_candles();
        let mtf = Mtf::build(&m1, &params.tfs).map_err(|source| StressError::Mtf {
            symbol: symbol.clone(),
            source,
        })?;
        let atr_1m = atr_1m_before(&m1)?;
        for (tf_index, tf) in params.tfs.iter().enumerate() {
            let view = tf_view(&mtf, *tf)?;
            for k in &params.k {
                let cell = run_cell(&m1, &atr_1m, &view, *k, params);
                results.push(StressResult {
                    symbol: symbol.clone(),
                    class,
                    tf: *tf,
                    k: *k,
                    positions: cell.triggered + cell.expired,
                    triggered: cell.triggered,
                    expired: cell.expired,
                    stress_m1: cell.stress_m1,
                    share_m1: share(cell.stress_m1, cell.triggered),
                    stress_tf: cell.stress_tf,
                    share_tf: share(cell.stress_tf, cell.triggered),
                    stress_depth_low: DepthStat::of(&cell.depth_low),
                    stress_depth_close: DepthStat::of(&cell.depth_close),
                });
                if let Some(acc) = pools
                    .get_mut(tf_index)
                    .and_then(|pair| pair.get_mut(class_index))
                {
                    acc.present = true;
                    acc.triggered += cell.triggered;
                    acc.stress_m1 += cell.stress_m1;
                    acc.stress_tf += cell.stress_tf;
                    acc.depth_low.extend_from_slice(&cell.depth_low);
                    acc.depth_close.extend_from_slice(&cell.depth_close);
                }
            }
        }
    }

    let mut pooled = Vec::new();
    for (class_index, class) in [StressClass::Major, StressClass::Alt]
        .into_iter()
        .enumerate()
    {
        for (tf_index, tf) in params.tfs.iter().enumerate() {
            let Some(acc) = pools.get(tf_index).and_then(|pair| pair.get(class_index)) else {
                continue;
            };
            if !acc.present {
                continue;
            }
            pooled.push(StressPooled {
                class,
                tf: *tf,
                triggered: acc.triggered,
                stress_m1: acc.stress_m1,
                share_m1: share(acc.stress_m1, acc.triggered),
                stress_tf: acc.stress_tf,
                share_tf: share(acc.stress_tf, acc.triggered),
                stress_depth_low: DepthStat::of(&acc.depth_low),
                stress_depth_close: DepthStat::of(&acc.depth_close),
            });
        }
    }

    let mut calibration = StressCalibration {
        schema: STRESS_SCHEMA.to_string(),
        tv4_version: env!("CARGO_PKG_VERSION").to_string(),
        costs_file_hash: costs.file_hash().to_string(),
        params: StressParamsRecord::from_params(params),
        datasets: Vec::new(),
        results,
        pooled,
        measurement_hash: String::new(),
    };
    calibration.measurement_hash = calibration.compute_hash()?;
    Ok(calibration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_mediane_suit_la_convention_de_s8() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0]), Some(3.0));
        assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
        // Effectif pair : moyenne des deux valeurs centrales.
        assert_eq!(median(&[4.0, 1.0, 2.0, 8.0]), Some(3.0));
    }

    #[test]
    fn le_p95_est_le_rang_le_plus_proche() {
        assert_eq!(p95_nearest_rank(&[]), None);
        assert_eq!(p95_nearest_rank(&[7.0]), Some(7.0));
        // n = 3 : ceil(2,85) = 3, donc le maximum.
        assert_eq!(p95_nearest_rank(&[1.0, 3.0, 2.0]), Some(3.0));
        // n = 20 : ceil(19) = 19, donc la 19e valeur triée et non la 20e.
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(p95_nearest_rank(&values), Some(19.0));
        // n = 21 : ceil(19,95) = 20.
        let values: Vec<f64> = (1..=21).map(f64::from).collect();
        assert_eq!(p95_nearest_rank(&values), Some(20.0));
        // n = 100 : ceil(95) = 95.
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(p95_nearest_rank(&values), Some(95.0));
    }
}
