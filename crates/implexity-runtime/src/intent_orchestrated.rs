// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, MatchingTimeNewtonGuess, ProviderCapabilities, ProviderDescriptor,
    ProviderProblem, ResponseSpec, Sensitivity, TOPOLOGY_COORDINATE,
};
use implexity_core::orchestration::{EngineeringIntent, OrchestrationPlan, OrchestrationPlanner, PlanStatus};
use implexity_core::registries::Registries;
use implexity_core::wire::{fingerprint, to_wire};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::NamedArrays;
use implexity_optim::optimizer::{OptimizerLifecycleConfig, lifecycle};
use implexity_optim::provider_ops::LifecycleDeclaration;
use implexity_optim::provider_ops::{
    CachedEvaluation, DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, ProviderScope,
    design_interface, design_operations,
};
use implexity_solve::geometry_design_map::{GeometryDesignMap, from_context};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::addin::JsonMap;
use crate::orchestration_runtime::{DesignArg, ExecuteRequest, ExecutionContext, OrchestrationRuntime};
use crate::results::{ExecutionOutput, json_problem, problem_json};

pub const PROVIDER_ID: &str = "intent_orchestrated";

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message.into())
}


pub fn checked_preflight_effects(value: Option<&Value>) -> CaeResult<Value> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(json!({"status": "unknown", "possible_effects": []}));
    };
    let allowed = ["constitutive_evaluation", "geometry_evaluation", "initial_equilibrium_solve"];
    let ok = value.as_object().is_some_and(|m| {
        m.len() == 2
            && m.get("status").and_then(Value::as_str).is_some_and(|s| s == "declared" || s == "unknown")
            && m.get("possible_effects").and_then(Value::as_array).is_some_and(|effects| {
                let unique: std::collections::BTreeSet<String> =
                    effects.iter().map(Value::to_string).collect();
                unique.len() == effects.len()
                    && effects.iter().all(|e| e.as_str().is_some_and(|s| allowed.contains(&s)))
            })
    });
    if !ok {
        return Err(contract("invalid provider preflight effects declaration"));
    }
    let mut effects: Vec<String> =
        value["possible_effects"].as_array().into_iter().flatten().map(crate::pyval::py_str).collect();
    effects.sort();
    Ok(json!({"status": value["status"], "possible_effects": effects}))
}

#[derive(Debug, Clone)]
pub struct IntentOrchestratedProvider {
    registries: &'static Registries,
}

impl Default for IntentOrchestratedProvider {
    fn default() -> Self {
        Self { registries: implexity_core::registries::global() }
    }
}

type Parts = (Value, Arc<OrchestrationPlan>, JsonMap);

impl IntentOrchestratedProvider {
    #[must_use]
    pub fn new(registries: &'static Registries) -> Self {
        Self { registries }
    }

    fn sync(&self) -> CaeResult<()> {
        self.registries.providers.sync_orchestration_bundles(&self.registries.addins)
    }

    fn plan(&self, intent: &EngineeringIntent) -> CaeResult<OrchestrationPlan> {
        let snapshot = self.registries.addins.snapshot();
        OrchestrationPlanner.plan_snapshot(intent, &snapshot, &self.registries.sufficiency)
    }


    pub fn normalise_value(&self, problem: &Value) -> CaeResult<Value> {
        let Some(p) = problem.as_object() else {
            return Err(contract("intent-orchestrated problem must be an object"));
        };
        let raw_intent = match p.get("intent") {
            Some(v) if crate::pyval::truthy(Some(v)) => v.clone(),
            _ => Value::Object(Map::new()),
        };
        let intent = EngineeringIntent::from_mapping(&raw_intent)?;
        self.sync()?;
        let plan = self.plan(&intent)?;
        let mut ctx =
            crate::pyval::mapping_or_empty(p.get("context").filter(|v| crate::pyval::truthy(Some(v))));
        ctx.entry("authoring").or_insert_with(|| Value::Object(intent.authoring.clone()));
        if !ctx.contains_key("provider_problems")
            && let Some(Value::Object(pp)) = intent.authoring.get("provider_problems")
        {
            ctx.insert("provider_problems".into(), Value::Object(pp.clone()));
        }
        for key in ["quantities", "boundary_conditions", "states", "external_quantities"] {
            if let Some(Value::Object(row)) = intent.authoring.get(key)
                && !ctx.contains_key(key)
            {
                ctx.insert(key.into(), Value::Object(row.clone()));
            }
        }
        let wire_plan = to_wire(&plan.as_dict())?;
        if let Some(mapping) = from_context(&ctx)? {
            ctx.insert("geometry_design_map".into(), Value::Object(mapping.spec().clone()));
        }
        let declaration =
            json!({"intent": to_wire(&raw_intent)?, "context": to_wire(&Value::Object(ctx.clone()))?});
        let declaration_fp = fingerprint(&declaration)?;
        let plan_fp = fingerprint(&wire_plan)?;
        if let Some(v) = p.get("declaration_fingerprint").filter(|v| crate::pyval::truthy(Some(v)))
            && v.as_str() != Some(declaration_fp.as_str())
        {
            return Err(contract("STALE_PHYSICS_PLAN: authored problem changed; plan again"));
        }
        if let Some(v) = p.get("plan_fingerprint").filter(|v| crate::pyval::truthy(Some(v)))
            && v.as_str() != Some(plan_fp.as_str())
        {
            return Err(contract("STALE_PHYSICS_PLAN: catalogue or intent changed; plan again"));
        }
        Ok(json!({
            "intent": to_wire(&raw_intent)?,
            "context": to_wire(&Value::Object(ctx))?,
            "plan": wire_plan,
            "active_design_coordinates": plan.active_design_coordinates,
            "plan_fingerprint": plan_fp,
            "declaration_fingerprint": declaration_fp,
        }))
    }


