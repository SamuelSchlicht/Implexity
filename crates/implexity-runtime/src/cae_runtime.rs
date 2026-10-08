// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, CoordinateOptimizationSettings, LegacySingleArrayOptimizationSettings, ProviderCapabilities,
    ProviderProblem, ResponseSpec, TOPOLOGY_COORDINATE,
};
use implexity_core::coupling_graph::validate_provider_couplings;
use implexity_core::mathematical_contracts::validate_provider_mathematics;
use implexity_core::numeric_contract::real_array;
use implexity_core::orchestration::{EngineeringIntent, ExternalPortValue, OrchestrationPlanner};
use implexity_core::packages::PackageManager;
use implexity_core::py_repr::repr_str;
use implexity_core::registries::Registries;
use implexity_core::semantic_physics::validate_provider_semantics;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design_freedom::validate_blocks;
use implexity_optim::design_state::{
    NormaliseOptions, normalise_design, provider_evaluate, provider_sensitivity,
};
use implexity_optim::numeric::{array_to_value, float_value};
use implexity_optim::optimizer::{ProgressCallback, optimise, optimise_design};
use implexity_optim::provider_ops::{DesignOp, design_operations, provides};
use implexity_optim::regime::validate_schedule;
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::addin::JsonMap;
use crate::orchestration_runtime::{DesignArg, ExecuteRequest, ExecutionContext, OrchestrationRuntime};
use crate::problem::{PreparedProblem, normalise};
use crate::results::{ExecutionOutput, evaluation_value, fields_value, responses_value};

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message.into())
}

