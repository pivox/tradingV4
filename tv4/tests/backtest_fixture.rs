//! Backtest S3 sur la fixture réelle `fixtures/kraken_xbtusd_1m.csv` (XBTUSD 1m, 10 000 bougies).
//!
//! Les valeurs de contrôle viennent d'un oracle Python indépendant (bibliothèque standard seulement,
//! mêmes formules et même ordre d'opérations que `sim` et `engine`). Si le code diverge, on
//! n'ajuste pas le test : c'est la spécification ou le code qui est faux.
//!
//! S5 : le test `sma_cross_15m_valeurs_de_l_oracle` épingle de la même façon `sma_cross` 15m (20/50)
//! sur la fixture, avec les valeurs de l'oracle Python indépendant (`s5-oracle/oracle.json`, point 4),
//! qui concordent au bit avec le moteur, trade par trade. Même règle : on n'ajuste pas le test.

mod common;

use common::{fixture_1m, okx_fees, repo_path, slippage, sma_cross_1m, strategy_from_file};
use tv4::engine::{CellConfig, CellResult, run_cell};
use tv4::model::hash;
use tv4::report::{DataInfo, EngineInfo, Metrics};
use tv4::sim::{ExitReason, SimRules};

const CAPITAL: f64 = 300.0;

fn run() -> (Vec<tv4::model::Candle>, CellResult) {
    run_avec(sma_cross_1m())
}

fn run_avec(strategie: Box<dyn tv4::strategy::Strategy>) -> (Vec<tv4::model::Candle>, CellResult) {
    let candles = fixture_1m();
    let config = CellConfig {
        symbol: "BTC-USD",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: CAPITAL,
        sim_rules: SimRules::default(),
    };
    let result = run_cell(&candles, strategie, &config).expect("backtest valide");
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

/// Un trade de l'oracle : `(sig_in, t_in, sig_out, t_out, px_in, px_out, qty, pnl)`.
type TradeOracle = (i64, i64, i64, i64, f64, f64, f64, f64);

/// Les dix trades de `sma_cross` 15m (20/50, `stop_atr_mult` 2,0) sur la fixture, issus de l'oracle
/// Python indépendant. Au quatrième, le signal de sortie (`1578145500000`) tombe sur une minute absente
/// de la fixture : le fill a lieu à l'open de la bougie présente suivante (`1578145560000`).
const TRADES_15M_ORACLE: [TradeOracle; 10] = [
    (
        1577889900000,
        1577889900000,
        1577920500000,
        1577920500000,
        7207.602,
        7159.0187000000005,
        0.039403671161815486,
        -3.8957019693484654,
    ),
    (
        1577970060000,
        1577970060000,
        1577983500000,
        1577983500000,
        7136.766599999999,
        7091.052700000001,
        0.03927800724676949,
        -3.7514922589769526,
    ),
    (
        1578027600000,
        1578027600000,
        1578091500000,
        1578091500000,
        7200.1983,
        7265.5654,
        0.03297637547326886,
        0.4859704397229905,
    ),
    (
        1578118500000,
        1578118500000,
        1578145500000,
        1578145560000,
        7347.0716999999995,
        7285.7553,
        0.037732930317679464,
        -4.246135493926772,
    ),
    (
        1578174300000,
        1578174300000,
        1578226500000,
        1578226500000,
        7349.97315,
        7435.98015,
        0.03717112693592997,
        1.273340199937877,
    ),
    (
        1578242760000,
        1578242760000,
        1578260700000,
        1578260700000,
        7466.131199999999,
        7375.81025,
        0.03675427496303788,
        -5.228947820857911,
    ),
    (
        1578284100000,
        1578284100000,
        1578339000000,
        1578339000000,
        7535.766,
        7525.93515,
        0.03575775365330662,
        -2.2365332101275612,
    ),
    (
        1578346200000,
        1578346200000,
        1578400200000,
        1578400200000,
        7583.79,
        7863.166450000001,
        0.035252132272042096,
        7.9427320371779615,
    ),
    (
        1578421800000,
        1578421800000,
        1578484800000,
        1578484800000,
        8051.923949999999,
        8335.6301,
        0.032511383438663014,
        7.358942239862074,
    ),
    (
        1578492000000,
        1578492000000,
        1578499200000,
        1578499200000,
        8391.29355,
        8297.74905,
        0.03358608019996617,
        -5.103611410550684,
    ),
];

fn strategie_15m() -> Box<dyn tv4::strategy::Strategy> {
    strategy_from_file("sma_cross", "config/strategies/sma_cross.toml")
}

#[test]
fn sma_cross_15m_valeurs_de_l_oracle() {
    let (candles, result) = run_avec(strategie_15m());
    // 50 bougies 15m de warmup : la 1m 482 est la première jouée (la fixture a des minutes absentes).
    assert_eq!(result.start_index, 482);
    assert_eq!(result.bars_in_window, 9_518);
    assert_eq!(result.trades.len(), 10);
    assert!(!result.forced_close);
    assert_eq!(result.signals_emitted, 20);
    assert_eq!(result.signals_accepted, 20);
    assert_eq!(result.rejections.total(), 0);

    assert_rel("pnl_net", result.pnl_net, -7.401437247087472, 1e-9);
    assert_rel("cash_final", result.cash_final, 292.5985627529125, 1e-9);
    assert_rel("fees_total", result.fees_total, 18.9897174461591, 1e-9);
    assert_rel(
        "slippage_total",
        result.slippage_total,
        2.71282035329757,
        1e-9,
    );

    // Trade par trade, au bit près : mêmes expressions que `sim`, dans le même ordre.
    for (k, (t, o)) in result.trades.iter().zip(TRADES_15M_ORACLE).enumerate() {
        let (sig_in, t_in, sig_out, t_out, px_in, px_out, qty, pnl) = o;
        assert_eq!(t.entry_signal_ms, sig_in, "trade {k} : signal d'entrée");
        assert_eq!(t.entry_ms, t_in, "trade {k} : fill d'entrée");
        assert_eq!(t.exit_signal_ms, sig_out, "trade {k} : signal de sortie");
        assert_eq!(t.exit_ms, t_out, "trade {k} : fill de sortie");
        assert_eq!(
            t.entry_price.to_bits(),
            px_in.to_bits(),
            "trade {k} : px_in"
        );
        assert_eq!(
            t.exit_price.to_bits(),
            px_out.to_bits(),
            "trade {k} : px_out"
        );
        assert_eq!(t.qty.to_bits(), qty.to_bits(), "trade {k} : qty");
        assert_eq!(t.pnl_net.to_bits(), pnl.to_bits(), "trade {k} : pnl_net");
        assert_eq!(t.exit_reason, ExitReason::Signal, "trade {k}");
    }
    // Le cas signalé par l'oracle : le fill suit le signal d'une minute (minute absente de la fixture).
    assert_eq!(result.trades[3].exit_signal_ms, 1_578_145_500_000);
    assert_eq!(result.trades[3].exit_ms, 1_578_145_560_000);

    let metrics = metrics_of(&result, &candles);
    assert_eq!(
        metrics.config_hash,
        "e85b28f7cff8f25137970a52a8f5cde0c429cc3cbeee7505f83bd1eed497646a"
    );
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
