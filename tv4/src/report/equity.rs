//! `equity.csv` : la courbe d'equity échantillonnée (docs/03-architecture.md §7.3).
//!
//! En-tête `time_ms,equity,drawdown`. Une ligne par seau UTC de `step` qui contient au moins une 1m, la
//! valeur étant celle de la dernière 1m présente du seau. `time_ms` est la clôture de cette 1m
//! (`open_time + 60 000`) et `drawdown` le drawdown courant `−(1 − e / pic)` de la série 1m complète,
//! relevé au même point (≤ 0, fraction). Ni ligne initiale, ni ligne après la clôture forcée.

use std::path::{Path, PathBuf};

use crate::model::{Timeframe, Ts};

use super::drawdown::DrawdownTracker;
use super::{ReportError, write_bytes_atomic};

/// Nom du fichier écrit.
pub const EQUITY_FILE: &str = "equity.csv";
/// Colonnes de `equity.csv`, dans l'ordre.
pub const EQUITY_COLUMNS: [&str; 3] = ["time_ms", "equity", "drawdown"];
/// Durée d'une 1m en millisecondes.
const MINUTE_MS: Ts = 60_000;

/// Indices des 1m retenues : la dernière de chaque seau `open_time.div_euclid(step)` (dernière de la
/// série, ou dont la suivante change de seau). Un seau sans 1m ne produit pas de ligne.
pub fn sample_indices(open_times: &[Ts], step: Timeframe) -> Vec<usize> {
    let step_ms = step.ms();
    let mut out = Vec::new();
    for (j, &open) in open_times.iter().enumerate() {
        let last_of_bucket = match open_times.get(j + 1) {
            Some(&next) => next.div_euclid(step_ms) != open.div_euclid(step_ms),
            None => true,
        };
        if last_of_bucket {
            out.push(j);
        }
    }
    out
}

fn check_lengths(open_times: &[Ts], equity: &[f64]) -> Result<(), ReportError> {
    if open_times.len() != equity.len() {
        return Err(ReportError::Series {
            detail: format!(
                "{} instants pour {} valeurs d'equity",
                open_times.len(),
                equity.len()
            ),
        });
    }
    Ok(())
}

/// Contenu de `equity.csv` en mémoire. `open_times` et `equity` sont ceux de la fenêtre jouée (une
/// valeur par 1m), `initial_cash` le pic de départ du drawdown.
pub fn equity_csv(
    open_times: &[Ts],
    equity: &[f64],
    initial_cash: f64,
    step: Timeframe,
) -> Result<Vec<u8>, ReportError> {
    check_lengths(open_times, equity)?;
    let csv_error = |e: csv::Error| ReportError::Csv(e.to_string());
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b',')
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    writer.write_record(EQUITY_COLUMNS).map_err(csv_error)?;
    let step_ms = step.ms();
    let mut tracker = DrawdownTracker::new(initial_cash);
    for (j, (&open, &e)) in open_times.iter().zip(equity).enumerate() {
        let at = open + MINUTE_MS;
        let dd = tracker.step(e, at);
        let last_of_bucket = match open_times.get(j + 1) {
            Some(&next) => next.div_euclid(step_ms) != open.div_euclid(step_ms),
            None => true,
        };
        if last_of_bucket {
            // `0.0 - dd` et non `-dd` : un drawdown nul s'écrit `0` et jamais `-0`.
            let row = [at.to_string(), e.to_string(), (0.0 - dd).to_string()];
            writer.write_record(&row).map_err(csv_error)?;
        }
    }
    writer
        .into_inner()
        .map_err(|e| ReportError::Csv(e.to_string()))
}

/// Écrit `equity.csv` dans `dir` de façon atomique. Rend le chemin final.
pub fn write_equity_csv(
    dir: &Path,
    open_times: &[Ts],
    equity: &[f64],
    initial_cash: f64,
    step: Timeframe,
) -> Result<PathBuf, ReportError> {
    let bytes = equity_csv(open_times, equity, initial_cash, step)?;
    write_bytes_atomic(dir, EQUITY_FILE, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: Ts = 3_600_000;
    const M: Ts = 60_000;

    fn minutes(from: Ts, count: i64) -> Vec<Ts> {
        (0..count).map(|k| from + k * M).collect()
    }

    #[test]
    fn echantillonnage_1m_garde_toutes_les_lignes() {
        let t = minutes(0, 5);
        assert_eq!(sample_indices(&t, Timeframe::M1), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn echantillonnage_1h_garde_la_derniere_1m_de_chaque_heure() {
        // 150 minutes contiguës depuis 00:00 : heures 0 (60), 1 (60), 2 (30).
        let t = minutes(0, 150);
        assert_eq!(sample_indices(&t, Timeframe::H1), vec![59, 119, 149]);
    }

    #[test]
    fn un_seau_vide_ne_produit_pas_de_ligne() {
        // Heure 0 : 3 minutes ; heure 1 : aucune ; heure 2 : 2 minutes.
        let mut t = minutes(0, 3);
        t.extend(minutes(2 * H, 2));
        assert_eq!(sample_indices(&t, Timeframe::H1), vec![2, 4]);
    }

    #[test]
    fn echantillonnage_1d_aligne_sur_minuit_utc() {
        let day = 24 * H;
        let t = vec![day - 2 * M, day - M, day, day + M];
        assert_eq!(sample_indices(&t, Timeframe::D1), vec![1, 3]);
    }

    #[test]
    fn epoque_negative_div_euclid() {
        let t = vec![-2 * M, -M, 0];
        assert_eq!(sample_indices(&t, Timeframe::H1), vec![1, 2]);
    }

    #[test]
    fn le_csv_porte_l_instant_de_cloture_et_un_drawdown_nul_sans_signe() {
        let t = minutes(0, 3);
        let e = [100.0, 99.0, 101.0];
        let texte = String::from_utf8(equity_csv(&t, &e, 100.0, Timeframe::M1).unwrap()).unwrap();
        assert_eq!(
            texte,
            "time_ms,equity,drawdown\n60000,100,0\n120000,99,-0.010000000000000009\n180000,101,0\n"
        );
    }

    #[test]
    fn le_drawdown_echantillonne_vient_de_la_serie_1m_complete() {
        // Le creux de 90 tombe à la 2e minute, hors des points échantillonnés à l'heure ; le point
        // échantillonné (dernière minute) voit pourtant le pic de 110.
        let t = minutes(0, 3);
        let e = [110.0, 90.0, 100.0];
        let texte = String::from_utf8(equity_csv(&t, &e, 100.0, Timeframe::H1).unwrap()).unwrap();
        let lignes: Vec<&str> = texte.lines().collect();
        assert_eq!(lignes.len(), 2);
        let dd = 1.0 - 100.0 / 110.0;
        assert_eq!(lignes[1], format!("180000,100,{}", 0.0 - dd));
    }

    #[test]
    fn longueurs_differentes_refusees() {
        assert!(equity_csv(&[0, M], &[1.0], 1.0, Timeframe::M1).is_err());
    }
}
