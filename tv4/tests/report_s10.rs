//! Rapports s10 (docs/specs/s10.md §12 et §16, lot E) : arithmétique de docs/04 §3.3 recalculée à la
//! main, `metrics.json` `tv4-metrics-s10`, `trades.csv` avec `spread_cost`, `profiles.json` (tableau
//! d'étiquettes, statuts du profil maker, portes croisées, déterminisme).
//!
//! Marché inventé : trades construits à la main et marche synthétique, aucune donnée réelle.

mod common;
mod portfolio_common;
mod profiles_common;
mod report_s9_common;

use profiles_common::{FAST_PARAMS, costs_file, render_all, run, scenario};
use serde_json::Value;
use tv4::model::Timeframe;
use tv4::model::costs::CostProfileName;
use tv4::report::cost_arithmetic::{self, CostArithmeticInput};
use tv4::report::metrics_s9::MetricsS9;
use tv4::report::profiles::{
    MakerStatus, ProfileInput, ProfileLabel, ProfilesInput, ProfilesReport, RenderedProfile,
};
use tv4::sim::{ExitReason, Liquidity, Trade};

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

// ---------------------------------------------------------------------------------------------
// Arithmétique de 04 §3.3, sur des trades construits à la main
// ---------------------------------------------------------------------------------------------

/// Paramètres d'un trade de test ; tout le reste est neutre.
struct Spec {
    qty: f64,
    ref_price: f64,
    entry_price: f64,
    stop_at_entry: Option<f64>,
    fees: (f64, f64),
    slippage: (f64, f64),
    spread: (f64, f64),
    pnl_net: f64,
    entry_ms: i64,
}

fn trade(spec: &Spec) -> Trade {
    Trade {
        lot_id: 0,
        entry_signal_ms: spec.entry_ms - 2 * MINUTE,
        entry_order_ms: spec.entry_ms,
        entry_ms: spec.entry_ms,
        entry_ref_price: spec.ref_price,
        entry_price: spec.entry_price,
        entry_fee: spec.fees.0,
        entry_slippage: spec.slippage.0,
        entry_spread: spec.spread.0,
        entry_liquidity: Liquidity::Taker,
        exit_signal_ms: spec.entry_ms + 10 * MINUTE,
        exit_order_ms: spec.entry_ms + 10 * MINUTE,
        exit_ms: spec.entry_ms + 10 * MINUTE,
        exit_ref_price: spec.ref_price,
        exit_price: spec.ref_price,
        exit_fee: spec.fees.1,
        exit_slippage: spec.slippage.1,
        exit_spread: spec.spread.1,
        exit_liquidity: Liquidity::Taker,
        qty: spec.qty,
        pnl_net: spec.pnl_net,
        mfe: 0.0,
        mae: 0.0,
        stop_at_exit: None,
        stop_at_entry: spec.stop_at_entry,
        stop_moved: false,
        capped_by_cash: false,
        exit_reason: ExitReason::Signal,
        forced_close: false,
        entry_tag: "",
        exit_tag: "",
    }
}

/// Trois trades : A (R défini), B (R défini, perdant), C (sans stop, donc sans R).
fn three_trades() -> Vec<Trade> {
    vec![
        trade(&Spec {
            qty: 2.0,
            ref_price: 100.0,
            entry_price: 100.0,
            stop_at_entry: Some(95.0),
            fees: (0.7, 0.7),
            slippage: (0.2, 0.1),
            spread: (0.05, 0.05),
            pnl_net: 5.0,
            entry_ms: 2 * MINUTE,
        }),
        trade(&Spec {
            qty: 1.0,
            ref_price: 50.0,
            entry_price: 50.0,
            stop_at_entry: Some(40.0),
            fees: (0.2, 0.2),
            slippage: (0.1, 0.1),
            spread: (0.0, 0.0),
            pnl_net: -3.0,
            entry_ms: 3 * MINUTE,
        }),
        trade(&Spec {
            qty: 3.0,
            ref_price: 10.0,
            entry_price: 10.0,
            stop_at_entry: None,
            fees: (0.1, 0.1),
            slippage: (0.0, 0.0),
            spread: (0.0, 0.0),
            pnl_net: 1.0,
            entry_ms: 4 * MINUTE,
        }),
    ]
}

