//! Traduction des signaux de stratégie en ordres du simulateur (docs/03-architecture.md §6 et §7.2).
//!
//! S3 ne gère que l'entrée au marché dimensionnée au risque et la sortie au marché. Tout le reste est
//! rejeté et compté, jamais ignoré en silence : un signal accepté sans être exécuté fausserait le
//! résultat.

use serde::Serialize;

use crate::model::Ts;
use crate::sim::{MarketOrder, MarketOrderKind, SimCounters, Simulator, sizing};
use crate::strategy::{OrderKind, Signal, Size};

/// Rejets de signaux par motif, tous des `u64` nommés (jamais une map).
///
/// Les quatre rejets du simulateur (`entry_while_long`, `exit_without_position`, `duplicate_exit`,
/// `pending_full`) sont recopiés de [`SimCounters`] en fin de run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SignalRejections {
    pub limit_order_unsupported: u64,
    pub take_profit_unsupported: u64,
    pub size_unsupported: u64,
    pub risk_without_stop: u64,
    pub invalid_risk_sizing: u64,
    pub move_stop_unsupported: u64,
    pub cancel_pending_unsupported: u64,
    pub place_limit_unsupported: u64,
    pub cancel_slot_unsupported: u64,
    pub entry_while_long: u64,
    pub exit_without_position: u64,
    pub duplicate_exit: u64,
    pub pending_full: u64,
}

impl SignalRejections {
    /// Somme de tous les motifs.
    pub fn total(&self) -> u64 {
        self.limit_order_unsupported
            + self.take_profit_unsupported
            + self.size_unsupported
            + self.risk_without_stop
            + self.invalid_risk_sizing
            + self.move_stop_unsupported
            + self.cancel_pending_unsupported
            + self.place_limit_unsupported
            + self.cancel_slot_unsupported
            + self.entry_while_long
            + self.exit_without_position
            + self.duplicate_exit
            + self.pending_full
    }

    /// Recopie les rejets du simulateur (source de vérité pour ces quatre motifs).
    pub fn with_sim_counters(mut self, sim: &SimCounters) -> SignalRejections {
        self.entry_while_long = sim.rejected_entry_while_long;
        self.exit_without_position = sim.rejected_exit_without_position;
        self.duplicate_exit = sim.rejected_duplicate_exit;
        self.pending_full = sim.rejected_pending_full;
        self
    }
}

/// Contexte de la bougie qui a émis le signal.
#[derive(Clone, Copy, Debug)]
pub struct SignalContext {
    /// Close de la bougie du signal : prix de référence du sizing (seul prix connu à cet instant).
    pub ref_price: f64,
    /// Equity à la clôture de la bougie du signal.
    pub equity: f64,
    /// `now_ms` de la bougie du signal.
    pub signal_ms: Ts,
}

