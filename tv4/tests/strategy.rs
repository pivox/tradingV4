//! Tests d'intégration de `strategy` avec `indicators`, sans moteur (le moteur arrive en S3).

use tv4::indicators::{Atr, Sma, batch};
use tv4::model::{Candle, ExchangeId, FeeSchedule, MAX_INDICATOR_PERIOD, Timeframe};
use tv4::strategy::registry;
use tv4::strategy::{
    BarContext, BarState, IndicatorKind, IndicatorSpec, OrderKind, Position, Signal, TfView,
};

const CONFIG: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/strategies/sma_cross.toml"
);
const BASE_MS: i64 = 1_704_067_200_000;
const M15_MS: i64 = 900_000;

#[test]
fn la_periode_maximale_est_appliquee_par_les_indicateurs() {
    assert!(Sma::new(MAX_INDICATOR_PERIOD).is_ok());
    assert!(Sma::new(MAX_INDICATOR_PERIOD + 1).is_err());
}

/// Onde triangulaire exacte, sans libm : `close_i = 110 - 0,2 × |(i mod 200) - 100|`.
/// Valeurs inventées, pas des données de marché.
fn triangle_wave(count: usize) -> Vec<Candle> {
    let mut candles = Vec::with_capacity(count);
    let mut previous_close = 110.0 - 0.2 * 100.0;
    for i in 0..count {
        let phase = (i % 200) as f64 - 100.0;
        let close = 110.0 - 0.2 * phase.abs();
        let open = previous_close;
        candles.push(Candle {
            open_time_ms: BASE_MS + i as i64 * M15_MS,
            open,
            high: open.max(close) + 0.5,
            low: open.min(close) - 0.5,
            close,
            volume: 1.0,
            trades: None,
        });
        previous_close = close;
    }
    candles
}

/// Calcule les colonnes demandées par `indicators()` (mapping fait par le moteur en S3).
fn columns_for(specs: &[IndicatorSpec], candles: &[Candle]) -> Vec<Vec<f64>> {
    let closes = candles.iter().map(|c| c.close);
    specs
        .iter()
        .map(|spec| match spec.kind {
            IndicatorKind::Sma { period } => batch(Sma::new(period).unwrap(), closes.clone()),
            IndicatorKind::Atr { period } => {
                batch(Atr::new(period).unwrap(), candles.iter().copied())
            }
            autre => panic!("indicateur non utilisé par sma_cross : {autre:?}"),
        })
        .collect()
}

/// Ce test attrape aussi une faute de frappe sur un nom d'indicateur dans `on_bar` (par exemple
/// `ctx.ind(tf, "fsat")`) : un nom non résolu rend `&[]`, la stratégie sort sans signal, et les
/// assertions « au moins 2 entrées et 2 sorties » puis « stop = close - k × atr » échouent. Un test
/// qui ne ferait que résoudre les noms de `indicators()` serait tautologique : il ne voit pas `on_bar`.
#[test]
fn sma_cross_de_bout_en_bout_sans_moteur() {
    let text = std::fs::read_to_string(CONFIG).unwrap();
    let params = toml::from_str::<toml::Value>(&text).unwrap();
    let mut strategy = registry::build("sma_cross", &params).unwrap();
    let tf = Timeframe::M15;
    assert_eq!(strategy.timeframes(), [tf]);

    let candles = triangle_wave(600);
    let specs = strategy.indicators();
    let names: Vec<&'static str> = specs.iter().map(|s| s.name).collect();
    let owned = columns_for(&specs, &candles);
    let columns: Vec<&[f64]> = owned.iter().map(Vec::as_slice).collect();
    let base = TfView::new(tf, &candles, &names, &columns).unwrap();
    let fees = FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap();

    let mut position: Option<Position> = None;
    let mut out: Vec<Signal> = Vec::new();
    let mut events: Vec<(usize, Signal)> = Vec::new();
    for (i, candle) in candles.iter().enumerate() {
        let views = [base.with_closed(i + 1).unwrap()];
        let state = BarState {
            now_ms: candle.open_time_ms + M15_MS,
            equity: 300.0,
            quote_free: 300.0,
            qty_free: position.map_or(0.0, |p| p.qty),
            position,
            fees,
        };
        let ctx = BarContext::new("BTC-USDC", state, &[], &views, tf.bit()).unwrap();
        out.clear();
        strategy.on_bar(&ctx, &mut out);
        for signal in &out {
            events.push((i, *signal));
            match signal {
                Signal::EnterLong { .. } => {
                    position = Some(Position {
                        qty: 1.0,
                        avg_price: candle.close,
                        stop: None,
                        take_profit: None,
                        opened_at_ms: candle.open_time_ms,
                    });
                }
                Signal::ExitLong { .. } => position = None,
                autre => panic!("signal inattendu : {autre:?}"),
            }
        }
    }

    // Aucun signal avant l'index 50 (la première valeur de la SMA lente est à
    // l'index 49, et un croisement exige une valeur précédente valide).
    assert!(events.iter().all(|(i, _)| *i >= strategy.warmup_bars()));
    assert!(matches!(
        events.first(),
        Some((_, Signal::EnterLong { .. }))
    ));
    for pair in events.windows(2) {
        let alternate = matches!(
            (pair[0].1, pair[1].1),
            (Signal::EnterLong { .. }, Signal::ExitLong { .. })
                | (Signal::ExitLong { .. }, Signal::EnterLong { .. })
        );
        assert!(alternate, "entrées et sorties doivent alterner : {pair:?}");
    }
    let entries = events
        .iter()
        .filter(|(_, s)| matches!(s, Signal::EnterLong { .. }))
        .count();
    let exits = events.len() - entries;
    assert!(
        entries >= 2 && exits >= 2,
        "{entries} entrées, {exits} sorties"
    );
    for (i, signal) in &events {
        if let Signal::EnterLong { stop, kind, .. } = signal {
            assert_eq!(*kind, OrderKind::Market);
            let stop = stop.expect("un stop est posé");
            assert!(stop > 0.0 && stop < candles[*i].close, "stop {stop}");
            // stop_atr_mult = 2.0 dans config/strategies/sma_cross.toml ; colonne `atr` = index 2.
            assert_eq!(stop, candles[*i].close - 2.0 * owned[2][*i]);
        }
    }
}