/// Grille de dix minutes dont l'equity vaut 300, 300, 300, 310, 320, ...
fn grid() -> (Vec<i64>, Vec<f64>) {
    let open: Vec<i64> = (0..10).map(|m| m * MINUTE).collect();
    let equity: Vec<f64> = (0..10)
        .map(|m| {
            if m < 3 {
                300.0
            } else {
                300.0 + 10.0 * (m - 2) as f64
            }
        })
        .collect();
    (open, equity)
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
}

fn assert_near(name: &str, got: Option<f64>, want: f64) {
    let got = got.unwrap_or_else(|| panic!("{name} indéfini"));
    assert!(near(got, want), "{name} : {got} au lieu de {want}");
}

fn arithmetic(
    trades: &[Trade],
    profile: CostProfileName,
    tf: Timeframe,
) -> cost_arithmetic::CostArithmetic {
    let (open, equity) = grid();
    cost_arithmetic::compute(&CostArithmeticInput {
        trades,
        profile,
        decision_tf: tf,
        span_ms: 10 * DAY,
        grid_open_ms: &open,
        equity: &equity,
    })
    .expect("arithmétique")
}

#[test]
fn chaque_cle_de_l_arithmetique_est_recalculee_a_la_main() {
    let c = arithmetic(&three_trades(), CostProfileName::Baseline, Timeframe::D1);

    // cost_i = frais + slippage + spread ; notional = qty × entry_ref_price ; gross = pnl + cost.
    let cost = [
        0.7 + 0.7 + 0.2 + 0.1 + 0.05 + 0.05,
        0.2 + 0.2 + 0.1 + 0.1,
        0.1 + 0.1,
    ];
    let notional = [200.0, 50.0, 30.0];
    let pnl = [5.0, -3.0, 1.0];
    let gross: Vec<f64> = pnl.iter().zip(cost).map(|(p, c)| p + c).collect();
    let rate: Vec<f64> = cost.iter().zip(notional).map(|(c, n)| c / n).collect();

    assert_eq!(c.decision_tf, Timeframe::D1);
    assert!(c.protocol_tf);
    assert_near(
        "round_trip_cost_mean",
        c.round_trip_cost_mean,
        (cost[0] + cost[1] + cost[2]) / 3.0,
    );
    assert_near(
        "round_trip_cost_rate_mean",
        c.round_trip_cost_rate_mean,
        (rate[0] + rate[1] + rate[2]) / 3.0,
    );
    // Médiane de trois valeurs : la centrale.
    let mut sorted = rate.clone();
    sorted.sort_by(f64::total_cmp);
    assert_near(
        "round_trip_cost_rate_median",
        c.round_trip_cost_rate_median,
        sorted[1],
    );
    assert_near("fees", Some(c.cost_breakdown.fees), 1.4 + 0.4 + 0.2);
    assert_near("slippage", Some(c.cost_breakdown.slippage), 0.3 + 0.2);
    assert_near("spread", Some(c.cost_breakdown.spread), 0.1);

    // R : risque 2 × (100 − 95) = 10 et 1 × (50 − 40) = 10 ; le trade C n'a pas de R.
    assert_eq!(c.trades_with_r, 2);
    assert_near(
        "cost_r_mean",
        c.cost_r_mean,
        (cost[0] / 10.0 + cost[1] / 10.0) / 2.0,
    );
    assert_near(
        "cost_r_median",
        c.cost_r_median,
        (cost[0] / 10.0 + cost[1] / 10.0) / 2.0,
    );
    assert_eq!(c.cost_r_limit, 0.10);
    // 0,12 R de coût moyen contre 0,10 : la porte échoue.
    assert_eq!(c.cost_r_pass, Some(false));

    let mean_gross = (gross[0] + gross[1] + gross[2]) / 3.0;
    let mean_cost = (cost[0] + cost[1] + cost[2]) / 3.0;
    assert_near("gross_expectancy", c.gross_expectancy, mean_gross);
    assert_near("cost_to_gross", c.cost_to_gross, mean_cost / mean_gross);
    assert_eq!(c.cost_third_pass, mean_cost <= mean_gross / 3.0);
    assert!(!c.cost_third_pass, "{mean_cost} contre {mean_gross}");

    let gross_rates: Vec<f64> = gross.iter().zip(notional).map(|(g, n)| g / n).collect();
    let mut sorted = gross_rates.clone();
    sorted.sort_by(f64::total_cmp);
    assert_near("gross_median_rate", c.gross_median_rate, sorted[1]);
    let mean_rate = (rate[0] + rate[1] + rate[2]) / 3.0;
    assert_eq!(
        c.gross_median_vs_own_cost_pass,
        Some(sorted[1] >= 2.0 * mean_rate)
    );

    // 3 trades en 10 jours.
    let per_year = 3.0 / (10.0 / 365.25);
    assert_near("trades_per_year", Some(c.trades_per_year), per_year);
    // Equity de la dernière minute de grille ≤ entry_ms : 300 à la minute 2, 310 à la 3, 320 à la 4.
    let fractions = [2.0 * 100.0 / 300.0, 1.0 * 50.0 / 310.0, 3.0 * 10.0 / 320.0];
    let fraction_mean = (fractions[0] + fractions[1] + fractions[2]) / 3.0;
    assert_near(
        "capital_fraction_mean",
        c.capital_fraction_mean,
        fraction_mean,
    );
    assert_near(
        "hurdle_annual",
        c.hurdle_annual,
        per_year * fraction_mean * mean_rate,
    );
}

