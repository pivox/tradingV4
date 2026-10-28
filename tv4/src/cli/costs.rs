//! Sous-commande `tv4 costs calibrate-stress` (docs/specs/s10.md §11 et §14.2).
//!
//! Mesure la part des stops synthétiques qui se déclenchent dans une minute en stress (range supérieur à
//! `--range-mult` fois l'ATR14 des 1m précédentes) : la donnée qui règle la probabilité de stress de
//! `config/costs.toml` (04 §3.1 point 3). Le calcul est dans [`crate::research::stress`] ; la CLI lit les
//! datasets figés (1m, mono ou multi-séries), renseigne leurs `dataset_hash`, date le fichier et l'écrit
//! de façon durable et sans écrasement : `<root>/calibration/stress-<AAAAMMJJTHHMMSSZ>.json`, ou
//! `--out FICHIER`.
//!
//! Écriture comme `tv4 instruments fetch` : `<nom>.tmp`, `sync_all`, `rename`, `sync_all` du dossier. Un
//! fichier existant n'est jamais écrasé ([`CliError::OutputExists`]), et l'on échoue avant le calcul si
//! `--out` existe déjà.
//!
//! Codes de sortie : 2 pour une erreur d'usage (dataset absent, paramètre invalide, y compris `--tf 1m`
//! ou un TF répété), 1 pour une erreur applicative (dataset altéré, série vide, symbole fourni deux fois).
//! Le groupe `costs` laisse de la place à `costs show` plus tard.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use super::backtest::resolve_dataset_path;
use super::instruments::{refuse_existing, write_new_durable};
use super::{CliError, now_ms};
use crate::model::Timeframe;
use crate::model::costs::CostsFile;
use crate::report;
use crate::research::stress::{
    DatasetRef, StressCalibration, StressError, StressParams, calibrate,
};
use crate::store::{self, Columns, time::format_rfc3339_utc};

/// Dossier des calibrations sous la racine de travail.
const CALIBRATION_DIR: &str = "calibration";

/// Opérations sur les profils de coûts.
#[derive(Debug, Subcommand)]
pub enum CostsAction {
    /// Calibre la composante de stress des stops sur des datasets 1m figés (écrit un fichier daté).
    CalibrateStress(CalibrateStressArgs),
}

/// Arguments de `tv4 costs calibrate-stress`.
#[derive(Debug, Args)]
pub struct CalibrateStressArgs {
    /// Racine de travail : contient `data/` et `datasets/`, et reçoit `calibration/`.
    #[arg(long, value_name = "DOSSIER", default_value = ".")]
    pub root: PathBuf,
    /// Dataset figé en 1m, mono ou multi-séries : un nom (résolu en `<root>/datasets/<nom>.json`) ou le
    /// chemin d'un manifeste. Répétable ; un symbole ne peut pas figurer dans deux datasets.
    #[arg(long, value_name = "NOM|JSON", required = true)]
    pub dataset: Vec<PathBuf>,
    /// Fichier des profils de coûts : les classes `major` et `alt` en viennent.
    #[arg(long, value_name = "TOML", default_value = "config/costs.toml")]
    pub costs: PathBuf,
    /// TF de décision étudié, répétable (défaut : 1d et 4h). Le 1m n'en est pas un.
    #[arg(long, value_name = "TF")]
    pub tf: Vec<Timeframe>,
    /// Multiple d'ATR du stop synthétique, répétable (défaut : 1,5 ; 2 ; 3, plage de 04 §3.3).
    #[arg(long, value_name = "K", value_parser = positive_number)]
    pub k: Vec<f64>,
    /// Durée de vie d'une position, en bougies du TF (défaut 30).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub horizon_bars: Option<u32>,
    /// Multiple d'ATR14 des 1m précédentes au-delà duquel une minute est en stress (défaut 5).
    #[arg(long, value_name = "M", value_parser = positive_number)]
    pub range_mult: Option<f64>,
    /// Fichier de sortie. Défaut : `<root>/calibration/stress-<horodatage UTC>.json`. Un fichier existant
    /// n'est jamais écrasé.
    #[arg(long, value_name = "JSON")]
    pub out: Option<PathBuf>,
}

/// Nombre fini et strictement positif (`--k`, `--range-mult`).
fn positive_number(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .parse()
        .map_err(|_| format!("`{text}` n'est pas un nombre"))?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(format!(
            "`{text}` : un nombre strictement positif est attendu"
        ))
    }
}

