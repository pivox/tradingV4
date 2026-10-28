//! Simulateur partagé du portefeuille multi-paires (docs S9 §3).
//!
//! Un [`Book`] possède une jambe [`Simulator`] par paire et l'état commun : le cash, le compteur de lots
//! et le journal de trades. Il prête cet état par `mem::swap` (O(1), aucune allocation) à la seule jambe
//! qui agit : l'arithmétique d'une jambe est donc exactement celle du mono-paire, et le chemin de S8
//! (`Simulator` seul) n'est pas touché. Le quote que les autres jambes ont réservé pour leurs limits
//! d'achat est déduit du cash libre de la jambe qui agit (`foreign_reserved`, soustrait en dernier).
//!
//! Le `Book` ne connaît ni `strategy` ni `engine` : il reçoit des [`Instruction`] déjà converties. La
//! paire d'un trade est `trade_legs[i]` (le journal et `trade_legs` restent alignés) ; le `lot_id` est
//! unique sur tout le portefeuille.
//!
//! Invariants (vérifiés par `debug_assert!` après chaque opération sur une jambe, et à chaque minute par
//! [`Book::check_invariants`], sans effet en release) :
//! 1. `cash ≥ 0` et fini (spot, aucun emprunt) ;
//! 2. la somme des quotes réservés égale `total_reserved` et ne dépasse pas `cash × (1 + 1e-12)` ;
//! 3. jambes en position ou avec une entrée en attente ≤ `max_positions` ;
//! 4. mode recherche : somme des coûts d'entrée des lots ouverts ≤ `initial_cash × (1 + 1e-9)`, ce qui
//!    suppose le plafond `initial_cash / N` par entrée (le plafond de [`Book::submit`] s'applique à
//!    l'ordre : une grille de `Place` d'achat sur une même jambe n'est pas plafonnée au total) ;
//! 5. mode capitalisé : l'invariant 1 suffit, le notional cumulé est borné par le cash dépensé ;
//! 6. `trade_legs` et le journal de trades ont la même longueur.

use std::mem;

use serde::Serialize;

use crate::model::{Candle, FeeSchedule, InstrumentRules, Side, Slippage, Ts};

use super::orders::{OrderType, Origin};
use super::{
    Instruction, PendingView, PositionView, Rejection, SimCounters, SimDetail, SimError, SimRules,
    Simulator, SubmitContext, Trade,
};

/// Mode de sizing du portefeuille (docs S9 §7.1). Le `Book` s'en sert pour l'invariant 4 et pour refuser
/// `min_order_bump` hors mode capitalisé ; la base de sizing et le plafond par entrée sont décidés par
/// l'engine, qui les passe à [`Book::submit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SizingMode {
    /// Base = capital initial, plafond `initial_cash / N` par entrée.
    Research,
    /// Base = equity du portefeuille au signal, aucun plafond.
    Compounding,
}

/// Paramètres d'un [`Book`].
#[derive(Clone, Copy, Debug)]
pub struct BookConfig {
    pub fees: FeeSchedule,
    pub slippage: Slippage,
    pub rules: SimRules,
    pub initial_cash: f64,
    /// Nombre maximal de positions ouvertes plus entrées en attente (≥ 1).
    pub max_positions: u16,
    pub mode: SizingMode,
    /// Règle `min_order_bump` (docs S9 §7.3) : risque effectif maximal, en part de la base de sizing,
    /// d'une entrée portée au minimum d'ordre. Mode capitalisé seulement.
    pub min_order_bump_max_risk: Option<f64>,
    /// Capacité préallouée du journal de trades.
    pub trades_capacity: usize,
}

/// Refus d'une instruction par le [`Book`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookRejection {
    /// Refus de la jambe.
    Sim(Rejection),
    /// Positions ouvertes plus entrées en attente ≥ `max_positions`, décidé avant d'appeler la jambe.
    MaxPositions,
    /// Indice de jambe hors du portefeuille.
    UnknownLeg,
}

