// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_mesh::numeric::pairwise_sum;
use serde_json::{Map, Value, json};
use crate::array::{Field, nested};
use crate::errors::{PResult, ModelError};
use crate::pyval::{nums, repr_float, strs};

pub const LIMITATIONS: [&str; 6] = [
    "Quasi-1D cross-section-averaged inviscid calorically perfect gas.",
    "No combustion, viscosity, turbulence, coolant, species or moving geometry.",
    "First-order space/time; shock/contact smearing requires grid refinement.",
    "Characteristic reservoir inlet and static-pressure outlet exclude reversed or sonic boundary flow.",
    "Boundary states use local 1-D isentropic characteristics; no guaranteed steady state or nonreflection.",
    "No design adjoint; not registered as the qualified reacting-flow backend.",
];

fn is_regular(value: &Value) -> Option<Vec<usize>> {
    fn check(v: &Value, shape: &[usize], d: usize) -> bool {
        if d == shape.len() {
            return !v.is_array();
        }
        matches!(v, Value::Array(a) if a.len() == shape[d] && a.iter().all(|x| check(x, shape, d + 1)))
    }
    let mut shape = Vec::new();
    let mut cur = value;
    while let Value::Array(a) = cur {
        shape.push(a.len());
        match a.first() {
            Some(f) => cur = f,
            None => break,
        }
    }
    check(value, &shape, 0).then_some(shape)
}

fn all_real(value: &Value) -> bool {
    match value {
        Value::Array(a) => a.iter().all(all_real),
        Value::Number(_) => true,
        _ => false,
    }
}


pub fn array(value: &Value, name: &str, shape: Option<&[usize]>) -> PResult<Field> {
    if is_regular(value).is_none() {
        return Err(ModelError::invalid(format!("{name} requires a regular numeric array")));
    }
    if !all_real(value) {
        return Err(ModelError::invalid(format!(
            "{name} requires real numbers, not strings or booleans"
        )));
    }
    let field = Field::from_nested(value)
        .ok_or_else(|| ModelError::invalid(format!("{name} requires finite real numbers")))?;
    let shape_ok = shape.is_none_or(|s| s == field.shape.as_slice());
    if !field.values.iter().all(|v| v.is_finite()) || !shape_ok {
        let expected = shape.map_or_else(|| "None".to_string(), crate::pyval::shape_repr);
        return Err(ModelError::invalid(format!(
            "{name} has nonfinite values or incorrect shape; expected {expected}"
        )));
    }
    Ok(field)
}


pub fn scalar(value: &Value, name: &str) -> PResult<f64> {
    Ok(array(value, name, Some(&[]))?.values[0])
}

