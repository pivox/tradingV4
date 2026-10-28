//! Tests d'intégration des datasets multi-séries (S9 lot A, docs/specs/s9.md §12).
//!
//! Les séries sont synthétiques (générateur de `common`, prix inventés). Le chemin mono-série du
//! jalon 1 doit rester identique à l'octet : manifeste, `dataset_hash` et lecture.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use tv4::model::{Candle, ExchangeId, Timeframe};
use tv4::store::{
    BuildRequest, BuildStatus, Columns, GapsPolicy, MultiBuildRequest, Partition, PartitionLock,
    StoreError, Symbol, Window, build_dataset, build_dataset_multi, dsh, load_dataset,
    load_dataset_multi, write_partition,
};

/// 2024-01-01T00:00:00Z.
const T_2024_01: i64 = 1_704_067_200_000;
/// 2024-02-01T00:00:00Z.
const T_2024_02: i64 = 1_706_745_600_000;
const MINUTE: i64 = 60_000;
/// 2030-01-01T00:00:00Z : tous les mois des tests sont clos à cet instant.
const FUTUR: i64 = 1_893_456_000_000;
/// 2024-02-10T00:00:00Z : janvier est clos, février ne l'est pas.
const EN_FEVRIER: i64 = 1_707_523_200_000;

fn sym(text: &str) -> Symbol {
    text.parse().unwrap()
}

fn partition(symbol: &str) -> Partition {
    Partition::new(ExchangeId::Kraken, sym(symbol), Timeframe::M1)
}

/// Écrit `n` bougies 1m synthétiques (graine `seed`) de `symbol` depuis `debut`, à l'instant `now`.
fn ecrire(root: &Path, symbol: &str, seed: u64, debut: i64, n: usize, now: i64) -> Columns {
    let colonnes = Columns::from_candles(&common::synthetic_candles(n, seed, debut, 0.0));
    write_partition(root, &partition(symbol), &colonnes, "synthetic", now).unwrap();
    colonnes
}

fn fenetre(debut: i64, minutes: i64) -> Window {
    Window::new(debut, debut + minutes * MINUTE).unwrap()
}

fn multi<'a>(
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

fn mono<'a>(name: &'a str, symbol: &str, window: Window, policy: GapsPolicy) -> BuildRequest<'a> {
    BuildRequest {
        name,
        partition: partition(symbol),
        window,
        gaps_policy: policy,
    }
}

fn manifeste_path(root: &Path, name: &str) -> std::path::PathBuf {
    root.join("datasets").join(format!("{name}.json"))
}

/// Vue binaire des 5 colonnes de prix, pour une égalité au bit près.
fn bits(c: &Columns) -> Vec<[u64; 5]> {
    (0..c.len())
        .map(|i| {
            [
                c.open()[i].to_bits(),
                c.high()[i].to_bits(),
                c.low()[i].to_bits(),
                c.close()[i].to_bits(),
                c.volume()[i].to_bits(),
            ]
        })
        .collect()
}

#[test]
fn deux_series_donnent_les_colonnes_de_deux_builds_mono() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 1, T_2024_01, 300, FUTUR);
    ecrire(root, "BTC-USD", 2, T_2024_01, 300, FUTUR);
    let w = fenetre(T_2024_01, 200);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];
    let sortie =
        build_dataset_multi(root, &multi("deux", &symboles, w, GapsPolicy::Skip), FUTUR).unwrap();
    assert_eq!(sortie.status, BuildStatus::Created);
    assert_eq!(sortie.manifest.row_count, 400);
    // Symboles triés en ordre d'octets, quel que soit l'ordre de la demande.
    let attendus: Vec<&str> = vec!["BTC-USD", "ETH-USD"];
    let listes: Vec<&str> = sortie.manifest.symbols.iter().map(Symbol::as_str).collect();
    assert_eq!(listes, attendus);
    assert_eq!(sortie.manifest.tfs, vec![Timeframe::M1]);
    // Fichiers triés par (symbole, période).
    let chemins: Vec<&str> = sortie
        .manifest
        .files
        .iter()
        .map(|f| f.symbol.as_str())
        .collect();
    assert_eq!(chemins, attendus);

    let charge = load_dataset_multi(root, &manifeste_path(root, "deux")).unwrap();
    assert_eq!(charge.manifest, sortie.manifest);
    assert_eq!(charge.series.len(), 2);

    // Deux builds mono des mêmes données : mêmes colonnes, au bit près.
    for (symbole, colonnes) in &charge.series {
        let nom = format!("mono_{}", symbole.to_lowercase().replace('-', "_"));
        build_dataset(root, &mono(&nom, symbole, w, GapsPolicy::Skip), FUTUR).unwrap();
        let seule = load_dataset(root, &manifeste_path(root, &nom)).unwrap();
        assert_eq!(colonnes.len(), 200, "{symbole}");
        assert_eq!(bits(colonnes), bits(&seule.columns), "{symbole}");
        assert_eq!(colonnes.open_time_ms(), seule.columns.open_time_ms());
        assert_eq!(colonnes.trades(), seule.columns.trades());
        assert_eq!(colonnes.synthetic(), seule.columns.synthetic());
    }
    let ordre: Vec<&str> = charge.series.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(ordre, attendus);
}

