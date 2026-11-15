//! Image possédée d'un run contrôlé par la politique unique de `verify`.
//! Aucun prix ni marquage n'est déduit de la courbe CSV échantillonnée.
use super::stats_inputs::StatsProvenance;
pub use super::verify::VerifiedRun;
use super::verify::{self, VerifyError};
use crate::research::stats::{StatsSizing, TradeRecord, Window};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub provenance: StatsProvenance,
    pub pair_symbols: Vec<String>,
    /// Séries effectivement admises du dataset, y compris un éventuel marché de contexte.
    pub market_symbols: Vec<String>,
    /// S8 utilise historiquement l'equity courante pour dimensionner les entrées.
    pub sizing: StatsSizing,
    pub trades: Vec<TradeRecord>,
    /// Lignes liquidées après la dernière minute du run, même si leur jambe finit plus tôt.
    pub terminal_rows: Vec<usize>,
    /// TF déclaré dans les paramètres ; inconnu pour les anciens contrats qui ne le publient pas.
    pub decision_timeframe_minutes: Option<u32>,
    /// Soldes après liquidation, indépendants des seules statistiques de trades conservés.
    pub cash_final: f64,
    pub equity_final: f64,
}

/// Une seule lecture de chaque fichier ; les contrôles sont ceux de `verify`.
pub fn read_run(dir: &Path) -> Result<VerifiedRun, VerifyError> {
    let run = verify::load_verified_run(dir)?;
    snapshot(&run).map_err(|detail| VerifyError::Portfolio {
        label: "stats",
        dir: dir.display().to_string(),
        detail,
    })?;
    Ok(run)
}

pub(crate) fn required_string(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("champ `{pointer}` absent ou mal typé"))
}
pub(crate) fn required_number(value: &Value, pointer: &str) -> Result<f64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("champ `{pointer}` absent ou non fini"))
}
fn integer(value: &Value, pointer: &str) -> Result<i64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("champ `{pointer}` absent ou mal typé"))
}

/// Analyse uniquement les octets possédés ; ne rouvre aucun fichier.
pub fn snapshot(run: &VerifiedRun) -> Result<RunSnapshot, String> {
    let m = run.manifest();
    let metrics = run.metrics();
    let s8 = m["schema"] == super::manifest::SCHEMA;
    let research = m["schema"] == super::manifest::SCHEMA_S11;
    let window = Window {
        start_ms: integer(m, "/window/start_ms")?,
        end_ms: integer(m, "/window/end_ms")?,
    };
    super::stats_inputs::validate_window(window)?;
    let measured = if s8 {
        Window {
            start_ms: integer(metrics, "/window/first_open_time_ms")?,
            end_ms: integer(metrics, "/window/last_open_time_ms")?
                .checked_add(60_000)
                .ok_or("débordement de fin de fenêtre")?,
        }
    } else {
        Window {
            start_ms: integer(metrics, "/data/window/start_ms")?,
            end_ms: integer(metrics, "/data/window/end_ms")?,
        }
    };
    if measured != window {
        return Err("fenêtre du manifeste différente de la fenêtre réellement jouée".into());
    }
    let provenance = StatsProvenance {
        run_id: required_string(m, "/run_id")?,
        config_hash: required_string(m, "/config_hash")?,
        dataset_hash: required_string(m, "/dataset_hash")?,
        result_hash: required_string(m, "/result_hash")?,
        initial_cash: required_number(metrics, "/config/initial_cash")?,
        window,
        profile: m
            .get("profile")
            .filter(|v| !v.is_null())
            .map(|_| required_string(m, "/profile"))
            .transpose()?,
        line: if research {
            Some(required_string(metrics, "/research/line")?)
        } else {
            None
        },
        files: serde_json::from_value(
            m.pointer("/outputs/files")
                .cloned()
                .ok_or("outputs.files absent")?,
        )
        .map_err(|e| e.to_string())?,
    };
    provenance.validate()?;
    for key in ["config_hash", "dataset_hash"] {
        if m.get(key) != metrics.get(key) {
            return Err(format!("{key} diffère entre manifeste et métriques"));
        }
    }
    if !s8 && required_number(m, "/initial_cash")?.to_bits() != provenance.initial_cash.to_bits() {
        return Err("capital incohérent".into());
    }
    let pair_symbols = if s8 {
        vec![required_string(metrics, "/data/symbol")?]
    } else {
        // L'univers joué vient des funnels, pas de data.symbols qui inclut le contexte BTC.
        metrics
            .get("funnel_by_pair")
            .and_then(Value::as_array)
            .ok_or("funnel_by_pair absent")?
            .iter()
            .map(|v| required_string(v, "/symbol"))
            .collect::<Result<Vec<_>, _>>()?
    };
    let sizing = if s8 {
        StatsSizing::Compounding
    } else {
        match metrics
            .pointer("/config/portfolio/sizing")
            .and_then(Value::as_str)
        {
            Some("research") => StatsSizing::Research,
            Some("compounding") => StatsSizing::Compounding,
            _ => return Err("config.portfolio.sizing absent ou inconnu".into()),
        }
    };
    let (trades, terminal_rows) = parse_trade_rows(run.trades_bytes(), research, window)?;
    // Seul ce contrat historique documente `params.tf` comme TF de décision.
    // Un paramètre `tf` arbitraire peut désigner un filtre et ne suffit pas à l'identifier.
    let decision_timeframe_minutes = if metrics
        .pointer("/config/strategy/id")
        .and_then(Value::as_str)
        == Some("sma_cross")
    {
        metrics
            .pointer("/config/strategy/params/tf")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<crate::model::Timeframe>().ok())
            .map(|tf| (tf.ms() / 60_000) as u32)
    } else {
        None
    };
    let market_symbols = if s8 {
        pair_symbols.clone()
    } else {
        metrics
            .pointer("/data/series")
            .and_then(Value::as_array)
            .ok_or("data.series absent")?
            .iter()
            .map(|v| required_string(v, "/symbol"))
            .collect::<Result<Vec<_>, _>>()?
    };
    if trades.iter().any(|t| !pair_symbols.contains(&t.symbol)) {
        return Err("symbole du journal absent de l'univers joué".into());
    }
    Ok(RunSnapshot {
        provenance,
        pair_symbols,
        market_symbols,
        sizing,
        trades,
        terminal_rows,
        decision_timeframe_minutes,
        cash_final: required_number(metrics, "/cash_final")?,
        equity_final: required_number(metrics, "/equity_final")?,
    })
}

