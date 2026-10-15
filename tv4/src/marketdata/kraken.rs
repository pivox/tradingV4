//! Lecteur en flux des dumps OHLCVT de Kraken (docs/03-architecture.md §4, S2).
//!
//! Format d'une ligne, sept champs séparés par une virgule, sans en-tête, fins de ligne `\n` ou `\r\n` :
//!
//! ```text
//! timestamp,open,high,low,close,volume,trades
//! ```
//!
//! | Index | Champ | Type lu | Règle |
//! |---|---|---|---|
//! | 0 | `timestamp` | `i64` | Secondes Unix UTC, début de la bougie. Entier exigé. |
//! | 1 à 4 | `open`, `high`, `low`, `close` | `f64` | Prix en devise de cotation. |
//! | 5 | `volume` | `f64` | En devise de base. |
//! | 6 | `trades` | `u32` | Obligatoire : vide, négatif ou décimal est refusé. |
//!
//! Le parsing est strict : pas de `trim` (un espace rend le champ invalide), délimiteur `,`, pas de
//! commentaires. Le crate `csv` saute les lignes vides en silence ; on ne cherche pas à les détecter.
//!
//! # Ce qui est refusé ici, ce qui est laissé à S5
//!
//! Le lecteur refuse : un nombre de champs différent de 7, un champ non numérique, un horodatage
//! décimal, négatif ou hors secondes (≥ 2100, typiquement des millisecondes), un `open_time` non aligné
//! sur le timeframe passé au lecteur, et toute violation de [`Candle::validate`].
//!
//! Il ne contrôle pas l'ordre, les doublons ni les trous : les lignes passent dans l'ordre du fichier,
//! et le contrôle de continuité (`continuity.rs`, S5) en fait un rapport bloquant. Aucune bougie n'est
//! jamais synthétisée.
//!
//! Deux lecteurs partagent le même décodage : [`KrakenCsvReader`] (validant, une [`Candle`] par ligne,
//! utilisé par `ingest csv`, la fixture et les tests) et [`KrakenRawReader`] ([`RawRow`] sans contrôle de
//! valeur, utilisé par `ingest kraken` : `continuity` compte alors toutes les fautes au lieu de s'arrêter
//! à la première).
//!
//! # Numéro de ligne
//!
//! Les erreurs portent `line = position().record() + 1`, soit le rang de l'enregistrement à partir de 1.
//! Il égale le numéro de ligne physique tant que le fichier n'a ni ligne vide ni champ entre guillemets
//! sur plusieurs lignes, ce qui est le cas des dumps Kraken. On n'utilise volontairement pas
//! `Position::line()` : sur csv 1.4.0, il est faux après une ligne vide (`"a\n\nb"` place `b` à la
//! ligne 2) et décalé d'une ligne sur un fichier CRLF.

use std::fs::File;
use std::io::Read;
use std::iter::FusedIterator;
use std::path::Path;

use csv::StringRecord;
use thiserror::Error;

use crate::model::{Candle, ModelError, Timeframe, Ts};

/// Nombre de champs d'une ligne OHLCVT Kraken.
pub const FIELD_COUNT: usize = 7;

/// Borne haute exclusive des horodatages en secondes : 2100-01-01T00:00:00Z.
///
/// Un horodatage déjà en millisecondes (≈ 1,7e12) dépasse cette borne et est refusé.
pub const MAX_TIMESTAMP_S: i64 = 4_102_444_800;

