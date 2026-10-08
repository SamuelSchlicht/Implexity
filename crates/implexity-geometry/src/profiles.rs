// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::any::Any;
use std::sync::Arc;

use crate::error::{GResult, GeometryError, model_err, value_err};
use crate::fieldclass::{ClassKind, FieldClass};
use crate::kinds::{StaticInfo, entry, kind_info};
use crate::node::{
    Attr, ConstructArgs, EvalCtx, Kernel, KernelBox, KernelEntryList, KernelInputs, KindInfo, Node,
    NodeOp, ParamSpec,
};
use crate::scalar::Scalar;
use crate::value::ParamValue;

const MODULE: &str = "implexity.implicit.profiles";
const MAX_VERTICES: usize = 256;
const MAX_SECTIONS: usize = 32;
type Bounds2 = ([f64; 2], [f64; 2]);

fn array_default(values: &[f64]) -> ParamValue {
    ParamValue::List(values.iter().map(|x| ParamValue::Float(*x)).collect())
}

fn vertices_default() -> ParamValue {
    ParamValue::List(vec![
        array_default(&[-1.0, -1.0]),
        array_default(&[1.0, -1.0]),
        array_default(&[1.0, 1.0]),
        array_default(&[-1.0, 1.0]),
    ])
}

fn numeric_array(n: &Node, name: &str) -> GResult<(Vec<usize>, Vec<f64>)> {
    let a = n
        .param(name)
        .ok_or_else(|| GeometryError::Value(format!("{name} is required")))?
        .as_ndarray()
        .map_err(|_| GeometryError::Value(format!("{name} must be a numeric array")))?;
    let data = a.to_f64_vec();
    if !data.iter().all(|x| x.is_finite()) {
        return value_err(format!("{name} must contain finite numbers"));
    }
    Ok((a.shape().to_vec(), data))
}

fn positive(n: &Node, name: &str) -> GResult<f64> {
    let v = n.pf(name)?;
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        value_err(format!("{name} must be finite and positive"))
    }
}

fn finite(n: &Node, name: &str) -> GResult<f64> {
    let v = n.pf(name)?;
    if v.is_finite() {
        Ok(v)
    } else {
        value_err(format!("{name} must be finite"))
    }
}

fn norm2<S: Scalar>(x: S, y: S) -> S {
    let q = x * x + y * y;
    if q.val() == 0.0 {
        S::cst(0.0)
    } else {
        q.sqrt()
    }
}

fn prism<S: Scalar>(a: S, b: S) -> S {
    if a.val() <= 0.0 && b.val() <= 0.0 {
        a.max(b)
    } else {
        norm2(a.max_c(0.0), b.max_c(0.0))
    }
}

fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn intersects(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2], eps: f64) -> bool {
    let o = [
        cross(a, b, c),
        cross(a, b, d),
        cross(c, d, a),
        cross(c, d, b),
    ];
    let opposite = |u: f64, v: f64| (u > eps && v < -eps) || (u < -eps && v > eps);
    if opposite(o[0], o[1]) && opposite(o[2], o[3]) {
        return true;
    }
    let on = |p: [f64; 2], q: [f64; 2], r: [f64; 2], v: f64| {
        v.abs() <= eps
            && r[0] >= p[0].min(q[0]) - eps
            && r[0] <= p[0].max(q[0]) + eps
            && r[1] >= p[1].min(q[1]) - eps
            && r[1] <= p[1].max(q[1]) + eps
    };
    on(a, b, c, o[0]) || on(a, b, d, o[1]) || on(c, d, a, o[2]) || on(c, d, b, o[3])
}

fn polygon_check(v: &[[f64; 2]]) -> GResult<f64> {
    if !(3..=MAX_VERTICES).contains(&v.len()) {
        return value_err(format!("a profile needs 3 to {MAX_VERTICES} vertices"));
    }
    let mut area = 0.0;
    let (lo, hi) = polygon_bounds(v);
    let scale = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0);
    let eps = 1e-12 * scale * scale;
    for i in 0..v.len() {
        let a = v[i];
        let b = v[(i + 1) % v.len()];
        let q = (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2);
        if q <= 1e-24 * scale * scale {
            return value_err("profile edges must have nonzero length");
        }
        area += cross(v[0], a, b);
        for j in i + 1..v.len() {
            if j == i + 1 || (i == 0 && j + 1 == v.len()) {
                continue;
            }
            if intersects(a, b, v[j], v[(j + 1) % v.len()], eps) {
                return value_err("profile edges must not cross or touch non-adjacent edges");
            }
        }
    }
    if area.abs() <= eps {
        return value_err("profile area must be nonzero");
    }
    Ok(area.signum())
}

