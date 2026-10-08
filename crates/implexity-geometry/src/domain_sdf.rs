// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use rayon::prelude::*;
use serde_json::{Value, json};

pub const PAIR_BUDGET: usize = 1_000_000;
pub const MAX_TRIANGLES: usize = 400_000;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("mesh rejected:\n  {}", problems.join("\n  "))]
pub struct MeshError {
    pub problems: Vec<String>,
}

fn merr<T>(p: impl Into<String>) -> Result<T, MeshError> {
    Err(MeshError { problems: vec![p.into()] })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriMesh {
    pub v: Vec<[f64; 3]>,
    pub f: Vec<[usize; 3]>,
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
fn norm(a: [f64; 3]) -> f64 {
    crate::numpy::norm(&a)
}


pub fn read_mesh(data: &[u8], name: &str, scale: f64) -> Result<TriMesh, MeshError> {
    let raw = if name.to_lowercase().ends_with(".ply") || data.starts_with(b"ply") {
        parse_ply(data, name)?
    } else if data.len() >= 6
        && data[..6].to_ascii_lowercase().starts_with(b"solid")
        && data[..data.len().min(1000)].windows(5).any(|w| w == b"facet")
    {
        parse_stl_ascii(data)
    } else {
        parse_stl_binary(data, name)?
    };
    if raw.f.len() > MAX_TRIANGLES {
        return merr(format!("{name}: {} triangles exceeds the cap of {MAX_TRIANGLES}", raw.f.len()));
    }
    let mut m = weld(&raw, 1e-9);
    for p in &mut m.v {
        for c in p.iter_mut() {
            *c *= scale;
        }
    }
    Ok(m)
}

fn parse_stl_binary(data: &[u8], name: &str) -> Result<TriMesh, MeshError> {
    if data.len() < 84 {
        return merr(format!("{name}: too short for binary STL"));
    }
    let n = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
    let need = 84 + 50 * n;
    if data.len() < need {
        return merr(format!(
            "{name}: binary STL declares {n} triangles but the file holds {}",
            (data.len() - 84) / 50
        ));
    }
    let mut v = Vec::with_capacity(3 * n);
    for t in 0..n {
        let base = 84 + 50 * t + 12;
        for k in 0..3 {
            let p: [f64; 3] = std::array::from_fn(|c| {
                let o = base + 12 * k + 4 * c;
                f64::from(f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]))
            });
            v.push(p);
        }
    }
    Ok(TriMesh { v, f: (0..n).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect() })
}

fn parse_stl_ascii(data: &[u8]) -> TriMesh {
    let text = String::from_utf8_lossy(data);
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut v = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i] == "vertex" && i + 3 < toks.len() {
            let p = |k: usize| toks[i + k].parse::<f64>().unwrap_or(f64::NAN);
            v.push([p(1), p(2), p(3)]);
            i += 4;
        } else {
            i += 1;
        }
    }
    let n = v.len() / 3;
    TriMesh { v, f: (0..n).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect() }
}

#[derive(Clone, Copy)]
enum Prop {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Prop {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "char" | "int8" => Self::I8,
            "uchar" | "uint8" => Self::U8,
            "short" | "int16" => Self::I16,
            "ushort" | "uint16" => Self::U16,
            "int" | "int32" => Self::I32,
            "uint" | "uint32" => Self::U32,
            "float" | "float32" => Self::F32,
            "double" | "float64" => Self::F64,
            _ => return None,
        })
    }
    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
    fn read(self, b: &[u8], little: bool) -> f64 {
        macro_rules! rd {
            ($t:ty, $n:expr) => {{
                let mut a = [0u8; $n];
                a.copy_from_slice(&b[..$n]);
                if little { <$t>::from_le_bytes(a) } else { <$t>::from_be_bytes(a) }
            }};
        }
        match self {
            Self::I8 => f64::from(i8::from_le_bytes([b[0]])),
            Self::U8 => f64::from(b[0]),
            Self::I16 => f64::from(rd!(i16, 2)),
            Self::U16 => f64::from(rd!(u16, 2)),
            Self::I32 => f64::from(rd!(i32, 4)),
            Self::U32 => f64::from(rd!(u32, 4)),
            Self::F32 => f64::from(rd!(f32, 4)),
            Self::F64 => rd!(f64, 8),
        }
    }
}