    pub fn parts(&self, problem: &Value) -> CaeResult<Parts> {
        if !problem.is_object() {
            return Err(contract("orchestrated problem must be an object"));
        }
        let p = self.normalise_value(problem)?;
        if let Some(v) = problem.get("plan_fingerprint").filter(|v| crate::pyval::truthy(Some(v)))
            && Some(v) != p.get("plan_fingerprint")
        {
            return Err(contract("STALE_PHYSICS_PLAN: catalogue or intent changed; plan again"));
        }
        let plan = self.plan(&EngineeringIntent::from_mapping(&p["intent"])?)?;
        let ctx = crate::pyval::mapping_or_empty(p.get("context"));
        Ok((p, Arc::new(plan), ctx))
    }

    fn parts_of(&self, problem: &ProviderProblem) -> CaeResult<Parts> {
        let value =
            problem_json(problem).ok_or_else(|| contract("orchestrated problem must be an object"))?;
        self.parts(value)
    }


    pub fn replan_problem_revision(&self, previous: &Value, revised: &Value) -> CaeResult<Value> {
        self.normalise_value(previous)?;
        let Some(r) = revised.as_object() else {
            return Err(contract("revised orchestration problem must be an object"));
        };
        let mut declaration = Map::new();
        for key in ["intent", "context"] {
            if let Some(v) = r.get(key) {
                declaration.insert(key.into(), v.clone());
            }
        }
        let declaration = reconcile_provider_problem_aliases(previous, &Value::Object(declaration))?;
        self.normalise_value(&declaration)
    }


    pub fn replan_problem_for_responses(
        &self,
        previous: &Value,
        revised: &Value,
        responses: &[ResponseSpec],
    ) -> CaeResult<Value> {
        if responses.is_empty() {
            return Err(contract("reviewed run responses must be nonempty typed ResponseSpec entries"));
        }
        let Some(Value::Object(_)) = revised.get("intent") else {
            return Err(contract("reviewed run requires an authored engineering intent"));
        };
        let mut declaration = revised.clone();
        let mut goals = Vec::new();
        let mut constraints = Vec::new();
        for row in responses {
            let relation = match row.sense.as_str() {
                "minimise" => "minimize",
                "maximise" => "maximize",
                "upper" => "less_equal",
                "lower" => "greater_equal",
                _ => "equal",
            };
            let mut value = Map::new();
            value.insert("response".into(), Value::String(row.name.clone()));
            value.insert("relation".into(), Value::String(relation.into()));
            value.insert("weight".into(), row.weight.to_value());
            if matches!(row.sense.as_str(), "upper" | "lower" | "equal") {
                value.insert(
                    "value".into(),
                    row.target.map_or(Value::Null, implexity_core::pyobj::PyNum::to_value),
                );
                constraints.push(Value::Object(value));
            } else {
                goals.push(Value::Object(value));
            }
        }
        if let Some(Value::Object(intent)) = declaration.get_mut("intent") {
            intent.insert("goals".into(), Value::Array(goals));
            intent.insert("constraints".into(), Value::Array(constraints));
        }
        self.replan_problem_revision(previous, &declaration)
    }


