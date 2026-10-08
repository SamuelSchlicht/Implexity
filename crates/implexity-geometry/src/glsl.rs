// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use serde_json::{Map, Value, json};

use crate::error::{GResult, GeometryError};
use crate::eval::{
    Aabb, DEFAULT_SMOOTH_MM, EvalOptions, aabb_of, eval_points, field_class_of, field_class_with_overrides,
    node_id,
};
use crate::fieldclass::{ClassKind, FieldClass};
use crate::glsl_text::{PRELUDE, TEXTURE_BODY};
use crate::node::{Mode, Node, NodeRef, Registry, SmoothKind};
use crate::ops::{ArrayLinear, ArrayPolar, Blend, BoolOp};
use crate::pyfmt::{self, PyObj, str_repr};
use crate::tpms::{Family, Tpms};
use crate::value::ParamValue;

pub const GLSL_VERSION: &str = "300 es";
pub const DEFAULT_TEXTURE_RES: usize = 128;
pub const DEFAULT_TEXTURE_MAX_SAMPLES: usize = 6 * 1024 * 1024;
pub const TEXTURE_MEASURE_SAMPLES: usize = 4000;
pub const CACHE_MAX: usize = 32;
pub const TEX_CACHE_MAX: usize = 8;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

#[must_use]
pub fn texture_res() -> usize {
    env_usize("IMPLEXITY_GLSL_TEXTURE_RES", DEFAULT_TEXTURE_RES)
}

#[must_use]
pub fn texture_max_samples() -> usize {
    env_usize("IMPLEXITY_GLSL_TEXTURE_MAX", DEFAULT_TEXTURE_MAX_SAMPLES)
}

fn terr(message: impl Into<String>) -> GeometryError {
    GeometryError::Transpile { message: message.into(), refusals: Vec::new() }
}


pub fn float_literal(x: f64) -> GResult<String> {
    if !x.is_finite() {
        return Err(terr(format!("a parameter is {}; a shader cannot carry it", pyfmt::float_repr(x))));
    }
    let s = pyfmt::fmt_g(x, 9);
    if let Some((m, e)) = s.split_once('e') {
        let m = if m.contains('.') { m.to_string() } else { format!("{m}.0") };
        return Ok(format!("{m}e{e}"));
    }
    if s.contains('.') { Ok(s) } else { Ok(format!("{s}.0")) }
}

#[must_use]
pub fn ident(text: &str) -> String {
    let mut s: String = text.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if s.is_empty() { "_".into() } else { s }
}

fn indent(body: &str) -> String {
    body.trim_end_matches('\n')
        .split('\n')
        .map(|ln| if ln.trim().is_empty() { ln.to_string() } else { format!("    {ln}") })
        .collect::<Vec<_>>()
        .join("\n")
}

pub const REFUSED: [(&str, &str); 6] = [
    (
        "lattice.controlled_assembly",
        "transformed controlled volumes: use the shared sampled-field raymarcher",
    ),
    (
        "cell_grid_field",
        "cell-centred sampled implicit field: use the registered texture path, not scalar uniforms",
    ),
    (
        "lattice.controlled",
        "spatial control tensor with nonlocal synthesis: use the shared sampled-field raymarcher; no scalar-uniform approximation",
    ),
    ("mesh_sdf", "no closed form: the field IS a sampled array (that is what the node is)"),
    ("grid_field", "no closed form: the field IS a sampled array (that is what the node is)"),
    (
        "spline_curve",
        "no closed form: the control arrays are vectors of arbitrary length, not scalar uniforms, and the distance is a minimum over a sampled polyline",
    ),
];

pub const EMITTED: [&str; 34] = [
    "sphere",
    "box",
    "rounded_box",
    "cylinder",
    "capsule",
    "torus",
    "plane",
    "cone",
    "constant",
    "union",
    "intersect",
    "difference",
    "fillet",
    "chamfer",
    "offset",
    "with_fields",
    "shell",
    "negate",
    "remap.affine",
    "remap.clamp",
    "remap.soft_clamp",
    "interpolate",
    "mask",
    "translate",
    "rotate",
    "scale.uniform",
    "scale.nonuniform",
    "scale.nonuniform_safe",
    "warp.twist",
    "warp.bend",
    "warp.taper",
    "array.linear",
    "array.polar",
    "tpms",
];

fn refused_reason(kind: &str) -> Option<&'static str> {
    REFUSED.iter().find(|(k, _)| *k == kind).map(|(_, r)| *r)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Uniform {
    pub name: String,
    pub ty: String,
    pub value: f64,
    pub path: String,
    pub param: String,
    pub node: String,
    pub units: String,
    pub discrete: bool,
}

impl Uniform {
    fn to_json(&self) -> Value {
        json!({"name": self.name, "type": self.ty, "value": self.value, "path": self.path, "param": self.param,
            "node": self.node, "units": self.units, "discrete": self.discrete})
    }
}

pub struct Emitter {
    mode: String,
    smooth_kind: String,
    tpms_range_reduce: bool,
    uniforms: HashMap<String, Uniform>,
    uniform_order: Vec<String>,
    param_map: HashMap<(String, String), String>,
    funcs: Vec<(String, String)>,
    seen: HashMap<usize, String>,
    refusals: Vec<Map<String, Value>>,
    samplers: BTreeMap<String, Map<String, Value>>,
    n: usize,
    used_smooth_r: bool,
}

enum Fail {
    Refuse(String),
    Hard(GeometryError),
}

impl From<GeometryError> for Fail {
    fn from(e: GeometryError) -> Self {
        Self::Hard(e)
    }
}

type ER<T> = Result<T, Fail>;

impl Emitter {

    pub fn new(mode: &str, smooth_kind: &str, tpms_range_reduce: bool) -> GResult<Self> {
        if mode != "exact" && mode != "smooth" {
            return Err(terr(format!("mode {}; expected 'exact' or 'smooth'", str_repr(mode))));
        }
        if smooth_kind != "poly" && smooth_kind != "exp" {
            return Err(terr(format!("smooth_kind {}; expected 'poly' or 'exp'", str_repr(smooth_kind))));
        }
        Ok(Self {
            mode: mode.into(),
            smooth_kind: smooth_kind.into(),
            tpms_range_reduce,
            uniforms: HashMap::new(),
            uniform_order: Vec::new(),
            param_map: HashMap::new(),
            funcs: Vec::new(),
            seen: HashMap::new(),
            refusals: Vec::new(),
            samplers: BTreeMap::new(),
            n: 0,
            used_smooth_r: false,
        })
    }

    fn mode_enum(&self) -> Mode {
        if self.mode == "smooth" { Mode::Smooth } else { Mode::Exact }
    }

    fn fname(&mut self, node: &Node, path: &[String]) -> String {
        self.n += 1;
        let tail = path.last().map_or_else(|| "root".to_string(), |p| ident(p));
        format!("n{}_{}_{}", self.n, ident(node.kind()), tail)
    }

