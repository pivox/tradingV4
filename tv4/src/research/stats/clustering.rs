//! Composantes connexes d'un panel strictement comparable, sans intersection de dates.
use super::daily::{DatedReturn, grid, invalid, mean, validate_window};
use super::types::{Metric, StatsError, StatsOptions, Window, finite};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerSeries {
    pub config_hash: String,
    pub dataset_hash: String,
    pub line: String,
    pub capital: f64,
    pub window: Window,
    pub daily_returns: Vec<DatedReturn>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusteringStats {
    pub method: String,
    pub correlation_threshold: f64,
    pub observed_configs: usize,
    /// N est déjà vérifié contre le registre par l'appelant ; None signifie inconnu.
    pub declared_verified_n: Option<usize>,
    pub coverage: Option<f64>,
    pub clusters: Vec<Vec<String>>,
    pub panel_n_eff: usize,
    pub global_n_eff: Metric,
}
fn validate(peer: &PeerSeries) -> Result<(), StatsError> {
    validate_window(peer.window)?;
    if !peer.capital.is_finite()
        || peer.capital <= 0.
        || peer.config_hash.is_empty()
        || peer.dataset_hash.is_empty()
        || peer.line.is_empty()
    {
        return Err(invalid("identités et capital positif fini du peer requis"));
    }
    let dates: Vec<_> = grid(peer.window).collect();
    if dates.is_empty() || dates.len() != peer.daily_returns.len() {
        return Err(invalid("grille quotidienne complète requise"));
    }
    for (expected, actual) in dates.iter().zip(&peer.daily_returns) {
        if expected.start_ms != actual.start_ms
            || expected.end_ms != actual.end_ms
            || !actual.value.is_finite()
            || actual.value < -1.
        {
            return Err(invalid(
                "dates quotidiennes identiques et rendements finis >=-1 requis",
            ));
        }
    }
    Ok(())
}
fn pearson(a: &[DatedReturn], b: &[DatedReturn]) -> Result<Option<f64>, StatsError> {
    if a.len() < 2 {
        return Ok(None);
    }
    let av: Vec<_> = a.iter().map(|p| p.value).collect();
    let bv: Vec<_> = b.iter().map(|p| p.value).collect();
    let ma = mean(&av)?;
    let mb = mean(&bv)?;
    // Échelle commune par série : évite les carrés débordants sans changer Pearson.
    let sa = av.iter().map(|v| (v - ma).abs()).fold(0., f64::max);
    let sb = bv.iter().map(|v| (v - mb).abs()).fold(0., f64::max);
    if sa == 0. || sb == 0. {
        return Ok(None);
    }
    let (mut aa, mut bb, mut ab) = (0., 0., 0.);
    for (&a, &b) in av.iter().zip(&bv) {
        let x = finite(a - ma)? / sa;
        let y = finite(b - mb)? / sb;
        aa += x * x;
        bb += y * y;
        ab += x * y;
    }
    Ok(Some(finite(ab / (aa.sqrt() * bb.sqrt()))?))
}
fn root(parents: &mut [usize], mut i: usize) -> usize {
    while parents[i] != i {
        parents[i] = parents[parents[i]];
        i = parents[i];
    }
    i
}
pub fn clustering(
    peers: &[PeerSeries],
    declared_verified_n: Option<usize>,
    options: &StatsOptions,
) -> Result<ClusteringStats, StatsError> {
    options.validate()?;
    let mut distinct = BTreeMap::new();
    for p in peers {
        validate(p)?;
        if let Some(previous) = distinct.insert(&p.config_hash, p)
            && previous != p
        {
            return Err(invalid("config_hash dupliqué avec données contradictoires"));
        }
    }
    let panel: Vec<_> = distinct.values().copied().collect();
    if let Some(first) = panel.first() {
        for p in &panel {
            if p.dataset_hash != first.dataset_hash
                || p.line != first.line
                || p.capital != first.capital
                || p.window != first.window
            {
                return Err(invalid(
                    "dataset, ligne, capital et fenêtre identiques requis",
                ));
            }
        }
    }
    let m = panel.len();
    if declared_verified_n.is_some_and(|n| n < m || n == 0) {
        return Err(invalid(
            "N déclaré positif doit être >= M configurations distinctes",
        ));
    }
    let mut parents: Vec<_> = (0..m).collect();
    for i in 0..m {
        for j in i + 1..m {
            if pearson(&panel[i].daily_returns, &panel[j].daily_returns)?
                .is_some_and(|c| c > options.correlation_threshold)
            {
                let ri = root(&mut parents, i);
                let rj = root(&mut parents, j);
                parents[rj] = ri;
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (i, p) in panel.iter().enumerate() {
        groups
            .entry(root(&mut parents, i))
            .or_default()
            .push(p.config_hash.clone());
    }
    let mut clusters: Vec<_> = groups.into_values().collect();
    clusters.sort();
    let count = clusters.len();
    Ok(ClusteringStats {
        method: "pearson_strict_connected_components_v1".into(),
        correlation_threshold: options.correlation_threshold,
        observed_configs: m,
        declared_verified_n,
        coverage: declared_verified_n.map(|n| m as f64 / n as f64),
        panel_n_eff: count,
        clusters,
        global_n_eff: if m > 0 && declared_verified_n == Some(m) {
            Metric::available(count as f64)?
        } else {
            Metric::unavailable(
                "N global non vérifié ou panel incomplet : nombre de clusters du panel seulement",
            )
        },
    })
}
