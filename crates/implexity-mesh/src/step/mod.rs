// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




pub mod brep;
pub mod p21;

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::MeshError;
use crate::numeric::{pairwise_sum, py_round_digits};
use crate::pyfmt::{fmt_e, fmt_g};
use crate::topology::{Tri, Vec3, cross, dot, norm};

use brep::{Bound, Brep, Face, Shell, Solid};

pub const DEFAULT_MAX_STEP_TRIANGLES: usize = 150_000;
pub const DEFAULT_ANGLE_TOL_DEG: f64 = 0.5;
pub const DEFAULT_PLANE_TOL_MM: f64 = 1e-9;
pub const CONFUSION_MM: f64 = 1e-7;
pub const DEFAULT_VOLUME_GUARD_REL: f64 = 1e-9;
pub const SCHEMAS: [(&str, &str); 3] = [("AP214", "AP214IS"), ("AP203", "AP203"), ("AP242", "AP242DIS")];
pub const DEFAULT_SCHEMA: &str = "AP214";
pub const FORMATS: [&str; 1] = ["step"];
pub const NATIVE_CHECKER: &str =
    "implexity-mesh native B-rep check (closure, orientation, planarity, wire intersection, connectivity)";

#[must_use]
pub fn max_step_triangles() -> usize {
    std::env::var("IMPLEXITY_MAX_STEP_TRIANGLES")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map_or(DEFAULT_MAX_STEP_TRIANGLES, crate::cast::trunc_usize)
}

#[must_use]
pub fn have_writer() -> (bool, &'static str) {
    (true, "native ISO 10303-21 planar B-rep writer (implexity-mesh)")
}

pub type Adjacency = (Vec<[i64; 3]>, Vec<[usize; 3]>, Vec<[usize; 2]>);

#[must_use]
pub fn face_adjacency(faces: &[Tri]) -> Adjacency {
    let nf = faces.len();
    let mut keyed: Vec<([usize; 2], usize)> = Vec::with_capacity(nf * 3);
    for (f, t) in faces.iter().enumerate() {
        for s in 0..3 {
            let (a, b) = (t[s], t[(s + 1) % 3]);
            keyed.push(([a.min(b), a.max(b)], 3 * f + s));
        }
    }
    keyed.sort_unstable();
    let mut nbr = vec![[-1i64; 3]; nf];
    let mut eid = vec![[0usize; 3]; nf];
    let mut uniq: Vec<[usize; 2]> = Vec::new();
    let mut i = 0;
    while i < keyed.len() {
        let mut j = i;
        while j < keyed.len() && keyed[j].0 == keyed[i].0 {
            j += 1;
        }
        let e = uniq.len();
        uniq.push(keyed[i].0);
        for k in &keyed[i..j] {
            eid[k.1 / 3][k.1 % 3] = e;
        }
        if j - i == 2 {
            let (a, b) = (keyed[i].1, keyed[i + 1].1);
            nbr[a / 3][a % 3] = crate::cast::i64_of(b / 3);
            nbr[b / 3][b % 3] = crate::cast::i64_of(a / 3);
        }
        i = j;
    }
    (nbr, eid, uniq)
}

#[must_use]
pub fn check_mesh(faces: &[Tri]) -> Value {
    let mut directed: Vec<[usize; 2]> = Vec::with_capacity(faces.len() * 3);
    for t in faces {
        directed.extend([[t[0], t[1]], [t[1], t[2]], [t[2], t[0]]]);
    }
    let mut und: Vec<[usize; 2]> = directed.iter().map(|e| [e[0].min(e[1]), e[0].max(e[1])]).collect();
    directed.sort_unstable();
    und.sort_unstable();
    let counts = |v: &[[usize; 2]]| {
        let mut out = Vec::new();
        let mut i = 0;
        while i < v.len() {
            let mut j = i;
            while j < v.len() && v[j] == v[i] {
                j += 1;
            }
            out.push(j - i);
            i = j;
        }
        out
    };
    let cd = counts(&directed);
    let cs = counts(&und);
    let twice = cd.iter().filter(|&&c| c > 1).count();
    let boundary = cs.iter().filter(|&&c| c == 1).count();
    let nonmanifold = cs.iter().filter(|&&c| c > 2).count();
    json!({
        "directed_edges_seen_twice": twice, "boundary_edges": boundary,
        "nonmanifold_edges": nonmanifold, "orientation_consistent": twice == 0,
        "closed": boundary == 0 && nonmanifold == 0,
    })
}

#[must_use]
pub fn face_components(faces: &[Tri], nv: usize) -> (Vec<usize>, usize) {
    let (cid, count, _labels) = crate::topology::face_components(faces, nv);
    let mut used = vec![false; count];
    for &c in &cid {
        used[c] = true;
    }
    let mut remap = vec![0usize; count];
    let mut k = 0;
    for (i, u) in used.iter().enumerate() {
        if *u {
            remap[i] = k;
            k += 1;
        }
    }
    (cid.iter().map(|&c| remap[c]).collect(), k)
}

