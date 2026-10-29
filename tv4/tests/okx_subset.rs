//! Tests de la liste figée des bases tradables sur OKX en USDC (docs/specs/s11.md §7, lot C).
//!
//! Fixtures : `tests/fixtures/okx_instruments_spot.json` (4 instruments USDC, ADA `suspend`) et
//! `tests/fixtures/okx_instruments_usd_unified.json`, **inventée pour ce test** (forme du carnet USD
//! unifié d'OKX, où l'USDC n'apparaît que dans `tradeQuoteCcyList` : `BTC-USD` live, `ETH-USD` sans
//! USDC, `SOL-USD` suspendu, `XRP-USD` en préouverture, `ADA-USDC` live). Aucune de ces valeurs n'est lue
//! sur un compte OKX : la forme réelle reste à valider sur le fichier du propriétaire (S9 §8.7). Les
//! instruments ajoutés en cours de test sont aussi inventés.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tv4::marketdata::okx::instruments::{InstrumentsError, InstrumentsFile};
use tv4::model::universe::{PairSubset, UniverseConfig};
use tv4::research::universe::{UniverseError, okx_usdc_subset, write_okx_usdc_subset};
use tv4::store::StoreError;
use tv4::store::universe::read_pair_subset;

fn fixture(nom: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(nom)
}

fn config_texte(alias: &str) -> String {
    format!(
        r#"
schema = "tv4-universe-config-v1"
rules_version = "u1"
quotes = ["USD", "USDT"]
excluded_bases = ["USDT"]

[survivorship]
expected = ["ICN"]

[okx_base_aliases]
{alias}
"#
    )
}

fn config(alias: &str) -> UniverseConfig {
    UniverseConfig::from_toml_str(&config_texte(alias)).unwrap()
}

fn instruments(nom: &str) -> InstrumentsFile {
    InstrumentsFile::load(&fixture(nom)).unwrap()
}

fn ligne(inst_id: &str, base: &str, quote: &str, state: &str, liste: &[&str]) -> Value {
    json!({
        "instId": inst_id, "baseCcy": base, "quoteCcy": quote, "tickSz": "0.01", "lotSz": "0.000001",
        "minSz": "1", "state": state, "listTime": "1755763200000", "ruleType": "normal",
        "tradeQuoteCcyList": liste, "upcChg": []
    })
}

fn fichier(lignes: &[Value]) -> InstrumentsFile {
    InstrumentsFile::from_response("eea", "SPOT", 1_700_000_000_123, lignes).unwrap()
}

#[test]
fn la_fixture_spot_donne_btc_eth_sol_et_pas_ada_suspendue() {
    let subset = okx_usdc_subset(
        &instruments("okx_instruments_spot.json"),
        "okx_instruments_spot.json",
        &config(""),
    )
    .unwrap();
    assert_eq!(subset.bases, ["BTC", "ETH", "SOL"]);
    assert!(subset.contains_base("SOL"));
    assert!(!subset.contains_base("ADA"));
    assert_eq!(subset.kind, "okx_usdc_tradable");
    assert_eq!(subset.schema, "tv4-pair-subset-v1");
    assert_eq!(subset.rule.quote, "USDC");
    assert_eq!(subset.rule.state, "live");
    assert!(subset.rule.trade_quote_ccy);
    assert!(subset.rule.aliases.is_empty());
    assert_eq!(subset.source.instruments_file, "okx_instruments_spot.json");
    assert_eq!(subset.source.site, "eea");
    assert_eq!(
        subset.source.instruments_hash,
        "blake3:99e4c377d7c8bae9c2f26a867c209ad77a88856bff57d80a40de744e7b77c90b"
    );
    assert!(!subset.is_empty());
}

#[test]
fn la_forme_usd_unifiee_est_tradable_par_la_seule_liste_de_cotations() {
    let subset = okx_usdc_subset(
        &instruments("okx_instruments_usd_unified.json"),
        "okx_instruments_usd_unified.json",
        &config(""),
    )
    .unwrap();
    // BTC-USD (live, USDC dans tradeQuoteCcyList) et ADA-USDC (cotation USDC) ; ETH-USD n'a pas
    // l'USDC, SOL-USD est suspendu, XRP-USD en préouverture.
    assert_eq!(subset.bases, ["ADA", "BTC"]);
}