enum PlyProp {
    Scalar(String, Prop),
    List(String, Prop, Prop),
}

struct PlyElement {
    name: String,
    count: usize,
    props: Vec<PlyProp>,
}

#[allow(clippy::too_many_lines)]
fn parse_ply(data: &[u8], name: &str) -> Result<TriMesh, MeshError> {
    let Some(end) = data.windows(10).position(|w| w == b"end_header") else {
        return merr(format!("{name}: PLY header missing vertex/face counts"));
    };
    let mut body_start = end + 10;
    while body_start < data.len() && data[body_start] != b'\n' {
        body_start += 1;
    }
    body_start = (body_start + 1).min(data.len());
    let header = String::from_utf8_lossy(&data[..end]);
    let mut format = "ascii".to_string();
    let mut elements: Vec<PlyElement> = Vec::new();
    for line in header.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        match t.as_slice() {
            ["format", f, ..] => format = (*f).to_string(),
            ["element", n, c] => elements.push(PlyElement {
                name: (*n).to_string(),
                count: c.parse().unwrap_or(0),
                props: Vec::new(),
            }),
            ["property", "list", ct, it, pname] => {
                if let (Some(e), Some(c), Some(i)) = (elements.last_mut(), Prop::parse(ct), Prop::parse(it)) {
                    e.props.push(PlyProp::List((*pname).to_string(), c, i));
                }
            }
            ["property", ty, pname] => {
                if let (Some(e), Some(p)) = (elements.last_mut(), Prop::parse(ty)) {
                    e.props.push(PlyProp::Scalar((*pname).to_string(), p));
                }
            }
            _ => {}
        }
    }
    let nv = elements.iter().find(|e| e.name == "vertex").map(|e| e.count);
    let nf = elements.iter().find(|e| e.name == "face").map(|e| e.count);
    let (Some(_), Some(_)) = (nv, nf) else {
        return merr(format!("{name}: PLY header missing vertex/face counts"));
    };
    let mut v = Vec::new();
    let mut f = Vec::new();
    let push_face = |poly: &[usize], f: &mut Vec<[usize; 3]>| {
        for c in 1..poly.len().saturating_sub(1) {
            f.push([poly[0], poly[c], poly[c + 1]]);
        }
    };
    let bad = || MeshError { problems: vec![format!("{name}: truncated PLY body")] };
    if format == "ascii" {
        let text = String::from_utf8_lossy(&data[body_start..]);
        let mut lines = text.lines();
        for e in &elements {
            for _ in 0..e.count {
                let line = lines.next().ok_or_else(bad)?;
                let t: Vec<f64> =
                    line.split_whitespace().map(|x| x.parse::<f64>().unwrap_or(f64::NAN)).collect();
                if e.name == "vertex" {
                    if t.len() < 3 {
                        return Err(bad());
                    }
                    v.push([t[0], t[1], t[2]]);
                } else if e.name == "face" {
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let k = t.first().copied().unwrap_or(0.0) as usize;
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let poly: Vec<usize> = t.iter().skip(1).take(k).map(|x| *x as usize).collect();
                    push_face(&poly, &mut f);
                }
            }
        }
    } else {
        let little = match format.as_str() {
            "binary_little_endian" => true,
            "binary_big_endian" => false,
            other => return merr(format!("{name}: unsupported PLY format {other}")),
        };
        let mut pos = body_start;
        let take = |pos: &mut usize, p: Prop| -> Result<f64, MeshError> {
            let n = p.size();
            if *pos + n > data.len() {
                return Err(bad());
            }
            let x = p.read(&data[*pos..*pos + n], little);
            *pos += n;
            Ok(x)
        };
        for e in &elements {
            for _ in 0..e.count {
                let mut xyz = [f64::NAN; 3];
                for prop in &e.props {
                    match prop {
                        PlyProp::Scalar(pn, p) => {
                            let x = take(&mut pos, *p)?;
                            match pn.as_str() {
                                "x" => xyz[0] = x,
                                "y" => xyz[1] = x,
                                "z" => xyz[2] = x,
                                _ => {}
                            }
                        }
                        PlyProp::List(pn, c, i) => {
                            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                            let k = take(&mut pos, *c)? as usize;
                            let mut poly = Vec::with_capacity(k);
                            for _ in 0..k {
                                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                                poly.push(take(&mut pos, *i)? as usize);
                            }
                            if e.name == "face" && (pn == "vertex_indices" || pn == "vertex_index") {
                                push_face(&poly, &mut f);
                            }
                        }
                    }
                }
                if e.name == "vertex" {
                    v.push(xyz);
                }
            }
        }
    }
    Ok(TriMesh { v, f })
}

