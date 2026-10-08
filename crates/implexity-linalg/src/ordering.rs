// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::cmp::Reverse;
use std::collections::BinaryHeap;

use faer::dyn_stack::{MemBuffer, MemStack};
use faer::sparse::SymbolicSparseColMatRef;
use faer::sparse::linalg::amd;

use crate::error::LinalgError;
use crate::sparse::CscMatrix;

pub const NESTED_DISSECTION_MINIMUM_SIZE: usize = 20_000;

const LEAF_VERTICES: usize = 64;
const LEAF_WEIGHT: u64 = 256;
const COARSEST: usize = 120;
const BALANCE: f64 = 1.2;
const PASSES: usize = 8;
const INITIAL_TRIALS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymmetricPattern {
    ptr: Vec<usize>,
    idx: Vec<usize>,
}

impl SymmetricPattern {


    pub fn of(a: &CscMatrix) -> Result<Self, LinalgError> {
        let (m, n) = a.shape();
        if m != n {
            return Err(LinalgError::Shape(format!("symmetric pattern of a non-square {m}×{n} matrix")));
        }
        let (ap, ai) = (a.indptr(), a.indices());
        let mut count = vec![0usize; n + 1];
        for j in 0..n {
            for &i in &ai[ap[j]..ap[j + 1]] {
                if i != j {
                    count[i + 1] += 1;
                    count[j + 1] += 1;
                }
            }
        }
        for j in 0..n {
            count[j + 1] += count[j];
        }
        let mut next = count.clone();
        let mut raw = vec![0usize; count[n]];
        for j in 0..n {
            for &i in &ai[ap[j]..ap[j + 1]] {
                if i != j {
                    raw[next[i]] = j;
                    next[i] += 1;
                    raw[next[j]] = i;
                    next[j] += 1;
                }
            }
        }
        let mut ptr = Vec::with_capacity(n + 1);
        ptr.push(0);
        let mut idx = Vec::with_capacity(raw.len());
        for v in 0..n {
            let list = &mut raw[count[v]..count[v + 1]];
            list.sort_unstable();
            let mut last = usize::MAX;
            for &u in list.iter() {
                if u != last {
                    idx.push(u);
                    last = u;
                }
            }
            ptr.push(idx.len());
        }
        Ok(Self { ptr, idx })
    }

    #[must_use]
    pub fn n(&self) -> usize {
        self.ptr.len() - 1
    }

    #[must_use]
    pub fn ptr(&self) -> &[usize] {
        &self.ptr
    }

    #[must_use]
    pub fn idx(&self) -> &[usize] {
        &self.idx
    }

    fn neighbors(&self, v: usize) -> &[usize] {
        &self.idx[self.ptr[v]..self.ptr[v + 1]]
    }

    #[must_use]
    pub fn is_pattern_of(&self, a: &CscMatrix) -> bool {
        let n = self.n();
        if a.shape() != (n, n) {
            return false;
        }
        let (ap, ai) = (a.indptr(), a.indices());
        let mut entries = 0usize;
        for j in 0..n {
            for &i in &ai[ap[j]..ap[j + 1]] {
                if i == j {
                    continue;
                }
                if self.neighbors(i).binary_search(&j).is_err() {
                    return false;
                }
                entries += if ai[ap[i]..ap[i + 1]].binary_search(&j).is_ok() { 1 } else { 2 };
            }
        }
        entries == self.idx.len()
    }
}

#[derive(Clone, Debug)]
struct Graph {
    xadj: Vec<usize>,
    adj: Vec<usize>,
    ewgt: Vec<u64>,
    vwgt: Vec<u64>,
}

impl Graph {
    fn n(&self) -> usize {
        self.vwgt.len()
    }

    fn neighbors(&self, v: usize) -> &[usize] {
        &self.adj[self.xadj[v]..self.xadj[v + 1]]
    }

    fn edge_weights(&self, v: usize) -> &[u64] {
        &self.ewgt[self.xadj[v]..self.xadj[v + 1]]
    }

    fn total(&self) -> u64 {
        self.vwgt.iter().sum()
    }

