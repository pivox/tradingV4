//! Tests d'intégration du lot B de S11 (docs/specs/s11.md §4.4, §4.5, §4.7, §6.1, §6.2, §7.2) : datasets
//! de recherche, métadonnées de bloc hors de `dsh-v1`, exclusions qualité sur la série complète du store,
//! immutabilité étendue, lecture vérifiée d'une série, fichiers d'univers et liste figée de paires.
//!
//! **Toutes les données sont synthétiques** (instants et prix inventés, générés ici) : aucune valeur de
//! marché n'y est mesurée. Les noms de paires (`SYN-USD`, `OTH-USD`) sont fictifs.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use tv4::model::universe::{PairSubset, SubsetRule, SubsetSource};
use tv4::model::{
    Block, Candle, ExchangeId, InstrumentRules, ModelError, QuarterExclusion, Timeframe, Ts,
    UniverseRef,
};
use tv4::research::universe::QUALITY_TFS;
use tv4::store::time::{MS_PER_DAY, days_from_civil, parse_coverage_end, parse_rfc3339_utc};
use tv4::store::universe::{
    self, AtBuild, CsvRow, InstrumentRule, QualityExpectation, QuarterBuild, RejectedPair,
    UnionEntry, UniverseBuild, UniverseRules, UniverseSource, parse_csv, render_csv,
};
use tv4::store::{
    BuildRequest, BuildStatus, Columns, GapsPolicy, MultiBuildRequest, Partition, ResearchMeta,
    StoreError, Symbol, Window, build_dataset, build_dataset_multi, build_dataset_research,
    load_dataset, load_dataset_multi, quality_report, read_dataset_manifest, read_series_window,
    research_meta, write_partition,
};

const MINUTE: i64 = 60_000;
/// 2030-01-01T00:00:00Z : tous les fichiers des tests sont clos à cet instant.
const FUTUR: i64 = 1_893_456_000_000;

fn ms(y: i64, m: u32, d: u32) -> Ts {
    days_from_civil(y, m, d) * MS_PER_DAY
}

fn sym(text: &str) -> Symbol {
    text.parse().unwrap()
}

fn partition(symbol: &str) -> Partition {
    Partition::new(ExchangeId::Kraken, sym(symbol), Timeframe::M1)
}

/// `minutes` bougies 1m synthétiques depuis `debut`, sans les minutes dont l'indice est dans `trous`.
fn serie(debut: Ts, minutes: usize, trous: &[Range<usize>]) -> Columns {
    let candles: Vec<Candle> = (0..minutes)
        .filter(|i| !trous.iter().any(|t| t.contains(i)))
        .map(|i| {
            let base = 100.0 + (i % 50) as f64 * 0.1;
            Candle {
                open_time_ms: debut + i as i64 * MINUTE,
                open: base,
                high: base + 0.5,
                low: base - 0.5,
                close: base + 0.1,
                volume: 1.0 + (i % 3) as f64,
                trades: None,
            }
        })
        .collect();
    Columns::from_candles(&candles)
}

fn ecrire(root: &Path, symbol: &str, colonnes: &Columns) {
    write_partition(root, &partition(symbol), colonnes, "synthetic", FUTUR).unwrap();
}

/// Le premier trimestre 2019 en minutes (90 jours).
const T1_2019_MIN: usize = 90 * 1440;

/// Trous du jeu « qualité » : les quatre premières heures (un seau 4h) des jours 60 à 65 du trimestre,
/// soit six seaux 4h absents sur 540 (1,11 % > 1 %). Chaque jour garde des minutes, donc les seaux 12h
/// et 1d sont tous présents : exclus en 5m, 15m, 30m, 1h et 4h, gardés en 12h et en 1d.
fn trous_qualite() -> Vec<Range<usize>> {
    (60..66).map(|d| d * 1440..d * 1440 + 240).collect()
}

/// Écrit la série du premier trimestre 2019 avec les trous de qualité, tous après le 15 février.
fn ecrire_t1_2019(root: &Path, symbol: &str) {
    ecrire(
        root,
        symbol,
        &serie(ms(2019, 1, 1), T1_2019_MIN, &trous_qualite()),
    );
}

/// Fenêtre de dataset DEV qui coupe la série avant les trous : `[2017-12-01, 2019-02-15)`.
fn fenetre_courte() -> Window {
    Window::new(ms(2017, 12, 1), ms(2019, 2, 15)).unwrap()
}

fn requete<'a>(
    name: &'a str,
    symbols: &'a [Symbol],
    window: Window,
    policy: GapsPolicy,
) -> MultiBuildRequest<'a> {
    MultiBuildRequest {
        name,
        exchange: ExchangeId::Kraken,
        symbols,
        tf: Timeframe::M1,
        window,
        gaps_policy: policy,
    }
}

fn meta_dev() -> ResearchMeta {
    ResearchMeta {
        block: Block::Dev,
        universe: None,
        universe_quality: None,
    }
}

fn manifeste_path(root: &Path, name: &str) -> PathBuf {
    root.join("datasets").join(format!("{name}.json"))
}

fn exclusions_attendues() -> BTreeMap<String, Vec<QuarterExclusion>> {
    BTreeMap::from([(
        "SYN-USD".to_string(),
        vec![QuarterExclusion {
            quarter: "2019-Q1".to_string(),
            tfs: vec![
                Timeframe::M5,
                Timeframe::M15,
                Timeframe::M30,
                Timeframe::H1,
                Timeframe::H4,
            ],
        }],
    )])
}

// ---------------------------------------------------------------------------------------------
// Datasets de recherche
// ---------------------------------------------------------------------------------------------

#[test]
fn le_manifeste_de_recherche_porte_les_champs_et_le_hash_reste_un_hash_de_contenu() {
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let symboles = [sym("SYN-USD")];
    let fenetre = fenetre_courte();

    let recherche = build_dataset_research(
        racine.path(),
        &requete(
            "recherche",
            &symboles,
            fenetre,
            GapsPolicy::ZeroVolumeMarked,
        ),
        &meta_dev(),
        FUTUR,
    )
    .unwrap();
    let ordinaire = build_dataset_multi(
        racine.path(),
        &requete(
            "ordinaire",
            &symboles,
            fenetre,
            GapsPolicy::ZeroVolumeMarked,
        ),
        FUTUR,
    )
    .unwrap();

    // `block` est hors de `dsh-v1` : même contenu, même fenêtre, même politique, même hash.
    assert_eq!(
        recherche.manifest.dataset_hash,
        ordinaire.manifest.dataset_hash
    );
    assert_eq!(recherche.status, BuildStatus::Created);

    let manifeste = &recherche.manifest;
    assert_eq!(manifeste.block, Some(Block::Dev));
    assert_eq!(
        manifeste.decision_window,
        Some([
            "2018-01-01T00:00:00Z".to_string(),
            "2019-02-15T00:00:00Z".to_string()
        ])
    );
    assert_eq!(manifeste.universe, None);
    // La qualité se juge sur la série complète du store : les trous de mars, hors de la fenêtre du
    // dataset, excluent le trimestre 2019-Q1 qui recoupe la fenêtre de décision.
    assert_eq!(manifeste.quality_exclusions, Some(exclusions_attendues()));

    // Le manifeste ordinaire n'a aucune des quatre clés (rendu d'avant S11).
    let texte = fs::read_to_string(manifeste_path(racine.path(), "ordinaire")).unwrap();
    for cle in ["block", "decision_window", "universe", "quality_exclusions"] {
        assert!(!texte.contains(cle), "{cle} écrite dans {texte}");
    }
    let texte = fs::read_to_string(manifeste_path(racine.path(), "recherche")).unwrap();
    assert!(texte.contains("\"block\": \"DEV\""), "{texte}");
    assert!(!texte.contains("\"universe\""), "{texte}");
}

