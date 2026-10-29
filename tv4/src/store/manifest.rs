//! Manifestes JSON du store (docs/03 §5.2) : un par fichier de bougies, un par dataset figé.
//!
//! Écriture : `serde_json::to_value` puis `to_string_pretty` (objets en `BTreeMap`, donc clés triées,
//! indentation de 2 espaces) et un `\n` final, par `write_bytes_durable`. Lecture : `from_slice`, les
//! champs inconnus sont ignorés. Aucun manifeste n'entre dans un hash.
//!
//! Écarts à l'exemple de docs/03 §5.2 : `period` remplace `month` (les fichiers annuels n'ont pas de
//! mois), `continuity` est écrit par `tv4 ingest kraken` (S5) et absent des fichiers écrits par
//! `tv4 ingest csv` ou en S4 (la clé n'est alors pas écrite, et un manifeste S4 se relit sans erreur),
//! et chaque fichier d'un manifeste de dataset porte aussi `symbol`, `tf`, `period` et `snapshot`.
//!
//! S11 (docs/specs/s11.md §6.1) ajoute quatre champs facultatifs au manifeste de dataset, **hors de
//! `dsh-v1`** : `block`, `decision_window`, `universe` et `quality_exclusions`. Absents, leur clé n'est
//! pas écrite (`skip_serializing_if`) : un manifeste existant se relit et se réécrit à l'octet.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{Block, ExchangeId, QuarterExclusion, Timeframe, Ts, UniverseRef};

use super::StoreError;
use super::durable::{io_error, write_bytes_durable};
use super::layout::{GapsPolicy, Symbol};

/// Bloc `continuity` d'un manifeste de fichier (docs/03 §5.2), écrit par `tv4 ingest kraken`.
///
/// Les compteurs portent sur la période du fichier : un trou est compté dans la période de la bougie
/// qui le termine, un doublon exact ou une bougie non clôturée dans la période qui contient son
/// horodatage. `ohlc_errors` vaut toujours 0 dans un fichier écrit (une erreur OHLC bloque la paire à
/// l'ingestion) ; le champ est gardé pour suivre le format de docs/03 §5.2.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityCounts {
    /// Nombre de trous (minutes absentes consécutives comptées pour un seul).
    pub gaps: u64,
    /// Plus long trou en millisecondes de bougies absentes ; 0 sans trou.
    pub longest_gap_ms: i64,
    /// Doublons exacts retirés.
    pub duplicates: u64,
    /// Bougies exclues parce que leur fin dépasse la fin de couverture.
    pub rejected_unclosed: u64,
    pub ohlc_errors: u64,
}

/// Manifeste d'un fichier de bougies : `<période>._manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifest {
    pub exchange: ExchangeId,
    pub symbol: Symbol,
    pub tf: Timeframe,
    /// `2024-01` (fichier mensuel) ou `2024` (fichier annuel). Écart à docs/03 §5.2 (`month`).
    pub period: String,
    /// Fin de la période <= instant d'écriture (D4).
    pub closed: bool,
    pub row_count: u64,
    pub ts_min: Ts,
    pub ts_max: Ts,
    /// `blake3:<64 hex>`
    pub content_hash: String,
    /// `dsh-v1`
    pub hash_format: String,
    /// Provenance, `[a-z0-9-]{1,32}` : `kraken-csv` pour `tv4 ingest csv`.
    pub source: String,
    /// RFC 3339 UTC à la seconde ; n'entre dans aucun hash.
    pub ingested_at: String,
    /// Absent des manifestes S4 et de `tv4 ingest csv` : la clé n'est alors pas écrite, et un manifeste
    /// S4 se relit sans erreur.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityCounts>,
}

/// Manifeste d'un dataset figé : `datasets/<nom>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub name: String,
    pub exchange: ExchangeId,
    pub symbols: Vec<Symbol>,
    pub tfs: Vec<Timeframe>,
    /// `[début, fin)` en RFC 3339 UTC.
    pub window: [String; 2],
    pub gaps_policy: GapsPolicy,
    /// Tri par (symbole, tf, période) croissants.
    pub files: Vec<DatasetFile>,
    /// Lignes hachées : dans la fenêtre, avant tout remplissage (différent de la somme des
    /// `row_count` des fichiers quand la fenêtre coupe un fichier).
    pub row_count: u64,
    pub dataset_hash: String,
    pub hash_format: String,
    pub built_at: String,
    pub tv4_version: String,
    /// Bloc de recherche (`DEV`, `VALIDATION`, `HOLDOUT`, `FORWARD`) d'un dataset de recherche
    /// (s11 §6.1). Hors de `dsh-v1` : le `dataset_hash` reste un hash de contenu. Absent d'un
    /// manifeste ordinaire ; présent, il impose les trois autres champs ci-dessous (sauf `universe`,
    /// absent en univers fixe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<Block>,
    /// `[début du bloc, min(fin du bloc, fin de la fenêtre))` en RFC 3339 UTC : la fenêtre où le moteur
    /// a le droit de décider, le reste du dataset étant la chauffe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_window: Option<[String; 2]>,
    /// `rules_hash` et `window_hash` de l'univers trimestriel lu au build (s11 §4.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe: Option<UniverseRef>,
    /// Par symbole, les trimestres qui recoupent la fenêtre de décision et ont au moins un TF exclu par
    /// la qualité (s11 §4.3). Présent, éventuellement vide, sur tout manifeste qui porte `block`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_exclusions: Option<BTreeMap<String, Vec<QuarterExclusion>>>,
}