#[must_use]
pub fn component_volumes(
    points: &[Vec3],
    faces: &[Tri],
    cid: &[usize],
    ncomp: usize,
    unit_scale: f64,
) -> Vec<f64> {
    let mut out = vec![0.0; ncomp];
    for (t, &c) in faces.iter().zip(cid) {
        out[c] += dot(points[t[0]], cross(points[t[1]], points[t[2]])) / 6.0;
    }
    let s3 = unit_scale.powi(3);
    out.iter().map(|v| v * s3).collect()
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[must_use]
pub fn triangle_normals(points: &[Vec3], faces: &[Tri]) -> (Vec<Vec3>, Vec<f64>) {
    let mut nrm = Vec::with_capacity(faces.len());
    let mut area = Vec::with_capacity(faces.len());
    for t in faces {
        let p0 = points[t[0]];
        let n = cross(sub(points[t[1]], p0), sub(points[t[2]], p0));
        let l = norm(n);
        nrm.push(if l > 0.0 { [n[0] / l, n[1] / l, n[2] / l] } else { [0.0; 3] });
        area.push(0.5 * l);
    }
    (nrm, area)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Regions {
    pub label: Vec<i64>,
    pub point: Vec<Vec3>,
    pub normal: Vec<Vec3>,
    pub area: Vec<f64>,
}

impl Regions {
    #[must_use]
    pub fn count(&self) -> usize {
        self.point.len()
    }
}

fn slot(label: i64, n: usize) -> usize {
    if label < 0 { n - 1 } else { crate::cast::idx(label) }
}

#[must_use]
pub fn plane_regions(
    points: &[Vec3],
    faces: &[Tri],
    nbr: &[[i64; 3]],
    angle_tol_deg: f64,
    plane_tol_mm: f64,
    unit_scale: f64,
) -> Regions {
    let nf = faces.len();
    let (nrm, ar) = triangle_normals(points, faces);
    let cosmin = (angle_tol_deg * (std::f64::consts::PI / 180.0)).cos();
    let dtol = plane_tol_mm / unit_scale;
    let mut label = vec![-1i64; nf];
    let mut order: Vec<usize> = (0..nf).collect();
    order.sort_by(|&a, &b| (-ar[a]).total_cmp(&(-ar[b])));
    let mut stack = Vec::new();
    let mut r: i64 = 0;
    for &s in &order {
        if label[s] >= 0 {
            continue;
        }
        let n0 = nrm[s];
        if n0.iter().all(|&x| x.abs() <= 0.0) {
            continue;
        }
        let d0 = dot(n0, points[faces[s][0]]);
        label[s] = r;
        stack.push(s);
        while let Some(f) = stack.pop() {
            for &g in &nbr[f] {
                if g < 0 {
                    continue;
                }
                let g = crate::cast::idx(g);
                if label[g] >= 0 || dot(nrm[g], n0) < cosmin {
                    continue;
                }
                let t = faces[g];
                if t.iter().any(|&v| (dot(points[v], n0) - d0).abs() > dtol) {
                    continue;
                }
                label[g] = r;
                stack.push(g);
            }
        }
        r += 1;
    }
    let nreg = crate::cast::idx(r);
    let mut pt = vec![[0.0; 3]; nreg];
    let mut nn = vec![[0.0; 3]; nreg];
    let mut aw = vec![0.0; nreg];
    if nreg > 0 {
        for f in 0..nf {
            let k = slot(label[f], nreg);
            let t = faces[f];
            let cen: Vec3 =
                std::array::from_fn(|a| (points[t[0]][a] + points[t[1]][a] + points[t[2]][a]) / 3.0);
            for a in 0..3 {
                nn[k][a] += nrm[f][a] * ar[f];
                pt[k][a] += cen[a] * ar[f];
            }
            aw[k] += ar[f];
        }
    }
    for ((p, n), w) in pt.iter_mut().zip(nn.iter_mut()).zip(&aw) {
        let w = w.max(1e-300);
        for x in p.iter_mut() {
            *x /= w;
        }
        let l = norm(*n).max(1e-300);
        for x in n.iter_mut() {
            *x /= l;
        }
    }
    Regions { label, point: pt, normal: nn, area: ar }
}

#[must_use]
pub fn region_deviation(points: &[Vec3], faces: &[Tri], regions: &Regions, unit_scale: f64) -> Vec<f64> {
    let n = regions.count();
    faces
        .iter()
        .zip(&regions.label)
        .map(|(t, &l)| {
            let k = slot(l, n);
            let (lp, ln) = (regions.point[k], regions.normal[k]);
            t.iter().fold(0.0f64, |m, &v| m.max(dot(sub(points[v], lp), ln).abs())) * unit_scale
        })
        .collect()
}

fn py_mod(a: f64, b: f64) -> f64 {
    let m = a % b;
    if m != 0.0 && ((b < 0.0) != (m < 0.0)) {
        m + b
    } else if m == 0.0 {
        0.0f64.copysign(b)
    } else {
        m
    }
}

#[must_use]
pub fn region_loops(
    faces: &[Tri],
    nbr: &[[i64; 3]],
    label: &[i64],
    region_faces: &[usize],
    plane_n: Vec3,
    points: &[Vec3],
) -> (Vec<Vec<usize>>, bool) {

    let mut keys: Vec<usize> = Vec::new();
    let mut outs: Vec<Vec<usize>> = Vec::new();
    let mut slot_of: HashMap<usize, usize> = HashMap::new();
    let mut nedge = 0usize;
    for &f in region_faces {
        let t = faces[f];
        for k in 0..3 {
            let g = nbr[f][k];
            if g >= 0 && label[crate::cast::idx(g)] == label[f] {
                continue;
            }
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let s = *slot_of.entry(a).or_insert_with(|| {
                keys.push(a);
                outs.push(Vec::new());
                keys.len() - 1
            });
            outs[s].push(b);
            nedge += 1;
        }
    }
    if nedge == 0 {
        return (Vec::new(), false);
    }
    let (u, w) = brep::plane_frame(plane_n);
    let ang = |v: Vec3| dot(v, w).atan2(dot(v, u));
    let mut loops = Vec::new();
    let mut used = 0usize;
    let mut cursor = 0usize;
    loop {
        while cursor < keys.len() && outs[cursor].is_empty() {
            cursor += 1;
        }
        if cursor >= keys.len() {
            break;
        }
        let start = keys[cursor];
        let mut lp = vec![start];
        let mut cur_a = start;
        let Some(mut cur_b) = outs[cursor].pop() else { break };
        used += 1;
        let mut closed = false;
        loop {
            if cur_b == start {
                closed = true;
                break;
            }
            lp.push(cur_b);
            let Some(&sb) = slot_of.get(&cur_b) else { break };
            let cand = &mut outs[sb];
            if cand.is_empty() {
                break;
            }
            let nxt = if cand.len() == 1 {
                cand.pop().unwrap_or(0)
            } else {
                let base = ang(sub(points[cur_a], points[cur_b]));
                let mut best: Option<f64> = None;
                let mut bi = 0;
                for (i, &c) in cand.iter().enumerate() {
                    let mut da =
                        py_mod(ang(sub(points[c], points[cur_b])) - base, 2.0 * std::f64::consts::PI);
                    if da <= 1e-12 {
                        da = 2.0 * std::f64::consts::PI;
                    }
                    if best.is_none_or(|b| da < b) {
                        best = Some(da);
                        bi = i;
                    }
                }
                cand.remove(bi)
            };
            used += 1;
            cur_a = cur_b;
            cur_b = nxt;
            if lp.len() > nedge + 2 {
                break;
            }
        }
        if !closed || lp.len() < 3 {
            return (loops, false);
        }
        loops.push(lp);
    }
    (loops, used == nedge)
}

#[must_use]
pub fn loop_area2(lp: &[usize], points: &[Vec3], pt: Vec3, n: Vec3) -> f64 {
    let (u, w) = brep::plane_frame(n);
    let m = lp.len();
    let x: Vec<f64> = lp.iter().map(|&v| dot(sub(points[v], pt), u)).collect();
    let y: Vec<f64> = lp.iter().map(|&v| dot(sub(points[v], pt), w)).collect();
    let (mut s1, mut s2) = (0.0, 0.0);
    for i in 0..m {
        s1 += x[i] * y[(i + 1) % m];
        s2 += y[i] * x[(i + 1) % m];
    }
    0.5 * (s1 - s2)
}

#[must_use]
pub fn mesh_volume_mm3(points: &[Vec3], faces: &[Tri], unit_scale: f64) -> f64 {
    let t: Vec<f64> = faces.iter().map(|t| dot(points[t[0]], cross(points[t[1]], points[t[2]]))).collect();
    pairwise_sum(&t) / 6.0 * unit_scale.powi(3)
}

fn peak_rss_mb() -> Value {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else { return Value::Null };
    text.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(Value::Null, |kb| json!(kb / 1024.0))
}

fn seconds_since(t0: Instant) -> f64 {
    py_round_digits(t0.elapsed().as_secs_f64(), 2)
}

#[derive(Clone, Debug)]
pub struct BuildOptions<'a> {
    pub merge: bool,
    pub angle_tol_deg: f64,
    pub plane_tol_mm: f64,
    pub unit_scale: f64,
    pub cap_mask: Option<&'a [bool]>,
    pub sew: bool,
}

impl Default for BuildOptions<'_> {
    fn default() -> Self {
        Self {
            merge: true,
            angle_tol_deg: DEFAULT_ANGLE_TOL_DEG,
            plane_tol_mm: DEFAULT_PLANE_TOL_MM,
            unit_scale: 1e3,
            cap_mask: None,
            sew: false,
        }
    }
}

