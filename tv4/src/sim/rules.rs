//! Règles du simulateur, partie du `config_hash` (docs/04-protocole-recherche.md §3.1, qui prime sur
//! docs/03-architecture.md §7.2 là où ils diffèrent, docs/03 §15.1 ligne 5).
//!
//! Chaque règle de fill est un champ : changer une règle change le `config_hash`, donc le résultat
//! n'est jamais comparé à un autre sans qu'on le sache.

use serde::Serialize;

use super::SimError;

/// Version des règles du simulateur, entre dans le `config_hash`.
pub const SIM_RULES_VERSION: &str = "s6.1";

/// Borne basse de `market_buy_cash_margin` : très au-dessus de l'ulp relative d'un f64 (~1,1e-16),
/// pour que `1.0 - m` et le plafond de cash gardent un sens.
pub const MIN_CASH_MARGIN: f64 = 1e-9;

/// Règle intrabar quand le stop et le take-profit sont touchés dans la même 1m (docs/04 §3.1 point 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntrabarRule {
    /// Le stop passe en premier (défaut, pessimiste).
    StopFirst,
    /// Le take-profit passe en premier (variante de sensibilité).
    TpFirst,
}

/// Règles du simulateur, partie du `config_hash`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SimRules {
    pub version: &'static str,
    /// Taille maximale de la file d'ordres en attente (docs/03 §7.1 point 5).
    pub max_pending: usize,
    /// Fraction du cash laissée libre sur tout achat market (docs/03 §2, ligne Capital).
    pub market_buy_cash_margin: f64,
    /// Délai d'activation d'un ordre, en minutes après le signal : l'ordre décidé à la clôture de la 1m
    /// `i` est actif à `signal_ms + délai × 1 min`, soit l'open de la 1m `i + 2` sans trou (docs/04
    /// §3.1 point 1). 0 donne le fill à l'open de `i + 1` (S3).
    pub activation_delay_bars: u32,
    /// Nombre de ticks qu'une 1m doit franchir au-delà du niveau d'une limit pour la remplir (docs/04
    /// §3.1 point 2). 0 donne un fill au toucher, toléré pour les tests.
    pub limit_cross_ticks: u32,
    pub intrabar: IntrabarRule,
    /// Un take-profit touché dans la 1m où l'entrée a été remplie est ignoré (docs/04 §3.1 point 3). Pour une
    /// entrée limit au repos remplie en cours de 1m, il l'est toujours, même si ce drapeau est faux (le TP est
    /// posé après le fill).
    pub tp_ignored_on_fill_bar: bool,
    /// Une 1m à volume nul ne remplit rien : ni market, ni limit, ni stop, ni take-profit.
    pub zero_volume_no_fill: bool,
    /// Part maximale du volume 1m en quote (`volume × low`) qu'un achat peut représenter (0,5 %).
    pub max_volume_share: f64,
    /// Composante de stress du slippage de stop, en bps, ajoutée au slippage scalaire. 0 au jalon 1 :
    /// la distribution est dégénérée et déterministe, sa calibration est au jalon 1.5 (S10, issue #12).
    pub stop_stress_bps: f64,
}

impl Default for SimRules {
    fn default() -> SimRules {
        SimRules {
            version: SIM_RULES_VERSION,
            max_pending: 64,
            market_buy_cash_margin: 0.05,
            activation_delay_bars: 1,
            limit_cross_ticks: 1,
            intrabar: IntrabarRule::StopFirst,
            tp_ignored_on_fill_bar: true,
            zero_volume_no_fill: true,
            max_volume_share: 0.005,
            stop_stress_bps: 0.0,
        }
    }
}

impl SimRules {
    /// Contrôle des champs, dans l'ordre du tableau de la spécification ; la première erreur est
    /// rendue. La marge doit rester au-dessus de l'ulp d'un f64 : c'est elle qui garantit `cash >= 0`
    /// malgré les arrondis. Le contrôle `slippage_bps + stop_stress_bps < 10 000` est fait par
    /// [`Simulator::new`](super::Simulator::new), qui connaît le slippage.
    pub fn validate(&self) -> Result<(), SimError> {
        if self.version != SIM_RULES_VERSION {
            return Err(SimError::InvalidRules {
                reason: "version de règles inconnue (attendue : s6.1)",
            });
        }
        if !(1..=256).contains(&self.max_pending) {
            return Err(SimError::InvalidRules {
                reason: "`max_pending` doit être entre 1 et 256",
            });
        }
        let m = self.market_buy_cash_margin;
        if !(m.is_finite() && (MIN_CASH_MARGIN..1.0).contains(&m)) {
            return Err(SimError::InvalidRules {
                reason: "`market_buy_cash_margin` doit être un nombre fini dans [1e-9, 1[",
            });
        }
        if self.activation_delay_bars > 1440 {
            return Err(SimError::InvalidRules {
                reason: "`activation_delay_bars` doit être au plus 1440",
            });
        }
        if self.limit_cross_ticks > 1000 {
            return Err(SimError::InvalidRules {
                reason: "`limit_cross_ticks` doit être au plus 1000",
            });
        }
        if !(self.max_volume_share.is_finite() && self.max_volume_share > 0.0) {
            return Err(SimError::InvalidRules {
                reason: "`max_volume_share` doit être un nombre fini strictement positif",
            });
        }
        if !(self.stop_stress_bps.is_finite()
            && self.stop_stress_bps >= 0.0
            && self.stop_stress_bps < 10_000.0)
        {
            return Err(SimError::InvalidRules {
                reason: "`stop_stress_bps` doit être un nombre fini dans [0, 10 000[",
            });
        }
        Ok(())
    }
}
