//! Types de base et chemins du store (docs/03 §5.1).
//!
//! Symbole canonique, fenêtre semi-ouverte, politique de trous, partition, période d'un fichier et
//! chemins relatifs à la racine de travail. Rien ici ne touche le disque : les chemins sont des
//! `String` avec `/` pour séparateur, ce qui est aussi la forme écrite dans les manifestes (D10).

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::model::{ExchangeId, Timeframe, Ts};

use super::StoreError;
use super::time::{MAX_TS_MS, MS_PER_DAY, civil_from_days, days_from_civil};

/// Longueur maximale d'un symbole canonique, en octets.
const MAX_SYMBOL_LEN: usize = 32;

/// Symbole canonique `BASE-QUOTE` (docs/dataset-hash.md §2.2) : `[A-Z0-9]+-[A-Z0-9]+`, un seul `-`,
/// 32 octets au plus. Jamais le symbole natif de l'exchange (`XBTUSD` s'écrit `BTC-USD`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Symbol(String);

impl Symbol {
    /// Texte canonique du symbole.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Vrai si `part` est une suite non vide de `[A-Z0-9]`.
fn is_symbol_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

impl FromStr for Symbol {
    type Err = StoreError;

    fn from_str(text: &str) -> Result<Symbol, StoreError> {
        let valide = text.len() <= MAX_SYMBOL_LEN
            && text
                .split_once('-')
                .is_some_and(|(base, quote)| is_symbol_part(base) && is_symbol_part(quote));
        if valide {
            Ok(Symbol(text.to_string()))
        } else {
            Err(StoreError::InvalidSymbol(text.to_string()))
        }
    }
}

impl TryFrom<String> for Symbol {
    type Error = StoreError;