struct Builder {
    brep: Brep,
    edge_of: HashMap<(usize, usize), usize>,
    point_of: HashMap<usize, usize>,
    wire_edges: usize,
    faces_out: usize,
}

impl Builder {
    fn point(&mut self, p: &[Vec3], i: usize) -> usize {
        *self.point_of.entry(i).or_insert_with(|| {
            self.brep.points.push(p[i]);
            self.brep.points.len() - 1
        })
    }
    fn edge(&mut self, p: &[Vec3], a: usize, b: usize) -> (usize, bool) {
        let k = (a.min(b), a.max(b));
        if let Some(&e) = self.edge_of.get(&k) {
            return (e, a < b);
        }
        let (pa, pb) = (self.point(p, k.0), self.point(p, k.1));
        self.brep.edges.push([pa, pb]);
        let e = self.brep.edges.len() - 1;
        self.edge_of.insert(k, e);
        (e, a < b)
    }
    fn wire(&mut self, p: &[Vec3], lp: &[usize]) -> Bound {
        let m = lp.len();
        let edges = (0..m).map(|q| self.edge(p, lp[q], lp[(q + 1) % m])).collect();
        self.wire_edges += m;
        Bound { edges, orientation: true }
    }
    fn add_face(&mut self, shell: usize, face: Face) {
        self.brep.faces.push(face);
        let id = self.brep.faces.len() - 1;
        self.brep.shells[shell].faces.push(id);
        self.faces_out += 1;
    }
    fn triangle(&mut self, p: &[Vec3], t: Tri, shell: usize) {
        let p0 = p[t[0]];
        let n = cross(sub(p[t[1]], p0), sub(p[t[2]], p0));
        let l = norm(n);
        if l <= 0.0 {
            return;
        }
        let bound = self.wire(p, &[t[0], t[1], t[2]]);
        self.add_face(
            shell,
            Face { point: p0, normal: [n[0] / l, n[1] / l, n[2] / l], same_sense: true, bounds: vec![bound] },
        );
    }
}

