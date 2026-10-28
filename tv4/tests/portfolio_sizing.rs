//! Sizing du portefeuille (docs S9 §7) : mode recherche (base = capital initial, plafond `capital / N`),
//! mode capitalisé (base = equity du portefeuille au signal), capital hors du `config_hash` et règle
//! `min_order_bump` du palier 20 %.

mod common;
mod portfolio_common;

use portfolio_common::{
    MINUTE, Scripted, T0, buy_risk, config, flat_series, instrument, names, run, sell_all, try_run,
};
use tv4::engine::{EngineError, PortfolioConfig, PortfolioResult};
use tv4::model::{Candle, InstrumentRules};
use tv4::report::writers::{TRADES_COLUMNS, trades_csv_legs};
use tv4::sim::SizingMode;
use tv4::strategy::{Strategy, registry};

const UNIVERSE: [&str; 1] = ["BTC-USD"];

fn sma_cross_fast() -> Box<dyn Strategy> {
    let text = "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0";
    let value = toml::from_str::<toml::Value>(text).expect("TOML valide");
    registry::build("sma_cross", &value).expect("stratégie valide")
}

/// `sma_cross` 5/20 en 1m sur une sinusoïde de 3 % (période de 240 minutes) : une trentaine de trades.
fn sinus_run(initial_cash: f64, sizing: SizingMode, max_positions: u16) -> PortfolioResult {
    let universe = names(&UNIVERSE);
    let instruments = vec![instrument(0.0)];
    let candles = common::sinus::sinusoide(7_200, 0.03);
    run(
        &[("BTC-USD", candles)],
        vec![sma_cross_fast()],
        PortfolioConfig {
            sizing,
            max_positions,
            ..config(&universe, &instruments, initial_cash)
        },
    )
}

fn csv_rows(result: &PortfolioResult) -> Vec<Vec<String>> {
    let bytes = trades_csv_legs(
        &result.trades,
        &result.trade_legs,
        &["BTC-USD"],
        result.window_last_open_ms + MINUTE - result.window_first_open_ms,
    )
    .unwrap();
    let mut reader = csv::ReaderBuilder::new().from_reader(bytes.as_slice());
    reader
        .records()
        .map(|record| record.unwrap().iter().map(str::to_string).collect())
        .collect()
}

/// Colonnes qui suivent le capital : la quantité et tout ce qui en est le produit.
const SCALED: [&str; 11] = [
    "qty",
    "pnl_net",
    "entry_fee",
    "exit_fee",
    "entry_slippage",
    "exit_slippage",
    "mfe",
    "mae",
    "capped_by_cash",
    "risk_quote",
    "r_multiple",
];

#[test]
fn le_mode_recherche_donne_les_memes_trades_a_300_et_a_3000() {
    let small = sinus_run(300.0, SizingMode::Research, 8);
    let large = sinus_run(3000.0, SizingMode::Research, 8);
    // Même hash : le capital n'y est pas.
    assert_eq!(small.config_hash, large.config_hash);
    assert_eq!(small.initial_cash, 300.0);
    assert_eq!(large.initial_cash, 3000.0);
    assert!(
        small.trades.len() >= 10,
        "assez de trades pour comparer : {}",
        small.trades.len()
    );
    assert_eq!(small.trades.len(), large.trades.len());

    let (a, b) = (csv_rows(&small), csv_rows(&large));
    for (row_a, row_b) in a.iter().zip(&b) {
        for (index, name) in TRADES_COLUMNS.iter().enumerate() {
            if SCALED.contains(name) {
                continue;
            }
            assert_eq!(row_a[index], row_b[index], "colonne `{name}`");
        }
    }
    // Les colonnes qui suivent le capital lui sont proportionnelles (au pas de lot près).
    let column = |name: &str| TRADES_COLUMNS.iter().position(|c| *c == name).unwrap();
    for (row_a, row_b) in a.iter().zip(&b) {
        let qty_a: f64 = row_a[column("qty")].parse().unwrap();
        let qty_b: f64 = row_b[column("qty")].parse().unwrap();
        assert!(
            (qty_b / qty_a - 10.0).abs() < 1e-5,
            "quantités {qty_a} et {qty_b}"
        );
        // Le multiple de risque (frais compris) est celui du trade, quel que soit le capital.
        let r_a = &row_a[column("r_multiple")];
        let r_b = &row_b[column("r_multiple")];
        if r_a.is_empty() {
            assert!(r_b.is_empty());
        } else {
            let (r_a, r_b): (f64, f64) = (r_a.parse().unwrap(), r_b.parse().unwrap());
            assert!(
                (r_a - r_b).abs() <= 1e-5 * r_a.abs().max(1.0),
                "R {r_a} {r_b}"
            );
        }
    }
    // Plafond `capital / N` : chaque entrée vaut au plus 1/8 du capital, et le plafond a mordu.
    for result in [&small, &large] {
        for trade in &result.trades {
            assert!(
                trade.qty * trade.entry_price <= result.initial_cash / 8.0 * (1.0 + 1e-9),
                "notional {} au-dessus du plafond",
                trade.qty * trade.entry_price
            );
        }
        assert!(result.detail.entries_capped_by_notional > 0);
        assert!(result.max_cost_to_initial <= 1.0 / 8.0 + 1e-9);
    }
}

