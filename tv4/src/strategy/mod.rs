//! Interface `Strategy` et types échangés avec le moteur (docs/03-architecture.md §6).
//!
//! Le module ne dépend que de `model` (règle vérifiée par `tests/architecture.rs`) : il ne connaît ni
//! les indicateurs concrets, ni le simulateur, ni `chrono`. Le moteur (S3) fait le lien : il calcule
//! les colonnes demandées par [`Strategy::indicators`] avec `indicators`, construit les [`TfView`] et
//! le [`BarContext`] de chaque bougie 1m, puis appelle [`Strategy::on_bar`].
//!
//! Écarts d'implémentation (S2) à l'extrait de docs/03 §6 :
//!
//! 1. `Side` vient de `model`, sans redéfinition ni réexport.
//! 2. Erreurs typées ([`StrategyError`], `thiserror`) au lieu d'`anyhow`, réservé au binaire.
//! 3. Pas d'`expect` dans `params_json` : `SmaCross::new` calcule une fois le JSON des paramètres.
//! 4. Tous les champs de [`BarContext`] sont privés, un accesseur par champ (`ctx.position()`) ; les
//!    scalaires sont regroupés dans [`BarState`].
//! 5. Constructeurs publics [`TfView::new`], [`TfView::with_closed`] et [`BarContext::new`], avec
//!    contrôle des longueurs et des invariants.
//! 6. [`TfView`] garde les colonnes complètes et un compteur `closed` ; le bornage `[..closed]` se fait
//!    dans les accesseurs, donc `with_closed` est en O(1) par bougie.
//! 7. `ind(tf, name)` n'utilise aucune table de hachage : les colonnes d'un TF sont dans l'ordre de
//!    `indicators()` restreint à ce TF ; la position du nom est une comparaison de `&'static str`.
//!    [`BarContext::ind_at`] donne l'accès direct par index.
//! 8. Masque `just_closed: u8` construit avec `Timeframe::bit()`.
//! 9. Ajout de [`BarContext::ind_at`] et de [`validate_declaration`].
//! 10. Le registre rend un `Result<_, StrategyError>`.
//! 11. [`BarState`], [`TfView`] et [`BarContext`] vivent dans le sous-module privé `context` et sont
//!     réexportés : un champ privé reste visible des modules descendants, donc définis ici, ils
//!     auraient été lisibles par `sma_cross` et toute stratégie future (fuite du futur sans `unsafe`).
//!     `tests/architecture.rs` interdit aux autres fichiers de `strategy/` d'accéder aux champs.
//!
//! Ajouts de S8 :
//!
//! 12. [`Strategy::funnel`] (méthode par défaut, `None`) : la stratégie déclare, hors boucle, le nombre
//!     de signaux d'entrée bruts qu'elle a vus et ceux qu'elle a filtrés par motif ; le moteur en fait
//!     l'entonnoir de `metrics.json`.
//! 13. Tag de sortie réservé [`TIME_STOP_TAG`] : une stratégie qui sort sur un délai l'utilise pour que
//!     `trades.csv` écrive `exit_type = time_stop` (sinon `strategy_exit`).
//!
//! Ajouts de S9 :
//!
//! 14. [`RegimeView`], [`BarContext::with_regime`] et [`BarContext::regime`] : les bougies 1d closes de
//!     la paire de régime, leur SMA200 et l'aide `bull()` (`>` strict), avec la garde anti look-ahead
//!     des vues. [`Strategy::uses_regime`] (défaut `false`) déclare le besoin.

#![forbid(unsafe_code)]

pub mod registry;
pub mod sma_cross;

use crate::model::{MAX_INDICATOR_PERIOD, Side, Timeframe, Ts};
use thiserror::Error;

// ---------------------------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------------------------

