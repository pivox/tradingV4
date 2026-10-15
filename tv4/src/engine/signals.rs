//! Traduction des signaux de stratégie en instructions du simulateur (docs/03-architecture.md §6 et
//! §7.2).
//!
//! Le moteur ne décide d'aucun refus : il convertit champ à champ un [`Signal`] en
//! [`Instruction`](crate::sim::Instruction), le simulateur applique ses contrôles dans l'ordre de sa
//! spécification et rend un motif de refus, que le moteur compte. Un signal refusé est toujours compté,
//! jamais ignoré en silence.

use serde::Serialize;

use crate::sim::{Instruction, OrderPrice, Rejection, Simulator, SizeSpec, SubmitContext};
use crate::strategy::{OrderKind, Signal, Size};

/// Rejets de signaux par motif, tous des `u64` nommés (jamais une map). Un champ par variante de
/// [`Rejection`], dans l'ordre de la spécification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SignalRejections {
    pub pending_full: u64,
    pub entry_while_long: u64,
    pub exit_without_position: u64,
    pub duplicate_exit: u64,
    pub risk_without_stop: u64,
    pub invalid_risk_sizing: u64,
    pub invalid_size: u64,
    pub invalid_price: u64,
    pub invalid_ttl: u64,
    pub invalid_slot: u64,
    pub place_limit_risk_pct: u64,
    pub slot_already_open: u64,
    pub insufficient_quote: u64,
    pub sell_without_free_qty: u64,
    pub below_minimum: u64,
    pub move_stop_without_position: u64,
    pub cancel_unknown_slot: u64,
}

impl SignalRejections {
    /// Compte un refus du simulateur dans le champ de son motif.
    pub fn record(&mut self, rejection: Rejection) {
        let field = match rejection {
            Rejection::PendingFull => &mut self.pending_full,
            Rejection::EntryWhileLong => &mut self.entry_while_long,
            Rejection::ExitWithoutPosition => &mut self.exit_without_position,
            Rejection::DuplicateExit => &mut self.duplicate_exit,
            Rejection::RiskWithoutStop => &mut self.risk_without_stop,
            Rejection::InvalidRiskSizing => &mut self.invalid_risk_sizing,
            Rejection::InvalidSize => &mut self.invalid_size,
            Rejection::InvalidPrice => &mut self.invalid_price,
            Rejection::InvalidTtl => &mut self.invalid_ttl,
            Rejection::InvalidSlot => &mut self.invalid_slot,
            Rejection::PlaceLimitRiskPct => &mut self.place_limit_risk_pct,
            Rejection::SlotAlreadyOpen => &mut self.slot_already_open,
            Rejection::InsufficientQuote => &mut self.insufficient_quote,
            Rejection::SellWithoutFreeQty => &mut self.sell_without_free_qty,
            Rejection::BelowMinimum => &mut self.below_minimum,
            Rejection::MoveStopWithoutPosition => &mut self.move_stop_without_position,
            Rejection::CancelUnknownSlot => &mut self.cancel_unknown_slot,
        };
        *field += 1;
    }

    /// Somme de tous les motifs.
    pub fn total(&self) -> u64 {
        self.pending_full
            + self.entry_while_long
            + self.exit_without_position
            + self.duplicate_exit
            + self.risk_without_stop
            + self.invalid_risk_sizing
            + self.invalid_size
            + self.invalid_price
            + self.invalid_ttl
            + self.invalid_slot
            + self.place_limit_risk_pct
            + self.slot_already_open
            + self.insufficient_quote
            + self.sell_without_free_qty
            + self.below_minimum
            + self.move_stop_without_position
            + self.cancel_unknown_slot
    }
}

/// Contexte de la bougie qui a émis le signal (même contenu que
/// [`SubmitContext`](crate::sim::SubmitContext)).
pub type SignalContext = SubmitContext;

fn size_spec(size: Size) -> SizeSpec {
    match size {
        Size::PctEquity(pct) => SizeSpec::PctEquity(pct),
        Size::RiskPct(pct) => SizeSpec::RiskPct(pct),
        Size::Quote(quote) => SizeSpec::Quote(quote),
    }
}

