//! Ligne de commande `tv4` (clap, dérivation). Aucune logique métier ici : chaque sous-commande est
//! une fonction qui appelle les autres modules (docs/03-architecture.md §2).
//!
//! `backtest` existe depuis S3 (fixture CSV 1m), `ingest csv` et `dataset build` depuis S4, `ingest
//! kraken` depuis S5, `ingest okx` et `compare-sources` depuis S7, `verify` et `bench` depuis S8. `grid`
//! arrive au jalon 1.5.
//!
//! Codes de sortie ([`CliError::exit_code`]) : 0 succès, 1 erreur applicative, 2 erreur d'usage (clap),
//! 3 `tv4 verify` a trouvé deux runs différents.
//!
//! La CLI est le seul module qui lit l'horloge système ([`now_ms`]) : le store reçoit l'instant en
//! paramètre (docs/03 §5.3, écart 5 de S4).

use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};

use crate::model::Ts;

pub use backtest::{BacktestArgs, RunInputArgs};
pub use bench::BenchArgs;
pub use compare_sources::{
    BarReport, COMPARE_LOCK_TIMEOUT, CompareReport, CompareSourcesArgs, CompareSourcesOptions,
    DAY_DELTA_THRESHOLD, DAYS_ABOVE_MAX_PERCENT, DateRange, DeltaStats, FileReport,
    MEDIAN_ABS_DELTA_4H_MAX, MinuteCounts, RangeReport, ShareReport, SourceReport,
    ThresholdsReport, Verdict, WICK_THRESHOLDS, WickReport, WickSide, compare_sources,
};
pub use dataset::DatasetBuildArgs;
pub use ingest::{IngestCsvArgs, IngestKrakenArgs, IngestSummary, ingest_kraken};
pub use ingest_okx::{
    DEFAULT_BATCH_PAGES, DEFAULT_LOCK_TIMEOUT, DEFAULT_WORKERS, IngestOkxArgs, MonthReport,
    MonthStatus, OkxIngestOptions, OkxIngestReport, UtcDate, ingest_okx,
};
pub use verify::VerifyArgs;

mod backtest;
mod bench;
mod compare_sources;
mod dataset;
mod ingest;
mod ingest_okx;
mod verify;