    fn uniform(&mut self, path: &[String], node: &Node, pname: &str) -> ER<String> {
        let key = (path.join("/"), pname.to_string());
        if let Some(hit) = self.param_map.get(&key) {
            return Ok(hit.clone());
        }
        let stem = if path.is_empty() {
            "root".to_string()
        } else {
            path.iter().map(|p| ident(p)).collect::<Vec<_>>().join("_")
        };
        let mut name = format!("u_{stem}_{}", ident(pname));
        if self.uniforms.contains_key(&name) {
            name = format!("{name}_{}", self.uniforms.len());
        }
        let value = node.param(pname).cloned().unwrap_or(ParamValue::Str(String::new()));
        let arr = value
            .as_ndarray()
            .map_err(|_| Fail::Hard(GeometryError::Value(format!("could not convert {pname} to float"))))?;
        if arr.ndim() != 0 {
            let shape =
                PyObj::List(arr.shape().iter().map(|n| PyObj::Int(i64::try_from(*n).unwrap_or(0))).collect());
            return Err(Fail::Refuse(format!(
                "parameter {} is an array of shape {}, not a scalar; a uniform cannot carry a control field",
                str_repr(pname),
                shape.repr()
            )));
        }
        let info = node.info();
        let units = info.param(pname).map(|s| s.units.clone()).unwrap_or_default();
        let discrete = info.discrete.iter().any(|d| d == pname);
        let v = arr.to_f64_vec().first().copied().unwrap_or(f64::NAN);
        self.uniforms.insert(
            name.clone(),
            Uniform {
                name: name.clone(),
                ty: "float".into(),
                value: v,
                path: path.join("/"),
                param: pname.into(),
                node: node.kind().into(),
                units,
                discrete,
            },
        );
        self.uniform_order.push(name.clone());
        self.param_map.insert(key, name.clone());
        Ok(name)
    }

    fn smooth_r(&mut self) -> String {
        if !self.used_smooth_r {
            self.uniforms.insert(
                "u_smooth_r_mm".into(),
                Uniform {
                    name: "u_smooth_r_mm".into(),
                    ty: "float".into(),
                    value: DEFAULT_SMOOTH_MM,
                    path: String::new(),
                    param: "smooth_r_mm".into(),
                    node: "<context>".into(),
                    units: "mm".into(),
                    discrete: false,
                },
            );
            self.uniform_order.push("u_smooth_r_mm".into());
            self.used_smooth_r = true;
        }
        "u_smooth_r_mm".into()
    }


    pub fn emit(&mut self, node: &NodeRef, path: &[String]) -> GResult<String> {
        if let Some(hit) = self.seen.get(&node_id(node)) {
            return Ok(hit.clone());
        }
        if let Some(reason) = node.op().glsl_refusal() {
            let name = self.fallback(node, path, &reason);
            self.seen.insert(node_id(node), name.clone());
            return Ok(name);
        }
        let mut kids = Vec::new();
        for (nm, c) in node.named_children() {
            let mut p = path.to_vec();
            p.push(nm.clone());
            kids.push(self.emit(c, &p)?);
        }
        let name = self.fname(node, path);
        let body = self.body(node, path, &kids);
        match body {
            Ok(body) => {
                let src = fn_source(&name, &body);
                self.funcs.push((name.clone(), src));
                self.seen.insert(node_id(node), name.clone());
                Ok(name)
            }
            Err(Fail::Refuse(why)) => {
                let name = self.fallback(node, path, &why);
                self.seen.insert(node_id(node), name.clone());
                Ok(name)
            }
            Err(Fail::Hard(e)) => Err(e),
        }
    }

    fn fallback(&mut self, node: &NodeRef, path: &[String], reason: &str) -> String {
        let stem = if path.is_empty() {
            "root".to_string()
        } else {
            path.iter().map(|p| ident(p)).collect::<Vec<_>>().join("_")
        };
        let mut tex = format!("u_tex_{stem}");
        if self.samplers.contains_key(&tex) {
            tex = format!("{tex}_{}", self.samplers.len());
        }
        let mut rec = Map::new();
        rec.insert("name".into(), json!(tex));
        rec.insert("path".into(), json!(path.join("/")));
        rec.insert("kind".into(), json!(node.kind()));
        rec.insert("reason".into(), json!(reason));
        rec.insert("fallback".into(), json!("texture3d"));
        self.samplers.insert(tex.clone(), rec.clone());
        self.refusals.push(rec);
        let name = self.fname(node, path);
        let body = TEXTURE_BODY.replace("{tex}", &tex);
        let src = fn_source(&name, &body);
        self.funcs.push((name.clone(), src));
        name
    }

    #[must_use]
    pub fn source(&self, root_fn: &str) -> String {
        let mut out = Vec::new();
        for tex in self.samplers.keys() {
            out.push(format!("uniform highp sampler3D {tex};"));
            out.push(format!("uniform vec3 {tex}_lo;"));
            out.push(format!("uniform vec3 {tex}_hi;"));
            out.push(format!("uniform vec3 {tex}_dim;"));
        }
        for n in &self.uniform_order {
            let u = &self.uniforms[n];
            out.push(format!("uniform {} {n};", u.ty));
        }
        out.push(PRELUDE.to_string());
        for (_n, src) in &self.funcs {
            out.push(src.clone());
        }
        out.push(format!("float sdf(vec3 p) {{ return {root_fn}(p); }}"));
        out.push(String::new());
        out.join("\n")
    }

    fn p(&mut self, n: &Node, path: &[String], name: &str) -> ER<String> {
        self.uniform(path, n, name)
    }

    fn ps<const K: usize>(&mut self, n: &Node, path: &[String], names: [&str; K]) -> ER<[String; K]> {
        let mut out: [String; K] = std::array::from_fn(|_| String::new());
        for (o, nm) in out.iter_mut().zip(names) {
            *o = self.p(n, path, nm)?;
        }
        Ok(out)
    }

    fn smin(&self, a: &str, b: &str, k: &str) -> String {
        format!("smin_{}({a}, {b}, {k})", self.smooth_kind)
    }

    fn smax(&self, a: &str, b: &str, k: &str) -> String {
        format!("smax_{}({a}, {b}, {k})", self.smooth_kind)
    }

    fn bool_k(&mut self, n: &Node, path: &[String]) -> ER<String> {
        let r = self.smooth_r();
        let k = self.p(n, path, "k_scale")?;
        Ok(format!("{r} * {k}"))
    }

