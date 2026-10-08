// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_authoring::physics_binding::{Occupancy, PreparedPhysics, Warm};
use implexity_core::pyobj::py_str;
use implexity_geometry::direct_occupancy::DirectOccupancyRepresentation;
use implexity_geometry::eval::value_and_grad_params;
use implexity_geometry::pyfmt::{fmt_e, fmt_g};
use implexity_geometry::{FieldClass, NodeRef, ParamRef};
use implexity_optim::design::NamedArrays;
use implexity_optim::numeric::{float_value, np_sum};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use super::spec::{
    Classification, Constraint, Drive, Free, MIN_OCCUPANCY, MaskBridge, OptimizeSpec, array_param, rebind,
};
use crate::error::{JobError, JobResult};

pub type Log<'a> = &'a (dyn Fn(&str) + Send + Sync);

fn contributions() -> &'static implexity_core::contributions::ContributionRegistry {
    &implexity_core::registries::global().contributions
}

fn binding_err(e: implexity_authoring::error::AuthoringError) -> JobError {
    if e.class() == "BindingError" { JobError::optimize(e.problem_list()) } else { e.into() }
}

fn np_tuple(v: &[f64]) -> String {
    format!(
        "({})",
        v.iter()
            .map(|x| format!("np.float64({})", implexity_core::py_repr::repr_float(*x)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[must_use]
pub fn is_capability_refusal(e: &JobError) -> bool {
    let m = e.message();
    m.starts_with("exact parameter gradients through a direct-occupancy model")
        || m.contains("does not provide exact response gradients")
}

#[derive(Clone)]
pub struct Evaluation {
    pub total: f64,
    pub aux: Map<String, Value>,
    pub warm: Option<Warm>,
    pub grad: Option<NamedArrays>,
    pub physics_residuals: Vec<f64>,
    pub physics_residual_grads: Vec<NamedArrays>,
    pub dm: Vec<f64>,
}

impl std::fmt::Debug for Evaluation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaluation").field("total", &self.total).finish_non_exhaustive()
    }
}

pub struct Problem {
    pub spec: Arc<OptimizeSpec>,
    pub bridge: MaskBridge,
    pub geometry_representation: Option<DirectOccupancyRepresentation>,
    pub field_class: Option<FieldClass>,
    pub step_factor: Option<f64>,
    pub physics: Box<dyn PreparedPhysics>,
    pub dm0: Vec<f64>,
    pub probe: Map<String, Value>,
    pub terms: Vec<String>,
    pub term_refusals: Vec<String>,
    pub constraints: Vec<Constraint>,
    pub l0: Option<f64>,
    pub refs: Value,
    pub penalties_at_start: Value,
    pub start_state: Value,
    pub warm0: Option<Warm>,
    pub obj_meta: Value,
}

impl std::fmt::Debug for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Problem").field("probe", &self.probe).finish_non_exhaustive()
    }
}

#[must_use]
pub fn spec_wire(spec: &OptimizeSpec) -> Value {
    json!({
        "case": spec.case, "norm": spec.norm, "box": spec.bbox, "settings": spec.settings,
        "objective": spec.objective, "occupancy": spec.occupancy, "physics": spec.physics.name(),
        "free": spec.free.iter().map(Free::describe).collect::<Vec<_>>(),
        "constraints": spec.constraints.iter().map(Constraint::describe).collect::<Vec<_>>(),
    })
}

fn occupancy_of(bridge: &MaskBridge, values: Vec<f64>) -> Occupancy {
    Occupancy { shape: bridge.shape, values }
}

fn aux_numbers(aux: &Map<String, Value>) -> std::collections::BTreeMap<String, f64> {
    aux.iter().filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f))).collect()
}

impl Problem {