#[test]
fn le_seuil_de_0_15_r_ne_vaut_qu_en_maker_sur_4h_ou_1h() {
    let trades = three_trades();
    // Coût moyen de 0,12 R : sous 0,15, au-dessus de 0,10.
    let verdict = |profile, tf| arithmetic(&trades, profile, tf).cost_r_pass;
    assert_eq!(verdict(CostProfileName::Maker, Timeframe::H4), Some(true));
    assert_eq!(verdict(CostProfileName::Maker, Timeframe::H1), Some(true));
    assert_eq!(verdict(CostProfileName::Maker, Timeframe::D1), Some(false));
    assert_eq!(verdict(CostProfileName::Maker, Timeframe::M15), Some(false));
    assert_eq!(
        verdict(CostProfileName::Baseline, Timeframe::H4),
        Some(false)
    );
    assert_eq!(
        verdict(CostProfileName::Adverse, Timeframe::H1),
        Some(false)
    );
    let limit = |profile, tf| arithmetic(&trades, profile, tf).cost_r_limit;
    assert_eq!(limit(CostProfileName::Maker, Timeframe::H4), 0.15);
    assert_eq!(limit(CostProfileName::Baseline, Timeframe::H4), 0.10);
    // `protocol_tf` : 1d et 4h, et le 1h seulement en maker.
    let protocol = |profile, tf| arithmetic(&trades, profile, tf).protocol_tf;
    assert!(protocol(CostProfileName::Baseline, Timeframe::H4));
    assert!(!protocol(CostProfileName::Baseline, Timeframe::H1));
    assert!(protocol(CostProfileName::Maker, Timeframe::H1));
    assert!(!protocol(CostProfileName::Maker, Timeframe::M15));
}

/// Un trade dont `cost = 3` et `gross = pnl_net + 3`.
fn single(pnl_net: f64, fees: f64) -> Vec<Trade> {
    vec![trade(&Spec {
        qty: 1.0,
        ref_price: 100.0,
        entry_price: 100.0,
        stop_at_entry: Some(90.0),
        fees: (fees, 0.0),
        slippage: (0.0, 0.0),
        spread: (0.0, 0.0),
        pnl_net,
        entry_ms: 2 * MINUTE,
    })]
}

#[test]
fn la_porte_du_tiers_aux_frontieres() {
    let third = |trades: &[Trade]| {
        arithmetic(trades, CostProfileName::Baseline, Timeframe::D1).cost_third_pass
    };
    // gross = 9 et cost = 3 : exactement le tiers, la porte passe (≤).
    assert!(third(&single(6.0, 3.0)));
    // gross = 8,999 : le coût dépasse le tiers.
    assert!(!third(&single(5.999, 3.0)));
    // Espérance brute nulle ou négative : la porte échoue, même sans coût.
    assert!(!third(&single(0.0, 0.0)));
    assert!(!third(&single(-1.0, 0.0)));
    assert!(!third(&single(-5.0, 3.0)));
    let c = arithmetic(&single(-5.0, 3.0), CostProfileName::Baseline, Timeframe::D1);
    assert_eq!(
        c.cost_to_gross, None,
        "pas de ratio sur une espérance brute ≤ 0"
    );
    let c = arithmetic(&single(6.0, 3.0), CostProfileName::Baseline, Timeframe::D1);
    assert_near("cost_to_gross", c.cost_to_gross, 3.0 / 9.0);
}

