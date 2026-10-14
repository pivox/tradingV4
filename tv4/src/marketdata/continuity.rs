//! Contrôle de continuité d'une série 1m brute (docs/03 §2, §5.1 ; docs/04 §2.5) : classe chaque ligne,
//! trie, dédoublonne, compte les trous, et produit un rapport JSON.
//!
//! # Bloquant : rien n'est écrit pour la paire
//!
//! Erreur de format (y compris une lecture du zip interrompue), valeur non finie, horodatage non aligné
//! sur la minute, OHLC incohérent (prix nul ou négatif, `low > high`, `open` ou `close` hors de
//! `[low, high]`, volume négatif), doublon divergent, aucune bougie gardée. Ces cas ne se « réparent »
//! pas en silence : un dump qui les contient est suspect dans son ensemble, et le rapport les compte
//! tous au lieu de s'arrêter au premier.
//!
//! # Non bloquant : compté dans le rapport
//!
//! Doublon exact (dédoublonné), désordre (trié), bougie non clôturée (exclue après le dédoublonnage :
//! sa fin dépasse la fin de couverture ; chaque minute distincte compte une fois, ses répétitions sont
//! des doublons et une répétition divergente bloque comme pour une minute clôturée), trous, mouvement 1m supérieur à 20 % (marqué et listé, docs/04 §2.5). Aucune bougie
//! n'est jamais synthétisée à l'ingestion (docs/03 §5.1, §5.3) : un trou reste un trou.
//!
//! # Mémoire
//!
//! Une `Candle` occupe 56 octets : XBTUSD 1m de 2013 à 2026 fait au plus 6,8 millions de lignes, soit
//! environ 380 Mo pour le `Vec<Candle>`. La série est triée et dédoublonnée en place.

use serde::Serialize;

use crate::marketdata::kraken::{KrakenError, RawRow};
use crate::model::{Candle, ModelError, Timeframe, Ts};

/// Un mouvement 1m strictement supérieur à ce ratio est marqué (docs/04 §2.5).
pub const LARGE_MOVE_THRESHOLD: f64 = 0.20;

/// Nombre maximal d'exemples bloquants gardés dans le rapport.
pub const MAX_EXAMPLES: usize = 100;

/// Nombre maximal de mouvements listés (tous sont comptés).
pub const MAX_LISTED_MOVES: usize = 1000;

/// Durée d'une bougie 1m en millisecondes.
const M1_MS: i64 = 60_000;

/// Motif de blocage d'une paire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockingKind {
    FormatError,
    NonFinite,
    Misaligned,
    OhlcError,
    DivergentDuplicate,
    Empty,
}

impl BlockingKind {
    /// Nom stable (celui du rapport JSON), pour les messages imprimés.
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockingKind::FormatError => "format_error",
            BlockingKind::NonFinite => "non_finite",
            BlockingKind::Misaligned => "misaligned",
            BlockingKind::OhlcError => "ohlc_error",
            BlockingKind::DivergentDuplicate => "divergent_duplicate",
            BlockingKind::Empty => "empty",
        }
    }
}

/// Exemple d'une faute bloquante.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BlockingExample {
    pub kind: BlockingKind,
    /// Numéro de ligne dans l'entrée (absent pour un doublon divergent, détecté après le tri).
    pub line: Option<u64>,
    pub open_time_ms: Option<Ts>,
    /// Message de l'erreur (`ModelError` ou `KrakenError` rendu en texte).
    pub detail: String,
}

/// Mouvement 1m supérieur au seuil.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LargeMove {
    pub open_time_ms: Ts,
    /// `max(high, close_préc) / min(low, close_préc) - 1`.
    #[serde(rename = "move")]
    pub move_ratio: f64,
}