    #[allow(clippy::too_many_lines)]
    fn body(&mut self, n: &Node, path: &[String], kids: &[String]) -> ER<String> {
        let kind = n.kind().to_string();
        let kid = |i: usize| kids.get(i).cloned().unwrap_or_default();
        let call = |i: usize| format!("{}(p)", kids.get(i).cloned().unwrap_or_default());
        Ok(match kind.as_str() {
            "sphere" => format!("return vlen3(p) - {};", self.p(n, path, "radius_mm")?),
            "box" => {
                let [bx, by, bz] = self.ps(n, path, ["bx_mm", "by_mm", "bz_mm"])?;
                format!(
                    "vec3 b = vec3({bx}, {by}, {bz});\nvec3 q = abs(p) - b;\nreturn vlen3(max(q, vec3(0.0)))\n     + min(max(q.x, max(q.y, q.z)), 0.0);"
                )
            }
            "rounded_box" => {
                let [bx, by, bz] = self.ps(n, path, ["bx_mm", "by_mm", "bz_mm"])?;
                let r = self.p(n, path, "radius_mm")?;
                format!(
                    "float r = {r};\nvec3 b = vec3({bx}, {by}, {bz}) - r;\nvec3 q = abs(p) - b;\nreturn vlen3(max(q, vec3(0.0)))\n     + min(max(q.x, max(q.y, q.z)), 0.0) - r;"
                )
            }
            "cylinder" => {
                let r = self.p(n, path, "radius_mm")?;
                let h = self.p(n, path, "half_height_mm")?;
                format!(
                    "float dx = vlen2(p.x, p.y) - {r};\nfloat dz = abs(p.z) - {h};\nfloat ox = max(dx, 0.0);\nfloat oz = max(dz, 0.0);\nfloat outside = sqrt(ox * ox + oz * oz + IMPLEXITY_SAFE_EPS);\nreturn outside + min(max(dx, dz), 0.0);"
                )
            }
            "capsule" => {
                let a = self.ps(n, path, ["ax_mm", "ay_mm", "az_mm"])?;
                let b = self.ps(n, path, ["bx_mm", "by_mm", "bz_mm"])?;
                let r = self.p(n, path, "radius_mm")?;
                format!(
                    "vec3 a  = vec3({}, {}, {});\nvec3 b  = vec3({}, {}, {});\nvec3 pa = p - a;\nvec3 ba = b - a;\nfloat h = clamp(dot(pa, ba) / (dot(ba, ba) + IMPLEXITY_SAFE_EPS),\n                0.0, 1.0);\nreturn vlen3(pa - ba * h) - {r};",
                    a[0], a[1], a[2], b[0], b[1], b[2]
                )
            }
            "torus" => {
                let big = self.p(n, path, "major_mm")?;
                let r = self.p(n, path, "minor_mm")?;
                format!("float q = vlen2(p.x, p.y) - {big};\nreturn vlen2(q, p.z) - {r};")
            }
            "plane" => {
                let [nx, ny, nz] = self.ps(n, path, ["nx", "ny", "nz"])?;
                let off = self.p(n, path, "offset_mm")?;
                format!(
                    "vec3 nrm = vec3({nx}, {ny}, {nz});\nfloat nn = sqrt(dot(nrm, nrm) + IMPLEXITY_SAFE_EPS);\nreturn dot(p, nrm) / nn - {off};"
                )
            }
            "cone" => {
                let [r1, r2, h] = self.ps(n, path, ["r1_mm", "r2_mm", "half_height_mm"])?;
                format!(
                    "float r1 = {r1}, r2 = {r2}, h = {h};\nfloat qx = vlen2(p.x, p.y);\nfloat qy = p.z;\nfloat cap_r = (qy < 0.0) ? r1 : r2;\nfloat cax = qx - min(qx, cap_r);\nfloat cay = abs(qy) - h;\nfloat k1x = r2, k1y = h;\nfloat k2x = r2 - r1, k2y = 2.0 * h;\nfloat dot2k2 = k2x * k2x + k2y * k2y + IMPLEXITY_SAFE_EPS;\nfloat t = clamp(((k1x - qx) * k2x + (k1y - qy) * k2y) / dot2k2,\n                0.0, 1.0);\nfloat cbx = qx - k1x + k2x * t;\nfloat cby = qy - k1y + k2y * t;\nfloat s = (cbx < 0.0 && cay < 0.0) ? -1.0 : 1.0;\nreturn s * sqrt(min(cax * cax + cay * cay,\n                    cbx * cbx + cby * cby) + IMPLEXITY_SAFE_EPS);"
                )
            }
            "constant" => format!(
                "return {} + 0.0 * p.x;",
                self.p(n, path, "value_mm")?
            ),
            "union" | "intersect" | "difference" => {
                let (a, b) = (call(0), call(1));
                if self.mode == "exact" {
                    match kind.as_str() {
                        "union" => format!("return min({a}, {b});"),
                        "intersect" => format!("return max({a}, {b});"),
                        _ => format!("return max({a}, -{b});"),
                    }
                } else {
                    let k = self.bool_k(n, path)?;
                    match kind.as_str() {
                        "union" => format!("return {};", self.smin(&a, &b, &k)),
                        "intersect" => format!("return {};", self.smax(&a, &b, &k)),
                        _ => format!("return {};", self.smax(&a, &format!("-{b}"), &k)),
                    }
                }
            }
            "fillet" => {
                let (a, b) = (call(0), call(1));
                let r = self.p(n, path, "radius_mm")?;
                let op = n.op().as_any().downcast_ref::<Blend>().map_or(BoolOp::Union, |bl| bl.op);
                let head = "";
                match op {
                    BoolOp::Union => format!("{head}return {};", self.smin(&a, &b, &r)),
                    BoolOp::Intersect => format!("{head}return {};", self.smax(&a, &b, &r)),
                    BoolOp::Difference => format!("{head}return {};", self.smax(&a, &format!("-{b}"), &r)),
                }
            }
            "chamfer" => {
                let (a, b) = (call(0), call(1));
                let r = self.p(n, path, "radius_mm")?;
                let op = n.op().as_any().downcast_ref::<Blend>().map_or(BoolOp::Union, |bl| bl.op);
                let head = format!(
                    "float a = {a}, b = {b}, r = {r};\n"
                );
                match op {
                    BoolOp::Union => {
                        format!("{head}return min(min(a, b), (a + b - r) * IMPLEXITY_ROOT_HALF);")
                    }
                    BoolOp::Intersect => {
                        format!("{head}return max(max(a, b), (a + b + r) * IMPLEXITY_ROOT_HALF);")
                    }
                    BoolOp::Difference => {
                        format!("{head}return max(max(a, -b), (a - b + r) * IMPLEXITY_ROOT_HALF);")
                    }
                }
            }
            "offset" => format!("return {}(p) - {};", kid(0), self.p(n, path, "distance_mm")?),
            "shell" => format!("return abs({}(p)) - {};", kid(0), self.p(n, path, "thickness_mm")?),
            "with_fields" => format!("return {}(p);", kid(0)),
            "negate" => format!("return -{}(p);", kid(0)),
            "remap.affine" => {
                let s = self.p(n, path, "scale")?;
                let sh = self.p(n, path, "shift_mm")?;
                format!("return {s} * {}(p) + {sh};", kid(0))
            }
            "remap.clamp" => {
                let lo = self.p(n, path, "lo_mm")?;
                let hi = self.p(n, path, "hi_mm")?;
                format!("return clamp({}(p), {lo}, {hi});", kid(0))
            }
            "remap.soft_clamp" => {
                let l = self.p(n, path, "limit_mm")?;
                format!("float L = {l};\nreturn L * tanh({}(p) / L);", kid(0))
            }
            "interpolate" => {
                let t = self.p(n, path, "t")?;
                format!("float t = {t};\nreturn (1.0 - t) * {}(p) + t * {}(p);", kid(0), kid(1))
            }
            "mask" => format!(
                "return ({}(p) < 0.0) ? {}(p) : {}(p);",
                kid(2),
                kid(0),
                kid(1)
            ),
            "translate" => {
                let d = self.ps(n, path, ["dx_mm", "dy_mm", "dz_mm"])?;
                format!("return {}(p - vec3({}, {}, {}));", kid(0), d[0], d[1], d[2])
            }
            "rotate" => {
                let r = self.ps(n, path, ["rx_deg", "ry_deg", "rz_deg"])?;
                format!(
                    "mat3 R = implexityRot({}, {}, {});\nreturn {}(p * R);",
                    r[0],
                    r[1],
                    r[2],
                    kid(0)
                )
            }
            "scale.uniform" => {
                let s = self.p(n, path, "scale")?;
                format!(
                    "float s = {s};\nreturn s * {}(p / s);",
                    kid(0)
                )
            }
            "scale.nonuniform" => {
                let s = self.ps(n, path, ["sx", "sy", "sz"])?;
                format!("return {}(p / vec3({}, {}, {}));", kid(0), s[0], s[1], s[2])
            }
            "scale.nonuniform_safe" => {
                let s = self.ps(n, path, ["sx", "sy", "sz"])?;
                format!(
                    "vec3 s = vec3({}, {}, {});\nreturn min(s.x, min(s.y, s.z)) * {}(p / s);",
                    s[0],
                    s[1],
                    s[2],
                    kid(0)
                )
            }
            "warp.twist" => {
                let rate = self.p(n, path, "rate_deg_per_mm")?;
                self.p(n, path, "extent_mm")?;
                format!(
                    "float k = {rate} * (IMPLEXITY_PI / 180.0);\nfloat th = -k * p.z;\nfloat c = cos(th), s = sin(th);\nreturn {}(vec3(p.x * c - p.y * s, p.x * s + p.y * c, p.z));",
                    kid(0)
                )
            }
            "warp.bend" => {
                let r = self.p(n, path, "radius_mm")?;
                self.p(n, path, "extent_mm")?;
                format!(
                    "float R = {r};\nfloat u = R - p.z;\nfloat v = p.x;\nfloat rho = sqrt(u * u + v * v + IMPLEXITY_SAFE_EPS);\nreturn {}(vec3(R * atan2s(v, u), p.y, R - rho));",
                    kid(0)
                )
            }
            "warp.taper" => {
                let a = self.p(n, path, "rate_per_mm")?;
                self.p(n, path, "extent_mm")?;
                format!("float s = 1.0 + {a} * p.z;\nreturn {}(vec3(p.x * s, p.y * s, p.z));", kid(0))
            }
            "array.linear" => self.array_linear(n, path, &kid(0))?,
            "array.polar" => self.array_polar(n, path, &kid(0))?,
            "tpms" => self.tpms(n, path)?,
            other => {
                let reason = refused_reason(other).map(str::to_string).or_else(|| n.op().glsl_refusal()).unwrap_or_else(|| {
                    format!(
                        "no closed form: {} is not one of the {} kinds this transpiler emits, and it is not in its REFUSED table either -- a node kind has been added to the kernel that this module has not been told about",
                        str_repr(other),
                        EMITTED.len()
                    )
                });
                return Err(Fail::Refuse(reason));
            }
        })
    }

