//! Modèle de coûts de `model::costs` (docs/specs/s10.md §4 et §5.1) : lecture et validation de
//! `config/costs.toml`, fichier de mesures, classes sans constante, résolution par profil, loi du
//! stop. Aucune valeur de marché n'est inventée : les valeurs lues sont celles du fichier du dépôt
//! (qui cite 04 §3 et §3.1), et les autres sont des gabarits de test.

use std::path::Path;

use tv4::model::costs::{
    ClassSource, CostMeasures, CostProfileName, CostsFile, EntryMode, LegCost, PairClass,
    STOP_QUANTILE_NODES, StopCost, StopSlippage, StopSlippageBps, ValueSource, base_asset,
    resolve_leg,
};
use tv4::model::hash::blake3_hex;
use tv4::model::rng::leg_key;
use tv4::model::{ModelError, Slippage, Timeframe};

/// Copie figée des défauts de §4.1 (`tests/fixtures/costs_c1.toml`) : tous les tests qui épinglent une
/// valeur ou un hash la lisent. La calibration du stress (§11.3) réécrit `config/costs.toml` sans la
/// toucher.
const FROZEN_TOML: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/costs_c1.toml");

/// Le vrai fichier du dépôt : réservé aux tests de forme, sans valeur épinglée.
const REPO_TOML: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/costs.toml");

fn frozen_text() -> String {
    std::fs::read_to_string(FROZEN_TOML).unwrap()
}

fn frozen_costs() -> CostsFile {
    CostsFile::load(Path::new(FROZEN_TOML)).unwrap()
}

/// Copie figée dont chaque `ancien` (présent exactement une fois au moins) est remplacé, une
/// fois, par `nouveau`.
fn text_with(replacements: &[(&str, &str)]) -> String {
    let mut text = frozen_text();
    for (old, new) in replacements {
        assert!(
            text.contains(old),
            "gabarit absent de la copie figée : {old}"
        );
        text = text.replacen(old, new, 1);
    }
    text
}

fn costs_with(replacements: &[(&str, &str)]) -> CostsFile {
    CostsFile::from_toml_str(&text_with(replacements)).unwrap()
}

fn invalid_field(error: ModelError) -> String {
    match error {
        ModelError::CostsInvalid { field, .. } => field,
        autre => panic!("CostsInvalid attendu, reçu : {autre}"),
    }
}

fn measures_json(base_block: &str) -> String {
    format!(
        r#"{{"schema":"tv4-cost-measures-v1","venue":"okx","captured_from_ms":1000,
        "captured_to_ms":172801000,"bases":{{"BTC":{base_block}}}}}"#
    )
}

const FULL_BASE: &str = r#"{"inst_id":"BTC-USDC","hours":30.0,
    "half_spread_bps":{"median":0.8,"p95":4.0},"market_slippage_bps":{"median":1.5}}"#;

fn resolve(
    file: &CostsFile,
    measures: Option<&CostMeasures>,
    profile: CostProfileName,
    symbol: &str,
    tf: Timeframe,
) -> Result<(LegCost, tv4::model::costs::LegCostRecord), ModelError> {
    resolve_leg(file, measures, profile, symbol, tf, 1)
}

// --- Fichier de coûts ---

#[test]
fn la_copie_figee_porte_les_defauts_de_la_spec() {
    let file = frozen_costs();
    assert_eq!(file.schema, "tv4-costs-v1");
    assert_eq!(file.rules_version, "c1");
    assert!(!file.stress_calibrated);
    assert_eq!(file.calibration_ref(), None);
    assert_eq!(file.classes.major, ["BTC", "ETH"]);
    assert!(file.classes.top20.is_empty());
    assert_eq!(file.file_hash(), blake3_hex(frozen_text().as_bytes()));
    assert_eq!(file.file_hash().len(), 64);
}

/// Test de forme sur le vrai fichier du dépôt, sans valeur épinglée : il reste vrai avant comme après
/// l'inscription du stress calibré (spec §11.3).
#[test]
fn le_fichier_du_depot_se_lit_valide_et_se_hache() {
    let bytes = std::fs::read(REPO_TOML).unwrap();
    let file = CostsFile::load(Path::new(REPO_TOML)).unwrap();
    assert_eq!(file.schema, "tv4-costs-v1");
    assert_eq!(file.rules_version, "c1");
    assert_eq!(file.file_hash(), blake3_hex(&bytes));
    assert_eq!(file.file_hash().len(), 64);
    assert_eq!(
        file.stress_calibrated,
        file.calibration_ref().is_some(),
        "stress_calibrated et la référence de calibration vont ensemble"
    );
}

/// Anti-dérive : la copie figée et le fichier du dépôt ne diffèrent que par le stress calibré (spec
/// §11.3). Toute autre valeur modifiée dans `config/costs.toml` doit l'être aussi dans la copie, ou la
/// copie ne représente plus les défauts livrés. Comparaison des valeurs lues, pas des octets.
#[test]
fn la_copie_figee_ne_differe_du_depot_que_par_le_stress_calibre() {
    let repo = CostsFile::load(Path::new(REPO_TOML)).unwrap();
    let frozen = frozen_costs();
    // Le hash du fichier (octets) et les trois champs du stress calibré sont les seuls écarts permis.
    let sans_stress = |file: &CostsFile| {
        let mut defaults = file.defaults;
        for class in [
            &mut defaults.major,
            &mut defaults.top20,
            &mut defaults.beyond,
        ] {
            class.stress_probability = 0.0;
        }
        defaults
    };
    assert_eq!(repo.schema, frozen.schema);
    assert_eq!(repo.rules_version, frozen.rules_version);
    assert_eq!(repo.classes, frozen.classes);
    assert_eq!(
        sans_stress(&repo),
        sans_stress(&frozen),
        "défauts de classe"
    );
    assert_eq!(repo.profiles, frozen.profiles);
}

