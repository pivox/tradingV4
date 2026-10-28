//! Rapport du mode portefeuille (S9, lot F, docs/specs/s9.md §9, §10 et §13) : `metrics.json` de schéma
//! `tv4-metrics-s9`, entonnoir par paire avec ses motifs de refus, exécutabilité G0 et test G1, trou de
//! cotation dans les sorties, et équivalence des champs communs avec le chemin de S8 sur une paire.
//!
//! Marché inventé : les données sont plates ou synthétiques, aucune donnée réelle.

mod common;
mod portfolio_common;

use common::{fixture_1m, kraken_btc_usd, okx_fees, repo_path, slippage};
use portfolio_common::{
    DAY, MINUTE, Scripted, T0, buy_quote, config, flat_bar, flat_series, instrument, names, run,
};
use serde_json::Value;
use tv4::engine::{
    CellConfig, DEFAULT_QUOTE_GAP_MS, PortfolioConfig, PortfolioInputs, PortfolioResult, run_cell,
    run_portfolio,
};
use tv4::model::{Candle, ExchangeId, InstrumentRules, Side, Timeframe, Ts, hash};
use tv4::report::executability::{
    self, ExecutabilityInput, G1Counts, LegMarket, OkxPair, PairInput, PairVerdict,
};
use tv4::report::metrics_s9::{
    DatasetInfo, InstrumentsInfo, MetricsS9, MetricsS9Inputs, SeriesInfo,
};
use tv4::report::writers::{TRADES_COLUMNS, trades_csv_legs};
use tv4::report::{DataInfo, DatasetRef, Metrics};
use tv4::sim::{SimRules, SizingMode};
use tv4::store::Columns;
use tv4::strategy::{Signal, Size, Strategy, registry};

fn dataset_info(data: &[(&str, Vec<Candle>)], exchange: ExchangeId) -> DatasetInfo {
    DatasetInfo {
        name: "synthetique".to_string(),
        exchange,
        tf: Timeframe::M1,
        gaps_policy: "skip".to_string(),
        hash_format: "dsh-v1".to_string(),
        dataset_hash: "blake3:aa".to_string(),
        series: data
            .iter()
            .map(|(symbol, candles)| SeriesInfo {
                symbol: (*symbol).to_string(),
                rows: candles.len() as u64,
                first_ms: candles.first().map_or(0, |c| c.open_time_ms),
                last_ms: candles.last().map_or(0, |c| c.open_time_ms),
            })
            .collect(),
    }
}

/// Colonnes des paires, dans l'ordre de `data`.
fn columns_of(data: &[(&str, Vec<Candle>)]) -> Vec<Columns> {
    data.iter()
        .map(|(_, candles)| Columns::from_candles(candles))
        .collect()
}

/// Construit `metrics.json` d'un run du moteur ; `okx` donne les minimums et l'état de chaque paire.
fn metrics_of(
    result: &PortfolioResult,
    data: &[(&str, Vec<Candle>)],
    okx: Option<Vec<OkxPair>>,
    exchange: ExchangeId,
) -> MetricsS9 {
    let columns = columns_of(data);
    let markets: Vec<LegMarket<'_>> = result
        .universe
        .iter()
        .map(|symbol| {
            let index = data.iter().position(|(s, _)| s == symbol).unwrap();
            LegMarket {
                open_time_ms: columns[index].open_time_ms(),
                low: columns[index].low(),
                close: columns[index].close(),
                volume: columns[index].volume(),
            }
        })
        .collect();
    let instruments = match &okx {
        Some(pairs) => InstrumentsInfo::okx(
            "spot.json".to_string(),
            "blake3:bb".to_string(),
            "2026-10-28T12:45:00.123Z".to_string(),
            pairs,
            Vec::new(),
        ),
        None => InstrumentsInfo::toml(),
    };
    MetricsS9::new(
        result,
        MetricsS9Inputs {
            dataset: dataset_info(data, exchange),
            instruments,
            okx_pairs: okx,
            markets,
        },
    )
    .expect("metrics s9")
}

fn json_of(metrics: &MetricsS9) -> Value {
    // `to_value` et non un aller-retour par texte : l'analyse des flottants de serde_json n'est pas
    // exacte au dernier bit sans l'option `float_roundtrip`.
    serde_json::to_value(metrics).unwrap()
}

fn okx_pair(inst: &str, min_sz: f64, state: &str) -> OkxPair {
    OkxPair {
        inst_id: inst.to_string(),
        min_sz,
        state: state.to_string(),
    }
}

fn buyer_at(minute: i64, signal: Signal) -> Box<dyn Strategy> {
    Box::new(Scripted::new(vec![(T0 + minute * MINUTE, signal)]).0)
}

fn by_symbol<'a>(list: &'a Value, symbol: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|e| e["symbol"] == symbol)
        .unwrap()
}

