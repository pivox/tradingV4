//! Croisement de deux moyennes mobiles simples, mono-timeframe (docs/03-architecture.md §6).
//!
//! Stratégie de démonstration de la tranche verticale : ce n'est pas une hypothèse de recherche
//! (aucune fiche pré-enregistrée, docs/04 §4.3).

use super::{
    BarContext, IndicatorKind, IndicatorSpec, OrderKind, Signal, Size, Strategy, StrategyError,
};
use crate::model::{MAX_INDICATOR_PERIOD, Timeframe};
use serde::{Deserialize, Serialize};

/// Identifiant dans le registre.
pub const ID: &str = "sma_cross";
/// Version de la logique, entre dans le `config_hash`.
pub const VERSION: &str = "1.0.0";
/// Période de l'ATR qui place le stop.
pub const ATR_PERIOD: usize = 14;
/// Fraction de l'equity risquée par trade.
pub const RISK_PCT: f64 = 0.01;

/// Paramètres lus depuis `config/strategies/sma_cross.toml`.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SmaCrossParams {
    pub tf: Timeframe,
    pub fast: usize,
    pub slow: usize,
    pub stop_atr_mult: f64,
}

/// Entre long quand la SMA rapide passe au-dessus de la lente, sort quand elle repasse dessous.
pub struct SmaCross {
    p: SmaCrossParams,
    /// JSON des paramètres, calculé une fois à la construction (une allocation par run).
    params_json: serde_json::Value,
    /// Écart `fast - slow` de la dernière bougie valide ; `None` en warmup.
    prev_diff: Option<f64>,
}

impl SmaCross {
    /// Valide les paramètres puis fige leur JSON.
    pub fn new(p: SmaCrossParams) -> Result<Self, StrategyError> {
        let invalid = |reason: String| StrategyError::InvalidParams { id: ID, reason };
        if p.fast < 1 {
            return Err(invalid("`fast` doit valoir au moins 1".to_string()));
        }
        if p.fast >= p.slow {
            return Err(invalid(format!(
                "`fast` ({}) doit être strictement inférieur à `slow` ({})",
                p.fast, p.slow
            )));
        }
        if p.slow > MAX_INDICATOR_PERIOD {
            return Err(invalid(format!(
                "`slow` ({}) dépasse le maximum {MAX_INDICATOR_PERIOD}",
                p.slow
            )));
        }
        // Contrôle avant `to_value`, qui change NaN en `null` sans erreur : un `nan` en TOML serait
        // sinon haché comme `null`.
        if !p.stop_atr_mult.is_finite() || p.stop_atr_mult <= 0.0 {
            return Err(invalid(format!(
                "`stop_atr_mult` ({}) doit être fini et strictement positif",
                p.stop_atr_mult
            )));
        }
        let params_json = serde_json::to_value(&p)
            .map_err(|source| StrategyError::ParamsJson { id: ID, source })?;
        Ok(SmaCross {
            p,
            params_json,
            prev_diff: None,
        })
    }
}