impl CalibrateStressArgs {
    /// Paramètres de la calibration : les défauts du module, remplacés par les options données.
    pub(super) fn params(&self) -> StressParams {
        let mut params = StressParams::default();
        if !self.tf.is_empty() {
            params.tfs.clone_from(&self.tf);
        }
        if !self.k.is_empty() {
            params.k.clone_from(&self.k);
        }
        if let Some(horizon) = self.horizon_bars {
            params.horizon_bars = usize::try_from(horizon).unwrap_or(usize::MAX);
        }
        if let Some(range_mult) = self.range_mult {
            params.range_mult = range_mult;
        }
        params
    }
}

/// Compact `AAAAMMJJTHHMMSSZ` d'un instant, pour le nom du fichier.
fn file_stamp(created_at: &str) -> String {
    created_at
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect()
}

/// Séries 1m et références des datasets donnés.
struct LoadedDatasets {
    series: Vec<(String, Columns)>,
    datasets: Vec<DatasetRef>,
}

/// Lit chaque dataset donné : ses séries 1m et sa référence (nom et `dataset_hash`).
fn load_series(args: &CalibrateStressArgs) -> Result<LoadedDatasets, CliError> {
    let mut series: Vec<(String, Columns)> = Vec::new();
    let mut refs: Vec<DatasetRef> = Vec::with_capacity(args.dataset.len());
    for dataset in &args.dataset {
        let shown = dataset.display().to_string();
        let path = resolve_dataset_path(&args.root, dataset)?;
        if !path.is_file() {
            return Err(CliError::DatasetNotFound {
                dataset: shown,
                path: path.display().to_string(),
            });
        }
        let loaded = store::load_dataset_multi(&args.root, &path)?;
        if loaded.manifest.tfs.as_slice() != [Timeframe::M1] {
            return Err(CliError::DatasetTimeframe {
                path: shown,
                tf: loaded
                    .manifest
                    .tfs
                    .first()
                    .copied()
                    .unwrap_or(Timeframe::M1),
            });
        }
        refs.push(DatasetRef {
            name: loaded.manifest.name.clone(),
            dataset_hash: loaded.manifest.dataset_hash.clone(),
        });
        series.extend(loaded.series);
    }
    Ok(LoadedDatasets {
        series,
        datasets: refs,
    })
}

/// `tv4 costs calibrate-stress`.
pub(super) fn calibrate_stress(args: &CalibrateStressArgs) -> Result<(), CliError> {
    // Échec rapide : un fichier de sortie donné et déjà présent, avant tout calcul.
    if let Some(out) = &args.out {
        refuse_existing(out)?;
    }
    let params = args.params();
    let costs = CostsFile::load(&args.costs)?;
    // Les paramètres sont validés par `calibrate` avant toute série : un appel à vide les contrôle sans
    // lire un seul dataset, et seule l'absence de série est alors attendue.
    match calibrate(&[], &costs, &params) {
        Err(StressError::NoSeries) => {}
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }

    let loaded = load_series(args)?;
    let calibration = calibrate(&loaded.series, &costs, &params)?.with_datasets(loaded.datasets)?;

    let created_at = format_rfc3339_utc(now_ms()?)?;
    let path = match &args.out {
        Some(out) => out.clone(),
        None => args
            .root
            .join(CALIBRATION_DIR)
            .join(format!("stress-{}.json", file_stamp(&created_at))),
    };
    refuse_existing(&path)?;
    let bytes = report::json_bytes(&calibration.file(&created_at, crate::GIT_SHA))?;
    write_new_durable(&path, &bytes)?;
    print_summary(&path, &calibration);
    Ok(())
}

/// Sortie standard : chemin, `measurement_hash` et tableau `pooled`.
fn print_summary(path: &Path, calibration: &StressCalibration) {
    println!("fichier : {}", path.display());
    println!("measurement_hash : {}", calibration.measurement_hash);
    for dataset in &calibration.datasets {
        println!("dataset {} : {}", dataset.name, dataset.dataset_hash);
    }
    println!("classe tf déclenchés stress_m1 part_m1 stress_tf part_tf");
    for pooled in &calibration.pooled {
        println!(
            "{} {} {} {} {} {} {}",
            format!("{:?}", pooled.class).to_ascii_lowercase(),
            pooled.tf,
            pooled.triggered,
            pooled.stress_m1,
            share(pooled.share_m1),
            pooled.stress_tf,
            share(pooled.share_tf),
        );
    }
}

/// Part pour la sortie standard : `n/a` quand aucun déclenchement.
fn share(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.4}"),
        None => "n/a".to_string(),
    }
}
