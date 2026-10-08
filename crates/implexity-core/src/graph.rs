// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet};

#[must_use]
pub fn strongly_connected(
    nodes: &BTreeSet<String>,
    adj: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<BTreeSet<String>> {
    struct T<'a> {
        adj: &'a BTreeMap<String, BTreeSet<String>>,
        index: usize,
        stack: Vec<String>,
        indices: BTreeMap<String, usize>,
        low: BTreeMap<String, usize>,
        on: BTreeSet<String>,
        sccs: Vec<BTreeSet<String>>,
    }
    impl T<'_> {
        fn strong(&mut self, v: &str) {
            self.indices.insert(v.to_string(), self.index);
            self.low.insert(v.to_string(), self.index);
            self.index += 1;
            self.stack.push(v.to_string());
            self.on.insert(v.to_string());
            let next: Vec<String> = self.adj.get(v).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            for w in next {
                if !self.indices.contains_key(&w) {
                    self.strong(&w);
                    let lw = self.low.get(&w).copied().unwrap_or(usize::MAX);
                    if let Some(lv) = self.low.get_mut(v) {
                        *lv = (*lv).min(lw);
                    }
                } else if self.on.contains(&w) {
                    let iw = self.indices.get(&w).copied().unwrap_or(usize::MAX);
                    if let Some(lv) = self.low.get_mut(v) {
                        *lv = (*lv).min(iw);
                    }
                }
            }
            if self.low.get(v) == self.indices.get(v) {
                let mut comp = BTreeSet::new();
                while let Some(w) = self.stack.pop() {
                    self.on.remove(&w);
                    let done = w == v;
                    comp.insert(w);
                    if done {
                        break;
                    }
                }
                if comp.len() > 1 {
                    self.sccs.push(comp);
                }
            }
        }
    }
    let mut t = T {
        adj,
        index: 0,
        stack: Vec::new(),
        indices: BTreeMap::new(),
        low: BTreeMap::new(),
        on: BTreeSet::new(),
        sccs: Vec::new(),
    };
    for a in nodes {
        if !t.indices.contains_key(a) {
            t.strong(a);
        }
    }
    t.sccs
}


