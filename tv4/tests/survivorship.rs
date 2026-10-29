//! Tests du test de survivance (docs/specs/s11.md §5, lot C) sur des zips **synthétiques** écrits à la
//! volée : les noms de paires sont ceux des motifs de 04 §2.2 (`LUNAUSD`, `USTUSD`...) mais les
//! bougies (prix 100, volumes 1,5, dates choisies par rapport à la fin de couverture) sont inventées et
//! ne disent rien du dump réel. Aucun zip n'est commité.

mod common;

use std::path::{Path, PathBuf};

use common::{ecrire_zip, manifeste_synthetique, sha256_fichier};
use tv4::marketdata::kraken::{KrakenError, RawRow};
use tv4::marketdata::kraken_zip::{KrakenZip, Sha256Hex};
use tv4::model::Ts;
use tv4::model::universe::UniverseConfig;
use tv4::research::survivorship::{
    PairObservation, SurvivorshipReport, ZipSummary, build_report, plan_entries, span_of_rows,
    survey_zip,
};
use tv4::store::time::{MS_PER_DAY, parse_coverage_end};

const DAY: Ts = MS_PER_DAY;

fn config() -> UniverseConfig {
    UniverseConfig::load(&PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../config/universe.toml"
    )))
    .unwrap()
}

/// Fin de couverture du manifeste synthétique `2026-09-30` (date seule, inclusive).
fn fin() -> Ts {
    parse_coverage_end("2026-09-30").unwrap()
}

/// Lignes quotidiennes synthétiques de `premiere` à `derniere` (ms, incluses).
fn lignes(premiere: Ts, derniere: Ts) -> String {
    let mut texte = String::new();
    let mut t = premiere;
    while t <= derniere {
        texte.push_str(&format!("{},100.0,101.0,99.0,100.5,1.5,3\n", t / 1000));
        t += DAY;
    }
    texte
}

/// Entrées synthétiques : (nom, dernier instant ou `None` pour une entrée vide).
fn entrees() -> Vec<(String, Option<Ts>)> {
    let f = fin();
    let jours = |n: i64| Some(f - n * DAY);
    vec![
        ("XBTUSD_1440.csv".into(), jours(1)),
        ("XBTUSD_60.csv".into(), jours(1)),
        ("XBTUSD_1.csv".into(), jours(1)),
        ("XBTUSD_5.csv".into(), jours(1)),
        ("WAVESEUR_1440.csv".into(), jours(10)),
        ("USTUSD_1440.csv".into(), jours(91)),
        ("FTTUSD_1440.csv".into(), jours(150)),
        ("OLDUSD_1440.csv".into(), jours(200)),
        ("LUNAUSD_1440.csv".into(), jours(100)),
        ("LUNA2USD_1440.csv".into(), jours(5)),
        ("BOUNDUSD_1440.csv".into(), jours(90)),
        // Pièges de préfixe : ni ICN* ni UST*.
        ("AICNUSD_1440.csv".into(), jours(2)),
        ("USDTUSD_1440.csv".into(), jours(2)),
        // Repli sur _5 puis _1 faute d'entrée journalière ni horaire.
        ("CINQUSD_5.csv".into(), jours(3)),
        ("CINQUSD_1.csv".into(), jours(3)),
        ("UNEUSD_1.csv".into(), jours(4)),
        // Forme héritée, entrée sans bougie, nom étranger.
        ("XXBTZUSD_1440.csv".into(), jours(1)),
        ("VIDEUSD_1440.csv".into(), None),
        ("README.txt".into(), jours(1)),
        ("abc_1440.csv".into(), jours(1)),
    ]
}

fn ecrire(chemin: &Path, entrees: &[(String, Option<Ts>)], inverse: bool) {
    let contenus: Vec<(String, Vec<u8>)> = entrees
        .iter()
        .map(|(nom, derniere)| {
            let texte = derniere.map_or(String::new(), |d| lignes(d - 5 * DAY, d));
            (nom.clone(), texte.into_bytes())
        })
        .collect();
    let manifeste = manifeste_synthetique("2026-09-30");
    let mut liste: Vec<(&str, &[u8])> = vec![("MANIFEST.json", manifeste.as_slice())];
    let mut ordre: Vec<&(String, Vec<u8>)> = contenus.iter().collect();
    if inverse {
        ordre.reverse();
    }
    liste.extend(ordre.into_iter().map(|(n, o)| (n.as_str(), o.as_slice())));
    ecrire_zip(chemin, &liste);
}

