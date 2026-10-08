// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_authoring::physics_binding::{self, ImplicitPhysics};
use implexity_core::py_repr::{repr_float, repr_str};
use implexity_core::pyobj::{list_repr, py_str, repr, truthy, type_name};
use implexity_geometry::direct_occupancy::{
    DirectOccupancyRepresentation, OccupancyModel, OccupancyOwner, Resolved, try_resolve_direct_occupancy,
};
use implexity_geometry::eval::{EvalOptions, eval_points, field_class_of};
use implexity_geometry::node::Mode;
use implexity_geometry::pyfmt::{fmt_g, g};
use implexity_geometry::{FieldClass, NodeRef, ParamRef, ParamValue};
use indexmap::IndexMap;
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use crate::error::{JobError, JobResult};

const NO_SCALE: &str = "has no finite bounds and no explicit scale; a design variable with no range is a design \
variable with no learning rate (declare lo/hi, or pass scale=<the size of a meaningful change> and the run will \
report the scale as INFERRED)";

pub const OCCUPANCY_PROFILES: [&str; 2] = ["compact", "logistic"];
pub const CONSTRAINT_KINDS: [&str; 3] = ["volume_fraction", "bound", "response_bound"];
pub const MIN_OCCUPANCY: f64 = 1.0e-3;
pub const SCALINGS: [&str; 2] = ["unit_range", "raw"];
pub const OPTIMIZERS: [&str; 2] = ["adam", "mma"];

fn opt<T>(problems: Vec<String>) -> JobResult<T> {
    Err(JobError::optimize(problems))
}

fn opt1<T>(problem: impl Into<String>) -> JobResult<T> {
    Err(JobError::optimize1(problem))
}

pub(crate) fn str_tuple_repr(items: &[&str]) -> String {
    match items {
        [one] => format!("({},)", repr_str(one)),
        _ => format!("({})", items.iter().map(|s| repr_str(s)).collect::<Vec<_>>().join(", ")),
    }
}

pub(crate) fn shape_tuple(shape: &[usize]) -> String {
    crate::manager::declare::int_tuple(shape)
}

pub(crate) fn py_float(value: &Value) -> JobResult<f64> {
    implexity_optim::pyval::py_float(value).map_err(|e| JobError::value(e.to_string()))
}

pub(crate) fn py_int(value: &Value) -> JobResult<i64> {
    implexity_optim::pyval::py_int(value).map_err(|e| JobError::value(e.to_string()))
}

pub(crate) fn json_array(value: &Value) -> Option<ArrayD<f64>> {
    fn rec(v: &Value, depth: usize, shape: &mut Vec<usize>, out: &mut Vec<f64>) -> Option<()> {
        match v {
            Value::Array(items) => {
                if shape.len() == depth {
                    shape.push(items.len());
                } else if shape.get(depth) != Some(&items.len()) {
                    return None;
                }
                for item in items {
                    rec(item, depth + 1, shape, out)?;
                }
                Some(())
            }
            Value::Number(n) => {
                if shape.len() != depth {
                    return None;
                }
                out.push(n.as_f64()?);
                Some(())
            }
            Value::Bool(b) => {
                if shape.len() != depth {
                    return None;
                }
                out.push(f64::from(u8::from(*b)));
                Some(())
            }
            _ => None,
        }
    }
    let mut shape = Vec::new();
    let mut out = Vec::new();
    rec(value, 0, &mut shape, &mut out)?;
    ArrayD::from_shape_vec(IxDyn(&shape), out).ok()
}

#[must_use]
pub fn safe(a: &ArrayD<f64>) -> Value {
    if a.ndim() == 0 {
        return a.iter().next().map_or(Value::Null, |v| implexity_optim::numeric::float_value(*v));
    }
    implexity_optim::numeric::array_to_value(a)
}

pub(crate) fn param_array(node: &NodeRef, name: &str) -> JobResult<ArrayD<f64>> {
    match node.param(name) {
        None => Ok(ArrayD::from_elem(IxDyn(&[]), f64::NAN)),
        Some(ParamValue::Str(s)) if s.is_empty() => Ok(ArrayD::from_elem(IxDyn(&[]), f64::NAN)),
        Some(p) => crate::manager::param_to_array(p).ok_or_else(|| {
            JobError::value(format!(
                "could not convert parameter {} of {} to float",
                repr_str(name),
                node.kind()
            ))
        }),
    }
}

#[must_use]
pub fn array_param(a: &ArrayD<f64>) -> ParamValue {
    if a.ndim() == 0 {
        return ParamValue::Float(a.iter().next().copied().unwrap_or(f64::NAN));
    }
    ParamValue::array_f64(a.shape().to_vec(), a.iter().copied().collect())
        .unwrap_or(ParamValue::Float(f64::NAN))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Free {
    pub r: ParamRef,
    pub document_parameter: Option<String>,
    pub lo: Option<f64>,
    pub hi: Option<f64>,
    pub scale: Option<f64>,
    pub units: String,
    pub start: ArrayD<f64>,
    pub shape: Vec<usize>,
    pub size: usize,
    pub slot: String,
    pub scale_from: Option<String>,
}

impl Free {
    fn new(r: ParamRef, lo: Option<f64>, hi: Option<f64>, scale: Option<f64>) -> Self {
        Self {
            r,
            document_parameter: None,
            lo,
            hi,
            scale,
            units: "?".into(),
            start: ArrayD::zeros(IxDyn(&[])),
            shape: Vec::new(),
            size: 0,
            slot: String::new(),
            scale_from: None,
        }
    }

    #[must_use]
    pub fn span(&self) -> Option<f64> {
        match (self.lo, self.hi) {
            (Some(lo), Some(hi)) => Some(hi - lo),
            _ => self.scale,
        }
    }

    #[must_use]
    pub fn origin(&self) -> f64 {
        self.lo.unwrap_or(0.0)
    }

    #[must_use]
    pub fn ref_str(&self) -> String {
        self.document_parameter.as_ref().map_or_else(|| self.r.as_str(), |name| format!("parameter:{name}"))
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        let f = |v: Option<f64>| v.map_or(Value::Null, implexity_optim::numeric::float_value);
        let mut description = json!({
            "ref": self.ref_str(), "units": self.units, "lo": f(self.lo), "hi": f(self.hi), "span": f(self.span()),
            "scale_from": self.scale_from, "shape": self.shape, "size": self.size,
        });
        if let Some(name) = &self.document_parameter { description["parameter"] = json!(name); }
        description
    }

    #[must_use]
    pub fn child_ref(&self) -> ParamRef {
        ParamRef::new(self.r.path.iter().skip(1).cloned().collect(), self.r.name.clone())
    }
}

fn opt_float(v: Option<&Value>) -> JobResult<Option<f64>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => py_float(v).map(Some),
    }
}

