//! Fichier d'instruments OKX, schéma `tv4-instruments-v1` (docs/specs/s9.md §8.5 et §8.6).
//!
//! `GET /api/v5/account/instruments?instType=SPOT` rend les instruments **autorisés pour le compte**,
//! donc, pour un résident de l'EEE, les paires USDC et non les paires USDT (docs/03 §11). Le fichier
//! garde tous les instruments rendus, triés par `inst_id` en ordre d'octets, avec les valeurs
//! numériques **en chaînes** telles qu'OKX les donne (un recalcul en PHP au jalon 2 reste exact).
//!
//! # Hash
//!
//! `instruments_hash` = `"blake3:" +` BLAKE3 du JSON canonique (clés triées, sans espace) de l'objet
//! `{schema, exchange, site, inst_type, instruments}`. `fetched_at`, `server_time_ms` et `endpoint` en
//! sont exclus : deux lectures de contenu identique ont le même hash. [`InstrumentsFile::load`] le
//! vérifie ; un fichier modifié à la main donne [`InstrumentsError::HashMismatch`].
//!
//! # Conversion en règles de simulation
//!
//! [`InstrumentsFile::to_rules`] : `tick_size = tickSz`, `lot_size = lotSz`, `min_qty = minSz` et
//! `min_notional = 0`, parce qu'OKX n'a aucun minimum de notional en devise de cotation
//! (docs/04 §3.1 point 6). Le résultat passe par [`InstrumentRules::validate`].

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::sign::iso_ms;
use crate::model::hash::{HASH_PREFIX, hash_json};
use crate::model::{InstrumentRules, ModelError, Ts};

/// Identifiant du schéma du fichier.
pub const SCHEMA: &str = "tv4-instruments-v1";

/// Nom de l'exchange écrit dans le fichier.
pub const EXCHANGE: &str = "okx";

/// Chemin de l'endpoint interrogé, écrit dans le fichier.
pub const ENDPOINT: &str = "/api/v5/account/instruments";

/// Site de l'EEE.
pub const SITE_EEA: &str = "eea";

/// Erreurs du fichier d'instruments.
#[derive(Debug, Error)]
pub enum InstrumentsError {
    /// Le fichier n'a pas pu être lu.
    #[error("fichier d'instruments `{path}` illisible : {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// JSON invalide ou champs inattendus.
    #[error("fichier d'instruments `{path}` invalide : {reason}")]
    Parse { path: String, reason: String },

    /// Le schéma n'est pas `tv4-instruments-v1`.
    #[error("schéma d'instruments inconnu : « {found} » au lieu de « {SCHEMA} »")]
    UnknownSchema { found: String },

    /// Le hash écrit ne correspond pas au contenu : fichier modifié.
    #[error("hash des instruments faux : le fichier annonce {expected}, le contenu donne {found}")]
    HashMismatch { expected: String, found: String },

    /// Les instruments ne sont pas triés par `inst_id` en ordre d'octets, sans doublon.
    #[error("instruments non triés ou en double près de `{inst_id}`")]
    Unsorted { inst_id: String },

