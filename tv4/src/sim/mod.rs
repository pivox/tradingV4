//! Simulateur d'exécution spot, ordres market seulement (docs/03-architecture.md §2 et §7.2).
//!
//! Le module ne voit que `model` (règle vérifiée par `tests/architecture.rs` : `sim` ne dépend ni de
//! `strategy`, ni de `engine`, ni de `cli`). Il n'a ni horloge ni `chrono` : le temps est un `i64` en
//! millisecondes UTC. Portefeuille spot : pas de short, pas de levier, pas d'emprunt, `cash >= 0`.
//!
//! Cycle d'une bougie `i` (l'engine appelle dans cet ordre) : [`Simulator::on_open`] (fills des ordres
//! émis à `i−1` ou avant, au prix `open[i]`), [`Simulator::observe_intrabar`], puis, après la
//! stratégie, [`Simulator::submit`]. Un ordre soumis à `i` n'est donc vu que par l'`on_open` de
//! `i+1` (règle I3, docs/03 §7.5) : c'est structurel, il n'y a jamais de fill à `close[i]`.
//!
//! Écarts temporaires de S3 (codes repris dans `metrics.json`, champ `s3_deviations`) :
//!
//! | Code | Écart | Retour |
//! |---|---|---|
//! | `stops_inactive` | le stop d'une entrée n'est jamais exécuté ; ses franchissements sont comptés | S6 |
//! | `risk_sizing_on_signal_close` | sizing au risque calculé sur `close[i]` (fait par l'engine) | S6 |
//! | `market_buy_cash_margin` | achat plafonné à `cash × (1 − marge) / (px × (1 + taker))` | reste en S6 si validé |
//! | `no_lot_tick_min_size` | ni arrondi lot ou tick, ni `minSz` | S6 et jalon 1.5 |
//! | `activation_delay_0` | fill à `open[i+1]`, pas à l'ouverture suivante + 1 minute | `sim_rules` S6 |
//! | `no_volume_rule` | pas de règle de volume (volume 0, part du volume 1m) | S6 et jalon 1.5 |
//!
//! L'ancien écart `single_tf_1m` (moteur mono-timeframe côté `engine`) est levé en S5.
//!
//! La position restante en fin de données est soldée par une clôture forcée au dernier close, avec
//! slippage et frais taker (par prudence : la sortie serait réelle).

#![forbid(unsafe_code)]

mod fills;
mod orders;
mod portfolio;
pub mod sizing;

pub use orders::{MarketOrder, MarketOrderKind, Rejection};
pub use portfolio::{ExitReason, Portfolio, Trade};

use portfolio::EntryLeg;
use serde::Serialize;
use thiserror::Error;

use crate::model::{Candle, FeeSchedule, Slippage, Timeframe, Ts};

/// Version des règles du simulateur, entre dans le `config_hash`.
pub const SIM_RULES_VERSION: &str = "s3-market-only";

/// Borne basse de `market_buy_cash_margin` : très au-dessus de l'ulp relative d'un f64 (~1,1e-16),
/// pour que `1.0 - m` et le plafond de cash gardent un sens.
pub const MIN_CASH_MARGIN: f64 = 1e-9;

/// Erreurs du simulateur. Les violations d'invariants sont des erreurs, jamais corrigées en silence.
#[derive(Debug, Error)]
pub enum SimError {
    #[error("capital initial {value} invalide : un nombre fini strictement positif est attendu")]
    InvalidInitialCash { value: f64 },

    #[error("règles du simulateur invalides : {reason}")]
    InvalidRules { reason: &'static str },

    #[error("bougie {open_time_ms} : le cash serait {cash}, négatif ou non fini (invariant violé)")]
    NegativeCash { open_time_ms: Ts, cash: f64 },

    #[error("bougie {open_time_ms} : un montant du fill n'est pas un nombre fini")]
    NonFiniteAmount { open_time_ms: Ts },

    #[error(
        "bougie {open_time_ms} : l'état du portefeuille contredit l'ordre exécuté (invariant violé)"
    )]
    InvariantViolated { open_time_ms: Ts },
}

/// Mode des stops. Une seule variante en S3 : l'activation (S6) changera la sérialisation, donc le
/// `config_hash`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopMode {
    /// Le stop est mémorisé et ses franchissements comptés, jamais exécutés.
    #[default]
    Inactive,
}