/// État partagé par toutes les jambes.
struct Shared {
    cash: f64,
    next_lot_id: u64,
    trades: Vec<Trade>,
}

/// Résultat final du portefeuille.
#[derive(Clone, Debug, PartialEq)]
pub struct BookOutcome {
    /// Journal de trades de tout le portefeuille, dans l'ordre de génération.
    pub trades: Vec<Trade>,
    /// Indice de la jambe de chaque trade, aligné sur `trades`.
    pub trade_legs: Vec<u16>,
    /// Cash final, positions soldées.
    pub cash: f64,
    /// Vrai si au moins une position a été soldée à la fin de la fenêtre.
    pub forced_close: bool,
    /// Compteurs de chaque jambe, dans l'ordre des jambes.
    pub counters: Vec<SimCounters>,
    pub details: Vec<SimDetail>,
    /// Entrées refusées par `max_positions`, par jambe.
    pub max_positions_rejected: Vec<u64>,
    /// Plus grand nombre de jambes en position observé.
    pub max_open_positions: u16,
    /// Plus grand rapport `Σ qty × prix moyen / initial_cash` des positions ouvertes.
    pub max_cost_to_initial: f64,
}

/// Simulateur partagé : une jambe par paire, un seul cash.
pub struct Book {
    legs: Vec<Simulator>,
    shared: Shared,
    trade_legs: Vec<u16>,
    /// Somme des `quote_reserved` des jambes, recalculée (jamais cumulée par différences) après chaque
    /// opération : aucune dérive.
    total_reserved: f64,
    max_positions: u16,
    mode: SizingMode,
    initial_cash: f64,
    max_positions_rejected: Vec<u64>,
    max_open_positions: u16,
    max_cost_to_initial: f64,
    forced_close: bool,
}

impl Book {
    /// Construit une jambe par instrument, dans l'ordre donné. Le cash des jambes est mis à zéro : seul
    /// `shared.cash` porte le capital.
    pub fn new(config: BookConfig, instruments: &[InstrumentRules]) -> Result<Book, SimError> {
        if instruments.is_empty() || instruments.len() > usize::from(u16::MAX) {
            return Err(SimError::InvalidRules {
                reason: "un portefeuille compte entre 1 et 65 535 jambes",
            });
        }
        if config.max_positions == 0 {
            return Err(SimError::InvalidRules {
                reason: "`max_positions` doit valoir au moins 1",
            });
        }
        if let Some(risk) = config.min_order_bump_max_risk {
            if !(risk.is_finite() && risk > 0.0 && risk <= 1.0) {
                return Err(SimError::InvalidRules {
                    reason: "le risque de `min_order_bump` doit être un nombre dans ]0, 1]",
                });
            }
            if config.mode != SizingMode::Compounding {
                return Err(SimError::InvalidRules {
                    reason: "`min_order_bump` n'est accepté qu'en mode capitalisé",
                });
            }
        }
        let mut legs = Vec::with_capacity(instruments.len());
        for instrument in instruments {
            let mut leg = Simulator::new(
                config.fees,
                config.slippage,
                config.rules,
                *instrument,
                config.initial_cash,
                0,
            )?;
            leg.cash = 0.0;
            leg.bump_max_risk_pct = config.min_order_bump_max_risk;
            legs.push(leg);
        }
        let count = legs.len();
        Ok(Book {
            legs,
            shared: Shared {
                cash: config.initial_cash,
                next_lot_id: 0,
                trades: Vec::with_capacity(config.trades_capacity),
            },
            trade_legs: Vec::with_capacity(config.trades_capacity),
            total_reserved: 0.0,
            max_positions: config.max_positions,
            mode: config.mode,
            initial_cash: config.initial_cash,
            max_positions_rejected: vec![0; count],
            max_open_positions: 0,
            max_cost_to_initial: 0.0,
            forced_close: false,
        })
    }

    /// Nombre de jambes.
    pub fn legs(&self) -> usize {
        self.legs.len()
    }