    fn induced(&self, keep: impl Fn(usize) -> bool) -> (Self, Vec<usize>) {
        let n = self.n();
        let mut local = vec![usize::MAX; n];
        let mut ids = Vec::new();
        for (v, slot) in local.iter_mut().enumerate() {
            if keep(v) {
                *slot = ids.len();
                ids.push(v);
            }
        }
        let mut xadj = Vec::with_capacity(ids.len() + 1);
        xadj.push(0);
        let mut adj = Vec::new();
        let mut ewgt = Vec::new();
        let mut vwgt = Vec::with_capacity(ids.len());
        for &v in &ids {
            for (&u, &w) in self.neighbors(v).iter().zip(self.edge_weights(v)) {
                if local[u] != usize::MAX {
                    adj.push(local[u]);
                    ewgt.push(w);
                }
            }
            xadj.push(adj.len());
            vwgt.push(self.vwgt[v]);
        }
        (Self { xadj, adj, ewgt, vwgt }, ids)
    }

    fn components(&self) -> (Vec<usize>, usize) {
        let n = self.n();
        let mut label = vec![usize::MAX; n];
        let mut count = 0;
        let mut stack = Vec::new();
        for s in 0..n {
            if label[s] != usize::MAX {
                continue;
            }
            label[s] = count;
            stack.push(s);
            while let Some(v) = stack.pop() {
                for &u in self.neighbors(v) {
                    if label[u] == usize::MAX {
                        label[u] = count;
                        stack.push(u);
                    }
                }
            }
            count += 1;
        }
        (label, count)
    }
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn as_u64(v: usize) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

fn compress(p: &SymmetricPattern) -> (Graph, Vec<usize>, Vec<usize>) {
    let n = p.n();
    let hash: Vec<u64> = (0..n)
        .map(|v| {

            p.neighbors(v)
                .iter()
                .chain(std::iter::once(&v))
                .fold(0u64, |h, &u| h.wrapping_add(mix(as_u64(u))))
        })
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by_key(|&v| (hash[v], v));
    let closed_equal = |u: usize, v: usize| -> bool {
        let (nu, nv) = (p.neighbors(u), p.neighbors(v));
        nu.len() == nv.len()
            && nu.binary_search(&v).is_ok()
            && nu.iter().filter(|&&x| x != v).eq(nv.iter().filter(|&&x| x != u))
    };
    let mut rep = vec![usize::MAX; n];
    let mut start = 0;
    while start < n {
        let mut end = start + 1;
        while end < n && hash[order[end]] == hash[order[start]] {
            end += 1;
        }
        for a in start..end {
            let v = order[a];
            if rep[v] != usize::MAX {
                continue;
            }
            rep[v] = v;
            for &u in &order[a + 1..end] {
                if rep[u] == usize::MAX && closed_equal(v, u) {
                    rep[u] = v;
                }
            }
        }
        start = end;
    }

    let mut class = vec![usize::MAX; n];
    let mut count = 0;
    for v in 0..n {
        if rep[v] == v {
            class[v] = count;
            count += 1;
        }
    }
    let mut member_ptr = vec![0usize; count + 1];
    for v in 0..n {
        class[v] = class[rep[v]];
        member_ptr[class[v] + 1] += 1;
    }
    for c in 0..count {
        member_ptr[c + 1] += member_ptr[c];
    }
    let mut next = member_ptr.clone();
    let mut members = vec![0usize; n];
    for v in 0..n {
        members[next[class[v]]] = v;
        next[class[v]] += 1;
    }
    let mut xadj = Vec::with_capacity(count + 1);
    xadj.push(0);
    let mut adj = Vec::new();
    let mut vwgt = Vec::with_capacity(count);
    for c in 0..count {
        let r = members[member_ptr[c]];
        let begin = adj.len();
        for &u in p.neighbors(r) {
            if class[u] != c {
                adj.push(class[u]);
            }
        }
        adj[begin..].sort_unstable();
        let mut w = begin;
        for k in begin..adj.len() {
            if k == begin || adj[k] != adj[w - 1] {
                adj[w] = adj[k];
                w += 1;
            }
        }
        adj.truncate(w);
        xadj.push(adj.len());
        vwgt.push(as_u64(member_ptr[c + 1] - member_ptr[c]));
    }
    let ewgt = vec![1; adj.len()];
    (Graph { xadj, adj, ewgt, vwgt }, member_ptr, members)
}

fn minimum_degree(g: &Graph) -> Result<Vec<usize>, LinalgError> {
    let n = g.n();
    if n <= 2 {
        return Ok((0..n).collect());
    }
    let pattern = SymbolicSparseColMatRef::new_checked(n, n, &g.xadj, None, &g.adj);
    let mut fwd = vec![0usize; n];
    let mut inv = vec![0usize; n];
    let mut mem = MemBuffer::try_new(amd::order_scratch::<usize>(n, g.adj.len()))
        .map_err(|_| LinalgError::OutOfMemory("minimum degree ordering workspace".into()))?;
    amd::order(&mut fwd, &mut inv, pattern, amd::Control::default(), MemStack::new(&mut mem))?;
    Ok(fwd)
}

fn pseudo_peripheral(g: &Graph, start: usize) -> usize {
    let n = g.n();
    let mut level = vec![usize::MAX; n];
    let mut queue = Vec::with_capacity(n);
    let mut root = start;
    let mut eccentricity = 0;
    for _ in 0..8 {
        level.fill(usize::MAX);
        queue.clear();
        queue.push(root);
        level[root] = 0;
        let mut head = 0;
        while head < queue.len() {
            let v = queue[head];
            head += 1;
            for &u in g.neighbors(v) {
                if level[u] == usize::MAX {
                    level[u] = level[v] + 1;
                    queue.push(u);
                }
            }
        }
        let last = queue[queue.len() - 1];
        let depth = level[last];
        if depth <= eccentricity {
            break;
        }
        eccentricity = depth;

        let far = queue
            .iter()
            .copied()
            .filter(|&v| level[v] == depth)
            .min_by_key(|&v| (g.neighbors(v).len(), v))
            .unwrap_or(last);
        root = far;
    }
    root
}

const SIDE0: u8 = 0;
const SIDE1: u8 = 1;
const SEP: u8 = 2;

#[derive(Clone, Debug)]
struct Separator {
    part: Vec<u8>,
    weight: [u64; 3],
}

impl Separator {
    fn from_parts(g: &Graph, part: Vec<u8>) -> Self {
        let mut weight = [0u64; 3];
        for (v, &p) in part.iter().enumerate() {
            weight[usize::from(p)] += g.vwgt[v];
        }
        Self { part, weight }
    }

