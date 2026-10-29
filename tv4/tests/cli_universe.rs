//! `tv4 universe build|verify|survivorship|okx-subset`, `tv4 ingest kraken --pairs-from-universe` et
//! `tv4 dataset build --block` (S11, lot F, docs/specs/s11.md §4, §5, §6, §7 et §11) : on lance le vrai
//! binaire sur un zip Kraken **synthétique** (voir `cli_research_common`), sans aucune donnée de marché
//! réelle ni secret.
//!
//! Codes de sortie vérifiés : 0 succès, 1 erreur d'exécution (fichier, store, univers), 2 erreur d'usage.

mod cli_research_common;
mod common;

use std::fs;
use std::path::Path;

use cli_research_common::{World, ecrire_zip_variante, json, ok, repo, run, strings};

fn shown(path: &Path) -> String {
    path.display().to_string()
}

fn config() -> String {
    shown(&repo("config/universe.toml"))
}

fn okx_fixture() -> String {
    shown(&repo("tv4/tests/fixtures/okx_instruments_spot.json"))
}

/// Contenu de tous les fichiers d'un dossier, triés par nom.
fn contenu(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
        .expect("dossier")
        .map(|entry| {
            let path = entry.expect("entrée").path();
            (
                path.file_name()
                    .expect("nom")
                    .to_string_lossy()
                    .into_owned(),
                fs::read(&path).expect("fichier"),
            )
        })
        .collect();
    files.sort();
    files
}

fn verify_args(w: &World) -> Vec<String> {
    strings(&[
        "universe",
        "verify",
        "--root",
        &shown(&w.root),
        "--universe",
        &shown(&w.universe),
    ])
}

// ---------------------------------------------------------------------------------------------
// universe build
// ---------------------------------------------------------------------------------------------

#[test]
fn build_ecrit_les_fichiers_attendus_et_deux_builds_donnent_les_memes_octets() {
    let w = World::bare();
    let (stdout, _) = ok(&w.build_args());
    assert!(
        stdout.contains("2026-Q3 : 3 membres sur 3 éligibles"),
        "{stdout}"
    );
    assert!(
        stdout.contains("3 règles d'instrument écrites (decimals_v1)"),
        "{stdout}"
    );
    let names: Vec<String> = contenu(&w.universe).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        [
            "instruments_kraken.toml",
            "universe.json",
            "universe_2026-Q3.csv"
        ]
    );

    // ADAUSD n'a que 2 mois d'historique à D = 2026-07-01 : jamais membre. Rang par volume.
    let csv = fs::read_to_string(w.universe.join("universe_2026-Q3.csv")).expect("csv");
    let symbols: Vec<&str> = csv
        .lines()
        .skip(1)
        .filter_map(|line| line.split(',').nth(2))
        .collect();
    assert_eq!(symbols, ["BTC-USD", "ETH-USD", "SOL-USD"], "{csv}");

    // Un second build dans un autre dossier : mêmes octets, dans l'ordre des fichiers.
    let second = w._dir.path().join("univers2");
    let mut args = w.build_args();
    let out_index = args.iter().position(|a| a == "--out").expect("--out") + 1;
    args[out_index] = shown(&second);
    ok(&args);
    assert_eq!(contenu(&w.universe), contenu(&second));

    // Un second build au même endroit rend les mêmes octets (trimestre figé, rien à réécrire).
    let before = contenu(&w.universe);
    ok(&w.build_args());
    assert_eq!(contenu(&w.universe), before);
}

#[test]
fn build_refuse_un_zip_sans_fin_de_couverture_et_n_ecrit_rien() {
    let w = World::bare();
    let zip = w._dir.path().join("sans_manifeste.zip");
    common::ecrire_zip(&zip, &[("XBTUSD_1.csv", b"1780000000,1,1,1,1,1,1\n")]);
    let sha = common::sha256_fichier(&zip);
    let out = w._dir.path().join("o");
    let args = strings(&[
        "universe",
        "build",
        "--zip",
        &shown(&zip),
        "--sha256",
        &sha,
        "--out",
        &shown(&out),
        "--config",
        &config(),
    ]);
    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("--coverage-end"), "{texte}");
    assert!(!out.exists(), "rien ne doit être écrit");
}

