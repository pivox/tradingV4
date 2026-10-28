//! Filtre de régime de `strategy` (S9 §6.1 et §6.4, lot D) : `RegimeView`, `BarContext::with_regime`,
//! `Strategy::uses_regime`, avec la garde anti look-ahead.

use tv4::indicators::{Sma, batch};
use tv4::model::{Candle, ExchangeId, FeeSchedule, Timeframe};
use tv4::strategy::{
    BarContext, BarState, IndicatorSpec, REGIME_SMA_PERIOD, REGIME_TF, RegimeView, Signal,
    Strategy, StrategyError, TfView,
};

/// 2024-01-01T00:00:00Z en millisecondes.
const T0: i64 = 1_704_067_200_000;
const DAY: i64 = 86_400_000;

/// Bougie 1d de test (valeurs inventées, pas des données de marché).
fn daily(index: usize, close: f64) -> Candle {
    Candle {
        open_time_ms: T0 + index as i64 * DAY,
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 1.0,
        trades: None,
    }
}

fn series(closes: &[f64]) -> Vec<Candle> {
    closes
        .iter()
        .enumerate()
        .map(|(i, close)| daily(i, *close))
        .collect()
}

fn state(now_ms: i64) -> BarState {
    BarState {
        now_ms,
        equity: 300.0,
        quote_free: 300.0,
        qty_free: 0.0,
        position: None,
        fees: FeeSchedule::new(ExchangeId::Okx, 20, 35).unwrap(),
    }
}

/// Contexte minimal : une vue 1h vide, le régime éventuel, à l'instant `now_ms`.
fn context<'a>(
    now_ms: i64,
    h1: &'a [TfView<'a>],
    regime: Option<RegimeView<'a>>,
) -> Result<BarContext<'a>, StrategyError> {
    BarContext::new("ETH-USDC", state(now_ms), &[], h1, 0)?.with_regime(regime)
}

fn h1_views(candles: &[Candle]) -> Vec<TfView<'_>> {
    vec![TfView::new(Timeframe::H1, candles, &[], &[]).unwrap()]
}

#[test]
fn les_constantes_de_regime_sont_celles_de_la_spec() {
    assert_eq!(REGIME_TF, Timeframe::D1);
    assert_eq!(REGIME_SMA_PERIOD, 200);
}

#[test]
fn sans_vue_de_regime_le_contexte_n_en_a_pas() {
    let h1 = h1_views(&[]);
    let ctx = BarContext::new("ETH-USDC", state(T0), &[], &h1, 0).unwrap();
    assert!(ctx.regime().is_none());
    let ctx = context(T0, &h1, None).unwrap();
    assert!(ctx.regime().is_none());
}

#[test]
fn with_regime_none_retire_la_vue() {
    let candles = series(&[100.0]);
    let sma = [f64::NAN];
    let view = RegimeView::new(&candles, &sma).unwrap();
    let h1 = h1_views(&[]);
    let ctx = context(T0 + DAY, &h1, Some(view)).unwrap();
    assert!(ctx.regime().is_some());
    assert!(ctx.with_regime(None).unwrap().regime().is_none());
}

#[test]
fn la_vue_refuse_une_sma_de_longueur_differente() {
    let candles = series(&[100.0, 101.0, 102.0]);
    let sma = [f64::NAN, f64::NAN];
    assert!(matches!(
        RegimeView::new(&candles, &sma),
        Err(StrategyError::RegimeLength { candles: 3, sma: 2 })
    ));
}

#[test]
fn with_regime_refuse_une_bougie_non_close() {
    let candles = series(&[100.0, 101.0, 102.0]);
    let sma = [f64::NAN; 3];
    let view = RegimeView::new(&candles, &sma).unwrap();
    let h1 = h1_views(&[]);
    // La dernière bougie (jour 2) est visible dès T0 + 3 j, pas une milliseconde avant.
    let close_ms = T0 + 3 * DAY;
    assert!(context(close_ms, &h1, Some(view)).is_ok());
    match context(close_ms - 1, &h1, Some(view)) {
        Err(StrategyError::LookAhead {
            tf,
            open_time_ms,
            now_ms,
        }) => {
            assert_eq!(tf, Timeframe::D1);
            assert_eq!(open_time_ms, T0 + 2 * DAY);
            assert_eq!(now_ms, close_ms - 1);
        }
        autre => panic!("LookAhead attendu, obtenu {autre:?}"),
    }
}

#[test]
fn une_vue_vide_n_a_jamais_de_look_ahead() {
    let view = RegimeView::new(&[], &[]).unwrap();
    let h1 = h1_views(&[]);
    let ctx = context(T0, &h1, Some(view)).unwrap();
    let regime = ctx.regime().unwrap();
    assert!(regime.closed().is_empty());
    assert_eq!(regime.sma(), None);
    assert_eq!(regime.bull(), None);
}

#[test]
fn la_sma_vaut_none_avant_200_bougies_closes() {
    // 250 jours de clôtures croissantes, SMA200 calculée par la bibliothèque d'indicateurs.
    let closes: Vec<f64> = (0..250).map(|i| 100.0 + i as f64).collect();
    let candles = series(&closes);
    let sma = batch(Sma::new(REGIME_SMA_PERIOD).unwrap(), closes.iter().copied());
    let h1 = h1_views(&[]);
    for closed in [0usize, 1, 100, 199, 200, 201, 250] {
        let view = RegimeView::new(&candles[..closed], &sma[..closed]).unwrap();
        let ctx = context(T0 + closed as i64 * DAY, &h1, Some(view)).unwrap();
        let regime = ctx.regime().unwrap();
        assert_eq!(regime.closed().len(), closed);
        if closed < REGIME_SMA_PERIOD {
            assert_eq!(regime.sma(), None, "{closed} bougies closes");
            assert_eq!(regime.bull(), None, "{closed} bougies closes");
        } else {
            let value = regime.sma().expect("SMA200 définie dès 200 closes");
            assert_eq!(value, sma[closed - 1]);
            // Clôtures croissantes : la dernière est au-dessus de la moyenne des 200 dernières.
            assert_eq!(regime.bull(), Some(true), "{closed} bougies closes");
        }
    }
}