    fn imbalance(&self) -> u64 {
        self.weight[0].abs_diff(self.weight[1])
    }

    fn score(&self) -> (u64, u64) {
        (self.weight[2], self.imbalance())
    }
}

fn max_side(total: u64) -> u64 {

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let bound = (BALANCE * total as f64 / 2.0).ceil() as u64;
    bound
}

fn gains_of(g: &Graph, part: &[u8], v: usize) -> [i64; 2] {
    let mut side = [0u64; 2];
    for &u in g.neighbors(v) {
        if part[u] != SEP {
            side[usize::from(part[u])] += g.vwgt[u];
        }
    }
    let own = i64::try_from(g.vwgt[v]).unwrap_or(i64::MAX);
    let w = |x: u64| i64::try_from(x).unwrap_or(i64::MAX);
    [own - w(side[1]), own - w(side[0])]
}

struct Move {
    v: usize,
    to: u8,
    pulled: std::ops::Range<usize>,
}

#[allow(clippy::too_many_lines)]
fn refine(g: &Graph, sep: &mut Separator) {
    let n = g.n();
    let total = g.total();
    let bound = max_side(total);
    let limit = (n / 100).clamp(25, 150);
    let mut locked = vec![false; n];
    let mut gains = [vec![0i64; n], vec![0i64; n]];
    let mut mark = vec![usize::MAX; n];
    let mut stamp = 0usize;
    for _pass in 0..PASSES {
        let start_score = sep.score();
        locked.fill(false);
        let mut heaps: [BinaryHeap<(i64, Reverse<usize>)>; 2] = [BinaryHeap::new(), BinaryHeap::new()];
        for v in (0..n).filter(|&v| sep.part[v] == SEP) {
            let gv = gains_of(g, &sep.part, v);
            for t in 0..2 {
                gains[t][v] = gv[t];
                heaps[t].push((gv[t], Reverse(v)));
            }
        }
        let mut log: Vec<Move> = Vec::new();
        let mut pulled_log: Vec<usize> = Vec::new();
        let mut best = (sep.score(), 0usize);
        let mut since_best = 0usize;
        loop {

            let mut top: [Option<(i64, usize)>; 2] = [None, None];
            for to in [SIDE0, SIDE1] {
                let t = usize::from(to);
                while let Some(&(gv, Reverse(v))) = heaps[t].peek() {
                    if sep.part[v] == SEP && !locked[v] && gains[t][v] == gv {
                        top[t] = Some((gv, v));
                        break;
                    }
                    heaps[t].pop();
                }
            }
            let admissible = |t: usize| top[t].filter(|&(_, v)| sep.weight[t] + g.vwgt[v] <= bound);
            let choice = match (admissible(0), admissible(1)) {
                (Some(a), Some(b)) => {
                    if a.0 > b.0 || (a.0 == b.0 && sep.weight[0] <= sep.weight[1]) {
                        Some((SIDE0, a.1))
                    } else {
                        Some((SIDE1, b.1))
                    }
                }
                (Some(a), None) => Some((SIDE0, a.1)),
                (None, Some(b)) => Some((SIDE1, b.1)),
                (None, None) => None,
            };
            let Some((to, v)) = choice else { break };
            let other = 1 - to;
            locked[v] = true;
            sep.part[v] = to;
            sep.weight[2] -= g.vwgt[v];
            sep.weight[usize::from(to)] += g.vwgt[v];
            let pulled_start = pulled_log.len();
            for &u in g.neighbors(v) {
                if sep.part[u] == other {
                    sep.part[u] = SEP;
                    sep.weight[usize::from(other)] -= g.vwgt[u];
                    sep.weight[2] += g.vwgt[u];
                    pulled_log.push(u);
                }
            }
            log.push(Move { v, to, pulled: pulled_start..pulled_log.len() });

            stamp += 1;
            for &u in &pulled_log[pulled_start..] {
                mark[u] = stamp;
            }
            let wv = i64::try_from(g.vwgt[v]).unwrap_or(i64::MAX);
            let (ti, oi) = (usize::from(to), usize::from(other));
            for &x in g.neighbors(v) {
                if sep.part[x] == SEP && !locked[x] && mark[x] != stamp {
                    gains[oi][x] -= wv;
                    heaps[oi].push((gains[oi][x], Reverse(x)));
                }
            }
            for &u in &pulled_log[pulled_start..] {
                let wu = i64::try_from(g.vwgt[u]).unwrap_or(i64::MAX);
                for &x in g.neighbors(u) {
                    if sep.part[x] == SEP && !locked[x] && mark[x] != stamp {
                        gains[ti][x] += wu;
                        heaps[ti].push((gains[ti][x], Reverse(x)));
                    }
                }
            }
            for &u in &pulled_log[pulled_start..] {
                if !locked[u] {
                    let gu = gains_of(g, &sep.part, u);
                    for t in 0..2 {
                        gains[t][u] = gu[t];
                        heaps[t].push((gu[t], Reverse(u)));
                    }
                }
            }
            if sep.score() < best.0 {
                best = (sep.score(), log.len());
                since_best = 0;
            } else {
                since_best += 1;
                if since_best > limit {
                    break;
                }
            }
        }

        while log.len() > best.1 {
            let Some(m) = log.pop() else { break };
            let other = 1 - m.to;
            for &u in &pulled_log[m.pulled.clone()] {
                sep.part[u] = other;
                sep.weight[2] -= g.vwgt[u];
                sep.weight[usize::from(other)] += g.vwgt[u];
            }
            sep.part[m.v] = SEP;
            sep.weight[usize::from(m.to)] -= g.vwgt[m.v];
            sep.weight[2] += g.vwgt[m.v];
        }
        if sep.score() >= start_score {
            break;
        }
    }
}

fn matching(g: &Graph, seed: u64) -> (Vec<usize>, usize) {
    let n = g.n();
    let max_weight = {
        let total = g.total();
        (3 * total).div_ceil(2 * as_u64(COARSEST)).max(1)
    };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by_key(|&v| mix(as_u64(v) ^ seed));
    let mut mate = vec![usize::MAX; n];
    for &v in &order {
        if mate[v] != usize::MAX {
            continue;
        }
        let mut best: Option<(u64, usize)> = None;
        for (&u, &w) in g.neighbors(v).iter().zip(g.edge_weights(v)) {
            if mate[u] == usize::MAX
                && u != v
                && g.vwgt[v] + g.vwgt[u] <= max_weight
                && best.is_none_or(|(bw, bu)| w > bw || (w == bw && u < bu))
            {
                best = Some((w, u));
            }
        }
        match best {
            Some((_, u)) => {
                mate[v] = u;
                mate[u] = v;
            }
            None => mate[v] = v,
        }
    }

    let mut waiting = vec![usize::MAX; n];
    for &v in &order {
        if mate[v] != v {
            continue;
        }
        let partner = g
            .neighbors(v)
            .iter()
            .map(|&h| waiting[h])
            .find(|&w| w != usize::MAX && mate[w] == w && w != v && g.vwgt[v] + g.vwgt[w] <= max_weight);
        if let Some(w) = partner {
            mate[v] = w;
            mate[w] = v;
        } else {
            for &h in g.neighbors(v) {
                waiting[h] = v;
            }
        }
    }
    let mut cmap = vec![usize::MAX; n];
    let mut cn = 0;
    for v in 0..n {
        if cmap[v] == usize::MAX {
            cmap[v] = cn;
            cmap[mate[v]] = cn;
            cn += 1;
        }
    }
    (cmap, cn)
}

fn contract(g: &Graph, cmap: &[usize], cn: usize) -> Graph {
    let n = g.n();
    let mut first = vec![usize::MAX; cn];
    let mut second = vec![usize::MAX; cn];
    for (v, &c) in cmap.iter().enumerate().take(n) {
        if first[c] == usize::MAX {
            first[c] = v;
        } else {
            second[c] = v;
        }
    }
    let mut xadj = Vec::with_capacity(cn + 1);
    xadj.push(0);
    let mut adj = Vec::with_capacity(g.adj.len());
    let mut ewgt = Vec::with_capacity(g.adj.len());
    let mut vwgt = Vec::with_capacity(cn);
    let mut slot = vec![usize::MAX; cn];
    for c in 0..cn {
        let begin = adj.len();
        let mut weight = 0;
        for v in [first[c], second[c]] {
            if v == usize::MAX {
                continue;
            }
            weight += g.vwgt[v];
            for (&u, &w) in g.neighbors(v).iter().zip(g.edge_weights(v)) {
                let cu = cmap[u];
                if cu == c {
                    continue;
                }
                if slot[cu] != usize::MAX && slot[cu] >= begin {
                    ewgt[slot[cu]] += w;
                } else {
                    slot[cu] = adj.len();
                    adj.push(cu);
                    ewgt.push(w);
                }
            }
        }
        xadj.push(adj.len());
        vwgt.push(weight);
    }
    Graph { xadj, adj, ewgt, vwgt }
}

fn grown_separator(g: &Graph, seed: usize) -> Separator {
    let n = g.n();
    let half = g.total() / 2;
    let mut part = vec![SIDE1; n];
    let mut queued = vec![false; n];
    let mut queue = std::collections::VecDeque::new();
    let mut grown = 0u64;
    let mut next_start = 0;
    queue.push_back(seed);
    queued[seed] = true;
    while grown < half {
        let v = if let Some(v) = queue.pop_front() {
            v
        } else {
            while next_start < n && queued[next_start] {
                next_start += 1;
            }
            if next_start == n {
                break;
            }
            queued[next_start] = true;
            next_start
        };
        part[v] = SIDE0;
        grown += g.vwgt[v];
        for &u in g.neighbors(v) {
            if !queued[u] {
                queued[u] = true;
                queue.push_back(u);
            }
        }
    }
    let boundary = |side: u8| -> Vec<usize> {
        (0..n).filter(|&v| part[v] == side && g.neighbors(v).iter().any(|&u| part[u] == 1 - side)).collect()
    };
    let b0 = boundary(SIDE0);
    let b1 = boundary(SIDE1);
    let w0: u64 = b0.iter().map(|&v| g.vwgt[v]).sum();
    let w1: u64 = b1.iter().map(|&v| g.vwgt[v]).sum();
    for v in if w0 <= w1 { b0 } else { b1 } {
        part[v] = SEP;
    }
    Separator::from_parts(g, part)
}

fn bisect(g: &Graph, seed: u64) -> Separator {
    let mut levels: Vec<(Graph, Vec<usize>)> = Vec::new();
    loop {
        let current = levels.last().map_or(g, |(c, _)| c);
        if current.n() <= COARSEST {
            break;
        }
        let (cmap, cn) = matching(current, seed.wrapping_add(as_u64(levels.len())));
        if cn * 20 > current.n() * 19 {
            break;
        }
        let coarse = contract(current, &cmap, cn);
        levels.push((coarse, cmap));
    }
    let coarsest = levels.last().map_or(g, |(c, _)| c);
    let n = coarsest.n();
    let mut seeds = vec![pseudo_peripheral(coarsest, 0)];
    for t in 1..INITIAL_TRIALS {
        let s = usize::try_from(mix(seed ^ as_u64(t)) % as_u64(n)).unwrap_or(0);
        if !seeds.contains(&s) {
            seeds.push(s);
        }
    }
    let bound = max_side(coarsest.total());
    let mut best: Option<Separator> = None;
    for s in seeds {
        let mut sep = grown_separator(coarsest, s);
        refine(coarsest, &mut sep);
        let balanced = sep.weight[0] <= bound && sep.weight[1] <= bound;
        let key = (!balanced, sep.score());
        if best.as_ref().is_none_or(|b| {
            let bb = b.weight[0] <= bound && b.weight[1] <= bound;
            key < (!bb, b.score())
        }) {
            best = Some(sep);
        }
    }
    let Some(mut sep) = best else {
        return Separator::from_parts(g, vec![SIDE0; g.n()]);
    };

    for k in (0..levels.len()).rev() {
        let finer = if k == 0 { g } else { &levels[k - 1].0 };
        let cmap = &levels[k].1;
        let part: Vec<u8> = cmap.iter().map(|&c| sep.part[c]).collect();
        sep = Separator::from_parts(finer, part);
        refine(finer, &mut sep);
    }
    sep
}

const PARALLEL_PART: usize = 2_000;

fn dissect(g: &Graph, ids: &[usize], depth: u64) -> Result<Vec<usize>, LinalgError> {
    let n = g.n();
    if n <= LEAF_VERTICES || g.total() <= LEAF_WEIGHT {
        return Ok(minimum_degree(g)?.into_iter().map(|v| ids[v]).collect());
    }
    let (label, count) = g.components();
    if count > 1 {
        let mut out = Vec::with_capacity(n);
        for c in 0..count {
            let (sub, local) = g.induced(|v| label[v] == c);
            let sub_ids: Vec<usize> = local.iter().map(|&v| ids[v]).collect();
            out.extend(dissect(&sub, &sub_ids, depth + 1)?);
        }
        return Ok(out);
    }
    let sep = bisect(g, mix(depth.wrapping_mul(0x51_7CC1) ^ as_u64(n)));
    if sep.weight[0] == 0 || sep.weight[1] == 0 {

        return Ok(minimum_degree(g)?.into_iter().map(|v| ids[v]).collect());
    }
    let part = |side: u8| -> Result<Vec<usize>, LinalgError> {
        let (sub, local) = g.induced(|v| sep.part[v] == side);
        let sub_ids: Vec<usize> = local.iter().map(|&v| ids[v]).collect();
        dissect(&sub, &sub_ids, depth + 1)
    };
    let (first, second) = if n >= PARALLEL_PART {
        rayon::join(|| part(SIDE0), || part(SIDE1))
    } else {
        (part(SIDE0), part(SIDE1))
    };
    let mut out = first?;
    out.extend(second?);
    out.extend((0..n).filter(|&v| sep.part[v] == SEP).map(|v| ids[v]));
    Ok(out)
}



pub fn nested_dissection(p: &SymmetricPattern) -> Result<Vec<usize>, LinalgError> {
    let n = p.n();
    if n == 0 {
        return Ok(Vec::new());
    }
    let (graph, member_ptr, members) = compress(p);
    let ids: Vec<usize> = (0..graph.n()).collect();
    let order = dissect(&graph, &ids, 0)?;
    let mut out = Vec::with_capacity(n);
    for c in order {
        out.extend_from_slice(&members[member_ptr[c]..member_ptr[c + 1]]);
    }
    Ok(out)
}



pub fn approximate_minimum_degree(p: &SymmetricPattern) -> Result<Vec<usize>, LinalgError> {
    let n = p.n();
    let g = Graph { xadj: p.ptr.clone(), adj: p.idx.clone(), ewgt: vec![1; p.idx.len()], vwgt: vec![1; n] };
    minimum_degree(&g)
}

