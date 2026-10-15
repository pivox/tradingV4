//! Simulateur d'exécution spot complet (docs/03-architecture.md §2 et §7.2, règles de fill de
//! docs/04-protocole-recherche.md §3.1).
//!
//! Le module ne voit que `model` (règle vérifiée par `tests/architecture.rs` : `sim` ne dépend ni de
//! `strategy`, ni de `engine`, ni de `cli`). Il n'a ni horloge ni `chrono` : le temps est un `i64` en
//! millisecondes UTC. Portefeuille spot : pas de short, pas de levier, pas d'emprunt, `cash >= 0`.
//!
//! Cycle d'une 1m `j` (l'engine appelle dans cet ordre) : [`Simulator::on_bar`], qui déroule les étapes
//! ci-dessous, puis, après la stratégie, [`Simulator::submit`] pour chaque instruction. Une instruction
//! soumise à la clôture de la 1m `i` porte `signal_ms = open_time[i] + 60 000` et n'est active qu'à
//! `signal_ms + activation_delay_bars × 60 000` : avec les défauts, la première 1m qui peut la voir est
//! `i + 2` (règle I3, docs/03 §7.5). C'est structurel : un ordre posé pendant `i` n'existe pas pour
//! `on_bar(i)`.
//!
//! Étapes de `on_bar` (E6) :
//!
//! 0. annulations et expirations effectives, puis `MoveStop` effectif, puis résolution du prix à
//!    l'arrivée des limits qui deviennent actives (marketables ou au repos) ;
//! 1. volume nul : aucun fill (la 1m ne met à jour que MFE et MAE) ;
//! 2. ordres market actifs, dans l'ordre d'arrivée, à l'open ;
//! 3. MFE et MAE des lots ouverts ;
//! 4. limits d'achat actives : prises en taker si elles étaient marketables à l'arrivée et que l'open
//!    les franchit encore, sinon remplies à `L` en maker quand la 1m les traverse d'au moins
//!    `limit_cross_ticks` ticks ;
//! 5. stop, ventes limit et take-profit selon [`IntrabarRule`] (mêmes règles de prix pour les ventes
//!    limit et le take-profit) ;
//! 6. réservations recalculées, contrôle du cash, invariants de debug.
//!
//! Écarts d'implémentation (S6) à docs/03, détail dans docs/03 §7.2 :
//!
//! 1. Le sizing (`PctEquity`, `RiskPct`, `Quote`) et l'arrondi au lot sont dans `sim` : un fill a besoin
//!    d'une quantité en lots entiers.
//! 2. `CancelSlot`, `CancelAll` et `MoveStop` sont retardés comme les ordres : une annulation instantanée
//!    serait optimiste (l'ordre peut être rempli pendant la latence). Un slot reste occupé jusqu'à ce
//!    que son annulation soit effective.
//! 3. Un `Enter` limit est plafonné par le cash, comme un market ; un `Place` d'achat qui dépasse le
//!    quote libre est refusé (`InsufficientQuote`) : une grille ne doit pas voir ses niveaux rétrécir en
//!    silence. Un `Enter` market n'est refusé au signal que si le quote libre ne couvre pas un lot ; sinon son cash
//!    n'est jugé qu'au fill (plafond, ou abandon compté `buys_rejected_insufficient_cash`).
//! 4. La règle de volume (0,5 % de `volume × low`) ne refuse que les achats. Une sortie n'est jamais
//!    refusée, son dépassement est compté (`exits_over_volume_share`, écart
//!    `exit_volume_rule_counted_not_rejected`). La clôture forcée l'ignore.
//! 5. Le volume quote d'une 1m vaut `volume × low`, le choix le plus pessimiste.
//! 6. Un ordre se remplit en entier ou pas du tout dans une 1m. Une vente partielle de la position
//!    ferme les lots les plus anciens d'abord (FIFO) ; un trade est la part d'un lot fermée par une vente.
//! 7. `RiskPct` d'un market est calculé au fill, sur le prix de fill et l'equity du signal.
//! 8. Le slippage de stop est un scalaire en bps plus une composante de stress à 0 ([`SimRules`]) : pas
//!    de distribution au jalon 1, aucun générateur aléatoire dans le simulateur (calibration en S10).
//! 9. Les minimums d'instrument de Kraken sont à 0 : à valider (S9, issue #11).
//! 10. Une limit traversée d'un tick est remplie en entier : pas de modèle de file d'attente.
//! 11. Limits pessimistes (règles `s6.1`, proposées au propriétaire le 15/10 ; remplacent la règle de l'issue S6
//!     `min(L, open)` / `max(L, open)` en maker) : une limit s'applique à tout ordre limit, y compris le
//!     take-profit. Son prix de référence à l'arrivée `P_arr` est l'open de la 1m d'activation, ou le
//!     close de la dernière 1m antérieure si la minute manque. Marketable à l'arrivée (achat `P_arr < L`,
//!     vente `P_arr > L`), elle est prise en taker à la première 1m à volume dont l'open franchit encore
//!     `L`, à `min(open × (1 + s), L)` (`max(open × (1 − s), L)` en vente) ; si l'open ne la franchit
//!     plus, elle passe au repos. Au repos, elle est remplie à `L` exactement, en maker, par la première
//!     1m à volume qui la traverse d'au moins `limit_cross_ticks` ticks, même si l'open est déjà
//!     au-delà de `L`. Le prix d'un fill au repos peut donc sortir de `[low, high]` (gap).
//! 12. MFE et MAE d'un lot comptent les extrêmes de toute la 1m de fill, y compris ceux qui précèdent le
//!     fill d'une limit (MFE surestimé d'un lot ouvert par une limit d'achat) ou suivent la sortie d'un
//!     stop ou d'un take-profit. Convention de la spécification, gardée pour que l'oracle reste exact.
//!
//! Écarts de S3 levés : `stops_inactive`, `risk_sizing_on_signal_close`, `no_lot_tick_min_size`,
//! `activation_delay_0`, `no_volume_rule`. Reste `market_buy_cash_margin` (achat market plafonné à
//! `cash × (1 − marge) / (px × (1 + taker))`).
//!
//! Aucune allocation dans `on_bar` ni dans `submit` : la file d'ordres est préallouée à `max_pending`,
//! les lots à `LOTS_CAPACITY` (un dépassement fait grandir le `VecDeque`, ce qui reste exceptionnel).

#![forbid(unsafe_code)]

mod fills;
mod orders;
mod portfolio;
mod rules;
pub mod sizing;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;

use serde::Serialize;
use thiserror::Error;

use crate::model::instrument::{STEP_SNAP, ceil_steps, floor_steps};
use crate::model::{
    BPS_PER_UNIT, Candle, FeeSchedule, InstrumentRules, ModelError, Side, Slippage, Ts,
};

pub use orders::{Instruction, OrderPrice, PendingView, Rejection, SizeSpec, SubmitContext};
pub use portfolio::{ExitReason, Liquidity, PositionView, Trade};
pub use rules::{IntrabarRule, MIN_CASH_MARGIN, SIM_RULES_VERSION, SimRules};

use orders::{Arrival, Order, OrderType, Origin};
use portfolio::Lot;

/// Durée d'une 1m en millisecondes.
const MINUTE_MS: i64 = 60_000;

/// Capacité initiale de la file de lots FIFO.
const LOTS_CAPACITY: usize = 256;