/// Erreurs du lecteur Kraken. Chaque erreur de contenu porte le numéro de ligne (voir le module).
#[derive(Debug, Error)]
pub enum KrakenError {
    /// Le fichier n'a pas pu être ouvert.
    #[error("ouverture de `{path}` impossible : {source}")]
    Open {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Erreur du crate `csv` : UTF-8 invalide ou erreur d'entrée-sortie en cours de lecture.
    #[error("ligne {line} : lecture CSV impossible : {source}")]
    Csv {
        line: u64,
        #[source]
        source: csv::Error,
    },

    /// La ligne n'a pas sept champs.
    #[error(
        "ligne {line} : {found} champs au lieu de 7 (timestamp, open, high, low, close, volume, trades)"
    )]
    FieldCount { line: u64, found: usize },

    /// Un champ n'est pas du type attendu.
    #[error("ligne {line} : champ `{field}` invalide : `{value}`")]
    InvalidField {
        line: u64,
        field: &'static str,
        value: String,
    },

    /// L'horodatage n'est pas dans `[0, MAX_TIMESTAMP_S)`.
    #[error(
        "ligne {line} : horodatage {timestamp_s} s hors de [0, {max}) : des secondes Unix sont attendues, pas des millisecondes"
    )]
    TimestampOutOfRange {
        line: u64,
        timestamp_s: i64,
        max: i64,
    },

    /// La bougie est refusée par le modèle (valeurs non finies, OHLC incohérent, alignement...).
    #[error("ligne {line} : {source}")]
    Invalid {
        line: u64,
        #[source]
        source: ModelError,
    },
}

/// Ligne OHLCVT décodée mais non validée : seules les erreurs de format sont rendues par le lecteur
/// brut. Les contrôles de valeur (fini, positif, OHLC, alignement, volume) sont faits par
/// `continuity`, qui les compte tous au lieu de s'arrêter au premier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawRow {
    /// Rang de l'enregistrement à partir de 1 (voir la doc du module).
    pub line: u64,
    /// `timestamp × 1000`, contrôlé dans `[0, MAX_TIMESTAMP_S)` secondes.
    pub open_time_ms: Ts,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub trades: u32,
}

impl RawRow {
    /// La bougie correspondante, sans validation.
    pub fn candle(&self) -> Candle {
        Candle {
            open_time_ms: self.open_time_ms,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            trades: Some(self.trades),
        }
    }
}

/// Lecteur en flux de lignes brutes ([`RawRow`]), sans allocation par ligne (l'enregistrement est
/// réutilisé). Épuisé à la première erreur de format, comme [`KrakenCsvReader`] qui s'appuie dessus.
///
/// Les chaînes `nan`, `NaN` et `inf` se parsent en `f64` : elles passent ici et sont classées « non
/// fini » par `continuity`.
pub struct KrakenRawReader<R: Read> {
    csv: csv::Reader<R>,
    record: StringRecord,
    records_read: u64,
    finished: bool,
}

impl<R: Read> KrakenRawReader<R> {
    /// Lecteur brut sur `source`.
    ///
    /// Pas de `BufReader` : `csv::Reader` a son propre tampon. Le nombre de champs est contrôlé par
    /// [`parse_raw`] (`flexible(true)`) pour rendre une erreur [`KrakenError::FieldCount`] lisible.
    pub fn new(source: R) -> Self {
        let csv = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(source);
        KrakenRawReader {
            csv,
            record: StringRecord::new(),
            records_read: 0,
            finished: false,
        }
    }
}

impl<R: Read> Iterator for KrakenRawReader<R> {
    type Item = Result<RawRow, KrakenError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.csv.read_record(&mut self.record) {
            Ok(false) => {
                self.finished = true;
                None
            }
            Err(source) => {
                self.finished = true;
                let line = source
                    .position()
                    .map_or(self.records_read + 1, |p| p.record() + 1);
                Some(Err(KrakenError::Csv { line, source }))
            }
            Ok(true) => {
                self.records_read += 1;
                // Le rang de l'enregistrement lu, à partir de 1 (voir la doc du module).
                let line = self
                    .record
                    .position()
                    .map_or(self.records_read, |p| p.record() + 1);
                let result = parse_raw(&self.record, line);
                if result.is_err() {
                    self.finished = true;
                }
                Some(result)
            }
        }
    }
}

impl<R: Read> FusedIterator for KrakenRawReader<R> {}

/// Lecteur en flux : une [`Candle`] validée par ligne, construite sur [`KrakenRawReader`].
///
/// L'itérateur est épuisé dès la première erreur. Un `ZipFile` (qui implémente [`Read`]) se branche
/// directement sur [`KrakenCsvReader::new`] ; `tv4 ingest kraken` utilise le lecteur brut.
pub struct KrakenCsvReader<R: Read> {
    raw: KrakenRawReader<R>,
    timeframe: Timeframe,
    finished: bool,
}

