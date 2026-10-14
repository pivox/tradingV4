//! Tests d'intégration du store (docs/03 §5, S4) : aller-retour, fichier golden et fixture réelle.
//!
//! Les bougies 1 000 sont synthétiques (générateur de `common`, prix inventés). Le fichier golden
//! contient les trois bougies du vecteur de `docs/dataset-hash.md` §7 ; la fixture réelle est la
//! tranche XBTUSD de `fixtures/`. Aucun type arrow ou parquet ici : tout passe par l'API du store.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use tv4::model::{Candle, ExchangeId, Timeframe};
use tv4::store::{
    BuildRequest, Columns, ContinuityInput, GapsPolicy, Partition, Period, Projection, ReadOptions,
    Window, WriteOptions, build_dataset, inspect_parquet, load_dataset, quality_report,
    read_file_manifest, read_parquet, write_partition, write_partition_with,
};

/// 2024-01-31T12:00:00Z : une série de 1 000 bougies avec trous y passe en février.
const T_31_JANV_12H: i64 = 1_706_702_400_000;
/// 2026-10-14T00:00:00Z : instant d'écriture des fichiers golden.
const NOW_GOLDEN: i64 = 1_791_936_000_000;

/// `content_hash` du mois 2024-01 des trois bougies du §7 (fenêtre du mois, `skip`).
const CONTENT_HASH_GOLDEN: &str =
    "blake3:3da10e85c1fdd2673a5cc9a0ea7dbd2bf5c171e3f11c23e04dff603392bdd8ab";
/// `dataset_hash` du vecteur du §7 (fenêtre `[1704067200000, 1704067380000)`, `skip`).
const DATASET_HASH_GOLDEN: &str =
    "blake3:a15f886ff621e7e10b7d4769793d818b23686441a2ce3d4dd6a4918b740cae8e";
/// `content_hash` du mois 2020-01 de `fixtures/kraken_xbtusd_1m.csv` (10 000 lignes).
const CONTENT_HASH_FIXTURE: &str =
    "blake3:d94f2eb46e2c4b6738b7ef4ed2441f3928db379e5b29f48f95532ba18c56f353";

/// `dataset_hash` de la fixture sur `[2020-01-01T00:00:00Z, 2020-01-09T00:00:00Z)`.
const DATASET_HASH_FIXTURE_SKIP: &str =
    "blake3:943075a3c2fe27072d3c2a9a1113580fbfcb04542c6ae2e7a82db44aaf54a495";
const DATASET_HASH_FIXTURE_FFILL: &str =
    "blake3:0685813dbc5085b4e87715cf5e7d54041bf478e7079d3b7cbcb68b6826e3697b";
const DATASET_HASH_FIXTURE_ZVM: &str =
    "blake3:dba3820a47d23a38a9ad913c81837ee34549eed0905dc2240079b34ad59e0dc8";

/// Empreinte blake3 des octets du Parquet golden (relevée à la production du fichier).
const GOLDEN_PARQUET_BLAKE3: &str =
    "fdb3082449843de1c929599cde12d128ea4502c59469212e6fed01bbf3abb87b";

/// Manifeste golden exact (docs/03 §5.2, écarts de S4 : `period`, pas de `continuity`).
const GOLDEN_MANIFEST: &str = r#"{
  "closed": true,
  "content_hash": "blake3:3da10e85c1fdd2673a5cc9a0ea7dbd2bf5c171e3f11c23e04dff603392bdd8ab",
  "exchange": "kraken",
  "hash_format": "dsh-v1",
  "ingested_at": "2026-10-14T00:00:00Z",
  "period": "2024-01",
  "row_count": 3,
  "source": "golden",
  "symbol": "BTC-USD",
  "tf": "1m",
  "ts_max": 1704067320000,
  "ts_min": 1704067200000
}
"#;

const GOLDEN_PARQUET: &str = "fixtures/golden/kraken_BTC-USD_1m_2024-01.parquet";
const GOLDEN_MANIFEST_FILE: &str = "fixtures/golden/kraken_BTC-USD_1m_2024-01._manifest.json";

fn partition(exchange: ExchangeId, symbol: &str, tf: Timeframe) -> Partition {
    Partition::new(exchange, symbol.parse().expect("symbole valide"), tf)
}

