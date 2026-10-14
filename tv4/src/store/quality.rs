//! Rapport de qualité trimestriel d'une série 1m (docs/04 §2.5 et §9 point 13 ; docs/03 §15.1 ligne 13).
//!
//! Pour chaque (timeframe, trimestre civil UTC), le rapport donne le nombre de bougies attendues depuis
//! la première bougie de la série, le nombre présentes, la fraction absente et un drapeau `excluded`
//! quand plus de 1 % manquent (seuil strict, docs/04 §2.5). Les timeframes supérieurs au 1m ne sont pas
//! lus dans des fichiers : ils sont déduits de la série 1m en regroupant les minutes présentes par
//! seau (un seau sans aucune minute est une bougie absente). Le calcul est en une passe par timeframe,
//! sans copie de la série, et n'utilise que le calendrier entier de `store::time`.

use serde::Serialize;

use crate::model::{Timeframe, Ts};

use super::StoreError;
use super::time::{MS_PER_DAY, civil_from_days, days_from_civil, format_rfc3339_utc};

/// Seuil de docs/04 §2.5 : un trimestre est exclu pour une (paire, TF) si plus de 1 % des bougies
/// manquent. Comparaison stricte : exactement 1 % ne l'exclut pas.
pub const QUALITY_MISSING_THRESHOLD: f64 = 0.01;

/// Durée d'une bougie 1m en millisecondes.
const M1_MS: Ts = 60_000;

/// Une ligne du rapport : un timeframe, un trimestre.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QualityRow {
    pub tf: Timeframe,
    /// `2024-Q1`
    pub quarter: String,
    /// Seaux complets attendus entre la première bougie de la série (ou le début du trimestre) et la fin
    /// de la plage observée (ou la fin du trimestre).
    pub expected: u64,
    pub present: u64,
    pub missing: u64,
    pub missing_fraction: f64,
    /// Plus de [`QUALITY_MISSING_THRESHOLD`] des bougies manquent.
    pub excluded: bool,
    /// Plus long trou de bougies absentes en millisecondes, attribué au trimestre de la bougie qui le
    /// termine ; 0 sans trou.
    pub longest_gap_ms: i64,
}

/// Rapport de qualité d'une série 1m.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QualityReport {
    /// Date de première bougie (RFC 3339 UTC).
    pub first_open_time: String,
    /// Fin exclusive de la plage observée : dernière 1m + 60 s (RFC 3339 UTC).
    pub end_time: String,
    pub threshold: f64,
    /// Triées par (tf croissant, trimestre).
    pub rows: Vec<QualityRow>,
}

/// Index de trimestre civil : `année × 4 + (mois - 1) / 3`.
fn quarter_index(ts: Ts) -> i64 {
    let (year, month, _) = civil_from_days(ts.div_euclid(MS_PER_DAY));
    year * 4 + i64::from((month - 1) / 3)
}

/// Premier instant du trimestre `index`.
fn quarter_start_ms(index: i64) -> Ts {
    let year = index.div_euclid(4);
    let first_month = u32::try_from(index.rem_euclid(4) * 3 + 1).unwrap_or(1);
    days_from_civil(year, first_month, 1) * MS_PER_DAY
}

fn quarter_label(index: i64) -> String {
    format!("{:04}-Q{}", index.div_euclid(4), index.rem_euclid(4) + 1)
}

