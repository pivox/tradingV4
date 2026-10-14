//! JSON canonique (clés triées) et hash blake3 (docs/03-architecture.md §3.1, §5.3 et §7.4).
//!
//! Servent au `config_hash`, au `run_id` et au `result_hash`. Le `dataset_hash` suit, lui, un
//! encodage binaire à part, `dsh-v1` (docs/dataset-hash.md), qui n'utilise que [`blake3_hex`].
//!
//! # Pourquoi passer par `serde_json::Value`
//!
//! `serde_json::to_string(&struct)` écrit les champs dans l'ordre de déclaration : réordonner un
//! champ dans le code changerait le hash. On sérialise donc d'abord en `serde_json::Value`, dont
//! les objets sont des `BTreeMap` (clés triées), puis on écrit cette `Value`. Ce tri n'existe que
//! si la feature `preserve_order` de `serde_json` reste désactivée : deux tests le vérifient.
//!
//! Ce n'est pas la canonicalisation RFC 8785 (JCS) : c'est suffisant pour un hash interne.
//!
//! # Flottants
//!
//! `serde_json` écrit `null` à la place d'un NaN ou d'un infini, sans erreur : deux valeurs
//! différentes auraient alors le même hash. [`canonical_json`] refuse donc ces valeurs avant
//! d'écrire quoi que ce soit, et ramène `-0.0` à `0.0` (docs/03 §7.4).

use std::fmt;

use serde::ser::{self, Serialize, Serializer};
use serde_json::Value;

use super::ModelError;

/// Préfixe des hashes dans les manifestes (`"content_hash": "blake3:…"`, docs/03 §5.2).
pub const HASH_PREFIX: &str = "blake3:";

/// Hash blake3 d'octets, rendu en hexadécimal minuscule (64 caractères).
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Comme [`blake3_hex`], précédé de `blake3:` : forme des manifestes.
pub fn blake3_prefixed(bytes: &[u8]) -> String {
    format!("{HASH_PREFIX}{}", blake3_hex(bytes))
}

/// JSON canonique de `value` : clés triées, sans espace, `-0.0` écrit `0.0`.
///
/// Refuse les flottants non finis (`ModelError::NonFiniteFloat`).
pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, ModelError> {
    // Le contrôle doit précéder `to_value`, qui transformerait déjà un NaN en `null`.
    if value.serialize(FiniteChecker).is_err() {
        return Err(ModelError::NonFiniteFloat);
    }
    let mut tree = serde_json::to_value(value)?;
    normalize_negative_zero(&mut tree);
    Ok(serde_json::to_string(&tree)?)
}

/// Hash blake3 (hexadécimal) du JSON canonique de `value` : base du `config_hash` (docs/03 §5.3).
pub fn hash_json<T: Serialize>(value: &T) -> Result<String, ModelError> {
    Ok(blake3_hex(canonical_json(value)?.as_bytes()))
}

/// Remplace chaque `-0.0` de l'arbre par `0.0`.
fn normalize_negative_zero(value: &mut Value) {
    match value {
        // `0.0 == -0.0` est vrai : cette garde attrape les deux zéros flottants.
        Value::Number(number) if number.is_f64() && number.as_f64() == Some(0.0) => {
            *value = Value::from(0.0_f64);
        }
        Value::Number(_) => {}
        Value::Array(elements) => elements.iter_mut().for_each(normalize_negative_zero),
        Value::Object(fields) => fields.values_mut().for_each(normalize_negative_zero),
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Détecteur de flottants non finis
//
// Un `Serializer` serde qui ne produit rien : il parcourt la valeur comme le ferait `serde_json`
// et échoue dès qu'il rencontre un `f32` ou `f64` NaN ou infini. Tout le reste est ignoré. Le code
// est long parce que le trait `Serializer` impose une méthode par type de donnée, mais il ne
// contient qu'une seule vraie règle (voir `serialize_f32` et `serialize_f64`).
// ---------------------------------------------------------------------------------------------

/// Erreur interne du détecteur ; convertie en `ModelError::NonFiniteFloat` par l'appelant.
#[derive(Debug)]
struct NonFiniteError;

impl fmt::Display for NonFiniteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("flottant non fini")
    }
}

