//! Détecteurs de look-ahead (docs/03-architecture.md §7.5, S5 passe C). Tous tournent dans
//! `cargo test`, donc en CI.
//!
//! * Espion I2 : un dataset **synthétique** où chaque prix vaut l'instant de clôture de sa 1m. Une
//!   stratégie espionne vérifie, à chaque appel, que rien de ce qu'elle voit ne dépasse `now_ms` et que
//!   le nombre de bougies visibles par TF est celui que donnent les horodatages (recherche dichotomique
//!   indépendante de `closed_count`).
//! * Perturbation I4 : on modifie le high et le low d'une 1m future et on exige que tout ce que la
//!   stratégie a vu jusque-là, ses signaux et ses fills soient identiques au bit. La stratégie canari
//!   hache tout le visible, donc la moindre fuite change son empreinte.
//! * Mutant : la formule fautive `open_time_tf <= open_time_1m` est injectée par
//!   `Mtf::build_with_rule` ; les détecteurs doivent rougir, et ce test le vérifie.
//!
//! Toutes les données sont synthétiques (générateur de `tests/common`) ou, pour un contrôle pur de
//! `mtf`, les horodatages de la fixture réelle servant de prix.

mod common;

use std::sync::{Arc, Mutex};

use common::{SplitMix64, T0, fixture_1m, okx_fees, slippage, synthetic_candles};
use proptest::prelude::*;
use tv4::engine::{CellConfig, CellResult, EngineError, NoopObserver, run_cell, run_cell_mtf};
use tv4::indicators::{Atr, Sma, batch};
use tv4::model::{Candle, Timeframe, Ts};
use tv4::mtf::{ClosedRule, Mtf, is_closed};
use tv4::sim::{ExitReason, SimRules};
use tv4::strategy::{
    BarContext, IndicatorKind, IndicatorSpec, OrderKind, Signal, Size, Strategy, StrategyError,
};

const MINUTE: Ts = 60_000;

fn config() -> CellConfig<'static> {
    CellConfig {
        symbol: "TEST-USD",
        fees: okx_fees(),
        slippage: slippage(10.0),
        initial_cash: 1_000.0,
        sim_rules: SimRules::default(),
    }
}

// ---------------------------------------------------------------------------------------------
// Aides indépendantes de `mtf`
// ---------------------------------------------------------------------------------------------

/// Débuts de seaux `tf` distincts, calculés par division euclidienne des horodatages (sans `mtf`).
fn seaux(m1: &[Candle], tf: Timeframe) -> Vec<Ts> {
    let mut out: Vec<Ts> = Vec::new();
    for c in m1 {
        let b = c.open_time_ms - c.open_time_ms.rem_euclid(tf.ms());
        if out.last() != Some(&b) {
            out.push(b);
        }
    }
    out
}

/// Nombre de bougies `tf` clôturées à l'instant `now_ms` : recherche dichotomique sur les débuts de
/// seaux, jamais `closed_count`.
fn n_attendu(seaux: &[Ts], tf: Timeframe, now_ms: Ts) -> usize {
    seaux.partition_point(|&b| b + tf.ms() <= now_ms)
}

/// Contrôle pur de `mtf`, sans moteur : pour tout `i` et tout TF, `closed_counts(tf)[i]` égale
/// `n_attendu` et le `high` de chaque bougie visible est `<= now_i`. Rend les violations.
fn espion_pur(mtf: &Mtf<'_>, tfs: &[Timeframe]) -> Vec<String> {
    let m1 = mtf.m1();
    let mut violations = Vec::new();
    for &tf in tfs {
        let bornes = seaux(m1, tf);
        let (Some(candles), Some(counts)) = (mtf.candles(tf), mtf.closed_counts(tf)) else {
            violations.push(format!("{tf} absent du Mtf"));
            continue;
        };
        for (i, (c1, &compte)) in m1.iter().zip(counts).enumerate() {
            let now_ms = c1.open_time_ms + MINUTE;
            let attendu = n_attendu(&bornes, tf, now_ms);
            if compte != attendu {
                violations.push(format!(
                    "{tf} à la 1m {i} : {compte} bougies visibles, {attendu} attendues"
                ));
            }
            if let Some(trop_haut) = candles
                .get(..compte)
                .and_then(|visibles| visibles.iter().find(|b| b.high > now_ms as f64))
            {
                violations.push(format!(
                    "{tf} à la 1m {i} : high {} > now {now_ms} (bougie {})",
                    trop_haut.high, trop_haut.open_time_ms
                ));
            }
        }
    }
    violations
}