fn parse_ref(s: &str) -> JobResult<ParamRef> {
    Ok(ParamRef::parse(s)?)
}

fn as_free(entry: &Value) -> JobResult<Free> {
    match entry {
        Value::String(s) => Ok(Free::new(parse_ref(s)?, None, None, None)),
        Value::Object(d) => {
            let Some(r) = d.get("ref").filter(|v| !v.is_null()) else {
                return opt1(format!("a free entry needs a 'ref': {}", repr(entry)));
            };
            let r = match r {
                Value::String(s) => parse_ref(s)?,
                other => {
                    return opt1(format!(
                        "a free entry's 'ref' must be a ParamRef or 'a/b:name', got {}",
                        repr(other)
                    ));
                }
            };
            let mut unknown: Vec<&String> =
                d.keys().filter(|k| !["ref", "lo", "hi", "scale", "parameter", "units", "start"].contains(&k.as_str())).collect();
            if !unknown.is_empty() {
                unknown.sort();
                return opt1(format!(
                    "free {}: unknown key(s) {}; a free entry declares lo, hi and scale",
                    r.as_str(),
                    list_repr(&unknown)
                ));
            }
            let mut free = Free::new(r, opt_float(d.get("lo"))?, opt_float(d.get("hi"))?, opt_float(d.get("scale"))?);
            if let Some(name) = d.get("parameter").and_then(Value::as_str) {
                free.document_parameter = Some(name.into());
                free.units = d.get("units").and_then(Value::as_str).ok_or_else(|| JobError::value("named coordinate units are required"))?.into();
                free.start = ArrayD::from_elem(IxDyn(&[]), py_float(d.get("start").ok_or_else(|| JobError::value("named coordinate start is required"))?)?);
            }
            Ok(free)
        }
        other => opt1(format!(
            "a free entry must be a ParamRef, 'a/b:name', a dict or a Free, got {}",
            repr_str(type_name(other))
        )),
    }
}

fn root_at(child: &NodeRef, path: &[String]) -> JobResult<Option<NodeRef>> {
    let Some((first, rest)) = path.split_first() else { return Ok(None) };
    if first != "model" {
        return Err(implexity_geometry::GeometryError::Model(format!(
            "no child {} under optimize (has model)",
            repr_str(first)
        ))
        .into());
    }
    Ok(Some(child.at(rest)?))
}

fn geometry_message(e: &JobError) -> String {
    e.message()
}


#[allow(clippy::too_many_lines)]
pub fn resolve_free(child: &NodeRef, entries: &[Value]) -> JobResult<Vec<Free>> {
    let mut problems = Vec::new();
    let mut out: Vec<Free> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let mut fr = match as_free(entry) {
            Ok(f) => f,
            Err(JobError::Problems { class, problems: p, .. }) if class == "OptimizeError" => {
                problems.extend(p);
                continue;
            }
            Err(e) => return Err(e),
        };
        let key = fr.ref_str();
        if !seen.insert(key) {
            problems.push(format!("free {} appears twice", fr.ref_str()));
            continue;
        }
        let node = match root_at(child, &fr.r.path) {
            Ok(n) => n,
            Err(exc) => {
                let hint = if child.at(&fr.r.path).is_ok() {
                    let mut p = vec!["model".to_string()];
                    p.extend(fr.r.path.iter().cloned());
                    format!(
                        "; it resolves against the child -- write it rooted at the Optimize node, as {}",
                        repr_str(&ParamRef::new(p, fr.r.name.clone()).as_str())
                    )
                } else {
                    String::new()
                };
                problems.push(format!(
                    "free {} does not resolve: {}{hint}",
                    fr.ref_str(),
                    geometry_message(&exc)
                ));
                continue;
            }
        };
        let Some(node) = node else {
            problems.push(format!(
                "free {} names a parameter of the Optimize node itself; free parameters live beneath it, in the model",
                fr.ref_str()
            ));
            continue;
        };
        let info = node.info();
        let Some(pspec) = info.param(&fr.r.name) else {
            let names = info.sorted_param_names();
            problems.push(format!(
                "free {}: {} has no parameter {}; it has {}",
                fr.ref_str(),
                node.kind(),
                repr_str(&fr.r.name),
                if names.is_empty() { "none".to_string() } else { names.join(", ") }
            ));
            continue;
        };
        if info.discrete.contains(&fr.r.name) {
            problems.push(format!(
                "free {} names a DISCRETE parameter of {}: the field is piecewise constant in it (a pattern's copy \
                 count is the case that exists), so its derivative is zero everywhere it is defined and undefined at \
                 every integer.  Gradient descent cannot move it; set it, and optimise around it.",
                fr.ref_str(),
                node.kind()
            ));
            continue;
        }
        if fr.document_parameter.is_none() { fr.units.clone_from(&pspec.units); }
        let v = if fr.document_parameter.is_some() { fr.start.clone() } else { param_array(&node, &fr.r.name)? };
        fr.shape = v.shape().to_vec();
        fr.size = v.len();
        fr.start.clone_from(&v);
        let bad = v.iter().filter(|x| !x.is_finite()).count();
        if bad > 0 {
            problems.push(format!(
                "free {}: start value is not finite ({bad} of {} entries are nan/inf)",
                fr.ref_str(),
                v.len()
            ));
            continue;
        }
        if let (Some(lo), Some(hi)) = (fr.lo, fr.hi)
            && !(hi > lo)
        {
            problems.push(format!(
                "free {}: bounds are the wrong way round or empty -- lo = {}, hi = {} [{}]",
                fr.ref_str(),
                g(lo),
                g(hi),
                fr.units
            ));
            continue;
        }
        for (nm, b) in [("lo", fr.lo), ("hi", fr.hi)] {
            if let Some(b) = b
                && !b.is_finite()
            {
                problems.push(format!(
                    "free {}: {nm} bound is {}, which is not a number this can scale by",
                    fr.ref_str(),
                    repr_float(b)
                ));
            }
        }
        if fr.lo.is_some() && fr.hi.is_some() {
            fr.scale_from = Some("bounds".into());
        } else if fr.scale.is_some_and(|s| s > 0.0) {
            fr.scale_from = Some("declared scale (INFERRED, not a bound)".into());
        } else {
            problems.push(format!("free {} [{}] {NO_SCALE}", fr.ref_str(), fr.units));
            continue;
        }
        let vmin = v.iter().copied().fold(f64::INFINITY, f64::min);
        let vmax = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let which = |k: usize| {
            if v.ndim() == 0 { "the value is".to_string() } else { format!("{k} of {} entries are", v.len()) }
        };
        if let Some(lo) = fr.lo
            && vmin < lo - 1e-15
        {
            let k = v.iter().filter(|x| **x < lo).count();
            problems.push(format!(
                "free {} starts OUT OF BOUNDS: {} below lo = {} [{}] (worst {})",
                fr.ref_str(),
                which(k),
                g(lo),
                fr.units,
                fmt_g(vmin, 6)
            ));
        }
        if let Some(hi) = fr.hi
            && vmax > hi + 1e-15
        {
            let k = v.iter().filter(|x| **x > hi).count();
            problems.push(format!(
                "free {} starts OUT OF BOUNDS: {} above hi = {} [{}] (worst {})",
                fr.ref_str(),
                which(k),
                g(hi),
                fr.units,
                fmt_g(vmax, 6)
            ));
        }
        out.push(fr);
    }
    if entries.is_empty() {
        problems.push(
            "free is empty: an Optimize node with no design variable is its child, and should be written as its child"
                .into(),
        );
    }
    if !problems.is_empty() {
        return opt(problems);
    }
    out.sort_by_key(Free::ref_str);
    for (i, fr) in out.iter_mut().enumerate() {
        fr.slot = format!("d{i:03}");
    }
    Ok(out)
}