/// Huit paires plates à 100 sur 100 minutes, chacune avec un signal d'achat à une minute choisie pour
/// provoquer un motif de refus précis (N = 3, capital 1 000, mode capitalisé).
///
/// * A, F, G : entrées qui remplissent (les trois places) ;
/// * B : minimum d'ordre de 5 unités pour 1 unité demandée (`min_notional`) ;
/// * C : 1e-9 unité demandée pour un lot de 1e-8 (`lot_step`) ;
/// * D : volume 1m dérisoire, règle des 0,5 % (`volume`, refusé au fill) ;
/// * H : `PlaceLimit` d'achat de 10 000 pour 1 000 de quote libre, refusé à la soumission (`cash`) ;
/// * E : portefeuille plein au moment du signal (`max_positions`).
fn rejection_run() -> (PortfolioResult, Vec<(&'static str, Vec<Candle>)>) {
    let symbols = [
        "A-USD", "B-USD", "C-USD", "D-USD", "E-USD", "F-USD", "G-USD", "H-USD",
    ];
    let thin = |symbol: &str| if symbol == "D-USD" { 10.0 } else { 1.0e6 };
    let data: Vec<(&str, Vec<Candle>)> = symbols
        .iter()
        .map(|s| (*s, flat_series(0, 100, 100.0, thin(s))))
        .collect();
    let universe = names(&symbols);
    let instruments: Vec<InstrumentRules> = symbols
        .iter()
        .map(|s| instrument(if *s == "B-USD" { 5.0 } else { 0.0 }))
        .collect();
    let strategies: Vec<Box<dyn Strategy>> = vec![
        buyer_at(10, buy_quote(100.0)),
        buyer_at(20, buy_quote(100.0)),
        buyer_at(30, buy_quote(1.0e-7)),
        buyer_at(40, buy_quote(100.0)),
        buyer_at(50, buy_quote(100.0)),
        buyer_at(15, buy_quote(100.0)),
        buyer_at(45, buy_quote(100.0)),
        buyer_at(
            42,
            Signal::PlaceLimit {
                side: Side::Buy,
                price: 90.0,
                size: Size::Quote(10_000.0),
                ttl_bars: 100,
                slot: 1,
                tag: "grille",
            },
        ),
    ];
    let result = run(
        &data,
        strategies,
        PortfolioConfig {
            max_positions: 3,
            sizing: SizingMode::Compounding,
            sim_rules: SimRules::default(),
            ..config(&universe, &instruments, 1000.0)
        },
    );
    (result, data)
}

#[test]
fn chaque_motif_de_refus_tombe_dans_sa_colonne_de_funnel_by_pair() {
    let (result, data) = rejection_run();
    let metrics = metrics_of(&result, &data, None, ExchangeId::Kraken);
    let json = json_of(&metrics);
    let pairs = &json["funnel_by_pair"];
    let rejected = |symbol: &str, key: &str| by_symbol(pairs, symbol)["rejected"][key].as_u64();
    assert_eq!(rejected("B-USD", "min_notional"), Some(1));
    assert_eq!(rejected("C-USD", "lot_step"), Some(1));
    assert_eq!(rejected("D-USD", "volume"), Some(1));
    assert_eq!(rejected("E-USD", "max_positions"), Some(1));
    assert_eq!(rejected("H-USD", "cash"), Some(1));
    for symbol in ["A-USD", "F-USD", "G-USD"] {
        let pair = by_symbol(pairs, symbol);
        assert_eq!(pair["entry_fills"], 1, "{symbol}");
        assert_eq!(pair["trades"], 1, "{symbol}");
        assert_eq!(
            pair["rejected"],
            serde_json::json!({
                "cash": 0, "lot_step": 0, "max_positions": 0, "min_notional": 0, "other": 0, "volume": 0
            })
        );
    }
    // Chaque paire refusée a exactement un refus, dans un seul motif.
    for symbol in ["B-USD", "C-USD", "D-USD", "E-USD", "H-USD"] {
        let total: u64 = by_symbol(pairs, symbol)["rejected"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_u64().unwrap())
            .sum();
        assert_eq!(total, 1, "{symbol}");
        assert_eq!(by_symbol(pairs, symbol)["entry_fills"], 0, "{symbol}");
    }
    // Les listes par paire suivent l'ordre de l'univers.
    let order: Vec<&str> = pairs
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        [
            "A-USD", "B-USD", "C-USD", "D-USD", "E-USD", "F-USD", "G-USD", "H-USD"
        ]
    );
    // Σ funnel_by_pair = funnel, compteur par compteur.
    for key in [
        "evaluations",
        "entry_signals",
        "entry_orders",
        "entry_fills",
        "trades",
    ] {
        let sum: u64 = pairs
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p[key].as_u64().unwrap())
            .sum();
        assert_eq!(Some(sum), json["funnel"][key].as_u64(), "{key}");
    }
    // Les refus de `max_positions` sont dans le bloc portfolio et dans `signals` (emitted − accepted).
    assert_eq!(json["portfolio"]["positions_cap_hits"], 1);
    assert_eq!(json["funnel"]["entry_rejected_max_positions"], 1);
    assert_eq!(json["signals"]["rejected_max_positions"], 1);
    assert_eq!(
        json["signals"]["rejected"].as_u64().unwrap(),
        json["signals"]["emitted"].as_u64().unwrap()
            - json["signals"]["accepted"].as_u64().unwrap()
    );
    assert_eq!(json["portfolio"]["max_open_positions"], 3);
}