    #[allow(clippy::too_many_lines)]
    pub fn new(spec: &Arc<OptimizeSpec>, log: Option<Log<'_>>, survey: bool) -> JobResult<Self> {
        let bridge = spec.bridge()?;
        let classification = bridge.check(&spec.model)?;
        let (geometry_representation, field_class, step_factor) = match classification {
            Classification::Direct(d) => (Some(d), None, None),
            Classification::Field(fc) => {
                let s = fc.safe_step_factor();
                (None, Some(fc), s)
            }
        };
        let mut physics = spec.physics.prepare(&spec_wire(spec), log).map_err(binding_err)?;
        let dm0 = bridge.occupancy(&spec.model, step_factor)?;
        #[allow(clippy::cast_precision_loss)]
        let v_model = np_sum(&dm0) / dm0.len() as f64;
        let band = dm0.iter().filter(|v| **v > 1e-12 && **v < 1.0 - 1e-12).count();
        let mut probe = Map::new();
        probe.insert("V_model".into(), float_value(v_model));
        probe.insert("band_cells".into(), json!(band));
        probe.insert("cells".into(), json!(dm0.len()));
        let initial_occupancy = occupancy_of(&bridge, dm0.clone());
        let measured = crate::solver_recovery::once("physics_probe", || physics.probe(&initial_occupancy).map_err(binding_err))?;
        if let Some(m) = measured.as_object() {
            for (k, v) in m {
                probe.insert(k.clone(), v.clone());
            }
        }
        let v = probe.get("V_model").and_then(Value::as_f64).unwrap_or(f64::NAN);
        if !(MIN_OCCUPANCY..=1.0 - MIN_OCCUPANCY).contains(&v) {
            let units = spec.settings.get("model_units").map(py_str).unwrap_or_default();
            let max = dm0.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            return Err(JobError::optimize1(format!(
                "the model occupies {} of the analysis box at its start parameters ({} of {} element centres in the \
                 band, max occupancy {}).  Below {} there is nothing for the physics to act on and above {} there is \
                 no surface to move, so every gradient would be zero or meaningless.  The usual cause is a UNIT \
                 mismatch: this node was told the graph speaks {}, so it sampled the child over origin {} with \
                 extents [{}, {}, {}] {units} and read its return value as {units}.  The analysis box is {} mm.",
                fmt_g(v, 4),
                probe.get("band_cells").map(py_str).unwrap_or_default(),
                probe.get("cells").map(py_str).unwrap_or_default(),
                fmt_e(max, 3),
                implexity_geometry::pyfmt::g(MIN_OCCUPANCY),
                implexity_geometry::pyfmt::g(1.0 - MIN_OCCUPANCY),
                implexity_core::pyobj::repr(&Value::from(units.clone())),
                np_tuple(&bridge.origin),
                fmt_g(bridge.extent[0], 4),
                fmt_g(bridge.extent[1], 4),
                fmt_g(bridge.extent[2], 4),
                implexity_core::pyobj::repr(spec.bbox.get("domain_mm").unwrap_or(&Value::Null)),
            )));
        }
        let names = spec.term_names();
        let (terms, term_refusals) = implexity_core::objective_terms::applicability(
            contributions(),
            &names,
            &Value::Object(probe.clone()),
            Some(spec.physics.name()),
        );
        let mut constraints = spec.constraints.clone();
        for c in &mut constraints {
            c.resolve(v)?;
        }
        let mut out = Self {
            spec: Arc::clone(spec),
            bridge,
            geometry_representation,
            field_class,
            step_factor,
            physics,
            dm0,
            probe,
            terms,
            term_refusals,
            constraints,
            l0: None,
            refs: json!({}),
            penalties_at_start: Value::Null,
            start_state: Value::Null,
            warm0: None,
            obj_meta: Value::Null,
        };
        if survey {
            return Ok(out);
        }
        if !out.term_refusals.is_empty() {
            return Err(JobError::optimize(out.term_refusals.clone()));
        }
        out.obj_meta = out.physics.build_objective(&spec.objective).map_err(binding_err)?;
        Ok(out)
    }

    #[must_use]
    pub fn lattice(&self) -> Value {
        self.physics.report()
    }

    #[must_use]
    pub fn classification(&self) -> Classification {
        match (&self.geometry_representation, &self.field_class) {
            (Some(d), _) => Classification::Direct(d.clone()),
            (None, Some(fc)) => Classification::Field(fc.clone()),
            (None, None) => Classification::Direct(DirectOccupancyRepresentation::default()),
        }
    }


