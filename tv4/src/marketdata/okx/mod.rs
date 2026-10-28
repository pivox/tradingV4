//! Lecteur des bougies OKX `history-candles` (docs/03-architecture.md §11, S7, première partie).
//!
//! Le module contient le client REST synchrone ([`OkxClient`], `ureq`), le token bucket partagé entre
//! threads ([`TokenBucket`]), le retry avec jitter, la pagination vers le passé sur une fenêtre
//! `[start, end)` ([`fetch_window`]) et le décodage d'une page ([`parse_page`]). Il ne lit que :
//! l'écriture Parquet par lots de pages, la reprise et `compare-sources` relèvent de `store` et de `cli`.
//! Le module ne dépend que de `model`.
//!
//! # Partie privée (S9)
//!
//! [`sign`] (HMAC-SHA256, Base64, horodatage ISO 8601, signature `OK-ACCESS-SIGN`), [`private`] (clé en
//! lecture seule, client des trois requêtes de `tv4 instruments fetch`) et [`instruments`] (fichier
//! `tv4-instruments-v1`, règles de simulation). Le client public ci-dessous n'est pas touché. Voir
//! docs/specs/s9.md §8.
//!
//! # Requête
//!
//! ```text
//! GET {base_url}/api/v5/market/history-candles?instId=BTC-USDC&bar=1m&after={ts}&limit=300
//! ```
//!
//! `after` est une borne exclusive : OKX renvoie les bougies dont l'`open_time` est **strictement**
//! plus ancien. Chaque requête porte `after` ; `before` n'est jamais envoyé (seul, il renvoie les plus
//! récentes et ferait boucler la pagination).
//!
//! | `Timeframe` | `bar` OKX | Remarque |
//! |---|---|---|
//! | 1m, 5m, 15m, 30m | `1m`, `5m`, `15m`, `30m` | |
//! | 1h, 4h | `1H`, `4H` | Les barres tombent déjà sur les bornes UTC. |
//! | 1d | `1Dutc` | Jamais `1D`, qui ouvre à 16:00 UTC (mesuré le 13/10). |
//! | 12h | refusé | `12Hutc` existe mais n'a pas été vérifié : [`OkxError::UnsupportedTimeframe`]. |
//!
//! # Réponse
//!
//! ```text
//! {"code":"0","msg":"","data":[["ts","o","h","l","c","vol","volCcy","volCcyQuote","confirm"], ...]}
//! ```
//!
//! Triée par `ts` décroissant, tous les champs en chaînes. `ts` est en millisecondes, le volume lu est
//! l'index 5 (monnaie de base), `trades` vaut `None` (OKX ne le fournit pas). Au moins neuf champs
//! sont exigés ; un champ en plus à la fin serait ignoré. Le décodage est strict : une page qui n'est
//! pas strictement décroissante, un `ts` non aligné sur la durée de la barre, un nombre mal formé ou une
//! bougie refusée par [`Candle::validate`] fait échouer toute la page, sans retry (la donnée est fausse,
//! la redemander ne la corrigera pas).
//!
//! # Bougies non clôturées
//!
//! `confirm = "0"` marque la bougie en cours de formation. Elle est exclue du résultat et comptée,
//! comme `continuity.rs` compte `rejected_unclosed` : on garde les `ts` rejetés (liste croissante dans
//! [`WindowFetch::rejected_unclosed`]), que la partie suivante reportera dans le `_manifest.json`
//! (03 §5.2) comme `store::partition` le fait pour les minutes non clôturées de Kraken. Pour une
//! bougie non clôturée, seuls `ts` (alignement, ordre) et `confirm` sont contrôlés : ses prix sont
//! provisoires et ne sont jamais utilisés.

mod bucket;
mod client;
pub mod instruments;
pub mod private;
pub mod sign;

use serde_json::Value;
use thiserror::Error;

use crate::model::{Candle, ModelError, Timeframe, Ts};

pub use bucket::{Jitter, TokenBucket};
pub use client::{
    DEFAULT_BASE_URL, FetchStats, MAX_LIMIT, OkxClient, OkxConfig, WindowError, WindowFetch,
    fetch_window,
};

/// Nombre minimal de champs d'une ligne : `ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm`.
const MIN_FIELDS: usize = 9;

