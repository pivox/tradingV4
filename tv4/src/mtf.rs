//! Multi-timeframe (docs/03-architecture.md §2 ligne `mtf`, §7.1 point 2, §7.5) : agrégation des
//! bougies 1m vers les TF supérieurs (frontières UTC), index `closed_count[tf][i]` et masque
//! `just_closed[i]`. Ne dépend que de `model` (règle vérifiée par `tests/architecture.rs`).
//!
//! Un seau TF sans aucune 1m ne produit pas de bougie : un trou reste un trou. `closed_count` est
//! calculé par comparaison d'horodatages, jamais par arithmétique d'indices (fausse dès qu'il manque
//! une minute). Une bougie TF dont la fin n'est pas atteinte par les données n'est jamais comptée,
//! donc jamais exposée. Les bougies synthétiques d'un dataset `ffill` participent comme les autres.

#![forbid(unsafe_code)]

use std::convert::Infallible;

use thiserror::Error;

use crate::model::{Candle, Timeframe, Ts};

/// Erreurs de construction d'un [`Mtf`].
#[derive(Debug, Error)]
pub enum MtfError {
    #[error("aucune bougie 1m")]
    Empty,
    #[error("aucun timeframe demandé")]
    NoTimeframe,
    #[error("timeframe {tf} demandé deux fois")]
    DuplicateTimeframe { tf: Timeframe },
    #[error("bougie {index} : open_time {open_time_ms} non aligné sur la minute")]
    NotOneMinute { index: usize, open_time_ms: Ts },
    #[error(
        "bougie {index} : open_time {open_time_ms} non strictement croissant (précédent : {previous_ms})"
    )]
    NotIncreasing {
        index: usize,
        previous_ms: Ts,
        open_time_ms: Ts,
    },
}

/// Règle de clôture : vrai si la bougie `tf` ouverte à `open_tf` est clôturée à la fin de la 1m
/// ouverte à `open_1m`. Doit être croissante en `open_1m` (balayage à deux pointeurs).
pub type ClosedRule = fn(open_tf: Ts, tf: Timeframe, open_1m: Ts) -> bool;

/// Règle du moteur : `open_tf + ms(tf) <= open_1m + 60 000` (la H1 10:00 est visible à la 1m 10:59).
pub fn is_closed(open_tf: Ts, tf: Timeframe, open_1m: Ts) -> bool {
    open_tf.saturating_add(tf.ms()) <= open_1m.saturating_add(Timeframe::M1.ms())
}

/// Agrège `m1` (1m alignées, strictement croissantes : précondition non vérifiée ici, voir
/// [`Mtf::build`]) vers `tf`. Seules les bougies présentes dans un seau y entrent : open de la
/// première, high max, low min, close de la dernière, volume somme dans l'ordre chronologique
/// (boucle explicite, pas de `.sum()`, docs/03 §7.4), `trades` somme si toutes les minutes du seau en
/// ont une, absent sinon (et absent en cas de dépassement de `u32`). `tf == M1` : copie. O(n), une
/// allocation.
pub fn aggregate(m1: &[Candle], tf: Timeframe) -> Vec<Candle> {
    match aggregate_with(m1, tf, |_, _| Ok::<(), Infallible>(())) {
        Ok(out) => out,
        Err(never) => match never {},
    }
}