    fn array_linear(&mut self, n: &Node, path: &[String], kid: &str) -> ER<String> {
        let al = n.op().as_any().downcast_ref::<ArrayLinear>();
        let axes: Vec<usize> = al.map_or_else(|| vec![0, 1], |a| a.axes.clone());
        let nb = al.map_or(1, |a| a.neighbours);
        let mut lines = Vec::new();
        let ac = ["x", "y", "z"];
        for &a in &axes {
            let c = ac[a];
            let pitch = self.p(n, path, ["pitch_x_mm", "pitch_y_mm", "pitch_z_mm"][a])?;
            let cnt = self.p(n, path, ["count_x", "count_y", "count_z"][a])?;
            lines.extend([
                format!("float pit{a} = {pitch}, cnt{a} = {cnt};"),
                format!("float hlf{a} = 0.5 * (cnt{a} - 1.0);"),
                format!("float t{a} = p.{c} / pit{a} + hlf{a};"),
                format!("float j{a} = clamp(roundEven(t{a}), 0.0, cnt{a} - 1.0);"),
                format!("float ln{a} = (t{a} - j{a} >= 0.0) ? 1.0 : -1.0;"),
                format!("float c{a}_0 = p.{c} - pit{a} * (j{a} - hlf{a});"),
            ]);
            if nb != 0 {
                lines.push(format!("float k{a} = clamp(j{a} + ln{a}, 0.0, cnt{a} - 1.0);"));
                lines.push(format!("float c{a}_1 = p.{c} - pit{a} * (k{a} - hlf{a});"));
            }
        }
        let mut deltas: Vec<Vec<usize>> = vec![Vec::new()];
        for _ in &axes {
            let opts: &[usize] = if nb != 0 { &[0, 1] } else { &[0] };
            deltas = deltas
                .iter()
                .flat_map(|d| opts.iter().map(move |s| [d.clone(), vec![*s]].concat()))
                .collect();
        }
        for (i, delta) in deltas.iter().enumerate() {
            let mut vals = ["p.x".to_string(), "p.y".to_string(), "p.z".to_string()];
            for (a, d) in axes.iter().zip(delta) {
                vals[*a] = format!("c{a}_{d}");
            }
            let call = format!("{kid}(vec3({}, {}, {}))", vals[0], vals[1], vals[2]);
            if i == 0 {
                lines.push(format!("float best = {call};"));
            } else {
                lines.push(format!("best = min(best, {call});"));
            }
        }
        lines.push("return best;".into());
        Ok(lines.join("\n"))
    }

    fn array_polar(&mut self, n: &Node, path: &[String], kid: &str) -> ER<String> {
        let cnt = self.p(n, path, "count")?;
        let nb = n.op().as_any().downcast_ref::<ArrayPolar>().map_or(1, |a| a.neighbours);
        let mut lines = vec![
            format!("float cnt = {cnt};"),
            "float sector = 2.0 * IMPLEXITY_PI / cnt;".to_string(),
            "float th = atan2s(p.y, p.x);".to_string(),
            "float r = vlen2(p.x, p.y);".to_string(),
            "float t = th / sector;".to_string(),
            "float j = roundEven(t);".to_string(),
            "float ln = (t - j >= 0.0) ? 1.0 : -1.0;".to_string(),
            "float t0 = th - j * sector;".to_string(),
            format!("float best = {kid}(vec3(r * cos(t0), r * sin(t0), p.z));"),
        ];
        if nb != 0 {
            lines.push("float t1 = th - (j + ln) * sector;".into());
            lines.push(format!("best = min(best, {kid}(vec3(r * cos(t1), r * sin(t1), p.z)));"));
        }
        lines.push("return best;".into());
        Ok(lines.join("\n"))
    }

    fn tpms(&mut self, n: &Node, path: &[String]) -> ER<String> {
        let t = n.op().as_any().downcast_ref::<Tpms>();
        let family = t.map_or(Family::Gyroid, |t| t.family);
        let proven = t.is_none_or(|t| t.proven);
        let grad_bound = t.map_or(1.0, Tpms::grad_bound);
        let period = self.p(n, path, "period_mm")?;
        let level = self.p(n, path, "level")?;
        let phase = if self.tpms_range_reduce {
            format!(
                "vec3 u = 6.28318530717958648 * fract(p / {period});"
            )
        } else {
            format!("float k0 = 2.0 * IMPLEXITY_PI / {period};\nvec3 u = p * k0;")
        };
        let mut lines = vec![
            phase,
            "float c1 = cos(u.x), c2 = cos(u.y), c3 = cos(u.z);".to_string(),
            "float s1 = sin(u.x), s2 = sin(u.y), s3 = sin(u.z);".to_string(),
        ];
        if matches!(family, Family::Iwp | Family::Neovius | Family::Frd | Family::Lidinoid) {
            lines.push("float C1 = cos(2.0*u.x), C2 = cos(2.0*u.y), C3 = cos(2.0*u.z);".into());
        }
        if family == Family::Lidinoid {
            lines.push("float S1 = sin(2.0*u.x), S2 = sin(2.0*u.y), S3 = sin(2.0*u.z);".into());
        }
        lines.push(
            match family {
                Family::Gyroid => "float g = s1*c2 + s2*c3 + s3*c1;",
                Family::SchwarzP => "float g = c1 + c2 + c3;",
                Family::SchwarzD => "float g = s1*s2*s3 + s1*c2*c3 + c1*s2*c3 + c1*c2*s3;",
                Family::Iwp => "float g = 2.0*(c1*c2 + c2*c3 + c3*c1) - (C1 + C2 + C3);",
                Family::Neovius => "float g = 3.0*(c1 + c2 + c3) + 4.0*c1*c2*c3;",
                Family::Frd => "float g = 4.0*c1*c2*c3 - (C1*C2 + C2*C3 + C3*C1);",
                Family::Lidinoid => "float g = 0.5*(S1*c2*s3 + S2*c3*s1 + S3*c1*s2)\n        - 0.5*(C1*C2 + C2*C3 + C3*C1) + 0.15;",
            }
            .to_string(),
        );
        lines.push(format!("g = g - {level};"));
        if proven {
            lines.push(format!("float k0n = 2.0 * IMPLEXITY_PI / {period};"));
            lines.push(format!("return g / (k0n * {});", float_literal(grad_bound)?));
        } else {
            lines.push("return g;".into());
        }
        Ok(lines.join("\n"))
    }
}