/// Erreurs du client et du décodage OKX.
///
/// Les numéros de ligne (`row`) comptent à partir de 1 dans l'ordre de la page reçue.
#[derive(Debug, Error)]
pub enum OkxError {
    /// Le timeframe n'a pas de barre OKX UTC vérifiée.
    #[error("timeframe {timeframe} sans barre OKX UTC vérifiée (1m, 5m, 15m, 30m, 1h, 4h, 1d)")]
    UnsupportedTimeframe { timeframe: Timeframe },

    /// Erreur de transport (connexion, TLS, timeout, lecture du corps).
    #[error("transport OKX : {message}")]
    Transport { message: String },

    /// Statut HTTP hors 2xx (429 et 5xx sont retentés, les autres 4xx échouent d'emblée).
    #[error("HTTP {status} : {body}")]
    Http { status: u16, body: String },

    /// Enveloppe JSON avec `code != "0"` (typiquement `50011`, trop de requêtes).
    #[error("OKX code {code} : {msg}")]
    Api { code: String, msg: String },

    /// Enveloppe illisible : JSON invalide, `code` ou `data` absent.
    #[error("réponse OKX illisible : {reason}")]
    Envelope { reason: String },

    /// Tous les essais ont échoué ; `last` est l'erreur du dernier.
    #[error("abandon après {attempts} essais : {last}")]
    RetriesExhausted {
        attempts: u32,
        #[source]
        last: Box<OkxError>,
    },

    /// Une ligne n'est pas un tableau d'au moins neuf champs.
    #[error("ligne {row} : {found} champs au lieu de {MIN_FIELDS} au moins")]
    RowShape { row: usize, found: usize },

    /// Un champ n'est pas du type attendu (chaîne numérique, `confirm` valant `0` ou `1`).
    #[error("ligne {row} : champ `{field}` invalide : `{value}`")]
    InvalidField {
        row: usize,
        field: &'static str,
        value: String,
    },

    /// `ts` n'est pas un multiple de la durée de la barre.
    #[error("ligne {row} : ts {ts} non aligné sur la barre {timeframe}")]
    MisalignedTs {
        row: usize,
        ts: Ts,
        timeframe: Timeframe,
    },

    /// La page n'est pas strictement décroissante en `ts`.
    #[error("ligne {row} : ts {ts} n'est pas strictement inférieur au précédent ({previous})")]
    NotDescending { row: usize, previous: Ts, ts: Ts },

    /// La page contient un `ts` supérieur ou égal à `after` : la borne exclusive n'est pas respectée
    /// (sans ce contrôle, la pagination pourrait boucler).
    #[error("ts {ts} supérieur ou égal à la borne exclusive after={after}")]
    NotBeforeCursor { after: Ts, ts: Ts },

    /// La bougie est refusée par le modèle (valeurs non finies, OHLC incohérent...).
    #[error("ligne {row} : {source}")]
    InvalidCandle {
        row: usize,
        #[source]
        source: ModelError,
    },
}

/// Barre OKX (paramètre `bar`) d'un timeframe. `1d` donne `1Dutc` et jamais `1D` (voir le module).
pub fn okx_bar(timeframe: Timeframe) -> Result<&'static str, OkxError> {
    match timeframe {
        Timeframe::M1 => Ok("1m"),
        Timeframe::M5 => Ok("5m"),
        Timeframe::M15 => Ok("15m"),
        Timeframe::M30 => Ok("30m"),
        Timeframe::H1 => Ok("1H"),
        Timeframe::H4 => Ok("4H"),
        Timeframe::D1 => Ok("1Dutc"),
        Timeframe::H12 => Err(OkxError::UnsupportedTimeframe { timeframe }),
    }
}

/// Une page décodée.
///
/// Dans l'ordre reçu : `ts` strictement décroissant. [`fetch_window`] ne remet à
/// `on_page` que les lignes de la fenêtre (les champs `candles` et `rejected_unclosed` sont alors
/// filtrés) ; `oldest_ts` et `rows` décrivent toujours la page reçue en entier.
#[derive(Clone, Debug, PartialEq)]
pub struct OkxPage {
    /// Bougies clôturées (`confirm = "1"`), validées, `trades = None`.
    pub candles: Vec<Candle>,
    /// `ts` des bougies non clôturées (`confirm = "0"`), exclues de `candles`.
    pub rejected_unclosed: Vec<Ts>,
    /// `ts` de la plus ancienne ligne de la page, clôturée ou non : le prochain `after`.
    pub oldest_ts: Option<Ts>,
    /// Nombre de lignes reçues, `confirm = "0"` comprises.
    pub rows: usize,
}