impl std::error::Error for NonFiniteError {}

impl ser::Error for NonFiniteError {
    fn custom<T: fmt::Display>(_message: T) -> Self {
        NonFiniteError
    }
}

/// Serializer-détecteur. Aussi utilisé comme sérialiseur des sous-éléments (listes, tables...).
struct FiniteChecker;

impl Serializer for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;
    type SerializeSeq = FiniteChecker;
    type SerializeTuple = FiniteChecker;
    type SerializeTupleStruct = FiniteChecker;
    type SerializeTupleVariant = FiniteChecker;
    type SerializeMap = FiniteChecker;
    type SerializeStruct = FiniteChecker;
    type SerializeStructVariant = FiniteChecker;

    // La règle : un flottant doit être fini.
    fn serialize_f32(self, v: f32) -> Result<(), NonFiniteError> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(NonFiniteError)
        }
    }

    fn serialize_f64(self, v: f64) -> Result<(), NonFiniteError> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(NonFiniteError)
        }
    }

    // Les types qui contiennent d'autres valeurs : on descend dedans.
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn serialize_seq(self, _size: Option<usize>) -> Result<FiniteChecker, NonFiniteError> {
        Ok(FiniteChecker)
    }

    fn serialize_tuple(self, _size: usize) -> Result<FiniteChecker, NonFiniteError> {
        Ok(FiniteChecker)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _size: usize,
    ) -> Result<FiniteChecker, NonFiniteError> {
        Ok(FiniteChecker)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _size: usize,
    ) -> Result<FiniteChecker, NonFiniteError> {
        Ok(FiniteChecker)
    }

    fn serialize_map(self, _size: Option<usize>) -> Result<FiniteChecker, NonFiniteError> {
        Ok(FiniteChecker)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _size: usize,
    ) -> Result<FiniteChecker, NonFiniteError> {
        Ok(FiniteChecker)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _size: usize,
    ) -> Result<FiniteChecker, NonFiniteError> {
        Ok(FiniteChecker)
    }

    // Tout le reste ne peut pas contenir de flottant.
    fn serialize_bool(self, _v: bool) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_i8(self, _v: i8) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_i16(self, _v: i16) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_i32(self, _v: i32) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_i64(self, _v: i64) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_i128(self, _v: i128) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_u8(self, _v: u8) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_u16(self, _v: u16) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_u32(self, _v: u32) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_u64(self, _v: u64) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_u128(self, _v: u128) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_char(self, _v: char) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_str(self, _v: &str) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_none(self) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_unit(self) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), NonFiniteError> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

// Les conteneurs (listes, tables, structs) vérifient chacun de leurs éléments.

impl ser::SerializeSeq for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;

    fn serialize_element<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn end(self) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

impl ser::SerializeTuple for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;

    fn serialize_element<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn end(self) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

impl ser::SerializeTupleStruct for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn end(self) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

impl ser::SerializeTupleVariant for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn end(self) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

impl ser::SerializeMap for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, cle: &T) -> Result<(), NonFiniteError> {
        cle.serialize(FiniteChecker)
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn end(self) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

impl ser::SerializeStruct for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn end(self) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

impl ser::SerializeStructVariant for FiniteChecker {
    type Ok = ();
    type Error = NonFiniteError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), NonFiniteError> {
        value.serialize(FiniteChecker)
    }

    fn end(self) -> Result<(), NonFiniteError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde::Serialize;

    use super::*;

    /// Deux déclarations des mêmes champs, dans deux ordres différents.
    #[derive(Serialize)]
    struct OrdreAbc {
        alpha: u32,
        milieu: f64,
        zeta: &'static str,
    }