fn polygon_bounds(v: &[[f64; 2]]) -> Bounds2 {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in v {
        for a in 0..2 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    (lo, hi)
}

fn polygon_value<S: Scalar>(v: &[[S; 2]], q: [S; 2], axis_closed: bool) -> S {
    let mut inside = false;
    let mut area = 0.0;
    let mut best: Option<S> = None;
    let mut edge = 0usize;
    for i in 0..v.len() {
        let a = v[i];
        let b = v[(i + 1) % v.len()];
        area += cross(
            [v[0][0].val(), v[0][1].val()],
            [a[0].val(), a[1].val()],
            [b[0].val(), b[1].val()],
        );
        if (a[1].val() > q[1].val()) != (b[1].val() > q[1].val()) {
            let crossing = a[0].val()
                + (q[1].val() - a[1].val()) * (b[0].val() - a[0].val()) / (b[1].val() - a[1].val());
            if q[0].val() < crossing {
                inside = !inside;
            }
        }
        if axis_closed && a[0].val() == 0.0 && b[0].val() == 0.0 {
            continue;
        }
        let ab = [b[0] - a[0], b[1] - a[1]];
        let pa = [q[0] - a[0], q[1] - a[1]];
        let t =
            ((pa[0] * ab[0] + pa[1] * ab[1]) / (ab[0] * ab[0] + ab[1] * ab[1])).clip_c(0.0, 1.0);
        let d = norm2(pa[0] - ab[0] * t, pa[1] - ab[1] * t);
        if best.is_none_or(|x| d.val() < x.val()) {
            best = Some(d);
            edge = i;
        }
    }
    let Some(d) = best else {
        return S::cst(f64::NAN);
    };
    if d.val() == 0.0 {
        let a = v[edge];
        let b = v[(edge + 1) % v.len()];
        let ab = [b[0] - a[0], b[1] - a[1]];
        return ((q[0] - a[0]) * ab[1] - (q[1] - a[1]) * ab[0]) * area.signum()
            / norm2(ab[0], ab[1]);
    }
    if inside { -d } else { d }
}

#[derive(Clone, Debug)]
struct Relation {
    kind: String,
    start: usize,
    end: usize,
    dimension: Option<usize>,
}

fn relations(raw: Option<Attr>) -> GResult<Vec<Relation>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let rows = match raw {
        Attr::List(v) => v,
        _ => return value_err("constraints must be a list"),
    };
    let mut out = Vec::new();
    for row in rows {
        let obj = row.to_json();
        let m = obj
            .as_object()
            .ok_or_else(|| GeometryError::Value("each constraint must be an object".into()))?;
        let kind = m
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let keys: &[&str] = if kind == "length" {
            &["type", "start", "end", "dimension"]
        } else {
            &["type", "start", "end"]
        };
        if !["horizontal", "vertical", "length"].contains(&kind)
            || m.len() != keys.len()
            || m.keys().any(|k| !keys.contains(&k.as_str()))
        {
            return value_err(
                "constraint must be horizontal, vertical or length with its required indices",
            );
        }
        let index = |key: &str| -> GResult<usize> {
            m.get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|x| usize::try_from(x).ok())
                .ok_or_else(|| {
                    GeometryError::Value(format!("constraint {key} must be a nonnegative integer"))
                })
        };
        let start = index("start")?;
        let end = index("end")?;
        if start >= end || out.iter().any(|r: &Relation| r.end == end) {
            return value_err("constraints require start < end and one relation per target vertex");
        }
        out.push(Relation {
            kind: kind.into(),
            start,
            end,
            dimension: if kind == "length" {
                Some(index("dimension")?)
            } else {
                None
            },
        });
    }
    out.sort_by_key(|r| r.end);
    Ok(out)
}

fn resolve<S: Scalar>(
    mut v: Vec<[S; 2]>,
    dims: &[S],
    constraints: &[Relation],
) -> GResult<Vec<[S; 2]>> {
    for c in constraints {
        if c.end >= v.len() {
            return value_err("constraint vertex index is outside the profile");
        }
        match c.kind.as_str() {
            "horizontal" => v[c.end][1] = v[c.start][1],
            "vertical" => v[c.end][0] = v[c.start][0],
            _ => {
                let d = dims
                    .get(c.dimension.unwrap_or(usize::MAX))
                    .copied()
                    .ok_or_else(|| {
                        GeometryError::Value(
                            "constraint dimension index is outside dimensions_mm".into(),
                        )
                    })?;
                if !(d.val() > 0.0) {
                    return value_err("constrained lengths must be positive");
                }
                let delta = [v[c.end][0] - v[c.start][0], v[c.end][1] - v[c.start][1]];
                let norm = norm2(delta[0], delta[1]);
                if !(norm.val() > 0.0) {
                    return value_err("a length constraint needs a nonzero authored direction");
                }
                v[c.end] = [
                    v[c.start][0] + delta[0] * d / norm,
                    v[c.start][1] + delta[1] * d / norm,
                ];
            }
        }
    }
    Ok(v)
}

#[derive(Clone, Copy)]
enum ProfileKind {
    Rectangle,
    Circle,
    Polygon,
}
struct Profile {
    kind: ProfileKind,
    constraints: Vec<Relation>,
}
static RECTANGLE: StaticInfo = StaticInfo::new();
static CIRCLE: StaticInfo = StaticInfo::new();
static POLYGON: StaticInfo = StaticInfo::new();