/// Erreurs du module `strategy` : déclaration, paramètres et construction des vues.
#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("stratégie inconnue : `{id}` (connues : {known})")]
    UnknownStrategy { id: String, known: String },

    #[error("stratégie `{id}` : paramètres TOML invalides : {source}")]
    ParamsToml {
        id: &'static str,
        #[source]
        source: toml::de::Error,
    },

    #[error("stratégie `{id}` : paramètres invalides : {reason}")]
    InvalidParams { id: &'static str, reason: String },

    #[error("stratégie `{id}` : paramètres non sérialisables en JSON : {source}")]
    ParamsJson {
        id: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("stratégie `{id}` : aucun timeframe déclaré")]
    NoTimeframe { id: &'static str },

    #[error("timeframe {tf} déclaré plusieurs fois")]
    DuplicateTimeframe { tf: Timeframe },

    #[error("indicateur `{name}` sur {tf} : ce timeframe n'est pas dans ceux de la stratégie")]
    IndicatorOutsideTimeframes { name: &'static str, tf: Timeframe },

    #[error("indicateur `{name}` déclaré deux fois sur {tf}")]
    DuplicateIndicator { tf: Timeframe, name: &'static str },

    #[error("indicateur `{name}` : période {period} hors de 1..={max}")]
    InvalidPeriod {
        name: &'static str,
        period: usize,
        max: usize,
    },

    #[error("vue {tf} : {names} noms d'indicateurs pour {columns} colonnes")]
    IndicatorCountMismatch {
        tf: Timeframe,
        names: usize,
        columns: usize,
    },

    #[error("vue {tf} : la colonne `{name}` a {found} valeurs, {expected} bougies attendues")]
    IndicatorLength {
        tf: Timeframe,
        name: &'static str,
        expected: usize,
        found: usize,
    },

    #[error("vue {tf} : {closed} bougies clôturées demandées, {len} disponibles")]
    ClosedOutOfRange {
        tf: Timeframe,
        closed: usize,
        len: usize,
    },

    #[error("contexte de bougie sans aucune vue de timeframe")]
    NoView,

    #[error(
        "masque just_closed {mask:#010b} : contient des timeframes non déclarés (déclarés : {declared:#010b})"
    )]
    UnknownJustClosed { mask: u8, declared: u8 },

    #[error(
        "vue {tf} : la dernière bougie visible (open_time {open_time_ms}) n'est pas clôturée à {now_ms}"
    )]
    LookAhead {
        tf: Timeframe,
        open_time_ms: Ts,
        now_ms: Ts,
    },

    #[error(
        "vue de régime : {candles} bougies pour {sma} valeurs de SMA, une valeur par bougie attendue"
    )]
    RegimeLength { candles: usize, sma: usize },
}

// ---------------------------------------------------------------------------------------------
// Déclaration des indicateurs et signaux
// ---------------------------------------------------------------------------------------------

/// Type d'indicateur demandé par une stratégie.
///
/// L'entrée est implicite : le close pour `Sma`, `Ema` et `Rsi` ; la bougie pour `Atr` et `Vwap`
/// (VWAP ancré sur la session journalière UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndicatorKind {
    Sma { period: usize },
    Ema { period: usize },
    Rsi { period: usize },
    Atr { period: usize },
    Vwap,
}

/// Un indicateur déclaré : `name` sert à le retrouver par [`BarContext::ind`], `tf` est le
/// timeframe de ses bougies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndicatorSpec {
    pub name: &'static str,
    pub tf: Timeframe,
    pub kind: IndicatorKind,
}

/// Type d'un ordre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OrderKind {
    /// Au marché, rempli à l'ouverture de la bougie suivante (taker).
    Market,
    /// Limite au prix `price`, valable `ttl_bars` bougies 1m (maker).
    Limit { price: f64, ttl_bars: u32 },
}

/// Taille d'un ordre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    /// Fraction de l'equity (0,25 pour 25 %).
    PctEquity(f64),
    /// Fraction de l'equity risquée jusqu'au stop ; exige un stop.
    RiskPct(f64),
    /// Montant en monnaie de cotation.
    Quote(f64),
}