    /// Un champ obligatoire d'un instrument de la réponse OKX est absent ou mal typé.
    #[error("réponse OKX : instrument {row} : champ `{field}` absent ou invalide")]
    ResponseField { row: usize, field: &'static str },

    /// Deux instruments de la réponse portent le même `instId`.
    #[error("réponse OKX : instrument `{inst_id}` en double")]
    DuplicateInstrument { inst_id: String },

    /// L'instrument demandé n'est pas dans le fichier.
    #[error("instrument `{inst_id}` absent du fichier d'instruments")]
    NotListed { inst_id: String },

    /// Un pas ou un minimum n'est pas un nombre.
    #[error("instrument `{inst_id}` : `{field}` = « {value} » n'est pas un nombre")]
    InvalidNumber {
        inst_id: String,
        field: &'static str,
        value: String,
    },

    /// Les règles refusées par le modèle (pas dont l'inverse n'est pas entier, etc.).
    #[error("instrument `{inst_id}` : {source}")]
    InvalidRules {
        inst_id: String,
        #[source]
        source: ModelError,
    },

    /// Le JSON canonique n'a pas pu être produit.
    #[error("hash des instruments impossible : {0}")]
    Canonical(#[from] ModelError),
}

/// Changement annoncé d'un instrument (`upcChg`), stocké et haché mais pas appliqué au backtest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpcomingChange {
    pub inst_id: String,
    pub param: String,
    pub new_value: String,
    pub eff_time: String,
}

/// Un instrument du fichier. Les nombres restent en chaînes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instrument {
    pub inst_id: String,
    pub base_ccy: String,
    pub quote_ccy: String,
    pub tick_sz: String,
    pub lot_sz: String,
    pub min_sz: String,
    pub state: String,
    pub list_time: String,
    pub rule_type: String,
    pub trade_quote_ccy_list: Vec<String>,
    /// Tableau brut d'OKX (`param`, `newValue`, `effTime`), recopié tel quel.
    pub upc_chg: Vec<Value>,
}

/// Fichier d'instruments, schéma `tv4-instruments-v1`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentsFile {
    pub schema: String,
    pub exchange: String,
    pub site: String,
    pub endpoint: String,
    pub inst_type: String,
    pub fetched_at: String,
    pub server_time_ms: Ts,
    pub instruments_hash: String,
    pub instruments: Vec<Instrument>,
}

/// Partie hachée du fichier.
#[derive(Serialize)]
struct Hashed<'a> {
    schema: &'a str,
    exchange: &'a str,
    site: &'a str,
    inst_type: &'a str,
    instruments: &'a [Instrument],
}

/// Site OKX d'une URL de base : `eea` pour `eea.okx.com`, `global` pour `www.okx.com`, `other` sinon
/// (serveur de test).
pub fn site_of(base_url: &str) -> &'static str {
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .unwrap_or(base_url);
    match rest.split(['/', ':']).next().unwrap_or("") {
        "eea.okx.com" => SITE_EEA,
        "www.okx.com" => "global",
        _ => "other",
    }
}

/// Nom de fichier daté : `spot-AAAAMMJJTHHMMSSZ.json` (heure UTC du serveur, en secondes).
pub fn file_name(inst_type: &str, server_time_ms: Ts) -> String {
    let iso = iso_ms(server_time_ms);
    // `2026-10-28T12:45:00.123Z` devient `20261028T124500Z`.
    let compact: String = iso
        .chars()
        .take_while(|&c| c != '.')
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    format!("{}-{compact}Z.json", inst_type.to_ascii_lowercase())
}

impl InstrumentsFile {
    /// Construit le fichier depuis le tableau `data` de la réponse OKX.
    ///
    /// Les instruments sont triés par `inst_id` (octets). Un `instId` en double est une erreur. Un
    /// champ absent est une erreur pour `instId`, `tickSz`, `lotSz`, `minSz` et `state`, et une
    /// chaîne vide pour les autres.
    pub fn from_response(
        site: &str,
        inst_type: &str,
        server_time_ms: Ts,
        data: &[Value],
    ) -> Result<InstrumentsFile, InstrumentsError> {
        let mut instruments = data
            .iter()
            .enumerate()
            .map(|(index, row)| parse_instrument(index + 1, row))
            .collect::<Result<Vec<Instrument>, InstrumentsError>>()?;
        instruments.sort_by(|a, b| a.inst_id.as_bytes().cmp(b.inst_id.as_bytes()));
        if let Some(pair) = instruments
            .windows(2)
            .find(|w| w[0].inst_id == w[1].inst_id)
        {
            return Err(InstrumentsError::DuplicateInstrument {
                inst_id: pair[0].inst_id.clone(),
            });
        }
        let mut file = InstrumentsFile {
            schema: SCHEMA.to_string(),
            exchange: EXCHANGE.to_string(),
            site: site.to_string(),
            endpoint: ENDPOINT.to_string(),
            inst_type: inst_type.to_string(),
            fetched_at: iso_ms(server_time_ms),
            server_time_ms,
            instruments_hash: String::new(),
            instruments,
        };
        file.instruments_hash = file.compute_hash()?;
        Ok(file)
    }