fn sorted_keys(keys: &[&str]) -> String {
    let mut k = keys.to_vec();
    k.sort_unstable();
    crate::pyval::list_repr(&k)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Boundary {
    Transmissive,
    Reflecting,
    SupersonicInflow([f64; 3]),
    SubsonicReservoir {
        total_pressure: f64,
        total_temperature: f64,
    },
    SubsonicPressureOutlet {
        pressure: f64,
    },
}

impl Boundary {
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Transmissive => json!({"kind": "transmissive"}),
            Self::Reflecting => json!({"kind": "reflecting"}),
            Self::SupersonicInflow(w) => json!({"kind": "supersonic_inflow", "primitive": w}),
            Self::SubsonicReservoir { total_pressure, total_temperature } => {
                json!({"kind": "subsonic_reservoir", "total_pressure_Pa": total_pressure, "total_temperature_K": total_temperature})
            }
            Self::SubsonicPressureOutlet { pressure } => {
                json!({"kind": "subsonic_pressure_outlet", "pressure_Pa": pressure})
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub gamma: f64,
    pub gas_constant: f64,
    pub x_faces: Vec<f64>,
    pub area_faces: Vec<f64>,
    pub initial: Vec<[f64; 3]>,
    pub left: Boundary,
    pub right: Boundary,
    pub end_time: f64,
    pub cfl: f64,
    pub max_steps: i64,
    pub provenance: String,
}

fn exact_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}


#[allow(clippy::too_many_lines)]
pub fn normalize(problem: &Value) -> PResult<Problem> {
    let keys = [
        "gamma",
        "gas_constant_J_kgK",
        "x_faces_m",
        "area_faces_m2",
        "initial_primitive",
        "boundaries",
        "end_time_s",
        "cfl",
        "max_steps",
        "provenance",
    ];
    let Some(p) =
        problem.as_object().filter(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(ModelError::invalid(format!(
            "quasi-1D problem requires exactly {}",
            sorted_keys(&keys)
        )));
    };
    let gamma = scalar(&p["gamma"], "gamma")?;
    let gas = scalar(&p["gas_constant_J_kgK"], "gas_constant_J_kgK")?;
    let end = scalar(&p["end_time_s"], "end_time_s")?;
    let cfl = scalar(&p["cfl"], "cfl")?;
    if !(gamma > 1.0 && gamma <= 2.0) || gas <= 0.0 || end <= 0.0 || !(cfl > 0.0 && cfl <= 0.9) {
        return Err(ModelError::invalid(
            "require 1<gamma<=2, positive gas constant/time, and 0<CFL<=0.9",
        ));
    }
    let max_steps = exact_int(&p["max_steps"])
        .filter(|s| (1..=1_000_000).contains(s))
        .ok_or_else(|| ModelError::invalid("max_steps must be an integer in [1,1000000]"))?;
    let xf = array(&p["x_faces_m"], "x_faces_m", None)?;
    if xf.shape.len() != 1
        || !(3..=4097).contains(&xf.values.len())
        || xf.values.windows(2).any(|w| w[1] - w[0] <= 0.0)
    {
        return Err(ModelError::invalid("require 2..4096 cells with increasing face positions"));
    }
    let af = array(&p["area_faces_m2"], "area_faces_m2", Some(&xf.shape))?;
    if af.values.iter().any(|a| *a <= 0.0) {
        return Err(ModelError::invalid("all face areas must be positive"));
    }
    let n = xf.values.len() - 1;
    let prim = array(&p["initial_primitive"], "initial_primitive", Some(&[n, 3]))?;
    let initial: Vec<[f64; 3]> = prim.values.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    if initial.iter().any(|w| w[0] <= 0.0 || w[2] <= 0.0) {
        return Err(ModelError::invalid(
            "initial primitive columns are positive density, velocity, positive pressure",
        ));
    }
    let Some(bc) = p["boundaries"]
        .as_object()
        .filter(|m| m.len() == 2 && m.contains_key("left") && m.contains_key("right"))
    else {
        return Err(ModelError::invalid("both left and right boundary conditions required"));
    };
    let mut sides = Vec::new();
    for (side, entry) in bc {
        let Some(entry) = entry.as_object() else {
            return Err(ModelError::invalid("boundary must be an object"));
        };
        let kind = entry.get("kind").and_then(Value::as_str).unwrap_or("");
        let only = |set: &[&str]| entry.len() == set.len() && set.iter().all(|k| entry.contains_key(*k));
        let b = if (kind == "transmissive" || kind == "reflecting") && only(&["kind"]) {
            if kind == "transmissive" { Boundary::Transmissive } else { Boundary::Reflecting }
        } else if kind == "supersonic_inflow" && only(&["kind", "primitive"]) {
            let w = array(&entry["primitive"], "boundary primitive", Some(&[3]))?;
            let w = [w.values[0], w.values[1], w.values[2]];
            if w[0] <= 0.0 || w[2] <= 0.0 {
                return Err(ModelError::invalid("positive inlet density and pressure required"));
            }
            let sound = (gamma * w[2] / w[0]).sqrt();
            let inward = if side == "left" { w[1] } else { -w[1] };
            if inward <= sound {
                return Err(ModelError::invalid(
                    "all-characteristic inflow requires inward Mach number greater than one",
                ));
            }
            Boundary::SupersonicInflow(w)
        } else if kind == "subsonic_reservoir" || kind == "subsonic_pressure_outlet" {
            let names: &[&str] = if kind == "subsonic_reservoir" {
                &["total_pressure_Pa", "total_temperature_K"]
            } else {
                &["pressure_Pa"]
            };
            let mut expected = vec!["kind"];
            expected.extend_from_slice(names);
            if !only(&expected) {
                return Err(ModelError::invalid("incorrect characteristic boundary keys"));
            }
            let values: Vec<f64> = names.iter().map(|k| scalar(&entry[*k], k)).collect::<PResult<_>>()?;
            if values.iter().any(|v| *v <= 0.0) {
                return Err(ModelError::invalid(
                    "boundary absolute pressure and temperature must be positive",
                ));
            }
            if kind == "subsonic_reservoir" {
                Boundary::SubsonicReservoir { total_pressure: values[0], total_temperature: values[1] }
            } else {
                Boundary::SubsonicPressureOutlet { pressure: values[0] }
            }
        } else {
            return Err(ModelError::invalid("unsupported boundary or extra keys"));
        };
        sides.push((side.clone(), b));
    }
    let provenance = p["provenance"]
        .as_str()
        .filter(|s| !crate::pyval::strip(s).is_empty())
        .ok_or_else(|| ModelError::invalid("problem provenance required"))?;
    let get =
        |name: &str| sides.iter().find(|(s, _)| s == name).map_or(Boundary::Transmissive, |(_, b)| b.clone());
    let out = Problem {
        gamma,
        gas_constant: gas,
        x_faces: xf.values,
        area_faces: af.values,
        initial,
        left: get("left"),
        right: get("right"),
        end_time: end,
        cfl,
        max_steps,
        provenance: provenance.to_string(),
    };
    for (left, index) in [(true, 0), (false, n - 1)] {
        let b = if left { &out.left } else { &out.right };
        if matches!(b, Boundary::SubsonicReservoir { .. } | Boundary::SubsonicPressureOutlet { .. }) {
            characteristic_boundary(out.initial[index], b, left, out.gamma, out.gas_constant)?;
        }
    }
    Ok(out)
}

#[must_use]
pub fn conservative(w: [f64; 3], gamma: f64) -> [f64; 3] {
    let (rho, u, p) = (w[0], w[1], w[2]);
    [rho, rho * u, p / (gamma - 1.0) + 0.5 * rho * u * u]
}


pub fn primitive(q: &[[f64; 3]], gamma: f64) -> PResult<Vec<[f64; 3]>> {
    if !q.iter().all(|c| c.iter().all(|v| v.is_finite())) || q.iter().any(|c| c[0] <= 0.0) {
        return Err(ModelError::invalid(
            "Euler step has nonpositive density or nonfinite state; no clipping applied",
        ));
    }
    let out: Vec<[f64; 3]> = q
        .iter()
        .map(|c| {
            let u = c[1] / c[0];
            [c[0], u, (gamma - 1.0) * (c[2] - 0.5 * c[0] * u * u)]
        })
        .collect();
    if out.iter().any(|w| !w[2].is_finite() || w[2] <= 0.0) {
        return Err(ModelError::invalid("Euler step has nonpositive pressure; no clipping applied"));
    }
    Ok(out)
}


pub fn characteristic_boundary(
    w: [f64; 3],
    b: &Boundary,
    left: bool,
    gamma: f64,
    gas: f64,
) -> PResult<[f64; 3]> {
    let (rho, u, pressure) = (w[0], w[1], w[2]);
    let sound = (gamma * pressure / rho).sqrt();
    let inward = if left { 1.0 } else { -1.0 };
    match *b {
        Boundary::SubsonicReservoir { total_pressure, total_temperature } => {
            let speed = inward * u;
            if speed < 0.0 || speed >= sound {
                return Err(ModelError::invalid(
                    "reservoir requires inward subsonic interior flow; reversal/sonic inflow unsupported",
                ));
            }
            let invariant = speed - 2.0 * sound / (gamma - 1.0);
            let c0sq = gamma * gas * total_temperature;
            let sonic = (c0sq / (1.0 + 0.5 * (gamma - 1.0))).sqrt();
            let residual =
                |v: f64| v - 2.0 * (c0sq - 0.5 * (gamma - 1.0) * v * v).sqrt() / (gamma - 1.0) - invariant;
            let (mut lo, mut hi) = (0.0, sonic);
            if residual(lo) > 0.0 || residual(hi) <= 0.0 {
                return Err(ModelError::invalid(
                    "reservoir data admit no strictly subsonic inward characteristic state",
                ));
            }
            for _ in 0..60 {
                let mid = 0.5 * (lo + hi);
                if residual(mid) > 0.0 {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let speed = 0.5 * (lo + hi);
            let temperature = (c0sq - 0.5 * (gamma - 1.0) * speed * speed) / (gamma * gas);
            let p = total_pressure * (temperature / total_temperature).powf(gamma / (gamma - 1.0));
            Ok([p / (gas * temperature), inward * speed, p])
        }
        Boundary::SubsonicPressureOutlet { pressure: target } => {
            let outward = -inward;
            let speed = outward * u;
            if speed < 0.0 || speed >= sound {
                return Err(ModelError::invalid(
                    "pressure outlet requires outward subsonic interior flow; reversal/sonic outflow unsupported",
                ));
            }
            let density = rho * (target / pressure).powf(1.0 / gamma);
            let c = (gamma * target / density).sqrt();
            let v = speed + 2.0 * (sound - c) / (gamma - 1.0);
            if !(0.0..c).contains(&v) {
                return Err(ModelError::invalid(
                    "pressure outlet data produce reversed or sonic boundary flow",
                ));
            }
            Ok([density, outward * v, target])
        }
        _ => Ok(w),
    }
}


pub fn face_flux(q: &[[f64; 3]], p: &Problem) -> PResult<(Vec<[f64; 3]>, Vec<f64>)> {
    let g = p.gamma;
    let n = q.len();
    let mut ghosts = Vec::new();
    for (left, index) in [(true, 0), (false, n - 1)] {
        let b = if left { &p.left } else { &p.right };
        let mut v = q[index];
        match b {
            Boundary::Reflecting => v[1] *= -1.0,
            Boundary::SupersonicInflow(w) => v = conservative(*w, g),
            Boundary::SubsonicReservoir { .. } | Boundary::SubsonicPressureOutlet { .. } => {
                let w = primitive(&[q[index]], g)?[0];
                v = conservative(characteristic_boundary(w, b, left, g, p.gas_constant)?, g);
            }
            Boundary::Transmissive => {}
        }
        ghosts.push(v);
    }
    let mut extended = Vec::with_capacity(n + 2);
    extended.push(ghosts[0]);
    extended.extend_from_slice(q);
    extended.push(ghosts[1]);
    let w = primitive(&extended, g)?;
    let waves: Vec<f64> = w.iter().map(|x| x[1].abs() + (g * x[2] / x[0]).sqrt()).collect();
    let speeds: Vec<f64> = waves.windows(2).map(|a| a[0].max(a[1])).collect();
    let physical: Vec<[f64; 3]> = extended
        .iter()
        .zip(&w)
        .map(|(qq, ww)| [ww[0] * ww[1], ww[0] * ww[1] * ww[1] + ww[2], (qq[2] + ww[2]) * ww[1]])
        .collect();
    let mut numerical: Vec<[f64; 3]> = (0..=n)
        .map(|i| {
            let s = speeds[i];
            let mut f = [0.0; 3];
            for c in 0..3 {
                f[c] = 0.5 * (physical[i][c] + physical[i + 1][c])
                    - 0.5 * s * (extended[i + 1][c] - extended[i][c]);
            }
            f
        })
        .collect();
    for (b, index) in [(&p.left, 0), (&p.right, n)] {
        if matches!(b, Boundary::Reflecting) {
            numerical[index][0] = 0.0;
            numerical[index][2] = 0.0;
        }
    }
    Ok((numerical, speeds))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quasi1DResult {
    pub time: f64,
    pub steps: u64,
    pub x: Vec<f64>,
    pub w: Vec<[f64; 3]>,
    pub temperature: Vec<f64>,
    pub mach: Vec<f64>,
    pub q: Vec<[f64; 3]>,
    pub initial: [f64; 3],
    pub final_: [f64; 3],
    pub exchange: [f64; 3],
    pub wall_impulse: f64,
    pub balance: [f64; 3],
    pub relative: [f64; 3],
    pub provenance: String,
}

fn column_sum(rows: impl Iterator<Item = [f64; 3]>) -> [f64; 3] {
    let rows: Vec<[f64; 3]> = rows.collect();
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let col: Vec<f64> = rows.iter().map(|r| r[c]).collect();
        *o = pairwise_sum(&col);
    }
    out
}


pub fn solve(problem: &Value) -> PResult<Quasi1DResult> {
    let p = normalize(problem)?;
    let (xf, area) = (&p.x_faces, &p.area_faces);
    let n = xf.len() - 1;
    let vol: Vec<f64> = (0..n).map(|i| 0.5 * (area[i] + area[i + 1]) * (xf[i + 1] - xf[i])).collect();
    if vol.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(ModelError::invalid("invalid derived cell volumes"));
    }
    let g = p.gamma;
    let mut q: Vec<[f64; 3]> = p.initial.iter().map(|w| conservative(*w, g)).collect();
    primitive(&q, g)?;
    let initial = column_sum(q.iter().zip(&vol).map(|(c, v)| [c[0] * v, c[1] * v, c[2] * v]));
    let mut exchange = [0.0; 3];
    let mut wall_impulse = 0.0;
    let mut t = 0.0;
    let mut steps: u64 = 0;
    while t < p.end_time {
        if i64::try_from(steps).unwrap_or(i64::MAX) >= p.max_steps {
            return Err(ModelError::invalid(format!(
                "maximum step count reached at t={}; requested final time not reached",
                repr_float(t)
            )));
        }
        let (f, speed) = face_flux(&q, &p)?;
        let integrated: Vec<[f64; 3]> =
            f.iter().zip(area).map(|(fi, a)| [fi[0] * a, fi[1] * a, fi[2] * a]).collect();
        let ratio: Vec<f64> =
            (0..n).map(|i| vol[i] / (area[i] * speed[i] + area[i + 1] * speed[i + 1])).collect();
        let min_ratio = ratio.iter().copied().fold(f64::INFINITY, f64::min);
        let dt = (p.end_time - t).min(p.cfl * min_ratio);
        #[allow(clippy::float_cmp)]
        if !dt.is_finite() || dt <= 0.0 || t + dt == t {
            return Err(ModelError::invalid("CFL step cannot advance finite time"));
        }
        let w = primitive(&q, g)?;
        let source: Vec<f64> = (0..n).map(|i| w[i][2] * (area[i + 1] - area[i])).collect();
        let new: Vec<[f64; 3]> = (0..n)
            .map(|i| {
                let mut c = q[i];
                for k in 0..3 {
                    let s = if k == 1 { source[i] } else { 0.0 };
                    c[k] = q[i][k] + dt * (integrated[i][k] - integrated[i + 1][k] + s) / vol[i];
                }
                c
            })
            .collect();
        primitive(&new, g)?;
        for k in 0..3 {
            exchange[k] += dt * (integrated[n][k] - integrated[0][k]);
        }
        wall_impulse += dt * pairwise_sum(&source);
        q = new;
        t += dt;
        steps += 1;
    }
    let w = primitive(&q, g)?;
    let final_ = column_sum(q.iter().zip(&vol).map(|(c, v)| [c[0] * v, c[1] * v, c[2] * v]));
    let wall = [0.0, wall_impulse, 0.0];
    let mut balance = [0.0; 3];
    let mut relative = [0.0; 3];
    for k in 0..3 {
        balance[k] = final_[k] - initial[k] + exchange[k] - wall[k];
        let scale = (initial[k].abs() + final_[k].abs() + exchange[k].abs() + wall[k].abs()).max(1.0);
        relative[k] = balance[k] / scale;
    }
    if relative.iter().map(|r| r.abs()).fold(0.0, f64::max) > 1e-10 {
        return Err(ModelError::invalid("integrated mass/momentum/energy ledger failed"));
    }
    let sound: Vec<f64> = w.iter().map(|x| (g * x[2] / x[0]).sqrt()).collect();
    let temperature: Vec<f64> = w.iter().map(|x| x[2] / (x[0] * p.gas_constant)).collect();
    if temperature.iter().any(|t| !t.is_finite() || *t <= 0.0) {
        return Err(ModelError::invalid("derived absolute gas temperature is not finite and positive"));
    }
    Ok(Quasi1DResult {
        time: t,
        steps,
        x: (0..n).map(|i| 0.5 * (xf[i] + xf[i + 1])).collect(),
        mach: w.iter().zip(&sound).map(|(x, s)| x[1] / s).collect(),
        w,
        temperature,
        q,
        initial,
        final_,
        exchange,
        wall_impulse,
        balance,
        relative,
        provenance: p.provenance,
    })
}

impl Quasi1DResult {
    #[must_use]
    pub fn fields(&self) -> Vec<(String, Field)> {
        let n = self.w.len();
        let col = |c: usize| self.w.iter().map(|x| x[c]).collect::<Vec<f64>>();
        vec![
            ("x_m".into(), Field::new(vec![n], self.x.clone())),
            ("density_kg_m3".into(), Field::new(vec![n], col(0))),
            ("velocity_m_s".into(), Field::new(vec![n], col(1))),
            ("pressure_Pa".into(), Field::new(vec![n], col(2))),
            ("temperature_K".into(), Field::new(vec![n], self.temperature.clone())),
            ("mach".into(), Field::new(vec![n], self.mach.clone())),
            (
                "conservative_per_m3".into(),
                Field::new(vec![n, 3], self.q.iter().flatten().copied().collect()),
            ),
        ]
    }

    #[must_use]
    pub fn diagnostics(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("schema".into(), json!("implexity-quasi1d-euler-result/1"));
        m.insert("status".into(), json!("completed_requested_time"));
        m.insert("time_s".into(), json!(self.time));
        m.insert("steps".into(), json!(self.steps));
        m.insert("method".into(), json!("first_order_Rusanov_forward_Euler_CFL"));
        m.insert(
            "ledger".into(),
            json!({
                "initial_mass_momentum_energy": nums(&self.initial),
                "final_mass_momentum_energy": nums(&self.final_),
                "outward_boundary_integrals": nums(&self.exchange),
                "wall_pressure_impulse_N_s": self.wall_impulse,
                "balance_error_kg_kgm_s_J": nums(&self.balance),
                "scaled_balance_error": nums(&self.relative),
            }),
        );
        m.insert("limitations".into(), strs(&LIMITATIONS));
        m.insert("optimization_supported".into(), json!(false));
        m.insert("physical_qualification".into(), json!(false));
        m.insert("provenance".into(), json!(self.provenance));
        m
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let m = self.diagnostics();
        let fields: Map<String, Value> = self.fields().into_iter().map(|(k, f)| (k, f.to_nested())).collect();
        let mut out = Map::new();
        for (k, v) in &m {
            out.insert(k.clone(), v.clone());
            if k == "method" {
                out.insert("fields".into(), Value::Object(fields.clone()));
            }
        }
        Value::Object(out)
    }
}

#[must_use]
pub fn linspace(start: f64, stop: f64, num: usize) -> Vec<f64> {
    implexity_mesh::numeric::linspace(start, stop, num)
}

#[must_use]
pub fn rows(values: &[[f64; 3]]) -> Value {
    nested(&[values.len(), 3], &values.iter().flatten().copied().collect::<Vec<_>>())
}