    fn document_model_of(&self, p: &NamedArrays) -> JobResult<Option<implexity_geometry::document::Model>> {
        let Some(context) = self.spec.settings.get("document_parameter_context").filter(|v| !v.is_null()) else { return Ok(None) };
        if context["schema"] != "implexity-document-parameter-optimization/1" { return Err(JobError::value("document parameter optimization context is unsupported")); }
        let base = context["base_dir"].as_str().map(std::path::Path::new);
        let mut document = implexity_geometry::document::build(&context["document"], base, None)?;
        let mut values = std::collections::BTreeMap::new();
        for free in &self.spec.free { if let Some(name) = &free.document_parameter {
            let value = p.get(&free.slot).filter(|v| v.ndim() == 0).and_then(|v| v.first()).copied().ok_or_else(|| JobError::value("named coordinate must be a scalar"))?;
            values.insert(name.clone(), float_value(value));
        } }
        document.set_parameters(&values)?;
        Ok(Some(document))
    }

    pub fn model_of(&self, p: &NamedArrays, drive: Option<&Drive>) -> JobResult<NodeRef> {
        let mut vals = Vec::new();
        for fr in &self.spec.free {
            if fr.document_parameter.is_some() { continue; }
            if let Some(v) = p.get(&fr.slot) {
                vals.push((fr.child_ref(), array_param(v)));
            }
        }
        for (key, v) in drive.into_iter().flatten() {
            let r = ParamRef::parse(key)?;
            vals.push((ParamRef::new(r.path.iter().skip(1).cloned().collect(), r.name), array_param(v)));
        }
        let root = if let Some(document) = self.document_model_of(p)? {
            let context = &self.spec.settings["document_parameter_context"];
            document.node(context["node"].as_str().ok_or_else(|| JobError::value("document parameter node is missing"))?)?
        } else { Arc::clone(&self.spec.model) };
        rebind(&root, &vals)
    }


    pub fn occupancy(&self, p: &NamedArrays, drive: Option<&Drive>) -> JobResult<Vec<f64>> {
        self.bridge.occupancy(&self.model_of(p, drive)?, self.step_factor)
    }


    pub fn calibrate(&mut self) -> JobResult<(f64, Value)> {
        let occupancy = occupancy_of(&self.bridge, self.dm0.clone());
        crate::solver_recovery::once("physics_calibration", || self.physics.calibrate(&occupancy).map_err(binding_err))?;
        let l0 =
            self.physics.l0().ok_or_else(|| JobError::runtime("the physics binding calibrated no l0"))?;
        self.l0 = Some(l0);
        self.refs = match self.physics.refs() {
            Value::Null => json!({}),
            v => v,
        };
        self.penalties_at_start = self.physics.penalties_at_start();
        let mut state = match self.physics.start_state() {
            Some(w) => self.warm_json(&w),
            None => Map::new(),
        };
        #[allow(clippy::cast_precision_loss)]
        let v = np_sum(&self.dm0) / self.dm0.len() as f64;
        state.insert("V_model".into(), float_value(v));
        self.start_state = Value::Object(state);
        self.warm0 = self.physics.warm0();
        Ok((l0, self.refs.clone()))
    }

