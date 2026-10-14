//! Bougies en colonnes possédées (structure de tableaux), docs/03 §5.1 et §8.
//!
//! Le store échange des `Vec<f64>` avec l'extérieur : aucun type arrow n'en sort (le confinement est
//! vérifié par `tests/architecture.rs`). Les colonnes servent à l'écriture Parquet, au calcul du
//! `content_hash` et à la lecture ; le moteur, lui, garde `&[Candle]` (conversion à la frontière
//! par [`Columns::to_candles`]).

use std::ops::Range;

use crate::model::{Candle, Ts};

use super::StoreError;
use super::layout::Window;

/// Bougies en colonnes, toutes de même longueur.
///
/// `trades` vaut `None` quand la colonne n'a pas été lue (projection à 6 colonnes) : `to_candles`
/// rend alors `trades = None` partout, et le hash comme l'écriture refusent ces colonnes
/// ([`StoreError::MissingTrades`]). `synthetic[i]` est vrai pour une bougie créée par `fill_gaps`
/// (jamais stockée, jamais hachée).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Columns {
    open_time_ms: Vec<Ts>,
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
    trades: Option<Vec<Option<u32>>>,
    synthetic: Vec<bool>,
}

/// Les colonnes rendues par valeur ([`Columns::into_parts`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnsParts {
    pub open_time_ms: Vec<Ts>,
    pub open: Vec<f64>,
    pub high: Vec<f64>,
    pub low: Vec<f64>,
    pub close: Vec<f64>,
    pub volume: Vec<f64>,
    pub trades: Option<Vec<Option<u32>>>,
    /// Vide = aucune ligne synthétique.
    pub synthetic: Vec<bool>,
}

impl Columns {
    /// Vide, avec la colonne `trades` (`Some(vec![])`).
    pub fn with_capacity(n: usize) -> Columns {
        Columns {
            open_time_ms: Vec::with_capacity(n),
            open: Vec::with_capacity(n),
            high: Vec::with_capacity(n),
            low: Vec::with_capacity(n),
            close: Vec::with_capacity(n),
            volume: Vec::with_capacity(n),
            trades: Some(Vec::with_capacity(n)),
            synthetic: Vec::with_capacity(n),
        }
    }

    /// Valide les longueurs ([`StoreError::ColumnLengths`]) ; `synthetic` vide = tout faux.
    pub fn from_parts(parts: ColumnsParts) -> Result<Columns, StoreError> {
        let n = parts.open_time_ms.len();
        let mut lengths = vec![
            ("open", parts.open.len()),
            ("high", parts.high.len()),
            ("low", parts.low.len()),
            ("close", parts.close.len()),
            ("volume", parts.volume.len()),
        ];
        if let Some(trades) = &parts.trades {
            lengths.push(("trades", trades.len()));
        }
        if !parts.synthetic.is_empty() {
            lengths.push(("synthetic", parts.synthetic.len()));
        }
        if let Some((name, len)) = lengths.iter().find(|(_, len)| *len != n) {
            return Err(StoreError::ColumnLengths {
                detail: format!("open_time_ms = {n}, {name} = {len}"),
            });
        }
        let synthetic = if parts.synthetic.is_empty() {
            vec![false; n]
        } else {
            parts.synthetic
        };
        Ok(Columns {
            open_time_ms: parts.open_time_ms,
            open: parts.open,
            high: parts.high,
            low: parts.low,
            close: parts.close,
            volume: parts.volume,
            trades: parts.trades,
            synthetic,
        })
    }

    /// Rend les colonnes par valeur.
    pub fn into_parts(self) -> ColumnsParts {
        ColumnsParts {
            open_time_ms: self.open_time_ms,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            trades: self.trades,
            synthetic: self.synthetic,
        }
    }

    /// Copie de bougies (`trades` conservés, aucune synthétique).
    pub fn from_candles(candles: &[Candle]) -> Columns {
        let mut columns = Columns::with_capacity(candles.len());
        for candle in candles {
            columns.push(candle, false);
        }
        columns
    }

    /// Ajoute une bougie. Si `trades` n'est pas lu, `candle.trades` est ignoré.
    pub fn push(&mut self, candle: &Candle, synthetic: bool) {
        self.open_time_ms.push(candle.open_time_ms);
        self.open.push(candle.open);
        self.high.push(candle.high);
        self.low.push(candle.low);
        self.close.push(candle.close);
        self.volume.push(candle.volume);
        if let Some(trades) = &mut self.trades {
            trades.push(candle.trades);
        }
        self.synthetic.push(synthetic);
    }

