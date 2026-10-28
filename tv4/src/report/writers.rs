//! Écriture de `trades.csv` (docs/03-architecture.md §7.2 et §11).
//!
//! Le format est minimal et stable : séparateur `,`, fin de ligne `\n`, en-tête toujours écrit (même
//! sans trade), un trade par ligne dans l'ordre du simulateur. Les entiers sont écrits en décimal, les
//! `f64` par `Display` (la représentation la plus courte qui se relit à l'identique, jamais d'exposant),
//! les valeurs absentes sont des champs vides, les booléens valent `true` ou `false`. Deux runs
//! identiques produisent des fichiers identiques à l'octet.
//!
//! Depuis S8 le fichier écrit a 36 colonnes : les 25 colonnes de S6 ([`TRADES_COLUMNS_S6`], inchangées et
//! dans le même ordre) puis 11 colonnes dérivées. [`trades_csv_s6`] garde le rendu de S6 à l'octet près
//! (c'est lui que l'oracle Python recoupe) et [`project_s6`] ramène un fichier S8 à ses 25 premières
//! colonnes.
//!
//! Vocabulaire de `exit_type` : `time_stop`, `strategy_exit`, `limit_sell`, `stop_loss`,
//! `trailing_stop`, `take_profit`, `end_of_window`. Les valeurs `roll` (F3 rejetée par G0) et
//! `gap_forced_exit` (un trou signifie « le temps avance, pas le prix », docs/03 §5.1) ne sont jamais
//! écrites au jalon 1. Les colonnes `regime_at_entry` (étiquette de régime, jalon 1.5) et
//! `spread_cost` (spread non modélisé, S10) sont toujours vides au jalon 1.

use std::path::{Path, PathBuf};

use crate::sim::{ExitReason, Liquidity, Trade};

use super::{ReportError, write_bytes_atomic};

/// Tag de sortie réservé aux sorties temporelles, qui donne `exit_type = time_stop`. Même valeur que
/// `strategy::TIME_STOP_TAG` (`report` ne connaît pas `strategy` ; `tests/report_s8.rs` vérifie
/// l'égalité).
pub const TIME_STOP_TAG: &str = "time_stop";

/// Nom du fichier écrit.
pub const TRADES_FILE: &str = "trades.csv";

/// Colonnes de S6, dans l'ordre : les 25 premières colonnes de `trades.csv`.
pub const TRADES_COLUMNS_S6: [&str; 25] = [
    "lot_id",
    "entry_signal_ms",
    "entry_ms",
    "entry_liquidity",
    "entry_ref_price",
    "entry_price",
    "entry_fee",
    "entry_slippage",
    "exit_signal_ms",
    "exit_ms",
    "exit_reason",
    "exit_liquidity",
    "exit_ref_price",
    "exit_price",
    "exit_fee",
    "exit_slippage",
    "qty",
    "pnl_net",
    "mfe",
    "mae",
    "stop_at_exit",
    "capped_by_cash",
    "forced_close",
    "entry_tag",
    "exit_tag",
];

/// Colonnes de `trades.csv`, dans l'ordre : celles de S6 puis les 11 de S8.
pub const TRADES_COLUMNS: [&str; 36] = [
    "lot_id",
    "entry_signal_ms",
    "entry_ms",
    "entry_liquidity",
    "entry_ref_price",
    "entry_price",
    "entry_fee",
    "entry_slippage",
    "exit_signal_ms",
    "exit_ms",
    "exit_reason",
    "exit_liquidity",
    "exit_ref_price",
    "exit_price",
    "exit_fee",
    "exit_slippage",
    "qty",
    "pnl_net",
    "mfe",
    "mae",
    "stop_at_exit",
    "capped_by_cash",
    "forced_close",
    "entry_tag",
    "exit_tag",
    "symbol",
    "entry_order_ms",
    "exit_order_ms",
    "exit_type",
    "holding_ms",
    "position_time_share",
    "stop_at_entry",
    "risk_quote",
    "r_multiple",
    "regime_at_entry",
    "spread_cost",
];

