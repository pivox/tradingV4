//! Modèle de coûts des profils baseline, adverse et maker (docs/specs/s10.md §4 et §5.1).
//!
//! - [`CostsFile`] lit `config/costs.toml` (schéma `tv4-costs-v1`) : trois classes de paires, leurs
//!   défauts et les transformations des profils. **Aucune valeur de marché ni liste d'actifs n'est
//!   en constante dans le code** : tout vient du fichier.
//! - [`CostMeasures`] lit le fichier de mesures facultatif `tv4-cost-measures-v1` (capture du top of
//!   book, S17), qui remplace les défauts paire par paire.
//! - [`resolve_leg`] est une fonction pure : fichier, mesures, profil, symbole, TF de décision et
//!   graine donnent un [`LegCost`] (la valeur `Copy` que verra le simulateur, taux précalculés) et un
//!   [`LegCostRecord`] (les valeurs résolues en bps et leurs sources, pour les rapports et le
//!   `config_hash`).
//! - [`StopSlippage`] est la loi du slippage de stop : une fonction quantile linéaire par morceaux,
//!   sans libm, sur cinq nœuds. Le tirage lui-même (`model::rng`) est fait par le simulateur.
//!
//! Les paramètres sont des `f64` en bps, comme `Slippage` (s10 §19 n° 13). Ce module ne dépend de
//! rien hors de `model`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::hash::blake3_hex;
use super::rng;
use super::{BPS_PER_UNIT, MAX_BPS, ModelError, Slippage, Timeframe, read_config_text};

/// Schéma de `config/costs.toml`.
pub const COSTS_SCHEMA: &str = "tv4-costs-v1";

/// Schéma du fichier de mesures de S17.
pub const MEASURES_SCHEMA: &str = "tv4-cost-measures-v1";

/// Version du modèle de coûts, distincte de celle des règles de fill (`s6.1`).
pub const COSTS_RULES_VERSION: &str = "c1";

/// Abscisses des nœuds de la loi du stop : `u = 0 ; 0,5 ; 0,95 ; 0,99 ; 1`. Constante de `c1`.
pub const STOP_QUANTILE_NODES: [f64; 5] = [0.0, 0.5, 0.95, 0.99, 1.0];

/// Capture minimale d'une mesure de coûts, en heures (04 §3.2 : « ≥ 24 h de capture »).
pub const MIN_MEASURE_HOURS: f64 = 24.0;

/// Borne exclusive de la graine : `2^53`, pour qu'elle s'écrive exactement en JSON, en PHP comme en
/// JavaScript (s10 §8.4).
pub const MAX_COST_SEED: u64 = 1 << 53;

/// Borne exclusive, en bps, des taux et des mesures : 100 %.
const MAX_RATE_BPS: f64 = MAX_BPS as f64;

// ---------------------------------------------------------------------------------------------
// Noms : classes, profils, sources
// ---------------------------------------------------------------------------------------------

/// Classe de paires. Le **contenu** des classes (les actifs de base) vient de `config/costs.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairClass {
    Major,
    Top20,
    Beyond,
}

impl PairClass {
    /// Rendu canonique, identique à la forme sérialisée.
    pub const fn as_str(self) -> &'static str {
        match self {
            PairClass::Major => "major",
            PairClass::Top20 => "top20",
            PairClass::Beyond => "beyond",
        }
    }
}

impl fmt::Display for PairClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Profil de coûts d'un run (s10 §4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CostProfileName {
    Baseline,
    Adverse,
    Maker,
}

impl CostProfileName {
    /// Les trois profils, dans l'ordre fixe des runs de s10 : baseline, adverse, maker.
    pub const ALL: [CostProfileName; 3] = [
        CostProfileName::Baseline,
        CostProfileName::Adverse,
        CostProfileName::Maker,
    ];

    /// Rendu canonique, identique à la forme sérialisée.
    pub const fn as_str(self) -> &'static str {
        match self {
            CostProfileName::Baseline => "baseline",
            CostProfileName::Adverse => "adverse",
            CostProfileName::Maker => "maker",
        }
    }

    /// `true` pour les profils qui jugent la porte de coûts (baseline et adverse) ; le profil maker
    /// est informatif (s10 §4.4).
    pub const fn gates(self) -> bool {
        !matches!(self, CostProfileName::Maker)
    }
}

impl fmt::Display for CostProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// D'où vient la classe d'une paire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassSource {
    /// Base listée dans `classes.major` ou `classes.top20`.
    List,
    /// Base listée nulle part : classe la plus chère (`beyond`).
    Fallback,
}

