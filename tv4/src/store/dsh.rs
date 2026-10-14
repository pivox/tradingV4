//! Encodeur `dsh-v1` : `content_hash` d'un fichier et `dataset_hash` d'un dataset figé
//! (docs/dataset-hash.md, docs/03 §5.3).
//!
//! Le hash porte sur le contenu logique des bougies, sérialisé dans un flux d'octets possédé par le
//! projet : en-tête texte, puis 53 octets par ligne. Rien n'est dérivé d'un schéma arrow, d'un
//! `Debug` ou des octets Parquet. `docs/dataset-hash.md` fait foi ; le vecteur de test du §7 (233
//! octets, hash en dur) est reproduit par les tests de ce fichier.
//!
//! # Un seul chemin pour les octets
//!
//! [`emit_segment`] produit le flux d'un segment vers un puits (`FnMut(&[u8])`) : [`segment_bytes`]
//! le branche sur un `Vec<u8>`, [`hash_segment`] et [`dataset_hash`] sur un `blake3::Hasher`.
//! Toutes les erreurs sont levées avant le premier octet émis. Une bougie synthétique ne passe jamais
//! ici : le hash se calcule sur les lignes stockées, avant `fill_gaps` (§3.1).

use crate::model::Timeframe;

use super::StoreError;
use super::columns::Columns;
use super::layout::{GapsPolicy, Partition, Period, Window};

/// Valeur de `hash_format` dans les manifestes.
pub const HASH_FORMAT: &str = "dsh-v1";
/// Préfixe de l'en-tête d'un segment.
pub const HEADER_PREFIX: &str = "tradingv4-dataset-v1";
/// Longueur d'une ligne encodée, en octets (§3.2).
pub const ROW_LEN: usize = 53;
/// Préfixe d'un hash dans les manifestes JSON.
const HASH_PREFIX: &str = "blake3:";
/// Bits du NaN canonique (§3.3).
const CANONICAL_NAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// Sélection des lignes par la fenêtre (§3.1) : `Filter` ignore les lignes hors fenêtre (dataset),
/// `Strict` les refuse (`content_hash` d'un fichier, §5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMode {
    Filter,
    Strict,
}

/// En-tête d'un segment, champs bruts (validés par l'encodeur, §2.2).
#[derive(Clone, Copy, Debug)]
pub struct SegmentSpec<'a> {
    pub exchange: &'a str,
    pub symbol: &'a str,
    pub tf: Timeframe,
    pub window: Window,
    pub gaps_policy: GapsPolicy,
}

/// Bits canoniques d'un flottant (§3.3) : tout zéro → 0, tout NaN → `0x7FF8000000000000`, sinon
/// `to_bits()`.
pub fn canonical_f64_bits(x: f64) -> u64 {
    if x.is_nan() {
        CANONICAL_NAN_BITS
    } else if x == 0.0 {
        0
    } else {
        x.to_bits()
    }
}

/// Les 53 octets de la ligne `index` de `columns` (§3.2). [`StoreError::MissingTrades`] si `trades`
/// n'est pas lu, [`StoreError::ColumnLengths`] si `index` est hors bornes.
pub fn encode_row(columns: &Columns, index: usize) -> Result<[u8; ROW_LEN], StoreError> {
    let trades = columns.trades().ok_or(StoreError::MissingTrades)?;
    let hors_bornes = || StoreError::ColumnLengths {
        detail: format!("ligne {index} hors de 0..{}", columns.len()),
    };
    let open_time_ms = *columns.open_time_ms().get(index).ok_or_else(hors_bornes)?;
    let valeurs = [
        columns.open().get(index),
        columns.high().get(index),
        columns.low().get(index),
        columns.close().get(index),
        columns.volume().get(index),
    ];
    let trades = *trades.get(index).ok_or_else(hors_bornes)?;
    let mut out = [0u8; ROW_LEN];
    out[0..8].copy_from_slice(&open_time_ms.to_le_bytes());
    for (k, x) in valeurs.into_iter().enumerate() {
        let x = *x.ok_or_else(hors_bornes)?;
        out[8 + 8 * k..16 + 8 * k].copy_from_slice(&canonical_f64_bits(x).to_le_bytes());
    }
    if let Some(n) = trades {
        out[48] = 1;
        out[49..53].copy_from_slice(&n.to_le_bytes());
    }
    Ok(out)
}