    /// Hash du contenu (voir le module), indépendant de `fetched_at` et de `server_time_ms`.
    pub fn compute_hash(&self) -> Result<String, InstrumentsError> {
        let hashed = Hashed {
            schema: &self.schema,
            exchange: &self.exchange,
            site: &self.site,
            inst_type: &self.inst_type,
            instruments: &self.instruments,
        };
        Ok(format!("{HASH_PREFIX}{}", hash_json(&hashed)?))
    }

    /// Lit un fichier d'instruments et vérifie son schéma, son tri et son hash.
    pub fn load(path: &Path) -> Result<InstrumentsFile, InstrumentsError> {
        let shown = path.display().to_string();
        let text = fs::read_to_string(path).map_err(|source| InstrumentsError::Read {
            path: shown.clone(),
            source,
        })?;
        InstrumentsFile::from_json_str(&text, &shown)
    }

    /// Comme [`InstrumentsFile::load`], sur un texte déjà lu. `origin` nomme la source dans les
    /// erreurs.
    pub fn from_json_str(text: &str, origin: &str) -> Result<InstrumentsFile, InstrumentsError> {
        let file: InstrumentsFile =
            serde_json::from_str(text).map_err(|error| InstrumentsError::Parse {
                path: origin.to_string(),
                reason: error.to_string(),
            })?;
        if file.schema != SCHEMA {
            return Err(InstrumentsError::UnknownSchema { found: file.schema });
        }
        if let Some(pair) = file
            .instruments
            .windows(2)
            .find(|w| w[0].inst_id.as_bytes() >= w[1].inst_id.as_bytes())
        {
            return Err(InstrumentsError::Unsorted {
                inst_id: pair[1].inst_id.clone(),
            });
        }
        let found = file.compute_hash()?;
        if found != file.instruments_hash {
            return Err(InstrumentsError::HashMismatch {
                expected: file.instruments_hash,
                found,
            });
        }
        Ok(file)
    }

    /// Octets du fichier : JSON indenté, retour à la ligne final. Le hash est celui du contenu, pas
    /// celui de ces octets.
    pub fn to_bytes(&self) -> Result<Vec<u8>, InstrumentsError> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(ModelError::from)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// L'instrument `inst_id`, s'il est listé.
    pub fn get(&self, inst_id: &str) -> Option<&Instrument> {
        self.instruments
            .binary_search_by(|instrument| instrument.inst_id.as_bytes().cmp(inst_id.as_bytes()))
            .ok()
            .map(|index| &self.instruments[index])
    }

    /// Règles de simulation de `inst_id` (voir le module). `min_notional` vaut 0.
    pub fn to_rules(&self, inst_id: &str) -> Result<InstrumentRules, InstrumentsError> {
        let instrument = self
            .get(inst_id)
            .ok_or_else(|| InstrumentsError::NotListed {
                inst_id: inst_id.to_string(),
            })?;
        let number = |field: &'static str, value: &str| -> Result<f64, InstrumentsError> {
            value
                .parse::<f64>()
                .map_err(|_| InstrumentsError::InvalidNumber {
                    inst_id: inst_id.to_string(),
                    field,
                    value: value.to_string(),
                })
        };
        InstrumentRules::new(
            number("tickSz", &instrument.tick_sz)?,
            number("lotSz", &instrument.lot_sz)?,
            number("minSz", &instrument.min_sz)?,
            0.0,
        )
        .map_err(|source| InstrumentsError::InvalidRules {
            inst_id: inst_id.to_string(),
            source,
        })
    }

    /// Changements annoncés (`upcChg`) de tous les instruments, dans l'ordre du fichier. Une entrée
    /// dont les trois champs ne sont pas des chaînes est ignorée.
    pub fn upcoming_changes(&self) -> Vec<UpcomingChange> {
        let text =
            |entry: &Value, key: &str| entry.get(key).and_then(Value::as_str).map(str::to_string);
        self.instruments
            .iter()
            .flat_map(|instrument| {
                instrument.upc_chg.iter().filter_map(|entry| {
                    Some(UpcomingChange {
                        inst_id: instrument.inst_id.clone(),
                        param: text(entry, "param")?,
                        new_value: text(entry, "newValue")?,
                        eff_time: text(entry, "effTime")?,
                    })
                })
            })
            .collect()
    }
}