/// Rapport de qualité trimestriel de `m1_open_times`, série 1m strictement croissante et alignée
/// (les `open_time_ms` d'une [`Columns`](super::Columns)). Erreur si vide, non alignée ou non
/// strictement croissante.
pub fn quality_report(
    m1_open_times: &[Ts],
    tfs: &[Timeframe],
) -> Result<QualityReport, StoreError> {
    let (Some(&first), Some(&last)) = (m1_open_times.first(), m1_open_times.last()) else {
        return Err(StoreError::EmptyInput);
    };
    for (index, &open_time_ms) in m1_open_times.iter().enumerate() {
        Timeframe::M1.check_alignment(open_time_ms)?;
        if index > 0 && open_time_ms <= m1_open_times[index - 1] {
            return Err(StoreError::NotStrictlyIncreasing {
                index,
                previous_ms: m1_open_times[index - 1],
                open_time_ms,
            });
        }
    }
    let end = last + M1_MS;
    let mut sorted: Vec<Timeframe> = tfs.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut rows = Vec::new();
    for tf in sorted {
        let ms = tf.ms();
        let first_bucket = tf.open_time_of(first);
        let q_first = quarter_index(first_bucket);
        let q_last = quarter_index(end - 1);
        let count = usize::try_from(q_last - q_first + 1).unwrap_or(0);
        let mut present = vec![0u64; count];
        let mut longest = vec![0i64; count];

        // Seaux présents : un par seau contenant au moins une minute, dédoublonnés à la volée (la série
        // est croissante). Un seau qui déborde la fin de la plage n'est ni attendu ni présent.
        let mut previous: Option<Ts> = None;
        for &t in m1_open_times {
            let bucket = tf.open_time_of(t);
            if previous == Some(bucket) {
                continue;
            }
            if bucket + ms > end {
                break;
            }
            let slot = usize::try_from(quarter_index(bucket) - q_first).unwrap_or(0);
            present[slot] += 1;
            if let Some(p) = previous {
                let gap = bucket - p - ms;
                if gap > 0 {
                    longest[slot] = longest[slot].max(gap);
                }
            }
            previous = Some(bucket);
        }

        for (slot, quarter) in (q_first..=q_last).enumerate() {
            let lo = quarter_start_ms(quarter).max(first_bucket);
            let hi = quarter_start_ms(quarter + 1).min(end);
            // Division entière : seuls les seaux complets sont attendus.
            let expected = if hi - lo >= ms {
                u64::try_from((hi - lo) / ms).unwrap_or(0)
            } else {
                0
            };
            if expected == 0 {
                continue;
            }
            let missing = expected.saturating_sub(present[slot]);
            let missing_fraction = missing as f64 / expected as f64;
            rows.push(QualityRow {
                tf,
                quarter: quarter_label(quarter),
                expected,
                present: present[slot],
                missing,
                missing_fraction,
                excluded: missing_fraction > QUALITY_MISSING_THRESHOLD,
                longest_gap_ms: longest[slot],
            });
        }
    }
    Ok(QualityReport {
        first_open_time: format_rfc3339_utc(first)?,
        end_time: format_rfc3339_utc(end)?,
        threshold: QUALITY_MISSING_THRESHOLD,
        rows,
    })
}

