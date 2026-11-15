//! Capture froide depuis les séries admises et les traces exactes du moteur.
use crate::{
    engine::{CellResult, PortfolioResult},
    model::{Candle, Timeframe, hash},
    report::{
        self, Metrics, ReportError,
        profiles::RenderedProfile,
        stats::{self, RunSnapshot},
        stats_inputs::{
            CONVENTIONS, STATS_INPUTS_FILE, STATS_INPUTS_SCHEMA, SimulationHashes, StatsInputs,
            StatsInputsRef, StatsProvenance,
        },
    },
    research::stats::{
        CashPoint, MarkPoint, MarketSeries, PricePoint, StatsSizing, TradeClosure, Window,
    },
    sim::{ExitReason, SizingMode, Trade},
    store::Columns,
};
use std::{collections::BTreeMap, ops::Deref, path::Path};
const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
fn invalid(detail: String) -> ReportError {
    ReportError::Series { detail }
}

/// Le rendu public reste indépendant de la capture et accepte ses fixtures historiques.
pub(super) struct CapturedProfile {
    rendered: RenderedProfile,
    inputs: StatsInputs,
}
impl Deref for CapturedProfile {
    type Target = RenderedProfile;
    fn deref(&self) -> &Self::Target {
        &self.rendered
    }
}
impl CapturedProfile {
    pub fn write_inputs(&self, dir: &Path) -> Result<StatsInputsRef, ReportError> {
        write(dir, &self.inputs)
    }
}
pub(super) fn write(dir: &Path, packet: &StatsInputs) -> Result<StatsInputsRef, ReportError> {
    let bytes = report::json_bytes(packet)?;
    if let Err(error) = report::write_bytes_atomic(dir, STATS_INPUTS_FILE, &bytes) {
        // Une erreur de rename/sync ne doit pas conserver un paquet provisoire.
        let _ = std::fs::remove_file(dir.join("stats-inputs.json.tmp"));
        return Err(error);
    }
    Ok(StatsInputsRef {
        file: STATS_INPUTS_FILE.into(),
        schema: STATS_INPUTS_SCHEMA.into(),
        hash: hash::blake3_prefixed(&bytes),
    })
}
fn hashes(metrics: &[u8], trades: &[u8], equity: &[u8]) -> SimulationHashes {
    SimulationHashes {
        metrics_json: hash::blake3_prefixed(metrics),
        trades_csv: hash::blake3_prefixed(trades),
        equity_csv: hash::blake3_prefixed(equity),
    }
}

/// Les profils d'une même ligne partagent l'extraction des prix ; seules les petites séries
/// quotidiennes sont copiées dans chaque paquet, jamais les colonnes minute.
#[derive(Default)]
pub(super) struct MarketCache {
    key: Option<(Window, Vec<String>)>,
    markets: Vec<MarketSeries>,
}

