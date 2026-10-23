//! Backtest S3 sur la fixture réelle `fixtures/kraken_xbtusd_1m.csv` (XBTUSD 1m, 10 000 bougies).
//!
//! Les valeurs de contrôle viennent d'un oracle Python indépendant (bibliothèque standard seulement,
//! mêmes formules et même ordre d'opérations que `sim` et `engine`). Si le code diverge, on
//! n'ajuste pas le test : c'est la spécification ou le code qui est faux.
//!
//! S5 : le test `sma_cross_15m_valeurs_de_l_oracle` épingle de la même façon `sma_cross` 15m (20/50)
//! sur la fixture, avec les valeurs de l'oracle Python indépendant (`s5-oracle/oracle.json`, point 4),
//! qui concordent au bit avec le moteur, trade par trade. Même règle : on n'ajuste pas le test.
//!
//! S6 : les règles du simulateur complet (délai d'activation, règle de volume, lots, stops actifs)
//! changent ces valeurs. Les nouvelles viennent de l'oracle S6 (`s6-oracle/oracle.json`, écrit depuis la
//! spécification et sans le code du moteur), recoupées au bit avec le moteur par la revue du 15/10 :
//! chiffres, trades et BLAKE3 de `trades.csv`. Les valeurs S3 et S5 ne sont plus produites par le moteur
//! et ne figurent plus que dans docs/03 §7.2.

mod common;

use common::{
    fixture_1m, kraken_btc_usd, okx_fees, repo_path, slippage, sma_cross_1m, strategy_from_file,
};
use tv4::engine::{CellConfig, CellResult, run_cell};
use tv4::model::hash;
use tv4::report::writers::trades_csv;
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
        instrument: kraken_btc_usd(),
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
    assert!(!result.trades.is_empty());
    for (k, t) in result.trades.iter().enumerate() {
        let entree = candles
            .binary_search_by_key(&t.entry_ms, |c| c.open_time_ms)
            .unwrap_or_else(|_| panic!("trade {k} : bougie d'entrée introuvable"));
        let sortie = candles
            .binary_search_by_key(&t.exit_ms, |c| c.open_time_ms)
            .unwrap_or_else(|_| panic!("trade {k} : bougie de sortie introuvable"));
        // S6 : délai d'activation de 1 minute, l'ordre n'est jamais rempli avant `signal + 1 min`.
        assert!(t.entry_ms >= t.entry_signal_ms + 60_000, "trade {k}");
        assert!(t.exit_ms >= t.exit_signal_ms + 60_000, "trade {k}");
        assert!(t.exit_ms > t.entry_ms, "trade {k}");
        // Entrée au market : open de la 1m de fill, plus le slippage de 5 bps, taker.
        assert_eq!(t.entry_ref_price, candles[entree].open, "trade {k}");
        assert_eq!(t.entry_price, candles[entree].open * (1.0 + 0.0005));
        // Le prix de fill reste dans la bougie de fill (I3), au slippage près.
        assert!(
            t.entry_price >= candles[entree].low && t.entry_price <= candles[entree].high * 1.001,
            "trade {k}"
        );
        match t.exit_reason {
            ExitReason::Signal => {
                assert_eq!(t.exit_ref_price, candles[sortie].open, "trade {k}");
                assert_eq!(t.exit_price, candles[sortie].open * (1.0 - 0.0005));
            }
            ExitReason::Stop => {
                // Stop au pire de (niveau, open) puis slippage de 5 bps en défaveur (taker).
                let stop = t.stop_at_exit.expect("stop renseigné");
                assert!(candles[sortie].low <= stop, "trade {k} : stop non franchi");
                assert_eq!(
                    t.exit_ref_price,
                    stop.min(candles[sortie].open),
                    "trade {k}"
                );
                assert_eq!(t.exit_price, t.exit_ref_price * (1.0 - 0.0005), "trade {k}");
            }
            ExitReason::ForcedClose => assert!(t.forced_close, "trade {k}"),
            _ => {}
        }

        // Quantité en lots entiers de 1e-8.
        let lots = t.qty * 1e8;
        assert!(
            (lots - lots.round()).abs() < 1e-3,
            "trade {k} : qty {}",
            t.qty
        );

        // Frais : pourcentage taker du notionnel de chaque jambe (entrée et sortie taker ici).
        assert_rel(
            "entry_fee",
            t.entry_fee,
            t.qty * t.entry_price * taker,
            1e-12,
        );
        assert_rel("exit_fee", t.exit_fee, t.qty * t.exit_price * taker, 1e-12);

        // PnL recalculé depuis les champs, tolérance relative au notionnel.
        let cout = t.qty * t.entry_price;
        let produit = t.qty * t.exit_price;
        let pnl = (produit - produit * taker) - (cout + cout * taker);
        assert!(
            (pnl - t.pnl_net).abs() <= 1e-9 * cout,
            "trade {k} : pnl {pnl} != {}",
            t.pnl_net
        );
        // Entrée au market avec slippage : le plus bas de la 1m de fill est sous le prix d'entrée, donc
        // MAE ≤ 0, et le MFE (plus haut − entrée) ne peut pas être sous le MAE.
        assert!(t.mae <= 0.0 && t.mfe >= t.mae, "trade {k}");
    }
    let ecart = (result.pnl_net - (result.pnl_realized + result.pnl_marked)).abs();
    assert!(ecart <= 1e-9 * CAPITAL, "écart de PnL {ecart}");
}

