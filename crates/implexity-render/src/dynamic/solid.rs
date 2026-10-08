// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::HashMap;

use crate::RenderError;

pub const MAX_SIMPLICES: usize = 1 << 26;

#[derive(Clone, Copy, Debug)]
pub enum Sites<'a> {
    Nodes(&'a [f64]),
    Cells(&'a [f64]),
}

#[derive(Clone, Copy, Debug)]
pub enum Topology<'a> {
    Grid {
        nodes: &'a [usize],
        spacing: &'a [f64],
        origin: &'a [f64],
    },
    Mesh {
        dims: usize,
        points: &'a [f64],
        cells: &'a [usize],
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Body<'a> {
    pub topology: Topology<'a>,
    pub displacement: Option<&'a [f64]>,
    pub scale: f64,
    pub mask: Option<(Sites<'a>, f64)>,
    pub colour: Option<Sites<'a>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Triangles {
    pub points: Vec<[[f64; 3]; 3]>,
    pub values: Vec<[f64; 3]>,
    pub outline: Vec<[[f64; 3]; 2]>,
}

impl Triangles {
    #[must_use]
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in self.points.iter().flatten() {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        (lo[0] <= hi[0]).then_some((lo, hi))
    }

    #[must_use]
    pub fn value_range(&self) -> Option<(f64, f64)> {
        let (lo, hi) = self
            .values
            .iter()
            .flatten()
            .filter(|v| v.is_finite())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| (l.min(*v), h.max(*v)));
        (lo <= hi).then_some((lo, hi))
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn lerp(a: [f64; 3], b: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] + s * (b[0] - a[0]), a[1] + s * (b[1] - a[1]), a[2] + s * (b[2] - a[2])]
}

fn sorted3(mut f: [usize; 3]) -> [usize; 3] {
    f.sort_unstable();
    f
}

fn invalid(m: impl Into<String>) -> RenderError {
    RenderError::Invalid(m.into())
}

const PERM3: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
const PERM2: [[usize; 2]; 2] = [[0, 1], [1, 0]];

impl Body<'_> {
    #[must_use]
    pub fn dims(&self) -> usize {
        match self.topology {
            Topology::Grid { nodes, .. } => nodes.len(),
            Topology::Mesh { dims, .. } => dims,
        }
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        match self.topology {
            Topology::Grid { nodes, .. } => nodes.iter().product(),
            Topology::Mesh { dims, points, .. } => points.len() / dims.max(1),
        }
    }

    #[must_use]
    pub fn cell_count(&self) -> usize {
        match self.topology {
            Topology::Grid { nodes, .. } => nodes.iter().map(|n| n.saturating_sub(1)).product(),
            Topology::Mesh { dims, cells, .. } => cells.len() / (dims + 1),
        }
    }

    fn simplices_per_cell(&self) -> usize {
        match (self.topology, self.dims()) {
            (Topology::Grid { .. }, 3) => 6,
            (Topology::Grid { .. }, _) => 2,
            (Topology::Mesh { .. }, _) => 1,
        }
    }



    pub fn check(&self) -> Result<(), RenderError> {
        let d = self.dims();
        if !(2..=3).contains(&d) {
            return Err(invalid("a solid body is 2-D or 3-D"));
        }
        match self.topology {
            Topology::Grid { nodes, spacing, origin } => {
                if nodes.iter().any(|n| *n < 2) || spacing.len() != d || origin.len() != d {
                    return Err(invalid(
                        "a grid body needs two nodes per axis and a spacing and origin per axis",
                    ));
                }
            }
            Topology::Mesh { dims, points, cells } => {
                let n = points.len() / dims;
                if points.len() % dims != 0 || cells.len() % (dims + 1) != 0 || cells.iter().any(|c| *c >= n)
                {
                    return Err(invalid(
                        "a mesh body needs whole points and cells with node indices in range",
                    ));
                }
            }
        }
        let (nodes, cells) = (self.node_count(), self.cell_count());
        if cells.saturating_mul(self.simplices_per_cell()) > MAX_SIMPLICES {
            return Err(invalid(format!("a solid body has at most {MAX_SIMPLICES} simplices")));
        }
        if self.displacement.is_some_and(|u| u.len() != nodes * d) || !self.scale.is_finite() {
            return Err(invalid(format!(
                "the body displacement needs {d} components per node and a finite scale"
            )));
        }
        for s in [self.mask.map(|m| m.0), self.colour].into_iter().flatten() {
            let ok = match s {
                Sites::Nodes(v) => v.len() == nodes,
                Sites::Cells(v) => v.len() == cells,
            };
            if !ok {
                return Err(invalid("body mask and colour values need one value per node or per cell"));
            }
        }
        Ok(())
    }

    fn reference(&self, n: usize) -> [f64; 3] {
        match self.topology {
            Topology::Grid { nodes, spacing, origin } => {
                let mut out = [0.0; 3];
                let mut rest = n;
                for a in (0..nodes.len()).rev() {
                    let i = rest % nodes[a];
                    rest /= nodes[a];
                    out[a] = origin[a] + i as f64 * spacing[a];
                }
                out
            }
            Topology::Mesh { dims, points, .. } => {
                let mut out = [0.0; 3];
                out[..dims].copy_from_slice(&points[n * dims..(n + 1) * dims]);
                out
            }
        }
    }

    fn deformed(&self, n: usize) -> [f64; 3] {
        let mut p = self.reference(n);
        if let Some(u) = self.displacement {
            let d = self.dims();
            for a in 0..d {
                p[a] += self.scale * u[n * d + a];
            }
        }
        p
    }

    fn grid_corners(nodes: &[usize], cell: usize) -> ([usize; 8], usize) {
        let d = nodes.len();
        let mut idx = [0usize; 3];
        let mut rest = cell;
        for a in (0..d).rev() {
            let n = nodes[a] - 1;
            idx[a] = rest % n;
            rest /= n;
        }
        let mut corners = [0usize; 8];
        for (c, corner) in corners.iter_mut().enumerate().take(1 << d) {
            *corner = (0..d).fold(0, |acc, a| acc * nodes[a] + idx[a] + ((c >> a) & 1));
        }
        (corners, 1 << d)
    }

    fn cell_nodes(&self, cell: usize) -> ([usize; 8], usize) {
        match self.topology {
            Topology::Grid { nodes, .. } => Self::grid_corners(nodes, cell),
            Topology::Mesh { dims, cells, .. } => {
                let k = dims + 1;
                let mut out = [0usize; 8];
                out[..k].copy_from_slice(&cells[cell * k..(cell + 1) * k]);
                (out, k)
            }
        }
    }

    fn simplices(&self, cell: usize, out: &mut Vec<[usize; 4]>) {
        out.clear();
        match self.topology {
            Topology::Grid { nodes, .. } => {
                let (c, _) = Self::grid_corners(nodes, cell);
                if nodes.len() == 3 {
                    for p in PERM3 {
                        let (a, b) = (1 << p[0], (1 << p[0]) | (1 << p[1]));
                        out.push([c[0], c[a], c[b], c[7]]);
                    }
                } else {
                    for p in PERM2 {
                        out.push([c[0], c[1 << p[0]], c[3], 0]);
                    }
                }
            }
            Topology::Mesh { dims, cells, .. } => {
                let k = dims + 1;
                let mut s = [0usize; 4];
                s[..k].copy_from_slice(&cells[cell * k..(cell + 1) * k]);
                out.push(s);
            }
        }
    }

    fn kept(&self, cell: usize) -> bool {
        match self.mask {
            None => true,
            Some((Sites::Cells(v), t)) => v[cell] >= t,
            Some((Sites::Nodes(v), t)) => {
                let (nodes, k) = self.cell_nodes(cell);
                nodes[..k].iter().map(|n| v[*n]).sum::<f64>() / k as f64 >= t
            }
        }
    }

    fn value(&self, cell: usize, node: usize) -> f64 {
        match self.colour {
            None => f64::NAN,
            Some(Sites::Nodes(v)) => v[node],
            Some(Sites::Cells(v)) => v[cell],
        }
    }



    #[allow(clippy::too_many_lines)]
    pub fn section(&self, point: [f64; 3], normal: [f64; 3]) -> Result<Triangles, RenderError> {
        self.check()?;
        let mut out = Triangles::default();
        let mut simplices = Vec::with_capacity(6);
        if self.dims() == 2 {
            let mut edges: HashMap<[usize; 2], u32> = HashMap::new();
            let mut kept = Vec::new();
            for cell in 0..self.cell_count() {
                if !self.kept(cell) {
                    continue;
                }
                self.simplices(cell, &mut simplices);
                for s in &simplices {
                    for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                        let e = if s[i] < s[j] { [s[i], s[j]] } else { [s[j], s[i]] };
                        *edges.entry(e).or_default() += 1;
                    }
                    kept.push((cell, *s));
                }
            }
            for (cell, s) in kept {
                out.points.push([self.deformed(s[0]), self.deformed(s[1]), self.deformed(s[2])]);
                out.values.push([self.value(cell, s[0]), self.value(cell, s[1]), self.value(cell, s[2])]);
            }
            let mut boundary: Vec<[usize; 2]> =
                edges.into_iter().filter(|(_, c)| *c == 1).map(|(e, _)| e).collect();
            boundary.sort_unstable();
            out.outline = boundary.into_iter().map(|[a, b]| [self.deformed(a), self.deformed(b)]).collect();
            return Ok(out);
        }
        let nn = dot(normal, normal).sqrt();
        if !(nn > 1e-300 && nn.is_finite()) {
            return Err(invalid("the section normal must not vanish"));
        }
        let n = normal.map(|x| x / nn);
        let dist = |node: usize| dot(sub(self.reference(node), point), n);

        let mut crossing: Vec<(usize, [usize; 4], [f64; 4])> = Vec::new();
        for cell in 0..self.cell_count() {
            let (nodes, k) = self.cell_nodes(cell);
            let (mut below, mut above) = (false, false);
            for node in &nodes[..k] {
                if dist(*node) > 0.0 {
                    above = true;
                } else {
                    below = true;
                }
            }
            if !below || !above || !self.kept(cell) {
                continue;
            }
            self.simplices(cell, &mut simplices);
            for s in &simplices {
                let d = s.map(dist);
                let pos = d.iter().filter(|x| **x > 0.0).count();
                if pos > 0 && pos < 4 {
                    crossing.push((cell, *s, d));
                }
            }
        }

        let mut faces: HashMap<[usize; 3], u32> = HashMap::with_capacity(crossing.len() * 4);
        for (_, s, _) in &crossing {
            for f in [[s[1], s[2], s[3]], [s[0], s[2], s[3]], [s[0], s[1], s[3]], [s[0], s[1], s[2]]] {
                *faces.entry(sorted3(f)).or_default() += 1;
            }
        }
        for (cell, s, d) in &crossing {
            let cut = |i: usize, j: usize| -> ([f64; 3], f64) {
                let t = d[i] / (d[i] - d[j]);
                let (vi, vj) = (self.value(*cell, s[i]), self.value(*cell, s[j]));
                (lerp(self.deformed(s[i]), self.deformed(s[j]), t), vi + t * (vj - vi))
            };
            let pos: Vec<usize> = (0..4).filter(|i| d[*i] > 0.0).collect();
            let neg: Vec<usize> = (0..4).filter(|i| d[*i] <= 0.0).collect();

            let edges: Vec<(usize, usize)> = match (pos.len(), neg.len()) {
                (1, 3) => neg.iter().map(|j| (pos[0], *j)).collect(),
                (3, 1) => pos.iter().map(|j| (neg[0], *j)).collect(),
                _ => vec![(pos[0], neg[0]), (pos[0], neg[1]), (pos[1], neg[1]), (pos[1], neg[0])],
            };
            let corners: Vec<([f64; 3], f64)> = edges.iter().map(|(i, j)| cut(*i, *j)).collect();
            for k in 1..corners.len() - 1 {
                out.points.push([corners[0].0, corners[k].0, corners[k + 1].0]);
                out.values.push([corners[0].1, corners[k].1, corners[k + 1].1]);
            }
            for k in 0..edges.len() {
                let (e1, e2) = (edges[k], edges[(k + 1) % edges.len()]);
                let mut f = [s[e1.0], s[e1.1], s[e2.0], s[e2.1]];
                f.sort_unstable();
                let mut face = [f[0], 0, 0];
                let mut m = 1;
                for x in &f[1..] {
                    if *x != face[m - 1] && m < 3 {
                        face[m] = *x;
                        m += 1;
                    }
                }
                if m == 3 && faces.get(&face) == Some(&1) {
                    out.outline.push([corners[k].0, corners[(k + 1) % edges.len()].0]);
                }
            }
        }
        Ok(out)
    }



    pub fn surface(&self, max_triangles: usize) -> Result<Triangles, RenderError> {
        self.check()?;
        if self.dims() != 3 {
            return Err(invalid("a body surface needs a 3-D body"));
        }
        let mut out = Triangles::default();
        let push = |out: &mut Triangles, cell: usize, f: [usize; 3]| -> Result<(), RenderError> {
            if out.points.len() >= max_triangles {
                return Err(invalid(format!("the body surface exceeds {max_triangles} triangles")));
            }
            out.points.push(f.map(|n| self.deformed(n)));
            out.values.push(f.map(|n| self.value(cell, n)));
            Ok(())
        };
        match self.topology {
            Topology::Grid { nodes, .. } => {
                let cells = [nodes[0] - 1, nodes[1] - 1, nodes[2] - 1];
                let kept: Vec<bool> = (0..self.cell_count()).map(|c| self.kept(c)).collect();
                for (cell, _) in kept.iter().enumerate().filter(|(_, k)| **k) {
                    let idx = [cell / (cells[1] * cells[2]), (cell / cells[2]) % cells[1], cell % cells[2]];
                    let (corner, _) = Self::grid_corners(nodes, cell);
                    for a in 0..3 {
                        let (b, c) = match a {
                            0 => (1, 2),
                            1 => (0, 2),
                            _ => (0, 1),
                        };
                        for side in 0..2usize {
                            let neighbour = if side == 1 { idx[a] + 1 < cells[a] } else { idx[a] > 0 };
                            if neighbour {
                                let mut j = idx;
                                if side == 1 {
                                    j[a] += 1;
                                } else {
                                    j[a] -= 1;
                                }
                                if kept[(j[0] * cells[1] + j[1]) * cells[2] + j[2]] {
                                    continue;
                                }
                            }
                            let q = |ub: usize, uc: usize| corner[(side << a) | (ub << b) | (uc << c)];
                            let (q00, q10, q11, q01) = (q(0, 0), q(1, 0), q(1, 1), q(0, 1));
                            let outward = (side == 1) == (a != 1);
                            if outward {
                                push(&mut out, cell, [q00, q10, q11])?;
                                push(&mut out, cell, [q00, q11, q01])?;
                            } else {
                                push(&mut out, cell, [q00, q11, q10])?;
                                push(&mut out, cell, [q00, q01, q11])?;
                            }
                        }
                    }
                }
            }
            Topology::Mesh { .. } => {
                let mut faces: HashMap<[usize; 3], (u32, usize, [usize; 3])> = HashMap::new();
                let mut simplices = Vec::with_capacity(1);
                for cell in 0..self.cell_count() {
                    if !self.kept(cell) {
                        continue;
                    }
                    self.simplices(cell, &mut simplices);
                    let s = simplices[0];
                    for opposite in 0..4 {
                        let mut f = [0usize; 3];
                        let mut m = 0;
                        for (i, node) in s.iter().enumerate() {
                            if i != opposite {
                                f[m] = *node;
                                m += 1;
                            }
                        }
                        let (p0, p1, p2) = (self.reference(f[0]), self.reference(f[1]), self.reference(f[2]));
                        let normal = cross(sub(p1, p0), sub(p2, p0));
                        if dot(normal, sub(self.reference(s[opposite]), p0)) > 0.0 {
                            f.swap(1, 2);
                        }
                        let e = faces.entry(sorted3(f)).or_insert((0, cell, f));
                        e.0 += 1;
                    }
                }
                let mut boundary: Vec<(usize, [usize; 3])> =
                    faces.into_values().filter(|(n, _, _)| *n == 1).map(|(_, c, f)| (c, f)).collect();
                boundary.sort_unstable();
                for (cell, f) in boundary {
                    push(&mut out, cell, f)?;
                }
            }
        }
        Ok(out)
    }
}