/// Les sept `exit_type` écrits au jalon 1, triés.
pub const EXIT_TYPES: [&str; 7] = [
    "end_of_window",
    "limit_sell",
    "stop_loss",
    "strategy_exit",
    "take_profit",
    "time_stop",
    "trailing_stop",
];

/// Ce que le writer de 36 colonnes sait en dehors des trades.
#[derive(Clone, Copy, Debug)]
pub struct TradesContext<'a> {
    /// Symbole canonique des données (`BTC-USD`).
    pub symbol: &'a str,
    /// `last_open_time_ms + 60 000 − first_open_time_ms` de la fenêtre jouée.
    pub span_ms: i64,
}

fn liquidity(value: Liquidity) -> &'static str {
    match value {
        Liquidity::Maker => "maker",
        Liquidity::Taker => "taker",
    }
}

fn reason(value: ExitReason) -> &'static str {
    match value {
        ExitReason::Signal => "signal",
        ExitReason::LimitSell => "limit_sell",
        ExitReason::Stop => "stop",
        ExitReason::TakeProfit => "take_profit",
        ExitReason::ForcedClose => "forced_close",
    }
}

/// `exit_type` d'un trade (vocabulaire de docs/03 §15.1 ligne 3).
pub fn exit_type(trade: &Trade) -> &'static str {
    match trade.exit_reason {
        ExitReason::Signal if trade.exit_tag == TIME_STOP_TAG => "time_stop",
        ExitReason::Signal => "strategy_exit",
        ExitReason::LimitSell => "limit_sell",
        ExitReason::Stop if trade.stop_moved => "trailing_stop",
        ExitReason::Stop => "stop_loss",
        ExitReason::TakeProfit => "take_profit",
        ExitReason::ForcedClose => "end_of_window",
    }
}

/// Risque du trade en quote : `qty × (entry_price − stop_at_entry)` si le stop d'entrée est sous le
/// prix d'entrée effectif (slippage compris, frais exclus), sinon `None`.
pub fn risk_quote(trade: &Trade) -> Option<f64> {
    match trade.stop_at_entry {
        Some(stop) if stop < trade.entry_price => Some(trade.qty * (trade.entry_price - stop)),
        _ => None,
    }
}

/// Multiple de risque `pnl_net / risk_quote`, défini quand le risque l'est (et strictement positif).
pub fn r_multiple(trade: &Trade) -> Option<f64> {
    risk_quote(trade)
        .filter(|risk| *risk > 0.0)
        .map(|risk| trade.pnl_net / risk)
}

/// Durée de détention `exit_ms − entry_ms`.
pub fn holding_ms(trade: &Trade) -> Result<i64, ReportError> {
    trade
        .exit_ms
        .checked_sub(trade.entry_ms)
        .ok_or(ReportError::Overflow {
            context: "holding_ms",
        })
}

fn optional(value: Option<f64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// Les 25 champs de S6 d'un trade.
fn row_s6(t: &Trade) -> [String; 25] {
    [
        t.lot_id.to_string(),
        t.entry_signal_ms.to_string(),
        t.entry_ms.to_string(),
        liquidity(t.entry_liquidity).to_string(),
        t.entry_ref_price.to_string(),
        t.entry_price.to_string(),
        t.entry_fee.to_string(),
        t.entry_slippage.to_string(),
        t.exit_signal_ms.to_string(),
        t.exit_ms.to_string(),
        reason(t.exit_reason).to_string(),
        liquidity(t.exit_liquidity).to_string(),
        t.exit_ref_price.to_string(),
        t.exit_price.to_string(),
        t.exit_fee.to_string(),
        t.exit_slippage.to_string(),
        t.qty.to_string(),
        t.pnl_net.to_string(),
        t.mfe.to_string(),
        t.mae.to_string(),
        optional(t.stop_at_exit),
        t.capped_by_cash.to_string(),
        t.forced_close.to_string(),
        t.entry_tag.to_string(),
        t.exit_tag.to_string(),
    ]
}

fn csv_writer() -> csv::Writer<Vec<u8>> {
    csv::WriterBuilder::new()
        .delimiter(b',')
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new())
}

