//! Blocs, embargos, chauffe et purge de `model::blocks` (docs/specs/s11.md §3 et §13 ligne 5), et
//! trimestres civils de `model::universe`.
//!
//! Les six instants de frontière sont écrits une seule fois dans `model/blocks.rs` (un test de
//! `tests/architecture.rs` refuse leur littéral ailleurs dans le code hors tests). Ils sont recalculés
//! ici par une conversion calendaire écrite à part (somme de longueurs d'années et de mois, sans
//! l'algorithme de `store::time`), puis recoupés avec `store::time::days_from_civil`. Aucune donnée
//! de marché : ce sont des dates de calendrier.

use tv4::model::blocks::{
    BLOCKS, BLOCKS_VERSION, Block, BlockSpan, EMBARGOS, WARMUP_PREFIX_DAYS_DEFAULT,
    WARMUP_PREFIX_DAYS_MAX, Zone, decision_window, is_purged, zone_of,
};
use tv4::model::universe::quarter_bounds;
use tv4::model::{ModelError, Ts};
use tv4::store::time::{civil_from_days, days_from_civil};

const DAY_MS: Ts = 86_400_000;

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Minuit UTC du jour civil `(year, month, day)` en millisecondes, par comptage des années et des
/// mois depuis 1970 : indépendant de `store::time::days_from_civil`.
fn naive_ms(year: i64, month: u32, day: i64) -> Ts {
    let mut days: i64 = (1970..year)
        .map(|y| if is_leap(y) { 366 } else { 365 })
        .sum();
    days += (1..month).map(|m| days_in_month(year, m)).sum::<i64>();
    days += day - 1;
    days * DAY_MS
}

fn store_ms(year: i64, month: u32, day: u32) -> Ts {
    days_from_civil(year, month, day) * DAY_MS
}

#[test]
fn la_conversion_calendaire_du_test_recoupe_celle_du_store() {
    for (y, m, d) in [
        (1970, 1, 1),
        (2018, 1, 1),
        (2020, 2, 29),
        (2023, 3, 1),
        (2100, 3, 1),
        (2026, 7, 1),
    ] {
        assert_eq!(
            naive_ms(y, m, i64::from(d)),
            store_ms(y, m, d),
            "{y}-{m}-{d}"
        );
    }
    // Repère connu : 2000-01-01 est le jour 10 957.
    assert_eq!(naive_ms(2000, 1, 1), 10_957 * DAY_MS);
}

#[test]
fn les_six_instants_sont_recalcules_par_le_calendrier() {
    // Dates de 04 §2.3, bornes de fin inclusives : la fin du bloc est le lendemain 00:00 UTC.
    let dev = Block::Dev.span();
    let validation = Block::Validation.span();
    let holdout = Block::Holdout.span();
    let forward = Block::Forward.span();

    // 1. début de DEV : 2018-01-01.
    assert_eq!(dev.start_ms, naive_ms(2018, 1, 1));
    // 2. fin de DEV : 2022-12-31 inclus, soit 2023-01-01.
    assert_eq!(dev.end_ms, Some(naive_ms(2023, 1, 1)));
    // 3. fin de l'embargo 1 et début de VALIDATION : 2023-03-01.
    assert_eq!(validation.start_ms, naive_ms(2023, 3, 1));
    // 4. fin de VALIDATION : 2024-12-31 inclus, soit 2025-01-01.
    assert_eq!(validation.end_ms, Some(naive_ms(2025, 1, 1)));
    // 5. fin de l'embargo 2 et début de HOLDOUT : 2025-03-01.
    assert_eq!(holdout.start_ms, naive_ms(2025, 3, 1));
    // 6. fin de HOLDOUT : 2026-06-30 inclus, soit 2026-07-01, début de FORWARD.
    assert_eq!(holdout.end_ms, Some(naive_ms(2026, 7, 1)));
    assert_eq!(forward.start_ms, naive_ms(2026, 7, 1));
    assert_eq!(forward.end_ms, None);

    // Mêmes valeurs avec l'algorithme du store.
    assert_eq!(dev.start_ms, store_ms(2018, 1, 1));
    assert_eq!(dev.end_ms, Some(store_ms(2023, 1, 1)));
    assert_eq!(validation.start_ms, store_ms(2023, 3, 1));
    assert_eq!(validation.end_ms, Some(store_ms(2025, 1, 1)));
    assert_eq!(holdout.start_ms, store_ms(2025, 3, 1));
    assert_eq!(holdout.end_ms, Some(store_ms(2026, 7, 1)));
}