// ---------------------------------------------------------------------------------------------
// metrics.json et trades.csv s10 sur un run joué
// ---------------------------------------------------------------------------------------------

const ROWS: usize = 6_000;

fn rendered() -> Vec<RenderedProfile> {
    let data = scenario(ROWS);
    let file = costs_file();
    let result = run(&data, &file, 1, FAST_PARAMS);
    let dataset = profiles_common::dataset_info(&data.series);
    render_all(&result, &data, &dataset, &file)
}

fn json_of(metrics: &MetricsS9) -> Value {
    serde_json::to_value(metrics).unwrap()
}

#[test]
fn metrics_s10_porte_les_blocs_de_couts_de_chaque_profil() {
    let profiles = rendered();
    let names = ["baseline", "adverse", "maker"];
    for (profile, name) in profiles.iter().zip(names) {
        let m = json_of(&profile.metrics);
        assert_eq!(m["schema"], "tv4-metrics-s10");
        assert_eq!(m["config"]["schema"], "tv4-portfolio-config-v2");
        assert_eq!(m["costs"]["profile"], name);
        assert_eq!(m["costs"]["gates_profile"], name != "maker");
        assert_eq!(m["costs"]["fee_grid"], "okx_regular");
        assert_eq!(m["costs"]["seed"], 1);
        assert_eq!(m["costs"]["rules_version"], "c1");
        assert_eq!(m["costs"]["costs_file"], "costs.toml");
        assert!(m["costs"]["costs_file_hash"].as_str().unwrap().len() == 64);
        assert_eq!(m["costs"]["cost_measures_file"], Value::Null);
        assert_eq!(m["costs"]["cost_measures_hash"], Value::Null);
        assert_eq!(m["costs"]["stress_calibrated"], false);
        assert_eq!(m["costs"]["stress_calibration_ref"], Value::Null);
        let legs = m["costs"]["legs"].as_array().unwrap();
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0]["symbol"], "ETH-USD");
        assert_eq!(legs[0]["class"], "major");
        assert_eq!(legs[1]["symbol"], "SOL-USD");
        assert_eq!(legs[1]["class_source"], "fallback");
        assert_eq!(
            legs[0]["entry"],
            if name == "maker" { "maker" } else { "taker" }
        );
        // Toutes les clés de l'arithmétique sont présentes, même indéfinies.
        for key in [
            "decision_tf",
            "protocol_tf",
            "round_trip_cost_mean",
            "round_trip_cost_rate_mean",
            "round_trip_cost_rate_median",
            "cost_breakdown",
            "cost_r_mean",
            "cost_r_median",
            "trades_with_r",
            "cost_r_limit",
            "cost_r_pass",
            "gross_expectancy",
            "cost_to_gross",
            "cost_third_pass",
            "gross_median_rate",
            "gross_median_vs_own_cost_pass",
            "trades_per_year",
            "capital_fraction_mean",
            "hurdle_annual",
        ] {
            assert!(m["cost_arithmetic"].get(key).is_some(), "{name} : {key}");
        }
        assert_eq!(m["cost_arithmetic"]["decision_tf"], "1m");
        assert_eq!(m["cost_arithmetic"]["protocol_tf"], false);
        assert_eq!(m["cost_counters"]["by_pair"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn les_ecarts_du_simulateur_suivent_le_profil() {
    let profiles = rendered();
    let deviations = |i: usize| -> Vec<String> {
        json_of(&profiles[i].metrics)["sim_deviations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    for i in 0..3 {
        let d = deviations(i);
        assert!(!d.contains(&"stop_slippage_fixed".to_string()), "{d:?}");
        assert!(d.contains(&"stop_stress_uncalibrated".to_string()), "{d:?}");
        assert!(d.contains(&"cost_defaults_unmeasured".to_string()), "{d:?}");
        assert!(d.contains(&"market_buy_cash_margin".to_string()), "{d:?}");
    }
    for i in [0, 1] {
        assert!(deviations(i).contains(&"no_maker_fill_probability".to_string()));
    }
    assert!(!deviations(2).contains(&"no_maker_fill_probability".to_string()));
}

#[test]
fn un_run_hors_profils_n_a_aucun_champ_s10() {
    // `MetricsS9::new` laisse les trois blocs absents : le JSON s9 ne porte aucune clé s10.
    let profiles = rendered();
    let mut metrics = profiles[0].metrics.clone();
    metrics.costs = None;
    metrics.cost_arithmetic = None;
    metrics.cost_counters = None;
    let m = json_of(&metrics);
    for key in ["costs", "cost_arithmetic", "cost_counters"] {
        assert!(m.get(key).is_none(), "{key}");
    }
}

#[test]
fn le_trades_csv_s10_remplit_spread_cost() {
    let profiles = rendered();
    for (profile, name) in profiles.iter().zip(["baseline", "adverse", "maker"]) {
        let mut reader = csv::Reader::from_reader(profile.trades_bytes.as_slice());
        let headers = reader.headers().unwrap().clone();
        let spread = headers.iter().position(|h| h == "spread_cost").unwrap();
        let mut rows = 0usize;
        let mut sum = 0.0;
        for record in reader.records() {
            let record = record.unwrap();
            let cell = record.get(spread).unwrap();
            assert!(!cell.is_empty(), "{name} : spread_cost vide");
            sum += cell.parse::<f64>().unwrap();
            rows += 1;
        }
        assert!(rows > 0, "{name} sans trade : le scénario ne couvre rien");
        let counters = json_of(&profile.metrics);
        let total = counters["cost_arithmetic"]["cost_breakdown"]["spread"]
            .as_f64()
            .unwrap();
        // La colonne vaut `entry_spread + exit_spread` de chaque trade, comme la somme du bloc.
        assert!(total > 0.0, "{name} : aucun coût de spread");
        assert!(near(sum, total), "{name} : {sum} contre {total}");
    }
}

#[test]
fn le_rendu_d_un_profil_est_deterministe() {
    let a = rendered();
    let b = rendered();
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.metrics_bytes, y.metrics_bytes);
        assert_eq!(x.trades_bytes, y.trades_bytes);
        assert_eq!(x.equity_bytes, y.equity_bytes);
    }
    assert_ne!(a[0].metrics.config_hash, a[1].metrics.config_hash);
    assert_ne!(a[0].metrics.config_hash, a[2].metrics.config_hash);
}

#[test]
fn la_part_maker_est_celle_de_la_formule_de_la_spec() {
    let profiles = rendered();
    let counters = profiles[2].metrics.cost_counters.as_ref().unwrap();
    let filled = counters.total.maker_entries_filled_maker;
    let taken = counters.total.maker_entries_taken_at_arrival;
    assert!(filled + taken > 0, "le scénario ne convertit aucune entrée");
    assert_eq!(
        counters.maker_entry_maker_share,
        Some(filled as f64 / (filled + taken) as f64)
    );
    // Somme des paires = total.
    let by_pair: u64 = counters
        .by_pair
        .iter()
        .map(|pair| pair.counters.maker_entries_filled_maker)
        .sum();
    assert_eq!(by_pair, filled);
    // Baseline et adverse ne convertissent rien : part indéfinie.
    for i in [0, 1] {
        let c = profiles[i].metrics.cost_counters.as_ref().unwrap();
        assert_eq!(c.total.maker_entries_converted, 0);
        assert_eq!(c.maker_entry_maker_share, None);
    }
}

#[test]
fn new_s10_refuse_un_run_sans_bloc_de_couts() {
    use tv4::engine::{PortfolioInputs, run_portfolio};
    use tv4::report::metrics_s9::{CostsInputs, InstrumentsInfo, MetricsS9Inputs};
    let data = scenario(2_000);
    let file = costs_file();
    let profiles = run(&data, &file, 1, FAST_PARAMS);
    let legacy = run_portfolio(PortfolioInputs {
        series: &data.series,
        strategies: (0..2)
            .map(|_| profiles_common::sma_cross(FAST_PARAMS))
            .collect(),
        config: portfolio_common::config(&data.universe, &data.rules, 300.0),
    })
    .expect("run hérité");
    let mut run0 = profiles.runs[0].clone();
    run0.result = legacy;
    let dataset = profiles_common::dataset_info(&data.series);
    let erreur = MetricsS9::new_s10(
        &run0,
        MetricsS9Inputs {
            dataset,
            instruments: InstrumentsInfo::toml(),
            okx_pairs: None,
            markets: profiles_common::markets(&data.series, &data.universe),
        },
        CostsInputs {
            file: &file,
            file_label: "costs.toml",
            measures: None,
            measures_label: None,
        },
    )
    .unwrap_err();
    assert!(erreur.to_string().contains("bloc `costs`"), "{erreur}");
}

// ---------------------------------------------------------------------------------------------
// profiles.json
// ---------------------------------------------------------------------------------------------

/// Les trois métriques du run joué, modifiées par `edit`, puis `profiles.json`.
fn report_with(
    base: &[RenderedProfile],
    min_share: f64,
    edit: impl Fn(&mut [MetricsS9]),
) -> ProfilesReport {
    let mut metrics: Vec<MetricsS9> = base.iter().map(|r| r.metrics.clone()).collect();
    edit(&mut metrics);
    let hashes: Vec<String> = base.iter().map(RenderedProfile::result_hash).collect();
    let input = |i: usize| ProfileInput {
        metrics: &metrics[i],
        result_hash: &hashes[i],
    };
    ProfilesReport::build(&ProfilesInput {
        baseline: input(0),
        adverse: input(1),
        maker: input(2),
        min_maker_entry_share: min_share,
    })
    .expect("profiles.json")
}

fn set(m: &mut MetricsS9, n_trades: u64, expectancy: Option<f64>) {
    m.min_trades = 30;
    m.n_trades = n_trades;
    m.expectancy_net = expectancy;
}

fn set_share(m: &mut MetricsS9, share: Option<f64>) {
    m.cost_counters.as_mut().unwrap().maker_entry_maker_share = share;
}

#[test]
fn les_cinq_lignes_du_tableau_d_etiquettes() {
    let base = rendered();
    let label = |edit: &dyn Fn(&mut [MetricsS9])| report_with(&base, 0.5, edit).label;

    // 1. baseline ou adverse sous min_trades : indéterminé.
    assert_eq!(
        label(&|m| {
            set(&mut m[0], 29, Some(1.0));
            set(&mut m[1], 40, Some(1.0));
            set(&mut m[2], 40, Some(1.0));
        }),
        ProfileLabel::Indeterminate
    );
    assert_eq!(
        label(&|m| {
            set(&mut m[0], 40, Some(1.0));
            set(&mut m[1], 0, None);
            set(&mut m[2], 40, Some(1.0));
        }),
        ProfileLabel::Indeterminate
    );
    // 2. baseline et adverse positifs : « tient les coûts », même avec un maker sous min_trades.
    let report = report_with(&base, 0.5, |m| {
        set(&mut m[0], 40, Some(1.0));
        set(&mut m[1], 40, Some(0.5));
        set(&mut m[2], 3, Some(2.0));
    });
    assert_eq!(report.label, ProfileLabel::PassesCosts);
    assert_eq!(report.label_fr, "tient les coûts");
    assert_eq!(report.maker.status, MakerStatus::TooFewTrades);
    assert!(report.gates.both_gate_profiles_positive);
    // 3. baseline et adverse non positifs, maker positif et rempli en maker : « dépend du fill ».
    let report = report_with(&base, 0.5, |m| {
        set(&mut m[0], 40, Some(-1.0));
        set(&mut m[1], 40, Some(0.0));
        set(&mut m[2], 40, Some(1.0));
        set_share(&mut m[2], Some(0.8));
    });
    assert_eq!(report.label, ProfileLabel::FillDependent);
    assert_eq!(report.label_fr, "dépend du fill");
    assert_eq!(report.maker.status, MakerStatus::Positive);
    assert!(!report.gates.both_gate_profiles_positive);
    // maker positif mais pris surtout en taker : ni « dépend du fill », ni autre chose que « négatif ».
    let report = report_with(&base, 0.5, |m| {
        set(&mut m[0], 40, Some(-1.0));
        set(&mut m[1], 40, Some(-1.0));
        set(&mut m[2], 40, Some(1.0));
        set_share(&mut m[2], Some(0.3));
    });
    assert_eq!(report.maker.status, MakerStatus::PositiveMostlyTaker);
    assert_eq!(report.label, ProfileLabel::Negative);
    assert_eq!(report.label_fr, "négatif");
    // 4. baseline positif, adverse non positif : « fragile aux coûts ».
    let report = report_with(&base, 0.5, |m| {
        set(&mut m[0], 40, Some(1.0));
        set(&mut m[1], 40, Some(-1.0));
        set(&mut m[2], 40, Some(1.0));
        set_share(&mut m[2], Some(0.9));
    });
    assert_eq!(report.label, ProfileLabel::CostFragile);
    assert_eq!(report.label_fr, "fragile aux coûts");
    // 5. le reste : adverse positif sans baseline.
    assert_eq!(
        label(&|m| {
            set(&mut m[0], 40, Some(-1.0));
            set(&mut m[1], 40, Some(1.0));
            set(&mut m[2], 40, Some(1.0));
        }),
        ProfileLabel::Negative
    );
}

#[test]
fn les_quatre_statuts_maker() {
    let base = rendered();
    let status = |n: u64, expectancy: Option<f64>, share: Option<f64>, min_share: f64| {
        report_with(&base, min_share, |m| {
            set(&mut m[2], n, expectancy);
            set_share(&mut m[2], share);
        })
        .maker
        .status
    };
    assert_eq!(
        status(29, Some(1.0), Some(1.0), 0.5),
        MakerStatus::TooFewTrades
    );
    assert_eq!(
        status(30, Some(0.0), Some(1.0), 0.5),
        MakerStatus::NonPositive
    );
    assert_eq!(
        status(30, Some(-1.0), Some(1.0), 0.5),
        MakerStatus::NonPositive
    );
    assert_eq!(
        status(30, Some(1.0), Some(0.49), 0.5),
        MakerStatus::PositiveMostlyTaker
    );
    assert_eq!(status(30, Some(1.0), Some(0.5), 0.5), MakerStatus::Positive);
    // Aucune entrée convertie : la part est indéfinie et le statut reste « positif ».
    assert_eq!(status(30, Some(1.0), None, 0.5), MakerStatus::Positive);
    // Le seuil est celui du fichier de coûts passé à la construction.
    assert_eq!(
        status(30, Some(1.0), Some(0.6), 0.7),
        MakerStatus::PositiveMostlyTaker
    );
}

#[test]
fn le_profil_maker_n_entre_pas_dans_l_etiquette_hors_fill_dependent() {
    let base = rendered();
    let with_maker = |n: u64, expectancy: Option<f64>| {
        report_with(&base, 0.5, |m| {
            set(&mut m[0], 40, Some(1.0));
            set(&mut m[1], 40, Some(1.0));
            set(&mut m[2], n, expectancy);
        })
        .label
    };
    for (n, e) in [(0, None), (3, Some(-5.0)), (100, Some(5.0))] {
        assert_eq!(with_maker(n, e), ProfileLabel::PassesCosts);
    }
}

#[test]
fn la_porte_du_gain_brut_median_lit_le_cout_baseline() {
    let base = rendered();
    let gate = |edit: &dyn Fn(&mut [MetricsS9])| {
        report_with(&base, 0.5, edit)
            .gates
            .gross_median_vs_2x_baseline_cost
    };
    let tune = |m: &mut MetricsS9, gross: Option<f64>, cost: Option<f64>| {
        let a = m.cost_arithmetic.as_mut().unwrap();
        a.gross_median_rate = gross;
        a.round_trip_cost_rate_mean = cost;
    };
    // Baseline : 0,02 ≥ 2 × 0,01. L'adverse a un coût bien plus grand, sans effet sur la porte.
    assert_eq!(
        gate(&|m| {
            tune(&mut m[0], Some(0.02), Some(0.01));
            tune(&mut m[1], Some(0.02), Some(0.5));
        }),
        Some(true)
    );
    // Juste sous le double du coût baseline.
    assert_eq!(
        gate(&|m| {
            tune(&mut m[0], Some(0.0199), Some(0.01));
            tune(&mut m[1], Some(1.0), Some(0.0));
        }),
        Some(false)
    );
    // Valeur indéfinie côté baseline : porte indéfinie, même si les autres profils sont définis.
    assert_eq!(
        gate(&|m| {
            tune(&mut m[0], None, Some(0.01));
            tune(&mut m[1], Some(1.0), Some(0.0));
        }),
        None
    );
}

#[test]
fn les_portes_croisees_reprennent_les_profils_de_porte() {
    let base = rendered();
    let report = report_with(&base, 0.5, |m| {
        for (i, pass) in [Some(true), Some(false), Some(true)]
            .into_iter()
            .enumerate()
        {
            let a = m[i].cost_arithmetic.as_mut().unwrap();
            a.cost_r_pass = pass;
            a.cost_third_pass = i == 0;
        }
    });
    assert_eq!(report.gates.cost_r.baseline, Some(true));
    assert_eq!(report.gates.cost_r.adverse, Some(false));
    assert!(report.gates.cost_third.baseline);
    assert!(!report.gates.cost_third.adverse);
    assert_eq!(report.profiles.len(), 3);
    let roles: Vec<&str> = report.profiles.iter().map(|p| p.role).collect();
    assert_eq!(roles, ["gate", "gate", "informative"]);
    let dirs: Vec<&str> = report.profiles.iter().map(|p| p.dir).collect();
    assert_eq!(dirs, ["baseline", "adverse", "maker"]);
}

#[test]
fn profiles_json_est_deterministe_et_sans_horodatage() {
    let base = rendered();
    let again = rendered();
    let a = profiles_common::profiles_report(&base, &costs_file());
    let b = profiles_common::profiles_report(&again, &costs_file());
    assert_eq!(a.bytes().unwrap(), b.bytes().unwrap());
    let text = String::from_utf8(a.bytes().unwrap()).unwrap();
    assert!(text.ends_with('\n'));
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["schema"], "tv4-profiles-v1");
    for key in ["created_at", "started_at", "timings", "commit"] {
        assert!(json.get(key).is_none(), "{key}");
    }
    assert_eq!(json["initial_cash"], 300.0);
    assert_eq!(json["seed"], 1);
    assert_eq!(json["fee_grid"], "okx_regular");
    assert_eq!(json["stress_calibrated"], false);
    assert_eq!(json["cost_measures_hash"], Value::Null);
    assert_eq!(json["maker"]["min_maker_entry_share"], 0.5);
    // profiles_id : BLAKE3 de run_id_baseline|run_id_adverse|run_id_maker.
    let ids: Vec<&str> = json["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        json["profiles_id"],
        tv4::model::hash::blake3_hex(format!("{}|{}|{}", ids[0], ids[1], ids[2]).as_bytes())
    );
    // L'étiquette est toujours présente avec son libellé français.
    assert_eq!(json["label_fr"], a.label.fr());
}