/// Valeurs de l'oracle S6 pour `sma_cross` 1m (60/240) : 3 trades, 25 signaux dont 20 achats rejetés
/// par la règle de volume au fill (`oracle.json`, `sma_cross.fixture_1m_defauts`). Avant S6 (oracle S3) :
/// 23 trades, `pnl_net` -15.408523707748486, cash final 284.5914762922515, 46 signaux.
#[test]
fn valeurs_de_controle_de_l_oracle() {
    let (candles, result) = run();
    assert_eq!(result.start_index, 240);
    assert_eq!(result.bars_in_window, 9_760);
    assert_eq!(result.trades.len(), 3);
    assert!(!result.forced_close);
    assert_eq!(result.signals_emitted, 25);
    assert_eq!(result.signals_accepted, 25);
    assert_eq!(result.rejections.total(), 0);
    assert_eq!(result.sim.entries_capped_by_cash, 3);
    assert_eq!(result.sim.fills, 6);
    assert_eq!(result.sim.fills_market, 5);
    assert_eq!(result.sim.stops_triggered, 1);
    assert_eq!(result.sim.buys_rejected_volume_share, 20);
    assert_eq!(result.sim.exits_over_volume_share, 3);

    assert_rel("pnl_net", result.pnl_net, -3.198931025058812, 1e-9);
    assert_rel(
        "pnl_realized",
        result.pnl_realized,
        -3.198931025058812,
        1e-9,
    );
    assert_eq!(result.pnl_marked, 0.0);
    assert_rel("fees_total", result.fees_total, 5.9626163066636, 1e-9);
    assert_rel(
        "slippage_total",
        result.slippage_total,
        0.8518032333953195,
        1e-9,
    );
    assert_rel("cash_final", result.cash_final, 296.8010689749412, 1e-9);

    // Premier trade (entrée sur signal, sortie sur signal), vérifié à la main sur les lignes du CSV.
    let t = result.trades[0];
    assert_eq!(t.entry_signal_ms, 1_578_441_060_000);
    assert_eq!(t.entry_ms, 1_578_441_120_000);
    let signal = candles
        .binary_search_by_key(&(t.entry_signal_ms - 60_000), |c| c.open_time_ms)
        .unwrap();
    assert_eq!(candles[signal].close, 8149.3, "close de la 1m du signal");
    assert_eq!(candles[signal + 2].open, 8173.2, "open de la 1m du fill");
    assert_ne!(t.entry_ref_price, candles[signal].close);
    assert_eq!(t.entry_ref_price, 8173.2);
    assert_rel("entry_price", t.entry_price, 8177.286599999999, 1e-12);
    assert_rel("qty", t.qty, 0.03473107, 1e-12);
    assert_eq!(t.exit_signal_ms, 1_578_453_960_000);
    assert_eq!(t.exit_ms, 1_578_454_020_000);
    assert_eq!(t.exit_ref_price, 8254.7);
    assert_rel("exit_price", t.exit_price, 8250.572650000002, 1e-12);
    assert_rel("entry_fee", t.entry_fee, 0.994020696601317, 1e-12);
    assert_rel("exit_fee", t.exit_fee, 1.0029292568653245, 1e-12);
    assert_rel("pnl_net du trade 1", t.pnl_net, 0.5483529791069373, 1e-12);
    assert_rel("mfe", t.mfe, 9.968282486337994, 1e-12);
    assert_rel("mae", t.mae, -0.9442197076619684, 1e-12);
    assert_rel(
        "stop_at_exit",
        t.stop_at_exit.unwrap(),
        8107.5197146872015,
        1e-12,
    );
    assert!(t.capped_by_cash);
    assert_eq!(t.exit_reason, ExitReason::Signal);

    // Trade 2 (sortie sur stop) et trade 3 (sortie sur signal).
    let (u, v) = (result.trades[1], result.trades[2]);
    assert_eq!(
        (u.entry_ms, u.exit_ms),
        (1_578_479_340_000, 1_578_481_020_000)
    );
    assert_eq!(u.exit_reason, ExitReason::Stop);
    assert_rel("entry_price 2", u.entry_price, 8353.1745, 1e-12);
    assert_eq!(u.exit_ref_price, 8334.0);
    assert_rel("exit_price 2", u.exit_price, 8329.833, 1e-12);
    assert_rel("qty 2", u.qty, 0.03406191, 1e-12);
    assert_rel("pnl_net 2", u.pnl_net, -2.783948922245031, 1e-12);
    assert_eq!(
        (v.entry_ms, v.exit_ms),
        (1_578_492_780_000, 1_578_494_880_000)
    );
    assert_eq!(v.exit_reason, ExitReason::Signal);
    assert_rel("entry_price 3", v.entry_price, 8266.3311, 1e-12);
    assert_eq!(v.exit_ref_price, 8300.2);
    assert_rel("exit_price 3", v.exit_price, 8296.049900000002, 1e-12);
    assert_rel("qty 3", v.qty, 0.03410092, 1e-12);
    assert_rel("pnl_net 3", v.pnl_net, -0.963335081920718, 1e-12);

    // `trades.csv` entier, à l'octet : BLAKE3 du fichier attendu de l'oracle.
    assert_eq!(
        hash::blake3_hex(&trades_csv(&result.trades).unwrap()),
        "43865c157588a1471936e4cc25dbbe8f00f6a715866b8e08610733ae3f4763cc"
    );
    let metrics = metrics_of(&result, &candles);
    assert_eq!(
        metrics.config_hash,
        "6ea489939050b331f9a2b716a34e52f10ef8dd4a846f9578da4151d6a2e8aa49"
    );
}

