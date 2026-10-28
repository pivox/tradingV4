//! Instructions, ordres en attente et motifs de refus du simulateur S6 (docs/03-architecture.md §7.2,
//! docs/04-protocole-recherche.md §3.1).
//!
//! `sim` ne connaît pas `strategy` : l'engine traduit un `Signal` en [`Instruction`], champ à champ,
//! sans aucune logique de refus (tous les refus sont décidés par [`Simulator::submit`](super::Simulator::submit)).

use crate::model::{Side, Ts};

/// Taille demandée par une instruction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizeSpec {
    /// Fraction de l'equity du signal (0,25 pour 25 %).
    PctEquity(f64),
    /// Fraction de l'equity du signal risquée entre le prix de fill et le stop ; exige un stop.
    RiskPct(f64),
    /// Montant en monnaie de cotation.
    Quote(f64),
}

/// Prix d'un ordre d'entrée ou de sortie.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OrderPrice {
    Market,
    /// Limite au prix `price`, valable `ttl_bars` bougies 1m à partir de l'activation.
    Limit {
        price: f64,
        ttl_bars: u32,
    },
}

/// Instruction envoyée au simulateur à la clôture d'une 1m.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Instruction {
    /// Ouvre une position longue (refusée si une position ou une entrée est déjà là).
    Enter {
        size: SizeSpec,
        price: OrderPrice,
        stop: Option<f64>,
        take_profit: Option<f64>,
        tag: &'static str,
    },
    /// Sort toute la position (market) ou sa quantité libre (limit).
    Exit {
        price: OrderPrice,
        tag: &'static str,
    },
    /// Déplace le stop de la position agrégée (effectif après le délai d'activation).
    MoveStop { stop: f64 },
    /// Annule tous les ordres en attente (effectif après le délai d'activation).
    CancelAll,
    /// Limit autonome qui coexiste avec les autres ; `slot` est unique parmi les ordres en attente.
    Place {
        side: Side,
        price: f64,
        size: SizeSpec,
        ttl_bars: u32,
        slot: u16,
        tag: &'static str,
    },
    /// Annule l'ordre du `slot` (effectif après le délai d'activation).
    CancelSlot { slot: u16 },
}

/// Ce que le moteur sait à l'instant du signal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SubmitContext {
    /// `open_time[i] + 60 000` : clôture de la 1m qui a émis le signal.
    pub signal_ms: Ts,
    /// `cash + qty × close[i]`.
    pub equity: f64,
    /// `close[i]` de la 1m courante : seul prix connu à cet instant.
    pub ref_price: f64,
}

/// Motifs de refus d'une instruction, un compteur chacun dans `SignalRejections` (côté engine).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// La file contient déjà `max_pending` ordres.
    PendingFull,
    /// Entrée alors qu'une position est ouverte ou qu'une entrée est déjà en attente.
    EntryWhileLong,
    /// Sortie sans position (pas de short en spot).
    ExitWithoutPosition,
    /// Une sortie est déjà en attente.
    DuplicateExit,
    /// `RiskPct` sans stop.
    RiskWithoutStop,
    /// `RiskPct` avec un stop qui n'est pas strictement entre 0 et le prix de référence.
    InvalidRiskSizing,
    /// Taille non finie, nulle ou négative, ou fraction d'equity au-dessus de 1.
    InvalidSize,
    /// Prix (limite, stop, take-profit) non fini, nul ou négatif, ou nul après arrondi au tick.
    InvalidPrice,
    /// `ttl_bars` nul.
    InvalidTtl,
    /// `slot` nul.
    InvalidSlot,
    /// `Place` avec `RiskPct` (pas de stop attaché).
    PlaceLimitRiskPct,
    /// Un ordre en attente porte déjà ce `slot`.
    SlotAlreadyOpen,
    /// Le quote libre ne couvre pas la réservation d'un achat.
    InsufficientQuote,
    /// Vente limit alors que toute la quantité est déjà réservée.
    SellWithoutFreeQty,
    /// Quantité ou notional sous les minimums de l'instrument après arrondi.
    BelowMinimum,
    /// `MoveStop` sans position.
    MoveStopWithoutPosition,
    /// `CancelSlot` d'un slot sans ordre en attente.
    CancelUnknownSlot,
}