#[test]
fn les_embargos_sont_recalcules_et_font_cinquante_neuf_jours() {
    assert_eq!(
        EMBARGOS[0],
        (naive_ms(2023, 1, 1), naive_ms(2023, 3, 1)),
        "embargo 1 : 2023-01-01 → 2023-02-28 inclus"
    );
    assert_eq!(
        EMBARGOS[1],
        (naive_ms(2025, 1, 1), naive_ms(2025, 3, 1)),
        "embargo 2 : 2025-01-01 → 2025-02-28 inclus"
    );
    // 31 + 28 jours : 59 et non 60 (04 §2.3 donne « 60 j » et des dates qui en donnent 59, s11 §3.2).
    for (start, end) in EMBARGOS {
        assert_eq!((end - start) / DAY_MS, 59);
        assert_eq!((end - start) % DAY_MS, 0);
    }
}

#[test]
fn blocs_et_embargos_pavent_le_temps_sans_recouvrement() {
    // Ordre chronologique : DEV, embargo 1, VALIDATION, embargo 2, HOLDOUT, FORWARD.
    assert_eq!(
        BLOCKS.map(|(block, _)| block),
        [
            Block::Dev,
            Block::Validation,
            Block::Holdout,
            Block::Forward
        ]
    );
    assert_eq!(BLOCKS.map(|(block, _)| block), Block::ALL);
    let mut cursor = naive_ms(2018, 1, 1);
    let mut pieces: Vec<(Ts, Option<Ts>)> = Vec::new();
    for (index, (block, span)) in BLOCKS.iter().enumerate() {
        assert_eq!(*span, block.span());
        assert_eq!(
            span.start_ms, cursor,
            "{block} commence où le précédent finit"
        );
        pieces.push((span.start_ms, span.end_ms));
        cursor = match span.end_ms {
            Some(end) => {
                assert!(end > span.start_ms);
                // Un embargo suit chaque bloc fermé, sauf avant FORWARD qui suit HOLDOUT directement.
                match EMBARGOS.iter().find(|(start, _)| *start == end) {
                    Some((_, embargo_end)) => *embargo_end,
                    None => {
                        assert_eq!(*block, Block::Holdout, "seul HOLDOUT touche FORWARD");
                        end
                    }
                }
            }
            None => {
                assert_eq!(index, BLOCKS.len() - 1, "seul FORWARD est ouvert à droite");
                i64::MAX
            }
        };
    }
    assert_eq!(cursor, i64::MAX);
    // Aucun recouvrement entre deux pièces consécutives (blocs et embargos confondus).
    for pair in pieces.windows(2) {
        assert!(pair[0].1.is_some_and(|end| end <= pair[1].0));
    }
}

#[test]
fn zone_of_aux_bornes() {
    let [(_, dev), (_, validation), (_, holdout), (_, forward)] = BLOCKS;
    let dev_end = dev.end_ms.unwrap();
    let validation_end = validation.end_ms.unwrap();
    let holdout_end = holdout.end_ms.unwrap();

    assert_eq!(zone_of(i64::MIN), Zone::BeforeDev);
    assert_eq!(zone_of(0), Zone::BeforeDev);
    assert_eq!(zone_of(dev.start_ms - 1), Zone::BeforeDev);
    assert_eq!(zone_of(dev.start_ms), Zone::Block(Block::Dev));
    assert_eq!(zone_of(dev_end - 1), Zone::Block(Block::Dev));
    assert_eq!(zone_of(dev_end), Zone::Embargo(1));
    assert_eq!(zone_of(validation.start_ms - 1), Zone::Embargo(1));
    assert_eq!(zone_of(validation.start_ms), Zone::Block(Block::Validation));
    assert_eq!(zone_of(validation_end - 1), Zone::Block(Block::Validation));
    assert_eq!(zone_of(validation_end), Zone::Embargo(2));
    assert_eq!(zone_of(holdout.start_ms - 1), Zone::Embargo(2));
    assert_eq!(zone_of(holdout.start_ms), Zone::Block(Block::Holdout));
    assert_eq!(zone_of(holdout_end - 1), Zone::Block(Block::Holdout));
    assert_eq!(zone_of(holdout_end), Zone::Block(Block::Forward));
    assert_eq!(zone_of(forward.start_ms), Zone::Block(Block::Forward));
    assert_eq!(zone_of(i64::MAX), Zone::Block(Block::Forward));
}

