//! Stratégie de grille à N niveaux, STRATÉGIE DE TEST seulement (docs/03 §11 point 6 : aucune stratégie
//! grid n'est écrite au jalon 1). Elle sert aux goldens de `tests/golden.rs` et au test d'allocation.
//!
//! M1, warmup 0, aucun indicateur. Le niveau `k` (de 1 à N) achète à `B_k` et revend à `S_k = B_{k-1}`.
//! L'état « l'ordre a disparu de la file » vaut « rempli » parce que la stratégie n'annule jamais, que
//! le TTL n'est pas atteint et que les achats ne sont pas refusés : les tests le vérifient.

use std::sync::{Arc, Mutex};

use tv4::model::{Side, Timeframe};
use tv4::strategy::{BarContext, IndicatorSpec, Signal, Size, Strategy};

/// Prix de départ de la grille.
pub const CENTRE: f64 = 100.0;
/// Nombre de niveaux.
pub const NIVEAUX: usize = 4;
/// Montant acheté à chaque niveau.
pub const QUOTE: f64 = 20.0;
/// TTL des ordres de grille, en 1m : jamais atteint dans les tests.
const TTL: u32 = 1_000_000;

#[derive(Clone, Copy, Default)]
struct Niveau {
    achat_pose: bool,
    vente_posee: bool,
    tenu: bool,
}

/// Compteurs lisibles après le run (la stratégie est consommée par `run_cell`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Suivi {
    /// Cycles complets (achat puis vente remplis).
    pub cycles: u64,
    /// Niveaux dont l'achat est rempli et la vente pas encore.
    pub tenus: usize,
}

pub struct GrilleTest {
    espacement: f64,
    /// `achats[k]` vaut `B_k` pour `k` de 0 à N (`B_0` = centre).
    achats: [f64; NIVEAUX + 1],
    /// Quantité vendue du niveau `k` (indice `k`, l'indice 0 est inutilisé).
    quantites: [f64; NIVEAUX + 1],
    niveaux: [Niveau; NIVEAUX + 1],
    suivi: Arc<Mutex<Suivi>>,
}

impl GrilleTest {
    /// Grille d'espacement relatif `espacement` (par exemple 0,005). Le second résultat est le suivi
    /// partagé, à lire après le run.
    pub fn new(espacement: f64) -> (GrilleTest, Arc<Mutex<Suivi>>) {
        let mut achats = [CENTRE; NIVEAUX + 1];
        let mut quantites = [0.0; NIVEAUX + 1];
        let mut b = CENTRE;
        for k in 1..=NIVEAUX {
            b /= 1.0 + espacement;
            achats[k] = (b * 10000.0 + 0.5).floor() / 10000.0;
            quantites[k] = (QUOTE / achats[k] * 1e8 + 1e-6).floor() / 1e8;
        }
        let suivi = Arc::new(Mutex::new(Suivi::default()));
        let grille = GrilleTest {
            espacement,
            achats,
            quantites,
            niveaux: [Niveau::default(); NIVEAUX + 1],
            suivi: Arc::clone(&suivi),
        };
        (grille, suivi)
    }

    /// `B_k`, prix d'achat du niveau `k` (de 0 à N).
    pub fn prix_achat(&self, k: usize) -> f64 {
        self.achats[k]
    }
}

impl Strategy for GrilleTest {
    fn id(&self) -> &'static str {
        "grille_test"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({
            "centre": CENTRE,
            "niveaux": NIVEAUX,
            "espacement": self.espacement,
            "quote": QUOTE,
        })
    }
    fn timeframes(&self) -> &[Timeframe] {
        &[Timeframe::M1]
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        Vec::new()
    }
    fn warmup_bars(&self) -> usize {
        0
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        let Some(derniere) = ctx.last(Timeframe::M1) else {
            return;
        };
        let close = derniere.close;
        let s = self.espacement;
        for k in 1..=NIVEAUX {
            let achat_k = self.achats[k];
            let vente_k = self.achats[k - 1];
            let slot_achat = k as u16;
            let slot_vente = 1000 + k as u16;
            let niveau = &mut self.niveaux[k];
            // 1. l'achat a disparu de la file : il est rempli.
            if niveau.achat_pose && ctx.pending_slot(slot_achat).is_none() {
                niveau.tenu = true;
                niveau.achat_pose = false;
                if let Ok(mut suivi) = self.suivi.lock() {
                    suivi.tenus += 1;
                }
            }
            // 2. la vente a disparu de la file : le cycle est complet.
            if niveau.vente_posee && ctx.pending_slot(slot_vente).is_none() {
                niveau.tenu = false;
                niveau.vente_posee = false;
                if let Ok(mut suivi) = self.suivi.lock() {
                    suivi.cycles += 1;
                    suivi.tenus -= 1;
                }
            }
            // 3. niveau libre et prix assez au-dessus de l'achat : on pose l'achat.
            if !niveau.tenu && !niveau.achat_pose && close > achat_k * (1.0 + s / 2.0) {
                out.push(Signal::PlaceLimit {
                    side: Side::Buy,
                    price: achat_k,
                    size: Size::Quote(QUOTE),
                    ttl_bars: TTL,
                    slot: slot_achat,
                    tag: "grille_achat",
                });
                niveau.achat_pose = true;
            }
            // 4. niveau tenu et prix assez en dessous de la vente : on pose la vente.
            if niveau.tenu && !niveau.vente_posee && close < vente_k * (1.0 - s / 2.0) {
                out.push(Signal::PlaceLimit {
                    side: Side::Sell,
                    price: vente_k,
                    size: Size::Quote(self.quantites[k] * vente_k),
                    ttl_bars: TTL,
                    slot: slot_vente,
                    tag: "grille_vente",
                });
                niveau.vente_posee = true;
            }
        }
    }
}