/// Traduit un signal et le soumet au simulateur. Rend vrai si l'ordre est accepté.
///
/// Les rejets du simulateur ne sont pas comptés ici : ils le sont dans [`SimCounters`].
pub fn dispatch(
    signal: &Signal,
    context: SignalContext,
    sim: &mut Simulator,
    rejections: &mut SignalRejections,
) -> bool {
    match *signal {
        Signal::EnterLong {
            size,
            kind,
            stop,
            take_profit,
            tag,
        } => {
            // Ordre des contrôles imposé : le type d'ordre, puis le TP, puis la taille, puis le stop.
            if matches!(kind, OrderKind::Limit { .. }) {
                rejections.limit_order_unsupported += 1;
                return false;
            }
            if take_profit.is_some() {
                rejections.take_profit_unsupported += 1;
                return false;
            }
            let pct = match size {
                Size::RiskPct(pct) => pct,
                Size::PctEquity(_) | Size::Quote(_) => {
                    rejections.size_unsupported += 1;
                    return false;
                }
            };
            let Some(stop) = stop else {
                rejections.risk_without_stop += 1;
                return false;
            };
            let Some(qty) = sizing::risk_qty(context.equity, pct, context.ref_price, stop) else {
                rejections.invalid_risk_sizing += 1;
                return false;
            };
            sim.submit(MarketOrder {
                kind: MarketOrderKind::Buy { qty },
                signal_ms: context.signal_ms,
                tag,
                inactive_stop: Some(stop),
            })
            .is_ok()
        }
        Signal::ExitLong { kind, tag } => {
            if matches!(kind, OrderKind::Limit { .. }) {
                rejections.limit_order_unsupported += 1;
                return false;
            }
            sim.submit(MarketOrder {
                kind: MarketOrderKind::SellAll,
                signal_ms: context.signal_ms,
                tag,
                inactive_stop: None,
            })
            .is_ok()
        }
        Signal::MoveStop { .. } => {
            rejections.move_stop_unsupported += 1;
            false
        }
        Signal::CancelPending => {
            rejections.cancel_pending_unsupported += 1;
            false
        }
        Signal::PlaceLimit { .. } => {
            rejections.place_limit_unsupported += 1;
            false
        }
        Signal::CancelSlot { .. } => {
            rejections.cancel_slot_unsupported += 1;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExchangeId, FeeSchedule, Side, Slippage};
    use crate::sim::SimRules;

    fn simulator() -> Simulator {
        let fees = FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap();
        Simulator::new(fees, Slippage::ZERO, SimRules::default(), 1000.0, 4).unwrap()
    }

    fn context() -> SignalContext {
        SignalContext {
            ref_price: 100.0,
            equity: 1000.0,
            signal_ms: 60_000,
        }
    }

    fn enter(size: Size, kind: OrderKind, stop: Option<f64>, tp: Option<f64>) -> Signal {
        Signal::EnterLong {
            size,
            kind,
            stop,
            take_profit: tp,
            tag: "t",
        }
    }

    fn limit() -> OrderKind {
        OrderKind::Limit {
            price: 99.0,
            ttl_bars: 3,
        }
    }

    /// Envoie un signal à un simulateur neuf et rend (accepté, rejets).
    fn run(signal: Signal) -> (bool, SignalRejections) {
        let mut sim = simulator();
        let mut rejections = SignalRejections::default();
        let accepted = dispatch(&signal, context(), &mut sim, &mut rejections);
        (accepted, rejections.with_sim_counters(&sim.counters()))
    }

    #[test]
    fn entree_limit_rejetee() {
        let (ok, r) = run(enter(Size::RiskPct(0.01), limit(), Some(98.0), None));
        assert!(!ok);
        assert_eq!(r.limit_order_unsupported, 1);
        assert_eq!(r.total(), 1);
    }

    #[test]
    fn take_profit_rejete() {
        let (ok, r) = run(enter(
            Size::RiskPct(0.01),
            OrderKind::Market,
            Some(98.0),
            Some(110.0),
        ));
        assert!(!ok);
        assert_eq!(r.take_profit_unsupported, 1);
        assert_eq!(r.total(), 1);
    }

    #[test]
    fn tailles_non_gerees_rejetees() {
        for size in [Size::PctEquity(0.2), Size::Quote(50.0)] {
            let (ok, r) = run(enter(size, OrderKind::Market, Some(98.0), None));
            assert!(!ok);
            assert_eq!(r.size_unsupported, 1);
            assert_eq!(r.total(), 1);
        }
    }

    #[test]
    fn risque_sans_stop_rejete() {
        let (ok, r) = run(enter(Size::RiskPct(0.01), OrderKind::Market, None, None));
        assert!(!ok);
        assert_eq!(r.risk_without_stop, 1);
        assert_eq!(r.total(), 1);
    }

    #[test]
    fn sizing_invalide_rejete() {
        // Stop au-dessus du prix de référence.
        let (ok, r) = run(enter(
            Size::RiskPct(0.01),
            OrderKind::Market,
            Some(101.0),
            None,
        ));
        assert!(!ok);
        assert_eq!(r.invalid_risk_sizing, 1);
        assert_eq!(r.total(), 1);
    }

    #[test]
    fn entree_valide_acceptee_puis_refusee_par_le_simulateur() {
        let mut sim = simulator();
        let mut r = SignalRejections::default();
        let signal = enter(Size::RiskPct(0.01), OrderKind::Market, Some(98.0), None);
        assert!(dispatch(&signal, context(), &mut sim, &mut r));
        assert!(sim.has_pending());
        // Deuxième achat : refusé par le simulateur (achat déjà en attente).
        assert!(!dispatch(&signal, context(), &mut sim, &mut r));
        let r = r.with_sim_counters(&sim.counters());
        assert_eq!(r.entry_while_long, 1);
        assert_eq!(r.total(), 1);
    }

    #[test]
    fn sortie_limit_rejetee() {
        let (ok, r) = run(Signal::ExitLong {
            kind: limit(),
            tag: "t",
        });
        assert!(!ok);
        assert_eq!(r.limit_order_unsupported, 1);
        assert_eq!(r.total(), 1);
    }

    #[test]
    fn sortie_sans_position_refusee_par_le_simulateur() {
        let (ok, r) = run(Signal::ExitLong {
            kind: OrderKind::Market,
            tag: "t",
        });
        assert!(!ok);
        assert_eq!(r.exit_without_position, 1);
        assert_eq!(r.total(), 1);
    }

    #[test]
    fn signaux_non_geres() {
        let (_, r) = run(Signal::MoveStop { stop: 95.0 });
        assert_eq!((r.move_stop_unsupported, r.total()), (1, 1));
        let (_, r) = run(Signal::CancelPending);
        assert_eq!((r.cancel_pending_unsupported, r.total()), (1, 1));
        let (_, r) = run(Signal::PlaceLimit {
            side: Side::Buy,
            price: 99.0,
            size: Size::Quote(10.0),
            ttl_bars: 2,
            slot: 1,
            tag: "t",
        });
        assert_eq!((r.place_limit_unsupported, r.total()), (1, 1));
        let (_, r) = run(Signal::CancelSlot { slot: 1 });
        assert_eq!((r.cancel_slot_unsupported, r.total()), (1, 1));
    }

    #[test]
    fn l_ordre_des_controles_est_celui_du_tableau() {
        // Limit + PctEquity : compté en `limit_order_unsupported`, pas en `size_unsupported`.
        let (_, r) = run(enter(Size::PctEquity(0.2), limit(), None, None));
        assert_eq!(r.limit_order_unsupported, 1);
        assert_eq!(r.size_unsupported, 0);
        // TP + PctEquity : le TP passe avant la taille.
        let (_, r) = run(enter(
            Size::PctEquity(0.2),
            OrderKind::Market,
            None,
            Some(110.0),
        ));
        assert_eq!(r.take_profit_unsupported, 1);
        assert_eq!(r.size_unsupported, 0);
    }
}
