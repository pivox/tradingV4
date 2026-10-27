//! Rapports de S8 : `metrics.json` (`tv4-metrics-s8`), `trades.csv` (36 colonnes), `equity.csv` et
//! `manifest.json` (docs/03-architecture.md §7.3, spécification S8 §2 à §6).
//!
//! Trois familles :
//!
//! * des scénarios écrits à la main sur des bougies 1m INVENTÉES (aucune donnée de marché) qui
//!   couvrent chaque `exit_type`, les horodatages d'ordre et l'entonnoir ;
//! * un run synthétique de plusieurs dizaines de trades dont chaque agrégat de `metrics.json` est
//!   recalculé ici, depuis `trades.csv` et l'equity, par un code indépendant de `report` ;
//! * le binaire `tv4 backtest` sur la fixture : quatre fichiers, schémas, clés, options.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::scripte::Scripte;
use common::{T0, kraken_btc_usd, okx_fees, repo_path, slippage};
use tv4::engine::{CellConfig, CellResult, run_cell};
use tv4::model::{Candle, ExchangeId, InstrumentRules, Side, Timeframe, hash};
use tv4::report::writers::{
    TRADES_COLUMNS, TRADES_COLUMNS_S6, TradesContext, project_s6, trades_csv, trades_csv_s6,
};
use tv4::report::{DataInfo, Metrics};
use tv4::sim::{ExitReason, SimRules, Trade};
use tv4::strategy::{OrderKind, Signal, Size};

mod common_helpers {}

const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const ZERO_HASH: &str = "blake3:0000000000000000000000000000000000000000000000000000000000000000";

// ---------------------------------------------------------------------------------------------
// Scénarios à la main
// ---------------------------------------------------------------------------------------------

type Ligne = (f64, f64, f64, f64);

fn bougies(lignes: &[Ligne]) -> Vec<Candle> {
    lignes
        .iter()
        .enumerate()
        .map(|(k, &(open, high, low, close))| Candle {
            open_time_ms: T0 + k as i64 * MINUTE,
            open,
            high,
            low,
            close,
            volume: 1.0e6,
            trades: None,
        })
        .collect()
}

const ST: [Ligne; 5] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.6, 99.7, 100.2),
    (100.5, 101.0, 100.0, 100.4),
    (100.4, 100.5, 98.5, 98.8),
    (98.8, 99.0, 98.0, 98.5),
];

const BASE: [Ligne; 6] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 101.0, 99.8, 100.8),
    (101.0, 102.0, 100.5, 101.5),
    (101.5, 103.0, 101.0, 102.5),
    (103.0, 104.0, 102.5, 103.5),
    (103.5, 104.0, 103.0, 103.8),
];

const FB: [Ligne; 5] = [
    ST[0],
    ST[1],
    (100.5, 103.5, 100.3, 101.0),
    (101.0, 103.2, 100.8, 102.0),
    (102.0, 102.5, 101.5, 102.0),
];

const FIFO: [Ligne; 7] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.2, 99.8, 100.0),
    (100.0, 100.2, 99.0, 99.5),
    (99.5, 100.0, 99.4, 99.8),
    (101.0, 102.0, 100.9, 101.5),
    (101.5, 103.5, 101.4, 103.0),
    (103.0, 103.2, 102.8, 103.0),
];

const LIM: [Ligne; 7] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.4, 97.5, 99.0),
    (99.0, 99.5, 98.2, 98.5),
    (98.5, 98.9, 98.0, 98.4),
    (98.4, 98.8, 98.05, 98.6),
    (98.6, 98.7, 97.0, 97.5),
    (97.5, 98.0, 97.0, 97.8),
];

/// Stop suiveur : le stop monte à 100,0 (effectif à l'open de la 1m 5) puis la 1m 6 le franchit.
const TRAIL: [Ligne; 8] = [
    (100.0, 100.5, 99.5, 100.0),
    (100.0, 100.6, 99.7, 100.2),
    (100.5, 101.0, 100.0, 100.4),
    (100.4, 100.8, 100.1, 100.6),
    (100.6, 101.0, 100.2, 100.8),
    (100.8, 101.0, 100.3, 100.5),
    (100.5, 100.6, 99.0, 99.2),
    (99.2, 99.5, 99.0, 99.3),
];

fn entree(quote: f64, limite: Option<(f64, u32)>, stop: Option<f64>, tp: Option<f64>) -> Signal {
    Signal::EnterLong {
        size: Size::Quote(quote),
        kind: match limite {
            Some((price, ttl_bars)) => OrderKind::Limit { price, ttl_bars },
            None => OrderKind::Market,
        },
        stop,
        take_profit: tp,
        tag: "entree",
    }
}

fn sortie() -> Signal {
    Signal::ExitLong {
        kind: OrderKind::Market,
        tag: "sortie",
    }
}

fn pose(side: Side, price: f64, size: Size, slot: u16) -> Signal {
    Signal::PlaceLimit {
        side,
        price,
        size,
        ttl_bars: 100,
        slot,
        tag: "grille",
    }
}

fn config(rules: SimRules) -> CellConfig<'static> {
    CellConfig {
        symbol: "TEST-USD",
        fees: okx_fees(),
        slippage: slippage(10.0),
        initial_cash: 1000.0,
        sim_rules: rules,
        instrument: InstrumentRules::new(0.01, 1e-8, 0.0, 0.0).expect("instrument valide"),
    }
}

/// Résultat, bougies et métriques d'un scénario écrit à la main.
fn jouer(lignes: &[Ligne], plan: Vec<(usize, Signal)>) -> (CellResult, Vec<Candle>, Metrics) {
    let candles = bougies(lignes);
    let result = run_cell(
        &candles,
        Box::new(Scripte::new(plan)),
        &config(SimRules::default()),
    )
    .expect("run valide");
    let metrics = metrics_de(&result, &candles);
    (result, candles, metrics)
}

fn metrics_de(result: &CellResult, candles: &[Candle]) -> Metrics {
    let data = DataInfo::fixture(ExchangeId::Kraken, "TEST-USD", candles).expect("données");
    let times: Vec<i64> = candles[result.start_index..]
        .iter()
        .map(|c| c.open_time_ms)
        .collect();
    Metrics::new(result, &times, data, ZERO_HASH.to_string()).expect("métriques")
}

fn index_1m(ms: i64) -> i64 {
    (ms - T0) / MINUTE
}

/// Invariants d'horodatage de `trades.csv` (spécification §3.3).
fn assert_horodatages(trades: &[Trade]) {
    for t in trades {
        assert!(
            t.entry_signal_ms <= t.entry_order_ms
                && t.entry_order_ms <= t.entry_ms
                && t.entry_ms <= t.exit_ms,
            "entrée : {t:?}"
        );
        assert!(
            t.exit_signal_ms <= t.exit_order_ms && t.exit_order_ms <= t.exit_ms,
            "sortie : {t:?}"
        );
    }
}

fn contexte(metrics: &Metrics) -> TradesContext<'_> {
    TradesContext {
        symbol: &metrics.data.symbol,
        span_ms: metrics.window.span_ms(),
    }
}

#[test]
fn un_stop_initial_est_ordonne_a_l_instant_du_fill_d_entree() {
    let (r, _, m) = jouer(&ST, vec![(0, entree(500.0, None, Some(99.0), None))]);
    let t = &r.trades[0];
    assert_eq!(t.exit_reason, ExitReason::Stop);
    assert!(!t.stop_moved);
    assert_eq!(t.stop_at_entry, Some(99.0));
    // Signal à la clôture de la 1m 0, ordre d'achat actif à l'open de la 1m 2 ; le stop est actif dès
    // la 1m de fill (03 §7.2 étape 5) et franchi à la 1m 3.
    assert_eq!(index_1m(t.entry_signal_ms), 1);
    assert_eq!(index_1m(t.entry_order_ms), 2);
    assert_eq!(index_1m(t.entry_ms), 2);
    assert_eq!(index_1m(t.exit_signal_ms), 1);
    assert_eq!(t.exit_order_ms, t.entry_ms);
    assert_eq!(index_1m(t.exit_ms), 3);
    assert_horodatages(&r.trades);
    assert_eq!(m.funnel.exits_by_type.stop_loss, 1);
    assert_eq!(m.distribution.by_exit_type[0].exit_type, "stop_loss");
}

