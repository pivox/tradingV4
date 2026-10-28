//! Rang de liquidité 90 j et priorité des signaux d'une même minute (docs S9 §5).
//!
//! La règle de docs/04 §3.1 point 5 est « horodatage du signal, puis rang de liquidité 90 j, jamais
//! l'ordre alphabétique ». L'horodatage est départagé par construction (un signal plus ancien est
//! traité à une minute plus ancienne). Ce module ne calcule que le rang :
//!
//! * **volume quote du jour** d'une jambe = Σ `volume × close` de ses 1m du jour UTC, jours du warmup
//!   compris (le rang mesure la liquidité, pas un plafond prudent de fill : `close`, pas `low`) ;
//! * **score du jour `d`** = somme des volumes des jours `d − 90 … d − 1`, soit 90 jours complets avant
//!   `d`. Il ne dépend que de données closes à 00:00 UTC du jour `d` : il est causal. Avant 90 jours de
//!   données, la somme porte sur les jours disponibles (les jours sans bougie valent 0) et le jour est
//!   compté dans `partial_days` ;
//! * **ordre** : score décroissant ([`f64::total_cmp`]), puis position dans l'univers de la
//!   configuration en cas d'égalité exacte (deux scores nuls compris). Jamais l'alphabet.
//!
//! La table `[jours × jambes]` est précalculée avant la boucle ; la boucle lit une ligne en O(1), sans tri
//! ni allocation. Chaque score est recalculé de zéro sur ses 90 valeurs dans l'ordre des jours : pas de
//! somme glissante, donc pas de dérive.

use crate::model::{Candle, Ts};

use super::portfolio_config::LIQUIDITY_WINDOW_DAYS;

/// Durée d'un jour UTC en millisecondes.
const DAY_MS: Ts = 86_400_000;

/// Table de rang précalculée : pour chaque jour UTC, les indices des jambes du mieux classé au moins bien.
#[derive(Clone, Debug)]
pub struct RankTable {
    /// Début (00:00 UTC) du premier jour de la table.
    first_day_ms: Ts,
    legs: usize,
    days: usize,
    /// `days × legs` indices de jambes, ligne par jour.
    rows: Vec<u16>,
    /// Scores `days × legs`, gardés pour les tests et le rapport.
    scores: Vec<f64>,
}

impl RankTable {
    /// Construit la table sur les bougies 1m de chaque jambe (une tranche par jambe, dans l'ordre de
    /// l'univers). Une jambe sans bougie a un volume nul tous les jours. La table couvre du premier
    /// jour de données au lendemain du dernier (le rang d'une minute se lit au jour de `now_ms`, qui
    /// peut être minuit du jour suivant).
    pub fn build(series: &[&[Candle]]) -> RankTable {
        let legs = series.len();
        let mut first: Option<Ts> = None;
        let mut last: Option<Ts> = None;
        for candles in series {
            if let (Some(a), Some(b)) = (candles.first(), candles.last()) {
                let a = a.open_time_ms.div_euclid(DAY_MS);
                let b = b.open_time_ms.div_euclid(DAY_MS);
                first = Some(first.map_or(a, |f: Ts| f.min(a)));
                last = Some(last.map_or(b, |l: Ts| l.max(b)));
            }
        }
        let (first_day, last_day) = match (first, last) {
            (Some(a), Some(b)) => (a, b),
            _ => (0, 0),
        };
        let days = usize::try_from(last_day - first_day + 2).unwrap_or(1);
        // Volumes quote par jour, rangés `[jambe][jour]`, accumulés dans l'ordre chronologique.
        let mut volume = vec![0.0_f64; legs * days];
        for (k, candles) in series.iter().enumerate() {
            for candle in *candles {
                let day = usize::try_from(candle.open_time_ms.div_euclid(DAY_MS) - first_day)
                    .unwrap_or(0);
                if let Some(slot) = volume.get_mut(k * days + day) {
                    *slot += candle.volume * candle.close;
                }
            }
        }
        let window = LIQUIDITY_WINDOW_DAYS as usize;
        let mut scores = vec![0.0_f64; days * legs];
        for day in 0..days {
            let from = day.saturating_sub(window);
            for k in 0..legs {
                let mut sum = 0.0_f64;
                for j in from..day {
                    sum += volume.get(k * days + j).copied().unwrap_or(0.0);
                }
                if let Some(slot) = scores.get_mut(day * legs + k) {
                    *slot = sum;
                }
            }
        }
        let mut rows = Vec::with_capacity(days * legs);
        let mut order: Vec<u16> = Vec::with_capacity(legs);
        for day in 0..days {
            order.clear();
            order.extend((0..legs).map(|k| u16::try_from(k).unwrap_or(u16::MAX)));
            let row = scores.get(day * legs..(day + 1) * legs).unwrap_or(&[]);
            order.sort_by(|&a, &b| {
                let sa = row.get(usize::from(a)).copied().unwrap_or(0.0);
                let sb = row.get(usize::from(b)).copied().unwrap_or(0.0);
                sb.total_cmp(&sa).then(a.cmp(&b))
            });
            rows.extend_from_slice(&order);
        }
        RankTable {
            first_day_ms: first_day * DAY_MS,
            legs,
            days,
            rows,
            scores,
        }
    }

    /// Nombre de jambes classées.
    pub fn legs(&self) -> usize {
        self.legs
    }