/// Règles du simulateur, partie du `config_hash`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SimRules {
    pub version: &'static str,
    /// Taille maximale de la file d'ordres en attente (docs/03 §7.1 point 5).
    pub max_pending: usize,
    /// Fraction du cash laissée libre sur tout achat market (docs/03 §2, ligne Capital).
    pub market_buy_cash_margin: f64,
    pub stops: StopMode,
}

impl Default for SimRules {
    fn default() -> SimRules {
        SimRules {
            version: SIM_RULES_VERSION,
            max_pending: 64,
            market_buy_cash_margin: 0.05,
            stops: StopMode::Inactive,
        }
    }
}

impl SimRules {
    /// `max_pending >= 1` et marge finie dans `[MIN_CASH_MARGIN, 1[`. C'est la marge qui garantit
    /// `cash >= 0` malgré les arrondis : une marge nulle ou sous l'ulp relative d'un f64 (où
    /// `1.0 - m == 1.0`) ne garantirait rien, elle est refusée.
    pub fn validate(&self) -> Result<(), SimError> {
        if self.max_pending < 1 {
            return Err(SimError::InvalidRules {
                reason: "`max_pending` doit valoir au moins 1",
            });
        }
        let m = self.market_buy_cash_margin;
        if !(m.is_finite() && (MIN_CASH_MARGIN..1.0).contains(&m)) {
            return Err(SimError::InvalidRules {
                reason: "`market_buy_cash_margin` doit être un nombre fini dans [1e-9, 1[",
            });
        }
        Ok(())
    }
}

/// Compteurs du simulateur, tous des `u64` nommés (jamais une map).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SimCounters {
    /// Ordres exécutés (entrées, sorties et clôture forcée).
    pub fills: u64,
    pub entries_capped_by_cash: u64,
    pub fills_rejected_insufficient_cash: u64,
    pub stop_breaches_ignored: u64,
    pub orders_unfilled_at_end: u64,
    pub rejected_pending_full: u64,
    pub rejected_entry_while_long: u64,
    pub rejected_exit_without_position: u64,
    pub rejected_duplicate_exit: u64,
}

/// Résultat final du simulateur.
#[derive(Clone, Debug, PartialEq)]
pub struct SimOutcome {
    pub trades: Vec<Trade>,
    /// Cash final, position soldée.
    pub cash: f64,
    /// Quantité finale : toujours 0.0 (clôture forcée).
    pub qty: f64,
    pub forced_close: bool,
    pub counters: SimCounters,
}

/// Sortie d'une position : paramètres propres à chaque motif.
#[derive(Clone, Copy)]
struct Exit {
    signal_ms: Ts,
    ms: Ts,
    ref_price: f64,
    tag: &'static str,
    reason: ExitReason,
    /// Bougie citée dans les erreurs.
    at_ms: Ts,
}

/// Simulateur market. Toutes les allocations ont lieu dans [`Simulator::new`].
pub struct Simulator {
    taker: f64,
    slippage: Slippage,
    rules: SimRules,
    portfolio: Portfolio,
    pending: Vec<MarketOrder>,
    trades: Vec<Trade>,
    counters: SimCounters,
}

impl Simulator {
    /// Valide les règles et le capital, puis préalloue la file d'ordres et les trades.
    pub fn new(
        fees: FeeSchedule,
        slippage: Slippage,
        rules: SimRules,
        initial_cash: f64,
        trades_capacity: usize,
    ) -> Result<Simulator, SimError> {
        if !(initial_cash.is_finite() && initial_cash > 0.0) {
            return Err(SimError::InvalidInitialCash {
                value: initial_cash,
            });
        }
        rules.validate()?;
        Ok(Simulator {
            taker: fees.taker_rate(),
            slippage,
            rules,
            portfolio: Portfolio::new(initial_cash),
            pending: Vec::with_capacity(rules.max_pending),
            trades: Vec::with_capacity(trades_capacity),
            counters: SimCounters::default(),
        })
    }