#[test]
fn un_stop_deplace_est_un_trailing_stop_ordonne_a_l_activation_du_dernier_move_stop() {
    let (r, _, m) = jouer(
        &TRAIL,
        vec![
            (0, entree(500.0, None, Some(98.0), None)),
            (3, Signal::MoveStop { stop: 100.0 }),
        ],
    );
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.exit_reason, ExitReason::Stop);
    assert!(t.stop_moved);
    assert_eq!(
        t.stop_at_entry,
        Some(98.0),
        "le stop d'entrée reste celui de l'entrée"
    );
    assert_eq!(t.stop_at_exit, Some(100.0));
    assert_eq!(
        index_1m(t.exit_signal_ms),
        4,
        "signal à la clôture de la 1m 3"
    );
    assert_eq!(index_1m(t.exit_order_ms), 5, "effectif à l'open de la 1m 5");
    assert_eq!(index_1m(t.exit_ms), 6);
    assert_horodatages(&r.trades);
    assert_eq!(m.funnel.exits_by_type.trailing_stop, 1);
    assert_eq!(m.funnel.exits_by_type.stop_loss, 0);
    assert_eq!((m.funnel.other_signals, m.funnel.other_accepted), (1, 1));
    assert_eq!(r.sim.move_stops_applied, 1);
    let csv = String::from_utf8(trades_csv(&r.trades, &contexte(&m)).unwrap()).unwrap();
    assert!(
        csv.lines().nth(1).unwrap().contains(",trailing_stop,"),
        "{csv}"
    );
}

#[test]
fn un_take_profit_est_ordonne_a_l_activation_du_take_profit() {
    let (r, _, m) = jouer(&FB, vec![(0, entree(500.0, None, Some(99.0), Some(103.0)))]);
    let t = &r.trades[0];
    assert_eq!(t.exit_reason, ExitReason::TakeProfit);
    // Fill à la 1m 2, take-profit actif à la 1m 3 (celui de la 1m de fill est ignoré), exécuté à la 1m 3.
    assert_eq!(index_1m(t.entry_ms), 2);
    assert_eq!(index_1m(t.exit_order_ms), 3);
    assert_eq!(index_1m(t.exit_ms), 3);
    assert_horodatages(&r.trades);
    assert_eq!(m.funnel.exits_by_type.take_profit, 1);
}

#[test]
fn une_sortie_par_signal_est_ordonnee_a_l_activation_de_l_ordre() {
    let (r, _, m) = jouer(
        &BASE,
        vec![(0, entree(500.0, None, Some(95.0), None)), (2, sortie())],
    );
    let t = &r.trades[0];
    assert_eq!(t.exit_reason, ExitReason::Signal);
    assert_eq!(index_1m(t.exit_signal_ms), 3);
    assert_eq!(index_1m(t.exit_order_ms), 4);
    assert_eq!(index_1m(t.exit_ms), 4);
    assert_horodatages(&r.trades);
    assert_eq!(m.funnel.exits_by_type.strategy_exit, 1);
    assert_eq!((m.funnel.exit_signals, m.funnel.exit_orders), (1, 1));
    assert_eq!((m.funnel.entry_signals, m.funnel.entry_orders), (1, 1));
}

#[test]
fn une_vente_limit_et_la_cloture_forcee_sur_deux_lots_fifo() {
    let (r, _, m) = jouer(
        &FIFO,
        vec![
            (0, pose(Side::Buy, 100.0, Size::Quote(100.0), 1)),
            (0, pose(Side::Buy, 99.5, Size::Quote(99.5), 2)),
            (3, pose(Side::Sell, 103.0, Size::Quote(154.5), 3)),
        ],
    );
    assert_eq!(r.trades.len(), 3);
    assert_eq!(r.trades[0].exit_reason, ExitReason::LimitSell);
    assert_eq!(r.trades[1].exit_reason, ExitReason::LimitSell);
    assert_eq!(r.trades[2].exit_reason, ExitReason::ForcedClose);
    // La vente limit est posée à la clôture de la 1m 3 et active à l'open de la 1m 5.
    assert_eq!(index_1m(r.trades[0].exit_order_ms), 5);
    // Clôture forcée : ordre, signal et fill au même instant (clôture de la dernière 1m).
    let forcee = &r.trades[2];
    assert_eq!(forcee.exit_order_ms, forcee.exit_ms);
    assert_eq!(forcee.exit_signal_ms, forcee.exit_ms);
    assert_horodatages(&r.trades);
    // Sans stop, ni risque ni R.
    assert!(r.trades.iter().all(|t| t.stop_at_entry.is_none()));
    assert_eq!(m.trades_with_r, 0);
    assert_eq!(m.expectancy_r, None);
    assert_eq!(m.distribution.histogram_r, None);
    let exits = &m.funnel.exits_by_type;
    assert_eq!((exits.limit_sell, exits.end_of_window), (2, 1));
    // Deux lots ouverts, trois lignes de trades (le lot 1 est vendu en deux parts).
    assert_eq!((m.funnel.entry_fills, m.funnel.trades), (2, 3));
    // Les places d'achat sont des entrées, la place de vente une sortie.
    assert_eq!((m.funnel.entry_signals, m.funnel.exit_signals), (2, 1));
    let csv = String::from_utf8(trades_csv(&r.trades, &contexte(&m)).unwrap()).unwrap();
    for ligne in csv.lines().skip(1) {
        let champs: Vec<&str> = ligne.split(',').collect();
        assert_eq!(champs.len(), 36);
        assert_eq!((champs[31], champs[32], champs[33]), ("", "", ""));
    }
}

#[test]
fn une_entree_limit_expiree_tombe_dans_other_de_l_entonnoir() {
    let (r, _, m) = jouer(&LIM, vec![(0, entree(300.0, Some((98.0, 3)), None, None))]);
    assert!(r.trades.is_empty());
    assert_eq!(r.sim.limits_expired, 1);
    let f = &m.funnel;
    assert_eq!((f.entry_signals, f.entry_orders, f.entry_fills), (1, 1, 0));
    assert_eq!(f.entry_not_filled_by_reason.other, 1);
    assert_eq!(m.n_trades, 0);
    assert_eq!(m.expectancy_net, None);
    assert_eq!(m.win_rate, None);
    assert_eq!(m.profit_factor, None);
    assert_eq!(m.avg_holding_ms, None);
    assert_eq!((m.mfe_mean, m.mae_mean), (None, None));
    assert_eq!(m.verdict, "rejected_too_few_trades");
    assert_eq!(m.exposure.bars_in_position, 0);
    assert_eq!(m.max_drawdown.m1.pct, 0.0);
    assert_eq!(
        (m.max_drawdown.m1.peak_ms, m.max_drawdown.m1.trough_ms),
        (None, None)
    );
    assert_eq!(m.distribution.pnl_net_median, None);
    // Aucun trade : l'en-tête seul.
    let csv = trades_csv(&r.trades, &contexte(&m)).unwrap();
    assert_eq!(csv.iter().filter(|b| **b == b'\n').count(), 1);
}

#[test]
fn les_rejets_de_signaux_se_repartissent_par_classe_et_se_somment() {
    // `Place` d'achat sans quote suffisant (entrée refusée) et sortie sans position (autre refus).
    let (r, _, m) = jouer(
        &ST,
        vec![
            (0, pose(Side::Buy, 90.0, Size::Quote(5_000.0), 1)),
            (0, sortie()),
            (1, Signal::MoveStop { stop: 90.0 }),
        ],
    );
    let f = &m.funnel;
    assert_eq!(f.entry_rejected_by_reason.insufficient_quote, 1);
    assert_eq!(r.rejections.exit_without_position, 1);
    assert_eq!(r.rejections.move_stop_without_position, 1);
    assert_eq!(m.signals_rejected, 3);
    assert_eq!(m.signals.rejected_by_reason, r.rejections);
    assert_eq!(
        f.entry_signals + f.exit_signals + f.other_signals,
        m.signals.emitted
    );
    assert_eq!(
        f.entry_orders + f.exit_orders + f.other_accepted,
        m.signals.accepted
    );
    // Les sommes de classes redonnent les motifs : ce signal-ci n'est qu'une entrée refusée.
    assert_eq!(f.entry_orders, 0);
    assert_eq!(f.entry_not_filled_by_reason.other, 0);
}