    fn try_from(text: String) -> Result<Symbol, StoreError> {
        text.parse()
    }
}

impl From<Symbol> for String {
    fn from(symbol: Symbol) -> String {
        symbol.0
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Fenêtre semi-ouverte `[start_ms, end_ms)`, `start < end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    start_ms: Ts,
    end_ms: Ts,
}

impl Window {
    /// Construit une fenêtre ; [`StoreError::EmptyWindow`] si `start_ms >= end_ms`.
    pub fn new(start_ms: Ts, end_ms: Ts) -> Result<Window, StoreError> {
        if start_ms >= end_ms {
            return Err(StoreError::EmptyWindow { start_ms, end_ms });
        }
        Ok(Window { start_ms, end_ms })
    }

    /// Début inclus.
    pub const fn start_ms(self) -> Ts {
        self.start_ms
    }

    /// Fin exclue.
    pub const fn end_ms(self) -> Ts {
        self.end_ms
    }

    /// Vrai si `start <= ts < end`.
    pub const fn contains(self, ts: Ts) -> bool {
        self.start_ms <= ts && ts < self.end_ms
    }

    /// Vrai si `[min, max]` (bornes incluses) recoupe la fenêtre : `min < end && max >= start`.
    pub const fn intersects(self, min: Ts, max: Ts) -> bool {
        min < self.end_ms && max >= self.start_ms
    }

    /// Les deux bornes sont des débuts de bougie de `tf` ([`StoreError::WindowNotAligned`] sinon).
    pub fn check_aligned(self, tf: Timeframe) -> Result<(), StoreError> {
        for ts_ms in [self.start_ms, self.end_ms] {
            if !tf.is_aligned(ts_ms) {
                return Err(StoreError::WindowNotAligned { tf, ts_ms });
            }
        }
        Ok(())
    }
}

/// Politique de trous d'un dataset (docs/03 §5.1, §15.1 ligne 13 ; docs/dataset-hash.md §2.2).
///
/// Le remplissage est appliqué au chargement, jamais au stockage ni au hash : la valeur n'entre dans
/// le hash que par l'en-tête du segment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapsPolicy {
    /// Les trous restent des trous (défaut de docs/03 §5.1).
    #[default]
    Skip,
    /// Bougies manquantes intérieures recopiées du close précédent, volume nul, marquées synthétiques.
    Ffill,
    /// Mêmes bougies que `Ffill`, mais le simulateur devra interdire tout fill dedans (S6).
    ZeroVolumeMarked,
}

impl GapsPolicy {
    /// Rendu canonique, identique à la forme sérialisée et à l'en-tête dsh-v1.
    pub const fn as_str(self) -> &'static str {
        match self {
            GapsPolicy::Skip => "skip",
            GapsPolicy::Ffill => "ffill",
            GapsPolicy::ZeroVolumeMarked => "zero_volume_marked",
        }
    }
}

impl FromStr for GapsPolicy {
    type Err = StoreError;

    fn from_str(text: &str) -> Result<GapsPolicy, StoreError> {
        match text {
            "skip" => Ok(GapsPolicy::Skip),
            "ffill" => Ok(GapsPolicy::Ffill),
            "zero_volume_marked" => Ok(GapsPolicy::ZeroVolumeMarked),
            autre => Err(StoreError::UnknownGapsPolicy(autre.to_string())),
        }
    }
}

impl fmt::Display for GapsPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Partition d'écriture (exchange, symbole, timeframe) : un seul écrivain à la fois (verrou).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub exchange: ExchangeId,
    pub symbol: Symbol,
    pub tf: Timeframe,
}

impl Partition {
    pub fn new(exchange: ExchangeId, symbol: Symbol, tf: Timeframe) -> Partition {
        Partition {
            exchange,
            symbol,
            tf,
        }
    }

    /// `kraken/BTC-USD/1m`, pour les messages.
    pub fn label(&self) -> String {
        format!("{}/{}/{}", self.exchange, self.symbol, self.tf)
    }

    /// `data/candles/kraken/BTC-USD/1m`
    pub fn dir_rel(&self) -> String {
        format!("data/candles/{}", self.label())
    }

    /// `data/candles/kraken/BTC-USD/1m/2024-01.parquet`
    pub fn file_rel(&self, period: Period) -> String {
        format!("{}/{}.parquet", self.dir_rel(), period.label())
    }

    /// `data/candles/kraken/BTC-USD/1m/2024-01._manifest.json`
    pub fn manifest_rel(&self, period: Period) -> String {
        format!("{}/{}._manifest.json", self.dir_rel(), period.label())
    }

    /// `state/locks/kraken/BTC-USD/1m.lock`
    pub fn lock_rel(&self) -> String {
        format!("state/locks/{}.lock", self.label())
    }
}

/// Période d'un fichier : un mois (TF inférieur à 1h) ou une année (TF de 1h et plus).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Period {
    Month { year: i32, month: u32 },
    Year { year: i32 },
}

impl Period {
    /// Les fichiers des TF inférieurs à 1h sont mensuels (1m, 5m, 15m, 30m), les autres annuels.
    pub fn is_monthly(tf: Timeframe) -> bool {
        tf < Timeframe::H1
    }

    /// Période du fichier de `tf` qui contient `ts`
    /// ([`StoreError::TimestampOutOfRange`] hors de `[0, MAX_TS_MS)`).
    pub fn containing(tf: Timeframe, ts: Ts) -> Result<Period, StoreError> {
        if !(0..MAX_TS_MS).contains(&ts) {
            return Err(StoreError::TimestampOutOfRange { ts_ms: ts });
        }
        let (y, m, _) = civil_from_days(ts.div_euclid(MS_PER_DAY));
        // L'année est dans [1970, 9999] puisque `ts` est dans [0, MAX_TS_MS).
        let year = i32::try_from(y).map_err(|_| StoreError::TimestampOutOfRange { ts_ms: ts })?;
        Ok(if Period::is_monthly(tf) {
            Period::Month { year, month: m }
        } else {
            Period::Year { year }
        })
    }

    /// Premier instant de la période.
    fn start_ms(self) -> Ts {
        match self {
            Period::Month { year, month } => {
                days_from_civil(i64::from(year), month, 1) * MS_PER_DAY
            }
            Period::Year { year } => days_from_civil(i64::from(year), 1, 1) * MS_PER_DAY,
        }
    }

    /// `[premier instant de la période, premier instant de la suivante)`.
    pub fn window(self) -> Window {
        // Construit directement : une période valide a toujours `start < end`.
        Window {
            start_ms: self.start_ms(),
            end_ms: self.next().start_ms(),
        }
    }

    /// Période suivante.
    pub fn next(self) -> Period {
        match self {
            Period::Month { year, month: 12 } => Period::Month {
                year: year + 1,
                month: 1,
            },
            Period::Month { year, month } => Period::Month {
                year,
                month: month + 1,
            },
            Period::Year { year } => Period::Year { year: year + 1 },
        }
    }

    /// `2024-01` ou `2024`.
    pub fn label(self) -> String {
        match self {
            Period::Month { year, month } => format!("{year:04}-{month:02}"),
            Period::Year { year } => format!("{year:04}"),
        }
    }

    /// Inverse de [`Period::label`] pour le `tf` donné ([`StoreError::InvalidPeriod`] sinon) :
    /// `2024-01` exige un TF inférieur à 1h, `2024` un TF de 1h ou plus.
    pub fn parse(tf: Timeframe, label: &str) -> Result<Period, StoreError> {
        let invalid = || StoreError::InvalidPeriod {
            tf,
            label: label.to_string(),
        };
        let number = |text: &str| -> Option<i32> {
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            text.parse().ok()
        };
        let period = if Period::is_monthly(tf) {
            let (year, month) = label.split_once('-').ok_or_else(invalid)?;
            if year.len() != 4 || month.len() != 2 {
                return Err(invalid());
            }
            let month = number(month)
                .and_then(|m| u32::try_from(m).ok())
                .ok_or_else(invalid)?;
            if !(1..=12).contains(&month) {
                return Err(invalid());
            }
            Period::Month {
                year: number(year).ok_or_else(invalid)?,
                month,
            }
        } else {
            if label.len() != 4 {
                return Err(invalid());
            }
            Period::Year {
                year: number(label).ok_or_else(invalid)?,
            }
        };
        // Avant 1970, aucun horodatage n'est accepté par le store : pas de période correspondante.
        let year = match period {
            Period::Month { year, .. } | Period::Year { year } => year,
        };
        if !(1970..=9999).contains(&year) {
            return Err(invalid());
        }
        Ok(period)
    }

    /// Périodes qui recoupent `window`, dans l'ordre : de `containing(start)` à `containing(end − 1)`.
    pub fn covering(tf: Timeframe, window: Window) -> Result<Vec<Period>, StoreError> {
        let first = Period::containing(tf, window.start_ms())?;
        let last = Period::containing(tf, window.end_ms() - 1)?;
        let mut periods = vec![first];
        let mut current = first;
        while current != last {
            current = current.next();
            periods.push(current);
        }
        Ok(periods)
    }
}

/// Joint un chemin relatif de manifeste à la racine, après contrôle ([`StoreError::UnsafePath`]
/// sinon) : non vide, pas de `/` initial, pas de `\`, aucun composant vide, `.` ou `..`, et préfixe
/// `data/candles/` ou `datasets/snapshots/`. Joint composant par composant.
pub fn resolve_rel(root: &Path, rel: &str) -> Result<PathBuf, StoreError> {
    let unsafe_path = || StoreError::UnsafePath {
        path: rel.to_string(),
    };
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') {
        return Err(unsafe_path());
    }
    if !(rel.starts_with("data/candles/") || rel.starts_with("datasets/snapshots/")) {
        return Err(unsafe_path());
    }
    let mut path = root.to_path_buf();
    for component in rel.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(unsafe_path());
        }
        path = path.join(component);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partition(tf: Timeframe) -> Partition {
        Partition::new(ExchangeId::Kraken, "BTC-USD".parse().unwrap(), tf)
    }

    #[test]
    fn periode_mensuelle_sous_l_heure_annuelle_au_dela() {
        for tf in [Timeframe::M1, Timeframe::M5, Timeframe::M15, Timeframe::M30] {
            assert!(Period::is_monthly(tf), "{tf} devrait être mensuel");
        }
        for tf in [Timeframe::H1, Timeframe::H4, Timeframe::H12, Timeframe::D1] {
            assert!(!Period::is_monthly(tf), "{tf} devrait être annuel");
        }
    }

    #[test]
    fn bornes_des_periodes() {
        let janvier = Period::Month {
            year: 2024,
            month: 1,
        };
        let w = janvier.window();
        assert_eq!(
            (w.start_ms(), w.end_ms()),
            (1_704_067_200_000, 1_706_745_600_000)
        );
        // Février 2024 est bissextile.
        let fevrier = janvier.next();
        assert_eq!(fevrier.window().end_ms(), 1_709_251_200_000);
        assert_eq!(
            Period::Month {
                year: 2023,
                month: 12
            }
            .next(),
            janvier
        );
        let annee = Period::Year { year: 2024 }.window();
        assert_eq!(
            (annee.start_ms(), annee.end_ms()),
            (1_704_067_200_000, 1_735_689_600_000)
        );
    }

    #[test]
    fn periode_contenant_un_instant() {
        let m1 = Timeframe::M1;
        let janvier = Period::Month {
            year: 2024,
            month: 1,
        };
        assert_eq!(Period::containing(m1, 1_704_067_200_000).unwrap(), janvier);
        // Dernière milliseconde de janvier, puis première de février.
        assert_eq!(Period::containing(m1, 1_706_745_599_999).unwrap(), janvier);
        assert_eq!(
            Period::containing(m1, 1_706_745_600_000).unwrap(),
            janvier.next()
        );
        assert_eq!(
            Period::containing(Timeframe::D1, 1_706_745_600_000).unwrap(),
            Period::Year { year: 2024 }
        );
        assert!(Period::containing(m1, -1).is_err());
        assert!(Period::containing(m1, MAX_TS_MS).is_err());
    }

    #[test]
    fn periodes_couvrant_une_fenetre() {
        // Du 2024-01-20 au 2024-03-01 : janvier et février.
        let w = Window::new(1_705_708_800_000, 1_709_251_200_000).unwrap();
        let periodes = Period::covering(Timeframe::M1, w).unwrap();
        assert_eq!(
            periodes,
            vec![
                Period::Month {
                    year: 2024,
                    month: 1
                },
                Period::Month {
                    year: 2024,
                    month: 2
                }
            ]
        );
        let une_minute = Window::new(1_704_067_200_000, 1_704_067_260_000).unwrap();
        assert_eq!(
            Period::covering(Timeframe::M1, une_minute).unwrap().len(),
            1
        );
        // Un TF annuel sur deux années.
        let deux_ans = Window::new(1_704_067_200_000, 1_767_225_600_000).unwrap();
        assert_eq!(Period::covering(Timeframe::D1, deux_ans).unwrap().len(), 2);
    }

    #[test]
    fn libelle_et_lecture_de_periode() {
        let mois = Period::Month {
            year: 2024,
            month: 1,
        };
        assert_eq!(mois.label(), "2024-01");
        assert_eq!(Period::parse(Timeframe::M1, "2024-01").unwrap(), mois);
        assert_eq!(Period::Year { year: 2024 }.label(), "2024");
        assert_eq!(
            Period::parse(Timeframe::H1, "2024").unwrap(),
            Period::Year { year: 2024 }
        );
        for (tf, texte) in [
            (Timeframe::H1, "2024-01"),
            (Timeframe::M1, "2024"),
            (Timeframe::M1, "2024-13"),
            (Timeframe::M1, "2024-1"),
            (Timeframe::M1, "1969-12"),
            (Timeframe::H1, "abcd"),
            (Timeframe::M1, ""),
        ] {
            assert!(
                matches!(
                    Period::parse(tf, texte),
                    Err(StoreError::InvalidPeriod { .. })
                ),
                "{tf} « {texte} » aurait dû être refusé"
            );
        }
    }

    #[test]
    fn chemins_de_partition() {
        let p = partition(Timeframe::M1);
        let mois = Period::Month {
            year: 2024,
            month: 1,
        };
        assert_eq!(p.label(), "kraken/BTC-USD/1m");
        assert_eq!(
            p.file_rel(mois),
            "data/candles/kraken/BTC-USD/1m/2024-01.parquet"
        );
        assert_eq!(
            p.manifest_rel(mois),
            "data/candles/kraken/BTC-USD/1m/2024-01._manifest.json"
        );
        assert_eq!(p.lock_rel(), "state/locks/kraken/BTC-USD/1m.lock");
        let h = partition(Timeframe::H1);
        assert_eq!(
            h.file_rel(Period::Year { year: 2024 }),
            "data/candles/kraken/BTC-USD/1h/2024.parquet"
        );
        assert_eq!(
            h.manifest_rel(Period::Year { year: 2024 }),
            "data/candles/kraken/BTC-USD/1h/2024._manifest.json"
        );
    }

    #[test]
    fn symbole_canonique() {
        for ok in ["BTC-USD", "BTC-USDC", "ETH-EUR"] {
            assert_eq!(ok.parse::<Symbol>().unwrap().as_str(), ok);
        }
        let trop_long = format!("{}-USD", "A".repeat(29));
        assert_eq!(trop_long.len(), 33);
        for ko in [
            "btc-usd",
            "BTCUSD",
            "BTC-",
            "-USD",
            "BTC-USD-X",
            "BTC|USD",
            "BTC-USD\n",
            "",
            trop_long.as_str(),
        ] {
            assert!(
                matches!(ko.parse::<Symbol>(), Err(StoreError::InvalidSymbol(_))),
                "« {ko:?} » aurait dû être refusé"
            );
        }
        let symbole: Symbol = "BTC-USD".parse().unwrap();
        let json = serde_json::to_string(&symbole).unwrap();
        assert_eq!(json, "\"BTC-USD\"");
        assert_eq!(serde_json::from_str::<Symbol>(&json).unwrap(), symbole);
        assert!(serde_json::from_str::<Symbol>("\"btc-usd\"").is_err());
    }

    #[test]
    fn fenetre_non_vide_et_alignement() {
        assert!(matches!(
            Window::new(10, 10),
            Err(StoreError::EmptyWindow { .. })
        ));
        assert!(Window::new(11, 10).is_err());
        let w = Window::new(60_000, 180_000).unwrap();
        assert!(w.check_aligned(Timeframe::M1).is_ok());
        assert!(matches!(
            w.check_aligned(Timeframe::M5),
            Err(StoreError::WindowNotAligned { ts_ms: 60_000, .. })
        ));
        assert!(w.contains(60_000) && w.contains(179_999));
        assert!(!w.contains(59_999) && !w.contains(180_000));
        // [min, max] avec bornes incluses.
        assert!(w.intersects(0, 60_000));
        assert!(!w.intersects(0, 59_999));
        assert!(w.intersects(179_999, 500_000));
        assert!(!w.intersects(180_000, 500_000));
    }

    #[test]
    fn gaps_policy_rendu_lecture_serde() {
        assert_eq!(GapsPolicy::default(), GapsPolicy::Skip);
        for (politique, texte) in [
            (GapsPolicy::Skip, "skip"),
            (GapsPolicy::Ffill, "ffill"),
            (GapsPolicy::ZeroVolumeMarked, "zero_volume_marked"),
        ] {
            assert_eq!(politique.as_str(), texte);
            assert_eq!(politique.to_string(), texte);
            assert_eq!(texte.parse::<GapsPolicy>().unwrap(), politique);
            let json = serde_json::to_string(&politique).unwrap();
            assert_eq!(json, format!("\"{texte}\""));
            assert_eq!(
                serde_json::from_str::<GapsPolicy>(&json).unwrap(),
                politique
            );
        }
        assert!(matches!(
            "forward".parse::<GapsPolicy>(),
            Err(StoreError::UnknownGapsPolicy(_))
        ));
    }

    #[test]
    fn chemin_relatif_sur() {
        let racine = Path::new("/racine");
        assert_eq!(
            resolve_rel(racine, "data/candles/x.parquet").unwrap(),
            racine.join("data").join("candles").join("x.parquet")
        );
        assert!(resolve_rel(racine, "datasets/snapshots/h/data/candles/x.parquet").is_ok());
        for ko in [
            "/etc/passwd",
            "../x",
            "data/candles/../x",
            "data\\candles\\x",
            "",
            "data//candles/x",
            "./data/candles/x",
            "runs/x",
            "data/candles/",
        ] {
            assert!(
                matches!(resolve_rel(racine, ko), Err(StoreError::UnsafePath { .. })),
                "« {ko} » aurait dû être refusé"
            );
        }
    }
}