#[test]
fn un_minimum_d_ordre_eleve_rejette_a_300_et_pas_a_3000() {
    // Prix 100, stop 98 : le risque de 1 % donne 1,5 unité à 300 et 15 à 3000. Un minimum de 2 unités
    // (N = 1, donc aucun plafond de notional avant le minimum) rejette l'entrée à 300 seulement.
    let universe = names(&UNIVERSE);
    let instruments = vec![instrument(2.0)];
    let play = |cash: f64| {
        run(
            &[("BTC-USD", flat_series(0, 600, 100.0, 1.0e6))],
            vec![Box::new(
                Scripted::new(vec![(T0 + 100 * MINUTE, buy_risk(0.01, 98.0))]).0,
            )],
            PortfolioConfig {
                max_positions: 1,
                ..config(&universe, &instruments, cash)
            },
        )
    };
    let small = play(300.0);
    let large = play(3000.0);
    assert_eq!(small.config_hash, large.config_hash);
    assert!(small.trades.is_empty(), "{:?}", small.trades);
    assert_eq!(small.legs[0].counters.buys_rejected_below_minimum, 1);
    assert_eq!(small.legs[0].detail.below_min_min_qty, 1);
    assert_eq!(large.trades.len(), 1);
    assert_eq!(large.legs[0].counters.buys_rejected_below_minimum, 0);
    // Le trade accepté à 3000 a la quantité du risque (15 unités).
    assert!((large.trades[0].qty - 15.0).abs() < 1e-6);
    // La fraction demandée est la même : le sizing ne dépend pas du capital.
    assert_eq!(small.legs[0].entry_fractions, large.legs[0].entry_fractions);
    assert_eq!(small.legs[0].entry_fractions.len(), 1);
    assert!((small.legs[0].entry_fractions[0] - 0.5).abs() < 1e-12);
}

/// Un achat à 100 (stop `100 - distance`), sa vente après un saut à 110, puis un second achat à 110.
fn double_trade(sizing: SizingMode, distance: f64) -> PortfolioResult {
    let mut candles: Vec<Candle> = flat_series(0, 200, 100.0, 1.0e6);
    candles.extend(flat_series(200, 400, 110.0, 1.0e6));
    let universe = names(&UNIVERSE);
    let instruments = vec![instrument(0.0)];
    let strategy = Scripted::new(vec![
        (T0 + 100 * MINUTE, buy_risk(0.01, 100.0 - distance)),
        (T0 + 250 * MINUTE, sell_all()),
        (T0 + 400 * MINUTE, buy_risk(0.01, 110.0 - distance)),
    ])
    .0;
    run(
        &[("BTC-USD", candles)],
        vec![Box::new(strategy)],
        PortfolioConfig {
            sizing,
            max_positions: 2,
            ..config(&universe, &instruments, 300.0)
        },
    )
}

#[test]
fn le_mode_capitalise_risque_un_pour_cent_de_l_equity_courante() {
    // Distance de stop 4 : en recherche, 1 % de 300 / 4 = 0,75 unité à chaque trade (notional 75 < 150).
    let research = double_trade(SizingMode::Research, 4.0);
    assert_eq!(research.trades.len(), 2);
    assert!((research.trades[0].qty - 0.75).abs() < 1e-6);
    assert!((research.trades[1].qty - 0.75).abs() < 1e-6);

    // En capitalisé, le second trade risque 1 % de l'equity au moment du signal (après le gain du
    // premier), pas 1 % de 300.
    let compounding = double_trade(SizingMode::Compounding, 4.0);
    assert_eq!(compounding.trades.len(), 2);
    assert!((compounding.trades[0].qty - 0.75).abs() < 1e-6);
    // Equity au signal : la minute du second signal est la 399 (now_ms = minute 400).
    let signal_index = compounding
        .grid_open_ms
        .iter()
        .position(|t| *t == T0 + 399 * MINUTE)
        .unwrap();
    let equity_at_signal = compounding.equity[signal_index];
    assert!(equity_at_signal > 300.0, "le premier trade a gagné");
    let expected = equity_at_signal * 0.01 / 4.0;
    assert!(
        (compounding.trades[1].qty - expected).abs() < 1e-6,
        "quantité {} attendue {expected}",
        compounding.trades[1].qty
    );
    assert!(compounding.trades[1].qty > research.trades[1].qty);

    // Les deux modes ont deux hashes distincts.
    assert_ne!(research.config_hash, compounding.config_hash);
}