    fn warm_json(&self, w: &Warm) -> Map<String, Value> {
        if let Some(v) = w.downcast_ref::<Value>() {
            return v.as_object().cloned().unwrap_or_default();
        }
        if let Some(v) = w.downcast_ref::<Map<String, Value>>() {
            return v.clone();
        }
        self.physics
            .warm_to_arrays(w)
            .map(|arrays| {
                arrays
                    .into_iter()
                    .map(|(k, (shape, data))| {
                        let a = ArrayD::from_shape_vec(IxDyn(&shape), data)
                            .unwrap_or_else(|_| ArrayD::zeros(IxDyn(&[0])));
                        (k, super::spec::safe(&a))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn warm_arrays(&self, warm: Option<&Warm>) -> NamedArrays {
        let Some(w) = warm else { return NamedArrays::new() };
        let Some(arrays) = self.physics.warm_to_arrays(w) else { return NamedArrays::new() };
        NamedArrays::from_pairs(arrays.into_iter().filter_map(|(k, (shape, data))| {
            ArrayD::from_shape_vec(IxDyn(&shape), data).ok().map(|a| (k, a))
        }))
    }


    pub fn warm_from(&self, arrays: &NamedArrays) -> JobResult<Option<Warm>> {
        if arrays.is_empty() {
            return Ok(None);
        }
        let map = arrays
            .iter()
            .map(|(k, a)| (k.to_string(), (a.shape().to_vec(), a.iter().copied().collect())))
            .collect();
        self.physics.warm_from_arrays(&map).map_err(binding_err)
    }


    pub fn restore(&mut self, l0: f64, refs: &Value, extras: &Value) -> JobResult<()> {
        self.l0 = Some(l0);
        self.refs = refs.clone();
        let extras = if extras.is_null() { json!({}) } else { extras.clone() };
        self.physics.restore(l0, refs, &extras).map_err(binding_err)?;
        Ok(())
    }

    #[must_use]
    pub fn row_quantities(&self, aux: &Map<String, Value>) -> Map<String, Value> {
        let mut out = Map::new();
        for [row_key, aux_key, _, _] in self.spec.physics.row_quantities() {
            if let Some(v) = aux.get(&aux_key).and_then(Value::as_f64) {
                out.insert(row_key, float_value(v));
            }
        }
        for (k, v) in
            implexity_core::objective_terms::row_diagnostics(contributions(), &self.terms, &aux_numbers(aux))
        {
            out.insert(k, float_value(v));
        }
        out
    }

    fn refuse_direct_gradient(&self) -> JobResult<()> {
        if self.geometry_representation.is_some() {
            return Err(JobError::optimize1(
                "exact parameter gradients through a direct-occupancy model are not available in this build: the \
                 geometry kernel's direct-occupancy resolution has no reverse pass (recorded in docs/HANDOFF.md); \
                 evaluate it without a gradient or optimise it through a registered modular provider",
            ));
        }
        Ok(())
    }

    fn pull_back(&self, model: &NodeRef, p: &NamedArrays, cot_dm: &[f64]) -> JobResult<NamedArrays> {
        let factor = self.step_factor.unwrap_or(f64::NAN);
        let bridge = &self.bridge;
        let document = self.document_model_of(p)?;
        let mut refs = Vec::new();
        let mut pullbacks = Vec::new();
        for free in &self.spec.free {
            let consumers = if let Some(name) = &free.document_parameter {
                let document = document.as_ref().ok_or_else(|| JobError::value("named coordinate has no document context"))?;
                let context = &self.spec.settings["document_parameter_context"];
                document.parameter_consumers(&document.node(context["node"].as_str().unwrap_or(""))?, name)?
            } else { vec![(free.child_ref(), 1.0)] };
            let mut map = Vec::new();
            for (reference, coefficient) in consumers {
                let index = refs.iter().position(|r| r == &reference).unwrap_or_else(|| { refs.push(reference); refs.len()-1 });
                map.push((index, coefficient));
            }
            pullbacks.push(map);
        }
        let cot = cot_dm.to_vec();
        let loss = move |f: &[f64]| -> (f64, Vec<f64>) {
            let c: Vec<f64> =
                f.iter().zip(&cot).map(|(fv, c)| c * bridge.profile_of(*fv, factor).1).collect();
            (0.0, c)
        };
        let (_, grads) = value_and_grad_params(model, &refs, &loss, &bridge.points, &bridge.eval_options()?)?;
        let mut out = NamedArrays::new();
        for (fr, map) in self.spec.free.iter().zip(pullbacks) {
            let gradient = if fr.document_parameter.is_some() {
                let mut value = 0.0;
                for (index, coefficient) in map {
                    let g = &grads[index];
                    if !g.shape.is_empty() || g.data.len() != 1 { return Err(JobError::value("named parameter consumer gradient is not scalar")); }
                    value += coefficient * g.data[0];
                }
                if !value.is_finite() { return Err(JobError::value("document parameter pullback is not finite")); }
                ArrayD::from_elem(IxDyn(&[]), value)
            } else {
                let g = &grads[map[0].0];
                ArrayD::from_shape_vec(IxDyn(&g.shape), g.data.clone()).map_err(|e| JobError::runtime(e.to_string()))?
            };
            out.insert(fr.slot.clone(), gradient);
        }
        Ok(out)
    }


    #[allow(clippy::too_many_lines)]
    pub fn evaluate(
        &mut self,
        p: &NamedArrays,
        warm: Option<&Warm>,
        drive: Option<&Drive>,
        penalise: bool,
        want_grad: bool,
        physics_cons: &[usize],
    ) -> JobResult<Evaluation> {
        crate::solver_recovery::once("physics_evaluation", || self.evaluate_once(p, warm, drive, penalise, want_grad, physics_cons))
    }

    fn evaluate_once(
        &mut self,
        p: &NamedArrays,
        warm: Option<&Warm>,
        drive: Option<&Drive>,
        penalise: bool,
        want_grad: bool,
        physics_cons: &[usize],
    ) -> JobResult<Evaluation> {
        let l0 = self.l0.ok_or_else(|| JobError::runtime("the problem is not calibrated"))?;
        let model = self.model_of(p, drive)?;
        if want_grad {
            self.refuse_direct_gradient()?;
        }
        let dm = self.bridge.occupancy(&model, self.step_factor)?;
        let n = dm.len();
        #[allow(clippy::cast_precision_loss)]
        let nf = n as f64;
        let v_mean = np_sum(&dm) / nf;
        let occ = occupancy_of(&self.bridge, dm.clone());
        let penal: Vec<usize> = if penalise { (0..self.constraints.len()).collect() } else { Vec::new() };

        let mut response_keys: Vec<String> = Vec::new();
        for j in penal.iter().chain(physics_cons) {
            if let Constraint::ResponseBound { aux_key, .. } = &self.constraints[*j]
                && !response_keys.contains(aux_key)
            {
                response_keys.push(aux_key.clone());
            }
        }
        let (l_phys, g_dm, aux_v, next) = if want_grad {
            let (l, g, aux, w) = self.physics.value_and_grad(&occ, warm).map_err(binding_err)?;
            (l, Some(g), aux, w)
        } else {
            let (l, aux, w) = self.physics.value(&occ, warm).map_err(binding_err)?;
            (l, None, aux, w)
        };
        let mut response_grads: std::collections::BTreeMap<String, Vec<f64>> =
            std::collections::BTreeMap::new();
        if want_grad && !response_keys.is_empty() {
            let Some((_, grads)) =
                self.physics.response_values_and_grads(&occ, warm, &response_keys).map_err(binding_err)?
            else {
                return Err(JobError::optimize1(format!(
                    "the physics binding {} does not provide exact response gradients, so the response_bound \
                     constraint(s) on {} cannot be differentiated",
                    implexity_core::py_repr::repr_str(self.spec.physics.name()),
                    response_keys.join(", ")
                )));
            };
            for (k, gk) in response_keys.iter().zip(grads) {
                response_grads.insert(k.clone(), gk);
            }
        }
        let mut aux = aux_v.as_object().cloned().unwrap_or_default();
        aux.insert("L_physical".into(), float_value(l_phys));
        let mut total = l_phys / l0;
        let mut pen = 0.0;
        let aux_value = Value::Object(aux.clone());
        let mut cot = g_dm.as_ref().map(|g| g.iter().map(|x| x / l0).collect::<Vec<f64>>());
        for j in &penal {
            let c = &self.constraints[*j];
            pen += c.penalty(v_mean, &aux_value);
            if let Some(cot) = cot.as_mut() {
                match c {
                    Constraint::VolumeFraction { weight, resolved, .. } => {
                        let t = resolved.unwrap_or(f64::NAN);
                        let dv = 2.0 * weight * (v_mean - t) / (t * t) / nf;
                        for x in cot.iter_mut() {
                            *x += dv;
                        }
                    }
                    Constraint::ResponseBound { aux_key, sense, scale, weight, .. } => {
                        let r = c.residual(v_mean, &aux_value);
                        if r > 0.0 {
                            let sign = if sense == "max" { 1.0 } else { -1.0 };
                            let coef = 2.0 * weight * r * sign / scale;
                            for (x, gr) in cot.iter_mut().zip(&response_grads[aux_key]) {
                                *x += coef * gr;
                            }
                        }
                    }
                }
            }
        }
        total += pen;
        aux.insert("V_model".into(), float_value(v_mean));
        aux.insert("constraint_penalty".into(), float_value(pen));
        let aux_value = Value::Object(aux.clone());
        let physics_residuals: Vec<f64> =
            physics_cons.iter().map(|j| self.constraints[*j].residual(v_mean, &aux_value)).collect();
        let mut grad = None;
        let mut physics_residual_grads = Vec::new();
        if let Some(cot) = cot {
            grad = Some(self.pull_back(&model, p, &cot)?);
            for j in physics_cons {
                if let Constraint::ResponseBound { aux_key, sense, scale, .. } = &self.constraints[*j] {
                    let sign = if sense == "max" { 1.0 } else { -1.0 };
                    let c: Vec<f64> = response_grads[aux_key].iter().map(|g| sign * g / scale).collect();
                    physics_residual_grads.push(self.pull_back(&model, p, &c)?);
                }
            }
        }
        Ok(Evaluation { total, aux, warm: next, grad, physics_residuals, physics_residual_grads, dm })
    }


    pub fn occupancy_residual(
        &self,
        j: usize,
        p: &NamedArrays,
        drive: Option<&Drive>,
        want_grad: bool,
    ) -> JobResult<(f64, Option<NamedArrays>)> {
        let model = self.model_of(p, drive)?;
        let dm = self.bridge.occupancy(&model, self.step_factor)?;
        #[allow(clippy::cast_precision_loss)]
        let nf = dm.len() as f64;
        let v_mean = np_sum(&dm) / nf;
        let c = &self.constraints[j];
        let r = c.residual(v_mean, &Value::Null);
        if !want_grad {
            return Ok((r, None));
        }
        self.refuse_direct_gradient()?;
        let t = c.resolved().unwrap_or(f64::NAN);
        let cot = vec![1.0 / (nf * t); dm.len()];
        Ok((r, Some(self.pull_back(&model, p, &cot)?)))
    }


    pub fn response_jacobian(
        &mut self,
        p: &NamedArrays,
        warm: Option<&Warm>,
        drive: Option<&Drive>,
        names: &[String],
    ) -> JobResult<(Vec<f64>, Vec<NamedArrays>)> {
        self.refuse_direct_gradient()?;
        let model = self.model_of(p, drive)?;
        let dm = self.bridge.occupancy(&model, self.step_factor)?;
        let occ = occupancy_of(&self.bridge, dm);
        let Some((values, grads)) =
            crate::solver_recovery::once("physics_sensitivity", || self.physics.response_values_and_grads(&occ, warm, names).map_err(binding_err))?
        else {
            return Err(JobError::optimize1(format!(
                "the physics binding {} does not provide exact response gradients; per-response derivatives are \
                 unavailable",
                implexity_core::py_repr::repr_str(self.spec.physics.name())
            )));
        };
        let mut rows = Vec::with_capacity(grads.len());
        for g in &grads {
            rows.push(self.pull_back(&model, p, g)?);
        }
        Ok((values, rows))
    }


    pub fn response_values(
        &mut self,
        p: &NamedArrays,
        warm: Option<&Warm>,
        drive: Option<&Drive>,
        names: &[String],
    ) -> JobResult<Vec<f64>> {
        let ev = self.evaluate(p, warm, drive, true, false, &[])?;
        names
            .iter()
            .map(|n| {
                ev.aux
                    .get(n)
                    .and_then(Value::as_f64)
                    .ok_or_else(|| JobError::of("KeyError", implexity_core::py_repr::repr_str(n)))
            })
            .collect()
    }


    pub fn response_jvp(
        &mut self,
        p: &NamedArrays,
        warm: Option<&Warm>,
        drive: Option<&Drive>,
        names: &[String],
        tangent: &NamedArrays,
    ) -> JobResult<Vec<f64>> {
        let (_, rows) = self.response_jacobian(p, warm, drive, names)?;
        Ok(rows
            .iter()
            .map(|row| {
                let mut acc = 0.0;
                for (k, g) in row.iter() {
                    if let Some(t) = tangent.get(k) {
                        acc += g.iter().zip(t.iter()).map(|(a, b)| a * b).sum::<f64>();
                    }
                }
                acc
            })
            .collect())
    }


    pub fn response_vjp(
        &mut self,
        p: &NamedArrays,
        warm: Option<&Warm>,
        drive: Option<&Drive>,
        names: &[String],
        w: &[f64],
    ) -> JobResult<NamedArrays> {
        self.refuse_direct_gradient()?;
        let model = self.model_of(p, drive)?;
        let dm = self.bridge.occupancy(&model, self.step_factor)?;
        let occ = occupancy_of(&self.bridge, dm.clone());
        let Some((_, grads)) =
            crate::solver_recovery::once("physics_sensitivity", || self.physics.response_values_and_grads(&occ, warm, names).map_err(binding_err))?
        else {
            return Err(JobError::optimize1(format!(
                "the physics binding {} does not provide exact response gradients; per-response derivatives are \
                 unavailable",
                implexity_core::py_repr::repr_str(self.spec.physics.name())
            )));
        };
        let mut cot = vec![0.0; dm.len()];
        for (wi, g) in w.iter().zip(&grads) {
            for (c, gv) in cot.iter_mut().zip(g) {
                *c += wi * gv;
            }
        }
        self.pull_back(&model, p, &cot)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Design {
    pub frees: Vec<Free>,
    pub scaling: String,
    pub spans: std::collections::BTreeMap<String, f64>,
    pub origins: std::collections::BTreeMap<String, f64>,
}

impl Design {
    #[must_use]
    pub fn new(frees: &[Free], scaling: &str) -> Self {
        let mut spans = std::collections::BTreeMap::new();
        let mut origins = std::collections::BTreeMap::new();
        for fr in frees {
            if scaling == "unit_range" {
                spans.insert(fr.slot.clone(), fr.span().unwrap_or(f64::NAN));
                origins.insert(fr.slot.clone(), fr.origin());
            } else {
                spans.insert(fr.slot.clone(), 1.0);
                origins.insert(fr.slot.clone(), 0.0);
            }
        }
        Self { frees: frees.to_vec(), scaling: scaling.into(), spans, origins }
    }

    #[must_use]
    pub fn z_of(&self, p: &NamedArrays) -> NamedArrays {
        NamedArrays::from_pairs(p.iter().map(|(k, v)| {
            let (o, s) = (self.origins[k], self.spans[k]);
            (k.to_string(), v.mapv(|x| (x - o) / s))
        }))
    }

    #[must_use]
    pub fn p_of(&self, z: &NamedArrays) -> NamedArrays {
        NamedArrays::from_pairs(z.iter().map(|(k, v)| {
            let (o, s) = (self.origins[k], self.spans[k]);
            (k.to_string(), v.mapv(|x| o + x * s))
        }))
    }

    #[must_use]
    pub fn grad_z(&self, gp: &NamedArrays) -> NamedArrays {
        NamedArrays::from_pairs(gp.iter().map(|(k, v)| {
            let s = self.spans[k];
            (k.to_string(), v.mapv(|x| x * s))
        }))
    }

    #[must_use]
    pub fn project(&self, z: &NamedArrays) -> (NamedArrays, std::collections::BTreeMap<String, usize>) {
        let mut out = NamedArrays::new();
        let mut active = std::collections::BTreeMap::new();
        for fr in &self.frees {
            let Some(v) = z.get(&fr.slot) else { continue };
            let (o, s) = (self.origins[&fr.slot], self.spans[&fr.slot]);
            let lo = fr.lo.map_or(-1.0e300, |l| (l - o) / s);
            let hi = fr.hi.map_or(1.0e300, |h| (h - o) / s);

            let c = v.mapv(|x| if x.is_nan() { x } else { x.max(lo).min(hi) });
            let n = c.iter().zip(v.iter()).filter(|(a, b)| a != b).count();
            active.insert(fr.slot.clone(), n);
            out.insert(fr.slot.clone(), c);
        }
        (out, active)
    }

    #[must_use]
    pub fn start(&self) -> NamedArrays {
        NamedArrays::from_pairs(self.frees.iter().map(|f| (f.slot.clone(), f.start.clone())))
    }
}