#[test]
fn le_hash_du_fichier_suit_ses_octets() {
    let a = CostsFile::from_toml_str(&frozen_text()).unwrap();
    let b = CostsFile::from_toml_str(&format!("{}\n", frozen_text())).unwrap();
    assert_ne!(a.file_hash(), b.file_hash(), "un octet change le hash");
    assert_eq!(a, CostsFile::from_toml_str(&frozen_text()).unwrap());
}

/// Un entier TOML (`p50 = 5`) est accepté et lu comme le même flottant : mêmes valeurs résolues que `5.0`
/// (seul `costs_file_hash`, haché sur les octets, diffère). Non-régression : la spec ne promet aucun refus.
#[test]
fn un_entier_toml_dans_la_loi_du_stop_vaut_le_flottant() {
    let flottant = frozen_costs();
    let entier = costs_with(&[("p50 = 5.0", "p50 = 5")]);
    assert_ne!(entier.file_hash(), flottant.file_hash());
    for profile in [
        CostProfileName::Baseline,
        CostProfileName::Adverse,
        CostProfileName::Maker,
    ] {
        let a = resolve(&flottant, None, profile, "BTC-USD", Timeframe::D1).unwrap();
        let b = resolve(&entier, None, profile, "BTC-USD", Timeframe::D1).unwrap();
        assert_eq!(a.0, b.0, "{profile:?}");
        assert_eq!(a.1, b.1, "{profile:?}");
    }
}

#[test]
fn la_reference_de_calibration_se_lit() {
    let file = costs_with(&[(
        "stress_calibration_ref = \"\"",
        "stress_calibration_ref = \"c.json\"",
    )]);
    assert_eq!(file.calibration_ref(), Some("c.json"));
}

#[test]
fn un_fichier_de_couts_illisible_est_refuse() {
    let absent = CostsFile::load(Path::new("/n/existe/pas.toml"));
    assert!(matches!(absent, Err(ModelError::ConfigRead { .. })));
    let invalide = CostsFile::from_toml_str("schema = 3");
    assert!(matches!(invalide, Err(ModelError::ConfigToml(_))));
}

#[test]
fn une_cle_inconnue_est_refusee_partout() {
    let racine = format!("bogus = 1\n{}", frozen_text());
    assert!(matches!(
        CostsFile::from_toml_str(&racine),
        Err(ModelError::ConfigToml(_))
    ));
    for (ancien, nouveau) in [
        ("half_spread_bps = 0.5", "half_spread_bps = 0.5\nbogus = 1"),
        ("p99 = 200.0, p100", "p99 = 200.0, bogus = 1.0, p100"),
        (
            "half_spread_mult = 2.0",
            "half_spread_mult = 2.0\nbogus = 1",
        ),
        (
            "fill_probability = 0.45",
            "fill_probability = 0.45\nbogus = 1",
        ),
        ("top20 = []", "top20 = []\nbogus = []"),
    ] {
        let error = CostsFile::from_toml_str(&text_with(&[(ancien, nouveau)])).unwrap_err();
        assert!(matches!(error, ModelError::ConfigToml(_)), "{ancien}");
    }
}

#[test]
fn schema_et_version_de_regles_sont_verifies() {
    let schema = CostsFile::from_toml_str(&text_with(&[(
        "schema = \"tv4-costs-v1\"",
        "schema = \"tv4-costs-v2\"",
    )]));
    assert_eq!(invalid_field(schema.unwrap_err()), "schema");
    let rules = CostsFile::from_toml_str(&text_with(&[(
        "rules_version = \"c1\"",
        "rules_version = \"c2\"",
    )]));
    assert_eq!(invalid_field(rules.unwrap_err()), "rules_version");
}

#[test]
fn un_actif_dans_deux_classes_est_refuse() {
    let error =
        CostsFile::from_toml_str(&text_with(&[("top20 = []", "top20 = [\"BTC\"]")])).unwrap_err();
    assert_eq!(invalid_field(error), "classes.top20");
    let deux_fois = CostsFile::from_toml_str(&text_with(&[(
        "major = [\"BTC\", \"ETH\"]",
        "major = [\"BTC\", \"BTC\"]",
    )]));
    assert_eq!(invalid_field(deux_fois.unwrap_err()), "classes.major");
    let vide = CostsFile::from_toml_str(&text_with(&[("top20 = []", "top20 = [\"\"]")]));
    assert_eq!(invalid_field(vide.unwrap_err()), "classes.top20");
}

#[test]
fn des_quantiles_decroissants_sont_refuses() {
    let error = CostsFile::from_toml_str(&text_with(&[(
        "p95 = 50.0, p99 = 200.0",
        "p95 = 4.0, p99 = 200.0",
    )]))
    .unwrap_err();
    assert_eq!(invalid_field(error), "defaults.major.stop_slippage_bps");
    let p100 = CostsFile::from_toml_str(&text_with(&[(
        "p99 = 200.0, p100 = 200.0",
        "p99 = 200.0, p100 = 100.0",
    )]));
    assert_eq!(
        invalid_field(p100.unwrap_err()),
        "defaults.major.stop_slippage_bps"
    );
}

#[test]
fn une_probabilite_hors_de_zero_un_est_refusee() {
    let directe = CostsFile::from_toml_str(&text_with(&[(
        "stress_probability = 0.03\nstress_drop = 0.03",
        "stress_probability = 1.5\nstress_drop = 0.03",
    )]));
    assert_eq!(
        invalid_field(directe.unwrap_err()),
        "defaults.major.stress_probability"
    );
    let fill = CostsFile::from_toml_str(&text_with(&[(
        "fill_probability = 0.45",
        "fill_probability = 1.01",
    )]));
    assert_eq!(
        invalid_field(fill.unwrap_err()),
        "profiles.maker.fill_probability"
    );
    let part = CostsFile::from_toml_str(&text_with(&[(
        "min_maker_entry_share = 0.5",
        "min_maker_entry_share = 1.5",
    )]));
    assert_eq!(
        invalid_field(part.unwrap_err()),
        "profiles.maker.min_maker_entry_share"
    );
}