    /// Nombre de lignes.
    pub fn len(&self) -> usize {
        self.open_time_ms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open_time_ms.is_empty()
    }

    pub fn open_time_ms(&self) -> &[Ts] {
        &self.open_time_ms
    }

    pub fn open(&self) -> &[f64] {
        &self.open
    }

    pub fn high(&self) -> &[f64] {
        &self.high
    }

    pub fn low(&self) -> &[f64] {
        &self.low
    }

    pub fn close(&self) -> &[f64] {
        &self.close
    }

    pub fn volume(&self) -> &[f64] {
        &self.volume
    }

    /// `None` si la colonne n'a pas été lue (projection à 6 colonnes).
    pub fn trades(&self) -> Option<&[Option<u32>]> {
        self.trades.as_deref()
    }

    /// Masque des bougies synthétiques (`fill_gaps`).
    pub fn synthetic(&self) -> &[bool] {
        &self.synthetic
    }

    /// Nombre de bougies synthétiques.
    pub fn synthetic_count(&self) -> usize {
        self.synthetic.iter().filter(|s| **s).count()
    }

    /// La bougie `index` (`trades = None` si la colonne n'est pas lue), `None` hors bornes.
    pub fn get(&self, index: usize) -> Option<Candle> {
        Some(Candle {
            open_time_ms: *self.open_time_ms.get(index)?,
            open: *self.open.get(index)?,
            high: *self.high.get(index)?,
            low: *self.low.get(index)?,
            close: *self.close.get(index)?,
            volume: *self.volume.get(index)?,
            trades: self
                .trades
                .as_ref()
                .and_then(|trades| trades.get(index).copied().flatten()),
        })
    }

    /// Conversion à la frontière du moteur (D6) : `engine::run_cell` garde `&[Candle]`.
    pub fn to_candles(&self) -> Vec<Candle> {
        (0..self.len()).filter_map(|i| self.get(i)).collect()
    }

    /// Copie des lignes `range` (panique hors bornes, comme un slice : réservée au crate, appelée
    /// sur des plages valides par construction).
    pub(crate) fn slice(&self, range: Range<usize>) -> Columns {
        Columns {
            open_time_ms: self.open_time_ms[range.clone()].to_vec(),
            open: self.open[range.clone()].to_vec(),
            high: self.high[range.clone()].to_vec(),
            low: self.low[range.clone()].to_vec(),
            close: self.close[range.clone()].to_vec(),
            volume: self.volume[range.clone()].to_vec(),
            trades: self.trades.as_ref().map(|t| t[range.clone()].to_vec()),
            synthetic: self.synthetic[range].to_vec(),
        }
    }

    /// Concatène `other` à la suite ([`StoreError::ColumnLengths`] si l'un a `trades` et l'autre non).
    pub fn extend_from(&mut self, other: &Columns) -> Result<(), StoreError> {
        match (&mut self.trades, &other.trades) {
            (Some(mine), Some(theirs)) => mine.extend_from_slice(theirs),
            (None, None) => {}
            _ => {
                return Err(StoreError::ColumnLengths {
                    detail: "la colonne trades est lue d'un côté seulement".to_string(),
                });
            }
        }
        self.open_time_ms.extend_from_slice(&other.open_time_ms);
        self.open.extend_from_slice(&other.open);
        self.high.extend_from_slice(&other.high);
        self.low.extend_from_slice(&other.low);
        self.close.extend_from_slice(&other.close);
        self.volume.extend_from_slice(&other.volume);
        self.synthetic.extend_from_slice(&other.synthetic);
        Ok(())
    }

    /// Garde les lignes de `window` (en place, l'ordre est conservé).
    pub fn retain_window(&mut self, window: Window) {
        let keep: Vec<bool> = self
            .open_time_ms
            .iter()
            .map(|ts| window.contains(*ts))
            .collect();
        if keep.iter().all(|k| *k) {
            return;
        }
        fn filtre<T: Copy>(values: &mut Vec<T>, keep: &[bool]) {
            let mut flags = keep.iter();
            values.retain(|_| flags.next().copied().unwrap_or(false));
        }
        filtre(&mut self.open_time_ms, &keep);
        filtre(&mut self.open, &keep);
        filtre(&mut self.high, &keep);
        filtre(&mut self.low, &keep);
        filtre(&mut self.close, &keep);
        filtre(&mut self.volume, &keep);
        filtre(&mut self.synthetic, &keep);
        if let Some(trades) = &mut self.trades {
            filtre(trades, &keep);
        }
    }