#[test]
fn profiles_json_refuse_des_profils_qui_ne_decrivent_pas_le_meme_run() {
    let base = rendered();
    let hashes: Vec<String> = base.iter().map(RenderedProfile::result_hash).collect();
    let build = |metrics: &[MetricsS9; 3]| {
        let input = |i: usize| ProfileInput {
            metrics: &metrics[i],
            result_hash: &hashes[i],
        };
        ProfilesReport::build(&ProfilesInput {
            baseline: input(0),
            adverse: input(1),
            maker: input(2),
            min_maker_entry_share: 0.5,
        })
    };
    let metrics = |i: usize| base[i].metrics.clone();
    // Ordre des profils inversé.
    assert!(build(&[metrics(1), metrics(0), metrics(2)]).is_err());
    // Une graine différente dans le profil maker.
    let mut maker = metrics(2);
    maker.costs.as_mut().unwrap().seed = 2;
    assert!(build(&[metrics(0), metrics(1), maker]).is_err());
    // Un autre jeu de données.
    let mut adverse = metrics(1);
    adverse.dataset_hash = "blake3:bb".to_string();
    assert!(build(&[metrics(0), adverse, metrics(2)]).is_err());
    // Un autre capital.
    let mut adverse = metrics(1);
    adverse.config.initial_cash = 3_000.0;
    assert!(build(&[metrics(0), adverse, metrics(2)]).is_err());
    // Les métriques d'un run s9, sans blocs de coûts.
    let mut baseline = metrics(0);
    baseline.costs = None;
    assert!(build(&[baseline, metrics(1), metrics(2)]).is_err());
    assert!(build(&[metrics(0), metrics(1), metrics(2)]).is_ok());
}