    pub fn preflight_value(&self, problem: &Value, topology: bool) -> CaeResult<JsonMap> {
        let (p, plan, ctx) = self.parts(problem)?;
        if contract_version(&p["intent"]) == Some(2) && topology {
            return Err(contract("bare-array preflight is compatibility-only"));
        }
        if plan.status != PlanStatus::Ready {
            let issues = if !plan.blocked_reasons.is_empty() {
                &plan.blocked_reasons
            } else if !plan.missing_physics.is_empty() {
                &plan.missing_physics
            } else {
                &plan.missing_authoring
            };
            let mut out = Map::new();
            out.insert("ok".into(), Value::Bool(false));
            out.insert("issues".into(), json!(issues));
            out.insert("orchestrationPlan".into(), plan.as_dict());
            return Ok(out);
        }
        let plan_dict = plan.as_dict().as_object().cloned().unwrap_or_default();
        let admission = crate::execution_readiness::inspect_execution(
            &p["intent"],
            &plan_dict,
            Some(&ctx),
            &self.registries.addins,
        )?;
        if admission.get("execution_status").and_then(Value::as_str) == Some("blocked") {
            let mut out = Map::new();
            out.insert("ok".into(), Value::Bool(false));
            out.insert("issues".into(), admission.get("execution_issues").cloned().unwrap_or(json!([])));
            out.insert("orchestrationPlan".into(), Value::Object(admission));
            return Ok(out);
        }
        let coupling = self.coupling_report(problem, false)?;
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(crate::pyval::truthy(coupling.get("ok"))));
        out.insert("issues".into(), coupling.get("errors").cloned().unwrap_or(json!([])));
        out.insert("warnings".into(), coupling.get("warnings").cloned().unwrap_or(json!([])));
        out.insert("couplingReport".into(), Value::Object(coupling));
        out.insert("orchestrationPlan".into(), plan.as_dict());
        out.insert("active_design_coordinates".into(), json!(plan.active_design_coordinates));
        Ok(out)
    }


    pub fn coupling_report(&self, problem: &Value, for_optimization: bool) -> CaeResult<JsonMap> {
        let (_p, plan, ctx) = self.parts(problem)?;
        let entries = self
            .registries
            .addins
            .snapshot()
            .entries
            .iter()
            .map(|e| (e.contract.addin_id.clone(), Arc::clone(e)))
            .collect::<BTreeMap<_, _>>();
        crate::orchestration_coupling::validate_orchestration_couplings(
            PROVIDER_ID,
            &plan,
            &entries,
            &ctx,
            for_optimization,
            None,
        )
    }

    fn runtime(&self) -> OrchestrationRuntime<'static> {
        OrchestrationRuntime::new(&self.registries.addins)
    }

    fn geometry_map(ctx: &JsonMap) -> CaeResult<Option<GeometryDesignMap>> {
        from_context(ctx)
    }


    #[allow(clippy::too_many_lines)]
    pub fn mapped_execution(
        &self,
        problem: &Value,
        design: &NamedArrays,
        operation: &str,
        options: MappedOptions<'_>,
    ) -> CaeResult<ExecutionOutput> {
        let (p, plan, mut ctx) = self.parts(problem)?;
        let mapping = Self::geometry_map(&ctx)?;
        let physical = match &mapping {
            None => design.clone(),
            Some(m) => {
                let forward = m.forward(design)?;
                let coordinates = &plan.active_design_coordinates;
                let names = forward.names();
                if names.len() != coordinates.len() || !coordinates.iter().all(|c| names.contains(c)) {
                    return Err(contract(
                        "geometry map outputs do not match the selected physics coordinates",
                    ));
                }
                coordinates.iter().map(|c| (c.clone(), forward.get(c).cloned().unwrap_or_default())).collect()
            }
        };
        if let Some(r) = options.response {
            ctx.insert("response".into(), Value::String(r.to_string()));
        }
        if let Some(rs) = options.responses {
            ctx.insert("responses".into(), json!(rs));
        }
        let context = ExecutionContext::from_json(ctx);
        let runtime = self.runtime();
        let raw = if operation == "cached_evaluation" {
            runtime.cached_evaluation_design(
                &plan,
                &context,
                DesignArg::Named(&physical),
                options.operating_point,
            )?
        } else {
            let mut request = ExecuteRequest::new(operation);
            request.design = Some(DesignArg::Named(&physical));
            request.matching_time_guess = options.matching_time_guess;
            request.require_accepted = options.require_accepted;
            request.result_artifacts = options.result_artifacts;
            runtime.execute(&plan, &context, request)?
        };
        let raw = if operation == "sensitivity" {
            match raw {
                ExecutionOutput::Sensitivity(s) => {
                    if mapping.is_none()
                        && contract_version_or(&p["intent"], 1) == 1
                        && design.names() == [TOPOLOGY_COORDINATE.to_string()]
                    {
                        ExecutionOutput::DesignSensitivity(DesignSensitivity {
                            value: s.value,
                            gradients: NamedArrays::single(TOPOLOGY_COORDINATE, s.gradient),
                            diagnostics: s.diagnostics,
                        })
                    } else {
                        return Err(contract(
                            "intent orchestration did not return all named design gradients",
                        ));
                    }
                }
                other => other,
            }
        } else {
            raw
        };
        let Some(mapping) = mapping else { return Ok(raw) };
        if operation == "export_matching_time_guess" {
            return Ok(raw);
        }
        let evidence = mapping.evidence(design, &physical)?;
        match operation {
            "accept_design" | "install_matching_time_guess" => {
                let ExecutionOutput::Json(ack) = raw else {
                    return Err(contract("mapped lifecycle owner acknowledged a stale physical design"));
                };
                if ack.get("design_state_id") != evidence.get("physical_design_state_id") {
                    return Err(contract("mapped lifecycle owner acknowledged a stale physical design"));
                }
                let mut out = ack.clone();
                out.insert(
                    "design_state_id".into(),
                    evidence.get("design_state_id").cloned().unwrap_or(Value::Null),
                );
                out.insert(
                    "physical_design_state_id".into(),
                    evidence.get("physical_design_state_id").cloned().unwrap_or(Value::Null),
                );
                out.insert("physical_acknowledgement".into(), Value::Object(ack));
                out.insert("geometry_design_map".into(), Value::Object(evidence));
                Ok(ExecutionOutput::Json(out))
            }
            "sensitivity" | "sensitivities" => {
                let reframe = |diag: &JsonMap| -> CaeResult<JsonMap> {
                    let (mut d, _) = mapping.reframe_result(Some(diag), None)?;
                    d.insert(
                        "design_state_id".into(),
                        evidence.get("design_state_id").cloned().unwrap_or(Value::Null),
                    );
                    d.insert("geometry_design_map".into(), Value::Object(evidence.clone()));
                    Ok(d)
                };
                match raw {
                    ExecutionOutput::DesignSensitivity(s) => {
                        Ok(ExecutionOutput::DesignSensitivity(DesignSensitivity {
                            value: s.value,
                            gradients: mapping.pullback(design, &s.gradients)?,
                            diagnostics: reframe(&s.diagnostics)?,
                        }))
                    }
                    ExecutionOutput::Sensitivities(s) => {
                        let mut gradients = BTreeMap::new();
                        for (k, g) in s.gradients {
                            gradients.insert(k, mapping.pullback(design, &g)?);
                        }
                        Ok(ExecutionOutput::Sensitivities(DesignSensitivities {
                            responses: s.responses,
                            gradients,
                            diagnostics: reframe(&s.diagnostics)?,
                        }))
                    }
                    _ => Err(contract("mapped sensitivity requires named complete gradients")),
                }
            }
            _ => match raw {
                ExecutionOutput::Evaluation(e) | ExecutionOutput::Cached(CachedEvaluation::Available(e)) => {
                    let arrays: NamedArrays = e
                        .fields
                        .iter()
                        .filter_map(|(k, v)| match v {
                            FieldValue::Array(a) => Some((k.clone(), a.clone())),
                            FieldValue::Json(_) => None,
                        })
                        .collect();
                    let (mut diagnostics, reframed) =
                        mapping.reframe_result(Some(&e.diagnostics), Some(&arrays))?;
                    diagnostics.insert(
                        "design_state_id".into(),
                        evidence.get("design_state_id").cloned().unwrap_or(Value::Null),
                    );
                    diagnostics.insert("geometry_design_map".into(), Value::Object(evidence));
                    let mut fields: BTreeMap<String, FieldValue> = BTreeMap::new();
                    for (k, v) in &e.fields {
                        if let FieldValue::Json(j) = v {
                            fields.insert(k.clone(), FieldValue::Json(j.clone()));
                        }
                    }
                    for (k, a) in reframed.iter() {
                        fields.insert(k.to_string(), FieldValue::Array(a.clone()));
                    }
                    Ok(ExecutionOutput::Evaluation(Evaluation {
                        provider: e.provider,
                        responses: e.responses,
                        diagnostics,
                        fields,
                    }))
                }
                ExecutionOutput::Json(mut m) => {
                    m.insert("geometry_design_map".into(), Value::Object(evidence));
                    Ok(ExecutionOutput::Json(m))
                }
                other => Ok(other),
            },
        }
    }


    pub fn authoring_coordinates(&self, problem: &Value) -> CaeResult<Vec<String>> {
        let (_p, plan, ctx) = self.parts(problem)?;
        Ok(if Self::geometry_map(&ctx)?.is_some() {
            vec![TOPOLOGY_COORDINATE.to_string()]
        } else {
            plan.active_design_coordinates.clone()
        })
    }


    pub fn design_coordinate_shape(
        &self,
        problem: &Value,
        coordinate: &str,
    ) -> CaeResult<Option<Vec<usize>>> {
        let (_p, _plan, ctx) = self.parts(problem)?;
        Ok(Self::geometry_map(&ctx)?.filter(|_| coordinate == TOPOLOGY_COORDINATE).map(|m| m.control_shape()))
    }


    pub fn analysis_shape(&self, problem: &Value, design: &NamedArrays) -> CaeResult<Vec<usize>> {
        let (_p, _plan, ctx) = self.parts(problem)?;
        Ok(match Self::geometry_map(&ctx)? {
            Some(m) => m.analysis_shape().to_vec(),
            None => design.get(TOPOLOGY_COORDINATE).map(|a| a.shape().to_vec()).unwrap_or_default(),
        })
    }


    pub fn validate_design_model(
        &self,
        problem: &Value,
        node: &implexity_geometry::Node,
        parameter: &str,
    ) -> CaeResult<()> {
        let (_p, _plan, ctx) = self.parts(problem)?;
        if let Some(m) = Self::geometry_map(&ctx)? {
            m.validate_model(node, parameter)?;
        }
        Ok(())
    }


    pub fn lifecycle_for_problem(&self, problem: &Value) -> CaeResult<OptimizerLifecycleConfig> {
        let (_p, plan, ctx) = self.parts(problem)?;
        let snapshot = self.registries.addins.snapshot();
        let entries: BTreeMap<String, _> =
            snapshot.entries.iter().map(|e| (e.contract.addin_id.clone(), Arc::clone(e))).collect();
        let mut owners: Vec<String> = Vec::new();
        for v in plan.response_providers.values() {
            let s = crate::pyval::py_str(v);
            if !owners.contains(&s) {
                owners.push(s);
            }
        }
        if owners.is_empty()
            || owners.iter().any(|a| !plan.selected_addins.contains(a) || !entries.contains_key(a))
        {
            return Err(contract("optimizer lifecycle has invalid selected response owners"));
        }
        let mut commits = Vec::new();
        for aid in &owners {
            let entry = &entries[aid];
            let Some(child) = entry.adapter.as_ref().and_then(|a| a.provider()) else {
                return Err(contract(format!(
                    "provider {} lacks an explicit optional-commit lifecycle declaration",
                    implexity_core::py_repr::repr_str(aid)
                )));
            };
            let child_problem_raw = crate::orchestration_runtime::provider_problem_raw(&ctx, aid);
            let explicit =
                design_operations(child.as_ref()).is_some_and(|o| o.provides(DesignOp::OptimizerLifecycle));
            let operations: Vec<&str> =
                entry.contract.supported_operations.iter().map(String::as_str).collect();
            if !explicit && !operations.contains(&"accept_design") {
                return Err(contract(format!(
                    "provider {} lacks an explicit optional-commit lifecycle declaration",
                    implexity_core::py_repr::repr_str(aid)
                )));
            }
            let child_problem = json_problem(child_problem_raw);
            let config = lifecycle(child.as_ref(), None, None, Some(&child_problem))?;
            match config.acceptance_operation.as_deref() {
                None => {
                    if operations.contains(&"accept_design") {
                        return Err(contract(format!(
                            "provider {} has contradictory acceptance declarations",
                            implexity_core::py_repr::repr_str(aid)
                        )));
                    }
                }
                Some(op) => {
                    if op != "accept_design" || !operations.contains(&"accept_design") {
                        return Err(contract(format!(
                            "provider {} acceptance operation is not declared by its execution contract",
                            implexity_core::py_repr::repr_str(aid)
                        )));
                    }
                    commits.push(aid.clone());
                }
            }
        }
        if commits.len() > 1 {
            return Err(contract("multiple stateful response owners require an atomic commit protocol"));
        }
        if !commits.is_empty()
            && plan
                .selected_addins
                .iter()
                .any(|a| !entries[a].contract.supported_operations.iter().any(|o| o == "accept_design"))
        {
            return Err(contract("selected execution graph does not support accepted-design forwarding"));
        }
        OptimizerLifecycleConfig::new(
            self.authoring_coordinates(problem)?,
            "sensitivity_design",
            "evaluate_design",
            None,
            (!commits.is_empty()).then_some("accept_design"),
            !commits.is_empty(),
            false,
        )
    }


    pub fn preflight_effects_value(&self, problem: &Value) -> CaeResult<Value> {
        let (_p, plan, ctx) = self.parts(problem)?;
        let snapshot = self.registries.addins.snapshot();
        let mut effects: std::collections::BTreeSet<String> =
            std::iter::once("geometry_evaluation".to_string()).collect();
        let mut known = true;
        for owner in &plan.selected_addins {
            let provider = snapshot
                .entries
                .iter()
                .find(|e| &e.contract.addin_id == owner)
                .and_then(|e| e.adapter.as_ref())
                .and_then(|a| a.provider());
            let child = crate::orchestration_runtime::provider_problem_raw(&ctx, owner);
            let declared = match provider
                .as_ref()
                .and_then(|p| design_operations(p.as_ref()))
                .and_then(|o| o.preflight_effects(&child))
            {
                Some(v) => Some(v?),
                None => None,
            };
            let checked = checked_preflight_effects(declared.as_ref())?;
            known = known && checked["status"] == "declared";
            for e in checked["possible_effects"].as_array().into_iter().flatten() {
                effects.insert(crate::pyval::py_str(e));
            }
        }
        Ok(json!({"status": if known { "declared" } else { "unknown" }, "possible_effects": effects}))
    }


    pub fn validate_matching_time_guess_lifecycle(
        &self,
        problem: &Value,
        consume: bool,
        produce: bool,
    ) -> CaeResult<JsonMap> {
        let (_p, plan, _ctx) = self.parts(problem)?;
        self.runtime().validate_matching_time_guess_lifecycle(&plan, consume, produce)
    }


    pub fn numerical_computation_effort_scope(
        &self,
        problem: &Value,
    ) -> CaeResult<Option<crate::provider_job_authority::SelectedEffortScope>> {
        let (_p, plan, _ctx) = self.parts(problem)?;
        crate::provider_job_authority::selected_provider_computation_effort_scope(
            &plan,
            &self.registries.addins,
        )
    }


    pub fn staged_initial_guess(
        &self,
        problem: &Value,
        design: &NamedArrays,
        policy: &Value,
    ) -> CaeResult<ProviderScope> {
        let (_p, plan, ctx) = self.parts(problem)?;
        let mut owners: Vec<String> = Vec::new();
        for v in plan.response_providers.values() {
            let s = crate::pyval::py_str(v);
            if !owners.contains(&s) {
                owners.push(s);
            }
        }
        if owners.len() != 1 {
            return Err(contract("staged initialization requires exactly one selected response owner"));
        }
        let owner = &owners[0];
        if !plan.selected_addins.contains(owner) {
            return Err(contract(format!(
                "staged initialization owner {} is not selected",
                implexity_core::py_repr::repr_str(owner)
            )));
        }
        let snapshot = self.registries.addins.snapshot();
        let provider = snapshot
            .entries
            .iter()
            .find(|e| &e.contract.addin_id == owner)
            .and_then(|e| e.adapter.as_ref())
            .and_then(|a| a.provider());
        let Some(provider) = provider.filter(|p| p.provider_id().unwrap_or_else(|| p.name()) == owner) else {
            return Err(contract(format!(
                "staged initialization owner {} is unavailable or mismatched",
                implexity_core::py_repr::repr_str(owner)
            )));
        };
        let problems = match ctx.get("provider_problems") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(v) if !crate::pyval::truthy(Some(v)) => Map::new(),
            Some(_) => return Err(contract("provider problem routing is malformed")),
        };
        let provider_problem_raw = problems
            .get(owner)
            .or_else(|| ctx.get("problem"))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let physical = match Self::geometry_map(&ctx)? {
            Some(m) => m.forward(design)?,
            None => design.clone(),
        };
        let provider_problem = json_problem(provider_problem_raw);
        let Some(scope) = design_operations(provider.as_ref())
            .and_then(|o| o.staged_initial_guess_scope(&provider_problem, &physical, policy))
        else {
            return Ok(ProviderScope { value: None, guard: None });
        };
        let scope = scope?;
        let Some(report) = &scope.value else { return Ok(scope) };
        let Some(r) = report.as_object() else {
            return Err(contract("staged initialization owner returned no public trace"));
        };
        let final_row = r.get("restoration_schedule").and_then(Value::as_array).and_then(|s| s.last());
        let safe = r.get("schema").and_then(Value::as_str) == Some("implexity-optimization-preview-trace/1")
            && r.get("authoritative") == Some(&Value::Bool(false))
            && r.get("correction_required") == Some(&Value::Bool(true))
            && final_row.is_some_and(|f| {
                f.get("stage").and_then(Value::as_str) == Some("exact_correction")
                    && f.get("lagged_coupling_ids") == Some(&json!([]))
                    && f.get("inactive_coupling_ids") == Some(&json!([]))
            });
        if !safe {
            return Err(contract("staged initialization owner returned an unsafe restoration trace"));
        }
        Ok(scope)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MappedOptions<'a> {
    pub response: Option<&'a str>,
    pub responses: Option<&'a [String]>,
    pub operating_point: usize,
    pub matching_time_guess: Option<&'a MatchingTimeNewtonGuess>,
    pub require_accepted: bool,
    pub result_artifacts: bool,
}

