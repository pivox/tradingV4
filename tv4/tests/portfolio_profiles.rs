//! Trois profils de coûts sur une préparation partagée (docs/specs/s10.md §9, §10 et §16, lot C) :
//! préparation faite une fois, profil identique au bit à un run isolé, curseur de régime neuf à chaque
//! profil, adverse différent d'un baseline re-tarifé, indépendance de l'ordre et des jambes, graine,
//! refus de configuration.
//!
//! Marché inventé : marches synthétiques (`common::synthetic_candles`) et paliers journaliers plats
//! pour la paire de régime. Aucune donnée réelle ; les valeurs attendues sont recalculées dans le test.

mod common;
mod portfolio_common;

use portfolio_common::{
    DAY, MINUTE, Scripted, T0, buy_quote, buy_risk, config, flat_bar, flat_series, instrument,
    names,
};
use tv4::engine::{
    CostSetup, EngineError, LoopObserver, NoopObserver, PortfolioConfig, PortfolioInputs,
    PortfolioResult, ProfileInputs, ProfilesInputs, ProfilesResult, run_portfolio_counted,
    run_portfolio_profile, run_portfolio_profiles,
};
use tv4::model::costs::{CostProfileName, CostsFile};
use tv4::model::{Candle, ExchangeId, FeeSchedule, Slippage, Timeframe};
use tv4::sim::{ExitReason, SimRules, SizingMode};
use tv4::store::Columns;
use tv4::strategy::{BarContext, IndicatorSpec, Signal, Strategy, StrategyFunnel, registry};

/// Paramètres d'un `sma_cross` 1m qui trade souvent sur une marche synthétique (warmup de 20 bougies).
const FAST_PARAMS: &str = "tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 2.0";

fn costs_file() -> CostsFile {
    CostsFile::load(&common::costs_c1_path()).expect("copie figée costs_c1.toml")
}

fn sma_cross(text: &str) -> Box<dyn Strategy> {
    let value = toml::from_str::<toml::Value>(text).expect("TOML valide");
    registry::build("sma_cross", &value).expect("stratégie valide")
}

/// Un jeu frais de `legs` stratégies construit par `make`, trois fois (un jeu par profil).
fn sets(legs: usize, make: &dyn Fn() -> Box<dyn Strategy>) -> [Vec<Box<dyn Strategy>>; 3] {
    let set = || (0..legs).map(|_| make()).collect::<Vec<_>>();
    [set(), set(), set()]
}

fn synthetic(symbol: &str, seed: u64, rows: usize) -> (String, Columns) {
    (
        symbol.to_string(),
        Columns::from_candles(&common::synthetic_candles(rows, seed, T0, 0.0)),
    )
}

/// Compte les passages de l'observateur autour de la boucle de chaque profil.
#[derive(Default)]
struct Loops {
    started: u32,
    finished: u32,
}

impl LoopObserver for Loops {
    fn loop_started(&mut self) {
        self.started += 1;
    }
    fn loop_finished(&mut self) {
        self.finished += 1;
    }
}

fn setup(file: &CostsFile, seed: u64) -> CostSetup<'_> {
    CostSetup {
        file,
        measures: None,
        seed,
    }
}

/// Lance les trois profils de `sma_cross` sur `series`, mode recherche.
fn profiles(
    series: &[(String, Columns)],
    universe: &[String],
    file: &CostsFile,
    seed: u64,
    params: &str,
) -> ProfilesResult {
    let instruments = vec![instrument(0.0); universe.len()];
    run_portfolio_profiles(
        ProfilesInputs {
            series,
            strategies: sets(universe.len(), &|| sma_cross(params)),
            config: config(universe, &instruments, 300.0),
            costs: setup(file, seed),
        },
        &mut NoopObserver,
    )
    .expect("profils valides")
}

// ---------------------------------------------------------------------------------------------
// Préparation partagée et curseur de régime
// ---------------------------------------------------------------------------------------------