pub fn rebind(root: &NodeRef, values: &[(ParamRef, ParamValue)]) -> JobResult<NodeRef> {
    let mut by_path: BTreeMap<Vec<String>, Vec<(String, ParamValue)>> = BTreeMap::new();
    for (r, v) in values {
        by_path.entry(r.path.clone()).or_default().push((r.name.clone(), v.clone()));
    }
    if by_path.is_empty() {
        return Ok(Arc::clone(root));
    }
    let mut touched = std::collections::BTreeSet::new();
    for p in by_path.keys() {
        for i in 0..=p.len() {
            touched.insert(p[..i].to_vec());
        }
    }
    fn go(
        node: &NodeRef,
        path: &mut Vec<String>,
        touched: &std::collections::BTreeSet<Vec<String>>,
        by_path: &BTreeMap<Vec<String>, Vec<(String, ParamValue)>>,
    ) -> NodeRef {
        if !touched.contains(path) {
            return Arc::clone(node);
        }
        let kids: Vec<NodeRef> = node
            .named_children()
            .map(|(n, c)| {
                path.push(n.clone());
                let out = go(c, path, touched, by_path);
                path.pop();
                out
            })
            .collect();
        let mut params = node.params().clone();
        if let Some(sets) = by_path.get(path) {
            for (k, v) in sets {
                params.insert(k.clone(), v.clone());
            }
        }
        Arc::new(node.with_children(kids).with_params(params))
    }
    Ok(go(root, &mut Vec::new(), &touched, &by_path))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Classification {
    Field(FieldClass),
    Direct(DirectOccupancyRepresentation),
}

impl Classification {
    #[must_use]
    pub fn report(&self) -> Value {
        match self {
            Self::Direct(d) => {
                json!({"field_class": null, "step_factor": null, "geometry_representation": d.to_wire()})
            }
            Self::Field(fc) => json!({
                "field_class": fc.as_json(),
                "step_factor": fc.safe_step_factor().map_or(Value::Null, implexity_optim::numeric::float_value),
                "geometry_representation": null,
            }),
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Direct(d) => d.to_string(),
            Self::Field(fc) => fc.repr(),
        }
    }

    #[must_use]
    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaskBridge {
    pub shape: [usize; 3],
    pub h_m: f64,
    pub units: String,
    pub to_m: f64,
    pub origin: [f64; 3],
    pub band_h: f64,
    pub mode: String,
    pub smooth_r: Option<f64>,
    pub profile: String,
    pub points: Vec<[f64; 3]>,
    pub extent: [f64; 3],
}

impl MaskBridge {

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grid: [usize; 3],
        h_m: f64,
        units: &str,
        band_h: f64,
        mode: &str,
        smooth_r: Option<f64>,
        origin_mm: &Value,
        profile: &str,
    ) -> JobResult<Self> {
        if units != "mm" && units != "m" {
            return opt1(format!("model units must be 'mm' or 'm', got {}", repr_str(units)));
        }
        let to_m = if units == "mm" { 1.0e-3 } else { 1.0 };
        let world = json_array(origin_mm).filter(|a| a.shape() == [3] && a.iter().all(|v| v.is_finite()));
        let Some(world) = world else {
            return opt1("origin_mm must contain three finite world coordinates");
        };
        let origin: [f64; 3] = std::array::from_fn(|i| world[i] * 1e-3 / to_m);
        if !(band_h > 0.0) {
            return opt1(format!("band_h must be > 0 cells, got {}", repr_float(band_h)));
        }
        if !OCCUPANCY_PROFILES.contains(&profile) {
            return opt1(format!(
                "occupancy profile must be one of {}, got {}",
                str_tuple_repr(&OCCUPANCY_PROFILES),
                repr_str(profile)
            ));
        }
        #[allow(clippy::cast_precision_loss)]
        let ax: [Vec<f64>; 3] = std::array::from_fn(|i| {
            (0..grid[i]).map(|k| origin[i] + (k as f64 + 0.5) * h_m / to_m).collect()
        });
        let mut points = Vec::with_capacity(grid.iter().product());
        for x in &ax[0] {
            for y in &ax[1] {
                for z in &ax[2] {
                    points.push([*x, *y, *z]);
                }
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let extent: [f64; 3] = std::array::from_fn(|i| grid[i] as f64 * h_m / to_m);
        Ok(Self {
            shape: grid,
            h_m,
            units: units.into(),
            to_m,
            origin,
            band_h,
            mode: mode.into(),
            smooth_r,
            profile: profile.into(),
            points,
            extent,
        })
    }


    pub fn eval_options(&self) -> JobResult<EvalOptions> {
        let mode = Mode::parse(&self.mode)?;
        Ok(EvalOptions { mode, smooth_r_mm: self.smooth_r, validate: false, ..EvalOptions::default() })
    }

    fn owner(&self) -> OccupancyOwner {
        let to_mm = self.to_m * 1e3;
        OccupancyOwner {
            analysis_shape: Some(self.shape.to_vec()),
            points_mm: Some(self.points.iter().map(|p| [p[0] * to_mm, p[1] * to_mm, p[2] * to_mm]).collect()),
        }
    }


    pub fn direct(&self, model: &NodeRef) -> JobResult<Option<Resolved>> {
        Ok(try_resolve_direct_occupancy(&OccupancyModel::Node(model), Some(&self.owner()))?)
    }


    pub fn check(&self, model: &NodeRef) -> JobResult<Classification> {
        if let Some(r) = self.direct(model)? {
            return Ok(Classification::Direct(r.metadata.representation_record()));
        }
        let mode = Mode::parse(&self.mode)?;
        let fc = field_class_of(model, mode)?;
        if fc.safe_step_factor().is_none() {
            return opt1(format!(
                "the model's field class is {}: f(x) carries only the sign and the zero set, so a band of {} \
                 cell(s) around the surface is not a length and the occupancy it produces is not a volume \
                 fraction.  Normalise the field (divide by |grad f|, which is what the lattice pipeline's geff does) \
                 and declare the result LIPSCHITZ -- FieldClass.from_measurement records a measured constant \
                 honestly -- or wrap it in a node whose field_class says BOUND.",
                fc.kind().name(),
                g(self.band_h)
            ));
        }
        Ok(Classification::Field(fc))
    }

    #[must_use]
    pub fn profile_of(&self, f: f64, factor: f64) -> (f64, f64) {
        let scale = factor * self.to_m;
        let d = f * scale;
        let w = self.band_h * self.h_m;
        if self.profile == "logistic" {
            let t = (-1.5 * d / w).tanh();
            return (0.5 * (1.0 + t), 0.5 * (1.0 - t * t) * (-1.5 / w) * scale);
        }
        let raw = (w - d) / (2.0 * w);
        let s = raw.clamp(0.0, 1.0);
        let v = s * s * (3.0 - 2.0 * s);
        let ds = if raw > 0.0 && raw < 1.0 { -1.0 / (2.0 * w) } else { 0.0 };
        (v, 6.0 * s * (1.0 - s) * ds * scale)
    }


    pub fn occupancy(&self, model: &NodeRef, factor: Option<f64>) -> JobResult<Vec<f64>> {
        if let Some(r) = self.direct(model)? {
            return Ok(r.values);
        }
        let factor = match factor {
            Some(f) => f,
            None => match self.check(model)? {
                Classification::Field(fc) => fc.safe_step_factor().unwrap_or(f64::NAN),
                Classification::Direct(_) => {
                    return opt1(
                        "direct occupancy has no field-to-distance step factor; consume it through MaskBridge.occupancy",
                    );
                }
            },
        };
        let f = eval_points(model, &self.points, &self.eval_options()?)?;
        Ok(f.iter().map(|v| self.profile_of(*v, factor).0).collect())
    }

    #[must_use]
    pub fn cells(&self) -> usize {
        self.shape.iter().product()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Constraint {
    VolumeFraction {
        target: Value,
        weight: f64,
        resolved: Option<f64>,
    },
    ResponseBound {
        response: String,
        aux_key: String,
        units: String,
        bound: f64,
        sense: String,
        scale: f64,
        weight: f64,
    },
}

impl Constraint {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::VolumeFraction { .. } => "volume_fraction",
            Self::ResponseBound { .. } => "response_bound",
        }
    }

    #[must_use]
    pub fn needs_physics(&self) -> bool {
        matches!(self, Self::ResponseBound { .. })
    }


    pub fn resolve(&mut self, v_start: f64) -> JobResult<f64> {
        match self {
            Self::VolumeFraction { target, resolved, .. } => {
                let t = match target {
                    Value::Null => v_start,
                    Value::String(s) if s == "start" => v_start,
                    other => py_float(other)?,
                };
                *resolved = Some(t);
                if !(0.0 < t && t <= 1.0) {
                    return opt1(format!(
                        "constraint volume_fraction: target must be a fraction of the analysis box in (0, 1], got {}",
                        repr_float(t)
                    ));
                }
                Ok(t)
            }
            Self::ResponseBound { bound, .. } => Ok(*bound),
        }
    }

    #[must_use]
    pub fn resolved(&self) -> Option<f64> {
        match self {
            Self::VolumeFraction { resolved, .. } => *resolved,
            Self::ResponseBound { bound, .. } => Some(*bound),
        }
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        let f = implexity_optim::numeric::float_value;
        match self {
            Self::VolumeFraction { target, weight, resolved } => json!({
                "kind": "volume_fraction", "target": target,
                "resolved_target": resolved.map_or(Value::Null, f), "weight": f(*weight),
            }),
            Self::ResponseBound { response, units, bound, sense, scale, weight, .. } => json!({
                "kind": "response_bound", "response": response, "units": units, "sense": sense,
                "bound": f(*bound), "scale": f(*scale), "weight": f(*weight), "resolved_target": f(*bound),
            }),
        }
    }

    #[must_use]
    pub fn digest(&self) -> String {
        match self {
            Self::VolumeFraction { target, weight, .. } => {
                format!("volume_fraction:{}:{}", repr(target), g(*weight))
            }
            Self::ResponseBound { response, sense, bound, scale, weight, .. } => format!(
                "response_bound:{response}:{sense}:{}:{}:{}",
                repr_float(*bound),
                repr_float(*scale),
                g(*weight)
            ),
        }
    }

    #[must_use]
    pub fn residual(&self, v_mean: f64, aux: &Value) -> f64 {
        match self {
            Self::VolumeFraction { resolved, .. } => v_mean / resolved.unwrap_or(f64::NAN) - 1.0,
            Self::ResponseBound { aux_key, bound, sense, scale, .. } => {
                let value = aux.get(aux_key).and_then(Value::as_f64).unwrap_or(f64::NAN);
                if sense == "max" { (value - bound) / scale } else { (bound - value) / scale }
            }
        }
    }

    #[must_use]
    pub fn penalty(&self, v_mean: f64, aux: &Value) -> f64 {
        match self {
            Self::VolumeFraction { weight, resolved, .. } => {
                let t = resolved.unwrap_or(f64::NAN);
                weight * ((v_mean - t) / t).powi(2)
            }
            Self::ResponseBound { weight, .. } => {
                let r = self.residual(v_mean, aux);
                weight * r.max(0.0).powi(2)
            }
        }
    }
}

fn response_bound(
    response: &str,
    aux_key: &str,
    units: &str,
    bound: f64,
    sense: &str,
    scale: Option<f64>,
    weight: f64,
) -> JobResult<Constraint> {
    let scale = if let Some(s) = scale {
        s
    } else {
        if bound == 0.0 {
            return opt1(format!(
                "response_bound on {}: a zero bound needs an explicit positive 'scale' in {units}",
                repr_str(response)
            ));
        }
        bound.abs()
    };
    if !(scale > 0.0 && scale.is_finite() && bound.is_finite()) {
        return opt1(format!(
            "response_bound on {}: bound and scale must be finite and scale > 0",
            repr_str(response)
        ));
    }
    if !(weight >= 0.0) {
        return opt1(format!("response_bound on {}: weight must be >= 0", repr_str(response)));
    }
    Ok(Constraint::ResponseBound {
        response: response.into(),
        aux_key: aux_key.into(),
        units: units.into(),
        bound,
        sense: sense.into(),
        scale,
        weight,
    })
}

fn sorted_unknown(d: &Map<String, Value>, allowed: &[&str]) -> Vec<String> {
    let mut u: Vec<String> = d.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
    u.sort();
    u
}


#[allow(clippy::too_many_lines)]
pub fn parse_constraints(
    entries: &[Value],
    free: &mut [Free],
    responses: &BTreeMap<String, (String, String)>,
    terms: &[String],
) -> JobResult<Vec<Constraint>> {
    let mut problems = Vec::new();
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let Some(d) = e.as_object() else {
            problems.push(format!(
                "constraints[{i}] must be an object {{kind: ...}}, got {}",
                repr_str(type_name(e))
            ));
            continue;
        };
        let kind = d.get("kind").cloned().unwrap_or(Value::Null);
        let Some(kind_s) = kind.as_str().filter(|k| CONSTRAINT_KINDS.contains(k)) else {
            problems.push(format!(
                "constraints[{i}]: unknown kind {}; this node knows {}",
                repr(&kind),
                CONSTRAINT_KINDS.join(", ")
            ));
            continue;
        };
        match kind_s {
            "volume_fraction" => {
                let unknown = sorted_unknown(d, &["kind", "target", "weight"]);
                if !unknown.is_empty() {
                    problems.push(format!(
                        "constraints[{i}] (volume_fraction): unknown key(s) {}",
                        list_repr(&unknown)
                    ));
                    continue;
                }
                let weight = match d.get("weight") {
                    None => 25.0,
                    Some(w) => py_float(w)?,
                };
                out.push(Constraint::VolumeFraction {
                    target: d.get("target").cloned().unwrap_or_else(|| Value::from("start")),
                    weight,
                    resolved: None,
                });
            }
            "response_bound" => {
                let unknown = sorted_unknown(d, &["kind", "response", "max", "min", "scale", "weight"]);
                if !unknown.is_empty() {
                    problems.push(format!(
                        "constraints[{i}] (response_bound): unknown key(s) {}",
                        list_repr(&unknown)
                    ));
                    continue;
                }
                let name = d.get("response").cloned().unwrap_or(Value::Null);
                if d.contains_key("max") == d.contains_key("min") {
                    problems.push(format!(
                        "constraints[{i}] (response_bound): give exactly one of 'max' or 'min'"
                    ));
                    continue;
                }
                let name_s = name.as_str().map(str::to_string);
                let (aux_key, units) = match name_s.as_deref() {
                    Some(n) if responses.contains_key(n) => responses[n].clone(),
                    Some(n) if terms.iter().any(|t| t == n) => (n.to_string(), "term units".to_string()),
                    _ => {
                        let mut rs: Vec<&String> = responses.keys().collect();
                        rs.sort();
                        let mut ts: Vec<&String> = terms.iter().collect();
                        ts.sort();
                        ts.dedup();
                        problems.push(format!(
                            "constraints[{i}] (response_bound): unknown response {}; the physics declares {} and the \
                             active terms are {}",
                            repr(&name),
                            list_repr(&rs),
                            list_repr(&ts)
                        ));
                        continue;
                    }
                };
                let sense = if d.contains_key("max") { "max" } else { "min" };
                let numbers = (|| -> JobResult<(f64, Option<f64>, f64)> {
                    let bound = py_float(&d[sense])?;
                    let scale = opt_float(d.get("scale"))?;
                    let weight = match d.get("weight") {
                        None => 25.0,
                        Some(w) => py_float(w)?,
                    };
                    Ok((bound, scale, weight))
                })();
                let Ok((bound, scale, weight)) = numbers else {
                    problems.push(format!(
                        "constraints[{i}] (response_bound): bound, scale and weight must be numbers"
                    ));
                    continue;
                };
                let name_s = name_s.unwrap_or_default();
                match response_bound(&name_s, &aux_key, &units, bound, sense, scale, weight) {
                    Ok(c) => out.push(c),
                    Err(JobError::Problems { problems: p, .. }) => problems.extend(p),
                    Err(e) => return Err(e),
                }
            }
            _ => {
                let unknown = sorted_unknown(d, &["kind", "ref", "lo", "hi"]);
                if !unknown.is_empty() {
                    problems
                        .push(format!("constraints[{i}] (bound): unknown key(s) {}", list_repr(&unknown)));
                    continue;
                }
                let Some(r) = d.get("ref").and_then(Value::as_str) else {
                    problems.push(format!("constraints[{i}] (bound): needs a 'ref'"));
                    continue;
                };
                let r = parse_ref(r)?;
                let Some(fr) = free.iter_mut().find(|f| f.r.path == r.path && f.r.name == r.name) else {
                    let mut names: Vec<String> = Vec::new();
                    problems.push(format!(
                        "constraints[{i}]: bound on {}, which is not free in this optimisation (free: {}) -- a bound \
                         on a parameter nothing can move is not a constraint",
                        r.as_str(),
                        {
                            names.extend(entries_free_names(free));
                            names.join(", ")
                        }
                    ));
                    continue;
                };
                if let Some(lo) = d.get("lo") {
                    fr.lo = Some(py_float(lo)?);
                }
                if let Some(hi) = d.get("hi") {
                    fr.hi = Some(py_float(hi)?);
                }
            }
        }
    }
    if !problems.is_empty() {
        return opt(problems);
    }
    Ok(out)
}

fn entries_free_names(free: &[Free]) -> Vec<String> {
    let mut v: Vec<String> = free.iter().map(Free::ref_str).collect();
    v.sort();
    v
}

#[must_use]
pub fn defaults() -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in [
        ("grid", Value::Null),
        ("iters", json!(8)),
        ("lr", json!(0.05)),
        ("band_h", json!(1.0)),
        ("model_units", json!("mm")),
        ("eval_mode", json!("smooth")),
        ("smooth_r", Value::Null),
        ("lattice_volfrac", Value::Null),
        ("scaling", json!("unit_range")),
        ("steerable", json!(false)),
        ("live_every", json!(1)),
        ("momentum", json!("reset")),
        ("cg_tol", Value::Null),
        ("optimizer", Value::Null),
        ("mma", Value::Null),
        ("occupancy", Value::Null),
        ("physics", Value::Null),
        ("nonfinite_retries", json!(4)),
        ("document_parameter_context", Value::Null),
    ] {
        m.insert(k.into(), v);
    }
    m
}

#[must_use]
pub fn mma_defaults() -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in [
        ("globalize", json!(true)),
        ("move_limit", json!(0.2)),
        ("epsimin", json!(1e-7)),
        ("max_inner", json!(200)),
        ("c", Value::Null),
        ("asymptote_init", json!(0.5)),
        ("asymptote_lo", json!(0.01)),
        ("asymptote_hi", json!(10.0)),
        ("max_conservative", json!(15)),
        ("kkt_tol", Value::Null),
        ("feas_tol", json!(1e-3)),
    ] {
        m.insert(k.into(), v);
    }
    m
}