fn golden_partition() -> Partition {
    partition(ExchangeId::Kraken, "BTC-USD", Timeframe::M1)
}

/// Les trois bougies du vecteur de `docs/dataset-hash.md` §7.
fn bougies_golden() -> Vec<Candle> {
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

/// Égalité au bit : `==` sur des `f64` ne distingue pas `-0.0` de `0.0`.
fn assert_memes_bougies(lues: &[Candle], attendues: &[Candle]) {
    assert_eq!(lues.len(), attendues.len(), "nombre de bougies");
    for (i, (a, b)) in lues.iter().zip(attendues).enumerate() {
        assert_eq!(a.open_time_ms, b.open_time_ms, "open_time de la bougie {i}");
        for (nom, x, y) in [
            ("open", a.open, b.open),
            ("high", a.high, b.high),
            ("low", a.low, b.low),
            ("close", a.close, b.close),
            ("volume", a.volume, b.volume),
        ] {
            assert_eq!(x.to_bits(), y.to_bits(), "{nom} de la bougie {i}");
        }
        assert_eq!(a.trades, b.trades, "trades de la bougie {i}");
    }
}

fn chemin(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Copie les deux fichiers golden dans l'arborescence d'une racine temporaire.
fn installer_golden(root: &Path) {
    let p = golden_partition();
    let mois = Period::Month {
        year: 2024,
        month: 1,
    };
    for (source, rel) in [
        (GOLDEN_PARQUET, p.file_rel(mois)),
        (GOLDEN_MANIFEST_FILE, p.manifest_rel(mois)),
    ] {
        let destination = chemin(root, &rel);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(common::repo_path(source), destination).unwrap();
    }
}

#[test]
fn aller_retour_1000_bougies() {
    let mut candles = common::synthetic_candles(1_000, 11, T_31_JANV_12H, 0.05);
    // Le générateur n'écrit pas de `trades` : on en met un sur trois pour les exercer.
    for (i, c) in candles.iter_mut().enumerate() {
        c.trades = (i % 3 == 0).then_some(i as u32);
    }
    let dossier = tempfile::tempdir().unwrap();
    let p = partition(ExchangeId::Kraken, "SYN-USD", Timeframe::M1);
    let colonnes = Columns::from_candles(&candles);
    let sorties = write_partition(dossier.path(), &p, &colonnes, "synthetic", NOW_GOLDEN).unwrap();
    assert_eq!(sorties.len(), 2, "janvier et février");
    assert_eq!(sorties.iter().map(|s| s.row_count).sum::<u64>(), 1_000);

    // Lecture directe de chaque fichier.
    let mut relues = Vec::new();
    for sortie in &sorties {
        let lues = read_parquet(
            &chemin(dossier.path(), &sortie.file_rel),
            &ReadOptions::default(),
        )
        .unwrap();
        assert!(lues.synthetic().iter().all(|s| !s));
        relues.extend(lues.to_candles());
    }
    assert_memes_bougies(&relues, &candles);

    // Même contenu par build puis load (skip), sur une fenêtre qui couvre tout.
    let debut = candles[0].open_time_ms;
    let fin = candles[candles.len() - 1].open_time_ms + 60_000;
    let demande = BuildRequest {
        name: "aller_retour",
        partition: p,
        window: Window::new(debut, fin).unwrap(),
        gaps_policy: GapsPolicy::Skip,
    };
    let build = build_dataset(dossier.path(), &demande, NOW_GOLDEN).unwrap();
    assert_eq!(build.manifest.row_count, 1_000);
    assert_eq!(build.manifest.files.len(), 2);
    let charge = load_dataset(
        dossier.path(),
        &dossier.path().join("datasets/aller_retour.json"),
    )
    .unwrap();
    assert_eq!(charge.columns.synthetic_count(), 0);
    assert_memes_bougies(&charge.columns.to_candles(), &candles);
}

#[test]
fn golden_parquet_content_hash_et_dataset_hash() {
    let dossier = tempfile::tempdir().unwrap();
    installer_golden(dossier.path());
    let p = golden_partition();
    let mois = Period::Month {
        year: 2024,
        month: 1,
    };
    let fichier = chemin(dossier.path(), &p.file_rel(mois));

    // (a) content_hash recalculé, en dur, et égal au manifeste.
    let lues = read_parquet(&fichier, &ReadOptions::default()).unwrap();
    let (hex, lignes) = tv4::store::dsh::content_hash(&p, mois, &lues).unwrap();
    assert_eq!(format!("blake3:{hex}"), CONTENT_HASH_GOLDEN);
    assert_eq!(lignes, 3);
    let manifeste = read_file_manifest(&chemin(dossier.path(), &p.manifest_rel(mois))).unwrap();
    assert_eq!(manifeste.content_hash, CONTENT_HASH_GOLDEN);

    // (b) dataset_hash du vecteur du §7.
    let demande = BuildRequest {
        name: "golden",
        partition: p,
        window: Window::new(1_704_067_200_000, 1_704_067_380_000).unwrap(),
        gaps_policy: GapsPolicy::Skip,
    };
    let build = build_dataset(dossier.path(), &demande, NOW_GOLDEN).unwrap();
    assert_eq!(build.manifest.dataset_hash, DATASET_HASH_GOLDEN);
    assert_eq!(build.manifest.row_count, 3);

    // (c) chargement : les trois bougies du §7 au bit, le -0.0 du volume compris.
    let charge =
        load_dataset(dossier.path(), &dossier.path().join("datasets/golden.json")).unwrap();
    assert_memes_bougies(&charge.columns.to_candles(), &bougies_golden());
    assert_eq!(charge.columns.volume()[2].to_bits(), (-0.0_f64).to_bits());

    // (d) pied de page.
    let info = inspect_parquet(&fichier).unwrap();
    assert_eq!(info.created_by.as_deref(), Some("tv4"));
    assert_eq!(info.row_groups.len(), 1);
    assert_eq!(info.row_groups[0].min_open_time_ms, Some(1_704_067_200_000));
    assert_eq!(info.row_groups[0].max_open_time_ms, Some(1_704_067_320_000));
}

#[test]
fn golden_fichiers_epingles() {
    // Échoue si quelqu'un régénère le fichier sans mettre à jour cette empreinte, la spec et
    // fixtures/README.md (même principe que `fixture_kraken.rs::les_fixtures_ne_sont_pas_modifiees`).
    let octets = fs::read(common::repo_path(GOLDEN_PARQUET)).unwrap();
    assert_eq!(
        blake3::hash(&octets).to_hex().as_str(),
        GOLDEN_PARQUET_BLAKE3,
        "le Parquet golden a changé"
    );
    let manifeste = fs::read_to_string(common::repo_path(GOLDEN_MANIFEST_FILE)).unwrap();
    assert_eq!(manifeste, GOLDEN_MANIFEST, "le manifeste golden a changé");
}

#[test]
fn golden_reecrit_par_tv4_meme_content_hash() {
    // Les octets Parquet peuvent différer après une montée de version, le hash logique non.
    let dossier = tempfile::tempdir().unwrap();
    let colonnes = Columns::from_candles(&bougies_golden());
    let sorties = write_partition(
        dossier.path(),
        &golden_partition(),
        &colonnes,
        "golden",
        NOW_GOLDEN,
    )
    .unwrap();
    assert_eq!(sorties.len(), 1);
    assert_eq!(sorties[0].content_hash, CONTENT_HASH_GOLDEN);
    assert_eq!(sorties[0].row_count, 3);
    assert!(sorties[0].closed);
}

fn colonnes_fixture() -> Columns {
    Columns::from_candles(&common::fixture_1m())
}

#[test]
fn qualite_sur_la_fixture() {
    let colonnes = colonnes_fixture();
    let rapport = quality_report(colonnes.open_time_ms(), &Timeframe::ALL).unwrap();
    assert_eq!(rapport.first_open_time, "2020-01-01T00:00:00Z");
    assert_eq!(rapport.end_time, "2020-01-08T21:30:00Z");
    assert_eq!(rapport.threshold, 0.01);
    // (tf, attendues, présentes, absentes, plus long trou en ms) : le seul trimestre est 2020-Q1. La
    // fraction est `absentes / attendues`, comparée au bit.
    let attendu: [(Timeframe, u64, u64, u64, i64); 8] = [
        (Timeframe::M1, 11_370, 10_000, 1_370, 480_000),
        (Timeframe::M5, 2_274, 2_271, 3, 300_000),
        (Timeframe::M15, 758, 758, 0, 0),
        (Timeframe::M30, 379, 379, 0, 0),
        (Timeframe::H1, 189, 189, 0, 0),
        (Timeframe::H4, 47, 47, 0, 0),
        (Timeframe::H12, 15, 15, 0, 0),
        (Timeframe::D1, 7, 7, 0, 0),
    ];
    assert_eq!(rapport.rows.len(), 8);
    for (ligne, (tf, expected, present, missing, gap)) in rapport.rows.iter().zip(attendu) {
        assert_eq!((ligne.tf, ligne.quarter.as_str()), (tf, "2020-Q1"));
        assert_eq!(
            (ligne.expected, ligne.present, ligne.missing),
            (expected, present, missing)
        );
        assert_eq!(
            ligne.missing_fraction,
            missing as f64 / expected as f64,
            "{tf}"
        );
        assert_eq!(ligne.excluded, tf == Timeframe::M1, "{tf}");
        assert_eq!(ligne.longest_gap_ms, gap, "{tf}");
    }
    // Les deux valeurs de la spécification, relues du Python indépendant.
    assert_eq!(rapport.rows[0].missing_fraction, 0.12049252418645559);
    assert_eq!(rapport.rows[1].missing_fraction, 0.0013192612137203166);
}

#[test]
fn continuite_du_store_sur_la_fixture() {
    let dossier = tempfile::tempdir().unwrap();
    let p = partition(ExchangeId::Kraken, "BTC-USD", Timeframe::M1);
    let options = WriteOptions {
        source: "kraken-ohlcvt",
        now_ms: NOW_GOLDEN,
        closed_until_ms: NOW_GOLDEN,
        continuity: Some(ContinuityInput {
            duplicates: &[],
            rejected_unclosed: &[],
        }),
    };
    let sorties = write_partition_with(dossier.path(), &p, &colonnes_fixture(), &options).unwrap();
    assert_eq!(sorties[0].content_hash, CONTENT_HASH_FIXTURE);
    let bloc = sorties[0].continuity.unwrap();
    // 951 trous, le plus long de 8 minutes absentes d'affilée (mesuré sur la fixture réelle).
    assert_eq!((bloc.gaps, bloc.longest_gap_ms), (951, 480_000));
    assert_eq!(
        (bloc.duplicates, bloc.rejected_unclosed, bloc.ohlc_errors),
        (0, 0, 0)
    );
    let manifeste =
        read_file_manifest(&chemin(dossier.path(), &p.manifest_rel(sorties[0].period))).unwrap();
    assert_eq!(manifeste.continuity, Some(bloc));
}

#[test]
fn fixture_reelle_content_hash_2020_01() {
    let dossier = tempfile::tempdir().unwrap();
    let p = partition(ExchangeId::Kraken, "BTC-USD", Timeframe::M1);
    let sorties = write_partition(
        dossier.path(),
        &p,
        &colonnes_fixture(),
        "kraken-csv",
        NOW_GOLDEN,
    )
    .unwrap();
    assert_eq!(sorties.len(), 1, "une seule période, 2020-01");
    assert_eq!(sorties[0].period.label(), "2020-01");
    assert_eq!(sorties[0].content_hash, CONTENT_HASH_FIXTURE);
    assert_eq!(sorties[0].row_count, 10_000);
    // Relecture : mêmes bougies que le CSV, au bit.
    let relues = read_parquet(
        &chemin(dossier.path(), &sorties[0].file_rel),
        &ReadOptions::default(),
    )
    .unwrap();
    assert_memes_bougies(&relues.to_candles(), &common::fixture_1m());
}

#[test]
fn fixture_reelle_dataset_hash_trois_politiques() {
    let dossier = tempfile::tempdir().unwrap();
    let p = partition(ExchangeId::Kraken, "BTC-USD", Timeframe::M1);
    write_partition(
        dossier.path(),
        &p,
        &colonnes_fixture(),
        "kraken-csv",
        NOW_GOLDEN,
    )
    .unwrap();
    // [2020-01-01T00:00:00Z, 2020-01-09T00:00:00Z)
    let fenetre = Window::new(1_577_836_800_000, 1_578_528_000_000).unwrap();
    let cas = [
        (
            "fixture_skip",
            GapsPolicy::Skip,
            DATASET_HASH_FIXTURE_SKIP,
            10_000,
            0,
        ),
        (
            "fixture_ffill",
            GapsPolicy::Ffill,
            DATASET_HASH_FIXTURE_FFILL,
            11_370,
            1_370,
        ),
        (
            "fixture_zvm",
            GapsPolicy::ZeroVolumeMarked,
            DATASET_HASH_FIXTURE_ZVM,
            11_370,
            1_370,
        ),
    ];
    for (nom, politique, hash, bougies, synthetiques) in cas {
        let demande = BuildRequest {
            name: nom,
            partition: p.clone(),
            window: fenetre,
            gaps_policy: politique,
        };
        let build = build_dataset(dossier.path(), &demande, NOW_GOLDEN).unwrap();
        assert_eq!(build.manifest.dataset_hash, hash, "{nom}");
        assert_eq!(build.manifest.row_count, 10_000, "{nom}");
        let manifeste = dossier.path().join("datasets").join(format!("{nom}.json"));
        let charge = load_dataset(dossier.path(), &manifeste).unwrap();
        assert_eq!(charge.columns.len(), bougies, "{nom}");
        assert_eq!(charge.columns.synthetic_count(), synthetiques, "{nom}");
    }
}

#[test]
fn projection_six_colonnes_sur_la_fixture() {
    let dossier = tempfile::tempdir().unwrap();
    let p = partition(ExchangeId::Kraken, "BTC-USD", Timeframe::M1);
    let sorties = write_partition(
        dossier.path(),
        &p,
        &colonnes_fixture(),
        "kraken-csv",
        NOW_GOLDEN,
    )
    .unwrap();
    let lues = read_parquet(
        &chemin(dossier.path(), &sorties[0].file_rel),
        &ReadOptions {
            projection: Projection::NoTrades,
            window: Some(Window::new(1_577_836_800_000, 1_577_923_200_000).unwrap()),
            batch_size: 4096,
        },
    )
    .unwrap();
    assert!(lues.trades().is_none());
    assert!(!lues.is_empty());
    assert!(lues.open_time_ms().iter().all(|t| *t < 1_577_923_200_000));
}

/// Production des fichiers golden, une seule fois (jamais en CI). Ne fait rien sans
/// `TV4_REGENERER_GOLDEN=1` :
///
/// ```sh
/// TV4_REGENERER_GOLDEN=1 cargo test --locked --test store -- --ignored produire_les_fichiers_golden --nocapture
/// ```
///
/// Écrit les trois bougies du §7 par `write_partition` (source `golden`, instant 2026-10-14T00:00:00Z
/// pour un manifeste déterministe), copie les deux fichiers sous `fixtures/golden/` et affiche
/// l'empreinte blake3 du Parquet, à reporter dans `GOLDEN_PARQUET_BLAKE3` et `fixtures/README.md`.
#[test]
#[ignore = "production unique des fichiers golden : TV4_REGENERER_GOLDEN=1"]
fn produire_les_fichiers_golden() {
    if std::env::var("TV4_REGENERER_GOLDEN").as_deref() != Ok("1") {
        return;
    }
    let dossier = tempfile::tempdir().unwrap();
    let p = golden_partition();
    let colonnes = Columns::from_candles(&bougies_golden());
    let sorties = write_partition(dossier.path(), &p, &colonnes, "golden", NOW_GOLDEN).unwrap();
    assert_eq!(sorties[0].content_hash, CONTENT_HASH_GOLDEN);
    let mois = sorties[0].period;
    let cibles = [
        (p.file_rel(mois), GOLDEN_PARQUET),
        (p.manifest_rel(mois), GOLDEN_MANIFEST_FILE),
    ];
    for (rel, destination) in cibles {
        let destination = common::repo_path(destination);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(chemin(dossier.path(), &rel), &destination).unwrap();
        println!("écrit : {}", destination.display());
    }
    let octets = fs::read(common::repo_path(GOLDEN_PARQUET)).unwrap();
    println!(
        "blake3 du Parquet golden : {}",
        blake3::hash(&octets).to_hex()
    );
}