#[test]
fn build_sans_trimestre_entierement_couvert_echoue_en_code_1() {
    let w = World::bare();
    let mut args = w.build_args();
    args.extend(strings(&["--coverage-end", "2026-07-01T00:00:00Z"]));
    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
    assert!(
        texte.contains("aucun trimestre entièrement couvert"),
        "{texte}"
    );
    assert!(!w.universe.join("universe.json").exists());
}

#[test]
fn build_refuse_les_options_invalides_en_code_2() {
    let w = World::bare();
    // `build_args` donne déjà --from-quarter : on retire le couple pour tester une valeur invalide.
    let mut args = w.build_args();
    let index = args
        .iter()
        .position(|a| a == "--from-quarter")
        .expect("--from-quarter");
    args[index + 1] = "2026-Q5".to_string();
    let (code, texte) = run(&args);
    assert_eq!(code, Some(2), "{texte}");

    let mut args = w.build_args();
    args.extend(strings(&["--at", "2026-07-01T12:00:00Z"]));
    let (code, texte) = run(&args);
    assert_eq!(code, Some(2), "{texte}");
    assert!(texte.contains("minuit UTC"), "{texte}");
    assert!(!w.universe.join("universe.json").exists());
}

#[test]
fn build_ecrit_les_dates_at_et_fige_les_trimestres_existants() {
    let w = World::bare();
    let mut args = w.build_args();
    args.extend(strings(&["--at", "2026-08-01T00:00:00Z"]));
    let (stdout, _) = ok(&args);
    assert!(
        stdout.contains("--at 2026-08-01T00:00:00Z : 3 éligibles"),
        "{stdout}"
    );
    assert!(w.universe.join("eligible_2026-08-01.csv").exists());
    let before = contenu(&w.universe);

    // Un autre zip avec les mêmes paires mais d'autres prix : le trimestre figé se reconstruirait
    // différemment, la commande refuse sans rien écrire.
    let autre = w._dir.path().join("variante.zip");
    let sha = ecrire_zip_variante(&autre, 1);
    let mut args = w.build_args();
    args[3] = shown(&autre);
    args[5] = sha;
    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("déjà figé"), "{texte}");
    assert_eq!(contenu(&w.universe), before);
}

// ---------------------------------------------------------------------------------------------
// universe verify
// ---------------------------------------------------------------------------------------------

#[test]
fn verify_accepte_un_univers_ingere() {
    let w = World::with_universe();
    let (stdout, _) = ok(&verify_args(&w));
    assert!(
        stdout.contains("3 membre(s) de trimestre sur 3 symbole(s), tout est identique au store"),
        "{stdout}"
    );
}

#[test]
fn verify_refuse_un_csv_altere() {
    let w = World::with_universe();
    let csv = w.universe.join("universe_2026-Q3.csv");
    let text = fs::read_to_string(&csv).expect("csv");
    fs::write(&csv, text.replacen("USD,", "USD ,", 1)).expect("altération");
    let (code, texte) = run(&verify_args(&w));
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("erreur :"), "{texte}");
}

#[test]
fn verify_refuse_un_store_sans_les_series_de_l_union() {
    let w = World::bare();
    ok(&w.build_args());
    let (code, texte) = run(&verify_args(&w));
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("aucune bougie 1m dans le store"), "{texte}");
    assert!(texte.contains("diffère du store (3 écart(s))"), "{texte}");
}

#[test]
fn verify_detecte_une_serie_modifiee_dans_le_store() {
    let w = World::with_universe();
    // Le store est réingéré depuis un autre zip : les `input_hash` de l'univers ne tiennent plus.
    let autre = w._dir.path().join("variante.zip");
    let sha = ecrire_zip_variante(&autre, 1);
    let mut args = w.ingest_args();
    args[3] = shown(&autre);
    args[5] = sha;
    // Les mois déjà écrits ne se réécrivent pas : le contrôle passe par un second store vierge.
    let root = w._dir.path().join("racine2");
    let root_index = args.iter().position(|a| a == "--root").expect("--root") + 1;
    args[root_index] = shown(&root);
    ok(&args);
    let verify = strings(&[
        "universe",
        "verify",
        "--root",
        &shown(&root),
        "--universe",
        &shown(&w.universe),
    ]);
    let (code, texte) = run(&verify);
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("diffère du store"), "{texte}");
}