    // -----------------------------------------------------------------------------------------
    // Prêt de l'état partagé
    // -----------------------------------------------------------------------------------------

    /// Prête l'état partagé à la jambe `k`, appelle `f`, puis rend l'état. Aucune allocation.
    fn with_leg<R>(
        &mut self,
        k: usize,
        f: impl FnOnce(&mut Simulator) -> R,
    ) -> Result<R, SimError> {
        let legs = self.legs.len();
        if k >= legs {
            return Err(SimError::UnknownLeg { leg: k, legs });
        }
        // Somme des quotes réservés des autres jambes, dans l'ordre des jambes, depuis 0.0 : avec une
        // seule jambe elle vaut exactement 0.0 (le chemin mono-paire reste identique au bit).
        let mut foreign = 0.0;
        for (j, other) in self.legs.iter().enumerate() {
            if j != k {
                foreign += other.quote_reserved;
            }
        }
        let Some(leg) = self.legs.get_mut(k) else {
            return Err(SimError::UnknownLeg { leg: k, legs });
        };
        mem::swap(&mut self.shared.cash, &mut leg.cash);
        mem::swap(&mut self.shared.next_lot_id, &mut leg.next_lot_id);
        mem::swap(&mut self.shared.trades, &mut leg.trades);
        leg.foreign_reserved = foreign;
        let trades_before = leg.trades.len();
        let out = f(leg);
        leg.foreign_reserved = 0.0;
        let added = leg.trades.len().saturating_sub(trades_before);
        mem::swap(&mut self.shared.cash, &mut leg.cash);
        mem::swap(&mut self.shared.next_lot_id, &mut leg.next_lot_id);
        mem::swap(&mut self.shared.trades, &mut leg.trades);
        // Le nombre de jambes tient dans un `u16` (vérifié par `new`).
        let leg_index = u16::try_from(k).unwrap_or(u16::MAX);
        for _ in 0..added {
            self.trade_legs.push(leg_index);
        }
        let mut total = 0.0;
        for leg in &self.legs {
            total += leg.quote_reserved;
        }
        self.total_reserved = total;
        self.check_invariants();
        Ok(out)
    }

    /// Une jambe est occupée quand elle a une position ou une entrée en attente (`Enter`, ou `Place`
    /// d'achat) : elle compte alors pour une place de `max_positions`.
    fn occupied(leg: &Simulator) -> bool {
        leg.qty_lots > 0
            || leg.pending.iter().any(|order| {
                order.origin == Origin::Enter
                    || (order.origin == Origin::Place && order.kind == OrderType::LimitBuy)
            })
    }

    fn occupied_count(&self) -> usize {
        self.legs.iter().filter(|leg| Self::occupied(leg)).count()
    }

    /// Met à jour `max_open_positions` et `max_cost_to_initial` après un changement de quantité.
    fn update_stats(&mut self) {
        let mut open = 0u16;
        let mut cost = 0.0;
        for leg in &self.legs {
            if leg.qty_lots > 0 {
                open += 1;
                cost += leg.qty_of(leg.qty_lots) * leg.avg_price;
            }
        }
        if open > self.max_open_positions {
            self.max_open_positions = open;
        }
        let ratio = cost / self.initial_cash;
        if ratio > self.max_cost_to_initial {
            self.max_cost_to_initial = ratio;
        }
    }

    // -----------------------------------------------------------------------------------------
    // Opérations exposées au moteur
    // -----------------------------------------------------------------------------------------

    /// Déroule la 1m `candle` de la jambe `k` (annulations, fills, stops).
    pub fn on_bar(&mut self, k: usize, candle: &Candle) -> Result<(), SimError> {
        let changed = self.with_leg(k, |leg| {
            let before = leg.qty_lots;
            leg.on_bar(candle).map(|()| leg.qty_lots != before)
        })??;
        if changed {
            self.update_stats();
        }
        Ok(())
    }