/// `[a-z0-9_]+`
fn is_exchange_field(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `[A-Z0-9]+-[A-Z0-9]+`, un seul `-`.
fn is_symbol_field(text: &str) -> bool {
    let partie = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    text.split_once('-')
        .is_some_and(|(base, quote)| partie(base) && partie(quote))
}

/// Émet le flux d'un segment (en-tête puis lignes) vers `emit` et rend le nombre de lignes.
///
/// Étapes : validation des champs d'en-tête ; sélection par la fenêtre ; tri stable si les lignes ne
/// sont pas déjà strictement croissantes ; dédoublonnage (octets égaux : une seule ; différents :
/// erreur) ; enfin en-tête puis lignes. Tout ce qui précède l'émission est fait avant le premier
/// octet émis.
fn emit_segment(
    spec: &SegmentSpec<'_>,
    columns: &Columns,
    mode: WindowMode,
    mut emit: impl FnMut(&[u8]),
) -> Result<u64, StoreError> {
    if !is_exchange_field(spec.exchange) {
        return Err(StoreError::HeaderField {
            field: "exchange",
            value: spec.exchange.to_string(),
        });
    }
    if !is_symbol_field(spec.symbol) {
        return Err(StoreError::HeaderField {
            field: "symbol",
            value: spec.symbol.to_string(),
        });
    }
    if columns.trades().is_none() {
        return Err(StoreError::MissingTrades);
    }
    let times = columns.open_time_ms();
    let mut indices = Vec::with_capacity(times.len());
    for (index, &ts) in times.iter().enumerate() {
        if spec.window.contains(ts) {
            indices.push(index);
        } else if mode == WindowMode::Strict {
            return Err(StoreError::RowOutsideWindow {
                open_time_ms: ts,
                start_ms: spec.window.start_ms(),
                end_ms: spec.window.end_ms(),
            });
        }
    }
    if indices.windows(2).any(|p| times[p[1]] <= times[p[0]]) {
        indices.sort_by_key(|&i| times[i]);
    }
    let mut keep: Vec<usize> = Vec::with_capacity(indices.len());
    for index in indices {
        if let Some(&last) = keep.last()
            && times[last] == times[index]
        {
            if encode_row(columns, last)? != encode_row(columns, index)? {
                return Err(StoreError::DivergentDuplicate {
                    open_time_ms: times[index],
                });
            }
            continue;
        }
        keep.push(index);
    }
    let row_count = keep.len() as u64;
    let header = format!(
        "{HEADER_PREFIX}|{}|{}|{}|{},{}|{}|{row_count}\n",
        spec.exchange,
        spec.symbol,
        spec.tf,
        spec.window.start_ms(),
        spec.window.end_ms(),
        spec.gaps_policy,
    );
    emit(header.as_bytes());
    for index in keep {
        emit(&encode_row(columns, index)?);
    }
    Ok(row_count)
}

/// Flux complet d'un segment (en-tête puis lignes) : sert aux tests (233 octets du §7).
pub fn segment_bytes(
    spec: &SegmentSpec<'_>,
    columns: &Columns,
    mode: WindowMode,
) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    emit_segment(spec, columns, mode, |chunk| bytes.extend_from_slice(chunk))?;
    Ok(bytes)
}

/// Hash d'un segment unique (hex minuscule, 64 caractères) et nombre de lignes hachées.
pub fn hash_segment(
    spec: &SegmentSpec<'_>,
    columns: &Columns,
    mode: WindowMode,
) -> Result<(String, u64), StoreError> {
    let mut hasher = blake3::Hasher::new();
    let rows = emit_segment(spec, columns, mode, |chunk| {
        hasher.update(chunk);
    })?;
    Ok((hasher.finalize().to_hex().to_string(), rows))
}

/// `content_hash` d'un fichier (§5.1) : segment unique, fenêtre = période du fichier, `gaps_policy` =
/// `skip`, mode `Strict`. Rend le hex (sans préfixe) et le nombre de lignes.
pub fn content_hash(
    partition: &Partition,
    period: Period,
    columns: &Columns,
) -> Result<(String, u64), StoreError> {
    let spec = SegmentSpec {
        exchange: partition.exchange.as_str(),
        symbol: partition.symbol.as_str(),
        tf: partition.tf,
        window: period.window(),
        gaps_policy: GapsPolicy::Skip,
    };
    hash_segment(&spec, columns, WindowMode::Strict)
}