/// Trois paires plates à 100, très liquides, qui émettent chacune un achat market au risque de 50 % de la
/// base (stop à 99) à la même minute : le risque pousse chaque entrée au plafond. Mode recherche, N = 2.
fn research_three_legs(initial_cash: f64) -> (PortfolioResult, Vec<(&'static str, Vec<Candle>)>) {
    let symbols = ["A-USD", "B-USD", "C-USD"];
    let data: Vec<(&str, Vec<Candle>)> = symbols
        .iter()
        .map(|s| (*s, flat_series(0, 100, 100.0, 1.0e6)))
        .collect();
    let universe = names(&symbols);
    let instruments = vec![instrument(0.0); 3];
    let strategies: Vec<Box<dyn Strategy>> = (0..3)
        .map(|_| buyer_at(10, portfolio_common::buy_risk(0.5, 99.0)))
        .collect();
    let result = run(
        &data,
        strategies,
        PortfolioConfig {
            max_positions: 2,
            sizing: SizingMode::Research,
            ..config(&universe, &instruments, initial_cash)
        },
    );
    (result, data)
}

#[test]
fn bloc_portfolio_en_recherche_a_trois_jambes_et_n_egal_2() {
    // docs S9 §3.3 (lot E), test non-debug : le bloc `portfolio` publié respecte N et le plafond de
    // coût cumulé du mode recherche quand N mord, avec un refus `max_positions` dans l'entonnoir.
    let (result, data) = research_three_legs(1000.0);
    let metrics = metrics_of(&result, &data, None, ExchangeId::Kraken);
    let json = json_of(&metrics);
    let portfolio = &json["portfolio"];
    assert_eq!(portfolio["sizing"], "research");
    assert_eq!(portfolio["legs"], 3);
    assert_eq!(portfolio["initial_cash"], 1000.0);
    assert_eq!(portfolio["max_open_positions"], 2);
    assert_eq!(portfolio["positions_cap_hits"], 1);
    // Le coût cumulé (positions ouvertes au plus fort) ne dépasse pas le capital initial, et il en
    // approche : deux entrées de capital / N = 500 moins la marge de cash du dernier achat market.
    let cost = portfolio["max_cost_to_initial"].as_f64().unwrap();
    assert!(cost <= 1.0 + 1e-9, "{cost}");
    assert!(cost > 0.9, "{cost}");
    // Le plafond par entrée est celui de `capital / N`, jambe par jambe : aucune entrée au-dessus de 500,
    // et le plafond de notional a mordu sur les deux jambes qui sont entrées.
    assert_eq!(result.trades.len(), 2);
    for trade in &result.trades {
        assert!(
            trade.qty * trade.entry_price <= 500.0 * (1.0 + 1e-9),
            "notional {} au-dessus de capital / N",
            trade.qty * trade.entry_price
        );
    }
    let sim = &json["sim_by_pair"];
    let capped =
        |symbol: &str| by_symbol(sim, symbol)["detail"]["entries_capped_by_notional"].as_u64();
    assert_eq!(capped("A-USD"), Some(1));
    assert_eq!(capped("B-USD"), Some(1));
    assert_eq!(capped("C-USD"), Some(0));
    // Le refus tombe sur la troisième jambe dans l'ordre de priorité (égalité : ordre de l'univers).
    let pairs = &json["funnel_by_pair"];
    let rejected_max_positions: Vec<u64> = ["A-USD", "B-USD", "C-USD"]
        .iter()
        .map(|s| {
            by_symbol(pairs, s)["rejected"]["max_positions"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(rejected_max_positions, [0, 0, 1]);
    assert_eq!(json["funnel"]["entry_rejected_max_positions"], 1);
    // Le capital est hors du hash : à 3 000, les mêmes bornes tiennent et le ratio est le même.
    let (large, large_data) = research_three_legs(3000.0);
    assert_eq!(result.config_hash, large.config_hash);
    let large_json = json_of(&metrics_of(&large, &large_data, None, ExchangeId::Kraken));
    let large_cost = large_json["portfolio"]["max_cost_to_initial"]
        .as_f64()
        .unwrap();
    assert!(large_cost <= 1.0 + 1e-9, "{large_cost}");
    assert!((large_cost - cost).abs() < 1e-6, "{large_cost} {cost}");
    assert_eq!(large_json["portfolio"]["max_open_positions"], 2);
    assert_eq!(large_json["portfolio"]["positions_cap_hits"], 1);
}

#[test]
fn g1_compte_volume_min_notional_et_lot_step_mais_pas_cash_ni_max_positions() {
    let (result, data) = rejection_run();
    let okx: Vec<OkxPair> = result
        .universe
        .iter()
        .map(|s| okx_pair(&s.replace("-USD", "-USDC"), 0.0, "live"))
        .collect();
    let metrics = metrics_of(&result, &data, Some(okx), ExchangeId::Kraken);
    let g1 = &json_of(&metrics)["g1_executable"];
    assert_eq!(g1["fills"], 3);
    assert_eq!(g1["rejected_volume"], 1);
    assert_eq!(g1["rejected_min_notional"], 1);
    assert_eq!(g1["rejected_lot_step"], 1);
    assert_eq!(g1["rejected_cash"], 1);
    assert_eq!(g1["rejected_max_positions"], 1);
    // 3 / (3 + 1 + 1 + 1) : le cash et max_positions ne comptent pas.
    assert_eq!(g1["ratio"], 0.5);
    assert_eq!(g1["verdict"], "fail");
    assert_eq!(g1["threshold"], 0.95);
    // Le capital du run est 1 000 : G1 ne se lit qu'à 300.
    assert_eq!(g1["at_reference_capital"], false);
}

#[test]
fn sans_fichier_okx_g0_et_g1_sont_not_applicable() {
    let (result, data) = rejection_run();
    let metrics = metrics_of(&result, &data, None, ExchangeId::Kraken);
    let json = json_of(&metrics);
    assert_eq!(json["g1_executable"]["verdict"], "not_applicable");
    assert_eq!(json["instruments"]["source"], "toml");
    assert!(json["instruments"]["instruments_hash"].is_null());
    assert!(json["instruments"]["file"].is_null());
    let ex = &json["executability"];
    assert_eq!(ex["venue_matches_execution"], false);
    assert_eq!(ex["live_list"], serde_json::json!([]));
    assert_eq!(ex["rejected"], serde_json::json!([]));
    for pair in ex["pairs"].as_array().unwrap() {
        assert_eq!(pair["verdict"], "not_applicable");
        assert!(pair["okx_inst_id"].is_null());
        assert!(pair["min_sz"].is_null());
    }
    // Les minimums viennent du TOML : l'écart de S8 reste ouvert.
    let deviations: Vec<&str> = json["sim_deviations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert!(deviations.contains(&"instrument_minimums_unvalidated"));

    // Avec un fichier OKX, il est remplacé.
    let okx: Vec<OkxPair> = result
        .universe
        .iter()
        .map(|s| okx_pair(&s.replace("-USD", "-USDC"), 0.0, "live"))
        .collect();
    let with_okx = json_of(&metrics_of(&result, &data, Some(okx), ExchangeId::Kraken));
    let deviations: Vec<&str> = with_okx["sim_deviations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert!(!deviations.contains(&"instrument_minimums_unvalidated"));
    assert!(deviations.contains(&"instrument_minimums_current_not_historical"));
    assert_eq!(with_okx["instruments"]["source"], "okx");
    assert_eq!(with_okx["instruments"]["file"], "spot.json");
    assert_eq!(with_okx["instruments"]["not_live"], serde_json::json!([]));
}

#[test]
fn les_etats_non_live_sont_publies_dans_instruments_et_dans_le_verdict() {
    let (result, data) = rejection_run();
    let okx: Vec<OkxPair> = result
        .universe
        .iter()
        .map(|s| {
            let state = if s == "A-USD" { "suspend" } else { "live" };
            okx_pair(&s.replace("-USD", "-USDC"), 0.0, state)
        })
        .collect();
    let json = json_of(&metrics_of(&result, &data, Some(okx), ExchangeId::Okx));
    assert_eq!(
        json["instruments"]["not_live"],
        serde_json::json!(["A-USDC"])
    );
    // Dataset OKX, mais jambes en USD et instruments en USDC : pas les carnets d'exécution.
    assert_eq!(json["executability"]["venue_matches_execution"], false);
    // A a un notional de référence et un volume suffisant : seul l'état la rejette.
    let a = by_symbol(&json["executability"]["pairs"], "A-USD");
    assert_eq!(a["verdict"], "rejected_state");
    assert_eq!(a["state"], "suspend");
    assert_eq!(a["okx_inst_id"], "A-USDC");
}

/// Deux paires plates avec un achat chacune, sous les symboles `symbols`.
fn two_flat_legs(
    symbols: [&'static str; 2],
) -> (PortfolioResult, Vec<(&'static str, Vec<Candle>)>) {
    let data: Vec<(&str, Vec<Candle>)> = symbols
        .iter()
        .map(|s| (*s, flat_series(0, 100, 100.0, 1.0e6)))
        .collect();
    let universe = names(&symbols);
    let instruments = vec![instrument(0.0), instrument(0.0)];
    let strategies: Vec<Box<dyn Strategy>> = vec![
        buyer_at(10, buy_quote(100.0)),
        buyer_at(20, buy_quote(100.0)),
    ];
    let result = run(
        &data,
        strategies,
        PortfolioConfig {
            max_positions: 2,
            sizing: SizingMode::Compounding,
            sim_rules: SimRules::default(),
            ..config(&universe, &instruments, 1000.0)
        },
    );
    (result, data)
}

fn venue_flag(
    symbols: [&'static str; 2],
    okx: [&str; 2],
    exchange: ExchangeId,
    with_file: bool,
) -> bool {
    let (result, data) = two_flat_legs(symbols);
    let pairs = with_file.then(|| okx.iter().map(|i| okx_pair(i, 0.0, "live")).collect());
    let json = json_of(&metrics_of(&result, &data, pairs, exchange));
    json["executability"]["venue_matches_execution"]
        .as_bool()
        .unwrap()
}

#[test]
fn venue_matches_execution_exige_un_dataset_okx_en_instruments_usdc_d_execution() {
    let usdc = ["BTC-USDC", "ETH-USDC"];
    // Dataset OKX en USDT : les instruments d'exécution sont en USDC, les carnets mesurés ne sont pas
    // ceux du compte.
    assert!(!venue_flag(
        ["BTC-USDT", "ETH-USDT"],
        usdc,
        ExchangeId::Okx,
        true
    ));
    // Dataset OKX en USDC : chaque jambe est son propre instrument d'exécution.
    assert!(venue_flag(usdc, usdc, ExchangeId::Okx, true));
    // Dataset Kraken : jamais, même si le symbole est déjà en USDC.
    assert!(!venue_flag(usdc, usdc, ExchangeId::Kraken, true));
    // Dataset OKX sans fichier d'instruments : rien ne dit que l'exécution est en USDC.
    assert!(!venue_flag(usdc, usdc, ExchangeId::Okx, false));
}

/// Colonnes 1m écrites à la main pour G0.
struct Cols {
    open_time_ms: Vec<Ts>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
}

impl Cols {
    /// `minutes` minutes consécutives, prix plat `price`, volume `volume`, sauf les `absent` retirées.
    fn flat(minutes: i64, price: f64, volume: f64, absent: &[i64]) -> Cols {
        let mut cols = Cols {
            open_time_ms: Vec::new(),
            low: Vec::new(),
            close: Vec::new(),
            volume: Vec::new(),
        };
        for m in (0..minutes).filter(|m| !absent.contains(m)) {
            cols.open_time_ms.push(T0 + m * MINUTE);
            cols.low.push(price);
            cols.close.push(price);
            cols.volume.push(volume);
        }
        cols
    }

    fn market(&self) -> LegMarket<'_> {
        LegMarket {
            open_time_ms: &self.open_time_ms,
            low: &self.low,
            close: &self.close,
            volume: &self.volume,
        }
    }
}

fn pair<'a>(
    symbol: &'a str,
    cols: &'a Cols,
    fractions: &'a [f64],
    min_sz: f64,
    state: &str,
) -> PairInput<'a> {
    PairInput {
        symbol,
        okx: Some(okx_pair(&symbol.replace("-USD", "-USDC"), min_sz, state)),
        entry_orders: fractions.len() as u64,
        entry_fractions: fractions,
        market: cols.market(),
    }
}

fn g0(pairs: Vec<PairInput<'_>>, minutes: i64) -> Vec<(PairVerdict, f64)> {
    let out = executability::compute(&ExecutabilityInput {
        pairs,
        window_first_open_ms: T0,
        window_last_open_ms: T0 + (minutes - 1) * MINUTE,
        okx_rules: true,
        venue_matches_execution: true,
    });
    out.pairs
        .iter()
        .map(|p| (p.verdict, p.notional_ref.unwrap_or(f64::NAN)))
        .collect()
}

#[test]
fn g0_distingue_une_paire_epaisse_d_une_paire_mince_et_l_ordre_des_tests() {
    let minutes = 1000;
    // Épaisse : volume quote 1m = 100 × 1e5 = 1e7, plafond 5e4 ; mince : 100 × 100 = 1e4, plafond 50.
    let thick = Cols::flat(minutes, 100.0, 1.0e5, &[]);
    let thin = Cols::flat(minutes, 100.0, 100.0, &[]);
    // 0,2 de la base de 300 : notional de référence 60.
    let fractions = [0.2, 0.2, 0.2];
    let verdicts = g0(
        vec![
            pair("EPAIS-USD", &thick, &fractions, 0.0001, "live"),
            pair("MINCE-USD", &thin, &fractions, 0.0001, "live"),
        ],
        minutes,
    );
    assert_eq!(verdicts[0], (PairVerdict::Executable, 60.0));
    assert_eq!(verdicts[1], (PairVerdict::RejectedVolume, 60.0));

    // Minimum d'ordre de 1 unité à 100 : 100 > 60, l'épaisse passe à rejected_min_size.
    let verdicts = g0(
        vec![pair("EPAIS-USD", &thick, &fractions, 1.0, "live")],
        minutes,
    );
    assert_eq!(verdicts[0].0, PairVerdict::RejectedMinSize);

    // Ordre des tests : volume d'abord, même si le minimum d'ordre et l'état sont aussi en défaut.
    let verdicts = g0(
        vec![pair("MINCE-USD", &thin, &fractions, 1.0, "suspend")],
        minutes,
    );
    assert_eq!(verdicts[0].0, PairVerdict::RejectedVolume);
    // Puis le minimum, avant l'état.
    let verdicts = g0(
        vec![pair("EPAIS-USD", &thick, &fractions, 1.0, "suspend")],
        minutes,
    );
    assert_eq!(verdicts[0].0, PairVerdict::RejectedMinSize);
    let verdicts = g0(
        vec![pair("EPAIS-USD", &thick, &fractions, 0.0001, "suspend")],
        minutes,
    );
    assert_eq!(verdicts[0].0, PairVerdict::RejectedState);

    // Une paire sans entrée est indéterminée, jamais exécutable.
    let verdicts = g0(
        vec![pair("EPAIS-USD", &thick, &[], 0.0001, "live")],
        minutes,
    );
    assert_eq!(verdicts[0].0, PairVerdict::Indeterminate);
}

#[test]
fn g0_borne_le_notional_de_reference_par_le_capital_du_palier() {
    let minutes = 100;
    let thick = Cols::flat(minutes, 100.0, 1.0e5, &[]);
    // Fraction 0,25 (stop à 4 % pour 1 % de risque) : 75, sans réduction par le N du run de recherche
    // (le live au palier 100 % n'a pas de plafond capital / N par ligne).
    let verdicts = g0(
        vec![pair("A-USD", &thick, &[0.25], 0.0001, "live")],
        minutes,
    );
    assert_eq!(verdicts[0], (PairVerdict::Executable, 75.0));
    // Fraction 2,0 : 600 demandés, borné à 300, le cash du palier 100 % sans levier.
    let verdicts = g0(vec![pair("A-USD", &thick, &[2.0], 0.0001, "live")], minutes);
    assert_eq!(verdicts[0], (PairVerdict::Executable, 300.0));
    // Fraction 1,0 : exactement 300.
    let verdicts = g0(vec![pair("A-USD", &thick, &[1.0], 0.0001, "live")], minutes);
    assert_eq!(verdicts[0], (PairVerdict::Executable, 300.0));
    // Paire dont le plafond de volume (0,5 % de 1e4 = 50) est entre 37,5 et 75 : refusée sur 75, ce
    // que l'ancien plafond 300 / 8 laissait passer à tort.
    let thin = Cols::flat(minutes, 100.0, 100.0, &[]);
    let verdicts = g0(vec![pair("B-USD", &thin, &[0.25], 0.0001, "live")], minutes);
    assert_eq!(verdicts[0], (PairVerdict::RejectedVolume, 75.0));
}

#[test]
fn les_minutes_absentes_font_passer_la_mediane_de_volume_a_zero() {
    let minutes = 100;
    // Toutes présentes : volume quote 1m = 100 × 1e3 = 1e5, plafond 500 ≥ 60.
    let full = Cols::flat(minutes, 100.0, 1.0e3, &[]);
    // 60 minutes absentes sur 100 : plus de la moitié des minutes valent 0, médiane basse = 0.
    let absent: Vec<i64> = (0..60).collect();
    let holes = Cols::flat(minutes, 100.0, 1.0e3, &absent);
    let fractions = [0.2];
    let out = executability::compute(&ExecutabilityInput {
        pairs: vec![
            pair("PLEIN-USD", &full, &fractions, 0.0001, "live"),
            pair("TROUS-USD", &holes, &fractions, 0.0001, "live"),
        ],
        window_first_open_ms: T0,
        window_last_open_ms: T0 + (minutes - 1) * MINUTE,
        okx_rules: true,
        venue_matches_execution: true,
    });
    assert_eq!(out.pairs[0].median_quote_volume_1m, Some(1.0e5));
    assert_eq!(out.pairs[0].verdict, PairVerdict::Executable);
    assert_eq!(out.pairs[1].median_quote_volume_1m, Some(0.0));
    assert_eq!(out.pairs[1].volume_cap, Some(0.0));
    assert_eq!(out.pairs[1].verdict, PairVerdict::RejectedVolume);
    assert_eq!(out.live_list, ["PLEIN-USD"]);
    assert_eq!(out.rejected.len(), 1);
    assert_eq!(out.rejected[0].symbol, "TROUS-USD");
    assert_eq!(out.rejected[0].reason, "rejected_volume");
    // Et 30 minutes absentes seulement : la médiane est celle des minutes présentes.
    let absent: Vec<i64> = (0..30).collect();
    let few = Cols::flat(minutes, 100.0, 1.0e3, &absent);
    let out = executability::compute(&ExecutabilityInput {
        pairs: vec![pair("PEU-USD", &few, &fractions, 0.0001, "live")],
        window_first_open_ms: T0,
        window_last_open_ms: T0 + (minutes - 1) * MINUTE,
        okx_rules: true,
        venue_matches_execution: true,
    });
    assert_eq!(out.pairs[0].median_quote_volume_1m, Some(1.0e5));
}

#[test]
fn g1_a_19_fills_et_un_rejet_de_volume_passe_et_a_18_fills_echoue() {
    let counts = |fills, volume| G1Counts {
        fills,
        rejected_volume: volume,
        rejected_cash: 7,
        rejected_max_positions: 3,
        ..G1Counts::default()
    };
    let pass = executability::g1(counts(19, 1), 300.0, true);
    assert_eq!((pass.ratio, pass.verdict), (Some(0.95), "pass"));
    let fail = executability::g1(counts(18, 2), 300.0, true);
    assert_eq!((fail.ratio, fail.verdict), (Some(0.9), "fail"));
    assert!(pass.at_reference_capital);
}

#[test]
fn le_trou_de_cotation_ecrit_quote_gap_et_gap_forced_exit() {
    // X achète à la minute 100 puis n'a plus de cotation pendant 8 jours ; Y cote tout du long.
    let gap_minutes = 8 * 1440;
    let mut x = flat_series(0, 1000, 100.0, 1.0e6);
    x.extend(
        (1000 + gap_minutes..1100 + gap_minutes).map(|m| flat_bar(T0 + m * MINUTE, 100.0, 1.0e6)),
    );
    let y = flat_series(0, 1100 + gap_minutes, 50.0, 1.0e6);
    let data: Vec<(&str, Vec<Candle>)> = vec![("X-USD", x), ("Y-USD", y)];
    let universe = names(&["X-USD", "Y-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let strategies: Vec<Box<dyn Strategy>> = vec![
        Box::new(
            Scripted::new(vec![(
                T0 + 100 * MINUTE,
                portfolio_common::buy_risk(0.01, 90.0),
            )])
            .0,
        ),
        Box::new(Scripted::new(Vec::new()).0),
    ];
    let result = run(
        &data,
        strategies,
        PortfolioConfig {
            sizing: SizingMode::Compounding,
            ..config(&universe, &instruments, 1000.0)
        },
    );
    let metrics = metrics_of(&result, &data, None, ExchangeId::Kraken);
    let json = json_of(&metrics);
    assert_eq!(json["portfolio"]["quote_gap_exits"], 1);
    assert_eq!(json["funnel"]["exits_by_type"]["gap_forced_exit"], 1);
    assert_eq!(json["funnel"]["exits_by_type"]["trailing_stop"], 0);
    assert_eq!(
        json["sim_by_pair"][0]["detail"]["quote_gap_exits"], 1,
        "{}",
        json["sim_by_pair"]
    );
    let by_exit = json["distribution"]["by_exit_type"].as_array().unwrap();
    assert!(by_exit.iter().any(|e| e["exit_type"] == "gap_forced_exit"));
    // trades.csv : le vocabulaire de S9, avec le symbole de la jambe.
    let csv = trades_csv_legs(
        &result.trades,
        &result.trade_legs,
        &["X-USD", "Y-USD"],
        metrics.span_ms(),
    )
    .unwrap();
    let mut reader = csv::ReaderBuilder::new().from_reader(csv.as_slice());
    let header: Vec<String> = reader
        .headers()
        .unwrap()
        .iter()
        .map(str::to_string)
        .collect();
    assert_eq!(header, TRADES_COLUMNS.map(str::to_string));
    let row = reader.records().next().unwrap().unwrap();
    let col = |name: &str| header.iter().position(|h| h == name).unwrap();
    assert_eq!(&row[col("exit_reason")], "quote_gap");
    assert_eq!(&row[col("exit_type")], "gap_forced_exit");
    assert_eq!(&row[col("symbol")], "X-USD");
    // La sortie forcée est à g0 + 7 jours, et la fenêtre jouée est celle du portefeuille.
    assert_eq!(result.trades[0].exit_ms, T0 + 1000 * MINUTE + 7 * DAY);
    assert_eq!(
        metrics.span_ms(),
        result.window_last_open_ms + MINUTE - result.window_first_open_ms
    );
    let _ = DEFAULT_QUOTE_GAP_MS;
}

#[test]
fn les_statistiques_par_paire_somment_aux_totaux() {
    let (result, data) = rejection_run();
    let json = json_of(&metrics_of(&result, &data, None, ExchangeId::Kraken));
    let by_pair = json["distribution"]["by_pair"].as_array().unwrap();
    assert_eq!(by_pair.len(), 8);
    let trades: u64 = by_pair.iter().map(|p| p["trades"].as_u64().unwrap()).sum();
    assert_eq!(trades, json["n_trades"].as_u64().unwrap());
    assert_eq!(trades, 3);
    let pnl: f64 = by_pair.iter().map(|p| p["net_pnl"].as_f64().unwrap()).sum();
    assert!((pnl - json["pnl_net"].as_f64().unwrap()).abs() < 1e-9);
    // Une paire sans trade n'a ni taux de gain ni espérance.
    let b = by_symbol(&json["distribution"]["by_pair"], "B-USD");
    assert_eq!(b["trades"], 0);
    assert!(b["win_rate"].is_null());
    assert!(b["expectancy_r"].is_null());
    assert_eq!(b["net_pnl"], 0.0);
}

#[test]
fn metrics_s9_porte_toutes_les_cles_du_schema() {
    let (result, data) = rejection_run();
    let json = json_of(&metrics_of(&result, &data, None, ExchangeId::Kraken));
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let expected = [
        "approx",
        "avg_holding_ms",
        "cash_final",
        "config",
        "config_hash",
        "data",
        "dataset_hash",
        "distribution",
        "equity_final",
        "executability",
        "expectancy_net",
        "expectancy_r",
        "exposure",
        "fees_total",
        "fills",
        "forced_close",
        "funnel",
        "funnel_by_pair",
        "g1_executable",
        "gross_loss",
        "gross_profit",
        "instruments",
        "mae_mean",
        "max_drawdown",
        "mfe_mean",
        "min_trades",
        "n_losses",
        "n_trades",
        "n_trades_forced",
        "n_wins",
        "pnl_marked",
        "pnl_net",
        "pnl_realized",
        "portfolio",
        "priority",
        "profit_factor",
        "regime",
        "return_total",
        "schema",
        "signals",
        "signals_rejected",
        "sim",
        "sim_by_pair",
        "sim_deviations",
        "slippage_total",
        "trades_with_r",
        "verdict",
        "win_rate",
    ];
    assert_eq!(keys, expected);
    assert_eq!(json["schema"], "tv4-metrics-s9");
    // Pas de bloc `window` ni de `data.symbol` en S9 : la fenêtre est dans `data`.
    assert!(json.get("window").is_none());
    assert!(json["data"].get("symbol").is_none());
    assert!(json["data"]["window"]["start_ms"].is_i64());
    assert_eq!(json["data"]["symbols"][0], "A-USD");
    assert!(json["data"]["regime_symbol"].is_null());
    assert!(json["regime"].is_null());
    // Le capital est dans `config` mais le hash est celui du moteur, sans capital.
    assert_eq!(json["config"]["initial_cash"], 1000.0);
    assert_eq!(json["config"]["schema"], "tv4-portfolio-config-v1");
    assert_eq!(json["config_hash"], result.config_hash.as_str());
    assert_eq!(json["portfolio"]["initial_cash"], 1000.0);
    assert_eq!(json["priority"]["rank_source"], "dataset");
    assert_eq!(json["priority"]["rank_exchange"], "kraken");
    assert_eq!(
        json["funnel"]["exits_by_type"].as_object().unwrap().len(),
        8
    );
}

// ---------------------------------------------------------------------------------------------
// Équivalence des champs communs avec le chemin de S8 (une paire, mode capitalisé, N = 1)
// ---------------------------------------------------------------------------------------------

fn canonical(value: &Value) -> String {
    hash::canonical_json(value).unwrap()
}

#[test]
fn une_paire_en_mode_capitalise_donne_les_memes_champs_communs_que_s8() {
    let candles = fixture_1m();
    let params = repo_path("config/strategies/sma_cross.toml");
    let text = std::fs::read_to_string(params).unwrap();
    let value = toml::from_str::<toml::Value>(&text).unwrap();
    let strategy = || registry::build("sma_cross", &value).unwrap();

    let cell = CellConfig {
        symbol: "BTC-USD",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: 300.0,
        sim_rules: SimRules::default(),
        instrument: kraken_btc_usd(),
    };
    let s8 = run_cell(&candles, strategy(), &cell).unwrap();
    let open_times: Vec<Ts> = candles[s8.start_index..]
        .iter()
        .map(|c| c.open_time_ms)
        .collect();
    let data8 = DataInfo::dataset(
        DatasetRef {
            name: "fixture".to_string(),
            gaps_policy: "skip".to_string(),
            synthetic_bars: 0,
        },
        ExchangeId::Kraken,
        "BTC-USD",
        Timeframe::M1,
        &candles,
    )
    .unwrap();
    let m8 = serde_json::to_value(
        Metrics::new(&s8, &open_times, data8, "blake3:aa".to_string()).unwrap(),
    )
    .unwrap();

    let universe = vec!["BTC-USD".to_string()];
    let instruments = [kraken_btc_usd()];
    let series = vec![("BTC-USD".to_string(), Columns::from_candles(&candles))];
    let s9 = run_portfolio(PortfolioInputs {
        series: &series,
        strategies: vec![strategy()],
        config: PortfolioConfig {
            universe: &universe,
            regime_symbol: "BTC-USD",
            fees: okx_fees(),
            slippage: slippage(5.0),
            initial_cash: 300.0,
            sim_rules: SimRules::default(),
            instruments: &instruments,
            instruments_hash: None,
            max_positions: 1,
            sizing: SizingMode::Compounding,
            min_order_bump_max_risk: None,
            quote_gap_ms: DEFAULT_QUOTE_GAP_MS,
            min_trades: 30,
        },
    })
    .unwrap();
    let data = vec![("BTC-USD", candles.clone())];
    let m9 = json_of(&metrics_of(&s9, &data, None, ExchangeId::Kraken));

    // Champs repris tels quels de S8 : égaux au bit près (JSON canonique).
    for key in [
        "n_trades",
        "n_trades_forced",
        "forced_close",
        "pnl_net",
        "pnl_realized",
        "pnl_marked",
        "fees_total",
        "slippage_total",
        "equity_final",
        "cash_final",
        "fills",
        "sim",
        "min_trades",
        "verdict",
        "expectancy_net",
        "n_wins",
        "n_losses",
        "win_rate",
        "gross_profit",
        "gross_loss",
        "profit_factor",
        "avg_holding_ms",
        "mfe_mean",
        "mae_mean",
        "expectancy_r",
        "trades_with_r",
        "return_total",
        "max_drawdown",
        "exposure",
        "approx",
    ] {
        assert_eq!(canonical(&m8[key]), canonical(&m9[key]), "champ `{key}`");
    }
    // Distribution : la base est celle de S8, `by_pair` s'y ajoute.
    let mut d9 = m9["distribution"].clone();
    let by_pair = d9.as_object_mut().unwrap().remove("by_pair").unwrap();
    assert_eq!(canonical(&m8["distribution"]), canonical(&d9));
    assert_eq!(by_pair.as_array().unwrap().len(), 1);
    assert_eq!(by_pair[0]["trades"], m8["n_trades"]);
    // Entonnoir : S9 ajoute `entry_rejected_max_positions` et la case `gap_forced_exit` (nulles ici).
    let mut f9 = m9["funnel"].clone();
    let extra = f9
        .as_object_mut()
        .unwrap()
        .remove("entry_rejected_max_positions")
        .unwrap();
    assert_eq!(extra, 0);
    let exits = f9["exits_by_type"].as_object_mut().unwrap();
    assert_eq!(exits.remove("gap_forced_exit").unwrap(), 0);
    assert_eq!(canonical(&m8["funnel"]), canonical(&f9));
    // Signaux : mêmes totaux et mêmes motifs.
    assert_eq!(m8["signals"]["emitted"], m9["signals"]["emitted"]);
    assert_eq!(m8["signals"]["accepted"], m9["signals"]["accepted"]);
    assert_eq!(m8["signals"]["rejected"], m9["signals"]["rejected"]);
    assert_eq!(
        canonical(&m8["signals"]["rejected_by_reason"]),
        canonical(&m9["signals"]["rejected_by_reason"])
    );
    // L'unique entrée de `funnel_by_pair` est le funnel entier.
    let pair = &m9["funnel_by_pair"][0];
    for key in [
        "evaluations",
        "entry_signals",
        "entry_orders",
        "entry_fills",
        "trades",
    ] {
        assert_eq!(pair[key], m9["funnel"][key], "{key}");
    }
}