#[test]
fn le_stop_non_positif_et_le_filtre_de_tendance_sont_declares_par_sma_cross() {
    // sma_cross 15m + tendance 1h sur la fixture : raw − filtrés = entrées émises.
    let candles = common::fixture_1m();
    let strategie = common::strategy_from_file("sma_cross", "config/strategies/sma_cross_2tf.toml");
    let config = CellConfig {
        symbol: "BTC-USD",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: 300.0,
        sim_rules: SimRules::default(),
        instrument: kraken_btc_usd(),
    };
    let r = run_cell(&candles, strategie, &config).unwrap();
    let m = metrics_de(&r, &candles);
    let f = &m.funnel;
    let raw = f
        .raw_entry_signals
        .expect("sma_cross déclare son entonnoir");
    let filtres: u64 = f
        .entry_filtered_by_reason
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(raw - filtres, f.entry_signals);
    // Sur la fixture la tendance 1h ne filtre aucune entrée : les deux compteurs sont déclarés à 0
    // (le filtrage effectif est couvert par les tests unitaires de `sma_cross`).
    assert_eq!(filtres, 0);
    assert_eq!(
        f.entry_filtered_by_reason.keys().collect::<Vec<_>>(),
        ["stop_not_positive", "trend_filter"]
    );
    assert_eq!(f.entry_filtered_by_reason["trend_filter"], 0);
}

#[test]
fn une_strategie_sans_entonnoir_donne_raw_null_et_filtres_vides() {
    let (_, _, m) = jouer(&ST, vec![(0, entree(500.0, None, Some(99.0), None))]);
    assert_eq!(m.funnel.raw_entry_signals, None);
    assert!(m.funnel.entry_filtered_by_reason.is_empty());
    let json = serde_json::to_value(&m).unwrap();
    assert!(json["funnel"]["raw_entry_signals"].is_null());
    assert_eq!(
        json["funnel"]["entry_filtered_by_reason"],
        serde_json::json!({})
    );
}

#[test]
fn la_projection_s6_redonne_le_rendu_de_s6_sur_les_scenarios() {
    for (lignes, plan) in [
        (&ST[..], vec![(0, entree(500.0, None, Some(99.0), None))]),
        (
            &BASE[..],
            vec![(0, entree(500.0, None, Some(95.0), None)), (2, sortie())],
        ),
    ] {
        let (r, _, m) = jouer(lignes, plan);
        let complet = trades_csv(&r.trades, &contexte(&m)).unwrap();
        assert_eq!(
            project_s6(&complet).unwrap(),
            trades_csv_s6(&r.trades).unwrap()
        );
    }
}

#[test]
fn le_tag_time_stop_du_writer_est_celui_de_strategy() {
    assert_eq!(
        tv4::report::writers::TIME_STOP_TAG,
        tv4::strategy::TIME_STOP_TAG
    );
}

#[test]
fn with_min_trades_remplace_le_seuil_et_recalcule_le_hash() {
    let (r, candles, m) = jouer(&ST, vec![(0, entree(500.0, None, Some(99.0), None))]);
    assert_eq!(r.config.min_trades, 30);
    assert_eq!(r.config.min_trades, tv4::engine::DEFAULT_MIN_TRADES);
    let ancien = r.config_hash.clone();
    let r2 = r.clone().with_min_trades(1).unwrap();
    assert_eq!(r2.config.min_trades, 1);
    assert_ne!(r2.config_hash, ancien);
    assert_eq!(r2.config.hash().unwrap(), r2.config_hash);
    // Remettre 30 redonne le hash d'origine.
    assert_eq!(r2.clone().with_min_trades(30).unwrap().config_hash, ancien);
    assert!(r.clone().with_min_trades(0).is_err());
    // Le verdict suit le seuil : 1 trade perdant.
    assert_eq!(m.verdict, "rejected_too_few_trades");
    let m2 = metrics_de(&r2, &candles);
    assert_eq!(m2.min_trades, 1);
    assert_eq!(m2.verdict, "rejected_negative_expectancy");
    assert_eq!(m2.config_hash, r2.config_hash);
    // Aucune valeur de résultat ne bouge.
    assert_eq!(m2.pnl_net.to_bits(), m.pnl_net.to_bits());
    assert_eq!(m2.n_trades, m.n_trades);
}

// ---------------------------------------------------------------------------------------------
// Run synthétique recalculé de façon indépendante
// ---------------------------------------------------------------------------------------------

/// Colonnes de `trades.csv` lues par nom.
struct Csv {
    entetes: Vec<String>,
    lignes: Vec<Vec<String>>,
}

impl Csv {
    fn lire(octets: &[u8]) -> Csv {
        let mut reader = csv::ReaderBuilder::new().from_reader(octets);
        let entetes = reader
            .headers()
            .unwrap()
            .iter()
            .map(str::to_string)
            .collect();
        let lignes = reader
            .records()
            .map(|r| r.unwrap().iter().map(str::to_string).collect())
            .collect();
        Csv { entetes, lignes }
    }

    fn colonne(&self, nom: &str) -> Vec<&str> {
        let k = self
            .entetes
            .iter()
            .position(|e| e == nom)
            .unwrap_or_else(|| panic!("colonne {nom} absente"));
        self.lignes.iter().map(|l| l[k].as_str()).collect()
    }

    fn f64s(&self, nom: &str) -> Vec<f64> {
        self.colonne(nom)
            .iter()
            .map(|v| v.parse().unwrap())
            .collect()
    }

    fn i64s(&self, nom: &str) -> Vec<i64> {
        self.colonne(nom)
            .iter()
            .map(|v| v.parse().unwrap())
            .collect()
    }

    /// Valeurs optionnelles : champ vide = `None`.
    fn options(&self, nom: &str) -> Vec<Option<f64>> {
        self.colonne(nom)
            .iter()
            .map(|v| (!v.is_empty()).then(|| v.parse().unwrap()))
            .collect()
    }
}

/// Année UTC d'un instant en millisecondes : algorithme « civil from days » de H. Hinnant, réécrit ici
/// pour ne pas dépendre de `store::time`.
fn annee(ms: i64) -> i32 {
    let jours = ms.div_euclid(86_400_000);
    let z = jours + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    i32::try_from(if m <= 2 { y + 1 } else { y }).unwrap()
}