#[test]
fn un_ancien_manifeste_se_relit_et_n_est_jamais_reecrit() {
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let symboles = [sym("SYN-USD")];
    let demande = requete(
        "ordinaire",
        &symboles,
        fenetre_courte(),
        GapsPolicy::ZeroVolumeMarked,
    );
    build_dataset_multi(racine.path(), &demande, FUTUR).unwrap();
    let chemin = manifeste_path(racine.path(), "ordinaire");
    let avant = fs::read(&chemin).unwrap();

    // Relu : aucun champ de recherche, et pas de recherche à faire.
    let manifeste = read_dataset_manifest(&chemin).unwrap();
    assert_eq!(research_meta(&manifeste).unwrap(), None);
    // Reconstruit : inchangé, mêmes octets.
    let encore = build_dataset_multi(racine.path(), &demande, FUTUR + 1000).unwrap();
    assert_eq!(encore.status, BuildStatus::Unchanged);
    assert_eq!(fs::read(&chemin).unwrap(), avant);
}

#[test]
fn immutabilite_etendue_aux_metadonnees_de_recherche() {
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let symboles = [sym("SYN-USD")];
    let demande = requete(
        "fige",
        &symboles,
        fenetre_courte(),
        GapsPolicy::ZeroVolumeMarked,
    );
    build_dataset_research(racine.path(), &demande, &meta_dev(), FUTUR).unwrap();
    let chemin = manifeste_path(racine.path(), "fige");
    let avant = fs::read(&chemin).unwrap();

    // Mêmes métadonnées : inchangé.
    let encore = build_dataset_research(racine.path(), &demande, &meta_dev(), FUTUR + 5).unwrap();
    assert_eq!(encore.status, BuildStatus::Unchanged);

    // Une référence d'univers en plus : refusé, rien n'est écrit.
    let autre_univers = ResearchMeta {
        universe: Some(UniverseRef {
            rules_hash: format!("blake3:{}", "11".repeat(32)),
            window_hash: format!("blake3:{}", "22".repeat(32)),
        }),
        ..meta_dev()
    };
    let erreur =
        build_dataset_research(racine.path(), &demande, &autre_univers, FUTUR).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::DatasetMetaMismatch {
                field: "universe",
                ..
            }
        ),
        "{erreur}"
    );
    // Un build ordinaire au même hash est refusé aussi : il ne peut pas rendre un manifeste de bloc.
    let erreur = build_dataset_multi(racine.path(), &demande, FUTUR).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::DatasetMetaMismatch { field: "block", .. }
        ),
        "{erreur}"
    );
    // Le constructeur mono-série (S4) est refusé aussi : troisième constructeur de s11 §6.2.
    let mono = BuildRequest {
        name: "fige",
        partition: partition("SYN-USD"),
        window: fenetre_courte(),
        gaps_policy: GapsPolicy::ZeroVolumeMarked,
    };
    let erreur = build_dataset(racine.path(), &mono, FUTUR).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::DatasetMetaMismatch { field: "block", .. }
        ),
        "{erreur}"
    );
    assert_eq!(fs::read(&chemin).unwrap(), avant);

    // Et dans l'autre sens : un manifeste ordinaire n'est pas promu en dataset de bloc.
    let demande = requete(
        "ordinaire",
        &symboles,
        fenetre_courte(),
        GapsPolicy::ZeroVolumeMarked,
    );
    build_dataset_multi(racine.path(), &demande, FUTUR).unwrap();
    let erreur = build_dataset_research(racine.path(), &demande, &meta_dev(), FUTUR).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::DatasetMetaMismatch { field: "block", .. }
        ),
        "{erreur}"
    );
    // Idem pour un manifeste mono-série ordinaire (même octets que le multi à un symbole).
    let mono = BuildRequest {
        name: "mono",
        partition: partition("SYN-USD"),
        window: fenetre_courte(),
        gaps_policy: GapsPolicy::ZeroVolumeMarked,
    };
    build_dataset(racine.path(), &mono, FUTUR).unwrap();
    let demande_mono = requete(
        "mono",
        &symboles,
        fenetre_courte(),
        GapsPolicy::ZeroVolumeMarked,
    );
    let erreur =
        build_dataset_research(racine.path(), &demande_mono, &meta_dev(), FUTUR).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::DatasetMetaMismatch { field: "block", .. }
        ),
        "{erreur}"
    );
}

/// Cas attendu de s11 §6.2 : même `--to`, série prolongée après la fenêtre. Le contenu de la fenêtre,
/// donc le `dataset_hash`, ne change pas, mais les exclusions qualité (calculées sur la série complète
/// du store) changent : le build est refusé au lieu de rendre « inchangé » un manifeste périmé.
#[test]
fn immutabilite_quality_exclusions_forward_prolonge() {
    let racine = tempfile::tempdir().unwrap();
    // Série dense jusqu'au jour 80 du trimestre : aucune exclusion pour 2019-Q1. Le mois de mars est
    // encore ouvert à l'écriture (22 mars) : il pourra être complété par une ingestion plus récente.
    write_partition(
        racine.path(),
        &partition("SYN-USD"),
        &serie(ms(2019, 1, 1), 80 * 1440, &[]),
        "synthetic",
        ms(2019, 3, 23),
    )
    .unwrap();
    let symboles = [sym("SYN-USD")];
    let demande = requete(
        "prolonge",
        &symboles,
        fenetre_courte(),
        GapsPolicy::ZeroVolumeMarked,
    );
    let premier = build_dataset_research(racine.path(), &demande, &meta_dev(), FUTUR).unwrap();
    assert_eq!(premier.manifest.quality_exclusions, Some(BTreeMap::new()));
    let chemin = manifeste_path(racine.path(), "prolonge");
    let avant = fs::read(&chemin).unwrap();

    // Ingestion après la fin de la fenêtre, dans le même trimestre, avec les trous de qualité.
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let erreur =
        build_dataset_research(racine.path(), &demande, &meta_dev(), FUTUR + 5).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::DatasetMetaMismatch {
                field: "quality_exclusions",
                ..
            }
        ),
        "{erreur}"
    );
    // Le manifeste reste identique à l'octet.
    assert_eq!(fs::read(&chemin).unwrap(), avant);

    // Contrôle : le `dataset_hash` n'a pas changé (c'est bien la qualité qui diffère), et un build neuf
    // de la même fenêtre porte les nouvelles exclusions.
    let neuf = build_dataset_research(
        racine.path(),
        &requete(
            "prolonge_neuf",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &meta_dev(),
        FUTUR + 5,
    )
    .unwrap();
    assert_eq!(neuf.manifest.dataset_hash, premier.manifest.dataset_hash);
    assert_eq!(
        neuf.manifest.quality_exclusions,
        Some(exclusions_attendues())
    );
}