/// Marge relative sur le cash pour le contrôle de réservation à la pose : absorbe l'erreur d'arrondi
/// des sommes de réservations sans jamais laisser passer un dépassement visible.
const RESERVE_SLACK: f64 = 1e-9;

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

    #[error(transparent)]
    Model(#[from] ModelError),
}

/// Compteurs du simulateur, tous des `u64` nommés (jamais une map).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SimCounters {
    /// Toute exécution : achats, ventes, stops, take-profits, clôture forcée.
    pub fills: u64,
    /// Achats et ventes market (hors stop et clôture forcée).
    pub fills_market: u64,
    /// Achats et ventes limit (hors take-profit).
    pub fills_limit: u64,
    pub stops_triggered: u64,
    pub take_profits_triggered: u64,
    pub entries_capped_by_cash: u64,
    pub buys_rejected_insufficient_cash: u64,
    pub buys_rejected_below_minimum: u64,
    pub buys_rejected_volume_share: u64,
    pub entries_invalid_risk_at_fill: u64,
    pub entries_dropped_while_long: u64,
    pub exits_dropped_flat: u64,
    pub exits_over_volume_share: u64,
    /// Fills de limit (achat, vente ou take-profit) exécutés en taker parce que l'ordre était
    /// marketable à son arrivée et que l'open franchissait encore sa limite.
    pub limits_marketable_at_arrival: u64,
    pub limits_expired: u64,
    pub orders_cancelled: u64,
    pub sells_cancelled_by_exit: u64,
    pub take_profits_ignored_fill_bar: u64,
    pub stop_tp_same_bar: u64,
    pub move_stops_applied: u64,
    pub move_stops_dropped_flat: u64,
    pub bars_zero_volume_blocked: u64,
    pub orders_unfilled_at_end: u64,
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

/// Achat rempli, à transformer en lot.
#[derive(Clone, Copy)]
struct Entry {
    lots: u64,
    price: f64,
    cost: f64,
    fee: f64,
    slippage: f64,
    ref_price: f64,
    liquidity: Liquidity,
    capped: bool,
}

/// Vente à exécuter sur les lots les plus anciens.
#[derive(Clone, Copy)]
struct Sale {
    lots: u64,
    price: f64,
    /// Taux de frais (maker ou taker).
    rate: f64,
    ref_price: f64,
    reason: ExitReason,
    signal_ms: Ts,
    /// `exit_ms` du trade : `open_time` de la 1m, ou clôture de la dernière 1m pour la clôture forcée.
    exit_ms: Ts,
    tag: &'static str,
}

/// Simulateur spot. Toutes les allocations ont lieu dans [`Simulator::new`].
pub struct Simulator {
    maker: f64,
    taker: f64,
    slippage: Slippage,
    stop_rate: f64,
    rules: SimRules,
    instrument: InstrumentRules,
    inv_tick: f64,
    inv_lot: f64,
    min_lots: u64,
    /// Franchissement requis en ticks, voir [`cross_threshold`].
    kt: f64,
    cash: f64,
    quote_reserved: f64,
    reserved_lots: u64,
    /// Vrai quand la file a changé depuis le dernier calcul des réservations (S6, passe C : le recalcul
    /// est une fonction pure de la file, il est inutile tant qu'elle n'a pas bougé).
    reservations_dirty: bool,
    qty_lots: u64,
    avg_price: f64,
    opened_at_ms: Ts,
    stop: Option<f64>,
    stop_signal_ms: Ts,
    take_profit: Option<f64>,
    tp_signal_ms: Ts,
    /// Instant à partir duquel le take-profit est « arrivé » : la 1m qui suit le fill de l'entrée (la 1m
    /// de fill elle-même si `tp_ignored_on_fill_bar` est faux).
    tp_active_from_ms: Ts,
    /// Situation du take-profit par rapport à son prix à l'arrivée.
    tp_arrival: Arrival,
    /// Vrai pour la 1m où une entrée limit AU REPOS (maker, remplie en cours de 1m) a armé son take-profit :
    /// le TP est posé après le fill, jamais avant l'achat, donc son touché dans cette 1m est ignoré quel que
    /// soit `tp_ignored_on_fill_bar` (lecture pessimiste de l'ambiguïté du TP de la 1m de fill).
    tp_blocked_fill_bar: bool,
    /// Close de la dernière 1m passée à `on_bar` : prix à l'arrivée d'un ordre dont la minute
    /// d'activation manque dans la série.
    last_close: Option<f64>,
    /// `(niveau, effectif à partir de, signal_ms)` d'un `MoveStop` pas encore effectif.
    stop_update: Option<(f64, Ts, Ts)>,
    pending: Vec<Order>,
    lots: VecDeque<Lot>,
    next_lot_id: u64,
    trades: Vec<Trade>,
    counters: SimCounters,
}

/// Seuil de franchissement d'une limit, en ticks : `limit_cross_ticks − STEP_SNAP`, pour qu'un
/// franchissement exact soit accepté malgré l'erreur de représentation. À 0 tick (toucher du niveau,
/// toléré pour les tests seulement) le seuil reste 0 : retrancher `STEP_SNAP` ferait remplir une limit
/// à un prix hors de `[low, high]`.
fn cross_threshold(cross_ticks: u32) -> f64 {
    if cross_ticks == 0 {
        0.0
    } else {
        f64::from(cross_ticks) - STEP_SNAP
    }
}

/// Un ordre limit est marketable à l'arrivée si le prix de référence franchit déjà sa limite (inégalité
/// stricte) : achat `p_arr < limite`, vente `p_arr > limite`.
fn arrival_state(side: Side, p_arr: f64, limit: f64) -> Arrival {
    let marketable = match side {
        Side::Buy => p_arr < limit,
        Side::Sell => p_arr > limit,
    };
    if marketable {
        Arrival::Marketable
    } else {
        Arrival::Resting
    }
}

/// Prix valide pour une instruction : fini et strictement positif.
fn valid_price(price: f64) -> bool {
    price.is_finite() && price > 0.0
}

/// Taille valide : finie et strictement positive, et au plus 1 pour une fraction d'equity.
fn valid_size(size: SizeSpec) -> bool {
    match size {
        SizeSpec::PctEquity(pct) | SizeSpec::RiskPct(pct) => {
            pct.is_finite() && pct > 0.0 && pct <= 1.0
        }
        SizeSpec::Quote(quote) => quote.is_finite() && quote > 0.0,
    }
}

impl Simulator {
    /// Valide les règles, l'instrument et le capital, puis préalloue la file d'ordres, les lots et les
    /// trades.
    pub fn new(
        fees: FeeSchedule,
        slippage: Slippage,
        rules: SimRules,
        instrument: InstrumentRules,
        initial_cash: f64,
        trades_capacity: usize,
    ) -> Result<Simulator, SimError> {
        if !(initial_cash.is_finite() && initial_cash > 0.0) {
            return Err(SimError::InvalidInitialCash {
                value: initial_cash,
            });
        }
        rules.validate()?;
        instrument.validate()?;
        if slippage.bps() + rules.stop_stress_bps >= BPS_PER_UNIT {
            return Err(SimError::InvalidRules {
                reason: "le slippage et `stop_stress_bps` cumulés doivent rester sous 10 000 bps",
            });
        }
        Ok(Simulator {
            maker: fees.maker_rate(),
            taker: fees.taker_rate(),
            slippage,
            stop_rate: (slippage.bps() + rules.stop_stress_bps) / BPS_PER_UNIT,
            rules,
            instrument,
            inv_tick: instrument.inv_tick(),
            inv_lot: instrument.inv_lot(),
            min_lots: instrument.min_lots(),
            kt: cross_threshold(rules.limit_cross_ticks),
            cash: initial_cash,
            quote_reserved: 0.0,
            reserved_lots: 0,
            reservations_dirty: false,
            qty_lots: 0,
            avg_price: 0.0,
            opened_at_ms: 0,
            stop: None,
            stop_signal_ms: 0,
            take_profit: None,
            tp_signal_ms: 0,
            tp_active_from_ms: 0,
            tp_arrival: Arrival::Unresolved,
            tp_blocked_fill_bar: false,
            last_close: None,
            stop_update: None,
            pending: Vec::with_capacity(rules.max_pending),
            lots: VecDeque::with_capacity(LOTS_CAPACITY),
            next_lot_id: 0,
            trades: Vec::with_capacity(trades_capacity),
            counters: SimCounters::default(),
        })
    }