/// Compteurs du rapport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ContinuityStats {
    pub rows_read: u64,
    /// Bougies valides après tri, dédoublonnage et retrait des non clôturées. Compté même si la série
    /// est ensuite bloquée (la série rendue est alors vide).
    pub rows_kept: u64,
    /// Lignes dont l'horodatage est inférieur à celui de la ligne précédente lue.
    pub out_of_order: u64,
    /// Doublons exacts retirés.
    pub duplicates: u64,
    /// Lignes qui répètent un horodatage avec un contenu différent (bloquant).
    pub divergent_duplicates: u64,
    pub rejected_unclosed: u64,
    /// Valeur non finie (bloquant).
    pub non_finite: u64,
    /// Horodatage non aligné sur la minute (bloquant).
    pub misaligned: u64,
    /// Prix <= 0, OHLC incohérent ou volume < 0 (bloquant).
    pub ohlc_errors: u64,
    pub gaps: u64,
    /// Plus long trou en millisecondes de bougies absentes ; 0 sans trou.
    pub longest_gap_ms: i64,
    pub missing_minutes: u64,
    pub large_moves: u64,
}

/// Rapport de continuité d'une série 1m.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContinuityReport {
    pub stats: ContinuityStats,
    /// Motifs bloquants distincts, triés ; vide si la série est écrivable.
    pub blocking: Vec<BlockingKind>,
    /// Au plus [`MAX_EXAMPLES`], dans l'ordre de détection.
    pub examples: Vec<BlockingExample>,
    pub format_error: Option<String>,
    /// Au plus [`MAX_LISTED_MOVES`], chronologiques.
    pub large_moves: Vec<LargeMove>,
    pub first_open_ms: Option<Ts>,
    pub last_open_ms: Option<Ts>,
    /// Horodatages des doublons exacts retirés, transmis au store pour le bloc `continuity` des
    /// manifestes. Non sérialisés (taille non bornée) : seul le compteur va au rapport.
    #[serde(skip)]
    pub duplicate_times: Vec<Ts>,
    /// Horodatages des bougies non clôturées exclues (même usage).
    #[serde(skip)]
    pub unclosed_times: Vec<Ts>,
}

impl ContinuityReport {
    /// Vrai si la série ne doit pas être écrite.
    pub fn is_blocking(&self) -> bool {
        !self.blocking.is_empty()
    }
}

/// Série contrôlée.
pub struct CheckedSeries {
    /// Bougies 1m valides, triées, sans doublon, clôturées ; VIDE si le rapport est bloquant.
    pub candles: Vec<Candle>,
    pub report: ContinuityReport,
}

/// Numéro de ligne porté par une erreur du lecteur, s'il y en a un.
fn error_line(error: &KrakenError) -> Option<u64> {
    match error {
        KrakenError::Open { .. } => None,
        KrakenError::Csv { line, .. }
        | KrakenError::FieldCount { line, .. }
        | KrakenError::InvalidField { line, .. }
        | KrakenError::TimestampOutOfRange { line, .. }
        | KrakenError::Invalid { line, .. } => Some(*line),
    }
}

/// Lit `rows` jusqu'au bout (ou jusqu'à la première erreur de format) et rend la série contrôlée.
/// `end_ms` : fin de couverture exclusive. Ne panique jamais.
pub fn check_m1<I>(rows: I, end_ms: Ts) -> CheckedSeries
where
    I: Iterator<Item = Result<RawRow, KrakenError>>,
{
    check_m1_with_capacity(rows, end_ms, 0)
}