/// Intention d'une stratégie : le simulateur décide des fills, la stratégie ne touche jamais aux ordres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Signal {
    /// Ouvre ou renforce une position longue.
    EnterLong {
        size: Size,
        kind: OrderKind,
        stop: Option<f64>,
        take_profit: Option<f64>,
        tag: &'static str,
    },
    /// Sort TOUTE la position.
    ExitLong { kind: OrderKind, tag: &'static str },
    /// Pose ou déplace le stop de la position agrégée.
    MoveStop { stop: f64 },
    /// Annule TOUS les ordres en attente.
    CancelPending,
    /// Limit autonome, achat ou vente partielle, qui coexiste avec les autres ordres en attente
    /// (docs/03 §15.3). `slot` identifie le niveau pour la stratégie (par exemple l'index de grille)
    /// et est unique parmi les ordres en attente. Pas de stop attaché : `Size::RiskPct` est refusé
    /// (rejet compté par le simulateur, S6) ; `PctEquity` ou `Quote` seulement.
    PlaceLimit {
        side: Side,
        price: f64,
        size: Size,
        ttl_bars: u32,
        slot: u16,
        tag: &'static str,
    },
    /// Annule l'ordre en attente du `slot` donné.
    CancelSlot { slot: u16 },
}

/// Position ouverte (agrégée).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub qty: f64,
    pub avg_price: f64,
    pub stop: Option<f64>,
    pub take_profit: Option<f64>,
    pub opened_at_ms: Ts,
}

/// Ordre en attente, `Copy` (pas de `String`). `slot` vaut 0 pour les ordres issus de `EnterLong`
/// et `ExitLong`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingOrder {
    pub side: Side,
    pub kind: OrderKind,
    pub qty: f64,
    pub slot: u16,
    pub tag: &'static str,
    pub placed_at_ms: Ts,
    pub ttl_left: u32,
}

// ---------------------------------------------------------------------------------------------
// Vues et contexte
// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// Vues et contexte : sous-module privé `context` (champs invisibles des stratégies)
// ---------------------------------------------------------------------------------------------

mod context;
pub use context::{BarContext, BarState, REGIME_SMA_PERIOD, REGIME_TF, RegimeView, TfView};

// ---------------------------------------------------------------------------------------------
// Trait Strategy
// ---------------------------------------------------------------------------------------------

/// Tag de sortie réservé aux sorties temporelles : `trades.csv` en fait un `exit_type` `time_stop`.
pub const TIME_STOP_TAG: &str = "time_stop";

/// Entonnoir d'une stratégie : signaux d'entrée bruts et signaux filtrés par motif, déclarés par la
/// stratégie elle-même (le moteur ne voit que les signaux émis).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyFunnel {
    /// Signaux d'entrée vus avant tout filtre de la stratégie.
    pub raw_entry_signals: u64,
    /// Signaux écartés par la stratégie, par motif (identifiants stables), dans un ordre stable.
    pub filtered: Vec<(&'static str, u64)>,
}

/// Une stratégie. Le trait est volontairement neutre (`params_json`, pas de type associé).
///
/// `Send` interdit `Rc` et `RefCell` dans l'état d'une stratégie : garde-fou bon marché pour la grille
/// parallèle du jalon 1.5.
pub trait Strategy: Send {
    /// Identifiant du registre, par exemple `"sma_cross"`.
    fn id(&self) -> &'static str;

    /// Version de la logique, par exemple `"1.0.0"` ; entre dans le `config_hash`.
    fn version(&self) -> &'static str;

    /// Paramètres en JSON (clés triées par le hash) ; entre dans le `config_hash`.
    fn params_json(&self) -> serde_json::Value;

    /// Timeframes utilisés, par exemple `[H1, M15]`.
    fn timeframes(&self) -> &[Timeframe];

    /// Indicateurs précalculés par le moteur, dans un ordre stable (celui des colonnes de [`TfView`]).
    fn indicators(&self) -> Vec<IndicatorSpec>;

    /// Bougies de warmup, en bougies du PLUS GRAND timeframe déclaré.
    fn warmup_bars(&self) -> usize;