pub(super) fn portfolio(
    rendered: RenderedProfile,
    result: &PortfolioResult,
    series: &[(String, Columns)],
    tf: Timeframe,
    cache: &mut MarketCache,
) -> Result<CapturedProfile, ReportError> {
    let metrics = &rendered.metrics;
    let files = hashes(
        &rendered.metrics_bytes,
        &rendered.trades_bytes,
        &rendered.equity_bytes,
    );
    let window = Window {
        start_ms: metrics.data.window.start_ms,
        end_ms: metrics.data.window.end_ms,
    };
    let provenance = StatsProvenance {
        run_id: metrics.run_id(),
        config_hash: metrics.config_hash.clone(),
        dataset_hash: metrics.dataset_hash.clone(),
        result_hash: files.trades_csv.clone(),
        initial_cash: result.initial_cash,
        window,
        profile: metrics.costs.as_ref().map(|c| c.profile.as_str().into()),
        line: metrics.research.as_ref().map(|r| r.line.as_str().into()),
        files,
    };
    let sizing = match result.sizing {
        SizingMode::Research => StatsSizing::Research,
        SizingMode::Compounding => StatsSizing::Compounding,
    };
    let market_symbols: Vec<_> = metrics
        .data
        .series
        .iter()
        .map(|m| m.symbol.clone())
        .collect();
    let key = (window, market_symbols.clone());
    if cache.key.as_ref() != Some(&key) {
        cache.markets = series
            .iter()
            .filter(|(s, _)| metrics.data.series.iter().any(|m| m.symbol == *s))
            .map(|(s, c)| {
                market(
                    s,
                    c.open_time_ms()
                        .iter()
                        .copied()
                        .zip(c.open().iter().copied())
                        .zip(c.close().iter().copied())
                        .map(|((t, o), c)| (t, o, c)),
                    window,
                )
            })
            .collect();
        cache.key = Some(key);
    }
    let markets = cache.markets.clone();
    let image = RunSnapshot {
        provenance,
        pair_symbols: result.universe.clone(),
        market_symbols,
        sizing,
        trades: stats::parse_trades(&rendered.trades_bytes, result.research.is_some(), window)
            .map_err(invalid)?,
        terminal_rows: terminal_rows(&result.trades),
        decision_timeframe_minutes: Some(minutes(tf)),
        cash_final: result.cash_final,
        equity_final: result.equity_final,
    };
    let inputs = capture(
        &image,
        &result.grid_open_ms,
        &result.equity,
        &result.cash,
        markets,
        tf,
    )?;
    Ok(CapturedProfile { rendered, inputs })
}
pub(super) fn cell(
    metrics: &Metrics,
    result: &CellResult,
    candles: &[Candle],
    grid: &[i64],
    tf: Timeframe,
    bytes: [&[u8]; 3],
) -> Result<StatsInputs, ReportError> {
    let window = Window {
        start_ms: metrics.window.first_open_time_ms,
        end_ms: metrics.window.last_open_time_ms + MINUTE,
    };
    let files = hashes(bytes[0], bytes[1], bytes[2]);
    let provenance = StatsProvenance {
        run_id: report::run_id(
            &metrics.config_hash,
            &metrics.dataset_hash,
            window.start_ms,
            window.end_ms,
        ),
        config_hash: metrics.config_hash.clone(),
        dataset_hash: metrics.dataset_hash.clone(),
        result_hash: files.trades_csv.clone(),
        initial_cash: result.config.initial_cash,
        window,
        profile: None,
        line: None,
        files,
    };
    let symbols = vec![metrics.data.symbol.clone()];
    let image = RunSnapshot {
        provenance,
        pair_symbols: symbols.clone(),
        market_symbols: symbols,
        sizing: StatsSizing::Compounding,
        trades: stats::parse_trades(bytes[1], false, window).map_err(invalid)?,
        terminal_rows: terminal_rows(&result.trades),
        decision_timeframe_minutes: Some(minutes(tf)),
        cash_final: result.cash_final,
        equity_final: result.equity_final,
    };
    let markets = vec![market(
        &metrics.data.symbol,
        candles.iter().map(|c| (c.open_time_ms, c.open, c.close)),
        window,
    )];
    capture(&image, grid, &result.equity, &result.cash, markets, tf)
}
fn minutes(tf: Timeframe) -> u32 {
    (tf.ms() / MINUTE) as u32
}
fn terminal_rows(trades: &[Trade]) -> Vec<usize> {
    trades
        .iter()
        .enumerate()
        .filter(|(_, t)| t.exit_reason == ExitReason::ForcedClose)
        .map(|(i, _)| i)
        .collect()
}
fn capture(
    image: &RunSnapshot,
    grid: &[i64],
    equity: &[f64],
    cash: &[f64],
    markets: Vec<MarketSeries>,
    tf: Timeframe,
) -> Result<StatsInputs, ReportError> {
    if grid.len() != equity.len() || grid.len() != cash.len() {
        return Err(invalid("traces moteur non alignées".into()));
    }
    let w = image.provenance.window;
    let mut marks = BTreeMap::new();
    marks.insert(w.start_ms, image.provenance.initial_cash);
    for (&t, &e) in grid.iter().zip(equity) {
        let close = (t + MINUTE).min(w.end_ms);
        if close.rem_euclid(DAY) == 0 {
            marks.insert(close, e);
        }
    }
    let mut closures = Vec::with_capacity(image.trades.len());
    for t in &image.trades {
        let row = t.row;
        let i = grid.partition_point(|&g| g < t.exit_ms);
        let at_ms = if image.terminal_rows.binary_search(&row).is_ok() {
            w.end_ms
        } else {
            grid.get(i)
                .map_or(w.end_ms, |g| (*g + MINUTE).min(w.end_ms))
        };
        if at_ms != w.end_ms {
            let e = equity
                .get(i)
                .ok_or_else(|| invalid("réalisation sans equity moteur".into()))?;
            marks.insert(at_ms, *e);
        }
        closures.push(TradeClosure { row, at_ms });
    }
    marks.insert(w.end_ms, image.equity_final);
    let mut cash_points = Vec::new();
    for (&t, &balance) in grid.iter().zip(cash) {
        if cash_points
            .last()
            .is_none_or(|p: &CashPoint| p.cash != balance)
        {
            cash_points.push(CashPoint {
                at_ms: t,
                cash: balance,
            });
        }
    }
    if cash_points.first().is_none_or(|p| p.at_ms != w.start_ms) {
        return Err(invalid("cash moteur absent à l'origine".into()));
    }
    cash_points.push(CashPoint {
        at_ms: w.end_ms,
        cash: image.cash_final,
    });
    let packet = StatsInputs {
        schema: STATS_INPUTS_SCHEMA.into(),
        conventions: CONVENTIONS.into(),
        provenance: image.provenance.clone(),
        pair_symbols: image.pair_symbols.clone(),
        sizing: image.sizing,
        decision_timeframe_minutes: minutes(tf),
        closures,
        marks: marks
            .into_iter()
            .map(|(at_ms, equity)| MarkPoint {
                at_ms,
                equity,
                day_close: at_ms.rem_euclid(DAY) == 0,
            })
            .collect(),
        cash: cash_points,
        markets,
    };
    packet.validate_against(image).map_err(invalid)?;
    Ok(packet)
}
/// Jours complets seulement ; aucune imputation ni copie de la série minute.
fn market(symbol: &str, rows: impl Iterator<Item = (i64, f64, f64)>, w: Window) -> MarketSeries {
    let mut daily_closes = Vec::new();
    let mut valuations = BTreeMap::new();
    let mut day_start = None;
    let mut count = 0usize;
    let mut previous = None;
    for (t, open, close) in rows {
        if t >= w.end_ms {
            break;
        }
        if t == w.start_ms {
            valuations.insert(t, open);
        }
        let at = t + MINUTE;
        if at > w.start_ms && at <= w.end_ms && (at.rem_euclid(DAY) == 0 || at == w.end_ms) {
            valuations.insert(at, close);
        }
        if previous.is_some_and(|p| p + MINUTE != t) {
            day_start = None;
        }
        if t.rem_euclid(DAY) == 0 {
            day_start = Some(t);
            count = 0;
        }
        count += 1;
        if at.rem_euclid(DAY) == 0 && day_start == Some(at - DAY) && count == 1440 {
            daily_closes.push(PricePoint { at_ms: at, close });
        }
        previous = Some(t);
    }
    MarketSeries {
        symbol: symbol.into(),
        daily_closes,
        valuations: valuations
            .into_iter()
            .map(|(at_ms, close)| PricePoint { at_ms, close })
            .collect(),
    }
}
/// Retire d'abord tous les marqueurs de complétude, même si un ancien layout échoue au nettoyage.
pub(super) fn cleanup(dir: &Path) -> Result<(), ReportError> {
    clean_tree(dir, 0, true)?;
    clean_tree(dir, 0, false)
}
fn clean_tree(dir: &Path, depth: u8, markers: bool) -> Result<(), ReportError> {
    let files: &[&str] = if markers {
        &[
            "manifest.json",
            "manifest.json.tmp",
            "profiles.json",
            "lines.json",
        ]
    } else {
        &[
            STATS_INPUTS_FILE,
            "stats.json",
            "report.html",
            "stats-inputs.json.tmp",
            "stats.json.tmp",
        ]
    };
    for name in files {
        super::remove_stale(dir, name)?;
    }
    if depth < 2 {
        for name in ["full", "okx_usdc", "baseline", "adverse", "maker"] {
            let sub = dir.join(name);
            if std::fs::symlink_metadata(&sub).is_ok_and(|m| m.file_type().is_dir()) {
                clean_tree(&sub, depth + 1, markers)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historique_ne_prolonge_pas_les_jours_incomplets_et_ouverture_distincte() {
        let window = Window {
            start_ms: DAY + MINUTE,
            end_ms: 3 * DAY,
        };
        let rows = (0..4320)
            .filter(|&i| i != 1439)
            .map(|i| (i * MINUTE, 200. + i as f64, 100. + i as f64));
        let m = market("ETH-USD", rows, window);
        assert_eq!(
            m.daily_closes.iter().map(|p| p.at_ms).collect::<Vec<_>>(),
            [2 * DAY, 3 * DAY]
        );
        assert_eq!(
            m.valuations[0],
            PricePoint {
                at_ms: window.start_ms,
                close: 1641.
            }
        );
        assert!(m.valuations.iter().all(|p| p.at_ms <= window.end_ms));
        let midnight = market(
            "ETH-USD",
            (0..4320).map(|i| (i * MINUTE, 200. + i as f64, 100. + i as f64)),
            Window {
                start_ms: DAY,
                end_ms: 2 * DAY,
            },
        );
        assert_eq!(
            midnight.daily_closes[0],
            PricePoint {
                at_ms: DAY,
                close: 1539.
            }
        );
        assert_eq!(
            midnight.valuations[0],
            PricePoint {
                at_ms: DAY,
                close: 1640.
            },
            "l'ouverture initiale diffère de la clôture de la veille au même instant"
        );
    }
    #[test]
    fn sorties_marquees_a_la_grille_et_liquidation_a_la_fin_globale() {
        use crate::research::stats::TradeRecord;
        let h = format!("blake3:{}", "a".repeat(64));
        let trades = (0..2)
            .map(|row| TradeRecord {
                row,
                lot_id: row as u64,
                symbol: "ETH-USD".into(),
                entry_ms: 0,
                exit_ms: if row == 0 { 2 * MINUTE } else { MINUTE },
                qty: 1.,
                entry_price: 100.,
                exit_price: 100.5,
                entry_fee: 0.,
                exit_fee: 0.,
                pnl_net: 0.5,
                r_multiple: None,
                included: row == 0,
            })
            .collect();
        let image = RunSnapshot {
            provenance: StatsProvenance {
                run_id: "a".repeat(64),
                config_hash: "b".repeat(64),
                dataset_hash: h.clone(),
                result_hash: h.clone(),
                initial_cash: 300.,
                window: Window {
                    start_ms: 0,
                    end_ms: 10 * MINUTE,
                },
                profile: None,
                line: None,
                files: SimulationHashes {
                    metrics_json: h.clone(),
                    trades_csv: h.clone(),
                    equity_csv: h,
                },
            },
            pair_symbols: vec!["ETH-USD".into()],
            market_symbols: vec!["ETH-USD".into()],
            sizing: StatsSizing::Compounding,
            trades,
            terminal_rows: vec![1],
            decision_timeframe_minutes: Some(1),
            cash_final: 301.,
            equity_final: 301.,
        };
        let packet = capture(
            &image,
            &[0, MINUTE, 8 * MINUTE, 9 * MINUTE],
            &[300., 200., 320., 350.],
            &[299., 200., 200., 300.],
            vec![],
            Timeframe::M1,
        )
        .unwrap();
        assert_eq!(
            packet.closures,
            [
                TradeClosure {
                    row: 0,
                    at_ms: 9 * MINUTE
                },
                TradeClosure {
                    row: 1,
                    at_ms: 10 * MINUTE
                }
            ]
        );
        assert_eq!(
            packet.marks.last().unwrap().equity,
            301.,
            "coût terminal postérieur au dernier marquage minute"
        );
        assert_eq!(
            packet
                .marks
                .iter()
                .find(|p| p.at_ms == 9 * MINUTE)
                .unwrap()
                .equity,
            320.
        );
        assert_eq!(
            packet.cash,
            vec![
                CashPoint {
                    at_ms: 0,
                    cash: 299.
                },
                CashPoint {
                    at_ms: MINUTE,
                    cash: 200.
                },
                CashPoint {
                    at_ms: 9 * MINUTE,
                    cash: 300.
                },
                CashPoint {
                    at_ms: 10 * MINUTE,
                    cash: 301.
                }
            ]
        );
        assert!(
            !packet.marks.iter().any(|p| p.at_ms == 3 * MINUTE),
            "aucun déplacement fictif de la sortie économique"
        );
        let mut gap_image = image.clone();
        gap_image.provenance.window.end_ms = 2 * DAY + MINUTE;
        let gap = capture(
            &gap_image,
            &[0, DAY - 2 * MINUTE, DAY + MINUTE],
            &[300., 200., 350.],
            &[299., 200., 300.],
            vec![],
            Timeframe::M1,
        )
        .unwrap();
        assert!(
            !gap.marks
                .iter()
                .any(|m| m.at_ms == DAY || m.at_ms == 2 * DAY),
            "aucun marquage quotidien prolongé à travers un trou de grille"
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(STATS_INPUTS_FILE)).unwrap();
        assert!(write(dir.path(), &packet).is_err());
        assert!(
            !dir.path().join("stats-inputs.json.tmp").exists(),
            "paquet temporaire retiré après échec de rename"
        );
    }
}
