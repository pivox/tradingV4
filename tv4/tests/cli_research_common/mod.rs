//! Aides communes aux tests de CLI de recherche S11 (docs/specs/s11.md §11) : un zip Kraken
//! **synthétique**, la chaîne `ingest kraken` puis `universe build` puis `dataset build --block`, et le
//! lancement du binaire `tv4`. Tout est inventé : aucune valeur de marché réelle, aucun secret.
//!
//! Le marché : quatre paires aux noms fictifs-réalistes (`XBTUSD`, `ETHUSD`, `SOLUSD`, `ADAUSD`), des
//! bougies 1m espacées de 4 heures du 2025-06-01 au 2026-10-01. `ADAUSD` n'existe que depuis le
//! 2026-05-01 : moins de 365 jours d'historique à `D = 2026-07-01`, donc jamais éligible. Les trois
//! autres sont membres du trimestre 2026-Q3 de FORWARD, avec des volumes qui fixent leur rang
//! (`XBTUSD` > `ETHUSD` > `SOLUSD`). Le bloc FORWARD n'est pas protégé : aucune garde ne s'y applique.
//!
//! Inclus par `mod cli_research_common;` à côté de `mod common;` ; chaque binaire n'en utilise qu'une
//! partie.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common::{SplitMix64, ecrire_zip, manifeste_synthetique, sha256_fichier};
use tv4::model::universe::{PairSubset, SubsetRule, SubsetSource};
use tv4::store::time::{MS_PER_DAY, days_from_civil};
use tv4::store::universe::write_pair_subset;

/// Pas des bougies synthétiques : 4 heures.
const STEP_S: i64 = 4 * 3600;
/// Fin de couverture exclusive du zip : 2026-10-01.
pub const COVERAGE_END_TEXT: &str = "2026-10-01T00:00:00Z";
/// Fin de fenêtre du dataset FORWARD des tests (`--to`).
pub const FORWARD_TO: &str = "2026-09-01T00:00:00Z";

/// Instant UTC en secondes.
fn secs(year: i64, month: u32, day: u32) -> i64 {
    days_from_civil(year, month, day) * MS_PER_DAY / 1000
}

/// CSV Kraken (sans en-tête) d'une paire : une bougie par pas de 4 h dans `[debut, fin)`, prix en marche
/// multiplicative autour de `base`, volume à `volume` (± 25 %).
fn csv_paire(seed: u64, debut: i64, fin: i64, base: f64, volume: f64) -> String {
    let mut rng = SplitMix64(seed);
    let mut texte = String::new();
    let mut close = base;
    let mut t = debut;
    while t < fin {
        let open = close;
        close = open * (1.0 + (rng.next_f64() - 0.5) * 0.04);
        let high = open.max(close) * 1.002;
        let low = open.min(close) * 0.998;
        let vol = volume * (0.75 + 0.5 * rng.next_f64());
        let count = 5 + (rng.next_u64() % 50);
        writeln!(
            texte,
            "{t},{open:.2},{high:.2},{low:.2},{close:.2},{vol:.4},{count}"
        )
        .expect("écriture en mémoire");
        t += STEP_S;
    }
    texte
}

/// Écrit le zip synthétique et rend son SHA-256.
pub fn ecrire_zip_synthetique(chemin: &Path) -> String {
    ecrire_zip_variante(chemin, 0)
}

/// Comme [`ecrire_zip_synthetique`], avec d'autres tirages de prix si `variante` n'est pas nul (un autre
/// zip, donc un autre `input_hash`, avec les mêmes paires et les mêmes rangs).
pub fn ecrire_zip_variante(chemin: &Path, variante: u64) -> String {
    let debut = secs(2025, 6, 1);
    let fin = secs(2026, 10, 1);
    let xbt = csv_paire(1 + 100 * variante, debut, fin, 60_000.0, 40.0);
    let eth = csv_paire(2 + 100 * variante, debut, fin, 3_000.0, 400.0);
    let sol = csv_paire(3 + 100 * variante, debut, fin, 150.0, 4_000.0);
    // ADAUSD : historique trop court à D = 2026-07-01.
    let ada = csv_paire(4 + 100 * variante, secs(2026, 5, 1), fin, 0.5, 90_000.0);
    ecrire_zip(
        chemin,
        &[
            ("MANIFEST.json", &manifeste_synthetique("2026-09-30")),
            ("XBTUSD_1.csv", xbt.as_bytes()),
            ("ETHUSD_1.csv", eth.as_bytes()),
            ("SOLUSD_1.csv", sol.as_bytes()),
            ("ADAUSD_1.csv", ada.as_bytes()),
        ],
    );
    sha256_fichier(chemin)
}

pub fn tv4(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tv4"))
        .args(args)
        .output()
        .expect("le binaire tv4 doit pouvoir être lancé")
}

