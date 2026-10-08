// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrackTopologyState {
    pub open_edges: Vec<bool>,
    pub components: Vec<Vec<usize>>,
    pub topology_changed: bool,
}

fn components(n: usize, edges: &[[usize; 2]], open: &[bool]) -> Vec<Vec<usize>> {
    let mut adj = vec![Vec::new(); n];
    for (k, &[a, b]) in edges.iter().enumerate() {
        if open[k] {
            continue;
        }
        adj[a].push(b);
        adj[b].push(a);
    }
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    for s in 0..n {
        if seen[s] {
            continue;
        }
        let mut stack = vec![s];
        seen[s] = true;
        let mut c = Vec::new();
        while let Some(u) = stack.pop() {
            c.push(u);
            for &v in &adj[u] {
                if !seen[v] {
                    seen[v] = true;
                    stack.push(v);
                }
            }
        }
        c.sort_unstable();
        out.push(c);
    }
    out
}



pub fn update_crack_topology(
    node_count: usize,
    edges: &[[usize; 2]],
    old_open: &[bool],
    driving: &[f64],
    threshold: f64,
    margin: f64,
) -> CaeResult<CrackTopologyState> {
    if old_open.len() != driving.len()
        || old_open.len() != edges.len()
        || edges.iter().flatten().any(|&i| i >= node_count)
    {
        return Err(CaeError::contract("edge-state mismatch"));
    }
    if driving.iter().any(|d| (d - threshold).abs() <= margin) {
        return Err(CaeError::contract("crack nucleation boundary requires event localization"));
    }
    let open: Vec<bool> = old_open.iter().zip(driving).map(|(o, d)| *o || *d > threshold).collect();
    let changed = open.iter().zip(old_open).any(|(a, b)| a != b);
    Ok(CrackTopologyState {
        components: components(node_count, edges, &open),
        open_edges: open,
        topology_changed: changed,
    })
}

#[must_use]
pub fn crack_event_fraction(d0: &[f64], d1: &[f64], threshold: f64) -> Option<f64> {
    d0.iter()
        .zip(d1)
        .filter(|(a, b)| **a < threshold && **b >= threshold && *b - *a > 0.0)
        .map(|(a, b)| (threshold - a) / (b - a))
        .reduce(f64::min)
}