#[test]
fn une_probabilite_multipliee_au_dela_de_un_est_refusee() {
    // 0,6 est une probabilité, mais 0,6 × 2 (profil adverse) n'en est plus une.
    let error = CostsFile::from_toml_str(&text_with(&[(
        "stress_probability = 0.03\nstress_drop = 0.03",
        "stress_probability = 0.6\nstress_drop = 0.03",
    )]))
    .unwrap_err();
    assert_eq!(invalid_field(error), "defaults.major.stress_probability");
    // Et 0,5 × 2 = 1 reste admis.
    costs_with(&[(
        "stress_probability = 0.03\nstress_drop = 0.03",
        "stress_probability = 0.5\nstress_drop = 0.03",
    )]);
}

#[test]
fn les_valeurs_negatives_non_finies_ou_hors_bornes_sont_refusees() {
    for (ancien, nouveau, champ) in [
        (
            "half_spread_bps = 0.5",
            "half_spread_bps = -1.0",
            "defaults.major.half_spread_bps",
        ),
        (
            "half_spread_bps = 0.5",
            "half_spread_bps = nan",
            "defaults.major.half_spread_bps",
        ),
        (
            "market_slippage_bps = 1.0",
            "market_slippage_bps = inf",
            "defaults.major.market_slippage_bps",
        ),
        (
            "p50 = 5.0",
            "p50 = -5.0",
            "defaults.major.stop_slippage_bps.p50",
        ),
        (
            "stress_drop = 0.03",
            "stress_drop = 0.0",
            "defaults.major.stress_drop",
        ),
        (
            "stress_drop = 0.03",
            "stress_drop = 1.0",
            "defaults.major.stress_drop",
        ),
        (
            "half_spread_mult = 2.0",
            "half_spread_mult = -2.0",
            "profiles.adverse.half_spread_mult",
        ),
        (
            "adverse_selection_bps = 1.5",
            "adverse_selection_bps = 10000.0",
            "profiles.maker.adverse_selection_bps",
        ),
        (
            "entry_offset_bps = 0.0",
            "entry_offset_bps = 10000.0",
            "profiles.maker.entry_offset_bps",
        ),
        (
            "ttl_minutes_1d = 1440",
            "ttl_minutes_1d = 0",
            "profiles.maker.ttl_minutes_1d",
        ),
        (
            "ttl_minutes_max_intraday = 60",
            "ttl_minutes_max_intraday = 0",
            "profiles.maker.ttl_minutes_max_intraday",
        ),
    ] {
        let error = CostsFile::from_toml_str(&text_with(&[(ancien, nouveau)])).unwrap_err();
        assert_eq!(invalid_field(error), champ, "{nouveau}");
    }
}

// --- Classes sans constante dans le code ---

#[test]
fn la_base_est_le_texte_avant_le_premier_tiret() {
    assert_eq!(base_asset("BTC-USD").unwrap(), "BTC");
    assert_eq!(base_asset("ETH-USDC").unwrap(), "ETH");
    assert_eq!(base_asset("A-B-C").unwrap(), "A");
    for mauvais in ["BTCUSD", "", "-USD"] {
        assert!(
            matches!(base_asset(mauvais), Err(ModelError::CostsSymbol { .. })),
            "{mauvais}"
        );
    }
}

#[test]
fn les_classes_viennent_du_fichier() {
    let file = frozen_costs();
    assert_eq!(file.class_of("BTC"), (PairClass::Major, ClassSource::List));
    assert_eq!(file.class_of("ETH"), (PairClass::Major, ClassSource::List));
    assert_eq!(
        file.class_of("XRP"),
        (PairClass::Beyond, ClassSource::Fallback)
    );
    for (symbol, classe) in [
        ("BTC-USD", PairClass::Major),
        ("ETH-USDC", PairClass::Major),
    ] {
        let (_, record) = resolve(
            &file,
            None,
            CostProfileName::Baseline,
            symbol,
            Timeframe::D1,
        )
        .unwrap();
        assert_eq!(record.class, classe);
        assert_eq!(record.class_source, ClassSource::List);
    }
    let (_, xrp) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "XRP-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(xrp.class, PairClass::Beyond);
    assert_eq!(xrp.class_source, ClassSource::Fallback);
    assert_eq!(xrp.half_spread_bps, file.defaults.beyond.half_spread_bps);
}

#[test]
fn une_liste_top20_de_test_est_respectee() {
    let file = costs_with(&[("top20 = []", "top20 = [\"XRP\", \"SOL\"]")]);
    for base in ["XRP", "SOL"] {
        assert_eq!(file.class_of(base), (PairClass::Top20, ClassSource::List));
    }
    let (_, record) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "SOL-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(record.class, PairClass::Top20);
    assert_eq!(record.class_source, ClassSource::List);
    assert_eq!(record.half_spread_bps, file.defaults.top20.half_spread_bps);
    assert_eq!(
        record.stop_slippage_bps,
        file.defaults.top20.stop_slippage_bps
    );
    // `major` garde la priorité d'une liste, et DOGE reste au repli.
    assert_eq!(file.class_of("BTC").0, PairClass::Major);
    assert_eq!(file.class_of("DOGE").0, PairClass::Beyond);
}

// --- Loi du stop ---

fn law(p50: f64, p95: f64, p99: f64, p100: f64) -> StopSlippage {
    StopSlippage::from_quantiles(StopSlippageBps {
        p50,
        p95,
        p99,
        p100,
    })
}

#[test]
fn le_quantile_est_exact_aux_noeuds() {
    let l = law(5.0, 50.0, 200.0, 300.0);
    assert_eq!(l.quantile_bps(0.0), 0.0);
    assert_eq!(l.quantile_bps(0.5), 5.0);
    assert_eq!(l.quantile_bps(0.95), 50.0);
    assert_eq!(l.quantile_bps(0.99), 200.0);
    // `u = 1` est hors de [0, 1) : il vaut p100.
    assert_eq!(l.quantile_bps(1.0), 300.0);
    assert_eq!(
        l.quantiles(),
        StopSlippageBps {
            p50: 5.0,
            p95: 50.0,
            p99: 200.0,
            p100: 300.0
        }
    );
}

