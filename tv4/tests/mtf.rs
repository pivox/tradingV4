//! Oracle d'agrégation sur la fixture réelle et proptest d'agrégat (docs/03-architecture.md §7.5,
//! S5 passe C).
//!
//! L'oracle compare l'agrégat 1m vers 5m et 1m vers 1h de `mtf::aggregate` aux fichiers natifs de
//! Kraken (`fixtures/kraken_xbtusd_5m.csv` et `_1h.csv`, tranche réelle du dump XBTUSD). Les seaux
//! natifs qui dépassent la fin du 1m sont exclus des deux côtés : un seau dont la fin tombe après la
//! dernière minute disponible est incomplet côté 1m, donc non comparable. Le proptest, lui, travaille
//! sur des bougies 1m **synthétiques** (prix inventés), annoncées comme telles.

mod common;

use common::fixture_1m;
use proptest::prelude::*;
use tv4::marketdata::kraken;
use tv4::model::{Candle, Timeframe, Ts};
use tv4::mtf::aggregate;

use common::repo_path;

/// Tolérance relative sur le volume : voir `tolerance_de_volume_justifiee`.
const TOLERANCE_VOLUME: f64 = 1e-12;

/// Bougies natives de Kraken pour `fichier` et `tf`.
fn natif(fichier: &str, tf: Timeframe) -> Vec<Candle> {
    let file = std::fs::File::open(repo_path(fichier)).expect("fixture native");
    kraken::read_all(file, tf).expect("fixture native lisible")
}

/// Résultat d'une comparaison d'agrégat à un fichier natif.
struct Comparaison {
    /// Seaux comparés (même nombre des deux côtés, mêmes horodatages).
    comparees: usize,
    /// Seaux natifs exclus parce qu'ils dépassent la fin du 1m.
    exclues: usize,
    /// Seaux dont le volume n'est pas égal au bit.
    volumes_inegaux: usize,
    /// Écart relatif maximal du volume.
    ecart_relatif_max: f64,
}

/// Compare `aggregate(m1, tf)` à `natives` en excluant les seaux qui dépassent la fin du 1m.
/// OHLC et `trades` à l'égalité exacte ; le volume est mesuré, la tolérance est vérifiée par l'appelant.
fn comparer(m1: &[Candle], natives: &[Candle], tf: Timeframe) -> Comparaison {
    let fin_ms = m1.last().expect("1m non vide").open_time_ms + Timeframe::M1.ms();
    let debut_ms = tf.open_time_of(m1[0].open_time_ms);
    let agregees = aggregate(m1, tf);
    let retenues: Vec<&Candle> = natives
        .iter()
        .filter(|b| b.open_time_ms >= debut_ms && b.open_time_ms + tf.ms() <= fin_ms)
        .collect();
    let exclues = natives
        .iter()
        .filter(|b| b.open_time_ms + tf.ms() > fin_ms)
        .count();
    // Les seaux agrégés incomplets en fin de série sont exclus de la même façon.
    let nos: Vec<&Candle> = agregees
        .iter()
        .filter(|b| b.open_time_ms + tf.ms() <= fin_ms)
        .collect();
    assert_eq!(
        nos.len(),
        retenues.len(),
        "même nombre de seaux comparés des deux côtés"
    );
    let mut volumes_inegaux = 0usize;
    let mut ecart_relatif_max = 0.0_f64;
    for (a, b) in nos.iter().zip(&retenues) {
        assert_eq!(a.open_time_ms, b.open_time_ms, "mêmes horodatages");
        let t = a.open_time_ms;
        assert_eq!(a.open.to_bits(), b.open.to_bits(), "open du seau {t}");
        assert_eq!(a.high.to_bits(), b.high.to_bits(), "high du seau {t}");
        assert_eq!(a.low.to_bits(), b.low.to_bits(), "low du seau {t}");
        assert_eq!(a.close.to_bits(), b.close.to_bits(), "close du seau {t}");
        assert_eq!(a.trades, b.trades, "trades du seau {t}");
        if a.volume.to_bits() != b.volume.to_bits() {
            volumes_inegaux += 1;
        }
        let relatif = (a.volume - b.volume).abs() / b.volume.abs();
        assert!(
            relatif <= TOLERANCE_VOLUME,
            "volume du seau {t} : {} contre {} (écart relatif {relatif:e})",
            a.volume,
            b.volume
        );
        ecart_relatif_max = ecart_relatif_max.max(relatif);
    }
    Comparaison {
        comparees: nos.len(),
        exclues,
        volumes_inegaux,
        ecart_relatif_max,
    }
}