#[test]
fn le_hash_egale_le_recalcul_dsh_a_deux_segments() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let eth = ecrire(root, "ETH-USD", 3, T_2024_01, 120, FUTUR);
    let btc = ecrire(root, "BTC-USD", 4, T_2024_01, 120, FUTUR);
    let w = fenetre(T_2024_01, 100);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];
    let sortie =
        build_dataset_multi(root, &multi("deux", &symboles, w, GapsPolicy::Skip), FUTUR).unwrap();
    // Recalcul indépendant, segments donnés dans le désordre : `dsh-v1` les trie.
    let (hex, lignes) = dsh::dataset_hash(
        "kraken",
        w,
        GapsPolicy::Skip,
        &[
            ("ETH-USD", Timeframe::M1, &eth),
            ("BTC-USD", Timeframe::M1, &btc),
        ],
    )
    .unwrap();
    assert_eq!(sortie.manifest.dataset_hash, dsh::prefixed(&hex));
    assert_eq!(sortie.manifest.row_count, lignes);
    assert_eq!(lignes, 200);
}

#[test]
fn le_hash_change_si_un_prix_de_la_seconde_serie_change() {
    let w = fenetre(T_2024_01, 100);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];

    let construire = |prix_modifie: bool| -> String {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ecrire(root, "ETH-USD", 5, T_2024_01, 120, FUTUR);
        let mut btc: Vec<Candle> = common::synthetic_candles(120, 6, T_2024_01, 0.0);
        if prix_modifie {
            btc[50].close += 0.01;
            btc[50].high = btc[50].high.max(btc[50].close);
        }
        let colonnes = Columns::from_candles(&btc);
        write_partition(root, &partition("BTC-USD"), &colonnes, "synthetic", FUTUR).unwrap();
        build_dataset_multi(root, &multi("deux", &symboles, w, GapsPolicy::Skip), FUTUR)
            .unwrap()
            .manifest
            .dataset_hash
    };
    let base = construire(false);
    assert_eq!(base, construire(false), "build reproductible");
    assert_ne!(base, construire(true), "un prix BTC modifié change le hash");

    // Le hash d'ETH seul diffère de celui d'ETH + BTC (le segment de BTC compte).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 5, T_2024_01, 120, FUTUR);
    let seul = build_dataset(root, &mono("eth", "ETH-USD", w, GapsPolicy::Skip), FUTUR).unwrap();
    assert_ne!(seul.manifest.dataset_hash, base);
}

#[test]
fn l_ordre_des_symboles_est_sans_effet_sur_le_manifeste() {
    let w = fenetre(T_2024_01, 100);
    let construire = |ordre: [&str; 3]| -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (graine, symbole) in [(7, "ETH-USD"), (8, "BTC-USD"), (9, "SOL-USD")] {
            ecrire(root, symbole, graine, T_2024_01, 120, FUTUR);
        }
        let symboles: Vec<Symbol> = ordre.iter().map(|s| sym(s)).collect();
        build_dataset_multi(root, &multi("trois", &symboles, w, GapsPolicy::Skip), FUTUR).unwrap();
        fs::read(manifeste_path(root, "trois")).unwrap()
    };
    let a = construire(["ETH-USD", "BTC-USD", "SOL-USD"]);
    let b = construire(["SOL-USD", "ETH-USD", "BTC-USD"]);
    let c = construire(["BTC-USD", "SOL-USD", "ETH-USD"]);
    assert_eq!(a, b);
    assert_eq!(a, c);
}