/// Remplace les prix par l'instant de clôture de la 1m (`open_time + 60 000`, en `f64`) : la valeur
/// d'un prix dit ainsi quand il est devenu connaissable. Volume 1, `trades` absent.
fn horodater(m1: &[Candle]) -> Vec<Candle> {
    m1.iter()
        .map(|c| {
            let t = (c.open_time_ms + MINUTE) as f64;
            Candle {
                open_time_ms: c.open_time_ms,
                open: t,
                high: t,
                low: t,
                close: t,
                volume: 1.0,
                trades: None,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Espion I2
// ---------------------------------------------------------------------------------------------

/// Les huit TF (revue S5-R3) : le 1m déclaré avec un TF supérieur (`just_closed` contient alors M1 à
/// chaque `i`) et les TF 30m, 12h et 1d passent aussi par le moteur, pas seulement par l'espion pur.
const TF_ESPION: [Timeframe; 8] = Timeframe::ALL;

/// Plus grand TF de l'espion : celui qui fixe le départ de la fenêtre (warmup d'une bougie).
const TF_ESPION_MAX: Timeframe = Timeframe::D1;

/// Stratégie espionne : ne signale rien, vérifie ce qu'elle voit à chaque appel.
struct Espion {
    /// Débuts de seaux par TF de `TF_ESPION`, calculés une fois par le test.
    seaux: Vec<Vec<Ts>>,
    /// `open_time` de toutes les 1m du dataset.
    ouvertures: Vec<Ts>,
    violations: Arc<Mutex<Vec<String>>>,
    appels: Arc<Mutex<Vec<Ts>>>,
}

impl Espion {
    fn new(m1: &[Candle]) -> Espion {
        Espion {
            seaux: TF_ESPION.iter().map(|&tf| seaux(m1, tf)).collect(),
            ouvertures: m1.iter().map(|c| c.open_time_ms).collect(),
            violations: Arc::new(Mutex::new(Vec::new())),
            appels: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

const NOMS_SMA1: &str = "sma1";

impl Strategy for Espion {
    fn id(&self) -> &'static str {
        "espion"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &TF_ESPION
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        TF_ESPION
            .iter()
            .map(|&tf| IndicatorSpec {
                name: NOMS_SMA1,
                tf,
                kind: IndicatorKind::Sma { period: 1 },
            })
            .collect()
    }
    fn warmup_bars(&self) -> usize {
        1
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, _out: &mut Vec<Signal>) {
        let now_ms = ctx.now_ms();
        let mut fautes: Vec<String> = Vec::new();
        for (k, &tf) in TF_ESPION.iter().enumerate() {
            let visibles = ctx.candles(tf);
            let attendu = n_attendu(&self.seaux[k], tf, now_ms);
            if visibles.len() != attendu {
                fautes.push(format!(
                    "{tf} à {now_ms} : {} bougies visibles, {attendu} attendues",
                    visibles.len()
                ));
            }
            if let Some(b) = visibles.iter().find(|b| b.high > now_ms as f64) {
                fautes.push(format!("{tf} à {now_ms} : high {} futur", b.high));
            }
            let colonne = ctx.ind(tf, NOMS_SMA1);
            if colonne.len() != visibles.len() {
                fautes.push(format!(
                    "{tf} à {now_ms} : colonne de longueur {}",
                    colonne.len()
                ));
            }
            if let Some(v) = colonne
                .iter()
                .find(|v| !v.is_finite() || **v > now_ms as f64)
            {
                fautes.push(format!("{tf} à {now_ms} : SMA {v} futur ou non fini"));
            }
            // Dernière bougie visible : son close est l'instant de clôture de la dernière 1m présente
            // de son seau.
            if let (Some(derniere), Some(&debut)) =
                (visibles.last(), self.seaux[k].get(attendu.wrapping_sub(1)))
            {
                let fin_seau = debut + tf.ms();
                let n_avant_fin = self.ouvertures.partition_point(|&o| o < fin_seau);
                let derniere_1m = n_avant_fin
                    .checked_sub(1)
                    .and_then(|idx| self.ouvertures.get(idx));
                match derniere_1m {
                    Some(&o) if derniere.close == (o + MINUTE) as f64 => {}
                    autre => fautes.push(format!(
                        "{tf} à {now_ms} : close {} au lieu de la clôture de la 1m {autre:?}",
                        derniere.close
                    )),
                }
            }
        }
        if !fautes.is_empty() {
            self.violations.lock().unwrap().extend(fautes);
        }
        self.appels.lock().unwrap().push(now_ms);
    }
}

/// Dataset synthétique horodaté : 3 jours de 1m (4 320 bougies) avec des trous (`gap_prob` 0,1).
fn dataset_horodate() -> Vec<Candle> {
    horodater(&synthetic_candles(3 * 1440, 2024, T0, 0.1))
}

#[test]
fn espion_i2_aucune_donnee_future_visible() {
    let m1 = dataset_horodate();
    let espion = Espion::new(&m1);
    let violations = Arc::clone(&espion.violations);
    let appels = Arc::clone(&espion.appels);
    let seaux_max = espion.seaux[TF_ESPION.len() - 1].clone();
    let seaux_par_tf = espion.seaux.clone();

    let result = run_cell(&m1, Box::new(espion), &config()).expect("run de l'espion");

    let violations = violations.lock().unwrap();
    assert!(violations.is_empty(), "violations : {violations:#?}");

    // Départ recalculé par horodatages : la première 1m `i` telle que 1 bougie du plus grand TF (1d) est
    // clôturée à la fin de la 1m `i - 1` (warmup 1).
    let now = |i: usize| m1[i].open_time_ms + MINUTE;
    let start = (1..m1.len())
        .find(|&i| n_attendu(&seaux_max, TF_ESPION_MAX, now(i - 1)) >= 1)
        .expect("une 1d se clôture dans le dataset");
    assert_eq!(result.start_index, start);

    // Nombre d'appels : les 1m de la fenêtre où au moins une des bougies déclarées vient de se clôturer.
    let attendus: Vec<Ts> = (start..m1.len())
        .filter(|&i| {
            TF_ESPION.iter().enumerate().any(|(k, &tf)| {
                n_attendu(&seaux_par_tf[k], tf, now(i))
                    > n_attendu(&seaux_par_tf[k], tf, now(i - 1))
            })
        })
        .map(now)
        .collect();
    assert!(!attendus.is_empty());
    assert_eq!(*appels.lock().unwrap(), attendus);
}

#[test]
fn espion_i2_controle_pur_de_mtf() {
    let tous = Timeframe::ALL;
    // Dataset synthétique horodaté.
    let synthetique = dataset_horodate();
    let mtf = Mtf::build(&synthetique, &tous).expect("mtf synthétique");
    assert_eq!(espion_pur(&mtf, &tous), Vec::<String>::new());

    // Fixture réelle : les horodatages sont réels, les prix sont remplacés par l'instant de clôture.
    let fixture = horodater(&fixture_1m());
    let mtf = Mtf::build(&fixture, &tous).expect("mtf de la fixture");
    assert_eq!(espion_pur(&mtf, &tous), Vec::<String>::new());
}

// ---------------------------------------------------------------------------------------------
// Canari et perturbation I4
// ---------------------------------------------------------------------------------------------

const TF_CANARI: [Timeframe; 2] = [Timeframe::M5, Timeframe::H1];

/// FNV-1a 64 bits, mot par mot, sans allocation.
fn fnv(h: &mut u64, mot: u64) {
    for octet in mot.to_le_bytes() {
        *h ^= u64::from(octet);
        *h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
}

const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;

fn hacher_bougie(h: &mut u64, c: &Candle) {
    fnv(h, c.open_time_ms as u64);
    fnv(h, c.open.to_bits());
    fnv(h, c.high.to_bits());
    fnv(h, c.low.to_bits());
    fnv(h, c.close.to_bits());
    fnv(h, c.volume.to_bits());
    fnv(h, c.trades.map_or(u64::MAX, u64::from));
}

/// Stratégie canari : hache tout ce qu'elle voit et en tire ses signaux, de sorte qu'une seule
/// valeur future visible change le journal et très probablement les fills.
struct Canari {
    journal: JournalPartage,
}

/// Journal du canari partagé avec le test : `(now_ms, empreinte)` à chaque appel.
type JournalPartage = Arc<Mutex<Vec<(Ts, u64)>>>;

impl Canari {
    fn new() -> (Canari, JournalPartage) {
        let journal = Arc::new(Mutex::new(Vec::new()));
        (
            Canari {
                journal: Arc::clone(&journal),
            },
            journal,
        )
    }
}

impl Strategy for Canari {
    fn id(&self) -> &'static str {
        "canari"
    }
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn params_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn timeframes(&self) -> &[Timeframe] {
        &TF_CANARI
    }
    fn indicators(&self) -> Vec<IndicatorSpec> {
        TF_CANARI
            .iter()
            .flat_map(|&tf| {
                [
                    IndicatorSpec {
                        name: "sma3",
                        tf,
                        kind: IndicatorKind::Sma { period: 3 },
                    },
                    IndicatorSpec {
                        name: "atr3",
                        tf,
                        kind: IndicatorKind::Atr { period: 3 },
                    },
                ]
            })
            .collect()
    }
    /// 3 bougies H1 : la SMA 3 et l'ATR 3 sont définies sur la dernière (la spécification disait 2, ce
    /// qui est refusé au démarrage par le contrôle d'indicateur en warmup).
    fn warmup_bars(&self) -> usize {
        3
    }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        let mut h = FNV_OFFSET;
        for &tf in &TF_CANARI {
            let visibles = ctx.candles(tf);
            fnv(&mut h, visibles.len() as u64);
            for c in visibles {
                hacher_bougie(&mut h, c);
            }
            for index in 0..2 {
                let colonne = ctx.ind_at(tf, index);
                fnv(&mut h, colonne.len() as u64);
                for v in colonne {
                    fnv(&mut h, v.to_bits());
                }
            }
        }
        self.journal.lock().unwrap().push((ctx.now_ms(), h));
        let a_plat = ctx.position().is_none();
        if h & 1 != 0 && a_plat {
            if let Some(derniere) = ctx.last(Timeframe::M5) {
                out.push(Signal::EnterLong {
                    size: Size::RiskPct(0.01),
                    kind: OrderKind::Market,
                    stop: Some(derniere.close * 0.99),
                    take_profit: None,
                    tag: "canari",
                });
            }
        } else if h & 2 != 0 && !a_plat {
            out.push(Signal::ExitLong {
                kind: OrderKind::Market,
                tag: "canari",
            });
        }
    }
}

/// Fills d'un run, `(ms, sens, prix, quantité)` au bit, jusqu'à `limite_ms` inclus. La clôture forcée
/// de fin de données n'est pas un fill de la stratégie : elle est exclue.
fn fills(result: &CellResult, limite_ms: Ts) -> Vec<(Ts, char, u64, u64)> {
    let mut out = Vec::new();
    for t in &result.trades {
        if t.entry_ms <= limite_ms {
            out.push((t.entry_ms, 'A', t.entry_price.to_bits(), t.qty.to_bits()));
        }
        if t.exit_reason == ExitReason::Signal && t.exit_ms <= limite_ms {
            out.push((t.exit_ms, 'V', t.exit_price.to_bits(), t.qty.to_bits()));
        }
    }
    out.sort_by_key(|f| f.0);
    out
}

/// Journal du canari (instant de l'appel, empreinte de ce qu'il voit) et résultat du run.
type RunCanari = (Vec<(Ts, u64)>, CellResult);

/// Rejoue le canari : journal et résultat.
fn jouer(candles: &[Candle]) -> RunCanari {
    let (canari, journal) = Canari::new();
    let result = run_cell(candles, Box::new(canari), &config()).expect("run du canari");
    let journal = journal.lock().unwrap().clone();
    (journal, result)
}

/// Perturbe le high (`× (1 + d)`) et le low (`× (1 - d / 2)`) de la 1m `j` : la bougie reste valide.
fn perturber(candles: &[Candle], j: usize, d: f64) -> Vec<Candle> {
    let mut out = candles.to_vec();
    out[j].high *= 1.0 + d;
    out[j].low *= 1.0 - d / 2.0;
    out
}

/// Vérifie que ce que le canari a fait jusqu'à `open_time[i]` + 1 minute est identique dans les deux
/// runs : journal des appels (donc tout le visible) pour `now_ms <= limite_journal_ms`, fills jusqu'à
/// `limite_fills_ms`.
fn assert_passe_identique(
    (journal_a, result_a): &RunCanari,
    (journal_b, result_b): &RunCanari,
    limite_journal_ms: Ts,
    limite_fills_ms: Ts,
) -> Result<(), TestCaseError> {
    let prefixe = |journal: &[(Ts, u64)]| -> Vec<(Ts, u64)> {
        journal
            .iter()
            .copied()
            .filter(|(now_ms, _)| *now_ms <= limite_journal_ms)
            .collect()
    };
    prop_assert_eq!(prefixe(journal_a), prefixe(journal_b), "journaux du canari");
    prop_assert_eq!(
        fills(result_a, limite_fills_ms),
        fills(result_b, limite_fills_ms),
        "fills"
    );
    Ok(())
}

/// Indices `j` dont l'`open_time` est dans `(open_i, open_i + ms]`.
fn candidats_j(candles: &[Candle], i: usize, ms: Ts) -> Vec<usize> {
    let open_i = candles[i].open_time_ms;
    (i + 1..candles.len())
        .take_while(|&j| candles[j].open_time_ms <= open_i + ms)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1000, .. ProptestConfig::default() })]

    /// Perturbation ciblée : `j` est tiré dans `(i, i + ms(H1)]`, là où une clôture prématurée d'une
    /// bougie H1 ou M5 ferait fuiter la 1m `j` vers la décision prise à la 1m `i`.
    #[test]
    fn perturbation_i4_ciblee(
        graine in any::<u64>(),
        fraction_i in 0.0f64..1.0,
        fraction_j in 0.0f64..1.0,
        amplitude in 0.01f64..0.5,
    ) {
        // 720 1m synthétiques (12 h), trous (`gap_prob` 0,05).
        let candles = synthetic_candles(720, graine, T0, 0.05);
        let original = jouer(&candles);
        let start = original.1.start_index;
        let n = candles.len();
        prop_assume!(start + 1 < n - 1);
        let i = start + ((fraction_i * (n - 1 - start) as f64) as usize).min(n - 2 - start);
        let candidats = candidats_j(&candles, i, Timeframe::H1.ms());
        prop_assume!(!candidats.is_empty());
        let j = candidats[((fraction_j * candidats.len() as f64) as usize).min(candidats.len() - 1)];
        let perturbe = jouer(&perturber(&candles, j, amplitude));
        assert_passe_identique(
            &original,
            &perturbe,
            candles[i].open_time_ms + MINUTE,
            candles[i + 1].open_time_ms,
        )?;
    }
}

#[test]
fn perturbation_k_dernieres_bougies() {
    let candles = synthetic_candles(720, 7, T0, 0.05);
    let n = candles.len();
    let original = jouer(&candles);
    for k in [1usize, 5, 60, 240] {
        let mut perturbees = candles.clone();
        for j in n - k..n {
            perturbees[j] = perturber(&perturbees, j, 0.3)[j];
        }
        let perturbe = jouer(&perturbees);
        let i = n - k - 1;
        assert!(i >= original.1.start_index, "k = {k} : i dans la fenêtre");
        assert_passe_identique(
            &original,
            &perturbe,
            candles[i].open_time_ms + MINUTE,
            candles[i + 1].open_time_ms,
        )
        .unwrap_or_else(|e| panic!("k = {k} : {e}"));
        // Sensibilité : à partir de 60 minutes perturbées, des bougies M5 perturbées sont clôturées et
        // visibles avant la fin ; le journal complet doit donc changer, sinon la perturbation ne
        // prouverait rien.
        if k >= 60 {
            assert_ne!(
                original.0, perturbe.0,
                "k = {k} : la perturbation doit être visible plus tard"
            );
        }
    }
}

#[test]
fn perturbation_trous_injectes() {
    for graine in 0..64u64 {
        let mut rng = SplitMix64(0x7405_0000 + graine);
        let candles = synthetic_candles(720, 1_000 + graine, T0, 0.05);
        let n = candles.len();
        let original = jouer(&candles);
        let start = original.1.start_index;
        let i = start + (rng.next_u64() as usize) % (n - 2 - start);
        // 1 à 30 minutes retirées, tirées après `i`.
        let combien = (1 + (rng.next_u64() % 30) as usize).min(n - 1 - i);
        let mut retirees: Vec<usize> = Vec::new();
        while retirees.len() < combien {
            let idx = i + 1 + (rng.next_u64() as usize) % (n - 1 - i);
            if !retirees.contains(&idx) {
                retirees.push(idx);
            }
        }
        let troue: Vec<Candle> = candles
            .iter()
            .enumerate()
            .filter(|(idx, _)| !retirees.contains(idx))
            .map(|(_, c)| *c)
            .collect();
        let perturbe = jouer(&troue);
        // Fills jusqu'à `open_i` : la 1m `i + 1` a pu être retirée, donc un fill à son open a pu bouger.
        assert_passe_identique(
            &original,
            &perturbe,
            candles[i].open_time_ms + MINUTE,
            candles[i].open_time_ms,
        )
        .unwrap_or_else(|e| panic!("graine {graine} : {e}"));
    }
}

// ---------------------------------------------------------------------------------------------
// Mutant : la formule fautive doit faire rougir les détecteurs
// ---------------------------------------------------------------------------------------------

/// Formule fautive : une bougie TF est « clôturée » dès que sa première minute est ouverte, donc son
/// high et son low incluent des minutes futures.
fn mutant(open_tf: Ts, _tf: Timeframe, open_1m: Ts) -> bool {
    open_tf <= open_1m
}

/// Empreinte de ce qu'un `Mtf` expose à la 1m `i` : tranches visibles de M5 et H1 et colonnes SMA 3 et
/// ATR 3 recalculées par `indicators::batch` sur les séries TF complètes (causales : une colonne
/// tronquée à `closed_count` ne dépend pas du reste).
fn empreintes_visibles(mtf: &Mtf<'_>, jusqu_a: usize) -> Vec<u64> {
    let colonnes: Vec<(Vec<f64>, Vec<f64>)> = TF_CANARI
        .iter()
        .map(|&tf| {
            let candles = mtf.candles(tf).expect("tf demandé");
            (
                batch(
                    Sma::new(3).expect("période"),
                    candles.iter().map(|c| c.close),
                ),
                batch(Atr::new(3).expect("période"), candles.iter().copied()),
            )
        })
        .collect();
    (0..=jusqu_a)
        .map(|i| {
            let mut h = FNV_OFFSET;
            for (k, &tf) in TF_CANARI.iter().enumerate() {
                let candles = mtf.candles(tf).expect("tf demandé");
                let compte = mtf.closed_counts(tf).expect("tf demandé")[i];
                fnv(&mut h, compte as u64);
                for c in &candles[..compte] {
                    hacher_bougie(&mut h, c);
                }
                for colonne in [&colonnes[k].0, &colonnes[k].1] {
                    for v in &colonne[..compte] {
                        fnv(&mut h, v.to_bits());
                    }
                }
            }
            h
        })
        .collect()
}

/// Nombre de cas détectés sur `cas` tirages à graine fixe : la perturbation de la 1m `j` change une
/// empreinte visible à une 1m `<= i`. Le générateur est celui de `perturbation_i4_ciblee`.
fn detections(regle: ClosedRule, cas: usize) -> usize {
    let mut rng = SplitMix64(0x4D55_5441_4E54);
    let mut detectes = 0;
    for _ in 0..cas {
        let candles = synthetic_candles(720, rng.next_u64(), T0, 0.05);
        let fraction_i = rng.next_f64();
        let fraction_j = rng.next_f64();
        let amplitude = 0.01 + rng.next_f64() * 0.49;
        let n = candles.len();
        let reference = Mtf::build(&candles, &TF_CANARI).expect("mtf de référence");
        // Départ de la fenêtre : 3 H1 clôturées à la fin de la 1m précédente (règle du moteur).
        let h1 = reference.closed_counts(Timeframe::H1).expect("H1");
        let start = (1..n).find(|&i| h1[i - 1] >= 3).expect("3 H1 en 12 h");
        if start + 1 >= n - 1 {
            continue;
        }
        let i = start + ((fraction_i * (n - 1 - start) as f64) as usize).min(n - 2 - start);
        let candidats = candidats_j(&candles, i, Timeframe::H1.ms());
        if candidats.is_empty() {
            continue;
        }
        let j =
            candidats[((fraction_j * candidats.len() as f64) as usize).min(candidats.len() - 1)];
        let perturbees = perturber(&candles, j, amplitude);
        let avant = Mtf::build_with_rule(&candles, &TF_CANARI, regle).expect("mtf");
        let apres = Mtf::build_with_rule(&perturbees, &TF_CANARI, regle).expect("mtf perturbé");
        if empreintes_visibles(&avant, i) != empreintes_visibles(&apres, i) {
            detectes += 1;
        }
    }
    detectes
}

#[test]
fn mutant_closed_count_fait_rougir_les_detecteurs() {
    let m1 = dataset_horodate();

    // (a) L'espion pur, sans moteur, voit le mutant.
    let mtf_mutant = Mtf::build_with_rule(&m1, &TF_ESPION, mutant).expect("mtf mutant");
    let violations = espion_pur(&mtf_mutant, &TF_ESPION);
    assert!(
        !violations.is_empty(),
        "le contrôle pur doit rougir sur la formule fautive"
    );
    let sain = Mtf::build_with_rule(&m1, &TF_ESPION, is_closed).expect("mtf sain");
    assert!(espion_pur(&sain, &TF_ESPION).is_empty());

    // (b) Le garde-fou S2 (`BarContext::new`) refuse la bougie non clôturée avant même la stratégie.
    let espion = Espion::new(&m1);
    let erreur = run_cell_mtf(&mtf_mutant, Box::new(espion), &config(), &mut NoopObserver)
        .expect_err("le moteur doit refuser le mutant");
    assert!(
        matches!(
            erreur,
            EngineError::Strategy(StrategyError::LookAhead { .. })
        ),
        "{erreur:?}"
    );

    // (c) Le harnais de perturbation au niveau `mtf`, indépendant du garde-fou : 256 cas, graine fixe.
    // Nombre exact de détections mesuré le 14/10 : voir la constante ci-dessous.
    let avec_mutant = detections(mutant, 256);
    let avec_regle_saine = detections(is_closed, 256);
    assert!(
        avec_mutant >= 64,
        "le mutant doit être détecté sur au moins 64 cas sur 256 : {avec_mutant}"
    );
    assert_eq!(
        avec_regle_saine, 0,
        "la règle du moteur ne doit jamais être détectée"
    );
    assert_eq!(
        avec_mutant, DETECTIONS_MESUREES,
        "détections du mutant sur 256 cas"
    );
}

/// Détections du mutant sur 256 cas (mesure du 14/10, graine fixe).
const DETECTIONS_MESUREES: usize = 121;
