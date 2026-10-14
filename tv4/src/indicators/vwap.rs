//! VWAP ancré sur la session journalière UTC.

use super::Indicator;
use crate::model::Candle;

/// Durée d'une session : un jour UTC.
const SESSION_MS: i64 = 86_400_000;

/// VWAP ancré à 00:00 UTC, entrée : bougie. **Choix à valider par le propriétaire.**
///
/// Prix typique `tp = (high + low + close) / 3`, `VWAP = Σ(tp × volume) / Σ volume` depuis le début de
/// la session (le jour UTC de l'`open_time`).
///
/// Pourquoi une session journalière UTC :
///
/// - un VWAP cumulé depuis le début du dataset dépendrait du point de départ ; avec un ancrage à
///   00:00 UTC, mêmes frontières que les bougies 1d de docs/03 §5, toute valeur finie ne dépend que
///   des bougies de sa session ;
/// - la première session n'est valide que si la première bougie ouvre à 00:00 ; sinon elle rend NaN,
///   ce qui sert de warmup : deux datasets de départs différents ne diffèrent que par NaN contre
///   valeur, jamais par deux valeurs finies différentes ;
/// - volume cumulé nul (session commencée par des bougies à volume 0) : on rend `tp`, la limite d'un
///   VWAP de volume infinitésimal ; la valeur reste finie et causale ;
/// - le warmup peut durer jusqu'à une session, soit 1 440 bougies 1m : une stratégie qui utilise le
///   VWAP en 1m doit le refléter dans `warmup_bars` ;
/// - sur une série 1d, la valeur est le `tp` de chaque bougie.
#[derive(Debug, Clone, Default)]
pub struct Vwap {
    session: Option<i64>,
    valid: bool,
    cum_pv: f64,
    cum_v: f64,
}

impl Vwap {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Indicator for Vwap {
    type Input = Candle;

    fn update(&mut self, c: Candle) -> f64 {
        debug_assert!(
            c.high.is_finite() && c.low.is_finite() && c.close.is_finite() && c.volume.is_finite(),
            "vwap : bougie non finie"
        );
        // `div_euclid` : la bonne session aussi avant 1970.
        let day = c.open_time_ms.div_euclid(SESSION_MS);
        match self.session {
            None => {
                self.session = Some(day);
                self.valid = c.open_time_ms.rem_euclid(SESSION_MS) == 0;
                self.cum_pv = 0.0;
                self.cum_v = 0.0;
            }
            Some(s) if s != day => {
                self.session = Some(day);
                self.valid = true;
                self.cum_pv = 0.0;
                self.cum_v = 0.0;
            }
            Some(_) => {}
        }
        let tp = (c.high + c.low + c.close) / 3.0;
        self.cum_pv += tp * c.volume;
        self.cum_v += c.volume;
        if !self.valid {
            f64::NAN
        } else if self.cum_v > 0.0 {
            self.cum_pv / self.cum_v
        } else {
            tp
        }
    }

    fn reset(&mut self) {
        *self = Vwap::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::batch;

    /// 2024-01-01T00:00:00Z en millisecondes.
    const MIDNIGHT: i64 = 1_704_067_200_000;
    const MINUTE: i64 = 60_000;

    /// Bougie de test (valeurs inventées) : high, low, close, volume ; open = close.
    fn candle(open_time_ms: i64, high: f64, low: f64, close: f64, volume: f64) -> Candle {
        Candle {
            open_time_ms,
            open: close,
            high,
            low,
            close,
            volume,
            trades: None,
        }
    }

    #[test]
    fn depart_a_minuit() {
        // tp_0 = (12+9+9)/3 = 10 ; tp_1 = (15+12+12)/3 = 13.
        // VWAP_1 = (10 × 1 + 13 × 2) / 3 = 12.
        let out = batch(
            Vwap::new(),
            [
                candle(MIDNIGHT, 12.0, 9.0, 9.0, 1.0),
                candle(MIDNIGHT + MINUTE, 15.0, 12.0, 12.0, 2.0),
            ],
        );
        assert_eq!(out, [10.0, 12.0]);
    }

    #[test]
    fn le_jour_suivant_remet_a_zero() {
        let out = batch(
            Vwap::new(),
            [
                candle(MIDNIGHT, 12.0, 9.0, 9.0, 1.0),
                candle(MIDNIGHT + 86_400_000, 30.0, 27.0, 30.0, 5.0),
            ],
        );
        // tp = 29 sur la seule bougie de la nouvelle session.
        assert_eq!(out[1], 29.0);
    }

    #[test]
    fn depart_en_cours_de_session_est_nan_jusqu_au_jour_suivant() {
        let out = batch(
            Vwap::new(),
            [
                candle(MIDNIGHT + 5 * MINUTE, 12.0, 9.0, 9.0, 1.0),
                candle(MIDNIGHT + 6 * MINUTE, 12.0, 9.0, 9.0, 1.0),
                candle(MIDNIGHT + 86_400_000, 30.0, 27.0, 30.0, 5.0),
            ],
        );
        assert!(out[0].is_nan() && out[1].is_nan());
        assert_eq!(out[2], 29.0);
    }

    #[test]
    fn volume_nul_en_debut_de_session_rend_le_prix_typique() {
        let out = batch(
            Vwap::new(),
            [
                candle(MIDNIGHT, 12.0, 9.0, 9.0, 0.0),
                candle(MIDNIGHT + MINUTE, 15.0, 12.0, 12.0, 0.0),
                candle(MIDNIGHT + 2 * MINUTE, 15.0, 12.0, 12.0, 2.0),
            ],
        );
        assert_eq!(out[0], 10.0);
        assert_eq!(out[1], 13.0);
        assert_eq!(out[2], 13.0);
    }

    #[test]
    fn open_time_negatif_utilise_la_bonne_session() {
        // -1 min est le 31/12/1969 23:59 : session -1, valide seulement si la 1re bougie ouvre à minuit.
        let before = batch(
            Vwap::new(),
            [
                candle(-MINUTE, 12.0, 9.0, 9.0, 1.0),
                candle(0, 15.0, 12.0, 12.0, 2.0),
            ],
        );
        assert!(before[0].is_nan());
        assert_eq!(before[1], 13.0, "1970-01-01 est une nouvelle session");
        // -1 jour exactement : minuit de la session -1, valide.
        let midnight = batch(Vwap::new(), [candle(-86_400_000, 12.0, 9.0, 9.0, 1.0)]);
        assert_eq!(midnight[0], 10.0);
    }

    #[test]
    fn new_est_default() {
        let a = format!("{:?}", Vwap::new());
        let b = format!("{:?}", Vwap::default());
        assert_eq!(a, b);
    }

    #[test]
    fn reset_redonne_une_instance_neuve() {
        let candles = [
            candle(MIDNIGHT + 5 * MINUTE, 12.0, 9.0, 9.0, 1.0),
            candle(MIDNIGHT + 86_400_000, 30.0, 27.0, 30.0, 5.0),
        ];
        let mut vwap = Vwap::new();
        let first: Vec<u64> = candles.iter().map(|&c| vwap.update(c).to_bits()).collect();
        vwap.reset();
        let second: Vec<u64> = candles.iter().map(|&c| vwap.update(c).to_bits()).collect();
        assert_eq!(first, second);
    }
}