fn map_value(m: impl IntoIterator<Item = (&'static str, Value)>) -> Map<String, Value> {
    m.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}



#[allow(clippy::too_many_lines)]
pub fn build_solid(
    vertices: &[Vec3],
    faces: &[Tri],
    opts: &BuildOptions<'_>,
    progress: Option<&dyn Fn(f64)>,
) -> Result<(Brep, Map<String, Value>), MeshError> {
    let budget = max_step_triangles();
    if faces.len() > budget {
        return Err(MeshError::invalid(format!(
            "{} triangles exceeds the STEP triangle budget of {budget}. Use a coarser tolerance or raise \
             IMPLEXITY_MAX_STEP_TRIANGLES. STL, PLY and 3MF do not use this STEP budget",
            faces.len()
        )));
    }
    if let Some(bad) = faces.iter().flatten().find(|&&v| v >= vertices.len()) {
        return Err(MeshError::invalid(format!("triangle vertex index {bad} is out of range")));
    }
    let us = opts.unit_scale;
    let p: Vec<Vec3> = vertices.iter().map(|v| [v[0] * us, v[1] * us, v[2] * us]).collect();
    let mut rep = map_value([
        ("triangles", json!(faces.len())),
        ("vertices", json!(vertices.len())),
        ("merge_requested", json!(opts.merge)),
        ("unit", json!("mm")),
        ("mesh_volume_mm3", json!(mesh_volume_mm3(vertices, faces, us))),
    ]);
    let mc = check_mesh(faces);
    rep.insert("input_mesh".into(), mc.clone());
    if !(mc["closed"] == true && mc["orientation_consistent"] == true) {
        return Err(MeshError::invalid(format!(
            "STEP requires a closed, consistently oriented manifold surface. Found {} boundary edges, \
             {} non-manifold edges and {} repeated directed edges. Heal or cap the mesh before export",
            mc["boundary_edges"], mc["nonmanifold_edges"], mc["directed_edges_seen_twice"]
        )));
    }
    if rep["mesh_volume_mm3"].as_f64().unwrap_or(0.0) < 0.0 {
        return Err(MeshError::invalid(
            "The mesh encloses negative volume. Reverse its winding before STEP export",
        ));
    }
    let t0 = Instant::now();
    let (nbr, _eid, _uniq) = face_adjacency(faces);
    rep.insert("adjacency_seconds".into(), json!(seconds_since(t0)));
    rep.insert("open_edges".into(), json!(nbr.iter().flatten().filter(|&&g| g < 0).count()));

    let t0 = Instant::now();
    let regions;
    if opts.merge {
        let mut plane_tol = opts.plane_tol_mm;
        let mut clamped = None;
        if plane_tol >= CONFUSION_MM {
            clamped = Some(plane_tol);
            plane_tol = CONFUSION_MM / 100.0;
        }
        regions = plane_regions(&p, faces, &nbr, opts.angle_tol_deg, plane_tol, 1.0);
        let dev = region_deviation(&p, faces, &regions, 1.0);
        let worst = dev.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut m = map_value([
            ("angle_tol_deg", json!(opts.angle_tol_deg)),
            ("plane_tol_mm", json!(plane_tol)),
            ("regions", json!(regions.count())),
            ("worst_vertex_to_plane_mm", json!(if dev.is_empty() { 0.0 } else { worst })),
            ("occt_confusion_mm", json!(CONFUSION_MM)),
            ("seconds", json!(seconds_since(t0))),
        ]);
        if let Some(from) = clamped {
            m.insert("plane_tol_clamped_from_mm".into(), json!(from));
            m.insert(
                "plane_tol_clamp_reason".into(),
                json!(format!(
                    "Plane tolerance was clamped to 1/100 of the {} mm interoperability tolerance to retain face planarity",
                    fmt_g(CONFUSION_MM, 6)
                )),
            );
        }
        m.insert("checker".into(), json!(NATIVE_CHECKER));
        rep.insert("merge".into(), Value::Object(m));
    } else {
        let (nrm, ar) = triangle_normals(&p, faces);
        let point = faces
            .iter()
            .map(|t| std::array::from_fn(|a| (p[t[0]][a] + p[t[1]][a] + p[t[2]][a]) / 3.0))
            .collect();
        regions = Regions {
            label: (0..faces.len()).map(crate::cast::i64_of).collect(),
            point,
            normal: nrm,
            area: ar,
        };
        rep.insert("merge".into(), json!({"skipped": true}));
    }
    let nreg = regions.count();
    if let Some(cb) = progress {
        cb(0.15);
    }
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); nreg];
    for (f, &l) in regions.label.iter().enumerate() {
        if l >= 0 {
            members[crate::cast::idx(l)].push(f);
        }
    }

    let t0 = Instant::now();
    let mut shortest = f64::INFINITY;
    let mut nzero = 0usize;
    for t in faces {
        for s in 0..3 {
            let d = norm(sub(p[t[s]], p[t[(s + 1) % 3]]));
            if d <= 0.0 {
                nzero += 1;
            }
            shortest = shortest.min(d);
        }
    }
    if nzero > 0 {
        return Err(MeshError::invalid(format!(
            "{nzero} triangle edges have coincident endpoints. Weld duplicate vertices and remove degenerate triangles before STEP export"
        )));
    }
    rep.insert("shortest_edge_mm".into(), json!(shortest));
    let (cid, ncomp) = face_components(faces, p.len());
    let cvol = component_volumes(&p, faces, &cid, ncomp, 1.0);
    rep.insert("components".into(), json!(ncomp));
    rep.insert("component_volumes_mm3".into(), json!(cvol));
    rep.insert("components_positive".into(), json!(cvol.iter().filter(|&&v| v > 0.0).count()));
    rep.insert("components_negative_voids".into(), json!(cvol.iter().filter(|&&v| v < 0.0).count()));

    let mut b = Builder {
        brep: Brep { shells: vec![Shell::default(); ncomp], ..Brep::default() },
        edge_of: HashMap::new(),
        point_of: HashMap::new(),
        wire_edges: 0,
        faces_out: 0,
    };
    let (mut merged_tris, mut rejected, mut rejected_tris, mut holes) = (0usize, 0usize, 0usize, 0usize);
    let mut per_region: Vec<(usize, usize, usize)> = Vec::with_capacity(nreg);
    for (r, rf) in members.iter().enumerate() {
        let ntri = rf.len();
        if ntri == 0 {
            continue;
        }
        if ntri == 1 || !opts.merge {
            b.triangle(&p, faces[rf[0]], cid[rf[0]]);
            per_region.push((r, ntri, 1));
            continue;
        }
        let (loops, mut okw) = region_loops(faces, &nbr, &regions.label, rf, regions.normal[r], &p);
        let mut pos = Vec::new();
        if okw {
            let a2: Vec<f64> =
                loops.iter().map(|lp| loop_area2(lp, &p, regions.point[r], regions.normal[r])).collect();
            pos = (0..a2.len()).filter(|&i| a2[i] > 0.0).collect();
            okw = pos.len() == 1 && a2.iter().all(|a| a.abs() > 0.0);
        }
        if !okw {
            rejected += 1;
            rejected_tris += ntri;
            for &f in rf {
                b.triangle(&p, faces[f], cid[f]);
            }
            per_region.push((r, ntri, ntri));
            continue;
        }
        let outer = pos[0];
        let mut bounds = vec![b.wire(&p, &loops[outer])];
        for (i, lp) in loops.iter().enumerate() {
            if i != outer {
                bounds.push(b.wire(&p, lp));
                holes += 1;
            }
        }
        b.add_face(
            cid[rf[0]],
            Face { point: regions.point[r], normal: regions.normal[r], same_sense: true, bounds },
        );
        merged_tris += ntri;
        per_region.push((r, ntri, 1));
    }
    rep.insert("build_seconds".into(), json!(seconds_since(t0)));
    if let Some(cb) = progress {
        cb(0.55);
    }
    if let (Some(cm), true) = (opts.cap_mask, opts.merge)
        && cm.len() == faces.len()
    {
        let mut capa = vec![0.0; nreg];
        let mut tota = vec![0.0; nreg];
        if nreg > 0 {
            for (f, &l) in regions.label.iter().enumerate() {
                let k = slot(l, nreg);
                capa[k] += regions.area[f] * if cm[f] { 1.0 } else { 0.0 };
                tota[k] += regions.area[f];
            }
        }
        let is_cap: Vec<bool> = (0..nreg).map(|k| capa[k] > 0.5 * tota[k].max(1e-300)).collect();
        let total = pairwise_sum(&tota);
        let mut byt = Map::new();
        if !per_region.is_empty() {
            for (tag, want) in [("cap", true), ("lattice", false)] {
                let rows: Vec<&(usize, usize, usize)> =
                    per_region.iter().filter(|x| is_cap[x.0] == want).collect();
                let tris: usize = rows.iter().map(|x| x.1).sum();
                let fcs: usize = rows.iter().map(|x| x.2).sum();
                let sel: Vec<f64> = (0..nreg).filter(|&k| is_cap[k] == want).map(|k| tota[k]).collect();
                #[allow(clippy::cast_precision_loss)]
                let ratio = tris as f64 / fcs.max(1) as f64;
                byt.insert(
                    tag.into(),
                    json!({
                        "regions": rows.len(), "triangles": tris, "faces": fcs,
                        "merge_ratio": py_round_digits(ratio, 3),
                        "area_fraction": py_round_digits(pairwise_sum(&sel) / total.max(1e-300), 4),
                    }),
                );
            }
        }
        rep.insert("merge_by_region_type".into(), Value::Object(byt));
        rep.insert(
            "merge_by_region_type_note".into(),
            json!(
                "a region counts as CAP when more than half its area sits on the domain boundary.  The cap is planar by \
                 construction and collapses; the lattice surface is a smooth level set and must not"
            ),
        );
    }
    rep.insert("faces".into(), json!(b.faces_out));
    rep.insert("faces_unmerged_would_be".into(), json!(faces.len()));
    rep.insert("edges".into(), json!(b.brep.edges.len()));
    rep.insert("wire_edge_uses".into(), json!(b.wire_edges));
    rep.insert("inner_wires".into(), json!(holes));
    if opts.merge
        && let Some(Value::Object(m)) = rep.get_mut("merge")
    {
        #[allow(clippy::cast_precision_loss)]
        let ratio = faces.len() as f64 / b.faces_out.max(1) as f64;
        m.insert("regions_merged".into(), json!(nreg - rejected));
        m.insert("regions_rejected".into(), json!(rejected));
        m.insert("triangles_in_merged_regions".into(), json!(merged_tris));
        m.insert("triangles_in_rejected_regions".into(), json!(rejected_tris));
        m.insert("merge_ratio".into(), json!(py_round_digits(ratio, 3)));
        if rejected > 0 {
            m.insert(
                "rejected_note".into(),
                json!(
                    "a region is rejected when its boundary walk does not consume every boundary edge exactly once, a \
                     loop does not close, or the loops do not come out as exactly one outer bound plus holes.  Its \
                     triangles are emitted individually -- the solid stays correct, it just does not get that region's \
                     saving"
                ),
            );
        }
    }
    let mut brep = b.brep;
    if opts.sew {
        let t0 = Instant::now();
        let all: Vec<usize> = (0..brep.shells.len()).collect();
        let census = brep::shell_closure(&brep, &all);
        let free = census["shell_edges_with_one_face"].as_u64().unwrap_or(0);
        let multiple = census["shell_edges_with_more_faces"].as_u64().unwrap_or(0);
        rep.insert(
            "sewing".into(),
            json!({
                "seconds": seconds_since(t0), "tolerance_mm": CONFUSION_MM,
                "free_edges": free, "multiple_edges": multiple, "degenerated_shapes": 0,
                "note": "Shell closure is checked using shared mesh edges",
                "found_nothing_to_sew": free == 0 && multiple == 0,
            }),
        );
    }
    let mut closure = map_value([
        ("shell_edges", json!(0)),
        ("shell_edges_with_one_face", json!(0)),
        ("shell_edges_with_two_faces", json!(0)),
        ("shell_edges_with_more_faces", json!(0)),
    ]);
    for s in 0..brep.shells.len() {
        let c = brep::shell_closure(&brep, &[s]);
        for (k, v) in &mut closure {
            let add = c.get(k.as_str()).and_then(Value::as_u64).unwrap_or(0);
            *v = json!(v.as_u64().unwrap_or(0) + add);
        }
    }
    let closed = closure["shell_edges_with_one_face"] == 0 && closure["shell_edges_with_more_faces"] == 0;
    closure.insert("shells".into(), json!(brep.shells.len()));
    closure.insert("shell_closed_measured".into(), json!(closed));
    rep.extend(closure);

    let pos: Vec<usize> = (0..ncomp).filter(|&i| cvol[i] > 0.0).collect();
    let neg: Vec<usize> = (0..ncomp).filter(|&i| cvol[i] < 0.0).collect();
    let mut owner: Vec<Vec<usize>> = vec![Vec::new(); pos.len()];
    let mut assigned = Map::new();
    for &j in &neg {
        if pos.is_empty() {
            break;
        }
        let k = if pos.len() > 1 { owner_of_void(&p, faces, &cid, j, &pos) } else { 0 };
        owner[k].push(j);
        assigned.insert(j.to_string(), json!(pos[k]));
    }
    rep.insert("voids_assigned".into(), Value::Object(assigned));
    let mut void_orientation = Map::new();
    for (k, &i) in pos.iter().enumerate() {
        let want = owner[k].iter().fold(cvol[i], |acc, &j| acc + cvol[j]);
        let mut shells = vec![i];
        shells.extend(owner[k].iter().copied());
        let mut got = brep.volume_of_shells(&shells);
        if got < 0.0 {
            for &s in &shells {
                brep.shells[s].reversed = !brep.shells[s].reversed;
            }
            got = -got;
        }
        if !owner[k].is_empty() && (got - want).abs() > 1e-9 * want.abs().max(1.0) {
            for &j in &owner[k] {
                brep.shells[j].reversed = !brep.shells[j].reversed;
            }
            let mut got2 = brep.volume_of_shells(&shells);
            let flip_all = got2 < 0.0;
            if flip_all {
                got2 = -got2;
            }
            let take_reversed = (got2 - want).abs() < (got - want).abs();
            void_orientation.insert(
                i.to_string(),
                json!({"wanted_mm3": want, "as_built_mm3": got, "reversed_mm3": got2,
                       "took": if take_reversed { "reversed" } else { "as_built" }}),
            );
            if take_reversed {
                if flip_all {
                    for &s in &shells {
                        brep.shells[s].reversed = !brep.shells[s].reversed;
                    }
                }
            } else {
                for &j in &owner[k] {
                    brep.shells[j].reversed = !brep.shells[j].reversed;
                }
            }
        }
        brep.solids.push(Solid { shells });
    }
    if !void_orientation.is_empty() {
        rep.insert("void_orientation".into(), Value::Object(void_orientation));
    }
    if brep.solids.is_empty() {
        return Err(MeshError::invalid(
            "no component of this mesh encloses positive volume -- there is no solid to write",
        ));
    }
    if brep.solids.len() > 1 {
        let n = brep.solids.len();
        rep.insert(
            "compound_note".into(),
            json!(format!(
                "{n} disconnected bodies -> a STEP file carrying {n} manifold_solid_brep entities, not one.  That is the \
                 design being disconnected, not the conversion adding something: see the component table in the body \
                 report"
            )),
        );
    }
    if let Some(cb) = progress {
        cb(0.7);
    }
    if let Value::Object(check) = brep::check_solid(&brep, CONFUSION_MM) {
        rep.extend(check);
    }
    rep.insert("checker".into(), json!(NATIVE_CHECKER));
    rep.insert("peak_rss_mb".into(), peak_rss_mb());
    let v_mesh = rep["mesh_volume_mm3"].as_f64().unwrap_or(0.0);
    let vol = rep["volume_mm3"].as_f64().unwrap_or(0.0);
    rep.insert("volume_vs_mesh_rel".into(), json!((vol - v_mesh).abs() / v_mesh.abs().max(1e-300)));
    if let Some(cb) = progress {
        cb(0.8);
    }
    Ok((brep, rep))
}