fn contract_version(intent: &Value) -> Option<i64> {
    intent.get("contract_version").or_else(|| intent.get("contractVersion")).and_then(Value::as_i64)
}

fn contract_version_or(intent: &Value, default: i64) -> i64 {
    intent.get("contract_version").map_or(Some(default), Value::as_i64).unwrap_or(i64::MIN)
}

fn get<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut value = root;
    for key in path {
        value = value.as_object()?.get(*key)?;
    }
    Some(value)
}


pub fn reconcile_provider_problem_aliases(previous: &Value, revised: &Value) -> CaeResult<Value> {
    let paths: [&[&str]; 3] = [
        &["context", "provider_problems"],
        &["intent", "authoring", "provider_problems"],
        &["context", "authoring", "provider_problems"],
    ];
    let equal = |a: Option<&Value>, b: Option<&Value>| -> bool {
        match (a, b) {
            (Some(a), Some(b)) => implexity_core::pyobj::py_eq(a, b),
            _ => false,
        }
    };
    let mut result = revised.clone();
    let present: Vec<(&[&str], Value)> =
        paths.iter().filter_map(|p| get(&result, p).map(|v| (*p, v.clone()))).collect();
    if present.is_empty() {
        return Ok(result);
    }
    if present.iter().any(|(_, v)| !v.is_object()) {
        return Err(contract("provider_problems authoring copies must be objects"));
    }
    let changed: Vec<&(&[&str], Value)> =
        present.iter().filter(|(p, v)| !equal(Some(v), get(previous, p))).collect();
    let candidates: Vec<&(&[&str], Value)> =
        if changed.is_empty() { present.iter().take(1).collect() } else { changed };
    let chosen = candidates[0].1.clone();
    if candidates[1..].iter().any(|(_, v)| !equal(Some(&chosen), Some(v))) {
        return Err(contract(
            "CONFLICTING_PHYSICS_AUTHORING: different provider-problem copies were edited inconsistently; edit one copy or supply the same declaration in all copies",
        ));
    }
    for path in paths {
        let mut target = &mut result;
        for key in &path[..path.len() - 1] {
            let Some(m) = target.as_object_mut() else {
                return Err(contract("provider-problem authoring containers must be objects"));
            };
            let entry = m.entry((*key).to_string()).or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                return Err(contract("provider-problem authoring containers must be objects"));
            }
            target = entry;
        }
        if let Some(m) = target.as_object_mut() {
            m.insert(path[path.len() - 1].to_string(), chosen.clone());
        }
    }
    Ok(result)
}

