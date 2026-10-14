//! État du compte, vues par timeframe et contexte de bougie (docs/03-architecture.md §6).
//!
//! Ces types vivent dans un sous-module PRIVE de `strategy` : leurs champs ne sont visibles que d'ici,
//! donc ni `sma_cross`, ni les stratégies à venir, ni `registry` ne peuvent lire une colonne ou une
//! bougie au-delà de `closed` (un champ privé reste lisible par les modules descendants de celui qui
//! définit le type ; d'où ce module frère, voir `tests/architecture.rs`). Les seules voies de lecture
//! sont les accesseurs, bornés à la dernière bougie clôturée.

use super::{PendingOrder, Position, StrategyError};
use crate::model::{Candle, FeeSchedule, Timeframe, Ts};

/// État du compte et de l'horloge à la clôture de la 1m courante, fourni par le moteur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarState {
    /// Instant de clôture de la bougie 1m courante.
    pub now_ms: Ts,
    /// Cash + quantité × close précédent.
    pub equity: f64,
    /// Cash moins le quote réservé par les limits d'achat en attente.
    pub quote_free: f64,
    /// Quantité moins celle réservée par les limits de vente en attente.
    pub qty_free: f64,
    pub position: Option<Position>,
    pub fees: FeeSchedule,
}

/// Vue d'un timeframe : bougies et colonnes d'indicateurs COMPLÈTES, plus un compteur `closed`.
///
/// Le bornage se fait à la lecture, dans [`BarContext`] : le moteur construit chaque vue une fois par
/// run (contrôle des longueurs en O(k)), puis appelle [`TfView::with_closed`] à chaque bougie en O(1).
#[derive(Clone, Copy)]
pub struct TfView<'a> {
    tf: Timeframe,
    candles: &'a [Candle],
    names: &'a [&'static str],
    columns: &'a [&'a [f64]],
    closed: usize,
}

impl<'a> TfView<'a> {
    /// `names[i]` désigne `columns[i]`, dans l'ordre de [`Strategy::indicators`] restreint à `tf`.
    ///
    /// Contrôles : autant de noms que de colonnes, noms distincts, chaque colonne de la longueur de
    /// `candles`. `closed` vaut 0 à la construction.
    pub fn new(
        tf: Timeframe,
        candles: &'a [Candle],
        names: &'a [&'static str],
        columns: &'a [&'a [f64]],
    ) -> Result<Self, StrategyError> {
        if names.len() != columns.len() {
            return Err(StrategyError::IndicatorCountMismatch {
                tf,
                names: names.len(),
                columns: columns.len(),
            });
        }
        for (i, name) in names.iter().enumerate() {
            if names[..i].contains(name) {
                return Err(StrategyError::DuplicateIndicator { tf, name });
            }
        }
        for (name, column) in names.iter().zip(columns) {
            if column.len() != candles.len() {
                return Err(StrategyError::IndicatorLength {
                    tf,
                    name,
                    expected: candles.len(),
                    found: column.len(),
                });
            }
        }
        Ok(TfView {
            tf,
            candles,
            names,
            columns,
            closed: 0,
        })
    }

    /// Fixe le nombre de bougies clôturées visibles. O(1) : seul `closed <= candles.len()` est contrôlé.
    pub fn with_closed(self, closed: usize) -> Result<Self, StrategyError> {
        if closed > self.candles.len() {
            return Err(StrategyError::ClosedOutOfRange {
                tf: self.tf,
                closed,
                len: self.candles.len(),
            });
        }
        Ok(TfView { closed, ..self })
    }

    pub fn tf(&self) -> Timeframe {
        self.tf
    }

    pub fn closed(&self) -> usize {
        self.closed
    }

    /// Bougies clôturées visibles. `closed <= len` est garanti par `with_closed`.
    fn visible_candles(&self) -> &'a [Candle] {
        self.candles.get(..self.closed).unwrap_or(&[])
    }

    /// Colonne `index` bornée à `closed` ; `&[]` si l'index est hors bornes.
    fn visible_column(&self, index: usize) -> &'a [f64] {
        self.columns
            .get(index)
            .and_then(|column| column.get(..self.closed))
            .unwrap_or(&[])
    }
}