/// Décode le tableau `data` d'une réponse. `after` est la borne exclusive envoyée : tout `ts` qui la
/// dépasse est une erreur ([`OkxError::NotBeforeCursor`]).
pub fn parse_page(data: &[Value], timeframe: Timeframe, after: Ts) -> Result<OkxPage, OkxError> {
    let mut page = OkxPage {
        candles: Vec::with_capacity(data.len()),
        rejected_unclosed: Vec::new(),
        oldest_ts: None,
        rows: data.len(),
    };
    let mut previous: Option<Ts> = None;
    for (index, value) in data.iter().enumerate() {
        let row = index + 1;
        let fields = value
            .as_array()
            .filter(|fields| fields.len() >= MIN_FIELDS)
            .ok_or_else(|| OkxError::RowShape {
                row,
                found: value.as_array().map_or(0, Vec::len),
            })?;
        let text = |index: usize, field: &'static str| -> Result<&str, OkxError> {
            fields
                .get(index)
                .and_then(Value::as_str)
                .ok_or_else(|| OkxError::InvalidField {
                    row,
                    field,
                    value: fields.get(index).map_or_else(String::new, Value::to_string),
                })
        };
        let invalid = |field: &'static str, value: &str| OkxError::InvalidField {
            row,
            field,
            value: value.to_string(),
        };

        let ts_text = text(0, "ts")?;
        let ts = ts_text.parse::<Ts>().map_err(|_| invalid("ts", ts_text))?;
        if !timeframe.is_aligned(ts) {
            return Err(OkxError::MisalignedTs { row, ts, timeframe });
        }
        if ts >= after {
            return Err(OkxError::NotBeforeCursor { after, ts });
        }
        if let Some(previous) = previous.filter(|&previous| ts >= previous) {
            return Err(OkxError::NotDescending { row, previous, ts });
        }
        previous = Some(ts);
        page.oldest_ts = Some(ts);

        match text(8, "confirm")? {
            "1" => {}
            "0" => {
                page.rejected_unclosed.push(ts);
                continue;
            }
            other => return Err(invalid("confirm", other)),
        }

        let number = |index: usize, field: &'static str| -> Result<f64, OkxError> {
            let value = text(index, field)?;
            value.parse::<f64>().map_err(|_| invalid(field, value))
        };
        let candle = Candle::new(
            ts,
            number(1, "open")?,
            number(2, "high")?,
            number(3, "low")?,
            number(4, "close")?,
            number(5, "vol")?,
            None,
        )
        .map_err(|source| OkxError::InvalidCandle { row, source })?;
        page.candles.push(candle);
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 2024-01-01T00:00:00Z en millisecondes.
    const T0: Ts = 1_704_067_200_000;

    fn row(ts: Ts, confirm: &str) -> Value {
        json!([
            ts.to_string(),
            "100",
            "101",
            "99",
            "100.5",
            "12.5",
            "1",
            "1250",
            confirm
        ])
    }

    fn parse(rows: &[Value]) -> Result<OkxPage, OkxError> {
        parse_page(rows, Timeframe::M1, T0 + 10 * 60_000)
    }

    #[test]
    fn barres_okx_des_timeframes() {
        assert_eq!(okx_bar(Timeframe::M1).ok(), Some("1m"));
        assert_eq!(okx_bar(Timeframe::H4).ok(), Some("4H"));
        assert_eq!(okx_bar(Timeframe::D1).ok(), Some("1Dutc"));
        assert!(matches!(
            okx_bar(Timeframe::H12),
            Err(OkxError::UnsupportedTimeframe { .. })
        ));
    }

    #[test]
    fn page_valide_gardee_dans_l_ordre_recu() {
        let page = parse(&[row(T0 + 120_000, "1"), row(T0 + 60_000, "1"), row(T0, "1")]).unwrap();
        assert_eq!(page.rows, 3);
        assert_eq!(page.oldest_ts, Some(T0));
        let times: Vec<Ts> = page.candles.iter().map(|c| c.open_time_ms).collect();
        assert_eq!(times, [T0 + 120_000, T0 + 60_000, T0]);
        assert_eq!(page.candles[0].volume, 12.5);
        assert_eq!(page.candles[0].trades, None);
    }

    #[test]
    fn confirm_zero_est_exclue_et_comptee_mais_garde_le_curseur() {
        let page = parse(&[row(T0 + 60_000, "0"), row(T0, "1")]).unwrap();
        assert_eq!(page.candles.len(), 1);
        assert_eq!(page.rejected_unclosed, [T0 + 60_000]);
        assert_eq!(page.rows, 2);
        let seule = parse(&[row(T0, "0")]).unwrap();
        assert!(seule.candles.is_empty());
        assert_eq!(seule.oldest_ts, Some(T0));
    }

    #[test]
    fn page_vide_donne_une_page_vide() {
        let page = parse(&[]).unwrap();
        assert_eq!((page.rows, page.oldest_ts), (0, None));
    }

    #[test]
    fn ordre_non_strictement_decroissant_refuse() {
        assert!(matches!(
            parse(&[row(T0, "1"), row(T0 + 60_000, "1")]),
            Err(OkxError::NotDescending { row: 2, .. })
        ));
        assert!(matches!(
            parse(&[row(T0, "1"), row(T0, "1")]),
            Err(OkxError::NotDescending { row: 2, .. })
        ));
    }

    #[test]
    fn ts_non_aligne_refuse() {
        assert!(matches!(
            parse(&[row(T0 + 1, "1")]),
            Err(OkxError::MisalignedTs { row: 1, .. })
        ));
    }

    #[test]
    fn ts_au_dela_de_after_refuse() {
        // `after` exclusif : un ts égal à la borne est déjà une faute.
        assert!(matches!(
            parse_page(&[row(T0, "1")], Timeframe::M1, T0),
            Err(OkxError::NotBeforeCursor { .. })
        ));
    }

    #[test]
    fn nombre_mal_forme_ou_champ_manquant_refuse() {
        let mauvais = json!(["x", "1", "1", "1", "1", "1", "1", "1", "1"]);
        assert!(matches!(
            parse(&[mauvais]),
            Err(OkxError::InvalidField { field: "ts", .. })
        ));
        let prix = json!([T0.to_string(), "abc", "1", "1", "1", "1", "1", "1", "1"]);
        assert!(matches!(
            parse(&[prix]),
            Err(OkxError::InvalidField { field: "open", .. })
        ));
        let nombre_json = json!([T0, "1", "1", "1", "1", "1", "1", "1", "1"]);
        assert!(matches!(
            parse(&[nombre_json]),
            Err(OkxError::InvalidField { field: "ts", .. })
        ));
        assert!(matches!(
            parse(&[json!(["1", "2"])]),
            Err(OkxError::RowShape { row: 1, found: 2 })
        ));
        assert!(matches!(
            parse(&[json!("pas un tableau")]),
            Err(OkxError::RowShape { found: 0, .. })
        ));
        let confirm = json!([T0.to_string(), "1", "1", "1", "1", "1", "1", "1", "2"]);
        assert!(matches!(
            parse(&[confirm]),
            Err(OkxError::InvalidField {
                field: "confirm",
                ..
            })
        ));
    }

    #[test]
    fn bougie_invalide_selon_le_modele_refusee() {
        // high < low.
        let incoherente = json!([T0.to_string(), "100", "98", "99", "100", "1", "1", "1", "1"]);
        assert!(matches!(
            parse(&[incoherente]),
            Err(OkxError::InvalidCandle { row: 1, .. })
        ));
        let nan = json!([T0.to_string(), "NaN", "1", "1", "1", "1", "1", "1", "1"]);
        assert!(matches!(
            parse(&[nan]),
            Err(OkxError::InvalidCandle { row: 1, .. })
        ));
    }

    #[test]
    fn champ_supplementaire_en_fin_de_ligne_ignore() {
        let longue = json!([
            T0.to_string(),
            "100",
            "101",
            "99",
            "100.5",
            "1",
            "1",
            "1",
            "1",
            "x"
        ]);
        assert_eq!(parse(&[longue]).unwrap().candles.len(), 1);
    }
}