/// Comme [`aggregate`], en appelant `visit(index, bougie)` sur chaque 1m au passage : [`Mtf::build`]
/// y contrôle les horodatages pour ne lire la série 1m qu'une fois (la lecture mémoire domine, docs/03
/// §8). La première erreur de `visit` interrompt l'agrégation.
fn aggregate_with<E>(
    m1: &[Candle],
    tf: Timeframe,
    mut visit: impl FnMut(usize, &Candle) -> Result<(), E>,
) -> Result<Vec<Candle>, E> {
    if tf == Timeframe::M1 {
        for (index, c) in m1.iter().enumerate() {
            visit(index, c)?;
        }
        return Ok(m1.to_vec());
    }
    // `max(1)` : `ms(tf) / 60 000` vaut au moins 1 pour tout TF, la garde évite seulement une
    // division par zéro théorique.
    let per_bucket = usize::try_from(tf.ms() / Timeframe::M1.ms()).map_or(1, |v| v.max(1));
    let mut out: Vec<Candle> = Vec::with_capacity(m1.len() / per_bucket + 2);
    // Bornes `[start, end)` du seau courant : la division euclidienne de `open_time_of` n'est payée
    // qu'à l'ouverture d'un seau, pas à chaque minute (mesuré : 20 ms contre 11 ms sur 2 ans de 1m).
    let (mut start, mut end) = (0, 0);
    for (index, c) in m1.iter().enumerate() {
        visit(index, c)?;
        match out.last_mut() {
            Some(current) if (start..end).contains(&c.open_time_ms) => {
                current.high = current.high.max(c.high);
                current.low = current.low.min(c.low);
                current.close = c.close;
                current.volume += c.volume;
                current.trades = match (current.trades, c.trades) {
                    (Some(a), Some(b)) => a.checked_add(b),
                    _ => None,
                };
            }
            _ => {
                start = tf.open_time_of(c.open_time_ms);
                end = start.saturating_add(tf.ms());
                out.push(Candle {
                    open_time_ms: start,
                    open: c.open,
                    high: c.high,
                    low: c.low,
                    close: c.close,
                    volume: c.volume,
                    trades: c.trades,
                });
            }
        }
    }
    Ok(out)
}

/// Contrôle d'une 1m (alignée sur la minute, strictement après la précédente, `previous` étant
/// l'`open_time` de la précédente) et mise à jour de `previous`.
fn check_candle(previous: &mut Option<Ts>, index: usize, candle: &Candle) -> Result<(), MtfError> {
    if !Timeframe::M1.is_aligned(candle.open_time_ms) {
        return Err(MtfError::NotOneMinute {
            index,
            open_time_ms: candle.open_time_ms,
        });
    }
    if let Some(previous_ms) = *previous
        && candle.open_time_ms <= previous_ms
    {
        return Err(MtfError::NotIncreasing {
            index,
            previous_ms,
            open_time_ms: candle.open_time_ms,
        });
    }
    *previous = Some(candle.open_time_ms);
    Ok(())
}

/// Séries agrégées et index d'une série 1m, pour un ensemble de TF. Tout est alloué ici, jamais dans
/// la boucle du moteur.
pub struct Mtf<'a> {
    m1: &'a [Candle],
    /// Triées par TF croissant.
    series: Vec<TfSeries>,
    /// Longueur `m1.len()`.
    just_closed: Vec<u8>,
}

struct TfSeries {
    tf: Timeframe,
    /// Vide pour M1 : la série est `m1` elle-même, sans copie.
    candles: Vec<Candle>,
    /// Longueur `m1.len()`.
    closed_count: Vec<usize>,
}