/// Comme [`check_m1`], avec une capacité initiale pour le `Vec<Candle>` (par exemple la taille
/// décompressée de l'entrée divisée par 40, une ligne Kraken faisant une quarantaine d'octets) : évite
/// les doublements successifs, qui pèsent sur le pic mémoire d'une longue série.
pub fn check_m1_with_capacity<I>(rows: I, end_ms: Ts, capacity: usize) -> CheckedSeries
where
    I: Iterator<Item = Result<RawRow, KrakenError>>,
{
    let mut stats = ContinuityStats::default();
    let mut examples: Vec<BlockingExample> = Vec::new();
    let mut format_error: Option<String> = None;
    let mut duplicate_times: Vec<Ts> = Vec::new();
    let mut unclosed_times: Vec<Ts> = Vec::new();
    // Bornée : une entrée géante ne doit pas réserver d'un coup plus que ce qu'elle contient.
    let mut candles: Vec<Candle> = Vec::with_capacity(capacity.min(16_000_000));

    let mut push_example = |kind, line, open_time_ms, detail: String| {
        if examples.len() < MAX_EXAMPLES {
            examples.push(BlockingExample {
                kind,
                line,
                open_time_ms,
                detail,
            });
        }
    };

    // 1. Une passe de lecture : chaque ligne est classée, la première faute gagne.
    let mut last_read: Option<Ts> = None;
    for item in rows {
        let row = match item {
            Ok(row) => row,
            Err(error) => {
                // Les erreurs d'entrée-sortie du zip (deflate corrompu, CRC faux) arrivent ici,
                // enrobées par `KrakenError::Csv`.
                let detail = error.to_string();
                push_example(
                    BlockingKind::FormatError,
                    error_line(&error),
                    None,
                    detail.clone(),
                );
                format_error = Some(detail);
                break;
            }
        };
        stats.rows_read += 1;
        if last_read.is_some_and(|previous| row.open_time_ms < previous) {
            stats.out_of_order += 1;
        }
        last_read = Some(row.open_time_ms);

        let candle = row.candle();
        match candle.validate() {
            Ok(()) => {}
            Err(error @ ModelError::CandleNotFinite { .. }) => {
                stats.non_finite += 1;
                push_example(
                    BlockingKind::NonFinite,
                    Some(row.line),
                    Some(row.open_time_ms),
                    error.to_string(),
                );
                continue;
            }
            Err(error) => {
                stats.ohlc_errors += 1;
                push_example(
                    BlockingKind::OhlcError,
                    Some(row.line),
                    Some(row.open_time_ms),
                    error.to_string(),
                );
                continue;
            }
        }
        if !Timeframe::M1.is_aligned(row.open_time_ms) {
            stats.misaligned += 1;
            push_example(
                BlockingKind::Misaligned,
                Some(row.line),
                Some(row.open_time_ms),
                ModelError::MisalignedOpenTime {
                    timeframe: Timeframe::M1,
                    open_time_ms: row.open_time_ms,
                }
                .to_string(),
            );
            continue;
        }
        // Les lignes non clôturées restent dans le vecteur jusqu'après le dédoublonnage (étape 3bis) :
        // un doublon d'une minute non clôturée est ainsi compté comme doublon, pas comme autant de
        // minutes non clôturées, et une divergence y est signalée comme ailleurs.
        candles.push(candle);
    }

    // 2. Tri stable : à horodatage égal l'ordre du fichier est conservé, ce qui rend le choix du
    // doublon gardé déterministe.
    candles.sort_by_key(|c| c.open_time_ms);

    // 3. Dédoublonnage en place. Dans un groupe d'horodatages égaux, la première ligne est gardée ; une
    // autre ligne identique est un doublon exact, une ligne différente rend le groupe divergent (toutes
    // ses lignes sont alors retirées, la paire est bloquée).
    let len = candles.len();
    let mut write = 0;
    let mut read = 0;
    while read < len {
        let time = candles[read].open_time_ms;
        let first = candles[read];
        let mut end = read + 1;
        let mut divergent = false;
        while end < len && candles[end].open_time_ms == time {
            if candles[end] == first {
                stats.duplicates += 1;
                duplicate_times.push(time);
            } else {
                stats.divergent_duplicates += 1;
                divergent = true;
            }
            end += 1;
        }
        if divergent {
            push_example(
                BlockingKind::DivergentDuplicate,
                None,
                Some(time),
                "deux lignes différentes pour le même open_time".to_string(),
            );
        } else {
            candles[write] = first;
            write += 1;
        }
        read = end;
    }
    candles.truncate(write);

    // 3bis. Retrait des minutes non clôturées (fin de bougie au-delà de la fin de couverture). La série
    // est triée et sans doublon : elles sont en queue, et chaque minute distincte compte une fois.
    while let Some(last) = candles.last() {
        if last.open_time_ms.saturating_add(M1_MS) <= end_ms {
            break;
        }
        stats.rejected_unclosed += 1;
        unclosed_times.push(last.open_time_ms);
        candles.pop();
    }
    // Ordre croissant, comme les autres listes d'horodatages du rapport.
    unclosed_times.reverse();

    // 4. Bornes, trous et mouvements sur la série gardée.
    stats.rows_kept = candles.len() as u64;
    let first_open_ms = candles.first().map(|c| c.open_time_ms);
    let last_open_ms = candles.last().map(|c| c.open_time_ms);
    let mut large_moves: Vec<LargeMove> = Vec::new();
    let mut previous: Option<&Candle> = None;
    for candle in &candles {
        // Mouvement : vrai range relatif, en tenant compte de la clôture précédente (un gap d'ouverture
        // compte). Pour la première bougie : `high / low - 1`.
        let (haut, bas) = match previous {
            Some(p) => (candle.high.max(p.close), candle.low.min(p.close)),
            None => (candle.high, candle.low),
        };
        let movement = haut / bas - 1.0;
        if movement > LARGE_MOVE_THRESHOLD {
            stats.large_moves += 1;
            if large_moves.len() < MAX_LISTED_MOVES {
                large_moves.push(LargeMove {
                    open_time_ms: candle.open_time_ms,
                    move_ratio: movement,
                });
            }
        }
        if let Some(p) = previous {
            let delta = candle.open_time_ms - p.open_time_ms;
            if delta > M1_MS {
                let absent_ms = delta - M1_MS;
                stats.gaps += 1;
                stats.missing_minutes += (absent_ms / M1_MS) as u64;
                stats.longest_gap_ms = stats.longest_gap_ms.max(absent_ms);
            }
        }
        previous = Some(candle);
    }

    // 5. Verdict.
    let mut blocking: Vec<BlockingKind> = Vec::new();
    for (count, kind) in [
        (u64::from(format_error.is_some()), BlockingKind::FormatError),
        (stats.non_finite, BlockingKind::NonFinite),
        (stats.misaligned, BlockingKind::Misaligned),
        (stats.ohlc_errors, BlockingKind::OhlcError),
        (stats.divergent_duplicates, BlockingKind::DivergentDuplicate),
        (u64::from(stats.rows_kept == 0), BlockingKind::Empty),
    ] {
        if count > 0 {
            blocking.push(kind);
        }
    }
    blocking.sort_unstable();
    blocking.dedup();
    if !blocking.is_empty() {
        // Libère la mémoire avant la paire suivante.
        candles.clear();
        candles.shrink_to_fit();
    }

    CheckedSeries {
        candles,
        report: ContinuityReport {
            stats,
            blocking,
            examples,
            format_error,
            large_moves,
            first_open_ms,
            last_open_ms,
            duplicate_times,
            unclosed_times,
        },
    }
}