#[test]
fn demandes_de_recherche_refusees_sans_rien_ecrire() {
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let symboles = [sym("SYN-USD")];
    let existe = |nom: &str| manifeste_path(racine.path(), nom).exists();

    // Politique de trous autre que zero_volume_marked.
    for politique in [GapsPolicy::Skip, GapsPolicy::Ffill] {
        let erreur = build_dataset_research(
            racine.path(),
            &requete("refus", &symboles, fenetre_courte(), politique),
            &meta_dev(),
            FUTUR,
        )
        .unwrap_err();
        assert!(
            matches!(erreur, StoreError::ResearchGapsPolicy { .. }),
            "{erreur}"
        );
    }

    // Timeframe autre que 1m.
    let mut demande = requete(
        "refus",
        &symboles,
        fenetre_courte(),
        GapsPolicy::ZeroVolumeMarked,
    );
    demande.tf = Timeframe::M5;
    let erreur = build_dataset_research(racine.path(), &demande, &meta_dev(), FUTUR).unwrap_err();
    assert!(
        matches!(erreur, StoreError::ResearchTimeframe { tf: Timeframe::M5 }),
        "{erreur}"
    );

    // Fenêtre qui commence après le début du bloc.
    let tard = Window::new(ms(2018, 1, 2), ms(2019, 2, 15)).unwrap();
    // Fenêtre qui déborde la fin de DEV.
    let deborde = Window::new(ms(2017, 12, 1), ms(2023, 1, 2)).unwrap();
    for fenetre in [tard, deborde] {
        let erreur = build_dataset_research(
            racine.path(),
            &requete("refus", &symboles, fenetre, GapsPolicy::ZeroVolumeMarked),
            &meta_dev(),
            FUTUR,
        )
        .unwrap_err();
        assert!(
            matches!(
                erreur,
                StoreError::ResearchWindow {
                    block: Block::Dev,
                    ..
                }
            ),
            "{erreur}"
        );
    }
    // Fenêtre de VALIDATION qui se termine avant le début du bloc.
    let validation = ResearchMeta {
        block: Block::Validation,
        ..meta_dev()
    };
    let erreur = build_dataset_research(
        racine.path(),
        &requete(
            "refus",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &validation,
        FUTUR,
    )
    .unwrap_err();
    assert!(
        matches!(
            erreur,
            StoreError::ResearchWindow {
                block: Block::Validation,
                ..
            }
        ),
        "{erreur}"
    );
    assert!(!existe("refus"));
}

#[test]
fn research_meta_recalcule_et_recoupe() {
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let symboles = [sym("SYN-USD")];
    build_dataset_research(
        racine.path(),
        &requete(
            "recherche",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &meta_dev(),
        FUTUR,
    )
    .unwrap();
    let chemin = manifeste_path(racine.path(), "recherche");
    let manifeste = read_dataset_manifest(&chemin).unwrap();

    let info = research_meta(&manifeste).unwrap().unwrap();
    assert_eq!(info.block, Block::Dev);
    assert_eq!(info.decision_start_ms, ms(2018, 1, 1));
    assert_eq!(info.decision_end_ms, ms(2019, 2, 15));
    assert_eq!(info.universe, None);
    let dans_t1 = ms(2019, 2, 1);
    assert!(info.quality.is_excluded("SYN-USD", dans_t1, Timeframe::H4));
    assert!(!info.quality.is_excluded("SYN-USD", dans_t1, Timeframe::D1));
    // Le 1m n'exclut jamais rien, et un autre trimestre ou un autre symbole non plus.
    assert!(!info.quality.is_excluded("SYN-USD", dans_t1, Timeframe::M1));
    assert!(
        !info
            .quality
            .is_excluded("SYN-USD", ms(2019, 4, 2), Timeframe::H4)
    );
    assert!(!info.quality.is_excluded("OTH-USD", dans_t1, Timeframe::H4));

    // Un dataset de recherche ne se charge pas en mono-série ; le chargement multi-séries le rend.
    let erreur = load_dataset(racine.path(), &chemin).unwrap_err();
    assert!(
        matches!(erreur, StoreError::ResearchNeedsPortfolio { .. }),
        "{erreur}"
    );
    let charge = load_dataset_multi(racine.path(), &chemin).unwrap();
    assert_eq!(charge.manifest, manifeste);
    assert_eq!(charge.series.len(), 1);

    // Une fenêtre de décision qui diverge du bloc et de la fenêtre est refusée.
    let mut abime = manifeste.clone();
    abime.decision_window = Some([
        "2018-01-02T00:00:00Z".to_string(),
        "2019-02-15T00:00:00Z".to_string(),
    ]);
    let erreur = research_meta(&abime).unwrap_err();
    assert!(
        matches!(
            erreur,
            StoreError::ResearchMetaInvalid {
                field: "decision_window",
                ..
            }
        ),
        "{erreur}"
    );
    // `block` sans `decision_window`, sans `quality_exclusions`, et l'inverse.
    let mut sans_fenetre = manifeste.clone();
    sans_fenetre.decision_window = None;
    assert!(matches!(
        research_meta(&sans_fenetre),
        Err(StoreError::ResearchMetaInvalid {
            field: "decision_window",
            ..
        })
    ));
    let mut sans_qualite = manifeste.clone();
    sans_qualite.quality_exclusions = None;
    assert!(matches!(
        research_meta(&sans_qualite),
        Err(StoreError::ResearchMetaInvalid {
            field: "quality_exclusions",
            ..
        })
    ));
    for champ in ["decision_window", "universe", "quality_exclusions"] {
        let mut sans_bloc = manifeste.clone();
        sans_bloc.block = None;
        if champ == "universe" {
            sans_bloc.universe = Some(UniverseRef {
                rules_hash: format!("blake3:{}", "11".repeat(32)),
                window_hash: format!("blake3:{}", "22".repeat(32)),
            });
            sans_bloc.decision_window = None;
            sans_bloc.quality_exclusions = None;
        }
        let erreur = research_meta(&sans_bloc).unwrap_err();
        assert!(
            matches!(erreur, StoreError::ResearchMetaInvalid { .. }),
            "{champ} : {erreur}"
        );
    }
    // Une politique de trous autre que zero_volume_marked est refusée.
    let mut skip = manifeste;
    skip.gaps_policy = GapsPolicy::Skip;
    assert!(matches!(
        research_meta(&skip),
        Err(StoreError::ResearchGapsPolicy { .. })
    ));
}

#[test]
fn un_symbole_sans_exclusion_n_a_pas_d_entree_et_le_dernier_trimestre_complet_n_exclut_rien() {
    let racine = tempfile::tempdir().unwrap();
    // Série complète sur trois jours : aucun trou, aucune exclusion.
    ecrire(
        racine.path(),
        "SYN-USD",
        &serie(ms(2019, 1, 1), 3 * 1440, &[]),
    );
    let symboles = [sym("SYN-USD")];
    let sortie = build_dataset_research(
        racine.path(),
        &requete(
            "propre",
            &symboles,
            Window::new(ms(2017, 12, 1), ms(2019, 1, 4)).unwrap(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &meta_dev(),
        FUTUR,
    )
    .unwrap();
    // `quality_exclusions` est présent, vide, sur tout manifeste qui porte `block`.
    assert_eq!(sortie.manifest.quality_exclusions, Some(BTreeMap::new()));
    let texte = fs::read_to_string(manifeste_path(racine.path(), "propre")).unwrap();
    assert!(texte.contains("\"quality_exclusions\": {}"), "{texte}");
}

/// Propriété d'emboîtement des TF de qualité : sur un trimestre intérieur (bornes alignées sur le jour),
/// un seau d'un TF supérieur absent entraîne l'absence de tous les seaux des TF inférieurs qu'il
/// contient, et chaque seau a le même nombre d'enfants. La fraction absente d'un TF est donc au moins
/// celle du TF supérieur : l'ensemble des TF exclus est toujours un préfixe de `QUALITY_TFS` (les plus
/// courts d'abord), jamais un 1d exclu et un 5m gardé. Séries synthétiques à pannes aléatoires.
#[test]
fn les_tf_exclus_forment_toujours_un_prefixe_de_quality_tfs() {
    // 2019-01-01 → 2019-10-01 : le trimestre 2019-Q2 est intérieur, de part et d'autre.
    let debut = ms(2019, 1, 1);
    let minutes = ((ms(2019, 10, 1) - debut) / MINUTE) as usize;
    let mut longueurs_vues = std::collections::BTreeSet::new();
    for seed in 0..60_u64 {
        let mut rng = common::SplitMix64(seed + 1);
        let mut presente = vec![true; minutes];
        // Pannes aléatoires dont la longueur maximale (30 minutes, 6 heures ou 2 jours) et le nombre
        // varient d'une graine à l'autre ; les graines paires ajoutent des minutes isolées absentes à une
        // densité de 30 à 99 % (un seau de 5m n'est absent que si ses cinq minutes le sont).
        let pannes = rng.next_u64() % 80;
        let maximum = [30_usize, 360, 2 * 1440][(rng.next_u64() % 3) as usize];
        for _ in 0..pannes {
            let longueur = 1 + (rng.next_u64() % maximum as u64) as usize;
            let depart = (rng.next_u64() % minutes as u64) as usize;
            for etat in presente.iter_mut().skip(depart).take(longueur) {
                *etat = false;
            }
        }
        if seed % 2 == 0 {
            let densite = 0.3 + rng.next_f64() * 0.69;
            for etat in presente.iter_mut() {
                if rng.next_f64() < densite {
                    *etat = false;
                }
            }
        }
        // La première et la dernière minute restent présentes : la plage observée est toute la série.
        presente[0] = true;
        presente[minutes - 1] = true;
        let temps: Vec<Ts> = presente
            .iter()
            .enumerate()
            .filter(|(_, etat)| **etat)
            .map(|(i, _)| debut + i as i64 * MINUTE)
            .collect();
        let rapport = quality_report(&temps, &QUALITY_TFS).unwrap();
        let exclus: Vec<_> = QUALITY_TFS
            .iter()
            .copied()
            .filter(|tf| {
                rapport
                    .rows
                    .iter()
                    .any(|row| row.tf == *tf && row.quarter == "2019-Q2" && row.excluded)
            })
            .collect();
        assert_eq!(
            exclus,
            QUALITY_TFS[..exclus.len()],
            "graine {seed} : TF exclus {exclus:?}, non préfixe de QUALITY_TFS"
        );
        longueurs_vues.insert(exclus.len());
    }
    // La propriété n'est pas vide : les tirages exercent plusieurs longueurs de préfixe.
    assert!(
        longueurs_vues.len() >= 3,
        "longueurs de préfixe vues : {longueurs_vues:?}"
    );
}

#[test]
fn comparaison_aux_exclusions_des_csv_de_l_univers() {
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let symboles = [sym("SYN-USD")];
    let tfs_store = vec![
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H4,
    ];
    let avec = |attendu: QualityExpectation| ResearchMeta {
        universe_quality: Some(attendu),
        ..meta_dev()
    };

    // Les CSV disent la même chose que le store (et un trimestre hors fenêtre est ignoré).
    let bon = QualityExpectation::from_entries([
        (
            "2019-Q1".to_string(),
            "SYN-USD".to_string(),
            tfs_store.clone(),
        ),
        ("2018-Q3".to_string(), "SYN-USD".to_string(), Vec::new()),
        (
            "2020-Q1".to_string(),
            "OTH-USD".to_string(),
            vec![Timeframe::H1],
        ),
    ])
    .unwrap();
    build_dataset_research(
        racine.path(),
        &requete(
            "accord",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &avec(bon),
        FUTUR,
    )
    .unwrap();

    // Un CSV qui n'exclut que la 1h : écart.
    let faux = QualityExpectation::from_entries([(
        "2019-Q1".to_string(),
        "SYN-USD".to_string(),
        vec![Timeframe::H1],
    )])
    .unwrap();
    let erreur = build_dataset_research(
        racine.path(),
        &requete(
            "ecart",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &avec(faux),
        FUTUR,
    )
    .unwrap_err();
    assert!(
        matches!(&erreur, StoreError::UniverseQualityMismatch { quarter, symbol, .. }
            if quarter == "2019-Q1" && symbol == "SYN-USD"),
        "{erreur}"
    );
    // Un CSV qui annonce une exclusion que le store ne voit pas, ou ne voit aucune : écart aussi.
    let manque = QualityExpectation::from_entries([(
        "2018-Q2".to_string(),
        "SYN-USD".to_string(),
        vec![Timeframe::D1],
    )])
    .unwrap();
    assert!(matches!(
        build_dataset_research(
            racine.path(),
            &requete(
                "ecart",
                &symboles,
                fenetre_courte(),
                GapsPolicy::ZeroVolumeMarked
            ),
            &avec(manque),
            FUTUR,
        ),
        Err(StoreError::UniverseQualityMismatch { .. })
    ));
    // Un membre de l'univers absent des symboles du dataset.
    let absent = QualityExpectation::from_entries([(
        "2019-Q1".to_string(),
        "OTH-USD".to_string(),
        Vec::new(),
    )])
    .unwrap();
    let erreur = build_dataset_research(
        racine.path(),
        &requete(
            "ecart",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &avec(absent),
        FUTUR,
    )
    .unwrap_err();
    assert!(
        matches!(erreur, StoreError::UniverseMemberMissing { .. }),
        "{erreur}"
    );
    assert!(!manifeste_path(racine.path(), "ecart").exists());
}

#[test]
fn qualite_calculee_depuis_la_premiere_bougie_de_la_serie_pas_du_dataset() {
    // Série qui commence le 2019-01-01 ; le dataset commence le 2017-12-01 et finit au 15 février.
    // Calculée sur la fenêtre seule, la qualité de 2019-Q1 serait parfaite (aucun trou avant le 15
    // février, et la plage s'arrêterait au 15) : l'exclusion n'existe que sur la série complète.
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    let colonnes =
        read_series_window(racine.path(), &partition("SYN-USD"), Some(fenetre_courte())).unwrap();
    let rapport = tv4::store::quality_report(colonnes.open_time_ms(), &[Timeframe::H4]).unwrap();
    assert!(
        rapport.rows.iter().all(|r| !r.excluded),
        "{:?}",
        rapport.rows
    );
    let complete = read_series_window(racine.path(), &partition("SYN-USD"), None).unwrap();
    let rapport = tv4::store::quality_report(complete.open_time_ms(), &[Timeframe::H4]).unwrap();
    assert!(rapport.rows.iter().any(|r| r.excluded));
}

// ---------------------------------------------------------------------------------------------
// read_series_window
// ---------------------------------------------------------------------------------------------

#[test]
fn lecture_verifiee_d_une_serie_avec_ou_sans_fenetre() {
    let racine = tempfile::tempdir().unwrap();
    // Deux mois : décembre 2018 (3 jours) et janvier 2019 (3 jours).
    ecrire(
        racine.path(),
        "SYN-USD",
        &serie(ms(2018, 12, 30), 6 * 1440, &[]),
    );

    let tout = read_series_window(racine.path(), &partition("SYN-USD"), None).unwrap();
    assert_eq!(tout.len(), 6 * 1440);
    assert_eq!(tout.synthetic_count(), 0);

    let fenetre = Window::new(ms(2019, 1, 1), ms(2019, 1, 2)).unwrap();
    let partie = read_series_window(racine.path(), &partition("SYN-USD"), Some(fenetre)).unwrap();
    assert_eq!(partie.len(), 1440);
    assert_eq!(partie.open_time_ms().first(), Some(&ms(2019, 1, 1)));

    // Partition absente : série vide.
    let vide = read_series_window(racine.path(), &partition("OTH-USD"), None).unwrap();
    assert!(vide.is_empty());
}

#[test]
#[allow(clippy::single_range_in_vec_init)] // une plage de minutes = un trou
fn lecture_verifiee_refuse_un_fichier_altere() {
    let racine = tempfile::tempdir().unwrap();
    let autre = tempfile::tempdir().unwrap();
    ecrire(racine.path(), "SYN-USD", &serie(ms(2019, 1, 1), 2000, &[]));
    // Même partition, autres valeurs : on remplace le Parquet de la première par celui de la seconde.
    ecrire(
        autre.path(),
        "SYN-USD",
        &serie(ms(2019, 1, 1), 2000, &[5..6]),
    );
    let relatif = "data/candles/kraken/SYN-USD/1m/2019-01.parquet";
    fs::copy(autre.path().join(relatif), racine.path().join(relatif)).unwrap();

    for fenetre in [
        None,
        Some(Window::new(ms(2019, 1, 1), ms(2019, 2, 1)).unwrap()),
    ] {
        let erreur = read_series_window(racine.path(), &partition("SYN-USD"), fenetre).unwrap_err();
        assert!(
            matches!(erreur, StoreError::FileHashMismatch { .. }),
            "{erreur}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Fichiers d'univers
// ---------------------------------------------------------------------------------------------

fn hash(octet: &str) -> String {
    format!("blake3:{}", octet.repeat(32))
}

fn ligne(rang: u32, base: &str, tfs: Vec<Timeframe>) -> CsvRow {
    CsvRow {
        rank: rang,
        base: base.to_string(),
        symbol: format!("{base}-USD"),
        kraken_pair: format!("{base}USD"),
        quote: "USD".to_string(),
        volume_quote_90d: 1234.5 * f64::from(rang),
        first_active: "2016-03-01T00:00:00Z".to_string(),
        longest_gap_365d_ms: 3_600_000,
        last_close: 0.300_000_000_000_000_04,
        excluded_tfs: tfs,
        input_hash: hash("ab"),
    }
}

fn tfs_qualite() -> Vec<Timeframe> {
    vec![
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H4,
    ]
}

fn regles() -> UniverseRules {
    UniverseRules {
        top_n: 12,
        history_days: 365,
        quote_gap_ms: 604_800_000,
        liquidity_window_days: 90,
        min_price: 0.01,
        quotes: vec!["USD".into(), "USDT".into()],
        quote_preference: vec!["USD".into(), "USDT".into()],
        excluded_bases: vec!["EUR".into(), "USDT".into()],
        quality_threshold: 0.01,
        quality_tfs: vec![
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::M30,
            Timeframe::H1,
            Timeframe::H4,
            Timeframe::H12,
            Timeframe::D1,
        ],
        quality_uses_quarter_data: true,
        rejection_uses_full_series: true,
        config_file_hash: hash("cd"),
        rules_version: "u1".to_string(),
    }
}

/// Un trimestre synthétique : `SYN` (rang 1) et, si demandé, `OTH` (rang 2).
fn trimestre(label: &str, avec_oth: bool, tfs_syn: Vec<Timeframe>) -> QuarterBuild {
    let mut rows = vec![ligne(1, "SYN", tfs_syn)];
    if avec_oth {
        rows.push(ligne(2, "OTH", Vec::new()));
    }
    let quality_hashes = rows
        .iter()
        .map(|r| (r.symbol.clone(), hash("ee")))
        .collect();
    QuarterBuild {
        quarter: label.to_string(),
        eligible: u32::try_from(rows.len()).unwrap() + 3,
        rows,
        quality_hashes,
    }
}

fn regles_instruments() -> Vec<InstrumentRule> {
    vec![
        InstrumentRule {
            symbol: "SYN-USD".to_string(),
            tick_size: 0.1,
            lot_size: 1e-8,
            min_qty: 0.0,
            min_notional: 0.0,
        },
        InstrumentRule {
            symbol: "BTC-USD".to_string(),
            tick_size: 0.1,
            lot_size: 1e-8,
            min_qty: 0.0,
            min_notional: 0.0,
        },
        InstrumentRule {
            symbol: "OTH-USD".to_string(),
            tick_size: 0.0001,
            lot_size: 1e-8,
            min_qty: 0.0,
            min_notional: 0.0,
        },
    ]
}

/// Univers synthétique qui couvre `[2018-Q1, dernier]` pour `SYN` (avec `OTH` à partir de 2019).
fn univers(derniere_annee: i64, dernier_trimestre: u32) -> UniverseBuild {
    let mut quarters = Vec::new();
    for annee in 2018..=derniere_annee {
        for q in 1..=4_u32 {
            if annee == derniere_annee && q > dernier_trimestre {
                break;
            }
            let label = format!("{annee}-Q{q}");
            let tfs = if label == "2019-Q1" {
                tfs_qualite()
            } else {
                Vec::new()
            };
            quarters.push(trimestre(&label, annee >= 2019, tfs));
        }
    }
    UniverseBuild {
        source: UniverseSource {
            kind: "kraken_ohlcvt_zip".to_string(),
            zip_sha256: "0f".repeat(32),
            release: "r1".to_string(),
            coverage_end: "2020-01-01T00:00:00Z".to_string(),
            pairs_in_zip: 7,
            candidates: 3,
        },
        rules: regles(),
        quarters,
        at: vec![AtBuild {
            date: "2018-12-31T00:00:00Z".to_string(),
            rows: vec![ligne(1, "SYN", Vec::new()), ligne(2, "OTH", Vec::new())],
        }],
        rejected_pairs: vec![
            RejectedPair {
                kraken_pair: "ZZZUSD".to_string(),
                reasons: vec![
                    "ohlc_inconsistent".to_string(),
                    "divergent_duplicate".to_string(),
                ],
            },
            RejectedPair {
                kraken_pair: "AAAUSD".to_string(),
                reasons: vec!["divergent_duplicate".to_string()],
            },
        ],
        unrecognized_pairs: vec!["XXBTZUSD".to_string(), "ABCDEFUSD".to_string()],
        union: vec![
            UnionEntry {
                symbol: "SYN-USD".to_string(),
                kraken_pair: "SYNUSD".to_string(),
                first_quarter: "2018-Q1".to_string(),
            },
            UnionEntry {
                symbol: "OTH-USD".to_string(),
                kraken_pair: "OTHUSD".to_string(),
                first_quarter: "2019-Q1".to_string(),
            },
        ],
        instruments: regles_instruments(),
        instruments_method: "decimals_v1".to_string(),
    }
}

#[test]
fn csv_canonique_aller_retour_et_formes_refusees() {
    let lignes = vec![
        ligne(1, "SYN", vec![Timeframe::H1, Timeframe::H4]),
        ligne(2, "OTH", Vec::new()),
    ];
    let texte = render_csv("test.csv", &lignes).unwrap();
    let mut it = texte.lines();
    assert_eq!(it.next(), Some(universe::CSV_HEADER));
    assert_eq!(
        it.next(),
        Some(
            "1,SYN,SYN-USD,SYNUSD,USD,1234.5,2016-03-01T00:00:00Z,3600000,0.30000000000000004,1h;4h,\
             blake3:abababababababababababababababababababababababababababababababab"
        )
    );
    assert!(texte.ends_with('\n') && !texte.contains('\r'));
    assert_eq!(parse_csv("test.csv", &texte).unwrap(), lignes);

    // Formes non canoniques ou invalides.
    let crlf = texte.replace('\n', "\r\n");
    let mauvaise_entete = texte.replacen("rank", "rang", 1);
    let colonne_en_moins = texte.replacen(",1h;4h", "", 1);
    let rang_saute = texte.replacen("\n2,OTH", "\n3,OTH", 1);
    let un_espace = texte.replacen("SYN-USD", "SYN-USD ", 1);
    let un_m1 = texte.replacen("1h;4h", "1m;4h", 1);
    let desordre = texte.replacen("1h;4h", "4h;1h", 1);
    let zero_inutile = texte.replacen("1234.5", "1234.50", 1);
    for abime in [
        crlf,
        mauvaise_entete,
        colonne_en_moins,
        rang_saute,
        un_espace,
        un_m1,
        desordre,
        zero_inutile,
    ] {
        let erreur = parse_csv("test.csv", &abime).unwrap_err();
        assert!(
            matches!(erreur, StoreError::UniverseFileInvalid { .. }),
            "{erreur}"
        );
    }
    // Le rendu refuse aussi une ligne qui contredit sa base ou une liste non ordonnée par rang.
    let mut incoherente = ligne(1, "SYN", Vec::new());
    incoherente.symbol = "OTH-USD".to_string();
    assert!(render_csv("t.csv", &[incoherente]).is_err());
    assert!(render_csv("t.csv", &[ligne(2, "SYN", Vec::new())]).is_err());
}

#[test]
fn ecriture_et_relecture_verifiees_d_un_univers() {
    let dossier = tempfile::tempdir().unwrap();
    let fichier = universe::write_universe(dossier.path(), &univers(2019, 1)).unwrap();

    // Fichiers produits.
    let mut noms: Vec<String> = fs::read_dir(dossier.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    noms.sort();
    assert_eq!(
        noms,
        [
            "eligible_2018-12-31.csv",
            "instruments_kraken.toml",
            "universe.json",
            "universe_2018-Q1.csv",
            "universe_2018-Q2.csv",
            "universe_2018-Q3.csv",
            "universe_2018-Q4.csv",
            "universe_2019-Q1.csv",
        ]
    );
    // Listes diagnostiques triées en ordre d'octets à l'écriture.
    assert_eq!(fichier.rejected_pairs[0].kraken_pair, "AAAUSD");
    assert_eq!(
        fichier.rejected_pairs[1].reasons,
        ["divergent_duplicate", "ohlc_inconsistent"]
    );
    assert_eq!(fichier.unrecognized_pairs, ["ABCDEFUSD", "XXBTZUSD"]);
    // Pas d'horodatage de construction : deux écritures donnent les mêmes octets.
    let autre = tempfile::tempdir().unwrap();
    universe::write_universe(autre.path(), &univers(2019, 1)).unwrap();
    for nom in [
        "universe.json",
        "universe_2019-Q1.csv",
        "instruments_kraken.toml",
    ] {
        assert_eq!(
            fs::read(dossier.path().join(nom)).unwrap(),
            fs::read(autre.path().join(nom)).unwrap(),
            "{nom}"
        );
    }
    let json = fs::read_to_string(dossier.path().join("universe.json")).unwrap();
    assert!(
        json.ends_with("}\n") && json.starts_with("{\n  \"at\""),
        "{json}"
    );

    // Relecture : hashes vérifiés, calendrier construit.
    let charge = universe::load(dossier.path()).unwrap();
    assert_eq!(charge.file(), &fichier);
    assert_eq!(charge.quarters().len(), 5);
    let q1_2019 = ms(2019, 1, 15);
    assert_eq!(
        charge.schedule().rank_at(q1_2019, "SYN-USD"),
        Some(1),
        "rang du membre"
    );
    assert_eq!(charge.schedule().rank_at(q1_2019, "OTH-USD"), Some(2));
    assert_eq!(charge.schedule().rank_at(ms(2018, 6, 1), "OTH-USD"), None);
    assert_eq!(
        charge.union_symbols(ms(2018, 1, 1), ms(2019, 4, 1)),
        ["SYN-USD", "OTH-USD"]
    );
    assert_eq!(
        charge.union_symbols(ms(2018, 1, 1), ms(2018, 12, 31)),
        ["SYN-USD"]
    );
    // Les exclusions des CSV, pour les trimestres de la fenêtre seulement.
    let attendu = charge.quality_expectation(ms(2019, 1, 1), ms(2019, 2, 1));
    assert_eq!(
        attendu
            .entries()
            .get(&("2019-Q1".to_string(), "SYN-USD".to_string())),
        Some(&tfs_qualite())
    );
    assert_eq!(attendu.entries().len(), 2);
}

#[test]
fn les_regles_d_instrument_ecrites_se_relisent_comme_config_instruments() {
    let dossier = tempfile::tempdir().unwrap();
    universe::write_universe(dossier.path(), &univers(2018, 1)).unwrap();
    let charge = universe::load(dossier.path()).unwrap();
    let texte = charge.instruments_text();
    assert!(texte.contains("Méthode : decimals_v1. Dérivé du dump, minimums non mesurés."));
    // Symboles et clés triés.
    let btc = texte.find("[kraken.BTC-USD]").unwrap();
    let oth = texte.find("[kraken.OTH-USD]").unwrap();
    let syn = texte.find("[kraken.SYN-USD]").unwrap();
    assert!(btc < oth && oth < syn);
    assert!(texte.contains(
        "[kraken.OTH-USD]\nlot_size = 1e-8\nmin_notional = 0.0\nmin_qty = 0.0\ntick_size = 0.0001\n"
    ));

    let lues =
        |symbole: &str| InstrumentRules::from_toml_str(ExchangeId::Kraken, symbole, texte).unwrap();
    // La règle de BTC-USD égale celle de `config/instruments.toml` (tick 0,1, lot 1e-8, minimums 0).
    let config = InstrumentRules::load(
        ExchangeId::Kraken,
        "BTC-USD",
        &common::repo_path("config/instruments.toml"),
    )
    .unwrap();
    assert_eq!(lues("BTC-USD"), config);
    assert_eq!(lues("OTH-USD").tick_size, 0.0001);
    assert!(matches!(
        InstrumentRules::from_toml_str(ExchangeId::Kraken, "ABS-USD", texte),
        Err(ModelError::InstrumentNotFound { .. })
    ));
    assert_eq!(
        charge.instruments_path(),
        dossier.path().join("instruments_kraken.toml")
    );

    // Règles invalides : tick nul, doublon, méthode vide.
    let mut mauvais = univers(2018, 1);
    mauvais.instruments[0].tick_size = 0.0;
    assert!(universe::write_universe(tempfile::tempdir().unwrap().path(), &mauvais).is_err());
    let mut double = univers(2018, 1);
    double.instruments.push(double.instruments[0].clone());
    assert!(universe::write_universe(tempfile::tempdir().unwrap().path(), &double).is_err());
    let mut sans_methode = univers(2018, 1);
    sans_methode.instruments_method = String::new();
    assert!(universe::write_universe(tempfile::tempdir().unwrap().path(), &sans_methode).is_err());
}

#[test]
fn identite_de_l_univers_window_hash() {
    let un = tempfile::tempdir().unwrap();
    let deux = tempfile::tempdir().unwrap();
    let trois = tempfile::tempdir().unwrap();
    universe::write_universe(un.path(), &univers(2019, 1)).unwrap();

    // Plus de trimestres, une autre source, un autre `--at`, d'autres diagnostics : même identité pour
    // les trimestres déjà lus.
    let mut plus_long = univers(2019, 3);
    plus_long.source.release = "r2".to_string();
    plus_long.source.coverage_end = "2020-06-01T00:00:00Z".to_string();
    plus_long.at.clear();
    plus_long.unrecognized_pairs.clear();
    universe::write_universe(deux.path(), &plus_long).unwrap();

    // Une ligne de CSV change : l'identité de la fenêtre qui la lit change.
    let mut modifie = univers(2019, 1);
    modifie.quarters[4].rows[0].last_close = 0.5;
    universe::write_universe(trois.path(), &modifie).unwrap();

    let (a, b) = (ms(2018, 1, 1), ms(2019, 2, 15));
    let h1 = universe::load(un.path())
        .unwrap()
        .window_hash(a, b)
        .unwrap();
    let h2 = universe::load(deux.path())
        .unwrap()
        .window_hash(a, b)
        .unwrap();
    let h3 = universe::load(trois.path())
        .unwrap()
        .window_hash(a, b)
        .unwrap();
    assert_eq!(h1, h2);
    assert_ne!(h1, h3);
    // Une fenêtre qui ne lit pas le trimestre modifié garde son identité.
    let avant = universe::load(un.path())
        .unwrap()
        .window_hash(a, ms(2018, 12, 31))
        .unwrap();
    let avant3 = universe::load(trois.path())
        .unwrap()
        .window_hash(a, ms(2018, 12, 31))
        .unwrap();
    assert_eq!(avant, avant3);
    assert_ne!(avant, h1);

    let charge = universe::load(un.path()).unwrap();
    let reference = charge.reference(a, b).unwrap();
    assert_eq!(reference.window_hash, h1);
    assert_eq!(reference.rules_hash, charge.rules_hash());
    // Fenêtre non couverte.
    let erreur = charge.window_hash(a, ms(2019, 7, 1)).unwrap_err();
    assert!(
        matches!(erreur, StoreError::UniverseWindowNotCovered { missing_ms, .. } if missing_ms == ms(2019, 4, 1)),
        "{erreur}"
    );
    assert!(charge.window_hash(ms(2017, 1, 1), b).is_err());
}

#[test]
fn trimestres_figes_et_regles_immuables() {
    let dossier = tempfile::tempdir().unwrap();
    universe::write_universe(dossier.path(), &univers(2019, 1)).unwrap();
    let json = dossier.path().join("universe.json");
    let csv = dossier.path().join("universe_2018-Q2.csv");
    let (json_avant, csv_avant) = (fs::read(&json).unwrap(), fs::read(&csv).unwrap());

    // Un trimestre figé qui changerait : refusé, rien n'est écrit.
    let mut change = univers(2019, 2);
    change.quarters[1].rows[0].volume_quote_90d = 7.0;
    let erreur = universe::write_universe(dossier.path(), &change).unwrap_err();
    assert!(
        matches!(&erreur, StoreError::FrozenQuarterChanged { quarter } if quarter == "2018-Q2"),
        "{erreur}"
    );
    // Même contenu de CSV mais une autre entrée (éligibles) : refusé aussi.
    let mut eligibles = univers(2019, 2);
    eligibles.quarters[0].eligible += 1;
    assert!(matches!(
        universe::write_universe(dossier.path(), &eligibles),
        Err(StoreError::FrozenQuarterChanged { .. })
    ));
    // Un dump plus court qui perdrait un trimestre figé : refusé.
    assert!(matches!(
        universe::write_universe(dossier.path(), &univers(2018, 4)),
        Err(StoreError::FrozenQuarterChanged { quarter }) if quarter == "2019-Q1"
    ));
    // Des règles différentes : refusé.
    let mut autre_regle = univers(2019, 2);
    autre_regle.rules.top_n = 11;
    assert!(matches!(
        universe::write_universe(dossier.path(), &autre_regle),
        Err(StoreError::UniverseRulesChanged)
    ));
    assert_eq!(fs::read(&json).unwrap(), json_avant);
    assert_eq!(fs::read(&csv).unwrap(), csv_avant);

    // Un trimestre de plus : accepté ; les trimestres figés gardent leurs octets.
    let nouveau = universe::write_universe(dossier.path(), &univers(2019, 2)).unwrap();
    assert_eq!(nouveau.quarters.len(), 6);
    assert_eq!(fs::read(&csv).unwrap(), csv_avant);
    // Rejouer la même écriture est sans effet.
    let json_apres = fs::read(&json).unwrap();
    universe::write_universe(dossier.path(), &univers(2019, 2)).unwrap();
    assert_eq!(fs::read(&json).unwrap(), json_apres);
}

/// Un univers plus long, avec une règle d'instrument et une date `--at` qui changent : les fichiers non
/// figés (`instruments_kraken.toml`, `eligible_<date>.csv`) diffèrent de ceux de `univers(2019, 1)`.
fn univers_prolonge_aux_fichiers_non_figes_differents() -> UniverseBuild {
    let mut build = univers(2019, 2);
    build.instruments[2].tick_size = 0.001;
    build.at[0].rows = vec![ligne(1, "SYN", Vec::new())];
    build
}

#[test]
fn une_ecriture_interrompue_avant_universe_json_se_rejoue() {
    let nouveau = univers_prolonge_aux_fichiers_non_figes_differents();
    // Dossier de référence : le résultat d'une écriture complète.
    let reference = tempfile::tempdir().unwrap();
    universe::write_universe(reference.path(), &univers(2019, 1)).unwrap();
    universe::write_universe(reference.path(), &nouveau).unwrap();

    // Un process interrompu après l'écriture d'une partie des fichiers, avant `universe.json` ; chaque
    // cas copie, de la reprise complète, le sous-ensemble de fichiers écrits avant l'arrêt.
    let cas: [(&str, &[&str]); 3] = [
        ("instruments seul", &["instruments_kraken.toml"]),
        ("date --at seule", &["eligible_2018-12-31.csv"]),
        (
            "instruments, date --at et nouveau trimestre",
            &[
                "instruments_kraken.toml",
                "eligible_2018-12-31.csv",
                "universe_2019-Q2.csv",
            ],
        ),
    ];
    for (nom, fichiers) in cas {
        let dossier = tempfile::tempdir().unwrap();
        universe::write_universe(dossier.path(), &univers(2019, 1)).unwrap();
        for fichier in fichiers {
            fs::copy(reference.path().join(fichier), dossier.path().join(fichier)).unwrap();
        }
        // L'état interrompu est incohérent : la relecture complète le refuse (verify, dataset build).
        assert!(
            matches!(
                universe::load(dossier.path()),
                Err(StoreError::UniverseHashMismatch { .. })
            ),
            "{nom}"
        );
        // Rejouer la commande répare : seule la partie figée est relue avant l'écriture.
        universe::write_universe(dossier.path(), &nouveau).unwrap();
        universe::load(dossier.path()).unwrap();
        for fichier in [
            "universe.json",
            "instruments_kraken.toml",
            "eligible_2018-12-31.csv",
            "universe_2019-Q2.csv",
        ] {
            assert_eq!(
                fs::read(dossier.path().join(fichier)).unwrap(),
                fs::read(reference.path().join(fichier)).unwrap(),
                "{nom} : {fichier}"
            );
        }
    }
}

#[test]
fn un_csv_de_trimestre_fige_altere_reste_refuse_a_la_reecriture() {
    let dossier = tempfile::tempdir().unwrap();
    universe::write_universe(dossier.path(), &univers(2019, 1)).unwrap();
    let csv = dossier.path().join("universe_2018-Q3.csv");
    let mut octets = fs::read(&csv).unwrap();
    octets.insert(octets.len() - 1, b'0');
    fs::write(&csv, &octets).unwrap();
    let json_avant = fs::read(dossier.path().join("universe.json")).unwrap();

    let erreur = universe::write_universe(dossier.path(), &univers(2019, 2)).unwrap_err();
    assert!(
        matches!(erreur, StoreError::UniverseHashMismatch { .. }),
        "{erreur}"
    );
    // Rien n'est écrit : ni le CSV altéré, ni `universe.json`, ni le trimestre ajouté.
    assert_eq!(fs::read(&csv).unwrap(), octets);
    assert_eq!(
        fs::read(dossier.path().join("universe.json")).unwrap(),
        json_avant
    );
    assert!(!dossier.path().join("universe_2019-Q2.csv").exists());
}

#[test]
fn relecture_refuse_tout_fichier_altere() {
    type Alteration = (&'static str, fn(&Path));
    let alterations: [Alteration; 5] = [
        ("csv de trimestre", |d| {
            let p = d.join("universe_2018-Q3.csv");
            let mut octets = fs::read(&p).unwrap();
            octets.insert(octets.len() - 1, b'0');
            fs::write(p, octets).unwrap();
        }),
        ("csv d'une date --at", |d| {
            let p = d.join("eligible_2018-12-31.csv");
            let texte = fs::read_to_string(&p).unwrap().replace("OTH", "OTX");
            fs::write(p, texte).unwrap();
        }),
        ("instruments", |d| {
            let p = d.join("instruments_kraken.toml");
            let texte = fs::read_to_string(&p).unwrap().replace("0.1", "0.2");
            fs::write(p, texte).unwrap();
        }),
        ("report_hash (diagnostic modifié)", |d| {
            let p = d.join("universe.json");
            let texte = fs::read_to_string(&p)
                .unwrap()
                .replace("XXBTZUSD", "XXETHZUSD");
            fs::write(p, texte).unwrap();
        }),
        (
            "rules_hash (règle modifiée, report_hash recalculé)",
            |d| {
                let p = d.join("universe.json");
                let mut fichier: universe::UniverseFile =
                    serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
                fichier.rules.top_n = 11;
                fichier.report_hash = fichier.compute_report_hash().unwrap();
                fs::write(&p, fichier.to_json_pretty().unwrap()).unwrap();
            },
        ),
    ];
    for (nom, alterer) in alterations {
        let dossier = tempfile::tempdir().unwrap();
        universe::write_universe(dossier.path(), &univers(2019, 1)).unwrap();
        assert!(universe::load(dossier.path()).is_ok());
        alterer(dossier.path());
        let erreur = universe::load(dossier.path()).unwrap_err();
        assert!(
            matches!(
                erreur,
                StoreError::UniverseHashMismatch { .. } | StoreError::UniverseFileInvalid { .. }
            ),
            "{nom} : {erreur}"
        );
        if nom.starts_with("rules_hash") {
            assert!(
                matches!(
                    erreur,
                    StoreError::UniverseHashMismatch {
                        field: "rules_hash",
                        ..
                    }
                ),
                "{erreur}"
            );
        }
    }
    // Un champ inconnu dans universe.json est refusé (un format qui bouge ne passe pas en silence).
    let dossier = tempfile::tempdir().unwrap();
    universe::write_universe(dossier.path(), &univers(2018, 1)).unwrap();
    let p = dossier.path().join("universe.json");
    let mut valeur: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    valeur["champ_futur"] = serde_json::json!(1);
    fs::write(&p, serde_json::to_vec_pretty(&valeur).unwrap()).unwrap();
    assert!(matches!(
        universe::load(dossier.path()),
        Err(StoreError::Json { .. })
    ));
    // Un dossier sans universe.json.
    assert!(matches!(
        universe::load(tempfile::tempdir().unwrap().path()),
        Err(StoreError::Io { .. })
    ));
}

#[test]
fn ecriture_refuse_les_contenus_incoherents() {
    let ecrit = |build: &UniverseBuild| {
        universe::write_universe(tempfile::tempdir().unwrap().path(), build)
    };
    // Trimestres dans le désordre.
    let mut desordre = univers(2018, 2);
    desordre.quarters.swap(0, 1);
    assert!(ecrit(&desordre).is_err());
    // quality_hashes qui ne porte pas exactement les membres.
    let mut hashes = univers(2018, 1);
    hashes.quarters[0].quality_hashes.clear();
    assert!(ecrit(&hashes).is_err());
    // Plus de membres que top_n.
    let mut trop = univers(2019, 1);
    trop.rules.top_n = 1;
    assert!(ecrit(&trop).is_err());
    // Moins d'éligibles que de membres.
    let mut eligibles = univers(2018, 1);
    eligibles.quarters[0].eligible = 0;
    assert!(ecrit(&eligibles).is_err());
    // TF exclu hors de quality_tfs, ou le 1m.
    let mut hors = univers(2018, 1);
    hors.rules.quality_tfs = vec![Timeframe::D1];
    hors.quarters[0].rows[0].excluded_tfs = vec![Timeframe::H1];
    assert!(ecrit(&hors).is_err());
    let mut m1 = univers(2018, 1);
    m1.rules.quality_tfs = vec![Timeframe::M1, Timeframe::D1];
    assert!(ecrit(&m1).is_err());
    // Deux dates --at le même jour.
    let mut meme_jour = univers(2018, 1);
    meme_jour.at.push(AtBuild {
        date: "2018-12-31T12:00:00Z".to_string(),
        rows: Vec::new(),
    });
    assert!(ecrit(&meme_jour).is_err());
    // Version de règles inattendue.
    let mut version = univers(2018, 1);
    version.rules.rules_version = "u2".to_string();
    assert!(ecrit(&version).is_err());
    // Union avec un doublon.
    let mut union = univers(2018, 1);
    union.union.push(union.union[0].clone());
    assert!(ecrit(&union).is_err());
}

// ---------------------------------------------------------------------------------------------
// Dataset de recherche avec univers
// ---------------------------------------------------------------------------------------------

#[test]
fn dataset_de_recherche_avec_univers_ecrit_la_reference_et_compare_les_csv() {
    let racine = tempfile::tempdir().unwrap();
    ecrire_t1_2019(racine.path(), "SYN-USD");
    ecrire_t1_2019(racine.path(), "OTH-USD");
    let dossier = tempfile::tempdir().unwrap();
    universe::write_universe(dossier.path(), &univers(2019, 1)).unwrap();
    let charge = universe::load(dossier.path()).unwrap();

    let (debut, fin) = (ms(2018, 1, 1), ms(2019, 2, 15));
    let symboles_univers = charge.union_symbols(debut, fin);
    assert_eq!(symboles_univers, ["SYN-USD", "OTH-USD"]);
    let symboles: Vec<Symbol> = symboles_univers.iter().map(|s| sym(s)).collect();
    let meta = ResearchMeta {
        block: Block::Dev,
        universe: Some(charge.reference(debut, fin).unwrap()),
        universe_quality: Some(charge.quality_expectation(debut, fin)),
    };
    // `OTH-USD` a les mêmes trous que `SYN-USD` dans le store, alors que son CSV ne lui en annonce
    // aucun : l'écart est signalé (un bug d'ingestion ou d'univers).
    let erreur = build_dataset_research(
        racine.path(),
        &requete(
            "univers",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &meta,
        FUTUR,
    )
    .unwrap_err();
    assert!(
        matches!(&erreur, StoreError::UniverseQualityMismatch { symbol, .. } if symbol == "OTH-USD"),
        "{erreur}"
    );

    // Avec un store propre pour OTH-USD (aucun trou), les deux sources s'accordent.
    let propre = tempfile::tempdir().unwrap();
    ecrire_t1_2019(propre.path(), "SYN-USD");
    ecrire(
        propre.path(),
        "OTH-USD",
        &serie(ms(2019, 1, 1), T1_2019_MIN, &[]),
    );
    let sortie = build_dataset_research(
        propre.path(),
        &requete(
            "univers",
            &symboles,
            fenetre_courte(),
            GapsPolicy::ZeroVolumeMarked,
        ),
        &meta,
        FUTUR,
    )
    .unwrap();
    assert_eq!(sortie.manifest.universe, meta.universe);
    assert_eq!(
        sortie.manifest.quality_exclusions,
        Some(exclusions_attendues())
    );
    let info = research_meta(&sortie.manifest).unwrap().unwrap();
    assert_eq!(info.universe, meta.universe);
    // La réécriture identique est inchangée ; le manifeste garde les symboles triés des deux jambes.
    assert_eq!(sortie.manifest.symbols, [sym("OTH-USD"), sym("SYN-USD")]);
    assert_eq!(
        build_dataset_research(
            propre.path(),
            &requete(
                "univers",
                &symboles,
                fenetre_courte(),
                GapsPolicy::ZeroVolumeMarked
            ),
            &meta,
            FUTUR,
        )
        .unwrap()
        .status,
        BuildStatus::Unchanged
    );
}

// ---------------------------------------------------------------------------------------------
// Liste figée de paires, fin de couverture
// ---------------------------------------------------------------------------------------------

fn sous_ensemble(bases: &[&str]) -> PairSubset {
    PairSubset::new(
        "okx_usdc_tradable",
        SubsetSource {
            instruments_file: "spot-20261030T101500Z.json".to_string(),
            instruments_hash: hash("77"),
            site: "eea".to_string(),
            fetched_at: "2026-10-30T10:15:00Z".to_string(),
        },
        SubsetRule {
            quote: "USDC".to_string(),
            state: "live".to_string(),
            trade_quote_ccy: true,
            aliases: BTreeMap::new(),
        },
        bases.iter().map(|b| (*b).to_string()).collect(),
    )
    .unwrap()
}

#[test]
fn liste_figee_ecrite_relue_et_jamais_ecrasee() {
    let dossier = tempfile::tempdir().unwrap();
    let chemin = dossier.path().join("okx_usdc.json");
    let liste = sous_ensemble(&["SOL", "BTC", "ETH"]);
    universe::write_pair_subset(&chemin, &liste).unwrap();
    let relue = universe::read_pair_subset(&chemin).unwrap();
    assert_eq!(relue, liste);
    assert_eq!(relue.bases, ["BTC", "ETH", "SOL"]);
    assert!(relue.contains_base("ETH") && !relue.contains_base("ADA"));
    let octets = fs::read(&chemin).unwrap();
    assert!(octets.ends_with(b"}\n"));

    // Jamais écrasée.
    let erreur = universe::write_pair_subset(&chemin, &sous_ensemble(&["BTC"])).unwrap_err();
    assert!(
        matches!(erreur, StoreError::SubsetExists { .. }),
        "{erreur}"
    );
    assert_eq!(fs::read(&chemin).unwrap(), octets);

    // Altérée à la main : le hash la refuse.
    let abimee = String::from_utf8(octets)
        .unwrap()
        .replace("\"ETH\"", "\"ADA\"");
    fs::write(&chemin, abimee).unwrap();
    assert!(matches!(
        universe::read_pair_subset(&chemin),
        Err(StoreError::Model(ModelError::UniverseInvalid { .. }))
    ));
    // Un fichier absent est une erreur d'E/S.
    assert!(matches!(
        universe::read_pair_subset(&dossier.path().join("absent.json")),
        Err(StoreError::Io { .. })
    ));
    // Le hash ne dépend ni de `fetched_at` ni du nom du fichier source.
    let mut autre = sous_ensemble(&["BTC", "ETH", "SOL"]);
    autre.source.fetched_at = "2027-01-01T00:00:00Z".to_string();
    autre.source.instruments_file = "autre.json".to_string();
    assert_eq!(autre.compute_hash().unwrap(), liste.subset_hash);
}

#[test]
fn fin_de_couverture_dans_le_store() {
    // Copie de la règle de l'ingestion, déplacée dans `store::time` (S11 lot B).
    assert_eq!(parse_coverage_end("2026-09-30"), Some(ms(2026, 10, 1)));
    assert_eq!(
        parse_coverage_end("2026-10-01T00:00:00Z"),
        parse_rfc3339_utc("2026-10-01T00:00:00Z").ok()
    );
    assert_eq!(parse_coverage_end("hier"), None);
}