#[test]
fn zone_of_est_coherente_avec_les_intervalles() {
    // Chaque instant d'un bloc est dans ce bloc ; chaque instant d'un embargo dans cet embargo.
    for (block, span) in BLOCKS {
        assert!(span.contains(span.start_ms));
        assert!(!span.contains(span.start_ms - 1));
        assert_eq!(zone_of(span.start_ms + DAY_MS), Zone::Block(block));
        if let Some(end) = span.end_ms {
            assert!(span.contains(end - 1));
            assert!(!span.contains(end));
        }
    }
    assert!(
        BlockSpan {
            start_ms: 5,
            end_ms: None
        }
        .contains(i64::MAX)
    );
    for (number, (start, end)) in (1u8..).zip(EMBARGOS) {
        assert_eq!(zone_of(start), Zone::Embargo(number));
        assert_eq!(zone_of(end - 1), Zone::Embargo(number));
        assert_ne!(zone_of(end), Zone::Embargo(number));
    }
}

#[test]
fn blocs_proteges() {
    assert!(!Block::Dev.is_protected());
    assert!(Block::Validation.is_protected());
    assert!(Block::Holdout.is_protected());
    assert!(!Block::Forward.is_protected());
}

#[test]
fn fenetre_de_decision_d_un_bloc_ferme() {
    for block in [Block::Dev, Block::Validation, Block::Holdout] {
        let span = block.span();
        let end = span.end_ms.unwrap();
        // Fin du bloc : le bloc entier.
        assert_eq!(decision_window(block, end).unwrap(), (span.start_ms, end));
        // Fin plus tôt : fenêtre raccourcie.
        assert_eq!(
            decision_window(block, end - DAY_MS).unwrap(),
            (span.start_ms, end - DAY_MS)
        );
        assert_eq!(
            decision_window(block, span.start_ms + 1).unwrap(),
            (span.start_ms, span.start_ms + 1)
        );
        // Fenêtre vide ou avant le bloc, ou qui déborde sur l'embargo.
        for refused in [span.start_ms, span.start_ms - 1, 0, end + 1, end + DAY_MS] {
            assert!(
                matches!(
                    decision_window(block, refused),
                    Err(ModelError::DecisionWindow { block: b, window_end_ms, .. })
                        if b == block && window_end_ms == refused
                ),
                "{block} {refused}"
            );
        }
    }
}

#[test]
fn fenetre_de_decision_de_forward_ouvert_a_droite() {
    let start = Block::Forward.span().start_ms;
    assert_eq!(
        decision_window(Block::Forward, start + 90 * DAY_MS).unwrap(),
        (start, start + 90 * DAY_MS)
    );
    assert_eq!(
        decision_window(Block::Forward, i64::MAX).unwrap(),
        (start, i64::MAX)
    );
    assert!(decision_window(Block::Forward, start).is_err());
    assert!(decision_window(Block::Forward, start - 1).is_err());
}

#[test]
fn le_message_de_fenetre_nomme_le_bloc() {
    let erreur = decision_window(Block::Validation, 1).unwrap_err();
    let texte = erreur.to_string();
    assert!(texte.contains("VALIDATION"), "{texte}");
    assert!(texte.contains("début du bloc"), "{texte}");
    let trop_tard = decision_window(Block::Dev, Block::Dev.span().end_ms.unwrap() + 1)
        .unwrap_err()
        .to_string();
    assert!(trop_tard.contains("après la fin du bloc"), "{trop_tard}");
}

