// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use crate::array::{Field, is_bool_array, nested, nested_bool};
use crate::errors::{PResult, ModelError};
use crate::pyval::{nums, repr_float, strs};
use crate::quasi1d_euler::{array, scalar};
use crate::roots::{Residual, attach};

pub const LIMITATIONS: [&str; 6] = [
    "3-D inviscid calorically perfect gas; no viscosity, turbulence, reaction or wall heat transfer.",
    "Uniform Cartesian grid and stationary stair-step slip walls; no cut cells or mesh-convergence guarantee.",
    "First-order Rusanov flux and unsplit explicit Euler; shock/contact diffusion requires refinement.",
    "Outer boundaries: reflecting, transmissive, supersonic inflow, or local normal-characteristic subsonic inlet/outlet.",
    "Subsonic boundaries reject normal flow reversal and sonic transitions; not multidimensional nonreflecting boundaries.",
    "No design derivatives; authored masks or immutable CAD snapshots sampled at cell centres, evaluation only.",
];

pub const FACE_NAMES: [&str; 6] = ["xmin", "xmax", "ymin", "ymax", "zmin", "zmax"];

#[derive(Debug, Clone, PartialEq)]
pub enum Boundary {
    Reflecting,
    Transmissive,
    SupersonicInflow([f64; 5]),
    SubsonicReservoir {
        total_pressure: f64,
        total_temperature: f64,
        direction: [f64; 3],
    },
    SubsonicPressureOutlet {
        pressure: f64,
    },
}

impl Boundary {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Reflecting => "reflecting",
            Self::Transmissive => "transmissive",
            Self::SupersonicInflow(_) => "supersonic_inflow",
            Self::SubsonicReservoir { .. } => "subsonic_reservoir",
            Self::SubsonicPressureOutlet { .. } => "subsonic_pressure_outlet",
        }
    }

    fn characteristic(&self) -> bool {
        matches!(self, Self::SubsonicReservoir { .. } | Self::SubsonicPressureOutlet { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub gamma: f64,
    pub gas: f64,
    pub shape: [usize; 3],
    pub spacing: [f64; 3],
    pub origin: [f64; 3],
    pub fluid_mask: Vec<bool>,
    pub initial: Vec<[f64; 5]>,
    pub boundaries: [Boundary; 6],
    pub end_time: f64,
    pub cfl: f64,
    pub max_steps: i64,
    pub provenance: String,
    pub export_wall_loads: bool,
    pub geometry: Option<Value>,
}

impl Problem {
    #[must_use]
    pub fn cells(&self) -> usize {
        self.shape.iter().product()
    }

    #[must_use]
    pub fn volume(&self) -> f64 {
        self.spacing[0] * self.spacing[1] * self.spacing[2]
    }

    #[must_use]
    pub fn face_area(&self, axis: usize) -> f64 {
        (0..3).filter(|a| *a != axis).map(|a| self.spacing[a]).product()
    }

    #[must_use]
    pub fn cell(&self, i: [usize; 3]) -> usize {
        (i[0] * self.shape[1] + i[1]) * self.shape[2] + i[2]
    }

    #[must_use]
    pub fn face_shape(&self, axis: usize) -> [usize; 3] {
        let mut s = self.shape;
        s[axis] += 1;
        s
    }

    #[must_use]
    pub fn all_fluid(&self) -> bool {
        self.fluid_mask.iter().all(|m| *m)
    }
}

fn exact_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}


pub fn sample_geometry(
    geometry: &Value,
    shape: [usize; 3],
    spacing: [f64; 3],
    origin: [f64; 3],
) -> PResult<Vec<bool>> {
    let Some(g) = geometry
        .as_object()
        .filter(|m| m.len() == 3 && ["model", "node", "inside"].iter().all(|k| m.contains_key(*k)))
    else {
        return Err(ModelError::invalid("geometry requires model, node and inside"));
    };
    let inside = g["inside"].as_str().unwrap_or("");
    let node = g["node"].as_str().unwrap_or("");
    if (inside != "solid" && inside != "fluid") || node.is_empty() {
        return Err(ModelError::invalid("geometry requires a named node and inside solid or fluid"));
    }
    let model = implexity_geometry::document::build(&g["model"], None, None)
        .map_err(|e| ModelError::contract(e.to_string()))?;
    let node = model.node(node).map_err(|e| ModelError::contract(e.to_string()))?;
    let mut points = Vec::with_capacity(shape.iter().product());
    #[allow(clippy::cast_precision_loss)]
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                let idx = [i, j, k];
                let mut pt = [0.0; 3];
                for a in 0..3 {
                    pt[a] = (origin[a] + (idx[a] as f64 + 0.5) * spacing[a]) * 1000.0;
                }
                points.push(pt);
            }
        }
    }
    if !points.iter().all(|p| p.iter().all(Scalar::is_finite)) {
        return Err(ModelError::invalid("CAD sampling coordinates are not finite"));
    }
    let values = implexity_geometry::eval::eval_points(
        &node,
        &points,
        &implexity_geometry::eval::EvalOptions::exact(),
    )
    .map_err(|e| ModelError::contract(e.to_string()))?;
    if values.len() != points.len() || !values.iter().all(Scalar::is_finite) {
        return Err(ModelError::invalid("CAD node returned invalid cell-centre field values"));
    }
    Ok(values.iter().map(|v| if inside == "solid" { *v > 0.0 } else { *v <= 0.0 }).collect())
}

fn sorted_list(keys: &[&str]) -> String {
    let mut k = keys.to_vec();
    k.sort_unstable();
    crate::pyval::list_repr(&k)
}