#[test]
fn hash_stable_et_independant_de_la_date_de_lecture_et_du_nom() {
    let ligne_btc = ligne("BTC-USDC", "BTC", "USDC", "live", &["USDC"]);
    let a = fichier(std::slice::from_ref(&ligne_btc));
    let b = InstrumentsFile::from_response("eea", "SPOT", 1_800_000_000_000, &[ligne_btc]).unwrap();
    assert_ne!(a.fetched_at, b.fetched_at);
    let sa = okx_usdc_subset(&a, "spot-a.json", &config("")).unwrap();
    let sb = okx_usdc_subset(&b, "autre-nom.json", &config("")).unwrap();
    assert_eq!(sa.subset_hash, sb.subset_hash);
    assert_ne!(sa.source, sb.source);
    assert_eq!(sa.subset_hash, sa.compute_hash().unwrap());
    // Un instrument de plus change le hash ; un alias aussi.
    let plus = fichier(&[
        ligne("BTC-USDC", "BTC", "USDC", "live", &["USDC"]),
        ligne("ETH-USDC", "ETH", "USDC", "live", &["USDC"]),
    ]);
    let s_plus = okx_usdc_subset(&plus, "x.json", &config("")).unwrap();
    assert_ne!(s_plus.subset_hash, sa.subset_hash);
    let s_alias = okx_usdc_subset(&a, "x.json", &config(r#"LUNA = "LUNC""#)).unwrap();
    assert_ne!(s_alias.subset_hash, sa.subset_hash);
    assert_eq!(
        s_alias.rule.aliases.get("LUNA").map(String::as_str),
        Some("LUNC")
    );
}

#[test]
fn aucun_alias_n_est_devine_et_l_alias_explicite_remplace_l_egalite() {
    let terra = fichier(&[
        ligne("LUNC-USDC", "LUNC", "USDC", "live", &["USDC"]),
        ligne("BTC-USDC", "BTC", "USDC", "live", &["USDC"]),
        ligne("TERRA-USDC", "TERRA", "USDC", "live", &["USDC"]),
    ]);
    // Sans alias : égalité stricte des bases, LUNA (Kraken) n'est pas LUNC (OKX).
    let sans = okx_usdc_subset(&terra, "x.json", &config("")).unwrap();
    assert_eq!(sans.bases, ["BTC", "LUNC", "TERRA"]);
    assert!(!sans.contains_base("LUNA"));
    // Avec LUNA = LUNC : la base Kraken LUNA est tradable par son alias.
    let avec = okx_usdc_subset(&terra, "x.json", &config(r#"LUNA = "LUNC""#)).unwrap();
    assert_eq!(avec.bases, ["BTC", "LUNA", "LUNC", "TERRA"]);
    // Un alias vers une base non tradable retire la base Kraken, même si OKX a un jeton du même nom :
    // OKX LUNA (autre jeton) n'est pas le LUNA de Kraken quand l'alias dit que ce dernier est LUNC.
    let autre = fichier(&[
        ligne("LUNA-USDC", "LUNA", "USDC", "live", &["USDC"]),
        ligne("BTC-USDC", "BTC", "USDC", "live", &["USDC"]),
    ]);
    let redirige = okx_usdc_subset(&autre, "x.json", &config(r#"LUNA = "LUNC""#)).unwrap();
    assert_eq!(redirige.bases, ["BTC"]);
}

#[test]
fn etats_et_cotations_hors_regle_sont_ecartes() {
    let melange = fichier(&[
        ligne("A-USDC", "A", "USDC", "live", &["USDC"]),
        ligne("B-USDC", "B", "USDC", "suspend", &["USDC"]),
        ligne("C-USDC", "C", "USDC", "preopen", &["USDC"]),
        ligne("D-EUR", "D", "EUR", "live", &["EUR"]),
        ligne("E-USD", "E", "USD", "live", &["USD", "USDT"]),
        ligne("F-USD", "F", "USD", "live", &["USDC", "USD"]),
    ]);
    let subset = okx_usdc_subset(&melange, "x.json", &config("")).unwrap();
    assert_eq!(subset.bases, ["A", "F"]);
}

#[test]
fn liste_vide_possible_et_fichier_non_spot_refuse() {
    let aucun = fichier(&[ligne("D-EUR", "D", "EUR", "live", &["EUR"])]);
    let subset = okx_usdc_subset(&aucun, "x.json", &config("")).unwrap();
    assert!(subset.is_empty());
    assert_eq!(subset.bases, Vec::<String>::new());
    let mut swap = aucun.clone();
    swap.inst_type = "SWAP".to_string();
    assert!(matches!(
        okx_usdc_subset(&swap, "x.json", &config("")),
        Err(UniverseError::NotOkxSpot { .. })
    ));
}

#[test]
fn ecriture_verifiee_refus_d_ecraser_et_hash_des_instruments() {
    let dossier = tempfile::tempdir().unwrap();
    let sortie = dossier.path().join("okx_usdc.json");
    let subset =
        write_okx_usdc_subset(&fixture("okx_instruments_spot.json"), &config(""), &sortie).unwrap();
    // Relecture vérifiée : schéma, tri et hash.
    let relu = read_pair_subset(&sortie).unwrap();
    assert_eq!(relu, subset);
    let texte = fs::read_to_string(&sortie).unwrap();
    assert!(texte.ends_with("}\n"));
    assert_eq!(PairSubset::from_json_str(&texte).unwrap(), subset);
    let valeur: Value = serde_json::from_str(&texte).unwrap();
    assert_eq!(valeur["bases"], json!(["BTC", "ETH", "SOL"]));
    assert_eq!(valeur["subset_hash"], json!(subset.subset_hash));
    // Un fichier existant n'est jamais écrasé, même avec un contenu différent.
    let avant = fs::read(&sortie).unwrap();
    let erreur = write_okx_usdc_subset(
        &fixture("okx_instruments_usd_unified.json"),
        &config(""),
        &sortie,
    )
    .unwrap_err();
    assert!(
        matches!(
            erreur,
            UniverseError::Store(StoreError::SubsetExists { .. })
        ),
        "{erreur}"
    );
    assert_eq!(fs::read(&sortie).unwrap(), avant);
    // Une liste modifiée à la main est refusée à la lecture.
    let altere = texte.replace("\"ETH\"", "\"DOGE\"");
    fs::write(&sortie, altere).unwrap();
    assert!(read_pair_subset(&sortie).is_err());
}

#[test]
fn instruments_modifies_a_la_main_erreur_de_hash() {
    let dossier = tempfile::tempdir().unwrap();
    let copie = dossier.path().join("instruments.json");
    let texte = fs::read_to_string(fixture("okx_instruments_spot.json")).unwrap();
    // BTC-USDC passe de `live` à `suspend` sans recalcul du hash.
    let altere = texte.replacen("\"state\": \"live\"", "\"state\": \"suspend\"", 1);
    assert_ne!(altere, texte);
    fs::write(&copie, altere).unwrap();
    let sortie = dossier.path().join("okx_usdc.json");
    let erreur = write_okx_usdc_subset(&copie, &config(""), &sortie).unwrap_err();
    assert!(
        matches!(
            erreur,
            UniverseError::Instruments(InstrumentsError::HashMismatch { .. })
        ),
        "{erreur}"
    );
    assert!(!sortie.exists());
}

#[test]
fn la_configuration_du_depot_a_une_table_d_alias_vide() {
    let depot = UniverseConfig::load(&PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../config/universe.toml"
    )))
    .unwrap();
    assert!(depot.okx_base_aliases.is_empty());
    let subset = okx_usdc_subset(
        &instruments("okx_instruments_spot.json"),
        "okx_instruments_spot.json",
        &depot,
    )
    .unwrap();
    assert_eq!(subset.bases, ["BTC", "ETH", "SOL"]);
}
