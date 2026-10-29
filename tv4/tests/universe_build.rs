//! Tests du lot C de S11 (docs/specs/s11.md §4.1 à §4.4 et §4.7, §4.6) : éligibilité, classement et
//! sélection de l'univers trimestriel, exclusion qualité, hashes d'entrée, règles d'instrument dérivées.
//!
//! **Toutes les séries sont synthétiques** (instants, prix et volumes inventés, générés ici) et ne
//! prétendent à aucune valeur de marché ; seule exception, la fixture `fixtures/kraken_xbtusd_1m.csv`,
//! extrait réel du dépôt, lue pour recouper le tick et le lot de `config/instruments.toml`. Les noms de
//! paires (`EEEUSD`, `AAAUSD`...) sont fictifs. Les tests qui écrivent un dossier d'univers passent par
//! l'écriture vérifiée de `store::universe`.

mod common;

use std::path::PathBuf;

use common::SplitMix64;
use tv4::engine::{DEFAULT_QUOTE_GAP_MS, RankTable};
use tv4::marketdata::continuity::check_m1;
use tv4::marketdata::kraken::{KrakenCsvReader, KrakenError, RawRow};
use tv4::model::universe::UniverseConfig;
use tv4::model::{Candle, ExchangeId, InstrumentRules, Timeframe, Ts};
use tv4::research::universe::{
    self, Candidate, Eligibility, EligibleMetrics, Ineligible, PairEntry, PairScan, PairStats,
    QUALITY_TFS, QuarterSelection, TOP_N, UniverseError, candidates_from_entries, covered_quarters,
    derive_instrument_rule, input_hash, is_legacy_pair_name, quality_hash, quarter_label_of,
    scan_checked, select, select_at, tick_decimals, union_entries, universe_rules,
};
use tv4::store::time::{MS_PER_DAY, days_from_civil, parse_coverage_end};
use tv4::store::universe::{
    QuarterBuild, UniverseBuild, UniverseSource, load, render_instruments_toml, write_universe,
};
use tv4::store::{
    BuildStatus, Columns, GapsPolicy, MultiBuildRequest, Partition, StoreError, Symbol, Window,
    build_dataset_multi, write_partition,
};

const MIN: Ts = 60_000;
const DAY: Ts = MS_PER_DAY;
const SIX_H: Ts = 6 * 3_600_000;
const T: Ts = DEFAULT_QUOTE_GAP_MS;

fn ms(y: i64, m: u32, d: u32) -> Ts {
    days_from_civil(y, m, d) * MS_PER_DAY
}

fn config() -> UniverseConfig {
    UniverseConfig::load(&PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../config/universe.toml"
    )))
    .unwrap()
}

fn candle(t: Ts, close: f64, volume: f64) -> Candle {
    Candle {
        open_time_ms: t,
        open: close,
        high: close,
        low: close,
        close,
        volume,
        trades: Some(1),
    }
}

/// Instants `start, start + step, ...` strictement avant `end`.
fn regular(start: Ts, end: Ts, step: Ts) -> Vec<Ts> {
    (0..)
        .map(|i| start + i * step)
        .take_while(|&t| t < end)
        .collect()
}

fn series(times: &[Ts], close: f64, volume: f64) -> Vec<Candle> {
    times.iter().map(|&t| candle(t, close, volume)).collect()
}

fn stats_of(times: &[Ts]) -> PairStats {
    PairStats::from_m1(&series(times, 10.0, 1.0)).unwrap()
}

fn candidate(pair: &str) -> Candidate {
    let candidates = candidates_from_entries(&[format!("{pair}_1.csv")], &config())
        .unwrap()
        .candidates;
    candidates.into_iter().next().unwrap()
}

fn entry(pair: &str, candles: &[Candle]) -> PairEntry {
    PairEntry {
        candidate: candidate(pair),
        stats: PairStats::from_m1(candles).unwrap(),
    }
}

fn eligibility_of(times: &[Ts], date: Ts) -> Eligibility {
    stats_of(times).eligibility(date).unwrap()
}

fn is_eligible(times: &[Ts], date: Ts) -> bool {
    matches!(eligibility_of(times, date), Eligibility::Eligible(_))
}

const D: Ts = 1_577_836_800_000; // 2020-01-01T00:00:00Z

fn ws() -> Ts {
    D - 365 * DAY
}

// ---------------------------------------------------------------------------------------------
// Paires candidates
// ---------------------------------------------------------------------------------------------

#[test]
fn les_dates_de_reference_sont_recalculees() {
    assert_eq!(D, ms(2020, 1, 1));
}