    /// Soumet une instruction décidée à la clôture d'une 1m de la jambe `k`. `max_notional` est le
    /// plafond par entrée du mode recherche (`initial_cash / N`), `None` en mode capitalisé.
    ///
    /// Le refus `MaxPositions` est décidé ici, avant d'appeler la jambe : une entrée (`Enter`, ou `Place`
    /// d'achat) sur une jambe libre est refusée quand positions ouvertes plus entrées en attente ≥ N. Une
    /// jambe déjà occupée n'est pas concernée : c'est elle qui rend `EntryWhileLong`, comme en mono-paire.
    pub fn submit(
        &mut self,
        k: usize,
        instruction: Instruction,
        ctx: SubmitContext,
        max_notional: Option<f64>,
    ) -> Result<(), BookRejection> {
        let Some(leg) = self.legs.get(k) else {
            return Err(BookRejection::UnknownLeg);
        };
        let is_entry = matches!(
            instruction,
            Instruction::Enter { .. }
                | Instruction::Place {
                    side: Side::Buy,
                    ..
                }
        );
        if is_entry
            && !Self::occupied(leg)
            && self.occupied_count() >= usize::from(self.max_positions)
        {
            if let Some(hits) = self.max_positions_rejected.get_mut(k) {
                *hits += 1;
            }
            return Err(BookRejection::MaxPositions);
        }
        self.with_leg(k, |leg| leg.submit_capped(instruction, ctx, max_notional))
            .map_err(|_| BookRejection::UnknownLeg)?
            .map_err(BookRejection::Sim)
    }

    /// Sortie forcée de la jambe `k` pour trou de cotation à `exit_ms` (docs S9 §4.3). Rend faux si la
    /// jambe est à plat.
    pub fn force_exit_quote_gap(&mut self, k: usize, exit_ms: Ts) -> Result<bool, SimError> {
        let exited = self.with_leg(k, |leg| leg.force_exit_quote_gap(exit_ms))??;
        if exited {
            self.update_stats();
        }
        Ok(exited)
    }

    /// Equity du portefeuille : `cash + Σ qty × dernier close` de chaque jambe qui a vu une bougie. Avec
    /// une seule jambe, c'est `Simulator::equity_at(close)` au bit.
    pub fn equity(&self) -> f64 {
        let mut equity = self.shared.cash;
        for leg in &self.legs {
            if let Some(close) = leg.last_close {
                equity += leg.qty_of(leg.qty_lots) * close;
            }
        }
        equity
    }

    /// Cash du portefeuille.
    pub fn cash(&self) -> f64 {
        self.shared.cash
    }

    /// Cash moins le quote réservé par les limits d'achat de toutes les jambes.
    pub fn quote_free(&self) -> f64 {
        self.shared.cash - self.total_reserved
    }

    /// Position agrégée de la jambe `k`, `None` à plat (ou jambe inconnue).
    pub fn position(&self, k: usize) -> Option<PositionView> {
        self.legs.get(k).and_then(Simulator::position)
    }

    /// Quantité de la jambe `k` non réservée par des ventes limit (0.0 pour une jambe inconnue).
    pub fn qty_free(&self, k: usize) -> f64 {
        self.legs.get(k).map_or(0.0, Simulator::qty_free)
    }