fn safe_class(node: &NodeRef, mode: Mode) -> FieldClass {
    field_class_of(node, mode).unwrap_or_else(|_| FieldClass::implicit())
}

#[must_use]
pub fn fc_text(fc: &FieldClass) -> String {
    if fc.kind() == ClassKind::Lipschitz {
        return format!("LIPSCHITZ(k={}{})", pyfmt::g(fc.k()), if fc.measured() { ", MEASURED" } else { "" });
    }
    fc.kind().name().to_string()
}

fn fn_source(name: &str, body: &str) -> String {
    format!("float {name}(vec3 p) {{\n{}\n}}\n", indent(body))
}


pub fn traced_field_class<H: std::hash::BuildHasher>(
    node: &NodeRef,
    mode: Mode,
    overrides: &HashMap<usize, FieldClass, H>,
) -> GResult<FieldClass> {
    field_class_with_overrides(node, mode, overrides)
}

#[must_use]
pub fn step_factor_of(fc: &FieldClass) -> (Option<f64>, &'static str, String) {
    let Some(fac) = fc.safe_step_factor() else {
        return (
            None,
            "fixed",
            "the graph is IMPLICIT: no step length is safe, so sphere tracing is unsound and the viewport marches at a fixed fraction of the scene scale instead".into(),
        );
    };
    if fc.measured() && fac > 1.0 {
        return (
            Some(1.0),
            "sphere",
            format!(
                "the Lipschitz constant {} was MEASURED over {} samples and is below 1; a measurement may not buy a step longer than |f|, so the factor is capped at 1",
                pyfmt::g(fc.k()),
                fc.samples()
            ),
        );
    }
    if fc.measured() {
        return (
            Some(fac),
            "sphere",
            format!(
                "1/k for a MEASURED k = {} over {} samples -- evidence, not a proof; fieldclass.require would refuse it where a guarantee was asked for",
                pyfmt::g(fc.k()),
                fc.samples()
            ),
        );
    }
    if fc.kind() == ClassKind::Lipschitz {
        return (Some(fac), "sphere", format!("1/k for a PROVEN k = {}", pyfmt::g(fc.k())));
    }
    (Some(fac), "sphere", format!("{}: a full step of |f| cannot overshoot", fc.kind().name()))
}

#[must_use]
pub fn frame_hint(node: &NodeRef) -> Option<Aabb> {
    let mut out: Option<Aabb> = None;
    for (_p, n) in node.walk() {
        let Some((lo, hi)) = aabb_of(&n) else { continue };
        out = Some(match out {
            None => (lo, hi),
            Some((l, h)) => {
                (std::array::from_fn(|a| l[a].min(lo[a])), std::array::from_fn(|a| h[a].max(hi[a])))
            }
        });
    }
    out
}