/// D'où vient une valeur de coût résolue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueSource {
    /// Médiane du fichier de mesures.
    Measured,
    /// Défaut de la classe.
    Default,
}

// ---------------------------------------------------------------------------------------------
// Fichier de coûts
// ---------------------------------------------------------------------------------------------

/// Les quatre quantiles de la loi du slippage de stop, en bps (valeurs aux nœuds 0,5 ; 0,95 ; 0,99
/// et 1 ; la valeur au nœud 0 est toujours 0).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopSlippageBps {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub p100: f64,
}

impl StopSlippageBps {
    /// Les quatre valeurs multipliées par `mult` (profil adverse).
    fn scaled(self, mult: f64) -> StopSlippageBps {
        StopSlippageBps {
            p50: self.p50 * mult,
            p95: self.p95 * mult,
            p99: self.p99 * mult,
            p100: self.p100 * mult,
        }
    }
}

/// Coûts par défaut d'une classe de paires.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassCosts {
    /// Demi-spread, en bps.
    pub half_spread_bps: f64,
    /// Slippage d'un ordre market, en bps.
    pub market_slippage_bps: f64,
    /// Loi du slippage d'un stop, en bps.
    pub stop_slippage_bps: StopSlippageBps,
    /// Probabilité qu'un stop soit rempli au prix de stress.
    pub stress_probability: f64,
    /// Chute de stress : le fill est porté à `stop × (1 − stress_drop)` s'il est pire.
    pub stress_drop: f64,
}

/// Actifs de base de chaque classe. Une base listée nulle part est `beyond`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassLists {
    pub major: Vec<String>,
    pub top20: Vec<String>,
}

/// Défauts des trois classes.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassDefaults {
    pub major: ClassCosts,
    pub top20: ClassCosts,
    pub beyond: ClassCosts,
}

impl ClassDefaults {
    /// Défauts de `class`.
    pub fn get(&self, class: PairClass) -> &ClassCosts {
        match class {
            PairClass::Major => &self.major,
            PairClass::Top20 => &self.top20,
            PairClass::Beyond => &self.beyond,
        }
    }
}

/// Transformation du profil adverse (04 §3, ligne « Adverse »).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdverseTransform {
    pub half_spread_mult: f64,
    pub market_slippage_mult: f64,
    pub stop_slippage_mult: f64,
    pub stress_probability_mult: f64,
}

/// Paramètres du profil maker (04 §3, ligne « Maker », et §3.1 point 2).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MakerSettings {
    /// Probabilité qu'un ordre au repos traversé soit rempli (un tirage par ordre).
    pub fill_probability: f64,
    /// Sélection adverse débitée sur chaque fill au repos, en bps.
    pub adverse_selection_bps: f64,
    /// Décalage de la limit d'entrée sous le bid du signal, en bps (haché).
    pub entry_offset_bps: f64,
    /// TTL de la limit d'entrée quand le TF de décision est 1d, en minutes.
    pub ttl_minutes_1d: u32,
    /// Plafond du TTL pour un TF de décision intraday, en minutes.
    pub ttl_minutes_max_intraday: u32,
    /// Part minimale d'entrées remplies en maker pour l'étiquette « dépend du fill ». Ne change
    /// aucun trade : hors du `config_hash`.
    pub min_maker_entry_share: f64,
}

/// Transformations des profils adverse et maker.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSettings {
    pub adverse: AdverseTransform,
    pub maker: MakerSettings,
}

/// `config/costs.toml`, schéma `tv4-costs-v1`, validé.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostsFile {
    pub schema: String,
    pub rules_version: String,
    /// `true` une fois la probabilité de stress inscrite d'après `tv4 costs calibrate-stress`.
    pub stress_calibrated: bool,
    /// Référence de la calibration (chemin ou identifiant) ; chaîne vide tant qu'elle n'existe pas.
    pub stress_calibration_ref: String,
    pub classes: ClassLists,
    pub defaults: ClassDefaults,
    pub profiles: ProfileSettings,
    #[serde(skip)]
    file_hash: String,
}

impl CostsFile {
    /// Lit et valide un texte au format de `config/costs.toml`. Le BLAKE3 des octets du texte est
    /// gardé ([`CostsFile::file_hash`]).
    pub fn from_toml_str(text: &str) -> Result<CostsFile, ModelError> {
        let mut file: CostsFile = toml::from_str(text)?;
        file.validate()?;
        file.file_hash = blake3_hex(text.as_bytes());
        Ok(file)
    }

    /// Lit et valide un fichier (en pratique `config/costs.toml`).
    pub fn load(path: &Path) -> Result<CostsFile, ModelError> {
        CostsFile::from_toml_str(&read_config_text(path)?)
    }

