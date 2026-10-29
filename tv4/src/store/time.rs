//! Dates du store en arithmétique entière, sans `chrono` (docs/03 §5, décision D9 de S4).
//!
//! Le store n'a besoin que de trois choses : les bornes d'un mois ou d'une année, le rendu
//! `YYYY-MM-DDTHH:MM:SSZ` des horodatages de manifeste, et la lecture stricte de ce même format sur
//! la ligne de commande. Ces opérations tiennent en quelques lignes d'arithmétique sur les jours
//! (algorithme de Howard Hinnant, calendrier grégorien proleptique) : pas de dépendance de dates.
//!
//! Le format est volontairement étroit : exactement 20 octets, `T` et `Z` majuscules, pas de fraction,
//! pas de décalage `+00:00`. Un seul rendu par instant, donc un seul texte à comparer ou à hacher.

use std::str::FromStr;

use crate::model::Ts;

use super::StoreError;

/// Millisecondes dans un jour.
pub const MS_PER_DAY: i64 = 86_400_000;

/// Borne haute exclusive des horodatages acceptés par le store : 10000-01-01T00:00:00Z (au-delà,
/// l'année n'a plus quatre chiffres). Les horodatages négatifs (avant 1970) sont refusés aussi.
pub const MAX_TS_MS: i64 = 253_402_300_800_000;

/// Jours depuis 1970-01-01 du jour civil (y, m, d), calendrier grégorien proleptique.
pub const fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let mp = if m > 2 { m - 3 } else { m + 9 }; // mars = 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Inverse de [`days_from_civil`] : (année, mois 1-12, jour 1-31) du jour `z` compté depuis
/// 1970-01-01.
pub const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Année bissextile du calendrier grégorien.
const fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Nombre de jours du mois `m` (1 à 12) de l'année `y`.
pub const fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
    }
}

/// Rend `ms` en `YYYY-MM-DDTHH:MM:SSZ`, millisecondes tronquées vers le bas.
///
/// Erreur [`StoreError::TimestampOutOfRange`] hors de `[0, MAX_TS_MS)`.
pub fn format_rfc3339_utc(ms: Ts) -> Result<String, StoreError> {
    if !(0..MAX_TS_MS).contains(&ms) {
        return Err(StoreError::TimestampOutOfRange { ts_ms: ms });
    }
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Ok(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    ))
}

/// Lit `n` chiffres ASCII (et rien d'autre) ; `None` sinon.
fn digits(bytes: &[u8]) -> Option<i64> {
    let mut value = 0_i64;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(b - b'0');
    }
    Some(value)
}

/// Lecture stricte : exactement 20 octets `YYYY-MM-DDTHH:MM:SSZ`, chiffres ASCII, `T` et `Z`
/// majuscules, mois 1-12, jour valide pour le mois (années bissextiles grégoriennes), heure < 24,
/// minute < 60, seconde < 60 (pas de seconde intercalaire). Pas de fraction, pas de décalage
/// `+00:00`, pas d'espace.
pub fn parse_rfc3339_utc(text: &str) -> Result<Ts, StoreError> {
    let invalid = || StoreError::InvalidDate {
        text: text.to_string(),
    };
    let b = text.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return Err(invalid());
    }
    let field = |range: std::ops::Range<usize>| digits(&b[range]).ok_or_else(invalid);
    let y = field(0..4)?;
    let mo = field(5..7)?;
    let d = field(8..10)?;
    let h = field(11..13)?;
    let mi = field(14..16)?;
    let s = field(17..19)?;
    // Les champs viennent de deux chiffres au plus : la conversion en u32 ne peut pas échouer.
    let mo = u32::try_from(mo).map_err(|_| invalid())?;
    let d = u32::try_from(d).map_err(|_| invalid())?;
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return Err(invalid());
    }
    Ok((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s) * 1000)
}

/// Fin de couverture exclusive lue dans le manifeste d'un dump : `AAAA-MM-JJ` est inclusive (le
/// lendemain à 00:00 UTC), `AAAA-MM-JJTHH:MM:SSZ` est prise telle quelle ; toute autre forme est
/// ignorée (`None`).
///
/// Copie de la fonction de `cli/ingest.rs` (docs/specs/s11.md §1 et §14, lot B) : `marketdata` ne
/// peut dépendre que de `model`, et `tv4 universe` comme `tv4 ingest kraken` ont besoin de la même
/// règle. La CLI de l'ingestion l'importera d'ici au lot F ; comportement inchangé.
pub fn parse_coverage_end(text: &str) -> Option<Ts> {
    if text.len() == 10 {
        let debut_du_jour = parse_rfc3339_utc(&format!("{text}T00:00:00Z")).ok()?;
        Some(debut_du_jour + MS_PER_DAY)
    } else {
        parse_rfc3339_utc(text).ok()
    }
}

/// Horodatage UTC lu sur la ligne de commande (`--from`, `--to`) : `FromStr` = [`parse_rfc3339_utc`],
/// ce qui donne à clap une erreur d'usage (code 2) sur un mauvais format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcMs(pub Ts);