fn sorted_keys(m: &Map<String, Value>) -> Vec<String> {
    let mut v: Vec<String> = m.keys().cloned().collect();
    v.sort();
    v
}


pub fn mma_options(over: &Value) -> JobResult<Map<String, Value>> {
    let defaults = mma_defaults();
    if over.is_null() {
        return Ok(defaults);
    }
    let Some(over) = over.as_object() else {
        return opt1(format!("mma must be an object of driver options, got {}", repr_str(type_name(over))));
    };
    let mut bad: Vec<String> = over.keys().filter(|k| !defaults.contains_key(*k)).cloned().collect();
    if !bad.is_empty() {
        bad.sort();
        return opt1(format!(
            "unknown mma option(s) {}; the driver takes {}",
            list_repr(&bad),
            list_repr(&sorted_keys(&defaults))
        ));
    }
    let mut o = defaults;
    for (k, v) in over {
        o.insert(k.clone(), v.clone());
    }
    let mut problems = Vec::new();
    let globalize = truthy(&o["globalize"]);
    o.insert("globalize".into(), Value::Bool(globalize));
    let f = |k: &str| py_float(&o[k]);
    let move_limit = f("move_limit")?;
    if !(0.0 < move_limit && move_limit <= 1.0) {
        problems.push(format!("mma.move_limit must lie in (0, 1], got {}", repr(&o["move_limit"])));
    }
    if !(f("epsimin")? > 0.0) {
        problems.push(format!("mma.epsimin must be > 0, got {}", repr(&o["epsimin"])));
    }
    if py_int(&o["max_inner"])? < 1 {
        problems.push(format!("mma.max_inner must be >= 1, got {}", repr(&o["max_inner"])));
    }
    if !o["c"].is_null() && !(f("c")? > 0.0) {
        problems.push(format!("mma.c must be > 0 or null, got {}", repr(&o["c"])));
    }
    let (lo, init, hi) = (f("asymptote_lo")?, f("asymptote_init")?, f("asymptote_hi")?);
    if !(0.0 < lo && lo < init && init <= hi) {
        problems.push(format!(
            "mma asymptotes must satisfy 0 < asymptote_lo < asymptote_init <= asymptote_hi, got {} < {} <= {}",
            repr(&o["asymptote_lo"]),
            repr(&o["asymptote_init"]),
            repr(&o["asymptote_hi"])
        ));
    }
    if py_int(&o["max_conservative"])? < 0 {
        problems.push(format!("mma.max_conservative must be >= 0, got {}", repr(&o["max_conservative"])));
    }
    if !o["kkt_tol"].is_null() && !(f("kkt_tol")? > 0.0) {
        problems.push(format!("mma.kkt_tol must be > 0 or null, got {}", repr(&o["kkt_tol"])));
    }
    if !(f("feas_tol")? >= 0.0) {
        problems.push(format!("mma.feas_tol must be >= 0, got {}", repr(&o["feas_tol"])));
    }
    if !problems.is_empty() {
        return opt(problems);
    }
    Ok(o)
}


