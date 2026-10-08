// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use implexity_core::json::{DumpOptions, dumps};
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::{py_eq, py_str, truthy};

use super::embed::{capability, format_of, read_embedded};
use super::{CONT_KEYS, ProvResult, canonical, core, format_e, get, invalid, jsonable_f64, pyfloat, pyint};

pub const CONFIRMED: &str = "confirmed";
pub const REFUTED: &str = "refuted";
pub const UNCHECKABLE: &str = "not_checkable_here";

pub const TOL_VOLUME_REL: f64 = 1e-5;
pub const TOL_AREA_REL: f64 = 1e-5;
pub const TOL_BBOX_ABS_MM: f64 = 1e-4;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mesh {
    pub vertices: Vec<[f64; 3]>,
    pub faces: Vec<[i64; 3]>,
}

fn verr(msg: impl Into<String>) -> super::ProvenanceError {
    invalid(msg)
}

fn read_stl(path: &Path) -> ProvResult<(Mesh, String)> {
    let raw = std::fs::read(path).map_err(|e| verr(format!("{}: {e}", path.display())))?;
    if raw.len() < 84 {
        return Err(verr(format!("{}: shorter than an STL header", path.display())));
    }
    let n = u32::from_le_bytes([raw[80], raw[81], raw[82], raw[83]]) as usize;
    let want = 84 + n * 50;
    if raw.len() != want {
        return Err(verr(format!(
            "{}: {n} facets declared needs {want} bytes, file is {}",
            path.display(),
            raw.len()
        )));
    }
    let mut mesh = Mesh::default();
    for t in 0..n {
        let base = 84 + t * 50 + 12;
        let mut face = [0i64; 3];
        for (c, slot) in face.iter_mut().enumerate() {
            let mut v = [0.0f64; 3];
            for (d, x) in v.iter_mut().enumerate() {
                let o = base + c * 12 + d * 4;
                *x = f64::from(f32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]));
            }
            *slot = i64::try_from(mesh.vertices.len()).unwrap_or(i64::MAX);
            mesh.vertices.push(v);
        }
        mesh.faces.push(face);
    }
    Ok((mesh, "binary STL, struct-parsed against the format definition".into()))
}

fn ply_type(name: &str) -> ProvResult<(usize, char)> {

    Ok(match name {
        "float" | "float32" => (4, 'f'),
        "double" => (8, 'f'),
        "int" | "int32" => (4, 'i'),
        "uint" => (4, 'u'),
        "uchar" | "uint8" => (1, 'u'),
        "char" => (1, 'i'),
        "short" => (2, 'i'),
        "ushort" => (2, 'u'),
        other => return Err(verr(format!("KeyError: {}", repr_str(other)))),
    })
}

fn ply_value(raw: &[u8], off: usize, ty: (usize, char)) -> ProvResult<f64> {
    let b = raw.get(off..off + ty.0).ok_or_else(|| verr("buffer is smaller than requested size"))?;
    #[allow(clippy::cast_precision_loss)]
    let v = match (ty.0, ty.1) {
        (4, 'f') => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        (8, 'f') => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        (4, 'i') => f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        (4, _) => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        (2, 'i') => f64::from(i16::from_le_bytes([b[0], b[1]])),
        (2, _) => f64::from(u16::from_le_bytes([b[0], b[1]])),
        (1, 'i') => f64::from(i8::from_le_bytes([b[0]])),
        _ => f64::from(b[0]),
    };
    Ok(v)
}

fn ply_int(raw: &[u8], off: usize, ty: (usize, char)) -> ProvResult<i64> {
    let b = raw.get(off..off + ty.0).ok_or_else(|| verr("buffer is smaller than requested size"))?;
    Ok(match (ty.0, ty.1) {
        (4, 'i') => i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        (4, 'u') => i64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        (2, 'i') => i64::from(i16::from_le_bytes([b[0], b[1]])),
        (2, _) => i64::from(u16::from_le_bytes([b[0], b[1]])),
        (1, 'i') => i64::from(i8::from_le_bytes([b[0]])),
        (1, _) => i64::from(b[0]),
        _ => {
            #[allow(clippy::cast_possible_truncation)]
            let v = ply_value(raw, off, ty)? as i64;
            v
        }
    })
}