/// `dataset_hash` (§5.2) : un segment par série (symbole, tf), triés par `(symbol, tf.as_str())`
/// octet par octet, mêmes `exchange`, fenêtre et `gaps_policy`, mode `Filter`, un seul hasher pour
/// tout le flux. Rend le hex et la somme des lignes hachées.
///
/// [`StoreError::DuplicateSegment`] si une série revient.
pub fn dataset_hash(
    exchange: &str,
    window: Window,
    gaps_policy: GapsPolicy,
    series: &[(&str, Timeframe, &Columns)],
) -> Result<(String, u64), StoreError> {
    let mut ordre: Vec<&(&str, Timeframe, &Columns)> = series.iter().collect();
    ordre.sort_by(|a, b| (a.0, a.1.as_str()).cmp(&(b.0, b.1.as_str())));
    if let Some(paire) = ordre
        .windows(2)
        .find(|p| p[0].0 == p[1].0 && p[0].1 == p[1].1)
    {
        return Err(StoreError::DuplicateSegment {
            symbol: paire[0].0.to_string(),
            tf: paire[0].1,
        });
    }
    let mut hasher = blake3::Hasher::new();
    let mut total = 0_u64;
    for (symbol, tf, columns) in ordre {
        let spec = SegmentSpec {
            exchange,
            symbol,
            tf: *tf,
            window,
            gaps_policy,
        };
        total += emit_segment(&spec, columns, WindowMode::Filter, |chunk| {
            hasher.update(chunk);
        })?;
    }
    Ok((hasher.finalize().to_hex().to_string(), total))
}

/// `blake3:` suivi du hex, forme écrite dans les manifestes.
pub fn prefixed(hex: &str) -> String {
    format!("{HASH_PREFIX}{hex}")
}

