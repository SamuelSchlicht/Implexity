// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Value, json};

use crate::topology::{Vec3, cross, dot, norm};

#[derive(Clone, Debug, PartialEq)]
pub struct Bound {
    pub edges: Vec<(usize, bool)>,
    pub orientation: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub point: Vec3,
    pub normal: Vec3,
    pub same_sense: bool,
    pub bounds: Vec<Bound>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shell {
    pub faces: Vec<usize>,
    pub reversed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Solid {
    pub shells: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Brep {
    pub points: Vec<Vec3>,
    pub edges: Vec<[usize; 2]>,
    pub faces: Vec<Face>,
    pub shells: Vec<Shell>,
    pub solids: Vec<Solid>,
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

impl Brep {
    #[must_use]
    pub fn loop_vertices(&self, shell: &Shell, bound: &Bound) -> Vec<usize> {
        let forward = bound.orientation != shell.reversed;
        let directed = |&(e, fwd): &(usize, bool)| {
            let [a, b] = self.edges[e];
            if fwd == forward { [a, b] } else { [b, a] }
        };
        if forward {
            bound.edges.iter().map(|x| directed(x)[0]).collect()
        } else {
            bound.edges.iter().rev().map(|x| directed(x)[0]).collect()
        }
    }

    fn directed_edges(&self, shell: &Shell, bound: &Bound) -> Vec<(usize, [usize; 2])> {
        let forward = bound.orientation != shell.reversed;
        let dir = |&(e, fwd): &(usize, bool)| {
            let [a, b] = self.edges[e];
            (e, if fwd == forward { [a, b] } else { [b, a] })
        };
        if forward {
            bound.edges.iter().map(dir).collect()
        } else {
            bound.edges.iter().rev().map(dir).collect()
        }
    }

    #[must_use]
    pub fn effective_normal(shell: &Shell, face: &Face) -> Vec3 {
        let s = if face.same_sense == shell.reversed { -1.0 } else { 1.0 };
        [face.normal[0] * s, face.normal[1] * s, face.normal[2] * s]
    }

    fn shell_faces<'a>(&'a self, shells: &'a [usize]) -> impl Iterator<Item = (&'a Shell, &'a Face)> + 'a {
        shells
            .iter()
            .flat_map(move |&s| self.shells[s].faces.iter().map(move |&f| (&self.shells[s], &self.faces[f])))
    }

    #[must_use]
    pub fn solid_shells(&self) -> Vec<usize> {
        self.solids.iter().flat_map(|s| s.shells.iter().copied()).collect()
    }

    #[must_use]
    pub fn volume_of_shells(&self, shells: &[usize]) -> f64 {
        mass_properties(self, shells).0
    }
}

fn mass_properties(brep: &Brep, shells: &[usize]) -> (f64, Vec3, f64) {
    let Some(r) = brep.points.first().copied() else {
        return (0.0, [0.0; 3], 0.0);
    };
    let (mut v6, mut c24, mut area) = (0.0, [0.0; 3], 0.0);
    for (shell, face) in brep.shell_faces(shells) {
        let mut vec_area = [0.0; 3];
        for bound in &face.bounds {
            let lp = brep.loop_vertices(shell, bound);
            if lp.len() < 3 {
                continue;
            }
            let q0 = sub(brep.points[lp[0]], r);
            for w in lp[1..].windows(2) {
                let qi = sub(brep.points[w[0]], r);
                let qj = sub(brep.points[w[1]], r);
                let t = dot(q0, cross(qi, qj));
                v6 += t;
                for a in 0..3 {
                    c24[a] += t * (q0[a] + qi[a] + qj[a]);
                }
                let c = cross(sub(qi, q0), sub(qj, q0));
                for a in 0..3 {
                    vec_area[a] += 0.5 * c[a];
                }
            }
        }
        area += norm(vec_area);
    }
    let volume = v6 / 6.0;
    let centroid = if v6 == 0.0 { r } else { std::array::from_fn(|a| r[a] + c24[a] / (4.0 * v6)) };
    (volume, centroid, area)
}

#[must_use]
pub fn shell_closure(brep: &Brep, shells: &[usize]) -> BTreeMap<&'static str, Value> {
    let mut users: HashMap<usize, HashSet<usize>> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();
    for &s in shells {
        for &f in &brep.shells[s].faces {
            for b in &brep.faces[f].bounds {
                for &(e, _) in &b.edges {
                    let set = users.entry(e).or_insert_with(|| {
                        order.push(e);
                        HashSet::new()
                    });
                    set.insert(f);
                }
            }
        }
    }
    let (mut n1, mut n2, mut nmore) = (0usize, 0usize, 0usize);
    for e in &order {
        match users[e].len() {
            1 => n1 += 1,
            2 => n2 += 1,
            _ => nmore += 1,
        }
    }
    BTreeMap::from([
        ("shell_edges", json!(order.len())),
        ("shell_edges_with_one_face", json!(n1)),
        ("shell_edges_with_two_faces", json!(n2)),
        ("shell_edges_with_more_faces", json!(nmore)),
        ("shell_closed_measured", json!(n1 == 0 && nmore == 0)),
    ])
}

#[derive(Default)]
struct Failures {
    kinds: BTreeMap<&'static str, (usize, Vec<&'static str>)>,
}

impl Failures {
    fn add(&mut self, kind: &'static str, status: &'static str) {
        let e = self.kinds.entry(kind).or_default();
        e.0 += 1;
        if e.1.len() < 3 {
            e.1.push(status);
        }
    }

    fn to_json(&self) -> Value {
        let mut out = Vec::new();
        for kind in ["shell", "face", "wire", "edge", "vertex"] {
            if let Some((n, first)) = self.kinds.get(kind) {
                out.push(json!({"kind": kind, "invalid": n, "first_statuses": first}));
            }
        }
        Value::Array(out)
    }
}

#[must_use]
pub fn plane_frame(n: Vec3) -> (Vec3, Vec3) {
    let t = if n[0].abs() > 0.9 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
    let u = cross(n, t);
    let l = norm(u);
    let u = [u[0] / l, u[1] / l, u[2] / l];
    (u, cross(n, u))
}

fn segments_cross(p: [f64; 2], q: [f64; 2], r: [f64; 2], s: [f64; 2], tol: f64) -> bool {
    let orient =
        |a: [f64; 2], b: [f64; 2], c: [f64; 2]| (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    let scale = |a: [f64; 2], b: [f64; 2]| ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let (lpq, lrs) = (scale(p, q), scale(r, s));
    let d1 = orient(p, q, r);
    let d2 = orient(p, q, s);
    let d3 = orient(r, s, p);
    let d4 = orient(r, s, q);
    let (t1, t2) = (tol * lpq, tol * lrs);
    if ((d1 > t1 && d2 < -t1) || (d1 < -t1 && d2 > t1)) && ((d3 > t2 && d4 < -t2) || (d3 < -t2 && d4 > t2)) {
        return true;
    }

    if d1.abs() <= t1 && d2.abs() <= t1 && lpq > 0.0 {
        let dir = [(q[0] - p[0]) / lpq, (q[1] - p[1]) / lpq];
        let proj = |x: [f64; 2]| (x[0] - p[0]) * dir[0] + (x[1] - p[1]) * dir[1];
        let (a, b) = (proj(r).min(proj(s)), proj(r).max(proj(s)));
        let overlap = b.min(lpq) - a.max(0.0);
        return overlap > tol.max(1e-12) * lpq.max(1.0);
    }
    false
}

fn loops_intersect(pts2: &[[f64; 2]], segs: &[[usize; 2]]) -> bool {
    let bbox = |s: &[usize; 2]| {
        let (a, b) = (pts2[s[0]], pts2[s[1]]);
        [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
    };
    let boxes: Vec<[f64; 4]> = segs.iter().map(bbox).collect();
    let mut idx: Vec<usize> = (0..segs.len()).collect();
    idx.sort_by(|&a, &b| boxes[a][0].total_cmp(&boxes[b][0]));
    let span = boxes.iter().fold(0.0f64, |m, b| m.max((b[2] - b[0]).abs()).max((b[3] - b[1]).abs()));
    let tol = 1e-12 * span.max(1e-300);
    for (k, &i) in idx.iter().enumerate() {
        for &j in &idx[k + 1..] {
            if boxes[j][0] > boxes[i][2] + tol {
                break;
            }
            if boxes[j][1] > boxes[i][3] + tol || boxes[i][1] > boxes[j][3] + tol {
                continue;
            }
            let (si, sj) = (segs[i], segs[j]);
            if si[0] == sj[0] || si[0] == sj[1] || si[1] == sj[0] || si[1] == sj[1] {
                continue;
            }
            if segments_cross(pts2[si[0]], pts2[si[1]], pts2[sj[0]], pts2[sj[1]], 1e-12) {
                return true;
            }
        }
    }
    false
}


#[must_use]
pub fn check_solid(brep: &Brep, confusion_mm: f64) -> Value {
    let mut fails = Failures::default();
    for e in &brep.edges {
        if norm(sub(brep.points[e[1]], brep.points[e[0]])) <= 0.0 {
            fails.add("edge", "BRepCheck_InvalidDegeneratedFlag");
        }
    }
    for solid in &brep.solids {
        for (k, &s) in solid.shells.iter().enumerate() {
            let shell = &brep.shells[s];
            check_shell(brep, shell, confusion_mm, &mut fails);
            let v = brep.volume_of_shells(&[s]);
            let ok = if k == 0 { v > 0.0 } else { v < 0.0 };
            if !ok {
                fails.add("shell", "BRepCheck_BadOrientationOfSubshape");
            }
        }
    }
    let shells = brep.solid_shells();
    let (volume, centroid, area) = mass_properties(brep, &shells);
    let valid = fails.kinds.is_empty();
    let mut out = serde_json::Map::new();
    out.insert("brepcheck_valid".into(), json!(valid));
    if !valid {
        out.insert("brepcheck_failures".into(), fails.to_json());
    }
    out.insert("volume_mm3".into(), json!(volume));
    out.insert("centroid_mm".into(), json!(centroid));
    out.insert("area_mm2".into(), json!(area));
    let n_faces: usize = shells.iter().map(|&s| brep.shells[s].faces.len()).sum();
    let n_edges: usize = shells
        .iter()
        .flat_map(|&s| brep.shells[s].faces.iter())
        .flat_map(|&f| brep.faces[f].bounds.iter())
        .map(|b| b.edges.len())
        .sum();
    out.insert("n_solids".into(), json!(brep.solids.len()));
    out.insert("n_shells".into(), json!(shells.len()));
    out.insert("n_faces".into(), json!(n_faces));
    out.insert("n_edges".into(), json!(n_edges));
    out.insert("n_vertices".into(), json!(2 * n_edges));
    Value::Object(out)
}

fn check_shell(brep: &Brep, shell: &Shell, confusion_mm: f64, fails: &mut Failures) {

    let mut uses: HashMap<usize, Vec<(usize, bool)>> = HashMap::new();
    for &f in &shell.faces {
        let face = &brep.faces[f];
        check_face(brep, shell, face, confusion_mm, fails);
        for bound in &face.bounds {
            for (e, d) in brep.directed_edges(shell, bound) {
                uses.entry(e).or_default().push((f, d == brep.edges[e]));
            }
        }
    }
    let mut not_closed = false;
    let mut bad_orientation = false;
    let mut uf = crate::topology::UnionFind::new(brep.faces.len());
    for list in uses.values() {
        if list.len() == 2 {
            if list[0].1 == list[1].1 {
                bad_orientation = true;
            }
            uf.union(list[0].0, list[1].0);
        } else {
            not_closed = true;
        }
    }
    if not_closed {
        fails.add("shell", "BRepCheck_NotClosed");
    }
    if bad_orientation {
        fails.add("shell", "BRepCheck_BadOrientationOfSubshape");
    }
    if let Some(&first) = shell.faces.first() {
        let root = uf.find(first);
        if shell.faces.iter().any(|&f| uf.find(f) != root) {
            fails.add("shell", "BRepCheck_NotConnected");
        }
    }
}

fn check_face(brep: &Brep, shell: &Shell, face: &Face, confusion_mm: f64, fails: &mut Failures) {
    let n = Brep::effective_normal(shell, face);
    let (u, w) = plane_frame(face.normal);
    let mut pts2: HashMap<usize, usize> = HashMap::new();
    let mut coords: Vec<[f64; 2]> = Vec::new();
    let mut segs: Vec<[usize; 2]> = Vec::new();
    let mut face_ok = true;
    for (k, bound) in face.bounds.iter().enumerate() {
        let directed = brep.directed_edges(shell, bound);
        let m = directed.len();
        if m < 3 || (0..m).any(|i| directed[i].1[1] != directed[(i + 1) % m].1[0]) {
            fails.add("wire", "BRepCheck_NotClosed");
            face_ok = false;
            continue;
        }
        let mut vec_area = [0.0; 3];
        let q0 = brep.points[directed[0].1[0]];
        for (_, [a, b]) in &directed {
            let (pa, pb) = (brep.points[*a], brep.points[*b]);
            let c = cross(sub(pa, q0), sub(pb, q0));
            for x in 0..3 {
                vec_area[x] += 0.5 * c[x];
            }
            let off = dot(sub(pa, face.point), face.normal).abs();
            if off > confusion_mm {
                fails.add("vertex", "BRepCheck_InvalidPointOnSurface");
            }
            let mut id = |v: usize| {
                *pts2.entry(v).or_insert_with(|| {
                    let d = sub(brep.points[v], face.point);
                    coords.push([dot(d, u), dot(d, w)]);
                    coords.len() - 1
                })
            };
            let (ia, ib) = (id(*a), id(*b));
            segs.push([ia, ib]);
        }
        let signed = dot(vec_area, n);
        let ok = if k == 0 { signed > 0.0 } else { signed < 0.0 };
        if !ok {
            fails.add("face", "BRepCheck_BadOrientationOfSubshape");
            face_ok = false;
        }
    }
    if face_ok && segs.len() > 3 && loops_intersect(&coords, &segs) {
        fails.add("wire", "BRepCheck_SelfIntersectingWire");
    }
}

#[must_use]
pub fn point_inside(points: &[Vec3], tris: &[[usize; 3]], p: Vec3) -> bool {
    const DIRS: [Vec3; 4] = [
        [0.577_215_664_901_532_9, 0.314_159_265_358_979_3, 0.754_877_666_246_692_7],
        [-0.271_828_182_845_904_5, 0.618_033_988_749_894_8, 0.141_421_356_237_309_5],
        [0.173_205_080_756_887_7, -0.223_606_797_749_979, 0.618_033_988_749_894_8],
        [-0.331_662_479_035_539_9, -0.477_121_254_719_662_4, -0.845_098_040_014_256_8],
    ];
    'dirs: for d in DIRS {
        let mut hits = 0usize;
        for t in tris {
            let (a, b, c) = (points[t[0]], points[t[1]], points[t[2]]);
            let e1 = sub(b, a);
            let e2 = sub(c, a);
            let pv = cross(d, e2);
            let det = dot(e1, pv);
            let scale = norm(e1) * norm(e2) * norm(d);
            if det.abs() <= 1e-14 * scale {

                if dot(sub(p, a), cross(e1, e2)).abs() <= 1e-14 * scale * norm(sub(p, a)).max(1.0) {
                    continue 'dirs;
                }
                continue;
            }
            let inv = 1.0 / det;
            let s = sub(p, a);
            let bu = dot(s, pv) * inv;
            let q = cross(s, e1);
            let bv = dot(d, q) * inv;
            let t_hit = dot(e2, q) * inv;
            let eps = 1e-12;
            if bu < -eps || bv < -eps || bu + bv > 1.0 + eps || t_hit < -eps {
                continue;
            }
            if bu < eps || bv < eps || bu + bv > 1.0 - eps || t_hit < eps {
                continue 'dirs;
            }
            hits += 1;
        }
        return hits % 2 == 1;
    }
    false
}