#[allow(clippy::too_many_lines)]
pub fn normalize(problem: &Value) -> PResult<Problem> {
    let keys = [
        "gamma",
        "gas_constant_J_kgK",
        "shape",
        "spacing_m",
        "fluid_mask",
        "initial_primitive",
        "boundaries",
        "end_time_s",
        "cfl",
        "max_steps",
        "provenance",
    ];
    let optional = ["origin_m", "geometry", "export_wall_loads"];
    let Some(p) = problem.as_object().filter(|m| {
        keys.iter().all(|k| m.contains_key(*k))
            && m.keys().all(|k| keys.contains(&k.as_str()) || optional.contains(&k.as_str()))
    }) else {
        return Err(ModelError::invalid(format!(
            "3-D Euler requires {} and permits origin_m, geometry and export_wall_loads",
            sorted_list(&keys)
        )));
    };
    let export_wall_loads = match p.get("export_wall_loads") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ModelError::invalid("export_wall_loads must be boolean")),
    };
    let shape_ok = p["shape"].as_array().filter(|a| a.len() == 3).and_then(|a| {
        let v: Vec<i64> = a.iter().filter_map(exact_int).collect();
        (v.len() == 3 && v.iter().all(|x| (2..=256).contains(x)) && v.iter().product::<i64>() <= 262_144)
            .then_some(v)
    });
    let Some(shape_v) = shape_ok else {
        return Err(ModelError::invalid(
            "shape requires three integers 2..256 and at most 262144 cells",
        ));
    };
    let shape = [shape_v[0], shape_v[1], shape_v[2]].map(|v| usize::try_from(v).unwrap_or(0));
    let gamma = scalar(&p["gamma"], "gamma")?;
    let gas = scalar(&p["gas_constant_J_kgK"], "gas_constant_J_kgK")?;
    let end = scalar(&p["end_time_s"], "end_time_s")?;
    let cfl = scalar(&p["cfl"], "cfl")?;
    if !(gamma > 1.0 && gamma <= 2.0) || gas <= 0.0 || end <= 0.0 || !(cfl > 0.0 && cfl <= 0.8) {
        return Err(ModelError::invalid(
            "require 1<gamma<=2, positive gas constant/time and 0<CFL<=0.8",
        ));
    }
    let max_steps = exact_int(&p["max_steps"])
        .filter(|s| (1..=1_000_000).contains(s))
        .ok_or_else(|| ModelError::invalid("max_steps must be an integer 1..1000000"))?;
    let h = array(&p["spacing_m"], "spacing_m", Some(&[3]))?.values;
    let hp = h[0] * h[1] * h[2];
    if h.iter().any(|v| *v <= 0.0) || !hp.is_finite() || hp <= 0.0 {
        return Err(ModelError::invalid("positive finite spacing and derived cell volume required"));
    }
    let spacing = [h[0], h[1], h[2]];
    let origin_v = match p.get("origin_m") {
        Some(v) => array(v, "origin_m", Some(&[3]))?.values,
        None => vec![0.0; 3],
    };
    let origin = [origin_v[0], origin_v[1], origin_v[2]];
    let n = shape[0] * shape[1] * shape[2];
    let geometry = p.get("geometry").filter(|v| !v.is_null()).cloned();
    let mask_value = &p["fluid_mask"];
    let mut mask: Vec<bool> = if let Some(g) = &geometry {
        let sampled = sample_geometry(g, shape, spacing, origin)?;
        if !mask_value.is_null() {
            let explicit = Field::from_nested(mask_value);
            let ok = is_bool_array(mask_value)
                && explicit.as_ref().is_some_and(|e| {
                    e.shape == shape.to_vec() && e.values.iter().zip(&sampled).all(|(a, b)| (*a != 0.0) == *b)
                });
            if !ok {
                return Err(ModelError::invalid(
                    "explicit fluid_mask conflicts with sampled CAD geometry or is not boolean; use null",
                ));
            }
        }
        sampled
    } else if mask_value.is_null() {
        vec![true; n]
    } else {
        let f = Field::from_nested(mask_value)
            .ok_or_else(|| ModelError::invalid("fluid_mask must be a rectangular boolean array"))?;
        if f.shape != shape.to_vec() || !is_bool_array(mask_value) {
            return Err(ModelError::invalid(
                "fluid_mask must be null or a boolean array of the declared shape",
            ));
        }
        f.values.iter().map(|v| *v != 0.0).collect()
    };
    if !mask.iter().any(|m| *m) {
        return Err(ModelError::invalid("at least one fluid cell required"));
    }
    mask.shrink_to_fit();
    let w = array(&p["initial_primitive"], "initial_primitive", None)?;
    let initial: Vec<[f64; 5]> = if w.shape == [5] {
        vec![[w.values[0], w.values[1], w.values[2], w.values[3], w.values[4]]; n]
    } else if w.shape == [shape[0], shape[1], shape[2], 5] {
        w.values.chunks(5).map(|c| [c[0], c[1], c[2], c[3], c[4]]).collect()
    } else {
        return Err(ModelError::invalid(
            "initial primitive requires [rho,u,v,w,p] or shape+[5], with positive density/pressure",
        ));
    };
    if initial.iter().any(|x| x[0] <= 0.0 || x[4] <= 0.0) {
        return Err(ModelError::invalid(
            "initial primitive requires [rho,u,v,w,p] or shape+[5], with positive density/pressure",
        ));
    }
    let Some(bmap) =
        p["boundaries"].as_object().filter(|m| m.len() == 6 && FACE_NAMES.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(ModelError::invalid(
            "six outer face declarations xmin/xmax/ymin/ymax/zmin/zmax required",
        ));
    };
    let mut parsed: Vec<(String, Boundary)> = Vec::new();
    for (name, b) in bmap {
        let Some(b) = b.as_object() else {
            return Err(ModelError::invalid("boundary must be an object"));
        };
        let kind = b.get("kind").and_then(Value::as_str).unwrap_or("");
        let ax = "xyz".find(&name[..1]).unwrap_or(0);
        let sign = if name.ends_with("min") { 1.0 } else { -1.0 };
        let only = |set: &[&str]| b.len() == set.len() && set.iter().all(|k| b.contains_key(*k));
        let parsed_b = if (kind == "reflecting" || kind == "transmissive") && only(&["kind"]) {
            if kind == "reflecting" { Boundary::Reflecting } else { Boundary::Transmissive }
        } else if kind == "supersonic_inflow" && only(&["kind", "primitive"]) {
            let v = array(&b["primitive"], "boundary primitive", Some(&[5]))?.values;
            if v[0] <= 0.0 || v[4] <= 0.0 || sign * v[ax + 1] <= (gamma * v[4] / v[0]).sqrt() {
                return Err(ModelError::invalid(
                    "prescribed inflow requires positive state and inward normal Mach > 1",
                ));
            }
            Boundary::SupersonicInflow([v[0], v[1], v[2], v[3], v[4]])
        } else if kind == "subsonic_reservoir" || kind == "subsonic_pressure_outlet" {
            let inlet = kind == "subsonic_reservoir";
            let fields: &[&str] =
                if inlet { &["total_pressure_Pa", "total_temperature_K"] } else { &["pressure_Pa"] };
            let mut expected = vec!["kind"];
            expected.extend_from_slice(fields);
            if inlet {
                expected.push("flow_direction");
            }
            if !only(&expected) {
                return Err(ModelError::invalid("incorrect characteristic boundary keys"));
            }
            let values: Vec<f64> = fields.iter().map(|k| scalar(&b[*k], k)).collect::<PResult<_>>()?;
            if values.iter().any(|v| *v <= 0.0) {
                return Err(ModelError::invalid(
                    "positive absolute boundary pressure/temperature required",
                ));
            }
            if inlet {
                let d = array(&b["flow_direction"], "flow_direction", Some(&[3]))?.values;
                let norm = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if !norm.is_finite() || norm <= 0.0 || sign * d[ax] <= 0.0 {
                    return Err(ModelError::invalid("finite nonzero inward inlet direction required"));
                }
                Boundary::SubsonicReservoir {
                    total_pressure: values[0],
                    total_temperature: values[1],
                    direction: [d[0] / norm, d[1] / norm, d[2] / norm],
                }
            } else {
                Boundary::SubsonicPressureOutlet { pressure: values[0] }
            }
        } else {
            return Err(ModelError::invalid("unsupported 3-D boundary kind or keys"));
        };
        parsed.push((name.clone(), parsed_b));
    }
    let find = |n: &str| parsed.iter().find(|(k, _)| k == n).map_or(Boundary::Reflecting, |(_, b)| b.clone());
    let boundaries = [find("xmin"), find("xmax"), find("ymin"), find("ymax"), find("zmin"), find("zmax")];
    let provenance = p["provenance"]
        .as_str()
        .filter(|s| !crate::pyval::strip(s).is_empty())
        .ok_or_else(|| ModelError::invalid("provenance required"))?;
    let out = Problem {
        gamma,
        gas,
        shape,
        spacing,
        origin,
        fluid_mask: mask,
        initial,
        boundaries,
        end_time: end,
        cfl,
        max_steps,
        provenance: provenance.to_string(),
        export_wall_loads,
        geometry,
    };
    for axis in 0..3 {
        for (side, index) in [(0, 0), (1, out.shape[axis] - 1)] {
            let b = &out.boundaries[axis * 2 + side];
            if b.characteristic() {
                for_each_layer(&out, axis, index, |c| {
                    if out.fluid_mask[c] {
                        characteristic::<f64>(out.initial[c], b, axis, side == 0, &out, true)?;
                    }
                    Ok(())
                })?;
            }
        }
    }
    Ok(out)
}