/// Tolérance relative 1e-10 : opérations réordonnées du ledger, jamais un arrondi du CSV.
pub(crate) fn close(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-10 * a.abs().max(b.abs()).max(1.)
}

/// Le décodeur JSON sans float_roundtrip peut déplacer un capital publié d'un ULP.
/// Huit epsilon relatifs couvrent ces conversions, sans tolérance métier sur le capital.
pub(crate) fn capital_matches(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= 8. * f64::EPSILON * a.abs().max(b.abs())
}

/// Parse strictement les 36 colonnes, même celles inutilisées par les calculs.
/// `purge_end_of_window` est réservé aux runs S11 validés (bloc research présent).
pub fn parse_trades(
    bytes: &[u8],
    purge_end_of_window: bool,
    window: Window,
) -> Result<Vec<TradeRecord>, String> {
    parse_trade_rows(bytes, purge_end_of_window, window).map(|(trades, _)| trades)
}

fn parse_trade_rows(
    bytes: &[u8],
    purge_end_of_window: bool,
    window: Window,
) -> Result<(Vec<TradeRecord>, Vec<usize>), String> {
    use super::writers::{EXIT_TYPES_S9, TRADES_COLUMNS};
    super::stats_inputs::validate_window(window)?;
    let mut reader = csv::Reader::from_reader(bytes);
    if reader
        .headers()
        .map_err(|e| e.to_string())?
        .iter()
        .ne(TRADES_COLUMNS)
    {
        return Err("en-tête trades.csv : 36 colonnes exactes requises".into());
    }
    let mut trades = Vec::new();
    let mut terminal_rows = Vec::new();
    for (row, record) in reader.records().enumerate() {
        let r = record.map_err(|e| e.to_string())?;
        if r.len() != 36 {
            return Err(format!("ligne {row} : 36 champs requis"));
        }
        let bad = |i: usize| format!("ligne {row}, {} : valeur invalide", TRADES_COLUMNS[i]);
        let number = |i: usize| -> Result<f64, String> {
            r[i].parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| bad(i))
        };
        let positive = |i: usize| -> Result<f64, String> {
            let n = number(i)?;
            if n <= 0. { Err(bad(i)) } else { Ok(n) }
        };
        let nonnegative = |i: usize| -> Result<f64, String> {
            let n = number(i)?;
            if n < 0. { Err(bad(i)) } else { Ok(n) }
        };
        let optional = |i: usize| -> Result<Option<f64>, String> {
            if r[i].is_empty() {
                Ok(None)
            } else {
                number(i).map(Some)
            }
        };
        let time = |i: usize| -> Result<i64, String> {
            r[i].parse::<i64>()
                .ok()
                .filter(|v| (0..crate::store::time::MAX_TS_MS).contains(v))
                .ok_or_else(|| bad(i))
        };
        let boolean =
            |i: usize| -> Result<bool, String> { r[i].parse::<bool>().map_err(|_| bad(i)) };
        let lot_id = r[0].parse::<u64>().map_err(|_| bad(0))?;
        let entry_signal = time(1)?;
        let entry_ms = time(2)?;
        let exit_signal = time(8)?;
        let exit_ms = time(9)?;
        let entry_order = time(26)?;
        let exit_order = time(27)?;
        if entry_signal > entry_order
            || entry_order > entry_ms
            || exit_signal > exit_order
            || exit_order > exit_ms
            || entry_ms > exit_ms
            || entry_ms < window.start_ms
            || exit_ms > window.end_ms
        {
            return Err(format!("ligne {row} : horloges incohérentes"));
        }
        for i in [3, 11] {
            if !["maker", "taker"].contains(&&r[i]) {
                return Err(bad(i));
            }
        }
        if ![
            "signal",
            "limit_sell",
            "stop",
            "take_profit",
            "forced_close",
            "quote_gap",
        ]
        .contains(&&r[10])
        {
            return Err(bad(10));
        }
        let entry_ref = positive(4)?;
        let entry_price = positive(5)?;
        let entry_fee = nonnegative(6)?;
        let entry_slippage = nonnegative(7)?;
        let exit_ref = positive(12)?;
        let exit_price = positive(13)?;
        let exit_fee = nonnegative(14)?;
        let exit_slippage = nonnegative(15)?;
        let qty = positive(16)?;
        let pnl_net = number(17)?;
        let _ = number(18)?;
        let _ = number(19)?;
        for i in [20, 31] {
            if optional(i)?.is_some_and(|v| v <= 0.) {
                return Err(bad(i));
            }
        }
        let _ = boolean(21)?;
        let forced = boolean(22)?;
        // Les tags sont des chaînes opaques ; le régime du jalon 1 est strictement absent.
        if r[25].is_empty() || !r[34].is_empty() {
            return Err(bad(if r[25].is_empty() { 25 } else { 34 }));
        }
        if !EXIT_TYPES_S9.contains(&&r[28]) {
            return Err(bad(28));
        }
        let expected = match &r[10] {
            "signal" => {
                if &r[24] == "time_stop" {
                    "time_stop"
                } else {
                    "strategy_exit"
                }
            }
            "limit_sell" => "limit_sell",
            "stop" => {
                if &r[28] == "trailing_stop" {
                    "trailing_stop"
                } else {
                    "stop_loss"
                }
            }
            "take_profit" => "take_profit",
            "forced_close" => "end_of_window",
            _ => "gap_forced_exit",
        };
        if &r[28] != expected || forced != (&r[10] == "forced_close") {
            return Err(bad(28));
        }
        let holding = time(29)?;
        if holding != exit_ms - entry_ms
            || !close(
                number(30)?,
                holding as f64 / (window.end_ms - window.start_ms) as f64,
            )
        {
            return Err(bad(29));
        }
        let stop = optional(31)?;
        let risk = optional(32)?;
        let r_multiple = optional(33)?;
        if let Some(risk) = risk
            && (risk <= 0.
                || !stop.is_some_and(|s| s < entry_price && close(risk, qty * (entry_price - s))))
        {
            return Err(bad(32));
        }
        if let Some(rmultiple) = r_multiple
            && !risk.is_some_and(|risk| close(rmultiple, pnl_net / risk))
        {
            return Err(bad(33));
        }
        let spread = optional(35)?;
        if spread.is_some_and(|v| v < 0.) {
            return Err(bad(35));
        }
        // Le spread taker est déjà dans le prix ; seul le débit maker résiduel sort du cash.
        // Le CSV S10 agrège les deux dans spread_cost, sans déplacer le prix d'un fill maker.
        let adverse_debit = if let Some(spread) = spread {
            let mut taker_spread = 0.;
            if &r[3] == "taker" {
                taker_spread += qty * (entry_price - entry_ref) - entry_slippage;
            }
            if &r[11] == "taker" {
                taker_spread += qty * (exit_ref - exit_price) - exit_slippage;
            }
            if !taker_spread.is_finite() || (taker_spread < 0. && !close(taker_spread, 0.)) {
                return Err(bad(35));
            }
            let debit = spread - taker_spread;
            if !debit.is_finite()
                || (debit < 0. && !close(debit, 0.))
                || (&r[3] == "taker" && &r[11] == "taker" && !close(debit, 0.))
            {
                return Err(bad(35));
            }
            debit.max(0.)
        } else {
            0.
        };
        // PnL et R sont déjà nets : ne déduire les frais ou le spread taker qu'une fois.
        if !close(
            pnl_net,
            qty * (exit_price - entry_price) - entry_fee - exit_fee - adverse_debit,
        ) {
            return Err(bad(17));
        }
        if &r[28] == "end_of_window" {
            terminal_rows.push(row);
        }
        let symbol = r[25].to_owned();
        trades.push(TradeRecord {
            row,
            lot_id,
            symbol,
            entry_ms,
            exit_ms,
            qty,
            entry_price,
            exit_price,
            entry_fee,
            exit_fee,
            pnl_net,
            r_multiple,
            included: !(purge_end_of_window && &r[28] == "end_of_window"),
        });
    }
    Ok((trades, terminal_rows))
}