#[allow(clippy::too_many_lines)]
fn read_ply(path: &Path) -> ProvResult<(Mesh, String)> {
    let raw = std::fs::read(path).map_err(|e| verr(format!("{}: {e}", path.display())))?;
    let marker = b"end_header\n";
    let i = raw
        .windows(marker.len())
        .position(|w| w == marker)
        .ok_or_else(|| verr(format!("{}: no PLY end_header", path.display())))?;
    let head_text = String::from_utf8_lossy(&raw[..i]).into_owned();
    let head: Vec<&str> = head_text.lines().collect();
    if head.first().is_none_or(|h| h.trim() != "ply") {
        return Err(verr(format!("{}: not a PLY", path.display())));
    }
    let mut fmt: Option<String> = None;
    let mut elements: Vec<(String, usize, Vec<Vec<String>>)> = Vec::new();
    for line in &head[1..] {
        let bits: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = bits.first() else { continue };
        match *first {
            "format" => fmt = bits.get(1).map(|s| (*s).to_string()),
            "element" => {
                let name = bits.get(1).ok_or_else(|| verr("list index out of range"))?;
                let count = bits.get(2).ok_or_else(|| verr("list index out of range"))?;
                let count: usize = count.parse().map_err(|_| {
                    verr(format!("invalid literal for int() with base 10: {}", repr_str(count)))
                })?;
                elements.push(((*name).to_string(), count, Vec::new()));
            }
            "property" => {
                if let Some(last) = elements.last_mut() {
                    last.2.push(bits[1..].iter().map(|s| (*s).to_string()).collect());
                }
            }
            _ => {}
        }
    }
    if fmt.as_deref() != Some("binary_little_endian") {
        return Err(verr(format!(
            "{}: this reader handles binary_little_endian, the file says {}",
            path.display(),
            fmt.as_deref().map_or_else(|| "None".to_string(), repr_str)
        )));
    }
    let mut off = i + marker.len();
    let mut vertices: Option<Vec<[f64; 3]>> = None;
    let mut faces: Option<Vec<[i64; 3]>> = None;
    for (name, count, props) in &elements {
        if props.first().is_some_and(|p| p.first().map(String::as_str) == Some("list")) {
            let p = &props[0];
            let count_ty = ply_type(p.get(1).map_or("", String::as_str))?;
            let index_ty = ply_type(p.get(2).map_or("", String::as_str))?;
            let mut rows: Vec<Vec<i64>> = Vec::with_capacity(*count);
            for _ in 0..*count {
                let k = ply_int(&raw, off, count_ty)?;
                off += count_ty.0;
                let k = usize::try_from(k).map_err(|_| verr("negative dimensions are not allowed"))?;
                let mut idx = Vec::with_capacity(k);
                for m in 0..k {
                    idx.push(ply_int(&raw, off + m * index_ty.0, index_ty)?);
                }
                off += k * index_ty.0;
                rows.push(idx);
            }
            if name == "face" {
                let mut f = Vec::with_capacity(rows.len());
                for r in &rows {
                    if r.len() < 3 {
                        return Err(verr(
                            "all the input array dimensions except for the concatenation axis must match exactly",
                        ));
                    }
                    f.push([r[0], r[1], r[2]]);
                }
                faces = Some(f);
            }
        } else {
            let mut layout = Vec::with_capacity(props.len());
            let mut stride = 0;
            for p in props {
                let ty = ply_type(p.first().map_or("", String::as_str))?;
                layout.push((p.get(1).cloned().unwrap_or_default(), stride, ty));
                stride += ty.0;
            }
            if raw.len() < off + count * stride {
                return Err(verr("buffer is smaller than requested size"));
            }
            if name == "vertex" {
                let field = |n: &str| {
                    layout
                        .iter()
                        .find(|(pn, _, _)| pn == n)
                        .map(|(_, o, t)| (*o, *t))
                        .ok_or_else(|| verr(format!("no field of name {n}")))
                };
                let (fx, fy, fz) = (field("x")?, field("y")?, field("z")?);
                let mut v = Vec::with_capacity(*count);
                for r in 0..*count {
                    let base = off + r * stride;
                    v.push([
                        ply_value(&raw, base + fx.0, fx.1)?,
                        ply_value(&raw, base + fy.0, fy.1)?,
                        ply_value(&raw, base + fz.0, fz.1)?,
                    ]);
                }
                vertices = Some(v);
            }
            off += count * stride;
        }
    }
    match (vertices, faces) {
        (Some(vertices), Some(faces)) => {
            Ok((Mesh { vertices, faces }, "binary PLY, header and body parsed against the format".into()))
        }
        _ => Err(verr(format!("{}: no vertex/face elements", path.display()))),
    }
}

#[derive(Debug, Clone, Default)]
struct Element {
    ns: String,
    local: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Element>,
}

fn decode_entities(s: &str) -> ProvResult<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let j = rest[i..].find(';').ok_or_else(|| verr("not well-formed (invalid token)"))? + i;
        let ent = &rest[i + 1..j];
        let c = match ent {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(h) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                    u32::from_str_radix(h, 16).ok()
                } else {
                    ent.strip_prefix('#').and_then(|d| d.parse().ok())
                };
                code.and_then(char::from_u32).ok_or_else(|| verr(format!("undefined entity &{ent};")))?
            }
        };
        out.push(c);
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

struct XmlParser<'a> {
    s: &'a str,
    at: usize,
}