#[test]
fn une_sma_non_finie_vaut_none_meme_apres_200_bougies() {
    let candles = series(&[100.0; 200]);
    let mut sma = vec![100.0; 200];
    sma[199] = f64::NAN;
    let view = RegimeView::new(&candles, &sma).unwrap();
    assert_eq!(view.sma(), None);
    assert_eq!(view.bull(), None);
    sma[199] = f64::INFINITY;
    let view = RegimeView::new(&candles, &sma).unwrap();
    assert_eq!(view.sma(), None);
    assert_eq!(view.bull(), None);
}

#[test]
fn bull_est_strict() {
    let sma = vec![100.0; 200];
    let build = |last_close: f64| {
        let mut closes = vec![100.0; 199];
        closes.push(last_close);
        series(&closes)
    };
    // close = SMA : pas haussier.
    let candles = build(100.0);
    let view = RegimeView::new(&candles, &sma).unwrap();
    assert_eq!(view.sma(), Some(100.0));
    assert_eq!(view.bull(), Some(false));
    // Juste au-dessus : haussier. Juste en dessous : non.
    let candles = build(100.000_001);
    let view = RegimeView::new(&candles, &sma).unwrap();
    assert_eq!(view.bull(), Some(true));
    let candles = build(99.999_999);
    let view = RegimeView::new(&candles, &sma).unwrap();
    assert_eq!(view.bull(), Some(false));
    // Clôture non finie : pas de verdict.
    let candles = build(f64::NAN);
    let view = RegimeView::new(&candles, &sma).unwrap();
    assert_eq!(view.bull(), None);
}

#[test]
fn le_debug_ne_montre_pas_la_serie() {
    let closes: Vec<f64> = (0..300).map(|i| 1000.0 + i as f64 * 0.123).collect();
    let candles = series(&closes);
    let sma = vec![5.0; 300];
    let view = RegimeView::new(&candles, &sma).unwrap();
    let text = format!("{view:?}");
    assert!(text.contains("closed: 300"), "{text}");
    assert!(text.len() < 200, "Debug trop long ({} octets)", text.len());
    assert!(!text.contains("1000.123"), "{text}");
}

#[test]
fn le_contexte_et_la_vue_restent_copy() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<RegimeView<'static>>();
    assert_copy::<BarContext<'static>>();
}

/// Stratégie espion : enregistre la bougie 1d la plus récente vue à chaque appel.
struct Spy {
    uses_regime: bool,
    seen: Vec<(i64, i64)>,
}

impl Strategy for Spy {
    fn id(&self) -> &'static str {
        "spy_regime"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::H1]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, _out: &mut Vec<Signal>) {
        if let Some(last) = ctx.regime().and_then(|r| r.closed().last()) {
            self.seen.push((last.open_time_ms, ctx.now_ms()));
        }
    }
    fn uses_regime(&self) -> bool {
        self.uses_regime
    }
}

#[test]
fn uses_regime_vaut_false_par_defaut_et_se_surcharge() {
    // `SmaCross` n'a pas besoin du régime : la méthode par défaut rend `false`.
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../config/strategies/sma_cross.toml"
    ))
    .unwrap();
    let params = toml::from_str::<toml::Value>(&text).unwrap();
    let strategy = tv4::strategy::registry::build("sma_cross", &params).unwrap();
    assert!(!strategy.uses_regime());

    let spy = Spy {
        uses_regime: true,
        seen: Vec::new(),
    };
    assert!(spy.uses_regime());
    let boxed: Box<dyn Strategy> = Box::new(spy);
    assert!(boxed.uses_regime());
}

/// Un « moteur » minimal : à chaque heure d'une période de 12 jours, tronque la série au nombre de
/// bougies 1d closes et vérifie que la stratégie ne voit jamais la bougie du jour en cours.
#[test]
fn l_espion_ne_voit_que_des_bougies_closes() {
    let closes: Vec<f64> = (0..12).map(|i| 100.0 + i as f64).collect();
    let candles = series(&closes);
    let sma = [f64::NAN; 12];
    let h1 = h1_views(&[]);
    let mut spy = Spy {
        uses_regime: true,
        seen: Vec::new(),
    };
    let mut out = Vec::new();
    for hour in 0..(10 * 24) {
        let now_ms = T0 + hour as i64 * 3_600_000 + 3_600_000;
        let closed = ((now_ms - T0) / DAY) as usize;
        let view = RegimeView::new(&candles[..closed], &sma[..closed]).unwrap();
        let ctx = context(now_ms, &h1, Some(view)).unwrap();
        spy.on_bar(&ctx, &mut out);
    }
    assert!(!spy.seen.is_empty());
    for (open_ms, now_ms) in &spy.seen {
        assert!(*open_ms <= *now_ms - DAY, "{open_ms} vu à {now_ms}");
    }
    // Un moteur fautif qui exposerait la bougie du jour en cours est refusé par la garde.
    let now_ms = T0 + 5 * DAY + 3_600_000;
    let view = RegimeView::new(&candles[..6], &sma[..6]).unwrap();
    assert!(matches!(
        context(now_ms, &h1, Some(view)),
        Err(StrategyError::LookAhead { .. })
    ));
}
