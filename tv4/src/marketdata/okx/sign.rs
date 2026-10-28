//! Signature des requêtes privées OKX v5 (docs/specs/s9.md §8.3).
//!
//! HMAC-SHA256 (RFC 2104, vecteurs RFC 4231) et Base64 standard (RFC 4648 §4) sont écrits ici sur
//! `sha2`, sans crate `hmac` ni `base64` : une soixantaine de lignes, testées sur les vecteurs
//! officiels (`tests/okx_sign.rs`). L'horodatage ISO 8601 à la milliseconde est calculé en
//! arithmétique entière, sans `chrono` (même choix que le store, décision D9 de S4).
//!
//! # Préhash
//!
//! ```text
//! OK-ACCESS-SIGN = Base64( HMAC-SHA256( secret, timestamp + METHODE + requestPath(+ "?" + query) + corps ) )
//! ```
//!
//! `timestamp` est au format `2020-12-08T09:08:57.715Z` (UTC, trois décimales). Le chemin signé est
//! exactement celui qui est envoyé, chaîne de requête comprise. Le corps d'un GET est vide.
//!
//! Aucune fonction de ce module ne journalise ni n'écrit quoi que ce soit : elles sont pures.

use sha2::{Digest, Sha256};

use crate::model::Ts;

/// Taille d'un bloc de SHA-256 en octets (RFC 2104, `B`).
const BLOCK: usize = 64;

/// Octet de remplissage intérieur (RFC 2104 `ipad`).
const IPAD: u8 = 0x36;

/// Octet de remplissage extérieur (RFC 2104 `opad`).
const OPAD: u8 = 0x5c;

/// Alphabet Base64 standard (RFC 4648 §4).
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Millisecondes par jour.
const MS_PER_DAY: i64 = 86_400_000;

/// HMAC-SHA256 de `msg` sous la clé `key` (toute longueur : une clé de plus de 64 octets est d'abord
/// hachée, une clé plus courte est complétée de zéros, RFC 2104 §2).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest: [u8; 32] = Sha256::digest(key).into();
        block[..32].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0u8; BLOCK];
    let mut outer_pad = [0u8; BLOCK];
    for (index, byte) in block.iter().enumerate() {
        inner_pad[index] = byte ^ IPAD;
        outer_pad[index] = byte ^ OPAD;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(msg);
    let inner_digest: [u8; 32] = inner.finalize().into();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

/// Base64 standard avec remplissage `=` (RFC 4648 §4).
pub fn base64_standard(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map_or(0, |&b| u32::from(b));
        let b2 = chunk.get(2).map_or(0, |&b| u32::from(b));
        let group = (b0 << 16) | (b1 << 8) | b2;
        let sextet = |shift: u32| char::from(ALPHABET[((group >> shift) & 0x3f) as usize]);
        text.push(sextet(18));
        text.push(sextet(12));
        text.push(if chunk.len() > 1 { sextet(6) } else { '=' });
        text.push(if chunk.len() > 2 { sextet(0) } else { '=' });
    }
    text
}

/// Horodatage ISO 8601 UTC à la milliseconde (`2020-12-08T09:08:57.715Z`), format de
/// `OK-ACCESS-TIMESTAMP`. Les instants antérieurs à 1970 sont acceptés (jours négatifs).
pub fn iso_ms(ts_ms: Ts) -> String {
    let days = ts_ms.div_euclid(MS_PER_DAY);
    let in_day = ts_ms.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hours = in_day / 3_600_000;
    let minutes = in_day % 3_600_000 / 60_000;
    let seconds = in_day % 60_000 / 1_000;
    let millis = in_day % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

/// Date civile (année, mois 1-12, jour 1-31) du jour `z` compté depuis 1970-01-01 (algorithme de
/// Howard Hinnant, calendrier grégorien proleptique).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Valeur de `OK-ACCESS-SIGN` : Base64 de HMAC-SHA256 du préhash
/// `timestamp + method + path_and_query + body`.
///
/// - `timestamp` : le texte de [`iso_ms`], exactement celui de l'en-tête `OK-ACCESS-TIMESTAMP` ;
/// - `method` : en majuscules (`GET`) ;
/// - `path_and_query` : le chemin avec sa chaîne de requête, tel qu'envoyé
///   (`/api/v5/account/instruments?instType=SPOT`) ;
/// - `body` : vide pour un GET.
pub fn sign(secret: &str, timestamp: &str, method: &str, path_and_query: &str, body: &str) -> String {
    let mut prehash =
        String::with_capacity(timestamp.len() + method.len() + path_and_query.len() + body.len());
    prehash.push_str(timestamp);
    prehash.push_str(method);
    prehash.push_str(path_and_query);
    prehash.push_str(body);
    base64_standard(&hmac_sha256(secret.as_bytes(), prehash.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn hmac_rfc4231_cas_1() {
        let tag = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex(&tag),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn iso_ms_exemple_de_la_doc_okx() {
        assert_eq!(iso_ms(1_607_418_537_715), "2020-12-08T09:08:57.715Z");
    }

    #[test]
    fn iso_ms_bornes_du_calendrier() {
        assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_ms(-1), "1969-12-31T23:59:59.999Z");
        // 2024-02-29T12:00:00.001Z (jour bissextile).
        assert_eq!(iso_ms(1_709_208_000_001), "2024-02-29T12:00:00.001Z");
        // 2100 n'est pas bissextile : 2100-03-01T00:00:00.000Z.
        assert_eq!(iso_ms(4_107_542_400_000), "2100-03-01T00:00:00.000Z");
        // 2000 l'est : 2000-02-29T23:59:59.999Z.
        assert_eq!(iso_ms(951_868_799_999), "2000-02-29T23:59:59.999Z");
    }

    #[test]
    fn base64_remplissage() {
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foo"), "Zm9v");
    }
}