impl<R: Read> KrakenCsvReader<R> {
    /// Lecteur sur `source`, pour des bougies du `timeframe` donné (contrôle d'alignement).
    pub fn new(source: R, timeframe: Timeframe) -> Self {
        KrakenCsvReader {
            raw: KrakenRawReader::new(source),
            timeframe,
            finished: false,
        }
    }
}

impl KrakenCsvReader<File> {
    /// Ouvre un fichier CSV non compressé.
    pub fn open(path: &Path, timeframe: Timeframe) -> Result<Self, KrakenError> {
        let file = File::open(path).map_err(|source| KrakenError::Open {
            path: path.display().to_string(),
            source,
        })?;
        Ok(KrakenCsvReader::new(file, timeframe))
    }
}

impl<R: Read> Iterator for KrakenCsvReader<R> {
    type Item = Result<Candle, KrakenError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let result = match self.raw.next()? {
            Ok(row) => validate_row(&row, self.timeframe),
            Err(error) => Err(error),
        };
        if result.is_err() {
            self.finished = true;
        }
        Some(result)
    }
}

impl<R: Read> FusedIterator for KrakenCsvReader<R> {}

/// Lit tout `source` et s'arrête à la première erreur. Commodité pour les tests et pour S3.
pub fn read_all<R: Read>(source: R, timeframe: Timeframe) -> Result<Vec<Candle>, KrakenError> {
    KrakenCsvReader::new(source, timeframe).collect()
}

/// Lit un champ `f64` ; l'allocation de la valeur n'a lieu que sur le chemin d'erreur.
fn parse_f64(text: &str, line: u64, field: &'static str) -> Result<f64, KrakenError> {
    text.parse::<f64>().map_err(|_| KrakenError::InvalidField {
        line,
        field,
        value: text.to_string(),
    })
}

/// Décode une ligne : nombre de champs, types, plage de l'horodatage. Aucun contrôle de valeur.
fn parse_raw(record: &StringRecord, line: u64) -> Result<RawRow, KrakenError> {
    if record.len() != FIELD_COUNT {
        return Err(KrakenError::FieldCount {
            line,
            found: record.len(),
        });
    }
    // `get` ne peut pas échouer après le contrôle de longueur ; on évite quand même tout `unwrap`.
    let field = |index: usize| record.get(index).unwrap_or("");

    let timestamp_text = field(0);
    let timestamp_s = timestamp_text
        .parse::<i64>()
        .map_err(|_| KrakenError::InvalidField {
            line,
            field: "timestamp",
            value: timestamp_text.to_string(),
        })?;
    let open = parse_f64(field(1), line, "open")?;
    let high = parse_f64(field(2), line, "high")?;
    let low = parse_f64(field(3), line, "low")?;
    let close = parse_f64(field(4), line, "close")?;
    let volume = parse_f64(field(5), line, "volume")?;
    let trades_text = field(6);
    let trades = trades_text
        .parse::<u32>()
        .map_err(|_| KrakenError::InvalidField {
            line,
            field: "trades",
            value: trades_text.to_string(),
        })?;

    let out_of_range = KrakenError::TimestampOutOfRange {
        line,
        timestamp_s,
        max: MAX_TIMESTAMP_S,
    };
    if !(0..MAX_TIMESTAMP_S).contains(&timestamp_s) {
        return Err(out_of_range);
    }
    // Impossible après le contrôle de plage, mais on ne passe pas par `unwrap`.
    let open_time_ms = timestamp_s.checked_mul(1000).ok_or(out_of_range)?;
    Ok(RawRow {
        line,
        open_time_ms,
        open,
        high,
        low,
        close,
        volume,
        trades,
    })
}