/// Erreurs de la couche CLI.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Un fichier d'entrée n'a pas pu être lu.
    #[error("lecture de `{path}` impossible : {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Le fichier de paramètres n'est pas du TOML valide.
    #[error("paramètres `{path}` : TOML invalide : {source}")]
    ParamsToml {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    #[error(transparent)]
    Model(#[from] crate::model::ModelError),

    #[error(transparent)]
    Kraken(#[from] crate::marketdata::kraken::KrakenError),

    #[error(transparent)]
    Strategy(#[from] crate::strategy::StrategyError),

    #[error(transparent)]
    Engine(#[from] crate::engine::EngineError),

    #[error(transparent)]
    Report(#[from] crate::report::ReportError),

    #[error(transparent)]
    Store(#[from] crate::store::StoreError),

    #[error(transparent)]
    KrakenZip(#[from] crate::marketdata::kraken_zip::KrakenZipError),

    #[error(transparent)]
    Okx(#[from] crate::marketdata::okx::OkxError),

    /// Une paire est demandée deux fois sur la ligne de commande de `ingest kraken`.
    #[error("paire « {pair} » demandée deux fois")]
    DuplicatePair { pair: String },

    /// Au moins une paire de `ingest kraken` n'a pas été écrite (bloquée, entrée absente, écriture en
    /// échec) ; les autres l'ont été, et les rapports disent pourquoi.
    #[error("ingestion incomplète : {failed} ; voir {summary}")]
    IngestIncomplete { failed: String, summary: String },

    /// Au moins une période de `ingest okx` n'a pas été écrite (échec après épuisement des essais, ou
    /// erreur d'écriture) ; les périodes déjà écrites restent valides et `--resume` reprend le reste.
    #[error(
        "ingestion OKX incomplète : {failed} ; les fichiers déjà écrits restent valides, relancer avec --resume"
    )]
    OkxIncomplete { failed: String },

    /// Un thread de téléchargement a paniqué : le compte rendu de l'ingestion n'est pas fiable.
    #[error(
        "un thread de `ingest okx` a paniqué : relancer avec --resume après avoir signalé l'incident"
    )]
    OkxWorkerPanicked,

    /// `compare-sources` : une partition 1m est absente ou vide.
    #[error(
        "compare-sources : aucune bougie 1m pour {partition} sous `{dir}` (ingérer d'abord la source)"
    )]
    CompareMissingSource { partition: String, dir: String },

    /// `compare-sources` : les plages des deux sources, bornées par --from et --to, ne se recoupent pas.
    #[error("compare-sources : recouvrement vide ({detail})")]
    CompareNoOverlap { detail: String },

    /// `compare-sources` : les plages se recoupent mais aucune minute n'est présente des deux côtés
    /// hors des plages exclues.
    #[error("compare-sources : aucune minute commune aux deux sources sur {detail}")]
    CompareNoCommonMinute { detail: String },

    /// L'horloge système ne donne pas un instant exploitable.
    #[error("horloge système antérieure à 1970 ou hors plage : impossible de dater l'écriture")]
    Clock,

    /// L'agrégation part du 1m : un dataset dans un autre timeframe n'est pas rejouable.
    #[error(
        "dataset `{path}` en {tf} : le moteur rejoue des bougies 1m et les agrège vers les timeframes de la stratégie ; construire le dataset en 1m"
    )]
    DatasetTimeframe {
        path: String,
        tf: crate::model::Timeframe,
    },

    /// Le binaire en cours d'exécution n'a pas pu être lu pour calculer son hash.
    #[error("hash du binaire `{path}` impossible : {source}")]
    BinaryHash {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// `tv4 verify` : les deux runs sont lisibles et cohérents mais diffèrent.
    #[error("les runs diffèrent ({files} fichier(s))")]
    VerifyMismatch { files: usize },

    #[error(transparent)]
    Verify(#[from] crate::report::verify::VerifyError),

    /// `tv4 bench` : une mesure est incohérente avec le run (bug du moteur ou de l'enveloppe).
    #[error("bench incohérent : {detail}")]
    BenchInconsistent { detail: String },

    /// Ni `--fixture` ni `--dataset` (ou les deux) : clap l'interdit, cette erreur évite une panique.
    #[error("indiquer --fixture ou --dataset (un seul des deux)")]
    DataSource,
}

impl CliError {
    /// Code de sortie du processus : 3 pour deux runs différents (`tv4 verify`), 1 pour toute autre
    /// erreur applicative ; clap garde 2 pour une erreur d'usage.
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::VerifyMismatch { .. } => 3,
            _ => 1,
        }
    }
}

/// Instant présent en millisecondes UTC : seul lecteur d'horloge du dépôt, injecté dans le store.
pub(crate) fn now_ms() -> Result<Ts, CliError> {
    let depuis_1970 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::Clock)?;
    Ts::try_from(depuis_1970.as_millis()).map_err(|_| CliError::Clock)
}

/// Arguments de la ligne de commande `tv4`.
#[derive(Debug, Parser)]
#[command(
    name = "tv4",
    version = crate::LONG_VERSION,
    about = "tradingV4 : moteur de backtest crypto spot",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Sous-commande à exécuter.
    #[command(subcommand)]
    pub command: Command,
}

/// Sous-commandes de `tv4` (docs/03 §2, module `cli`).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Ingère des bougies depuis une source de marché.
    Ingest {
        /// Source des bougies.
        #[command(subcommand)]
        source: IngestSource,
    },
    /// Opérations sur les datasets figés.
    Dataset {
        /// Opération demandée.
        #[command(subcommand)]
        action: DatasetAction,
    },
    /// Lance un backtest sur une fixture CSV Kraken 1m (S3) ou un dataset figé (S4).
    Backtest(BacktestArgs),
    /// Mesure le biais de lieu Kraken / OKX sur leur recouvrement (clôtures 4h et 1d, mèches 1m).
    CompareSources(CompareSourcesArgs),
    /// Compare deux runs à l'octet (S8) : code 0 identiques, 3 différents, 1 erreur ou run incohérent.
    Verify(VerifyArgs),
    /// Mesure le coût de la boucle par 1m et le coût moyen par appel de `on_bar` (S8).
    Bench(BenchArgs),
}