pub fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// Chemin d'un fichier du dépôt, relatif à la racine.
pub fn repo(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

/// Code, stdout et stderr d'une sortie.
pub fn texte(sortie: &Output) -> (Option<i32>, String, String) {
    (
        sortie.status.code(),
        String::from_utf8_lossy(&sortie.stdout).into_owned(),
        String::from_utf8_lossy(&sortie.stderr).into_owned(),
    )
}

/// Lance `tv4` et exige le code de sortie 0.
pub fn ok(args: &[String]) -> (String, String) {
    let (code, stdout, stderr) = texte(&tv4(args));
    assert_eq!(code, Some(0), "tv4 {args:?}\n{stdout}\n{stderr}");
    (stdout, stderr)
}

/// Lance `tv4` et rend (code, stdout + stderr).
pub fn run(args: &[String]) -> (Option<i32>, String) {
    let (code, stdout, stderr) = texte(&tv4(args));
    (code, format!("{stdout}{stderr}"))
}

/// Monde de test : racine du store, univers `u` construit, zip et paramètres de stratégie.
pub struct World {
    pub _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub zip: PathBuf,
    pub sha: String,
    pub universe: PathBuf,
    pub params: PathBuf,
}

impl World {
    /// Racine, zip et fichier de paramètres seulement : rien n'est encore construit.
    pub fn bare() -> World {
        let dir = tempfile::tempdir().expect("dossier temporaire");
        let root = dir.path().join("racine");
        fs::create_dir_all(&root).expect("racine");
        let zip = dir.path().join("dump.zip");
        let sha = ecrire_zip_synthetique(&zip);
        let params = dir.path().join("params.toml");
        fs::write(
            &params,
            "tf = \"4h\"\nfast = 3\nslow = 8\nstop_atr_mult = 2.0\n",
        )
        .expect("params");
        let universe = dir.path().join("univers");
        World {
            _dir: dir,
            root,
            zip,
            sha,
            universe,
            params,
        }
    }

    /// Arguments de `universe build` du trimestre 2026-Q3.
    pub fn build_args(&self) -> Vec<String> {
        let mut args = strings(&["universe", "build", "--zip"]);
        args.extend([
            self.zip.display().to_string(),
            "--sha256".to_string(),
            self.sha.clone(),
            "--out".to_string(),
            self.universe.display().to_string(),
            "--config".to_string(),
            repo("config/universe.toml").display().to_string(),
            "--from-quarter".to_string(),
            "2026-Q3".to_string(),
            "--to-quarter".to_string(),
            "2026-Q3".to_string(),
        ]);
        args
    }

    /// `universe build` puis `ingest kraken --pairs-from-universe`.
    pub fn with_universe() -> World {
        let world = World::bare();
        ok(&world.build_args());
        ok(&world.ingest_args());
        world
    }

    pub fn ingest_args(&self) -> Vec<String> {
        let mut args = strings(&["ingest", "kraken", "--zip"]);
        args.extend([
            self.zip.display().to_string(),
            "--sha256".to_string(),
            self.sha.clone(),
            "--pairs-from-universe".to_string(),
            self.universe.display().to_string(),
            "--root".to_string(),
            self.root.display().to_string(),
        ]);
        args
    }

    /// Arguments de `dataset build --block FORWARD --universe`.
    pub fn dataset_args(&self, name: &str, warmup_days: &str) -> Vec<String> {
        let mut args = strings(&[
            "dataset",
            "build",
            "--exchange",
            "kraken",
            "--tf",
            "1m",
            "--block",
            "FORWARD",
            "--to",
            FORWARD_TO,
            "--warmup-days",
            warmup_days,
            "--name",
            name,
        ]);
        args.extend([
            "--root".to_string(),
            self.root.display().to_string(),
            "--universe".to_string(),
            self.universe.display().to_string(),
        ]);
        args
    }

    /// Monde complet : univers, ingestion et dataset `fwd` (chauffe 5 jours).
    pub fn with_dataset() -> World {
        let world = World::with_universe();
        ok(&world.dataset_args("fwd", "5"));
        world
    }

    /// Arguments de `backtest` en mode recherche sur le dataset `fwd`, écrivant dans `out`.
    pub fn backtest_args(&self, out: &Path, extra: &[&str]) -> Vec<String> {
        let mut args = strings(&["backtest", "--dataset", "fwd", "--strategy", "sma_cross"]);
        args.extend([
            "--root".to_string(),
            self.root.display().to_string(),
            "--params".to_string(),
            self.params.display().to_string(),
            "--fees-file".to_string(),
            repo("config/fees.toml").display().to_string(),
            "--out".to_string(),
            out.display().to_string(),
            "--universe".to_string(),
            self.universe.display().to_string(),
        ]);
        args.extend(strings(extra));
        args
    }
}

impl World {
    /// Écrit une liste figée de paires (`tv4-pair-subset-v1`) de ces bases et rend son chemin.
    pub fn write_subset(&self, name: &str, bases: &[&str]) -> PathBuf {
        let path = self._dir.path().join(name);
        let subset = PairSubset::new(
            "okx_usdc_tradable",
            SubsetSource {
                instruments_file: "synthetique.json".to_string(),
                instruments_hash: format!("blake3:{}", "77".repeat(32)),
                site: "eea".to_string(),
                fetched_at: "2026-10-30T10:15:00Z".to_string(),
            },
            SubsetRule {
                quote: "USDC".to_string(),
                state: "live".to_string(),
                trade_quote_ccy: true,
                aliases: BTreeMap::new(),
            },
            bases.iter().map(|b| (*b).to_string()).collect(),
        )
        .expect("sous-ensemble valide");
        write_pair_subset(&path, &subset).expect("écriture du sous-ensemble");
        path
    }
}

/// Lit un fichier JSON.
pub fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).expect("fichier JSON")).expect("JSON valide")
}
