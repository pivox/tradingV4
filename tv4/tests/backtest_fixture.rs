//! Backtest S3 sur la fixture réelle `fixtures/kraken_xbtusd_1m.csv` (XBTUSD 1m, 10 000 bougies).
//!
//! Les valeurs de contrôle viennent d'un oracle Python indépendant (bibliothèque standard seulement,
//! mêmes formules et même ordre d'opérations que `sim` et `engine`). Si le code diverge, on
//! n'ajuste pas le test : c'est la spécification ou le code qui est faux.

mod common;

use common::{fixture_1m, okx_fees, repo_path, slippage, sma_cross_1m};
use tv4::engine::{CellConfig, CellResult, run_cell};
use tv4::model::hash;
use tv4::report::{DataInfo, EngineInfo, Metrics};
use tv4::sim::{ExitReason, SimRules};

const CAPITAL: f64 = 300.0;

fn run() -> (Vec<tv4::model::Candle>, CellResult) {
    let candles = fixture_1m();
    let config = CellConfig {
        symbol: "BTC-USD",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: CAPITAL,
        sim_rules: SimRules::default(),
    };
    let result = run_cell(&candles, sma_cross_1m(), &config).expect("backtest valide");
    (candles, result)
}

fn assert_rel(label: &str, obtenu: f64, attendu: f64, tolerance: f64) {
    let ecart = (obtenu - attendu).abs();
    assert!(
        ecart <= tolerance * attendu.abs(),
        "{label} : {obtenu} != {attendu} (écart {ecart})"
    );
}

#[test]
fn le_backtest_tourne_sur_la_fixture() {
    let (_, result) = run();
    assert!(!result.trades.is_empty());
    assert!(result.cash_final >= 0.0);
    assert_eq!(result.qty_final, 0.0);
    assert!(result.equity.iter().all(|e| e.is_finite() && *e > 0.0));
}

#[test]
fn les_trades_sont_coherents_a_la_main() {
    let (candles, result) = run();
    let taker = 0.0035;
    // Cash avant chaque entrée, chaîné à partir du capital par les PnL nets des trades.
    let mut cash = CAPITAL;
    for (k, t) in result.trades.iter().enumerate() {
        let entree = candles
            .binary_search_by_key(&t.entry_ms, |c| c.open_time_ms)
            .unwrap_or_else(|_| panic!("trade {k} : bougie d'entrée introuvable"));
        let sortie = candles
            .binary_search_by_key(&t.exit_ms, |c| c.open_time_ms)
            .unwrap_or_else(|_| panic!("trade {k} : bougie de sortie introuvable"));
        assert_eq!(t.exit_reason, ExitReason::Signal);
        assert_eq!(t.entry_ref_price, candles[entree].open, "trade {k}");
        assert_eq!(t.entry_price, candles[entree].open * (1.0 + 0.0005));
        assert_eq!(t.exit_ref_price, candles[sortie].open, "trade {k}");
        assert_eq!(t.exit_price, candles[sortie].open * (1.0 - 0.0005));
        assert!(t.entry_ms >= t.entry_signal_ms, "trade {k}");
        assert!(t.exit_ms >= t.exit_signal_ms, "trade {k}");
        assert!(t.exit_ms > t.entry_ms, "trade {k}");
        // Le signal vient de la bougie précédente de la série (politique `skip` : un trou peut
        // séparer les deux, le fill a alors lieu à l'open de la bougie suivante présente).
        assert_eq!(
            candles[entree - 1].open_time_ms + 60_000,
            t.entry_signal_ms,
            "trade {k}"
        );
        assert_eq!(
            candles[sortie - 1].open_time_ms + 60_000,
            t.exit_signal_ms,
            "trade {k}"
        );

        // Frais : même expression que `sim`, donc égalité au bit près.
        assert_eq!(t.entry_fee, t.qty * t.entry_price * taker, "trade {k}");
        assert_eq!(t.exit_fee, t.qty * t.exit_price * taker, "trade {k}");

        // Plafond de cash : toutes les entrées de la fixture sont plafonnées (qty = cap).
        assert!(t.capped_by_cash, "trade {k}");
        let plafond = cash * (1.0 - 0.05) / (t.entry_price * (1.0 + taker));
        assert!(
            (t.qty - plafond).abs() <= 1e-12 * plafond,
            "trade {k} : qty {} != plafond {plafond}",
            t.qty
        );
        cash += t.pnl_net;

        // PnL recalculé depuis les champs, tolérance relative au notionnel.
        let cout = t.qty * t.entry_price;
        let produit = t.qty * t.exit_price;
        let pnl = (produit - produit * taker) - (cout + cout * taker);
        assert!(
            (pnl - t.pnl_net).abs() <= 1e-12 * cout,
            "trade {k} : pnl {pnl} != {}",
            t.pnl_net
        );
    }
    let ecart = (result.pnl_net - (result.pnl_realized + result.pnl_marked)).abs();
    assert!(ecart <= 1e-9 * CAPITAL, "écart de PnL {ecart}");
}