#[must_use]
pub fn weld(m: &TriMesh, tol_rel: f64) -> TriMesh {
    if m.v.is_empty() {
        return m.clone();
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &m.v {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let diag = norm(sub(hi, lo));
    let diag = if diag == 0.0 { 1.0 } else { diag };
    #[allow(clippy::cast_possible_truncation)]
    let q: Vec<[i64; 3]> =
        m.v.iter().map(|p| p.map(|x| (x / (tol_rel * diag)).round_ties_even() as i64)).collect();
    let mut order: Vec<usize> = (0..q.len()).collect();
    order.sort_by(|a, b| q[*a].cmp(&q[*b]).then(a.cmp(b)));
    let mut inv = vec![0usize; q.len()];
    let mut v2 = Vec::new();
    let mut last: Option<[i64; 3]> = None;
    for &i in &order {
        if last != Some(q[i]) {
            v2.push(m.v[i]);
            last = Some(q[i]);
        }
        inv[i] = v2.len() - 1;
    }
    let f2 =
        m.f.iter()
            .map(|t| t.map(|i| inv[i]))
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[2] != t[0])
            .collect();
    TriMesh { v: v2, f: f2 }
}

#[must_use]
pub fn edges_of(f: &[[usize; 3]]) -> Vec<[usize; 2]> {
    let mut e: Vec<[usize; 2]> = f.iter().map(|t| [t[0], t[1]]).collect();
    e.extend(f.iter().map(|t| [t[1], t[2]]));
    e.extend(f.iter().map(|t| [t[2], t[0]]));
    e
}

fn edge_counts(f: &[[usize; 3]]) -> std::collections::BTreeMap<[usize; 2], Vec<[usize; 2]>> {
    let mut m: std::collections::BTreeMap<[usize; 2], Vec<[usize; 2]>> = std::collections::BTreeMap::new();
    for e in edges_of(f) {
        let u = if e[0] <= e[1] { e } else { [e[1], e[0]] };
        m.entry(u).or_default().push(e);
    }
    m
}

#[must_use]
pub fn is_watertight(f: &[[usize; 3]]) -> bool {
    edge_counts(f).values().all(|v| v.len() == 2 && v[0][0] == v[1][1])
}

#[must_use]
pub fn mesh_volume(m: &TriMesh) -> f64 {
    let terms: Vec<f64> = m.f.iter().map(|t| dot(m.v[t[0]], cross(m.v[t[1]], m.v[t[2]]))).collect();
    crate::numpy::sum(&terms) / 6.0
}

#[must_use]
pub fn mesh_area(m: &TriMesh) -> f64 {
    let terms: Vec<f64> =
        m.f.iter().map(|t| norm(cross(sub(m.v[t[1]], m.v[t[0]]), sub(m.v[t[2]], m.v[t[0]])))).collect();
    0.5 * crate::numpy::sum(&terms)
}

#[must_use]
pub fn tri_normals(m: &TriMesh) -> (Vec<[f64; 3]>, Vec<f64>) {
    m.f.iter()
        .map(|t| {
            let n = cross(sub(m.v[t[1]], m.v[t[0]]), sub(m.v[t[2]], m.v[t[0]]));
            let twice = norm(n);
            let d = twice.max(1e-300);
            (n.map(|x| x / d), 0.5 * twice)
        })
        .unzip()
}


pub fn check_mesh(m: &TriMesh, name: &str) -> Result<(), MeshError> {
    let mut problems = Vec::new();
    if m.f.len() < 4 {
        problems.push(format!("{name}: {} triangles is not a closed surface", m.f.len()));
    }
    if !m.v.is_empty() && !m.v.iter().flatten().all(|x| x.is_finite()) {
        problems.push(format!("{name}: non-finite vertex coordinates"));
    }
    if m.f.iter().flatten().any(|i| *i >= m.v.len()) {
        problems.push(format!("{name}: face index out of range"));
    }
    if !problems.is_empty() {
        return Err(MeshError { problems });
    }
    if !is_watertight(&m.f) {
        let bad = edge_counts(&m.f).values().filter(|v| v.len() != 2).count();
        problems.push(format!(
            "{name}: not watertight ({bad} edge(s) not shared by exactly two triangles). v1 ingestion requires a closed, consistently oriented shell -- export the part as a single solid body"
        ));
    }
    let vol = mesh_volume(m);
    if problems.is_empty() && vol <= 0.0 {
        problems.push(format!(
            "{name}: negative enclosed volume ({} m^3) -- the shell is oriented inward; flip the normals",
            crate::pyfmt::fmt_e(vol, 3)
        ));
    }
    if problems.is_empty() { Ok(()) } else { Err(MeshError { problems }) }
}

#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn closest_on_triangle(p: [f64; 3], a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> [f64; 3] {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    let va = d3 * d6 - d5 * d4;
    let vb = d5 * d2 - d1 * d6;
    let vc = d1 * d4 - d3 * d2;
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let lerp = |o: [f64; 3], d: [f64; 3], t: f64| [o[0] + t * d[0], o[1] + t * d[1], o[2] + t * d[2]];
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return lerp(a, ab, (d1 / (d1 - d3).max(1e-300)).clamp(0.0, 1.0));
    }
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return lerp(a, ac, (d2 / (d2 - d6).max(1e-300)).clamp(0.0, 1.0));
    }
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return lerp(b, sub(c, b), ((d4 - d3) / ((d4 - d3) + (d5 - d6)).max(1e-300)).clamp(0.0, 1.0));
    }
    let denom = (va + vb + vc).max(1e-300);
    let (v, w) = (vb / denom, vc / denom);
    [a[0] + v * ab[0] + w * ac[0], a[1] + v * ab[1] + w * ac[1], a[2] + v * ab[2] + w * ac[2]]
}