/// Lance `tv4 dataset build` avec `--symbol` répété dans l'ordre donné.
fn cli_build(root: &Path, symbols: &[&str]) -> std::process::Output {
    let mut args: Vec<String> = vec![
        "dataset".into(),
        "build".into(),
        "--root".into(),
        root.display().to_string(),
        "--name".into(),
        "cli".into(),
        "--exchange".into(),
        "kraken".into(),
    ];
    for s in symbols {
        args.push("--symbol".into());
        args.push((*s).into());
    }
    args.extend(
        [
            "--tf",
            "1m",
            "--from",
            "2024-01-01T00:00:00Z",
            "--to",
            "2024-01-01T01:40:00Z",
        ]
        .map(String::from),
    );
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(&args)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

/// Manifeste lu, sans `built_at` (horodatage de l'instant du build, hors hash).
fn manifeste_sans_date(root: &Path) -> serde_json::Value {
    let mut valeur: serde_json::Value =
        serde_json::from_slice(&fs::read(manifeste_path(root, "cli")).unwrap()).unwrap();
    valeur.as_object_mut().unwrap().remove("built_at");
    valeur
}

#[test]
fn la_ligne_de_commande_accepte_symbol_repete_dans_un_ordre_quelconque() {
    let mut manifestes = Vec::new();
    for ordre in [["ETH-USD", "BTC-USD"], ["BTC-USD", "ETH-USD"]] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ecrire(root, "ETH-USD", 10, T_2024_01, 120, FUTUR);
        ecrire(root, "BTC-USD", 11, T_2024_01, 120, FUTUR);
        let sortie = cli_build(root, &ordre);
        let stderr = String::from_utf8_lossy(&sortie.stderr).to_string();
        assert_eq!(sortie.status.code(), Some(0), "{stderr}");
        let stdout = String::from_utf8_lossy(&sortie.stdout).to_string();
        assert!(stdout.contains("200 bougies"), "{stdout}");
        assert!(stdout.contains("créé"), "{stdout}");
        manifestes.push(manifeste_sans_date(root));
    }
    assert_eq!(manifestes[0], manifestes[1]);
    assert_eq!(
        manifestes[0]["symbols"],
        serde_json::json!(["BTC-USD", "ETH-USD"])
    );
}

#[test]
fn la_ligne_de_commande_exige_un_symbole_et_refuse_un_doublon() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 12, T_2024_01, 120, FUTUR);
    let sortie = cli_build(root, &[]);
    assert_eq!(sortie.status.code(), Some(2), "usage : --symbol requis");
    let sortie = cli_build(root, &["ETH-USD", "ETH-USD"]);
    let stderr = String::from_utf8_lossy(&sortie.stderr).to_string();
    assert_eq!(sortie.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("ETH-USD"), "{stderr}");
    assert!(!manifeste_path(root, "cli").exists());
}

#[test]
fn un_manifeste_mono_est_inchange_a_l_octet() {
    let w = fenetre(T_2024_01, 100);
    // Même série, deux voies : le chemin mono du jalon 1 et le chemin multi à un seul symbole.
    let construire = |par_multi: bool| -> (Vec<u8>, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ecrire(root, "BTC-USD", 13, T_2024_01, 120, FUTUR);
        let sortie = if par_multi {
            let symboles = [sym("BTC-USD")];
            build_dataset_multi(root, &multi("seul", &symboles, w, GapsPolicy::Skip), FUTUR)
        } else {
            build_dataset(root, &mono("seul", "BTC-USD", w, GapsPolicy::Skip), FUTUR)
        }
        .unwrap();
        (
            fs::read(manifeste_path(root, "seul")).unwrap(),
            sortie.manifest.dataset_hash,
        )
    };
    let (par_mono, hash_mono) = construire(false);
    let (par_multi, hash_multi) = construire(true);
    assert_eq!(par_mono, par_multi);
    assert_eq!(hash_mono, hash_multi);
}