/// Filtre de régime écrit pour le test : `sma_cross` dont les entrées sont retenues tant que la paire de
/// régime n'est pas haussière. Il lit le régime à chaque appel, donc un curseur qui ne repartirait pas
/// de zéro changerait les trades.
struct RegimeGated {
    inner: Box<dyn Strategy>,
}

impl Strategy for RegimeGated {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn version(&self) -> &'static str {
        self.inner.version()
    }
    fn params_json(&self) -> serde_json::Value {
        self.inner.params_json()
    }
    fn timeframes(&self) -> &[Timeframe] {
        self.inner.timeframes()
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        self.inner.indicators()
    }
    fn warmup_bars(&self) -> usize {
        self.inner.warmup_bars()
    }
    fn funnel(&self) -> Option<StrategyFunnel> {
        self.inner.funnel()
    }
    fn uses_regime(&self) -> bool {
        true
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        self.inner.on_bar(ctx, out);
        if ctx.regime().and_then(|view| view.bull()) != Some(true) {
            out.retain(|signal| !matches!(signal, Signal::EnterLong { .. }));
        }
    }
}

fn gated() -> Box<dyn Strategy> {
    Box::new(RegimeGated {
        inner: sma_cross(FAST_PARAMS),
    })
}

const REGIME_DAYS: i64 = 210;
/// Dernier jour de la fenêtre où la dernière 1d close de BTC est haussière (le jour 205 juge la clôture du
/// jour 204 ; le jour 206 juge celle du jour 205, effondrée).
const LAST_BULL_DAY: i64 = 205;

/// Clôture journalière de la paire de régime : haussière jusqu'au jour 204, puis effondrée.
fn regime_close(day: i64) -> f64 {
    if day < 205 { 100.0 + day as f64 } else { 50.0 }
}

fn regime_series() -> (String, Columns) {
    let mut candles: Vec<Candle> = Vec::with_capacity(usize::try_from(REGIME_DAYS * 1440).unwrap());
    for day in 0..REGIME_DAYS {
        let price = regime_close(day);
        for minute in 0..1440 {
            candles.push(flat_bar(T0 + day * DAY + minute * MINUTE, price, 1.0e6));
        }
    }
    ("BTC-USD".to_string(), Columns::from_candles(&candles))
}

/// Deux jambes synthétiques de 210 jours et une paire de régime hors de l'univers.
fn regime_data() -> Vec<(String, Columns)> {
    let rows = usize::try_from(REGIME_DAYS * 1440).unwrap();
    vec![
        synthetic("ETH-USD", 7, rows),
        synthetic("SOL-USD", 8, rows),
        regime_series(),
    ]
}

/// Résultat brut débarrassé de ce qui n'est pas comparable (rien) : le `Debug` d'un `f64` est son
/// aller-retour le plus court, deux résultats au même `Debug` sont identiques au bit.
fn fingerprint(result: &PortfolioResult) -> String {
    format!("{result:?}")
}