/// Contrôles de valeur du lecteur validant : alignement sur le timeframe, puis [`Candle::validate`].
fn validate_row(row: &RawRow, timeframe: Timeframe) -> Result<Candle, KrakenError> {
    let line = row.line;
    timeframe
        .check_alignment(row.open_time_ms)
        .map_err(|source| KrakenError::Invalid { line, source })?;
    Candle::new(
        row.open_time_ms,
        row.open,
        row.high,
        row.low,
        row.close,
        row.volume,
        Some(row.trades),
    )
    .map_err(|source| KrakenError::Invalid { line, source })
}

// Les échantillons ci-dessous sont des échantillons de format, valeurs inventées (prix ronds autour de
// 100), pas des données de marché.
#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-01-01T00:00:00Z en secondes.
    const T0: i64 = 1_704_067_200;

    fn read(text: &str, tf: Timeframe) -> Result<Vec<Candle>, KrakenError> {
        read_all(text.as_bytes(), tf)
    }

    fn line_of(error: &KrakenError) -> u64 {
        match error {
            KrakenError::Csv { line, .. }
            | KrakenError::FieldCount { line, .. }
            | KrakenError::InvalidField { line, .. }
            | KrakenError::TimestampOutOfRange { line, .. }
            | KrakenError::Invalid { line, .. } => *line,
            KrakenError::Open { .. } => 0,
        }
    }

    fn sample() -> String {
        format!(
            "{T0},100.0,101.0,99.0,100.5,12.5,10\n{},100.5,102.0,100.0,101.0,8.25,7\n{},101.0,101.5,100.5,100.75,0,0\n",
            T0 + 60,
            T0 + 120
        )
    }

    #[test]
    fn trois_lignes_valides_donnent_trois_bougies() {
        let candles = read(&sample(), Timeframe::M1).unwrap();
        assert_eq!(candles.len(), 3);
        assert_eq!(
            candles[0],
            Candle {
                open_time_ms: T0 * 1000,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume: 12.5,
                trades: Some(10),
            }
        );
        assert_eq!(candles[1].open_time_ms, (T0 + 60) * 1000);
        assert_eq!(candles[1].volume, 8.25);
        assert_eq!(candles[1].trades, Some(7));
        assert_eq!(candles[2].volume, 0.0);
        assert_eq!(candles[2].trades, Some(0));
    }

    #[test]
    fn les_fins_de_ligne_crlf_donnent_le_meme_resultat() {
        let crlf = sample().replace('\n', "\r\n");
        assert_eq!(
            read(&crlf, Timeframe::M1).unwrap(),
            read(&sample(), Timeframe::M1).unwrap()
        );
    }

    #[test]
    fn un_fichier_vide_donne_une_liste_vide() {
        assert!(read("", Timeframe::M1).unwrap().is_empty());
    }

    #[test]
    fn mauvais_nombre_de_champs() {
        let text = format!("{T0},100,101,99,100,1,1\n{},100,101,99,100,1\n", T0 + 60);
        let error = read(&text, Timeframe::M1).unwrap_err();
        assert!(
            matches!(error, KrakenError::FieldCount { line: 2, found: 6 }),
            "{error:?}"
        );
        let text = format!(
            "{T0},100,101,99,100,1,1\n{},100,101,99,100,1,1,9\n",
            T0 + 60
        );
        let error = read(&text, Timeframe::M1).unwrap_err();
        assert!(
            matches!(error, KrakenError::FieldCount { line: 2, found: 8 }),
            "{error:?}"
        );
    }

    #[test]
    fn champ_non_numerique_ou_avec_espace() {
        let error = read(&format!("{T0},abc,101,99,100,1,1\n"), Timeframe::M1).unwrap_err();
        assert!(
            matches!(&error, KrakenError::InvalidField { field: "open", value, .. } if value == "abc"),
            "{error:?}"
        );
        let error = read(&format!("{T0}, 100.0,101,99,100,1,1\n"), Timeframe::M1).unwrap_err();
        assert!(
            matches!(error, KrakenError::InvalidField { field: "open", .. }),
            "{error:?}"
        );
    }

    #[test]
    fn horodatage_decimal_refuse() {
        let error = read("1704067200.5,100,101,99,100,1,1\n", Timeframe::M1).unwrap_err();
        assert!(
            matches!(
                error,
                KrakenError::InvalidField {
                    field: "timestamp",
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn horodatage_hors_plage() {
        let error = read("1704067200000,100,101,99,100,1,1\n", Timeframe::M1).unwrap_err();
        assert!(
            matches!(error, KrakenError::TimestampOutOfRange { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("millisecondes"), "{error}");
        let error = read("-60,100,101,99,100,1,1\n", Timeframe::M1).unwrap_err();
        assert!(matches!(error, KrakenError::TimestampOutOfRange { .. }));
        // Bornes de la plage : 4102444799 n'est aligné sur aucun timeframe (pas un multiple de 60 s), donc
        // la borne se vérifie par le type d'erreur : plage acceptée (alignement refusé), puis plage refusée.
        let below = read("4102444799,100,101,99,100,1,1\n", Timeframe::M1).unwrap_err();
        assert!(
            matches!(below, KrakenError::Invalid { .. }),
            "plage acceptée, alignement refusé : {below:?}"
        );
        let at = read("4102444800,100,101,99,100,1,1\n", Timeframe::M1).unwrap_err();
        assert!(
            matches!(at, KrakenError::TimestampOutOfRange { .. }),
            "{at:?}"
        );
        // Une borne alignée : le dernier multiple de 60 sous la borne est accepté.
        let last = MAX_TIMESTAMP_S - 60;
        assert!(read(&format!("{last},100,101,99,100,1,1\n"), Timeframe::M1).is_ok());
    }

    #[test]
    fn alignement_sur_le_timeframe_du_lecteur() {
        let error = read(&format!("{},100,101,99,100,1,1\n", T0 + 30), Timeframe::M1).unwrap_err();
        assert!(
            matches!(
                error,
                KrakenError::Invalid {
                    source: ModelError::MisalignedOpenTime { .. },
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(
            read(
                &format!("{},100,101,99,100,1,1\n", T0 + 3600),
                Timeframe::H1
            )
            .is_ok()
        );
        assert!(read(&format!("{},100,101,99,100,1,1\n", T0 + 60), Timeframe::H1).is_err());
    }

    #[test]
    fn bougie_refusee_par_le_modele() {
        let one = |fields: &str| read(&format!("{T0},{fields}\n"), Timeframe::M1).unwrap_err();
        let source = |error: KrakenError| match error {
            KrakenError::Invalid { source, .. } => source,
            autre => panic!("Invalid attendu : {autre:?}"),
        };
        assert!(matches!(
            source(one("100,99,101,100,1,1")),
            ModelError::InconsistentOhlc { .. }
        ));
        assert!(matches!(
            source(one("100,101,99,nan,1,1")),
            ModelError::CandleNotFinite { .. }
        ));
        assert!(matches!(
            source(one("100,inf,99,100,1,1")),
            ModelError::CandleNotFinite { .. }
        ));
        assert!(matches!(
            source(one("100,101,99,100,-1,1")),
            ModelError::NegativeVolume { .. }
        ));
        assert!(matches!(
            source(one("0,101,0,100,1,1")),
            ModelError::NonPositivePrice { .. }
        ));
    }

    #[test]
    fn trades_invalide() {
        for value in ["-1", "", "1.5", "4294967296"] {
            let error =
                read(&format!("{T0},100,101,99,100,1,{value}\n"), Timeframe::M1).unwrap_err();
            assert!(
                matches!(
                    error,
                    KrakenError::InvalidField {
                        field: "trades",
                        ..
                    }
                ),
                "trades = `{value}` : {error:?}"
            );
        }
    }

    #[test]
    fn une_ligne_d_en_tete_est_refusee() {
        // Prouve `has_headers(false)` : l'en-tête serait sinon avalé en silence.
        let text =
            format!("timestamp,open,high,low,close,volume,trades\n{T0},100,101,99,100,1,1\n");
        let error = read(&text, Timeframe::M1).unwrap_err();
        assert!(
            matches!(
                error,
                KrakenError::InvalidField {
                    line: 1,
                    field: "timestamp",
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn desordre_et_doublons_sont_rendus_tels_quels() {
        // L'ordre et les doublons sont contrôlés par `continuity.rs` (S5), pas par le lecteur.
        let text = format!(
            "{},100,101,99,100,1,1\n{T0},100,101,99,100,1,1\n{T0},100,101,99,100,1,1\n",
            T0 + 60
        );
        let candles = read(&text, Timeframe::M1).unwrap();
        let times: Vec<i64> = candles.iter().map(|c| c.open_time_ms).collect();
        assert_eq!(times, [(T0 + 60) * 1000, T0 * 1000, T0 * 1000]);
    }

    #[test]
    fn le_numero_de_ligne_est_celui_de_l_erreur() {
        let text = format!(
            "{T0},100,101,99,100,1,1\n{},100,101,99,100,1,1\n{},100,101,99,100,x,1\n",
            T0 + 60,
            T0 + 120
        );
        let error = read(&text, Timeframe::M1).unwrap_err();
        assert_eq!(line_of(&error), 3);
        assert!(error.to_string().contains("ligne 3"), "{error}");
        // En CRLF aussi (Position::line() serait décalé).
        let error = read(&text.replace('\n', "\r\n"), Timeframe::M1).unwrap_err();
        assert_eq!(line_of(&error), 3);
    }

    #[test]
    fn l_iterateur_est_epuise_apres_une_erreur() {
        let text = format!("{T0},100,101,99,100,x,1\n{},100,101,99,100,1,1\n", T0 + 60);
        let mut reader = KrakenCsvReader::new(text.as_bytes(), Timeframe::M1);
        assert!(matches!(reader.next(), Some(Err(_))));
        assert!(reader.next().is_none());
        assert!(reader.next().is_none());
    }

    #[test]
    fn utf8_invalide() {
        let mut bytes = format!("{T0},100,101,99,100,1,1\n").into_bytes();
        bytes.extend_from_slice(b"17040\xff7260,100,101,99,100,1,1\n");
        let error = read_all(bytes.as_slice(), Timeframe::M1).unwrap_err();
        assert!(
            matches!(error, KrakenError::Csv { line: 2, .. }),
            "{error:?}"
        );
    }

    #[test]
    fn le_lecteur_brut_laisse_passer_les_valeurs_invalides() {
        let text = format!(
            "{T0},nan,101,99,100,1,1\n{},100,99,101,100,1,1\n{},100,101,99,100,1,1\n{},100,101,99,100,-1,1\n",
            T0 + 60,
            T0 + 90,
            T0 + 120
        );
        let rows: Vec<RawRow> = KrakenRawReader::new(text.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), 4);
        assert!(rows[0].open.is_nan());
        assert_eq!(rows[0].line, 1);
        // low > high, rendu tel quel.
        assert_eq!((rows[1].high, rows[1].low), (99.0, 101.0));
        // Non aligné sur la minute, rendu tel quel.
        assert_eq!(rows[2].open_time_ms, (T0 + 90) * 1000);
        // Volume négatif, rendu tel quel.
        assert_eq!(rows[3].volume, -1.0);
        assert_eq!(rows[3].line, 4);
        assert_eq!(rows[3].candle().trades, Some(1));
        // Le lecteur validant refuse la première de ces lignes.
        assert!(read(&text, Timeframe::M1).is_err());
    }

    #[test]
    fn le_lecteur_brut_s_arrete_a_une_erreur_de_format() {
        let text = format!(
            "{T0},100,101,99,100,1,1\n{},100,101,99,100,x,1\n{},100,101,99,100,1,1\n",
            T0 + 60,
            T0 + 120
        );
        let mut reader = KrakenRawReader::new(text.as_bytes());
        assert!(matches!(reader.next(), Some(Ok(_))));
        assert!(matches!(
            reader.next(),
            Some(Err(KrakenError::InvalidField {
                line: 2,
                field: "volume",
                ..
            }))
        ));
        assert!(reader.next().is_none());
        assert!(reader.next().is_none());
    }
}