pub fn settings(over: &Map<String, Value>) -> JobResult<Map<String, Value>> {
    let defaults = defaults();
    let mut bad: Vec<String> = over.keys().filter(|k| !defaults.contains_key(*k)).cloned().collect();
    if !bad.is_empty() {
        bad.sort();
        return opt1(format!(
            "unknown setting(s) {}; this node takes {}",
            list_repr(&bad),
            list_repr(&sorted_keys(&defaults))
        ));
    }
    let mut s = defaults;
    for (k, v) in over {
        s.insert(k.clone(), v.clone());
    }
    let in_set = |v: &Value, set: &[&str]| v.as_str().is_some_and(|x| set.contains(&x));
    if !in_set(&s["scaling"], &SCALINGS) {
        return opt1(format!(
            "scaling must be one of {}, got {}",
            str_tuple_repr(&SCALINGS),
            repr(&s["scaling"])
        ));
    }
    if !s["optimizer"].is_null() && !in_set(&s["optimizer"], &OPTIMIZERS) {
        return opt1(format!(
            "optimizer must be one of {} or null (null picks 'mma' when constraints are declared and 'adam' \
             otherwise), got {}",
            str_tuple_repr(&OPTIMIZERS),
            repr(&s["optimizer"])
        ));
    }
    if !s["mma"].is_null() {
        mma_options(&s["mma"])?;
    }
    if !in_set(&s["momentum"], &["keep", "reset"]) {
        return opt1(format!("momentum must be 'keep' or 'reset', got {}", repr(&s["momentum"])));
    }
    if py_int(&s["iters"])? < 1 {
        return opt1(format!("iters must be >= 1, got {}", repr(&s["iters"])));
    }
    if !(py_float(&s["lr"])? > 0.0) {
        return opt1(format!("lr must be > 0, got {}", repr(&s["lr"])));
    }
    if !s["occupancy"].is_null() && !in_set(&s["occupancy"], &OCCUPANCY_PROFILES) {
        return opt1(format!(
            "occupancy must be one of {} or null, got {}",
            str_tuple_repr(&OCCUPANCY_PROFILES),
            repr(&s["occupancy"])
        ));
    }
    if !s["physics"].is_null() && s["physics"].as_str().is_none_or(str::is_empty) {
        return opt1(format!("physics must name a physics backend, got {}", repr(&s["physics"])));
    }
    let retries = &s["nonfinite_retries"];
    if !implexity_optim::pyval::is_int(retries) || retries.as_i64().is_some_and(|v| v < 0) {
        return opt1(format!("nonfinite_retries must be an integer >= 0, got {}", repr(retries)));
    }
    Ok(s)
}

