//! Série de régime BTC 1d / SMA200 du moteur portefeuille (docs S9 §6.2 et §6.3).
//!
//! `strategy` ne dépend que de `model` : [`RegimeView`] ne porte que des `Candle` et des `f64`. Le
//! moteur fait le reste ici, une fois avant la boucle : agrégation des 1m de la paire de régime en 1d
//! ([`Mtf`]) et SMA200 des clôtures 1d (colonne complète de [`batch`], `NaN` en warmup).
//!
//! À chaque appel d'une stratégie qui lit le régime, [`RegimeFeed::advance`] avance un pointeur dans les
//! 1m de la paire de régime tant que `open + 60 000 ≤ now_ms`, et lit le nombre de 1d closes à cet
//! indice (`closed_count` de [`Mtf`]). Le coût est O(1) amorti. Une 1m absente de la paire de régime
//! retarde la clôture de sa 1d : c'est la règle des vues de chaque jambe, et elle est prudente (jamais
//! de bougie non close).
//!
//! La paire de régime n'est pas dans la grille : si elle n'est pas dans l'univers, elle ne trade pas et
//! n'a pas de rang. Sa série vient du dataset, donc du `dataset_hash`.

use crate::indicators::{Sma, batch};
use crate::model::{Candle, Timeframe, Ts};
use crate::mtf::Mtf;
use crate::strategy::{REGIME_SMA_PERIOD, REGIME_TF, RegimeView};

use super::EngineError;

/// Durée d'un jour UTC en millisecondes.
const DAY_MS: Ts = 86_400_000;
/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = 60_000;

/// Bougies 1d de la paire de régime, SMA200 et pointeur de lecture.
pub(crate) struct RegimeFeed<'a> {
    mtf: Mtf<'a>,
    sma: Vec<f64>,
    /// Nombre de 1m de la paire de régime déjà closes à la dernière `now_ms` demandée.
    consumed: usize,
}

impl<'a> RegimeFeed<'a> {
    /// Construit la série de régime sur les 1m de `m1`. Les 1m ont déjà été contrôlées (alignées,
    /// strictement croissantes) par l'appelant ; `Mtf::build` les contrôle de toute façon.
    pub(crate) fn new(m1: &'a [Candle]) -> Result<RegimeFeed<'a>, EngineError> {
        let mtf = Mtf::build(m1, &[REGIME_TF])?;
        let closes = mtf.candles(REGIME_TF).unwrap_or(&[]);
        let sma = batch(Sma::new(REGIME_SMA_PERIOD)?, closes.iter().map(|c| c.close));
        Ok(RegimeFeed {
            mtf,
            sma,
            consumed: 0,
        })
    }

    /// Bougies 1d de la paire de régime (la dernière peut être incomplète).
    fn daily(&self) -> &[Candle] {
        self.mtf.candles(REGIME_TF).unwrap_or(&[])
    }

    /// Ouverture (ms) de la première 1m de la paire de régime à laquelle la SMA200 existe, c'est-à-dire
    /// à laquelle 200 bougies 1d sont closes. `None` si elle n'existe jamais dans les données.
    pub(crate) fn first_ready_open_ms(&self) -> Option<Ts> {
        let counts = self.mtf.closed_counts(REGIME_TF)?;
        let index = counts.iter().position(|&n| n >= REGIME_SMA_PERIOD)?;
        self.mtf.m1().get(index).map(|c| c.open_time_ms)
    }

    /// Nombre de bougies 1d closes à `now_ms`. `now_ms` ne doit pas décroître d'un appel à l'autre.
    pub(crate) fn advance(&mut self, now_ms: Ts) -> usize {
        let m1 = self.mtf.m1();
        while m1
            .get(self.consumed)
            .is_some_and(|c| c.open_time_ms.saturating_add(MINUTE_MS) <= now_ms)
        {
            self.consumed += 1;
        }
        if self.consumed == 0 {
            return 0;
        }
        self.mtf
            .closed_counts(REGIME_TF)
            .and_then(|counts| counts.get(self.consumed - 1))
            .copied()
            .unwrap_or(0)
    }

    /// Vue de régime sur les `closed` premières bougies 1d (`closed` vient de [`RegimeFeed::advance`]).
    pub(crate) fn view(&self, closed: usize) -> Result<RegimeView<'_>, EngineError> {
        let candles = self.daily().get(..closed).unwrap_or(&[]);
        let sma = self.sma.get(..closed).unwrap_or(&[]);
        Ok(RegimeView::new(candles, sma)?)
    }

