//! Ligne de commande `tv4` (clap, dérivation). Aucune logique métier ici : chaque sous-commande est
//! une fonction qui appelle les autres modules (docs/03-architecture.md §2).
//!
//! Les sous-commandes pas encore codées sortent en erreur en nommant l'étape du jalon 1 qui les
//! implémentera (docs/03 §11) ; `backtest` existe depuis S3 (fixture CSV 1m), `ingest csv` et
//! `dataset build` depuis S4, `ingest kraken` depuis S5. `grid` arrive au jalon 1.5.
//!
//! La CLI est le seul module qui lit l'horloge système ([`now_ms`]) : le store reçoit l'instant en
//! paramètre (docs/03 §5.3, écart 5 de S4).

use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};

use crate::model::Ts;

pub use backtest::BacktestArgs;
pub use dataset::DatasetBuildArgs;
pub use ingest::{IngestCsvArgs, IngestKrakenArgs, IngestSummary, ingest_kraken};

mod backtest;
mod bench;
mod dataset;
mod ingest;
mod verify;

/// Erreurs de la couche CLI.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// La sous-commande existe dans l'interface mais n'est pas encore codée.
    #[error(
        "la sous-commande « {command} » n'est pas encore implémentée : elle sera écrite à l'étape {step} du jalon 1 (docs/03-architecture.md §11)"
    )]
    NotImplemented {
        /// Nom complet tel que tapé par l'utilisateur, par exemple `ingest okx`.
        command: &'static str,
        /// Étape du jalon 1 qui l'implémentera, de `S2` à `S8`.
        step: &'static str,
    },

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

    /// Une paire est demandée deux fois sur la ligne de commande de `ingest kraken`.
    #[error("paire « {pair} » demandée deux fois")]
    DuplicatePair { pair: String },

    /// Au moins une paire de `ingest kraken` n'a pas été écrite (bloquée, entrée absente, écriture en
    /// échec) ; les autres l'ont été, et les rapports disent pourquoi.
    #[error("ingestion incomplète : {failed} ; voir {summary}")]
    IngestIncomplete { failed: String, summary: String },

    /// L'horloge système ne donne pas un instant exploitable.
    #[error("horloge système antérieure à 1970 ou hors plage : impossible de dater l'écriture")]
    Clock,

    /// `zero_volume_marked` n'est pas encore rejouable par le simulateur.
    #[error(
        "dataset `{path}` : gaps_policy = zero_volume_marked n'est pas encore jouable : le simulateur ne sait pas interdire un fill dans une bougie synthétique (S6, écart no_volume_rule de S3) ; construire le dataset en skip ou ffill"
    )]
    GapsPolicyUnsupported { path: String },

    /// L'agrégation part du 1m : un dataset dans un autre timeframe n'est pas rejouable.
    #[error(
        "dataset `{path}` en {tf} : le moteur rejoue des bougies 1m et les agrège vers les timeframes de la stratégie ; construire le dataset en 1m"
    )]
    DatasetTimeframe {
        path: String,
        tf: crate::model::Timeframe,
    },

    /// Ni `--fixture` ni `--dataset` (ou les deux) : clap l'interdit, cette erreur évite une panique.
    #[error("indiquer --fixture ou --dataset (un seul des deux)")]
    DataSource,
}

impl CliError {
    /// Raccourci pour les sous-commandes vides.
    pub(crate) fn not_implemented(command: &'static str, step: &'static str) -> Self {
        CliError::NotImplemented { command, step }
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
    /// Compare deux runs à l'octet.
    Verify,
    /// Mesure le coût moyen par bougie.
    Bench,
}

/// Sources d'ingestion.
#[derive(Debug, Subcommand)]
pub enum IngestSource {
    /// Dump OHLCVT complet de Kraken (zip, SHA-256 vérifié, rapport de continuité bloquant).
    Kraken(IngestKrakenArgs),
    /// API REST OKX `history-candles`.
    Okx,
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
            IngestSource::Okx => ingest::okx(),
            IngestSource::Csv(args) => ingest::csv(args),
        },
        Command::Dataset { action } => match action {
            DatasetAction::Build(args) => dataset::build(args),
        },
        Command::Backtest(args) => backtest::run(args),
        Command::Verify => verify::run(),
        Command::Bench => bench::run(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Chaque sous-commande vide, la commande tapée et l'étape annoncée.
    const EMPTY_COMMANDS: [(&[&str], &str, &str); 3] = [
        (&["tv4", "ingest", "okx"], "ingest okx", "S7"),
        (&["tv4", "verify"], "verify", "S8"),
        (&["tv4", "bench"], "bench", "S8"),
    ];

    #[test]
    fn la_definition_clap_est_coherente() {
        Cli::command().debug_assert();
    }

    #[test]
    fn chaque_sous_commande_vide_nomme_son_etape() {
        for (args, command, step) in EMPTY_COMMANDS {
            let cli = Cli::try_parse_from(args).expect("la ligne de commande doit être valide");
            let message = match run(&cli.command) {
                Ok(()) => panic!("« {command} » devrait échouer tant qu'elle est vide"),
                Err(error) => error.to_string(),
            };
            assert!(message.contains(command), "{message}");
            assert!(message.contains(step), "{message}");
            assert!(
                message.contains("n'est pas encore implémentée"),
                "{message}"
            );
        }
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
        assert_eq!(args.fixture, Some(std::path::PathBuf::from("f.csv")));
        assert!(args.dataset.is_none());
        assert_eq!(args.root, std::path::PathBuf::from("."));
        assert_eq!(args.fees, crate::model::ExchangeId::Okx);
        assert_eq!(args.slippage_bps.bps(), 5.0);
        assert_eq!(args.capital, 300.0);
        assert_eq!(args.fees_file, std::path::PathBuf::from("config/fees.toml"));
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
        assert!(args.fixture.is_none());
        assert_eq!(
            args.dataset,
            Some(std::path::PathBuf::from("datasets/d.json"))
        );
        assert_eq!(args.root, std::path::PathBuf::from("R"));
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
    fn grid_n_existe_pas_encore() {
        assert!(Cli::try_parse_from(["tv4", "grid"]).is_err());
    }

    #[test]
    fn sans_argument_clap_affiche_l_aide() {
        assert!(Cli::try_parse_from(["tv4"]).is_err());
    }
}