fn mediane(valeurs: &[f64]) -> Option<f64> {
    if valeurs.is_empty() {
        return None;
    }
    let mut v = valeurs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// Drawdown maximal `(pct, quote, pic, creux)` d'une série `(instant, equity)`, pic initial inclus.
fn drawdown_max(initial: f64, serie: &[(i64, f64)]) -> (f64, f64, Option<i64>, Option<i64>) {
    let mut pic = initial;
    let mut pic_a = None;
    let mut meilleur = (0.0, 0.0, None, None);
    for &(t, e) in serie {
        if e > pic {
            pic = e;
            pic_a = Some(t);
        }
        let dd = 1.0 - e / pic;
        if dd > meilleur.0 {
            meilleur = (dd, pic - e, pic_a, Some(t));
        }
    }
    meilleur
}

fn run_synthetique() -> (CellResult, Vec<Candle>, Metrics) {
    let candles = common::synthetic_candles(30_000, 7, T0, 0.002);
    let params =
        toml::from_str::<toml::Value>("tf = \"1m\"\nfast = 5\nslow = 20\nstop_atr_mult = 1.5\n")
            .unwrap();
    let strategie = tv4::strategy::registry::build("sma_cross", &params).unwrap();
    let config = CellConfig {
        symbol: "SYN-USD",
        fees: okx_fees(),
        slippage: slippage(5.0),
        initial_cash: 1_000_000.0,
        sim_rules: SimRules {
            max_volume_share: 1e9,
            ..SimRules::default()
        },
        instrument: InstrumentRules::new(1e-4, 1e-8, 0.0, 0.0).unwrap(),
    };
    let result = run_cell(&candles, strategie, &config).unwrap();
    let metrics = metrics_de(&result, &candles);
    (result, candles, metrics)
}

#[test]
fn chaque_agregat_de_metrics_se_recalcule_depuis_les_csv() {
    let (result, candles, m) = run_synthetique();
    let csv_octets = trades_csv(&result.trades, &contexte(&m)).unwrap();
    let csv = Csv::lire(&csv_octets);
    let n = csv.lignes.len();
    assert!(
        n >= 30,
        "le run synthétique doit atteindre min_trades : {n} trades"
    );
    assert_eq!(csv.entetes, TRADES_COLUMNS);
    assert_eq!(m.n_trades as usize, n);
    assert_horodatages(&result.trades);

    let pnl = csv.f64s("pnl_net");
    let mut somme = 0.0;
    let (mut gagnes, mut perdus, mut gp, mut gl) = (0u64, 0u64, 0.0, 0.0);
    for &p in &pnl {
        somme += p;
        if p > 0.0 {
            gagnes += 1;
            gp += p;
        } else if p < 0.0 {
            perdus += 1;
            gl += p;
        }
    }
    let nf = n as f64;
    assert_eq!(m.expectancy_net, Some(somme / nf));
    assert_eq!((m.n_wins, m.n_losses), (gagnes, perdus));
    assert_eq!(m.win_rate, Some(gagnes as f64 / nf));
    assert_eq!((m.gross_profit, m.gross_loss), (gp, gl));
    assert_eq!(m.profit_factor, (gl != 0.0).then(|| gp / -gl));
    let duree: i64 = csv
        .i64s("exit_ms")
        .iter()
        .zip(csv.i64s("entry_ms"))
        .map(|(s, e)| s - e)
        .sum();
    assert_eq!(m.avg_holding_ms, Some(duree as f64 / nf));
    assert_eq!(csv.i64s("holding_ms").iter().sum::<i64>(), duree);
    let mfe: f64 = csv.f64s("mfe").iter().fold(0.0, |a, v| a + v);
    let mae: f64 = csv.f64s("mae").iter().fold(0.0, |a, v| a + v);
    assert_eq!((m.mfe_mean, m.mae_mean), (Some(mfe / nf), Some(mae / nf)));

    // R : défini quand le stop d'entrée est sous le prix d'entrée ; dénominateur sans frais.
    let rs: Vec<f64> = csv.options("r_multiple").into_iter().flatten().collect();
    assert_eq!(m.trades_with_r as usize, rs.len());
    assert!(rs.len() >= 30);
    let somme_r = rs.iter().fold(0.0, |a, v| a + v);
    assert_eq!(m.expectancy_r, Some(somme_r / rs.len() as f64));
    let (quantites, entrees_px) = (csv.f64s("qty"), csv.f64s("entry_price"));
    let (stops, risques, multiples) = (
        csv.options("stop_at_entry"),
        csv.options("risk_quote"),
        csv.options("r_multiple"),
    );
    for (k, p) in pnl.iter().enumerate() {
        let stop = stops[k].expect("stop à l'entrée");
        let risque = quantites[k] * (entrees_px[k] - stop);
        assert_eq!(risques[k], Some(risque));
        assert_eq!(multiples[k], Some(p / risque));
    }
    let d = &m.distribution;
    assert_eq!(d.pnl_net_mean, m.expectancy_net);
    assert_eq!(d.r_mean, m.expectancy_r);
    assert_eq!(d.pnl_net_median, mediane(&pnl));
    assert_eq!(d.r_median, mediane(&rs));

    // Histogramme des R : seaux de 0,5 sur [-5, 10[.
    let h = d.histogram_r.as_ref().unwrap();
    let mut seaux = vec![0u64; 30];
    let (mut sous, mut sur) = (0u64, 0u64);
    for &r in &rs {
        let k = (r * 2.0).floor() as i64 + 10;
        if k < 0 {
            sous += 1;
        } else if k >= 30 {
            sur += 1;
        } else {
            seaux[k as usize] += 1;
        }
    }
    assert_eq!((&h.counts, h.underflow, h.overflow), (&seaux, sous, sur));
    assert_eq!(h.counts.iter().sum::<u64>() + sous + sur, rs.len() as u64);

    // PnL sans les N meilleurs : retrait par tri décroissant, somme dans l'ordre d'origine.
    for w in &d.pnl_without_best {
        let n_retire = w.n as usize;
        let attendu = (n > n_retire).then(|| {
            let mut ordre: Vec<usize> = (0..n).collect();
            ordre.sort_by(|&a, &b| pnl[b].partial_cmp(&pnl[a]).unwrap().then(a.cmp(&b)));
            let retires: Vec<usize> = ordre[..n_retire].to_vec();
            (0..n)
                .filter(|k| !retires.contains(k))
                .fold(0.0, |s, k| s + pnl[k])
        });
        assert_eq!(w.pnl_net, attendu, "sans les {n_retire} meilleurs");
    }
    assert_eq!(
        d.pnl_without_best.iter().map(|w| w.n).collect::<Vec<_>>(),
        [1, 3, 5, 10]
    );

    // Par année et par type de sortie.
    let sorties = csv.i64s("exit_ms");
    let mut annees: Vec<i32> = sorties.iter().map(|&t| annee(t)).collect();
    annees.sort_unstable();
    annees.dedup();
    assert_eq!(d.by_year.len(), annees.len());
    for (ligne, a) in d.by_year.iter().zip(&annees) {
        assert_eq!(ligne.year, *a);
        let membres: Vec<usize> = (0..n).filter(|&k| annee(sorties[k]) == *a).collect();
        assert_eq!(ligne.n_trades as usize, membres.len());
        assert_eq!(
            ligne.n_wins as usize,
            membres.iter().filter(|&&k| pnl[k] > 0.0).count()
        );
        assert_eq!(ligne.pnl_net, membres.iter().fold(0.0, |s, &k| s + pnl[k]));
    }
    let types = csv.colonne("exit_type");
    let mut noms: Vec<&str> = types.clone();
    noms.sort_unstable();
    noms.dedup();
    assert_eq!(d.by_exit_type.len(), noms.len());
    for (ligne, nom) in d.by_exit_type.iter().zip(&noms) {
        assert_eq!(ligne.exit_type, *nom);
        let membres: Vec<usize> = (0..n).filter(|&k| types[k] == *nom).collect();
        assert_eq!(ligne.n_trades as usize, membres.len());
        assert_eq!(ligne.pnl_net, membres.iter().fold(0.0, |s, &k| s + pnl[k]));
    }
    let e = &m.funnel.exits_by_type;
    let compte = |nom: &str| types.iter().filter(|t| **t == nom).count() as u64;
    assert_eq!(e.stop_loss, compte("stop_loss"));
    assert_eq!(e.strategy_exit, compte("strategy_exit"));
    assert_eq!(e.end_of_window, compte("end_of_window"));
    assert_eq!(
        e.end_of_window
            + e.limit_sell
            + e.stop_loss
            + e.strategy_exit
            + e.take_profit
            + e.time_stop
            + e.trailing_stop,
        n as u64
    );

    // Entonnoir.
    let mut lots = csv.i64s("lot_id");
    lots.sort_unstable();
    lots.dedup();
    assert_eq!(m.funnel.entry_fills as usize, lots.len());
    assert_eq!(m.funnel.trades as usize, n);
    let f = &m.funnel;
    assert_eq!(
        f.evaluations,
        candles.len() as u64 - result.start_index as u64
    );
    let rejet_entree = f.entry_rejected_by_reason;
    assert_eq!(
        f.entry_signals,
        f.entry_orders
            + rejet_entree.pending_full
            + rejet_entree.entry_while_long
            + rejet_entree.risk_without_stop
            + rejet_entree.invalid_risk_sizing
            + rejet_entree.invalid_size
            + rejet_entree.invalid_price
            + rejet_entree.invalid_ttl
            + rejet_entree.invalid_slot
            + rejet_entree.place_limit_risk_pct
            + rejet_entree.slot_already_open
            + rejet_entree.insufficient_quote
            + rejet_entree.below_minimum
    );
    let non_remplis = &f.entry_not_filled_by_reason;
    assert_eq!(
        f.entry_orders,
        f.entry_fills
            + non_remplis.insufficient_cash
            + non_remplis.below_minimum
            + non_remplis.volume_share
            + non_remplis.invalid_risk_at_fill
            + non_remplis.dropped_while_long
            + non_remplis.other
    );
    assert_eq!(
        f.entry_signals + f.exit_signals + f.other_signals,
        m.signals.emitted
    );
    assert_eq!(
        f.entry_orders + f.exit_orders + f.other_accepted,
        m.signals.accepted
    );
    let raw = f.raw_entry_signals.unwrap();
    let filtres: u64 = f
        .entry_filtered_by_reason
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(raw - filtres, f.entry_signals);

    // Exposition : 1m de la fenêtre dont l'open est dans [entrée, sortie[ d'un trade (un seul lot à la
    // fois avec sma_cross).
    let fenetre = &candles[result.start_index..];
    let entrees = csv.i64s("entry_ms");
    let en_position = fenetre
        .iter()
        .filter(|c| (0..n).any(|k| entrees[k] <= c.open_time_ms && c.open_time_ms < sorties[k]))
        .count() as u64;
    assert_eq!(m.exposure.bars_in_position, en_position);
    assert_eq!(m.exposure.bars_in_window, fenetre.len() as u64);
    assert_eq!(
        m.exposure.fraction,
        en_position as f64 / fenetre.len() as f64
    );
    assert!(en_position > 0);

    // Rendement.
    assert_eq!(m.return_total, result.equity_final / 1_000_000.0 - 1.0);

    // Drawdown 1m, puis 1h (dernière 1m de chaque heure, pic initial inclus).
    let serie: Vec<(i64, f64)> = fenetre
        .iter()
        .zip(&result.equity)
        .map(|(c, e)| (c.open_time_ms + MINUTE, *e))
        .collect();
    let (pct, quote, pic, creux) = drawdown_max(1_000_000.0, &serie);
    let dd = m.max_drawdown.m1;
    assert_eq!(
        (dd.pct, dd.quote, dd.peak_ms, dd.trough_ms),
        (pct, quote, pic, creux)
    );
    assert!(pct > 0.0);
    let mut horaire: Vec<(i64, f64)> = Vec::new();
    for (k, c) in fenetre.iter().enumerate() {
        let derniere = fenetre
            .get(k + 1)
            .is_none_or(|s| s.open_time_ms.div_euclid(HOUR) != c.open_time_ms.div_euclid(HOUR));
        if derniere {
            horaire.push(serie[k]);
        }
    }
    let (pct, quote, pic, creux) = drawdown_max(1_000_000.0, &horaire);
    let dh = m.max_drawdown.h1;
    assert_eq!(
        (dh.pct, dh.quote, dh.peak_ms, dh.trough_ms),
        (pct, quote, pic, creux)
    );
    assert!(m.max_drawdown.h1.pct <= m.max_drawdown.m1.pct);

    // Verdict : assez de trades, espérance nette signée.
    let attendu = if somme / nf > 0.0 {
        "ok"
    } else {
        "rejected_negative_expectancy"
    };
    assert_eq!(m.verdict, attendu);

    // Le bloc `approx` : sur 30 000 minutes (≈ 21 jours) l'annualisation est indéfinie.
    assert!(m.approx.ln_return.is_some() || result.equity_final <= 0.0);
    assert_eq!(m.approx.return_annualized, None);
}

#[test]
fn le_fichier_equity_recoupe_les_drawdowns_et_les_pas() {
    let (result, candles, m) = run_synthetique();
    let fenetre = &candles[result.start_index..];
    let times: Vec<i64> = fenetre.iter().map(|c| c.open_time_ms).collect();
    let initial = 1_000_000.0;
    let en_1m = |step| {
        Csv::lire(&tv4::report::equity::equity_csv(&times, &result.equity, initial, step).unwrap())
    };
    let minute = en_1m(Timeframe::M1);
    assert_eq!(minute.lignes.len(), fenetre.len());
    let horaire = en_1m(Timeframe::H1);
    let jour = en_1m(Timeframe::D1);
    // Chaque ligne d'un pas plus grand est une ligne du pas 1m, valeurs comprises.
    let par_temps: std::collections::BTreeMap<i64, (String, String)> = minute
        .lignes
        .iter()
        .map(|l| (l[0].parse().unwrap(), (l[1].clone(), l[2].clone())))
        .collect();
    for fichier in [&horaire, &jour] {
        assert_eq!(fichier.entetes, ["time_ms", "equity", "drawdown"]);
        for l in &fichier.lignes {
            let t: i64 = l[0].parse().unwrap();
            assert_eq!(par_temps[&t], (l[1].clone(), l[2].clone()));
        }
    }
    assert!(horaire.lignes.len() < minute.lignes.len() && jour.lignes.len() < horaire.lignes.len());
    // Une ligne par heure UTC contenant au moins une 1m.
    let mut heures: Vec<i64> = times.iter().map(|t| t.div_euclid(HOUR)).collect();
    heures.dedup();
    assert_eq!(horaire.lignes.len(), heures.len());
    // Instants : la clôture de la dernière 1m du seau, jamais postérieure aux données.
    for (l, h) in horaire.lignes.iter().zip(&heures) {
        let t: i64 = l[0].parse().unwrap();
        assert!(
            t > h * HOUR && t <= (h + 1) * HOUR + MINUTE,
            "{t} pour l'heure {h}"
        );
    }
    // Le drawdown du fichier 1m est celui des métriques ; il est ≤ 0 et sans zéro signé.
    let pire = minute
        .f64s("drawdown")
        .into_iter()
        .fold(0.0_f64, |a, v| if v < a { v } else { a });
    assert_eq!(-pire, m.max_drawdown.m1.pct);
    for v in minute.colonne("drawdown") {
        assert!(
            !v.starts_with("-0") || v.starts_with("-0."),
            "zéro signé : {v}"
        );
        assert!(v.parse::<f64>().unwrap() <= 0.0);
    }
    // Le dernier point de la série est la dernière 1m, avant la clôture forcée.
    let derniere = minute.lignes.last().unwrap();
    let fin = fenetre.last().unwrap().open_time_ms + MINUTE;
    assert_eq!(derniere[0].parse::<i64>().unwrap(), fin);
    assert_eq!(
        derniere[1].parse::<f64>().unwrap(),
        *result.equity.last().unwrap()
    );
}

// ---------------------------------------------------------------------------------------------
// Le binaire sur la fixture
// ---------------------------------------------------------------------------------------------

fn tv4(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(args)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

fn repo(relative: &str) -> String {
    repo_path(relative).display().to_string()
}

/// Entrées communes : fixture, paramètres, fichiers de configuration du dépôt.
fn entrees(params: &str) -> Vec<String> {
    [
        "--fixture",
        &repo("fixtures/kraken_xbtusd_1m.csv"),
        "--strategy",
        "sma_cross",
        "--params",
        &repo(params),
        "--fees-file",
        &repo("config/fees.toml"),
        "--instruments-file",
        &repo("config/instruments.toml"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn backtest(params: &str, extra: &[&str]) -> Output {
    let mut args = vec!["backtest".to_string()];
    args.extend(entrees(params));
    args.extend(extra.iter().map(|s| s.to_string()));
    tv4(&args)
}

fn ok(sortie: &Output) {
    assert_eq!(
        sortie.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&sortie.stderr)
    );
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn blake3_prefixe(path: &Path) -> String {
    hash::blake3_prefixed(&std::fs::read(path).unwrap())
}

const PARAMS_15M: &str = "config/strategies/sma_cross.toml";
const PARAMS_1M: &str = "config/strategies/sma_cross_1m.toml";

#[test]
fn la_fixture_produit_quatre_fichiers_coherents() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    let sortie = backtest(PARAMS_15M, &["--out", &out.display().to_string()]);
    ok(&sortie);
    for nom in ["metrics.json", "trades.csv", "equity.csv", "manifest.json"] {
        assert!(out.join(nom).is_file(), "{nom} absent");
        assert!(!out.join(format!("{nom}.tmp")).exists());
    }
    let metrics = json(&out.join("metrics.json"));
    let manifeste = json(&out.join("manifest.json"));
    assert_eq!(metrics["schema"], "tv4-metrics-s8");
    assert_eq!(manifeste["schema"], "tv4-manifest-s8");

    // Identité : mêmes hashes des deux côtés, run_id recalculé.
    assert_eq!(manifeste["config_hash"], metrics["config_hash"]);
    assert_eq!(manifeste["dataset_hash"], metrics["dataset_hash"]);
    assert_eq!(manifeste["verdict"], metrics["verdict"]);
    assert_eq!(manifeste["config"], metrics["config"]);
    assert_eq!(manifeste["hash_format"], "dsh-v1");
    let debut = manifeste["window"]["start_ms"].as_i64().unwrap();
    let fin = manifeste["window"]["end_ms"].as_i64().unwrap();
    assert_eq!(
        debut,
        metrics["window"]["first_open_time_ms"].as_i64().unwrap()
    );
    assert_eq!(
        fin,
        metrics["window"]["last_open_time_ms"].as_i64().unwrap() + MINUTE
    );
    assert_eq!(
        manifeste["run_id"].as_str().unwrap(),
        tv4::report::run_id(
            metrics["config_hash"].as_str().unwrap(),
            metrics["dataset_hash"].as_str().unwrap(),
            debut,
            fin
        )
    );
    assert_eq!(manifeste["window"]["start"], "2020-01-01T12:30:00Z");
    assert_eq!(manifeste["window"]["end"], "2020-01-08T21:30:00Z");

    // Empreintes des trois fichiers, result_hash = trades.csv.
    let fichiers = &manifeste["outputs"]["files"];
    for nom in ["metrics.json", "trades.csv", "equity.csv"] {
        assert_eq!(fichiers[nom], blake3_prefixe(&out.join(nom)), "{nom}");
    }
    assert_eq!(
        manifeste["result_hash"],
        blake3_prefixe(&out.join("trades.csv"))
    );
    assert_eq!(manifeste["outputs"]["equity_step"], "1h");
    assert_eq!(fichiers.as_object().unwrap().len(), 3);

    // Données : fixture, BLAKE3 du CSV (hex nu), rien de plus que le manifeste ne doit porter.
    let data = &manifeste["data"];
    assert_eq!(data["source"], "fixture");
    assert!(data["dataset"].is_null());
    assert_eq!(data["exchange"], "kraken");
    assert_eq!(
        data["fixture_blake3"],
        hash::blake3_hex(&std::fs::read(repo_path("fixtures/kraken_xbtusd_1m.csv")).unwrap())
    );
    assert_eq!(data["bars"], 10_000);

    // Moteur : tout ce que build.rs produit, et le hash du binaire de ce test.
    let moteur = &manifeste["engine"];
    assert_eq!(moteur["tv4_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(moteur["git_sha"], env!("TV4_GIT_SHA"));
    assert_eq!(moteur["rustc"], "1.97.0");
    assert_eq!(moteur["rustc_commit"].as_str().unwrap().len(), 40);
    assert!(matches!(
        moteur["profile"].as_str(),
        Some("debug" | "release")
    ));
    let hash_binaire = moteur["binary_hash"].as_str().unwrap();
    assert!(hash_binaire.starts_with("blake3:") && hash_binaire.len() == 7 + 64);
    assert_eq!(
        hash_binaire,
        blake3_prefixe(Path::new(env!("CARGO_BIN_EXE_tv4")))
    );
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(moteur["target_cpu"], "x86-64-v3");
        assert_eq!(moteur["target"], "x86_64-unknown-linux-gnu");
    }

    // Durées et date : entiers, `cli_cold_ms` englobe le chargement et le run.
    let t = &manifeste["timings"];
    let (charge, run, froid) = (
        t["load_ms"].as_u64().unwrap(),
        t["run_ms"].as_u64().unwrap(),
        t["cli_cold_ms"].as_u64().unwrap(),
    );
    assert!(charge + run <= froid + 1, "{charge} + {run} > {froid}");
    let debut_run = manifeste["started_at"].as_str().unwrap();
    assert!(debut_run.starts_with("20") && debut_run.ends_with('Z') && debut_run.len() == 20);

    // Aucun chemin absolu ni nom de dossier temporaire dans aucun fichier.
    let racine = repo_path("").canonicalize().unwrap().display().to_string();
    for nom in ["metrics.json", "trades.csv", "equity.csv", "manifest.json"] {
        let texte = std::fs::read_to_string(out.join(nom)).unwrap();
        assert!(
            !texte.contains(&dossier.path().display().to_string()),
            "{nom}"
        );
        assert!(
            !texte.contains(&racine) && !texte.contains("/home/"),
            "{nom}"
        );
    }

    // Sortie standard : une ligne, avec le run_id, le nombre de trades et le verdict.
    let stdout = String::from_utf8_lossy(&sortie.stdout);
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(
        stdout.starts_with(&format!(
            "run {} écrit dans ",
            manifeste["run_id"].as_str().unwrap()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains("2 trades, PnL net -4.52, verdict rejected_too_few_trades"),
        "{stdout}"
    );
}

#[test]
fn metrics_json_porte_toutes_les_cles_du_schema() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    ok(&backtest(
        PARAMS_15M,
        &["--out", &out.display().to_string()],
    ));
    let m = json(&out.join("metrics.json"));
    let racine: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        racine,
        [
            "approx",
            "avg_holding_ms",
            "cash_final",
            "config",
            "config_hash",
            "data",
            "dataset_hash",
            "distribution",
            "equity_final",
            "expectancy_net",
            "expectancy_r",
            "exposure",
            "fees_total",
            "fills",
            "forced_close",
            "funnel",
            "gross_loss",
            "gross_profit",
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
            "profit_factor",
            "return_total",
            "schema",
            "signals",
            "signals_rejected",
            "sim",
            "sim_deviations",
            "slippage_total",
            "trades_with_r",
            "verdict",
            "win_rate",
            "window"
        ]
    );
    let cles =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        cles(&m["data"]),
        [
            "bars",
            "dataset",
            "exchange",
            "first_open_time_ms",
            "gaps_policy",
            "last_open_time_ms",
            "minutes_missing",
            "source",
            "symbol",
            "synthetic_bars",
            "tf"
        ]
    );
    assert_eq!(cles(&m["max_drawdown"]), ["h1", "m1"]);
    assert_eq!(
        cles(&m["max_drawdown"]["m1"]),
        ["pct", "peak_ms", "quote", "trough_ms"]
    );
    assert_eq!(
        cles(&m["exposure"]),
        ["bars_in_position", "bars_in_window", "fraction"]
    );
    assert_eq!(
        cles(&m["funnel"]),
        [
            "entry_fills",
            "entry_filtered_by_reason",
            "entry_not_filled_by_reason",
            "entry_orders",
            "entry_rejected_by_reason",
            "entry_signals",
            "evaluations",
            "exit_orders",
            "exit_signals",
            "exits_by_type",
            "other_accepted",
            "other_signals",
            "raw_entry_signals",
            "trades"
        ]
    );
    assert_eq!(
        cles(&m["funnel"]["entry_not_filled_by_reason"]),
        [
            "below_minimum",
            "dropped_while_long",
            "insufficient_cash",
            "invalid_risk_at_fill",
            "other",
            "volume_share"
        ]
    );
    assert_eq!(
        cles(&m["distribution"]),
        [
            "by_exit_type",
            "by_year",
            "histogram_r",
            "pnl_net_mean",
            "pnl_net_median",
            "pnl_without_best",
            "r_mean",
            "r_median"
        ]
    );
    assert_eq!(
        cles(&m["approx"]),
        ["return_annualized", "ln_return", "ulp_tolerance"]
            .map(String::from)
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(cles(&m["config"]).len(), 9);
    assert!(m["config"].get("min_trades").is_some());
    assert!(m.get("engine").is_none());
    assert_eq!(m["funnel"]["raw_entry_signals"], 10);
    assert_eq!(m["funnel"]["entry_signals"], 10);
    assert_eq!(m["funnel"]["entry_orders"], 10);
    assert_eq!(m["funnel"]["entry_fills"], 2);
    assert_eq!(m["funnel"]["entry_not_filled_by_reason"]["volume_share"], 8);
    assert_eq!(m["funnel"]["evaluations"], 708);
    assert_eq!(m["data"]["minutes_missing"], 1_370);
    assert_eq!(m["signals_rejected"], 0);
    assert_eq!(m["trades_with_r"], 2);
}

#[test]
fn l_entonnoir_et_les_horodatages_tiennent_sur_la_fixture() {
    let dossier = tempfile::tempdir().unwrap();
    for params in [
        PARAMS_1M,
        PARAMS_15M,
        "config/strategies/sma_cross_2tf.toml",
    ] {
        let out = dossier.path().join(params.replace('/', "_"));
        ok(&backtest(params, &["--out", &out.display().to_string()]));
        let m = json(&out.join("metrics.json"));
        let f = &m["funnel"];
        let u = |v: &serde_json::Value| v.as_u64().unwrap();
        assert_eq!(
            u(&f["entry_signals"]) + u(&f["exit_signals"]) + u(&f["other_signals"]),
            u(&m["signals"]["emitted"]),
            "{params}"
        );
        assert_eq!(
            u(&f["entry_orders"]) + u(&f["exit_orders"]) + u(&f["other_accepted"]),
            u(&m["signals"]["accepted"]),
            "{params}"
        );
        let reste: u64 = f["entry_not_filled_by_reason"]
            .as_object()
            .unwrap()
            .values()
            .map(u)
            .sum();
        assert_eq!(
            u(&f["entry_orders"]) - u(&f["entry_fills"]),
            reste,
            "{params}"
        );
        let motifs = m["signals"]["rejected_by_reason"].as_object().unwrap();
        for (motif, total) in motifs {
            assert!(f["entry_rejected_by_reason"][motif].as_u64().unwrap() <= u(total));
        }
        assert_eq!(
            u(&f["raw_entry_signals"])
                - f["entry_filtered_by_reason"]
                    .as_object()
                    .unwrap()
                    .values()
                    .map(u)
                    .sum::<u64>(),
            u(&f["entry_signals"]),
            "{params}"
        );
        let csv = Csv::lire(&std::fs::read(out.join("trades.csv")).unwrap());
        assert_eq!(csv.entetes, TRADES_COLUMNS);
        assert_eq!(csv.entetes[..25], TRADES_COLUMNS_S6);
        for k in 0..csv.lignes.len() {
            let (s, o, e) = (
                csv.i64s("entry_signal_ms")[k],
                csv.i64s("entry_order_ms")[k],
                csv.i64s("entry_ms")[k],
            );
            assert!(
                s <= o && o <= e && e <= csv.i64s("exit_ms")[k],
                "{params} ligne {k}"
            );
            assert!(
                csv.i64s("exit_signal_ms")[k] <= csv.i64s("exit_order_ms")[k]
                    && csv.i64s("exit_order_ms")[k] <= csv.i64s("exit_ms")[k],
                "{params} ligne {k}"
            );
            assert_eq!(csv.colonne("symbol")[k], "BTC-USD");
            assert_eq!(csv.colonne("regime_at_entry")[k], "");
            assert_eq!(csv.colonne("spread_cost")[k], "");
        }
        // La projection du fichier écrit est le rendu S6.
        let complet = std::fs::read(out.join("trades.csv")).unwrap();
        let projete = project_s6(&complet).unwrap();
        assert_eq!(
            projete[..projete.iter().position(|b| *b == b'\n').unwrap()],
            TRADES_COLUMNS_S6.join(",").into_bytes()
        );
    }
}

#[test]
fn equity_step_1m_donne_une_ligne_par_1m_plus_l_en_tete() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    ok(&backtest(
        PARAMS_15M,
        &["--out", &out.display().to_string(), "--equity-step", "1m"],
    ));
    let m = json(&out.join("metrics.json"));
    let bars = m["window"]["bars"].as_u64().unwrap() as usize;
    let texte = std::fs::read_to_string(out.join("equity.csv")).unwrap();
    assert_eq!(texte.lines().count(), bars + 1);
    assert_eq!(texte.lines().next(), Some("time_ms,equity,drawdown"));
    assert_eq!(
        json(&out.join("manifest.json"))["outputs"]["equity_step"],
        "1m"
    );
    // Le pas par défaut (1h) a moins de lignes, une par heure UTC présente dans la fenêtre.
    let out_h = dossier.path().join("run_h");
    ok(&backtest(
        PARAMS_15M,
        &["--out", &out_h.display().to_string()],
    ));
    let lignes_h = std::fs::read_to_string(out_h.join("equity.csv"))
        .unwrap()
        .lines()
        .count();
    assert!(
        lignes_h > 100 && lignes_h < bars / 30,
        "{lignes_h} lignes pour {bars} 1m"
    );
    // Une valeur de pas inconnue est une erreur d'usage.
    let sortie = backtest(
        PARAMS_15M,
        &["--out", &out.display().to_string(), "--equity-step", "7m"],
    );
    assert_eq!(sortie.status.code(), Some(2));
    // Le pas n'entre ni dans le config_hash ni dans metrics.json.
    assert_eq!(
        json(&out.join("metrics.json"))["config_hash"],
        json(&out_h.join("metrics.json"))["config_hash"]
    );
    assert_eq!(
        std::fs::read(out.join("metrics.json")).unwrap(),
        std::fs::read(out_h.join("metrics.json")).unwrap()
    );
    assert_eq!(
        std::fs::read(out.join("trades.csv")).unwrap(),
        std::fs::read(out_h.join("trades.csv")).unwrap()
    );
}

#[test]
fn min_trades_est_dans_le_config_hash_et_pilote_le_verdict() {
    let dossier = tempfile::tempdir().unwrap();
    let defaut = dossier.path().join("defaut");
    ok(&backtest(
        PARAMS_15M,
        &["--out", &defaut.display().to_string()],
    ));
    let un = dossier.path().join("un");
    ok(&backtest(
        PARAMS_15M,
        &["--out", &un.display().to_string(), "--min-trades", "1"],
    ));
    let (a, b) = (
        json(&defaut.join("metrics.json")),
        json(&un.join("metrics.json")),
    );
    assert_ne!(a["config_hash"], b["config_hash"]);
    assert_eq!(
        (a["min_trades"].as_u64(), b["min_trades"].as_u64()),
        (Some(30), Some(1))
    );
    assert_eq!(a["verdict"], "rejected_too_few_trades");
    assert_eq!(b["verdict"], "rejected_negative_expectancy");
    assert_eq!(b["config"]["min_trades"], 1);
    // Mêmes résultats, seuls le seuil, le hash et le verdict changent.
    for cle in [
        "pnl_net",
        "n_trades",
        "cash_final",
        "expectancy_net",
        "fees_total",
    ] {
        assert_eq!(a[cle], b[cle], "{cle}");
    }
    // Les deux runs diffèrent donc aussi par run_id.
    assert_ne!(
        json(&defaut.join("manifest.json"))["run_id"],
        json(&un.join("manifest.json"))["run_id"]
    );
    // `--min-trades 0` est une erreur d'usage et n'écrit rien.
    let zero = dossier.path().join("zero");
    let sortie = backtest(
        PARAMS_15M,
        &["--out", &zero.display().to_string(), "--min-trades", "0"],
    );
    assert_eq!(sortie.status.code(), Some(2));
    assert!(!zero.exists());
}

#[test]
fn out_par_defaut_est_sous_root_runs_run_id() {
    let racine = tempfile::tempdir().unwrap();
    let r = racine.path().display().to_string();
    let sortie = backtest(PARAMS_15M, &["--root", &r]);
    ok(&sortie);
    let stdout = String::from_utf8_lossy(&sortie.stdout).into_owned();
    let run_id = stdout.split_whitespace().nth(1).unwrap().to_string();
    assert_eq!(run_id.len(), 64);
    let dossier: PathBuf = racine.path().join("runs").join(&run_id);
    for nom in ["metrics.json", "trades.csv", "equity.csv", "manifest.json"] {
        assert!(dossier.join(nom).is_file(), "{nom}");
    }
    assert_eq!(json(&dossier.join("manifest.json"))["run_id"], run_id);
    assert!(stdout.contains(&dossier.display().to_string()), "{stdout}");
    // Relancé sur le même dossier : réécrit fichier par fichier, mêmes octets déterministes.
    let avant = std::fs::read(dossier.join("metrics.json")).unwrap();
    ok(&backtest(PARAMS_15M, &["--root", &r]));
    assert_eq!(std::fs::read(dossier.join("metrics.json")).unwrap(), avant);
}

fn dataset_pret(racine: &Path) {
    let r = racine.display().to_string();
    let ingest = tv4(&[
        "ingest",
        "csv",
        "--csv",
        &repo("fixtures/kraken_xbtusd_1m.csv"),
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--root",
        &r,
    ]
    .map(String::from));
    ok(&ingest);
    let build = tv4(&[
        "dataset",
        "build",
        "--root",
        &r,
        "--name",
        "fixture_2020_01",
        "--exchange",
        "kraken",
        "--symbol",
        "BTC-USD",
        "--tf",
        "1m",
        "--from",
        "2020-01-01T00:00:00Z",
        "--to",
        "2020-01-09T00:00:00Z",
    ]
    .map(String::from));
    ok(&build);
}

fn backtest_dataset(racine: &Path, dataset: &str, extra: &[&str]) -> Output {
    let mut args: Vec<String> = [
        "backtest",
        "--root",
        &racine.display().to_string(),
        "--dataset",
        dataset,
        "--strategy",
        "sma_cross",
        "--params",
        &repo(PARAMS_15M),
        "--fees-file",
        &repo("config/fees.toml"),
        "--instruments-file",
        &repo("config/instruments.toml"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(extra.iter().map(|s| s.to_string()));
    tv4(&args)
}

#[test]
fn dataset_par_nom_et_par_chemin_donnent_les_memes_fichiers() {
    let racine = tempfile::tempdir().unwrap();
    dataset_pret(racine.path());
    let par_nom = racine.path().join("par_nom");
    let par_chemin = racine.path().join("par_chemin");
    ok(&backtest_dataset(
        racine.path(),
        "fixture_2020_01",
        &["--out", &par_nom.display().to_string()],
    ));
    ok(&backtest_dataset(
        racine.path(),
        "datasets/fixture_2020_01.json",
        &["--out", &par_chemin.display().to_string()],
    ));
    for nom in ["metrics.json", "trades.csv", "equity.csv"] {
        assert_eq!(
            std::fs::read(par_nom.join(nom)).unwrap(),
            std::fs::read(par_chemin.join(nom)).unwrap(),
            "{nom}"
        );
    }
    let manifeste = json(&par_nom.join("manifest.json"));
    assert_eq!(manifeste["data"]["source"], "dataset");
    assert_eq!(manifeste["data"]["dataset"], "fixture_2020_01");
    assert!(manifeste["data"]["fixture_blake3"].is_null());
    assert_eq!(manifeste["dataset_hash"], common::DATASET_HASH_SKIP);
    assert_eq!(manifeste["hash_format"], "dsh-v1");
    let m = json(&par_nom.join("metrics.json"));
    assert_eq!(m["dataset_hash"], common::DATASET_HASH_SKIP);
    assert_eq!(m["data"]["dataset"], "fixture_2020_01");
    assert_eq!(m["data"]["gaps_policy"], "skip");
    assert_eq!(m["data"]["minutes_missing"], 1_370);
    // Un nom inexistant cite le manifeste attendu ; un nom invalide est refusé avant toute lecture.
    let absent = backtest_dataset(
        racine.path(),
        "inconnu",
        &["--out", &racine.path().join("x").display().to_string()],
    );
    assert_eq!(absent.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&absent.stderr).contains("inconnu.json"));
    let invalide = backtest_dataset(
        racine.path(),
        "Mauvais-Nom",
        &["--out", &racine.path().join("y").display().to_string()],
    );
    assert_eq!(invalide.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&invalide.stderr).contains("Mauvais-Nom"));
}

#[test]
fn la_fixture_et_le_dataset_ont_des_dataset_hash_differents_mais_les_memes_resultats() {
    let racine = tempfile::tempdir().unwrap();
    dataset_pret(racine.path());
    let dataset = racine.path().join("dataset");
    ok(&backtest_dataset(
        racine.path(),
        "fixture_2020_01",
        &["--out", &dataset.display().to_string()],
    ));
    let fixture = racine.path().join("fixture");
    ok(&backtest(
        PARAMS_15M,
        &["--out", &fixture.display().to_string()],
    ));
    let (d, f) = (
        json(&dataset.join("metrics.json")),
        json(&fixture.join("metrics.json")),
    );
    // Le hash `dsh-v1` porte la fenêtre dans son en-tête : fenêtre du mois pour le dataset, fenêtre
    // des bougies lues pour la fixture. Les bougies sont les mêmes, pas le hash.
    assert_ne!(d["dataset_hash"], f["dataset_hash"]);
    assert!(f["dataset_hash"].as_str().unwrap().starts_with("blake3:"));
    for cle in [
        "pnl_net",
        "n_trades",
        "config_hash",
        "funnel",
        "distribution",
        "max_drawdown",
    ] {
        assert_eq!(d[cle], f[cle], "{cle}");
    }
    assert_eq!(
        std::fs::read(dataset.join("trades.csv")).unwrap(),
        std::fs::read(fixture.join("trades.csv")).unwrap()
    );
    assert_eq!(
        std::fs::read(dataset.join("equity.csv")).unwrap(),
        std::fs::read(fixture.join("equity.csv")).unwrap()
    );
}

#[test]
fn deux_process_donnent_les_trois_fichiers_deterministes_identiques() {
    let dossier = tempfile::tempdir().unwrap();
    let (a, b) = (dossier.path().join("a"), dossier.path().join("b"));
    ok(&backtest(PARAMS_15M, &["--out", &a.display().to_string()]));
    ok(&backtest(PARAMS_15M, &["--out", &b.display().to_string()]));
    for nom in ["metrics.json", "trades.csv", "equity.csv"] {
        assert_eq!(
            std::fs::read(a.join(nom)).unwrap(),
            std::fs::read(b.join(nom)).unwrap(),
            "{nom}"
        );
    }
    let (ma, mb) = (
        json(&a.join("manifest.json")),
        json(&b.join("manifest.json")),
    );
    assert_eq!(ma["run_id"], mb["run_id"]);
    assert_eq!(ma["result_hash"], mb["result_hash"]);
}

#[test]
fn les_options_inconnues_et_les_erreurs_de_donnees_ne_laissent_pas_de_manifeste() {
    let dossier = tempfile::tempdir().unwrap();
    let out = dossier.path().join("run");
    let mut args = vec!["backtest".to_string()];
    args.extend(entrees(PARAMS_15M));
    args.extend(["--out".to_string(), out.display().to_string()]);
    let position = args.iter().position(|a| a == "--fixture").unwrap();
    args[position + 1] = "/n/existe/pas.csv".to_string();
    let sortie = tv4(&args);
    assert_eq!(sortie.status.code(), Some(1));
    assert!(!out.join("manifest.json").exists());
}