/// `Debug` borné : n'imprime que les bougies et colonnes visibles (jamais le futur), et seulement
/// leurs longueurs, pour rester utilisable sur un million de bougies.
impl std::fmt::Debug for TfView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TfView")
            .field("tf", &self.tf)
            .field("closed", &self.closed)
            .field("names", &self.names)
            .field("visible_candles", &self.visible_candles().len())
            .field("visible_columns", &self.names.len())
            .finish()
    }
}

/// Vue en lecture seule construite par le moteur pour chaque bougie 1m.
///
/// Toutes les tranches s'arrêtent à la dernière bougie CLÔTURÉE du timeframe demandé à l'instant
/// `now_ms` (une bougie est clôturée quand `open_time + ms(tf) <= now_ms` : la H1 10:00 est visible
/// dans l'appel de la 1m 10:59). Les champs sont privés et les accesseurs bornés : pas de voie vers le
/// futur. La stratégie ne stocke jamais de référence issue du contexte, seulement des `f64` copiés.
#[derive(Clone, Copy, Debug)]
pub struct BarContext<'a> {
    symbol: &'a str,
    state: BarState,
    pending: &'a [PendingOrder],
    views: &'a [TfView<'a>],
    just_closed: u8,
}

impl<'a> BarContext<'a> {
    /// Construit le contexte en O(nombre de vues). Contrôles : au moins une vue, timeframes distincts,
    /// `just_closed` limité aux timeframes déclarés, et garde-fou anti look-ahead (I2) : la dernière
    /// bougie visible de chaque vue est bien clôturée à `now_ms`.
    pub fn new(
        symbol: &'a str,
        state: BarState,
        pending: &'a [PendingOrder],
        views: &'a [TfView<'a>],
        just_closed: u8,
    ) -> Result<Self, StrategyError> {
        if views.is_empty() {
            return Err(StrategyError::NoView);
        }
        let mut declared = 0u8;
        for view in views {
            let bit = view.tf.bit();
            if declared & bit != 0 {
                return Err(StrategyError::DuplicateTimeframe { tf: view.tf });
            }
            declared |= bit;
        }
        if just_closed & !declared != 0 {
            return Err(StrategyError::UnknownJustClosed {
                mask: just_closed,
                declared,
            });
        }
        for view in views {
            if let Some(last) = view
                .closed
                .checked_sub(1)
                .and_then(|index| view.candles.get(index))
                && last.open_time_ms.saturating_add(view.tf.ms()) > state.now_ms
            {
                return Err(StrategyError::LookAhead {
                    tf: view.tf,
                    open_time_ms: last.open_time_ms,
                    now_ms: state.now_ms,
                });
            }
        }
        Ok(BarContext {
            symbol,
            state,
            pending,
            views,
            just_closed,
        })
    }

    pub fn now_ms(&self) -> Ts {
        self.state.now_ms
    }

    pub fn symbol(&self) -> &'a str {
        self.symbol
    }

    pub fn equity(&self) -> f64 {
        self.state.equity
    }

    pub fn quote_free(&self) -> f64 {
        self.state.quote_free
    }

    pub fn qty_free(&self) -> f64 {
        self.state.qty_free
    }

    pub fn position(&self) -> Option<Position> {
        self.state.position
    }

    pub fn fees(&self) -> FeeSchedule {
        self.state.fees
    }

    /// Ordres en attente (au plus `max_pending`, règle du simulateur).
    pub fn pending(&self) -> &'a [PendingOrder] {
        self.pending
    }

    /// Vue du timeframe `tf`, `None` s'il n'est pas déclaré (au plus huit comparaisons d'énumération).
    fn view(&self, tf: Timeframe) -> Option<&'a TfView<'a>> {
        self.views.iter().find(|view| view.tf == tf)
    }

    /// Bougies clôturées de `tf`, chronologiques. Timeframe non déclaré : `&[]`.
    pub fn candles(&self, tf: Timeframe) -> &'a [Candle] {
        self.view(tf).map_or(&[], TfView::visible_candles)
    }

    /// Colonne de l'indicateur `name` sur `tf`, même longueur que [`BarContext::candles`], NaN en
    /// warmup. Timeframe ou nom inconnu : `&[]` (les tests de chaque stratégie attrapent une faute de
    /// frappe). Aucune allocation, aucun hachage : comparaison de `&'static str`.
    pub fn ind(&self, tf: Timeframe, name: &str) -> &'a [f64] {
        match self.view(tf) {
            Some(view) => match view.names.iter().position(|candidate| *candidate == name) {
                Some(index) => view.visible_column(index),
                None => &[],
            },
            None => &[],
        }
    }

    /// Comme [`BarContext::ind`], par index dans l'ordre de `indicators()` restreint à `tf` : le
    /// chemin le plus court. Index hors bornes : `&[]`.
    pub fn ind_at(&self, tf: Timeframe, index: usize) -> &'a [f64] {
        self.view(tf).map_or(&[], |view| view.visible_column(index))
    }

    /// Vrai si une bougie `tf` vient de se clôturer sur cette 1m.
    pub fn just_closed(&self, tf: Timeframe) -> bool {
        self.just_closed & tf.bit() != 0
    }

    /// Dernière bougie clôturée de `tf`.
    pub fn last(&self, tf: Timeframe) -> Option<&'a Candle> {
        self.candles(tf).last()
    }

    /// Ordre en attente du niveau `slot`, s'il existe. Évite qu'une stratégie à plusieurs limits
    /// reposte un niveau ouvert.
    pub fn pending_slot(&self, slot: u16) -> Option<&'a PendingOrder> {
        self.pending.iter().find(|order| order.slot == slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExchangeId, Side};
    use crate::strategy::OrderKind;

    /// 2024-01-01T00:00:00Z en millisecondes.
    const T0: i64 = 1_704_067_200_000;

    /// Bougie H1 de test (valeurs inventées).
    fn candle(open_time_ms: i64, close: f64) -> Candle {
        Candle {
            open_time_ms,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: 1.0,
            trades: None,
        }
    }

    fn state(now_ms: i64) -> BarState {
        BarState {
            now_ms,
            equity: 300.0,
            quote_free: 250.0,
            qty_free: 0.5,
            position: None,
            fees: FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
        }
    }

    fn pending(slot: u16) -> PendingOrder {
        PendingOrder {
            side: Side::Buy,
            kind: OrderKind::Limit {
                price: 99.0,
                ttl_bars: 5,
            },
            qty: 0.1,
            slot,
            tag: "test",
            placed_at_ms: T0,
            ttl_left: 5,
        }
    }

    fn hourly(count: usize) -> Vec<Candle> {
        (0..count)
            .map(|i| candle(T0 + i as i64 * 3_600_000, 100.0 + i as f64))
            .collect()
    }

    // --- TfView ---

    #[test]
    fn tf_view_refuse_un_nombre_de_colonnes_different() {
        let candles = hourly(3);
        let column = [1.0, 2.0, 3.0];
        let columns: [&[f64]; 1] = [&column];
        let names = ["a", "b"];
        let result = TfView::new(Timeframe::H1, &candles, &names, &columns);
        assert!(matches!(
            result,
            Err(StrategyError::IndicatorCountMismatch {
                names: 2,
                columns: 1,
                ..
            })
        ));
    }

    #[test]
    fn tf_view_refuse_une_colonne_de_mauvaise_longueur() {
        let candles = hourly(3);
        let column = [1.0, 2.0];
        let columns: [&[f64]; 1] = [&column];
        let result = TfView::new(Timeframe::H1, &candles, &["a"], &columns);
        assert!(matches!(
            result,
            Err(StrategyError::IndicatorLength {
                name: "a",
                expected: 3,
                found: 2,
                ..
            })
        ));
    }

    #[test]
    fn tf_view_refuse_les_noms_en_double() {
        let candles = hourly(2);
        let column = [1.0, 2.0];
        let columns: [&[f64]; 2] = [&column, &column];
        let result = TfView::new(Timeframe::H1, &candles, &["a", "a"], &columns);
        assert!(matches!(
            result,
            Err(StrategyError::DuplicateIndicator { name: "a", .. })
        ));
    }

    #[test]
    fn with_closed_controle_la_borne() {
        let candles = hourly(3);
        let view = TfView::new(Timeframe::H1, &candles, &[], &[]).unwrap();
        assert_eq!(view.closed(), 0);
        assert_eq!(view.tf(), Timeframe::H1);
        assert_eq!(view.with_closed(3).unwrap().closed(), 3);
        assert_eq!(view.with_closed(0).unwrap().closed(), 0);
        assert!(matches!(
            view.with_closed(4),
            Err(StrategyError::ClosedOutOfRange {
                closed: 4,
                len: 3,
                ..
            })
        ));
    }

    // --- BarContext::new ---

    #[test]
    fn contexte_sans_vue_refuse() {
        assert!(matches!(
            BarContext::new("BTC-USDC", state(T0), &[], &[], 0),
            Err(StrategyError::NoView)
        ));
    }

    #[test]
    fn deux_vues_du_meme_timeframe_refusees() {
        let candles = hourly(2);
        let view = TfView::new(Timeframe::H1, &candles, &[], &[]).unwrap();
        let views = [view, view];
        assert!(matches!(
            BarContext::new("BTC-USDC", state(T0), &[], &views, 0),
            Err(StrategyError::DuplicateTimeframe { tf: Timeframe::H1 })
        ));
    }

    #[test]
    fn just_closed_d_un_timeframe_non_declare_refuse() {
        let candles = hourly(2);
        let views = [TfView::new(Timeframe::H1, &candles, &[], &[]).unwrap()];
        let mask = Timeframe::H1.bit() | Timeframe::D1.bit();
        let result = BarContext::new("BTC-USDC", state(T0), &[], &views, mask);
        assert!(matches!(
            result,
            Err(StrategyError::UnknownJustClosed { mask: m, declared })
                if m == mask && declared == Timeframe::H1.bit()
        ));
    }

    #[test]
    fn look_ahead_et_frontiere_exacte() {
        let candles = hourly(3);
        let view = TfView::new(Timeframe::H1, &candles, &[], &[])
            .unwrap()
            .with_closed(1)
            .unwrap();
        let views = [view];
        // H1 de 00:00 : visible dès 01:00 (clôture de la 1m 00:59), pas avant.
        let h1 = Timeframe::H1.ms();
        assert!(BarContext::new("BTC-USDC", state(T0 + h1), &[], &views, 0).is_ok());
        assert!(matches!(
            BarContext::new("BTC-USDC", state(T0 + h1 - 1), &[], &views, 0),
            Err(StrategyError::LookAhead {
                tf: Timeframe::H1,
                ..
            })
        ));
        // Aucune bougie visible : jamais de look-ahead possible.
        let empty = [view.with_closed(0).unwrap()];
        assert!(BarContext::new("BTC-USDC", state(T0), &[], &empty, 0).is_ok());
    }

    // --- Accesseurs ---

    #[test]
    fn les_accesseurs_sont_bornes_a_closed() {
        let candles = hourly(5);
        let fast = [f64::NAN, 1.0, 2.0, 3.0, 4.0];
        let slow = [f64::NAN, f64::NAN, 10.0, 11.0, 12.0];
        let columns: [&[f64]; 2] = [&fast, &slow];
        let names = ["fast", "slow"];
        let view = TfView::new(Timeframe::H1, &candles, &names, &columns)
            .unwrap()
            .with_closed(3)
            .unwrap();
        let views = [view];
        let now = T0 + 3 * 3_600_000;
        let ctx = BarContext::new("BTC-USDC", state(now), &[], &views, 0).unwrap();

        assert_eq!(ctx.candles(Timeframe::H1), &candles[..3]);
        let f = ctx.ind(Timeframe::H1, "fast");
        assert_eq!(f.len(), 3);
        assert!(f[0].is_nan());
        assert_eq!(&f[1..], &[1.0, 2.0]);
        assert_eq!(ctx.ind(Timeframe::H1, "slow").len(), 3);
        // Timeframe non déclaré, nom inconnu : tranche vide.
        assert!(ctx.candles(Timeframe::D1).is_empty());
        assert!(ctx.ind(Timeframe::D1, "fast").is_empty());
        assert!(ctx.ind(Timeframe::H1, "inconnu").is_empty());
        // ind_at équivaut à ind par index.
        for (i, name) in names.iter().enumerate() {
            let a = ctx.ind_at(Timeframe::H1, i);
            let b = ctx.ind(Timeframe::H1, name);
            assert_eq!(a.len(), b.len());
            assert!(a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()));
        }
        assert!(ctx.ind_at(Timeframe::H1, 2).is_empty());
        assert!(ctx.ind_at(Timeframe::D1, 0).is_empty());
    }

    #[test]
    fn just_closed_et_last() {
        let candles = hourly(3);
        let view = TfView::new(Timeframe::H1, &candles, &[], &[]).unwrap();
        let empty = [view];
        let ctx = BarContext::new("BTC-USDC", state(T0), &[], &empty, Timeframe::H1.bit()).unwrap();
        assert!(ctx.just_closed(Timeframe::H1));
        assert!(!ctx.just_closed(Timeframe::M15));
        assert!(ctx.last(Timeframe::H1).is_none(), "closed == 0");

        let views = [view.with_closed(2).unwrap()];
        let ctx = BarContext::new("BTC-USDC", state(T0 + 2 * 3_600_000), &[], &views, 0).unwrap();
        assert!(!ctx.just_closed(Timeframe::H1));
        assert_eq!(ctx.last(Timeframe::H1), Some(&candles[1]));
    }

    #[test]
    fn slots_et_accesseurs_scalaires() {
        let candles = hourly(1);
        let views = [TfView::new(Timeframe::H1, &candles, &[], &[]).unwrap()];
        let orders = [pending(1), pending(3)];
        let s = state(T0);
        let ctx = BarContext::new("BTC-USDC", s, &orders, &views, 0).unwrap();
        assert_eq!(ctx.pending_slot(3), Some(&orders[1]));
        assert!(ctx.pending_slot(2).is_none());
        assert_eq!(ctx.pending().len(), 2);
        assert_eq!(ctx.now_ms(), T0);
        assert_eq!(ctx.symbol(), "BTC-USDC");
        assert_eq!(ctx.equity(), 300.0);
        assert_eq!(ctx.quote_free(), 250.0);
        assert_eq!(ctx.qty_free(), 0.5);
        assert_eq!(ctx.position(), None);
        assert_eq!(ctx.fees(), s.fees);
    }

    /// Le `Debug` d'un contexte ne montre jamais les valeurs futures ni les colonnes complètes.
    #[test]
    fn le_debug_ne_montre_que_le_visible() {
        let candles = hourly(5);
        let secret = [111.111, 222.222, 333.333, 444.444, 555.555];
        let columns: [&[f64]; 1] = [&secret];
        let view = TfView::new(Timeframe::H1, &candles, &["secret"], &columns)
            .unwrap()
            .with_closed(2)
            .unwrap();
        let views = [view];
        let ctx = BarContext::new("BTC-USDC", state(T0 + 2 * 3_600_000), &[], &views, 0).unwrap();
        let text = format!("{ctx:?}");
        assert!(!text.contains("333.333"), "valeur future exposée : {text}");
        assert!(!text.contains("555.555"), "valeur future exposée : {text}");
        assert!(!text.contains("104"), "bougie future exposée : {text}");
        assert!(text.contains("closed: 2"));
    }
}
