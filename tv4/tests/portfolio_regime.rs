//! Filtre de régime BTC 1d / SMA200 lu par une paire de l'univers (docs S9 §6) : ETH seule trade, la série
//! BTC vient du dataset sans être dans l'univers, et elle est causale.
//!
//! Marché inventé : BTC en paliers journaliers (haussier, effondrement, rebond), ETH plate à 100.

mod portfolio_common;

use portfolio_common::{
    DAY, MINUTE, RegimeEntry, Scripted, T0, buy_risk, config, flat_bar, instrument, names, run,
    try_run,
};
use tv4::engine::{CellConfig, EngineError, PortfolioConfig, run_cell};
use tv4::model::{Candle, Slippage, Timeframe};
use tv4::sim::{ExitReason, SimRules, SizingMode};
use tv4::store::{Columns, GapsPolicy, Window, dsh};
use tv4::strategy::Strategy;

const DAYS: i64 = 260;

/// Clôture journalière de BTC : haussière, effondrée du jour 220 au jour 239, puis rebond.
fn btc_close(day: i64) -> f64 {
    match day {
        ..220 => 100.0 + day as f64,
        220..240 => 50.0,
        _ => 400.0,
    }
}

/// `days` jours de 1m plates, au prix donné par `price(jour)`.
fn daily_series(days: i64, price: impl Fn(i64) -> f64) -> Vec<Candle> {
    let mut out = Vec::with_capacity(usize::try_from(days * 1440).unwrap());
    for d in 0..days {
        let p = price(d);
        for m in 0..1440 {
            out.push(flat_bar(T0 + d * DAY + m * MINUTE, p, 1.0e6));
        }
    }
    out
}

fn regime_strategy() -> (
    Box<dyn Strategy>,
    std::sync::Arc<std::sync::Mutex<portfolio_common::Spy>>,
) {
    let (strategy, spy) = Scripted::with_regime(RegimeEntry {
        every_ms: DAY,
        signal: buy_risk(0.01, 90.0),
    });
    (Box::new(strategy), spy)
}

fn eth_config<'a>(
    universe: &'a [String],
    instruments: &'a [tv4::model::InstrumentRules],
) -> PortfolioConfig<'a> {
    PortfolioConfig {
        sizing: SizingMode::Compounding,
        regime_symbol: "BTC-USD",
        ..config(universe, instruments, 1000.0)
    }
}

/// Oracle : jours (`D`) où la stratégie entre et sort, d'après la clôture 1d BTC du jour d'avant et sa
/// SMA200, calculés dans le test avec le même `>` strict. Les appels ont lieu aux minuits `D = 200..=260`.
fn oracle(close_of: impl Fn(i64) -> f64) -> (Vec<i64>, Vec<i64>) {
    let sma = |d: i64| -> f64 {
        let mut sum = 0.0;
        for k in d - 199..=d {
            sum += close_of(k);
        }
        sum / 200.0
    };
    let (mut entries, mut exits) = (Vec::new(), Vec::new());
    let mut flat = true;
    for day in 200..=DAYS {
        let bull = close_of(day - 1) > sma(day - 1);
        if flat && bull {
            entries.push(day);
            flat = false;
        } else if !flat && !bull {
            exits.push(day);
            flat = true;
        }
    }
    (entries, exits)
}