    // -----------------------------------------------------------------------------------------
    // Lecture
    // -----------------------------------------------------------------------------------------

    /// Quantité de `lots` lots, en monnaie de base. Une division, jamais une multiplication par le pas.
    fn qty_of(&self, lots: u64) -> f64 {
        lots as f64 / self.inv_lot
    }

    /// Equity à un prix donné : `cash + qty × close`.
    pub fn equity_at(&self, close: f64) -> f64 {
        self.cash + self.qty_of(self.qty_lots) * close
    }

    pub fn cash(&self) -> f64 {
        self.cash
    }

    /// Cash moins le quote réservé par les limits d'achat en attente.
    pub fn quote_free(&self) -> f64 {
        self.cash - self.quote_reserved
    }

    /// Quantité détenue (monnaie de base), 0 à plat.
    pub fn qty(&self) -> f64 {
        self.qty_of(self.qty_lots)
    }

    /// Quantité moins celle réservée par les limits de vente en attente.
    pub fn qty_free(&self) -> f64 {
        self.qty_of(self.qty_lots.saturating_sub(self.reserved_lots))
    }

    /// Position agrégée, `None` à plat.
    pub fn position(&self) -> Option<PositionView> {
        (self.qty_lots > 0).then(|| PositionView {
            qty: self.qty_of(self.qty_lots),
            avg_price: self.avg_price,
            stop: self.stop,
            take_profit: self.take_profit,
            opened_at_ms: self.opened_at_ms,
        })
    }

    /// Ordres en attente, dans l'ordre d'arrivée.
    pub fn pending(&self) -> impl Iterator<Item = PendingView> + '_ {
        self.pending.iter().map(|order| PendingView {
            side: order.side(),
            limit: (!order.is_market()).then_some((order.limit, order.ttl_bars)),
            qty: if order.is_market() {
                0.0
            } else {
                self.qty_of(order.lots)
            },
            slot: order.slot,
            tag: order.tag,
            signal_ms: order.signal_ms,
            expires_at_ms: order.expires_at_ms,
        })
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn counters(&self) -> &SimCounters {
        &self.counters
    }

    // -----------------------------------------------------------------------------------------
    // Soumission
    // -----------------------------------------------------------------------------------------

    /// Lots minimums non respectés, ou notional minimum non atteint, au prix `price`.
    fn below_min(&self, lots: u64, price: f64) -> bool {
        lots == 0
            || lots < self.min_lots
            || self.qty_of(lots) * price < self.instrument.min_notional
    }

    /// Réservation d'un achat limit : notional plus frais maker estimés.
    fn reservation(&self, lots: u64, limit: f64) -> f64 {
        let cost = self.qty_of(lots) * limit;
        cost + cost * self.maker
    }

    /// Recalcule `quote_reserved` et `reserved_lots` depuis la file : une fonction pure de la file, que
    /// l'on peut appeler une fois de plus sans rien changer. Elle ne parcourt la file que si un ordre a
    /// été ajouté ou retiré depuis le dernier calcul (`reservations_dirty`) : le résultat est identique,
    /// et une file de 20 limits qui ne bouge pas ne coûte plus rien à chaque 1m. Tout site qui modifie
    /// la file lève le drapeau ; `check_invariants` recalcule à part et compare, ce qui attrape un oubli.
    fn recompute_reservations(&mut self) {
        if !self.reservations_dirty {
            return;
        }
        self.reservations_dirty = false;
        let mut quote = 0.0;
        let mut lots = 0u64;
        for order in &self.pending {
            match order.kind {
                OrderType::LimitBuy => quote += order.reserved_quote,
                OrderType::LimitSell => lots += order.lots,
                OrderType::MarketBuy | OrderType::MarketSell => {}
            }
        }
        self.quote_reserved = quote;
        self.reserved_lots = lots;
    }

    /// Instant d'activation d'une instruction signalée à `signal_ms`.
    fn activation(&self, signal_ms: Ts) -> Ts {
        signal_ms.saturating_add(i64::from(self.rules.activation_delay_bars) * MINUTE_MS)
    }

    /// Ajoute un ordre accepté en fin de file (sans réallocation : `submit` a vérifié la capacité).
    fn accept(&mut self, order: Order) -> Result<(), Rejection> {
        self.pending.push(order);
        self.reservations_dirty = true;
        self.recompute_reservations();
        if cfg!(debug_assertions) {
            self.check_invariants();
        }
        Ok(())
    }

    /// Soumet une instruction décidée à la clôture d'une 1m. Les contrôles se font dans l'ordre de la
    /// spécification ; le premier motif vrai est rendu.
    pub fn submit(
        &mut self,
        instruction: Instruction,
        ctx: SubmitContext,
    ) -> Result<(), Rejection> {
        match instruction {
            Instruction::Enter {
                size,
                price,
                stop,
                take_profit,
                tag,
            } => self.submit_enter(size, price, stop, take_profit, tag, ctx),
            Instruction::Exit { price, tag } => self.submit_exit(price, tag, ctx),
            Instruction::Place {
                side,
                price,
                size,
                ttl_bars,
                slot,
                tag,
            } => self.submit_place(side, price, size, ttl_bars, slot, tag, ctx),
            Instruction::MoveStop { stop } => {
                if !valid_price(stop) {
                    return Err(Rejection::InvalidPrice);
                }
                if self.qty_lots == 0 {
                    return Err(Rejection::MoveStopWithoutPosition);
                }
                self.stop_update = Some((stop, self.activation(ctx.signal_ms), ctx.signal_ms));
                Ok(())
            }
            Instruction::CancelAll => {
                let act = self.activation(ctx.signal_ms);
                for order in &mut self.pending {
                    if order.cancel_from_ms.is_none() {
                        order.cancel_from_ms = Some(act);
                    }
                }
                Ok(())
            }
            Instruction::CancelSlot { slot } => {
                if slot == 0 {
                    return Err(Rejection::InvalidSlot);
                }
                let act = self.activation(ctx.signal_ms);
                let Some(order) = self.pending.iter_mut().find(|o| o.slot == slot) else {
                    return Err(Rejection::CancelUnknownSlot);
                };
                if order.cancel_from_ms.is_none() {
                    order.cancel_from_ms = Some(act);
                }
                Ok(())
            }
        }
    }