impl CaeProvider for IntentOrchestratedProvider {
    fn name(&self) -> &str {
        PROVIDER_ID
    }
    fn provider_id(&self) -> Option<&str> {
        Some(PROVIDER_ID)
    }
    fn implementation(&self) -> &'static str {
        "implexity.cae.providers.intent_orchestrated.IntentOrchestratedProvider"
    }
    fn orchestration_meta(&self) -> bool {
        true
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        self.sync()?;
        let snapshot = self.registries.addins.snapshot();
        let mut responses: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut coordinates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for e in &snapshot.entries {
            responses.extend(e.contract.responses.iter().map(|r| r.response.clone()));
            coordinates.extend(e.contract.design_inputs.iter().map(|r| r.coordinate.clone()));
        }
        let mut d = ProviderDescriptor::new(
            PROVIDER_ID,
            vec!["intent_composed_multiphysics".into()],
            responses.into_iter().collect(),
        );
        d.sensitivities = true;
        d.traits.insert(
            "editor".into(),
            json!({"kind": "native_json", "title": "Coupled physics setup",
                   "authoring_fields": ["intent", "context"],
                   "authoring_aliases": [
                       {"path": ["intent", "authoring", "provider_problems"],
                        "source": ["context", "provider_problems"]},
                       {"path": ["context", "authoring", "provider_problems"],
                        "source": ["context", "provider_problems"]}],
                   "authoring_notes": "Edit provider problems once under Context. The provider synchronizes the planning copies when applying a revision. Conflicting changes to different copies are rejected."}),
        );
        d.design_coordinates = coordinates.into_iter().collect();
        d.nonlinear = true;
        d.notes = vec![
            "Kernel meta-provider: numerical physics is supplied by the selected installed add-ins.".into(),
        ];
        Ok(ProviderCapabilities::Descriptor(Box::new(d.checked()?)))
    }
    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Ok(json_problem(self.normalise_value(problem)?))
    }
    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let value =
            problem_json(problem).ok_or_else(|| contract("orchestrated problem must be an object"))?;
        self.preflight_value(value, topology.is_some())
    }
    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        let (p, plan, ctx) = self.parts_of(problem)?;
        if contract_version(&p["intent"]) == Some(2) {
            return Err(contract("bare-array evaluation is compatibility-only"));
        }
        let topo = implexity_optim::numeric::array_to_value(topology);
        let mut request = ExecuteRequest::new("evaluate");
        request.topology = Some(DesignArg::Json(&topo));
        match self.runtime().execute(&plan, &ExecutionContext::from_json(ctx), request)? {
            ExecutionOutput::Evaluation(e) => Ok(e),
            _ => Err(contract("intent orchestration did not return an evaluation")),
        }
    }
    fn sensitivity(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        let (p, plan, mut ctx) = self.parts_of(problem)?;
        ctx.insert("response".into(), Value::String(response.into()));
        if contract_version(&p["intent"]) == Some(2) {
            return Err(contract("bare-array sensitivity is compatibility-only"));
        }
        let topo = implexity_optim::numeric::array_to_value(topology);
        let mut request = ExecuteRequest::new("sensitivity");
        request.topology = Some(DesignArg::Json(&topo));
        match self.runtime().execute(&plan, &ExecutionContext::from_json(ctx), request)? {
            ExecutionOutput::Sensitivity(s) => Ok(s),
            _ => Err(contract("intent orchestration did not return a sensitivity")),
        }
    }
    fn coupling_validation(
        &self,
        problem: Option<&ProviderProblem>,
        for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        let value = problem.and_then(problem_json).cloned().unwrap_or(Value::Null);
        Some(
            self.coupling_report(&value, for_optimization)
                .map(Value::Object)
                .map_err(|e| e.message().to_string()),
        )
    }
    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        design_interface::<Self>(name)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn value_of(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem_json(problem).ok_or_else(|| contract("orchestrated problem must be an object"))
}