#[test]
fn candidates_normalisees_triees_et_sans_piege() {
    let noms: Vec<String> = [
        "XBTUSD_1.csv",
        "XBTUSD_5.csv",
        "AIXBTUSD_1.csv",
        "ETHUSDT_1.csv",
        "ETHUSD_1.csv",
        "XDGUSD_1.csv",
        "XBTEUR_1.csv",
        "XBTETH_1.csv",
        "USDTUSD_1.csv",
        "EURUSD_1.csv",
        "XXBTZUSD_1.csv",
        "XXBTZUSD_5.csv",
        "XETHZUSD_1.csv",
        "MANIFEST.json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let ensemble = candidates_from_entries(&noms, &config()).unwrap();
    let paires: Vec<(&str, &str)> = ensemble
        .candidates
        .iter()
        .map(|c| (c.kraken_pair.as_str(), c.symbol.as_str()))
        .collect();
    assert_eq!(
        paires,
        [
            ("AIXBTUSD", "AIXBT-USD"),
            ("ETHUSD", "ETH-USD"),
            ("ETHUSDT", "ETH-USDT"),
            ("XBTUSD", "BTC-USD"),
            ("XDGUSD", "DOGE-USD"),
        ]
    );
    // Les bases fiat et stablecoins, les cotations hors liste et la forme héritée sont écartées.
    assert_eq!(ensemble.unrecognized_pairs, ["XETHZUSD", "XXBTZUSD"]);
    // 11 entrées `_1.csv` distinctes (XBTUSD_5, XXBTZUSD_5 et MANIFEST ne comptent pas).
    assert_eq!(ensemble.pairs_in_zip, 11);
    // L'ordre des entrées du zip est sans effet.
    let mut inverse = noms.clone();
    inverse.reverse();
    assert_eq!(
        candidates_from_entries(&inverse, &config()).unwrap(),
        ensemble
    );
    let base = &ensemble.candidates[3];
    assert_eq!((base.base.as_str(), base.quote.as_str()), ("BTC", "USD"));
}

#[test]
fn collision_de_symbole_refusee() {
    let noms: Vec<String> = ["XBTUSD_1.csv", "BTCUSD_1.csv", "ETHUSD_1.csv"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match candidates_from_entries(&noms, &config()) {
        Err(UniverseError::SymbolCollision { symbol, pairs }) => {
            assert_eq!(symbol, "BTC-USD");
            assert_eq!(pairs, "BTCUSD, XBTUSD");
        }
        autre => panic!("SymbolCollision attendue : {autre:?}"),
    }
}

#[test]
fn forme_heritee_reconnue_sans_deviner() {
    for ok in ["XXBTZUSD", "XETHZUSD", "XXBTZUSDT", "ZXXXXUSD"] {
        assert!(is_legacy_pair_name(ok), "{ok}");
    }
    for ko in [
        "XBTUSD",
        "ZECUSD",
        "XRPUSDT",
        "XXBTZEUR",
        "XXBTUSD",
        "AIXBTUSD",
        "XXBTZUSDC",
        "",
    ] {
        assert!(!is_legacy_pair_name(ko), "{ko}");
    }
}

#[test]
fn trimestres_couverts_et_libelles() {
    assert_eq!(quarter_label_of(ms(2018, 1, 1)), "2018-Q1");
    assert_eq!(quarter_label_of(ms(2018, 3, 31) + DAY - 1), "2018-Q1");
    assert_eq!(quarter_label_of(ms(2018, 4, 1)), "2018-Q2");
    assert_eq!(quarter_label_of(ms(2026, 12, 31)), "2026-Q4");
    // Couverture annoncée « 2026-09-30 » (date seule, inclusive) : 2018-Q1 à 2026-Q3.
    let fin = parse_coverage_end("2026-09-30").unwrap();
    let trimestres = covered_quarters("2018-Q1", fin).unwrap();
    assert_eq!(trimestres.len(), 35);
    assert_eq!(trimestres.first().map(String::as_str), Some("2018-Q1"));
    assert_eq!(trimestres.last().map(String::as_str), Some("2026-Q3"));
    // Un trimestre dont la fin dépasse la couverture n'est pas produit.
    let fin = parse_coverage_end("2026-09-29").unwrap();
    assert_eq!(
        covered_quarters("2018-Q1", fin)
            .unwrap()
            .last()
            .map(String::as_str),
        Some("2026-Q2")
    );
    assert!(covered_quarters("2030-Q1", fin).unwrap().is_empty());
    assert!(covered_quarters("2018-T1", fin).is_err());
}

// ---------------------------------------------------------------------------------------------
// Règle d'éligibilité (§4.2)
// ---------------------------------------------------------------------------------------------

#[test]
fn historique_exactement_365_jours() {
    let fin = D + DAY;
    assert!(is_eligible(&regular(ws(), fin, SIX_H), D));
    // Une minute plus tard : moins de 365 jours d'historique.
    assert_eq!(
        eligibility_of(&regular(ws() + MIN, fin, SIX_H), D),
        Eligibility::Ineligible(Ineligible::History)
    );
}

#[test]
fn continuite_trou_interieur() {
    let x = ws() + 30 * DAY;
    let fin = D + DAY;
    // Trou de T − 60 s : la minute active suivante est à X + T.
    let mut sous = regular(ws() - 2 * DAY, fin, SIX_H);
    sous.retain(|&t| t <= x || t >= x + T);
    assert!(is_eligible(&sous, D));
    // Trou de T exactement : la suivante est à X + T + 60 s.
    let mut egal = regular(ws() - 2 * DAY, fin, SIX_H);
    egal.retain(|&t| t <= x || t > x + T);
    egal.push(x + T + MIN);
    egal.sort_unstable();
    assert_eq!(
        eligibility_of(&egal, D),
        Eligibility::Ineligible(Ineligible::QuoteGap)
    );
}

#[test]
fn continuite_trou_initial_et_final() {
    let fin = D + DAY;
    // Initial : la dernière minute avant la fenêtre est à ws − 6 h, la suivante à ws + T − 60 s (trou
    // coupé à ws : T − 60 s) ou ws + T (T).
    for (suivante, attendu) in [(ws() + T - MIN, true), (ws() + T, false)] {
        let mut times = vec![ws() - SIX_H];
        times.extend(regular(suivante, fin, SIX_H));
        assert_eq!(
            is_eligible(&times, D),
            attendu,
            "suivante = ws + {}",
            suivante - ws()
        );
    }
    // Final : la dernière minute avant D est à D − T (trou final T − 60 s) ou D − T − 60 s (T).
    for (derniere, attendu) in [(D - T, true), (D - T - MIN, false)] {
        let mut times = regular(ws(), derniere, SIX_H);
        times.push(derniere);
        assert_eq!(
            is_eligible(&times, D),
            attendu,
            "dernière = D − {}",
            D - derniere
        );
    }
}

#[test]
fn un_trou_anterieur_a_la_fenetre_n_empeche_rien() {
    // Actives de ws − 400 j à ws − 390 j, puis rien pendant 380 jours (le trou se termine avant ws),
    // puis une minute toutes les 6 heures jusqu'à D : EOS, GNO, XLM, REP « reviennent 365 jours après
    // leur retour » (04 §2.2).
    let mut times = regular(ws() - 400 * DAY, ws() - 390 * DAY, SIX_H);
    times.extend(regular(ws() - 10 * DAY, D + DAY, SIX_H));
    match eligibility_of(&times, D) {
        Eligibility::Eligible(m) => assert_eq!(m.first_active_ms, ws() - 400 * DAY),
        autre => panic!("éligible attendue : {autre:?}"),
    }
}

#[test]
fn fenetre_sans_minute_active_n_est_pas_continue() {
    let times = regular(ws() - 400 * DAY, ws() - DAY, SIX_H);
    assert_eq!(
        eligibility_of(&times, D),
        Eligibility::Ineligible(Ineligible::QuoteGap)
    );
    // Et une paire qui ne cote qu'à partir de D n'a pas d'historique.
    let times = regular(D, D + 10 * DAY, SIX_H);
    assert_eq!(
        eligibility_of(&times, D),
        Eligibility::Ineligible(Ineligible::History)
    );
}

#[test]
fn les_minutes_a_volume_nul_ne_sont_pas_actives() {
    // Toutes les 6 h une minute active, mais un trou de cotation de 8 jours rempli de minutes réelles à
    // volume nul : les minutes réelles ne comblent pas le trou (S9 §4.3).
    let x = ws() + 30 * DAY;
    let mut candles: Vec<Candle> = regular(ws(), D + DAY, SIX_H)
        .into_iter()
        .filter(|&t| t <= x || t >= x + 8 * DAY)
        .map(|t| candle(t, 10.0, 1.0))
        .collect();
    candles.extend((1..8 * 24).map(|h| candle(x + h * 3_600_000, 10.0, 0.0)));
    candles.sort_by_key(|c| c.open_time_ms);
    let stats = PairStats::from_m1(&candles).unwrap();
    assert_eq!(
        stats.eligibility(D).unwrap(),
        Eligibility::Ineligible(Ineligible::QuoteGap)
    );
}

#[test]
fn prix_dernier_close_avant_d() {
    let fin = D + DAY;
    let mut times = regular(ws(), D, SIX_H);
    times.push(D - MIN);
    times.push(D); // minute à D : n'est pas lue.
    times.push(fin);
    for (avant, a_d, attendu) in [
        (0.01, 0.5, false),
        (0.0101, 0.005, true),
        (0.5, 0.005, true),
    ] {
        let candles: Vec<Candle> = times
            .iter()
            .map(|&t| {
                let close = if t == D - MIN {
                    avant
                } else if t == D {
                    a_d
                } else {
                    10.0
                };
                candle(t, close, 1.0)
            })
            .collect();
        let stats = PairStats::from_m1(&candles).unwrap();
        assert_eq!(stats.last_close_before(D), Some(avant));
        let verdict = stats.eligibility(D).unwrap();
        match (attendu, verdict) {
            (true, Eligibility::Eligible(m)) => assert_eq!(m.last_close, avant),
            (false, Eligibility::Ineligible(Ineligible::Price)) => {}
            (_, autre) => panic!("verdict inattendu pour {avant} : {autre:?}"),
        }
    }
}

#[test]
fn la_date_doit_etre_un_minuit() {
    let stats = stats_of(&regular(ws(), D + DAY, SIX_H));
    assert!(matches!(
        stats.eligibility(D + MIN),
        Err(UniverseError::DateNotMidnight { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// Volume quote 90 j : au bit près le score du rang de liquidité de S9
// ---------------------------------------------------------------------------------------------

/// Série de 200 jours avant `D` et 5 jours après, des minutes tirées par jour, avec des jours sans aucune
/// minute et des volumes nuls (valeurs synthétiques).
fn serie_volumes(seed: u64) -> Vec<Candle> {
    let mut rng = SplitMix64(seed);
    let mut candles = Vec::new();
    for jour in -200..5 {
        // Un jour sur sept sans aucune minute.
        if jour % 7 == 3 {
            continue;
        }
        let mut minutes: Vec<i64> = (0..40).map(|_| (rng.next_u64() % 1440) as i64).collect();
        minutes.sort_unstable();
        minutes.dedup();
        for minute in minutes {
            let volume = if rng.next_u64().is_multiple_of(10) {
                0.0
            } else {
                rng.next_f64() * 7.3
            };
            let close = 0.5 + rng.next_f64() * 120.17;
            candles.push(candle(D + jour * DAY + minute * MIN, close, volume));
        }
    }
    candles
}

#[test]
fn volume_90j_egal_au_score_du_rang_de_s9_au_bit() {
    let candles = serie_volumes(0x0DEC_0DED);
    let stats = PairStats::from_m1(&candles).unwrap();
    let table = RankTable::build(&[&candles]);
    for date in [D - 150 * DAY, D - 50 * DAY, D, D + 3 * DAY] {
        assert_eq!(
            stats.volume_quote_90d(date).to_bits(),
            table.score_at(date, 0).to_bits(),
            "date {date}"
        );
    }
    // Un jour sans minute vaut 0 : la fenêtre D − 90 … D − 1 saute les jours multiples de 7 + 3.
    assert!(stats.volume_quote_90d(D) > 0.0);
    let vide = PairStats::from_m1(&series(&[D - 100 * DAY], 5.0, 2.0)).unwrap();
    assert_eq!(vide.volume_quote_90d(D), 0.0);
    assert_eq!(vide.volume_quote_90d(D - 99 * DAY), 10.0);
}

// ---------------------------------------------------------------------------------------------
// Sélection : USD contre USDT, exclusions, top 12, déterminisme
// ---------------------------------------------------------------------------------------------

/// Série éligible à D de volume 90 j paramétré : une minute toutes les 6 h depuis `debut`, volume
/// `volume`, close 10.
fn eligible_series(debut: Ts, volume: f64) -> Vec<Candle> {
    series(&regular(debut, D + 2 * DAY, SIX_H), 10.0, volume)
}

fn symboles(selection: &QuarterSelection) -> Vec<&str> {
    selection
        .eligible
        .iter()
        .map(|s| s.candidate.symbol.as_str())
        .collect()
}

#[test]
fn usd_prefere_a_usdt_jamais_les_deux() {
    let ok = eligible_series(ws(), 1.0);
    let gros = eligible_series(ws(), 100.0);
    let court = eligible_series(ws() + 10 * DAY, 1.0);
    let paires = vec![
        entry("EEEUSD", &ok),
        entry("EEEUSDT", &gros),
        entry("GGGUSD", &court),
        entry("GGGUSDT", &ok),
        entry("FFFUSDT", &ok),
    ];
    let selection = select_at(&paires, D, &config()).unwrap();
    // EEE : l'USD est éligible, l'USDT (plus liquide) est ignorée. GGG : l'USD est inéligible
    // (historique), l'USDT est retenue. FFF : USDT seule.
    let mut vus = symboles(&selection);
    vus.sort_unstable();
    assert_eq!(vus, ["EEE-USD", "FFF-USDT", "GGG-USDT"]);
    assert_eq!(selection.eligible_count(), 3);
    // Aucun cumul de volume entre USD et USDT.
    let eee = selection
        .eligible
        .iter()
        .find(|s| s.candidate.base == "EEE")
        .unwrap();
    assert_eq!(eee.metrics.volume_quote_90d, 90.0 * 4.0 * 10.0);
}

#[test]
fn bases_exclues_et_cotations_hors_liste_ignorees() {
    let ok = eligible_series(ws(), 1.0);
    let conf = config();
    // Construites à la main pour contourner `candidates_from_entries` : `select` se protège aussi.
    let fiat = PairEntry {
        candidate: Candidate {
            kraken_pair: "USDTUSD".into(),
            symbol: "USDT-USD".into(),
            base: "USDT".into(),
            quote: "USD".into(),
        },
        stats: PairStats::from_m1(&ok).unwrap(),
    };
    let euro = PairEntry {
        candidate: Candidate {
            kraken_pair: "EURUSD".into(),
            symbol: "EUR-USD".into(),
            base: "EUR".into(),
            quote: "USD".into(),
        },
        stats: PairStats::from_m1(&ok).unwrap(),
    };
    let en_eur = PairEntry {
        candidate: Candidate {
            kraken_pair: "AAAEUR".into(),
            symbol: "AAA-EUR".into(),
            base: "AAA".into(),
            quote: "EUR".into(),
        },
        stats: PairStats::from_m1(&ok).unwrap(),
    };
    let bonne = entry("BBBUSD", &ok);
    let selection = select_at(&[fiat, euro, en_eur, bonne], D, &conf).unwrap();
    assert_eq!(symboles(&selection), ["BBB-USD"]);
}

#[test]
fn top_12_sur_14_eligibles() {
    // Quatorze bases, volumes croissants avec l'indice (1, 2, ... 14).
    let noms: Vec<String> = (0..14)
        .map(|i| format!("{}XXUSD", (b'A' + i as u8) as char))
        .collect();
    let paires: Vec<PairEntry> = noms
        .iter()
        .enumerate()
        .map(|(i, nom)| entry(nom, &eligible_series(ws(), 1.0 + i as f64)))
        .collect();
    let selection = select_at(&paires, D, &config()).unwrap();
    assert_eq!(selection.eligible_count(), 14);
    assert_eq!(selection.members().len(), TOP_N);
    // Rang 1 = volume le plus fort (indice 13) ; les deux plus faibles (0 et 1) sortent.
    assert_eq!(selection.eligible[0].candidate.kraken_pair, noms[13]);
    assert_eq!(selection.eligible[0].rank, 1);
    let membres: Vec<&str> = selection
        .members()
        .iter()
        .map(|s| s.candidate.kraken_pair.as_str())
        .collect();
    assert!(!membres.contains(&noms[0].as_str()));
    assert!(!membres.contains(&noms[1].as_str()));
    assert_eq!(selection.members().last().unwrap().rank, 12);
    // Moins de 12 éligibles : tous le sont (la rampe de 5 à 13 de 04 §2.2).
    let peu = select_at(&paires[..5], D, &config()).unwrap();
    assert_eq!(peu.members().len(), 5);
}

#[test]
fn egalite_de_volume_ancien_d_abord_puis_ordre_d_octets() {
    // Même volume 90 j (identique au bit : séries égales sur les 90 derniers jours), anciennetés
    // différentes pour AAA et BBB, égale pour CCC et DDD.
    let plus_ancien = eligible_series(ws() - 5 * DAY, 1.0);
    let ancien = eligible_series(ws() - DAY, 1.0);
    let recent = eligible_series(ws(), 1.0);
    let paires = vec![
        entry("DDDUSD", &recent),
        entry("AAAUSD", &recent),
        entry("CCCUSD", &ancien),
        entry("EEEUSD", &plus_ancien),
        entry("BBBUSD", &ancien),
    ];
    let selection = select_at(&paires, D, &config()).unwrap();
    // Volume égal : le plus ancien d'abord (EEE), puis ceux d'ancienneté égale par nom d'octets.
    assert_eq!(
        symboles(&selection),
        ["EEE-USD", "BBB-USD", "CCC-USD", "AAA-USD", "DDD-USD"]
    );
    // L'ordre d'entrée est sans effet.
    let mut inverse = paires.clone();
    inverse.reverse();
    assert_eq!(select_at(&inverse, D, &config()).unwrap(), selection);
}

#[test]
fn causalite_une_minute_posterieure_a_d_ne_change_rien() {
    let base = eligible_series(ws(), 1.0);
    let variee = eligible_series(ws(), 2.0);
    let avant = vec![entry("AAAUSD", &base), entry("BBBUSD", &variee)];
    let attendu = select_at(&avant, D, &config()).unwrap();
    // On modifie toutes les minutes d'open_time >= D (volume, close) et on en ajoute une loin après.
    let alteree = |candles: &[Candle]| -> Vec<Candle> {
        let mut v: Vec<Candle> = candles
            .iter()
            .map(|c| {
                if c.open_time_ms >= D {
                    candle(c.open_time_ms, 0.001, 9_999.0)
                } else {
                    *c
                }
            })
            .collect();
        v.push(candle(D + 50 * DAY, 3.0, 1.0));
        v
    };
    let apres = vec![
        entry("AAAUSD", &alteree(&base)),
        entry("BBBUSD", &alteree(&variee)),
    ];
    assert_eq!(select_at(&apres, D, &config()).unwrap(), attendu);
    // Modifier une minute antérieure change au contraire les valeurs.
    let mut anterieure = base.clone();
    // Indice 1400 : 350 jours après ws, dans la fenêtre de volume (D − 90 j, D).
    anterieure[1400].volume = 50.0;
    let touche = vec![entry("AAAUSD", &anterieure), entry("BBBUSD", &variee)];
    assert_ne!(select_at(&touche, D, &config()).unwrap(), attendu);
}

#[test]
fn select_traite_chaque_date() {
    let paires = vec![entry("AAAUSD", &eligible_series(ws(), 1.0))];
    // À D − 100 j, la paire n'a pas encore 365 jours d'historique ; à D elle est membre.
    let dates = [D - 100 * DAY, D];
    let selections = select(&paires, &dates, &config()).unwrap();
    assert_eq!(selections.len(), 2);
    assert!(selections[0].eligible.is_empty());
    assert_eq!(symboles(&selections[1]), ["AAA-USD"]);
    assert_eq!(selections[1].date_ms, D);
}

// ---------------------------------------------------------------------------------------------
// Exclusion qualité (§4.3)
// ---------------------------------------------------------------------------------------------

#[test]
fn qualite_six_seaux_4h_sur_546_excluent_le_4h_pas_le_1d() {
    // Une minute par heure du 2019-10-01 au 2020-04-01, sans toutes les minutes de six seaux 4h du
    // trimestre 2020-Q1 (indices 0, 7, 14, 21, 28, 35 : jours distincts, aucun seau 12h entier retiré).
    let debut = ms(2019, 10, 1);
    let q1 = ms(2020, 1, 1);
    let retires: Vec<Ts> = [0, 7, 14, 21, 28, 35]
        .iter()
        .map(|i| q1 + i * 4 * 3_600_000)
        .collect();
    let times: Vec<Ts> = regular(debut, ms(2020, 4, 1), 3_600_000)
        .into_iter()
        .filter(|&t| !retires.iter().any(|&r| t >= r && t < r + 4 * 3_600_000))
        .collect();
    let stats = stats_of(&times);
    let tfs = stats.excluded_tfs("2020-Q1");
    // 6 seaux sur 546 (1,0989 % > 1 %) en 4h, 24 sur 2184 en 1h ; 5m, 15m, 30m n'ont qu'une minute par
    // heure ; le 12h et le 1d sont complets. Le 1m n'y figure jamais.
    assert_eq!(
        tfs,
        [
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::M30,
            Timeframe::H1,
            Timeframe::H4
        ]
    );
    assert!(!tfs.contains(&Timeframe::D1));
    assert!(!tfs.contains(&Timeframe::M1));
    // Chaque jour garde une minute.
    for (_, tfs) in stats.excluded_quarters() {
        assert!(!tfs.contains(&Timeframe::M1));
    }
    assert!(QUALITY_TFS.iter().all(|tf| *tf != Timeframe::M1));
    // Cinq seaux seulement : 5/546 = 0,92 % < 1 %, le 4h n'est plus exclu.
    let times: Vec<Ts> = regular(debut, ms(2020, 4, 1), 3_600_000)
        .into_iter()
        .filter(|&t| {
            !retires[..5]
                .iter()
                .any(|&r| t >= r && t < r + 4 * 3_600_000)
        })
        .collect();
    assert!(
        !stats_of(&times)
            .excluded_tfs("2020-Q1")
            .contains(&Timeframe::H4)
    );
}

#[test]
fn les_tf_de_qualite_sont_ceux_de_5m_a_1d() {
    let attendus: Vec<Timeframe> = Timeframe::ALL
        .iter()
        .copied()
        .filter(|tf| *tf != Timeframe::M1)
        .collect();
    assert_eq!(QUALITY_TFS.to_vec(), attendus);
    let regles = universe_rules(&config());
    assert_eq!(regles.quality_tfs, attendus);
}

// ---------------------------------------------------------------------------------------------
// Plus long trou : recoupement avec un calcul naïf
// ---------------------------------------------------------------------------------------------

/// Calcul naïf du plus long trou de `[ws, we)` d'après la liste des minutes actives.
fn trou_naif(actives: &[Ts], ws: Ts, we: Ts) -> Option<Ts> {
    let dans: Vec<Ts> = actives
        .iter()
        .copied()
        .filter(|&a| a >= ws && a < we)
        .collect();
    let (premiere, derniere) = (dans.first()?, dans.last()?);
    let mut plus_long = (premiere - ws).max(we - (derniere + MIN));
    for paire in dans.windows(2) {
        plus_long = plus_long.max(paire[1] - (paire[0] + MIN));
    }
    Some(plus_long)
}

#[test]
fn plus_long_trou_egal_au_calcul_naif() {
    let mut rng = SplitMix64(0xFEED);
    // Environ 4 000 minutes réelles sur 9 000, dont 3 000 actives, donc plus de 256 trous : les blocs
    // de maxima sont exercés. Les autres minutes réelles ont un volume nul.
    let mut candles = Vec::new();
    let mut actives = Vec::new();
    for minute in 0..9_000_i64 {
        let u = rng.next_f64();
        if u < 0.33 {
            candles.push(candle(minute * MIN, 5.0, 1.0));
            actives.push(minute * MIN);
        } else if u < 0.45 {
            candles.push(candle(minute * MIN, 5.0, 0.0));
        }
    }
    let stats = PairStats::from_m1(&candles).unwrap();
    assert!(stats.gap_count() > 600, "{}", stats.gap_count());
    for _ in 0..4_000 {
        let a = (rng.next_u64() % 9_500) as i64 * MIN;
        let b = (rng.next_u64() % 9_500) as i64 * MIN;
        let (ws, we) = if a <= b { (a, b + MIN) } else { (b, a + MIN) };
        assert_eq!(
            stats.longest_gap_ms(ws, we),
            trou_naif(&actives, ws, we),
            "fenêtre [{ws}, {we})"
        );
    }
    // Fenêtres dégénérées.
    assert_eq!(stats.longest_gap_ms(MIN, MIN), None);
    assert_eq!(stats.longest_gap_ms(2 * MIN, MIN), None);
    assert_eq!(stats.longest_gap_ms(-5 * DAY, -4 * DAY), None);
    assert_eq!(stats.longest_gap_ms(100 * DAY, 101 * DAY), None);
}

#[test]
fn from_m1_refuse_vide_desordre_et_desalignement() {
    assert!(matches!(
        PairStats::from_m1(&[]),
        Err(UniverseError::EmptySeries)
    ));
    let desordre = [candle(2 * MIN, 1.0, 1.0), candle(MIN, 1.0, 1.0)];
    assert!(matches!(
        PairStats::from_m1(&desordre),
        Err(UniverseError::Store(_))
    ));
    let desalignee = [candle(MIN + 30_000, 1.0, 1.0)];
    assert!(PairStats::from_m1(&desalignee).is_err());
}

// ---------------------------------------------------------------------------------------------
// Passe 1 sur une série contrôlée
// ---------------------------------------------------------------------------------------------

fn lignes_brutes(candles: &[Candle]) -> Vec<Result<RawRow, KrakenError>> {
    candles
        .iter()
        .enumerate()
        .map(|(i, c)| {
            Ok(RawRow {
                line: i as u64 + 1,
                open_time_ms: c.open_time_ms,
                open: c.open,
                high: c.high,
                low: c.low,
                close: c.close,
                volume: c.volume,
                trades: 1,
            })
        })
        .collect()
}

#[test]
fn passe_1_accepte_une_serie_et_rejette_une_serie_bloquante() {
    let bonnes = eligible_series(ws(), 1.0);
    let fin_couverture = D + 10 * DAY;
    let controlee = check_m1(lignes_brutes(&bonnes).into_iter(), fin_couverture);
    match scan_checked(&controlee).unwrap() {
        PairScan::Accepted(stats) => {
            assert_eq!(stats.first_active_ms(), Some(ws()));
            assert!(matches!(
                stats.eligibility(D).unwrap(),
                Eligibility::Eligible(_)
            ));
        }
        PairScan::Rejected { reasons } => panic!("rejetée : {reasons:?}"),
    }
    // OHLC incohérent (low > high) : série bloquante, écartée avec son motif.
    let mut mauvaises = bonnes.clone();
    mauvaises[10].low = mauvaises[10].high + 1.0;
    let controlee = check_m1(lignes_brutes(&mauvaises).into_iter(), fin_couverture);
    match scan_checked(&controlee).unwrap() {
        PairScan::Rejected { reasons } => assert_eq!(reasons, ["ohlc_error"]),
        PairScan::Accepted(_) => panic!("la série bloquante doit être rejetée"),
    }
}

// ---------------------------------------------------------------------------------------------
// Hashes d'entrée : égaux au dataset_hash du store
// ---------------------------------------------------------------------------------------------

#[test]
fn input_hash_et_quality_hash_sont_le_dataset_hash_d_un_dataset_mono_serie_skip() {
    // Série de 2019-12-01 à 2020-04-01 : le segment d'entrée de D = 2020-04-01 est
    // [2019-04-02, 2020-04-01), le segment de qualité de 2020-Q1 est [2020-01-01, 2020-04-01).
    let candles = series(
        &regular(ms(2019, 12, 1), ms(2020, 4, 1), 5 * MIN),
        12.5,
        1.5,
    );
    let racine = tempfile::tempdir().unwrap();
    let symbole: Symbol = "AAA-USD".parse().unwrap();
    let partition = Partition::new(ExchangeId::Kraken, symbole.clone(), Timeframe::M1);
    write_partition(
        racine.path(),
        &partition,
        &Columns::from_candles(&candles),
        "synthetic",
        1_893_456_000_000,
    )
    .unwrap();
    let date = ms(2020, 4, 1);
    for (nom, debut, fin, attendu) in [
        (
            "entree",
            date - 365 * DAY,
            date,
            input_hash("AAA-USD", &candles, date).unwrap(),
        ),
        (
            "qualite",
            ms(2020, 1, 1),
            date,
            quality_hash("AAA-USD", &candles, "2020-Q1").unwrap(),
        ),
    ] {
        let symboles = [symbole.clone()];
        let issue = build_dataset_multi(
            racine.path(),
            &MultiBuildRequest {
                name: nom,
                exchange: ExchangeId::Kraken,
                symbols: &symboles,
                tf: Timeframe::M1,
                window: Window::new(debut, fin).unwrap(),
                gaps_policy: GapsPolicy::Skip,
            },
            1_893_456_000_000,
        )
        .unwrap();
        assert_eq!(issue.status, BuildStatus::Created);
        assert_eq!(issue.manifest.dataset_hash, attendu, "{nom}");
    }
    // Un trimestre sans minute a un hash défini (segment vide) ; le hash dépend du symbole.
    let vide = quality_hash("AAA-USD", &candles, "2021-Q1").unwrap();
    assert!(vide.starts_with("blake3:"));
    assert_ne!(
        input_hash("BBB-USD", &candles, date).unwrap(),
        input_hash("AAA-USD", &candles, date).unwrap()
    );
}

// ---------------------------------------------------------------------------------------------
// Règles d'instrument dérivées (§4.7)
// ---------------------------------------------------------------------------------------------

fn prix(valeurs: &[f64]) -> Vec<Candle> {
    valeurs
        .iter()
        .enumerate()
        .map(|(i, &v)| candle(i as i64 * MIN, v, 1.0))
        .collect()
}

#[test]
fn tick_derive_des_decimales() {
    // Une décimale : tick 0,1, la valeur de [kraken.BTC-USD] de config/instruments.toml.
    let regle = derive_instrument_rule("BTC-USD", &prix(&[7_000.1, 7_000.5, 7_001.0])).unwrap();
    let racine = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let btc = InstrumentRules::load(
        ExchangeId::Kraken,
        "BTC-USD",
        &racine.join("config/instruments.toml"),
    )
    .unwrap();
    assert_eq!(regle.tick_size, btc.tick_size);
    assert_eq!(regle.lot_size, btc.lot_size);
    assert_eq!((regle.min_qty, regle.min_notional), (0.0, 0.0));
    // Quatre décimales : 0,0001. Aucune : 1.
    assert_eq!(
        derive_instrument_rule("ABC-USD", &prix(&[0.1234, 0.5]))
            .unwrap()
            .tick_size,
        0.0001
    );
    assert_eq!(tick_decimals(&prix(&[3.0, 120.0])), 0);
    assert_eq!(
        derive_instrument_rule("ABC-USD", &prix(&[3.0, 120.0]))
            .unwrap()
            .tick_size,
        1.0
    );
    // Les quatre prix d'une bougie comptent.
    let mut une = prix(&[10.5]);
    une[0].low = 10.123;
    assert_eq!(tick_decimals(&une), 3);
}

#[test]
fn tick_derive_artefact_flottant_et_plafond() {
    // 0,30000000000000004 (0,1 + 0,2) : l'artefact n'ajoute aucune décimale.
    assert_eq!(0.1 + 0.2, 0.300_000_000_000_000_04);
    assert_eq!(tick_decimals(&prix(&[0.1 + 0.2, 0.5])), 1);
    // Un prix qui exige plus de 10 décimales donne 10.
    let un_tiers = 1.0 / 3.0;
    assert_eq!(tick_decimals(&prix(&[un_tiers])), 10);
    assert_eq!(
        derive_instrument_rule("ABC-USD", &prix(&[un_tiers]))
            .unwrap()
            .tick_size,
        1e-10
    );
    assert!(matches!(
        derive_instrument_rule("ABC-USD", &[]),
        Err(UniverseError::EmptySeries)
    ));
}

#[test]
fn tick_et_lot_de_la_fixture_btc_usd_egalent_config_instruments() {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/kraken_xbtusd_1m.csv");
    let candles: Vec<Candle> = KrakenCsvReader::open(&fixture, Timeframe::M1)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let regle = derive_instrument_rule("BTC-USD", &candles).unwrap();
    let btc = InstrumentRules::load(
        ExchangeId::Kraken,
        "BTC-USD",
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/instruments.toml"),
    )
    .unwrap();
    assert_eq!(regle.tick_size, btc.tick_size);
    assert_eq!(regle.lot_size, btc.lot_size);
    // Le fichier rendu se relit comme un fichier d'instruments de config/.
    let texte = render_instruments_toml(universe::INSTRUMENTS_METHOD, std::slice::from_ref(&regle))
        .unwrap();
    let relue = InstrumentRules::from_toml_str(ExchangeId::Kraken, "BTC-USD", &texte).unwrap();
    assert_eq!(relue, btc);
}

// ---------------------------------------------------------------------------------------------
// Assemblage d'un dossier d'univers et identité
// ---------------------------------------------------------------------------------------------

/// Trois paires synthétiques de 2019-01-01 à 2020-12-31, avec un volume distinct.
fn trois_paires() -> Vec<(String, Vec<Candle>)> {
    [("AAAUSD", 3.0), ("BBBUSD", 2.0), ("CCCUSD", 1.0)]
        .iter()
        .map(|(nom, volume)| {
            (
                nom.to_string(),
                series(
                    &regular(ms(2018, 12, 1), ms(2021, 1, 1), SIX_H),
                    10.5,
                    *volume,
                ),
            )
        })
        .collect()
}

/// Construit un `UniverseBuild` pour les trimestres `labels` à partir de [`trois_paires`].
fn construire(labels: &[&str], zip_sha: &str, avec_at: bool) -> UniverseBuild {
    let conf = config();
    let donnees = trois_paires();
    let entrees: Vec<PairEntry> = donnees
        .iter()
        .map(|(nom, candles)| entry(nom, candles))
        .collect();
    let mut quarters = Vec::new();
    let mut selections = Vec::new();
    for label in labels {
        let (debut, _) = tv4::model::universe::quarter_bounds(label).unwrap();
        let selection = select_at(&entrees, debut, &conf).unwrap();
        let rows = selection
            .members()
            .iter()
            .map(|membre| {
                let candles = &donnees
                    .iter()
                    .find(|(nom, _)| *nom == membre.candidate.kraken_pair)
                    .unwrap()
                    .1;
                let stats = &entrees
                    .iter()
                    .find(|e| e.candidate.symbol == membre.candidate.symbol)
                    .unwrap()
                    .stats;
                membre
                    .to_csv_row(
                        stats.excluded_tfs(label).to_vec(),
                        input_hash(&membre.candidate.symbol, candles, debut).unwrap(),
                    )
                    .unwrap()
            })
            .collect();
        let quality_hashes = selection
            .members()
            .iter()
            .map(|membre| {
                let candles = &donnees
                    .iter()
                    .find(|(nom, _)| *nom == membre.candidate.kraken_pair)
                    .unwrap()
                    .1;
                (
                    membre.candidate.symbol.clone(),
                    quality_hash(&membre.candidate.symbol, candles, label).unwrap(),
                )
            })
            .collect();
        quarters.push(QuarterBuild {
            quarter: (*label).to_string(),
            rows,
            eligible: selection.eligible_count(),
            quality_hashes,
        });
        selections.push(((*label).to_string(), selection));
    }
    let refs: Vec<(String, &QuarterSelection)> =
        selections.iter().map(|(l, s)| (l.clone(), s)).collect();
    let union = union_entries(&refs);
    let instruments = union
        .iter()
        .map(|u| {
            let candles = &donnees
                .iter()
                .find(|(nom, _)| *nom == u.kraken_pair)
                .unwrap()
                .1;
            derive_instrument_rule(&u.symbol, candles).unwrap()
        })
        .collect();
    let at = if avec_at {
        vec![tv4::store::universe::AtBuild {
            date: "2020-02-01T00:00:00Z".to_string(),
            rows: quarters[0].rows.clone(),
        }]
    } else {
        Vec::new()
    };
    UniverseBuild {
        source: UniverseSource {
            kind: "kraken_ohlcvt_zip".to_string(),
            zip_sha256: zip_sha.to_string(),
            release: "r1".to_string(),
            coverage_end: "2021-01-01T00:00:00Z".to_string(),
            pairs_in_zip: 3,
            candidates: 3,
        },
        rules: universe_rules(&conf),
        quarters,
        at,
        rejected_pairs: Vec::new(),
        unrecognized_pairs: Vec::new(),
        union,
        instruments,
        instruments_method: universe::INSTRUMENTS_METHOD.to_string(),
    }
}

#[test]
fn le_dossier_d_univers_s_ecrit_se_relit_et_garde_une_identite_stable() {
    let sha = "ab".repeat(32);
    let dossier = tempfile::tempdir().unwrap();
    let build = construire(&["2020-Q1", "2020-Q2"], &sha, false);
    // Les trimestres : les trois paires sont éligibles à D (365 jours d'historique depuis 2018-12-01),
    // classées par volume décroissant.
    assert_eq!(build.quarters[0].rows.len(), 3);
    let rangs: Vec<(u32, &str)> = build.quarters[0]
        .rows
        .iter()
        .map(|r| (r.rank, r.base.as_str()))
        .collect();
    assert_eq!(rangs, [(1, "AAA"), (2, "BBB"), (3, "CCC")]);
    assert_eq!(
        build
            .union
            .iter()
            .map(|u| (u.symbol.as_str(), u.first_quarter.as_str()))
            .collect::<Vec<_>>(),
        [
            ("AAA-USD", "2020-Q1"),
            ("BBB-USD", "2020-Q1"),
            ("CCC-USD", "2020-Q1")
        ]
    );
    write_universe(dossier.path(), &build).unwrap();
    let charge = load(dossier.path()).unwrap();
    assert_eq!(charge.file().rules.top_n, 12);
    assert_eq!(charge.schedule().quarters().len(), 2);
    let (debut, fin) = (ms(2020, 1, 1), ms(2020, 7, 1));
    let identite = charge.window_hash(debut, fin).unwrap();

    // Ajouter un trimestre, changer la source ou ajouter une date --at ne change pas l'identité des
    // trimestres existants.
    let autre = tempfile::tempdir().unwrap();
    let plus = construire(&["2020-Q1", "2020-Q2", "2020-Q3"], &"cd".repeat(32), true);
    write_universe(autre.path(), &plus).unwrap();
    let relu = load(autre.path()).unwrap();
    assert_eq!(relu.window_hash(debut, fin).unwrap(), identite);
    assert_ne!(relu.window_hash(debut, ms(2020, 10, 1)).unwrap(), identite);

    // Changer une ligne d'un CSV (ici un volume) la change.
    let modifie = tempfile::tempdir().unwrap();
    let mut change = construire(&["2020-Q1", "2020-Q2"], &sha, false);
    change.quarters[1].rows[0].volume_quote_90d += 1.0;
    write_universe(modifie.path(), &change).unwrap();
    let autre_identite = load(modifie.path())
        .unwrap()
        .window_hash(debut, fin)
        .unwrap();
    assert_ne!(autre_identite, identite);
    // Mais les identités du premier trimestre seul restent égales.
    assert_eq!(
        load(modifie.path())
            .unwrap()
            .window_hash(debut, ms(2020, 4, 1))
            .unwrap(),
        charge.window_hash(debut, ms(2020, 4, 1)).unwrap()
    );

    // Un trimestre figé ne se reconstruit pas différemment.
    match write_universe(dossier.path(), &change) {
        Err(StoreError::FrozenQuarterChanged { quarter }) => assert_eq!(quarter, "2020-Q2"),
        autre => panic!("FrozenQuarterChanged attendue : {autre:?}"),
    }
}

#[test]
fn deux_constructions_donnent_les_memes_octets() {
    let sha = "ab".repeat(32);
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write_universe(a.path(), &construire(&["2020-Q1", "2020-Q2"], &sha, true)).unwrap();
    write_universe(b.path(), &construire(&["2020-Q1", "2020-Q2"], &sha, true)).unwrap();
    for nom in [
        "universe.json",
        "universe_2020-Q1.csv",
        "universe_2020-Q2.csv",
        "eligible_2020-02-01.csv",
        "instruments_kraken.toml",
    ] {
        assert_eq!(
            std::fs::read(a.path().join(nom)).unwrap(),
            std::fs::read(b.path().join(nom)).unwrap(),
            "{nom}"
        );
    }
}

#[test]
fn les_regles_ecrites_sont_les_constantes_du_module() {
    let regles = universe_rules(&config());
    assert_eq!(regles.top_n as usize, TOP_N);
    assert_eq!(regles.history_days, 365);
    assert_eq!(regles.quote_gap_ms, 604_800_000);
    assert_eq!(regles.liquidity_window_days, 90);
    assert_eq!(regles.min_price, 0.01);
    assert_eq!(regles.quotes, ["USD", "USDT"]);
    assert_eq!(regles.quote_preference, regles.quotes);
    assert!(regles.quality_uses_quarter_data);
    assert!(regles.config_file_hash.starts_with("blake3:"));
    let mut tries = regles.excluded_bases.clone();
    tries.sort();
    assert_eq!(regles.excluded_bases, tries);
    // Les règles sont hachables : deux appels, même hash.
    assert_eq!(
        regles.hash().unwrap(),
        universe_rules(&config()).hash().unwrap()
    );
}

#[test]
fn metriques_d_une_paire_eligible_sont_publiables() {
    let selection = select_at(
        &[entry("AAAUSD", &eligible_series(ws(), 2.0))],
        D,
        &config(),
    )
    .unwrap();
    let membre = &selection.members()[0];
    let EligibleMetrics {
        first_active_ms,
        longest_gap_ms,
        last_close,
        ..
    } = membre.metrics;
    assert_eq!(first_active_ms, ws());
    assert_eq!(longest_gap_ms, SIX_H - MIN);
    assert_eq!(last_close, 10.0);
    let ligne = membre
        .to_csv_row(Vec::new(), format!("blake3:{}", "0".repeat(64)))
        .unwrap();
    assert_eq!(ligne.first_active, "2019-01-01T00:00:00Z");
    assert_eq!((ligne.rank, ligne.symbol.as_str()), (1, "AAA-USD"));
}

// ---------------------------------------------------------------------------------------------
// Recoupement avec le store : les exclusions qualité des CSV sont celles du build de recherche
// ---------------------------------------------------------------------------------------------

#[test]
fn les_exclusions_des_csv_egalent_celles_du_dataset_de_bloc() {
    use tv4::model::Block;
    use tv4::store::{ResearchMeta, build_dataset_research};

    let sha = "ab".repeat(32);
    let labels = [
        "2018-Q1", "2018-Q2", "2018-Q3", "2018-Q4", "2019-Q1", "2019-Q2", "2019-Q3", "2019-Q4",
        "2020-Q1", "2020-Q2",
    ];
    let build = construire(&labels, &sha, false);
    // Avant 2019-12, aucune paire n'a 365 jours d'historique : trimestres sans membre.
    assert!(build.quarters[0].rows.is_empty());
    let dernier = build.quarters.last().unwrap();
    assert_eq!(dernier.rows.len(), 3);
    // Une minute toutes les 6 h : 5m à 4h sont au-delà de 1 % de bougies absentes, le 12h et le 1d non.
    assert_eq!(
        dernier.rows[0].excluded_tfs,
        [
            Timeframe::M5,
            Timeframe::M15,
            Timeframe::M30,
            Timeframe::H1,
            Timeframe::H4
        ]
    );
    let univers = tempfile::tempdir().unwrap();
    write_universe(univers.path(), &build).unwrap();
    let charge = load(univers.path()).unwrap();

    let racine = tempfile::tempdir().unwrap();
    for (nom, candles) in trois_paires() {
        let symbole = entry(&nom, &candles).candidate.symbol;
        let partition = Partition::new(
            ExchangeId::Kraken,
            symbole.parse::<Symbol>().unwrap(),
            Timeframe::M1,
        );
        write_partition(
            racine.path(),
            &partition,
            &Columns::from_candles(&candles),
            "synthetic",
            1_893_456_000_000,
        )
        .unwrap();
    }
    let (debut, fin) = (ms(2018, 1, 1), ms(2020, 7, 1));
    let symboles: Vec<Symbol> = charge
        .union_symbols(debut, fin)
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(symboles.len(), 3);
    let meta = ResearchMeta {
        block: Block::Dev,
        universe: Some(charge.reference(debut, fin).unwrap()),
        universe_quality: Some(charge.quality_expectation(debut, fin)),
    };
    let requete = MultiBuildRequest {
        name: "dev_synthetique",
        exchange: ExchangeId::Kraken,
        symbols: &symboles,
        tf: Timeframe::M1,
        window: Window::new(debut, fin).unwrap(),
        gaps_policy: GapsPolicy::ZeroVolumeMarked,
    };
    let issue = build_dataset_research(racine.path(), &requete, &meta, 1_893_456_000_000).unwrap();
    // Le build compare chaque (trimestre, symbole) du CSV au calcul du store : il a réussi, donc les
    // deux calculs de qualité s'accordent, et les exclusions du manifeste sont celles du CSV.
    let exclusions = issue.manifest.quality_exclusions.unwrap();
    let aaa = &exclusions["AAA-USD"];
    assert!(
        aaa.iter()
            .any(|e| e.quarter == "2020-Q1" && e.tfs == dernier.rows[0].excluded_tfs)
    );

    // Un CSV altéré (un TF de moins) est refusé par le build.
    let mut altere = construire(&labels, &sha, false);
    altere.quarters[8].rows[0].excluded_tfs.pop();
    let autre = tempfile::tempdir().unwrap();
    write_universe(autre.path(), &altere).unwrap();
    let charge = load(autre.path()).unwrap();
    let meta = ResearchMeta {
        block: Block::Dev,
        universe: Some(charge.reference(debut, fin).unwrap()),
        universe_quality: Some(charge.quality_expectation(debut, fin)),
    };
    let requete = MultiBuildRequest {
        name: "dev_altere",
        ..requete
    };
    let erreur =
        build_dataset_research(racine.path(), &requete, &meta, 1_893_456_000_000).unwrap_err();
    assert!(
        matches!(erreur, StoreError::UniverseQualityMismatch { .. }),
        "{erreur}"
    );
}