fn owner_of_void(p: &[Vec3], faces: &[Tri], cid: &[usize], j: usize, pos: &[usize]) -> usize {
    let Some(f) = cid.iter().position(|&c| c == j) else { return 0 };
    let t = faces[f];
    let mut p0: Vec3 = std::array::from_fn(|a| (p[t[0]][a] + p[t[1]][a] + p[t[2]][a]) / 3.0);
    let n = cross(sub(p[t[1]], p[t[0]]), sub(p[t[2]], p[t[0]]));
    let ln = norm(n);
    if ln > 0.0 {
        for a in 0..3 {
            p0[a] -= n[a] / ln * 1e-6;
        }
    }
    for (k, &i) in pos.iter().enumerate() {
        let tris: Vec<[usize; 3]> =
            faces.iter().zip(cid).filter(|(_, c)| **c == i).map(|(t, _)| *t).collect();
        if brep::point_inside(p, &tris, p0) {
            return k;
        }
    }
    0
}

#[must_use]
pub fn utc_now() -> (i64, u32, u32, u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    civil(secs)
}

#[must_use]
pub fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let u = |v: i64| u32::try_from(v).unwrap_or(0);
    (y, u(m), u(d), u(rem / 3600), u(rem % 3600 / 60), u(rem % 60))
}