/// Vue d'un ordre en attente, pour le contexte de la stratégie.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingView {
    pub side: Side,
    /// `(prix arrondi, ttl_bars d'origine)` ; `None` pour un market.
    pub limit: Option<(f64, u32)>,
    /// Quantité d'une limit ; 0.0 pour un market (taille fixée au fill).
    pub qty: f64,
    /// 0 pour `Enter` et `Exit`.
    pub slot: u16,
    pub tag: &'static str,
    pub signal_ms: Ts,
    /// `None` pour un market.
    pub expires_at_ms: Option<Ts>,
}

/// Origine d'un ordre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    Enter,
    Exit,
    Place,
}

/// Type d'un ordre en attente.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrderType {
    MarketBuy,
    MarketSell,
    LimitBuy,
    LimitSell,
}

/// État d'une limit (ou du take-profit) au regard de son prix à l'arrivée (règle « limits
/// pessimistes », docs/03 §7.2 écart 11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arrival {
    /// Pas encore vu : l'ordre n'est pas actif, ou la 1m qui le décide n'est pas passée.
    Unresolved,
    /// Marketable à l'arrivée (achat : `P_arr < L`, vente : `P_arr > L`) et pas encore exécuté : il prend
    /// la liquidité (taker) dès la première 1m à volume dont l'open franchit encore `L`, sinon il passe au
    /// repos.
    Marketable,
    /// Au repos : rempli à `L` exactement, en maker, par une 1m qui le traverse d'au moins
    /// `limit_cross_ticks` ticks.
    Resting,
}

/// Ordre en attente. `Copy` : la file est un `Vec` préalloué, aucun ordre n'alloue.
#[derive(Clone, Copy, Debug)]
pub(super) struct Order {
    pub origin: Origin,
    pub kind: OrderType,
    pub slot: u16,
    pub tag: &'static str,
    pub signal_ms: Ts,
    pub active_from_ms: Ts,
    pub expires_at_ms: Option<Ts>,
    pub cancel_from_ms: Option<Ts>,
    /// Limite arrondie au tick (0.0 pour un market).
    pub limit: f64,
    /// `ttl_bars` d'origine (0 pour un market).
    pub ttl_bars: u32,
    /// Limits : lots fixés à la pose. Market : 0.
    pub lots: u64,
    /// `LimitBuy` : quote réservé à la pose. Sinon 0.0.
    pub reserved_quote: f64,
    /// `LimitBuy` issu d'`Enter` et plafonné par le cash.
    pub capped: bool,
    /// `MarketBuy` : taille à résoudre au fill.
    pub size: SizeSpec,
    /// `MarketBuy` : equity du signal.
    pub equity_at_signal: f64,
    /// `Enter` seulement.
    pub stop: Option<f64>,
    pub take_profit: Option<f64>,
    /// `MarketBuy` : plafond de notional (`qty × prix`) appliqué au fill (mode recherche du portefeuille,
    /// docs S9 §7.1). `None` hors portefeuille.
    pub max_notional: Option<f64>,
    /// Limits : situation par rapport au prix à l'arrivée. `Unresolved` pour un market.
    pub arrival: Arrival,
    /// Profil maker (s10 §7.3) : le tirage de fill de cet ordre au repos a échoué, il ne peut plus être
    /// rempli. Il garde sa réservation jusqu'à son expiration ou son annulation. Toujours faux hors du
    /// profil maker.
    pub queue_missed: bool,
    /// `LimitBuy` issu de la conversion d'une entrée market du profil maker (s10 §7.1).
    pub maker_entry: bool,
}

impl Order {
    /// Ordre de base : tous les champs spécifiques à zéro, à compléter par l'appelant.
    pub(super) fn base(
        origin: Origin,
        kind: OrderType,
        tag: &'static str,
        signal_ms: Ts,
        active_from_ms: Ts,
    ) -> Order {
        Order {
            origin,
            kind,
            slot: 0,
            tag,
            signal_ms,
            active_from_ms,
            expires_at_ms: None,
            cancel_from_ms: None,
            limit: 0.0,
            ttl_bars: 0,
            lots: 0,
            reserved_quote: 0.0,
            capped: false,
            size: SizeSpec::Quote(0.0),
            equity_at_signal: 0.0,
            stop: None,
            take_profit: None,
            max_notional: None,
            arrival: Arrival::Unresolved,
            queue_missed: false,
            maker_entry: false,
        }
    }

    pub(super) fn side(&self) -> Side {
        match self.kind {
            OrderType::MarketBuy | OrderType::LimitBuy => Side::Buy,
            OrderType::MarketSell | OrderType::LimitSell => Side::Sell,
        }
    }

    pub(super) fn is_market(&self) -> bool {
        matches!(self.kind, OrderType::MarketBuy | OrderType::MarketSell)
    }
}