fn rapport(chemin: &Path) -> SurvivorshipReport {
    let sha: Sha256Hex = sha256_fichier(chemin).parse().unwrap();
    let mut zip = KrakenZip::open_verified(chemin, &sha).unwrap();
    survey_zip(&mut zip, &config(), fin()).unwrap()
}

#[test]
fn motifs_presents_absents_et_pieges_de_prefixe() {
    let dossier = tempfile::tempdir().unwrap();
    let chemin = dossier.path().join("dump.zip");
    ecrire(&chemin, &entrees(), false);
    let rapport = rapport(&chemin);

    assert_eq!(rapport.schema, "tv4-survivorship-v1");
    // ICN et BSV n'ont aucune paire ; AICNUSD ne correspond pas à ICN* (préfixe, pas sous-chaîne).
    assert_eq!(rapport.absent, ["ICN*", "BSV*"]);
    let motif = |nom: &str| rapport.expected.iter().find(|e| e.pattern == nom).unwrap();
    assert!(!motif("ICN*").present);
    assert!(motif("ICN*").pairs.is_empty());
    // USDTUSD ne correspond pas à UST*.
    let ust = motif("UST*");
    assert!(ust.present);
    assert_eq!(
        ust.pairs
            .iter()
            .map(|p| p.kraken_pair.as_str())
            .collect::<Vec<_>>(),
        ["USTUSD"]
    );
    assert!(!ust.review_needed);
    // WAVESEUR : toutes cotations confondues.
    assert!(motif("WAVES*").present);
    assert_eq!(motif("WAVES*").bases, ["WAVES"]);
    assert!(motif("FTT*").present);
    // L'ordre des motifs est celui de la configuration.
    let ordre: Vec<&str> = rapport
        .expected
        .iter()
        .map(|e| e.pattern.as_str())
        .collect();
    assert_eq!(ordre, ["ICN*", "BSV*", "LUNA*", "UST*", "FTT*", "WAVES*"]);
}

#[test]
fn bases_multiples_sous_un_meme_prefixe_a_revoir() {
    let dossier = tempfile::tempdir().unwrap();
    let chemin = dossier.path().join("dump.zip");
    ecrire(&chemin, &entrees(), false);
    let rapport = rapport(&chemin);
    let luna = rapport
        .expected
        .iter()
        .find(|e| e.pattern == "LUNA*")
        .unwrap();
    // LUNAUSD et LUNA2USD : deux bases canoniques, `present` n'est pas corrigé.
    assert!(luna.present);
    assert!(luna.review_needed);
    assert_eq!(luna.bases, ["LUNA", "LUNA2"]);
    assert_eq!(
        luna.pairs
            .iter()
            .map(|p| p.kraken_pair.as_str())
            .collect::<Vec<_>>(),
        ["LUNA2USD", "LUNAUSD"]
    );
}

#[test]
fn delistees_borne_haute_et_listes_triees() {
    let dossier = tempfile::tempdir().unwrap();
    let chemin = dossier.path().join("dump.zip");
    ecrire(&chemin, &entrees(), false);
    let rapport = rapport(&chemin);
    // Sans bougie dans les 90 derniers jours de la couverture : OLD (200), FTT (150), LUNA (100) et
    // UST (91). BOUND (exactement 90) et les autres ne le sont pas.
    let delistees: Vec<&str> = rapport
        .delisted
        .pairs
        .iter()
        .map(|p| p.kraken_pair.as_str())
        .collect();
    assert_eq!(delistees, ["FTTUSD", "LUNAUSD", "OLDUSD", "USTUSD"]);
    assert_eq!(rapport.delisted.count, 4);
    assert_eq!(rapport.delisted.threshold_days, 90);
    // La plus ancienne dernière bougie : OLDUSD, 200 jours avant la fin de couverture.
    let earliest = rapport.delisted.earliest.as_ref().unwrap();
    assert_eq!(earliest.kraken_pair, "OLDUSD");
    assert_eq!(earliest.last, "2026-03-15T00:00:00Z");
    assert_eq!(
        rapport.program_start.at_most.as_deref(),
        Some("2026-03-15T00:00:00Z")
    );
    assert_eq!(rapport.program_start.method, "earliest_delisted_present");
    assert!(rapport.program_start.at_least_hypothesis.contains("BSV"));
    assert_eq!(rapport.zip.coverage_end, "2026-10-01T00:00:00Z");
}