    /// Jours UTC de `[from_ms, to_ms]` où le régime est haussier et où il est baissier, jugé sur la
    /// dernière 1d close du jour d'avant (`close > SMA200` strict, donc `close ≤ SMA200` baissier).
    /// Un jour sans 1d de la veille ou sans SMA n'est compté nulle part.
    pub(crate) fn day_counts(&self, from_ms: Ts, to_ms: Ts) -> (u64, u64) {
        let daily = self.daily();
        let mut bull = 0u64;
        let mut bear = 0u64;
        if to_ms < from_ms {
            return (0, 0);
        }
        for day in from_ms.div_euclid(DAY_MS)..=to_ms.div_euclid(DAY_MS) {
            let previous_open = (day - 1) * DAY_MS;
            let Ok(index) = daily.binary_search_by_key(&previous_open, |c| c.open_time_ms) else {
                continue;
            };
            let (Some(candle), Some(&sma)) = (daily.get(index), self.sma.get(index)) else {
                continue;
            };
            if !sma.is_finite() || !candle.close.is_finite() {
                continue;
            }
            if candle.close > sma {
                bull += 1;
            } else {
                bear += 1;
            }
        }
        (bull, bear)
    }
}

/// Le timeframe de régime est le 1d (garde de compilation de l'unité utilisée ci-dessus).
const _: () = assert!(matches!(REGIME_TF, Timeframe::D1));

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-01-01T00:00:00Z.
    const T0: Ts = 1_704_067_200_000;

    /// `days` jours de 1m (1 440 par jour), close = `100 + jour` (tendance haussière).
    fn minutes(days: i64) -> Vec<Candle> {
        let mut out = Vec::new();
        for d in 0..days {
            for m in 0..1440 {
                let price = 100.0 + d as f64;
                out.push(Candle {
                    open_time_ms: T0 + d * DAY_MS + m * MINUTE_MS,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume: 1.0,
                    trades: None,
                });
            }
        }
        out
    }

    #[test]
    fn la_sma_apparait_apres_200_cloture_1d() {
        let m1 = minutes(203);
        let mut feed = RegimeFeed::new(&m1).unwrap();
        // Premier instant où 200 bougies 1d sont closes : fin de la minute 23:59 du jour 199.
        let ready = feed.first_ready_open_ms().unwrap();
        assert_eq!(ready, T0 + 199 * DAY_MS + 1439 * MINUTE_MS);
        // Avant : pas de SMA. Le jour 199 n'est pas clos à 23:58.
        let closed = feed.advance(T0 + 199 * DAY_MS + 1439 * MINUTE_MS);
        assert_eq!(closed, 199);
        assert_eq!(feed.view(closed).unwrap().sma(), None);
        // À 00:00 du jour 200 : 200 bougies closes et SMA200 finie.
        let closed = feed.advance(T0 + 200 * DAY_MS);
        assert_eq!(closed, 200);
        let view = feed.view(closed).unwrap();
        // SMA des clôtures 100..299 : moyenne = 199,5.
        assert_eq!(view.sma(), Some(199.5));
        // Clôture 299 > 199,5 : haussier.
        assert_eq!(view.bull(), Some(true));
    }

    #[test]
    fn la_serie_trop_courte_n_a_jamais_de_sma() {
        let m1 = minutes(50);
        let feed = RegimeFeed::new(&m1).unwrap();
        assert_eq!(feed.first_ready_open_ms(), None);
    }

    #[test]
    fn une_minute_absente_retarde_la_cloture() {
        let mut m1 = minutes(3);
        // Retire la 1m 23:59 du jour 0.
        m1.retain(|c| c.open_time_ms != T0 + 1439 * MINUTE_MS);
        let mut feed = RegimeFeed::new(&m1).unwrap();
        // À 00:00 du jour 1, la 1d du jour 0 n'est pas encore close (sa dernière 1m manque).
        assert_eq!(feed.advance(T0 + DAY_MS), 0);
        // Elle l'est à la fin de la première 1m du jour 1.
        assert_eq!(feed.advance(T0 + DAY_MS + MINUTE_MS), 1);
    }

    #[test]
    fn jours_haussiers_et_baissiers() {
        // 205 jours haussiers puis une chute : les clôtures passent sous la SMA200.
        let mut m1 = minutes(230);
        for c in &mut m1 {
            if c.open_time_ms >= T0 + 210 * DAY_MS {
                c.open = 50.0;
                c.high = 50.0;
                c.low = 50.0;
                c.close = 50.0;
            }
        }
        let feed = RegimeFeed::new(&m1).unwrap();
        let (bull, bear) = feed.day_counts(T0 + 205 * DAY_MS, T0 + 225 * DAY_MS);
        // Jour d : juge la 1d du jour d - 1. Haussiers tant que le jour d - 1 ≤ 209 : d = 205..=210.
        assert_eq!(bull, 6);
        assert_eq!(bear, 15);
        assert_eq!(feed.day_counts(T0 + 10 * DAY_MS, T0 + 20 * DAY_MS), (0, 0));
        assert_eq!(feed.day_counts(T0 + 20 * DAY_MS, T0 + 10 * DAY_MS), (0, 0));
    }
}