#[must_use]
pub fn digest(obj: &Value) -> String {
    let text = implexity_core::json::canonical(obj);
    implexity_core::json::sha256_hex(text.as_bytes())[..16].to_string()
}

fn binding_problems(e: implexity_authoring::error::AuthoringError) -> JobError {
    if e.class() == "BindingError" { JobError::optimize(e.problem_list()) } else { e.into() }
}

fn contributions() -> &'static implexity_core::contributions::ContributionRegistry {
    &implexity_core::registries::global().contributions
}

#[derive(Clone)]
pub struct OptimizeSpec {
    pub model: NodeRef,
    pub settings: Map<String, Value>,
    pub physics: Arc<dyn ImplicitPhysics>,
    pub case: Value,
    pub norm: Value,
    pub case_warnings: Vec<String>,
    pub bbox: Map<String, Value>,
    pub free: Vec<Free>,
    pub constraints: Vec<Constraint>,
    pub objective: Value,
    pub occupancy: String,
    pub geometry_classification: Classification,
}

impl std::fmt::Debug for OptimizeSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptimizeSpec")
            .field("free", &self.free)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

pub(crate) fn box_grid(bbox: &Map<String, Value>) -> JobResult<[usize; 3]> {
    let g: Vec<usize> = bbox
        .get("grid")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| py_int(v).ok()).filter_map(|v| usize::try_from(v).ok()).collect())
        .unwrap_or_default();
    <[usize; 3]>::try_from(g.as_slice())
        .map_err(|_| JobError::optimize1("the physics analysis box has no 3-D grid"))
}