/// Inverse de [`prefixed`] : contrôle `blake3:` suivi de 64 caractères hexadécimaux minuscules
/// ([`StoreError::MalformedHash`] sinon, `path` ne sert qu'au message).
pub fn strip_prefix<'a>(path: &str, value: &'a str) -> Result<&'a str, StoreError> {
    value
        .strip_prefix(HASH_PREFIX)
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| StoreError::MalformedHash {
            path: path.to_string(),
            value: value.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Candle, ExchangeId};

    /// Flux complet du vecteur de docs/dataset-hash.md §7 (233 octets), recopié du document.
    const FLUX_HEX: &str = "74726164696e6776342d646174617365742d76317c6b72616b656e7c4254432d5553447c316d7c313730343036373230303030302c313730343036373338303030307c736b69707c330a00f451c28c0100003333333373a5e4400000000080a7e44000000000a0a2e4403333333343a6e440000000000000f43f011100000060de52c28c0100003333333343a6e44000000000f0a6e4400000000000a5e4406666666626a5e440000000000000e83f0000000000c0c853c28c0100006666666626a5e4406666666626a5e4406666666626a5e4406666666626a5e44000000000000000000100000000";
    const HASH_VECTEUR: &str = "a15f886ff621e7e10b7d4769793d818b23686441a2ce3d4dd6a4918b740cae8e";
    const HASH_CONTENU_MOIS: &str =
        "3da10e85c1fdd2673a5cc9a0ea7dbd2bf5c171e3f11c23e04dff603392bdd8ab";
    const FENETRE_VECTEUR: (i64, i64) = (1_704_067_200_000, 1_704_067_380_000);
    const EN_TETE: &str =
        "tradingv4-dataset-v1|kraken|BTC-USD|1m|1704067200000,1704067380000|skip|3\n";

    fn hex_vers_octets(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    fn octets_vers_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Les trois bougies du §7.
    fn trois_bougies() -> Vec<Candle> {
        vec![
            Candle {
                open_time_ms: 1_704_067_200_000,
                open: 42283.6,
                high: 42300.0,
                low: 42261.0,
                close: 42290.1,
                volume: 1.25,
                trades: Some(17),
            },
            Candle {
                open_time_ms: 1_704_067_260_000,
                open: 42290.1,
                high: 42295.5,
                low: 42280.0,
                close: 42281.2,
                volume: 0.75,
                trades: None,
            },
            Candle {
                open_time_ms: 1_704_067_320_000,
                open: 42281.2,
                high: 42281.2,
                low: 42281.2,
                close: 42281.2,
                volume: -0.0,
                trades: Some(0),
            },
        ]
    }

    fn fenetre() -> Window {
        Window::new(FENETRE_VECTEUR.0, FENETRE_VECTEUR.1).unwrap()
    }

    fn spec(window: Window) -> SegmentSpec<'static> {
        SegmentSpec {
            exchange: "kraken",
            symbol: "BTC-USD",
            tf: Timeframe::M1,
            window,
            gaps_policy: GapsPolicy::Skip,
        }
    }

    fn hash_vecteur(columns: &Columns) -> String {
        hash_segment(&spec(fenetre()), columns, WindowMode::Filter)
            .unwrap()
            .0
    }

    #[test]
    fn vecteur_golden_section_7() {
        let colonnes = Columns::from_candles(&trois_bougies());
        let octets = segment_bytes(&spec(fenetre()), &colonnes, WindowMode::Filter).unwrap();
        assert_eq!(octets.len(), 233);
        assert_eq!(octets_vers_hex(&octets), FLUX_HEX);
        assert_eq!(octets, hex_vers_octets(FLUX_HEX));
        assert_eq!(octets[..74], *EN_TETE.as_bytes());
        assert_eq!(EN_TETE.len(), 74);
        let (hex, lignes) = hash_segment(&spec(fenetre()), &colonnes, WindowMode::Filter).unwrap();
        assert_eq!((hex.as_str(), lignes), (HASH_VECTEUR, 3));
        assert_eq!(blake3::hash(&octets).to_hex().as_str(), HASH_VECTEUR);
    }

    #[test]
    fn hash_incremental_egal_hash_en_un_appel() {
        let colonnes = Columns::from_candles(&trois_bougies());
        let mut hasher = blake3::Hasher::new();
        hasher.update(EN_TETE.as_bytes());
        for i in 0..3 {
            hasher.update(&encode_row(&colonnes, i).unwrap());
        }
        assert_eq!(hasher.finalize().to_hex().as_str(), HASH_VECTEUR);
    }

    #[test]
    fn desordre_et_doublon_exact_meme_hash() {
        let b = trois_bougies();
        let melange = Columns::from_candles(&[b[2], b[0], b[1], b[0]]);
        assert_eq!(hash_vecteur(&melange), HASH_VECTEUR);
        let (_, lignes) = hash_segment(&spec(fenetre()), &melange, WindowMode::Filter).unwrap();
        assert_eq!(lignes, 3);
    }

    #[test]
    fn zero_positif_et_negatif_meme_hash() {
        let mut b = trois_bougies();
        b[2].volume = 0.0;
        assert_eq!(hash_vecteur(&Columns::from_candles(&b)), HASH_VECTEUR);
    }

    #[test]
    fn trades_zero_different_de_trades_absent() {
        let mut b = trois_bougies();
        b[1].trades = Some(0);
        let colonnes = Columns::from_candles(&b);
        assert_ne!(hash_vecteur(&colonnes), HASH_VECTEUR);
        let absent = encode_row(&Columns::from_candles(&trois_bougies()), 1).unwrap();
        let zero = encode_row(&colonnes, 1).unwrap();
        assert_eq!(absent[48..], [0, 0, 0, 0, 0]);
        assert_eq!(zero[48..], [1, 0, 0, 0, 0]);
    }

    #[test]
    fn nan_canonique() {
        let attendu = 0x7FF8_0000_0000_0000_u64;
        for x in [f64::NAN, -f64::NAN, f64::from_bits(0xFFF8_0000_0000_0123)] {
            assert!(x.is_nan());
            assert_eq!(canonical_f64_bits(x), attendu);
        }
        assert_eq!(canonical_f64_bits(-0.0), 0);
        assert_eq!(canonical_f64_bits(0.0), 0);
        assert_eq!(canonical_f64_bits(1.5), 1.5_f64.to_bits());
        assert_eq!(canonical_f64_bits(f64::INFINITY), f64::INFINITY.to_bits());
        let mut b = trois_bougies();
        b[0].open = f64::from_bits(0xFFF8_0000_0000_0123);
        let ligne = encode_row(&Columns::from_candles(&b), 0).unwrap();
        assert_eq!(octets_vers_hex(&ligne[8..16]), "000000000000f87f");
    }

    #[test]
    fn doublon_divergent_refuse() {
        let b = trois_bougies();
        let mut autre = b[1];
        autre.volume = 0.76;
        let colonnes = Columns::from_candles(&[b[0], b[1], autre, b[2]]);
        let erreur = hash_segment(&spec(fenetre()), &colonnes, WindowMode::Filter).unwrap_err();
        assert!(matches!(
            erreur,
            StoreError::DivergentDuplicate {
                open_time_ms: 1_704_067_260_000
            }
        ));
        assert!(erreur.to_string().contains("1704067260000"));
    }

    #[test]
    fn ligne_hors_fenetre() {
        let mut b = trois_bougies();
        b.push(Candle {
            open_time_ms: 1_704_067_380_000,
            ..b[2]
        });
        let colonnes = Columns::from_candles(&b);
        assert!(matches!(
            hash_segment(&spec(fenetre()), &colonnes, WindowMode::Strict),
            Err(StoreError::RowOutsideWindow {
                open_time_ms: 1_704_067_380_000,
                ..
            })
        ));
        let (hex, lignes) = hash_segment(&spec(fenetre()), &colonnes, WindowMode::Filter).unwrap();
        assert_eq!((hex.as_str(), lignes), (HASH_VECTEUR, 3));
    }

    #[test]
    fn champ_d_en_tete_hors_jeu_de_caracteres() {
        let colonnes = Columns::from_candles(&trois_bougies());
        let mut emis = 0_usize;
        for exchange in ["Kraken", "kra|ken", ""] {
            let s = SegmentSpec {
                exchange,
                ..spec(fenetre())
            };
            let r = emit_segment(&s, &colonnes, WindowMode::Filter, |c| emis += c.len());
            assert!(
                matches!(
                    r,
                    Err(StoreError::HeaderField {
                        field: "exchange",
                        ..
                    })
                ),
                "exchange « {exchange} »"
            );
        }
        for symbol in ["BTC|USD", "btc-usd", "BTC-USD\n", "BTCUSD", "BTC-USD-X"] {
            let s = SegmentSpec {
                symbol,
                ..spec(fenetre())
            };
            let r = emit_segment(&s, &colonnes, WindowMode::Filter, |c| emis += c.len());
            assert!(
                matches!(
                    r,
                    Err(StoreError::HeaderField {
                        field: "symbol",
                        ..
                    })
                ),
                "symbole « {symbol:?} »"
            );
        }
        assert_eq!(emis, 0, "aucun octet ne doit être émis avant l'erreur");
    }

    #[test]
    fn colonnes_sans_trades_refusees() {
        let mut parts = Columns::from_candles(&trois_bougies()).into_parts();
        parts.trades = None;
        let sans = Columns::from_parts(parts).unwrap();
        assert!(matches!(
            hash_segment(&spec(fenetre()), &sans, WindowMode::Filter),
            Err(StoreError::MissingTrades)
        ));
        assert!(matches!(
            encode_row(&sans, 0),
            Err(StoreError::MissingTrades)
        ));
    }

    fn partition_golden() -> Partition {
        Partition::new(
            ExchangeId::Kraken,
            "BTC-USD".parse().unwrap(),
            Timeframe::M1,
        )
    }

    #[test]
    fn content_hash_du_mois_golden() {
        let colonnes = Columns::from_candles(&trois_bougies());
        let janvier = Period::Month {
            year: 2024,
            month: 1,
        };
        let (hex, lignes) = content_hash(&partition_golden(), janvier, &colonnes).unwrap();
        assert_eq!((hex.as_str(), lignes), (HASH_CONTENU_MOIS, 3));
    }

    #[test]
    fn content_hash_egal_dataset_hash_mono_mois() {
        let colonnes = Columns::from_candles(&trois_bougies());
        let janvier = Period::Month {
            year: 2024,
            month: 1,
        };
        let (contenu, _) = content_hash(&partition_golden(), janvier, &colonnes).unwrap();
        let (dataset, _) = dataset_hash(
            "kraken",
            janvier.window(),
            GapsPolicy::Skip,
            &[("BTC-USD", Timeframe::M1, &colonnes)],
        )
        .unwrap();
        assert_eq!(contenu, dataset);
    }

    #[test]
    fn gaps_policy_dans_l_en_tete() {
        let colonnes = Columns::from_candles(&trois_bougies());
        let mut hashes = Vec::new();
        for politique in [
            GapsPolicy::Skip,
            GapsPolicy::Ffill,
            GapsPolicy::ZeroVolumeMarked,
        ] {
            let s = SegmentSpec {
                gaps_policy: politique,
                ..spec(fenetre())
            };
            let octets = segment_bytes(&s, &colonnes, WindowMode::Filter).unwrap();
            let fin = octets.iter().position(|b| *b == b'\n').unwrap();
            let en_tete = String::from_utf8(octets[..=fin].to_vec()).unwrap();
            assert!(
                en_tete.contains(&format!("|{}|3\n", politique.as_str())),
                "{en_tete:?}"
            );
            hashes.push(hash_segment(&s, &colonnes, WindowMode::Filter).unwrap().0);
        }
        assert_eq!(hashes[0], HASH_VECTEUR);
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[0], hashes[2]);
        assert_ne!(hashes[1], hashes[2]);
    }

    #[test]
    fn ordre_des_segments() {
        // Cinq séries synthétiques distinctes (une seule bougie chacune, prix inventés).
        let serie = |prix: f64| {
            Columns::from_candles(&[Candle {
                open_time_ms: 0,
                open: prix,
                high: prix,
                low: prix,
                close: prix,
                volume: 1.0,
                trades: None,
            }])
        };
        let jour = 86_400_000;
        let w = Window::new(0, jour * 2).unwrap();
        let (c1, c2, c3, c4, c5) = (serie(1.0), serie(2.0), serie(3.0), serie(4.0), serie(5.0));
        let fournies = [
            ("BTC-USDC", Timeframe::D1, &c1),
            ("BTC-USD", Timeframe::M1, &c2),
            ("BTC-USD", Timeframe::M15, &c3),
            ("BTC-USD", Timeframe::H1, &c4),
            ("BTC-USD", Timeframe::D1, &c5),
        ];
        let mut inverse = fournies;
        inverse.reverse();
        let (h1, n1) = dataset_hash("kraken", w, GapsPolicy::Skip, &fournies).unwrap();
        let (h2, n2) = dataset_hash("kraken", w, GapsPolicy::Skip, &inverse).unwrap();
        assert_eq!((h1.as_str(), n1), (h2.as_str(), n2));
        assert_eq!(n1, 5);
        // Ordre attendu : (BTC-USD,15m), (BTC-USD,1d), (BTC-USD,1h), (BTC-USD,1m), (BTC-USDC,1d).
        let attendu = [
            ("BTC-USD", Timeframe::M15, &c3),
            ("BTC-USD", Timeframe::D1, &c5),
            ("BTC-USD", Timeframe::H1, &c4),
            ("BTC-USD", Timeframe::M1, &c2),
            ("BTC-USDC", Timeframe::D1, &c1),
        ];
        let mut flux = Vec::new();
        for (symbol, tf, columns) in attendu {
            let s = SegmentSpec {
                exchange: "kraken",
                symbol,
                tf,
                window: w,
                gaps_policy: GapsPolicy::Skip,
            };
            flux.extend(segment_bytes(&s, columns, WindowMode::Filter).unwrap());
        }
        assert_eq!(blake3::hash(&flux).to_hex().as_str(), h1);
    }

    #[test]
    fn serie_en_double_refusee() {
        let colonnes = Columns::from_candles(&trois_bougies());
        let r = dataset_hash(
            "kraken",
            fenetre(),
            GapsPolicy::Skip,
            &[
                ("BTC-USD", Timeframe::M1, &colonnes),
                ("BTC-USD", Timeframe::M1, &colonnes),
            ],
        );
        assert!(matches!(r, Err(StoreError::DuplicateSegment { .. })));
    }

    #[test]
    fn prefixe_des_hashes() {
        let pref = prefixed(HASH_VECTEUR);
        assert_eq!(strip_prefix("m", &pref).unwrap(), HASH_VECTEUR);
        for ko in [
            HASH_VECTEUR,
            "blake3:abc",
            &format!("blake3:{}", HASH_VECTEUR.to_uppercase()),
            &format!("sha256:{HASH_VECTEUR}"),
        ] {
            assert!(matches!(
                strip_prefix("m", ko),
                Err(StoreError::MalformedHash { .. })
            ));
        }
    }
}