    /// BLAKE3 des octets du fichier lu (`costs_file_hash` du manifeste), en hexadécimal.
    pub fn file_hash(&self) -> &str {
        &self.file_hash
    }

    /// Référence de calibration, `None` tant que la chaîne est vide.
    pub fn calibration_ref(&self) -> Option<&str> {
        if self.stress_calibration_ref.is_empty() {
            None
        } else {
            Some(&self.stress_calibration_ref)
        }
    }

    /// Classe d'un actif de base : `major`, `top20` ou, faute de liste, `beyond` (repli).
    pub fn class_of(&self, base: &str) -> (PairClass, ClassSource) {
        if self.classes.major.iter().any(|b| b == base) {
            (PairClass::Major, ClassSource::List)
        } else if self.classes.top20.iter().any(|b| b == base) {
            (PairClass::Top20, ClassSource::List)
        } else {
            (PairClass::Beyond, ClassSource::Fallback)
        }
    }

    fn validate(&self) -> Result<(), ModelError> {
        if self.schema != COSTS_SCHEMA {
            return Err(invalid_costs(
                "schema",
                format!("`{}` attendu, `{}` lu", COSTS_SCHEMA, self.schema),
            ));
        }
        if self.rules_version != COSTS_RULES_VERSION {
            return Err(invalid_costs(
                "rules_version",
                format!(
                    "`{}` attendu, `{}` lu",
                    COSTS_RULES_VERSION, self.rules_version
                ),
            ));
        }
        // Un actif de base ne figure que dans une seule classe, une seule fois.
        let mut seen = BTreeSet::new();
        for (name, list) in [
            ("classes.major", &self.classes.major),
            ("classes.top20", &self.classes.top20),
        ] {
            for base in list {
                if base.is_empty() {
                    return Err(invalid_costs(name, "actif de base vide".to_string()));
                }
                if !seen.insert(base.as_str()) {
                    return Err(invalid_costs(
                        name,
                        format!("l'actif `{base}` figure dans plusieurs classes (ou deux fois)"),
                    ));
                }
            }
        }
        let adverse = &self.profiles.adverse;
        for (field, value) in [
            (
                "profiles.adverse.half_spread_mult",
                adverse.half_spread_mult,
            ),
            (
                "profiles.adverse.market_slippage_mult",
                adverse.market_slippage_mult,
            ),
            (
                "profiles.adverse.stop_slippage_mult",
                adverse.stop_slippage_mult,
            ),
            (
                "profiles.adverse.stress_probability_mult",
                adverse.stress_probability_mult,
            ),
        ] {
            check_non_negative(field, value)?;
        }
        for class in [PairClass::Major, PairClass::Top20, PairClass::Beyond] {
            let costs = self.defaults.get(class);
            let prefix = format!("defaults.{class}");
            validate_class(&prefix, costs)?;
            // La probabilité reste une probabilité après la multiplication du profil adverse.
            let adverse_probability = costs.stress_probability * adverse.stress_probability_mult;
            if adverse_probability.is_nan() || adverse_probability > 1.0 {
                return Err(invalid_costs(
                    format!("{prefix}.stress_probability"),
                    format!(
                        "{} × profiles.adverse.stress_probability_mult = {adverse_probability} dépasse 1",
                        costs.stress_probability
                    ),
                ));
            }
        }
        let maker = &self.profiles.maker;
        check_probability("profiles.maker.fill_probability", maker.fill_probability)?;
        check_rate_bps(
            "profiles.maker.adverse_selection_bps",
            maker.adverse_selection_bps,
        )?;
        check_rate_bps("profiles.maker.entry_offset_bps", maker.entry_offset_bps)?;
        check_probability(
            "profiles.maker.min_maker_entry_share",
            maker.min_maker_entry_share,
        )?;
        for (field, value) in [
            ("profiles.maker.ttl_minutes_1d", maker.ttl_minutes_1d),
            (
                "profiles.maker.ttl_minutes_max_intraday",
                maker.ttl_minutes_max_intraday,
            ),
        ] {
            if value < 1 {
                return Err(invalid_costs(
                    field,
                    "au moins 1 minute attendue".to_string(),
                ));
            }
        }
        Ok(())
    }
}

fn invalid_costs(field: impl Into<String>, reason: String) -> ModelError {
    ModelError::CostsInvalid {
        field: field.into(),
        reason,
    }
}

fn check_non_negative(field: &str, value: f64) -> Result<(), ModelError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(invalid_costs(
            field,
            format!("un nombre fini positif ou nul est attendu, {value} lu"),
        ))
    }
}