fn finish(writer: csv::Writer<Vec<u8>>) -> Result<Vec<u8>, ReportError> {
    writer
        .into_inner()
        .map_err(|e| ReportError::Csv(e.to_string()))
}

/// Contenu de `trades.csv` de S6 (25 colonnes) en mémoire : octets inchangés depuis S6, référence de
/// l'oracle.
pub fn trades_csv_s6(trades: &[Trade]) -> Result<Vec<u8>, ReportError> {
    let csv_error = |e: csv::Error| ReportError::Csv(e.to_string());
    let mut writer = csv_writer();
    writer.write_record(TRADES_COLUMNS_S6).map_err(csv_error)?;
    for t in trades {
        writer.write_record(row_s6(t)).map_err(csv_error)?;
    }
    finish(writer)
}

/// Contenu de `trades.csv` (36 colonnes) en mémoire.
pub fn trades_csv(trades: &[Trade], context: &TradesContext<'_>) -> Result<Vec<u8>, ReportError> {
    let csv_error = |e: csv::Error| ReportError::Csv(e.to_string());
    let mut writer = csv_writer();
    writer.write_record(TRADES_COLUMNS).map_err(csv_error)?;
    for t in trades {
        let holding = holding_ms(t)?;
        let risk = risk_quote(t);
        let r = r_multiple(t);
        let mut row: Vec<String> = Vec::with_capacity(TRADES_COLUMNS.len());
        row.extend(row_s6(t));
        row.push(context.symbol.to_string());
        row.push(t.entry_order_ms.to_string());
        row.push(t.exit_order_ms.to_string());
        row.push(exit_type(t).to_string());
        row.push(holding.to_string());
        row.push((holding as f64 / context.span_ms as f64).to_string());
        row.push(optional(t.stop_at_entry));
        row.push(optional(risk));
        row.push(optional(r));
        // `regime_at_entry` et `spread_cost` : vides au jalon 1.
        row.push(String::new());
        row.push(String::new());
        writer.write_record(&row).map_err(csv_error)?;
    }
    finish(writer)
}

/// Relit un `trades.csv` de S8 et réécrit ses 25 premières colonnes avec les réglages du writer : le
/// résultat est `trades_csv_s6` des mêmes trades, à l'octet.
pub fn project_s6(bytes: &[u8]) -> Result<Vec<u8>, ReportError> {
    let csv_error = |e: csv::Error| ReportError::CsvRead(e.to_string());
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b',')
        .has_headers(false)
        .from_reader(bytes);
    let mut writer = csv_writer();
    for record in reader.records() {
        let record = record.map_err(csv_error)?;
        if record.len() < TRADES_COLUMNS_S6.len() {
            return Err(ReportError::CsvRead(format!(
                "{} champs au lieu d'au moins {}",
                record.len(),
                TRADES_COLUMNS_S6.len()
            )));
        }
        writer
            .write_record(record.iter().take(TRADES_COLUMNS_S6.len()))
            .map_err(|e| ReportError::Csv(e.to_string()))?;
    }
    finish(writer)
}