    fn submit_enter(
        &mut self,
        size: SizeSpec,
        price: OrderPrice,
        stop: Option<f64>,
        take_profit: Option<f64>,
        tag: &'static str,
        ctx: SubmitContext,
    ) -> Result<(), Rejection> {
        if let OrderPrice::Limit { price, .. } = price
            && !valid_price(price)
        {
            return Err(Rejection::InvalidPrice);
        }
        if stop.is_some_and(|s| !valid_price(s)) || take_profit.is_some_and(|t| !valid_price(t)) {
            return Err(Rejection::InvalidPrice);
        }
        if let OrderPrice::Limit { ttl_bars, .. } = price
            && ttl_bars < 1
        {
            return Err(Rejection::InvalidTtl);
        }
        if !valid_size(size) {
            return Err(Rejection::InvalidSize);
        }
        if matches!(size, SizeSpec::RiskPct(_)) && stop.is_none() {
            return Err(Rejection::RiskWithoutStop);
        }
        if self.pending.len() >= self.rules.max_pending {
            return Err(Rejection::PendingFull);
        }
        if self.qty_lots > 0 || self.pending.iter().any(|o| o.origin == Origin::Enter) {
            return Err(Rejection::EntryWhileLong);
        }
        let act = self.activation(ctx.signal_ms);
        match price {
            OrderPrice::Market => {
                if sizing::size_qty(size, ctx.equity, ctx.ref_price, stop).is_none() {
                    return Err(Rejection::InvalidRiskSizing);
                }
                // Quote libre insuffisant pour un seul lot au prix de référence (tout le cash est réservé
                // par des limits d'achat) : le market serait abandonné au fill, autant le refuser au signal
                // pour que l'entonnoir soit juste. Un quote libre qui couvre au moins un lot plafonne l'achat
                // au fill (écart 3).
                let cap = (self.cash - self.quote_reserved)
                    * (1.0 - self.rules.market_buy_cash_margin)
                    / (ctx.ref_price * (1.0 + self.taker));
                if !matches!(floor_steps(cap, self.inv_lot), Some(k) if k > 0) {
                    return Err(Rejection::InsufficientQuote);
                }
                let mut order =
                    Order::base(Origin::Enter, OrderType::MarketBuy, tag, ctx.signal_ms, act);
                order.size = size;
                order.equity_at_signal = ctx.equity;
                order.stop = stop;
                order.take_profit = take_profit;
                self.accept(order)
            }
            OrderPrice::Limit { price, ttl_bars } => {
                let limit = match floor_steps(price, self.inv_tick) {
                    Some(k) if k > 0 => k as f64 / self.inv_tick,
                    _ => return Err(Rejection::InvalidPrice),
                };
                let Some(q) = sizing::size_qty(size, ctx.equity, limit, stop) else {
                    return Err(Rejection::InvalidRiskSizing);
                };
                let free = self.cash - self.quote_reserved;
                let cap =
                    free * (1.0 - self.rules.market_buy_cash_margin) / (limit * (1.0 + self.maker));
                let capped = cap < q;
                let qf = if capped { cap } else { q };
                if !(qf.is_finite() && qf > 0.0) {
                    return Err(Rejection::InsufficientQuote);
                }
                let Some(lots) = floor_steps(qf, self.inv_lot) else {
                    return Err(Rejection::InsufficientQuote);
                };
                if self.below_min(lots, limit) {
                    return Err(Rejection::BelowMinimum);
                }
                let reserved = self.reservation(lots, limit);
                if self.quote_reserved + reserved > self.cash * (1.0 - RESERVE_SLACK) {
                    return Err(Rejection::InsufficientQuote);
                }
                let mut order =
                    Order::base(Origin::Enter, OrderType::LimitBuy, tag, ctx.signal_ms, act);
                order.expires_at_ms = Some(act.saturating_add(i64::from(ttl_bars) * MINUTE_MS));
                order.limit = limit;
                order.ttl_bars = ttl_bars;
                order.lots = lots;
                order.reserved_quote = reserved;
                order.capped = capped;
                order.stop = stop;
                order.take_profit = take_profit;
                self.accept(order)
            }
        }
    }