    /// [`StoreError::NotStrictlyIncreasing`] à la première violation de l'ordre strict des
    /// `open_time_ms`.
    pub fn check_strictly_increasing(&self) -> Result<(), StoreError> {
        for (index, pair) in self.open_time_ms.windows(2).enumerate() {
            if pair[1] <= pair[0] {
                return Err(StoreError::NotStrictlyIncreasing {
                    index: index + 1,
                    previous_ms: pair[0],
                    open_time_ms: pair[1],
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bougies synthétiques (prix inventés) avec un `-0.0` et des `trades` variés.
    fn exemple() -> Vec<Candle> {
        vec![
            Candle {
                open_time_ms: 60_000,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume: 1.5,
                trades: Some(3),
            },
            Candle {
                open_time_ms: 120_000,
                open: 100.5,
                high: 100.5,
                low: 100.5,
                close: 100.5,
                volume: -0.0,
                trades: None,
            },
            Candle {
                open_time_ms: 180_000,
                open: 100.5,
                high: 102.0,
                low: 100.0,
                close: 101.0,
                volume: 2.0,
                trades: Some(0),
            },
        ]
    }

    #[test]
    fn aller_retour_bougies_colonnes() {
        let bougies = exemple();
        let colonnes = Columns::from_candles(&bougies);
        assert_eq!(colonnes.len(), 3);
        assert!(colonnes.synthetic().iter().all(|s| !s));
        let retour = colonnes.to_candles();
        assert_eq!(retour, bougies);
        // `==` ne distingue pas -0.0 de 0.0 : on compare les bits.
        assert_eq!(retour[1].volume.to_bits(), (-0.0_f64).to_bits());
        assert_eq!(colonnes.get(0), Some(bougies[0]));
        assert_eq!(colonnes.get(3), None);
    }

    #[test]
    fn longueurs_incoherentes_refusees() {
        let mut parts = Columns::from_candles(&exemple()).into_parts();
        parts.close.pop();
        assert!(matches!(
            Columns::from_parts(parts),
            Err(StoreError::ColumnLengths { .. })
        ));
        let mut parts = Columns::from_candles(&exemple()).into_parts();
        parts.synthetic = vec![true];
        assert!(Columns::from_parts(parts).is_err());
        // `synthetic` vide = tout faux.
        let mut parts = Columns::from_candles(&exemple()).into_parts();
        parts.synthetic = Vec::new();
        let colonnes = Columns::from_parts(parts).unwrap();
        assert_eq!(colonnes.synthetic(), &[false, false, false]);
    }

    #[test]
    fn projection_sans_trades() {
        let mut parts = Columns::from_candles(&exemple()).into_parts();
        parts.trades = None;
        let sans = Columns::from_parts(parts).unwrap();
        assert!(sans.trades().is_none());
        assert!(sans.to_candles().iter().all(|c| c.trades.is_none()));
        let mut avec = Columns::from_candles(&exemple());
        assert!(matches!(
            avec.extend_from(&sans),
            Err(StoreError::ColumnLengths { .. })
        ));
        let mut sans2 = sans.clone();
        assert!(sans2.extend_from(&avec.clone()).is_err());
        sans2.extend_from(&sans).unwrap();
        assert_eq!(sans2.len(), 6);
        // push ignore trades quand la colonne n'est pas lue.
        let mut vide = sans.slice(0..0);
        vide.push(&exemple()[0], false);
        assert_eq!(vide.len(), 1);
        assert!(vide.trades().is_none());
    }

    #[test]
    fn fenetre_et_croissance() {
        let mut colonnes = Columns::from_candles(&exemple());
        colonnes.retain_window(Window::new(100_000, 180_000).unwrap());
        assert_eq!(colonnes.open_time_ms(), &[120_000]);
        assert_eq!(colonnes.trades().unwrap(), &[None]);
        assert!(colonnes.check_strictly_increasing().is_ok());

        let mut doublon = Columns::from_candles(&exemple());
        doublon.push(&exemple()[2], false);
        assert!(matches!(
            doublon.check_strictly_increasing(),
            Err(StoreError::NotStrictlyIncreasing {
                index: 3,
                previous_ms: 180_000,
                open_time_ms: 180_000
            })
        ));
        let mut desordre = Columns::from_candles(&exemple());
        desordre.push(&exemple()[0], false);
        assert!(desordre.check_strictly_increasing().is_err());
    }

    #[test]
    fn tranche_et_concatenation() {
        let colonnes = Columns::from_candles(&exemple());
        let mut gauche = colonnes.slice(0..2);
        let droite = colonnes.slice(2..3);
        gauche.extend_from(&droite).unwrap();
        assert_eq!(gauche, colonnes);
        assert_eq!(colonnes.synthetic_count(), 0);
    }
}
