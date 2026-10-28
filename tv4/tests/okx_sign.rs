//! Signature des requêtes privées OKX (docs/specs/s9.md §8.3 et §8.8) : HMAC-SHA256 sur les vecteurs
//! RFC 4231, Base64 sur les vecteurs RFC 4648 §10, horodatage ISO 8601 et signatures de référence
//! recalculées par `openssl dgst -sha256 -hmac … -binary | base64` puis épinglées.
//!
//! Aucun accès réseau. Les secrets sont des valeurs factices évidentes.

use tv4::marketdata::okx::sign::{base64_standard, hmac_sha256, iso_ms, sign};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn hmac_sha256_vecteurs_rfc_4231() {
    // (clé, message, étiquette attendue) : cas 1, 2, 3, 4, 6 et 7 de la RFC 4231 §4.
    let cas_4_key: Vec<u8> = (1..=25).collect();
    let cas: Vec<(Vec<u8>, Vec<u8>, &str)> = vec![
        (
            vec![0x0b; 20],
            b"Hi There".to_vec(),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        ),
        (
            b"Jefe".to_vec(),
            b"what do ya want for nothing?".to_vec(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
        ),
        (
            vec![0xaa; 20],
            vec![0xdd; 50],
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
        ),
        (
            cas_4_key,
            vec![0xcd; 50],
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
        ),
        // Cas 6 : clé de 131 octets, plus longue qu'un bloc (hachée d'abord).
        (
            vec![0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        ),
        // Cas 7 : clé de 131 octets et message de plus d'un bloc.
        (
            vec![0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm."
                .to_vec(),
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
        ),
    ];
    for (index, (key, message, expected)) in cas.iter().enumerate() {
        assert_eq!(
            hex(&hmac_sha256(key, message)),
            *expected,
            "vecteur RFC 4231 n°{index}"
        );
    }
}

#[test]
fn hmac_sha256_cle_de_la_taille_d_un_bloc_et_cle_vide() {
    // Clés de 63, 64 et 65 octets : la frontière du hachage de la clé.
    let message = b"frontiere";
    let hmac_63 = hmac_sha256(&[0x41; 63], message);
    let hmac_64 = hmac_sha256(&[0x41; 64], message);
    let hmac_65 = hmac_sha256(&[0x41; 65], message);
    assert_ne!(hmac_63, hmac_64);
    assert_ne!(hmac_64, hmac_65);
    // Une clé de 64 octets n'est pas hachée ; une clé de 65 octets l'est (elle équivaut à son
    // empreinte SHA-256 de 32 octets, complétée de zéros).
    let empreinte = {
        use sha2::{Digest, Sha256};
        let digest: [u8; 32] = Sha256::digest([0x41u8; 65]).into();
        digest
    };
    assert_eq!(hmac_65, hmac_sha256(&empreinte, message));
    // Clé vide : définie, et égale à une clé de 64 zéros.
    assert_eq!(hmac_sha256(b"", message), hmac_sha256(&[0u8; 64], message));
}

#[test]
fn base64_vecteurs_rfc_4648_section_10() {
    for (entree, attendu) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64_standard(entree.as_bytes()), attendu, "{entree:?}");
    }
}

#[test]
fn base64_alphabet_complet() {
    // Tous les octets : les caractères `+` et `/` de l'alphabet standard apparaissent.
    assert_eq!(base64_standard(&[0xfb, 0xff, 0xbf]), "+/+/");
    assert_eq!(base64_standard(&[0xff]), "/w==");
    assert_eq!(base64_standard(&[0xfb]), "+w==");
    let tous: Vec<u8> = (0..=255).collect();
    let texte = base64_standard(&tous);
    assert_eq!(texte.len(), 344);
    assert!(texte.ends_with('='));
    assert!(texte.starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
}

#[test]
fn horodatage_iso_8601_a_la_milliseconde() {
    // Exemple de la documentation OKX v5.
    assert_eq!(iso_ms(1_607_418_537_715), "2020-12-08T09:08:57.715Z");
    assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(iso_ms(1_793_191_500_123), "2026-10-28T12:45:00.123Z");
    // Jour bissextile et siècle non bissextile (valeurs vérifiées avec `date -u`).
    assert_eq!(iso_ms(1_709_208_000_000), "2024-02-29T12:00:00.000Z");
    assert_eq!(iso_ms(4_107_542_400_000), "2100-03-01T00:00:00.000Z");
    // Toujours 24 caractères et un `Z` final, y compris avec des zéros à compléter.
    assert_eq!(iso_ms(1_000_000_005).len(), 24);
    assert_eq!(iso_ms(1_000_000_005), "1970-01-12T13:46:40.005Z");
}

#[test]
fn signature_de_reference_openssl() {
    // `printf '%s' '<préhash>' | openssl dgst -sha256 -hmac 'SECRET-DE-TEST-FACTICE' -binary | base64`
    let secret = "SECRET-DE-TEST-FACTICE";
    let ts = "2020-12-08T09:08:57.715Z";
    assert_eq!(
        sign(
            secret,
            ts,
            "GET",
            "/api/v5/account/instruments?instType=SPOT",
            ""
        ),
        "4INokyLb36R5yRduL5huwaD9OU7pnpshE5gSEEmItaM="
    );
    assert_eq!(
        sign(secret, ts, "GET", "/api/v5/account/config", ""),
        "/Np7uxpHbNZuc82rOPxsi8R1aU7aeO98cBGrlhNhUsI="
    );
    // Le corps entre dans le préhash (méthode POST : préhash du format de la documentation).
    assert_eq!(
        sign(secret, ts, "POST", "/api/v5/trade/order", "{\"a\":\"b\"}"),
        "L4D+40PAiJ9SUfIQJHEGzPn8RTdtXQ1N7stwNWrwfdc="
    );
}

#[test]
fn le_prehash_est_la_simple_concatenation() {
    let secret = "SECRET-DE-TEST-FACTICE";
    let ts = "2020-12-08T09:08:57.715Z";
    // Déplacer une frontière entre méthode, chemin et corps ne change pas le préhash : la
    // concaténation est sans séparateur, comme dans la documentation.
    assert_eq!(
        sign(secret, ts, "GET", "/a?x=1", ""),
        sign(secret, ts, "GET", "/a", "?x=1")
    );
    // La chaîne de requête est signée : l'omettre change la signature.
    assert_ne!(
        sign(secret, ts, "GET", "/a?x=1", ""),
        sign(secret, ts, "GET", "/a", "")
    );
    // Le secret, l'horodatage et la méthode comptent.
    let base = sign(secret, ts, "GET", "/a", "");
    assert_ne!(base, sign("AUTRE-SECRET-FACTICE", ts, "GET", "/a", ""));
    assert_ne!(
        base,
        sign(secret, "2020-12-08T09:08:57.716Z", "GET", "/a", "")
    );
    assert_ne!(base, sign(secret, ts, "POST", "/a", ""));
}