/// Un fichier listé par un dataset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetFile {
    /// Relatif à la racine : fichier vivant (période close) ou copie sous `datasets/snapshots/`.
    pub path: String,
    pub symbol: Symbol,
    pub tf: Timeframe,
    pub period: String,
    pub content_hash: String,
    /// Lignes du fichier entier.
    pub row_count: u64,
    /// Vrai si `path` est une copie figée d'un fichier non clos au moment du build.
    pub snapshot: bool,
}

/// Rendu canonique d'un manifeste : JSON à clés triées, indenté, avec un `\n` final.
fn render<T: Serialize>(path: &Path, manifest: &T) -> Result<Vec<u8>, StoreError> {
    let json_error = |source| StoreError::Json {
        path: path.display().to_string(),
        source,
    };
    let value = serde_json::to_value(manifest).map_err(json_error)?;
    let mut text = serde_json::to_string_pretty(&value).map_err(json_error)?;
    text.push('\n');
    Ok(text.into_bytes())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, StoreError> {
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    serde_json::from_slice(&bytes).map_err(|source| StoreError::Json {
        path: path.display().to_string(),
        source,
    })
}

/// Lit un manifeste de fichier.
pub fn read_file_manifest(path: &Path) -> Result<FileManifest, StoreError> {
    read_json(path)
}

/// Lit un manifeste de dataset.
pub fn read_dataset_manifest(path: &Path) -> Result<DatasetManifest, StoreError> {
    read_json(path)
}

/// Écrit un manifeste de fichier de façon durable.
pub(crate) fn write_file_manifest(path: &Path, manifest: &FileManifest) -> Result<(), StoreError> {
    write_bytes_durable(path, &render(path, manifest)?)
}

/// Écrit un manifeste de dataset de façon durable.
pub(crate) fn write_dataset_manifest(
    path: &Path,
    manifest: &DatasetManifest,
) -> Result<(), StoreError> {
    write_bytes_durable(path, &render(path, manifest)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifeste_fichier() -> FileManifest {
        FileManifest {
            exchange: ExchangeId::Kraken,
            symbol: "BTC-USD".parse().unwrap(),
            tf: Timeframe::M1,
            period: "2024-01".to_string(),
            closed: true,
            row_count: 3,
            ts_min: 1_704_067_200_000,
            ts_max: 1_704_067_320_000,
            content_hash: format!("blake3:{}", "3d".repeat(32)),
            hash_format: "dsh-v1".to_string(),
            source: "golden".to_string(),
            ingested_at: "2026-10-14T00:00:00Z".to_string(),
            continuity: None,
        }
    }

    #[test]
    fn manifeste_de_fichier_cles_triees_et_aller_retour() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m._manifest.json");
        let manifeste = manifeste_fichier();
        write_file_manifest(&path, &manifeste).unwrap();
        let texte = fs::read_to_string(&path).unwrap();
        assert!(
            texte.starts_with("{\n  \"closed\": true,\n  \"content_hash\""),
            "{texte}"
        );
        assert!(texte.ends_with("}\n"));
        let cles: Vec<&str> = texte
            .lines()
            .filter_map(|l| l.strip_prefix("  \""))
            .filter_map(|l| l.split('"').next())
            .collect();
        let mut triees = cles.clone();
        triees.sort_unstable();
        assert_eq!(cles, triees);
        assert!(!texte.contains("continuity"));
        assert_eq!(read_file_manifest(&path).unwrap(), manifeste);
    }

    #[test]
    fn champs_inconnus_ignores_a_la_lecture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m._manifest.json");
        write_file_manifest(&path, &manifeste_fichier()).unwrap();
        let mut valeur: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        valeur["champ_futur"] = serde_json::json!({"gaps": 0});
        fs::write(&path, serde_json::to_vec(&valeur).unwrap()).unwrap();
        assert_eq!(read_file_manifest(&path).unwrap(), manifeste_fichier());
    }

    #[test]
    fn manifeste_s4_sans_bloc_se_relit() {
        // Le golden S4 n'a pas de bloc `continuity`.
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/golden/kraken_BTC-USD_1m_2024-01._manifest.json");
        let manifeste = read_file_manifest(&golden).unwrap();
        assert_eq!(manifeste.continuity, None);
        assert_eq!(manifeste.source, "golden");
    }

    #[test]
    fn bloc_de_continuite_aller_retour_et_cle_ecrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m._manifest.json");
        let mut manifeste = manifeste_fichier();
        manifeste.continuity = Some(ContinuityCounts {
            gaps: 951,
            longest_gap_ms: 480_000,
            duplicates: 1,
            rejected_unclosed: 2,
            ohlc_errors: 0,
        });
        write_file_manifest(&path, &manifeste).unwrap();
        let texte = fs::read_to_string(&path).unwrap();
        assert!(
            texte.contains("\"continuity\": {\n    \"duplicates\": 1,"),
            "{texte}"
        );
        assert_eq!(read_file_manifest(&path).unwrap(), manifeste);
    }

    #[test]
    fn json_invalide_nomme_le_fichier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("casse._manifest.json");
        fs::write(&path, b"{ pas du json").unwrap();
        let erreur = read_file_manifest(&path).unwrap_err();
        assert!(matches!(erreur, StoreError::Json { .. }));
        assert!(erreur.to_string().contains("casse._manifest.json"));
        let absent = read_dataset_manifest(&dir.path().join("absent.json"));
        assert!(matches!(absent, Err(StoreError::Io { .. })));
    }

    #[test]
    fn manifeste_de_dataset_aller_retour() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.json");
        let manifeste = DatasetManifest {
            name: "exemple".to_string(),
            exchange: ExchangeId::Kraken,
            symbols: vec!["BTC-USD".parse().unwrap()],
            tfs: vec![Timeframe::M1],
            window: [
                "2020-01-01T00:00:00Z".to_string(),
                "2020-01-09T00:00:00Z".to_string(),
            ],
            gaps_policy: GapsPolicy::ZeroVolumeMarked,
            files: vec![DatasetFile {
                path: "data/candles/kraken/BTC-USD/1m/2020-01.parquet".to_string(),
                symbol: "BTC-USD".parse().unwrap(),
                tf: Timeframe::M1,
                period: "2020-01".to_string(),
                content_hash: format!("blake3:{}", "ab".repeat(32)),
                row_count: 10_000,
                snapshot: false,
            }],
            row_count: 10_000,
            dataset_hash: format!("blake3:{}", "cd".repeat(32)),
            hash_format: "dsh-v1".to_string(),
            built_at: "2026-10-14T10:00:00Z".to_string(),
            tv4_version: "0.1.0".to_string(),
            block: None,
            decision_window: None,
            universe: None,
            quality_exclusions: None,
        };
        write_dataset_manifest(&path, &manifeste).unwrap();
        let texte = fs::read_to_string(&path).unwrap();
        assert!(texte.contains("\"zero_volume_marked\""));
        // Les champs de recherche absents ne laissent aucune clé : le rendu d'un manifeste ordinaire
        // reste celui d'avant S11.
        for cle in ["block", "decision_window", "universe", "quality_exclusions"] {
            assert!(
                !texte.contains(cle),
                "clé {cle} écrite sans valeur : {texte}"
            );
        }
        assert_eq!(read_dataset_manifest(&path).unwrap(), manifeste);
        // Relu puis réécrit : mêmes octets.
        let relu = read_dataset_manifest(&path).unwrap();
        let autre = dir.path().join("d2.json");
        write_dataset_manifest(&autre, &relu).unwrap();
        assert_eq!(fs::read(&autre).unwrap(), fs::read(&path).unwrap());
    }

    #[test]
    fn manifeste_de_recherche_aller_retour_et_cles_triees() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.json");
        let mut valeurs = BTreeMap::new();
        valeurs.insert(
            "ETH-USD".to_string(),
            vec![QuarterExclusion {
                quarter: "2018-Q1".to_string(),
                tfs: vec![Timeframe::H1, Timeframe::H4],
            }],
        );
        let manifeste = DatasetManifest {
            name: "recherche".to_string(),
            exchange: ExchangeId::Kraken,
            symbols: vec!["ETH-USD".parse().unwrap()],
            tfs: vec![Timeframe::M1],
            window: [
                "2017-06-05T00:00:00Z".to_string(),
                "2023-01-01T00:00:00Z".to_string(),
            ],
            gaps_policy: GapsPolicy::ZeroVolumeMarked,
            files: Vec::new(),
            row_count: 1,
            dataset_hash: format!("blake3:{}", "cd".repeat(32)),
            hash_format: "dsh-v1".to_string(),
            built_at: "2026-10-14T10:00:00Z".to_string(),
            tv4_version: "0.1.0".to_string(),
            block: Some(Block::Dev),
            decision_window: Some([
                "2018-01-01T00:00:00Z".to_string(),
                "2023-01-01T00:00:00Z".to_string(),
            ]),
            universe: None,
            quality_exclusions: Some(valeurs),
        };
        write_dataset_manifest(&path, &manifeste).unwrap();
        let texte = fs::read_to_string(&path).unwrap();
        assert!(texte.contains("\"block\": \"DEV\""), "{texte}");
        assert!(texte.contains("\"quarter\": \"2018-Q1\""), "{texte}");
        // Pas d'univers en univers fixe : la clé n'est pas écrite.
        assert!(!texte.contains("\"universe\""), "{texte}");
        let cles: Vec<&str> = texte
            .lines()
            .filter_map(|l| l.strip_prefix("  \""))
            .filter_map(|l| l.split('"').next())
            .collect();
        let mut triees = cles.clone();
        triees.sort_unstable();
        assert_eq!(cles, triees);
        assert_eq!(read_dataset_manifest(&path).unwrap(), manifeste);
    }
}