// ---------------------------------------------------------------------------------------------
// ingest kraken --pairs-from-universe
// ---------------------------------------------------------------------------------------------

#[test]
fn ingest_pairs_from_universe_lit_l_union_et_se_rejoue_sans_reecrire() {
    let w = World::bare();
    ok(&w.build_args());
    let (stdout, _) = ok(&w.ingest_args());
    assert!(stdout.contains("3 paire(s) de l'union lues"), "{stdout}");
    for pair in ["BTC-USD", "ETH-USD", "SOL-USD"] {
        assert!(stdout.contains(pair), "{pair} : {stdout}");
    }
    assert!(!stdout.contains("ADA-USD"), "{stdout}");

    // `--pair` en plus de l'union : dédoublonné, rien n'est réécrit.
    let mut args = w.ingest_args();
    args.extend(strings(&["--pair", "XBTUSD"]));
    let (stdout, _) = ok(&args);
    assert!(stdout.contains("3 à ingérer au total"), "{stdout}");
    assert!(
        stdout.contains("0 créés, 0 réécrits, 16 inchangés"),
        "{stdout}"
    );
}

#[test]
fn ingest_sans_pair_ni_univers_est_une_erreur_d_usage() {
    let w = World::bare();
    let args = strings(&[
        "ingest",
        "kraken",
        "--zip",
        &shown(&w.zip),
        "--sha256",
        &w.sha,
        "--root",
        &shown(&w.root),
    ]);
    let (code, texte) = run(&args);
    assert_eq!(code, Some(2), "{texte}");
}

#[test]
fn ingest_refuse_un_dossier_d_univers_sans_universe_json() {
    let w = World::bare();
    let vide = w._dir.path().join("vide");
    fs::create_dir_all(&vide).expect("dossier");
    let mut args = w.ingest_args();
    let index = args
        .iter()
        .position(|a| a == "--pairs-from-universe")
        .expect("option")
        + 1;
    args[index] = shown(&vide);
    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
}

// ---------------------------------------------------------------------------------------------
// universe survivorship et okx-subset
// ---------------------------------------------------------------------------------------------

#[test]
fn survivorship_ecrit_un_rapport_stable_jamais_ecrase() {
    let w = World::bare();
    let report = w._dir.path().join("surv.json");
    let args = strings(&[
        "universe",
        "survivorship",
        "--zip",
        &shown(&w.zip),
        "--sha256",
        &w.sha,
        "--out",
        &shown(&report),
        "--config",
        &config(),
    ]);
    let (stdout, _) = ok(&args);
    assert!(stdout.contains("ICN* : absent"), "{stdout}");
    assert!(
        stdout.contains("4 paires dans le zip, 0 délistées"),
        "{stdout}"
    );
    let value = json(&report);
    assert!(
        value["report_hash"]
            .as_str()
            .is_some_and(|h| h.starts_with("blake3:")),
        "{value}"
    );
    let first = fs::read(&report).expect("rapport");

    // Même rapport : « inchangé », octets identiques.
    let (stdout, _) = ok(&args);
    assert!(stdout.contains("inchangé"), "{stdout}");
    assert_eq!(fs::read(&report).expect("rapport"), first);

    // Contenu différent : jamais écrasé.
    fs::write(&report, "x").expect("altération");
    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("jamais écrasé"), "{texte}");
    assert_eq!(fs::read(&report).expect("rapport"), b"x");
}

#[test]
fn okx_subset_derive_la_liste_depuis_la_fixture_et_ne_l_ecrase_pas() {
    let w = World::bare();
    let out = w._dir.path().join("okx_usdc.json");
    let args = strings(&[
        "universe",
        "okx-subset",
        "--instruments",
        &okx_fixture(),
        "--out",
        &shown(&out),
        "--config",
        &config(),
    ]);
    let (stdout, _) = ok(&args);
    assert!(
        stdout.contains("3 bases tradables en USDC (BTC, ETH, SOL)"),
        "{stdout}"
    );
    let value = json(&out);
    assert_eq!(value["schema"], "tv4-pair-subset-v1");
    assert_eq!(value["kind"], "okx_usdc_tradable");
    assert_eq!(value["bases"], serde_json::json!(["BTC", "ETH", "SOL"]));
    assert_eq!(value["source"]["site"], "eea");
    let first = fs::read(&out).expect("liste");

    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
    assert!(texte.contains("ne s'écrase jamais"), "{texte}");
    assert_eq!(fs::read(&out).expect("liste"), first);
}