#[test]
fn le_plafond_du_mode_recherche_est_capital_sur_n() {
    // Un risque de 50 % avec un stop très proche réclamerait toute la caisse : le plafond la limite à
    // capital / N = 250.
    let universe = names(&UNIVERSE);
    let instruments = vec![instrument(0.0)];
    let result = run(
        &[("BTC-USD", flat_series(0, 600, 100.0, 1.0e6))],
        vec![Box::new(
            Scripted::new(vec![(T0 + 100 * MINUTE, buy_risk(0.5, 99.0))]).0,
        )],
        PortfolioConfig {
            max_positions: 4,
            ..config(&universe, &instruments, 1000.0)
        },
    );
    assert_eq!(result.trades.len(), 1);
    let notional = result.trades[0].qty * result.trades[0].entry_price;
    assert!(notional <= 250.0 * (1.0 + 1e-9), "{notional}");
    assert!(notional > 249.0, "{notional}");
    assert_eq!(result.legs[0].detail.entries_capped_by_notional, 1);
    // En capitalisé, le même signal n'est pas plafonné par N (seulement par le cash, marge de 5 %).
    let uncapped = run(
        &[("BTC-USD", flat_series(0, 600, 100.0, 1.0e6))],
        vec![Box::new(
            Scripted::new(vec![(T0 + 100 * MINUTE, buy_risk(0.5, 99.0))]).0,
        )],
        PortfolioConfig {
            sizing: SizingMode::Compounding,
            max_positions: 4,
            ..config(&universe, &instruments, 1000.0)
        },
    );
    let notional = uncapped.trades[0].qty * uncapped.trades[0].entry_price;
    assert!(notional > 900.0, "{notional}");
    assert_eq!(uncapped.legs[0].detail.entries_capped_by_notional, 0);
}

#[test]
fn la_ligne_portee_au_minimum_d_ordre_du_palier_20_pour_cent() {
    // 60 € : 1 % de risque = 0,6, distance 2 → 0,3 unité, sous le minimum de 0,5. Porté au minimum, le
    // risque effectif est 0,5 × 2 = 1 = 1,67 % du capital : au-dessus de 1,5 %, refusé ; à 2 %, accepté.
    let universe = names(&UNIVERSE);
    let instruments = vec![instrument(0.5)];
    let play = |bump: Option<f64>| {
        run(
            &[("BTC-USD", flat_series(0, 600, 100.0, 1.0e6))],
            vec![Box::new(
                Scripted::new(vec![(T0 + 100 * MINUTE, buy_risk(0.01, 98.0))]).0,
            )],
            PortfolioConfig {
                sizing: SizingMode::Compounding,
                min_order_bump_max_risk: bump,
                ..config(&universe, &instruments, 60.0)
            },
        )
    };
    let none = play(None);
    assert!(none.trades.is_empty());
    assert_eq!(none.legs[0].detail.below_min_min_qty, 1);
    let refused = play(Some(0.015));
    assert!(refused.trades.is_empty());
    let bumped = play(Some(0.02));
    assert_eq!(bumped.trades.len(), 1);
    assert!((bumped.trades[0].qty - 0.5).abs() < 1e-9);
    assert_eq!(bumped.legs[0].detail.entries_bumped_to_min, 1);
    // La règle est dans le hash.
    assert_ne!(none.config_hash, bumped.config_hash);
    assert_ne!(refused.config_hash, bumped.config_hash);
}

#[test]
fn le_minimum_d_ordre_n_existe_qu_en_mode_capitalise() {
    let universe = names(&UNIVERSE);
    let instruments: Vec<InstrumentRules> = vec![instrument(0.5)];
    let error = try_run(
        &[("BTC-USD", flat_series(0, 100, 100.0, 1.0e6))],
        vec![Box::new(Scripted::new(Vec::new()).0)],
        PortfolioConfig {
            sizing: SizingMode::Research,
            min_order_bump_max_risk: Some(0.015),
            ..config(&universe, &instruments, 60.0)
        },
    )
    .unwrap_err();
    assert!(matches!(error, EngineError::Sim(_)), "{error}");
}

#[test]
fn la_base_de_sizing_du_mode_recherche_reste_le_capital_apres_un_gain() {
    // Contrôle croisé de l'invariant 7.4 : même après un gain, la base d'une entrée de recherche est le
    // capital initial. Le test ci-dessus le montre par la quantité ; ici par la fraction demandée.
    let research = double_trade(SizingMode::Research, 4.0);
    let fractions = &research.legs[0].entry_fractions;
    assert_eq!(fractions.len(), 2);
    // Fraction = qty × prix / base : 0,75 × 100 / 300 puis 0,75 × 110 / 300.
    assert!((fractions[0] - 0.25).abs() < 1e-12);
    assert!((fractions[1] - 0.275).abs() < 1e-12);
    let compounding = double_trade(SizingMode::Compounding, 4.0);
    let c = &compounding.legs[0].entry_fractions;
    // En capitalisé, la base est l'equity (plus grande après le gain) : la fraction du second trade est
    // la même quantité de risque rapportée à une base plus haute, soit 0,01 × 110 / 4 = 0,275 aussi
    // (le risque est proportionnel à la base).
    assert!((c[1] - 0.275).abs() < 1e-12);
}