/// Décode un instrument de la réponse. `row` compte à partir de 1.
fn parse_instrument(row: usize, value: &Value) -> Result<Instrument, InstrumentsError> {
    let required = |key: &str, field: &'static str| -> Result<String, InstrumentsError> {
        match value.get(key).and_then(Value::as_str) {
            Some(text) if !text.is_empty() => Ok(text.to_string()),
            _ => Err(InstrumentsError::ResponseField { row, field }),
        }
    };
    // Un pas ou un minimum peut être présent mais vide chez OKX pour un instrument spécial : la
    // conversion en règles échouera alors, avec le nom de l'instrument.
    let required_text = |key: &str, field: &'static str| -> Result<String, InstrumentsError> {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(InstrumentsError::ResponseField { row, field })
    };
    let optional = |key: &str, field: &'static str| -> Result<String, InstrumentsError> {
        match value.get(key) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(text)) => Ok(text.clone()),
            Some(_) => Err(InstrumentsError::ResponseField { row, field }),
        }
    };
    let quote_list = match value.get("tradeQuoteCcyList") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or(InstrumentsError::ResponseField {
                        row,
                        field: "tradeQuoteCcyList",
                    })
            })
            .collect::<Result<Vec<String>, InstrumentsError>>()?,
        Some(Value::String(text)) => text
            .split(',')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        Some(_) => {
            return Err(InstrumentsError::ResponseField {
                row,
                field: "tradeQuoteCcyList",
            });
        }
    };
    let upc_chg = match value.get("upcChg") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => {
            return Err(InstrumentsError::ResponseField {
                row,
                field: "upcChg",
            });
        }
    };
    Ok(Instrument {
        inst_id: required("instId", "instId")?,
        base_ccy: optional("baseCcy", "baseCcy")?,
        quote_ccy: optional("quoteCcy", "quoteCcy")?,
        tick_sz: required_text("tickSz", "tickSz")?,
        lot_sz: required_text("lotSz", "lotSz")?,
        min_sz: required_text("minSz", "minSz")?,
        state: required("state", "state")?,
        list_time: optional("listTime", "listTime")?,
        rule_type: optional("ruleType", "ruleType")?,
        trade_quote_ccy_list: quote_list,
        upc_chg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(inst_id: &str, min_sz: &str) -> Value {
        json!({
            "instId": inst_id, "baseCcy": "BTC", "quoteCcy": "USDC",
            "tickSz": "0.1", "lotSz": "0.00000001", "minSz": min_sz,
            "state": "live", "listTime": "1755763200000", "ruleType": "normal",
            "tradeQuoteCcyList": ["USDC"], "upcChg": []
        })
    }

    fn file_of(data: &[Value]) -> InstrumentsFile {
        InstrumentsFile::from_response(SITE_EEA, "SPOT", 1_793_191_500_123, data).unwrap()
    }

    #[test]
    fn nom_de_fichier_date() {
        assert_eq!(
            file_name("SPOT", 1_793_191_500_123),
            "spot-20261028T124500Z.json"
        );
    }

    #[test]
    fn site_selon_l_hote() {
        assert_eq!(site_of("https://eea.okx.com"), "eea");
        assert_eq!(site_of("https://www.okx.com/"), "global");
        assert_eq!(site_of("http://127.0.0.1:1234"), "other");
    }

    #[test]
    fn tri_par_octets_et_hash_stable() {
        let a = file_of(&[row("ETH-USDC", "0.001"), row("BTC-USDC", "0.0001")]);
        let ids: Vec<&str> = a.instruments.iter().map(|i| i.inst_id.as_str()).collect();
        assert_eq!(ids, ["BTC-USDC", "ETH-USDC"]);
        // Le hash ignore l'ordre de la réponse, `fetched_at` et `server_time_ms`.
        let b = InstrumentsFile::from_response(
            SITE_EEA,
            "SPOT",
            1_793_191_999_000,
            &[row("BTC-USDC", "0.0001"), row("ETH-USDC", "0.001")],
        )
        .unwrap();
        assert_eq!(a.instruments_hash, b.instruments_hash);
        assert_ne!(a.fetched_at, b.fetched_at);
        // Un minimum modifié change le hash.
        let c = file_of(&[row("ETH-USDC", "0.001"), row("BTC-USDC", "0.0002")]);
        assert_ne!(a.instruments_hash, c.instruments_hash);
        assert!(a.instruments_hash.starts_with("blake3:"));
    }

    #[test]
    fn aller_retour_et_hash_verifie() {
        let file = file_of(&[row("BTC-USDC", "0.0001")]);
        let text = String::from_utf8(file.to_bytes().unwrap()).unwrap();
        assert_eq!(InstrumentsFile::from_json_str(&text, "t").unwrap(), file);
        let altere = text.replace("\"0.0001\"", "\"0.0009\"");
        assert!(matches!(
            InstrumentsFile::from_json_str(&altere, "t"),
            Err(InstrumentsError::HashMismatch { .. })
        ));
        let autre_schema = text.replace(SCHEMA, "tv4-instruments-v0");
        assert!(matches!(
            InstrumentsFile::from_json_str(&autre_schema, "t"),
            Err(InstrumentsError::UnknownSchema { .. })
        ));
        assert!(matches!(
            InstrumentsFile::from_json_str("{}", "t"),
            Err(InstrumentsError::Parse { .. })
        ));
    }

    #[test]
    fn champs_obligatoires_et_doublons() {
        let mut sans_state = row("BTC-USDC", "0.0001");
        sans_state.as_object_mut().unwrap().remove("state");
        assert!(matches!(
            InstrumentsFile::from_response(SITE_EEA, "SPOT", 1, &[sans_state]),
            Err(InstrumentsError::ResponseField {
                row: 1,
                field: "state"
            })
        ));
        let mut sans_ccy = row("BTC-USDC", "0.0001");
        sans_ccy.as_object_mut().unwrap().remove("baseCcy");
        let file = InstrumentsFile::from_response(SITE_EEA, "SPOT", 1, &[sans_ccy]).unwrap();
        assert_eq!(file.instruments[0].base_ccy, "");
        assert!(matches!(
            InstrumentsFile::from_response(
                SITE_EEA,
                "SPOT",
                1,
                &[row("BTC-USDC", "1"), row("BTC-USDC", "2")]
            ),
            Err(InstrumentsError::DuplicateInstrument { .. })
        ));
    }

    #[test]
    fn conversion_en_regles() {
        let file = file_of(&[row("BTC-USDC", "0.0001")]);
        let rules = file.to_rules("BTC-USDC").unwrap();
        assert_eq!(rules.tick_size, 0.1);
        assert_eq!(rules.lot_size, 1e-8);
        assert_eq!(rules.min_qty, 0.0001);
        assert_eq!(rules.min_notional, 0.0);
        assert!(matches!(
            file.to_rules("SOL-USDC"),
            Err(InstrumentsError::NotListed { .. })
        ));
        let faux = file_of(&[row("BTC-USDC", "abc")]);
        assert!(matches!(
            faux.to_rules("BTC-USDC"),
            Err(InstrumentsError::InvalidNumber { field: "minSz", .. })
        ));
    }

    #[test]
    fn changements_annonces() {
        let mut avec = row("BTC-USDC", "0.0001");
        avec["upcChg"] =
            json!([{"param": "minSz", "newValue": "0.001", "effTime": "1793200000000"}]);
        let file = file_of(&[avec]);
        assert_eq!(
            file.upcoming_changes(),
            [UpcomingChange {
                inst_id: "BTC-USDC".into(),
                param: "minSz".into(),
                new_value: "0.001".into(),
                eff_time: "1793200000000".into(),
            }]
        );
    }
}