#[test]
fn okx_subset_refuse_un_fichier_d_instruments_altere() {
    let w = World::bare();
    let text =
        fs::read_to_string(repo("tv4/tests/fixtures/okx_instruments_spot.json")).expect("fixture");
    let altered = w._dir.path().join("instruments.json");
    fs::write(&altered, text.replacen("\"ADA-USDC\"", "\"ADB-USDC\"", 1)).expect("altération");
    let out = w._dir.path().join("okx_usdc.json");
    let args = strings(&[
        "universe",
        "okx-subset",
        "--instruments",
        &shown(&altered),
        "--out",
        &shown(&out),
        "--config",
        &config(),
    ]);
    let (code, texte) = run(&args);
    assert_eq!(code, Some(1), "{texte}");
    assert!(!out.exists());
}

// ---------------------------------------------------------------------------------------------
// dataset build --block
// ---------------------------------------------------------------------------------------------

fn manifest(w: &World, name: &str) -> serde_json::Value {
    json(&w.root.join("datasets").join(format!("{name}.json")))
}

#[test]
fn dataset_build_block_ecrit_les_metadonnees_de_recherche() {
    let w = World::with_universe();
    let (stdout, _) = ok(&w.dataset_args("fwd", "5"));
    assert!(
        stdout.contains(
            "fenêtre de décision [2026-07-01T00:00:00Z, 2026-09-01T00:00:00Z), 3 symbole(s)"
        ),
        "{stdout}"
    );
    let m = manifest(&w, "fwd");
    assert_eq!(m["block"], "FORWARD");
    assert_eq!(m["gaps_policy"], "zero_volume_marked");
    assert_eq!(
        m["window"],
        serde_json::json!(["2026-06-26T00:00:00Z", "2026-09-01T00:00:00Z"])
    );
    assert_eq!(
        m["decision_window"],
        serde_json::json!(["2026-07-01T00:00:00Z", "2026-09-01T00:00:00Z"])
    );
    assert_eq!(
        m["symbols"],
        serde_json::json!(["BTC-USD", "ETH-USD", "SOL-USD"])
    );
    assert!(m["universe"]["rules_hash"].as_str().is_some());
    assert!(m["universe"]["window_hash"].as_str().is_some());
    // 4h et 1d complets, 5m à 1h absents à 99 % : exclus pour le trimestre.
    assert_eq!(
        m["quality_exclusions"]["BTC-USD"][0]["tfs"],
        serde_json::json!(["5m", "15m", "30m", "1h"])
    );

    // Même commande : dataset inchangé.
    let (stdout, _) = ok(&w.dataset_args("fwd", "5"));
    assert!(stdout.contains("inchangé"), "{stdout}");
}

#[test]
fn dataset_build_chauffe_par_defaut_210_jours() {
    let w = World::with_universe();
    let mut args = w.dataset_args("fwd210", "5");
    let index = args
        .iter()
        .position(|a| a == "--warmup-days")
        .expect("option");
    args.drain(index..=index + 1);
    ok(&args);
    // 2026-07-01 moins 210 jours.
    assert_eq!(manifest(&w, "fwd210")["window"][0], "2025-12-03T00:00:00Z");
}

#[test]
fn le_dataset_hash_ne_depend_pas_du_bloc() {
    let w = World::with_universe();
    ok(&w.dataset_args("fwd", "5"));
    // Même fenêtre, mêmes symboles, même politique : sans `--block`, mêmes bougies, même hash.
    ok(&strings(&[
        "dataset",
        "build",
        "--root",
        &shown(&w.root),
        "--name",
        "plain",
        "--exchange",
        "kraken",
        "--tf",
        "1m",
        "--symbol",
        "BTC-USD",
        "--symbol",
        "ETH-USD",
        "--symbol",
        "SOL-USD",
        "--from",
        "2026-06-26T00:00:00Z",
        "--to",
        "2026-09-01T00:00:00Z",
        "--gaps-policy",
        "zero_volume_marked",
    ]));
    let with_block = manifest(&w, "fwd");
    let without = manifest(&w, "plain");
    assert_eq!(with_block["dataset_hash"], without["dataset_hash"]);
    assert!(without.get("block").is_none(), "{without}");
}