fn d2_to(p: [f64; 3], m: &TriMesh, t: usize) -> f64 {
    let f = m.f[t];
    let q = closest_on_triangle(p, m.v[f[0]], m.v[f[1]], m.v[f[2]]);
    let d = sub(p, q);
    dot(d, d)
}

struct BvhNode {
    lo: [f64; 3],
    hi: [f64; 3],
    left: usize,
    right: usize,
    start: usize,
    end: usize,
}

pub struct Bvh {
    nodes: Vec<BvhNode>,
    tris: Vec<usize>,
}

impl Bvh {
    #[must_use]
    pub fn new(m: &TriMesh) -> Self {
        let mut tris: Vec<usize> = (0..m.f.len()).collect();
        let cent: Vec<[f64; 3]> =
            m.f.iter()
                .map(|t| std::array::from_fn(|a| (m.v[t[0]][a] + m.v[t[1]][a] + m.v[t[2]][a]) / 3.0))
                .collect();
        let mut nodes = Vec::new();
        Self::build(m, &cent, &mut tris, 0, m.f.len(), &mut nodes);
        Self { nodes, tris }
    }

    fn build(
        m: &TriMesh,
        cent: &[[f64; 3]],
        tris: &mut [usize],
        start: usize,
        end: usize,
        nodes: &mut Vec<BvhNode>,
    ) -> usize {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &t in &tris[start..end] {
            for &vi in &m.f[t] {
                for a in 0..3 {
                    lo[a] = lo[a].min(m.v[vi][a]);
                    hi[a] = hi[a].max(m.v[vi][a]);
                }
            }
        }
        let id = nodes.len();
        nodes.push(BvhNode { lo, hi, left: usize::MAX, right: usize::MAX, start, end });
        if end - start > 4 {
            let ext = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
            let axis = (0..3).max_by(|a, b| ext[*a].total_cmp(&ext[*b])).unwrap_or(0);
            let mid = start + (end - start) / 2;
            tris[start..end]
                .select_nth_unstable_by(mid - start, |a, b| cent[*a][axis].total_cmp(&cent[*b][axis]));
            let l = Self::build(m, cent, tris, start, mid, nodes);
            let r = Self::build(m, cent, tris, mid, end, nodes);
            nodes[id].left = l;
            nodes[id].right = r;
        }
        id
    }