impl<'a> Mtf<'a> {
    /// Contrôle `m1` (non vide, alignée sur la minute, strictement croissante) et `tfs` (non vide,
    /// sans doublon), puis agrège et indexe avec [`is_closed`].
    pub fn build(m1: &'a [Candle], tfs: &[Timeframe]) -> Result<Mtf<'a>, MtfError> {
        Mtf::build_with_rule(m1, tfs, is_closed)
    }

    /// Point d'injection des tests (mutant, docs/03 §7.5) : même chose avec une autre règle de
    /// clôture. Réservé aux tests ; le moteur passe toujours par [`Mtf::build`].
    #[doc(hidden)]
    pub fn build_with_rule<R: Fn(Ts, Timeframe, Ts) -> bool>(
        m1: &'a [Candle],
        tfs: &[Timeframe],
        rule: R,
    ) -> Result<Mtf<'a>, MtfError> {
        if m1.is_empty() {
            return Err(MtfError::Empty);
        }
        if tfs.is_empty() {
            return Err(MtfError::NoTimeframe);
        }
        for (i, tf) in tfs.iter().enumerate() {
            if tfs.get(..i).is_some_and(|before| before.contains(tf)) {
                return Err(MtfError::DuplicateTimeframe { tf: *tf });
            }
        }
        let mut ordered: Vec<Timeframe> = tfs.to_vec();
        ordered.sort();
        // Un seul passage sur les 1m pour contrôler les horodatages et agréger vers le premier TF
        // supérieur ; sans TF supérieur, un passage de contrôle seul.
        let mut previous: Option<Ts> = None;
        let fused_tf = ordered.iter().copied().find(|tf| *tf != Timeframe::M1);
        let mut fused: Option<Vec<Candle>> = match fused_tf {
            Some(tf) => Some(aggregate_with(m1, tf, |index, candle| {
                check_candle(&mut previous, index, candle)
            })?),
            None => {
                for (index, candle) in m1.iter().enumerate() {
                    check_candle(&mut previous, index, candle)?;
                }
                None
            }
        };

        // `just_closed[i]` : bits des TF dont `closed_count` augmente à `i`, remplis pendant le
        // balayage de chaque TF.
        let mut just_closed = vec![0u8; m1.len()];
        let mut series = Vec::with_capacity(ordered.len());
        for tf in ordered {
            let candles = if tf == Timeframe::M1 {
                Vec::new()
            } else if Some(tf) == fused_tf {
                fused.take().unwrap_or_default()
            } else {
                aggregate(m1, tf)
            };
            let source: &[Candle] = if tf == Timeframe::M1 { m1 } else { &candles };
            let closed_count = closed_counts_of(m1, source, tf, &rule, &mut just_closed);
            series.push(TfSeries {
                tf,
                candles,
                closed_count,
            });
        }
        Ok(Mtf {
            m1,
            series,
            just_closed,
        })
    }

    /// Les bougies 1m d'origine.
    pub fn m1(&self) -> &'a [Candle] {
        self.m1
    }

    /// Nombre de bougies 1m.
    pub fn len(&self) -> usize {
        self.m1.len()
    }

    pub fn is_empty(&self) -> bool {
        self.m1.is_empty()
    }

    /// Masque des TF présents (`Timeframe::bit`).
    pub fn mask(&self) -> u8 {
        self.series.iter().fold(0, |mask, s| mask | s.tf.bit())
    }

    fn series_of(&self, tf: Timeframe) -> Option<&TfSeries> {
        self.series.iter().find(|s| s.tf == tf)
    }

    /// Bougies du TF (`m1` pour M1) ; `None` si le TF n'a pas été demandé. La dernière bougie d'un TF
    /// peut être incomplète : seule `closed_counts` dit combien sont visibles à chaque 1m.
    pub fn candles(&self, tf: Timeframe) -> Option<&[Candle]> {
        self.series_of(tf).map(|s| {
            if tf == Timeframe::M1 {
                self.m1
            } else {
                s.candles.as_slice()
            }
        })
    }

    /// `closed_count[tf][..]` complet (une valeur par 1m) ; `None` si le TF est absent.
    pub fn closed_counts(&self, tf: Timeframe) -> Option<&[usize]> {
        self.series_of(tf).map(|s| s.closed_count.as_slice())
    }

    /// Bits des TF dont `closed_count` augmente à `i` ; 0 hors bornes.
    pub fn just_closed(&self, i: usize) -> u8 {
        self.just_closed.get(i).copied().unwrap_or(0)
    }
}