    pub fn portfolio(&self) -> &Portfolio {
        &self.portfolio
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn counters(&self) -> SimCounters {
        self.counters
    }

    /// Equity à un prix donné : `cash + qty × close`.
    pub fn equity_at(&self, close: f64) -> f64 {
        self.portfolio.cash + self.portfolio.qty * close
    }

    /// Étape 5 : met un ordre en attente. Chaque refus incrémente son compteur et rend l'erreur.
    pub fn submit(&mut self, order: MarketOrder) -> Result<(), Rejection> {
        if self.pending.len() >= self.rules.max_pending {
            self.counters.rejected_pending_full += 1;
            return Err(Rejection::PendingFull);
        }
        match order.kind {
            MarketOrderKind::Buy { .. } => {
                let buy_pending = self
                    .pending
                    .iter()
                    .any(|o| matches!(o.kind, MarketOrderKind::Buy { .. }));
                if self.portfolio.is_long() || buy_pending {
                    self.counters.rejected_entry_while_long += 1;
                    return Err(Rejection::EntryWhileLong);
                }
            }
            MarketOrderKind::SellAll => {
                // Pas de short : la vente se juge sur la position à l'instant du signal, même si un
                // achat est en attente.
                if !self.portfolio.is_long() {
                    self.counters.rejected_exit_without_position += 1;
                    return Err(Rejection::ExitWithoutPosition);
                }
                if self
                    .pending
                    .iter()
                    .any(|o| matches!(o.kind, MarketOrderKind::SellAll))
                {
                    self.counters.rejected_duplicate_exit += 1;
                    return Err(Rejection::DuplicateExit);
                }
            }
        }
        self.pending.push(order);
        Ok(())
    }

    /// Étape 1 : exécute tous les ordres en attente (FIFO) à `open[i]`, puis vide la file.
    ///
    /// `clear()` et non `mem::take` : la capacité est conservée, le `push` suivant n'alloue pas.
    pub fn on_open(&mut self, candle: &Candle) -> Result<(), SimError> {
        let mut k = 0;
        while let Some(&order) = self.pending.get(k) {
            match order.kind {
                MarketOrderKind::Buy { qty } => self.fill_buy(order, qty, candle)?,
                MarketOrderKind::SellAll => self.fill_sell(order, candle)?,
            }
            k += 1;
        }
        self.pending.clear();
        Ok(())
    }

    /// Étape 2 : en S3, rien n'est exécuté. Un plus bas qui touche le stop inactif marque la position
    /// (une fois par trade) ; le compteur est incrémenté à la clôture du trade.
    pub fn observe_intrabar(&mut self, candle: &Candle) {
        if !self.portfolio.is_long() {
            return;
        }
        if let Some(leg) = self.portfolio.entry.as_mut()
            && let Some(stop) = leg.inactive_stop
            && candle.low <= stop
        {
            leg.stop_breached = true;
        }
    }

    /// Fin de run : les ordres encore en attente ne sont pas remplis (règle I3), la position restante
    /// est soldée au close de la dernière bougie (clôture forcée, avec slippage et frais taker).
    pub fn finish(mut self, last: &Candle) -> Result<SimOutcome, SimError> {
        self.counters.orders_unfilled_at_end += self.pending.len() as u64;
        self.pending.clear();
        let mut forced_close = false;
        if self.portfolio.is_long() {
            let end_ms = last.open_time_ms.saturating_add(Timeframe::M1.ms());
            self.close_position(Exit {
                signal_ms: end_ms,
                ms: end_ms,
                ref_price: last.close,
                tag: "forced_close",
                reason: ExitReason::ForcedClose,
                at_ms: last.open_time_ms,
            })?;
            forced_close = true;
        }
        Ok(SimOutcome {
            trades: self.trades,
            cash: self.portfolio.cash,
            qty: self.portfolio.qty,
            forced_close,
            counters: self.counters,
        })
    }

    /// Fill d'un achat à l'open de `candle`.
    fn fill_buy(
        &mut self,
        order: MarketOrder,
        requested: f64,
        candle: &Candle,
    ) -> Result<(), SimError> {
        if self.portfolio.is_long() {
            // Inatteignable en S3 : `submit` refuse un achat en position. Une erreur, jamais un
            // compteur de rejet : le signal a déjà été compté comme accepté au `submit`.
            debug_assert!(false, "achat exécuté alors que la position est longue");
            return Err(SimError::InvariantViolated {
                open_time_ms: candle.open_time_ms,
            });
        }
        let at = candle.open_time_ms;
        let Some(fill) = fills::buy_fill(
            self.portfolio.cash,
            requested,
            candle.open,
            self.slippage,
            self.taker,
            self.rules.market_buy_cash_margin,
        ) else {
            self.counters.fills_rejected_insufficient_cash += 1;
            return Ok(());
        };
        if !(fill.cost.is_finite() && fill.fee.is_finite() && fill.slippage.is_finite()) {
            return Err(SimError::NonFiniteAmount { open_time_ms: at });
        }
        let cash = self.portfolio.cash - (fill.cost + fill.fee);
        check_cash(cash, at)?;
        self.portfolio.cash = cash;
        self.portfolio.qty = fill.qty;
        self.portfolio.avg_price = fill.price;
        self.portfolio.opened_at_ms = at;
        self.portfolio.entry = Some(EntryLeg {
            signal_ms: order.signal_ms,
            ref_price: candle.open,
            price: fill.price,
            cost: fill.cost,
            fee: fill.fee,
            slippage: fill.slippage,
            tag: order.tag,
            inactive_stop: order.inactive_stop,
            stop_breached: false,
            capped_by_cash: fill.capped,
        });
        self.counters.fills += 1;
        if fill.capped {
            self.counters.entries_capped_by_cash += 1;
        }
        debug_assert!(self.portfolio.qty >= 0.0);
        Ok(())
    }

    /// Fill d'une vente de toute la position à l'open de `candle`.
    fn fill_sell(&mut self, order: MarketOrder, candle: &Candle) -> Result<(), SimError> {
        if !self.portfolio.is_long() {
            // Inatteignable en S3 : `submit` refuse une vente à plat. Une erreur, jamais un
            // compteur de rejet (voir `fill_buy`). En S6, un stop exécuté en intrabar pourra
            // fermer la position avant une vente en attente : il faudra alors un compteur distinct.
            debug_assert!(false, "vente exécutée alors que la position est à plat");
            return Err(SimError::InvariantViolated {
                open_time_ms: candle.open_time_ms,
            });
        }
        self.close_position(Exit {
            signal_ms: order.signal_ms,
            ms: candle.open_time_ms,
            ref_price: candle.open,
            tag: order.tag,
            reason: ExitReason::Signal,
            at_ms: candle.open_time_ms,
        })
    }

    /// Solde la position au prix de référence `exit.ref_price` et enregistre le trade.
    fn close_position(&mut self, exit: Exit) -> Result<(), SimError> {
        let Some(leg) = self.portfolio.entry else {
            // Position longue sans jambe d'entrée : état incohérent, jamais corrigé en silence.
            debug_assert!(false, "position longue sans jambe d'entrée");
            return Err(SimError::InvariantViolated {
                open_time_ms: exit.at_ms,
            });
        };
        let qty = self.portfolio.qty;
        let fill = fills::sell_fill(qty, exit.ref_price, self.slippage, self.taker);
        if !(fill.proceeds.is_finite() && fill.fee.is_finite() && fill.slippage.is_finite()) {
            return Err(SimError::NonFiniteAmount {
                open_time_ms: exit.at_ms,
            });
        }
        let net_proceeds = fill.proceeds - fill.fee;
        let cash = self.portfolio.cash + net_proceeds;
        check_cash(cash, exit.at_ms)?;
        let pnl_net = net_proceeds - (leg.cost + leg.fee);
        self.trades.push(Trade {
            entry_signal_ms: leg.signal_ms,
            entry_ms: self.portfolio.opened_at_ms,
            entry_ref_price: leg.ref_price,
            entry_price: leg.price,
            entry_fee: leg.fee,
            entry_slippage: leg.slippage,
            exit_signal_ms: exit.signal_ms,
            exit_ms: exit.ms,
            exit_ref_price: exit.ref_price,
            exit_price: fill.price,
            exit_fee: fill.fee,
            exit_slippage: fill.slippage,
            qty,
            pnl_net,
            inactive_stop: leg.inactive_stop,
            stop_breached: leg.stop_breached,
            capped_by_cash: leg.capped_by_cash,
            exit_reason: exit.reason,
            entry_tag: leg.tag,
            exit_tag: exit.tag,
        });
        self.portfolio.cash = cash;
        // Affectation exacte : pas de reste flottant, pas de short.
        self.portfolio.qty = 0.0;
        self.portfolio.avg_price = 0.0;
        self.portfolio.entry = None;
        self.counters.fills += 1;
        if leg.stop_breached {
            self.counters.stop_breaches_ignored += 1;
        }
        Ok(())
    }
}

/// Invariant du portefeuille spot, vérifié en release après chaque fill.
fn check_cash(cash: f64, open_time_ms: Ts) -> Result<(), SimError> {
    if cash.is_finite() && cash >= 0.0 {
        Ok(())
    } else {
        Err(SimError::NegativeCash { open_time_ms, cash })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ExchangeId;
    use proptest::prelude::*;

    const T0: Ts = 1_704_067_200_000;

    fn okx() -> FeeSchedule {
        FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap()
    }

    fn bps(value: f64) -> Slippage {
        Slippage::from_bps(value).unwrap()
    }

    fn bar(index: i64, open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            open_time_ms: T0 + index * 60_000,
            open,
            high,
            low,
            close,
            volume: 1.0,
            trades: None,
        }
    }

    fn flat_bar(index: i64, price: f64) -> Candle {
        bar(index, price, price, price, price)
    }

    fn buy(qty: f64) -> MarketOrder {
        MarketOrder {
            kind: MarketOrderKind::Buy { qty },
            signal_ms: T0,
            tag: "entree",
            inactive_stop: None,
        }
    }

    fn sell() -> MarketOrder {
        MarketOrder {
            kind: MarketOrderKind::SellAll,
            signal_ms: T0 + 60_000,
            tag: "sortie",
            inactive_stop: None,
        }
    }

    fn simulator(cash: f64, slippage: Slippage) -> Simulator {
        Simulator::new(okx(), slippage, SimRules::default(), cash, 16).unwrap()
    }

    fn close_to(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn frais_et_slippage_a_la_main() {
        let mut sim = simulator(1000.0, bps(10.0));
        sim.submit(buy(1.0)).unwrap();
        sim.on_open(&flat_bar(1, 100.0)).unwrap();
        close_to(sim.portfolio().cash(), 899.54965);
        sim.submit(sell()).unwrap();
        sim.on_open(&flat_bar(2, 110.0)).unwrap();
        let out = sim.finish(&flat_bar(2, 110.0)).unwrap();
        let t = out.trades[0];
        close_to(t.entry_price, 100.1);
        close_to(t.entry_fee, 0.35035);
        close_to(t.exit_price, 109.89);
        close_to(t.exit_fee, 0.384615);
        close_to(out.cash, 1009.055035);
        close_to(t.pnl_net, 9.055035);
        close_to(t.entry_fee + t.exit_fee, 0.734965);
        close_to(t.entry_slippage + t.exit_slippage, 0.21);
        assert!(!t.capped_by_cash);
        assert_eq!(t.exit_reason, ExitReason::Signal);
        assert!(!out.forced_close);
        assert_eq!(out.counters.fills, 2);
    }

    #[test]
    fn le_fill_est_a_l_open_jamais_au_close() {
        let mut sim = simulator(1000.0, bps(10.0));
        sim.submit(buy(1.0)).unwrap();
        let candle = bar(1, 100.0, 103.0, 99.0, 102.0);
        sim.on_open(&candle).unwrap();
        sim.submit(sell()).unwrap();
        sim.on_open(&bar(2, 105.0, 106.0, 104.0, 105.5)).unwrap();
        let out = sim.finish(&flat_bar(2, 105.0)).unwrap();
        let t = out.trades[0];
        assert_eq!(t.entry_ref_price, 100.0);
        assert_eq!(t.entry_price, 100.0 * (1.0 + bps(10.0).rate()));
        assert_ne!(t.entry_price, candle.close);
        assert_eq!(t.entry_ms, candle.open_time_ms);
    }

    #[test]
    fn plafond_de_cash() {
        let fees = FeeSchedule::new(ExchangeId::Okx, 0, 0).unwrap();
        let mut sim = Simulator::new(fees, Slippage::ZERO, SimRules::default(), 100.0, 4).unwrap();
        sim.submit(buy(5.0)).unwrap();
        sim.on_open(&flat_bar(1, 100.0)).unwrap();
        close_to(sim.portfolio().qty(), 0.95);
        close_to(sim.portfolio().cash(), 5.0);
        assert_eq!(sim.counters().entries_capped_by_cash, 1);
    }

    #[test]
    fn rejets_de_submit() {
        // Achat déjà en attente.
        let mut sim = simulator(1000.0, Slippage::ZERO);
        sim.submit(buy(1.0)).unwrap();
        assert_eq!(sim.submit(buy(1.0)), Err(Rejection::EntryWhileLong));
        // Vente à plat, même avec un achat en attente.
        assert_eq!(sim.submit(sell()), Err(Rejection::ExitWithoutPosition));
        // Position longue : achat refusé, vente acceptée une seule fois.
        sim.on_open(&flat_bar(1, 100.0)).unwrap();
        assert_eq!(sim.submit(buy(1.0)), Err(Rejection::EntryWhileLong));
        sim.submit(sell()).unwrap();
        assert_eq!(sim.submit(sell()), Err(Rejection::DuplicateExit));
        let c = sim.counters();
        assert_eq!(c.rejected_entry_while_long, 2);
        assert_eq!(c.rejected_exit_without_position, 1);
        assert_eq!(c.rejected_duplicate_exit, 1);

        // File pleine.
        let rules = SimRules {
            max_pending: 1,
            ..SimRules::default()
        };
        let mut petit = Simulator::new(okx(), Slippage::ZERO, rules, 1000.0, 4).unwrap();
        petit.submit(buy(1.0)).unwrap();
        assert_eq!(petit.submit(buy(1.0)), Err(Rejection::PendingFull));
        assert_eq!(petit.counters().rejected_pending_full, 1);
    }

    #[test]
    fn regles_invalides() {
        let invalide = |rules: SimRules| {
            matches!(
                Simulator::new(okx(), Slippage::ZERO, rules, 100.0, 1),
                Err(SimError::InvalidRules { .. })
            )
        };
        for marge in [0.0, 1.0, f64::NAN, -0.1, 1.5, 1e-17, 1e-10] {
            let rules = SimRules {
                market_buy_cash_margin: marge,
                ..SimRules::default()
            };
            assert!(invalide(rules), "marge {marge}");
        }
        let rules = SimRules {
            max_pending: 0,
            ..SimRules::default()
        };
        assert!(invalide(rules));
        for cash in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let resultat = Simulator::new(okx(), Slippage::ZERO, SimRules::default(), cash, 1);
            assert!(
                matches!(resultat, Err(SimError::InvalidInitialCash { .. })),
                "capital {cash}"
            );
        }
    }

    #[test]
    fn pas_de_short() {
        let mut sim = simulator(1000.0, Slippage::ZERO);
        sim.submit(buy(1.0)).unwrap();
        sim.on_open(&flat_bar(1, 100.0)).unwrap();
        sim.submit(sell()).unwrap();
        sim.on_open(&flat_bar(2, 101.0)).unwrap();
        assert_eq!(sim.portfolio().qty(), 0.0);
        assert!(!sim.portfolio().is_long());
        assert_eq!(sim.submit(sell()), Err(Rejection::ExitWithoutPosition));
    }

    #[test]
    fn finish_sans_position_et_ordre_en_attente() {
        let mut sim = simulator(1000.0, Slippage::ZERO);
        sim.submit(buy(1.0)).unwrap();
        assert!(sim.has_pending());
        let out = sim.finish(&flat_bar(1, 100.0)).unwrap();
        assert_eq!(out.counters.orders_unfilled_at_end, 1);
        assert!(out.trades.is_empty());
        assert!(!out.forced_close);
        assert_eq!(out.cash, 1000.0);
    }

    #[test]
    fn finish_cloture_forcee() {
        let s = bps(10.0);
        let mut sim = simulator(1000.0, s);
        sim.submit(buy(1.0)).unwrap();
        sim.on_open(&flat_bar(1, 100.0)).unwrap();
        let last = bar(2, 100.0, 112.0, 99.0, 110.0);
        let out = sim.finish(&last).unwrap();
        assert!(out.forced_close);
        assert_eq!(out.qty, 0.0);
        let t = out.trades[0];
        assert_eq!(t.exit_reason, ExitReason::ForcedClose);
        assert_eq!(t.exit_tag, "forced_close");
        assert_eq!(t.exit_price, 110.0 * (1.0 - s.rate()));
        assert_eq!(t.exit_ref_price, 110.0);
        assert_eq!(t.exit_ms, last.open_time_ms + 60_000);
        assert_eq!(t.exit_signal_ms, last.open_time_ms + 60_000);
        assert_eq!(out.counters.fills, 2);
    }

    #[test]
    fn depassement_du_stop_ignore() {
        let mut sim = simulator(1000.0, Slippage::ZERO);
        let mut order = buy(1.0);
        order.inactive_stop = Some(95.0);
        sim.submit(order).unwrap();
        sim.on_open(&flat_bar(1, 100.0)).unwrap();
        sim.observe_intrabar(&bar(1, 100.0, 100.0, 96.0, 99.0));
        assert_eq!(sim.counters().stop_breaches_ignored, 0, "stop non touché");
        sim.observe_intrabar(&bar(2, 99.0, 99.0, 94.0, 96.0));
        sim.observe_intrabar(&bar(3, 96.0, 97.0, 93.0, 97.0));
        // Rien n'est exécuté : la position est intacte, le compteur attend la clôture du trade.
        assert!(sim.portfolio().is_long());
        assert_eq!(sim.counters().stop_breaches_ignored, 0);
        sim.submit(sell()).unwrap();
        sim.on_open(&flat_bar(4, 97.0)).unwrap();
        // La bougie de sortie ne compte pas : la position est fermée à son open.
        sim.observe_intrabar(&bar(4, 97.0, 97.0, 90.0, 92.0));
        let out = sim.finish(&flat_bar(4, 97.0)).unwrap();
        assert_eq!(out.counters.stop_breaches_ignored, 1);
        assert!(out.trades[0].stop_breached);
        assert_eq!(out.trades[0].inactive_stop, Some(95.0));
    }

    #[test]
    fn la_capacite_de_la_file_survit_aux_fills() {
        let mut sim = simulator(1000.0, Slippage::ZERO);
        let capacite = sim.pending.capacity();
        sim.submit(buy(1.0)).unwrap();
        sim.on_open(&flat_bar(1, 100.0)).unwrap();
        assert_eq!(sim.pending.capacity(), capacite);
    }

    #[derive(Clone, Debug)]
    enum Op {
        Buy(f64),
        Sell,
        Open(f64),
    }

    fn ops() -> impl Strategy<Value = Vec<Op>> {
        let op = prop_oneof![
            (1e-9f64..1e9).prop_map(Op::Buy),
            Just(Op::Sell),
            (1e-3f64..1e7).prop_map(Op::Open),
        ];
        proptest::collection::vec(op, 1..50)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn cash_et_quantite_jamais_negatifs(
            taker in 0u32..=10_000,
            slippage in 0.0f64..9_999.9,
            margin in 1e-6f64..0.99,
            cash in 1e-3f64..1e9,
            operations in ops(),
        ) {
            let fees = FeeSchedule::new(ExchangeId::Okx, 0, taker).unwrap();
            let rules = SimRules { market_buy_cash_margin: margin, ..SimRules::default() };
            let mut sim = Simulator::new(
                fees, Slippage::from_bps(slippage).unwrap(), rules, cash, 8,
            ).unwrap();
            for (i, op) in operations.iter().enumerate() {
                let index = i as i64;
                match op {
                    Op::Buy(qty) => { let _ = sim.submit(buy(*qty)); }
                    Op::Sell => { let _ = sim.submit(sell()); }
                    Op::Open(price) => {
                        let result = sim.on_open(&flat_bar(index, *price));
                        prop_assert!(result.is_ok(), "{result:?}");
                        prop_assert!(sim.portfolio().cash().is_finite());
                        prop_assert!(sim.portfolio().cash() >= 0.0);
                        prop_assert!(sim.portfolio().qty() >= 0.0);
                    }
                }
            }
            let out = sim.finish(&flat_bar(100, 1.0));
            prop_assert!(out.is_ok(), "{out:?}");
        }
    }
}