    fn box_d2(n: &BvhNode, p: [f64; 3]) -> f64 {
        let mut d = 0.0;
        for a in 0..3 {
            let v = if p[a] < n.lo[a] {
                n.lo[a] - p[a]
            } else if p[a] > n.hi[a] {
                p[a] - n.hi[a]
            } else {
                0.0
            };
            d += v * v;
        }
        d
    }

    #[must_use]
    pub fn nearest(&self, m: &TriMesh, p: [f64; 3]) -> (f64, usize) {
        let mut best = (f64::INFINITY, usize::MAX);
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i];
            if Self::box_d2(n, p) > best.0 {
                continue;
            }
            if n.left == usize::MAX {
                for &t in &self.tris[n.start..n.end] {
                    let d = d2_to(p, m, t);
                    if d < best.0 || (d == best.0 && t < best.1) {
                        best = (d, t);
                    }
                }
            } else {
                let (l, r) = (n.left, n.right);
                let (dl, dr) = (Self::box_d2(&self.nodes[l], p), Self::box_d2(&self.nodes[r], p));
                if dl <= dr {
                    stack.push(r);
                    stack.push(l);
                } else {
                    stack.push(l);
                    stack.push(r);
                }
            }
        }
        best
    }
}

#[must_use]
pub fn unsigned_distance(m: &TriMesh, points: &[[f64; 3]]) -> (Vec<f64>, Vec<usize>) {
    if m.f.is_empty() {
        return (vec![f64::INFINITY; points.len()], vec![0; points.len()]);
    }
    let bvh = Bvh::new(m);
    points.par_iter().map(|p| bvh.nearest(m, *p)).map(|(d2, t)| (d2.sqrt(), t)).unzip()
}

#[must_use]
pub fn winding_number(m: &TriMesh, points: &[[f64; 3]]) -> Vec<f64> {
    points
        .par_iter()
        .map(|p| {
            let terms: Vec<f64> =
                m.f.iter()
                    .map(|t| {
                        let a = sub(m.v[t[0]], *p);
                        let b = sub(m.v[t[1]], *p);
                        let c = sub(m.v[t[2]], *p);
                        let (la, lb, lc) = (norm(a), norm(b), norm(c));
                        let det = dot(a, cross(b, c));
                        let den = la * lb * lc + dot(a, b) * lc + dot(b, c) * la + dot(c, a) * lb;
                        det.atan2(den)
                    })
                    .collect();
            crate::numpy::sum(&terms) / (2.0 * std::f64::consts::PI)
        })
        .collect()
}

#[must_use]
pub fn sdf_at(m: &TriMesh, points: &[[f64; 3]]) -> (Vec<f64>, Vec<f64>, Vec<usize>) {
    let (d, tri) = unsigned_distance(m, points);
    let w = winding_number(m, points);
    let s = d.iter().zip(&w).map(|(d, w)| if *w > 0.5 { -d } else { *d }).collect();
    (s, w, tri)
}