    fn submit_exit(
        &mut self,
        price: OrderPrice,
        tag: &'static str,
        ctx: SubmitContext,
    ) -> Result<(), Rejection> {
        if let OrderPrice::Limit { price, ttl_bars } = price {
            if !valid_price(price) {
                return Err(Rejection::InvalidPrice);
            }
            if ttl_bars < 1 {
                return Err(Rejection::InvalidTtl);
            }
        }
        if self.pending.len() >= self.rules.max_pending {
            return Err(Rejection::PendingFull);
        }
        if self.qty_lots == 0 {
            return Err(Rejection::ExitWithoutPosition);
        }
        if self.pending.iter().any(|o| o.origin == Origin::Exit) {
            return Err(Rejection::DuplicateExit);
        }
        let act = self.activation(ctx.signal_ms);
        match price {
            OrderPrice::Market => {
                let order =
                    Order::base(Origin::Exit, OrderType::MarketSell, tag, ctx.signal_ms, act);
                self.accept(order)
            }
            OrderPrice::Limit { price, ttl_bars } => {
                let free = self.qty_lots.saturating_sub(self.reserved_lots);
                if free == 0 {
                    return Err(Rejection::SellWithoutFreeQty);
                }
                let limit = match ceil_steps(price, self.inv_tick) {
                    Some(k) if k > 0 => k as f64 / self.inv_tick,
                    _ => return Err(Rejection::InvalidPrice),
                };
                if self.below_min(free, limit) {
                    return Err(Rejection::BelowMinimum);
                }
                let mut order =
                    Order::base(Origin::Exit, OrderType::LimitSell, tag, ctx.signal_ms, act);
                order.expires_at_ms = Some(act.saturating_add(i64::from(ttl_bars) * MINUTE_MS));
                order.limit = limit;
                order.ttl_bars = ttl_bars;
                order.lots = free;
                self.accept(order)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_place(
        &mut self,
        side: Side,
        price: f64,
        size: SizeSpec,
        ttl_bars: u32,
        slot: u16,
        tag: &'static str,
        ctx: SubmitContext,
    ) -> Result<(), Rejection> {
        if !valid_price(price) {
            return Err(Rejection::InvalidPrice);
        }
        if ttl_bars < 1 {
            return Err(Rejection::InvalidTtl);
        }
        if !valid_size(size) {
            return Err(Rejection::InvalidSize);
        }
        if slot == 0 {
            return Err(Rejection::InvalidSlot);
        }
        if matches!(size, SizeSpec::RiskPct(_)) {
            return Err(Rejection::PlaceLimitRiskPct);
        }
        if self.pending.len() >= self.rules.max_pending {
            return Err(Rejection::PendingFull);
        }
        if self.pending.iter().any(|o| o.slot == slot) {
            return Err(Rejection::SlotAlreadyOpen);
        }
        let act = self.activation(ctx.signal_ms);
        let expires = act.saturating_add(i64::from(ttl_bars) * MINUTE_MS);
        match side {
            Side::Buy => {
                let limit = match floor_steps(price, self.inv_tick) {
                    Some(k) if k > 0 => k as f64 / self.inv_tick,
                    _ => return Err(Rejection::InvalidPrice),
                };
                let Some(q) = sizing::size_qty(size, ctx.equity, limit, None) else {
                    return Err(Rejection::InvalidSize);
                };
                let Some(lots) = floor_steps(q, self.inv_lot) else {
                    return Err(Rejection::InsufficientQuote);
                };
                if self.below_min(lots, limit) {
                    return Err(Rejection::BelowMinimum);
                }
                let reserved = self.reservation(lots, limit);
                if self.quote_reserved + reserved > self.cash * (1.0 - RESERVE_SLACK) {
                    return Err(Rejection::InsufficientQuote);
                }
                let mut order =
                    Order::base(Origin::Place, OrderType::LimitBuy, tag, ctx.signal_ms, act);
                order.slot = slot;
                order.expires_at_ms = Some(expires);
                order.limit = limit;
                order.ttl_bars = ttl_bars;
                order.lots = lots;
                order.reserved_quote = reserved;
                self.accept(order)
            }
            Side::Sell => {
                let free = self.qty_lots.saturating_sub(self.reserved_lots);
                if free == 0 {
                    return Err(Rejection::SellWithoutFreeQty);
                }
                let limit = match ceil_steps(price, self.inv_tick) {
                    Some(k) if k > 0 => k as f64 / self.inv_tick,
                    _ => return Err(Rejection::InvalidPrice),
                };
                let Some(q) = sizing::size_qty(size, ctx.equity, limit, None) else {
                    return Err(Rejection::InvalidSize);
                };
                let lots = floor_steps(q, self.inv_lot).unwrap_or(free).min(free);
                if self.below_min(lots, limit) {
                    return Err(Rejection::BelowMinimum);
                }
                let mut order =
                    Order::base(Origin::Place, OrderType::LimitSell, tag, ctx.signal_ms, act);
                order.slot = slot;
                order.expires_at_ms = Some(expires);
                order.limit = limit;
                order.ttl_bars = ttl_bars;
                order.lots = lots;
                self.accept(order)
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Déroulé d'une 1m
    // -----------------------------------------------------------------------------------------

    /// Déroule les étapes 0 à 6 de la 1m `candle` (voir la doc du module).
    pub fn on_bar(&mut self, candle: &Candle) -> Result<(), SimError> {
        let t = candle.open_time_ms;
        self.tp_blocked_fill_bar = false;
        self.step_cancel_expire(t);
        self.step_arrival(candle);
        if self.rules.zero_volume_no_fill && candle.volume == 0.0 {
            // Étape 1 : rien n'est exécuté, seuls le compteur et les excursions bougent.
            if self.qty_lots > 0 || self.pending.iter().any(|o| o.active_from_ms <= t) {
                self.counters.bars_zero_volume_blocked += 1;
            }
            self.step_excursions(candle);
        } else {
            let vol_limit = self.rules.max_volume_share * (candle.volume * candle.low);
            let market_bought = self.step_market(candle, vol_limit)?;
            self.step_excursions(candle);
            let limit_bought = self.step_limit_buys(candle, vol_limit)?;
            self.step_exits(candle, vol_limit, market_bought || limit_bought)?;
        }
        self.recompute_reservations();
        self.last_close = Some(candle.close);
        if !(self.cash.is_finite() && self.cash >= 0.0) {
            return Err(SimError::NegativeCash {
                open_time_ms: t,
                cash: self.cash,
            });
        }
        if cfg!(debug_assertions) {
            self.check_invariants();
        }
        Ok(())
    }

    /// Étape 0 : annulations, expirations, puis `MoveStop` effectif.
    fn step_cancel_expire(&mut self, t: Ts) {
        if !self.pending.is_empty() {
            let before = self.pending.len();
            let counters = &mut self.counters;
            self.pending.retain(|order| {
                let gone = order.cancel_from_ms.is_some_and(|c| c <= t);
                if gone {
                    counters.orders_cancelled += 1;
                }
                !gone
            });
            self.pending.retain(|order| {
                let gone = order.expires_at_ms.is_some_and(|e| e <= t);
                if gone {
                    counters.limits_expired += 1;
                }
                !gone
            });
            if self.pending.len() != before {
                self.reservations_dirty = true;
                self.recompute_reservations();
            }
        }
        if let Some((stop, from, signal_ms)) = self.stop_update
            && from <= t
        {
            if self.qty_lots > 0 {
                self.stop = Some(stop);
                self.stop_signal_ms = signal_ms;
                self.counters.move_stops_applied += 1;
            } else {
                self.counters.move_stops_dropped_flat += 1;
            }
            self.stop_update = None;
        }
    }

    /// Fin de l'étape 0 : fixe, pour chaque limit qui devient active à cette 1m (et pour le take-profit
    /// arrivé), si elle est marketable à l'arrivée. Cela se fait sur toute 1m, volume nul compris : le
    /// prix à l'arrivée est celui de la minute d'activation, pas celui de la première 1m qui peut remplir.
    fn step_arrival(&mut self, candle: &Candle) {
        let t = candle.open_time_ms;
        for order in &mut self.pending {
            if order.arrival != Arrival::Unresolved || order.is_market() || order.active_from_ms > t
            {
                continue;
            }
            let p_arr = fills::arrival_price(order.active_from_ms, candle, self.last_close);
            order.arrival = arrival_state(order.side(), p_arr, order.limit);
        }
        if self.tp_arrival == Arrival::Unresolved
            && self.tp_active_from_ms <= t
            && let Some(tp) = self.take_profit
        {
            let p_arr = fills::arrival_price(self.tp_active_from_ms, candle, self.last_close);
            self.tp_arrival = arrival_state(Side::Sell, p_arr, tp);
        }
    }

    /// Étape 2 : ordres market actifs, dans l'ordre d'arrivée, à l'open. Rend vrai si un achat a été
    /// rempli.
    fn step_market(&mut self, candle: &Candle, vol_limit: f64) -> Result<bool, SimError> {
        let t = candle.open_time_ms;
        let mut bought = false;
        while let Some(k) = self
            .pending
            .iter()
            .position(|o| o.is_market() && o.active_from_ms <= t)
        {
            let order = self.pending.remove(k);
            self.reservations_dirty = true;
            match order.kind {
                OrderType::MarketBuy => {
                    bought |= self.fill_market_buy(order, candle, vol_limit)?;
                }
                OrderType::MarketSell => self.fill_market_sell(order, candle, vol_limit)?,
                OrderType::LimitBuy | OrderType::LimitSell => {}
            }
            self.recompute_reservations();
        }
        Ok(bought)
    }

    /// Achat market à l'open. Rend vrai si l'achat est rempli, faux s'il est écarté (compteur).
    fn fill_market_buy(
        &mut self,
        order: Order,
        candle: &Candle,
        vol_limit: f64,
    ) -> Result<bool, SimError> {
        if self.qty_lots > 0 {
            self.counters.entries_dropped_while_long += 1;
            return Ok(false);
        }
        let price = fills::market_buy_price(candle.open, self.slippage);
        let Some(q) = sizing::size_qty(order.size, order.equity_at_signal, price, order.stop)
        else {
            self.counters.entries_invalid_risk_at_fill += 1;
            return Ok(false);
        };
        let free = self.cash - self.quote_reserved;
        let cap = free * (1.0 - self.rules.market_buy_cash_margin) / (price * (1.0 + self.taker));
        let capped = cap < q;
        let qf = if capped { cap } else { q };
        if !(qf.is_finite() && qf > 0.0) {
            self.counters.buys_rejected_insufficient_cash += 1;
            return Ok(false);
        }
        let Some(lots) = floor_steps(qf, self.inv_lot) else {
            self.counters.buys_rejected_insufficient_cash += 1;
            return Ok(false);
        };
        if self.below_min(lots, price) {
            self.counters.buys_rejected_below_minimum += 1;
            return Ok(false);
        }
        let qty = self.qty_of(lots);
        if qty * price > vol_limit {
            self.counters.buys_rejected_volume_share += 1;
            return Ok(false);
        }
        let cost = qty * price;
        let fee = cost * self.taker;
        let slippage = qty * (price - candle.open);
        self.fill_buy(
            &order,
            candle,
            Entry {
                lots,
                price,
                cost,
                fee,
                slippage,
                ref_price: candle.open,
                liquidity: Liquidity::Taker,
                capped,
            },
        )?;
        self.counters.fills_market += 1;
        Ok(true)
    }

    fn fill_market_sell(
        &mut self,
        order: Order,
        candle: &Candle,
        vol_limit: f64,
    ) -> Result<(), SimError> {
        if self.qty_lots == 0 {
            self.counters.exits_dropped_flat += 1;
            return Ok(());
        }
        self.cancel_limit_sells();
        let price = fills::market_sell_price(candle.open, self.slippage);
        if self.qty_of(self.qty_lots) * price > vol_limit {
            self.counters.exits_over_volume_share += 1;
        }
        self.counters.fills_market += 1;
        self.sell_lots(
            Sale {
                lots: self.qty_lots,
                price,
                rate: self.taker,
                ref_price: candle.open,
                reason: ExitReason::Signal,
                signal_ms: order.signal_ms,
                exit_ms: candle.open_time_ms,
                tag: order.tag,
            },
            true,
        )
    }

    /// Étape 3 : excursions des lots ouverts.
    fn step_excursions(&mut self, candle: &Candle) {
        for lot in &mut self.lots {
            if candle.high > lot.hi {
                lot.hi = candle.high;
            }
            if candle.low < lot.lo {
                lot.lo = candle.low;
            }
        }
    }

    /// Étape 4 : limits d'achat actives. Une limit marketable à l'arrivée que l'open franchit encore est
    /// prise en taker à `min(open × (1 + s), L)` ; sinon (au repos, ou devenue au repos à cette 1m) elle
    /// est remplie à `L` en maker si le plus bas franchit `L` d'au moins `limit_cross_ticks` ticks.
    /// Rend vrai si au moins un achat a été rempli.
    fn step_limit_buys(&mut self, candle: &Candle, vol_limit: f64) -> Result<bool, SimError> {
        let t = candle.open_time_ms;
        let tick = self.instrument.tick_size;
        let mut bought = false;
        let mut k = 0;
        while let Some(candidate) = self.pending.get(k) {
            // Lecture par référence : la file est parcourue à chaque 1m, la copie d'un `Order` entier
            // n'a lieu que pour un ordre réellement rempli.
            if candidate.kind != OrderType::LimitBuy || candidate.active_from_ms > t {
                k += 1;
                continue;
            }
            debug_assert!(
                candidate.arrival != Arrival::Unresolved,
                "limit active sans prix à l'arrivée"
            );
            let marketable = candidate.arrival == Arrival::Marketable;
            let taker = marketable && candle.open < candidate.limit;
            let resting_cross =
                candle.low <= fills::buy_cross_level(candidate.limit, tick, self.kt);
            if marketable && !taker {
                // L'open ne franchit plus la limite : l'ordre est au repos à partir de cette 1m.
                self.pending[k].arrival = Arrival::Resting;
            }
            if !(taker || resting_cross) {
                k += 1;
                continue;
            }
            let order = self.pending.remove(k);
            self.reservations_dirty = true;
            self.recompute_reservations();
            if order.origin == Origin::Enter && self.qty_lots > 0 {
                self.counters.entries_dropped_while_long += 1;
                continue;
            }
            let price = if taker {
                let price = fills::marketable_buy_price(order.limit, candle.open, self.slippage);
                debug_assert!(
                    candle.open <= price && price <= order.limit,
                    "fill taker d'une limit d'achat hors de [open, limite]"
                );
                price
            } else {
                // Au repos : exactement `L`, même si un gap place toute la 1m sous `L` (le prix peut
                // alors sortir de `[low, high]`).
                order.limit
            };
            let qty = self.qty_of(order.lots);
            if qty * price > vol_limit {
                self.counters.buys_rejected_volume_share += 1;
                continue;
            }
            let cost = qty * price;
            let (fee, slippage, ref_price, liquidity) = if taker {
                (
                    cost * self.taker,
                    qty * (price - candle.open),
                    candle.open,
                    Liquidity::Taker,
                )
            } else {
                (cost * self.maker, 0.0, order.limit, Liquidity::Maker)
            };
            // La réservation couvre les frais maker : en taker, le cash libre (hors réservations des
            // autres ordres) peut ne pas couvrir le fill, qui est alors abandonné comme un achat market.
            if taker && self.cash - (cost + fee) < self.quote_reserved {
                self.counters.buys_rejected_insufficient_cash += 1;
                continue;
            }
            self.fill_buy(
                &order,
                candle,
                Entry {
                    lots: order.lots,
                    price,
                    cost,
                    fee,
                    slippage,
                    ref_price,
                    liquidity,
                    capped: order.capped,
                },
            )?;
            self.counters.fills_limit += 1;
            self.counters.limits_marketable_at_arrival += u64::from(taker);
            bought = true;
        }
        Ok(bought)
    }

    /// Étape 5 : stop, ventes limit et take-profit (voir [`IntrabarRule`]).
    fn step_exits(
        &mut self,
        candle: &Candle,
        vol_limit: f64,
        buy_filled_this_bar: bool,
    ) -> Result<(), SimError> {
        let pos = self.qty_lots > 0;
        let tick = self.instrument.tick_size;
        let stop_hit = pos && self.stop.is_some_and(|s| candle.low <= s);
        let tp_marketable = pos && self.tp_arrival == Arrival::Marketable;
        let tp_taker = tp_marketable && self.take_profit.is_some_and(|tp| candle.open > tp);
        if tp_marketable && !tp_taker {
            // L'open ne franchit plus le take-profit : il est au repos à partir de cette 1m.
            self.tp_arrival = Arrival::Resting;
        }
        let tp_cross = pos
            && (tp_taker
                || self
                    .take_profit
                    .is_some_and(|tp| candle.high >= fills::sell_cross_level(tp, tick, self.kt)));
        let tp_ignored = self.rules.tp_ignored_on_fill_bar || self.tp_blocked_fill_bar;
        let tp_ok = tp_cross && !(tp_ignored && buy_filled_this_bar);
        if tp_cross && !tp_ok {
            self.counters.take_profits_ignored_fill_bar += 1;
        }
        if stop_hit && tp_ok {
            self.counters.stop_tp_same_bar += 1;
        }
        if stop_hit && self.rules.intrabar == IntrabarRule::StopFirst {
            return self.execute_stop(candle, vol_limit);
        }
        self.fill_limit_sells(candle, vol_limit)?;
        if self.qty_lots > 0 && tp_ok {
            self.execute_take_profit(candle, vol_limit, tp_taker)?;
        }
        if self.qty_lots > 0 && stop_hit {
            self.execute_stop(candle, vol_limit)?;
        }
        Ok(())
    }

    /// Ventes limit actives, dans l'ordre d'arrivée : taker à `max(open × (1 − s), L)` si marketable à
    /// l'arrivée et que l'open franchit encore `L`, sinon maker à `L` quand le plus haut la franchit
    /// d'au moins `limit_cross_ticks` ticks.
    fn fill_limit_sells(&mut self, candle: &Candle, vol_limit: f64) -> Result<(), SimError> {
        let t = candle.open_time_ms;
        let tick = self.instrument.tick_size;
        let mut k = 0;
        while let Some(candidate) = self.pending.get(k) {
            if candidate.kind != OrderType::LimitSell || candidate.active_from_ms > t {
                k += 1;
                continue;
            }
            debug_assert!(
                candidate.arrival != Arrival::Unresolved,
                "limit active sans prix à l'arrivée"
            );
            let marketable = candidate.arrival == Arrival::Marketable;
            let taker = marketable && candle.open > candidate.limit;
            let resting_cross =
                candle.high >= fills::sell_cross_level(candidate.limit, tick, self.kt);
            if marketable && !taker {
                self.pending[k].arrival = Arrival::Resting;
            }
            if !(taker || resting_cross) {
                k += 1;
                continue;
            }
            let order = self.pending.remove(k);
            self.reservations_dirty = true;
            self.recompute_reservations();
            let price = if taker {
                let price = fills::marketable_sell_price(order.limit, candle.open, self.slippage);
                debug_assert!(
                    order.limit <= price && price <= candle.open,
                    "fill taker d'une limit de vente hors de [limite, open]"
                );
                price
            } else {
                order.limit
            };
            if self.qty_of(order.lots) * price > vol_limit {
                self.counters.exits_over_volume_share += 1;
            }
            self.counters.fills_limit += 1;
            self.counters.limits_marketable_at_arrival += u64::from(taker);
            let reason = if order.origin == Origin::Exit {
                ExitReason::Signal
            } else {
                ExitReason::LimitSell
            };
            self.sell_lots(
                Sale {
                    lots: order.lots,
                    price,
                    rate: if taker { self.taker } else { self.maker },
                    ref_price: if taker { candle.open } else { order.limit },
                    reason,
                    signal_ms: order.signal_ms,
                    exit_ms: t,
                    tag: order.tag,
                },
                taker,
            )?;
        }
        Ok(())
    }

    /// Stop franchi : exécuté au pire du niveau et de l'open, taker, avec slippage de stop.
    fn execute_stop(&mut self, candle: &Candle, vol_limit: f64) -> Result<(), SimError> {
        let Some(stop) = self.stop else {
            return Err(SimError::InvariantViolated {
                open_time_ms: candle.open_time_ms,
            });
        };
        let worst = fills::stop_reference(stop, candle.open);
        let price = fills::stop_price(worst, self.stop_rate);
        if self.qty_of(self.qty_lots) * price > vol_limit {
            self.counters.exits_over_volume_share += 1;
        }
        self.cancel_limit_sells();
        self.counters.stops_triggered += 1;
        self.sell_lots(
            Sale {
                lots: self.qty_lots,
                price,
                rate: self.taker,
                ref_price: worst,
                reason: ExitReason::Stop,
                signal_ms: self.stop_signal_ms,
                exit_ms: candle.open_time_ms,
                tag: "stop",
            },
            true,
        )
    }

    /// Take-profit franchi : comme une vente limit, taker à `max(open × (1 − s), tp)` s'il était
    /// marketable à l'arrivée et que l'open le franchit encore (`taker`), sinon maker à `tp`.
    fn execute_take_profit(
        &mut self,
        candle: &Candle,
        vol_limit: f64,
        taker: bool,
    ) -> Result<(), SimError> {
        let Some(tp) = self.take_profit else {
            return Err(SimError::InvariantViolated {
                open_time_ms: candle.open_time_ms,
            });
        };
        let price = if taker {
            let price = fills::marketable_sell_price(tp, candle.open, self.slippage);
            debug_assert!(
                tp <= price && price <= candle.open,
                "fill taker d'un take-profit hors de [tp, open]"
            );
            price
        } else {
            tp
        };
        if self.qty_of(self.qty_lots) * price > vol_limit {
            self.counters.exits_over_volume_share += 1;
        }
        self.cancel_limit_sells();
        self.counters.take_profits_triggered += 1;
        self.counters.limits_marketable_at_arrival += u64::from(taker);
        self.sell_lots(
            Sale {
                lots: self.qty_lots,
                price,
                rate: if taker { self.taker } else { self.maker },
                ref_price: if taker { candle.open } else { tp },
                reason: ExitReason::TakeProfit,
                signal_ms: self.tp_signal_ms,
                exit_ms: candle.open_time_ms,
                tag: "take_profit",
            },
            taker,
        )
    }

    /// Retire toutes les ventes limit en attente (une sortie complète les rend sans objet).
    fn cancel_limit_sells(&mut self) {
        let counters = &mut self.counters;
        self.pending.retain(|order| {
            let gone = order.kind == OrderType::LimitSell;
            if gone {
                counters.sells_cancelled_by_exit += 1;
            }
            !gone
        });
        self.reservations_dirty = true;
        self.recompute_reservations();
    }

    // -----------------------------------------------------------------------------------------
    // Fills, lots FIFO et trades
    // -----------------------------------------------------------------------------------------

    /// Enregistre un achat rempli : cash, lot FIFO, prix moyen et, pour une entrée, stop et take-profit.
    fn fill_buy(&mut self, order: &Order, candle: &Candle, entry: Entry) -> Result<(), SimError> {
        let t = candle.open_time_ms;
        if !(entry.cost.is_finite() && entry.fee.is_finite() && entry.slippage.is_finite()) {
            return Err(SimError::NonFiniteAmount { open_time_ms: t });
        }
        let cash = self.cash - (entry.cost + entry.fee);
        if !(cash.is_finite() && cash >= 0.0) {
            return Err(SimError::NegativeCash {
                open_time_ms: t,
                cash,
            });
        }
        self.cash = cash;
        let id = self.next_lot_id;
        self.next_lot_id += 1;
        self.lots.push_back(Lot {
            id,
            entry_signal_ms: order.signal_ms,
            entry_ms: t,
            entry_ref_price: entry.ref_price,
            entry_price: entry.price,
            lots_init: entry.lots,
            lots_left: entry.lots,
            cost: entry.cost,
            fee: entry.fee,
            slippage: entry.slippage,
            liquidity: entry.liquidity,
            capped: entry.capped,
            tag: order.tag,
            hi: candle.high,
            lo: candle.low,
        });
        if self.qty_lots == 0 {
            self.avg_price = entry.price;
            self.opened_at_ms = t;
        } else {
            let q_old = self.qty_of(self.qty_lots);
            let q = self.qty_of(entry.lots);
            self.avg_price = (self.avg_price * q_old + entry.price * q) / (q_old + q);
        }
        self.qty_lots += entry.lots;
        if order.origin == Origin::Enter {
            self.stop = order.stop;
            self.take_profit = order.take_profit;
            self.stop_signal_ms = order.signal_ms;
            self.tp_signal_ms = order.signal_ms;
            // Le take-profit arrive à la 1m suivante : celle du fill l'ignore (ou, si cette règle est
            // levée, il arrive à l'open de cette même 1m). Exception pessimiste : une entrée limit au repos
            // (maker) est remplie en cours de 1m, le TP est posé après le fill et ne peut pas être pris à
            // l'open de cette 1m (avant l'achat) : même traitement que `tp_ignored_on_fill_bar`.
            self.tp_arrival = Arrival::Unresolved;
            self.tp_blocked_fill_bar = entry.liquidity == Liquidity::Maker;
            if self.rules.tp_ignored_on_fill_bar || self.tp_blocked_fill_bar {
                self.tp_active_from_ms = t.saturating_add(MINUTE_MS);
            } else {
                self.tp_active_from_ms = t;
                if let Some(tp) = order.take_profit {
                    self.tp_arrival = arrival_state(Side::Sell, candle.open, tp);
                }
            }
        }
        self.counters.fills += 1;
        self.counters.entries_capped_by_cash += u64::from(entry.capped);
        Ok(())
    }

    /// Vend `sale.lots` lots en consommant les lots les plus anciens, et écrit un trade par part de lot.
    ///
    /// Les montants d'un trade sont au prorata `k / K` de la vente et `k / lots_init` du lot ; pour un
    /// lot fermé d'un coup, les deux parts valent exactement 1.0 et le trade reprend les montants au bit.
    /// `taker` vrai : le slippage de la vente est `qty × (référence − prix)`, sinon 0.
    fn sell_lots(&mut self, sale: Sale, taker: bool) -> Result<(), SimError> {
        let at = sale.exit_ms;
        let total = sale.lots;
        if total == 0 || total > self.qty_lots {
            return Err(SimError::InvariantViolated { open_time_ms: at });
        }
        let qty_total = self.qty_of(total);
        let proceeds = qty_total * sale.price;
        let fee = proceeds * sale.rate;
        let slippage = if taker {
            qty_total * (sale.ref_price - sale.price)
        } else {
            0.0
        };
        if !(proceeds.is_finite() && fee.is_finite() && slippage.is_finite()) {
            return Err(SimError::NonFiniteAmount { open_time_ms: at });
        }
        let cash = self.cash + (proceeds - fee);
        if !(cash.is_finite() && cash >= 0.0) {
            return Err(SimError::NegativeCash {
                open_time_ms: at,
                cash,
            });
        }
        self.cash = cash;
        let exit_liquidity = if taker {
            Liquidity::Taker
        } else {
            Liquidity::Maker
        };
        let forced = sale.reason == ExitReason::ForcedClose;
        let stop_at_exit = self.stop;
        let mut left = total;
        while left > 0 {
            let Some(&lot) = self.lots.front() else {
                return Err(SimError::InvariantViolated { open_time_ms: at });
            };
            let k = lot.lots_left.min(left);
            let share = k as f64 / total as f64;
            let part = k as f64 / lot.lots_init as f64;
            let proceeds_i = proceeds * share;
            let fee_i = fee * share;
            let slippage_i = slippage * share;
            let entry_cost = lot.cost * part;
            let entry_fee = lot.fee * part;
            let entry_slippage = lot.slippage * part;
            let qty = self.qty_of(k);
            let pnl_net = (proceeds_i - fee_i) - (entry_cost + entry_fee);
            self.trades.push(Trade {
                lot_id: lot.id,
                entry_signal_ms: lot.entry_signal_ms,
                entry_ms: lot.entry_ms,
                entry_ref_price: lot.entry_ref_price,
                entry_price: lot.entry_price,
                entry_fee,
                entry_slippage,
                entry_liquidity: lot.liquidity,
                exit_signal_ms: sale.signal_ms,
                exit_ms: sale.exit_ms,
                exit_ref_price: sale.ref_price,
                exit_price: sale.price,
                exit_fee: fee_i,
                exit_slippage: slippage_i,
                exit_liquidity,
                qty,
                pnl_net,
                mfe: qty * (lot.hi - lot.entry_price),
                mae: qty * (lot.lo - lot.entry_price),
                stop_at_exit,
                capped_by_cash: lot.capped,
                exit_reason: sale.reason,
                forced_close: forced,
                entry_tag: lot.tag,
                exit_tag: sale.tag,
            });
            left -= k;
            if k == lot.lots_left {
                self.lots.pop_front();
            } else if let Some(front) = self.lots.front_mut() {
                front.lots_left -= k;
            }
        }
        self.qty_lots -= total;
        self.counters.fills += 1;
        if self.qty_lots == 0 {
            self.avg_price = 0.0;
            self.stop = None;
            self.take_profit = None;
        }
        Ok(())
    }

    /// Fin de run : les ordres encore en attente ne sont pas remplis (règle I3), la position restante
    /// est soldée au close de la dernière 1m (clôture forcée, avec slippage et frais taker, règle de
    /// volume ignorée). Un `MoveStop` pas encore effectif est abandonné sans compteur.
    pub fn finish(mut self, last: &Candle) -> Result<SimOutcome, SimError> {
        self.counters.orders_unfilled_at_end += self.pending.len() as u64;
        self.pending.clear();
        self.reservations_dirty = true;
        self.recompute_reservations();
        self.stop_update = None;
        let mut forced_close = false;
        if self.qty_lots > 0 {
            let end_ms = last.open_time_ms.saturating_add(MINUTE_MS);
            self.sell_lots(
                Sale {
                    lots: self.qty_lots,
                    price: fills::market_sell_price(last.close, self.slippage),
                    rate: self.taker,
                    ref_price: last.close,
                    reason: ExitReason::ForcedClose,
                    signal_ms: end_ms,
                    exit_ms: end_ms,
                    tag: "forced_close",
                },
                true,
            )?;
            forced_close = true;
        }
        Ok(SimOutcome {
            cash: self.cash,
            qty: self.qty_of(self.qty_lots),
            forced_close,
            counters: self.counters,
            trades: self.trades,
        })
    }

    // -----------------------------------------------------------------------------------------
    // Invariants (E11)
    // -----------------------------------------------------------------------------------------

    /// Invariants du portefeuille, actifs en debug (donc dans les tests), sans effet en release.
    fn check_invariants(&self) {
        debug_assert!(
            self.cash.is_finite() && self.cash >= 0.0,
            "cash négatif ou non fini : {}",
            self.cash
        );
        debug_assert!(
            self.quote_reserved.is_finite() && self.quote_reserved >= 0.0,
            "quote_reserved négatif ou non fini : {}",
            self.quote_reserved
        );
        debug_assert!(
            self.quote_reserved <= self.cash,
            "quote_reserved dépasse le cash : {} > {}",
            self.quote_reserved,
            self.cash
        );
        debug_assert!(
            self.reserved_lots <= self.qty_lots,
            "reserved_lots dépasse la quantité : {} > {}",
            self.reserved_lots,
            self.qty_lots
        );
        // Les réservations stockées doivent égaler le recalcul complet depuis la file (même ordre de
        // sommation, donc égalité au bit) : un site qui oublierait de lever `reservations_dirty` rougit.
        let mut quote = 0.0;
        let mut reserved = 0u64;
        for order in &self.pending {
            match order.kind {
                OrderType::LimitBuy => quote += order.reserved_quote,
                OrderType::LimitSell => reserved += order.lots,
                OrderType::MarketBuy | OrderType::MarketSell => {}
            }
        }
        debug_assert!(
            !self.reservations_dirty
                && quote.to_bits() == self.quote_reserved.to_bits()
                && reserved == self.reserved_lots,
            "réservations périmées : stocké ({}, {}), recalculé ({quote}, {reserved})",
            self.quote_reserved,
            self.reserved_lots
        );
        let mut in_lots = 0u64;
        for lot in &self.lots {
            in_lots += lot.lots_left;
        }
        debug_assert!(
            in_lots == self.qty_lots && self.lots.is_empty() == (self.qty_lots == 0),
            "les lots FIFO ({in_lots}) contredisent la quantité ({})",
            self.qty_lots
        );
        debug_assert!(
            self.pending.len() <= self.rules.max_pending,
            "la file dépasse max_pending"
        );
        for (i, order) in self.pending.iter().enumerate() {
            debug_assert!(
                order.slot == 0
                    || !self
                        .pending
                        .iter()
                        .skip(i + 1)
                        .any(|other| other.slot == order.slot),
                "slot en double : {}",
                order.slot
            );
        }
    }
}