impl DesignOperations for IntentOrchestratedProvider {
    fn provides(&self, op: DesignOp) -> bool {
        !matches!(
            op,
            DesignOp::SensitivityMany
                | DesignOp::CandidateDesignAdmission
                | DesignOp::CandidateAdmission
                | DesignOp::OnTopologyEventAccepted
                | DesignOp::ProjectTopology
                | DesignOp::PhysicalCertification
                | DesignOp::EvaluateWithoutDesign
        )
    }
    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _op: usize,
    ) -> CaeResult<Evaluation> {
        match self.mapped_execution(value_of(problem)?, design, "evaluate", MappedOptions::default())? {
            ExecutionOutput::Evaluation(e) => Ok(e),
            other => {
                Err(contract(format!("intent orchestration returned {} for evaluate_design", other.kind())))
            }
        }
    }
    fn evaluate_results_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Evaluation> {
        let options = MappedOptions { result_artifacts: true, ..MappedOptions::default() };
        match self.mapped_execution(value_of(problem)?, design, "evaluate", options)? {
            ExecutionOutput::Evaluation(e) => Ok(e),
            other => Err(contract(format!(
                "intent orchestration returned {} for evaluate_results_design",
                other.kind()
            ))),
        }
    }
    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let value = value_of(problem)?;
        let report = self.preflight_value(value, false)?;
        if !crate::pyval::truthy(report.get("ok")) {
            return Ok(report);
        }
        self.mapped_execution(value, design, "preflight_design", MappedOptions::default())?
            .into_json("preflight_design")
    }
    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _op: usize,
    ) -> CaeResult<DesignSensitivity> {
        let options = MappedOptions { response: Some(response), ..MappedOptions::default() };
        match self.mapped_execution(value_of(problem)?, design, "sensitivity", options)? {
            ExecutionOutput::DesignSensitivity(s) => Ok(s),
            _ => Err(contract("intent orchestration did not return all named design gradients")),
        }
    }
    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _op: usize,
    ) -> CaeResult<DesignSensitivities> {
        let options = MappedOptions { responses: Some(responses), ..MappedOptions::default() };
        match self.mapped_execution(value_of(problem)?, design, "sensitivities", options)? {
            ExecutionOutput::Sensitivities(s) => Ok(s),
            other => Err(contract(format!(
                "intent orchestration returned {} for sensitivities_design",
                other.kind()
            ))),
        }
    }
    fn accept_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Option<Map<String, Value>>> {
        self.mapped_execution(value_of(problem)?, design, "accept_design", MappedOptions::default())?
            .into_json("accept_design")
            .map(Some)
    }
    fn optimizer_lifecycle(&self, problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        let problem =
            problem.ok_or_else(|| contract("optimizer lifecycle requires the orchestrated problem"))?;
        self.lifecycle_for_problem(value_of(problem)?).map(LifecycleDeclaration::Typed)
    }
    fn lifecycle_is_problem_specific(&self) -> bool {
        true
    }
    fn install_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        guess: &MatchingTimeNewtonGuess,
    ) -> CaeResult<Map<String, Value>> {
        let options = MappedOptions { matching_time_guess: Some(guess), ..MappedOptions::default() };
        self.mapped_execution(value_of(problem)?, design, "install_matching_time_guess", options)?
            .into_json("install_matching_time_guess")
    }
    fn export_matching_time_guess(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        require_accepted: bool,
    ) -> CaeResult<MatchingTimeNewtonGuess> {
        let options = MappedOptions { require_accepted, ..MappedOptions::default() };
        match self.mapped_execution(value_of(problem)?, design, "export_matching_time_guess", options)? {
            ExecutionOutput::Guess(g) => Ok(g),
            _ => Err(contract("intent orchestration did not export a typed matching-time guess")),
        }
    }
    fn authoring_design_coordinates(&self, problem: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(value_of(problem).and_then(|v| self.authoring_coordinates(v)))
    }
    fn preflight_effects(&self, problem: &Value) -> Option<CaeResult<Value>> {
        Some(self.preflight_effects_value(problem))
    }
    fn cached_evaluation_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        operating_point: usize,
    ) -> Option<CaeResult<CachedEvaluation>> {
        let options = MappedOptions { operating_point, ..MappedOptions::default() };
        Some(
            value_of(problem)
                .and_then(|v| self.mapped_execution(v, design, "cached_evaluation", options))
                .and_then(|out| match out {
                    ExecutionOutput::Cached(c) => Ok(c),
                    ExecutionOutput::Evaluation(e) => Ok(CachedEvaluation::Available(e)),
                    ExecutionOutput::Json(m) => Ok(CachedEvaluation::Unavailable(m)),
                    other => Err(contract(format!("cached evaluation returned {}", other.kind()))),
                }),
        )
    }
    fn replan_problem_revision(&self, previous: &Value, revised: &Value) -> Option<CaeResult<Value>> {
        Some(IntentOrchestratedProvider::replan_problem_revision(self, previous, revised))
    }
    fn has_computation_effort_scope(&self) -> bool {
        true
    }
    fn computation_effort_scope(&self, binding: &Value) -> Option<CaeResult<ProviderScope>> {
        Some(
            crate::provider_job_authority::orchestration_computation_effort_scope(binding)
                .map(|guard| ProviderScope { value: None, guard: Some(Box::new(guard)) }),
        )
    }
    fn staged_initial_guess_scope(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        policy: &Value,
    ) -> Option<CaeResult<ProviderScope>> {
        Some(value_of(problem).and_then(|v| self.staged_initial_guess(v, design, policy)))
    }
}