fn joined_messages(report: &Value) -> String {
    report
        .get("errors")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|e| e.get("message").map_or_else(|| crate::pyval::py_str(e), crate::pyval::py_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

type Admission = (Value, Option<Value>, Option<Value>);

#[derive(Debug, Clone, Copy)]
pub struct CaeRuntime {
    registries: &'static Registries,
    packages: &'static PackageManager,
}

impl Default for CaeRuntime {
    fn default() -> Self {
        Self {
            registries: implexity_core::registries::global(),
            packages: implexity_core::packages::global(),
        }
    }
}


pub fn catalogue_traits(
    name: &str,
    caps: &ProviderCapabilities,
    provider: &Arc<dyn CaeProvider>,
    traits: &mut Map<String, Value>,
) -> CaeResult<()> {
    traits.insert(
        "coupling_control".into(),
        crate::coupling_control::normalise_provider_coupling_control(
            name,
            caps,
            Some(provider.as_ref()),
            None,
        )?,
    );
    traits.insert(
        "boundary_control".into(),
        implexity_solve::boundary_control::normalise_provider_boundary_control(name, caps, provider)?,
    );
    Ok(())
}

impl CaeRuntime {
    #[must_use]
    pub fn new(registries: &'static Registries, packages: &'static PackageManager) -> Self {
        Self { registries, packages }
    }

    fn admission(
        &self,
        provider: &dyn CaeProvider,
        problem: &ProviderProblem,
        for_optimization: bool,
    ) -> CaeResult<Admission> {
        let coupling = validate_provider_couplings(
            provider,
            Some(problem),
            &self.registries.extensions,
            for_optimization,
        );
        if coupling.get("ok") != Some(&Value::Bool(true)) {
            return Err(contract(format!(
                "multiphysics coupling admission failed: {}",
                joined_messages(&coupling)
            )));
        }
        let math = validate_provider_mathematics(provider, Some(problem), for_optimization)?;
        let semantic = validate_provider_semantics(provider, Some(problem), for_optimization)?;
        let caps = provider.capabilities()?;
        let structures =
            caps.get("mathematical_structures").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        if !structures.is_empty() && math.is_none() {
            return Err(contract(
                "provider advertises advanced mathematical structures but has no mathematical_declaration",
            ));
        }
        if let Some(m) = &math
            && m.get("ok") != Some(&Value::Bool(true))
        {
            return Err(contract(format!("mathematical-structure admission failed: {}", joined_messages(m))));
        }
        if let Some(s) = &semantic
            && s.get("ok") != Some(&Value::Bool(true))
        {
            return Err(contract(format!("semantic-physics admission failed: {}", joined_messages(s))));
        }
        Ok((coupling, math, semantic))
    }


    pub fn catalogue(&self) -> CaeResult<Value> {
        self.packages.guard(|| {
            let providers = self.registries.providers.catalogue(&self.registries.addins, Some(&catalogue_traits))?;
            Ok(json!({
                "schema": "implexity-implicit-cae-catalogue/3",
                "provider_api": 1,
                "authoritative_gui": "/viewer/model.html",
                "authoritative_optimisation_runtime": "implicit_job",
                "array_optimiser_role": "legacy compatibility/reference path; not a second product job system",
                "providers": providers,
                "physics_addins": self.registries.addins.catalogue(),
                "intent_orchestration": {
                    "enabled": true,
                    "principle": "Responses and constraints select installed physics add-ins; the kernel closes compatible couplings and exact named-design derivative paths automatically.",
                },
                "compatibility": {
                    "legacy_single_array_coordinate": "model:control",
                    "engineering_catalogue": "/v1/implicit/engineering",
                    "implicit_problem": "/v1/implicit/problem",
                    "implicit_optimise": "/v1/implicit/optimize",
                    "implicit_jobs": "/v1/implicit/optimize/jobs",
                },
            }))
        })
    }


    pub fn plan_intent(&self, intent: &Value) -> CaeResult<Map<String, Value>> {
        self.packages.guard(|| {
            self.registries.providers.catalogue(&self.registries.addins, Some(&catalogue_traits))?;
            let parsed = EngineeringIntent::from_mapping(intent)?;
            let snapshot = self.registries.addins.snapshot();
            let planner = OrchestrationPlanner;
            let mut out = planner.plan_snapshot(&parsed, &snapshot, &self.registries.sufficiency)?.as_dict();
            let ladder = planner.plan_fidelity_ladder(&parsed, &snapshot, &self.registries.sufficiency)?;
            if let Value::Object(m) = &mut out {
                m.insert("fidelity_ladder".into(), Value::Object(ladder));
            }
            crate::execution_readiness::inspect_execution(
                intent,
                out.as_object().unwrap_or(&Map::new()),
                None,
                &self.registries.addins,
            )
        })
    }


    #[allow(clippy::too_many_arguments)]
    pub fn orchestrated_execute<'a>(
        &self,
        intent: &Value,
        operation: &'a str,
        topology: Option<&'a Value>,
        design: Option<&'a Value>,
        context: Option<&JsonMap>,
        responses: Option<Vec<ResponseSpec>>,
        settings: Option<&'a Value>,
        callback: Option<ProgressCallback<'a>>,
    ) -> CaeResult<ExecutionOutput> {
        self.packages.guard(|| {
            self.registries.providers.catalogue(&self.registries.addins, Some(&catalogue_traits))?;
            let parsed = EngineeringIntent::from_mapping(intent)?;
            let snapshot = self.registries.addins.snapshot();
            let plan = Arc::new(OrchestrationPlanner.plan_snapshot(
                &parsed,
                &snapshot,
                &self.registries.sufficiency,
            )?);
            let mut ctx = context.cloned().unwrap_or_default();
            ctx.entry("authoring").or_insert_with(|| Value::Object(parsed.authoring.clone()));
            let typed_ports: Vec<ExternalPortValue> = parsed.external_ports.clone();

            let external_is_ours = !ctx.contains_key("external_port_values");
            ctx.entry("external_port_values").or_insert_with(|| {
                Value::Array(typed_ports.iter().map(crate::results::external_port_wire).collect())
            });
            if !ctx.contains_key("provider_problems")
                && let Some(Value::Object(pp)) = parsed.authoring.get("provider_problems")
            {
                ctx.insert("provider_problems".into(), Value::Object(pp.clone()));
            }
            for key in ["quantities", "boundary_conditions", "states", "external_quantities"] {
                if let Some(Value::Object(row)) = parsed.authoring.get(key)
                    && !ctx.contains_key(key)
                {
                    ctx.insert(key.into(), Value::Object(row.clone()));
                }
            }
            let plan_dict = plan.as_dict().as_object().cloned().unwrap_or_default();
            let admission = crate::execution_readiness::inspect_execution(
                intent,
                &plan_dict,
                Some(&ctx),
                &self.registries.addins,
            )?;
            if admission.get("execution_status").and_then(Value::as_str) == Some("blocked") {

                let detail = [
                    ("execution_issues", admission.get("execution_issues")),
                    ("blocked_reasons", admission.get("blocked_reasons")),
                    ("missing_physics", admission.get("missing_physics")),
                ]
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v)))
                .find(|(_, v)| crate::pyval::truthy(Some(v)))
                .or_else(|| admission.get("missing_physics").map(|v| ("missing_physics", v)))
                .map_or_else(
                    || "None".to_string(),
                    |(k, v)| match v {
                        Value::Array(items) if k != "execution_issues" => {
                            let parts: Vec<String> = items.iter().map(crate::pyval::py_repr_value).collect();
                            if parts.len() == 1 {
                                format!("({},)", parts[0])
                            } else {
                                format!("({})", parts.join(", "))
                            }
                        }
                        other => crate::pyval::py_repr_value(other),
                    },
                );
                return Err(contract(format!("execution is not admissible: {detail}")));
            }
            let context = ExecutionContext {
                values: ctx,
                external_ports: external_is_ours.then_some(typed_ports),
                design_port_bindings: None,
            };
            let mut request = ExecuteRequest::new(operation);
            request.topology = topology.filter(|v| !v.is_null()).map(DesignArg::Json);
            request.design = design.filter(|v| !v.is_null()).map(DesignArg::Json);
            request.responses = responses;
            request.settings = settings;
            request.callback = callback;
            OrchestrationRuntime::new(&self.registries.addins).execute(&plan, &context, request)
        })
    }


    pub fn prepare(&self, declaration: &Value) -> CaeResult<PreparedProblem> {
        self.packages.guard(|| normalise(declaration, self.registries))
    }

    fn legacy(provider: &dyn CaeProvider) -> CaeResult<bool> {
        Ok(matches!(provider.capabilities()?, ProviderCapabilities::Legacy(_)))
    }

    fn named_hint(provider: &dyn CaeProvider, operation: &str) -> CaeResult<String> {
        let op = if operation == "evaluate" { DesignOp::EvaluateDesign } else { DesignOp::SensitivityDesign };
        if !provides(provider, op) {
            return Ok(String::new());
        }
        let coordinate = match provider.capabilities()? {
            ProviderCapabilities::Legacy(l) => l.topology_coordinate,
            other => other
                .get("topology_coordinate")
                .map_or_else(|| TOPOLOGY_COORDINATE.to_string(), |v| crate::pyval::py_str(&v)),
        };
        Ok(format!(
            "; this provider implements {operation}_design -- pass a named design mapping such as {{{}: {{'value': ..., 'lower': ..., 'upper': ...}}}} instead of a bare array",
            repr_str(&coordinate)
        ))
    }

    fn require_legacy(provider: &dyn CaeProvider, provider_id: &str, operation: &str) -> CaeResult<()> {
        let op = if operation == "evaluate" { DesignOp::Evaluate } else { DesignOp::Sensitivity };
        if provides(provider, op) {
            return Ok(());
        }
        Err(contract(format!(
            "provider {} has no single-array {operation}() operation{}",
            repr_str(provider_id),
            Self::named_hint(provider, operation)?
        )))
    }

    fn require_empty_design(value: Option<&Value>) -> CaeResult<()> {
        match value {
            None | Some(Value::Null) => Ok(()),
            Some(Value::Object(m)) if m.is_empty() => Ok(()),
            Some(_) => Err(contract(
                "evaluation-only provider accepts no design coordinates; pass null or an empty mapping",
            )),
        }
    }

    fn provider_label(provider: &dyn CaeProvider, declared: &str) -> String {
        provider
            .provider_id()
            .filter(|s| !s.is_empty())
            .or_else(|| Some(provider.name()).filter(|s| !s.is_empty()))
            .unwrap_or(declared)
            .to_string()
    }

    fn state(p: &PreparedProblem, topology: &Value) -> CaeResult<implexity_optim::design_state::DesignState> {
        normalise_design(topology, &NormaliseOptions::named(&p.design_coordinates))
    }


    #[allow(clippy::too_many_lines)]
    pub fn preflight(&self, declaration: &Value, topology: Option<&Value>) -> CaeResult<Map<String, Value>> {
        self.packages.guard(|| {
            let p = normalise(declaration, self.registries)?;
            let provider = p.provider.as_ref();
            let hierarchy = declaration.get("design_freedom").filter(|v| crate::pyval::truthy(Some(v)));
            let schedule = declaration.get("schedule").filter(|v| crate::pyval::truthy(Some(v)));
            if hierarchy.is_some() || schedule.is_some() {
                let raw_blocks =
                    hierarchy.and_then(|h| h.get("blocks")).and_then(Value::as_array).map(Vec::as_slice);
                let blocks = validate_blocks(raw_blocks, &p.design_coordinates)?;
                let op_count = p
                    .problem_raw
                    .get("mission")
                    .and_then(|m| m.get("operatingPoints").or_else(|| m.get("operating_points")))
                    .and_then(Value::as_array)
                    .map(Vec::len);
                let stages = schedule.and_then(Value::as_array).map(Vec::as_slice);
                validate_schedule(stages, &p.provider_name, &blocks, &p.design_coordinates, op_count)?;
            }
            let (coupling, math, semantic) = self.admission(provider, &p.problem, false)?;
            let mut report = if p.design_coordinates.is_empty() {
                Self::require_empty_design(topology)?;
                provider.preflight(&p.problem, None)?
            } else if Self::legacy(provider)? {
                let design: Option<ArrayD<f64>> = match topology {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(m)) => {
                        let coordinate = match provider.capabilities()? {
                            ProviderCapabilities::Legacy(l) => l.topology_coordinate,
                            _ => TOPOLOGY_COORDINATE.to_string(),
                        };
                        let mut raw = m.get(&coordinate).cloned();
                        if let Some(Value::Object(inner)) = &raw {
                            raw = inner.get("value").or_else(|| inner.get("values")).cloned();
                        }
                        match raw {
                            None | Some(Value::Null) => None,
                            Some(v) => Some(real_array(&v, "topology")?),
                        }
                    }
                    Some(v) => Some(real_array(v, "topology")?),
                };
                provider.preflight(&p.problem, design.as_ref())?
            } else {
                match topology {
                    None | Some(Value::Null) => provider.preflight(&p.problem, None)?,
                    Some(t) => {
                        let state = Self::state(&p, t)?;
                        let Some(ops) =
                            design_operations(provider).filter(|o| o.provides(DesignOp::PreflightDesign))
                        else {
                            return Err(contract("canonical provider omitted preflight_design"));
                        };
                        ops.preflight_design(&p.problem, &state.values())?
                    }
                }
            };
            report.insert("couplingReport".into(), coupling);
            if let Some(m) = math {
                report.insert("mathematicalStructureReport".into(), m);
            }
            if let Some(s) = semantic {
                report.insert("semanticPhysicsReport".into(), s);
            }
            Ok(report)
        })
    }


    pub fn evaluate(&self, declaration: &Value, topology: Option<&Value>) -> CaeResult<Map<String, Value>> {
        self.packages.guard(|| {
            let p = normalise(declaration, self.registries)?;
            let provider = p.provider.as_ref();
            self.admission(provider, &p.problem, false)?;
            let e = if p.design_coordinates.is_empty() {
                Self::require_empty_design(topology)?;
                let Some(ops) =
                    design_operations(provider).filter(|o| o.provides(DesignOp::EvaluateWithoutDesign))
                else {
                    return Err(contract(format!(
                        "provider {} has no design-free evaluate() operation",
                        repr_str(&p.provider_name)
                    )));
                };
                ops.evaluate_without_design(&p.problem)?
            } else if topology.is_some_and(Value::is_object) || !Self::legacy(provider)? {
                let Some(t @ Value::Object(_)) = topology else {
                    return Err(contract("canonical evaluation requires a named design mapping"));
                };
                let state = Self::state(&p, t)?;
                let e = provider_evaluate(provider, &p.problem, &state)?;
                let mut out = Map::new();
                out.insert("schema".into(), Value::String("implexity-implicit-cae-evaluation/2".into()));
                out.insert(
                    "provider".into(),
                    Value::String(Self::provider_label(provider, &p.provider_name)),
                );
                out.insert("responses".into(), responses_value(&e.responses));
                out.insert("diagnostics".into(), Value::Object(e.diagnostics));
                out.insert("fields".into(), fields_value(&e.fields));
                out.insert("design_coordinates".into(), json!(p.design_coordinates));
                return Ok(out);
            } else {
                Self::require_legacy(provider, &p.provider_name, "evaluate")?;
                let topo = real_array(topology.unwrap_or(&Value::Null), "topology")?;
                provider.evaluate(&p.problem, &topo)?
            };
            let mut out = evaluation_value(Some("implexity-implicit-cae-evaluation/2"), &e);
            out.insert("design_coordinates".into(), json!(p.design_coordinates));
            Ok(out)
        })
    }


    pub fn sensitivity(
        &self,
        declaration: &Value,
        topology: Option<&Value>,
        response: &str,
    ) -> CaeResult<Map<String, Value>> {
        self.packages.guard(|| {
            let p = normalise(declaration, self.registries)?;
            let provider = p.provider.as_ref();
            if p.design_coordinates.is_empty() {
                return Err(contract("evaluation-only provider has no sensitivity operation"));
            }
            self.admission(provider, &p.problem, true)?;
            if topology.is_some_and(Value::is_object) || !Self::legacy(provider)? {
                let Some(t @ Value::Object(_)) = topology else {
                    return Err(contract("canonical sensitivity requires a named design mapping"));
                };
                let state = Self::state(&p, t)?;
                let s = provider_sensitivity(provider, &p.problem, &state, response)?;
                let mut out = Map::new();
                out.insert("schema".into(), Value::String("implexity-implicit-cae-sensitivity/2".into()));
                out.insert(
                    "provider".into(),
                    Value::String(Self::provider_label(provider, &p.provider_name)),
                );
                out.insert("response".into(), Value::String(response.into()));
                out.insert("value".into(), float_value(s.value));
                out.insert("gradients".into(), s.gradients.to_wire());
                out.insert("diagnostics".into(), Value::Object(s.diagnostics));
                out.insert("design_coordinates".into(), json!(p.design_coordinates));
                return Ok(out);
            }
            Self::require_legacy(provider, &p.provider_name, "sensitivity")?;
            let topo = real_array(topology.unwrap_or(&Value::Null), "topology")?;
            let s = provider.sensitivity(&p.problem, &topo, response)?;
            let mut out = Map::new();
            out.insert("schema".into(), Value::String("implexity-implicit-cae-sensitivity/1".into()));
            out.insert("provider".into(), Value::String(s.provider));
            out.insert("response".into(), Value::String(s.response));
            out.insert("value".into(), float_value(s.value));
            out.insert("gradient".into(), array_to_value(&s.gradient));
            out.insert("diagnostics".into(), Value::Object(s.diagnostics));
            out.insert("topology_coordinate".into(), Value::String(TOPOLOGY_COORDINATE.into()));
            Ok(out)
        })
    }


    pub fn optimize(
        &self,
        declaration: &Value,
        topology: Option<&Value>,
        settings: Option<&Value>,
        callback: Option<ProgressCallback<'_>>,
    ) -> CaeResult<Map<String, Value>> {
        self.packages.guard(|| {
            let p = normalise(declaration, self.registries)?;
            let provider = p.provider.as_ref();
            if p.design_coordinates.is_empty() {
                return Err(contract("evaluation-only provider has no optimization operation"));
            }
            self.admission(provider, &p.problem, true)?;
            if topology.is_some_and(Value::is_object) || !Self::legacy(provider)? {
                let Some(t @ Value::Object(_)) = topology else {
                    return Err(contract("canonical optimization requires a named design mapping"));
                };
                let cfg = CoordinateOptimizationSettings::from_dict(settings)?;
                let run = optimise_design(
                    provider,
                    &p.problem,
                    t,
                    &p.responses,
                    &cfg,
                    Some(&p.design_coordinates),
                    None,
                    callback,
                )?;
                return Ok(run.record);
            }
            let cfg = LegacySingleArrayOptimizationSettings::from_dict(settings)?;
            let topo = real_array(topology.unwrap_or(&Value::Null), "topology")?;
            Ok(optimise(provider, &p.problem, &topo, &p.responses, &cfg, callback)?.record)
        })
    }
}