/// Un trade de l'oracle : `(sig_in, t_in, sig_out, t_out, px_in, px_out, qty, pnl)`.
type TradeOracle = (i64, i64, i64, i64, f64, f64, f64, f64);

/// Les deux trades de `sma_cross` 15m (20/50, `stop_atr_mult` 2,0) sur la fixture après S6, issus de
/// l'oracle S6 : une sortie sur signal, puis une sortie sur stop (`exit_signal_ms` est le signal de
/// l'entrée, aucun signal de sortie n'a précédé le stop). Les dix trades d'avant S6 sont dans l'historique
/// de ce fichier et dans docs/03 §7.2.
const TRADES_15M_ORACLE: [TradeOracle; 2] = [
    (
        1578027600000,
        1578027660000,
        1578091500000,
        1578091560000,
        7220.008199999999,
        7266.7648,
        0.02677055,
        -0.10566618514297943,
    ),
    (
        1578421800000,
        1578421860000,
        1578421800000,
        1578435600000,
        8083.939949999999,
        7952.416703468976,
        0.02351281,
        -4.412190431065511,
    ),
];

fn strategie_15m() -> Box<dyn tv4::strategy::Strategy> {
    strategy_from_file("sma_cross", "config/strategies/sma_cross.toml")
}

/// Valeurs de l'oracle S6 pour `sma_cross` 15m (20/50) : 2 trades, 11 signaux dont 8 achats rejetés par
/// la règle de volume au fill (`oracle.json`, `sma_cross.fixture_15m_defauts`). Avant S6 (oracle S5) :
/// 10 trades, `pnl_net` -7.401437247087472, cash final 292.5985627529125, 20 signaux, `config_hash`
/// `e85b28f7…`.
#[test]
fn sma_cross_15m_valeurs_de_l_oracle() {
    let (candles, result) = run_avec(strategie_15m());
    // 50 bougies 15m de warmup : la 1m 482 est la première jouée (la fixture a des minutes absentes).
    assert_eq!(result.start_index, 482);
    assert_eq!(result.bars_in_window, 9_518);
    assert_eq!(result.trades.len(), 2);
    assert!(!result.forced_close);
    assert_eq!(result.signals_emitted, 11);
    assert_eq!(result.signals_accepted, 11);
    assert_eq!(result.rejections.total(), 0);
    assert_eq!(result.sim.fills, 4);
    assert_eq!(result.sim.fills_market, 3);
    assert_eq!(result.sim.stops_triggered, 1);
    assert_eq!(result.sim.buys_rejected_volume_share, 8);
    assert_eq!(result.sim.exits_over_volume_share, 1);

    assert_rel("pnl_net", result.pnl_net, -4.517856616208519, 1e-9);
    assert_rel("cash_final", result.cash_final, 295.4821433837915, 1e-9);
    assert_rel("fees_total", result.fees_total, 2.6770754080714063, 1e-9);
    assert_rel(
        "slippage_total",
        result.slippage_total,
        0.3824389794246069,
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
    }
    assert_eq!(result.trades[0].exit_reason, ExitReason::Signal);
    // Le second trade sort sur le stop, au pire de (stop, open) : la référence est le stop lui-même.
    let stop = &result.trades[1];
    assert_eq!(stop.exit_reason, ExitReason::Stop);
    assert_eq!(stop.exit_ref_price, 7956.394900919436);
    assert_eq!(stop.stop_at_exit, Some(7956.394900919436));

    assert_eq!(
        hash::blake3_hex(&trades_csv(&result.trades).unwrap()),
        "e26a9b934782945495ab36aa5e38b755573027c60312d0a0526d4ff6fe9e9cc4"
    );
    let metrics = metrics_of(&result, &candles);
    assert_eq!(
        metrics.config_hash,
        "bec4b9066d08b9285645e941819551fcf11cc5b00c4c75f3c2f7e9fb20bb7d0a"
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

/// Fixture complétée (politique `ffill`) : chaque minute absente reçoit une bougie synthétique
/// `(pc, pc, pc, pc, volume 0)`, `pc` étant le close de la ligne précédente. 11 370 bougies.
fn completee(candles: &[tv4::model::Candle]) -> Vec<tv4::model::Candle> {
    let mut out: Vec<tv4::model::Candle> = Vec::with_capacity(candles.len() + 1_400);
    for c in candles {
        if let Some(prev) = out.last().copied() {
            let mut t = prev.open_time_ms + 60_000;
            while t < c.open_time_ms {
                out.push(tv4::model::Candle {
                    open_time_ms: t,
                    open: prev.close,
                    high: prev.close,
                    low: prev.close,
                    close: prev.close,
                    volume: 0.0,
                    trades: None,
                });
                t += 60_000;
            }
        }
        out.push(*c);
    }
    out
}

/// Résultat attendu d'un run de l'oracle S6.
struct Attendu {
    n_trades: usize,
    emis: u64,
    pnl_net: f64,
    cash_final: f64,
    fees: f64,
    stops: u64,
    minutes_sans_volume: u64,
    trades_csv_blake3: &'static str,
}

/// Les douze runs de `sma_cross` de l'oracle S6 (`oracle.json`, `sma_cross.*`) : fixture et fixture
/// complétée (politique `ffill`), en 1m, 15m et 2 TF, avec la règle de volume par défaut (`defauts`,
/// 0,5 %) puis sans elle (`1e9`). Chaque run est recoupé au bit par la revue du 15/10 : PnL, cash,
/// frais, compteurs et BLAKE3 de `trades.csv`.
fn runs_de_l_oracle() -> Vec<(&'static str, &'static str, &'static str, Attendu)> {
    vec![
        (
            "fixture",
            "1m",
            "defauts",
            Attendu {
                n_trades: 3,
                emis: 25,
                pnl_net: -3.198931025058812,
                cash_final: 296.8010689749412,
                fees: 5.9626163066636,
                stops: 1,
                minutes_sans_volume: 0,
                trades_csv_blake3: "43865c157588a1471936e4cc25dbbe8f00f6a715866b8e08610733ae3f4763cc",
            },
        ),
        (
            "fixture",
            "1m",
            "share_1e9",
            Attendu {
                n_trades: 23,
                emis: 27,
                pnl_net: -45.47137747728527,
                cash_final: 254.52862252271473,
                fees: 42.07664368805497,
                stops: 19,
                minutes_sans_volume: 0,
                trades_csv_blake3: "80440d53af847cd728e445064da3d10c36666aa14a371b80224e53c332d78db0",
            },
        ),
        (
            "fixture",
            "15m",
            "defauts",
            Attendu {
                n_trades: 2,
                emis: 11,
                pnl_net: -4.517856616208519,
                cash_final: 295.4821433837915,
                fees: 2.6770754080714063,
                stops: 1,
                minutes_sans_volume: 0,
                trades_csv_blake3: "e26a9b934782945495ab36aa5e38b755573027c60312d0a0526d4ff6fe9e9cc4",
            },
        ),
        (
            "fixture",
            "15m",
            "share_1e9",
            Attendu {
                n_trades: 10,
                emis: 14,
                pnl_net: -14.673978701960777,
                cash_final: 285.3260212980392,
                fees: 18.11568807013358,
                stops: 6,
                minutes_sans_volume: 0,
                trades_csv_blake3: "d807f4907020931271b319e1bb14e9cba92c97e1cf86e538791249cd0284d8bd",
            },
        ),
        (
            "fixture",
            "2tf",
            "defauts",
            Attendu {
                n_trades: 2,
                emis: 9,
                pnl_net: -4.517856616208519,
                cash_final: 295.4821433837915,
                fees: 2.6770754080714063,
                stops: 1,
                minutes_sans_volume: 0,
                trades_csv_blake3: "e26a9b934782945495ab36aa5e38b755573027c60312d0a0526d4ff6fe9e9cc4",
            },
        ),
        (
            "fixture",
            "2tf",
            "share_1e9",
            Attendu {
                n_trades: 8,
                emis: 12,
                pnl_net: -8.377775630301699,
                cash_final: 291.6222243696983,
                fees: 14.482379765043191,
                stops: 4,
                minutes_sans_volume: 0,
                trades_csv_blake3: "d4b42a0d7ded4a8925a4a4d572c1da983860dee1e49a4814dc457bdedce4f19e",
            },
        ),
        (
            "completee",
            "1m",
            "defauts",
            Attendu {
                n_trades: 5,
                emis: 34,
                pnl_net: -7.468584987007262,
                cash_final: 292.53141501299274,
                fees: 9.812552057958824,
                stops: 3,
                minutes_sans_volume: 8,
                trades_csv_blake3: "b62215bbacd177314f8f861f034a7655432d37023614de4849e64b25498a8152",
            },
        ),
        (
            "completee",
            "1m",
            "share_1e9",
            Attendu {
                n_trades: 30,
                emis: 36,
                pnl_net: -44.30902758357547,
                cash_final: 255.69097241642453,
                fees: 55.859710657051416,
                stops: 24,
                minutes_sans_volume: 156,
                trades_csv_blake3: "bf15f0f171ad5ddcc0659084f116cd3daab6b72acf9c5cd23642ad57fcefe74c",
            },
        ),
        (
            "completee",
            "15m",
            "defauts",
            Attendu {
                n_trades: 2,
                emis: 11,
                pnl_net: -4.517856616208519,
                cash_final: 295.4821433837915,
                fees: 2.6770754080714063,
                stops: 1,
                minutes_sans_volume: 12,
                trades_csv_blake3: "e26a9b934782945495ab36aa5e38b755573027c60312d0a0526d4ff6fe9e9cc4",
            },
        ),
        (
            "completee",
            "15m",
            "share_1e9",
            Attendu {
                n_trades: 10,
                emis: 14,
                pnl_net: -14.571277317735792,
                cash_final: 285.4287226822642,
                fees: 18.12125110522763,
                stops: 6,
                minutes_sans_volume: 486,
                trades_csv_blake3: "565c4769d9ccd6641a6dff72171ac257d149e3fb36fc77e467a2a146caa16a34",
            },
        ),
        (
            "completee",
            "2tf",
            "defauts",
            Attendu {
                n_trades: 2,
                emis: 9,
                pnl_net: -4.517856616208519,
                cash_final: 295.4821433837915,
                fees: 2.6770754080714063,
                stops: 1,
                minutes_sans_volume: 12,
                trades_csv_blake3: "e26a9b934782945495ab36aa5e38b755573027c60312d0a0526d4ff6fe9e9cc4",
            },
        ),
        (
            "completee",
            "2tf",
            "share_1e9",
            Attendu {
                n_trades: 8,
                emis: 12,
                pnl_net: -8.381487287225639,
                cash_final: 291.61851271277436,
                fees: 14.482274802629277,
                stops: 4,
                minutes_sans_volume: 409,
                trades_csv_blake3: "5bbe0ad4fbcc28c922e7bb5c0d5ebe7027ec1fa1743c1b84a8b48c2367f6a87d",
            },
        ),
    ]
}

#[test]
fn les_douze_runs_de_l_oracle_s6() {
    let brut = fixture_1m();
    let completee = completee(&brut);
    for (jeu, nom, partage, attendu) in runs_de_l_oracle() {
        let candles = if jeu == "fixture" { &brut } else { &completee };
        let params = match nom {
            "1m" => "config/strategies/sma_cross_1m.toml",
            "15m" => "config/strategies/sma_cross.toml",
            _ => "config/strategies/sma_cross_2tf.toml",
        };
        let share = if partage == "defauts" { 0.005 } else { 1e9 };
        let config = CellConfig {
            symbol: "BTC-USD",
            fees: okx_fees(),
            slippage: slippage(5.0),
            initial_cash: CAPITAL,
            sim_rules: SimRules {
                max_volume_share: share,
                ..SimRules::default()
            },
            instrument: kraken_btc_usd(),
        };
        let r = run_cell(candles, strategy_from_file("sma_cross", params), &config)
            .expect("backtest valide");
        let id = format!("{jeu} {nom} {partage}");
        assert_eq!(r.trades.len(), attendu.n_trades, "{id} : trades");
        assert_eq!(r.signals_emitted, attendu.emis, "{id} : signaux émis");
        assert_eq!(r.rejections.total(), 0, "{id} : rejets");
        assert_eq!(
            r.pnl_net.to_bits(),
            attendu.pnl_net.to_bits(),
            "{id} : pnl_net"
        );
        assert_eq!(
            r.cash_final.to_bits(),
            attendu.cash_final.to_bits(),
            "{id} : cash_final"
        );
        assert_eq!(
            r.fees_total.to_bits(),
            attendu.fees.to_bits(),
            "{id} : frais"
        );
        assert_eq!(r.sim.stops_triggered, attendu.stops, "{id} : stops");
        assert_eq!(
            r.sim.bars_zero_volume_blocked, attendu.minutes_sans_volume,
            "{id} : minutes sans volume"
        );
        assert_eq!(
            hash::blake3_hex(&trades_csv(&r.trades).unwrap()),
            attendu.trades_csv_blake3,
            "{id} : trades.csv"
        );
    }
}