impl Profile {
    fn info_of(kind: ProfileKind) -> &'static KindInfo {
        let (info, name, doc, mut params) = match kind {
            ProfileKind::Rectangle => (
                &RECTANGLE,
                "profile.rectangle",
                "Filled rectangle in local XY.",
                vec![
                    ParamSpec::float("width_mm", 2.0, "mm", "Full width."),
                    ParamSpec::float("height_mm", 2.0, "mm", "Full height."),
                ],
            ),
            ProfileKind::Circle => (
                &CIRCLE,
                "profile.circle",
                "Filled circle in local XY.",
                vec![ParamSpec::float("radius_mm", 1.0, "mm", "Radius.")],
            ),
            ProfileKind::Polygon => (
                &POLYGON,
                "profile.polygon",
                "Closed simple polygon with directed construction constraints.",
                vec![
                    ParamSpec::with(
                        "vertices_mm",
                        Some(vertices_default()),
                        "mm",
                        "Ordered XY vertices; closure is implicit.",
                    ),
                    ParamSpec::with(
                        "dimensions_mm",
                        Some(array_default(&[])),
                        "mm",
                        "Lengths referenced by constraints.",
                    ),
                ],
            ),
        };
        if !matches!(kind, ProfileKind::Polygon) {
            params.extend([
                ParamSpec::float("center_x_mm", 0.0, "mm", "Local x centre."),
                ParamSpec::float("center_y_mm", 0.0, "mm", "Local y centre."),
            ]);
        }
        info.get(|| {
            let mut k = kind_info(
                name,
                MODULE,
                doc,
                "Planar signed distance, independent of z.",
                Some(FieldClass::exact()),
                true,
                &[],
                params,
            );
            k.glsl_refusal = Some("profile field uses the shared sampled viewport".into());
            k
        })
    }
    fn construct(kind: ProfileKind, mut a: ConstructArgs) -> GResult<Node> {
        if !a.children.is_empty() {
            return model_err("a profile has no child nodes");
        }
        let constraints = if matches!(kind, ProfileKind::Polygon) {
            relations(a.take_attr("constraints"))?
        } else {
            Vec::new()
        };
        if !a.attrs.is_empty() {
            return value_err("unknown profile shape setting");
        }
        Node::new(
            Arc::new(Self { kind, constraints }),
            a.children,
            a.names,
            a.params,
        )
    }
    fn polygon(&self, n: &Node) -> GResult<Vec<[f64; 2]>> {
        let (shape, values) = numeric_array(n, "vertices_mm")?;
        if shape.len() != 2 || shape[1] != 2 || !(3..=MAX_VERTICES).contains(&shape[0]) {
            return value_err(format!(
                "vertices_mm must have shape (3..{MAX_VERTICES}, 2)"
            ));
        }
        let (ds, dims) = numeric_array(n, "dimensions_mm")?;
        if ds.len() > 1 || dims.iter().any(|d| *d <= 0.0) {
            return value_err(
                "dimensions_mm must be a positive length or a vector of positive lengths",
            );
        }
        resolve(
            values.chunks_exact(2).map(|p| [p[0], p[1]]).collect(),
            &dims,
            &self.constraints,
        )
    }
    fn bounds(&self, n: &Node) -> GResult<Bounds2> {
        match self.kind {
            ProfileKind::Polygon => {
                let v = self.polygon(n)?;
                polygon_check(&v)?;
                Ok(polygon_bounds(&v))
            }
            _ => {
                let cx = finite(n, "center_x_mm")?;
                let cy = finite(n, "center_y_mm")?;
                let (x, y) = match self.kind {
                    ProfileKind::Circle => {
                        let r = positive(n, "radius_mm")?;
                        (r, r)
                    }
                    _ => (
                        positive(n, "width_mm")? * 0.5,
                        positive(n, "height_mm")? * 0.5,
                    ),
                };
                let bounds = ([cx - x, cy - y], [cx + x, cy + y]);
                if bounds
                    .0
                    .iter()
                    .chain(bounds.1.iter())
                    .any(|v| !v.is_finite())
                {
                    return value_err("profile bounds must be finite");
                }
                Ok(bounds)
            }
        }
    }
}