#[derive(Clone, Debug)]
pub struct WriteOptions<'a> {
    pub schema: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub author: &'a str,
    pub pcurves: bool,
}

impl Default for WriteOptions<'_> {
    fn default() -> Self {
        Self {
            schema: DEFAULT_SCHEMA,
            name: "implexity_body",
            description: None,
            author: "",
            pcurves: true,
        }
    }
}



pub fn write_step(brep: &Brep, path: &Path, opts: &WriteOptions<'_>) -> Result<Value, MeshError> {
    let asked = opts.schema.to_uppercase();
    let Some((_, sch)) = SCHEMAS.iter().find(|(k, _)| *k == asked) else {
        let mut known: Vec<String> = SCHEMAS.iter().map(|(k, _)| format!("'{k}'")).collect();
        known.sort();
        return Err(MeshError::invalid(format!(
            "schema {}: one of [{}]",
            implexity_core::py_repr::repr_str(opts.schema),
            known.join(", ")
        )));
    };
    let protocol = p21::Protocol::from_occt_name(sch).unwrap_or(p21::Protocol::Ap214);
    let t0 = Instant::now();
    let header = p21::HeaderInfo {
        name: opts.name,
        description: opts.description.unwrap_or(""),
        author: opts.author,
        organization: "",
        originating_system: "implexity bodyexport/stepexport",
        timestamp: utc_now(),
    };
    let text = p21::write_brep(brep, protocol, &header, opts.pcurves)?;
    crate::formats::write_file(path, text.as_bytes())?;
    Ok(json!({
        "path": path.display().to_string(), "bytes": text.len(),
        "write_seconds": seconds_since(t0), "pcurves": opts.pcurves,
        "schema": sch, "schema_asked": asked,
        "length_unit": "MM (SI_UNIT(.MILLI.,.METRE.))", "product_name": opts.name,
        "statics_accepted": {"schema": true, "unit": true, "product_name": true, "assembly": true,
                             "nonmanifold": true, "surfacecurve": true},
        "occt": Value::Null, "writer": p21::PREPROCESSOR,
    }))
}