fn order_price(kind: OrderKind) -> OrderPrice {
    match kind {
        OrderKind::Market => OrderPrice::Market,
        OrderKind::Limit { price, ttl_bars } => OrderPrice::Limit { price, ttl_bars },
    }
}

/// Convertit un signal en instruction, sans aucune logique de refus.
fn instruction(signal: &Signal) -> Instruction {
    match *signal {
        Signal::EnterLong {
            size,
            kind,
            stop,
            take_profit,
            tag,
        } => Instruction::Enter {
            size: size_spec(size),
            price: order_price(kind),
            stop,
            take_profit,
            tag,
        },
        Signal::ExitLong { kind, tag } => Instruction::Exit {
            price: order_price(kind),
            tag,
        },
        Signal::MoveStop { stop } => Instruction::MoveStop { stop },
        Signal::CancelPending => Instruction::CancelAll,
        Signal::PlaceLimit {
            side,
            price,
            size,
            ttl_bars,
            slot,
            tag,
        } => Instruction::Place {
            side,
            price,
            size: size_spec(size),
            ttl_bars,
            slot,
            tag,
        },
        Signal::CancelSlot { slot } => Instruction::CancelSlot { slot },
    }
}

/// Traduit un signal et le soumet au simulateur. Rend vrai si l'instruction est acceptée ; sinon le
/// motif du refus est compté dans `rejections`.
pub fn dispatch(
    signal: &Signal,
    ctx: SignalContext,
    sim: &mut Simulator,
    rejections: &mut SignalRejections,
) -> bool {
    match sim.submit(instruction(signal), ctx) {
        Ok(()) => true,
        Err(rejection) => {
            rejections.record(rejection);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExchangeId, FeeSchedule, InstrumentRules, Side, Slippage};
    use crate::sim::SimRules;

    fn simulator() -> Simulator {
        let fees = FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap();
        let instrument = InstrumentRules::new(0.01, 1e-8, 0.0, 0.0).unwrap();
        Simulator::new(
            fees,
            Slippage::ZERO,
            SimRules::default(),
            instrument,
            1000.0,
            4,
        )
        .unwrap()
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
        (accepted, rejections)
    }

    #[test]
    fn entrees_valides_acceptees() {
        let cas = [
            enter(Size::RiskPct(0.01), OrderKind::Market, Some(98.0), None),
            enter(Size::PctEquity(0.2), OrderKind::Market, None, None),
            enter(
                Size::Quote(50.0),
                OrderKind::Market,
                Some(98.0),
                Some(110.0),
            ),
            enter(Size::RiskPct(0.01), limit(), Some(98.0), None),
        ];
        for signal in cas {
            let (ok, r) = run(signal);
            assert!(ok, "{signal:?}");
            assert_eq!(r.total(), 0);
        }
    }

    #[test]
    fn risque_sans_stop_refuse() {
        let (ok, r) = run(enter(Size::RiskPct(0.01), OrderKind::Market, None, None));
        assert!(!ok);
        assert_eq!((r.risk_without_stop, r.total()), (1, 1));
    }

    #[test]
    fn sizing_invalide_refuse() {
        // Stop au-dessus du prix de référence.
        let (ok, r) = run(enter(
            Size::RiskPct(0.01),
            OrderKind::Market,
            Some(101.0),
            None,
        ));
        assert!(!ok);
        assert_eq!((r.invalid_risk_sizing, r.total()), (1, 1));
    }

    #[test]
    fn entree_acceptee_puis_refusee_car_deja_en_attente() {
        let mut sim = simulator();
        let mut r = SignalRejections::default();
        let signal = enter(Size::RiskPct(0.01), OrderKind::Market, Some(98.0), None);
        assert!(dispatch(&signal, context(), &mut sim, &mut r));
        assert_eq!(sim.pending_len(), 1);
        assert!(!dispatch(&signal, context(), &mut sim, &mut r));
        assert_eq!((r.entry_while_long, r.total()), (1, 1));
    }

    #[test]
    fn sortie_sans_position_refusee() {
        let (ok, r) = run(Signal::ExitLong {
            kind: OrderKind::Market,
            tag: "t",
        });
        assert!(!ok);
        assert_eq!((r.exit_without_position, r.total()), (1, 1));
        let (ok, r) = run(Signal::ExitLong {
            kind: limit(),
            tag: "t",
        });
        assert!(!ok);
        assert_eq!((r.exit_without_position, r.total()), (1, 1));
    }

    #[test]
    fn signaux_s6_acceptes_et_refus_comptes() {
        // Accepté : une limit de grille, puis l'annulation de son slot.
        let mut sim = simulator();
        let mut r = SignalRejections::default();
        let place = Signal::PlaceLimit {
            side: Side::Buy,
            price: 99.0,
            size: Size::Quote(10.0),
            ttl_bars: 2,
            slot: 1,
            tag: "t",
        };
        assert!(dispatch(&place, context(), &mut sim, &mut r));
        // Même slot : refusé.
        assert!(!dispatch(&place, context(), &mut sim, &mut r));
        assert_eq!(r.slot_already_open, 1);
        assert!(dispatch(
            &Signal::CancelSlot { slot: 1 },
            context(),
            &mut sim,
            &mut r
        ));
        assert!(!dispatch(
            &Signal::CancelSlot { slot: 7 },
            context(),
            &mut sim,
            &mut r
        ));
        assert_eq!(r.cancel_unknown_slot, 1);
        assert!(dispatch(
            &Signal::CancelPending,
            context(),
            &mut sim,
            &mut r
        ));
        // MoveStop sans position.
        assert!(!dispatch(
            &Signal::MoveStop { stop: 95.0 },
            context(),
            &mut sim,
            &mut r
        ));
        assert_eq!(r.move_stop_without_position, 1);
        // RiskPct refusé pour une limit de grille.
        let risk = Signal::PlaceLimit {
            side: Side::Buy,
            price: 99.0,
            size: Size::RiskPct(0.01),
            ttl_bars: 2,
            slot: 2,
            tag: "t",
        };
        assert!(!dispatch(&risk, context(), &mut sim, &mut r));
        assert_eq!(r.place_limit_risk_pct, 1);
        assert_eq!(r.total(), 4);
    }

    #[test]
    fn record_compte_chaque_motif_une_fois() {
        let tous = [
            Rejection::PendingFull,
            Rejection::EntryWhileLong,
            Rejection::ExitWithoutPosition,
            Rejection::DuplicateExit,
            Rejection::RiskWithoutStop,
            Rejection::InvalidRiskSizing,
            Rejection::InvalidSize,
            Rejection::InvalidPrice,
            Rejection::InvalidTtl,
            Rejection::InvalidSlot,
            Rejection::PlaceLimitRiskPct,
            Rejection::SlotAlreadyOpen,
            Rejection::InsufficientQuote,
            Rejection::SellWithoutFreeQty,
            Rejection::BelowMinimum,
            Rejection::MoveStopWithoutPosition,
            Rejection::CancelUnknownSlot,
        ];
        let mut r = SignalRejections::default();
        for (i, motif) in tous.into_iter().enumerate() {
            r.record(motif);
            assert_eq!(r.total(), i as u64 + 1, "{motif:?}");
        }
        // Chaque champ vaut exactement 1 : le JSON a 17 clés à 1.
        let json = serde_json::to_value(r).unwrap();
        let champs = json.as_object().unwrap();
        assert_eq!(champs.len(), 17);
        assert!(champs.values().all(|v| v == 1));
    }

    #[test]
    fn conversion_champ_a_champ() {
        let signal = enter(
            Size::PctEquity(0.5),
            OrderKind::Limit {
                price: 90.0,
                ttl_bars: 7,
            },
            Some(80.0),
            Some(120.0),
        );
        assert_eq!(
            instruction(&signal),
            Instruction::Enter {
                size: SizeSpec::PctEquity(0.5),
                price: OrderPrice::Limit {
                    price: 90.0,
                    ttl_bars: 7
                },
                stop: Some(80.0),
                take_profit: Some(120.0),
                tag: "t",
            }
        );
        assert_eq!(instruction(&Signal::CancelPending), Instruction::CancelAll);
    }
}