// Les séries ci-dessous sont des échantillons de format aux prix inventés autour de 100, comme dans
// `kraken.rs`, pas des données de marché.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketdata::kraken::KrakenRawReader;

    /// 2024-01-01T00:00:00Z en secondes.
    const T0: i64 = 1_704_067_200;
    /// Fin de couverture très lointaine : aucune bougie n'est non clôturée sauf demande contraire.
    const LOIN: Ts = 4_000_000_000_000;

    fn ligne(minute: i64) -> String {
        format!("{},100,101,99,100.5,1,1\n", T0 + 60 * minute)
    }

    fn lignes(minutes: &[i64]) -> String {
        minutes.iter().map(|m| ligne(*m)).collect()
    }

    fn controle(texte: &str, fin: Ts) -> CheckedSeries {
        check_m1(KrakenRawReader::new(texte.as_bytes()), fin)
    }

    #[test]
    fn serie_saine_aucun_blocage() {
        let serie = controle(&lignes(&[0, 1, 2, 3]), LOIN);
        assert!(!serie.report.is_blocking());
        assert_eq!(serie.candles.len(), 4);
        let s = serie.report.stats;
        assert_eq!((s.rows_read, s.rows_kept), (4, 4));
        assert_eq!((s.gaps, s.missing_minutes, s.longest_gap_ms), (0, 0, 0));
        assert_eq!(s.duplicates + s.out_of_order + s.rejected_unclosed, 0);
        assert_eq!(serie.report.first_open_ms, Some(T0 * 1000));
        assert_eq!(serie.report.last_open_ms, Some((T0 + 180) * 1000));
        assert!(serie.report.examples.is_empty());
    }

    #[test]
    fn doublon_exact_dedoublonne_et_compte() {
        let serie = controle(&lignes(&[0, 1, 1, 2]), LOIN);
        assert!(!serie.report.is_blocking());
        assert_eq!(serie.candles.len(), 3);
        assert_eq!(serie.report.stats.duplicates, 1);
        assert_eq!(serie.report.duplicate_times, [(T0 + 60) * 1000]);
        assert_eq!(serie.report.stats.rows_read, 4);
    }

    #[test]
    fn doublon_divergent_bloquant() {
        let texte = format!(
            "{}{}{}",
            ligne(0),
            ligne(1),
            "1704067260,100,102,99,100.5,1,1\n"
        );
        let serie = controle(&texte, LOIN);
        assert_eq!(serie.report.blocking, [BlockingKind::DivergentDuplicate]);
        assert_eq!(serie.report.stats.divergent_duplicates, 1);
        assert!(serie.candles.is_empty());
        let exemple = &serie.report.examples[0];
        assert_eq!(exemple.kind, BlockingKind::DivergentDuplicate);
        assert_eq!(exemple.line, None);
        assert_eq!(exemple.open_time_ms, Some((T0 + 60) * 1000));
    }

    #[test]
    fn desordre_trie_et_compte() {
        let serie = controle(&lignes(&[2, 0, 1, 3]), LOIN);
        assert!(!serie.report.is_blocking());
        assert_eq!(serie.report.stats.out_of_order, 1);
        let temps: Vec<Ts> = serie.candles.iter().map(|c| c.open_time_ms).collect();
        assert_eq!(
            temps,
            [
                T0 * 1000,
                (T0 + 60) * 1000,
                (T0 + 120) * 1000,
                (T0 + 180) * 1000
            ]
        );
    }

    #[test]
    fn ohlc_incoherent_bloquant() {
        for fautif in [
            "1704067200,100,99,101,100,1,1", // low > high
            "1704067200,102,101,99,100,1,1", // open > high
            "1704067200,100,101,99,98,1,1",  // close < low
        ] {
            let serie = controle(&format!("{fautif}\n{}", ligne(1)), LOIN);
            assert_eq!(serie.report.blocking, [BlockingKind::OhlcError], "{fautif}");
            assert_eq!(serie.report.stats.ohlc_errors, 1);
            assert!(serie.candles.is_empty());
            assert_eq!(serie.report.examples[0].line, Some(1));
        }
    }

    #[test]
    fn prix_nul_ou_negatif_bloquant() {
        for fautif in ["1704067200,0,101,0,100,1,1", "1704067200,-1,101,-2,100,1,1"] {
            let serie = controle(&format!("{fautif}\n"), LOIN);
            assert!(
                serie.report.blocking.contains(&BlockingKind::OhlcError),
                "{fautif}"
            );
            assert_eq!(serie.report.stats.ohlc_errors, 1);
        }
    }

    #[test]
    fn volume_negatif_bloquant() {
        let serie = controle("1704067200,100,101,99,100,-1,1\n", LOIN);
        assert!(serie.report.blocking.contains(&BlockingKind::OhlcError));
        assert_eq!(serie.report.stats.ohlc_errors, 1);
    }

    #[test]
    fn nan_et_infini_bloquants() {
        for fautif in [
            "1704067200,nan,101,99,100,1,1",
            "1704067200,100,inf,99,100,1,1",
        ] {
            let serie = controle(&format!("{fautif}\n{}", ligne(1)), LOIN);
            assert_eq!(serie.report.blocking, [BlockingKind::NonFinite], "{fautif}");
            assert_eq!(serie.report.stats.non_finite, 1);
            assert!(serie.candles.is_empty());
        }
    }

    #[test]
    fn horodatage_non_aligne_bloquant() {
        let serie = controle(
            &format!("1704067230,100,101,99,100,1,1\n{}", ligne(1)),
            LOIN,
        );
        assert_eq!(serie.report.blocking, [BlockingKind::Misaligned]);
        assert_eq!(serie.report.stats.misaligned, 1);
        assert!(serie.candles.is_empty());
    }

    #[test]
    fn erreur_de_format_bloquante_et_arrete_la_lecture() {
        let texte = format!(
            "{}{}1704067320,100,101,99,100,x,1\n{}",
            ligne(0),
            ligne(1),
            ligne(3)
        );
        let serie = controle(&texte, LOIN);
        assert!(serie.report.blocking.contains(&BlockingKind::FormatError));
        assert!(
            serie
                .report
                .format_error
                .as_deref()
                .unwrap()
                .contains("ligne 3")
        );
        // La lecture s'est arrêtée à la ligne 3 : la ligne 4 n'est pas lue.
        assert_eq!(serie.report.stats.rows_read, 2);
        assert!(serie.candles.is_empty());
        assert_eq!(serie.report.examples[0].line, Some(3));
    }

    #[test]
    fn bougie_non_cloturee_exclue_et_comptee() {
        // Bougie 0 : se termine à T0 + 60 s. Fin de couverture exactement à cet instant : gardée.
        let fin = (T0 + 60) * 1000;
        let serie = controle(&lignes(&[0, 1, 2]), fin);
        // 0 est gardée (open + 60 000 == fin), 1 et 2 sont exclues.
        assert_eq!(serie.candles.len(), 1);
        assert_eq!(serie.report.stats.rejected_unclosed, 2);
        assert_eq!(
            serie.report.unclosed_times,
            [(T0 + 60) * 1000, (T0 + 120) * 1000]
        );
        assert!(!serie.report.is_blocking());
        // Une milliseconde de moins et la bougie 0 n'est plus clôturée : plus rien de gardé.
        let serie = controle(&lignes(&[0]), fin - 1);
        assert_eq!(serie.report.stats.rejected_unclosed, 1);
        assert_eq!(serie.report.blocking, [BlockingKind::Empty]);
    }

    #[test]
    fn doublons_d_une_minute_non_cloturee_comptes_une_seule_fois() {
        // Revue S5-R2. Fin de couverture : la minute 0 est clôturée, la minute 1 ne l'est pas. La minute 1
        // est répétée trois fois, deux fois à l'identique : une minute non clôturée, deux doublons
        // exacts, aucun blocage.
        let fin = (T0 + 60) * 1000;
        let texte = format!("{}{}{}{}", ligne(0), ligne(1), ligne(1), ligne(1));
        let serie = controle(&texte, fin);
        let s = serie.report.stats;
        assert_eq!((s.rows_read, s.rows_kept), (4, 1));
        assert_eq!((s.rejected_unclosed, s.duplicates), (1, 2));
        assert_eq!(s.divergent_duplicates, 0);
        assert_eq!(serie.report.unclosed_times, [(T0 + 60) * 1000]);
        assert_eq!(serie.report.duplicate_times, [(T0 + 60) * 1000; 2]);
        assert!(!serie.report.is_blocking());

        // Une des répétitions diffère : le doublon divergent est signalé et bloque, comme pour une minute
        // clôturée, au lieu de passer inaperçu parmi les non clôturées.
        let divergente = format!("{},100,102,99,100.5,1,1\n", T0 + 60);
        let texte = format!("{}{}{}{}", ligne(0), ligne(1), ligne(1), divergente);
        let serie = controle(&texte, fin);
        assert_eq!(serie.report.stats.divergent_duplicates, 1);
        assert_eq!(serie.report.stats.rejected_unclosed, 0);
        assert_eq!(serie.report.blocking, [BlockingKind::DivergentDuplicate]);
    }

    #[test]
    fn trous_comptes_avec_le_plus_long() {
        // Minutes présentes : 0, 1, 4 (trou de 2 minutes), 5, 15 (trou de 9 minutes).
        let serie = controle(&lignes(&[0, 1, 4, 5, 15]), LOIN);
        let s = serie.report.stats;
        assert_eq!(s.gaps, 2);
        assert_eq!(s.missing_minutes, 2 + 9);
        assert_eq!(s.longest_gap_ms, 9 * 60_000);
        assert!(!serie.report.is_blocking());
        assert_eq!(serie.candles.len(), 5);
    }

    #[test]
    fn mouvement_superieur_a_20_pourcent_marque_non_bloquant() {
        // Clôture précédente 100, puis high 121 : +21 % (marqué). Puis high 120 depuis la clôture 100 :
        // exactement 20 %, non marqué.
        let texte = format!(
            "{T0},100,100,100,100,1,1\n{},100,121,100,100,1,1\n{},100,120,100,100,1,1\n",
            T0 + 60,
            T0 + 120
        );
        let serie = controle(&texte, LOIN);
        assert!(!serie.report.is_blocking());
        assert_eq!(serie.report.stats.large_moves, 1);
        assert_eq!(serie.report.large_moves.len(), 1);
        assert_eq!(serie.report.large_moves[0].open_time_ms, (T0 + 60) * 1000);
        assert!(serie.report.large_moves[0].move_ratio > 0.20);
        assert_eq!(serie.candles.len(), 3);
        // Un gap d'ouverture compte aussi : clôture 100 puis bougie entière autour de 130.
        let texte = format!(
            "{T0},100,100,100,100,1,1\n{},130,130,130,130,1,1\n",
            T0 + 60
        );
        assert_eq!(controle(&texte, LOIN).report.stats.large_moves, 1);
    }

    #[test]
    fn exemples_limites_a_100() {
        let texte: String = (0..150)
            .map(|m| format!("{},100,99,101,100,1,1\n", T0 + 60 * m))
            .collect();
        let serie = controle(&texte, LOIN);
        assert_eq!(serie.report.stats.ohlc_errors, 150);
        assert_eq!(serie.report.examples.len(), MAX_EXAMPLES);
    }

    #[test]
    fn aucune_bougie_gardee_bloquant() {
        let serie = controle("", LOIN);
        assert_eq!(serie.report.blocking, [BlockingKind::Empty]);
        assert_eq!(serie.report.first_open_ms, None);
        assert!(serie.candles.is_empty());
    }

    #[test]
    fn serie_bloquante_rendue_vide() {
        let texte = format!(
            "{}{}",
            lignes(&[0, 1, 2]),
            "1704067230,100,101,99,100,1,1\n"
        );
        let serie = controle(&texte, LOIN);
        assert!(serie.report.is_blocking());
        assert!(serie.candles.is_empty());
        assert_eq!(serie.candles.capacity(), 0);
        // Les bougies valides ont été comptées avant le verdict.
        assert_eq!(serie.report.stats.rows_kept, 3);
    }

    #[test]
    fn le_rapport_se_serialise_sans_les_listes_d_horodatages() {
        let serie = controle(&lignes(&[0, 1, 1]), LOIN);
        let json = serde_json::to_value(&serie.report).unwrap();
        assert!(json.get("duplicate_times").is_none());
        assert!(json.get("unclosed_times").is_none());
        assert_eq!(json["stats"]["duplicates"], 1);
        assert_eq!(json["blocking"], serde_json::json!([]));
    }
}