// Les séries ci-dessous sont synthétiques (instants et trous inventés), pas des données de marché.
// Le tableau de la fixture réelle est vérifié dans `tests/store.rs`.
#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-03-31T23:00:00Z.
    const SOIR: Ts = 1_711_926_000_000;

    fn minutes(depuis: Ts, n: i64, sauf: std::ops::Range<i64>) -> Vec<Ts> {
        (0..n)
            .filter(|i| !sauf.contains(i))
            .map(|i| depuis + i * M1_MS)
            .collect()
    }

    fn ligne(rapport: &QualityReport, tf: Timeframe, trimestre: &str) -> QualityRow {
        rapport
            .rows
            .iter()
            .find(|r| r.tf == tf && r.quarter == trimestre)
            .unwrap_or_else(|| panic!("ligne {tf} {trimestre} absente : {:?}", rapport.rows))
            .clone()
    }

    #[test]
    fn qualite_a_cheval_sur_deux_trimestres() {
        // De 23:00 le 31/03 à 00:59 le 01/04 (120 minutes), cinq minutes absentes à 00:10..00:14.
        let temps = minutes(SOIR, 120, 70..75);
        let rapport = quality_report(&temps, &[Timeframe::H1, Timeframe::M1]).unwrap();
        assert_eq!(rapport.first_open_time, "2024-03-31T23:00:00Z");
        assert_eq!(rapport.end_time, "2024-04-01T01:00:00Z");
        assert_eq!(rapport.threshold, 0.01);
        // Tri par (tf croissant, trimestre) quel que soit l'ordre demandé.
        let ordre: Vec<(Timeframe, &str)> = rapport
            .rows
            .iter()
            .map(|r| (r.tf, r.quarter.as_str()))
            .collect();
        assert_eq!(
            ordre,
            [
                (Timeframe::M1, "2024-Q1"),
                (Timeframe::M1, "2024-Q2"),
                (Timeframe::H1, "2024-Q1"),
                (Timeframe::H1, "2024-Q2"),
            ]
        );
        let t1 = ligne(&rapport, Timeframe::M1, "2024-Q1");
        assert_eq!((t1.expected, t1.present, t1.missing), (60, 60, 0));
        assert!(!t1.excluded);
        let t2 = ligne(&rapport, Timeframe::M1, "2024-Q2");
        assert_eq!((t2.expected, t2.present, t2.missing), (60, 55, 5));
        assert_eq!(t2.missing_fraction, 5.0 / 60.0);
        assert!(t2.excluded);
        // Le trou est attribué au trimestre de la bougie qui le termine.
        assert_eq!((t1.longest_gap_ms, t2.longest_gap_ms), (0, 300_000));
        // En 1h, les deux seaux (23:00 et 00:00) sont présents.
        for trimestre in ["2024-Q1", "2024-Q2"] {
            let h = ligne(&rapport, Timeframe::H1, trimestre);
            assert_eq!((h.expected, h.present, h.missing), (1, 1, 0), "{trimestre}");
        }
    }

    #[test]
    fn qualite_seuil_strict_un_pourcent() {
        // Une série dans un seul trimestre : 2024-05-01T00:00:00Z, 100 minutes dont une absente
        // (exactement 1 %, non exclu).
        let mai = 1_714_521_600_000;
        let rapport = quality_report(&minutes(mai, 100, 50..51), &[Timeframe::M1]).unwrap();
        let ligne_mai = ligne(&rapport, Timeframe::M1, "2024-Q2");
        assert_eq!(
            (ligne_mai.expected, ligne_mai.present, ligne_mai.missing),
            (100, 99, 1)
        );
        assert_eq!(ligne_mai.missing_fraction, 0.01);
        assert!(!ligne_mai.excluded);
        // Deux absentes : 2 %, exclu.
        let deux = minutes(mai, 100, 50..52);
        let rapport = quality_report(&deux, &[Timeframe::M1]).unwrap();
        assert!(ligne(&rapport, Timeframe::M1, "2024-Q2").excluded);
    }

    #[test]
    fn un_seau_qui_deborde_la_fin_n_est_pas_attendu() {
        // Quatre minutes à partir de minuit : aucun seau 5m complet.
        let mai = 1_714_521_600_000;
        let rapport =
            quality_report(&minutes(mai, 4, 0..0), &[Timeframe::M1, Timeframe::M5]).unwrap();
        assert_eq!(rapport.rows.len(), 1);
        assert_eq!(rapport.rows[0].tf, Timeframe::M1);
        // Sept minutes : un seau 5m complet, un second incomplet (non attendu, non présent).
        let rapport = quality_report(&minutes(mai, 7, 0..0), &[Timeframe::M5]).unwrap();
        let m5 = ligne(&rapport, Timeframe::M5, "2024-Q2");
        assert_eq!((m5.expected, m5.present, m5.missing), (1, 1, 0));
    }

    #[test]
    fn qualite_refuse_vide_et_desordre() {
        assert!(matches!(
            quality_report(&[], &[Timeframe::M1]),
            Err(StoreError::EmptyInput)
        ));
        assert!(matches!(
            quality_report(&[SOIR + M1_MS, SOIR], &[Timeframe::M1]),
            Err(StoreError::NotStrictlyIncreasing { index: 1, .. })
        ));
        assert!(matches!(
            quality_report(&[SOIR, SOIR], &[Timeframe::M1]),
            Err(StoreError::NotStrictlyIncreasing { .. })
        ));
        assert!(matches!(
            quality_report(&[SOIR + 30_000], &[Timeframe::M1]),
            Err(StoreError::Model(_))
        ));
    }

    #[test]
    fn libelles_de_trimestre() {
        assert_eq!(quarter_label(2024 * 4), "2024-Q1");
        assert_eq!(quarter_label(2024 * 4 + 3), "2024-Q4");
        assert_eq!(quarter_start_ms(2024 * 4 + 1), 1_711_929_600_000);
        assert_eq!(quarter_index(SOIR), 2024 * 4);
        assert_eq!(quarter_index(SOIR + 3_600_000), 2024 * 4 + 1);
    }
}