impl OptimizeSpec {

    #[allow(clippy::too_many_lines)]
    pub fn new(
        model: &NodeRef,
        free: &[Value],
        objective: &[Value],
        constraints: &[Value],
        case: &Value,
        settings_in: &Map<String, Value>,
    ) -> JobResult<Self> {
        let reg = contributions();
        let mut settings = settings(settings_in)?;
        let physics =
            physics_binding::resolve(reg, settings["physics"].as_str()).map_err(binding_problems)?;
        settings.insert("physics".into(), Value::from(physics.name()));
        if !case.is_object() {
            return opt1(format!(
                "an Optimize node needs a case document of its physics package ({}): without one there is no load, \
                 no boundary condition and no material for the objective to be about",
                if physics.label().is_empty() { physics.name() } else { physics.label() }
            ));
        }
        let (norm, case_warnings) = physics.validate_case(case).map_err(|e| {
            if e.class() == "BindingError" {
                JobError::optimize(
                    e.problem_list().into_iter().map(|p| format!("case rejected: {p}")).collect(),
                )
            } else {
                e.into()
            }
        })?;
        let grid: Vec<usize> = match &settings["grid"] {
            Value::Null => Vec::new(),
            v => vec![usize::try_from(py_int(v)?).map_err(|_| JobError::optimize1("grid must be positive"))?],
        };
        let effective = physics.effective_case(&norm, &grid).map_err(binding_problems)?;
        let bbox = physics
            .analysis_box(&effective)
            .map_err(binding_problems)?
            .as_object()
            .cloned()
            .ok_or_else(|| JobError::optimize1("the physics analysis box is not an object"))?;
        let mut free_list = resolve_free(model, free)?;
        if free_list.iter().any(|f| f.document_parameter.is_some()) {
            let context = settings.get("document_parameter_context").filter(|v| v["schema"] == "implexity-document-parameter-optimization/1").ok_or_else(|| JobError::value("named coordinates require an authoritative document context"))?;
            let document = implexity_geometry::document::build(&context["document"], context["base_dir"].as_str().map(std::path::Path::new), None)?;
            let root = document.node(context["node"].as_str().ok_or_else(|| JobError::value("named coordinate root is missing"))?)?;
            for coordinate in &free_list {
                if let Some(name) = &coordinate.document_parameter {
                    if document.values().get(name).copied() != coordinate.start.first().copied() || document.doc["parameters"][name]["units"].as_str().unwrap_or("-") != coordinate.units { return Err(JobError::value("named coordinate units or initial value differ from the document")); }
                    let consumers = document.parameter_consumers(&root, name)?;
                    if consumers.is_empty() { return Err(JobError::value("named coordinate has no continuous consumers")); }
                    for other in free_list.iter().filter(|f| f.document_parameter.is_none()) {
                        if consumers.iter().any(|(reference,_)| reference == &other.child_ref()) { return Err(JobError::value("direct and named coordinates overlap")); }
                    }
                }
            }
        }
        let term_names: Vec<String> = implexity_core::objective_terms::terms(reg, Some(physics.name()))
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let responses: BTreeMap<String, (String, String)> =
            physics.responses().into_iter().map(|[rid, aux_key, unit, _]| (rid, (aux_key, unit))).collect();
        let constraints = parse_constraints(constraints, &mut free_list, &responses, &term_names)?;
        let mut terms: Vec<Value> =
            objective.iter().map(|t| if t.is_object() { t.clone() } else { json!({"term": t}) }).collect();
        if terms.is_empty() {
            let mut sorted = term_names.clone();
            sorted.sort();
            return opt1(format!("objective is empty; the active terms are {}", list_repr(&sorted)));
        }
        let mut named: std::collections::BTreeSet<String> =
            terms.iter().map(|t| t.get("term").map(py_str).unwrap_or_default()).collect();
        for c in &constraints {
            if let Constraint::ResponseBound { response, .. } = c
                && !responses.contains_key(response)
                && !named.contains(response)
            {
                terms.push(json!({"term": response, "weight": 0.0}));
                named.insert(response.clone());
            }
        }
        let mut probs = Vec::new();
        let block = json!({"terms": terms});
        let objective = physics.validate_objective(&block, &mut probs);
        if !probs.is_empty() {
            return opt(probs);
        }
        let occupancy = match settings["occupancy"].as_str() {
            Some(o) => o.to_string(),
            None => {
                if free_list.iter().any(|f| f.size > 1) {
                    "logistic".into()
                } else {
                    "compact".into()
                }
            }
        };
        let mut spec = Self {
            model: Arc::clone(model),
            settings,
            physics,
            case: case.clone(),
            norm,
            case_warnings,
            bbox,
            free: free_list,
            constraints,
            objective,
            occupancy,
            geometry_classification: Classification::Direct(DirectOccupancyRepresentation::default()),
        };
        spec.geometry_classification = spec.bridge()?.check(model)?;
        Ok(spec)
    }


    pub fn bridge(&self) -> JobResult<MaskBridge> {
        let grid = box_grid(&self.bbox)?;
        let h_mm = py_float(self.bbox.get("h_mm").unwrap_or(&Value::Null))?;
        let origin = self.bbox.get("origin_mm").cloned().unwrap_or_else(|| json!([0, 0, 0]));
        let smooth_r = match &self.settings["smooth_r"] {
            Value::Null => None,
            v => Some(py_float(v)?),
        };
        MaskBridge::new(
            grid,
            h_mm * 1e-3,
            self.settings["model_units"].as_str().unwrap_or("mm"),
            py_float(&self.settings["band_h"])?,
            self.settings["eval_mode"].as_str().unwrap_or("smooth"),
            smooth_r,
            &origin,
            &self.occupancy,
        )
    }

    #[must_use]
    pub fn int_setting(&self, key: &str) -> i64 {
        self.settings.get(key).and_then(|v| py_int(v).ok()).unwrap_or(0)
    }