#[test]
fn la_fixture_garde_son_dataset_hash_epingle_par_la_voie_multi() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let colonnes = Columns::from_candles(&common::fixture_1m());
    write_partition(root, &partition("BTC-USD"), &colonnes, "kraken-csv", FUTUR).unwrap();
    // [2020-01-01T00:00:00Z, 2020-01-09T00:00:00Z) : la fenêtre du hash épinglé de S4.
    let w = Window::new(1_577_836_800_000, 1_578_528_000_000).unwrap();
    let symboles = [sym("BTC-USD")];
    let sortie = build_dataset_multi(
        root,
        &multi("fixture", &symboles, w, GapsPolicy::Skip),
        FUTUR,
    )
    .unwrap();
    assert_eq!(sortie.manifest.dataset_hash, common::DATASET_HASH_SKIP);
    assert_eq!(sortie.manifest.row_count, 10_000);
    // Les deux lectures rendent les mêmes lignes pour un manifeste mono.
    let chemin = manifeste_path(root, "fixture");
    let seule = load_dataset(root, &chemin).unwrap();
    let plusieurs = load_dataset_multi(root, &chemin).unwrap();
    assert_eq!(plusieurs.series.len(), 1);
    assert_eq!(plusieurs.series[0].0, "BTC-USD");
    assert_eq!(plusieurs.series[0].1, seule.columns);
    assert_eq!(plusieurs.manifest, seule.manifest);
}

#[test]
fn load_dataset_refuse_toujours_un_dataset_multi_series() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 14, T_2024_01, 60, FUTUR);
    ecrire(root, "BTC-USD", 15, T_2024_01, 60, FUTUR);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];
    build_dataset_multi(
        root,
        &multi("deux", &symboles, fenetre(T_2024_01, 50), GapsPolicy::Skip),
        FUTUR,
    )
    .unwrap();
    let erreur = load_dataset(root, &manifeste_path(root, "deux")).unwrap_err();
    assert!(
        matches!(
            erreur,
            StoreError::MultiSeriesUnsupported { symbols: 2, tfs: 1 }
        ),
        "{erreur:?}"
    );
}

#[test]
fn erreurs_de_build() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 16, T_2024_01, 60, FUTUR);
    let w = fenetre(T_2024_01, 50);

    // Aucun symbole.
    let erreur =
        build_dataset_multi(root, &multi("vide", &[], w, GapsPolicy::Skip), FUTUR).unwrap_err();
    assert!(matches!(erreur, StoreError::NoSymbols), "{erreur:?}");

    // Deux symboles identiques.
    let doublon = [sym("ETH-USD"), sym("ETH-USD")];
    let erreur =
        build_dataset_multi(root, &multi("dbl", &doublon, w, GapsPolicy::Skip), FUTUR).unwrap_err();
    assert!(
        matches!(&erreur, StoreError::DuplicateSymbol { symbol } if symbol == "ETH-USD"),
        "{erreur:?}"
    );

    // Un symbole absent du stockage.
    let absent = [sym("ETH-USD"), sym("XRP-USD")];
    let erreur =
        build_dataset_multi(root, &multi("abs", &absent, w, GapsPolicy::Skip), FUTUR).unwrap_err();
    assert!(
        matches!(&erreur, StoreError::SymbolWithoutData { symbol, .. } if symbol == "XRP-USD"),
        "{erreur:?}"
    );
    assert!(erreur.to_string().contains("XRP-USD"));

    // Aucun manifeste n'a été écrit.
    for nom in ["vide", "dbl", "abs"] {
        assert!(!manifeste_path(root, nom).exists(), "{nom}");
    }
}

#[test]
fn un_verrou_de_partition_pris_bloque_le_build_puis_tout_est_libere() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 17, T_2024_01, 60, FUTUR);
    ecrire(root, "BTC-USD", 18, T_2024_01, 60, FUTUR);
    let w = fenetre(T_2024_01, 50);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];
    {
        let _verrou = PartitionLock::acquire(root, &partition("ETH-USD")).unwrap();
        let erreur =
            build_dataset_multi(root, &multi("deux", &symboles, w, GapsPolicy::Skip), FUTUR)
                .unwrap_err();
        assert!(matches!(erreur, StoreError::LockBusy { .. }), "{erreur:?}");
    }
    // Le build échoué a rendu ses verrous (BTC et le nom) : un second build réussit.
    build_dataset_multi(root, &multi("deux", &symboles, w, GapsPolicy::Skip), FUTUR).unwrap();
}