#[test]
fn le_quantile_interpole_au_milieu_des_segments() {
    let l = law(5.0, 50.0, 200.0, 300.0);
    let nodes = STOP_QUANTILE_NODES;
    let values = [0.0, 5.0, 50.0, 200.0, 300.0];
    for k in 0..4 {
        let u = (nodes[k] + nodes[k + 1]) / 2.0;
        let attendu = (values[k] + values[k + 1]) / 2.0;
        assert!((l.quantile_bps(u) - attendu).abs() < 1e-12, "segment {k}");
    }
    // Un quart du premier segment : un quart de p50.
    assert!((l.quantile_bps(0.125) - 1.25).abs() < 1e-12);
}

#[test]
fn le_quantile_est_croissant_borne_et_nul_en_zero() {
    for l in [
        law(5.0, 50.0, 200.0, 200.0),
        law(5.0, 50.0, 200.0, 300.0),
        law(0.0, 0.0, 0.0, 0.0),
        law(1.0, 1.0, 1.0, 1.0),
        law(15.0, 150.0, 600.0, 600.0),
    ] {
        let p100 = l.quantiles().p100;
        assert_eq!(l.quantile_bps(0.0), 0.0);
        let mut precedent = 0.0;
        for i in 0..=100_000_u32 {
            let u = f64::from(i) / 100_001.0;
            let q = l.quantile_bps(u);
            assert!(q >= precedent, "décroissant en u = {u}");
            assert!(q <= p100, "au-dessus de p100 en u = {u}");
            assert!(q >= 0.0);
            precedent = q;
        }
        // Le plus grand `u < 1` reste borné.
        assert!(l.quantile_bps(1.0 - f64::EPSILON / 2.0) <= p100);
    }
}

#[test]
fn les_u_hors_domaine_sont_bornes() {
    let l = law(5.0, 50.0, 200.0, 300.0);
    assert_eq!(l.quantile_bps(-0.5), 0.0);
    assert_eq!(l.quantile_bps(f64::NAN), 0.0);
    assert_eq!(l.quantile_bps(1.5), 300.0);
}

/// Intégrale de `quantile_bps` sur [0, 1] : règle du point milieu **par segment**, exacte pour une
/// fonction linéaire, aux nœuds de la loi.
fn numeric_mean(l: &StopSlippage) -> f64 {
    let nodes = STOP_QUANTILE_NODES;
    let steps = 2_000_u32;
    let mut total = 0.0;
    for k in 0..4 {
        let width = (nodes[k + 1] - nodes[k]) / f64::from(steps);
        let mut segment = 0.0;
        for i in 0..steps {
            let u = nodes[k] + (f64::from(i) + 0.5) * width;
            segment += l.quantile_bps(u) * width;
        }
        total += segment;
    }
    total
}

#[test]
fn la_moyenne_des_defauts_majors_est_20_625_bp() {
    let file = frozen_costs();
    let q = file.defaults.major.stop_slippage_bps;
    let l = StopSlippage::from_quantiles(q);
    assert!((l.mean_bps() - 20.625).abs() < 1e-12, "{}", l.mean_bps());
    assert!(
        (numeric_mean(&l) - 20.625).abs() < 1e-12,
        "{}",
        numeric_mean(&l)
    );
}