impl FromStr for UtcMs {
    type Err = StoreError;

    fn from_str(text: &str) -> Result<UtcMs, StoreError> {
        parse_rfc3339_utc(text).map(UtcMs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jours_civils_valeurs_connues() {
        let cas = [
            (1970, 1, 1, 0),
            (2000, 3, 1, 11_017),
            (2020, 1, 1, 18_262),
            (2024, 1, 1, 19_723),
            (2024, 2, 29, 19_782),
            (10_000, 1, 1, 2_932_897),
        ];
        for (y, m, d, jours) in cas {
            assert_eq!(days_from_civil(y, m, d), jours, "{y}-{m}-{d}");
            assert_eq!(civil_from_days(jours), (y, m, d), "inverse de {jours}");
        }
    }

    #[test]
    fn aller_retour_jour_par_jour_1970_2199() {
        let debut = days_from_civil(1970, 1, 1);
        let fin = days_from_civil(2200, 1, 1);
        let mut precedent = None;
        for z in debut..fin {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z, "jour {z}");
            assert!((1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m));
            // Continuité : un jour de plus, soit le jour suivant du même mois, soit le 1er du suivant.
            if let Some((py, pm, pd)) = precedent {
                let suivant_meme_mois = (y, m, d) == (py, pm, pd + 1);
                let premier_du_mois = d == 1 && (y, m) != (py, pm) && pd == days_in_month(py, pm);
                assert!(suivant_meme_mois || premier_du_mois, "saut au jour {z}");
            }
            precedent = Some((y, m, d));
        }
    }

    #[test]
    fn rendu_rfc3339() {
        assert_eq!(format_rfc3339_utc(0).unwrap(), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_rfc3339_utc(1_704_067_200_000).unwrap(),
            "2024-01-01T00:00:00Z"
        );
        assert_eq!(
            format_rfc3339_utc(1_709_251_199_000).unwrap(),
            "2024-02-29T23:59:59Z"
        );
        // Troncature des millisecondes.
        assert_eq!(
            format_rfc3339_utc(1_791_936_000_123).unwrap(),
            "2026-10-14T00:00:00Z"
        );
        assert!(matches!(
            format_rfc3339_utc(-1),
            Err(StoreError::TimestampOutOfRange { ts_ms: -1 })
        ));
        assert!(matches!(
            format_rfc3339_utc(MAX_TS_MS),
            Err(StoreError::TimestampOutOfRange { .. })
        ));
    }

    #[test]
    fn lecture_rfc3339_stricte() {
        assert_eq!(
            parse_rfc3339_utc("2024-01-01T00:00:00Z").unwrap(),
            1_704_067_200_000
        );
        assert_eq!(
            parse_rfc3339_utc("2020-01-09T00:00:00Z").unwrap(),
            1_578_528_000_000
        );
        let refuses = [
            "2024-01-01T00:00:00+00:00",
            "2024-01-01T00:00:00.000Z",
            "2024-01-01 00:00:00Z",
            "2024-02-30T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2024-01-01T24:00:00Z",
            "2024-01-01T00:00:60Z",
            "2024-1-01T00:00:00Z",
            "2024-01-01t00:00:00z",
            "+024-01-01T00:00:00Z",
            "",
        ];
        for texte in refuses {
            assert!(
                matches!(
                    parse_rfc3339_utc(texte),
                    Err(StoreError::InvalidDate { .. })
                ),
                "« {texte} » aurait dû être refusé"
            );
        }
    }

    #[test]
    fn rendu_et_lecture_sont_inverses() {
        for ms in [0, 86_400_000, 1_704_067_200_000, 1_791_936_000_000] {
            assert_eq!(
                parse_rfc3339_utc(&format_rfc3339_utc(ms).unwrap()).unwrap(),
                ms
            );
        }
    }

    #[test]
    fn couverture_en_date_inclusive_ou_instant() {
        // 2020-01-07 est inclusive : la fin exclusive est le lendemain 00:00 UTC.
        assert_eq!(parse_coverage_end("2020-01-07"), Some(1_578_441_600_000));
        assert_eq!(
            parse_coverage_end("2020-01-08T00:00:00Z"),
            Some(1_578_441_600_000)
        );
        // 2026-09-30 inclusive : fin au 2026-10-01 00:00 UTC.
        assert_eq!(
            parse_coverage_end("2026-09-30"),
            parse_rfc3339_utc("2026-10-01T00:00:00Z").ok()
        );
        for illisible in [
            "",
            "demain",
            "2020-13-01",
            "2020/01/07",
            "2020-01-07T00:00:00",
        ] {
            assert_eq!(parse_coverage_end(illisible), None, "« {illisible} »");
        }
    }

    #[test]
    fn utc_ms_depuis_texte() {
        assert_eq!(
            "2024-01-01T00:00:00Z".parse::<UtcMs>().unwrap(),
            UtcMs(1_704_067_200_000)
        );
        assert!("demain".parse::<UtcMs>().is_err());
    }
}