#[test]
fn les_fichiers_non_clos_sont_figes_serie_par_serie() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Janvier est clos ; février ne l'est pas à `EN_FEVRIER` : un snapshot par série.
    for (graine, symbole) in [(19, "ETH-USD"), (20, "BTC-USD")] {
        ecrire(root, symbole, graine, T_2024_02, 60, EN_FEVRIER);
    }
    let w = fenetre(T_2024_02, 50);
    let symboles = [sym("BTC-USD"), sym("ETH-USD")];
    let sortie = build_dataset_multi(
        root,
        &multi("fev", &symboles, w, GapsPolicy::Skip),
        EN_FEVRIER,
    )
    .unwrap();
    assert_eq!(sortie.snapshots, 2);
    for fichier in &sortie.manifest.files {
        assert!(fichier.snapshot);
        assert!(
            fichier.path.starts_with("datasets/snapshots/"),
            "{}",
            fichier.path
        );
        assert!(
            fichier.path.contains(fichier.symbol.as_str()),
            "{}",
            fichier.path
        );
        assert!(root.join(&fichier.path).is_file());
    }
    // Le snapshot suffit à recharger, même si le fichier vivant est réécrit ensuite.
    let charge = load_dataset_multi(root, &manifeste_path(root, "fev")).unwrap();
    assert_eq!(charge.series.len(), 2);
    assert!(charge.series.iter().all(|(_, c)| c.len() == 50));
    // Rebuild identique : inchangé, rien n'est réécrit.
    let encore = build_dataset_multi(
        root,
        &multi("fev", &symboles, w, GapsPolicy::Skip),
        EN_FEVRIER,
    )
    .unwrap();
    assert_eq!(encore.status, BuildStatus::Unchanged);
}

#[test]
fn la_politique_de_trous_s_applique_serie_par_serie_au_chargement() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // ETH sans trou ; BTC avec une minute absente (index 10).
    ecrire(root, "ETH-USD", 21, T_2024_01, 60, FUTUR);
    let mut btc = common::synthetic_candles(60, 22, T_2024_01, 0.0);
    btc.remove(10);
    write_partition(
        root,
        &partition("BTC-USD"),
        &Columns::from_candles(&btc),
        "synthetic",
        FUTUR,
    )
    .unwrap();
    let w = fenetre(T_2024_01, 59);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];
    let sortie = build_dataset_multi(
        root,
        &multi("trous", &symboles, w, GapsPolicy::Ffill),
        FUTUR,
    )
    .unwrap();
    // Hash et `row_count` portent sur les lignes réelles, avant remplissage.
    assert_eq!(sortie.manifest.row_count, 59 + 58);
    let charge = load_dataset_multi(root, &manifeste_path(root, "trous")).unwrap();
    let btc_charge = &charge.series[0];
    let eth_charge = &charge.series[1];
    assert_eq!(btc_charge.0, "BTC-USD");
    assert_eq!(btc_charge.1.len(), 59);
    assert_eq!(btc_charge.1.synthetic_count(), 1);
    assert_eq!(eth_charge.1.len(), 59);
    assert_eq!(eth_charge.1.synthetic_count(), 0);
    // La minute synthétique est la 11e (index 10) : volume nul.
    assert!(btc_charge.1.synthetic()[10]);
    assert!(btc_charge.1.volume()[10].abs() < f64::EPSILON);
}