fn for_each_layer(
    p: &Problem,
    axis: usize,
    index: usize,
    mut f: impl FnMut(usize) -> PResult<()>,
) -> PResult<()> {
    let (a1, a2) = others(axis);
    for u in 0..p.shape[a1] {
        for v in 0..p.shape[a2] {
            let mut idx = [0; 3];
            idx[axis] = index;
            idx[a1] = u;
            idx[a2] = v;
            f(p.cell(idx))?;
        }
    }
    Ok(())
}

#[must_use]
pub fn others(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

pub fn conservative<S: Scalar>(w: [S; 5], g: f64) -> [S; 5] {
    let rho = w[0];
    let ke = (w[1] * w[1] + w[2] * w[2] + w[3] * w[3]) * rho * 0.5;
    [rho, rho * w[1], rho * w[2], rho * w[3], w[4] / (g - 1.0) + ke]
}

pub fn primitive<S: Scalar>(q: [S; 5], g: f64) -> [S; 5] {
    let rho = q[0];
    let (u, v, w) = (q[1] / rho, q[2] / rho, q[3] / rho);
    let p = (q[4] - (u * u + v * v + w * w) * rho * 0.5) * (g - 1.0);
    [rho, u, v, w, p]
}


pub fn check_state<S: Scalar>(q: [S; 5], w: [S; 5]) -> PResult<()> {
    if !q.iter().all(|x| x.value().is_finite()) || q[0].value() <= 0.0 {
        return Err(ModelError::invalid("nonfinite/nonpositive Euler density"));
    }
    if !w[4].value().is_finite() || w[4].value() <= 0.0 {
        return Err(ModelError::invalid("nonpositive Euler pressure; no clipping"));
    }
    Ok(())
}


pub fn primitive_checked<S: Scalar>(q: [S; 5], g: f64, check: bool) -> PResult<[S; 5]> {
    if check && (!q.iter().all(|x| x.value().is_finite()) || q[0].value() <= 0.0) {
        return Err(ModelError::invalid("nonfinite/nonpositive Euler density"));
    }
    let w = primitive(q, g);
    if check {
        check_state(q, w)?;
    }
    Ok(w)
}

struct ReservoirResidual {
    mu: f64,
    upper: f64,
    c0sq: f64,
    g: f64,
}

impl ReservoirResidual {
    fn sound_squared<T: Scalar>(&self, f: T) -> T {
        let k = self.mu * self.mu + 0.5 * (self.g - 1.0);
        (((T::one() - f) * (T::one() + f)) * (0.5 * (self.g - 1.0)) + self.mu * self.mu) * self.c0sq / k
    }
}

impl Residual for ReservoirResidual {
    fn eval<T: Scalar>(&self, f: T, p: &[T]) -> T {
        f * (self.mu * self.upper) - self.sound_squared(f).sqrt() * (2.0 / (self.g - 1.0)) - p[0]
    }
}


pub fn characteristic<S: Scalar>(
    w: [S; 5],
    b: &Boundary,
    axis: usize,
    min_side: bool,
    p: &Problem,
    check: bool,
) -> PResult<[S; 5]> {
    let g = p.gamma;
    let inward = if min_side { 1.0 } else { -1.0 };
    let (rho, pressure) = (w[0], w[4]);
    let sound = (pressure * g / rho).sqrt();
    let mut out = w;
    match *b {
        Boundary::SubsonicReservoir { total_pressure, total_temperature, direction } => {
            let normal = w[axis + 1] * inward;
            if check && (normal.value() < 0.0 || normal.value() >= sound.value()) {
                return Err(ModelError::invalid(
                    "reservoir requires inward subsonic normal flow; reversal/sonic transition unsupported",
                ));
            }
            let mu = inward * direction[axis];
            let invariant = normal - sound * (2.0 / (g - 1.0));
            let c0sq = g * p.gas * total_temperature;
            let upper = (c0sq / (mu * mu + 0.5 * (g - 1.0))).sqrt();
            let r = ReservoirResidual { mu, upper, c0sq, g };
            let iv = invariant.value();
            let f = |x: f64| r.eval(x, &[iv]);
            if check && (!c0sq.is_finite() || f(0.0) > 0.0 || f(1.0) <= 0.0) {
                return Err(ModelError::invalid(
                    "reservoir data admit no inward normally subsonic characteristic state",
                ));
            }
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..60 {
                let mid = 0.5 * (lo + hi);
                if f(mid) > 0.0 {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let root = 0.5 * (lo + hi);
            let fraction: S = attach(&r, root, root, &[invariant]);
            let speed = fraction * upper;
            let temperature = r.sound_squared(fraction) / (g * p.gas);
            out[4] = (temperature / total_temperature).powf(g / (g - 1.0)) * total_pressure;
            out[0] = out[4] / (temperature * p.gas);
            for (c, d) in direction.iter().enumerate() {
                out[c + 1] = speed * *d;
            }
        }
        Boundary::SubsonicPressureOutlet { pressure: target } => {
            let normal = w[axis + 1] * (-inward);
            if check && (normal.value() < 0.0 || normal.value() >= sound.value()) {
                return Err(ModelError::invalid(
                    "pressure outlet requires outward subsonic normal flow; reversal/sonic transition unsupported",
                ));
            }
            let density = rho * (S::from_f64(target) / pressure).powf(1.0 / g);
            let c = (S::from_f64(g * target) / density).sqrt();
            let speed = normal + (sound - c) * (2.0 / (g - 1.0));
            if check && (speed.value() < 0.0 || speed.value() >= c.value()) {
                return Err(ModelError::invalid(
                    "pressure outlet data produce reversed or sonic normal flow",
                ));
            }
            out[0] = density;
            out[4] = S::from_f64(target);
            out[axis + 1] = speed * (-inward);
        }
        _ => {}
    }
    if check && (!out.iter().all(|x| x.value().is_finite()) || out[0].value() <= 0.0 || out[4].value() <= 0.0)
    {
        return Err(ModelError::invalid("invalid characteristic boundary state"));
    }
    Ok(out)
}

pub fn physical_flux<S: Scalar>(q: [S; 5], w: [S; 5], axis: usize) -> [S; 5] {
    let v = w[axis + 1];
    let mut f = [q[0] * v, q[1] * v, q[2] * v, q[3] * v, q[4] * v];
    f[axis + 1] += w[4];
    f[4] += w[4] * v;
    f
}

pub fn primitive_face_states<S: Scalar>(values: &[[S; 5]], active: &[bool]) -> (Vec<[S; 5]>, Vec<[S; 5]>) {
    let m = values.len();
    let mut slopes = vec![[S::zero(); 5]; m];
    for i in 1..m - 1 {
        let valid = active[i - 1] && active[i] && active[i + 1];
        for c in 0..5 {
            let l = values[i][c] - values[i - 1][c];
            let r = values[i + 1][c] - values[i][c];
            let central = (l + r) * 0.5;
            let magnitude = (l.abs() * 2.0).minimum(central.abs()).minimum(r.abs() * 2.0);
            let same = l.value() > 0.0 && r.value() > 0.0;
            let opposite = l.value() < 0.0 && r.value() < 0.0;
            let slope = if same {
                magnitude
            } else if opposite {
                -magnitude
            } else {
                S::zero()
            };
            slopes[i][c] = if valid { slope } else { S::zero() };
        }
    }
    let left = (0..m - 1).map(|i| std::array::from_fn(|c| values[i][c] + slopes[i][c] * 0.5)).collect();
    let right = (1..m).map(|i| std::array::from_fn(|c| values[i][c] - slopes[i][c] * 0.5)).collect();
    (left, right)
}

#[derive(Debug, Clone)]
pub struct Faces<S> {
    pub flux: Vec<[S; 5]>,
    pub speed: Vec<S>,
}

#[must_use]
pub fn face_index(p: &Problem, axis: usize, i: [usize; 3]) -> usize {
    let s = p.face_shape(axis);
    (i[0] * s[1] + i[1]) * s[2] + i[2]
}

fn reflect<S: Scalar>(mut x: [S; 5], axis: usize) -> [S; 5] {
    x[axis + 1] = -x[axis + 1];
    x
}


#[allow(clippy::too_many_lines)]
pub fn faces<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    axis: usize,
    reconstruct: bool,
    check: bool,
) -> PResult<Faces<S>> {
    let g = p.gamma;
    let m = p.shape[axis];
    let fshape = p.face_shape(axis);
    let nf = fshape.iter().product();
    let mut flux = vec![[S::zero(); 5]; nf];
    let mut speed = vec![S::zero(); nf];
    let (a1, a2) = others(axis);
    let (bmin, bmax) = (&p.boundaries[axis * 2], &p.boundaries[axis * 2 + 1]);
    for u in 0..p.shape[a1] {
        for v in 0..p.shape[a2] {
            let at = |t: usize| {
                let mut idx = [0; 3];
                idx[axis] = t;
                idx[a1] = u;
                idx[a2] = v;
                idx
            };
            let line: Vec<[S; 5]> = (0..m).map(|t| q[p.cell(at(t))]).collect();
            let mask: Vec<bool> = (0..m).map(|t| p.fluid_mask[p.cell(at(t))]).collect();
            let mut ghosts = [line[0], line[m - 1]];
            for (side, (b, index)) in [(bmin, 0), (bmax, m - 1)].into_iter().enumerate() {
                let cell = line[index];
                ghosts[side] = match b {
                    Boundary::Reflecting => reflect(cell, axis),
                    Boundary::SupersonicInflow(w) => conservative(w.map(S::from_f64), g),
                    Boundary::SubsonicReservoir { .. } | Boundary::SubsonicPressureOutlet { .. }
                        if mask[index] =>
                    {
                        let w = primitive_checked(cell, g, check)?;
                        conservative(characteristic(w, b, axis, side == 0, p, check)?, g)
                    }
                    _ => cell,
                };
            }
            let mut extended = Vec::with_capacity(m + 2);
            extended.push(ghosts[0]);
            extended.extend_from_slice(&line);
            extended.push(ghosts[1]);
            let mut active = Vec::with_capacity(m + 2);
            active.push(mask[0]);
            active.extend_from_slice(&mask);
            active.push(mask[m - 1]);
            let (mut left, mut right): (Vec<[S; 5]>, Vec<[S; 5]>) = if reconstruct {
                let mut stencil = active.clone();
                stencil[0] = false;
                stencil[m + 1] = false;
                let prims: Vec<[S; 5]> =
                    extended.iter().map(|x| primitive_checked(*x, g, check)).collect::<PResult<_>>()?;
                let (wl, wr) = primitive_face_states(&prims, &stencil);
                (
                    wl.into_iter().map(|w| conservative(w, g)).collect(),
                    wr.into_iter().map(|w| conservative(w, g)).collect(),
                )
            } else {
                (extended[..=m].to_vec(), extended[1..].to_vec())
            };
            for i in 0..=m {
                let (lm, rm) = (active[i], active[i + 1]);
                if !lm && rm {
                    left[i] = reflect(right[i], axis);
                }
                if lm && !rm {
                    right[i] = reflect(left[i], axis);
                }
                let wl = primitive_checked(left[i], g, check)?;
                let wr = primitive_checked(right[i], g, check)?;
                let sl = wl[axis + 1].abs() + (wl[4] * g / wl[0]).sqrt();
                let sr = wr[axis + 1].abs() + (wr[4] * g / wr[0]).sqrt();
                let s = sl.maximum(sr);
                let fl = physical_flux(left[i], wl, axis);
                let fr = physical_flux(right[i], wr, axis);
                let mut f: [S; 5] =
                    std::array::from_fn(|c| (fl[c] + fr[c]) * 0.5 - s * (right[i][c] - left[i][c]) * 0.5);
                let wall = (lm != rm)
                    || (i == 0 && matches!(bmin, Boundary::Reflecting))
                    || (i == m && matches!(bmax, Boundary::Reflecting));
                if wall {
                    for (c, fc) in f.iter_mut().enumerate() {
                        if c != axis + 1 {
                            *fc = S::zero();
                        }
                    }
                }
                if !(lm || rm) {
                    f = [S::zero(); 5];
                }
                let fi = face_index(p, axis, at(i));
                flux[fi] = f;
                speed[fi] = s;
            }
        }
    }
    Ok(Faces { flux, speed })
}

#[must_use]
pub fn outward(p: &Problem, axis: usize, flux: &[[f64; 5]]) -> [f64; 5] {
    let (a1, a2) = others(axis);
    let m = p.shape[axis];
    let area = p.face_area(axis);
    let mut out = [0.0; 5];
    for u in 0..p.shape[a1] {
        for v in 0..p.shape[a2] {
            for i in 0..=m {
                let mut idx = [0; 3];
                idx[axis] = i;
                idx[a1] = u;
                idx[a2] = v;
                let cell_at = |t: usize| {
                    let mut c = idx;
                    c[axis] = t;
                    p.fluid_mask[p.cell(c)]
                };
                let incidence: f64 = if i == 0 {
                    -f64::from(u8::from(cell_at(0)))
                } else if i == m {
                    f64::from(u8::from(cell_at(m - 1)))
                } else {
                    f64::from(u8::from(cell_at(i - 1))) - f64::from(u8::from(cell_at(i)))
                };
                if incidence != 0.0 {
                    let f = flux[face_index(p, axis, idx)];
                    for c in 0..5 {
                        out[c] += f[c] * incidence;
                    }
                }
            }
        }
    }
    out.map(|x| x * area)
}

pub fn divergence<S: Scalar>(p: &Problem, axis: usize, flux: &[[S; 5]], out: &mut [[S; 5]]) {
    let h = p.spacing[axis];
    for i in 0..p.shape[0] {
        for j in 0..p.shape[1] {
            for k in 0..p.shape[2] {
                let idx = [i, j, k];
                let mut next = idx;
                next[axis] += 1;
                let (fl, fr) = (flux[face_index(p, axis, idx)], flux[face_index(p, axis, next)]);
                let c = p.cell(idx);
                for comp in 0..5 {
                    out[c][comp] -= (fr[comp] - fl[comp]) / h;
                }
            }
        }
    }
}

#[must_use]
pub fn rate(p: &Problem, axis: usize, speed: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; p.cells()];
    add_rate(p, axis, speed, None, &mut out);
    out
}

pub fn add_rate(
    p: &Problem,
    axis: usize,
    speed: &[f64],
    face_phi: Option<(&[f64], &[f64])>,
    out: &mut [f64],
) {
    let h = p.spacing[axis];
    for i in 0..p.shape[0] {
        for j in 0..p.shape[1] {
            for k in 0..p.shape[2] {
                let idx = [i, j, k];
                let mut next = idx;
                next[axis] += 1;
                let (lo, hi) = (face_index(p, axis, idx), face_index(p, axis, next));
                let c = p.cell(idx);
                out[c] += match face_phi {
                    None => (speed[lo] + speed[hi]) / h,
                    Some((fphi, phi)) => (fphi[lo] * speed[lo] + fphi[hi] * speed[hi]) / (h * phi[c]),
                };
            }
        }
    }
}

#[must_use]
pub fn initial_state(p: &Problem) -> Vec<[f64; 5]> {
    p.initial.iter().map(|w| conservative(*w, p.gamma)).collect()
}


pub fn check_all(q: &[[f64; 5]], g: f64) -> PResult<Vec<[f64; 5]>> {
    if !q.iter().all(|c| c.iter().all(Scalar::is_finite)) || q.iter().any(|c| c[0] <= 0.0) {
        return Err(ModelError::invalid("nonfinite/nonpositive Euler density"));
    }
    let w: Vec<[f64; 5]> = q.iter().map(|c| primitive(*c, g)).collect();
    if w.iter().any(|x| !x[4].is_finite() || x[4] <= 0.0) {
        return Err(ModelError::invalid("nonpositive Euler pressure; no clipping"));
    }
    Ok(w)
}

fn masked_sum(q: &[[f64; 5]], mask: &[bool], weight: Option<&[f64]>, scale: f64) -> [f64; 5] {
    let mut out = [0.0; 5];
    for c in 0..5 {
        let col: Vec<f64> = q
            .iter()
            .enumerate()
            .filter(|(i, _)| mask[*i])
            .map(|(i, x)| x[c] * weight.map_or(1.0, |w| w[i]))
            .collect();
        out[c] = implexity_mesh::numeric::pairwise_sum(&col);
    }
    out.map(|x| x * scale)
}

#[must_use]
pub fn storage(p: &Problem, q: &[[f64; 5]], weight: Option<&[f64]>) -> [f64; 5] {
    masked_sum(q, &p.fluid_mask, weight, p.volume())
}


#[allow(clippy::too_many_lines)]
pub fn boundary_exchange(q: &[[f64; 5]], p: &Problem) -> PResult<Value> {
    let mut outer = Map::new();
    let mut internal = [0.0; 5];
    let mut wall_outer = [0.0; 3];
    let mut total = [0.0; 5];
    let mut scale = [1.0; 5];
    for axis in 0..3 {
        let f = faces(q, p, axis, false, true)?.flux;
        let o = outward(p, axis, &f);
        for c in 0..5 {
            total[c] += o[c];
        }
        let area = p.face_area(axis);
        let (a1, a2) = others(axis);
        let m = p.shape[axis];
        for u in 0..p.shape[a1] {
            for v in 0..p.shape[a2] {
                for i in 1..m {
                    let mut idx = [0; 3];
                    idx[axis] = i;
                    idx[a1] = u;
                    idx[a2] = v;
                    let mut prev = idx;
                    prev[axis] = i - 1;
                    let inc = f64::from(u8::from(p.fluid_mask[p.cell(prev)]))
                        - f64::from(u8::from(p.fluid_mask[p.cell(idx)]));
                    let ff = f[face_index(p, axis, idx)];
                    for c in 0..5 {
                        internal[c] += ff[c] * inc * area;
                        scale[c] += (ff[c] * inc).abs() * area;
                    }
                }
            }
        }
        for (side, index, sign) in [(0usize, 0usize, -1.0), (1, m, 1.0)] {
            let name = FACE_NAMES[axis * 2 + side];
            let mut rate_v = [0.0; 5];
            let mut fluid = 0usize;
            let mut sabs = [0.0; 5];
            for u in 0..p.shape[a1] {
                for v in 0..p.shape[a2] {
                    let mut idx = [0; 3];
                    idx[axis] = index;
                    idx[a1] = u;
                    idx[a2] = v;
                    let mut cell = idx;
                    cell[axis] = if side == 0 { 0 } else { m - 1 };
                    let active = p.fluid_mask[p.cell(cell)];
                    if active {
                        fluid += 1;
                        let ff = f[face_index(p, axis, idx)];
                        for c in 0..5 {
                            rate_v[c] += ff[c];
                            sabs[c] += ff[c].abs();
                        }
                    }
                }
            }
            let rate_v = rate_v.map(|x| sign * x * area);
            for c in 0..5 {
                scale[c] += sabs[c] * area;
            }
            #[allow(clippy::cast_precision_loss)]
            outer.insert(
                name.into(),
                json!({"kind": p.boundaries[axis * 2 + side].kind(), "fluid_area_m2": fluid as f64 * area,
                       "outward_mass_rate_kg_s": rate_v[0], "outward_momentum_rate_N": nums(&rate_v[1..4]),
                       "outward_total_energy_rate_W": rate_v[4]}),
            );
            if matches!(p.boundaries[axis * 2 + side], Boundary::Reflecting) {
                for c in 0..3 {
                    wall_outer[c] += rate_v[c + 1];
                }
            }
        }
    }
    let mut decomposed = internal;
    for face in outer.values() {
        let r = [
            face["outward_mass_rate_kg_s"].as_f64().unwrap_or(0.0),
            face["outward_momentum_rate_N"][0].as_f64().unwrap_or(0.0),
            face["outward_momentum_rate_N"][1].as_f64().unwrap_or(0.0),
            face["outward_momentum_rate_N"][2].as_f64().unwrap_or(0.0),
            face["outward_total_energy_rate_W"].as_f64().unwrap_or(0.0),
        ];
        for c in 0..5 {
            decomposed[c] += r[c];
        }
    }
    let worst = (0..5).map(|c| (decomposed[c] - total[c]).abs() / scale[c]).fold(0.0, f64::max);
    if worst > 1e-12 {
        return Err(ModelError::invalid("boundary flux decomposition failed"));
    }
    Ok(json!({
        "outer_faces": outer,
        "force_on_internal_voxel_solids_N": nums(&internal[1..4]),
        "force_on_outer_slip_walls_N": nums(&wall_outer),
        "total_outward_mass_momentum_energy_rate": nums(&total),
        "convention": "Positive mass/energy leaves fluid. Momentum includes pressure traction. Wall force is fluid-on-wall; force on fluid has opposite sign.",
        "method": "Final-state numerical face fluxes, including Rusanov dissipation; not time-integrated values or a thrust qualification.",
        "coupling_status": "Integrated diagnostics only; no transfer to a structural mesh.",
    }))
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallLayout {
    pub face_indices: Vec<[usize; 3]>,
    pub face_axes: Vec<usize>,
    pub face_signs: Vec<f64>,
    pub face_areas: Vec<f64>,
    pub normals: Vec<[f64; 3]>,
    pub outer: Vec<bool>,
    pub centres: Vec<[f64; 3]>,
    pub forces: Vec<[f64; 3]>,
    pub node_indices: Vec<[usize; 3]>,
    pub node_positions: Vec<[f64; 3]>,
    pub nodal_forces: Vec<[f64; 3]>,
    pub face_nodes: Vec<[usize; 4]>,
}


pub fn wall_loads(q: &[[f64; 5]], p: &Problem) -> PResult<WallLayout> {
    let mut face_indices = Vec::new();
    let mut face_axes = Vec::new();
    let mut signs = Vec::new();
    let mut forces = Vec::new();
    for axis in 0..3 {
        let f = faces(q, p, axis, false, true)?.flux;
        let fs = p.face_shape(axis);
        for i in 0..fs[0] {
            for j in 0..fs[1] {
                for k in 0..fs[2] {
                    let idx = [i, j, k];
                    let t = idx[axis];
                    let m = p.shape[axis];
                    let mut sign = 0i32;
                    if t > 0 && t < m {
                        let mut lo = idx;
                        lo[axis] = t - 1;
                        sign = i32::from(p.fluid_mask[p.cell(lo)]) - i32::from(p.fluid_mask[p.cell(idx)]);
                    }
                    if t == 0 && matches!(p.boundaries[axis * 2], Boundary::Reflecting) {
                        sign = -i32::from(p.fluid_mask[p.cell(idx)]);
                    }
                    if t == m && matches!(p.boundaries[axis * 2 + 1], Boundary::Reflecting) {
                        let mut c = idx;
                        c[axis] = m - 1;
                        sign = i32::from(p.fluid_mask[p.cell(c)]);
                    }
                    if sign != 0 {
                        let area = p.face_area(axis);
                        let ff = f[face_index(p, axis, idx)];
                        let s = f64::from(sign);
                        face_indices.push(idx);
                        face_axes.push(axis);
                        signs.push(s);
                        forces.push([ff[1] * s * area, ff[2] * s * area, ff[3] * s * area]);
                    }
                }
            }
        }
    }
    let n = face_indices.len();
    let mut normals = Vec::with_capacity(n);
    let mut areas = Vec::with_capacity(n);
    let mut outer = Vec::with_capacity(n);
    let mut centres = Vec::with_capacity(n);
    let mut corners: Vec<[usize; 3]> = Vec::with_capacity(4 * n);
    for f in 0..n {
        let axis = face_axes[f];
        let idx = face_indices[f];
        let mut normal = [0.0; 3];
        normal[axis] = -signs[f];
        normals.push(normal);
        areas.push(p.face_area(axis));
        outer.push(idx[axis] == 0 || idx[axis] == p.shape[axis]);
        #[allow(clippy::cast_precision_loss)]
        let centre: [f64; 3] = std::array::from_fn(|a| {
            p.origin[a] + (idx[a] as f64 + if a == axis { 0.0 } else { 0.5 }) * p.spacing[a]
        });
        centres.push(centre);
        let (t0, t1) = others(axis);
        for (du, dv) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
            let mut c = idx;
            c[t0] += du;
            c[t1] += dv;
            corners.push(c);
        }
    }
    let mut unique = corners.clone();
    unique.sort_unstable();
    unique.dedup();
    let inverse: Vec<usize> = corners.iter().map(|c| unique.binary_search(c).unwrap_or(0)).collect();
    let mut nodal = vec![[0.0; 3]; unique.len()];
    for (slot, node) in inverse.iter().enumerate() {
        let force = forces[slot / 4];
        for a in 0..3 {
            nodal[*node][a] += force[a] / 4.0;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let positions =
        unique.iter().map(|u| std::array::from_fn(|a| p.origin[a] + u[a] as f64 * p.spacing[a])).collect();
    let face_nodes = inverse.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    Ok(WallLayout {
        face_indices,
        face_axes,
        face_signs: signs,
        face_areas: areas,
        normals,
        outer,
        centres,
        forces,
        node_indices: unique,
        node_positions: positions,
        nodal_forces: nodal,
        face_nodes,
    })
}

fn rows3(v: &[[f64; 3]]) -> Value {
    Value::Array(v.iter().map(|r| nums(r)).collect())
}

impl WallLayout {
    #[must_use]
    pub fn to_value(&self, p: &Problem) -> Value {
        let resultant: [f64; 3] = std::array::from_fn(|a| {
            implexity_mesh::numeric::pairwise_sum(&self.forces.iter().map(|f| f[a]).collect::<Vec<_>>())
        });
        let moments: Vec<[f64; 3]> = self
            .centres
            .iter()
            .zip(&self.forces)
            .map(|(c, f)| {
                let r = [c[0] - p.origin[0], c[1] - p.origin[1], c[2] - p.origin[2]];
                [r[1] * f[2] - r[2] * f[1], r[2] * f[0] - r[0] * f[2], r[0] * f[1] - r[1] * f[0]]
            })
            .collect();
        let moment: [f64; 3] = std::array::from_fn(|a| {
            implexity_mesh::numeric::pairwise_sum(&moments.iter().map(|m| m[a]).collect::<Vec<_>>())
        });
        json!({
            "schema": "implexity-cartesian-wall-loads/1",
            "face_centres_m": rows3(&self.centres),
            "face_grid_indices": self.face_indices,
            "face_axes": self.face_axes,
            "face_areas_m2": nums(&self.face_areas),
            "solid_outward_normals": rows3(&self.normals),
            "outer_domain_wall": self.outer,
            "face_forces_N": rows3(&self.forces),
            "node_grid_indices": self.node_indices,
            "node_positions_m": rows3(&self.node_positions),
            "nodal_forces_N": rows3(&self.nodal_forces),
            "face_nodes": self.face_nodes,
            "resultant_force_N": nums(&resultant),
            "moment_about_grid_origin_Nm": nums(&moment),
            "method": "Final numerical slip-wall momentum flux, including Rusanov dissipation. Each planar face force is divided equally among its four corners; this preserves force, moment and work for bilinear face displacement.",
            "scope": "Frozen one-way load export, not a structural solution or two-way FSI. Outer domain walls are identified separately and need not belong to a physical part. Normal stress is numerical, not necessarily the adjacent cell pressure.",
        })
    }
}


pub fn residual_diagnostics(q: &[[f64; 5]], p: &Problem) -> PResult<Value> {
    let mut derivative = vec![[0.0; 5]; p.cells()];
    for axis in 0..3 {
        let f = faces(q, p, axis, false, true)?.flux;
        divergence(p, axis, &f, &mut derivative);
    }
    let active: Vec<[f64; 5]> =
        derivative.iter().zip(&p.fluid_mask).filter(|(_, m)| **m).map(|(d, _)| *d).collect();
    if !active.iter().all(|d| d.iter().all(Scalar::is_finite)) {
        return Err(ModelError::invalid("nonfinite final discrete Euler residual"));
    }
    let col = |c: usize, f: &dyn Fn(f64) -> f64| active.iter().map(|d| f(d[c])).collect::<Vec<f64>>();
    #[allow(clippy::cast_precision_loss)]
    let n = active.len() as f64;
    let mean: Vec<f64> =
        (0..5).map(|c| implexity_mesh::numeric::pairwise_sum(&col(c, &f64::abs)) / n).collect();
    let max: Vec<f64> = (0..5).map(|c| col(c, &f64::abs).into_iter().fold(0.0, f64::max)).collect();
    let integrated: Vec<f64> =
        (0..5).map(|c| implexity_mesh::numeric::pairwise_sum(&col(c, &|x| x)) * p.volume()).collect();
    Ok(json!({
        "components": ["density", "momentum_x", "momentum_y", "momentum_z", "total_energy"],
        "units": ["kg/(m\u{b3} s)", "N/m\u{b3}", "N/m\u{b3}", "N/m\u{b3}", "W/m\u{b3}"],
        "mean_absolute_time_derivative": nums(&mean),
        "maximum_absolute_time_derivative": nums(&max),
        "volume_integrated_time_derivative": nums(&integrated),
        "scope": "Final semi-discrete conservative time derivative on fluid cells, including boundary fluxes. Not normalized; compare each component in its stated units. No steady-state tolerance or convergence certification is inferred.",
    }))
}

#[derive(Debug, Clone)]
pub struct Euler3dResult {
    pub problem: Problem,
    pub time: f64,
    pub steps: u64,
    pub q: Vec<[f64; 5]>,
    pub w: Vec<[f64; 5]>,
    pub ledger: [[f64; 5]; 5],
    pub boundary_exchange: Value,
    pub residual_diagnostics: Value,
    pub wall_loads: Option<Value>,
}


pub fn solve(problem: &Value) -> PResult<Euler3dResult> {
    let p = normalize(problem)?;
    let g = p.gamma;
    let mut q = initial_state(&p);
    check_all(&q, g)?;
    let initial = storage(&p, &q, None);
    let mut exchange = [0.0; 5];
    let mut time = 0.0;
    let mut steps: u64 = 0;
    while time < p.end_time {
        if i64::try_from(steps).unwrap_or(i64::MAX) >= p.max_steps {
            return Err(ModelError::invalid(format!(
                "maximum steps reached at t={}; requested time not reached",
                repr_float(time)
            )));
        }
        let mut update = vec![[0.0; 5]; p.cells()];
        let mut r = vec![0.0; p.cells()];
        let mut out = [0.0; 5];
        for axis in 0..3 {
            let fc = faces(&q, &p, axis, false, true)?;
            divergence(&p, axis, &fc.flux, &mut update);
            add_rate(&p, axis, &fc.speed, None, &mut r);
            let o = outward(&p, axis, &fc.flux);
            for c in 0..5 {
                out[c] += o[c];
            }
        }
        let max_rate = r
            .iter()
            .zip(&p.fluid_mask)
            .filter(|(_, m)| **m)
            .map(|(x, _)| *x)
            .fold(f64::NEG_INFINITY, f64::max);
        let dt = (p.end_time - time).min(p.cfl / max_rate);
        #[allow(clippy::float_cmp)]
        if !dt.is_finite() || dt <= 0.0 || time + dt == time {
            return Err(ModelError::invalid("CFL step cannot advance time"));
        }
        let mut new = q.clone();
        for (c, cell) in new.iter_mut().enumerate() {
            if p.fluid_mask[c] {
                for k in 0..5 {
                    cell[k] += dt * update[c][k];
                }
            }
        }
        check_all(&new, g)?;
        q = new;
        for c in 0..5 {
            exchange[c] += dt * out[c];
        }
        time += dt;
        steps += 1;
    }
    let w = check_all(&q, g)?;
    let final_ = storage(&p, &q, None);
    let mut balance = [0.0; 5];
    let mut scaled = [0.0; 5];
    for c in 0..5 {
        balance[c] = final_[c] - initial[c] + exchange[c];
        scaled[c] = balance[c] / (initial[c].abs() + final_[c].abs() + exchange[c].abs()).max(1.0);
    }
    if scaled.iter().map(|x| x.abs()).fold(0.0, f64::max) > 1e-10 {
        return Err(ModelError::invalid("3-D Euler conservation ledger failed"));
    }
    if w.iter().any(|x| {
        let t = x[4] / (x[0] * p.gas);
        !t.is_finite() || t <= 0.0
    }) {
        return Err(ModelError::invalid("invalid derived temperature"));
    }
    let boundary_exchange = boundary_exchange(&q, &p)?;
    let residual_diagnostics = residual_diagnostics(&q, &p)?;
    let wall_loads = if p.export_wall_loads { Some(wall_loads(&q, &p)?.to_value(&p)) } else { None };
    Ok(Euler3dResult {
        time,
        steps,
        q,
        w,
        ledger: [initial, final_, exchange, balance, scaled],
        boundary_exchange,
        residual_diagnostics,
        wall_loads,
        problem: p,
    })
}

impl Euler3dResult {
    #[must_use]
    pub fn fields(&self) -> Vec<(String, Field)> {
        let p = &self.problem;
        let s = p.shape.to_vec();
        let mut s3 = s.clone();
        s3.push(3);
        let g = p.gamma;
        let col = |c: usize| self.w.iter().map(|x| x[c]).collect::<Vec<f64>>();
        vec![
            (
                "fluid_mask".into(),
                Field::new(s.clone(), p.fluid_mask.iter().map(|m| f64::from(u8::from(*m))).collect()),
            ),
            ("density_kg_m3".into(), Field::new(s.clone(), col(0))),
            ("velocity_m_s".into(), Field::new(s3, self.w.iter().flat_map(|x| [x[1], x[2], x[3]]).collect())),
            ("pressure_Pa".into(), Field::new(s.clone(), col(4))),
            (
                "temperature_K".into(),
                Field::new(s.clone(), self.w.iter().map(|x| x[4] / (x[0] * p.gas)).collect()),
            ),
            (
                "mach".into(),
                Field::new(
                    s,
                    self.w
                        .iter()
                        .map(|x| (x[1] * x[1] + x[2] * x[2] + x[3] * x[3]).sqrt() / (g * x[4] / x[0]).sqrt())
                        .collect(),
                ),
            ),
        ]
    }

    #[must_use]
    pub fn diagnostics(&self) -> Map<String, Value> {
        let p = &self.problem;
        let mut m = Map::new();
        m.insert("schema".into(), json!("implexity-euler3d-result/1"));
        m.insert("status".into(), json!("completed_requested_time"));
        m.insert("time_s".into(), json!(self.time));
        m.insert("steps".into(), json!(self.steps));
        m.insert("shape".into(), json!(p.shape));
        m.insert("spacing_m".into(), nums(&p.spacing));
        m.insert("origin_m".into(), nums(&p.origin));
        m.insert("method".into(), json!("unsplit_first_order_Rusanov_forward_Euler"));
        let sampling = p.geometry.as_ref().map_or(Value::Null, |g| {
            let doc = implexity_core::json::canonical(&g["model"]);
            let bytes: Vec<u8> = p.fluid_mask.iter().map(|b| u8::from(*b)).collect();
            json!({"document_sha256": hex::encode(Sha256::digest(doc.as_bytes())),
                   "node": g["node"], "inside": g["inside"],
                   "rule": "Exact-mode CAD field at cell centres; field <= 0 is inside. CAD coordinates in mm, solver coordinates in m.",
                   "fluid_mask_sha256": hex::encode(Sha256::digest(&bytes))})
        });
        m.insert("geometry_sampling".into(), sampling);
        m.insert(
            "ledger".into(),
            json!({"initial_mass_momentum_energy": nums(&self.ledger[0]), "final_mass_momentum_energy": nums(&self.ledger[1]),
                   "outward_boundary_integrals": nums(&self.ledger[2]), "balance_error": nums(&self.ledger[3]),
                   "scaled_balance_error": nums(&self.ledger[4])}),
        );
        m.insert("boundary_exchange".into(), self.boundary_exchange.clone());
        m.insert("residual_diagnostics".into(), self.residual_diagnostics.clone());
        m.insert("wall_loads".into(), self.wall_loads.clone().unwrap_or(Value::Null));
        m.insert(
            "solid_cell_fields".into(),
            json!("Inactive placeholders: exclude cells where fluid_mask is false."),
        );
        m.insert("limitations".into(), strs(&LIMITATIONS));
        m.insert("optimization_supported".into(), json!(false));
        m.insert("physical_qualification".into(), json!(false));
        m.insert("provenance".into(), json!(p.provenance));
        m
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut out = Map::new();
        for (k, v) in self.diagnostics() {
            let after = k == "geometry_sampling";
            out.insert(k, v);
            if after {
                let mut fields = Map::new();
                for (name, f) in self.fields() {
                    let v = if name == "fluid_mask" {
                        nested_bool(&f.shape, &self.problem.fluid_mask)
                    } else {
                        nested(&f.shape, &f.values)
                    };
                    fields.insert(name, v);
                }
                out.insert("fields".into(), Value::Object(fields));
            }
        }
        Value::Object(out)
    }
}