pub fn read_step(path: &Path) -> Result<(Brep, Value), MeshError> {
    let t0 = Instant::now();
    let bytes = std::fs::read(path).map_err(|e| MeshError::io(format!("reading {}", path.display()), e))?;
    let text = String::from_utf8_lossy(&bytes);
    let (brep, _ex) = p21::read_brep(&text)?;
    let mut out = map_value([("read_seconds", json!(seconds_since(t0)))]);
    if let Value::Object(c) = brep::check_solid(&brep, CONFUSION_MM) {
        out.extend(c);
    }
    let shells = brep.solid_shells();
    out.extend(brep::shell_closure(&brep, &shells).into_iter().map(|(k, v)| (k.to_string(), v)));
    Ok((brep, Value::Object(out)))
}

#[must_use]
pub fn step_header_facts(path: &Path) -> Value {
    let collapse = |s: &str, n: usize| -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(n).collect()
    };
    let mut facts = Map::new();
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let head: String = text.split_inclusive('\n').take(40).collect();
            for (key, tag) in [("file_schema", "FILE_SCHEMA"), ("file_name", "FILE_NAME")] {
                if let Some(i) = head.find(tag) {
                    let j = head[i..].find(';').map_or(head.len(), |j| i + j);
                    facts.insert(key.into(), json!(collapse(&head[i..j], 300)));
                }
            }
            for line in text.split_inclusive('\n') {
                if line.contains("LENGTH_UNIT") && line.contains("SI_UNIT") {
                    facts.insert("length_unit_entity".into(), json!(collapse(line, 200)));
                    break;
                }
                if line.starts_with('#') && line.contains("PRODUCT(") {
                    facts.insert("product_line".into(), json!(collapse(line, 200)));
                }
            }
        }
        Err(e) => {
            facts.insert("error".into(), json!(format!("{e}")));
        }
    }
    Value::Object(facts)
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct StepOptions<'a> {
    pub merge: bool,
    pub schema: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub angle_tol_deg: f64,
    pub plane_tol_mm: f64,
    pub volume_guard_rel: f64,
    pub unit_scale: f64,
    pub cap_mask: Option<&'a [bool]>,
    pub roundtrip: bool,
    pub pcurves: bool,
    pub reference_build: bool,
}

impl Default for StepOptions<'_> {
    fn default() -> Self {
        Self {
            merge: true,
            schema: DEFAULT_SCHEMA,
            name: "implexity_body",
            description: None,
            angle_tol_deg: DEFAULT_ANGLE_TOL_DEG,
            plane_tol_mm: DEFAULT_PLANE_TOL_MM,
            volume_guard_rel: DEFAULT_VOLUME_GUARD_REL,
            unit_scale: 1e3,
            cap_mask: None,
            roundtrip: true,
            pcurves: true,
            reference_build: true,
        }
    }
}