#[must_use]
pub fn mask_from_sdf(sdf: &[f64], width: f64) -> Vec<f64> {
    let w = width.max(1e-300);
    sdf.iter().map(|s| (0.5 - s / w).clamp(0.0, 1.0)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StructuredGrid {
    pub origin: [f64; 3],
    pub h: f64,
    pub counts: [usize; 3],
}

impl StructuredGrid {
    #[must_use]
    pub fn cell_volume(&self) -> f64 {
        self.h * self.h * self.h
    }

    #[must_use]
    pub fn element_centres(&self) -> Vec<[f64; 3]> {
        let mut out = Vec::with_capacity(self.counts.iter().product());
        #[allow(clippy::cast_precision_loss)]
        for i in 0..self.counts[0] {
            for j in 0..self.counts[1] {
                for k in 0..self.counts[2] {
                    let idx = [i, j, k];
                    out.push(std::array::from_fn(|a| self.origin[a] + (idx[a] as f64 + 0.5) * self.h));
                }
            }
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct DomainFields {
    pub mesh: TriMesh,
    pub grid: StructuredGrid,
    pub width: f64,
    pub mask: Vec<f64>,
    pub hard: Vec<f64>,
    pub sdf: Vec<f64>,
    pub winding: Vec<f64>,
    pub nearest_tri: Vec<usize>,
    node_sdf: std::sync::OnceLock<Vec<f64>>,
}

pub const REFINE_K: usize = 3;

impl DomainFields {

    pub fn new(
        mesh: TriMesh,
        grid: StructuredGrid,
        width: Option<f64>,
        refine: bool,
    ) -> Result<Self, MeshError> {
        check_mesh(&mesh, "mesh")?;
        let width = width.filter(|w| *w != 0.0).unwrap_or(grid.h);
        let p = grid.element_centres();
        let (sdf, winding, nearest_tri) = sdf_at(&mesh, &p);
        let mut mask = mask_from_sdf(&sdf, width);
        if refine {
            let idx: Vec<usize> = (0..sdf.len()).filter(|i| sdf[*i].abs() <= width).collect();
            if !idx.is_empty() {
                let k = REFINE_K;
                #[allow(clippy::cast_precision_loss)]
                let off: Vec<f64> = (0..k).map(|i| ((i as f64 + 0.5) / k as f64 - 0.5) * grid.h).collect();
                let mut pts = Vec::with_capacity(idx.len() * k * k * k);
                for &i in &idx {
                    for ox in &off {
                        for oy in &off {
                            for oz in &off {
                                pts.push([p[i][0] + ox, p[i][1] + oy, p[i][2] + oz]);
                            }
                        }
                    }
                }
                let (s, _, _) = sdf_at(&mesh, &pts);
                #[allow(clippy::cast_precision_loss)]
                let sub = mask_from_sdf(&s, grid.h / k as f64);
                for (n, &i) in idx.iter().enumerate() {
                    mask[i] = crate::numpy::mean(&sub[n * k * k * k..(n + 1) * k * k * k]);
                }
            }
        }
        let hard = mask.iter().map(|m| if *m > 0.0 { 1.0 } else { 0.0 }).collect();
        Ok(Self {
            mesh,
            grid,
            width,
            mask,
            hard,
            sdf,
            winding,
            nearest_tri,
            node_sdf: std::sync::OnceLock::new(),
        })
    }

    pub fn node_sdf(&self) -> &[f64] {
        self.node_sdf.get_or_init(|| {
            let g = self.grid;
            let mut pts = Vec::new();
            #[allow(clippy::cast_precision_loss)]
            for i in 0..=g.counts[0] {
                for j in 0..=g.counts[1] {
                    for k in 0..=g.counts[2] {
                        let idx = [i, j, k];
                        pts.push(std::array::from_fn(|a| g.origin[a] + idx[a] as f64 * g.h));
                    }
                }
            }
            sdf_at(&self.mesh, &pts).0
        })
    }

    #[must_use]
    pub fn stats(&self) -> Value {
        let cell = self.grid.cell_volume();
        let vol_mask = crate::numpy::sum(&self.mask) * cell;
        let vol_mesh = mesh_volume(&self.mesh);
        #[allow(clippy::cast_precision_loss)]
        let n = self.mask.len() as f64;
        #[allow(clippy::cast_precision_loss)]
        let band = self.sdf.iter().filter(|s| s.abs() <= self.width).count() as f64 / n;
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in &self.mesh.v {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        json!({
            "triangles": self.mesh.f.len(), "vertices": self.mesh.v.len(), "watertight": true,
            "mesh_volume_mm3": vol_mesh * 1e9, "mask_volume_mm3": vol_mask * 1e9,
            "volume_rel_err": (vol_mask - vol_mesh).abs() / vol_mesh.max(1e-300),
            "mesh_area_mm2": mesh_area(&self.mesh) * 1e6,
            "elements_inside": self.mask.iter().filter(|m| **m >= 0.5).count(),
            "elements_partial": self.mask.iter().filter(|m| **m > 0.0 && **m < 1.0).count(),
            "fill_fraction": crate::numpy::mean(&self.mask), "band_fraction": band, "mask_width_mm": self.width * 1e3,
            "bbox_mm": [lo.map(|v| v * 1e3), hi.map(|v| v * 1e3)],
        })
    }
}