struct ProfileK<S> {
    kind: ProfileKind,
    center: [S; 2],
    size: [S; 2],
    vertices: Vec<[S; 2]>,
}
impl<S: Scalar> ProfileK<S> {
    fn value(&self, q: [S; 2], axis_closed: bool) -> S {
        match self.kind {
            ProfileKind::Circle => {
                norm2(q[0] - self.center[0], q[1] - self.center[1]) - self.size[0]
            }
            ProfileKind::Rectangle => {
                let dx = if axis_closed && (self.center[0] - self.size[0]).val() == 0.0 {
                    q[0] - self.center[0] - self.size[0]
                } else {
                    (q[0] - self.center[0]).abs() - self.size[0]
                };
                prism(dx, (q[1] - self.center[1]).abs() - self.size[1])
            }
            ProfileKind::Polygon => polygon_value(&self.vertices, q, axis_closed),
        }
    }
}
impl<S: Scalar> Kernel<S> for ProfileK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        self.value([x[0], x[1]], false)
    }
    fn eval_revolved_profile(&self, r: S, z: S) -> S {
        self.value([r, z], true)
    }
}
impl NodeOp for Profile {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.kind)
    }
    fn struct_json(&self) -> Vec<(String, serde_json::Value)> {
        if !matches!(self.kind, ProfileKind::Polygon) {
            return Vec::new();
        }
        vec![(
            "constraints".into(),
            serde_json::Value::Array(
                self.constraints
                    .iter()
                    .map(|r| {
                        let mut v = serde_json::json!({"type":r.kind,"start":r.start,"end":r.end});
                        if let Some(i) = r.dimension {
                            v["dimension"] = i.into();
                        }
                        v
                    })
                    .collect(),
            ),
        )]
    }
    fn struct_tokens(&self) -> Vec<String> {
        self.struct_json()
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect()
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        self.bounds(n)?;
        Ok(Some(String::new()))
    }
    fn field_class(&self, _n: &Node, _kids: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::exact())
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _kids: Vec<KernelBox<S>>,
        _ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let mut k = ProfileK {
            kind: self.kind,
            center: [S::cst(0.0); 2],
            size: [S::cst(0.0); 2],
            vertices: Vec::new(),
        };
        match self.kind {
            ProfileKind::Polygon => {
                let v = inp.array("vertices_mm")?;
                let d = inp.array("dimensions_mm")?;
                if v.shape.len() != 2 || v.shape[1] != 2 || d.shape.len() > 1 {
                    return value_err("invalid profile array shape");
                }
                k.vertices = resolve(
                    v.data.chunks_exact(2).map(|p| [p[0], p[1]]).collect(),
                    &d.data,
                    &self.constraints,
                )?;
            }
            _ => {
                k.center = [inp.scalar("center_x_mm")?, inp.scalar("center_y_mm")?];
                k.size = match self.kind {
                    ProfileKind::Circle => [inp.scalar("radius_mm")?; 2],
                    _ => [
                        inp.scalar("width_mm")? * 0.5,
                        inp.scalar("height_mm")? * 0.5,
                    ],
                };
            }
        }
        Ok(Box::new(k))
    }
    fn glsl_refusal(&self) -> Option<String> {
        self.info().glsl_refusal.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn profile_bounds(n: &Node, depth: usize) -> GResult<Bounds2> {
    if depth > 64 {
        return value_err("profile graph is too deep");
    }
    if let Some(p) = n.op().as_any().downcast_ref::<Profile>() {
        return p.bounds(n);
    }
    let kids = n.children();
    match n.kind() {
        "union" | "intersect" | "difference" => {
            if kids.len() != 2 {
                return model_err("a profile Boolean requires two children");
            }
            let a = profile_bounds(&kids[0], depth + 1)?;
            let b = profile_bounds(&kids[1], depth + 1)?;
            Ok(match n.kind() {
                "difference" => a,
                "intersect" => (
                    [a.0[0].max(b.0[0]), a.0[1].max(b.0[1])],
                    [a.1[0].min(b.1[0]), a.1[1].min(b.1[1])],
                ),
                _ => (
                    [a.0[0].min(b.0[0]), a.0[1].min(b.0[1])],
                    [a.1[0].max(b.1[0]), a.1[1].max(b.1[1])],
                ),
            })
        }
        "offset" => {
            if kids.len() != 1 {
                return model_err("profile offset requires one child");
            }
            let (lo, hi) = profile_bounds(&kids[0], depth + 1)?;
            let d = finite(n, "distance_mm")?.max(0.0);
            Ok(([lo[0] - d, lo[1] - d], [hi[0] + d, hi[1] + d]))
        }
        _ => value_err("extrusion requires a profile or a Boolean/offset combination of profiles"),
    }
}

fn smoothing_margin(n: &Node, ctx: &EvalCtx) -> GResult<f64> {
    let k = finite(n, "k_scale")? * ctx.smooth_r;
    if !k.is_finite() || k <= 0.0 {
        return value_err("smooth preview requires a finite positive smoothing radius");
    }
    Ok(k * if ctx.smooth_kind == crate::node::SmoothKind::Exp {
        std::f64::consts::LN_2
    } else {
        0.25
    })
}

fn profile_level_bounds(n: &Node, level: f64, ctx: &EvalCtx, depth: usize) -> GResult<Bounds2> {
    if depth > 64 || !level.is_finite() {
        return value_err("profile preview bounds exceed finite graph limits");
    }
    if let Some(p) = n.op().as_any().downcast_ref::<Profile>() {
        let (lo, hi) = p.bounds(n)?;
        let d = level.max(0.0);
        return Ok(([lo[0] - d, lo[1] - d], [hi[0] + d, hi[1] + d]));
    }
    let kids = n.children();
    match n.kind() {
        "union" | "intersect" | "difference" => {
            let margin = smoothing_margin(n, ctx)?;
            let t = level + if n.kind() == "union" { margin } else { 0.0 };
            let a = profile_level_bounds(&kids[0], t, ctx, depth + 1)?;
            if n.kind() == "difference" {
                return Ok(a);
            }
            let b = profile_level_bounds(&kids[1], t, ctx, depth + 1)?;
            Ok(if n.kind() == "intersect" {
                (
                    [a.0[0].max(b.0[0]), a.0[1].max(b.0[1])],
                    [a.1[0].min(b.1[0]), a.1[1].min(b.1[1])],
                )
            } else {
                (
                    [a.0[0].min(b.0[0]), a.0[1].min(b.0[1])],
                    [a.1[0].max(b.1[0]), a.1[1].max(b.1[1])],
                )
            })
        }
        "offset" => {
            profile_level_bounds(&kids[0], level + finite(n, "distance_mm")?, ctx, depth + 1)
        }
        _ => value_err("unsupported profile preview bounds"),
    }
}

fn box_map(b: crate::eval::Aabb, f: impl Fn([f64; 3]) -> [f64; 3]) -> crate::eval::Aabb {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for bits in 0..8 {
        let p = f(std::array::from_fn(|a| {
            if bits & (1 << a) == 0 { b.0[a] } else { b.1[a] }
        }));
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    (lo, hi)
}

fn solid_level_bounds(
    n: &crate::node::NodeRef,
    level: f64,
    ctx: &EvalCtx,
    depth: usize,
) -> GResult<crate::eval::Aabb> {
    if depth > 128 || !level.is_finite() {
        return value_err("smooth preview bounds exceed finite graph limits");
    }
    let kids = n.children();
    let bounds = match n.kind() {
        "extrude" => {
            let (lo, hi) = profile_level_bounds(&kids[0], level, ctx, 0)?;
            let z = finite(n, "center_z_mm")?;
            let h = positive(n, "height_mm")? * 0.5 + level.max(0.0);
            ([lo[0], lo[1], z - h], [hi[0], hi[1], z + h])
        }
        "union" | "intersect" | "difference" => {
            let margin = smoothing_margin(n, ctx)?;
            let t = level + if n.kind() == "union" { margin } else { 0.0 };
            let a = solid_level_bounds(&kids[0], t, ctx, depth + 1)?;
            if n.kind() == "difference" {
                return Ok(a);
            }
            let b = solid_level_bounds(&kids[1], t, ctx, depth + 1)?;
            if n.kind() == "intersect" {
                (
                    std::array::from_fn(|i| a.0[i].max(b.0[i])),
                    std::array::from_fn(|i| a.1[i].min(b.1[i])),
                )
            } else {
                (
                    std::array::from_fn(|i| a.0[i].min(b.0[i])),
                    std::array::from_fn(|i| a.1[i].max(b.1[i])),
                )
            }
        }
        "offset" => {
            solid_level_bounds(&kids[0], level + finite(n, "distance_mm")?, ctx, depth + 1)?
        }
        "translate" => {
            let b = solid_level_bounds(&kids[0], level, ctx, depth + 1)?;
            let d = [
                finite(n, "dx_mm")?,
                finite(n, "dy_mm")?,
                finite(n, "dz_mm")?,
            ];
            box_map(b, |p| std::array::from_fn(|a| p[a] + d[a]))
        }
        "rotate" => {
            let b = solid_level_bounds(&kids[0], level, ctx, depth + 1)?;
            let (sx, cx) = finite(n, "rx_deg")?.to_radians().sin_cos();
            let (sy, cy) = finite(n, "ry_deg")?.to_radians().sin_cos();
            let (sz, cz) = finite(n, "rz_deg")?.to_radians().sin_cos();
            box_map(b, |p| {
                let x = [p[0], cx * p[1] - sx * p[2], sx * p[1] + cx * p[2]];
                let y = [cy * x[0] + sy * x[2], x[1], -sy * x[0] + cy * x[2]];
                [cz * y[0] - sz * y[1], sz * y[0] + cz * y[1], y[2]]
            })
        }
        "scale.uniform" | "scale.nonuniform" | "scale.nonuniform_safe" => {
            let s = if n.kind() == "scale.uniform" {
                [positive(n, "scale")?; 3]
            } else {
                [positive(n, "sx")?, positive(n, "sy")?, positive(n, "sz")?]
            };
            let factor = match n.kind() {
                "scale.uniform" => s[0],
                "scale.nonuniform_safe" => s[0].min(s[1]).min(s[2]),
                _ => 1.0,
            };
            let b = solid_level_bounds(&kids[0], level / factor, ctx, depth + 1)?;
            box_map(b, |p| std::array::from_fn(|a| p[a] * s[a]))
        }
        "reflect" => {
            let b = solid_level_bounds(&kids[0], level, ctx, depth + 1)?;
            let mut normal = [finite(n, "nx")?, finite(n, "ny")?, finite(n, "nz")?];
            let m = normal.iter().map(|x| x.abs()).fold(0.0f64, f64::max);
            if m == 0.0 {
                return value_err("reflection normal must be nonzero");
            }
            for x in &mut normal {
                *x /= m;
            }
            let norm = normal[0].hypot(normal[1]).hypot(normal[2]);
            for x in &mut normal {
                *x /= norm;
            }
            let offset = finite(n, "offset_mm")?;
            box_map(b, |p| {
                let d = p.iter().zip(normal).map(|(p, n)| p * n).sum::<f64>() - offset;
                std::array::from_fn(|a| p[a] - 2.0 * normal[a] * d)
            })
        }
        _ => {
            if !kids.is_empty()
                || crate::eval::field_class_of(n, crate::node::Mode::Smooth)?.kind()
                    != ClassKind::Exact
            {
                return value_err(format!(
                    "smooth profile preview bounds are unsupported through {}",
                    n.kind()
                ));
            }
            let (lo, hi) = crate::eval::aabb_of(n).ok_or_else(|| {
                GeometryError::Value(
                    "smooth profile preview requires bounded exact siblings".into(),
                )
            })?;
            let d = level.max(0.0);
            (
                std::array::from_fn(|i| lo[i] - d),
                std::array::from_fn(|i| hi[i] + d),
            )
        }
    };
    if bounds
        .0
        .iter()
        .chain(bounds.1.iter())
        .any(|x| !x.is_finite())
    {
        return value_err("smooth profile preview bounds must be finite");
    }
    Ok(bounds)
}

pub(crate) fn preview_bounds(
    n: &crate::node::NodeRef,
    ctx: &EvalCtx,
) -> GResult<Option<crate::eval::Aabb>> {
    let affected = ctx.smooth()
        && n.walk().iter().any(|(_, node)| {
            node.kind() == "extrude"
                && node.children()[0]
                    .walk()
                    .iter()
                    .any(|(_, child)| child.kind() == "union")
        });
    if !affected {
        return Ok(crate::eval::aabb_of(n));
    }
    if !ctx.smooth_r.is_finite() || ctx.smooth_r <= 0.0 {
        return value_err("smooth preview requires a finite positive smoothing radius");
    }
    let bounds = solid_level_bounds(n, 0.0, ctx, 0)?;
    if (0..3).any(|i| bounds.1[i] < bounds.0[i]) {
        return Ok(None);
    }
    Ok(Some(bounds))
}

#[derive(Clone, Copy)]
enum SolidKind {
    Extrude,
    Revolve,
    Reflect,
}
struct Solid(SolidKind);
static EXTRUDE: StaticInfo = StaticInfo::new();
static REVOLVE: StaticInfo = StaticInfo::new();
static REFLECT: StaticInfo = StaticInfo::new();
impl Solid {
    fn info_of(kind: SolidKind) -> &'static KindInfo {
        let (info, name, doc, params) = match kind {
            SolidKind::Extrude => (
                &EXTRUDE,
                "extrude",
                "Filled profile extrusion along local z.",
                vec![
                    ParamSpec::float("height_mm", 2.0, "mm", "Full extrusion height."),
                    ParamSpec::float("center_z_mm", 0.0, "mm", "Axial centre."),
                ],
            ),
            SolidKind::Revolve => (
                &REVOLVE,
                "revolve",
                "Filled radial-axial profile revolved around local z.",
                vec![],
            ),
            SolidKind::Reflect => (
                &REFLECT,
                "reflect",
                "Reflect an implicit shape in a plane.",
                vec![
                    ParamSpec::float("nx", 1.0, "-", "Plane normal x."),
                    ParamSpec::float("ny", 0.0, "-", "Plane normal y."),
                    ParamSpec::float("nz", 0.0, "-", "Plane normal z."),
                    ParamSpec::float(
                        "offset_mm",
                        0.0,
                        "mm",
                        "Signed plane offset along its unit normal.",
                    ),
                ],
            ),
        };
        info.get(|| {
            let mut k = kind_info(
                name,
                MODULE,
                doc,
                "Inherited field bound; seams are piecewise differentiable.",
                None,
                true,
                &[],
                params,
            );
            k.glsl_refusal = Some("implicit construction uses the shared sampled viewport".into());
            k
        })
    }
    fn construct(kind: SolidKind, a: ConstructArgs) -> GResult<Node> {
        if a.children.len() != 1 {
            return model_err("this construction requires one child");
        }
        if !a.attrs.is_empty() {
            return value_err("unknown construction shape setting");
        }
        Node::new(Arc::new(Self(kind)), a.children, a.names, a.params)
    }
}
struct SolidK<S> {
    kind: SolidKind,
    child: KernelBox<S>,
    p: [S; 4],
}
impl<S: Scalar> Kernel<S> for SolidK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        match self.kind {
            SolidKind::Extrude => prism(
                self.child.eval([x[0], x[1], S::cst(0.0)]),
                (x[2] - self.p[1]).abs() - self.p[0] * 0.5,
            ),
            SolidKind::Revolve => self.child.eval_revolved_profile(norm2(x[0], x[1]), x[2]),
            SolidKind::Reflect => {
                let d = x[0] * self.p[0] + x[1] * self.p[1] + x[2] * self.p[2] - self.p[3];
                self.child
                    .eval(std::array::from_fn(|i| x[i] - self.p[i] * d * 2.0))
            }
        }
    }
}
impl NodeOp for Solid {
    fn info(&self) -> &KindInfo {
        Self::info_of(self.0)
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        match self.0 {
            SolidKind::Extrude => {
                positive(n, "height_mm")?;
                finite(n, "center_z_mm")?;
                let (lo, hi) = profile_bounds(&n.children()[0], 0)?;
                if lo[0] >= hi[0] || lo[1] >= hi[1] {
                    return value_err("profile bounds are empty");
                }
            }
            SolidKind::Revolve => {
                let p = n.children()[0]
                    .op()
                    .as_any()
                    .downcast_ref::<Profile>()
                    .ok_or_else(|| {
                        GeometryError::Value(
                            "revolution requires one circle, rectangle or polygon profile".into(),
                        )
                    })?;
                let bounds = p.bounds(&n.children()[0])?;
                if bounds.0[0] < 0.0 {
                    return value_err("a revolved profile must lie in r >= 0");
                }
            }
            SolidKind::Reflect => {
                let p = [finite(n, "nx")?, finite(n, "ny")?, finite(n, "nz")?];
                finite(n, "offset_mm")?;
                let length = p[0].hypot(p[1]).hypot(p[2]);
                if !length.is_finite() || length <= 1e-12 {
                    return value_err("reflection plane normal must have finite nonzero length");
                }
            }
        }
        Ok(Some(String::new()))
    }
    fn field_class(&self, _n: &Node, kids: &[FieldClass]) -> GResult<FieldClass> {
        let fc = kids
            .first()
            .ok_or_else(|| GeometryError::Model("missing construction child".into()))?;
        if matches!(self.0, SolidKind::Extrude) && fc.kind() == ClassKind::Lipschitz {
            return FieldClass::lipschitz(fc.k().max(1.0), "extruded profile bound");
        }
        Ok(fc.clone())
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        Ok(match self.0 {
            SolidKind::Extrude => {
                let (lo, hi) = profile_bounds(&n.children()[0], 0)?;
                let z = n.pf("center_z_mm")?;
                let h = positive(n, "height_mm")? * 0.5;
                Some(([lo[0], lo[1], z - h], [hi[0], hi[1], z + h]))
            }
            SolidKind::Revolve => {
                let p = n.children()[0]
                    .op()
                    .as_any()
                    .downcast_ref::<Profile>()
                    .ok_or_else(|| GeometryError::Value("revolution requires a profile".into()))?;
                let (lo, hi) = p.bounds(&n.children()[0])?;
                Some(([-hi[0], -hi[0], lo[1]], [hi[0], hi[0], hi[1]]))
            }
            SolidKind::Reflect => {
                let Some((lo, hi)) = crate::eval::aabb_of(&n.children()[0]) else {
                    return Ok(None);
                };
                let mut p = [n.pf("nx")?, n.pf("ny")?, n.pf("nz")?];
                let length = p[0].hypot(p[1]).hypot(p[2]);
                for a in &mut p {
                    *a /= length;
                }
                let offset = n.pf("offset_mm")?;
                let mut mn = [f64::INFINITY; 3];
                let mut mx = [f64::NEG_INFINITY; 3];
                for bits in 0..8 {
                    let x = std::array::from_fn::<_, 3, _>(|a| {
                        if bits & (1 << a) == 0 { lo[a] } else { hi[a] }
                    });
                    let d = x[0] * p[0] + x[1] * p[1] + x[2] * p[2] - offset;
                    for a in 0..3 {
                        let y = x[a] - 2.0 * p[a] * d;
                        mn[a] = mn[a].min(y);
                        mx[a] = mx[a].max(y);
                    }
                }
                Some((mn, mx))
            }
        })
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        kids: Vec<KernelBox<S>>,
        _ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let child = kids
            .into_iter()
            .next()
            .ok_or_else(|| GeometryError::Model("missing construction child".into()))?;
        let mut p = [S::cst(0.0); 4];
        match self.0 {
            SolidKind::Extrude => {
                p[0] = inp.scalar("height_mm")?;
                p[1] = inp.scalar("center_z_mm")?;
            }
            SolidKind::Revolve => {}
            SolidKind::Reflect => {
                p = [
                    inp.scalar("nx")?,
                    inp.scalar("ny")?,
                    inp.scalar("nz")?,
                    inp.scalar("offset_mm")?,
                ];
                let scale = p[..3].iter().map(|x| x.val().abs()).fold(0.0, f64::max);
                for x in &mut p[..3] {
                    *x = *x / scale;
                }
                let norm = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                for x in &mut p[..3] {
                    *x = *x / norm;
                }
            }
        }
        Ok(Box::new(SolidK {
            kind: self.0,
            child,
            p,
        }))
    }
    fn glsl_refusal(&self) -> Option<String> {
        self.info().glsl_refusal.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn rectangle(a: ConstructArgs) -> GResult<Node> {
    Profile::construct(ProfileKind::Rectangle, a)
}
fn circle(a: ConstructArgs) -> GResult<Node> {
    Profile::construct(ProfileKind::Circle, a)
}
fn polygon(a: ConstructArgs) -> GResult<Node> {
    Profile::construct(ProfileKind::Polygon, a)
}
fn extrude(a: ConstructArgs) -> GResult<Node> {
    Solid::construct(SolidKind::Extrude, a)
}
fn revolve(a: ConstructArgs) -> GResult<Node> {
    Solid::construct(SolidKind::Revolve, a)
}
fn reflect(a: ConstructArgs) -> GResult<Node> {
    Solid::construct(SolidKind::Reflect, a)
}

pub fn entries() -> KernelEntryList {
    vec![
        entry(Profile::info_of(ProfileKind::Rectangle), rectangle),
        entry(Profile::info_of(ProfileKind::Circle), circle),
        entry(Profile::info_of(ProfileKind::Polygon), polygon),
        entry(Solid::info_of(SolidKind::Extrude), extrude),
        entry(Solid::info_of(SolidKind::Revolve), revolve),
        entry(Solid::info_of(SolidKind::Reflect), reflect),
        entry(Loft::info(), loft),
        entry(Sweep::info(), sweep),
    ]
}

struct Loft;
static LOFT: StaticInfo = StaticInfo::new();
impl Loft {
    fn info() -> &'static KindInfo {
        LOFT.get(|| {
            let mut k = kind_info(
                "loft",
                MODULE,
                "Loft corresponding convex polygons along local z.",
                "Lipschitz bound includes section slope; not an exact distance.",
                None,
                true,
                &[],
                vec![
                    ParamSpec::with(
                        "sections_mm",
                        Some(ParamValue::List(vec![
                            vertices_default(),
                            vertices_default(),
                        ])),
                        "mm",
                        "XY vertices with shape (sections, vertices, 2).",
                    ),
                    ParamSpec::with(
                        "heights_mm",
                        Some(array_default(&[-1.0, 1.0])),
                        "mm",
                        "Strictly increasing section heights.",
                    ),
                ],
            );
            k.glsl_refusal = Some("polygon loft uses the shared sampled viewport".into());
            k
        })
    }
    fn construct(a: ConstructArgs) -> GResult<Node> {
        if !a.children.is_empty() || !a.attrs.is_empty() {
            return model_err("loft has no children or shape settings");
        }
        Node::new(Arc::new(Self), a.children, a.names, a.params)
    }
    fn data(n: &Node) -> GResult<(Vec<Vec<[f64; 2]>>, Vec<f64>, f64)> {
        let (shape, values) = numeric_array(n, "sections_mm")?;
        let (hs, heights) = numeric_array(n, "heights_mm")?;
        if shape.len() != 3
            || shape[2] != 2
            || !(2..=MAX_SECTIONS).contains(&shape[0])
            || !(3..=64).contains(&shape[1])
            || hs != [shape[0]]
        {
            return value_err(
                "loft needs 2 to 32 sections with 3 to 64 corresponding XY vertices and matching heights",
            );
        }
        if heights
            .windows(2)
            .any(|h| h[1] <= h[0] || !(h[1] - h[0]).is_finite())
        {
            return value_err("loft heights must increase strictly");
        }
        let sections: Vec<Vec<[f64; 2]>> = values
            .chunks_exact(shape[1] * 2)
            .map(|v| v.chunks_exact(2).map(|p| [p[0], p[1]]).collect())
            .collect();
        let orientation = polygon_check(&sections[0])?;
        for v in &sections[1..] {
            if polygon_check(v)? != orientation {
                return value_err("loft sections must have the same vertex orientation");
            }
        }
        let mut velocity = 0.0f64;
        for (s, pair) in sections.windows(2).enumerate() {
            let span = heights[s + 1] - heights[s];
            for i in 0..shape[1] {
                velocity = velocity.max(
                    (pair[1][i][0] - pair[0][i][0]).hypot(pair[1][i][1] - pair[0][i][1]) / span,
                );
                let next = (i + 1) % shape[1];
                for j in 0..shape[1] {
                    if j == i || j == next {
                        continue;
                    }
                    let value = |t: f64| {
                        let point = |k: usize| {
                            std::array::from_fn(|a| {
                                pair[0][k][a] + t * (pair[1][k][a] - pair[0][k][a])
                            })
                        };
                        orientation * cross(point(i), point(next), point(j))
                    };
                    let q0 = value(0.0);
                    let q1 = value(1.0);
                    let qm = value(0.5);
                    let a = 2.0 * (q0 + q1 - 2.0 * qm);
                    let b = q1 - q0 - a;
                    let mut minimum = q0.min(q1);
                    if a > 0.0 {
                        let t = -b / (2.0 * a);
                        if t > 0.0 && t < 1.0 {
                            minimum = minimum.min(value(t));
                        }
                    }
                    let (lo, hi) = polygon_bounds(&pair[0]);
                    let (lo1, hi1) = polygon_bounds(&pair[1]);
                    let scale = (hi[0] - lo[0])
                        .max(hi[1] - lo[1])
                        .max(hi1[0] - lo1[0])
                        .max(hi1[1] - lo1[1])
                        .max(1.0);
                    if !minimum.is_finite() || minimum <= 1e-12 * scale * scale {
                        return value_err(
                            "every interpolated loft section must remain strictly convex",
                        );
                    }
                }
            }
        }
        let bound = velocity.hypot(1.0);
        if !bound.is_finite() {
            return value_err("loft section slope exceeds the finite field bound");
        }
        Ok((sections, heights, bound))
    }
}
struct LoftK<S> {
    sections: Vec<Vec<[S; 2]>>,
    heights: Vec<S>,
}
impl<S: Scalar> Kernel<S> for LoftK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let mut index = 0;
        for i in 1..self.heights.len() - 1 {
            if x[2].val() >= self.heights[i].val() {
                index = i;
            }
        }
        let t = ((x[2] - self.heights[index]) / (self.heights[index + 1] - self.heights[index]))
            .clip_c(0.0, 1.0);
        let v: Vec<[S; 2]> = self.sections[index]
            .iter()
            .zip(&self.sections[index + 1])
            .map(|(a, b)| [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])])
            .collect();
        let lateral = polygon_value(&v, [x[0], x[1]], false);
        lateral.max((self.heights[0] - x[2]).max(x[2] - self.heights[self.heights.len() - 1]))
    }
}
impl NodeOp for Loft {
    fn info(&self) -> &KindInfo {
        Self::info()
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        Self::data(n)?;
        Ok(Some(String::new()))
    }
    fn field_class(&self, n: &Node, _kids: &[FieldClass]) -> GResult<FieldClass> {
        FieldClass::lipschitz(Self::data(n)?.2, "convex section displacement bound")
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let (v, h, _) = Self::data(n)?;
        let (lo, hi) = polygon_bounds(&v.into_iter().flatten().collect::<Vec<_>>());
        Ok(Some(([lo[0], lo[1], h[0]], [hi[0], hi[1], h[h.len() - 1]])))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _kids: Vec<KernelBox<S>>,
        _ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = inp.array("sections_mm")?;
        let h = inp.array("heights_mm")?;
        if a.shape.len() != 3 || a.shape[2] != 2 || h.shape != [a.shape[0]] {
            return value_err("invalid loft array shape");
        }
        let sections = a
            .data
            .chunks_exact(a.shape[1] * 2)
            .map(|v| v.chunks_exact(2).map(|p| [p[0], p[1]]).collect())
            .collect();
        Ok(Box::new(LoftK {
            sections,
            heights: h.data.clone(),
        }))
    }
    fn glsl_refusal(&self) -> Option<String> {
        Self::info().glsl_refusal.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Sweep;
static SWEEP: StaticInfo = StaticInfo::new();
impl Sweep {
    fn info() -> &'static KindInfo {
        SWEEP.get(|| {
            let mut k = kind_info(
                "sweep.tube",
                MODULE,
                "Circular section swept along a polyline with rounded joints.",
                "Distance bound; overlap and nearest-segment seams are piecewise.",
                Some(FieldClass::bound()),
                true,
                &[],
                vec![
                    ParamSpec::with(
                        "path_mm",
                        Some(ParamValue::List(vec![
                            array_default(&[0.0, 0.0, -1.0]),
                            array_default(&[0.0, 0.0, 1.0]),
                        ])),
                        "mm",
                        "Polyline points with shape (points, 3).",
                    ),
                    ParamSpec::float("radius_mm", 0.5, "mm", "Circular section radius."),
                ],
            );
            k.glsl_refusal = Some("polyline sweep uses the shared sampled viewport".into());
            k
        })
    }
    fn construct(a: ConstructArgs) -> GResult<Node> {
        if !a.children.is_empty() || !a.attrs.is_empty() {
            return model_err("tube sweep has no children or shape settings");
        }
        Node::new(Arc::new(Self), a.children, a.names, a.params)
    }
    fn data(n: &Node) -> GResult<Vec<[f64; 3]>> {
        let (s, v) = numeric_array(n, "path_mm")?;
        if s.len() != 2 || s[1] != 3 || !(2..=MAX_VERTICES).contains(&s[0]) {
            return value_err("path_mm needs 2 to 256 three-dimensional points");
        }
        let radius = positive(n, "radius_mm")?;
        if v.iter()
            .any(|x| !(*x - radius).is_finite() || !(*x + radius).is_finite())
        {
            return value_err("sweep bounds must be finite");
        }
        let points: Vec<[f64; 3]> = v.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
        if points.windows(2).any(|p| {
            let d = std::array::from_fn::<_, 3, _>(|i| p[1][i] - p[0][i]);
            let q = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            !q.is_finite() || q <= 1e-24
        }) {
            return value_err("consecutive path points need finite nonzero separation");
        }
        Ok(points)
    }
}
struct SweepK<S> {
    path: Vec<[S; 3]>,
    radius: S,
}
impl<S: Scalar> Kernel<S> for SweepK<S> {
    fn eval(&self, x: [S; 3]) -> S {
        let mut best: Option<S> = None;
        for pair in self.path.windows(2) {
            let d = std::array::from_fn::<_, 3, _>(|a| pair[1][a] - pair[0][a]);
            let p = std::array::from_fn::<_, 3, _>(|a| x[a] - pair[0][a]);
            let dot = |a: [S; 3], b: [S; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            let t = (dot(p, d) / dot(d, d)).clip_c(0.0, 1.0);
            let e = std::array::from_fn::<_, 3, _>(|a| p[a] - t * d[a]);
            let q = dot(e, e);
            let distance = if q.val() == 0.0 {
                S::cst(0.0)
            } else {
                q.sqrt()
            };
            best = Some(best.map_or(distance, |b| b.min(distance)));
        }
        best.unwrap_or_else(|| S::cst(f64::NAN)) - self.radius
    }
}
impl NodeOp for Sweep {
    fn info(&self) -> &KindInfo {
        Self::info()
    }
    fn validate(&self, n: &Node) -> GResult<Option<String>> {
        Self::data(n)?;
        Ok(Some(String::new()))
    }
    fn field_class(&self, _n: &Node, _kids: &[FieldClass]) -> GResult<FieldClass> {
        Ok(FieldClass::bound())
    }
    fn aabb(&self, n: &Node) -> GResult<Option<([f64; 3], [f64; 3])>> {
        let points = Self::data(n)?;
        let radius = positive(n, "radius_mm")?;
        let lo = std::array::from_fn(|a| {
            points
                .iter()
                .map(|p| p[a] - radius)
                .fold(f64::INFINITY, f64::min)
        });
        let hi = std::array::from_fn(|a| {
            points
                .iter()
                .map(|p| p[a] + radius)
                .fold(f64::NEG_INFINITY, f64::max)
        });
        Ok(Some((lo, hi)))
    }
    fn kernel<S: Scalar>(
        &self,
        _n: &Node,
        inp: &KernelInputs<S>,
        _kids: Vec<KernelBox<S>>,
        _ctx: &EvalCtx,
    ) -> GResult<KernelBox<S>> {
        let a = inp.array("path_mm")?;
        if a.shape.len() != 2 || a.shape[1] != 3 {
            return value_err("invalid path array shape");
        }
        Ok(Box::new(SweepK {
            path: a.data.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect(),
            radius: inp.scalar("radius_mm")?,
        }))
    }
    fn glsl_refusal(&self) -> Option<String> {
        Self::info().glsl_refusal.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn loft(a: ConstructArgs) -> GResult<Node> {
    Loft::construct(a)
}
fn sweep(a: ConstructArgs) -> GResult<Node> {
    Sweep::construct(a)
}