impl Strategy for SmaCross {
    fn id(&self) -> &'static str {
        ID
    }

    fn version(&self) -> &'static str {
        VERSION
    }

    fn params_json(&self) -> serde_json::Value {
        self.params_json.clone()
    }

    fn timeframes(&self) -> &[Timeframe] {
        std::slice::from_ref(&self.p.tf)
    }

    fn indicators(&self) -> Vec<IndicatorSpec> {
        vec![
            IndicatorSpec {
                name: "fast",
                tf: self.p.tf,
                kind: IndicatorKind::Sma {
                    period: self.p.fast,
                },
            },
            IndicatorSpec {
                name: "slow",
                tf: self.p.tf,
                kind: IndicatorKind::Sma {
                    period: self.p.slow,
                },
            },
            IndicatorSpec {
                name: "atr",
                tf: self.p.tf,
                kind: IndicatorKind::Atr { period: ATR_PERIOD },
            },
        ]
    }

    /// La SMA lente est valide à l'index `slow - 1` et l'ATR de Wilder d'origine à l'index 13 :
    /// `max(slow, 14)` bougies suffisent pour que tout soit non NaN.
    fn warmup_bars(&self) -> usize {
        self.p.slow.max(ATR_PERIOD)
    }

    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        let tf = self.p.tf;
        if !ctx.just_closed(tf) {
            return;
        }
        let (Some(&f), Some(&s), Some(&atr), Some(c)) = (
            ctx.ind(tf, "fast").last(),
            ctx.ind(tf, "slow").last(),
            ctx.ind(tf, "atr").last(),
            ctx.last(tf),
        ) else {
            return;
        };
        // Warmup : aucun signal, et le premier bar valide ne peut pas déclencher de croisement.
        if !(f.is_finite() && s.is_finite() && atr.is_finite()) {
            self.prev_diff = None;
            return;
        }
        let d = f - s;
        let stop = c.close - self.p.stop_atr_mult * atr;
        match (self.prev_diff, ctx.position().is_none()) {
            (Some(p), true) if p <= 0.0 && d > 0.0 => {
                // Un stop non positif n'a pas de sens pour un long spot : pas d'entrée.
                if stop > 0.0 {
                    out.push(Signal::EnterLong {
                        size: Size::RiskPct(RISK_PCT),
                        kind: OrderKind::Market,
                        stop: Some(stop),
                        take_profit: None,
                        tag: "cross_up",
                    });
                }
            }
            (Some(p), false) if p >= 0.0 && d < 0.0 => out.push(Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "cross_down",
            }),
            _ => {}
        }
        self.prev_diff = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Candle, ExchangeId, FeeSchedule};
    use crate::strategy::{BarState, Position, TfView};

    const T0: i64 = 1_704_067_200_000;
    const M15: i64 = 900_000;

    fn strategy() -> SmaCross {
        SmaCross::new(SmaCrossParams {
            tf: Timeframe::M15,
            fast: 20,
            slow: 50,
            stop_atr_mult: 2.0,
        })
        .unwrap()
    }

    /// Une bougie 15m de test par pas : close 100, valeurs inventées.
    fn candles(count: usize) -> Vec<Candle> {
        (0..count)
            .map(|i| Candle {
                open_time_ms: T0 + i as i64 * M15,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 1.0,
                trades: None,
            })
            .collect()
    }

    fn long_position() -> Position {
        Position {
            qty: 1.0,
            avg_price: 100.0,
            stop: None,
            take_profit: None,
            opened_at_ms: T0,
        }
    }

    /// Joue la stratégie bougie par bougie sur des colonnes écrites à la main ; `closed` avance d'une
    /// bougie à chaque pas. Rend les signaux de chaque pas. `flags[i]` dit si la 15m vient de clôturer.
    fn run(
        strategy: &mut SmaCross,
        fast: &[f64],
        slow: &[f64],
        atr: &[f64],
        position: Option<Position>,
        flags: Option<&[bool]>,
    ) -> Vec<Vec<Signal>> {
        let candles = candles(fast.len());
        let columns: [&[f64]; 3] = [fast, slow, atr];
        let names = ["fast", "slow", "atr"];
        let base = TfView::new(Timeframe::M15, &candles, &names, &columns).unwrap();
        let fees = FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap();
        let mut all = Vec::new();
        for i in 0..fast.len() {
            let now_ms = T0 + (i as i64 + 1) * M15;
            let views = [base.with_closed(i + 1).unwrap()];
            let state = BarState {
                now_ms,
                equity: 300.0,
                quote_free: 300.0,
                qty_free: 0.0,
                position,
                fees,
            };
            let closed = flags.is_none_or(|f| f[i]);
            let mask = if closed { Timeframe::M15.bit() } else { 0 };
            let ctx = BarContext::new("BTC-USDC", state, &[], &views, mask).unwrap();
            let mut out = Vec::new();
            strategy.on_bar(&ctx, &mut out);
            all.push(out);
        }
        all
    }

    const NAN: f64 = f64::NAN;

    #[test]
    fn warmup_nan_ne_donne_aucun_signal() {
        let mut s = strategy();
        let out = run(&mut s, &[NAN, NAN], &[NAN, NAN], &[NAN, NAN], None, None);
        assert!(out.iter().all(Vec::is_empty));
        assert_eq!(s.prev_diff, None);
    }

    #[test]
    fn le_premier_bar_valide_ne_signale_pas() {
        let mut s = strategy();
        let out = run(&mut s, &[NAN, 11.0], &[NAN, 10.0], &[NAN, 1.0], None, None);
        assert!(out.iter().all(Vec::is_empty), "d > 0 mais aucun p");
        assert_eq!(s.prev_diff, Some(1.0));
    }

    #[test]
    fn croisement_haussier_sans_position_entre_long() {
        let mut s = strategy();
        // d : -1 puis +1. close = 100, atr = 2,5 : stop = 100 - 2 × 2,5 = 95.
        let out = run(&mut s, &[9.0, 11.0], &[10.0, 10.0], &[2.5, 2.5], None, None);
        assert!(out[0].is_empty());
        assert_eq!(
            out[1],
            [Signal::EnterLong {
                size: Size::RiskPct(0.01),
                kind: OrderKind::Market,
                stop: Some(95.0),
                take_profit: None,
                tag: "cross_up",
            }]
        );
    }

    #[test]
    fn croisement_haussier_avec_position_ne_fait_rien() {
        let mut s = strategy();
        let out = run(
            &mut s,
            &[9.0, 11.0],
            &[10.0, 10.0],
            &[2.5, 2.5],
            Some(long_position()),
            None,
        );
        assert!(out.iter().all(Vec::is_empty));
    }

    #[test]
    fn croisement_baissier_avec_position_sort() {
        let mut s = strategy();
        let out = run(
            &mut s,
            &[11.0, 9.0],
            &[10.0, 10.0],
            &[2.5, 2.5],
            Some(long_position()),
            None,
        );
        assert!(out[0].is_empty());
        assert_eq!(
            out[1],
            [Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "cross_down"
            }]
        );
    }

    #[test]
    fn croisement_baissier_sans_position_ne_fait_rien() {
        let mut s = strategy();
        let out = run(&mut s, &[11.0, 9.0], &[10.0, 10.0], &[2.5, 2.5], None, None);
        assert!(out.iter().all(Vec::is_empty));
    }

    #[test]
    fn sans_cloture_aucun_signal_et_prev_diff_inchange() {
        let mut s = strategy();
        // Pas 0 : clôture, d = -1. Pas 1 : pas de clôture (d = +1 ignoré). Pas 2 : clôture, d = +1 :
        // le croisement est détecté à la bougie clôturée suivante.
        let flags = [true, false, true];
        let out = run(
            &mut s,
            &[9.0, 11.0, 11.0],
            &[10.0, 10.0, 10.0],
            &[2.5, 2.5, 2.5],
            None,
            Some(&flags),
        );
        assert!(out[0].is_empty() && out[1].is_empty());
        assert_eq!(out[2].len(), 1, "entrée au bar clôturé suivant");

        let mut s = strategy();
        let _ = run(
            &mut s,
            &[9.0, 11.0],
            &[10.0, 10.0],
            &[2.5, 2.5],
            None,
            Some(&[true, false]),
        );
        assert_eq!(
            s.prev_diff,
            Some(-1.0),
            "inchangé par la bougie sans clôture"
        );
    }

    #[test]
    fn nan_apres_des_valeurs_valides_remet_a_zero() {
        let mut s = strategy();
        // d = -1, NaN, puis +1 : le bar valide suivant le NaN ne peut pas croiser.
        let out = run(
            &mut s,
            &[9.0, NAN, 11.0],
            &[10.0, NAN, 10.0],
            &[2.5, NAN, 2.5],
            None,
            None,
        );
        assert!(out.iter().all(Vec::is_empty));
        assert_eq!(s.prev_diff, Some(1.0));
    }

    #[test]
    fn stop_non_positif_ne_donne_aucune_entree() {
        let mut s = strategy();
        // atr énorme : stop = 100 - 2 × 80 < 0.
        let out = run(
            &mut s,
            &[9.0, 11.0],
            &[10.0, 10.0],
            &[80.0, 80.0],
            None,
            None,
        );
        assert!(out.iter().all(Vec::is_empty));
        assert_eq!(s.prev_diff, Some(1.0), "prev_diff avance quand même");
    }

    #[test]
    fn un_ecart_nul_puis_positif_entre() {
        let mut s = strategy();
        let out = run(
            &mut s,
            &[10.0, 11.0],
            &[10.0, 10.0],
            &[2.5, 2.5],
            None,
            None,
        );
        assert!(out[0].is_empty());
        assert_eq!(out[1].len(), 1, "p = 0 <= 0 et d > 0");
    }

    #[test]
    fn parametres_et_declaration() {
        let s = strategy();
        assert_eq!(s.warmup_bars(), 50);
        let small = SmaCross::new(SmaCrossParams {
            tf: Timeframe::H1,
            fast: 2,
            slow: 5,
            stop_atr_mult: 1.0,
        })
        .unwrap();
        assert_eq!(small.warmup_bars(), 14, "max(slow, ATR_PERIOD)");
        assert_eq!(small.timeframes(), [Timeframe::H1]);
        assert_eq!(
            s.params_json(),
            serde_json::json!({"fast": 20, "slow": 50, "stop_atr_mult": 2.0, "tf": "15m"})
        );
    }
}
