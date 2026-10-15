//! Écriture de `trades.csv` (docs/03-architecture.md §7.2 et §11), produit dès S6.
//!
//! Le format est minimal et stable : séparateur `,`, fin de ligne `\n`, en-tête toujours écrit (même
//! sans trade), un trade par ligne dans l'ordre du simulateur. Les entiers sont écrits en décimal, les
//! `f64` par `Display` (la représentation la plus courte qui se relit à l'identique, jamais d'exposant),
//! `stop_at_exit` est vide quand il n'y avait pas de stop, les booléens valent `true` ou `false`.
//! Deux runs identiques produisent des fichiers identiques à l'octet.

use std::path::{Path, PathBuf};

use crate::sim::{ExitReason, Liquidity, Trade};

use super::{ReportError, write_bytes_atomic};

/// Nom du fichier écrit.
pub const TRADES_FILE: &str = "trades.csv";

/// Colonnes de `trades.csv`, dans l'ordre.
pub const TRADES_COLUMNS: [&str; 25] = [
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

/// Contenu de `trades.csv` en mémoire.
pub fn trades_csv(trades: &[Trade]) -> Result<Vec<u8>, ReportError> {
    let csv_error = |e: csv::Error| ReportError::Csv(e.to_string());
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b',')
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    writer.write_record(TRADES_COLUMNS).map_err(csv_error)?;
    for t in trades {
        let row = [
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
            t.stop_at_exit.map(|s| s.to_string()).unwrap_or_default(),
            t.capped_by_cash.to_string(),
            t.forced_close.to_string(),
            t.entry_tag.to_string(),
            t.exit_tag.to_string(),
        ];
        writer.write_record(&row).map_err(csv_error)?;
    }
    writer
        .into_inner()
        .map_err(|e| ReportError::Csv(e.to_string()))
}

/// Écrit `trades.csv` dans `dir` de façon atomique (fichier `.tmp`, `sync_all`, `rename`). Rend le
/// chemin final.
pub fn write_trades_csv(dir: &Path, trades: &[Trade]) -> Result<PathBuf, ReportError> {
    let bytes = trades_csv(trades)?;
    write_bytes_atomic(dir, TRADES_FILE, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(stop: Option<f64>, forced: bool) -> Trade {
        Trade {
            lot_id: 3,
            entry_signal_ms: 1_000,
            entry_ms: 61_000,
            entry_ref_price: 100.0,
            entry_price: 100.05,
            entry_fee: 0.5,
            entry_slippage: 0.25,
            entry_liquidity: Liquidity::Taker,
            exit_signal_ms: 120_000,
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

    #[test]
    fn trades_csv_en_tete_et_format() {
        let texte = String::from_utf8(trades_csv(&[]).unwrap()).unwrap();
        assert_eq!(
            texte,
            "lot_id,entry_signal_ms,entry_ms,entry_liquidity,entry_ref_price,entry_price,entry_fee,\
             entry_slippage,exit_signal_ms,exit_ms,exit_reason,exit_liquidity,exit_ref_price,\
             exit_price,exit_fee,exit_slippage,qty,pnl_net,mfe,mae,stop_at_exit,capped_by_cash,\
             forced_close,entry_tag,exit_tag\n",
            "l'en-tête est écrit même sans trade"
        );
        let texte =
            String::from_utf8(trades_csv(&[trade(None, false), trade(Some(95.5), true)]).unwrap())
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
    fn ecriture_atomique_de_trades_csv() {
        let dir = std::env::temp_dir().join(format!("tv4-trades-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let chemin = write_trades_csv(&dir, &[trade(None, false)]).unwrap();
        assert_eq!(chemin, dir.join("trades.csv"));
        assert!(!dir.join("trades.csv.tmp").exists());
        assert_eq!(
            std::fs::read(&chemin).unwrap(),
            trades_csv(&[trade(None, false)]).unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