#[test]
fn la_preparation_est_faite_une_fois_pour_les_trois_profils() {
    let universe = names(&["ETH-USD", "SOL-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let data = regime_data();
    let file = costs_file();
    let mut observer = Loops::default();
    let all = run_portfolio_profiles(
        ProfilesInputs {
            series: &data,
            strategies: sets(2, &gated),
            config: config(&universe, &instruments, 300.0),
            costs: setup(&file, 1),
        },
        &mut observer,
    )
    .expect("profils valides");

    // Préparation : une fois pour trois profils.
    assert_eq!(all.prep.mtf_builds, 2, "un Mtf par jambe, pas 3 × 2");
    assert_eq!(all.prep.regime_series_built, 1);
    assert_eq!(all.prep.rank_tables_built, 1);
    assert_eq!(
        all.prep.candles_converted, 3,
        "deux jambes et la paire de régime"
    );
    // Les colonnes d'indicateurs : celles d'un seul run_portfolio.
    let single = run_portfolio_counted(
        PortfolioInputs {
            series: &data,
            strategies: vec![gated(), gated()],
            config: config(&universe, &instruments, 300.0),
        },
        &mut NoopObserver,
    )
    .expect("run isolé valide")
    .1;
    assert!(single.indicator_columns_computed > 0);
    assert_eq!(
        all.prep.indicator_columns_computed,
        single.indicator_columns_computed
    );
    assert_eq!(all.prep.mtf_builds, single.mtf_builds);
    assert_eq!(
        all.prep, single,
        "les compteurs d'un profil et de trois sont les mêmes"
    );
    // L'observateur entoure chacune des trois boucles.
    assert_eq!((observer.started, observer.finished), (3, 3));

    // Ordre fixe.
    let order: Vec<CostProfileName> = all.runs.iter().map(|run| run.profile).collect();
    assert_eq!(order, CostProfileName::ALL.to_vec());
}

#[test]
fn chaque_profil_est_identique_au_bit_a_un_run_isole_et_le_curseur_de_regime_repart_de_zero() {
    let universe = names(&["ETH-USD", "SOL-USD"]);
    let instruments = vec![instrument(0.0); 2];
    let data = regime_data();
    let file = costs_file();
    let all = run_portfolio_profiles(
        ProfilesInputs {
            series: &data,
            strategies: sets(2, &gated),
            config: config(&universe, &instruments, 300.0),
            costs: setup(&file, 1),
        },
        &mut NoopObserver,
    )
    .expect("profils valides");

    for run in &all.runs {
        let (isolated, prep) = run_portfolio_profile(
            ProfileInputs {
                series: &data,
                strategies: (0..2).map(|_| gated()).collect(),
                config: config(&universe, &instruments, 300.0),
                costs: setup(&file, 1),
            },
            run.profile,
            &mut NoopObserver,
        )
        .expect("run isolé valide");
        assert_eq!(prep.mtf_builds, 2);
        assert_eq!(
            fingerprint(&run.result),
            fingerprint(&isolated.result),
            "profil {}",
            run.profile
        );
        assert_eq!(run.legs, isolated.legs);
        assert_eq!(run.cost_by_leg, isolated.cost_by_leg);
        assert_eq!(run.result.config_hash, isolated.result.config_hash);
    }

    // Le régime est actif avec les profils : les entrées ne sont possibles que les jours haussiers de BTC
    // (jusqu'au jour 205), et il y en a.
    for run in &all.runs {
        let result = &run.result;
        let regime = result.regime.as_ref().expect("bloc régime");
        assert_eq!(regime.symbol, "BTC-USD");
        assert!(regime.days_bull > 0 && regime.days_bear > 0, "{regime:?}");
        // Le profil maker (TTL d'une minute, limit sous le bid) peut ne remplir aucune entrée.
        if run.profile != CostProfileName::Maker {
            assert!(!result.trades.is_empty(), "profil {}", run.profile);
        }
        for trade in &result.trades {
            assert!(
                trade.entry_signal_ms < T0 + (LAST_BULL_DAY + 1) * DAY,
                "profil {} : entrée le {} ms après le dernier jour haussier",
                run.profile,
                trade.entry_signal_ms - T0
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Graine, ordre de l'univers, indépendance des jambes
// ---------------------------------------------------------------------------------------------

const ROWS: usize = 10_240;
/// Peu de trades par jambe : le cash commun n'est jamais limitant, donc les jambes ne se couplent pas
/// (les tests d'indépendance ci-dessous en dépendent).
const SMALL_ROWS: usize = 2_500;

fn small_legs() -> Vec<(String, Columns)> {
    vec![
        synthetic("ETH-USD", 7, SMALL_ROWS),
        synthetic("SOL-USD", 8, SMALL_ROWS),
    ]
}

fn two_legs() -> Vec<(String, Columns)> {
    vec![synthetic("ETH-USD", 7, ROWS), synthetic("SOL-USD", 8, ROWS)]
}

#[test]
fn meme_graine_meme_resultat_et_une_autre_graine_change_un_prix_de_stop() {
    let universe = names(&["ETH-USD", "SOL-USD"]);
    let data = two_legs();
    let file = costs_file();
    let first = profiles(&data, &universe, &file, 1, FAST_PARAMS);
    let again = profiles(&data, &universe, &file, 1, FAST_PARAMS);
    let other = profiles(&data, &universe, &file, 2, FAST_PARAMS);
    for k in 0..3 {
        assert_eq!(
            fingerprint(&first.runs[k].result),
            fingerprint(&again.runs[k].result),
            "même graine, profil {}",
            first.runs[k].profile
        );
        assert_ne!(
            first.runs[k].result.config_hash, other.runs[k].result.config_hash,
            "la graine est dans le config_hash"
        );
    }
    // La graine ne change que les tirages : au moins un prix de stop diffère, sur le profil baseline.
    let stops = |run: &ProfilesResult| -> Vec<f64> {
        run.runs[0]
            .result
            .trades
            .iter()
            .filter(|trade| trade.exit_reason == ExitReason::Stop)
            .map(|trade| trade.exit_price)
            .collect()
    };
    let (a, b) = (stops(&first), stops(&other));
    assert!(
        a.len() >= 5,
        "le chemin du stop doit être exercé : {} stops",
        a.len()
    );
    assert_eq!(a.len(), b.len(), "les graines ne changent pas les signaux");
    assert!(
        a.iter().zip(&b).any(|(x, y)| x != y),
        "deux graines doivent donner au moins un prix de stop différent"
    );
}

/// Trades d'une jambe, dans l'ordre de génération.
fn trades_of(result: &PortfolioResult, symbol: &str) -> Vec<tv4::sim::Trade> {
    let leg = result
        .universe
        .iter()
        .position(|name| name == symbol)
        .expect("jambe de l'univers");
    result
        .trades
        .iter()
        .zip(&result.trade_legs)
        .filter(|&(_, &k)| usize::from(k) == leg)
        .map(|(trade, _)| {
            // `lot_id` est un compteur du portefeuille entier : il dépend des autres jambes.
            let mut trade = *trade;
            trade.lot_id = 0;
            trade
        })
        .collect()
}

#[test]
fn l_ordre_de_l_univers_ne_change_pas_les_trades_d_une_jambe() {
    let data = small_legs();
    let file = costs_file();
    let ab = profiles(
        &data,
        &names(&["ETH-USD", "SOL-USD"]),
        &file,
        1,
        FAST_PARAMS,
    );
    let ba = profiles(
        &data,
        &names(&["SOL-USD", "ETH-USD"]),
        &file,
        1,
        FAST_PARAMS,
    );
    for k in 0..3 {
        for symbol in ["ETH-USD", "SOL-USD"] {
            let (x, y) = (
                trades_of(&ab.runs[k].result, symbol),
                trades_of(&ba.runs[k].result, symbol),
            );
            if k < 2 {
                assert!(
                    !x.is_empty(),
                    "{symbol} doit trader (profil {})",
                    ab.runs[k].profile
                );
            }
            assert_eq!(x, y, "{symbol}, profil {}", ab.runs[k].profile);
        }
    }
}

#[test]
fn la_presence_d_une_autre_jambe_ne_change_pas_les_tirages_d_une_jambe() {
    let file = costs_file();
    let data = small_legs();
    let both = profiles(
        &data,
        &names(&["ETH-USD", "SOL-USD"]),
        &file,
        1,
        FAST_PARAMS,
    );
    let alone = profiles(&data[..1], &names(&["ETH-USD"]), &file, 1, FAST_PARAMS);
    for k in 0..3 {
        let with_b = trades_of(&both.runs[k].result, "ETH-USD");
        let solo = trades_of(&alone.runs[k].result, "ETH-USD");
        if k < 2 {
            assert!(!solo.is_empty());
        }
        // Même fenêtre : l'autre jambe démarre à la même minute (même longueur, même warmup).
        assert_eq!(
            with_b, solo,
            "ETH-USD seule ou avec SOL-USD, profil {}",
            both.runs[k].profile
        );
        // Mêmes tirages de stop : mêmes prix de sortie sur stop.
        let stop_prices = |trades: &[tv4::sim::Trade]| -> Vec<f64> {
            trades
                .iter()
                .filter(|t| t.exit_reason == ExitReason::Stop)
                .map(|t| t.exit_price)
                .collect()
        };
        assert_eq!(stop_prices(&with_b), stop_prices(&solo));
    }
}

// ---------------------------------------------------------------------------------------------
// §9.5 : l'adverse n'est pas le baseline re-tarifé
// ---------------------------------------------------------------------------------------------

/// Marché plat à 100 avec un coup de couteau à la minute 30 (low 90, close 96) qui touche le stop.
fn knife_market(len: i64) -> Vec<Candle> {
    let mut candles = flat_series(0, len, 100.0, 1.0e6);
    candles[30] = Candle {
        open_time_ms: T0 + 30 * MINUTE,
        open: 100.0,
        high: 100.0,
        low: 90.0,
        close: 96.0,
        volume: 1.0e6,
        trades: None,
    };
    candles
}

/// Un trade sur stop, puis une seconde entrée au montant fixé. Une stratégie par jeu, plan identique.
fn plan() -> Box<dyn Strategy> {
    let (strategy, _spy) = Scripted::new(vec![
        (T0 + 10 * MINUTE, buy_risk(0.01, 95.0)),
        (T0 + 50 * MINUTE, buy_quote(10.0)),
    ]);
    Box::new(strategy)
}

fn knife_run(min_qty: f64, file: &CostsFile) -> ProfilesResult {
    let universe = names(&["KNIFE-USD"]);
    let instruments = vec![instrument(min_qty)];
    let data = vec![(
        "KNIFE-USD".to_string(),
        Columns::from_candles(&knife_market(80)),
    )];
    run_portfolio_profiles(
        ProfilesInputs {
            series: &data,
            strategies: sets(1, &plan),
            config: PortfolioConfig {
                sizing: SizingMode::Compounding,
                ..config(&universe, &instruments, 1000.0)
            },
            costs: setup(file, 1),
        },
        &mut NoopObserver,
    )
    .expect("profils valides")
}

#[test]
fn l_adverse_n_est_pas_le_baseline_retarife() {
    let file = costs_file();

    // Étape 1, sans minimum d'ordre : les quantités de la seconde entrée dans chaque profil.
    let free = knife_run(0.0, &file);
    let (base_free, adverse_free) = (&free.runs[0].result.trades, &free.runs[1].result.trades);
    assert_eq!(base_free.len(), 2, "{base_free:?}");
    assert_eq!(adverse_free.len(), 2, "{adverse_free:?}");
    let (qb, qa) = (base_free[1].qty, adverse_free[1].qty);
    assert!(
        qa < qb,
        "un coût plus haut achète moins pour un même montant ({qa} contre {qb})"
    );

    // Étape 2 : un minimum d'ordre entre les deux quantités. Le baseline remplit la seconde entrée,
    // l'adverse la refuse faute de quantité.
    let min_qty = (qa + qb) / 2.0;
    let constrained = knife_run(min_qty, &file);
    let baseline = &constrained.runs[0];
    let adverse = &constrained.runs[1];
    assert_eq!(baseline.result.trades.len(), 2);
    assert_eq!(
        adverse.result.trades.len(),
        1,
        "un trade de moins en adverse"
    );
    assert!(
        adverse.result.sim.buys_rejected_below_minimum + adverse.result.rejections.below_minimum
            > 0,
        "la seconde entrée est refusée pour la quantité minimale"
    );

    // 1. La quantité du premier trade diffère (le sizing RiskPct dépend du prix de fill).
    let (b1, a1) = (&baseline.result.trades[0], &adverse.result.trades[0]);
    assert_ne!(b1.qty, a1.qty);
    assert_eq!(b1.exit_reason, ExitReason::Stop);
    assert_eq!(a1.exit_reason, ExitReason::Stop);

    // 3. Re-tarification du baseline : on garde la liste des trades et les quantités du baseline, et on
    // ne refait que les prix avec les coûts adverses, par les formules de §6. Elle a deux trades de
    // quantités baseline ; le run adverse en a un seul, d'une autre quantité, donc un autre PnL.
    let record = &adverse.legs[0];
    let h = record.half_spread_bps / 10_000.0;
    let s = record.market_slippage_bps / 10_000.0;
    let repriced_entry = b1.entry_ref_price * (1.0 + (h + s));
    assert_eq!(
        repriced_entry, a1.entry_price,
        "formule de §6 sur l'achat market"
    );
    let taker_fee = 0.0035;
    let repriced_pnl = b1.qty * (a1.exit_price - repriced_entry)
        - b1.qty * repriced_entry * 0.0035
        - b1.qty * a1.exit_price * taker_fee;
    assert_ne!(
        repriced_pnl, a1.pnl_net,
        "le PnL adverse n'est pas celui du baseline re-tarifé : la quantité a changé"
    );
    let repriced_count = baseline.result.trades.len();
    assert_ne!(repriced_count, adverse.result.trades.len());
    // La liste de trades diffère aussi à l'octet : quantité, prix de fill et frais.
    assert_ne!(
        (b1.qty, b1.entry_price, b1.entry_fee),
        (a1.qty, a1.entry_price, a1.entry_fee)
    );
}

// ---------------------------------------------------------------------------------------------
// Refus de configuration et d'entrée
// ---------------------------------------------------------------------------------------------

fn refused(
    tweak: impl FnOnce(&mut PortfolioConfig<'_>),
    strategies: [Vec<Box<dyn Strategy>>; 3],
) -> EngineError {
    let universe = names(&["ETH-USD"]);
    let instruments = vec![instrument(0.0)];
    let data = vec![synthetic("ETH-USD", 7, 2_000)];
    let file = costs_file();
    let mut cfg = config(&universe, &instruments, 300.0);
    tweak(&mut cfg);
    match run_portfolio_profiles(
        ProfilesInputs {
            series: &data,
            strategies,
            config: cfg,
            costs: setup(&file, 1),
        },
        &mut NoopObserver,
    ) {
        Ok(_) => panic!("la configuration aurait dû être refusée"),
        Err(error) => error,
    }
}

fn plain_sets() -> [Vec<Box<dyn Strategy>>; 3] {
    sets(1, &|| sma_cross(FAST_PARAMS))
}

#[test]
fn un_slippage_scalaire_est_refuse_avec_des_profils() {
    let error = refused(
        |cfg| cfg.slippage = Slippage::from_bps(5.0).unwrap(),
        plain_sets(),
    );
    assert!(
        matches!(error, EngineError::ScalarSlippageWithCosts { bps } if bps == 5.0),
        "{error}"
    );
}

#[test]
fn le_stress_du_simulateur_est_refuse_avec_des_profils() {
    let error = refused(
        |cfg| {
            cfg.sim_rules = SimRules {
                stop_stress_bps: 10.0,
                ..cfg.sim_rules
            }
        },
        plain_sets(),
    );
    assert!(
        matches!(error, EngineError::StopStressWithCosts { stop_stress_bps } if stop_stress_bps == 10.0),
        "{error}"
    );
}

#[test]
fn une_grille_sans_nom_est_refusee() {
    let error = refused(
        |cfg| cfg.fees = FeeSchedule::new(ExchangeId::Kraken, 25, 40).unwrap(),
        plain_sets(),
    );
    assert!(
        matches!(
            error,
            EngineError::NoFeeGrid {
                exchange: ExchangeId::Kraken
            }
        ),
        "{error}"
    );
}

#[test]
fn des_jeux_de_strategies_differents_sont_refuses() {
    // Paramètres différents dans le jeu adverse.
    let mut sets = plain_sets();
    sets[1] = vec![sma_cross(
        "tf = \"1m\"\nfast = 6\nslow = 20\nstop_atr_mult = 2.0",
    )];
    let error = refused(|_| {}, sets);
    assert!(
        matches!(error, EngineError::ProfileStrategiesDiffer),
        "{error}"
    );

    // Lecture du régime différente dans le jeu maker.
    let mut sets = plain_sets();
    sets[2] = vec![gated()];
    let error = refused(|_| {}, sets);
    assert!(
        matches!(error, EngineError::ProfileStrategiesDiffer),
        "{error}"
    );

    // Un jeu qui n'a pas une stratégie par jambe est refusé comme dans un run isolé.
    let mut sets = plain_sets();
    sets[1] = Vec::new();
    let error = refused(|_| {}, sets);
    assert!(
        matches!(
            error,
            EngineError::StrategyCountMismatch {
                strategies: 0,
                legs: 1
            }
        ),
        "{error}"
    );
}

/// Stratégie dont le TF de décision n'est pas déclaré.
struct BadDecision;

impl Strategy for BadDecision {
    fn id(&self) -> &'static str {
        "bad_decision"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M1]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn decision_tf(&self) -> Timeframe {
        Timeframe::M15
    }
    fn on_bar(&mut self, _ctx: &BarContext<'_>, _out: &mut Vec<Signal>) {}
}

#[test]
fn un_tf_de_decision_non_declare_est_refuse() {
    let error = refused(
        |_| {},
        sets(1, &|| Box::new(BadDecision) as Box<dyn Strategy>),
    );
    assert!(
        matches!(
            error,
            EngineError::DecisionTfNotDeclared {
                id: "bad_decision",
                tf: Timeframe::M15
            }
        ),
        "{error}"
    );
}

#[test]
fn les_stops_n_ont_pas_de_stress_dans_sim_rules_quand_les_profils_sont_valides() {
    // La configuration de référence des tests est acceptée : `stop_stress_bps` et le slippage valent zéro.
    let universe = names(&["ETH-USD", "SOL-USD"]);
    let data = two_legs();
    let file = costs_file();
    let all = profiles(&data, &universe, &file, 1, FAST_PARAMS);
    for run in &all.runs {
        assert_eq!(run.result.config.sim_rules.stop_stress_bps, 0.0);
        assert!(run.result.config.slippage_bps.is_none());
        assert!(run.result.config.fees.is_none());
        let costs = run.result.config.costs.as_ref().expect("bloc costs");
        assert_eq!(costs.profile, run.profile);
        assert_eq!(costs.seed, 1);
        // Chaque hash est celui de l'objet haché du run, et les trois sont distincts (critère 1).
        assert_eq!(run.result.config_hash, run.result.config.hash().unwrap());
    }
    let hashes: Vec<&str> = all
        .runs
        .iter()
        .map(|r| r.result.config_hash.as_str())
        .collect();
    assert!(hashes[0] != hashes[1] && hashes[1] != hashes[2] && hashes[0] != hashes[2]);
}

// ---------------------------------------------------------------------------------------------
// TF de décision
// ---------------------------------------------------------------------------------------------

#[test]
fn sma_cross_2tf_decide_en_15m_et_le_ttl_maker_suit() {
    let universe = names(&["ETH-USD"]);
    let instruments = vec![instrument(0.0)];
    let data = vec![synthetic("ETH-USD", 7, ROWS)];
    let file = costs_file();
    let all = run_portfolio_profiles(
        ProfilesInputs {
            series: &data,
            strategies: sets(1, &|| {
                common::strategy_from_file("sma_cross", "config/strategies/sma_cross_2tf.toml")
            }),
            config: config(&universe, &instruments, 300.0),
            costs: setup(&file, 1),
        },
        &mut NoopObserver,
    )
    .expect("profils valides");
    let strategy = common::strategy_from_file("sma_cross", "config/strategies/sma_cross_2tf.toml");
    assert_eq!(strategy.timeframes(), &[Timeframe::M15, Timeframe::H1]);
    assert_eq!(strategy.decision_tf(), Timeframe::M15);
    let maker = &all.runs[2];
    assert_eq!(maker.legs[0].decision_tf, Timeframe::M15);
    assert_eq!(maker.legs[0].maker_ttl_minutes, Some(15));
    assert_eq!(all.runs[0].legs[0].maker_ttl_minutes, None);
    // Le TF de décision est haché dans les trois runs.
    for run in &all.runs {
        let json = run.result.config.canonical_json().unwrap();
        assert!(json.contains(r#""decision_tf":"15m""#), "{json}");
    }
}