#[test]
fn oracle_kraken_5m_sur_la_fixture() {
    let m1 = fixture_1m();
    let natives = natif("fixtures/kraken_xbtusd_5m.csv", Timeframe::M5);
    let c = comparer(&m1, &natives, Timeframe::M5);
    // Mesure du 14/10 : 2 271 seaux de chaque côté, aucun exclu (le 5m de 21:25 est couvert par les
    // 1m de 21:25 à 21:29).
    assert_eq!(c.comparees, 2_271);
    assert_eq!(c.exclues, 0);
}

#[test]
fn oracle_kraken_1h_sur_la_fixture() {
    let m1 = fixture_1m();
    let natives = natif("fixtures/kraken_xbtusd_1h.csv", Timeframe::H1);
    let c = comparer(&m1, &natives, Timeframe::H1);
    // Exactement un seau natif exclu : la 1h de 21:00 le 08/01/2020 (1578517200000), qui dépasse la
    // dernière 1m (21:29).
    assert_eq!(c.comparees, 189);
    assert_eq!(c.exclues, 1);
    let derniere = natives.last().expect("1h native non vide");
    assert_eq!(derniere.open_time_ms, 1_578_517_200_000);
}

/// Justification de la tolérance de volume (OHLC et `trades` sont à l'égalité exacte).
///
/// Kraken somme les volumes des transactions du seau ; nous sommons les volumes 1m, déjà arrondis par
/// Kraken à la minute. L'ordre d'addition et donc l'arrondi diffèrent : seuls quelques volumes sont
/// égaux au bit, les autres s'écartent de quelques ulp. Mesure du 14/10 sur la fixture : 1 295 seaux
/// 5m sur 2 271 et 163 seaux 1h sur 189 ont un volume différent au bit, avec un écart relatif maximal
/// de 4,14e-15 en 5m et de 7,49e-15 en 1h. La tolérance de 1e-12 garde donc un facteur supérieur
/// à 100 sur la pire mesure, tout en restant très en deçà d'une erreur réelle d'agrégation (une minute
/// oubliée change le volume d'au moins 1e-4 relatif sur ces seaux).
#[test]
fn tolerance_de_volume_justifiee() {
    let m1 = fixture_1m();
    for (fichier, tf, attendus_inegaux) in [
        ("fixtures/kraken_xbtusd_5m.csv", Timeframe::M5, 1_295usize),
        ("fixtures/kraken_xbtusd_1h.csv", Timeframe::H1, 163usize),
    ] {
        let natives = natif(fichier, tf);
        let c = comparer(&m1, &natives, tf);
        assert_eq!(
            c.volumes_inegaux, attendus_inegaux,
            "{tf} : volumes différents au bit"
        );
        assert!(
            c.ecart_relatif_max < 1e-14,
            "{tf} : écart relatif max {:e}",
            c.ecart_relatif_max
        );
        assert!(
            c.ecart_relatif_max * 100.0 <= TOLERANCE_VOLUME,
            "{tf} : la tolérance doit garder un facteur 100"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Proptest : l'agrégat 15m est une fonction des bougies 1m de son seau
// ---------------------------------------------------------------------------------------------

/// 2024-01-01T00:00:00Z : aligné sur 15m (et sur le jour).
const T0: Ts = common::T0;
const M15_MS: Ts = 900_000;

/// Une minute tirée par proptest : présence (80 %), prix de base, débords, volume, trades.
type Minute = (u8, f64, f64, f64, f64, f64, u32);

fn minute() -> impl Strategy<Value = Minute> {
    (
        0u8..10,
        50.0f64..150.0,
        -0.01f64..0.01,
        0.0f64..0.01,
        0.0f64..0.01,
        0.0f64..5.0,
        0u32..1_000,
    )
}

/// Bougies **synthétiques** valides : `buckets` seaux de 15m consécutifs, chaque minute présente avec
/// probabilité 0,8 ; `avec_trades` choisit si les minutes portent un nombre de transactions.
fn construire(minutes: &[Minute], avec_trades: bool) -> Vec<Candle> {
    let mut out = Vec::new();
    for (k, &(presence, base, mouvement, haut, bas, volume, trades)) in minutes.iter().enumerate() {
        if presence >= 8 {
            continue;
        }
        let open = base;
        let close = base * (1.0 + mouvement);
        out.push(Candle {
            open_time_ms: T0 + k as i64 * 60_000,
            open,
            high: open.max(close) * (1.0 + haut),
            low: open.min(close) * (1.0 - bas),
            close,
            volume,
            trades: avec_trades.then_some(trades),
        });
    }
    out
}

/// Pli naïf d'un seau, écrit ici sans réutiliser `aggregate`.
fn plier(seau: &[Candle], debut: Ts) -> Candle {
    let mut acc = Candle {
        open_time_ms: debut,
        open: seau[0].open,
        high: seau[0].high,
        low: seau[0].low,
        close: seau[0].close,
        volume: seau[0].volume,
        trades: seau[0].trades,
    };
    for c in &seau[1..] {
        if c.high > acc.high {
            acc.high = c.high;
        }
        if c.low < acc.low {
            acc.low = c.low;
        }
        acc.close = c.close;
        acc.volume += c.volume;
        acc.trades = match (acc.trades, c.trades) {
            (Some(a), Some(b)) => a.checked_add(b),
            _ => None,
        };
    }
    acc
}

/// Bit à bit : toutes les valeurs numériques et l'absence ou la présence de `trades`.
fn egal_au_bit(a: &Candle, b: &Candle) -> bool {
    a.open_time_ms == b.open_time_ms
        && a.open.to_bits() == b.open.to_bits()
        && a.high.to_bits() == b.high.to_bits()
        && a.low.to_bits() == b.low.to_bits()
        && a.close.to_bits() == b.close.to_bits()
        && a.volume.to_bits() == b.volume.to_bits()
        && a.trades == b.trades
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]

    /// Pour chaque seau `b` de l'agrégat de toute la série : (1) il est égal au bit à l'agrégat des
    /// seules 1m du seau, (2) égal à un pli naïf, (3) inchangé quand on modifie (en restant valide) les
    /// 1m hors du seau.
    #[test]
    fn proptest_agregat_15m_fonction_de_son_seau(
        buckets in 1usize..=12,
        grille in prop::collection::vec(minute(), 180),
        avec_trades in any::<bool>(),
        facteur in 0.5f64..2.0,
    ) {
        let minutes = &grille[..buckets * 15];
        let m1 = construire(minutes, avec_trades);
        prop_assume!(!m1.is_empty());
        let tout = aggregate(&m1, Timeframe::M15);
        for bougie in &tout {
            let b = bougie.open_time_ms;
            prop_assert_eq!(b % M15_MS, 0, "seau aligné sur 15m");
            let dans: Vec<Candle> = m1
                .iter()
                .filter(|c| c.open_time_ms >= b && c.open_time_ms < b + M15_MS)
                .copied()
                .collect();
            prop_assert!(!dans.is_empty());
            // (1) fonction du seul seau.
            let seul = aggregate(&dans, Timeframe::M15);
            prop_assert_eq!(seul.len(), 1);
            prop_assert!(egal_au_bit(bougie, &seul[0]), "{:?} contre {:?}", bougie, seul[0]);
            // (2) pli naïf.
            prop_assert!(egal_au_bit(bougie, &plier(&dans, b)));
            // (3) invariance : les 1m hors du seau changent (prix mis à l'échelle, donc valides).
            let modifiees: Vec<Candle> = m1
                .iter()
                .map(|c| {
                    if c.open_time_ms >= b && c.open_time_ms < b + M15_MS {
                        *c
                    } else {
                        Candle {
                            open: c.open * facteur,
                            high: c.high * facteur,
                            low: c.low * facteur,
                            close: c.close * facteur,
                            volume: c.volume + 1.0,
                            ..*c
                        }
                    }
                })
                .collect();
            let apres = aggregate(&modifiees, Timeframe::M15);
            let retrouvee = apres.iter().find(|x| x.open_time_ms == b);
            prop_assert!(retrouvee.is_some_and(|x| egal_au_bit(bougie, x)));
        }
    }
}