#[test]
fn eth_seule_lit_le_regime_btc_et_n_entre_que_les_jours_haussiers() {
    let universe = names(&["ETH-USD"]);
    let instruments = vec![instrument(0.0)];
    let (strategy, spy) = regime_strategy();
    let result = run(
        &[
            ("ETH-USD", daily_series(DAYS, |_| 100.0)),
            ("BTC-USD", daily_series(DAYS, btc_close)),
        ],
        vec![strategy],
        eth_config(&universe, &instruments),
    );
    let (entries, exits) = oracle(btc_close);
    assert_eq!(entries, vec![200, 241], "l'oracle lui-même");
    assert_eq!(exits, vec![221]);

    // Seule ETH est dans l'univers : BTC n'a ni jambe, ni trade, ni rang.
    assert_eq!(result.universe, vec!["ETH-USD".to_string()]);
    assert_eq!(result.legs.len(), 1);
    assert_eq!(result.trades.len(), 2, "{:?}", result.trades);
    assert_eq!(result.trades[0].entry_signal_ms, T0 + 200 * DAY);
    assert_eq!(result.trades[0].exit_signal_ms, T0 + 221 * DAY);
    assert_eq!(result.trades[1].entry_signal_ms, T0 + 241 * DAY);
    assert_eq!(result.trades[1].exit_reason, ExitReason::ForcedClose);

    // La fenêtre démarre à la première minute où la SMA200 existe (fin du jour 199).
    assert_eq!(result.window_start_ms, T0 + 199 * DAY + 1439 * MINUTE);
    let regime = result.regime.as_ref().expect("bloc régime");
    assert_eq!(regime.symbol, "BTC-USD");
    assert_eq!(regime.first_ready_ms, T0 + 200 * DAY);
    // Jours de la fenêtre (200 à 259, plus le 199 partiel) jugés sur la 1d de la veille.
    assert!(regime.days_bull > 0 && regime.days_bear > 0);
    assert_eq!(
        regime.days_bull + regime.days_bear,
        60,
        "jours 200 à 259 de la fenêtre"
    );
    // Le hash porte le régime.
    assert_eq!(
        result.config.portfolio.regime.as_ref().unwrap().symbol,
        "BTC-USD"
    );

    // Espion : la dernière 1d vue est toujours close à `now_ms`, et le régime est toujours défini.
    let spy = spy.lock().unwrap();
    assert!(!spy.regime_calls.is_empty());
    for (now_ms, last_open, bull) in &spy.regime_calls {
        let last_open = last_open.expect("au moins une 1d close");
        assert!(last_open + DAY <= *now_ms, "1d non close à {now_ms}");
        assert!(bull.is_some(), "SMA200 absente à {now_ms}");
    }
}