#[must_use]
pub fn grid_shape(lo: [f64; 3], hi: [f64; 3], res: usize, cap: usize) -> [usize; 3] {
    let ext: [f64; 3] = std::array::from_fn(|a| hi[a] - lo[a]);
    let span = ext.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(1e-9);
    #[allow(clippy::cast_precision_loss)]
    let mut n: [f64; 3] = ext.map(|e| (e / span * res as f64).round_ties_even().clamp(2.0, 512.0));
    #[allow(clippy::cast_precision_loss)]
    while n[0] * n[1] * n[2] > cap as f64 {
        n = n.map(|v| (v * 0.8).floor().max(2.0));
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    n.map(|v| v as usize)
}


pub fn sample_subtree(
    node: &NodeRef,
    lo: [f64; 3],
    hi: [f64; 3],
    shape: [usize; 3],
    opts: &EvalOptions,
) -> GResult<Vec<f32>> {
    let ax: [Vec<f64>; 3] = std::array::from_fn(|a| crate::numpy::linspace(lo[a], hi[a], shape[a]));
    let [nx, ny, nz] = shape;
    let mut pts = Vec::with_capacity(nx * ny * nz);
    for x in &ax[0] {
        for y in &ax[1] {
            for z in &ax[2] {
                pts.push([*x, *y, *z]);
            }
        }
    }
    let mut out = vec![0f32; pts.len()];
    for (chunk_i, chunk) in pts.chunks(65536).enumerate() {
        let v = eval_points(node, chunk, opts)?;
        for (j, val) in v.into_iter().enumerate() {
            let c = chunk_i * 65536 + j;
            let (i, jj, k) = (c / (ny * nz), (c / nz) % ny, c % nz);
            #[allow(clippy::cast_possible_truncation)]
            {
                out[(k * ny + jj) * nx + i] = val as f32;
            }
        }
    }
    Ok(out)
}

#[must_use]
pub fn trilerp(data: &[f32], shape: [usize; 3], lo: [f64; 3], hi: [f64; 3], pts: &[[f64; 3]]) -> Vec<f64> {
    let [nx, ny, _nz] = shape;
    pts.iter()
        .map(|p| {
            #[allow(clippy::cast_precision_loss)]
            let g: [f64; 3] = std::array::from_fn(|a| {
                ((p[a] - lo[a]) / (hi[a] - lo[a]).max(1e-30)).clamp(0.0, 1.0) * (shape[a] as f64 - 1.0)
            });
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let i0: [usize; 3] =
                std::array::from_fn(|a| (g[a].floor() as i64).min(shape[a] as i64 - 2).max(0) as usize);
            #[allow(clippy::cast_precision_loss)]
            let f: [f64; 3] = std::array::from_fn(|a| g[a] - i0[a] as f64);
            let mut out = 0.0;
            for dx in 0..2 {
                for dy in 0..2 {
                    for dz in 0..2 {
                        let w = (if dx == 1 { f[0] } else { 1.0 - f[0] })
                            * (if dy == 1 { f[1] } else { 1.0 - f[1] })
                            * (if dz == 1 { f[2] } else { 1.0 - f[2] });
                        let v = data[((i0[2] + dz) * ny + i0[1] + dy) * nx + i0[0] + dx];
                        out += w * f64::from(v);
                    }
                }
            }
            out
        })
        .collect()
}

#[must_use]
pub fn interpolant_lipschitz(data: &[f32], shape: [usize; 3], lo: [f64; 3], hi: [f64; 3]) -> f64 {
    let [nx, ny, nz] = shape;
    #[allow(clippy::cast_precision_loss)]
    let h: [f64; 3] = std::array::from_fn(|a| (hi[a] - lo[a]) / (shape[a] as f64 - 1.0));
    let d = |i: usize, j: usize, k: usize| f64::from(data[(k * ny + j) * nx + i]);
    let mut best = 0.0f64;
    if nx < 2 || ny < 2 || nz < 2 {
        return best;
    }
    for i in 0..nx - 1 {
        for j in 0..ny - 1 {
            for k in 0..nz - 1 {
                for a in 0..2 {
                    for b in 0..2 {
                        for c in 0..2 {
                            let x = (d(i + 1, j + b, k + c) - d(i, j + b, k + c)).abs() / h[0];
                            let y = (d(i + a, j + 1, k + c) - d(i + a, j, k + c)).abs() / h[1];
                            let z = (d(i + a, j + b, k + 1) - d(i + a, j + b, k)).abs() / h[2];
                            best = best.max((x * x + y * y + z * z).sqrt());
                        }
                    }
                }
            }
        }
    }
    best
}


#[allow(clippy::too_many_arguments)]
pub fn measure_texture(
    node: &NodeRef,
    data: &[f32],
    shape: [usize; 3],
    lo: [f64; 3],
    hi: [f64; 3],
    samples: usize,
    seed: u64,
    mode: Mode,
) -> GResult<(Value, FieldClass)> {
    let mut rng = implexity_core::rng::default_rng(u128::from(seed));
    let u = rng.random_vec(3 * samples);
    let pts: Vec<[f64; 3]> =
        (0..samples).map(|i| std::array::from_fn(|a| lo[a] + u[3 * i + a] * (hi[a] - lo[a]))).collect();
    let fi = trilerp(data, shape, lo, hi, &pts);
    let opts = EvalOptions { mode, ..EvalOptions::default() };
    let ft = eval_points(node, &pts, &opts)?;
    let err: Vec<f64> = fi.iter().zip(&ft).map(|(a, b)| a - b).collect();
    #[allow(clippy::cast_precision_loss)]
    let h = (0..3).map(|a| (hi[a] - lo[a]) / (shape[a] as f64 - 1.0)).fold(f64::INFINITY, f64::min);
    let e = 0.5 * h;
    let mut g = vec![[0.0; 3]; samples];
    for a in 0..3 {
        let plus: Vec<[f64; 3]> =
            pts.iter().map(|p| std::array::from_fn(|b| if a == b { p[b] + e } else { p[b] })).collect();
        let minus: Vec<[f64; 3]> =
            pts.iter().map(|p| std::array::from_fn(|b| if a == b { p[b] - e } else { p[b] })).collect();
        let (fp, fm) = (trilerp(data, shape, lo, hi, &plus), trilerp(data, shape, lo, hi, &minus));
        for i in 0..samples {
            g[i][a] = (fp[i] - fm[i]) / (2.0 * e);
        }
    }
    let gn: Vec<f64> = g.iter().map(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()).collect();
    let near: Vec<usize> = (0..samples).filter(|i| ft[*i].abs() <= 2.0 * h).collect();
    let kmax = gn.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let kproven = interpolant_lipschitz(data, shape, lo, hi);
    let fc = FieldClass::lipschitz(
        kproven.max(1e-9),
        &format!(
            "PROVEN sup |grad| of the trilinear interpolant of {} on a {}x{}x{} grid; see glsl.interpolant_lipschitz",
            node.kind(),
            shape[0],
            shape[1],
            shape[2]
        ),
    )?;
    let abs_max = err.iter().fold(f64::NEG_INFINITY, |m, v| m.max(v.abs()));
    let sq: Vec<f64> = err.iter().map(|v| v * v).collect();
    let near_max = if near.is_empty() {
        Value::Null
    } else {
        json!(near.iter().fold(f64::NEG_INFINITY, |m, i| m.max(err[*i].abs())))
    };
    let rec = json!({
        "samples": samples, "seed": seed, "cell_mm": h,
        "against": "implexity.implicit.eval.eval_points on the same coordinates",
        "error": {
            "max_abs_mm": abs_max,
            "rms_mm": crate::numpy::mean(&sq).sqrt(),
            "mean_mm": crate::numpy::mean(&err),
            "max_over_report_mm": err.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            "max_under_report_mm": err.iter().copied().fold(f64::INFINITY, f64::min),
            "max_abs_frac_of_cell": abs_max / h,
            "near_surface_max_abs_mm": near_max,
            "near_surface_points": near.len(),
        },
        "gradient": {
            "sampled_max": kmax, "mean": crate::numpy::mean(&gn),
            "p99": crate::numpy::percentile(&gn, 99.0).unwrap_or(f64::NAN),
            "min": gn.iter().copied().fold(f64::INFINITY, f64::min), "eps_mm": e,
            "proven_sup": kproven,
            "note": "`sampled_max` is central differences at half a cell over the sample points; `proven_sup` is the EXACT supremum over the whole interpolant, from glsl.interpolant_lipschitz, and is what the class is built from.  The gap between them is how much a sampled maximum was missing.",
        },
        "field_class": fc.as_json(),
    });
    Ok((rec, fc))
}

pub struct GlslStats {
    pub compiles: AtomicU64,
    pub hits: AtomicU64,
}

pub static STATS: GlslStats = GlslStats { compiles: AtomicU64::new(0), hits: AtomicU64::new(0) };

impl GlslStats {
    pub fn reset(&self) {
        self.compiles.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
    }

    #[must_use]
    pub fn as_json(&self) -> Value {
        json!({"compiles": self.compiles.load(Ordering::Relaxed), "hits": self.hits.load(Ordering::Relaxed)})
    }
}

#[derive(Clone)]
struct CacheEntry {
    src: String,
    uniforms: HashMap<String, Uniform>,
    uniform_order: Vec<String>,
    refusals: Vec<Map<String, Value>>,
}

type SourceKey = (String, String, String, bool);
type TexKey = (String, [usize; 3], String, String, Option<u64>, usize, String);

#[derive(Clone)]
struct TexEntry {
    data: std::sync::Arc<Vec<f32>>,
    meas: Value,
    fc: FieldClass,
    interp_fc: FieldClass,
    own_fc: FieldClass,
}

fn source_cache() -> &'static Mutex<Vec<(SourceKey, CacheEntry)>> {
    static C: OnceLock<Mutex<Vec<(SourceKey, CacheEntry)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

fn tex_cache() -> &'static Mutex<Vec<(TexKey, TexEntry)>> {
    static C: OnceLock<Mutex<Vec<(TexKey, TexEntry)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn cache_clear() {
    if let Ok(mut c) = source_cache().lock() {
        c.clear();
    }
    if let Ok(mut c) = tex_cache().lock() {
        c.clear();
    }
}

#[must_use]
pub fn cache_size() -> usize {
    source_cache().lock().map_or(0, |c| c.len())
}

#[derive(Clone, Debug)]
pub struct TranspileOptions {
    pub mode: String,
    pub smooth_kind: String,
    pub textures: bool,
    pub texture_res: usize,
    pub measure_samples: usize,
    pub tpms_range_reduce: bool,
    pub bbox: Option<Aabb>,
    pub smooth_r_mm: Option<f64>,
    pub use_cache: bool,
    pub texture_class: String,
}

impl Default for TranspileOptions {
    fn default() -> Self {
        Self {
            mode: "exact".into(),
            smooth_kind: "poly".into(),
            textures: true,
            texture_res: texture_res(),
            measure_samples: TEXTURE_MEASURE_SAMPLES,
            tpms_range_reduce: true,
            bbox: None,
            smooth_r_mm: None,
            use_cache: true,
            texture_class: "proven".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShaderResult {
    pub payload: Map<String, Value>,
    pub textures_data: BTreeMap<String, std::sync::Arc<Vec<f32>>>,
}

fn uniforms_json(uniforms: &HashMap<String, Uniform>, values: &BTreeMap<String, f64>) -> Value {
    let mut m = Map::new();
    for (k, u) in uniforms {
        let mut v = u.to_json();
        if let Some(x) = values.get(k) {
            v["value"] = json!(x);
        }
        m.insert(k.clone(), v);
    }
    Value::Object(m)
}

fn box_json(lo: [f64; 3], hi: [f64; 3]) -> Value {
    json!([lo, hi])
}


#[allow(clippy::too_many_lines)]
pub fn transpile(node: &NodeRef, o: &TranspileOptions) -> GResult<ShaderResult> {
    let key: SourceKey = (node.structure_id(), o.mode.clone(), o.smooth_kind.clone(), o.tpms_range_reduce);
    let hit = if o.use_cache {
        source_cache().lock().ok().and_then(|c| c.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()))
    } else {
        None
    };
    let cached = hit.is_some();
    let entry = if let Some(h) = hit {
        STATS.hits.fetch_add(1, Ordering::Relaxed);
        h
    } else {
        STATS.compiles.fetch_add(1, Ordering::Relaxed);
        let mut em = Emitter::new(&o.mode, &o.smooth_kind, o.tpms_range_reduce)?;
        let root_fn = em.emit(node, &[])?;
        let src = em.source(&root_fn);
        let e =
            CacheEntry { src, uniforms: em.uniforms, uniform_order: em.uniform_order, refusals: em.refusals };
        if o.use_cache
            && let Ok(mut c) = source_cache().lock()
        {
            if c.len() >= CACHE_MAX {
                c.remove(0);
            }
            c.push((key, e.clone()));
        }
        e
    };
    let mode = if o.mode == "smooth" { Mode::Smooth } else { Mode::Exact };
    let bounds_ctx = crate::node::EvalCtx { mode, smooth_kind: if o.smooth_kind == "exp" { SmoothKind::Exp } else { SmoothKind::Poly }, smooth_r: o.smooth_r_mm.unwrap_or(DEFAULT_SMOOTH_MM) };
    let root_bounds = crate::profiles::preview_bounds(node, &bounds_ctx)?;
    let mut refusals = entry.refusals.clone();
    let mut tex_out: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut tex_data: BTreeMap<String, std::sync::Arc<Vec<f32>>> = BTreeMap::new();
    let mut overrides: HashMap<usize, FieldClass> = HashMap::new();
    let by_path: HashMap<String, NodeRef> = node.walk().into_iter().map(|(p, n)| (p.join("/"), n)).collect();
    for rec in &mut refusals {
        let path = rec["path"].as_str().unwrap_or_default().to_string();
        let sub = by_path.get(&path).cloned();
        let mut bx = match sub.as_ref() { Some(s) => crate::profiles::preview_bounds(s, &bounds_ctx)?, None => None };
        if bx.is_none() {
            bx = o.bbox;
        }
        let (Some(sub), Some((lo, hi))) = (sub.clone(), bx) else {
            rec.insert("fallback".into(), json!("none"));
            let reason = format!(
                "{}; and it has no conservative bounding box, so there is nowhere to sample it either -- this subtree cannot be traced at all",
                rec["reason"].as_str().unwrap_or_default()
            );
            rec.insert("reason".into(), json!(reason));
            if let Some(s) = sub {
                overrides.insert(node_id(&s), FieldClass::implicit());
            }
            continue;
        };
        let shape = grid_shape(lo, hi, o.texture_res, texture_max_samples());
        rec.insert("shape".into(), json!(shape));
        rec.insert("bbox".into(), box_json(lo, hi));
        rec.insert("samples".into(), json!(shape[0] * shape[1] * shape[2]));
        let name = rec["name"].as_str().unwrap_or_default().to_string();
        if !o.textures {
            rec.insert("fallback".into(), json!("declared"));
            overrides.insert(node_id(&sub), FieldClass::implicit());
            tex_out.insert(name, rec.clone());
            continue;
        }
        let tkey: TexKey = (
            sub.content_id(),
            shape,
            o.mode.clone(),
            o.smooth_kind.clone(),
            o.smooth_r_mm.map(f64::to_bits),
            o.measure_samples,
            o.texture_class.clone(),
        );
        let got =
            tex_cache().lock().ok().and_then(|c| c.iter().find(|(k, _)| *k == tkey).map(|(_, v)| v.clone()));
        let (te, reused) = if let Some(t) = got {
            (t, true)
        } else {
            let smooth_kind = if o.smooth_kind == "exp" { SmoothKind::Exp } else { SmoothKind::Poly };
            let eo = EvalOptions { mode, smooth_kind, smooth_r_mm: o.smooth_r_mm, validate: true };
            let data = sample_subtree(&sub, lo, hi, shape, &eo)?;
            let (meas, fc) =
                measure_texture(&sub, &data, shape, lo, hi, o.measure_samples, 20_250_828, mode)?;
            let interp_fc = fc.clone();
            let own_fc = safe_class(&sub, mode);
            let fc = match o.texture_class.as_str() {
                "proven" => fc,
                "measured" => FieldClass::from_measurement(
                    meas["gradient"]["sampled_max"].as_f64().unwrap_or(0.0).max(1e-9),
                    i64::try_from(o.measure_samples).unwrap_or(i64::MAX),
                    "sampled |grad| of the interpolant",
                )?,
                "conservative" => fc.weaker_of(&own_fc),
                other => {
                    return Err(terr(format!(
                        "texture_class {}; expected 'proven', 'measured' or 'conservative'",
                        str_repr(other)
                    )));
                }
            };
            let te = TexEntry { data: std::sync::Arc::new(data), meas, fc, interp_fc, own_fc };
            if let Ok(mut c) = tex_cache().lock() {
                if c.len() >= TEX_CACHE_MAX {
                    c.remove(0);
                }
                c.push((tkey, te.clone()));
            }
            (te, false)
        };
        overrides.insert(node_id(&sub), te.fc.clone());
        rec.insert("measurement".into(), te.meas.clone());
        rec.insert("field_class".into(), te.fc.as_json());
        rec.insert("interpolant_class".into(), te.interp_fc.as_json());
        rec.insert("node_class".into(), te.own_fc.as_json());
        rec.insert("texture_class".into(), json!(o.texture_class));
        rec.insert("texture_reused".into(), json!(reused));
        tex_data.insert(name.clone(), std::sync::Arc::clone(&te.data));
        tex_out.insert(name, rec.clone());
    }
    let fc = traced_field_class(node, mode, &overrides)?;
    let (factor, trace_mode, why) = step_factor_of(&fc);
    let mut bx = root_bounds;
    if bx.is_none() {
        for rec in tex_out.values() {
            if let Some(b) = rec.get("bbox").and_then(Value::as_array) {
                let p = |v: &Value| -> [f64; 3] { std::array::from_fn(|a| v[a].as_f64().unwrap_or(0.0)) };
                bx = Some((p(&b[0]), p(&b[1])));
                break;
            }
        }
    }
    if bx.is_none() {
        bx = o.bbox;
    }
    let known = bx.is_some();
    let hint = frame_hint(node);
    let (lo, hi) = bx.or(hint).unwrap_or(([-20.0; 3], [20.0; 3]));
    let diag = {
        let d = crate::numpy::norm(&[hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]);
        if d == 0.0 { 1.0 } else { d }
    };
    let src = &entry.src;
    let walk = node.walk();
    let transpiled: BTreeSet<String> =
        walk.iter().map(|(_, n)| n.kind().to_string()).filter(|k| EMITTED.contains(&k.as_str())).collect();
    let refused: BTreeSet<String> =
        refusals.iter().filter_map(|r| r["kind"].as_str().map(str::to_string)).collect();
    let mut out = Map::new();
    out.insert("kind".into(), json!("implicit_shader"));
    out.insert("units".into(), json!("mm"));
    out.insert("glsl_version".into(), json!(GLSL_VERSION));
    out.insert("structure_id".into(), json!(node.structure_id()));
    out.insert("content_id".into(), json!(node.content_id()));
    out.insert("node_kind".into(), json!(node.kind()));
    out.insert("mode".into(), json!(o.mode));
    out.insert("smooth_kind".into(), json!(o.smooth_kind));
    out.insert("tpms_range_reduce".into(), json!(o.tpms_range_reduce));
    out.insert("texture_class".into(), json!(o.texture_class));
    out.insert("glsl".into(), json!(src));
    out.insert("entry_point".into(), json!("sdf"));
    out.insert("uniform_order".into(), json!(entry.uniform_order));
    out.insert("field_class".into(), fc.as_json());
    out.insert("field_class_text".into(), json!(fc_text(&fc)));
    out.insert("step_factor".into(), factor.map_or(Value::Null, |f| json!(f)));
    out.insert("trace_mode".into(), json!(trace_mode));
    out.insert("step_note".into(), json!(why));
    out.insert("refusals".into(), Value::Array(refusals.iter().cloned().map(Value::Object).collect()));
    out.insert("transpiled_kinds".into(), json!(transpiled));
    out.insert("refused_kinds".into(), json!(refused));
    out.insert(
        "textures".into(),
        Value::Object(tex_out.into_iter().map(|(k, v)| (k, Value::Object(v))).collect()),
    );
    out.insert("bbox".into(), box_json(lo, hi));
    out.insert("bbox_known".into(), json!(known));
    out.insert(
        "bbox_source".into(),
        json!(if known {
            "aabb"
        } else if hint.is_some() {
            "frame_hint"
        } else {
            "default"
        }),
    );
    out.insert("scale_mm".into(), json!(diag));
    out.insert("eps_mm".into(), json!(diag * 1.0e-4));
    out.insert("tmax_mm".into(), json!(diag * 4.0));
    out.insert("program_cached".into(), json!(cached));
    out.insert("source_lines".into(), json!(src.matches('\n').count() + 1));
    out.insert("functions".into(), json!(src.matches("\nfloat n").count()));
    let values = values_for_uniforms(&node.structure_id(), &entry.uniforms, node, o.smooth_r_mm)?;
    out.insert("uniforms".into(), uniforms_json(&entry.uniforms, &values));
    Ok(ShaderResult { payload: out, textures_data: tex_data })
}

fn values_for_uniforms(
    structure_id: &str,
    uniforms: &HashMap<String, Uniform>,
    node: &NodeRef,
    smooth_r_mm: Option<f64>,
) -> GResult<BTreeMap<String, f64>> {
    if structure_id != node.structure_id() {
        return Err(terr(format!(
            "these uniform names were emitted for structure {structure_id} and this graph is {}; a parameter edit cannot change structure_id, so this is a different model and needs a new shader",
            node.structure_id()
        )));
    }
    let mut want: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (name, u) in uniforms {
        want.entry((u.path.clone(), u.param.clone())).or_default().push(name.clone());
    }
    let mut out = BTreeMap::new();
    for (path, n) in node.walk() {
        let key = path.join("/");
        for (pname, val) in n.params() {
            if let Some(names) = want.get(&(key.clone(), pname.clone()))
                && let Ok(arr) = val.as_ndarray()
                && arr.ndim() == 0
            {
                for name in names {
                    out.insert(name.clone(), arr.to_f64_vec()[0]);
                }
            }
        }
    }
    if uniforms.contains_key("u_smooth_r_mm") {
        out.insert("u_smooth_r_mm".into(), smooth_r_mm.unwrap_or(DEFAULT_SMOOTH_MM));
    }
    Ok(out)
}


pub fn values_for(
    payload: &Map<String, Value>,
    node: &NodeRef,
    smooth_r_mm: Option<f64>,
) -> GResult<BTreeMap<String, f64>> {
    let sid = payload.get("structure_id").and_then(Value::as_str).unwrap_or_default();
    let mut uniforms = HashMap::new();
    if let Some(m) = payload.get("uniforms").and_then(Value::as_object) {
        for (k, v) in m {
            uniforms.insert(
                k.clone(),
                Uniform {
                    name: k.clone(),
                    ty: v["type"].as_str().unwrap_or("float").into(),
                    value: v["value"].as_f64().unwrap_or(f64::NAN),
                    path: v["path"].as_str().unwrap_or_default().into(),
                    param: v["param"].as_str().unwrap_or_default().into(),
                    node: v["node"].as_str().unwrap_or_default().into(),
                    units: v["units"].as_str().unwrap_or_default().into(),
                    discrete: v["discrete"].as_bool().unwrap_or(false),
                },
            );
        }
    }
    values_for_uniforms(sid, &uniforms, node, smooth_r_mm)
}

#[must_use]
pub fn catalogue(registry: &Registry) -> Vec<Value> {
    registry
        .names()
        .into_iter()
        .filter_map(|kind| registry.get(&kind).map(|e| (kind, e.info.glsl_refusal.clone())))
        .map(|(kind, own_refusal)| {
            if EMITTED.contains(&kind.as_str()) {
                json!({"kind": kind, "status": "transpiled"})
            } else if let Some(reason) = refused_reason(&kind).map(str::to_string).or(own_refusal) {
                json!({"kind": kind, "status": "refused", "reason": reason, "fallback": "texture3d"})
            } else {
                json!({"kind": kind, "status": "unknown",
                    "reason": "this kind is registered in the kernel and is in neither this module's EMITTERS nor its REFUSED table; it will be refused by name at transpile time"})
            }
        })
        .collect()
}

#[must_use]
pub fn encode(result: &ShaderResult, include_textures: bool, max_texture_bytes: usize) -> Value {
    let mut out = result.payload.clone();
    let mut tex = Map::new();
    let mut total = 0usize;
    if let Some(Value::Object(textures)) = result.payload.get("textures") {
        for (name, rec) in textures {
            let mut rec = rec.as_object().cloned().unwrap_or_default();
            let Some(arr) = result.textures_data.get(name).filter(|_| include_textures) else {
                rec.insert("encoding".into(), json!("omitted"));
                tex.insert(name.clone(), Value::Object(rec));
                continue;
            };
            let raw: Vec<u8> = arr.iter().flat_map(|v| v.to_le_bytes()).collect();
            total += raw.len();
            if total > max_texture_bytes {
                rec.insert("encoding".into(), json!("omitted"));
                rec.insert(
                    "omitted_because".into(),
                    json!(format!("{total} bytes of texture would exceed the {max_texture_bytes}-byte reply budget; ask for a coarser texture_res")),
                );
                tex.insert(name.clone(), Value::Object(rec));
                continue;
            }
            rec.insert("encoding".into(), json!("base64:float32:x-fastest"));
            rec.insert("bytes".into(), json!(raw.len()));
            rec.insert("data".into(), json!(base64::engine::general_purpose::STANDARD.encode(&raw)));
            tex.insert(name.clone(), Value::Object(rec));
        }
    }
    out.insert("textures".into(), Value::Object(tex));
    out.insert("texture_bytes".into(), json!(total));
    Value::Object(out)
}
