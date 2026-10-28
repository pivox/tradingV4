//! Équivalence mono-jambe avec S8 (docs S9 §2.3) : un portefeuille à une jambe, en mode capitalisé, avec
//! `max_positions = 1` et sans régime, produit les mêmes trades, la même equity et les mêmes compteurs
//! que le chemin `run_cell`, à l'octet, sur la fixture réelle de S8 et avec les paramètres des deux
//! golden runs. `trades.csv` et `equity.csv` sont aussi comparés aux fichiers versionnés du golden.

mod common;
mod portfolio_common;

use std::path::PathBuf;

use common::{fixture_1m, kraken_btc_usd, okx_fees, repo_path, slippage};
use tv4::engine::{
    CellConfig, DEFAULT_QUOTE_GAP_MS, PortfolioConfig, PortfolioInputs, run_cell, run_portfolio,
};
use tv4::model::{Candle, Timeframe};
use tv4::report::equity::equity_csv;
use tv4::report::writers::{TradesContext, trades_csv, trades_csv_legs};
use tv4::sim::{SimRules, SizingMode};
use tv4::store::Columns;
use tv4::strategy::{Strategy, registry};

fn golden(name: &str, file: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden_runs")
        .join(name)
        .join(file);
    std::fs::read(path).expect("fichier golden")
}

fn strategy_from(path: &std::path::Path) -> Box<dyn Strategy> {
    let text = std::fs::read_to_string(path).expect("paramètres lisibles");
    let value = toml::from_str::<toml::Value>(&text).expect("TOML valide");
    registry::build("sma_cross", &value).expect("stratégie valide")
}

struct Both {
    s8: tv4::engine::CellResult,
    s9: tv4::engine::PortfolioResult,
}

fn run_both(params: &std::path::Path, candles: &[Candle]) -> Both {
    let cell = CellConfig {
        symbol: "BTC-USD",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: 300.0,
        sim_rules: SimRules::default(),
        instrument: kraken_btc_usd(),
    };
    let s8 = run_cell(candles, strategy_from(params), &cell).expect("run S8");

    let universe = vec!["BTC-USD".to_string()];
    let instruments = [kraken_btc_usd()];
    let config = PortfolioConfig {
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
    };
    let series = vec![("BTC-USD".to_string(), Columns::from_candles(candles))];
    let s9 = run_portfolio(PortfolioInputs {
        series: &series,
        strategies: vec![strategy_from(params)],
        config,
    })
    .expect("run S9");
    Both { s8, s9 }
}

fn bits(values: &[f64]) -> Vec<u64> {
    values.iter().map(|v| v.to_bits()).collect()
}

fn assert_equivalent(both: &Both) {
    let (a, b) = (&both.s8, &both.s9);
    assert_eq!(a.trades, b.trades, "trades");
    assert_eq!(b.trade_legs, vec![0u16; b.trades.len()]);
    assert_eq!(bits(&a.equity), bits(&b.equity), "equity à chaque minute");
    assert_eq!(a.bars_in_window, b.grid_minutes);
    assert_eq!(a.window_first_open_ms, b.window_first_open_ms);
    assert_eq!(a.window_last_open_ms, b.window_last_open_ms);
    assert_eq!(a.cash_final.to_bits(), b.cash_final.to_bits());
    assert_eq!(a.equity_final.to_bits(), b.equity_final.to_bits());
    assert_eq!(a.pnl_net.to_bits(), b.pnl_net.to_bits());
    assert_eq!(a.pnl_realized.to_bits(), b.pnl_realized.to_bits());
    assert_eq!(a.pnl_marked.to_bits(), b.pnl_marked.to_bits());
    assert_eq!(a.fees_total.to_bits(), b.fees_total.to_bits());
    assert_eq!(a.slippage_total.to_bits(), b.slippage_total.to_bits());
    assert_eq!(a.forced_close, b.forced_close);
    assert_eq!(a.n_trades_forced, b.n_trades_forced);
    assert_eq!(a.signals_emitted, b.signals_emitted);
    assert_eq!(a.signals_accepted, b.signals_accepted);
    assert_eq!(a.rejections, b.rejections);
    assert_eq!(a.sim, b.sim, "compteurs du simulateur");
    assert_eq!(a.funnel, b.funnel, "entonnoir");
    assert_eq!(a.bars_in_position, b.bars_in_position);
    assert_eq!(b.max_positions_rejected, 0);
    assert_eq!(b.quote_gap_exits, 0);
    assert!(b.max_open_positions <= 1);
}

#[test]
fn le_run_de_fumee_est_identique_a_s8() {
    let candles = fixture_1m();
    let params = repo_path("config/strategies/sma_cross.toml");
    let both = run_both(&params, &candles);
    assert_equivalent(&both);
    assert!(!both.s9.trades.is_empty(), "le chemin des fills est exercé");

    // Octets : trades.csv et equity.csv sont ceux du chemin S8 ET ceux du golden versionné.
    let span = both.s9.window_last_open_ms + 60_000 - both.s9.window_first_open_ms;
    let context = TradesContext {
        symbol: "BTC-USD",
        span_ms: span,
    };
    let s8_csv = trades_csv(&both.s8.trades, &context).unwrap();
    let s9_csv = trades_csv_legs(&both.s9.trades, &both.s9.trade_legs, &["BTC-USD"], span).unwrap();
    assert_eq!(s8_csv, s9_csv);
    assert_eq!(s9_csv, golden("sma_cross_15m_okx", "trades.csv"));
    let equity = equity_csv(&both.s9.grid_open_ms, &both.s9.equity, 300.0, Timeframe::H1).unwrap();
    assert_eq!(equity, golden("sma_cross_15m_okx", "equity.csv"));
}

#[test]
fn le_run_court_a_49_trades_est_identique_a_s8() {
    let candles = fixture_1m();
    let params = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden_runs/sma_cross_1m_court_okx/params.toml");
    let both = run_both(&params, &candles);
    assert_equivalent(&both);
    assert_eq!(both.s9.trades.len(), 49);
    let span = both.s9.window_last_open_ms + 60_000 - both.s9.window_first_open_ms;
    let s9_csv = trades_csv_legs(&both.s9.trades, &both.s9.trade_legs, &["BTC-USD"], span).unwrap();
    assert_eq!(s9_csv, golden("sma_cross_1m_court_okx", "trades.csv"));
    let equity = equity_csv(&both.s9.grid_open_ms, &both.s9.equity, 300.0, Timeframe::H1).unwrap();
    assert_eq!(equity, golden("sma_cross_1m_court_okx", "equity.csv"));
}

#[test]
fn la_config_hash_du_portefeuille_n_est_pas_celui_de_s8() {
    let candles = fixture_1m();
    let params = repo_path("config/strategies/sma_cross.toml");
    let both = run_both(&params, &candles);
    assert_ne!(both.s8.config_hash, both.s9.config_hash);
    assert_eq!(both.s9.config.schema, "tv4-portfolio-config-v1");
}