    #[derive(Serialize)]
    struct OrdreZya {
        zeta: &'static str,
        milieu: f64,
        alpha: u32,
    }

    #[derive(Serialize)]
    struct Nested {
        params: Vec<Option<f64>>,
        table: BTreeMap<String, f32>,
    }

    #[test]
    fn l_ordre_des_cles_ne_depend_pas_de_la_declaration() {
        let a = canonical_json(&OrdreAbc {
            alpha: 1,
            milieu: 2.5,
            zeta: "z",
        })
        .unwrap();
        let b = canonical_json(&OrdreZya {
            zeta: "z",
            milieu: 2.5,
            alpha: 1,
        })
        .unwrap();
        assert_eq!(a, r#"{"alpha":1,"milieu":2.5,"zeta":"z"}"#);
        assert_eq!(a, b);
        // Le piège évité : `to_string(&struct)` suit l'ordre de déclaration.
        let naif = serde_json::to_string(&OrdreZya {
            zeta: "z",
            milieu: 2.5,
            alpha: 1,
        })
        .unwrap();
        assert_eq!(naif, r#"{"zeta":"z","milieu":2.5,"alpha":1}"#);
        assert_eq!(blake3_hex(a.as_bytes()), blake3_hex(b.as_bytes()));
    }

    #[test]
    fn le_tri_est_recursif() {
        let value: Value =
            serde_json::from_str(r#"{"b":{"y":1,"x":[{"d":1,"c":2}]},"a":null}"#).unwrap();
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"a":null,"b":{"x":[{"c":2,"d":1}],"y":1}}"#
        );
    }

    /// Test comportemental : avec la feature `preserve_order`, `serde_json::Map` garde l'ordre
    /// d'insertion et ce test échoue.
    #[test]
    fn preserve_order_n_est_pas_active() {
        let value: Value = serde_json::from_str(r#"{"z":1,"a":2,"m":3}"#).unwrap();
        assert_eq!(value.to_string(), r#"{"a":2,"m":3,"z":1}"#);
    }

    /// Test de verrouillage : `indexmap` ne doit pas figurer parmi les dépendances de `serde_json`
    /// dans `Cargo.lock` (c'est ce que `preserve_order` y ajouterait). On cible l'entrée de
    /// `serde_json` : `indexmap` peut légitimement figurer ailleurs (dépendance optionnelle de
    /// `toml`, non activée).
    #[test]
    fn serde_json_n_a_pas_indexmap_dans_cargo_lock() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
        let text = std::fs::read_to_string(&path).unwrap();
        let lock: toml::Value = toml::from_str(&text).unwrap();
        let paquets = lock["package"].as_array().unwrap();
        let serde_json = paquets
            .iter()
            .find(|p| p["name"].as_str() == Some("serde_json"))
            .expect("serde_json absent de Cargo.lock");
        let dependances = serde_json["dependencies"].as_array().unwrap();
        assert!(!dependances.is_empty());
        for dependency in dependances {
            let name = dependency.as_str().unwrap();
            assert!(
                !name.starts_with("indexmap"),
                "indexmap dans serde_json : {name}"
            );
        }
    }

    #[test]
    fn blake3_vecteur_connu() {
        // Vecteur officiel blake3 pour l'entrée vide.
        assert_eq!(
            blake3_hex(b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(
            blake3_prefixed(b""),
            "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(blake3_hex(b"tradingv4").len(), 64);
    }

    #[test]
    fn hash_json_stable_sur_un_vecteur_connu() {
        // Valeur écrite en dur : si elle change, tous les config_hash déjà archivés changent.
        let json = canonical_json(&OrdreZya {
            zeta: "z",
            milieu: 2.5,
            alpha: 1,
        })
        .unwrap();
        assert_eq!(json, r#"{"alpha":1,"milieu":2.5,"zeta":"z"}"#);
        assert_eq!(
            hash_json(&OrdreZya {
                zeta: "z",
                milieu: 2.5,
                alpha: 1
            })
            .unwrap(),
            "3cac74bea92ba6c0542e781c903d886c2dce17b6a5c1920a6c801a6961905de2"
        );
    }

    #[test]
    fn rendu_fige_des_flottants_delicats() {
        // serde_json écrit les flottants avec un algorithme du plus court aller-retour. Si une montée de
        // version de serde_json change ce rendu (exposant, nombre de chiffres), tous les config_hash
        // changent : ce test doit alors échouer. Les chaînes attendues sont écrites en dur (relevées avec
        // serde_json 1.0.154 : l'exposant positif porte un `+`, `1e+21`).
        let cas: [(f64, &str); 8] = [
            (0.1, "0.1"),
            (1e-7, "1e-7"),
            (1e21, "1e+21"),
            (5e-324, "5e-324"),
            (123456789.125, "123456789.125"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (100.0, "100.0"),
            (-0.0, "0.0"),
        ];
        for (number, expected) in cas {
            assert_eq!(
                canonical_json(&number).unwrap(),
                expected,
                "flottant {number:e}"
            );
        }
    }

    #[test]
    fn refus_des_flottants_non_finis() {
        for mauvais in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let direct = canonical_json(&OrdreAbc {
                alpha: 1,
                milieu: mauvais,
                zeta: "z",
            });
            assert!(
                matches!(direct, Err(ModelError::NonFiniteFloat)),
                "{mauvais}"
            );
            let imbrique = canonical_json(&Nested {
                params: vec![Some(1.0), Some(mauvais)],
                table: BTreeMap::new(),
            });
            assert!(
                matches!(imbrique, Err(ModelError::NonFiniteFloat)),
                "{mauvais}"
            );
            assert!(hash_json(&mauvais).is_err());
        }
        let mut table = BTreeMap::new();
        table.insert("x".to_string(), f32::NAN);
        let f32_nan = canonical_json(&Nested {
            params: vec![],
            table,
        });
        assert!(matches!(f32_nan, Err(ModelError::NonFiniteFloat)));
    }

    #[test]
    fn serde_json_cache_bien_le_nan_sans_notre_controle() {
        // Justifie le détecteur : sans lui, NaN et `None` donneraient le même JSON.
        assert_eq!(serde_json::to_string(&f64::NAN).unwrap(), "null");
        assert_eq!(serde_json::to_value(f64::NAN).unwrap(), Value::Null);
    }

    #[test]
    fn les_valeurs_valides_et_les_null_legitimes_passent() {
        let value = Nested {
            params: vec![Some(1.5), None, Some(-2.0)],
            table: BTreeMap::new(),
        };
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"params":[1.5,null,-2.0],"table":{}}"#
        );
        assert!(canonical_json(&f64::MAX).is_ok());
    }

    #[test]
    fn zero_negatif_est_ramene_a_zero() {
        let neg = canonical_json(&OrdreAbc {
            alpha: 0,
            milieu: -0.0,
            zeta: "",
        })
        .unwrap();
        let pos = canonical_json(&OrdreAbc {
            alpha: 0,
            milieu: 0.0,
            zeta: "",
        })
        .unwrap();
        assert_eq!(neg, pos);
        assert_eq!(neg, r#"{"alpha":0,"milieu":0.0,"zeta":""}"#);
        assert_eq!(hash_json(&-0.0_f64).unwrap(), hash_json(&0.0_f64).unwrap());
    }

    #[test]
    fn deux_valeurs_differentes_donnent_deux_hashes() {
        let a = hash_json(&OrdreAbc {
            alpha: 1,
            milieu: 2.5,
            zeta: "z",
        })
        .unwrap();
        let b = hash_json(&OrdreAbc {
            alpha: 2,
            milieu: 2.5,
            zeta: "z",
        })
        .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn une_cle_de_table_non_texte_est_une_erreur_json() {
        let mut table = BTreeMap::new();
        table.insert((1, 2), 1.0);
        assert!(matches!(canonical_json(&table), Err(ModelError::Json(_))));
    }
}