#[test]
fn la_moyenne_est_triplee_en_adverse() {
    let file = frozen_costs();
    let (_, baseline) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    let (_, adverse) = resolve(
        &file,
        None,
        CostProfileName::Adverse,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert!((baseline.stop_slippage_mean_bps - 20.625).abs() < 1e-12);
    assert!((adverse.stop_slippage_mean_bps - 61.875).abs() < 1e-12);
    let StopCost::Drawn { dist, .. } = resolve(
        &file,
        None,
        CostProfileName::Adverse,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap()
    .0
    .stop
    else {
        panic!("s10 tire le stop");
    };
    assert!((numeric_mean(&dist) - 61.875).abs() < 1e-11);
}

// --- Résolution par profil ---

#[test]
fn le_profil_baseline_reprend_les_valeurs_de_la_classe() {
    let file = frozen_costs();
    let (cost, record) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    let d = file.defaults.major;
    assert_eq!(record.half_spread_bps, d.half_spread_bps);
    assert_eq!(record.market_slippage_bps, d.market_slippage_bps);
    assert_eq!(record.stop_slippage_bps, d.stop_slippage_bps);
    assert_eq!(record.stress_probability, d.stress_probability);
    assert_eq!(record.stress_drop, d.stress_drop);
    assert_eq!(
        (record.spread_source, record.slippage_source),
        (ValueSource::Default, ValueSource::Default)
    );
    assert_eq!(record.entry, "taker");
    assert_eq!(record.maker_fill_probability, None);
    assert_eq!(record.maker_ttl_minutes, None);
    assert_eq!(record.adverse_selection_bps, 0.0);
    assert_eq!(record.measured_p95_half_spread_bps, None);
    assert_eq!(record.decision_tf, Timeframe::D1);
    assert_eq!(cost.half_spread_rate, d.half_spread_bps / 10_000.0);
    assert_eq!(cost.market_slippage_rate, d.market_slippage_bps / 10_000.0);
    assert_eq!(cost.entry, EntryMode::Taker);
    assert_eq!(cost.adverse_selection_rate, 0.0);
    assert_eq!(
        cost.taker_rate(),
        cost.half_spread_rate + cost.market_slippage_rate
    );
    assert_eq!(cost.maker_entry_rate(), None);
    assert!(!cost.is_legacy(), "un coût s10 tire le stop");
    let StopCost::Drawn {
        stress_probability,
        stress_drop,
        ..
    } = cost.stop
    else {
        panic!("s10 tire le stop");
    };
    assert_eq!(
        (stress_probability, stress_drop),
        (d.stress_probability, d.stress_drop)
    );
}

#[test]
fn le_profil_adverse_applique_les_multiplicateurs() {
    let file = frozen_costs();
    let m = file.profiles.adverse;
    let d = file.defaults.major;
    let (cost, record) = resolve(
        &file,
        None,
        CostProfileName::Adverse,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(
        record.half_spread_bps,
        d.half_spread_bps * m.half_spread_mult
    );
    assert_eq!(
        record.market_slippage_bps,
        d.market_slippage_bps * m.market_slippage_mult
    );
    let q = record.stop_slippage_bps;
    assert_eq!(q.p50, d.stop_slippage_bps.p50 * m.stop_slippage_mult);
    assert_eq!(q.p95, d.stop_slippage_bps.p95 * m.stop_slippage_mult);
    assert_eq!(q.p99, d.stop_slippage_bps.p99 * m.stop_slippage_mult);
    assert_eq!(q.p100, d.stop_slippage_bps.p100 * m.stop_slippage_mult);
    assert_eq!(
        record.stress_probability,
        d.stress_probability * m.stress_probability_mult
    );
    assert_eq!(
        record.stress_drop, d.stress_drop,
        "la chute de stress ne change pas"
    );
    assert_eq!(record.entry, "taker");
    assert_eq!(record.adverse_selection_bps, 0.0);
    assert_eq!(cost.entry, EntryMode::Taker);
}

#[test]
fn le_profil_maker_garde_les_couts_de_la_baseline_et_change_l_entree() {
    let file = frozen_costs();
    let m = file.profiles.maker;
    let (_, baseline) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::H4,
    )
    .unwrap();
    let (cost, maker) = resolve(
        &file,
        None,
        CostProfileName::Maker,
        "BTC-USD",
        Timeframe::H4,
    )
    .unwrap();
    assert_eq!(maker.half_spread_bps, baseline.half_spread_bps);
    assert_eq!(maker.market_slippage_bps, baseline.market_slippage_bps);
    assert_eq!(maker.stop_slippage_bps, baseline.stop_slippage_bps);
    assert_eq!(maker.stress_probability, baseline.stress_probability);
    assert_eq!(maker.entry, "maker");
    assert_eq!(maker.maker_fill_probability, Some(m.fill_probability));
    assert_eq!(maker.maker_ttl_minutes, Some(60));
    assert_eq!(maker.adverse_selection_bps, m.adverse_selection_bps);
    assert_eq!(maker.entry_offset_bps, Some(m.entry_offset_bps));
    assert_eq!(
        cost.adverse_selection_rate,
        m.adverse_selection_bps / 10_000.0
    );
    assert_eq!(
        cost.entry,
        EntryMode::Maker {
            ttl_minutes: 60,
            fill_probability: m.fill_probability,
            offset_rate: m.entry_offset_bps / 10_000.0,
        }
    );
    // Décalage nul : la limit est placée au demi-spread seul.
    assert_eq!(cost.maker_entry_rate(), Some(cost.half_spread_rate));
}

#[test]
fn le_ttl_maker_vient_du_tf_de_decision() {
    let file = frozen_costs();
    for (tf, minutes) in [
        (Timeframe::M1, 1),
        (Timeframe::M15, 15),
        (Timeframe::H1, 60),
        (Timeframe::H4, 60),
        (Timeframe::H12, 60),
        (Timeframe::D1, 1_440),
    ] {
        let (cost, record) = resolve(&file, None, CostProfileName::Maker, "BTC-USD", tf).unwrap();
        let EntryMode::Maker { ttl_minutes, .. } = cost.entry else {
            panic!("maker attendu");
        };
        assert_eq!(ttl_minutes, minutes, "{tf:?}");
        assert_eq!(record.maker_ttl_minutes, Some(minutes));
        assert_eq!(record.decision_tf, tf);
    }
    // Les deux TTL se lisent dans le fichier.
    let other = costs_with(&[
        ("ttl_minutes_1d = 1440", "ttl_minutes_1d = 720"),
        (
            "ttl_minutes_max_intraday = 60",
            "ttl_minutes_max_intraday = 30",
        ),
    ]);
    let ttl = |tf| match resolve(&other, None, CostProfileName::Maker, "BTC-USD", tf)
        .unwrap()
        .0
        .entry
    {
        EntryMode::Maker { ttl_minutes, .. } => ttl_minutes,
        EntryMode::Taker => panic!("maker attendu"),
    };
    assert_eq!(ttl(Timeframe::D1), 720);
    assert_eq!(ttl(Timeframe::H4), 30);
    assert_eq!(ttl(Timeframe::M15), 15);
}

#[test]
fn le_decalage_maker_est_resolu_dans_le_mode_d_entree() {
    let file = costs_with(&[("entry_offset_bps = 0.0", "entry_offset_bps = 2.5")]);
    let (cost, record) = resolve(
        &file,
        None,
        CostProfileName::Maker,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    let EntryMode::Maker { offset_rate, .. } = cost.entry else {
        panic!("maker attendu");
    };
    assert_eq!(offset_rate, 2.5 / 10_000.0);
    assert_eq!(record.entry_offset_bps, Some(2.5));
    assert_eq!(
        cost.maker_entry_rate(),
        Some(cost.half_spread_rate + offset_rate)
    );
    // Hors du profil maker, aucun décalage.
    let (_, baseline) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(baseline.entry_offset_bps, None);
}

#[test]
fn la_cle_et_la_graine_viennent_du_symbole_et_de_l_appelant() {
    let file = frozen_costs();
    let (cost, _) = resolve_leg(
        &file,
        None,
        CostProfileName::Baseline,
        "ETH-USD",
        Timeframe::D1,
        77,
    )
    .unwrap();
    assert_eq!(cost.leg_key, leg_key("ETH-USD"));
    assert_eq!(cost.seed, 77);
    let (a, _) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "ETH-USD",
        Timeframe::D1,
    )
    .unwrap();
    let (b, _) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "ETH-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(a, b, "fonction pure");
}

#[test]
fn la_graine_au_dela_de_2_puissance_53_est_refusee() {
    let file = frozen_costs();
    let limite = 1_u64 << 53;
    assert!(
        resolve_leg(
            &file,
            None,
            CostProfileName::Baseline,
            "BTC-USD",
            Timeframe::D1,
            limite - 1
        )
        .is_ok()
    );
    let error = resolve_leg(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
        limite,
    )
    .unwrap_err();
    assert_eq!(invalid_field(error), "seed");
}

#[test]
fn un_symbole_sans_tiret_est_refuse_a_la_resolution() {
    let error = resolve(
        &frozen_costs(),
        None,
        CostProfileName::Baseline,
        "BTCUSD",
        Timeframe::D1,
    )
    .unwrap_err();
    assert!(matches!(error, ModelError::CostsSymbol { .. }));
}

// --- Bornes sur les valeurs résolues ---

#[test]
fn un_taux_total_de_cent_pour_cent_est_refuse_en_baseline() {
    let file = costs_with(&[("market_slippage_bps = 1.0", "market_slippage_bps = 9999.9")]);
    let error = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap_err();
    assert_eq!(
        invalid_field(error),
        "half_spread_bps + market_slippage_bps"
    );
}

#[test]
fn un_cas_adverse_qui_ne_franchit_la_borne_qu_apres_multiplication() {
    // Baseline : 0,5 + 3 400 = 3 400,5 bp ; adverse : 0,5 × 2 + 3 400 × 3 = 10 201 bp.
    let file = costs_with(&[("market_slippage_bps = 1.0", "market_slippage_bps = 3400.0")]);
    assert!(
        resolve(
            &file,
            None,
            CostProfileName::Baseline,
            "BTC-USD",
            Timeframe::D1
        )
        .is_ok()
    );
    assert!(
        resolve(
            &file,
            None,
            CostProfileName::Maker,
            "BTC-USD",
            Timeframe::D1
        )
        .is_ok()
    );
    let error = resolve(
        &file,
        None,
        CostProfileName::Adverse,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap_err();
    assert_eq!(
        invalid_field(error),
        "half_spread_bps + market_slippage_bps"
    );
    let message = resolve(
        &file,
        None,
        CostProfileName::Adverse,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap_err()
    .to_string();
    assert!(
        message.contains("BTC-USD") && message.contains("adverse"),
        "{message}"
    );
}

#[test]
fn un_cas_de_stop_qui_ne_franchit_la_borne_qu_apres_multiplication() {
    // p100 de 3 400 bp : sous 10 000 en baseline, 10 200 + 1 en adverse.
    let file = costs_with(&[("p99 = 200.0, p100 = 200.0", "p99 = 200.0, p100 = 3400.0")]);
    assert!(
        resolve(
            &file,
            None,
            CostProfileName::Baseline,
            "BTC-USD",
            Timeframe::D1
        )
        .is_ok()
    );
    let error = resolve(
        &file,
        None,
        CostProfileName::Adverse,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap_err();
    assert_eq!(
        invalid_field(error),
        "half_spread_bps + stop_slippage_bps.p100"
    );
}

#[test]
fn un_cas_qui_ne_franchit_la_borne_qu_avec_une_mesure() {
    let file = frozen_costs();
    // Le défaut passe ; la mesure (valide en soi, sous 10 000 bp) pousse le taux total au-delà.
    let slippage = CostMeasures::from_json_str(&measures_json(
        r#"{"inst_id":"BTC-USDC","hours":24.0,"market_slippage_bps":{"median":9999.9}}"#,
    ))
    .unwrap();
    assert!(
        resolve(
            &file,
            None,
            CostProfileName::Baseline,
            "BTC-USD",
            Timeframe::D1
        )
        .is_ok()
    );
    let error = resolve(
        &file,
        Some(&slippage),
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap_err();
    assert_eq!(
        invalid_field(error),
        "half_spread_bps + market_slippage_bps"
    );
    let spread = CostMeasures::from_json_str(&measures_json(
        r#"{"inst_id":"BTC-USDC","hours":24.0,"half_spread_bps":{"median":9900.0,"p95":9950.0}}"#,
    ))
    .unwrap();
    let error = resolve(
        &file,
        Some(&spread),
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap_err();
    assert_eq!(
        invalid_field(error),
        "half_spread_bps + stop_slippage_bps.p100"
    );
}

#[test]
fn un_decalage_maker_qui_depasse_cent_pour_cent_avec_le_spread_est_refuse() {
    // Valide dans le fichier (< 10 000 bp), mais 0,5 + 9 999,9 ≥ 10 000 une fois le spread ajouté.
    let file = costs_with(&[("entry_offset_bps = 0.0", "entry_offset_bps = 9999.9")]);
    assert!(
        resolve(
            &file,
            None,
            CostProfileName::Baseline,
            "BTC-USD",
            Timeframe::D1
        )
        .is_ok()
    );
    let error = resolve(
        &file,
        None,
        CostProfileName::Maker,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap_err();
    assert_eq!(invalid_field(error), "half_spread_bps + entry_offset_bps");
}

// --- Fichier de mesures ---

#[test]
fn le_fichier_de_mesures_se_lit_et_se_hache() {
    let text = measures_json(FULL_BASE);
    let m = CostMeasures::from_json_str(&text).unwrap();
    assert_eq!(m.schema, "tv4-cost-measures-v1");
    assert_eq!(m.venue, "okx");
    assert_eq!((m.captured_from_ms, m.captured_to_ms), (1000, 172_801_000));
    assert_eq!(m.bases["BTC"].inst_id, "BTC-USDC");
    assert_eq!(m.file_hash(), blake3_hex(text.as_bytes()));
}

#[test]
fn des_heures_au_dela_de_la_fenetre_de_capture_sont_refusees() {
    // Une seconde de fenêtre pour 30 h déclarées.
    let court =
        measures_json(FULL_BASE).replace("\"captured_to_ms\":172801000", "\"captured_to_ms\":2000");
    let error = CostMeasures::from_json_str(&court).unwrap_err();
    assert!(
        error.to_string().contains("fenêtre de capture"),
        "message en français attendu : {error}"
    );
    assert!(matches!(
        error,
        ModelError::MeasuresInvalid { ref field, .. } if field == "bases.BTC.hours"
    ));
    // 24 h déclarées sur 2 h de capture : le plancher de 04 §3.2 ne se contourne pas par `hours`.
    let deux_heures = measures_json(
        r#"{"inst_id":"BTC-USDC","hours":24.0,"half_spread_bps":{"median":0.8,"p95":4.0}}"#,
    )
    .replace("\"captured_to_ms\":172801000", "\"captured_to_ms\":7201000");
    assert!(CostMeasures::from_json_str(&deux_heures).is_err());
    // Des trous de capture sont permis : moins d'heures que la fenêtre passe, la fenêtre exacte aussi.
    assert!(CostMeasures::from_json_str(&measures_json(FULL_BASE)).is_ok());
    let exacte = measures_json(
        r#"{"inst_id":"BTC-USDC","hours":48.0,"half_spread_bps":{"median":0.8,"p95":4.0}}"#,
    );
    assert!(CostMeasures::from_json_str(&exacte).is_ok());
}

#[test]
fn le_fichier_de_mesures_se_charge_depuis_le_disque() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mesures.json");
    std::fs::write(&path, measures_json(FULL_BASE)).unwrap();
    assert!(CostMeasures::load(&path).is_ok());
    assert!(matches!(
        CostMeasures::load(&dir.path().join("absent.json")),
        Err(ModelError::ConfigRead { .. })
    ));
}

fn measures_error(text: &str) -> String {
    match CostMeasures::from_json_str(text) {
        Err(ModelError::MeasuresInvalid { field, .. }) => field,
        Err(autre) => format!("autre erreur : {autre}"),
        Ok(_) => "accepté".to_string(),
    }
}

#[test]
fn les_mesures_invalides_sont_refusees() {
    // Moins de 24 h de capture.
    assert_eq!(
        measures_error(&measures_json(
            r#"{"inst_id":"x","hours":23.9,"half_spread_bps":{"median":1.0,"p95":2.0}}"#
        )),
        "bases.BTC.hours"
    );
    // Valeur négative, non finie par construction JSON impossible, hors bornes.
    assert_eq!(
        measures_error(&measures_json(
            r#"{"inst_id":"x","hours":24,"half_spread_bps":{"median":-1.0,"p95":2.0}}"#
        )),
        "bases.BTC.half_spread_bps.median"
    );
    assert_eq!(
        measures_error(&measures_json(
            r#"{"inst_id":"x","hours":24,"half_spread_bps":{"median":1.0,"p95":10000.0}}"#
        )),
        "bases.BTC.half_spread_bps.p95"
    );
    assert_eq!(
        measures_error(&measures_json(
            r#"{"inst_id":"x","hours":24,"market_slippage_bps":{"median":10000.0}}"#
        )),
        "bases.BTC.market_slippage_bps.median"
    );
    // Schéma et fenêtre de capture.
    assert_eq!(
        measures_error(&measures_json(FULL_BASE).replace("tv4-cost-measures-v1", "autre")),
        "schema"
    );
    assert_eq!(
        measures_error(
            &measures_json(FULL_BASE)
                .replace("\"captured_to_ms\":172801000", "\"captured_to_ms\":1000")
        ),
        "captured_from_ms"
    );
    // Clé inconnue, à la racine comme dans un bloc.
    assert!(
        measures_error(&measures_json(FULL_BASE).replace("\"venue\"", "\"bogus\":1,\"venue\""))
            .starts_with("autre erreur")
    );
    assert!(
        measures_error(&measures_json(r#"{"inst_id":"x","hours":24,"bogus":1}"#))
            .starts_with("autre erreur")
    );
}

#[test]
fn les_zeros_de_forme_ne_passent_pas_la_validation() {
    // La forme de §4.2 est figée avec des zéros comme emplacements : `hours = 0` est refusé.
    let text = measures_json(
        r#"{"inst_id":"BTC-USDC","hours":0.0,"half_spread_bps":{"median":0.0,"p95":0.0},"market_slippage_bps":{"median":0.0}}"#,
    );
    assert_eq!(measures_error(&text), "bases.BTC.hours");
}

#[test]
fn une_base_mesuree_remplace_le_defaut_et_porte_la_source_measured() {
    let file = frozen_costs();
    let m = CostMeasures::from_json_str(&measures_json(FULL_BASE)).unwrap();
    let (cost, record) = resolve(
        &file,
        Some(&m),
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(record.half_spread_bps, 0.8);
    assert_eq!(record.market_slippage_bps, 1.5);
    assert_eq!(
        (record.spread_source, record.slippage_source),
        (ValueSource::Measured, ValueSource::Measured)
    );
    assert_eq!(cost.half_spread_rate, 0.8 / 10_000.0);
    // La loi du stop n'est pas mesurable : celle de la classe.
    assert_eq!(
        record.stop_slippage_bps,
        file.defaults.major.stop_slippage_bps
    );
    // Le p95 est lu et gardé, pas appliqué.
    assert_eq!(record.measured_p95_half_spread_bps, Some(4.0));
    assert_ne!(record.half_spread_bps, 4.0);
}

#[test]
fn une_base_absente_garde_le_defaut() {
    let file = frozen_costs();
    let m = CostMeasures::from_json_str(&measures_json(FULL_BASE)).unwrap();
    let (_, eth) = resolve(
        &file,
        Some(&m),
        CostProfileName::Baseline,
        "ETH-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(eth.half_spread_bps, file.defaults.major.half_spread_bps);
    assert_eq!(
        (eth.spread_source, eth.slippage_source),
        (ValueSource::Default, ValueSource::Default)
    );
    assert_eq!(eth.measured_p95_half_spread_bps, None);
}

#[test]
fn un_bloc_manquant_garde_le_defaut_de_la_classe() {
    let file = frozen_costs();
    let spread_seul = CostMeasures::from_json_str(&measures_json(
        r#"{"inst_id":"BTC-USDC","hours":30,"half_spread_bps":{"median":0.8,"p95":4.0}}"#,
    ))
    .unwrap();
    let (_, record) = resolve(
        &file,
        Some(&spread_seul),
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(
        (record.spread_source, record.slippage_source),
        (ValueSource::Measured, ValueSource::Default)
    );
    assert_eq!(
        record.market_slippage_bps,
        file.defaults.major.market_slippage_bps
    );
    let slippage_seul = CostMeasures::from_json_str(&measures_json(
        r#"{"inst_id":"BTC-USDC","hours":30,"market_slippage_bps":{"median":1.5}}"#,
    ))
    .unwrap();
    let (_, record) = resolve(
        &file,
        Some(&slippage_seul),
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(
        (record.spread_source, record.slippage_source),
        (ValueSource::Default, ValueSource::Measured)
    );
    assert_eq!(record.half_spread_bps, file.defaults.major.half_spread_bps);
    assert_eq!(record.measured_p95_half_spread_bps, None);
}

#[test]
fn une_mesure_est_multipliee_en_adverse() {
    let file = frozen_costs();
    let m = CostMeasures::from_json_str(&measures_json(FULL_BASE)).unwrap();
    let (_, adverse) = resolve(
        &file,
        Some(&m),
        CostProfileName::Adverse,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_eq!(
        adverse.half_spread_bps,
        0.8 * file.profiles.adverse.half_spread_mult
    );
    assert_eq!(
        adverse.market_slippage_bps,
        1.5 * file.profiles.adverse.market_slippage_mult
    );
}

// --- Chemin hérité ---

#[test]
fn le_cout_herite_reprend_l_expression_de_s9() {
    let slippage = Slippage::from_bps(5.0).unwrap();
    let cost = LegCost::legacy(slippage, 20.0);
    assert_eq!(cost.half_spread_rate, 0.0);
    assert_eq!(cost.market_slippage_rate, slippage.rate());
    assert_eq!(
        cost.stop,
        StopCost::Fixed {
            rate: (5.0 + 20.0) / 10_000.0
        }
    );
    assert_eq!(cost.entry, EntryMode::Taker);
    assert_eq!(cost.adverse_selection_rate, 0.0);
    assert!(cost.is_legacy());
    // `0.0 + s = s` exactement, jusqu'au signe d'un zéro.
    assert_eq!(cost.taker_rate(), slippage.rate());
    let zero = LegCost::legacy(Slippage::ZERO, 0.0);
    assert!(zero.is_legacy());
    assert_eq!(zero.taker_rate().to_bits(), 0.0_f64.to_bits());
}

// --- Blocs hachés ---

#[test]
fn les_blocs_haches_ont_la_forme_de_la_spec() {
    let file = frozen_costs();
    let (_, baseline) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::M15,
    )
    .unwrap();
    let leg = serde_json::to_value(baseline.hashed()).unwrap();
    let mut keys: Vec<&str> = leg
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "adverse_selection_bps",
            "class",
            "half_spread_bps",
            "market_slippage_bps",
            "stop_slippage_bps",
            "stress_drop",
            "stress_probability",
        ]
    );
    assert_eq!(leg["class"], "major");
    let stop = leg["stop_slippage_bps"].as_object().unwrap();
    assert_eq!(stop.len(), 4);
    assert_eq!(
        serde_json::to_value(baseline.entry_hashed()).unwrap(),
        serde_json::json!({"decision_tf": "15m", "mode": "taker"})
    );

    let (_, maker) = resolve(
        &file,
        None,
        CostProfileName::Maker,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    let m = file.profiles.maker;
    assert_eq!(
        serde_json::to_value(maker.entry_hashed()).unwrap(),
        serde_json::json!({
            "decision_tf": "1d",
            "entry_offset_bps": m.entry_offset_bps,
            "fill_probability": m.fill_probability,
            "mode": "maker",
            "ttl_minutes": 1440,
        })
    );
    assert_eq!(
        serde_json::to_value(maker.hashed()).unwrap()["adverse_selection_bps"],
        m.adverse_selection_bps
    );
}

#[test]
fn le_bloc_hache_ignore_les_sources_et_le_p95() {
    let file = frozen_costs();
    let m = CostMeasures::from_json_str(&measures_json(
        r#"{"inst_id":"BTC-USDC","hours":30,"half_spread_bps":{"median":0.5,"p95":9.0}}"#,
    ))
    .unwrap();
    // Même valeur, l'une mesurée et l'autre par défaut : mêmes trades, donc même bloc haché.
    let (_, measured) = resolve(
        &file,
        Some(&m),
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    let (_, default_leg) = resolve(
        &file,
        None,
        CostProfileName::Baseline,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    assert_ne!(measured.spread_source, default_leg.spread_source);
    assert_eq!(measured.hashed(), default_leg.hashed());
}

#[test]
fn le_record_se_serialise_sans_le_tf_ni_le_decalage() {
    let file = frozen_costs();
    let (_, record) = resolve(
        &file,
        None,
        CostProfileName::Maker,
        "BTC-USD",
        Timeframe::D1,
    )
    .unwrap();
    let json = serde_json::to_value(&record).unwrap();
    let object = json.as_object().unwrap();
    assert!(!object.contains_key("decision_tf") && !object.contains_key("entry_offset_bps"));
    assert_eq!(json["symbol"], "BTC-USD");
    assert_eq!(json["class"], "major");
    assert_eq!(json["class_source"], "list");
    assert_eq!(json["spread_source"], "default");
    assert_eq!(json["entry"], "maker");
    assert!(json["measured_p95_half_spread_bps"].is_null());
}

#[test]
fn les_noms_sont_ceux_de_la_serialisation() {
    for profile in CostProfileName::ALL {
        assert_eq!(serde_json::to_value(profile).unwrap(), profile.as_str());
        assert_eq!(profile.to_string(), profile.as_str());
    }
    assert_eq!(
        CostProfileName::ALL,
        [
            CostProfileName::Baseline,
            CostProfileName::Adverse,
            CostProfileName::Maker
        ]
    );
    assert!(CostProfileName::Baseline.gates() && CostProfileName::Adverse.gates());
    assert!(!CostProfileName::Maker.gates());
    for class in [PairClass::Major, PairClass::Top20, PairClass::Beyond] {
        assert_eq!(serde_json::to_value(class).unwrap(), class.as_str());
        assert_eq!(class.to_string(), class.as_str());
    }
}