#[test]
fn source_de_la_derniere_bougie_repli_sur_60_puis_5_puis_1() {
    let dossier = tempfile::tempdir().unwrap();
    let chemin = dossier.path().join("dump.zip");
    ecrire(&chemin, &entrees(), false);
    let noms: Vec<String> = entrees().into_iter().map(|(n, _)| n).collect();
    let plan = plan_entries(&noms);
    let source = |paire: &str| {
        let p = plan.pairs.iter().find(|p| p.kraken_pair == paire).unwrap();
        (
            p.source_resolution,
            p.source_entry.clone(),
            p.resolutions.clone(),
        )
    };
    assert_eq!(
        source("XBTUSD"),
        (1440, "XBTUSD_1440.csv".to_string(), vec![1, 5, 60, 1440])
    );
    assert_eq!(
        source("CINQUSD"),
        (5, "CINQUSD_5.csv".to_string(), vec![1, 5])
    );
    assert_eq!(source("UNEUSD"), (1, "UNEUSD_1.csv".to_string(), vec![1]));
    // Sans entrée journalière mais avec une horaire : 60.
    let avec_60: Vec<String> = ["HHHUSD_1.csv", "HHHUSD_60.csv", "HHHUSD_15.csv"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(plan_entries(&avec_60).pairs[0].source_resolution, 60);
    // Le rapport du zip reprend ces résolutions.
    let rapport = rapport(&chemin);
    let motif = rapport
        .expected
        .iter()
        .find(|e| e.pattern == "UST*")
        .unwrap();
    assert_eq!(motif.pairs[0].source_resolution, 1440);
}

#[test]
fn entrees_non_reconnues_listees_et_ecartees() {
    let dossier = tempfile::tempdir().unwrap();
    let chemin = dossier.path().join("dump.zip");
    ecrire(&chemin, &entrees(), false);
    let rapport = rapport(&chemin);
    // Forme héritée, nom étranger, nom mal formé et entrée sans bougie ; MANIFEST.json n'en est pas.
    assert_eq!(
        rapport.unrecognized_entries,
        [
            "README.txt",
            "VIDEUSD_1440.csv",
            "XXBTZUSD_1440.csv",
            "abc_1440.csv"
        ]
    );
    // 20 entrées + MANIFEST.json ; 12 paires observées (ni XXBTZUSD, ni VIDEUSD dont la source est vide).
    assert_eq!(rapport.zip.entries, 21);
    assert_eq!(rapport.zip.pairs, 12);
    assert_eq!(rapport.zip.release.as_deref(), Some("r1"));
    assert_eq!(rapport.zip.coverage_start.as_deref(), Some("2013-09-11"));
}

#[test]
fn deux_executions_et_ordre_des_entrees_donnent_les_memes_octets() {
    let dossier = tempfile::tempdir().unwrap();
    let a = dossier.path().join("a.zip");
    let b = dossier.path().join("b.zip");
    ecrire(&a, &entrees(), false);
    ecrire(&b, &entrees(), true);
    let (ra, rb) = (rapport(&a), rapport(&b));
    assert_eq!(
        ra.to_json_pretty().unwrap(),
        rapport(&a).to_json_pretty().unwrap()
    );
    // Le SHA-256 du zip diffère (entrées dans un autre ordre) : tout le reste est identique.
    let mut rb_meme_sha = rb.clone();
    rb_meme_sha.zip.sha256 = ra.zip.sha256.clone();
    rb_meme_sha.report_hash = rb_meme_sha.compute_report_hash().unwrap();
    assert_eq!(
        rb_meme_sha.to_json_pretty().unwrap(),
        ra.to_json_pretty().unwrap()
    );
    // Le hash d'intégrité se recalcule, et le JSON est à clés triées avec un `\n` final.
    assert_eq!(ra.report_hash, ra.compute_report_hash().unwrap());
    assert!(ra.report_hash.starts_with("blake3:"));
    let texte = ra.to_json_pretty().unwrap();
    assert!(texte.ends_with("}\n"));
    let valeur: serde_json::Value = serde_json::from_str(&texte).unwrap();
    let cles: Vec<&String> = valeur.as_object().unwrap().keys().collect();
    let mut triees = cles.clone();
    triees.sort();
    assert_eq!(cles, triees);
    // Le rapport se relit (schéma stable).
    let relu: SurvivorshipReport = serde_json::from_str(&texte).unwrap();
    assert_eq!(relu, ra);
}

#[test]
fn build_report_est_pur_et_trie_les_observations() {
    let obs = |paire: &str, derniere: Ts| PairObservation {
        kraken_pair: paire.to_string(),
        resolutions: vec![1440],
        source_resolution: 1440,
        first_ms: derniere - 10 * DAY,
        last_ms: derniere,
    };
    let f = fin();
    let zip = ZipSummary {
        sha256: "0".repeat(64),
        release: None,
        coverage_start: None,
        generated_at: None,
        entries: 3,
    };
    let ordre_a = [
        obs("ZZZUSD", f - 300 * DAY),
        obs("AAAUSD", f - 300 * DAY),
        obs("ICNUSD", f - DAY),
    ];
    let ordre_b = [ordre_a[2].clone(), ordre_a[0].clone(), ordre_a[1].clone()];
    let ra = build_report(&zip, f, &ordre_a, &[], &config()).unwrap();
    let rb = build_report(&zip, f, &ordre_b, &[], &config()).unwrap();
    assert_eq!(ra, rb);
    // Égalité de la dernière bougie : le nom en ordre d'octets départage `earliest`.
    assert_eq!(ra.delisted.earliest.as_ref().unwrap().kraken_pair, "AAAUSD");
    assert_eq!(ra.zip.pairs, 3);
    assert!(
        ra.expected
            .iter()
            .find(|e| e.pattern == "ICN*")
            .unwrap()
            .present
    );
    assert_eq!(ra.absent, ["BSV*", "LUNA*", "UST*", "FTT*", "WAVES*"]);
}

#[test]
fn bornes_d_un_flux_non_trie_et_flux_vide() {
    let ligne = |t: Ts| -> Result<RawRow, KrakenError> {
        Ok(RawRow {
            line: 1,
            open_time_ms: t,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            trades: 1,
        })
    };
    let rows = vec![ligne(5 * DAY), ligne(DAY), ligne(9 * DAY), ligne(3 * DAY)];
    assert_eq!(
        span_of_rows(rows.into_iter()).unwrap(),
        Some((DAY, 9 * DAY))
    );
    assert_eq!(span_of_rows(std::iter::empty()).unwrap(), None);
}

#[test]
fn le_repli_de_resolution_lit_bien_l_entree_choisie() {
    let dossier = tempfile::tempdir().unwrap();
    let chemin = dossier.path().join("dump.zip");
    ecrire(&chemin, &entrees(), false);
    // Motifs choisis pour voir les paires de repli dans le rapport.
    let config = UniverseConfig::from_toml_str(
        r#"
schema = "tv4-universe-config-v1"
rules_version = "u1"
quotes = ["USD", "USDT"]
excluded_bases = ["USDT"]

[survivorship]
expected = ["CINQ", "UNE", "XBT"]

[okx_base_aliases]
"#,
    )
    .unwrap();
    let sha: Sha256Hex = sha256_fichier(&chemin).parse().unwrap();
    let mut zip = KrakenZip::open_verified(&chemin, &sha).unwrap();
    let rapport = survey_zip(&mut zip, &config, fin()).unwrap();
    let paire = |motif: &str| {
        rapport
            .expected
            .iter()
            .find(|e| e.pattern == motif)
            .unwrap()
            .pairs[0]
            .clone()
    };
    let cinq = paire("CINQ*");
    assert_eq!(cinq.source_resolution, 5);
    assert_eq!(cinq.last, "2026-09-28T00:00:00Z");
    assert_eq!(cinq.first, "2026-09-23T00:00:00Z");
    assert_eq!(paire("UNE*").source_resolution, 1);
    assert_eq!(paire("XBT*").source_resolution, 1440);
    assert_eq!(paire("XBT*").resolutions, [1, 5, 60, 1440]);
}
