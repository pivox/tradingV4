//! Lots FIFO, trades clos et vue de la position (docs/03-architecture.md §7.2).

use crate::model::Ts;

/// Liquidité d'un fill : maker (limit, take-profit) ou taker (market, stop, clôture forcée).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liquidity {
    Maker,
    Taker,
}

/// Motif de sortie d'un trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// Vente issue d'un `Exit` (market, ou limit posée par `Exit`).
    Signal,
    /// Vente limit posée par `PlaceLimit`.
    LimitSell,
    /// Stop franchi, exécuté en taker avec slippage.
    Stop,
    /// Take-profit franchi, exécuté en maker.
    TakeProfit,
    /// Position soldée à la fin des données, au dernier close.
    ForcedClose,
    /// Sortie forcée d'une jambe dont la cotation est absente depuis plus de `quote_gap_ms` (paire
    /// suspendue ou délistée, docs/04 §2.2), au dernier close moins le slippage, frais taker. Jamais
    /// produite hors de [`Book`](super::Book) (docs/specs S9 §4.3).
    QuoteGap,
}

/// Position agrégée, telle que la stratégie la voit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionView {
    pub qty: f64,
    /// Prix moyen d'entrée (slippage compris, frais exclus), recalculé à chaque achat.
    pub avg_price: f64,
    /// Stop en vigueur (jamais celui d'un `MoveStop` pas encore effectif).
    pub stop: Option<f64>,
    pub take_profit: Option<f64>,
    pub opened_at_ms: Ts,
}

/// Un trade est la part d'un lot d'entrée fermée par une vente (FIFO). `Copy` : le vecteur de trades
/// est préalloué.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trade {
    pub lot_id: u64,
    pub entry_signal_ms: Ts,
    /// Instant d'arrivée de l'ordre d'achat rempli à l'exchange simulé (`active_from_ms` de l'ordre,
    /// c'est-à-dire `signal_ms + activation_delay_bars × 60 000`).
    pub entry_order_ms: Ts,
    /// `open_time` de la 1m de fill d'entrée.
    pub entry_ms: Ts,
    /// Open pour un market, limite pour une limit.
    pub entry_ref_price: f64,
    pub entry_price: f64,
    pub entry_fee: f64,
    pub entry_slippage: f64,
    /// Part de spread de l'entrée, en devise de cotation (s10 §6.3). Fill taker : part de l'impact due au
    /// demi-spread, déjà comprise dans le prix (`entry_slippage + entry_spread` = impact total). Fill au
    /// repos du profil maker : débit de sélection adverse, hors du prix, retiré du cash et du PnL.
    /// Chemin hérité : toujours `0.0`.
    pub entry_spread: f64,
    pub entry_liquidity: Liquidity,
    pub exit_signal_ms: Ts,
    /// Instant d'arrivée de l'ordre de sortie : `active_from_ms` d'un ordre en attente, `entry_ms` du
    /// fill qui a posé le stop initial, instant d'activation du dernier `MoveStop` appliqué pour un
    /// stop déplacé, activation du take-profit, `exit_ms` pour la clôture forcée.
    pub exit_order_ms: Ts,
    /// `open_time` de la 1m de fill de sortie ; clôture de la dernière 1m pour la clôture forcée.
    pub exit_ms: Ts,
    pub exit_ref_price: f64,
    pub exit_price: f64,
    pub exit_fee: f64,
    pub exit_slippage: f64,
    /// Part de spread de la sortie, même sens que `entry_spread`.
    pub exit_spread: f64,
    pub exit_liquidity: Liquidity,
    pub qty: f64,
    /// Produit net de la part de vente moins coût net de la part de lot (frais compris, et débits de
    /// sélection adverse du profil maker).
    pub pnl_net: f64,
    /// Excursion favorable maximale en quote : `qty × (plus haut − prix d'entrée)`.
    pub mfe: f64,
    /// Excursion défavorable maximale en quote : `qty × (plus bas − prix d'entrée)`, négative en général.
    pub mae: f64,
    pub stop_at_exit: Option<f64>,
    /// Stop de la position juste après le fill qui a ouvert ce lot (`None` sans stop).
    pub stop_at_entry: Option<f64>,
    /// Vrai si un `MoveStop` a été appliqué depuis l'ouverture de la position (stop suiveur).
    pub stop_moved: bool,
    pub capped_by_cash: bool,
    pub exit_reason: ExitReason,
    /// Redondant avec `exit_reason == ForcedClose`, demandé tel quel dans `trades.csv` (docs/03 §7.2).
    pub forced_close: bool,
    pub entry_tag: &'static str,
    pub exit_tag: &'static str,
}

/// Lot d'entrée ouvert : un achat rempli, consommé dans l'ordre d'arrivée par les ventes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Lot {
    pub id: u64,
    pub entry_signal_ms: Ts,
    /// `active_from_ms` de l'ordre d'achat rempli.
    pub order_ms: Ts,
    pub entry_ms: Ts,
    pub entry_ref_price: f64,
    pub entry_price: f64,
    /// Stop de la position juste après ce fill.
    pub stop_at_entry: Option<f64>,
    pub lots_init: u64,
    pub lots_left: u64,
    pub cost: f64,
    pub fee: f64,
    pub slippage: f64,
    /// Part de spread (taker) ou débit de sélection adverse (maker) de l'entrée, au prorata comme `fee`.
    pub spread: f64,
    /// Débit de sélection adverse retiré du cash à l'entrée (`0.0` hors profil maker), au prorata.
    pub debit: f64,
    pub liquidity: Liquidity,
    pub capped: bool,
    pub tag: &'static str,
    /// Plus haut et plus bas vus depuis le fill, pour MFE et MAE.
    pub hi: f64,
    pub lo: f64,
}