#[test]
fn le_futur_de_btc_ne_change_aucun_trade_passe() {
    let universe = names(&["ETH-USD"]);
    let instruments = vec![instrument(0.0)];
    let play = |close_after_250: f64| {
        let (strategy, spy) = regime_strategy();
        let btc = daily_series(DAYS, |d| {
            if d >= 250 {
                close_after_250
            } else {
                btc_close(d)
            }
        });
        let result = run(
            &[("ETH-USD", daily_series(DAYS, |_| 100.0)), ("BTC-USD", btc)],
            vec![strategy],
            eth_config(&universe, &instruments),
        );
        let calls = spy.lock().unwrap().regime_calls.clone();
        (result, calls)
    };
    let (base, base_calls) = play(400.0);
    let (changed, changed_calls) = play(5.0);

    // Le premier trade est entièrement passé avant la perturbation : identique au bit.
    assert_eq!(base.trades[0], changed.trades[0]);
    // Le second a la même entrée (signal du jour 241) ; sa sortie dépend du futur de BTC.
    assert_eq!(
        base.trades[1].entry_signal_ms,
        changed.trades[1].entry_signal_ms
    );
    assert_eq!(base.trades[1].entry_price, changed.trades[1].entry_price);
    assert_eq!(base.trades[1].exit_reason, ExitReason::ForcedClose);
    assert_eq!(changed.trades[1].exit_reason, ExitReason::Signal);
    assert_eq!(changed.trades[1].exit_signal_ms, T0 + 251 * DAY);
    // Tout ce que la stratégie a vu du régime jusqu'au jour 250 est identique.
    let upto = |calls: &[(i64, Option<i64>, Option<bool>)]| {
        calls
            .iter()
            .filter(|(now, _, _)| *now <= T0 + 250 * DAY)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(upto(&base_calls), upto(&changed_calls));
    assert!(upto(&base_calls).len() > 50 * 1000);
}

#[test]
fn une_minute_btc_anterieure_change_le_dataset_hash() {
    let eth = Columns::from_candles(&daily_series(3, |_| 100.0));
    let btc = Columns::from_candles(&daily_series(3, |d| 100.0 + d as f64));
    let mut perturbed = daily_series(3, |d| 100.0 + d as f64);
    perturbed[10].close += 0.1;
    perturbed[10].high += 0.1;
    let btc_perturbed = Columns::from_candles(&perturbed);
    let window = Window::new(T0, T0 + 3 * DAY).unwrap();
    let hash = |series: &[(&str, Timeframe, &Columns)]| {
        dsh::dataset_hash("kraken", window, GapsPolicy::Skip, series)
            .unwrap()
            .0
    };
    let eth_only = hash(&[("ETH-USD", Timeframe::M1, &eth)]);
    let both = hash(&[
        ("ETH-USD", Timeframe::M1, &eth),
        ("BTC-USD", Timeframe::M1, &btc),
    ]);
    let both_perturbed = hash(&[
        ("ETH-USD", Timeframe::M1, &eth),
        ("BTC-USD", Timeframe::M1, &btc_perturbed),
    ]);
    assert_ne!(
        eth_only, both,
        "BTC chargé pour le filtre compte dans le hash"
    );
    assert_ne!(both, both_perturbed, "un prix BTC antérieur change le hash");
}

#[test]
fn erreurs_de_regime() {
    let universe = names(&["ETH-USD"]);
    let instruments = vec![instrument(0.0)];

    // Dataset sans la série de régime.
    let (strategy, _) = regime_strategy();
    let error = try_run(
        &[("ETH-USD", daily_series(DAYS, |_| 100.0))],
        vec![strategy],
        eth_config(&universe, &instruments),
    )
    .unwrap_err();
    assert!(
        matches!(&error, EngineError::RegimeSeriesMissing { symbol } if symbol == "BTC-USD"),
        "{error}"
    );

    // Série de régime trop courte pour la SMA200.
    let (strategy, _) = regime_strategy();
    let error = try_run(
        &[
            ("ETH-USD", daily_series(150, |_| 100.0)),
            ("BTC-USD", daily_series(150, btc_close)),
        ],
        vec![strategy],
        eth_config(&universe, &instruments),
    )
    .unwrap_err();
    assert!(
        matches!(error, EngineError::RegimeNotReady { .. }),
        "{error}"
    );

    // `run_cell` refuse une stratégie qui lit le régime.
    let (strategy, _) = regime_strategy();
    let cell = CellConfig {
        symbol: "ETH-USD",
        fees: portfolio_common::fees(),
        slippage: Slippage::ZERO,
        initial_cash: 1000.0,
        sim_rules: SimRules::default(),
        instrument: instrument(0.0),
    };
    let error = run_cell(&daily_series(2, |_| 100.0), strategy, &cell).unwrap_err();
    assert!(
        matches!(error, EngineError::RegimeNeedsPortfolio),
        "{error}"
    );
}

#[test]
fn sans_stratege_de_regime_la_serie_btc_est_ignoree() {
    // Une stratégie qui ne lit pas le régime n'exige pas la série, et le hash n'en porte pas.
    let universe = names(&["ETH-USD"]);
    let instruments = vec![instrument(0.0)];
    let (plain, _) = Scripted::new(Vec::new());
    let result = run(
        &[("ETH-USD", daily_series(3, |_| 100.0))],
        vec![Box::new(plain)],
        eth_config(&universe, &instruments),
    );
    assert!(result.regime.is_none());
    assert!(result.config.portfolio.regime.is_none());
}

#[test]
fn la_paire_de_regime_peut_etre_dans_l_univers() {
    // BTC et ETH tradent toutes deux, et lisent la même série BTC.
    let universe = names(&["BTC-USD", "ETH-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let (btc_strategy, _) = regime_strategy();
    let (eth_strategy, _) = regime_strategy();
    let result = run(
        &[
            ("ETH-USD", daily_series(DAYS, |_| 100.0)),
            ("BTC-USD", daily_series(DAYS, btc_close)),
        ],
        vec![btc_strategy, eth_strategy],
        eth_config(&universe, &instruments),
    );
    // Chaque jambe a les deux mêmes trades de régime (stop 90 : pour BTC, stop sous le prix aussi).
    let count = |leg: u16| result.trade_legs.iter().filter(|l| **l == leg).count();
    assert_eq!(count(0), 2);
    assert_eq!(count(1), 2);
    assert_eq!(result.legs[0].symbol, "BTC-USD");
}