    /// Nombre de jours de la table.
    pub fn days(&self) -> usize {
        self.days
    }

    /// Indice du jour UTC de `ts_ms` dans la table, borné à ses limites.
    fn day_of(&self, ts_ms: Ts) -> usize {
        let day = (ts_ms - self.first_day_ms).div_euclid(DAY_MS);
        usize::try_from(day)
            .unwrap_or(0)
            .min(self.days.saturating_sub(1))
    }

    /// Indices des jambes dans l'ordre du rang du jour UTC de `now_ms`, du mieux classé au moins bien.
    pub fn order_at(&self, now_ms: Ts) -> &[u16] {
        let day = self.day_of(now_ms);
        self.rows
            .get(day * self.legs..(day + 1) * self.legs)
            .unwrap_or(&[])
    }

    /// Score de la jambe `leg` au jour UTC de `now_ms` (somme des volumes quote des 90 jours précédents).
    pub fn score_at(&self, now_ms: Ts, leg: usize) -> f64 {
        self.scores
            .get(self.day_of(now_ms) * self.legs + leg)
            .copied()
            .unwrap_or(0.0)
    }

    /// Nombre de jours UTC de `[from_ms, to_ms]` qui ont moins de 90 jours de données avant eux.
    pub fn partial_days(&self, from_ms: Ts, to_ms: Ts) -> u64 {
        if to_ms < from_ms {
            return 0;
        }
        let window = i64::from(LIQUIDITY_WINDOW_DAYS);
        let first = from_ms.div_euclid(DAY_MS);
        let last = to_ms.div_euclid(DAY_MS);
        let table_first = self.first_day_ms.div_euclid(DAY_MS);
        let mut count = 0u64;
        for day in first..=last {
            if day - table_first < window {
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-01-01T00:00:00Z.
    const T0: Ts = 1_704_067_200_000;

    fn bar(open_time_ms: Ts, close: f64, volume: f64) -> Candle {
        Candle {
            open_time_ms,
            open: close,
            high: close,
            low: close,
            close,
            volume,
            trades: None,
        }
    }

    /// Une bougie par jour à midi.
    fn daily(days: i64, close: f64, volume: f64) -> Vec<Candle> {
        (0..days)
            .map(|d| bar(T0 + d * DAY_MS + 12 * 3_600_000, close, volume))
            .collect()
    }

    #[test]
    fn le_plus_liquide_passe_en_premier_puis_l_ordre_de_l_univers() {
        let a = daily(120, 10.0, 1.0);
        let z = daily(120, 10.0, 5.0);
        let now = T0 + 100 * DAY_MS;
        let table = RankTable::build(&[&a, &z]);
        assert_eq!(table.order_at(now), &[1, 0]);
        // Égalité exacte : l'ordre de l'univers, quel que soit le nom.
        let table = RankTable::build(&[&a, &a]);
        assert_eq!(table.order_at(now), &[0, 1]);
        // Deux scores nuls (aucune donnée avant le premier jour) : l'ordre de l'univers.
        assert_eq!(table.order_at(T0), &[0, 1]);
    }

    #[test]
    fn le_score_est_la_somme_des_90_jours_complets_precedents() {
        let a = daily(200, 2.0, 3.0);
        let table = RankTable::build(&[&a]);
        // Jour 100 : jours 10 à 99, soit 90 jours de 2 × 3 = 6.
        assert_eq!(table.score_at(T0 + 100 * DAY_MS, 0), 540.0);
        // Jour 5 : cinq jours disponibles (0 à 4).
        assert_eq!(table.score_at(T0 + 5 * DAY_MS, 0), 30.0);
        // Jour 0 : rien avant.
        assert_eq!(table.score_at(T0, 0), 0.0);
    }

    #[test]
    fn le_rang_du_jour_ne_depend_pas_des_volumes_du_jour() {
        let a = daily(150, 10.0, 1.0);
        let mut z = daily(150, 10.0, 0.5);
        let day = 120;
        let now = T0 + day * DAY_MS;
        let before = RankTable::build(&[&a, &z]);
        assert_eq!(before.order_at(now), &[0, 1]);
        // Un volume énorme le jour `day` (et après) ne change pas le rang de ce jour.
        z[usize::try_from(day).unwrap()].volume = 1e12;
        let after = RankTable::build(&[&a, &z]);
        assert_eq!(after.order_at(now), &[0, 1]);
        assert_eq!(after.score_at(now, 1), before.score_at(now, 1));
        // Le lendemain, en revanche, il compte.
        assert_eq!(after.order_at(now + DAY_MS), &[1, 0]);
    }

    #[test]
    fn jours_partiels_et_bornes() {
        let a = daily(10, 1.0, 1.0);
        let table = RankTable::build(&[&a]);
        assert_eq!(table.partial_days(T0, T0 + 9 * DAY_MS), 10);
        assert_eq!(table.partial_days(T0 + 100 * DAY_MS, T0 + 99 * DAY_MS), 0);
        assert_eq!(table.legs(), 1);
        // Hors bornes : la première ou la dernière ligne.
        assert_eq!(table.order_at(T0 - 5 * DAY_MS), &[0]);
        assert_eq!(table.order_at(T0 + 1000 * DAY_MS), &[0]);
        assert!(table.days() >= 11);
    }

    #[test]
    fn aucune_bougie() {
        let table = RankTable::build(&[&[], &[]]);
        assert_eq!(table.order_at(0), &[0, 1]);
    }
}