fn check_probability(field: &str, value: f64) -> Result<(), ModelError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(invalid_costs(
            field,
            format!("une probabilité dans [0, 1] est attendue, {value} lu"),
        ))
    }
}

/// Un taux en bps qui doit rester strictement sous 100 %.
fn check_rate_bps(field: &str, value: f64) -> Result<(), ModelError> {
    if (0.0..MAX_RATE_BPS).contains(&value) {
        Ok(())
    } else {
        Err(invalid_costs(
            field,
            format!("un nombre fini dans [0, 10 000[ bps est attendu, {value} lu"),
        ))
    }
}

fn validate_class(prefix: &str, costs: &ClassCosts) -> Result<(), ModelError> {
    check_non_negative(&format!("{prefix}.half_spread_bps"), costs.half_spread_bps)?;
    check_non_negative(
        &format!("{prefix}.market_slippage_bps"),
        costs.market_slippage_bps,
    )?;
    let q = &costs.stop_slippage_bps;
    for (name, value) in [
        ("p50", q.p50),
        ("p95", q.p95),
        ("p99", q.p99),
        ("p100", q.p100),
    ] {
        check_non_negative(&format!("{prefix}.stop_slippage_bps.{name}"), value)?;
    }
    if !(q.p50 <= q.p95 && q.p95 <= q.p99 && q.p99 <= q.p100) {
        return Err(invalid_costs(
            format!("{prefix}.stop_slippage_bps"),
            "les quantiles doivent croître au sens large (p50 ≤ p95 ≤ p99 ≤ p100)".to_string(),
        ));
    }
    check_probability(
        &format!("{prefix}.stress_probability"),
        costs.stress_probability,
    )?;
    if !(costs.stress_drop.is_finite() && costs.stress_drop > 0.0 && costs.stress_drop < 1.0) {
        return Err(invalid_costs(
            format!("{prefix}.stress_drop"),
            format!(
                "un nombre dans ]0, 1[ est attendu, {} lu",
                costs.stress_drop
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Fichier de mesures
// ---------------------------------------------------------------------------------------------

/// Demi-spread mesuré : la médiane s'applique, le p95 est lu et gardé sans être appliqué.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HalfSpreadMeasure {
    pub median: f64,
    pub p95: f64,
}

/// Slippage market mesuré (médiane).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlippageMeasure {
    pub median: f64,
}

/// Mesure d'un actif de base.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseMeasure {
    /// Instrument mesuré (informatif).
    pub inst_id: String,
    /// Durée de la capture, en heures (≥ 24).
    pub hours: f64,
    pub half_spread_bps: Option<HalfSpreadMeasure>,
    pub market_slippage_bps: Option<SlippageMeasure>,
}

/// Fichier de mesures `tv4-cost-measures-v1`, validé. Clé par actif de base, comme les classes : une
/// mesure sur `BTC-USDC` s'applique à une jambe `BTC-USD` d'un dataset Kraken, parce que c'est le
/// coût du lieu d'exécution que le backtest doit payer.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostMeasures {
    pub schema: String,
    pub venue: String,
    pub captured_from_ms: i64,
    pub captured_to_ms: i64,
    pub bases: BTreeMap<String, BaseMeasure>,
    #[serde(skip)]
    file_hash: String,
}

impl CostMeasures {
    /// Lit et valide un texte JSON ; le BLAKE3 de ses octets est gardé.
    pub fn from_json_str(text: &str) -> Result<CostMeasures, ModelError> {
        let mut measures: CostMeasures = serde_json::from_str(text)?;
        measures.validate()?;
        measures.file_hash = blake3_hex(text.as_bytes());
        Ok(measures)
    }

    /// Lit et valide un fichier.
    pub fn load(path: &Path) -> Result<CostMeasures, ModelError> {
        CostMeasures::from_json_str(&read_config_text(path)?)
    }

    /// BLAKE3 des octets du fichier lu (`cost_measures_hash`), en hexadécimal.
    pub fn file_hash(&self) -> &str {
        &self.file_hash
    }

    fn validate(&self) -> Result<(), ModelError> {
        if self.schema != MEASURES_SCHEMA {
            return Err(invalid_measures(
                "schema",
                format!("`{}` attendu, `{}` lu", MEASURES_SCHEMA, self.schema),
            ));
        }
        if self.captured_from_ms >= self.captured_to_ms {
            return Err(invalid_measures(
                "captured_from_ms",
                "doit précéder captured_to_ms".to_string(),
            ));
        }
        for (base, measure) in &self.bases {
            let prefix = format!("bases.{base}");
            if base.is_empty() {
                return Err(invalid_measures("bases", "actif de base vide".to_string()));
            }
            if !(measure.hours.is_finite() && measure.hours >= MIN_MEASURE_HOURS) {
                return Err(invalid_measures(
                    format!("{prefix}.hours"),
                    format!(
                        "au moins {MIN_MEASURE_HOURS} h de capture sont exigées (04 §3.2), {} lu",
                        measure.hours
                    ),
                ));
            }
            // Des trous de capture sont permis (`hours` inférieur à la fenêtre), pas des heures en trop.
            let window_hours = (self.captured_to_ms - self.captured_from_ms) as f64 / 3_600_000.0;
            if measure.hours > window_hours {
                return Err(invalid_measures(
                    format!("{prefix}.hours"),
                    format!(
                        "{} h déclarées pour une fenêtre de capture de {window_hours} h \
                         (captured_to_ms − captured_from_ms)",
                        measure.hours
                    ),
                ));
            }
            if let Some(spread) = &measure.half_spread_bps {
                check_measure(&format!("{prefix}.half_spread_bps.median"), spread.median)?;
                check_measure(&format!("{prefix}.half_spread_bps.p95"), spread.p95)?;
            }
            if let Some(slippage) = &measure.market_slippage_bps {
                check_measure(
                    &format!("{prefix}.market_slippage_bps.median"),
                    slippage.median,
                )?;
            }
        }
        Ok(())
    }
}

fn invalid_measures(field: impl Into<String>, reason: String) -> ModelError {
    ModelError::MeasuresInvalid {
        field: field.into(),
        reason,
    }
}

fn check_measure(field: &str, value: f64) -> Result<(), ModelError> {
    if (0.0..MAX_RATE_BPS).contains(&value) {
        Ok(())
    } else {
        Err(invalid_measures(
            field,
            format!("un nombre fini dans [0, 10 000[ bps est attendu, {value} lu"),
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// Loi du slippage de stop
// ---------------------------------------------------------------------------------------------

/// Loi du slippage d'un stop : fonction quantile linéaire par morceaux sur les nœuds
/// [`STOP_QUANTILE_NODES`] (valeurs `0, p50, p95, p99, p100`, en bps).
///
/// Elle ne porte que les valeurs ; les abscisses sont une constante de `c1`. Le slippage de stop
/// n'est jamais favorable : `quantile_bps(u) ≥ 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StopSlippage {
    bps: [f64; 5],
}

impl StopSlippage {
    /// Loi des quatre quantiles donnés (le nœud `u = 0` vaut 0).
    pub const fn from_quantiles(q: StopSlippageBps) -> StopSlippage {
        StopSlippage {
            bps: [0.0, q.p50, q.p95, q.p99, q.p100],
        }
    }

    /// Les quatre quantiles de la loi.
    pub const fn quantiles(&self) -> StopSlippageBps {
        StopSlippageBps {
            p50: self.bps[1],
            p95: self.bps[2],
            p99: self.bps[3],
            p100: self.bps[4],
        }
    }

    /// Quantile de la loi en `u ∈ [0, 1)`, en bps : interpolation linéaire entre les deux nœuds qui
    /// encadrent `u`, soit `Q[k] + (Q[k+1] − Q[k]) × (u − U[k]) / (U[k+1] − U[k])`.
    ///
    /// Opérations IEEE de base seulement, dans cet ordre, sans `mul_add` ni libm (03 §7.4). Borné par
    /// `p100`, y compris à l'arrondi. Un `u` négatif ou NaN vaut 0, et `u ≥ 1` vaut `p100`.
    pub fn quantile_bps(&self, u: f64) -> f64 {
        let nodes = STOP_QUANTILE_NODES;
        if u.is_nan() || u <= 0.0 {
            return self.bps[0];
        }
        if u >= 1.0 {
            return self.bps[4];
        }
        let mut k = 0;
        while k < 3 && u >= nodes[k + 1] {
            k += 1;
        }
        let (q0, q1) = (self.bps[k], self.bps[k + 1]);
        let value = q0 + (q1 - q0) * (u - nodes[k]) / (nodes[k + 1] - nodes[k]);
        // Le plafond du segment absorbe un éventuel arrondi au-delà du nœud supérieur.
        value.min(q1)
    }

    /// Moyenne de la loi par la formule des trapèzes sur les nœuds : arithmétique sur les valeurs
    /// du fichier, pas une mesure. 20,625 bp pour les défauts des majors de 04 §3.1.
    pub fn mean_bps(&self) -> f64 {
        let nodes = STOP_QUANTILE_NODES;
        let mut total = 0.0;
        for k in 0..4 {
            total += (nodes[k + 1] - nodes[k]) * (self.bps[k] + self.bps[k + 1]) / 2.0;
        }
        total
    }
}

// ---------------------------------------------------------------------------------------------
// Coût d'une jambe, tel que le voit le simulateur
// ---------------------------------------------------------------------------------------------

/// Prix d'un stop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StopCost {
    /// Chemin hérité : taux fixe `(slippage + stop_stress_bps) / 10 000`, sans tirage.
    Fixed { rate: f64 },
    /// s10 : slippage tiré dans la loi, puis stress avec la probabilité donnée (s10 §5.2).
    Drawn {
        dist: StopSlippage,
        stress_probability: f64,
        stress_drop: f64,
    },
}

/// Mode d'entrée.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EntryMode {
    /// Une entrée market part en taker (baseline et adverse).
    Taker,
    /// Une entrée market devient une limit d'achat sous le bid du signal (profil maker, s10 §7).
    Maker {
        ttl_minutes: u32,
        fill_probability: f64,
        offset_rate: f64,
    },
}

/// Coût d'une jambe, `Copy`, taux précalculés (aucune division par 10 000 dans la boucle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegCost {
    /// Demi-spread, en fraction du prix.
    pub half_spread_rate: f64,
    /// Slippage d'un ordre market, en fraction du prix.
    pub market_slippage_rate: f64,
    pub stop: StopCost,
    pub entry: EntryMode,
    /// Sélection adverse débitée sur un fill au repos, en fraction du notionnel.
    pub adverse_selection_rate: f64,
    /// Clé de la jambe pour les tirages (`rng::leg_key` du symbole).
    pub leg_key: u64,
    /// Graine des tirages.
    pub seed: u64,
}

impl LegCost {
    /// Coût du chemin hérité (s8 et s9) : demi-spread nul, slippage market scalaire, stop à taux
    /// fixe `(slippage.bps() + stop_stress_bps) / BPS_PER_UNIT` (l'expression de `Simulator::new`),
    /// aucun tirage, entrée taker, aucune sélection adverse.
    pub fn legacy(slippage: Slippage, stop_stress_bps: f64) -> LegCost {
        LegCost {
            half_spread_rate: 0.0,
            market_slippage_rate: slippage.rate(),
            stop: StopCost::Fixed {
                rate: (slippage.bps() + stop_stress_bps) / BPS_PER_UNIT,
            },
            entry: EntryMode::Taker,
            adverse_selection_rate: 0.0,
            leg_key: 0,
            seed: 0,
        }
    }

    /// `true` pour un coût qui se comporte comme le chemin hérité : le simulateur y garde les
    /// expressions de S9, terme par terme.
    pub fn is_legacy(&self) -> bool {
        matches!(self.stop, StopCost::Fixed { .. })
            && matches!(self.entry, EntryMode::Taker)
            && self.half_spread_rate == 0.0
            && self.adverse_selection_rate == 0.0
    }

    /// Taux composé d'un fill taker, `h + s` (s10 §6.2). Pour le chemin hérité, `h = 0` et
    /// `0.0 + s = s` exactement.
    pub fn taker_rate(&self) -> f64 {
        self.half_spread_rate + self.market_slippage_rate
    }

    /// Taux sous lequel une entrée maker place sa limit par rapport à `close` : `h + o`, ou `h` seul
    /// quand le décalage est nul (s10 §7.1). `None` hors du mode maker.
    pub fn maker_entry_rate(&self) -> Option<f64> {
        match self.entry {
            EntryMode::Taker => None,
            EntryMode::Maker { offset_rate, .. } => Some(if offset_rate == 0.0 {
                self.half_spread_rate
            } else {
                self.half_spread_rate + offset_rate
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Résolution
// ---------------------------------------------------------------------------------------------

/// Valeurs résolues d'une jambe, en bps, et leurs sources. Sérialisé dans `metrics.costs.legs[]`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LegCostRecord {
    pub symbol: String,
    pub class: PairClass,
    pub class_source: ClassSource,
    pub spread_source: ValueSource,
    pub slippage_source: ValueSource,
    pub half_spread_bps: f64,
    pub market_slippage_bps: f64,
    /// p95 du demi-spread mesuré : lu et gardé, non appliqué.
    pub measured_p95_half_spread_bps: Option<f64>,
    pub stop_slippage_bps: StopSlippageBps,
    pub stop_slippage_mean_bps: f64,
    pub stress_probability: f64,
    pub stress_drop: f64,
    /// `"taker"` ou `"maker"`.
    pub entry: &'static str,
    pub maker_fill_probability: Option<f64>,
    pub maker_ttl_minutes: Option<u32>,
    pub adverse_selection_bps: f64,
    /// TF de décision de la stratégie (TTL maker, bloc haché `costs.entry`) : hors de
    /// `metrics.costs.legs[]`.
    #[serde(skip)]
    pub decision_tf: Timeframe,
    /// Décalage de la limit maker en bps (bloc haché `costs.entry`) : hors de
    /// `metrics.costs.legs[]`.
    #[serde(skip)]
    pub entry_offset_bps: Option<f64>,
}

/// La partie d'une jambe qui détermine les trades : le bloc `costs.legs.<symbole>` du `config_hash`
/// (s10 §10). Les sources (`measured`, `default`, `fallback`) n'y sont pas : elles ne changent aucun
/// trade.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LegCostHashed {
    pub adverse_selection_bps: f64,
    pub class: PairClass,
    pub half_spread_bps: f64,
    pub market_slippage_bps: f64,
    pub stop_slippage_bps: StopSlippageBps,
    pub stress_drop: f64,
    pub stress_probability: f64,
}

/// Le bloc `costs.entry` du `config_hash` d'une jambe : TF de décision et mode d'entrée.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EntryHashed {
    pub decision_tf: Timeframe,
    pub mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_offset_bps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_probability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<u32>,
}

impl LegCostRecord {
    /// Bloc haché de la jambe.
    pub fn hashed(&self) -> LegCostHashed {
        LegCostHashed {
            adverse_selection_bps: self.adverse_selection_bps,
            class: self.class,
            half_spread_bps: self.half_spread_bps,
            market_slippage_bps: self.market_slippage_bps,
            stop_slippage_bps: self.stop_slippage_bps,
            stress_drop: self.stress_drop,
            stress_probability: self.stress_probability,
        }
    }

    /// Bloc `costs.entry` haché : `{decision_tf, mode: "taker"}` ou, en maker, avec le décalage, la
    /// probabilité de fill et le TTL.
    pub fn entry_hashed(&self) -> EntryHashed {
        EntryHashed {
            decision_tf: self.decision_tf,
            mode: self.entry,
            entry_offset_bps: self.entry_offset_bps,
            fill_probability: self.maker_fill_probability,
            ttl_minutes: self.maker_ttl_minutes,
        }
    }
}

/// Actif de base d'un symbole : le texte avant le premier `-` (`BTC-USD` → `BTC`).
pub fn base_asset(symbol: &str) -> Result<&str, ModelError> {
    match symbol.split_once('-') {
        Some((base, _)) if !base.is_empty() => Ok(base),
        _ => Err(ModelError::CostsSymbol {
            symbol: symbol.to_string(),
        }),
    }
}

/// Refuse une graine qui ne s'écrit pas exactement en JSON (≥ 2^53).
pub fn check_seed(seed: u64) -> Result<(), ModelError> {
    if seed < MAX_COST_SEED {
        Ok(())
    } else {
        Err(invalid_costs(
            "seed",
            format!("la graine doit rester dans [0, 2^53[, {seed} lu"),
        ))
    }
}

/// TTL de la limit maker, en minutes, d'après le TF de décision : `ttl_minutes_1d` en 1d, sinon le
/// plus petit de la durée du TF et de `ttl_minutes_max_intraday` (s10 §4.5).
fn maker_ttl_minutes(settings: &MakerSettings, decision_tf: Timeframe) -> u32 {
    if decision_tf == Timeframe::D1 {
        settings.ttl_minutes_1d
    } else {
        let minutes = u32::try_from(decision_tf.ms() / 60_000).unwrap_or(u32::MAX);
        minutes.min(settings.ttl_minutes_max_intraday)
    }
}

/// Résout le coût d'une jambe : classe, puis défauts de la classe, remplacés par les mesures de la
/// base s'il y en a, puis transformation du profil, puis bornes sur les valeurs **résolues**.
///
/// Fonction pure. Les bornes de taux se vérifient ici et non sur le fichier seul : ce sont les
/// valeurs qui atteignent le simulateur, après mesures et multiplicateurs. Sans elles, une
/// combinaison admise (mesure élevée × 3 en adverse) donnerait un taux ≥ 100 %, donc un prix de
/// vente nul ou négatif. L'erreur nomme la jambe et le profil.
pub fn resolve_leg(
    file: &CostsFile,
    measures: Option<&CostMeasures>,
    profile: CostProfileName,
    symbol: &str,
    decision_tf: Timeframe,
    seed: u64,
) -> Result<(LegCost, LegCostRecord), ModelError> {
    check_seed(seed)?;
    let base = base_asset(symbol)?;
    let (class, class_source) = file.class_of(base);
    let defaults = file.defaults.get(class);
    let measure = measures.and_then(|m| m.bases.get(base));

    let (mut half_spread_bps, spread_source, measured_p95) =
        match measure.and_then(|m| m.half_spread_bps) {
            Some(spread) => (spread.median, ValueSource::Measured, Some(spread.p95)),
            None => (defaults.half_spread_bps, ValueSource::Default, None),
        };
    let (mut market_slippage_bps, slippage_source) =
        match measure.and_then(|m| m.market_slippage_bps) {
            Some(slippage) => (slippage.median, ValueSource::Measured),
            None => (defaults.market_slippage_bps, ValueSource::Default),
        };
    let mut stop_bps = defaults.stop_slippage_bps;
    let mut stress_probability = defaults.stress_probability;
    let stress_drop = defaults.stress_drop;

    if profile == CostProfileName::Adverse {
        let adverse = &file.profiles.adverse;
        half_spread_bps *= adverse.half_spread_mult;
        market_slippage_bps *= adverse.market_slippage_mult;
        stop_bps = stop_bps.scaled(adverse.stop_slippage_mult);
        stress_probability *= adverse.stress_probability_mult;
    }

    let maker = &file.profiles.maker;
    let is_maker = profile == CostProfileName::Maker;
    let adverse_selection_bps = if is_maker {
        maker.adverse_selection_bps
    } else {
        0.0
    };
    let entry_offset_bps = if is_maker {
        Some(maker.entry_offset_bps)
    } else {
        None
    };

    // Bornes sur les valeurs résolues.
    let context = |what: &str| format!("jambe {symbol}, profil {profile} : {what}");
    let below_one = |total: f64| (0.0..MAX_RATE_BPS).contains(&total);
    let spread_plus_slippage = half_spread_bps + market_slippage_bps;
    if !below_one(spread_plus_slippage) {
        return Err(invalid_costs(
            "half_spread_bps + market_slippage_bps",
            context(&format!(
                "total de {spread_plus_slippage} bps, sous 10 000 bps exigés"
            )),
        ));
    }
    let spread_plus_stop = half_spread_bps + stop_bps.p100;
    if !below_one(spread_plus_stop) {
        return Err(invalid_costs(
            "half_spread_bps + stop_slippage_bps.p100",
            context(&format!(
                "total de {spread_plus_stop} bps, sous 10 000 bps exigés"
            )),
        ));
    }
    if let Some(offset) = entry_offset_bps {
        let spread_plus_offset = half_spread_bps + offset;
        if !below_one(spread_plus_offset) {
            return Err(invalid_costs(
                "half_spread_bps + entry_offset_bps",
                context(&format!(
                    "total de {spread_plus_offset} bps, sous 10 000 bps exigés"
                )),
            ));
        }
    }
    if !(stress_probability.is_finite() && (0.0..=1.0).contains(&stress_probability)) {
        return Err(invalid_costs(
            "stress_probability",
            context(&format!("{stress_probability} n'est pas une probabilité")),
        ));
    }

    let dist = StopSlippage::from_quantiles(stop_bps);
    let ttl_minutes = maker_ttl_minutes(maker, decision_tf);
    let entry = if is_maker {
        EntryMode::Maker {
            ttl_minutes,
            fill_probability: maker.fill_probability,
            offset_rate: maker.entry_offset_bps / BPS_PER_UNIT,
        }
    } else {
        EntryMode::Taker
    };
    let cost = LegCost {
        half_spread_rate: half_spread_bps / BPS_PER_UNIT,
        market_slippage_rate: market_slippage_bps / BPS_PER_UNIT,
        stop: StopCost::Drawn {
            dist,
            stress_probability,
            stress_drop,
        },
        entry,
        adverse_selection_rate: adverse_selection_bps / BPS_PER_UNIT,
        leg_key: rng::leg_key(symbol),
        seed,
    };
    let record = LegCostRecord {
        symbol: symbol.to_string(),
        class,
        class_source,
        spread_source,
        slippage_source,
        half_spread_bps,
        market_slippage_bps,
        measured_p95_half_spread_bps: measured_p95,
        stop_slippage_bps: stop_bps,
        stop_slippage_mean_bps: dist.mean_bps(),
        stress_probability,
        stress_drop,
        entry: if is_maker { "maker" } else { "taker" },
        maker_fill_probability: is_maker.then_some(maker.fill_probability),
        maker_ttl_minutes: is_maker.then_some(ttl_minutes),
        adverse_selection_bps,
        decision_tf,
        entry_offset_bps,
    };
    Ok((cost, record))
}