    /// Ordres en attente de la jambe `k`, dans l'ordre d'arrivée (vide pour une jambe inconnue).
    pub fn pending(&self, k: usize) -> impl Iterator<Item = PendingView> + '_ {
        self.legs.get(k).into_iter().flat_map(Simulator::pending)
    }

    pub fn pending_len(&self, k: usize) -> usize {
        self.legs.get(k).map_or(0, Simulator::pending_len)
    }

    /// Close de la dernière 1m passée à `on_bar` pour la jambe `k`.
    pub fn last_close(&self, k: usize) -> Option<f64> {
        self.legs.get(k).and_then(|leg| leg.last_close)
    }

    pub fn counters(&self, k: usize) -> Option<&SimCounters> {
        self.legs.get(k).map(Simulator::counters)
    }

    pub fn detail(&self, k: usize) -> Option<&SimDetail> {
        self.legs.get(k).map(Simulator::detail)
    }

    /// Journal de trades et jambe de chaque trade (alignés).
    pub fn trades(&self) -> (&[Trade], &[u16]) {
        (&self.shared.trades, &self.trade_legs)
    }

    /// Entrées refusées par `max_positions`, toutes jambes confondues.
    pub fn positions_cap_hits(&self) -> u64 {
        self.max_positions_rejected.iter().sum()
    }

    /// Fin de fenêtre : les ordres en attente de chaque jambe sont abandonnés, et la position restante
    /// est soldée au close de la dernière bougie de la jambe (`last_per_leg[k]`). Une jambe sans bougie
    /// n'a rien à solder. `last_per_leg` compte une entrée par jambe.
    pub fn close_all_at_end(&mut self, last_per_leg: &[Option<Candle>]) -> Result<(), SimError> {
        let legs = self.legs.len();
        if last_per_leg.len() != legs {
            return Err(SimError::UnknownLeg {
                leg: last_per_leg.len(),
                legs,
            });
        }
        for (k, last) in last_per_leg.iter().enumerate() {
            let forced = self.with_leg(k, |leg| match last {
                Some(candle) => leg.close_at_end(candle),
                None => {
                    leg.drop_pending_at_end();
                    Ok(false)
                }
            })??;
            self.forced_close |= forced;
        }
        Ok(())
    }

    /// Consomme le portefeuille. À appeler après [`Book::close_all_at_end`].
    pub fn into_outcome(self) -> BookOutcome {
        BookOutcome {
            counters: self.legs.iter().map(|leg| leg.counters).collect(),
            details: self.legs.iter().map(|leg| leg.detail).collect(),
            trades: self.shared.trades,
            trade_legs: self.trade_legs,
            cash: self.shared.cash,
            forced_close: self.forced_close,
            max_positions_rejected: self.max_positions_rejected,
            max_open_positions: self.max_open_positions,
            max_cost_to_initial: self.max_cost_to_initial,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Invariants
    // -----------------------------------------------------------------------------------------

    /// Invariants du portefeuille (voir la doc du module). `debug_assert!` : sans effet en release. Appelé
    /// après chaque opération sur une jambe, et par le moteur à chaque minute de la grille.
    pub fn check_invariants(&self) {
        if !cfg!(debug_assertions) {
            return;
        }
        debug_assert!(
            self.shared.cash.is_finite() && self.shared.cash >= 0.0,
            "cash du portefeuille négatif ou non fini : {}",
            self.shared.cash
        );
        let mut sum = 0.0;
        let mut cost = 0.0;
        for leg in &self.legs {
            sum += leg.quote_reserved;
            cost += leg.qty_of(leg.qty_lots) * leg.avg_price;
            debug_assert!(
                leg.foreign_reserved == 0.0,
                "réservation étrangère laissée sur une jambe"
            );
        }
        debug_assert!(
            (sum - self.total_reserved).abs() <= 1e-9 * sum.abs().max(1.0),
            "total_reserved ({}) contredit la somme des jambes ({sum})",
            self.total_reserved
        );
        debug_assert!(
            self.total_reserved <= self.shared.cash * (1.0 + 1e-12),
            "les quotes réservés ({}) dépassent le cash ({})",
            self.total_reserved,
            self.shared.cash
        );
        debug_assert!(
            self.occupied_count() <= usize::from(self.max_positions),
            "{} jambes occupées pour max_positions = {}",
            self.occupied_count(),
            self.max_positions
        );
        if self.mode == SizingMode::Research {
            debug_assert!(
                cost <= self.initial_cash * (1.0 + 1e-9),
                "mode recherche : coût des positions ouvertes {cost} > capital initial {}",
                self.initial_cash
            );
        }
        debug_assert!(
            self.trade_legs.len() == self.shared.trades.len(),
            "trade_legs ({}) et journal de trades ({}) désalignés",
            self.trade_legs.len(),
            self.shared.trades.len()
        );
    }
}