    #[must_use]
    pub fn digest(&self) -> String {
        digest(&json!({
            "free": self.free.iter().map(Free::describe).collect::<Vec<_>>(),
            "objective": self.objective,
            "constraints": self.constraints.iter().map(Constraint::digest).collect::<Vec<_>>(),
            "case": self.case,
            "settings": self.settings,
            "occupancy": self.occupancy,
        }))
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        let rep = self.geometry_classification.report();
        json!({
            "free": self.free.iter().map(Free::describe).collect::<Vec<_>>(),
            "objective": self.objective,
            "constraints": self.constraints.iter().map(Constraint::describe).collect::<Vec<_>>(),
            "settings": self.settings,
            "physics": self.physics.name(),
            "occupancy": self.occupancy,
            "case_name": self.bbox.get("name").cloned().unwrap_or(Value::Null),
            "grid": self.bbox.get("grid").cloned().unwrap_or(Value::Null),
            "h_mm": self.bbox.get("h_mm").cloned().unwrap_or(Value::Null),
            "model_kind": self.model.kind(),
            "model_structure_id": self.model.structure_id(),
            "model_content_id": self.model.content_id(),
            "field_class": rep["field_class"],
            "step_factor": rep["step_factor"],
            "geometry_representation": rep["geometry_representation"],
        })
    }

    #[must_use]
    pub fn term_names(&self) -> Vec<String> {
        self.objective
            .get("terms")
            .and_then(Value::as_array)
            .map(|t| t.iter().map(|x| x.get("term").map(py_str).unwrap_or_default()).collect())
            .unwrap_or_default()
    }
}

pub type Drive = IndexMap<String, ArrayD<f64>>;


pub fn model_drive_of(spec: &OptimizeSpec) -> JobResult<Drive> {
    let mut free: std::collections::BTreeSet<(Vec<String>, String)> =
        spec.free.iter().map(|f| (f.r.path.clone(), f.r.name.clone())).collect();
    if let Some(context) = spec.settings.get("document_parameter_context").filter(|v| !v.is_null()) {
        let document = implexity_geometry::document::build(&context["document"], context["base_dir"].as_str().map(std::path::Path::new), None)?;
        let root = document.node(context["node"].as_str().unwrap_or(""))?;
        for (path,node) in root.walk() {
            if let Some(id) = document.id_of(&node) { if let Some(bindings) = document.bindings().get(&id) {
                for name in bindings.keys() { let mut rooted = vec!["model".into()]; rooted.extend(path.clone()); free.insert((rooted,name.clone())); }
            } }
        }
    }
    let mut out = Drive::new();
    for (path, node) in spec.model.walk() {
        for name in node.info().sorted_param_names() {
            let mut p = vec!["model".to_string()];
            p.extend(path.iter().cloned());
            if free.contains(&(p.clone(), name.clone())) {
                continue;
            }
            out.insert(ParamRef::new(p, name.clone()).as_str(), param_array(&node, &name)?);
        }
    }
    Ok(out)
}

#[must_use]
pub fn drive_value(drive: &Drive) -> Value {
    Value::Object(drive.iter().map(|(k, v)| (k.clone(), safe(v))).collect())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SteerDelta {
    pub changes: Vec<String>,
    pub sets: BTreeMap<String, ArrayD<f64>>,
    pub hot: bool,
    pub refused: Option<String>,
}

impl SteerDelta {
    #[must_use]
    pub fn as_dict(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("changes".into(), json!(self.changes));
        m.insert("hot".into(), json!(self.hot));
        m.insert("refused".into(), json!(self.refused));
        m.insert("keys".into(), json!(self.sets.keys().collect::<Vec<_>>()));
        m
    }
}

#[must_use]
pub fn brief(a: &ArrayD<f64>) -> String {
    if a.ndim() == 0 {
        return fmt_g(a.iter().next().copied().unwrap_or(f64::NAN), 6);
    }
    format!("[{} values, mean {}]", a.len(), fmt_g(implexity_optim::numeric::array_mean(a), 6))
}

#[must_use]
pub fn classify_model_steer(spec: &OptimizeSpec, drive: &Drive, req: &Value) -> SteerDelta {
    let mut d = SteerDelta { hot: true, ..SteerDelta::default() };
    let sets = req.get("set").filter(|v| truthy(v)).cloned().unwrap_or(Value::Null);
    let Some(sets) = sets.as_object().filter(|s| !s.is_empty()) else {
        d.refused =
            Some("a model steer is {'set': {'model/<path>:<name>': value, ...}}; nothing to set".into());
        return d;
    };
    let free: std::collections::BTreeSet<String> = spec.free.iter().map(Free::ref_str).collect();
    let mut keys: Vec<&String> = sets.keys().collect();
    keys.sort();
    for key in keys {
        let val = &sets[key];
        if free.contains(key) {
            d.refused = Some(format!(
                "{key} is a DESIGN VARIABLE of this run: steering it would overwrite the iterate mid-descent, which \
                 is not a steer but a restart.  Freeze it (remove it from free) and steer it, or accept the run and \
                 start a new one."
            ));
            return d;
        }
        let Some(cur) = drive.get(key) else {
            let mut names: Vec<&String> = drive.keys().collect();
            names.sort();
            let head: Vec<&str> = names.iter().take(12).map(|s| s.as_str()).collect();
            d.refused = Some(format!(
                "no parameter {key} in this model; it carries {}{}",
                head.join(", "),
                if drive.len() > 12 { " ..." } else { "" }
            ));
            return d;
        };
        let Some(new) = json_array(val) else {
            d.refused = Some(format!("{key}: the steered value is not finite"));
            return d;
        };
        if new.shape() != cur.shape() {
            d.hot = false;
            d.refused = Some(format!(
                "{key} moves from shape {} to {}: an array's SHAPE is structural (it is what the compiled kernel was \
                 traced on), so this is a graph edit and not a steer.  Stop the job, accept or discard, and start a \
                 new one.",
                shape_tuple(cur.shape()),
                shape_tuple(new.shape())
            ));
            return d;
        }
        if !new.iter().all(|v| v.is_finite()) {
            d.refused = Some(format!("{key}: the steered value is not finite"));
            return d;
        }
        d.changes.push(format!("{key}: {} -> {}", brief(cur), brief(&new)));
        d.sets.insert(key.clone(), new);
    }
    d
}


pub fn select_optimizer(spec: &OptimizeSpec, constraints: &[Constraint]) -> JobResult<String> {
    let kind = match spec.settings.get("optimizer").and_then(Value::as_str) {
        Some(k) => k.to_string(),
        None => {
            if constraints.is_empty() {
                "adam".into()
            } else {
                "mma".into()
            }
        }
    };
    if !OPTIMIZERS.contains(&kind.as_str()) {
        return opt1(format!(
            "optimizer must be one of {}, got {}",
            str_tuple_repr(&OPTIMIZERS),
            repr_str(&kind)
        ));
    }
    Ok(kind)
}