/// Sources d'ingestion.
#[derive(Debug, Subcommand)]
pub enum IngestSource {
    /// Dump OHLCVT complet de Kraken (zip, SHA-256 vérifié, rapport de continuité bloquant).
    Kraken(IngestKrakenArgs),
    /// API REST OKX `history-candles` : 3 threads, écriture par lots, reprise.
    Okx(IngestOkxArgs),
    /// CSV OHLCVT au format Kraken : ingestion minimale de S4 (fixture), sans contrôle de continuité.
    Csv(IngestCsvArgs),
}

/// Opérations sur les datasets.
#[derive(Debug, Subcommand)]
pub enum DatasetAction {
    /// Construit un dataset figé (manifeste et `dataset_hash`).
    Build(DatasetBuildArgs),
}

/// Exécute la sous-commande demandée.
pub fn run(command: &Command) -> Result<(), CliError> {
    match command {
        Command::Ingest { source } => match source {
            IngestSource::Kraken(args) => ingest::kraken(args),
            IngestSource::Okx(args) => ingest_okx::okx(args),
            IngestSource::Csv(args) => ingest::csv(args),
        },
        Command::Dataset { action } => match action {
            DatasetAction::Build(args) => dataset::build(args),
        },
        Command::Backtest(args) => backtest::run(args),
        Command::CompareSources(args) => compare_sources::run(args),
        Command::Verify(args) => verify::run(args),
        Command::Bench(args) => bench::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn la_definition_clap_est_coherente() {
        Cli::command().debug_assert();
    }

    #[test]
    fn verify_et_bench_se_parsent() {
        assert!(Cli::try_parse_from(["tv4", "verify"]).is_err());
        assert!(Cli::try_parse_from(["tv4", "verify", "a"]).is_err());
        let Command::Verify(args) = Cli::try_parse_from(["tv4", "verify", "a", "b"])
            .expect("verify valide")
            .command
        else {
            panic!("sous-commande attendue : verify");
        };
        assert_eq!(args.run_a, std::path::PathBuf::from("a"));
        assert_eq!(args.run_b, std::path::PathBuf::from("b"));

        let base = [
            "tv4",
            "bench",
            "--fixture",
            "f",
            "--strategy",
            "s",
            "--params",
            "p",
        ];
        let Command::Bench(args) = Cli::try_parse_from(base).expect("bench valide").command else {
            panic!("sous-commande attendue : bench");
        };
        assert_eq!(args.repeat, 5);
        assert_eq!(args.inputs.min_trades, 30);
        let mut avec_repeat = base.to_vec();
        avec_repeat.extend(["--repeat", "3"]);
        let Command::Bench(args) = Cli::try_parse_from(&avec_repeat)
            .expect("--repeat 3 valide")
            .command
        else {
            panic!("sous-commande attendue : bench");
        };
        assert_eq!(args.repeat, 3);
        avec_repeat.extend(["--repeat", "0"]);
        assert!(Cli::try_parse_from(avec_repeat).is_err());
        // Pas d'--out ni d'--equity-step : bench n'écrit rien.
        let mut avec_out = base.to_vec();
        avec_out.extend(["--out", "o"]);
        assert!(Cli::try_parse_from(avec_out).is_err());
    }

    #[test]
    fn le_code_de_sortie_distingue_runs_differents_et_erreur() {
        assert_eq!(CliError::VerifyMismatch { files: 1 }.exit_code(), 3);
        assert_eq!(CliError::Clock.exit_code(), 1);
        assert_eq!(
            CliError::BenchInconsistent {
                detail: String::new()
            }
            .exit_code(),
            1
        );
    }

    #[test]
    fn backtest_exige_ses_arguments_obligatoires() {
        assert!(Cli::try_parse_from(["tv4", "backtest"]).is_err());
        let cli = Cli::try_parse_from([
            "tv4",
            "backtest",
            "--fixture",
            "f.csv",
            "--strategy",
            "sma_cross",
            "--params",
            "p.toml",
            "--out",
            "sortie",
        ])
        .expect("les quatre arguments obligatoires suffisent");
        let Command::Backtest(args) = cli.command else {
            panic!("sous-commande attendue : backtest");
        };
        assert_eq!(args.inputs.fixture, Some(std::path::PathBuf::from("f.csv")));
        assert!(args.inputs.dataset.is_none());
        assert_eq!(args.inputs.root, std::path::PathBuf::from("."));
        assert_eq!(args.inputs.fees, crate::model::ExchangeId::Okx);
        assert_eq!(args.inputs.slippage_bps.bps(), 5.0);
        assert_eq!(args.inputs.capital, 300.0);
        assert_eq!(
            args.inputs.fees_file,
            std::path::PathBuf::from("config/fees.toml")
        );
        assert_eq!(args.inputs.min_trades, 30);
        assert_eq!(args.equity_step, crate::model::Timeframe::H1);
        assert_eq!(args.out, Some(std::path::PathBuf::from("sortie")));
    }

    #[test]
    fn backtest_out_est_facultatif_et_min_trades_zero_est_refuse() {
        let base = [
            "tv4",
            "backtest",
            "--fixture",
            "f",
            "--strategy",
            "s",
            "--params",
            "p",
        ];
        let avec = |extra: &[&str]| {
            let mut args = base.to_vec();
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        let Command::Backtest(args) = avec(&[]).expect("--out est facultatif").command else {
            panic!("sous-commande attendue : backtest");
        };
        assert!(args.out.is_none());
        assert!(avec(&["--min-trades", "0"]).is_err());
        let Command::Backtest(args) = avec(&["--min-trades", "5", "--equity-step", "1m"])
            .expect("options valides")
            .command
        else {
            panic!("sous-commande attendue : backtest");
        };
        assert_eq!(args.inputs.min_trades, 5);
        assert_eq!(args.equity_step, crate::model::Timeframe::M1);
        assert!(avec(&["--equity-step", "2m"]).is_err());
    }

    #[test]
    fn backtest_exige_fixture_ou_dataset_mais_pas_les_deux() {
        let base = [
            "tv4",
            "backtest",
            "--strategy",
            "s",
            "--params",
            "p",
            "--out",
            "o",
        ];
        let avec = |extra: &[&str]| {
            let mut args = base.to_vec();
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        assert!(avec(&[]).is_err());
        assert!(avec(&["--fixture", "f.csv", "--dataset", "d.json"]).is_err());
        let cli =
            avec(&["--dataset", "datasets/d.json", "--root", "R"]).expect("--dataset seul suffit");
        let Command::Backtest(args) = cli.command else {
            panic!("sous-commande attendue : backtest");
        };
        assert!(args.inputs.fixture.is_none());
        assert_eq!(
            args.inputs.dataset,
            Some(std::path::PathBuf::from("datasets/d.json"))
        );
        assert_eq!(args.inputs.root, std::path::PathBuf::from("R"));
    }

    #[test]
    fn ingest_csv_et_dataset_build_se_parsent() {
        let cli = Cli::try_parse_from([
            "tv4",
            "ingest",
            "csv",
            "--csv",
            "f.csv",
            "--exchange",
            "kraken",
            "--symbol",
            "BTC-USD",
            "--tf",
            "1m",
        ])
        .expect("ingest csv valide");
        let Command::Ingest {
            source: IngestSource::Csv(args),
        } = cli.command
        else {
            panic!("sous-commande attendue : ingest csv");
        };
        assert_eq!(args.symbol.as_str(), "BTC-USD");
        assert_eq!(args.tf, crate::model::Timeframe::M1);
        assert_eq!(args.root, std::path::PathBuf::from("."));

        let cli = Cli::try_parse_from([
            "tv4",
            "dataset",
            "build",
            "--name",
            "d",
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
        ])
        .expect("dataset build valide");
        let Command::Dataset {
            action: DatasetAction::Build(args),
        } = cli.command
        else {
            panic!("sous-commande attendue : dataset build");
        };
        assert_eq!(args.from.0, 1_577_836_800_000);
        assert_eq!(args.to.0, 1_578_528_000_000);
        assert_eq!(args.gaps_policy, crate::store::GapsPolicy::Skip);
        // Date sans heure ou politique inconnue : erreur d'usage.
        assert!(
            Cli::try_parse_from([
                "tv4",
                "dataset",
                "build",
                "--name",
                "d",
                "--exchange",
                "kraken",
                "--symbol",
                "BTC-USD",
                "--tf",
                "1m",
                "--from",
                "2020-01-01",
                "--to",
                "2020-01-09T00:00:00Z",
            ])
            .is_err()
        );
    }

    #[test]
    fn ingest_kraken_se_parse() {
        let cli = Cli::try_parse_from([
            "tv4",
            "ingest",
            "kraken",
            "--zip",
            "Kraken_OHLCVT_Full.zip",
            "--sha256",
            &"BC27".repeat(16),
            "--pair",
            "XBTUSD",
            "--pair",
            "ETHUSD",
            "--root",
            "R",
        ])
        .expect("ingest kraken valide");
        let Command::Ingest {
            source: IngestSource::Kraken(args),
        } = cli.command
        else {
            panic!("sous-commande attendue : ingest kraken");
        };
        assert_eq!(args.zip, std::path::PathBuf::from("Kraken_OHLCVT_Full.zip"));
        // Le SHA-256 en majuscules est accepté et normalisé en minuscules.
        assert_eq!(args.sha256.as_str(), "bc27".repeat(16));
        let paires: Vec<(&str, &str)> = args
            .pairs
            .iter()
            .map(|p| (p.as_str(), p.symbol()))
            .collect();
        assert_eq!(paires, [("XBTUSD", "BTC-USD"), ("ETHUSD", "ETH-USD")]);
        assert_eq!(args.root, std::path::PathBuf::from("R"));
    }

    #[test]
    fn ingest_okx_se_parse_avec_ses_defauts() {
        let cli = Cli::try_parse_from([
            "tv4",
            "ingest",
            "okx",
            "--inst",
            "BTC-USDC",
            "--inst",
            "BTC-USDT",
            "--from",
            "2025-08-21",
        ])
        .expect("ingest okx valide");
        let Command::Ingest {
            source: IngestSource::Okx(args),
        } = cli.command
        else {
            panic!("sous-commande attendue : ingest okx");
        };
        let insts: Vec<&str> = args.insts.iter().map(|s| s.as_str()).collect();
        assert_eq!(insts, ["BTC-USDC", "BTC-USDT"]);
        assert_eq!(args.tf, crate::model::Timeframe::M1);
        assert_eq!(args.from, UtcDate(1_755_734_400_000));
        assert!(args.to.is_none());
        assert!(!args.resume);
        assert_eq!(args.root, std::path::PathBuf::from("."));
        assert!(args.base_url.is_none());

        let cli = Cli::try_parse_from([
            "tv4",
            "ingest",
            "okx",
            "--inst",
            "BTC-USDC",
            "--tf",
            "4h",
            "--from",
            "2025-08-21",
            "--to",
            "2025-09-01",
            "--resume",
            "--root",
            "R",
        ])
        .expect("ingest okx valide");
        let Command::Ingest {
            source: IngestSource::Okx(args),
        } = cli.command
        else {
            panic!("sous-commande attendue : ingest okx");
        };
        assert_eq!(args.tf, crate::model::Timeframe::H4);
        assert_eq!(args.to, Some(UtcDate(1_756_684_800_000)));
        assert!(args.resume);
        assert_eq!(args.root, std::path::PathBuf::from("R"));
    }

    #[test]
    fn ingest_okx_exige_inst_et_from_et_refuse_les_arguments_mal_formes() {
        for incomplet in [
            vec!["tv4", "ingest", "okx", "--from", "2025-08-21"],
            vec!["tv4", "ingest", "okx", "--inst", "BTC-USDC"],
            vec![
                "tv4",
                "ingest",
                "okx",
                "--inst",
                "btc-usdc",
                "--from",
                "2025-08-21",
            ],
            vec![
                "tv4",
                "ingest",
                "okx",
                "--inst",
                "BTC-USDC",
                "--from",
                "21/08/2025",
            ],
            vec![
                "tv4",
                "ingest",
                "okx",
                "--inst",
                "BTC-USDC",
                "--from",
                "2025-08-21",
                "--tf",
                "2h",
            ],
        ] {
            assert!(Cli::try_parse_from(&incomplet).is_err(), "{incomplet:?}");
        }
    }

    #[test]
    fn ingest_kraken_exige_zip_sha_et_une_paire() {
        let sha = "ab".repeat(32);
        let complet = [
            "tv4", "ingest", "kraken", "--zip", "z", "--sha256", &sha, "--pair", "XBTUSD",
        ];
        assert!(Cli::try_parse_from(complet).is_ok());
        // Chacun des trois arguments obligatoires manque tour à tour.
        let sans_zip = [
            "tv4", "ingest", "kraken", "--sha256", &sha, "--pair", "XBTUSD",
        ];
        let sans_sha = ["tv4", "ingest", "kraken", "--zip", "z", "--pair", "XBTUSD"];
        let sans_paire = ["tv4", "ingest", "kraken", "--zip", "z", "--sha256", &sha];
        for incomplet in [&sans_zip[..], &sans_sha[..], &sans_paire[..]] {
            assert!(Cli::try_parse_from(incomplet).is_err(), "{incomplet:?}");
        }
        // SHA-256 mal formé, paire inconnue : erreurs d'usage.
        for (extra_sha, paire) in [
            ("abc", "XBTUSD"),
            (sha.as_str(), "xbtusd"),
            (sha.as_str(), "XBTFOO"),
        ] {
            let args = [
                "tv4", "ingest", "kraken", "--zip", "z", "--sha256", extra_sha, "--pair", paire,
            ];
            assert!(Cli::try_parse_from(args).is_err(), "{extra_sha} {paire}");
        }
    }

    #[test]
    fn compare_sources_se_parse_avec_ses_defauts_et_ses_exclusions() {
        let cli = Cli::try_parse_from([
            "tv4",
            "compare-sources",
            "--kraken",
            "BTC-USD",
            "--okx",
            "BTC-USDT",
        ])
        .expect("compare-sources valide");
        let Command::CompareSources(args) = cli.command else {
            panic!("sous-commande attendue : compare-sources");
        };
        assert_eq!(args.kraken.as_str(), "BTC-USD");
        assert_eq!(args.okx.as_str(), "BTC-USDT");
        assert!(args.from.is_none() && args.to.is_none() && args.exclude.is_empty());
        assert_eq!(args.root, std::path::PathBuf::from("."));

        let cli = Cli::try_parse_from([
            "tv4",
            "compare-sources",
            "--kraken",
            "BTC-USD",
            "--okx",
            "BTC-USDC",
            "--from",
            "2025-08-21",
            "--to",
            "2026-01-01",
            "--exclude",
            "2022-05-09..2022-05-16",
            "--exclude",
            "2025-10-10..2025-10-11",
            "--root",
            "R",
        ])
        .expect("compare-sources valide");
        let Command::CompareSources(args) = cli.command else {
            panic!("sous-commande attendue : compare-sources");
        };
        assert_eq!(args.from, Some(UtcDate(1_755_734_400_000)));
        assert_eq!(args.to, Some(UtcDate(1_767_225_600_000)));
        assert_eq!(args.exclude.len(), 2);
        assert_eq!(args.exclude[0].0, 1_652_054_400_000..1_652_659_200_000);
        assert_eq!(args.root, std::path::PathBuf::from("R"));
    }

    #[test]
    fn compare_sources_exige_les_deux_symboles_et_refuse_les_plages_mal_formees() {
        for incomplet in [
            vec!["tv4", "compare-sources", "--kraken", "BTC-USD"],
            vec!["tv4", "compare-sources", "--okx", "BTC-USDC"],
            vec![
                "tv4",
                "compare-sources",
                "--kraken",
                "btc-usd",
                "--okx",
                "BTC-USDC",
            ],
            vec![
                "tv4",
                "compare-sources",
                "--kraken",
                "BTC-USD",
                "--okx",
                "BTC-USDC",
                "--exclude",
                "2022-05-16..2022-05-09",
            ],
        ] {
            assert!(Cli::try_parse_from(&incomplet).is_err(), "{incomplet:?}");
        }
    }

    #[test]
    fn grid_n_existe_pas_encore() {
        assert!(Cli::try_parse_from(["tv4", "grid"]).is_err());
    }

    #[test]
    fn sans_argument_clap_affiche_l_aide() {
        assert!(Cli::try_parse_from(["tv4"]).is_err());
    }
}