#[test]
fn purge_les_quatre_cas() {
    let end = 1_000;
    // 1. Entrée avant la fin, soldée par la fin de fenêtre : purgé.
    assert!(is_purged(900, end, end, true));
    // 2. Entrée avant la fin, sortie à la fin ou après sans drapeau (garde) : purgé.
    assert!(is_purged(900, end, end, false));
    assert!(is_purged(900, end + 60_000, end, false));
    // 3. Entrée avant la fin, sortie normale avant la fin : conservé.
    assert!(!is_purged(900, end - 1, end, false));
    // 4. Entrée à la fin ou après (jamais acceptée par le moteur) : jamais purgée.
    assert!(!is_purged(end, end, end, true));
    assert!(!is_purged(end + 1, end + 2, end, false));
    // Une entrée d'un tout autre bloc (avant le début) reste jugée sur la sortie seule.
    assert!(!is_purged(0, 10, end, false));
}

#[test]
fn chauffe_de_210_jours_par_defaut() {
    assert_eq!(WARMUP_PREFIX_DAYS_DEFAULT, 210);
    // Dates de s11 §3.3 : 2017-06-05, 2022-08-03, 2024-08-03, 2025-12-03.
    let attendu = [
        (Block::Dev, (2017, 6, 5)),
        (Block::Validation, (2022, 8, 3)),
        (Block::Holdout, (2024, 8, 3)),
        (Block::Forward, (2025, 12, 3)),
    ];
    for (block, (y, m, d)) in attendu {
        let start = block.warmup_start_ms(WARMUP_PREFIX_DAYS_DEFAULT).unwrap();
        assert_eq!(start, naive_ms(y, m, i64::from(d)), "{block}");
        let (year, month, day) = civil_from_days(start / DAY_MS);
        assert_eq!((year, month, day), (y, m, d), "{block}");
        assert_eq!(start % DAY_MS, 0);
    }
    assert_eq!(
        Block::Dev.warmup_start_ms(0).unwrap(),
        Block::Dev.span().start_ms
    );
    assert_eq!(
        Block::Dev.warmup_start_ms(WARMUP_PREFIX_DAYS_MAX).unwrap(),
        Block::Dev.span().start_ms - 400 * DAY_MS
    );
    for hors in [-1, WARMUP_PREFIX_DAYS_MAX + 1, i64::MAX] {
        assert!(matches!(
            Block::Dev.warmup_start_ms(hors),
            Err(ModelError::WarmupOutOfRange { days }) if days == hors
        ));
    }
}

#[test]
fn nom_et_serde_des_blocs() {
    assert_eq!(BLOCKS_VERSION, "b1");
    for (block, nom) in [
        (Block::Dev, "DEV"),
        (Block::Validation, "VALIDATION"),
        (Block::Holdout, "HOLDOUT"),
        (Block::Forward, "FORWARD"),
    ] {
        assert_eq!(block.as_str(), nom);
        assert_eq!(block.to_string(), nom);
        assert_eq!(nom.parse::<Block>().unwrap(), block);
        assert_eq!(nom.to_lowercase().parse::<Block>().unwrap(), block);
        assert_eq!(serde_json::to_string(&block).unwrap(), format!("\"{nom}\""));
        assert_eq!(
            serde_json::from_str::<Block>(&format!("\"{nom}\"")).unwrap(),
            block
        );
    }
    assert!(matches!(
        "TEST".parse::<Block>(),
        Err(ModelError::UnknownBlock(texte)) if texte == "TEST"
    ));
    assert!(serde_json::from_str::<Block>("\"Dev\"").is_err());
    // Ordre chronologique des blocs.
    assert!(Block::Dev < Block::Validation && Block::Holdout < Block::Forward);
}

#[test]
fn trimestres_civils_recoupes_avec_le_calendrier() {
    for year in 2018..=2030 {
        for quarter in 1..=4u32 {
            let label = format!("{year}-Q{quarter}");
            let (start, end) = quarter_bounds(&label).unwrap();
            let first_month = 3 * (quarter - 1) + 1;
            assert_eq!(start, naive_ms(year, first_month, 1), "{label}");
            assert_eq!(start, store_ms(year, first_month, 1), "{label}");
            let (end_year, end_month) = if quarter == 4 {
                (year + 1, 1)
            } else {
                (year, first_month + 3)
            };
            assert_eq!(end, naive_ms(end_year, end_month, 1), "{label}");
        }
    }
}