    /// Appelée uniquement sur les bougies 1m où un timeframe déclaré vient de clôturer. `out` est
    /// réutilisé par le moteur (`clear()` avant l'appel) : aucune allocation par bougie.
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>);

    /// Entonnoir de la stratégie, lu une fois après la boucle (peut allouer). `None` si elle ne
    /// déclare rien.
    fn funnel(&self) -> Option<StrategyFunnel> {
        None
    }

    /// Vrai si la stratégie lit [`BarContext::regime`] (filtre BTC 1d / SMA200, docs/04 §4.2). Le
    /// moteur portefeuille ne charge alors la série de régime que pour elle, l'inscrit dans le
    /// `config_hash` et refuse un dataset qui n'en a pas. Défaut `false` : aucune stratégie du
    /// jalon 1 n'en a besoin.
    fn uses_regime(&self) -> bool {
        false
    }

    /// TF dont la clôture déclenche une entrée (docs S10 §4.5). Par défaut le plus grand TF déclaré ;
    /// une stratégie dont le grand TF n'est qu'un filtre la surcharge. Il fixe le TTL de l'entrée du
    /// profil maker et entre dans le `config_hash` des runs s10 seulement (`costs.entry.decision_tf`) :
    /// aucun hash hérité ne le lit.
    fn decision_tf(&self) -> Timeframe {
        self.timeframes()
            .iter()
            .copied()
            .max()
            .unwrap_or(Timeframe::M1)
    }
}