#[test]
fn valeurs_de_controle_de_l_oracle() {
    let (candles, result) = run();
    assert_eq!(result.start_index, 240);
    assert_eq!(result.bars_in_window, 9_760);
    assert_eq!(result.trades.len(), 23);
    assert!(!result.forced_close);
    assert_eq!(result.signals_emitted, 46);
    assert_eq!(result.signals_accepted, 46);
    assert_eq!(result.rejections.total(), 0);
    assert_eq!(result.sim.entries_capped_by_cash, 23);
    assert_eq!(result.sim.stop_breaches_ignored, 20);

    assert_rel("pnl_net", result.pnl_net, -15.408523707748486, 1e-9);
    assert_rel("fees_total", result.fees_total, 44.5242015770599, 1e-9);
    assert_rel(
        "slippage_total",
        result.slippage_total,
        6.360609094365312,
        1e-9,
    );
    assert_rel("cash_final", result.cash_final, 284.5914762922515, 1e-9);

    // Premier trade, vérifié à la main sur les lignes du CSV.
    let t = result.trades[0];
    assert_eq!(t.entry_signal_ms, 1_577_878_920_000);
    assert_eq!(t.entry_ms, 1_577_878_920_000);
    let signal = candles
        .binary_search_by_key(&(t.entry_ms - 60_000), |c| c.open_time_ms)
        .unwrap();
    assert_eq!(candles[signal].close, 7203.3, "close de la bougie 11:41");
    assert_eq!(candles[signal + 1].open, 7203.4, "open de la bougie 11:42");
    assert_ne!(t.entry_ref_price, candles[signal].close);
    assert_rel("entry_price", t.entry_price, 7207.001699999999, 1e-12);
    assert_rel("qty", t.qty, 0.039406953251203435, 1e-12);
    assert_eq!(t.exit_ms, 1_577_906_640_000);
    assert_eq!(t.exit_ref_price, 7210.0);
    assert_rel("exit_price", t.exit_price, 7206.395, 1e-12);
    assert_rel("entry_fee", t.entry_fee, 0.9940209267563527, 1e-12);
    assert_rel("exit_fee", t.exit_fee, 0.9939372480614718, 1e-12);
    assert_rel("pnl_net du trade 1", t.pnl_net, -2.0118663733552467, 1e-12);
}

fn metrics_of(result: &CellResult, candles: &[tv4::model::Candle]) -> Metrics {
    let data = DataInfo::fixture("0".repeat(64), "BTC-USD", candles).unwrap();
    let engine = EngineInfo {
        tv4_version: env!("CARGO_PKG_VERSION"),
        git_sha: tv4::GIT_SHA,
    };
    Metrics::new(result, data, engine)
}

#[test]
fn double_run_identique_a_l_octet() {
    let (candles_a, a) = run();
    let (candles_b, b) = run();
    assert_eq!(a.trades, b.trades);
    assert_eq!(a.equity.len(), b.equity.len());
    for (x, y) in a.equity.iter().zip(&b.equity) {
        assert_eq!(x.to_bits(), y.to_bits());
    }
    let ma = metrics_of(&a, &candles_a);
    let mb = metrics_of(&b, &candles_b);
    assert_eq!(
        hash::canonical_json(&ma).unwrap(),
        hash::canonical_json(&mb).unwrap()
    );
    assert_eq!(hash::hash_json(&ma.config).unwrap(), ma.config_hash);
}

#[test]
fn la_fixture_existe_dans_le_depot() {
    assert!(repo_path("fixtures/kraken_xbtusd_1m.csv").is_file());
}