#[test]
fn dataset_build_refuse_les_combinaisons_incoherentes_en_code_2() {
    let w = World::with_universe();
    let root = shown(&w.root);
    let universe = shown(&w.universe);
    let base = |extra: &[&str]| {
        let mut args = strings(&[
            "dataset",
            "build",
            "--root",
            &root,
            "--name",
            "x",
            "--exchange",
            "kraken",
        ]);
        args.extend(strings(extra));
        args
    };
    let cases: Vec<(Vec<String>, &str)> = vec![
        // --from interdit avec --block.
        (
            base(&[
                "--tf",
                "1m",
                "--block",
                "FORWARD",
                "--to",
                "2026-09-01T00:00:00Z",
                "--from",
                "2026-06-01T00:00:00Z",
                "--universe",
                &universe,
            ]),
            "--from",
        ),
        // --to interdit pour un bloc fermé.
        (
            base(&[
                "--tf",
                "1m",
                "--block",
                "DEV",
                "--to",
                "2022-01-01T00:00:00Z",
                "--universe",
                &universe,
            ]),
            "--to",
        ),
        // --to obligatoire pour FORWARD.
        (
            base(&["--tf", "1m", "--block", "FORWARD", "--universe", &universe]),
            "--to",
        ),
        // 1m seulement.
        (
            base(&[
                "--tf",
                "5m",
                "--block",
                "FORWARD",
                "--to",
                "2026-09-01T00:00:00Z",
                "--universe",
                &universe,
            ]),
            "--tf",
        ),
        // zero_volume_marked seulement.
        (
            base(&[
                "--tf",
                "1m",
                "--block",
                "FORWARD",
                "--to",
                "2026-09-01T00:00:00Z",
                "--gaps-policy",
                "skip",
                "--universe",
                &universe,
            ]),
            "--gaps-policy",
        ),
        // Ni --universe ni --symbol.
        (
            base(&[
                "--tf",
                "1m",
                "--block",
                "FORWARD",
                "--to",
                "2026-09-01T00:00:00Z",
            ]),
            "--symbol",
        ),
        // Chauffe hors bornes.
        (
            base(&[
                "--tf",
                "1m",
                "--block",
                "FORWARD",
                "--to",
                "2026-09-01T00:00:00Z",
                "--warmup-days",
                "401",
                "--universe",
                &universe,
            ]),
            "401",
        ),
        // --universe sans --block.
        (
            base(&[
                "--tf",
                "1m",
                "--from",
                "2026-06-01T00:00:00Z",
                "--to",
                "2026-07-01T00:00:00Z",
                "--universe",
                &universe,
            ]),
            "--block",
        ),
        // --warmup-days sans --block.
        (
            base(&[
                "--tf",
                "1m",
                "--from",
                "2026-06-01T00:00:00Z",
                "--to",
                "2026-07-01T00:00:00Z",
                "--symbol",
                "BTC-USD",
                "--warmup-days",
                "5",
            ]),
            "--block",
        ),
    ];
    for (args, needle) in cases {
        let (code, texte) = run(&args);
        assert_eq!(code, Some(2), "{args:?}\n{texte}");
        assert!(texte.contains(needle), "{needle} absent de : {texte}");
    }
    assert!(!w.root.join("datasets/x.json").exists());
}

#[test]
fn dataset_build_refuse_un_univers_qui_n_a_pas_ete_ingere() {
    let w = World::bare();
    ok(&w.build_args());
    // Store vide : les séries de l'union manquent.
    let (code, texte) = run(&w.dataset_args("fwd", "5"));
    assert_eq!(code, Some(1), "{texte}");
    assert!(!w.root.join("datasets/fwd.json").exists());
}