impl XmlParser<'_> {
    fn skip_misc(&mut self) -> ProvResult<()> {
        loop {
            let rest = &self.s[self.at..];
            let trimmed = rest.trim_start();
            self.at += rest.len() - trimmed.len();
            if trimmed.starts_with("<?") {
                self.at += trimmed.find("?>").ok_or_else(|| verr("unclosed token"))? + 2;
            } else if trimmed.starts_with("<!--") {
                self.at += trimmed.find("-->").ok_or_else(|| verr("unclosed token"))? + 3;
            } else if trimmed.starts_with("<!") {
                self.at += trimmed.find('>').ok_or_else(|| verr("unclosed token"))? + 1;
            } else {
                return Ok(());
            }
        }
    }

    fn element(&mut self, scopes: &mut Vec<BTreeMap<String, String>>) -> ProvResult<Element> {
        let rest = &self.s[self.at..];
        if !rest.starts_with('<') {
            return Err(verr("syntax error"));
        }
        let end = rest.find('>').ok_or_else(|| verr("unclosed token"))?;

        let mut q: Option<char> = None;
        let mut close = None;
        for (i, c) in rest.char_indices().skip(1) {
            match (q, c) {
                (Some(open), c) if c == open => q = None,
                (None, '"' | '\'') => q = Some(c),
                (None, '>') => {
                    close = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let close = close.unwrap_or(end);
        let inner = &rest[1..close];
        let self_closing = inner.ends_with('/');
        let inner = inner.trim_end_matches('/');
        let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
        let qname = &inner[..name_end];
        let mut raw_attrs: Vec<(String, String)> = Vec::new();
        let mut a = inner[name_end..].trim_start();
        while !a.is_empty() {
            let eq = a.find('=').ok_or_else(|| verr("not well-formed (invalid token)"))?;
            let key = a[..eq].trim().to_string();
            let after = a[eq + 1..].trim_start();
            let quote = after
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
                .ok_or_else(|| verr("not well-formed (invalid token)"))?;
            let vend = after[1..].find(quote).ok_or_else(|| verr("not well-formed (invalid token)"))? + 1;
            raw_attrs.push((key, decode_entities(&after[1..vend])?));
            a = after[vend + 1..].trim_start();
        }
        let mut scope = scopes.last().cloned().unwrap_or_default();
        for (k, v) in &raw_attrs {
            if k == "xmlns" {
                scope.insert(String::new(), v.clone());
            } else if let Some(p) = k.strip_prefix("xmlns:") {
                scope.insert(p.to_string(), v.clone());
            }
        }
        let (prefix, local) = qname.split_once(':').unwrap_or(("", qname));
        let ns = scope.get(prefix).cloned().unwrap_or_default();
        let mut el = Element { ns, local: local.to_string(), ..Element::default() };
        for (k, v) in raw_attrs {
            if !k.contains(':') && k != "xmlns" {
                el.attrs.insert(k, v);
            }
        }
        self.at += close + 1;
        if self_closing {
            return Ok(el);
        }
        scopes.push(scope);
        loop {
            let rest = &self.s[self.at..];
            let lt = rest.find('<').ok_or_else(|| verr("no element found"))?;
            self.at += lt;
            let rest = &self.s[self.at..];
            if rest.starts_with("</") {
                self.at += rest.find('>').ok_or_else(|| verr("unclosed token"))? + 1;
                break;
            } else if rest.starts_with("<!--") {
                self.at += rest.find("-->").ok_or_else(|| verr("unclosed token"))? + 3;
            } else if rest.starts_with("<![CDATA[") {
                self.at += rest.find("]]>").ok_or_else(|| verr("unclosed token"))? + 3;
            } else if rest.starts_with("<?") {
                self.at += rest.find("?>").ok_or_else(|| verr("unclosed token"))? + 2;
            } else {
                let child = self.element(scopes)?;
                el.children.push(child);
            }
        }
        scopes.pop();
        Ok(el)
    }
}

fn parse_xml(text: &str) -> ProvResult<Element> {
    let mut p = XmlParser { s: text.trim_start_matches('\u{feff}'), at: 0 };
    p.skip_misc()?;
    p.element(&mut Vec::new())
}

fn collect<'a>(el: &'a Element, ns: &str, local: &str, out: &mut Vec<&'a Element>) {
    if el.ns == ns && el.local == local {
        out.push(el);
    }
    for c in &el.children {
        collect(c, ns, local, out);
    }
}

fn read_3mf(path: &Path) -> ProvResult<(Mesh, String)> {
    let raw = std::fs::read(path).map_err(|e| verr(format!("{}: {e}", path.display())))?;
    let z = crate::zip::ZipArchive::new(&raw).map_err(|e| verr(e.0))?;
    let data = z
        .read("3D/3dmodel.model")
        .map_err(|_| verr("\"There is no item named '3D/3dmodel.model' in the archive\""))?;
    let text = String::from_utf8(data).map_err(|e| verr(e.to_string()))?;
    let root = parse_xml(&text)?;
    let ns = root.ns.clone();
    let unit = root.attrs.get("unit").cloned().unwrap_or_else(|| "millimeter".into());
    let mut objects = Vec::new();
    collect(&root, &ns, "object", &mut objects);
    let mut vertices: Vec<[f64; 3]> = Vec::new();
    let mut faces: Vec<[i64; 3]> = Vec::new();
    let child =
        |el: &Element, local: &str| el.children.iter().find(|c| c.ns == ns && c.local == local).cloned();
    let attr_f = |el: &Element, k: &str| -> ProvResult<f64> {
        let v = el.attrs.get(k).ok_or_else(|| verr(format!("KeyError: {}", repr_str(k))))?;
        pyfloat(&json!(v)).ok_or_else(|| verr(format!("could not convert string to float: {}", repr_str(v))))
    };
    let attr_i = |el: &Element, k: &str| -> ProvResult<i64> {
        let v = el.attrs.get(k).ok_or_else(|| verr(format!("KeyError: {}", repr_str(k))))?;
        pyint(&json!(v))
            .ok_or_else(|| verr(format!("invalid literal for int() with base 10: {}", repr_str(v))))
    };
    for obj_el in objects {
        let Some(mesh) = child(obj_el, "mesh") else { continue };
        let base = i64::try_from(vertices.len()).unwrap_or(i64::MAX);
        let vs = child(&mesh, "vertices").ok_or_else(|| verr("'NoneType' object is not iterable"))?;
        for v in &vs.children {
            vertices.push([attr_f(v, "x")?, attr_f(v, "y")?, attr_f(v, "z")?]);
        }
        let ts = child(&mesh, "triangles").ok_or_else(|| verr("'NoneType' object is not iterable"))?;
        for t in &ts.children {
            faces.push([base + attr_i(t, "v1")?, base + attr_i(t, "v2")?, base + attr_i(t, "v3")?]);
        }
    }
    let scale = match unit.as_str() {
        "micron" => 1e-3,
        "centimeter" => 10.0,
        "inch" => 25.4,
        "foot" => 304.8,
        "meter" => 1000.0,
        _ => 1.0,
    };
    for v in &mut vertices {
        for x in v.iter_mut() {
            *x *= scale;
        }
    }
    Ok((
        Mesh { vertices, faces },
        format!("3MF: zipfile + ElementTree on 3D/3dmodel.model (unit {})", repr_str(&unit)),
    ))
}


pub fn read_mesh(path: &Path, fmt: &str) -> Result<(Mesh, String), String> {
    let result = match fmt {
        "stl" => read_stl(path),
        "ply" => read_ply(path),
        "3mf" => read_3mf(path),
        other => return Err(format!("no native reader for {}", repr_str(other))),
    };
    result.map_err(|e| e.py_repr())
}

fn resolve(i: i64, n: usize) -> ProvResult<usize> {
    let n_i = i64::try_from(n).unwrap_or(i64::MAX);
    let j = if i < 0 { i + n_i } else { i };
    usize::try_from(j)
        .ok()
        .filter(|&j| j < n)
        .ok_or_else(|| verr(format!("index {i} is out of bounds for axis 0 with size {n}")))
}

pub type Welded = (Vec<[f64; 3]>, Vec<[usize; 3]>, usize);


pub fn weld(mesh: &Mesh) -> ProvResult<Welded> {
    let key = |v: &[f64; 3]| -> [u8; 24] {
        let mut k = [0u8; 24];
        for (d, x) in v.iter().enumerate() {
            k[8 * d..8 * d + 8].copy_from_slice(&x.to_le_bytes());
        }
        k
    };
    let keys: Vec<[u8; 24]> = mesh.vertices.iter().map(key).collect();
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
    let mut inv = vec![0usize; keys.len()];
    let mut uniq: Vec<[f64; 3]> = Vec::new();
    for (pos, &i) in order.iter().enumerate() {
        if pos == 0 || keys[order[pos - 1]] != keys[i] {
            uniq.push(mesh.vertices[i]);
        }
        inv[i] = uniq.len() - 1;
    }
    let n = mesh.vertices.len();
    let mut faces = Vec::with_capacity(mesh.faces.len());
    let mut dropped = 0;
    for f in &mesh.faces {
        let w = [inv[resolve(f[0], n)?], inv[resolve(f[1], n)?], inv[resolve(f[2], n)?]];
        if w[0] != w[1] && w[1] != w[2] && w[2] != w[0] {
            faces.push(w);
        } else {
            dropped += 1;
        }
    }
    Ok((uniq, faces, dropped))
}

fn pairwise_sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        return a.iter().fold(if n == 0 { 0.0 } else { -0.0 }, |s, v| s + v);
    }
    if n <= 128 {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for k in 0..8 {
                r[k] += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        return res;
    }
    let mut n2 = n / 2;
    n2 -= n2 % 8;
    pairwise_sum(&a[..n2]) + pairwise_sum(&a[n2..])
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[must_use]
pub fn signed_volume(v: &[[f64; 3]], f: &[[usize; 3]]) -> f64 {
    let terms: Vec<f64> = f
        .iter()
        .map(|t| {
            let (a, c) = (v[t[0]], cross(v[t[1]], v[t[2]]));
            a[0] * c[0] + a[1] * c[1] + a[2] * c[2]
        })
        .collect();
    pairwise_sum(&terms) / 6.0
}

#[must_use]
pub fn area(v: &[[f64; 3]], f: &[[usize; 3]]) -> f64 {
    let terms: Vec<f64> = f
        .iter()
        .map(|t| {
            let c = cross(sub(v[t[1]], v[t[0]]), sub(v[t[2]], v[t[0]]));
            (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt()
        })
        .collect();
    0.5 * pairwise_sum(&terms)
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

#[must_use]
pub fn topology(n_vertices: usize, f: &[[usize; 3]]) -> Value {
    if f.is_empty() {
        return json!({"boundary_edges": 0, "nonmanifold_edges": 0, "components": 0, "euler_characteristic": 0,
                       "genus": null, "orientation_consistent": true, "watertight": false});
    }
    let mut directed: Vec<(usize, usize)> = Vec::with_capacity(3 * f.len());
    for (a, b) in [(0, 1), (1, 2), (2, 0)] {
        directed.extend(f.iter().map(|t| (t[a], t[b])));
    }
    let mut undirected: Vec<(usize, usize)> = directed.iter().map(|&(a, b)| (a.min(b), a.max(b))).collect();
    undirected.sort_unstable();
    let mut uniq: Vec<((usize, usize), usize)> = Vec::new();
    for e in undirected {
        match uniq.last_mut() {
            Some((last, c)) if *last == e => *c += 1,
            _ => uniq.push((e, 1)),
        }
    }
    let boundary = uniq.iter().filter(|(_, c)| *c == 1).count();
    let nonmanifold = uniq.iter().filter(|(_, c)| *c > 2).count();
    let mut ds = directed.clone();
    ds.sort_unstable();
    let mut rs: Vec<(usize, usize)> = directed.iter().map(|&(a, b)| (b, a)).collect();
    rs.sort_unstable();
    let consistent = ds == rs;
    let mut parent: Vec<usize> = (0..n_vertices).collect();
    for ((a, b), _) in &uniq {
        let (ra, rb) = (find(&mut parent, *a), find(&mut parent, *b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut used: Vec<usize> = f.iter().flat_map(|t| t.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let mut roots: Vec<usize> = used.iter().map(|&v| find(&mut parent, v)).collect();
    roots.sort_unstable();
    roots.dedup();
    let comps = roots.len();
    let chi = i64::try_from(used.len()).unwrap_or(0) - i64::try_from(uniq.len()).unwrap_or(0)
        + i64::try_from(f.len()).unwrap_or(0);
    #[allow(clippy::cast_precision_loss)]
    let genus = if comps > 0 { json!(comps as f64 - chi as f64 / 2.0) } else { Value::Null };
    json!({
        "boundary_edges": boundary, "nonmanifold_edges": nonmanifold, "components": comps,
        "euler_characteristic": chi, "genus": genus, "orientation_consistent": consistent,
        "watertight": boundary == 0 && nonmanifold == 0 && consistent,
    })
}

fn bbox(v: &[[f64; 3]]) -> ProvResult<[[f64; 3]; 2]> {
    if v.is_empty() {
        return Err(verr("zero-size array to reduction operation minimum which has no identity"));
    }
    let mut lo = v[0];
    let mut hi = v[0];
    for p in v {
        for d in 0..3 {

            if p[d].is_nan() || lo[d].is_nan() {
                lo[d] = f64::NAN;
            } else if p[d] < lo[d] {
                lo[d] = p[d];
            }
            if p[d].is_nan() || hi[d].is_nan() {
                hi[d] = f64::NAN;
            } else if p[d] > hi[d] {
                hi[d] = p[d];
            }
        }
    }
    Ok([lo, hi])
}

fn bbox_value(b: &[[f64; 3]; 2]) -> Value {
    json!([
        b[0].iter().map(|&x| jsonable_f64(x)).collect::<Vec<_>>(),
        b[1].iter().map(|&x| jsonable_f64(x)).collect::<Vec<_>>()
    ])
}


pub fn reread_mesh(path: &Path, fmt: &str) -> ProvResult<Value> {
    let (mesh, how) = match read_mesh(path, fmt) {
        Ok(x) => x,
        Err(detail) => return Ok(json!({"readable": false, "detail": detail})),
    };
    let (vw, fw, _n) = weld(&mesh)?;
    let topo = topology(vw.len(), &fw);
    let bb = bbox(&mesh.vertices)?;
    let mut out = json!({
        "readable": true, "reader": how, "triangles": mesh.faces.len(),
        "vertices_as_stored": mesh.vertices.len(), "vertices_welded": vw.len(),
        "bbox_mm": bbox_value(&bb),
        "signed_volume_mm3": jsonable_f64(signed_volume(&vw, &fw)),
        "area_mm2": jsonable_f64(area(&vw, &fw)),
    });
    if let (Value::Object(m), Value::Object(t)) = (&mut out, topo) {
        m.extend(t);
    }
    Ok(out)
}

#[allow(clippy::needless_pass_by_value)]
fn claim(name: &str, expected: Value, observed: Value, status: &str, method: &str, detail: &str) -> Value {
    json!({"claim": name, "expected": expected, "observed": observed, "status": status, "method": method,
           "detail": detail})
}

enum Tol {
    Rel(f64),
    Abs(f64),
}

fn cmp_num(
    name: &str,
    expected: &Value,
    observed: &Value,
    tol: &Tol,
    method: &str,
    note: Option<&str>,
) -> Value {
    if expected.is_null() {
        return claim(
            name,
            Value::Null,
            observed.clone(),
            UNCHECKABLE,
            method,
            "the record makes no claim about this",
        );
    }
    let (Some(e), Some(o)) = (pyfloat(expected), pyfloat(observed)) else {
        return claim(name, expected.clone(), observed.clone(), UNCHECKABLE, method, "not a number");
    };
    let (ok, detail) = match tol {
        Tol::Rel(rel) => {
            let scale = e.abs().max(1e-30);
            (
                (e - o).abs() <= rel * scale,
                format!(
                    "relative deviation {}, tolerance {}",
                    format_e((e - o).abs() / scale, 3),
                    format_e(*rel, 0)
                ),
            )
        }
        Tol::Abs(a) => (
            (e - o).abs() <= *a,
            format!("absolute deviation {}, tolerance {}", format_e((e - o).abs(), 3), format_e(*a, 0)),
        ),
    };
    let detail = match note {
        Some(n) if !n.is_empty() => format!("{detail} | {n}"),
        _ => detail,
    };
    claim(name, jsonable_f64(e), jsonable_f64(o), if ok { CONFIRMED } else { REFUTED }, method, &detail)
}

fn cmp_eq(name: &str, expected: &Value, observed: &Value, method: &str, note: Option<&str>) -> Value {
    if expected.is_null() {
        return claim(
            name,
            Value::Null,
            observed.clone(),
            UNCHECKABLE,
            method,
            "the record makes no claim about this",
        );
    }
    let ok = py_eq(expected, observed);
    let detail = match note {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => (if ok { "equal" } else { "NOT equal" }).to_string(),
    };
    claim(name, expected.clone(), observed.clone(), if ok { CONFIRMED } else { REFUTED }, method, &detail)
}

fn uncheckable(name: &str, expected: &Value, why: &str, method: &str) -> Value {
    claim(name, expected.clone(), Value::Null, UNCHECKABLE, method, why)
}

fn summarise(mut out: Value) -> Value {
    let claims = out["claims"].as_array().cloned().unwrap_or_default();
    let count = |s: &str| claims.iter().filter(|c| c["status"] == json!(s)).count();
    let refuted: Vec<Value> =
        claims.iter().filter(|c| c["status"] == json!(REFUTED)).map(|c| c["claim"].clone()).collect();
    let summary = json!({
        "claims": claims.len(), "confirmed": count(CONFIRMED), "refuted": count(REFUTED),
        "not_checkable_here": count(UNCHECKABLE),
        "refuted_claims": refuted,
        "verdict": if refuted.is_empty() { "confirmed" } else { "refuted" },
        "note": "a record that cannot be checked is worth less than one that can; not_checkable_here counts the claims this verifier deliberately does not pretend to have re-measured",
    });
    if let Value::Object(m) = &mut out {
        m.insert("summary".into(), summary);
    }
    out
}

fn verify_step(path: &Path, art: &Value) -> Vec<Value> {
    let brep = get(art, "brep");
    let faces = std::fs::read(path).ok().map(|raw| {
        let body = String::from_utf8_lossy(&raw).into_owned();
        count_advanced_faces(&body)
    });
    vec![
        cmp_eq(
            "brep.faces",
            get(brep, "faces"),
            &faces.map_or(Value::Null, |n| json!(n)),
            "a text scan for ADVANCED_FACE entities -- no STEP library",
            Some("the count of ADVANCED_FACE instances in the DATA section"),
        ),
        uncheckable(
            "brep.volume_mm3",
            get(brep, "volume_mm3"),
            "OpenCASCADE (OCP) is not importable here, so the B-rep cannot be re-opened: ModuleNotFoundError",
            "OCP",
        ),
        uncheckable("brep.solids", get(brep, "solids"), "OpenCASCADE (OCP) is not importable here", "OCP"),
    ]
}

#[must_use]
pub fn count_advanced_faces(body: &str) -> usize {
    let needle = "ADVANCED_FACE(";
    let mut n = 0;
    let mut at = 0;
    while let Some(off) = body[at..].find(needle) {
        let i = at + off;
        let before = body[..i].trim_end_matches(char::is_whitespace);
        if before.ends_with('=') {
            n += 1;
        }
        at = i + needle.len();
    }
    n
}


#[allow(clippy::too_many_lines)]
pub fn verify(artefact: &Path, record: &Value, fmt: Option<&str>) -> ProvResult<Value> {
    let fmt: Option<String> = fmt.map(str::to_string).or_else(|| format_of(artefact).map(str::to_string));
    let f = fmt.clone().unwrap_or_default();
    let name = artefact.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut claims: Vec<Value> = Vec::new();
    let finish = |claims: Vec<Value>| {
        summarise(json!({"artefact": name, "format": fmt, "record_id": get(record, "record_id"),
                         "record_schema": get(record, "schema"), "claims": claims}))
    };
    if !artefact.is_file() {
        claims.push(claim("exists", json!(true), json!(false), REFUTED, "os.path.isfile", "no such file"));
        return Ok(finish(claims));
    }
    let stamped = get(get(record, "stamped"), &f).clone();
    let art = get(get(record, "artefacts"), &f).clone();
    let nbytes = std::fs::metadata(artefact).map(|m| m.len()).map_err(|e| invalid(e.to_string()))?;
    if get(&stamped, "bytes").is_null() {
        claims.push(uncheckable(
            "bytes",
            &Value::Null,
            &format!("this copy of the record carries no post-embedding size, which is what the copy INSIDE the file looks like; the sidecar carries it under stamped.{f}.bytes"),
            "os.path.getsize",
        ));
    } else {
        claims.push(cmp_eq(
            "bytes",
            get(&stamped, "bytes"),
            &json!(nbytes),
            "os.path.getsize",
            Some("the size of the file AFTER the record was embedded in it; the sidecar is the only copy that can carry it"),
        ));
    }
    if truthy(get(&stamped, "sha256")) {
        let sha = crate::digest::sha256_file(artefact).map_err(|e| invalid(e.to_string()))?;
        claims.push(cmp_eq(
            "sha256",
            get(&stamped, "sha256"),
            &json!(sha),
            "hashlib.sha256 over the file",
            None,
        ));
    } else {
        claims.push(uncheckable(
            "sha256",
            &Value::Null,
            &format!("a file cannot contain its own hash; the sidecar carries it under stamped.{f}.sha256"),
            "hashlib.sha256",
        ));
    }

    match read_embedded(artefact, fmt.as_deref()) {
        Err(e) => claims.push(claim(
            "embedded_record",
            get(record, "record_id").clone(),
            Value::Null,
            REFUTED,
            "implexity.provenance.read_embedded",
            &format!("the embedded record could not be read: {}", e.py_repr()),
        )),
        Ok(emb) => {
            let caps = capability();
            let carries = get(get(&caps, &f), "carries").as_str().unwrap_or("nothing").to_string();
            match emb {
                None => claims.push(cmp_eq(
                    "embedded_record",
                    &json!(carries),
                    &json!("nothing"),
                    "implexity.provenance.read_embedded",
                    Some("no implexity record found in the file"),
                )),
                Some(emb) => {
                    claims.push(cmp_eq(
                        "embedded_record_id",
                        get(record, "record_id"),
                        get(&emb, "record_id"),
                        "implexity.provenance.read_embedded",
                        None,
                    ));
                    if carries == "record" {
                        let same = canonical(&core(&emb)) == canonical(&core(record));
                        claims.push(claim(
                            "embedded_record_matches_sidecar",
                            json!(true),
                            json!(same),
                            if same { CONFIRMED } else { REFUTED },
                            "canonical JSON of core(record), byte compare",
                            if same {
                                "byte-identical outside the `stamped` block"
                            } else {
                                "the embedded copy and this record DIFFER"
                            },
                        ));
                    }
                }
            }
        }
    }

    let g = get(&art, "geometry").clone();
    match f.as_str() {
        "stl" | "ply" | "3mf" => {
            let (mesh, how) = match read_mesh(artefact, &f) {
                Ok(x) => x,
                Err(detail) => {
                    claims.push(claim(
                        "readable",
                        json!(true),
                        json!(false),
                        REFUTED,
                        "native reader",
                        &detail,
                    ));
                    return Ok(finish(claims));
                }
            };
            let (vw, fw, _ndeg) = weld(&mesh)?;
            let topo = topology(vw.len(), &fw);
            claims.push(cmp_eq("triangles", get(&g, "triangles"), &json!(mesh.faces.len()), &how, None));
            if f == "stl" {
                let note = format!(
                    "STL stores no shared vertices; the file's {} corner triples weld EXACTLY to {}",
                    mesh.vertices.len(),
                    vw.len()
                );
                claims.push(cmp_eq("vertices", get(&g, "vertices"), &json!(vw.len()), &how, Some(&note)));
            } else {
                claims.push(cmp_eq("vertices", get(&g, "vertices"), &json!(mesh.vertices.len()), &how, None));
            }
            claims.push(cmp_num(
                "signed_volume_mm3",
                get(&g, "signed_volume_mm3"),
                &jsonable_f64(signed_volume(&vw, &fw)),
                &Tol::Rel(TOL_VOLUME_REL),
                &how,
                Some("divergence theorem over the welded mesh"),
            ));
            claims.push(cmp_num(
                "area_mm2",
                get(&g, "area_mm2"),
                &jsonable_f64(area(&vw, &fw)),
                &Tol::Rel(TOL_AREA_REL),
                &how,
                None,
            ));
            let bb = get(&g, "bbox_mm");
            let obs = bbox(&mesh.vertices)?;
            if bb.is_null() {
                claims.push(uncheckable(
                    "bbox_mm",
                    &Value::Null,
                    "the record makes no bounding-box claim",
                    &how,
                ));
            } else {
                let mut dev: Option<f64> = None;
                for (r0, r1) in bb.as_array().into_iter().flatten().zip(obs.iter()) {
                    for (a, b) in r0.as_array().into_iter().flatten().zip(r1.iter()) {
                        let a = pyfloat(a).ok_or_else(|| invalid("bbox_mm entries must be numbers"))?;
                        let d = (a - b).abs();
                        dev = Some(match dev {

                            Some(m) if d.partial_cmp(&m) != Some(std::cmp::Ordering::Greater) => m,
                            _ => d,
                        });
                    }
                }
                let dev = dev.ok_or_else(|| invalid("max() arg is an empty sequence"))?;
                claims.push(claim(
                    "bbox_mm",
                    bb.clone(),
                    bbox_value(&obs),
                    if dev <= TOL_BBOX_ABS_MM { CONFIRMED } else { REFUTED },
                    &how,
                    &format!(
                        "max corner deviation {} mm, tolerance {} mm",
                        format_e(dev, 3),
                        format_e(TOL_BBOX_ABS_MM, 0)
                    ),
                ));
            }
            let four = json!({"boundary_edges": topo["boundary_edges"], "nonmanifold_edges": topo["nonmanifold_edges"],
                              "orientation_consistent": topo["orientation_consistent"]});
            let note = format!("all four: {}", dumps(&four, &DumpOptions::default()));
            claims.push(cmp_eq("watertight", get(&g, "watertight"), &topo["watertight"], &how, Some(&note)));
            let wc = get(&g, "watertight_components");
            claims.push(cmp_eq(
                "boundary_edges",
                get(wc, "boundary_edges"),
                &topo["boundary_edges"],
                &how,
                None,
            ));
            claims.push(cmp_eq(
                "nonmanifold_edges",
                get(wc, "nonmanifold_edges"),
                &topo["nonmanifold_edges"],
                &how,
                None,
            ));
            claims.push(cmp_eq(
                "orientation_consistent",
                get(wc, "orientation_consistent"),
                &topo["orientation_consistent"],
                &how,
                None,
            ));
            claims.push(cmp_eq("components", get(&g, "components"), &topo["components"], &how, None));
            claims.push(cmp_num(
                "genus",
                get(&g, "genus"),
                &topo["genus"],
                &Tol::Abs(1e-9),
                &how,
                Some("chi = V - E + F on the welded mesh, genus = components - chi/2"),
            ));
        }
        "step" => claims.extend(verify_step(artefact, &art)),
        "npz" => {
            claims.push(uncheckable(
                "geometry",
                &Value::Null,
                "a design .npz holds control fields, not geometry; nothing about a surface can be re-checked from it without the evaluator",
                "numpy.load",
            ));
            match npz_channels(artefact) {
                Err(detail) => {
                    claims.push(claim("readable", json!(true), json!(false), REFUTED, "numpy.load", &detail));
                }
                Ok((got, meta)) => {
                    let dd = get(record, "design");
                    claims.push(cmp_eq(
                        "design.channels",
                        get(dd, "channels"),
                        &json!(got),
                        "numpy.load over the .npz member names",
                        None,
                    ));
                    for k in CONT_KEYS {
                        let entry = get(get(dd, "continuation"), k);
                        let claimed = get(entry, "value");
                        let name = format!("design.continuation.{k}");
                        if let Some(v) = meta.get(k) {
                            claims.push(cmp_num(
                                &name,
                                claimed,
                                &jsonable_f64(*v),
                                &Tol::Abs(0.0),
                                "numpy.load",
                                None,
                            ));
                        } else {
                            claims.push(uncheckable(
                                &name,
                                claimed,
                                &format!(
                                    "not stored in this .npz; the record marks it '{}'",
                                    py_str(get(entry, "source"))
                                ),
                                "numpy.load",
                            ));
                        }
                    }
                }
            }
        }
        _ => {
            let shown = fmt.as_deref().map_or_else(|| "None".to_string(), repr_str);
            claims.push(uncheckable(
                "geometry",
                &Value::Null,
                &format!("no native reader for {shown} in this module; the file's size and hash are still checked above"),
                "implexity.provenance",
            ));
        }
    }

    let tol = get(&art, "tolerance");
    if !get(tol, "achieved_area_weighted_mm").is_null() {
        claims.push(uncheckable(
            "tolerance.achieved_area_weighted_mm",
            get(tol, "achieved_area_weighted_mm"),
            "the chord error is a distance from the mesh to the DESIGN's level set; re-checking it needs the design and the evaluator, which is exactly what this verifier does not have",
            "n/a",
        ));
    }
    let vol = get(&art, "volume");
    if !get(vol, "volume_fraction_density").is_null() {
        claims.push(uncheckable(
            "volume.volume_fraction_density",
            get(vol, "volume_fraction_density"),
            "the density integral is a property of the design field, not of the exported surface",
            "n/a",
        ));
    }
    let run = get(record, "run");
    if truthy(get(run, "loss")) {
        claims.push(uncheckable(
            "run.loss",
            get(get(run, "loss"), "best"),
            "the loss trajectory is a property of the run, not of the file; re-checking it means re-running the optimiser",
            "n/a",
        ));
    }
    Ok(finish(claims))
}

type NpzChannels = (Vec<String>, BTreeMap<String, f64>);

fn npz_channels(path: &Path) -> Result<NpzChannels, String> {
    let npz = crate::npz::load_file(path).map_err(|e| format!("ValueError({})", repr_str(&e.to_string())))?;
    let mut got: Vec<String> = npz
        .files()
        .into_iter()
        .filter(|k| !CONT_KEYS.contains(k) && *k != "implexity_provenance")
        .map(str::to_string)
        .collect();
    got.sort();
    let mut meta = BTreeMap::new();
    for k in CONT_KEYS {
        if let Some(a) = npz.get(k) {
            let v = a.to_f64().filter(|x| x.len() == 1).and_then(|x| x.iter().next().copied()).ok_or_else(
                || "TypeError('only length-1 arrays can be converted to Python scalars')".to_string(),
            )?;
            meta.insert(k.to_string(), v);
        }
    }
    Ok((got, meta))
}


pub fn verify_with_sidecar(artefact: &Path, sidecar: &Path, fmt: Option<&str>) -> ProvResult<Value> {
    let raw = std::fs::read(sidecar).map_err(|e| invalid(format!("{}: {e}", sidecar.display())))?;
    let text = String::from_utf8(raw).map_err(|e| invalid(e.to_string()))?;
    let record = implexity_core::json::parse_strict(&text).map_err(|e| invalid(e.to_string()))?;
    verify(artefact, &record, fmt)
}