#[allow(clippy::too_many_lines)]
pub fn mesh_to_step(
    vertices: &[Vec3],
    faces: &[Tri],
    path: &Path,
    opts: &StepOptions<'_>,
    progress: Option<&dyn Fn(f64)>,
) -> Result<Value, MeshError> {
    let t_all = Instant::now();
    let mut rep = map_value([
        ("schema_family", json!("ISO 10303 STEP")),
        (
            "kind",
            json!(
                "boundary-representation solid: manifold_solid_brep, advanced_face on PLANE surfaces, polygonal wires"
            ),
        ),
        (
            "is_not",
            json!(
                "not an analytic/feature model -- no cylinders, no fillets, no swept or lofted surfaces.  Fitting those \
                 back onto a level-set tessellation is reverse engineering and is out of scope"
            ),
        ),
    ]);
    let unmerged_opts = BuildOptions { merge: false, unit_scale: opts.unit_scale, ..BuildOptions::default() };
    let mut v_ref = 0.0;
    if opts.merge && opts.reference_build {
        let (_solid, br) = build_solid(vertices, faces, &unmerged_opts, None)?;
        v_ref = br["volume_mm3"].as_f64().unwrap_or(0.0);
        rep.insert("unmerged".into(), Value::Object(br));
    } else if opts.merge {
        v_ref = mesh_volume_mm3(vertices, faces, opts.unit_scale);
        rep.insert(
            "unmerged".into(),
            json!({"skipped": true,
                   "note": "reference_build=False: the merge guard compares the merged solid against the MESH volume \
                            (divergence theorem, exact) rather than against a second build of the same mesh",
                   "volume_mm3": v_ref, "mesh_volume_mm3": v_ref}),
        );
    }
    let (use_brep, ur) = if opts.merge {
        let merged_opts = BuildOptions {
            merge: true,
            angle_tol_deg: opts.angle_tol_deg,
            plane_tol_mm: opts.plane_tol_mm,
            unit_scale: opts.unit_scale,
            cap_mask: opts.cap_mask,
            sew: false,
        };
        let (msolid, mut mr) = build_solid(vertices, faces, &merged_opts, progress)?;
        let dv = (mr["volume_mm3"].as_f64().unwrap_or(0.0) - v_ref).abs() / v_ref.abs().max(1e-300);
        mr.insert("volume_vs_unmerged_rel".into(), json!(dv));
        let valid = mr["brepcheck_valid"] == true;
        let closed = mr["shell_closed_measured"] == true;
        let accept = valid && dv <= opts.volume_guard_rel && closed;
        rep.insert("merged".into(), Value::Object(mr.clone()));
        rep.insert(
            "merge_guard".into(),
            json!({"volume_guard_rel": opts.volume_guard_rel, "merged_vs_unmerged_rel": dv,
                   "merged_brepcheck_valid": valid, "merged_shell_closed": closed, "accepted": accept}),
        );
        if accept {
            rep.insert("written".into(), json!("merged"));
            (msolid, mr)
        } else {
            rep.insert(
                "merge_fallback".into(),
                json!(format!(
                    "the merged solid was REJECTED by the guard (valid={}, closed={}, dV={} vs allowed {}); the \
                     UNMERGED solid was written",
                    if valid { "True" } else { "False" },
                    if closed { "True" } else { "False" },
                    fmt_e(dv, 3),
                    fmt_e(opts.volume_guard_rel, 0)
                )),
            );
            rep.insert("written".into(), json!("unmerged"));
            build_solid(vertices, faces, &unmerged_opts, None)?
        }
    } else {
        let (s, r) = build_solid(vertices, faces, &unmerged_opts, progress)?;
        rep.insert("unmerged".into(), Value::Object(r.clone()));
        rep.insert("written".into(), json!("unmerged"));
        (s, r)
    };
    rep.insert("solid".into(), Value::Object(ur.clone()));
    let wopts = WriteOptions {
        schema: opts.schema,
        name: opts.name,
        description: opts.description,
        author: "",
        pcurves: opts.pcurves,
    };
    let write = write_step(&use_brep, path, &wopts)?;
    rep.insert("write".into(), write.clone());
    rep.insert("header".into(), step_header_facts(path));
    if opts.roundtrip {
        let rt = match read_step(path) {
            Ok((_b, mut r2)) => {
                let v1 = ur["volume_mm3"].as_f64().unwrap_or(0.0);
                let v2 = r2["volume_mm3"].as_f64().unwrap_or(0.0);
                r2["volume_vs_written_rel"] = json!((v2 - v1).abs() / v1.abs().max(1e-300));
                r2["faces_match"] = json!(r2["n_faces"] == ur["n_faces"]);
                r2
            }
            Err(e) => json!({"error": e.to_string()}),
        };
        rep.insert("roundtrip".into(), rt);
    }
    let rt = rep.get("roundtrip").cloned().unwrap_or(Value::Null);
    rep.insert(
        "accepted".into(),
        json!({
            "valid_solid": ur["brepcheck_valid"] == true,
            "closed_shell": ur["shell_closed_measured"] == true,
            "solids": ur["n_solids"], "faces": ur["n_faces"], "faces_unmerged": faces.len(),
            "volume_mm3": ur["volume_mm3"], "mesh_volume_mm3": ur["mesh_volume_mm3"],
            "volume_vs_mesh_rel": ur["volume_vs_mesh_rel"], "bytes": write["bytes"],
            "schema": write["schema"],
            "roundtrip_valid": rt.get("brepcheck_valid") == Some(&Value::Bool(true)),
            "roundtrip_volume_rel": rt.get("volume_vs_written_rel").cloned().unwrap_or(Value::Null),
        }),
    );
    rep.insert("seconds".into(), json!(seconds_since(t_all)));
    if let Some(cb) = progress {
        cb(1.0);
    }
    Ok(Value::Object(rep))
}
