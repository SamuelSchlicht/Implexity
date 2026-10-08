// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_runtime::dynamic_frames::store::{DynamicStore, FrameEntry};
use serde_json::Value;

use crate::error::{AgentError, AgentResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleSel {
    Last,
    Index(i64),
}

impl CycleSel {
    #[must_use]
    pub fn parse(v: Option<&Value>) -> Self {
        match v.and_then(Value::as_i64) {
            Some(i) => Self::Index(i),
            None => Self::Last,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    Indices(Vec<usize>),
    Phases(Vec<f64>),
    Times(Vec<f64>),
    Count(usize),
}

impl Selection {


    pub fn parse(v: Option<&Value>, default: usize) -> AgentResult<(Self, CycleSel)> {
        let Some(m) = v.and_then(Value::as_object) else { return Ok((Self::Count(default), CycleSel::Last)) };
        let cycle = CycleSel::parse(m.get("cycle"));
        let given: Vec<&str> =
            ["indices", "phases", "times", "count"].into_iter().filter(|k| m.contains_key(*k)).collect();
        if given.len() > 1 {
            return Err(AgentError::contract("frames takes exactly one of indices, phases, times or count"));
        }
        let nums = |k: &str| -> Vec<f64> {
            m.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default()
        };
        let sel = match given.first().copied() {
            Some("indices") => Self::Indices(
                m["indices"]
                    .as_array()
                    .map(|a| {
                        a.iter().filter_map(Value::as_u64).filter_map(|x| usize::try_from(x).ok()).collect()
                    })
                    .unwrap_or_default(),
            ),
            Some("phases") => Self::Phases(nums("phases")),
            Some("times") => Self::Times(nums("times")),
            Some("count") => {
                Self::Count(m["count"].as_u64().and_then(|x| usize::try_from(x).ok()).unwrap_or(default))
            }
            _ => Self::Count(default),
        };
        Ok((sel, cycle))
    }
}

#[must_use]
pub fn cycles(store: &DynamicStore) -> Vec<(i64, Vec<&FrameEntry>)> {
    let mut out: Vec<(i64, Vec<&FrameEntry>)> = Vec::new();
    for f in store.frames() {
        let Some(c) = f.cycle else { continue };
        match out.last_mut() {
            Some((k, v)) if *k == c => v.push(f),
            _ => out.push((c, vec![f])),
        }
    }
    out
}



pub fn cycle_frames(store: &DynamicStore, sel: CycleSel) -> AgentResult<(Option<i64>, Vec<&FrameEntry>)> {
    if store.frames().is_empty() {
        return Err(AgentError::refused("the store holds no frames"));
    }
    let cs = cycles(store);
    if cs.is_empty() {
        return Ok((None, store.frames().iter().collect()));
    }
    match sel {
        CycleSel::Last => {
            let most = cs.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
            let (c, v) = cs
                .iter()
                .rev()
                .find(|(_, v)| v.len() == most)
                .cloned()
                .unwrap_or_else(|| cs[cs.len() - 1].clone());
            Ok((Some(c), v))
        }
        CycleSel::Index(i) => {
            cs.into_iter().find(|(c, _)| *c == i).map(|(c, v)| (Some(c), v)).ok_or_else(|| {
                AgentError::contract(format!(
                    "cycle {i} is not retained in the store (see inspect_dynamic_results)"
                ))
            })
        }
    }
}

fn circular(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 1.0;
    d.min(1.0 - d)
}

fn nearest_phase<'a>(frames: &[&'a FrameEntry], phase: f64) -> Option<&'a FrameEntry> {
    frames
        .iter()
        .filter_map(|f| f.phase.map(|p| (circular(p, phase), *f)))
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.seq.cmp(&b.1.seq)))
        .map(|(_, f)| f)
}



pub fn select<'a>(
    store: &'a DynamicStore,
    sel: &Selection,
    cycle: CycleSel,
) -> AgentResult<Vec<&'a FrameEntry>> {
    let all = store.frames();
    let mut out: Vec<&FrameEntry> = Vec::new();
    match sel {
        Selection::Indices(ix) => {
            for &i in ix {
                out.push(all.get(i).ok_or_else(|| {
                    AgentError::contract(format!(
                        "frame index {i} is outside the {} retained frames",
                        all.len()
                    ))
                })?);
            }
        }
        Selection::Times(ts) => {
            for &t in ts {
                if let Some(f) = all.iter().min_by(|a, b| (a.t - t).abs().total_cmp(&(b.t - t).abs())) {
                    out.push(f);
                }
            }
        }
        Selection::Phases(ps) => {
            let (_, frames) = cycle_frames(store, cycle)?;
            if frames.iter().all(|f| f.phase.is_none()) {
                return Err(AgentError::contract(
                    "the store has no phases (a transient run); select frames by time or index",
                ));
            }
            out.extend(ps.iter().filter_map(|p| nearest_phase(&frames, *p)));
        }
        Selection::Count(n) => {
            let (_, frames) = cycle_frames(store, cycle)?;
            let n = (*n).max(1);
            if frames.iter().all(|f| f.phase.is_some()) {
                out.extend((0..n).filter_map(|k| nearest_phase(&frames, k as f64 / n as f64)));
            } else {
                let m = frames.len();
                out.extend(
                    (0..n.min(m))
                        .map(|k| frames[if n.min(m) > 1 { k * (m - 1) / (n.min(m) - 1) } else { m - 1 }]),
                );
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|f| seen.insert(f.seq));
    if out.is_empty() {
        return Err(AgentError::refused("no stored frame matches the selection"));
    }
    Ok(out)
}



pub fn animation(
    store: &DynamicStore,
    cycle: CycleSel,
    cycles_n: usize,
    per_cycle: usize,
) -> AgentResult<(Vec<&FrameEntry>, usize)> {
    let cs = cycles(store);
    let mut out = Vec::new();
    if cs.is_empty() {
        let all = store.frames();
        if all.is_empty() {
            return Err(AgentError::refused("the store holds no frames"));
        }
        let n = (per_cycle * cycles_n).min(all.len()).max(1);
        out.extend((0..n).map(|k| &all[if n > 1 { k * (all.len() - 1) / (n - 1) } else { all.len() - 1 }]));
    } else {
        let start = match cycle {
            CycleSel::Last => {
                let (last, _) = cycle_frames(store, CycleSel::Last)?;
                let pos = cs.iter().position(|(c, _)| Some(*c) == last).unwrap_or(cs.len() - 1);
                (pos + 1).saturating_sub(cycles_n)
            }
            CycleSel::Index(i) => cs.iter().position(|(c, _)| *c == i).ok_or_else(|| {
                AgentError::contract(format!(
                    "cycle {i} is not retained in the store (see inspect_dynamic_results)"
                ))
            })?,
        };
        for (_, frames) in cs.iter().skip(start).take(cycles_n) {
            out.extend((0..per_cycle).filter_map(|k| nearest_phase(frames, k as f64 / per_cycle as f64)));
        }
    }
    let distinct = out.iter().map(|f| f.seq).collect::<std::collections::BTreeSet<_>>().len();
    Ok((out, distinct))
}
