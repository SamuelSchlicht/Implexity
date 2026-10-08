// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use implexity_geometry::GeometryError;
use implexity_geometry::differentiable_drag::{
    DragPolicy, DragProblem, DragSession, DragSessionRegistry, solve_surface_drag,
};
use implexity_geometry::document::{self as D, Binding, Model};
use implexity_geometry::eval::{self as EV, EvalOptions};
use implexity_geometry::node::{Mode, Node, NodeRef, ParamRef};
use implexity_geometry::value::ParamValue;
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::interaction_runtime::{InteractionRuntime, RuntimeStores};
use crate::model_manager::ModelManager;
use crate::py::{Arr, jf, norm3, py_float, py_int, py_str, repr, truthy, uuid_hex};
use crate::sync::{ReentrantLock, lock};

pub const SCHEMA: &str = "implexity-direct-manipulation/1";
pub const MAX_CANDIDATES: usize = 24;
pub const MAX_DIRECT_REFS: usize = 96;

#[must_use]
pub fn manipulation_error(problems: Vec<String>) -> AuthoringError {
    AuthoringError::problems("ManipulationError", problems)
}

fn merr(message: impl Into<String>) -> AuthoringError {
    manipulation_error(vec![message.into()])
}

fn drag(e: implexity_geometry::differentiable_drag::DragError) -> AuthoringError {
    AuthoringError::value("DragError", e.0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub key: String,
    pub kind: String,
    pub value: f64,
    pub units: String,
    pub scale: f64,
    pub lower: f64,
    pub upper: f64,
    pub name: Option<String>,
    pub node: Option<String>,
    pub param: Option<String>,
    pub label: String,
    pub semantic: String,
    pub axis: Option<[f64; 3]>,
    pub direct_refs: Vec<ParamRef>,
    pub targets: Vec<(String, String)>,
}

impl Candidate {
    #[must_use]
    pub fn as_dict(&self, influence: Option<f64>) -> Value {
        let mut out = Map::new();
        out.insert("key".into(), json!(self.key));
        out.insert("kind".into(), json!(self.kind));
        out.insert("value".into(), jf(self.value));
        out.insert("units".into(), json!(self.units));
        out.insert("scale".into(), jf(self.scale));
        out.insert("min".into(), if self.lower.is_finite() { jf(self.lower) } else { Value::Null });
        out.insert("max".into(), if self.upper.is_finite() { jf(self.upper) } else { Value::Null });
        out.insert("label".into(), json!(if self.label.is_empty() { &self.key } else { &self.label }));
        out.insert("semantic".into(), json!(self.semantic));
        if let Some(n) = &self.name {
            out.insert("name".into(), json!(n));
        }
        if let Some(n) = &self.node {
            out.insert("node".into(), json!(n));
        }
        if let Some(p) = &self.param {
            out.insert("param".into(), json!(p));
        }
        if let Some(a) = self.axis {
            out.insert("axis".into(), json!(a.map(jf)));
        }
        if !self.targets.is_empty() {
            out.insert(
                "targets".into(),
                Value::Array(self.targets.iter().map(|(n, p)| json!({"node": n, "param": p})).collect()),
            );
        }
        if let Some(i) = influence {
            out.insert("influence".into(), jf(i));
        }
        Value::Object(out)
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub node_id: String,
    pub point: [f64; 3],
    pub normal: [f64; 3],
    pub spatial_gradient: [f64; 3],
    pub candidates: Vec<Candidate>,
    pub parameter_jacobian: Vec<f64>,
    pub base_document: Value,
    pub base_structure_id: String,
    pub base_content_id: String,
    pub last_content_id: String,
    pub mode: String,
    pub smooth_r_mm: f64,
    pub policy: DragPolicy,
    pub created_at: f64,
    pub stream: DragSession,
    pub last_values: Option<Vec<f64>>,
    pub last_result: Option<Value>,
    pub history_before: Option<Value>,
}

impl Session {
    #[must_use]
    pub fn initial_values(&self) -> Vec<f64> {
        self.candidates.iter().map(|c| c.value).collect()
    }
}

#[derive(Clone, Debug)]
struct UndoRecord {
    before: Value,
    after: Value,
    label: String,
}

pub struct SharedHistory<'a> {
    pub runtime: &'a InteractionRuntime,
    pub stores: &'a dyn RuntimeStores,
}

pub type Journal = Arc<dyn Fn(&str, &str, &Value) -> AResult<Value> + Send + Sync>;

#[derive(Default)]
struct MState {
    sessions: HashMap<String, Session>,
    active: Option<String>,
    reservation: Option<(String, String)>,
    undo: Vec<UndoRecord>,
    redo: Vec<UndoRecord>,
}

fn monotonic() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn unit_axis(param: &str) -> Option<[f64; 3]> {
    match param.to_lowercase().as_str() {
        "x" | "dx" | "x_mm" | "dx_mm" | "ax_mm" | "bx_mm" | "nx" => Some([1.0, 0.0, 0.0]),
        "y" | "dy" | "y_mm" | "dy_mm" | "ay_mm" | "by_mm" | "ny" => Some([0.0, 1.0, 0.0]),
        "z" | "dz" | "z_mm" | "dz_mm" | "az_mm" | "bz_mm" | "nz" => Some([0.0, 0.0, 1.0]),
        _ => None,
    }
}

#[must_use]
pub fn semantic(param: &str) -> (String, Option<[f64; 3]>) {
    let p = param.to_lowercase();
    let axis = unit_axis(&p);
    let s = |x: &str| x.to_string();
    if matches!(p.as_str(), "dx_mm" | "dy_mm" | "dz_mm") {
        return (s("translate"), axis);
    }
    if matches!(p.as_str(), "ax_mm" | "ay_mm" | "az_mm") {
        return (s("endpoint-a"), axis);
    }
    if matches!(p.as_str(), "bx_mm" | "by_mm" | "bz_mm") {
        return (s("endpoint-b"), axis);
    }
    if p.contains("radius") || matches!(p.as_str(), "major_mm" | "minor_mm" | "r1_mm" | "r2_mm") {
        return (s("radius"), None);
    }
    if p.contains("thickness") || matches!(p.as_str(), "offset_mm" | "distance_mm") {
        return (s("normal-offset"), None);
    }
    if p.starts_with("half_height") || matches!(p.as_str(), "height_mm" | "length_mm") {
        return (s("length"), Some([0.0, 0.0, 1.0]));
    }
    if matches!(p.as_str(), "period_mm" | "pitch_mm" | "cell_mm") {
        return (s("period"), None);
    }
    if axis.is_some() {
        return (s("axis-coordinate"), axis);
    }
    (s("surface"), None)
}

#[must_use]
pub fn finite_scalar(value: Option<&ParamValue>) -> Option<f64> {
    let x = match value? {
        ParamValue::Float(x) => *x,
        #[allow(clippy::cast_precision_loss)]
        ParamValue::Int(i) => *i as f64,
        ParamValue::Array(a)
            if a.ndim() == 0 && !matches!(a.data(), implexity_geometry::value::ArrayData::Bool(_)) =>
        {
            a.to_f64_vec().first().copied()?
        }
        _ => return None,
    };
    x.is_finite().then_some(x)
}

fn finite_scalar_json(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

#[must_use]
pub fn direct_bounds(param: &str, value: f64, extent_diagonal: f64) -> (f64, f64, f64) {
    let p = param.to_lowercase();
    let positive = p.contains("radius")
        || p.contains("thickness")
        || p.contains("height")
        || matches!(
            p.as_str(),
            "bx_mm"
                | "by_mm"
                | "bz_mm"
                | "major_mm"
                | "minor_mm"
                | "r1_mm"
                | "r2_mm"
                | "period_mm"
                | "pitch_mm"
                | "length_mm"
        );
    let scale = value.abs().max(extent_diagonal.max(1.0) * 0.10).max(1.0e-3);
    let lo = if positive { (value - 4.0 * scale).max(1.0e-9) } else { value - 4.0 * scale };
    let mut hi = value + 4.0 * scale;
    if positive {
        hi = hi.max(value * 4.0 + scale);
    }
    (lo, hi, scale)
}

fn paths_by_node(model: &Model, root: &NodeRef) -> BTreeMap<String, Vec<Vec<String>>> {
    let mut out: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for (path, node) in root.walk() {
        if let Some(nid) = model.id_of(&node) {
            out.entry(nid).or_default().push(path);
        }
    }
    out
}

fn node_at_path(root: &NodeRef, path: &[String]) -> AResult<NodeRef> {
    let mut node = Arc::clone(root);
    for name in path {
        let hit = node.named_children().find(|(n, _)| *n == name).map(|(_, c)| Arc::clone(c));
        match hit {
            Some(h) => node = h,
            None => {
                return Err(merr(format!(
                    "parameter path {} no longer exists",
                    repr(&json!(path.join("/")))
                )));
            }
        }
    }
    Ok(node)
}

fn ref_value(root: &NodeRef, r: &ParamRef) -> AResult<f64> {
    let node = node_at_path(root, &r.path)?;
    finite_scalar(node.param(&r.name))
        .ok_or_else(|| merr(format!("{} is not a finite scalar parameter", r.as_str())))
}

fn normalise_normal(g: [f64; 3]) -> AResult<[f64; 3]> {
    let n = norm3(g);
    if !n.is_finite() || n <= 1.0e-12 {
        return Err(merr("the picked point has no stable surface normal"));
    }
    Ok([g[0] / n, g[1] / n, g[2] / n])
}

fn vec3_of(value: Option<&Value>) -> Option<[f64; 3]> {
    let a = Arr::from_opt(value).ok()?;
    (a.data.len() == 3 && a.data.iter().all(|v| v.is_finite())).then(|| [a.data[0], a.data[1], a.data[2]])
}

fn discrete(node: &Node) -> &[String] {
    &node.info().discrete
}

fn expr_mentions(binding: &Binding, name: &str) -> bool {
    match binding {
        Binding::Bind { name: n, .. } => n == name,
        Binding::Expr { tree, .. } => D::expr_names(tree).contains(name),
        Binding::Array { .. } => false,
    }
}

pub struct ManipulationManager {
    models: Arc<ModelManager>,
    journal: Option<Journal>,
    lock: ReentrantLock,
    state: Mutex<MState>,
}

impl ManipulationManager {
    #[must_use]
    pub fn new(models: Arc<ModelManager>, journal: Option<Journal>) -> Self {
        Self { models, journal, lock: ReentrantLock::new(), state: Mutex::new(MState::default()) }
    }

    #[must_use]
    pub fn models(&self) -> &Arc<ModelManager> {
        &self.models
    }

    fn live_optimisation_active(&self) -> bool {
        self.models.live_optimisation().is_some_and(|v| truthy(&v))
    }


    pub fn status(&self, history: Option<&SharedHistory<'_>>) -> AResult<Value> {
        let _g = self.lock.acquire();
        let (active, reserved, n_undo, n_redo) = {
            let st = lock(&self.state);
            let active = st.active.as_ref().and_then(|a| st.sessions.get(a)).map(Self::session_summary);
            let reserved = st.reservation.as_ref().map(|r| json!({"operation": r.1}));
            (active, reserved, st.undo.len(), st.redo.len())
        };
        let hist = match history {
            Some(h) => Some(h.runtime.history_state(h.stores)?),
            None => None,
        };
        Ok(json!({
            "kind": "implicit_direct_manipulation", "schema": SCHEMA,
            "active": active, "reserved": reserved,
            "undo": hist.as_ref().map_or(json!(n_undo), |h| h["undo"].clone()),
            "redo": hist.as_ref().map_or(json!(n_redo), |h| h["redo"].clone()),
            "history": hist,
            "operations": ["begin", "preview", "commit", "cancel", "undo", "redo"],
            "interaction": ["semantic-handle", "differentiable-surface"],
            "note": "the cursor changes authored implicit-model parameters; display meshes are never edited",
        }))
    }


    pub fn observe_geometry_authority<T>(&self, callback: impl FnOnce() -> AResult<T>) -> AResult<T> {
        let _g = self.lock.acquire();
        callback()
    }

    #[must_use]
    pub fn has_active_geometry_writer(&self) -> bool {
        let _g = self.lock.acquire();
        lock(&self.state).active.is_some() || self.live_optimisation_active()
    }


    pub fn assert_idle(&self, operation: &str) -> AResult<()> {
        let _g = self.lock.acquire();
        if self.live_optimisation_active() {
            return Err(merr(format!(
                "{operation} is locked by an optimization; enter manual intervention or discard first"
            )));
        }
        let st = lock(&self.state);
        if st.active.is_some() {
            return Err(merr(format!(
                "{operation} is unavailable while a direct-manipulation gesture is active; commit or cancel the gesture first"
            )));
        }
        if let Some(r) = &st.reservation {
            return Err(merr(format!(
                "{operation} is unavailable while {} is reserving model authority",
                r.1
            )));
        }
        Ok(())
    }


    pub fn reserve_idle(&self, operation: &str, allow_optimization_job_id: Option<&str>) -> AResult<String> {
        let _g = self.lock.acquire();
        let live = self.models.live_optimisation();
        let owns_live = live.as_ref().and_then(Value::as_object).is_some_and(|l| {
            match (l.get("job_id").filter(|v| !v.is_null()), allow_optimization_job_id) {
                (None, None) => true,
                (Some(j), Some(a)) => j.as_str() == Some(a),
                _ => false,
            }
        });
        if live.as_ref().is_some_and(truthy) && !owns_live {
            return Err(merr(format!(
                "{operation} is locked by an optimization; enter manual intervention or discard first"
            )));
        }
        let mut st = lock(&self.state);
        if st.active.is_some() {
            return Err(merr(format!(
                "{operation} is unavailable while a direct-manipulation gesture is active; commit or cancel the gesture first"
            )));
        }
        if let Some(r) = &st.reservation {
            return Err(merr(format!(
                "{operation} is unavailable while {} is reserving model authority",
                r.1
            )));
        }
        let token = uuid_hex();
        st.reservation = Some((token.clone(), operation.to_string()));
        Ok(token)
    }


    pub fn release_reservation(&self, token: &str) -> AResult<()> {
        let _g = self.lock.acquire();
        let mut st = lock(&self.state);
        if st.reservation.as_ref().is_none_or(|r| r.0 != token) {
            return Err(merr("model-authority reservation is absent or owned by another operation"));
        }
        st.reservation = None;
        Ok(())
    }


    pub fn assert_reservation(&self, token: &str) -> AResult<()> {
        let _g = self.lock.acquire();
        if lock(&self.state).reservation.as_ref().is_none_or(|r| r.0 != token) {
            return Err(merr("model-authority reservation is absent or owned by another operation"));
        }
        Ok(())
    }


    pub fn run_reserved_idle<T>(
        &self,
        operation: &str,
        allow_optimization_job_id: Option<&str>,
        callback: impl FnOnce() -> AResult<T>,
    ) -> AResult<T> {
        let token = self.reserve_idle(operation, allow_optimization_job_id)?;
        let result = self.assert_reservation(&token).and_then(|()| callback());
        let released = self.release_reservation(&token);
        let value = result?;
        released?;
        Ok(value)
    }


    #[allow(clippy::too_many_lines)]
    pub fn begin(&self, request: &Value, history: Option<&SharedHistory<'_>>) -> AResult<Value> {
        if !request.is_object() {
            return Err(merr("the manipulation begin request must be an object"));
        }
        let _g = self.lock.acquire();
        {
            let st = lock(&self.state);
            if let Some(r) = &st.reservation {
                return Err(merr(format!(
                    "direct manipulation cannot begin while {} is reserving model authority",
                    r.1
                )));
            }
            if st.active.is_some() {
                return Err(merr("a direct-manipulation gesture is already active"));
            }
        }
        let model = self.models.require()?;
        if self.live_optimisation_active() {
            return Err(merr(
                "direct manipulation cannot begin while an optimisation is pushing live iterates",
            ));
        }
        let node_id = [request.get("node"), model.doc.get("root")]
            .into_iter()
            .flatten()
            .find(|v| truthy(v))
            .map(py_str)
            .unwrap_or_default();
        let Some(root) = model.node_table().get(&node_id).cloned() else {
            return Err(merr(format!("no model node {}", repr(&json!(node_id)))));
        };
        let point = vec3_of(request.get("point_mm"))
            .ok_or_else(|| merr("point_mm must contain three finite world coordinates"))?;
        let mode = request.get("mode").filter(|v| truthy(v)).map_or_else(|| "smooth".to_string(), py_str);
        if mode != "exact" && mode != "smooth" {
            return Err(merr("mode must be exact or smooth"));
        }
        let smooth_r = match request.get("smooth_r_mm") {
            None => 0.35,
            Some(v) => py_float(v)?,
        };
        if !smooth_r.is_finite() || smooth_r <= 0.0 {
            return Err(merr("smooth_r_mm must be finite and positive"));
        }
        let mut candidates = Self::candidates(&model, &root, &node_id, request.get("parameters"))?;
        if candidates.is_empty() {
            return Err(merr(
                "no editable scalar parameter influences the selected subtree; bind a named parameter or select a parametric node",
            ));
        }
        let (spatial, mut jac, method) =
            self.linearisation(&model, &root, point, &candidates, &mode, smooth_r)?;
        let mut normal = normalise_normal(spatial)?;
        if let Some(supplied) = request.get("normal").filter(|v| !v.is_null()) {
            let ns =
                vec3_of(Some(supplied)).ok_or_else(|| merr("normal must contain three finite values"))?;
            let ns = normalise_normal(ns)?;
            if crate::py::dot3(ns, normal) < 0.0 {
                normal = normal.map(|v| -v);
            }
        }
        let mut influence: Vec<f64> = jac.iter().zip(&candidates).map(|(j, c)| j.abs() * c.scale).collect();
        let total = crate::py::np_sum(&influence);
        if total > 0.0 {
            for x in &mut influence {
                *x /= total;
            }
        }
        let keep: Vec<usize> =
            influence.iter().enumerate().filter(|(_, x)| **x > 1.0e-10).map(|(i, _)| i).collect();
        if keep.is_empty() {
            return Err(merr(
                "the selected parameters have zero local influence at the picked surface point",
            ));
        }
        if keep.len() != candidates.len() {
            candidates = keep.iter().map(|i| candidates[*i].clone()).collect();
            jac = keep.iter().map(|i| jac[*i]).collect();
            influence = keep.iter().map(|i| influence[*i]).collect();
            let s = crate::py::np_sum(&influence).max(1.0e-30);
            for x in &mut influence {
                *x /= s;
            }
        }
        let policy = DragPolicy {
            damping: request.get("damping").map_or(Ok(1.0e-8), py_float)?,
            trust_radius: request.get("trust_radius").map_or(Ok(0.20), py_float)?,
            max_component_step: request.get("max_component_step").map_or(Ok(0.25), py_float)?,
            feasibility_tolerance: request.get("feasibility_tolerance").map_or(Ok(2.0e-3), py_float)?,
            ..DragPolicy::default()
        };
        let sid = uuid_hex();
        let initial: Vec<f64> = candidates.iter().map(|c| c.value).collect();
        let content = model.content_id().unwrap_or_else(|| "None".into());
        let stream = DragSessionRegistry::new().begin(&sid, &content, &initial).map_err(drag)?;
        let base_document = self.models.snapshot()?;
        let mut session = Session {
            id: sid.clone(),
            node_id: node_id.clone(),
            point,
            normal,
            spatial_gradient: spatial,
            candidates: candidates.clone(),
            parameter_jacobian: jac,
            base_document,
            base_structure_id: model.structure_id().unwrap_or_else(|| "None".into()),
            base_content_id: content.clone(),
            last_content_id: content,
            mode,
            smooth_r_mm: smooth_r,
            policy,
            created_at: monotonic(),
            stream,
            last_values: Some(initial.clone()),
            last_result: None,
            history_before: None,
        };
        if let Some(h) = history {
            if h.runtime.transactions.active_count() > 0 {
                return Err(merr("finish or cancel the active viewport transaction first"));
            }
            session.history_before = Some(InteractionRuntime::bundle(h.stores)?);
        }
        {
            let mut st = lock(&self.state);
            st.sessions.insert(sid.clone(), session);
            st.active = Some(sid.clone());
        }
        Ok(json!({
            "kind": "implicit_drag_session", "schema": SCHEMA, "session": sid, "node": node_id,
            "point_mm": point.map(jf), "normal": normal.map(jf), "spatial_gradient": spatial.map(jf),
            "derivative_method": method,
            "parameters": candidates.iter().enumerate().map(|(i, c)| c.as_dict(Some(influence[i]))).collect::<Vec<_>>(),
            "model": {"structure_id": model.structure_id(), "content_id": model.content_id()},
            "preview": {"sequence": -1, "values": initial.iter().map(|v| jf(*v)).collect::<Vec<_>>()},
        }))
    }

    fn get_active(&self, sid: &str) -> AResult<Session> {
        let st = lock(&self.state);
        match (st.active.as_deref(), st.sessions.get(sid)) {
            (Some(a), Some(s)) if !sid.is_empty() && a == sid => Ok(s.clone()),
            _ => Err(merr("unknown or inactive direct-manipulation session")),
        }
    }

    fn put_session(&self, s: Session) {
        lock(&self.state).sessions.insert(s.id.clone(), s);
    }

    fn close(&self, sid: &str) {
        let mut st = lock(&self.state);
        st.sessions.remove(sid);
        if st.active.as_deref() == Some(sid) {
            st.active = None;
        }
    }

    fn value_map(s: &Session, values: &[f64]) -> Value {
        Value::Object(s.candidates.iter().zip(values).map(|(c, v)| (c.key.clone(), jf(*v))).collect())
    }

    fn stale_preview(s: &Session, sequence: i64) -> Value {
        let values = s.last_values.clone().unwrap_or_else(|| s.initial_values());
        json!({"kind": "implicit_drag_preview", "schema": SCHEMA, "session": s.id, "sequence": sequence,
            "accepted": false, "stale": true, "latest": s.stream.accepted_sequence,
            "values": Self::value_map(s, &values)})
    }

    fn session_summary(s: &Session) -> Value {
        json!({"session": s.id, "node": s.node_id, "created_at": jf(s.created_at),
            "base_content_id": s.base_content_id, "current_content_id": s.last_content_id,
            "sequence": s.stream.accepted_sequence,
            "parameters": s.candidates.iter().map(|c| c.key.clone()).collect::<Vec<_>>()})
    }

    fn check_identity(&self, s: &Session) -> AResult<()> {
        let model = self.models.require()?;
        if model.structure_id().unwrap_or_else(|| "None".into()) != s.base_structure_id {
            return Err(merr("model structure changed during the drag"));
        }
        if model.content_id().unwrap_or_else(|| "None".into()) != s.last_content_id {
            return Err(merr("model content changed outside the active drag"));
        }
        Ok(())
    }

    fn drag_problem(
        s: &Session,
        displacement: [f64; 3],
        locked: Vec<bool>,
        weights: Option<Vec<f64>>,
    ) -> DragProblem {
        DragProblem {
            spatial_gradient: vec![s.spatial_gradient.to_vec()],
            parameter_jacobian: vec![s.parameter_jacobian.clone()],
            desired_displacement: vec![displacement.to_vec()],
            parameter_values: s.initial_values(),
            parameter_scales: s.candidates.iter().map(|c| c.scale).collect(),
            lower_bounds: Some(s.candidates.iter().map(|c| c.lower).collect()),
            upper_bounds: Some(s.candidates.iter().map(|c| c.upper).collect()),
            locked: Some(locked),
            parameter_weights: weights,
            sample_weights: None,
        }
    }


    pub fn preview(&self, request: &Value) -> AResult<Value> {
        if !request.is_object() {
            return Err(merr("the manipulation preview request must be an object"));
        }
        let sid = request.get("session").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        let sequence = request.get("sequence").map_or(Ok(-1), py_int)?;
        let _g = self.lock.acquire();
        let mut s = self.get_active(&sid)?;
        if !s.stream.mark_requested(sequence).map_err(drag)? {
            self.put_session(s.clone());
            return Ok(Self::stale_preview(&s, sequence));
        }
        self.put_session(s.clone());
        self.check_identity(&s)?;
        let displacement = Self::requested_displacement(request, &s)?;
        let (values, report) = if let Some(key) = request.get("semantic_parameter").filter(|v| truthy(v)) {
            Self::semantic_preview(&s, &py_str(key), request, displacement)?
        } else {
            let n = s.candidates.len();
            let locked: Vec<bool> = match request.get("locked") {
                None => vec![false; n],
                Some(v) => v.as_array().map(|a| a.iter().map(truthy).collect()).unwrap_or_default(),
            };
            let weights = match request.get("parameter_weights") {
                None | Some(Value::Null) => None,
                Some(v) => Some(Arr::from_json(v)?.data),
            };
            let pb = Self::drag_problem(&s, displacement, locked, weights);
            let result = solve_surface_drag(&pb, Some(s.policy)).map_err(drag)?;
            let init = s.initial_values();
            let values: Vec<f64> = init.iter().zip(&result.delta_native).map(|(a, b)| a + b).collect();
            (values, result.as_dict())
        };
        if sequence < s.stream.pending_sequence {
            return Ok(Self::stale_preview(&s, sequence));
        }
        let applied = self.apply(&s, &values)?;
        if !s.stream.accept_preview(sequence, &values).map_err(drag)? {
            self.put_session(s.clone());
            return Ok(Self::stale_preview(&s, sequence));
        }
        s.last_values = Some(values.clone());
        s.last_content_id = self.models.require()?.content_id().unwrap_or_else(|| "None".into());
        s.last_result = Some(report.clone());
        self.put_session(s.clone());
        Ok(json!({
            "kind": "implicit_drag_preview", "schema": SCHEMA, "session": s.id, "sequence": sequence,
            "accepted": true, "values": Self::value_map(&s, &values),
            "model": {"structure_id": s.base_structure_id, "content_id": s.last_content_id},
            "moved": applied.get("moved").cloned().unwrap_or_else(|| json!([])),
            "result": report,
        }))
    }


    pub fn guidance(&self, request: &Value) -> AResult<Value> {
        if !request.is_object() {
            return Err(merr("the manipulation guidance request must be an object"));
        }
        let sid = request.get("session").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        let _g = self.lock.acquire();
        let s = self.get_active(&sid)?;
        let raw = [request.get("parameter_gradients"), request.get("parameterGradients")]
            .into_iter()
            .flatten()
            .find(|v| truthy(v))
            .and_then(Value::as_object)
            .ok_or_else(|| merr("parameter gradients are required for adjoint-guided manual preview"))?;
        let mut g = vec![0.0; s.candidates.len()];
        let mut missing: Vec<String> = Vec::new();
        for (i, c) in s.candidates.iter().enumerate() {
            let mut aliases = vec![c.key.clone()];
            if let Some(n) = c.name.as_ref().filter(|n| !n.is_empty()) {
                aliases.push(n.clone());
            }
            if let (Some(n), Some(p)) =
                (c.node.as_ref().filter(|n| !n.is_empty()), c.param.as_ref().filter(|p| !p.is_empty()))
            {
                aliases.push(format!("{n}:{p}"));
            }
            let found = aliases.iter().find_map(|k| raw.get(k)).filter(|v| !v.is_null());
            let Some(found) = found else {
                missing.push(c.key.clone());
                continue;
            };
            g[i] = py_float(found).map_err(|_| merr(format!("gradient for {} must be numeric", c.key)))?;
            if !g[i].is_finite() {
                return Err(merr(format!("gradient for {} must be finite", c.key)));
            }
        }
        let require_all =
            request.get("require_all").or_else(|| request.get("requireAll")).is_some_and(truthy);
        if !missing.is_empty() && require_all {
            return Err(merr(format!("missing response gradients for {}", missing.join(", "))));
        }
        let h = match request.get("probe_mm").or_else(|| request.get("probeMm")) {
            None => 1.0e-4,
            Some(v) => py_float(v)?,
        };
        if !h.is_finite() || h <= 0.0 {
            return Err(merr("guidance probe must be positive"));
        }
        let mut deriv = Vec::new();
        let mut motion = Vec::new();
        for axis in 0..3 {
            let mut d = [0.0; 3];
            d[axis] = h;
            let pb = Self::drag_problem(&s, d, vec![false; s.candidates.len()], None);
            let result = solve_surface_drag(&pb, Some(s.policy)).map_err(drag)?;
            let dp = &result.delta_native;
            let dot: f64 = crate::py::np_dot(&g, dp);
            deriv.push(jf(dot / h));
            motion.push(Value::Array(dp.iter().map(|v| jf(v / h)).collect()));
        }
        Ok(json!({"kind": "implicit_drag_guidance", "schema": SCHEMA, "session": s.id,
            "response": request.get("response").filter(|v| truthy(v)).map_or_else(|| "response".to_string(), py_str),
            "predicted_change_per_mm_world": deriv, "parameter_motion_per_mm_world": motion,
            "missing_gradients": missing,
            "note": "first-order adjoint projection only; commit triggers fresh physics before optimization resumes"}))
    }


    #[allow(clippy::too_many_lines)]
    pub fn commit(&self, request: &Value, history: Option<&SharedHistory<'_>>) -> AResult<Value> {
        let sid = request.get("session").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        let _g = self.lock.acquire();
        let mut s = self.get_active(&sid)?;
        self.check_identity(&s)?;
        let final_sequence = match request.get("final_sequence") {
            None | Some(Value::Null) => None,
            Some(v) => Some(py_int(v)?),
        };
        let values = s.stream.commit(&s.base_content_id, final_sequence).map_err(drag)?;
        self.put_session(s.clone());
        if s.last_values.as_ref() != Some(&values) {
            self.apply(&s, &values)?;
        }
        let attempt = (|| -> AResult<(bool, Option<UndoRecord>, Value, i64, Value)> {
            let after = self.models.snapshot()?;
            let changed = D::dumps(&after) != D::dumps(&s.base_document);
            let record = changed.then(|| UndoRecord {
                before: s.base_document.clone(),
                after: after.clone(),
                label: format!("direct manipulation of {}", s.node_id),
            });
            let value_report = Self::value_map(&s, &values);
            let final_report = s.stream.accepted_sequence;
            let status = self.models.persist_live()?;
            Ok((changed, record, value_report, final_report, status))
        })();
        let (changed, record, value_report, final_report, status) = match attempt {
            Ok(r) => r,
            Err(persist_exc) => {
                s.stream.committed = false;
                if let Err(restore_exc) = self.models.replace_live(&s.base_document, false) {
                    self.put_session(s);
                    return Err(manipulation_error(vec![
                        format!(
                            "direct-manipulation persistence failed: {}: {}",
                            persist_exc.class(),
                            persist_exc
                        ),
                        format!(
                            "the active gesture retains authority, but restoring its exact base document also failed: {}: {}",
                            restore_exc.class(),
                            restore_exc
                        ),
                    ]));
                }
                s.last_content_id.clone_from(&s.base_content_id);
                s.last_values = Some(s.initial_values());
                s.last_result = None;
                self.put_session(s);
                return Err(merr(format!(
                    "direct-manipulation persistence failed; the exact base model was restored and this gesture remains active and retryable: {}: {}",
                    persist_exc.class(),
                    persist_exc
                )));
            }
        };
        self.close(&s.id);
        let mut shared_result: Option<Value> = None;
        if changed {
            if let (Some(h), Some(before), Some(rec)) = (history, s.history_before.as_ref(), record.as_ref())
            {
                shared_result = Some(h.runtime.record_external(
                    h.stores,
                    before,
                    &rec.label,
                    "direct_parameter_gesture",
                )?);
            }
            if history.is_none()
                && let Some(rec) = record.clone()
            {
                let mut st = lock(&self.state);
                st.undo.push(rec);
                let n = st.undo.len();
                if n > 100 {
                    st.undo.drain(..n - 100);
                }
                st.redo.clear();
            }
            if let Some(j) = &self.journal {
                let _ = j(
                    "manual_manipulation",
                    "Manual implicit geometry edit",
                    &json!({"node": s.node_id, "session": s.id,
                        "parameters": s.candidates.iter().map(|c| c.key.clone()).collect::<Vec<_>>()}),
                );
            }
        }
        let (n_undo, n_redo) = {
            let st = lock(&self.state);
            (st.undo.len(), st.redo.len())
        };
        Ok(json!({
            "kind": "implicit_drag_committed", "schema": SCHEMA, "session": s.id, "changed": changed,
            "values": value_report, "history": shared_result,
            "undo": shared_result.as_ref().map_or(json!(n_undo), |h| h["undo"].clone()),
            "redo": shared_result.as_ref().map_or(json!(n_redo), |h| h["redo"].clone()),
            "final_sequence": final_report, "model": status,
        }))
    }


    pub fn cancel(&self, request: &Value, history: Option<&SharedHistory<'_>>) -> AResult<Value> {
        let sid = request.get("session").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
        let _g = self.lock.acquire();
        let mut s = self.get_active(&sid)?;
        let status = self.models.replace_live(&s.base_document, false)?;
        s.stream.cancel().map_err(drag)?;
        self.close(&s.id);
        let hist = match history {
            Some(h) => Some(h.runtime.history_state(h.stores)?),
            None => None,
        };
        let (n_undo, n_redo) = {
            let st = lock(&self.state);
            (st.undo.len(), st.redo.len())
        };
        Ok(json!({
            "kind": "implicit_drag_cancelled", "schema": SCHEMA, "session": s.id, "restored": true,
            "model": status, "history": hist,
            "undo": hist.as_ref().map_or(json!(n_undo), |h| h["undo"].clone()),
            "redo": hist.as_ref().map_or(json!(n_redo), |h| h["redo"].clone()),
        }))
    }

    fn history_step(
        &self,
        request: &Value,
        history: Option<&SharedHistory<'_>>,
        redo: bool,
    ) -> AResult<Value> {
        let _g = self.lock.acquire();
        let action = if redo { "redo" } else { "undo" };
        self.assert_idle(action)?;
        if let Some(h) = history {
            let reply =
                if redo { h.runtime.redo(h.stores, request)? } else { h.runtime.undo(h.stores, request)? };
            let mut out = reply.as_object().cloned().unwrap_or_default();
            out.insert("kind".into(), json!(format!("implicit_drag_{action}")));
            out.insert("schema".into(), json!(SCHEMA));
            out.insert("model".into(), self.models.status()?);
            return Ok(Value::Object(out));
        }
        let rec = {
            let st = lock(&self.state);
            let stack = if redo { &st.redo } else { &st.undo };
            stack.last().cloned()
        };
        let Some(rec) = rec else {
            return Err(merr(if redo {
                "there is no direct manipulation to redo"
            } else {
                "there is no committed direct manipulation to undo"
            }));
        };
        let current = self.models.snapshot()?;
        let (expect, target) = if redo { (&rec.before, &rec.after) } else { (&rec.after, &rec.before) };
        if D::dumps(&current) != D::dumps(expect) {
            return Err(merr(if redo {
                "the model changed after undo; gesture redo was not applied"
            } else {
                "the model changed after this direct manipulation; gesture undo was not applied"
            }));
        }
        let status = self.models.replace_live(target, true)?;
        let (n_undo, n_redo) = {
            let mut st = lock(&self.state);
            if redo {
                st.undo.push(rec.clone());
                st.redo.pop();
            } else {
                st.redo.push(rec.clone());
                st.undo.pop();
            }
            (st.undo.len(), st.redo.len())
        };
        Ok(json!({"kind": format!("implicit_drag_{action}"), "schema": SCHEMA, "label": rec.label,
            "undo": n_undo, "redo": n_redo, "model": status}))
    }


    pub fn undo(&self, request: &Value, history: Option<&SharedHistory<'_>>) -> AResult<Value> {
        self.history_step(request, history, false)
    }


    pub fn redo(&self, request: &Value, history: Option<&SharedHistory<'_>>) -> AResult<Value> {
        self.history_step(request, history, true)
    }

    #[allow(clippy::too_many_lines)]
    fn candidates(
        model: &Model,
        root: &NodeRef,
        node_id: &str,
        requested: Option<&Value>,
    ) -> AResult<Vec<Candidate>> {
        let paths = paths_by_node(model, root);
        let subtree: BTreeSet<&String> = paths.keys().collect();
        let diag =
            EV::aabb_of(root).map_or(10.0, |(lo, hi)| norm3([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]));
        let table: BTreeMap<String, Value> =
            model.parameter_table().into_iter().map(|r| (py_str(&r["name"]), r)).collect();
        let table_order: Vec<String> = model.parameter_table().iter().map(|r| py_str(&r["name"])).collect();
        let explicit: Vec<String> = match requested {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.iter().map(py_str).collect(),
            Some(_) => {
                return Err(merr("parameters must be a list of parameter names or node:param references"));
            }
        };
        let mut named_order: Vec<String> = Vec::new();
        let mut direct_order: Vec<(String, String)> = Vec::new();
        let bindings = model.bindings();
        let no_bind = BTreeMap::new();
        if explicit.is_empty() {
            let mut named_rows: Vec<(bool, String)> = Vec::new();
            for name in &table_order {
                let row = &table[name];
                if row.get("derived").is_some_and(truthy) {
                    continue;
                }
                let binds_here = row.get("binds").and_then(Value::as_array).is_some_and(|b| {
                    b.iter().any(|x| {
                        subtree.contains(&py_str(x).split(':').next().unwrap_or_default().to_string())
                    })
                });
                if binds_here {
                    named_rows.push((!row.get("free").is_some_and(truthy), name.clone()));
                }
            }
            named_rows.sort();
            named_order = named_rows.into_iter().map(|(_, n)| n).collect();
            let selected = model
                .node_table()
                .get(node_id)
                .cloned()
                .ok_or_else(|| AuthoringError::Key(repr(&json!(node_id))))?;
            for p in selected.info().sorted_param_names() {
                if discrete(&selected).contains(&p)
                    || bindings.get(node_id).unwrap_or(&no_bind).contains_key(&p)
                {
                    continue;
                }
                if finite_scalar(selected.param(&p)).is_some() {
                    direct_order.push((node_id.to_string(), p));
                }
            }
            if named_order.is_empty() && direct_order.is_empty() {
                for nid in &subtree {
                    let n = &model.node_table()[*nid];
                    for p in n.info().sorted_param_names() {
                        if discrete(n).contains(&p) || bindings.get(*nid).unwrap_or(&no_bind).contains_key(&p)
                        {
                            continue;
                        }
                        if finite_scalar(n.param(&p)).is_some() {
                            direct_order.push(((*nid).clone(), p));
                        }
                    }
                }
            }
        } else {
            for key in &explicit {
                if table.contains_key(key) {
                    named_order.push(key.clone());
                    continue;
                }
                let raw = key.strip_prefix("node:").unwrap_or(key);
                match raw.rsplit_once(':') {
                    Some((nid, param)) if subtree.contains(&nid.to_string()) => {
                        direct_order.push((nid.to_string(), param.to_string()));
                    }
                    _ => {
                        return Err(merr(format!(
                            "no editable parameter {} in the selected subtree",
                            repr(&json!(key))
                        )));
                    }
                }
            }
        }
        let mut out: Vec<Candidate> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for name in &named_order {
            if seen.contains(name) || !table.contains_key(name) {
                continue;
            }
            let row = &table[name];
            let Some(v) = finite_scalar_json(row.get("value")) else { continue };
            if row.get("derived").is_some_and(truthy) {
                continue;
            }
            let lo = row.get("min").map_or(Ok(f64::NEG_INFINITY), py_float)?;
            let hi = row.get("max").map_or(Ok(f64::INFINITY), py_float)?;
            let scale = if lo.is_finite() && hi.is_finite() && hi > lo {
                hi - lo
            } else {
                v.abs().max(diag * 0.10).max(1.0e-3)
            };
            let (sem, axis) = Self::named_semantic(model, name, &subtree);
            let mut targets = Vec::new();
            for b in row.get("binds").and_then(Value::as_array).into_iter().flatten() {
                let b = py_str(b);
                if let Some((nid, param)) = b.rsplit_once(':')
                    && subtree.contains(&nid.to_string())
                {
                    targets.push((nid.to_string(), param.to_string()));
                }
            }
            out.push(Candidate {
                key: name.clone(),
                kind: "named".into(),
                value: v,
                units: row.get("units").filter(|u| truthy(u)).map_or_else(|| "-".into(), py_str),
                scale,
                lower: lo,
                upper: hi,
                name: Some(name.clone()),
                node: None,
                param: None,
                label: name.clone(),
                semantic: sem,
                axis,
                direct_refs: Vec::new(),
                targets,
            });
            seen.insert(name.clone());
            if out.len() >= MAX_CANDIDATES {
                break;
            }
        }
        for (nid, param) in &direct_order {
            let key = format!("node:{nid}:{param}");
            if seen.contains(&key) || out.len() >= MAX_CANDIDATES || !subtree.contains(nid) {
                continue;
            }
            let node = &model.node_table()[nid];
            if node.info().param(param).is_none() || bindings.get(nid).unwrap_or(&no_bind).contains_key(param)
            {
                continue;
            }
            let Some(value) = finite_scalar(node.param(param)) else { continue };
            let (lo, hi, scale) = direct_bounds(param, value, diag);
            let (sem, axis) = semantic(param);
            let refs: Vec<ParamRef> = paths
                .get(nid)
                .into_iter()
                .flatten()
                .map(|p| ParamRef::new(p.clone(), param.clone()))
                .collect();
            out.push(Candidate {
                key: key.clone(),
                kind: "direct".into(),
                value,
                units: node.info().param(param).map(|s| s.units.clone()).unwrap_or_default(),
                scale,
                lower: lo,
                upper: hi,
                name: None,
                node: Some(nid.clone()),
                param: Some(param.clone()),
                label: format!("{nid} \u{b7} {param}"),
                semantic: sem,
                axis,
                direct_refs: refs,
                targets: vec![(nid.clone(), param.clone())],
            });
            seen.insert(key);
        }
        Ok(out)
    }

    fn named_semantic(model: &Model, name: &str, subtree: &BTreeSet<&String>) -> (String, Option<[f64; 3]>) {
        let mut sems = Vec::new();
        for (nid, binds) in model.bindings() {
            if !subtree.contains(nid) {
                continue;
            }
            for (param, b) in binds {
                if matches!(b, Binding::Bind { .. } | Binding::Expr { .. }) && expr_mentions(b, name) {
                    sems.push(semantic(param));
                }
            }
        }
        if let Some(first) = sems.first()
            && sems.iter().all(|x| x == first)
        {
            return first.clone();
        }
        semantic(name)
    }

    fn eval_opts(mode: &str, smooth_r_mm: f64) -> EvalOptions {
        EvalOptions {
            mode: if mode == "smooth" { Mode::Smooth } else { Mode::Exact },
            smooth_r_mm: Some(smooth_r_mm),
            ..EvalOptions::default()
        }
    }

    #[allow(clippy::too_many_lines)]
    fn linearisation(
        &self,
        model: &Model,
        root: &NodeRef,
        point: [f64; 3],
        candidates: &[Candidate],
        mode: &str,
        smooth_r_mm: f64,
    ) -> AResult<([f64; 3], Vec<f64>, String)> {
        let mut direct_refs: Vec<ParamRef> = Vec::new();
        for (path, n) in root.walk() {
            if model.id_of(&n).is_none() {
                continue;
            }
            for p in n.info().sorted_param_names() {
                if discrete(&n).contains(&p) || finite_scalar(n.param(&p)).is_none() {
                    continue;
                }
                let r = ParamRef::new(path.clone(), p);
                if !direct_refs.contains(&r) {
                    direct_refs.push(r);
                }
            }
        }
        if direct_refs.len() > MAX_DIRECT_REFS {
            let wanted_names: BTreeSet<&str> =
                candidates.iter().filter(|c| c.kind == "named").filter_map(|c| c.name.as_deref()).collect();
            let mut keep = Vec::new();
            for r in &direct_refs {
                let n = node_at_path(root, &r.path)?;
                let nid = model.id_of(&n);
                let binding =
                    nid.as_ref().and_then(|id| model.bindings().get(id)).and_then(|b| b.get(&r.name));
                let mut wanted = candidates.iter().any(|c| c.kind == "direct" && c.direct_refs.contains(r));
                match binding {
                    Some(Binding::Bind { name, .. }) if wanted_names.contains(name.as_str()) => wanted = true,
                    Some(Binding::Expr { tree, .. }) => {
                        wanted =
                            wanted || D::expr_names(tree).iter().any(|x| wanted_names.contains(x.as_str()));
                    }
                    _ => {}
                }
                if wanted {
                    keep.push(r.clone());
                }
            }
            keep.truncate(MAX_DIRECT_REFS);
            direct_refs = keep;
        }
        if direct_refs.is_empty() {
            return Err(merr("the selected subtree has no differentiable scalar parameters"));
        }
        let opts = Self::eval_opts(mode, smooth_r_mm);
        let ad = (|| -> Result<([f64; 3], HashMap<ParamRef, f64>), GeometryError> {
            let g = EV::grad_x(root, &[point], &opts)?;
            let spatial = g.first().copied().unwrap_or([f64::NAN; 3]);
            let loss = |f: &[f64]| {
                let mut cot = vec![0.0; f.len()];
                if let Some(c) = cot.first_mut() {
                    *c = 1.0;
                }
                (f.first().copied().unwrap_or(f64::NAN), cot)
            };
            let (_v, grads) = EV::value_and_grad_params(root, &direct_refs, &loss, &[point], &opts)?;
            let map = grads.into_iter().map(|pg| (pg.r, pg.data.first().copied().unwrap_or(0.0))).collect();
            Ok((spatial, map))
        })();
        let (spatial, direct_gradient, method) = match ad {
            Ok((s, m)) => (s, m, "automatic differentiation through the implicit graph".to_string()),
            Err(exc) => {
                let (s, m) = Self::finite_difference_graph(root, &direct_refs, point, &opts)?;
                let cls = AuthoringError::from(exc.clone()).class().to_string();
                (s, m, format!("central finite-difference fallback ({cls}: {exc})"))
            }
        };
        let mut jac = vec![0.0; candidates.len()];
        for (j, c) in candidates.iter().enumerate() {
            if c.kind == "direct" {
                jac[j] = c
                    .direct_refs
                    .iter()
                    .fold(0.0, |acc, r| acc + direct_gradient.get(r).copied().unwrap_or(0.0));
            } else {
                let slopes = self.named_leaf_slopes(model, root, c, &direct_refs)?;
                jac[j] = direct_refs.iter().fold(0.0, |acc, r| {
                    acc + direct_gradient.get(r).copied().unwrap_or(0.0)
                        * slopes.get(r).copied().unwrap_or(0.0)
                });
            }
        }
        Ok((spatial, jac, method))
    }

    fn finite_difference_graph(
        root: &NodeRef,
        refs: &[ParamRef],
        point: [f64; 3],
        opts: &EvalOptions,
    ) -> AResult<([f64; 3], HashMap<ParamRef, f64>)> {
        let fd_opts = EvalOptions { validate: false, ..*opts };
        let value = |node: &NodeRef, x: [f64; 3]| -> AResult<f64> {
            Ok(EV::eval_points(node, &[x], &fd_opts)?.first().copied().unwrap_or(f64::NAN))
        };
        let h = (norm3(point) * 1.0e-7).max(1.0e-5);
        let mut spatial = [0.0; 3];
        for k in 0..3 {
            let mut plus = point;
            let mut minus = point;
            plus[k] += h;
            minus[k] -= h;
            spatial[k] = (value(root, plus)? - value(root, minus)?) / (2.0 * h);
        }
        let mut out = HashMap::new();
        for r in refs {
            let base = ref_value(root, r)?;
            let step = base.abs().max(1.0) * 1.0e-6;
            let p = EV::set_params(root, &[(r.clone(), ParamValue::Float(base + step))])?;
            let m = EV::set_params(root, &[(r.clone(), ParamValue::Float(base - step))])?;
            out.insert(r.clone(), (value(&p, point)? - value(&m, point)?) / (2.0 * step));
        }
        Ok((spatial, out))
    }

    fn named_leaf_slopes(
        &self,
        model: &Model,
        root: &NodeRef,
        c: &Candidate,
        refs: &[ParamRef],
    ) -> AResult<HashMap<ParamRef, f64>> {
        let name = c.name.clone().unwrap_or_default();
        let base = c.value;
        let step = (c.scale.abs() * 1.0e-6).max(base.abs() * 1.0e-7).max(1.0e-7);
        let plus = (base + step).min(c.upper);
        let minus = (base - step).max(c.lower);
        if plus <= minus {
            return Ok(refs.iter().map(|r| (r.clone(), 0.0)).collect());
        }
        let _g = self.models.live_lock();
        let root_id = model.id_of(root).unwrap_or_default();
        let mut copy = D::build(&model.to_doc()?, Some(self.models.dir()), None)?;
        let read = |m: &Model| -> AResult<HashMap<ParamRef, f64>> {
            let r0 = m
                .node_table()
                .get(&root_id)
                .cloned()
                .ok_or_else(|| merr("the manipulated node disappeared"))?;
            refs.iter().map(|r| ref_value(&r0, r).map(|v| (r.clone(), v))).collect()
        };
        let set = |m: &mut Model, x: f64| -> AResult<()> {
            let mut vals = BTreeMap::new();
            vals.insert(name.clone(), jf(x));
            m.set_parameters(&vals)?;
            Ok(())
        };
        set(&mut copy, plus)?;
        let vp = read(&copy)?;
        set(&mut copy, minus)?;
        let vm = read(&copy)?;
        let den = plus - minus;
        Ok(refs.iter().map(|r| (r.clone(), (vp[r] - vm[r]) / den)).collect())
    }

    fn requested_displacement(request: &Value, s: &Session) -> AResult<[f64; 3]> {
        if let Some(d) = request.get("displacement_mm").filter(|v| !v.is_null()) {
            return vec3_of(Some(d)).ok_or_else(|| merr("displacement_mm must contain three finite values"));
        }
        if let Some(d) = request.get("normal_delta_mm").filter(|v| !v.is_null()) {
            let d = py_float(d)?;
            if !d.is_finite() {
                return Err(merr("normal_delta_mm must be finite"));
            }
            return Ok(s.normal.map(|n| n * d));
        }
        Err(merr("preview requires displacement_mm or normal_delta_mm"))
    }

    fn semantic_preview(
        s: &Session,
        key: &str,
        request: &Value,
        displacement: [f64; 3],
    ) -> AResult<(Vec<f64>, Value)> {
        let Some(idx) = s.candidates.iter().position(|c| c.key == key) else {
            return Err(merr(format!("semantic_parameter {} is not active in this drag", repr(&json!(key)))));
        };
        let c = &s.candidates[idx];
        let delta = match request.get("parameter_delta").filter(|v| !v.is_null()) {
            Some(v) => py_float(v)?,
            None => match c.axis {
                Some(a) => crate::py::dot3(displacement, a),
                None => crate::py::dot3(displacement, s.normal),
            },
        };
        if !delta.is_finite() {
            return Err(merr("semantic parameter delta is not finite"));
        }
        let init = s.initial_values();
        let mut values = init.clone();
        let proposed = values[idx] + delta;
        values[idx] = proposed.max(c.lower).min(c.upper);
        let clipped = values[idx] != proposed;
        Ok((
            values.clone(),
            json!({"mode": "semantic", "parameter": c.key, "requested_delta": jf(delta),
                "achieved_delta": jf(values[idx] - init[idx]), "clipped": clipped,
                "warnings": if clipped { vec!["parameter reached its editing bound"] } else { Vec::new() }}),
        ))
    }

    fn apply(&self, s: &Session, values: &[f64]) -> AResult<Value> {
        if values.len() != s.candidates.len() || !values.iter().all(|v| v.is_finite()) {
            return Err(merr("preview produced invalid parameter values"));
        }
        let mut named = BTreeMap::new();
        let mut direct = Vec::new();
        for (c, v) in s.candidates.iter().zip(values) {
            if c.kind == "named" {
                named.insert(c.name.clone().unwrap_or_default(), jf(*v));
            } else {
                direct.push((c.node.clone().unwrap_or_default(), c.param.clone().unwrap_or_default(), *v));
            }
        }
        self.models
            .mutate_live(&named, &direct)
            .map_err(|e| if e.is_model_doc() { manipulation_error(e.problem_list()) } else { e })
    }
}