#[test]
fn un_fichier_modifie_ou_un_manifeste_incoherent_est_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 23, T_2024_01, 60, FUTUR);
    ecrire(root, "BTC-USD", 24, T_2024_01, 60, FUTUR);
    let w = fenetre(T_2024_01, 50);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];
    build_dataset_multi(root, &multi("deux", &symboles, w, GapsPolicy::Skip), FUTUR).unwrap();
    let chemin = manifeste_path(root, "deux");
    let original = fs::read(&chemin).unwrap();

    // Symboles non triés dans le manifeste.
    let mut valeur: serde_json::Value = serde_json::from_slice(&original).unwrap();
    valeur["symbols"] = serde_json::json!(["ETH-USD", "BTC-USD"]);
    fs::write(&chemin, serde_json::to_vec_pretty(&valeur).unwrap()).unwrap();
    let erreur = load_dataset_multi(root, &chemin).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::ManifestMismatch {
                field: "symbols",
                ..
            }
        ),
        "{erreur:?}"
    );

    // Fichier listé pour un symbole absent de `symbols`.
    let mut valeur: serde_json::Value = serde_json::from_slice(&original).unwrap();
    valeur["symbols"] = serde_json::json!(["BTC-USD"]);
    fs::write(&chemin, serde_json::to_vec_pretty(&valeur).unwrap()).unwrap();
    let erreur = load_dataset_multi(root, &chemin).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::ManifestMismatch {
                field: "symbol",
                ..
            }
        ),
        "{erreur:?}"
    );

    // Hash de dataset altéré : refus.
    let mut valeur: serde_json::Value = serde_json::from_slice(&original).unwrap();
    valeur["dataset_hash"] = serde_json::json!(format!("blake3:{}", "0".repeat(64)));
    fs::write(&chemin, serde_json::to_vec_pretty(&valeur).unwrap()).unwrap();
    let erreur = load_dataset_multi(root, &chemin).unwrap_err();
    assert!(
        matches!(erreur, StoreError::DatasetHashMismatch { .. }),
        "{erreur:?}"
    );

    // Fichier Parquet d'une série remplacé par un contenu valide mais différent.
    fs::write(&chemin, &original).unwrap();
    load_dataset_multi(root, &chemin).unwrap();
    let autre = tempfile::tempdir().unwrap();
    ecrire(autre.path(), "BTC-USD", 99, T_2024_01, 60, FUTUR);
    let rel = partition("BTC-USD").file_rel(tv4::store::Period::Month {
        year: 2024,
        month: 1,
    });
    fs::copy(autre.path().join(&rel), root.join(&rel)).unwrap();
    let erreur = load_dataset_multi(root, &chemin).unwrap_err();
    assert!(
        matches!(&erreur, StoreError::FileHashMismatch { path, .. } if path.contains("BTC-USD")),
        "{erreur:?}"
    );
}

#[test]
fn un_manifeste_multi_timeframe_ou_sans_symbole_ou_mal_groupe_est_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ecrire(root, "ETH-USD", 25, T_2024_01, 60, FUTUR);
    ecrire(root, "BTC-USD", 26, T_2024_01, 60, FUTUR);
    let w = fenetre(T_2024_01, 50);
    let symboles = [sym("ETH-USD"), sym("BTC-USD")];
    build_dataset_multi(root, &multi("deux", &symboles, w, GapsPolicy::Skip), FUTUR).unwrap();
    let chemin = manifeste_path(root, "deux");
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&chemin).unwrap()).unwrap();
    let ecrire_valeur = |valeur: &serde_json::Value| {
        fs::write(&chemin, serde_json::to_vec_pretty(valeur).unwrap()).unwrap();
    };

    let mut valeur = original.clone();
    valeur["tfs"] = serde_json::json!(["1m", "5m"]);
    ecrire_valeur(&valeur);
    let erreur = load_dataset_multi(root, &chemin).unwrap_err();
    assert!(
        matches!(erreur, StoreError::MultiTimeframeUnsupported { tfs: 2 }),
        "{erreur:?}"
    );

    let mut valeur = original.clone();
    valeur["symbols"] = serde_json::json!([]);
    ecrire_valeur(&valeur);
    let erreur = load_dataset_multi(root, &chemin).unwrap_err();
    assert!(matches!(erreur, StoreError::NoSymbols), "{erreur:?}");

    // Fichiers listés dans le désordre des symboles (ETH avant BTC).
    let mut valeur = original.clone();
    valeur["files"].as_array_mut().unwrap().reverse();
    ecrire_valeur(&valeur);
    let erreur = load_dataset_multi(root, &chemin).unwrap_err();
    assert!(
        matches!(
            &erreur,
            StoreError::ManifestMismatch {
                field: "symbol",
                ..
            }
        ),
        "{erreur:?}"
    );
}