/// Écrit `trades.csv` (36 colonnes) dans `dir` de façon atomique (fichier `.tmp`, `sync_all`,
/// `rename`). Rend le chemin final.
pub fn write_trades_csv(
    dir: &Path,
    trades: &[Trade],
    context: &TradesContext<'_>,
) -> Result<PathBuf, ReportError> {
    let bytes = trades_csv(trades, context)?;
    write_bytes_atomic(dir, TRADES_FILE, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(stop: Option<f64>, forced: bool) -> Trade {
        Trade {
            lot_id: 3,
            entry_signal_ms: 1_000,
            entry_order_ms: 61_000,
            entry_ms: 61_000,
            entry_ref_price: 100.0,
            entry_price: 100.05,
            entry_fee: 0.5,
            entry_slippage: 0.25,
            entry_liquidity: Liquidity::Taker,
            exit_signal_ms: 120_000,
            exit_order_ms: 121_000,
            exit_ms: 181_000,
            exit_ref_price: 103.0,
            exit_price: 102.9485,
            exit_fee: 0.125,
            exit_slippage: 0.0,
            exit_liquidity: Liquidity::Maker,
            qty: 0.00000001,
            pnl_net: -1.5,
            mfe: 2.0,
            mae: -0.75,
            stop_at_exit: stop,
            stop_at_entry: stop,
            stop_moved: false,
            capped_by_cash: true,
            exit_reason: if forced {
                ExitReason::ForcedClose
            } else {
                ExitReason::Signal
            },
            forced_close: forced,
            entry_tag: "cross_up",
            exit_tag: if forced { "forced_close" } else { "cross_down" },
        }
    }

    const EN_TETE_S6: &str = "lot_id,entry_signal_ms,entry_ms,entry_liquidity,entry_ref_price,entry_price,\
         entry_fee,entry_slippage,exit_signal_ms,exit_ms,exit_reason,exit_liquidity,exit_ref_price,\
         exit_price,exit_fee,exit_slippage,qty,pnl_net,mfe,mae,stop_at_exit,capped_by_cash,\
         forced_close,entry_tag,exit_tag";

    #[test]
    fn trades_csv_s6_en_tete_et_format() {
        let texte = String::from_utf8(trades_csv_s6(&[]).unwrap()).unwrap();
        assert_eq!(
            texte,
            format!("{EN_TETE_S6}\n"),
            "l'en-tête est écrit même sans trade"
        );
        let texte = String::from_utf8(
            trades_csv_s6(&[trade(None, false), trade(Some(95.5), true)]).unwrap(),
        )
        .unwrap();
        let lignes: Vec<&str> = texte.lines().collect();
        assert_eq!(lignes.len(), 3);
        assert_eq!(
            lignes[1],
            "3,1000,61000,taker,100,100.05,0.5,0.25,120000,181000,signal,maker,103,102.9485,0.125,0,\
             0.00000001,-1.5,2,-0.75,,true,false,cross_up,cross_down",
            "stop_at_exit vide, f64 par Display, booléens en toutes lettres"
        );
        assert_eq!(
            lignes[2],
            "3,1000,61000,taker,100,100.05,0.5,0.25,120000,181000,forced_close,maker,103,102.9485,0.125,0,\
             0.00000001,-1.5,2,-0.75,95.5,true,true,cross_up,forced_close"
        );
        assert!(!texte.contains('\r'));
    }

    #[test]
    fn les_colonnes_s8_prolongent_celles_de_s6() {
        assert_eq!(TRADES_COLUMNS[..25], TRADES_COLUMNS_S6);
        assert_eq!(TRADES_COLUMNS.len(), 36);
    }

    #[test]
    fn trades_csv_36_colonnes() {
        let context = TradesContext {
            symbol: "BTC-USD",
            span_ms: 1_000_000,
        };
        let texte = String::from_utf8(
            trades_csv(&[trade(Some(95.5), false), trade(None, true)], &context).unwrap(),
        )
        .unwrap();
        let lignes: Vec<&str> = texte.lines().collect();
        assert_eq!(lignes.len(), 3);
        assert_eq!(lignes[0], TRADES_COLUMNS.join(","));
        // risk = 1e-8 × (100.05 − 95.5) = 4.55e-8 environ ; pas de valeur à comparer en dur : on relit.
        let champs: Vec<&str> = lignes[1].split(',').collect();
        assert_eq!(champs.len(), 36);
        assert_eq!(champs[25], "BTC-USD");
        assert_eq!(champs[26], "61000");
        assert_eq!(champs[27], "121000");
        assert_eq!(champs[28], "strategy_exit");
        assert_eq!(champs[29], "120000");
        assert_eq!(champs[30], (120_000f64 / 1_000_000f64).to_string());
        assert_eq!(champs[31], "95.5");
        let risque: f64 = champs[32].parse().unwrap();
        assert_eq!(risque, 0.00000001 * (100.05 - 95.5));
        let r: f64 = champs[33].parse().unwrap();
        assert_eq!(r, -1.5 / risque);
        assert_eq!((champs[34], champs[35]), ("", ""));
        // Sans stop à l'entrée : ni risque ni R, `exit_type` de la clôture forcée.
        let champs: Vec<&str> = lignes[2].split(',').collect();
        assert_eq!(champs[28], "end_of_window");
        assert_eq!((champs[31], champs[32], champs[33]), ("", "", ""));
    }

    #[test]
    fn la_projection_s6_redonne_le_rendu_de_s6() {
        let context = TradesContext {
            symbol: "BTC-USD",
            span_ms: 1_000_000,
        };
        let trades = [trade(None, false), trade(Some(95.5), true)];
        let complet = trades_csv(&trades, &context).unwrap();
        assert_eq!(
            project_s6(&complet).unwrap(),
            trades_csv_s6(&trades).unwrap()
        );
        let vide = trades_csv(&[], &context).unwrap();
        assert_eq!(project_s6(&vide).unwrap(), trades_csv_s6(&[]).unwrap());
        assert!(project_s6(b"a,b\n1,2\n").is_err());
    }

    #[test]
    fn exit_type_selon_la_raison_le_tag_et_le_stop_deplace() {
        let mut t = trade(Some(95.0), false);
        assert_eq!(exit_type(&t), "strategy_exit");
        t.exit_tag = TIME_STOP_TAG;
        assert_eq!(exit_type(&t), "time_stop");
        t.exit_reason = ExitReason::LimitSell;
        assert_eq!(exit_type(&t), "limit_sell");
        t.exit_reason = ExitReason::Stop;
        assert_eq!(exit_type(&t), "stop_loss");
        t.stop_moved = true;
        assert_eq!(exit_type(&t), "trailing_stop");
        t.exit_reason = ExitReason::TakeProfit;
        assert_eq!(exit_type(&t), "take_profit");
        t.exit_reason = ExitReason::ForcedClose;
        assert_eq!(exit_type(&t), "end_of_window");
        for type_ in EXIT_TYPES {
            assert!(!type_.is_empty());
        }
    }

    #[test]
    fn risque_et_r_multiple() {
        let mut t = trade(Some(90.0), false);
        t.qty = 2.0;
        t.entry_price = 100.0;
        t.pnl_net = 5.0;
        assert_eq!(risk_quote(&t), Some(20.0));
        assert_eq!(r_multiple(&t), Some(0.25));
        t.stop_at_entry = Some(100.0);
        assert_eq!(risk_quote(&t), None);
        assert_eq!(r_multiple(&t), None);
        t.stop_at_entry = Some(110.0);
        assert_eq!(risk_quote(&t), None);
        t.stop_at_entry = None;
        assert_eq!(r_multiple(&t), None);
    }

    #[test]
    fn ecriture_atomique_de_trades_csv() {
        let dir = std::env::temp_dir().join(format!("tv4-trades-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let context = TradesContext {
            symbol: "BTC-USD",
            span_ms: 1_000_000,
        };
        let chemin = write_trades_csv(&dir, &[trade(None, false)], &context).unwrap();
        assert_eq!(chemin, dir.join("trades.csv"));
        assert!(!dir.join("trades.csv.tmp").exists());
        assert_eq!(
            std::fs::read(&chemin).unwrap(),
            trades_csv(&[trade(None, false)], &context).unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