/// Vérifie la déclaration d'une stratégie ; appelée par `registry::build`.
///
/// Timeframes non vides et distincts, chaque indicateur sur un timeframe déclaré, noms distincts par
/// timeframe, périodes dans `1..=MAX_INDICATOR_PERIOD`.
pub fn validate_declaration(strategy: &dyn Strategy) -> Result<(), StrategyError> {
    let timeframes = strategy.timeframes();
    if timeframes.is_empty() {
        return Err(StrategyError::NoTimeframe { id: strategy.id() });
    }
    for (i, tf) in timeframes.iter().enumerate() {
        if timeframes[..i].contains(tf) {
            return Err(StrategyError::DuplicateTimeframe { tf: *tf });
        }
    }
    let specs = strategy.indicators();
    for (i, spec) in specs.iter().enumerate() {
        if !timeframes.contains(&spec.tf) {
            return Err(StrategyError::IndicatorOutsideTimeframes {
                name: spec.name,
                tf: spec.tf,
            });
        }
        if specs[..i]
            .iter()
            .any(|other| other.tf == spec.tf && other.name == spec.name)
        {
            return Err(StrategyError::DuplicateIndicator {
                tf: spec.tf,
                name: spec.name,
            });
        }
        if let IndicatorKind::Sma { period }
        | IndicatorKind::Ema { period }
        | IndicatorKind::Rsi { period }
        | IndicatorKind::Atr { period } = spec.kind
            && !(1..=MAX_INDICATOR_PERIOD).contains(&period)
        {
            return Err(StrategyError::InvalidPeriod {
                name: spec.name,
                period,
                max: MAX_INDICATOR_PERIOD,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Garde-fous de compilation ---

    fn assert_copy<T: Copy>() {}
    fn assert_send<T: Send>() {}

    #[test]
    fn les_types_cles_sont_copy_et_send() {
        assert_copy::<Signal>();
        assert_copy::<PendingOrder>();
        assert_copy::<Position>();
        assert_copy::<BarState>();
        assert_send::<Box<dyn Strategy>>();
    }

    // --- validate_declaration ---

    /// Stratégie de test avec une déclaration configurable.
    struct Declared {
        timeframes: Vec<Timeframe>,
        specs: Vec<IndicatorSpec>,
    }

    impl Strategy for Declared {
        fn id(&self) -> &'static str {
            "declared"
        }
        fn version(&self) -> &'static str {
            "0.0.0"
        }
        fn params_json(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn timeframes(&self) -> &[Timeframe] {
            &self.timeframes
        }
        fn indicators(&self) -> Vec<IndicatorSpec> {
            self.specs.clone()
        }
        fn warmup_bars(&self) -> usize {
            1
        }
        fn on_bar(&mut self, _ctx: &BarContext<'_>, _out: &mut Vec<Signal>) {}
    }

    fn spec(name: &'static str, tf: Timeframe, kind: IndicatorKind) -> IndicatorSpec {
        IndicatorSpec { name, tf, kind }
    }

    #[test]
    fn la_declaration_est_validee() {
        let ok = Declared {
            timeframes: vec![Timeframe::H1],
            specs: vec![
                spec("a", Timeframe::H1, IndicatorKind::Sma { period: 5 }),
                spec("v", Timeframe::H1, IndicatorKind::Vwap),
            ],
        };
        assert!(validate_declaration(&ok).is_ok());

        let none = Declared {
            timeframes: vec![],
            specs: vec![],
        };
        assert!(matches!(
            validate_declaration(&none),
            Err(StrategyError::NoTimeframe { .. })
        ));

        let twice = Declared {
            timeframes: vec![Timeframe::H1, Timeframe::H1],
            specs: vec![],
        };
        assert!(matches!(
            validate_declaration(&twice),
            Err(StrategyError::DuplicateTimeframe { .. })
        ));

        let outside = Declared {
            timeframes: vec![Timeframe::H1],
            specs: vec![spec("a", Timeframe::D1, IndicatorKind::Sma { period: 5 })],
        };
        assert!(matches!(
            validate_declaration(&outside),
            Err(StrategyError::IndicatorOutsideTimeframes { name: "a", .. })
        ));

        let zero = Declared {
            timeframes: vec![Timeframe::H1],
            specs: vec![spec("a", Timeframe::H1, IndicatorKind::Rsi { period: 0 })],
        };
        assert!(matches!(
            validate_declaration(&zero),
            Err(StrategyError::InvalidPeriod { period: 0, .. })
        ));

        let large = Declared {
            timeframes: vec![Timeframe::H1],
            specs: vec![spec(
                "a",
                Timeframe::H1,
                IndicatorKind::Atr {
                    period: MAX_INDICATOR_PERIOD + 1,
                },
            )],
        };
        assert!(matches!(
            validate_declaration(&large),
            Err(StrategyError::InvalidPeriod { .. })
        ));

        let duplicate = Declared {
            timeframes: vec![Timeframe::H1],
            specs: vec![
                spec("a", Timeframe::H1, IndicatorKind::Sma { period: 5 }),
                spec("a", Timeframe::H1, IndicatorKind::Ema { period: 5 }),
            ],
        };
        assert!(matches!(
            validate_declaration(&duplicate),
            Err(StrategyError::DuplicateIndicator { name: "a", .. })
        ));
    }

    #[test]
    fn le_tf_de_decision_par_defaut_est_le_plus_grand_tf_declare() {
        let declared = |timeframes| Declared {
            timeframes,
            specs: vec![],
        };
        assert_eq!(declared(vec![Timeframe::H1]).decision_tf(), Timeframe::H1);
        // L'ordre de déclaration ne compte pas.
        assert_eq!(
            declared(vec![Timeframe::D1, Timeframe::M15, Timeframe::H4]).decision_tf(),
            Timeframe::D1
        );
        assert_eq!(
            declared(vec![Timeframe::M15, Timeframe::H1]).decision_tf(),
            Timeframe::H1
        );
    }

    #[test]
    fn un_meme_nom_sur_deux_timeframes_est_permis() {
        let ok = Declared {
            timeframes: vec![Timeframe::H1, Timeframe::D1],
            specs: vec![
                spec("a", Timeframe::H1, IndicatorKind::Sma { period: 5 }),
                spec("a", Timeframe::D1, IndicatorKind::Sma { period: 5 }),
            ],
        };
        assert!(validate_declaration(&ok).is_ok());
    }
}