/// `closed_count` d'un TF par balayage à deux pointeurs : `j` avance tant que la bougie TF `j` est
/// clôturée à la fin de la 1m `i`. O(n + m). Pour M1, la même règle s'applique à `m1` lui-même (pas
/// de raccourci par indice : avec `is_closed`, le résultat est `i + 1`). Pose au passage le bit de
/// `tf` dans `just_closed[i]` quand le compte augmente à `i`. La règle est générique pour que
/// [`Mtf::build`] la voie en clair et l'inline.
fn closed_counts_of<R: Fn(Ts, Timeframe, Ts) -> bool>(
    m1: &[Candle],
    source: &[Candle],
    tf: Timeframe,
    rule: &R,
    just_closed: &mut [u8],
) -> Vec<usize> {
    let bit = tf.bit();
    let mut counts = Vec::with_capacity(m1.len());
    let mut j = 0usize;
    let mut before = 0usize;
    for (c, flag) in m1.iter().zip(just_closed.iter_mut()) {
        while source
            .get(j)
            .is_some_and(|b| rule(b.open_time_ms, tf, c.open_time_ms))
        {
            j += 1;
        }
        if j > before {
            *flag |= bit;
        }
        before = j;
        counts.push(j);
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-01-01T00:00:00Z.
    const T0: Ts = 1_704_067_200_000;
    const MIN: Ts = 60_000;

    /// Bougie 1m **inventée** (prix ronds, aucun marché).
    fn c1(
        minute: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
        trades: Option<u32>,
    ) -> Candle {
        Candle {
            open_time_ms: T0 + minute * MIN,
            open,
            high,
            low,
            close,
            volume,
            trades,
        }
    }

    /// `minutes` 1m consécutives au prix `10 + minute`, valeurs inventées.
    fn flat(minutes: impl IntoIterator<Item = i64>) -> Vec<Candle> {
        minutes
            .into_iter()
            .map(|m| {
                let p = 10.0 + m as f64;
                c1(m, p, p + 1.0, p - 1.0, p + 0.5, 1.0, Some(2))
            })
            .collect()
    }

    #[test]
    fn agregation_a_la_main_avec_seau_troue() {
        // Seau 5m 00:00 : minutes 0, 1, 3 présentes (2 et 4 absentes).
        let m1 = vec![
            c1(0, 10.0, 12.0, 9.0, 11.0, 1.5, Some(3)),
            c1(1, 11.0, 15.0, 10.0, 14.0, 2.0, Some(4)),
            c1(3, 14.0, 14.5, 8.0, 9.0, 0.5, Some(5)),
        ];
        let out = aggregate(&m1, Timeframe::M5);
        assert_eq!(
            out,
            vec![Candle {
                open_time_ms: T0,
                open: 10.0,
                high: 15.0,
                low: 8.0,
                close: 9.0,
                volume: 4.0,
                trades: Some(12),
            }]
        );
    }

    #[test]
    fn seau_vide_pas_de_bougie() {
        // Minutes 0 et 10 : le seau 5m de la minute 5 est vide, il ne produit rien.
        let m1 = flat([0, 10]);
        let out = aggregate(&m1, Timeframe::M5);
        let ouvertures: Vec<i64> = out.iter().map(|c| (c.open_time_ms - T0) / MIN).collect();
        assert_eq!(ouvertures, [0, 10]);
    }

    #[test]
    fn trades_absent_si_une_minute_sans_trades() {
        let m1 = vec![
            c1(0, 10.0, 11.0, 9.0, 10.0, 1.0, Some(3)),
            c1(1, 10.0, 11.0, 9.0, 10.0, 1.0, None),
            c1(5, 10.0, 11.0, 9.0, 10.0, 1.0, Some(1)),
        ];
        let out = aggregate(&m1, Timeframe::M5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].trades, None);
        assert_eq!(out[1].trades, Some(1));
        // Dépassement de u32 : absent, jamais de panique.
        let grand = vec![
            c1(0, 10.0, 11.0, 9.0, 10.0, 1.0, Some(u32::MAX)),
            c1(1, 10.0, 11.0, 9.0, 10.0, 1.0, Some(1)),
        ];
        assert_eq!(aggregate(&grand, Timeframe::M5)[0].trades, None);
    }

    #[test]
    fn m1_identite() {
        let m1 = flat([0, 1, 5, 6]);
        assert_eq!(aggregate(&m1, Timeframe::M1), m1);
        let mtf = Mtf::build(&m1, &[Timeframe::M1]).unwrap();
        assert_eq!(mtf.candles(Timeframe::M1).unwrap(), &m1[..]);
        // Règle appliquée à M1 lui-même : i + 1 même avec des trous.
        assert_eq!(mtf.closed_counts(Timeframe::M1).unwrap(), [1, 2, 3, 4]);
        assert!(mtf.candles(Timeframe::M5).is_none());
        assert!(mtf.closed_counts(Timeframe::M5).is_none());
    }

    #[test]
    fn closed_count_par_horodatages_avec_trous() {
        // La 1m hh:14 manque : la 15m 00:00 se clôt sur la première 1m présente ensuite (15:00 ici, le
        // test la place à la minute 15), puisque sa fin (minute 15) <= fin de la 1m 15 (minute 16).
        let m1 = flat((0..14).chain(15..20));
        let mtf = Mtf::build(&m1, &[Timeframe::M15]).unwrap();
        let cc = mtf.closed_counts(Timeframe::M15).unwrap();
        // Index 13 = minute 13 ; index 14 = minute 15.
        assert_eq!(cc[13], 0, "pas encore clôturée à la fin de la minute 13");
        assert_eq!(
            cc[14], 1,
            "clôturée à la fin de la minute 15 (fin de bougie 00:15 <= 00:16)"
        );
        assert_eq!(mtf.just_closed(14), Timeframe::M15.bit());
        assert_eq!(mtf.just_closed(13), 0);
        // Les minutes 15 à 19 forment une 15m incomplète, jamais comptée.
        assert_eq!(cc[18], 1);
        assert_eq!(mtf.candles(Timeframe::M15).unwrap().len(), 2);
    }

    #[test]
    fn closed_count_n_est_pas_une_division_d_indices() {
        // Un trou de 5 minutes dans la première 15m : `(i + 1) / 15` se trompe d'une bougie 1m.
        let m1 = flat((0..5).chain(10..40));
        let mtf = Mtf::build(&m1, &[Timeframe::M15]).unwrap();
        let cc = mtf.closed_counts(Timeframe::M15).unwrap();
        // La 15m 00:00 se clôt à la fin de la 1m 14, qui est l'index 9 (5 + 4).
        let premier = cc.iter().position(|&n| n == 1).unwrap();
        assert_eq!(premier, 9);
        assert_ne!(
            (premier + 1) / 15,
            1,
            "la division d'indices donnerait 0 ici"
        );
        // La 15m 00:15 se clôt à la fin de la 1m 29, index 24.
        assert_eq!(cc.iter().position(|&n| n == 2), Some(24));
        // La 15m 00:30 n'est complète qu'à la minute 44 : absente des données, jamais comptée.
        assert_eq!(cc.last(), Some(&2));
    }

    #[test]
    fn bougie_incomplete_en_fin_jamais_comptee() {
        // Données jusqu'à hh:37 (minute 37 comprise) : la H1 00:00 n'est jamais comptée.
        let m1 = flat(0..38);
        let mtf = Mtf::build(&m1, &[Timeframe::H1, Timeframe::M15]).unwrap();
        assert_eq!(mtf.candles(Timeframe::H1).unwrap().len(), 1);
        assert_eq!(mtf.closed_counts(Timeframe::H1).unwrap().last(), Some(&0));
        assert!(
            mtf.closed_counts(Timeframe::H1)
                .unwrap()
                .iter()
                .all(|&n| n == 0)
        );
        // La 15m 00:30 est incomplète elle aussi (fin à 00:45) ; deux 15m sont clôturées.
        assert_eq!(mtf.closed_counts(Timeframe::M15).unwrap().last(), Some(&2));
        assert_eq!(mtf.candles(Timeframe::M15).unwrap().len(), 3);
    }

    #[test]
    fn just_closed_bits() {
        let m1 = flat(0..20);
        let mtf = Mtf::build(&m1, &[Timeframe::M1, Timeframe::M5, Timeframe::M15]).unwrap();
        assert_eq!(
            mtf.mask(),
            Timeframe::M1.bit() | Timeframe::M5.bit() | Timeframe::M15.bit()
        );
        let m1_bit = Timeframe::M1.bit();
        // Minute 3 : rien d'autre que la 1m ; minute 4 : la 5m 00:00 ; minute 14 : 5m et 15m.
        assert_eq!(mtf.just_closed(3), m1_bit);
        assert_eq!(mtf.just_closed(4), m1_bit | Timeframe::M5.bit());
        assert_eq!(mtf.just_closed(9), m1_bit | Timeframe::M5.bit());
        assert_eq!(
            mtf.just_closed(14),
            m1_bit | Timeframe::M5.bit() | Timeframe::M15.bit()
        );
        assert_eq!(mtf.just_closed(19), m1_bit | Timeframe::M5.bit());
        assert_eq!(mtf.just_closed(20), 0, "hors bornes");
        assert_eq!(mtf.len(), 20);
        assert!(!mtf.is_empty());
    }

    #[test]
    fn h1_visible_a_la_1m_hh59() {
        // Convention de frontière (docs/03 §7.5) : la H1 10:00 est visible à la 1m 10:59, pas avant.
        let m1 = flat(0..120);
        let mtf = Mtf::build(&m1, &[Timeframe::H1]).unwrap();
        let cc = mtf.closed_counts(Timeframe::H1).unwrap();
        assert_eq!(cc[58], 0);
        assert_eq!(cc[59], 1);
        assert_eq!(cc[118], 1);
        assert_eq!(cc[119], 2);
        assert_eq!(mtf.just_closed(59), Timeframe::H1.bit());
        // La règle seule.
        assert!(is_closed(T0, Timeframe::H1, T0 + 59 * MIN));
        assert!(!is_closed(T0, Timeframe::H1, T0 + 58 * MIN));
    }

    #[test]
    fn regle_injectee_par_build_with_rule() {
        // Règle fautive (docs/03 §7.5) : la bougie TF est « clôturée » dès que son open est atteint.
        fn fautive(open_tf: Ts, _tf: Timeframe, open_1m: Ts) -> bool {
            open_tf <= open_1m
        }
        let m1 = flat(0..120);
        let bon = Mtf::build(&m1, &[Timeframe::H1]).unwrap();
        let mutant = Mtf::build_with_rule(&m1, &[Timeframe::H1], fautive).unwrap();
        assert_eq!(bon.closed_counts(Timeframe::H1).unwrap()[0], 0);
        assert_eq!(mutant.closed_counts(Timeframe::H1).unwrap()[0], 1);
    }

    #[test]
    fn erreurs_d_entree() {
        let m1 = flat(0..10);
        assert!(matches!(
            Mtf::build(&[], &[Timeframe::M5]),
            Err(MtfError::Empty)
        ));
        assert!(matches!(Mtf::build(&m1, &[]), Err(MtfError::NoTimeframe)));
        assert!(matches!(
            Mtf::build(&m1, &[Timeframe::M5, Timeframe::M15, Timeframe::M5]),
            Err(MtfError::DuplicateTimeframe { tf: Timeframe::M5 })
        ));
        let mut desordre = flat(0..3);
        desordre.swap(1, 2);
        assert!(matches!(
            Mtf::build(&desordre, &[Timeframe::M5]),
            Err(MtfError::NotIncreasing { index: 2, .. })
        ));
        let mut doublon = flat(0..3);
        doublon[2].open_time_ms = doublon[1].open_time_ms;
        assert!(matches!(
            Mtf::build(&doublon, &[Timeframe::M5]),
            Err(MtfError::NotIncreasing { index: 2, .. })
        ));
        let mut decale = flat(0..3);
        decale[1].open_time_ms += 1_000;
        assert!(matches!(
            Mtf::build(&decale, &[Timeframe::M5]),
            Err(MtfError::NotOneMinute { index: 1, .. })
        ));
    }

    #[test]
    fn les_tf_sont_tries_et_le_masque_complet() {
        let m1 = flat(0..10);
        let mtf = Mtf::build(&m1, &[Timeframe::H1, Timeframe::M5]).unwrap();
        assert_eq!(mtf.mask(), Timeframe::M5.bit() | Timeframe::H1.bit());
        assert_eq!(mtf.m1().len(), 10);
    }
}
