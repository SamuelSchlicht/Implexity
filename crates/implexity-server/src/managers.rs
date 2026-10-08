// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use implexity_agent::error::{AgentError, AgentResult};
use implexity_agent::host::{
    AgentManagers, AgentModel, HostOp, ManagedCapsule, ManagedControl, ManagedStatus, ManagedSupervisor,
};
use implexity_authoring::error::AuthoringError;
use implexity_authoring::interaction_runtime::InteractionRuntime;
use implexity_authoring::model_manager::DetachedModelView;
use implexity_authoring::services::{Authoring, NativeStores, RouteCtx};
use implexity_jobs::error::JobError;
use implexity_jobs::managed_evaluation::{ManagedEvaluationControl, ManagedEvaluationManager};
use implexity_jobs::manager::ModelOptimizeManager;
use implexity_jobs::manager::evaluation::ManagedProviderEvaluationCapsule;
use implexity_jobs::result_arrays::ArrayReadError;
use implexity_mesh::MeshError;
use implexity_mesh::model_view::{
    Evaluated, LiveGuard, ModelStatus, ModelView, RegisteredGrid, SamplingHint,
};
use serde_json::{Map, Value, json};

use crate::service::Service;

fn job(e: &JobError) -> AgentError {
    if let Some(report) = e.solver_recovery() { return AgentError::SolverRecovery { message: e.message(), report: report.clone() }; }
    let message = e.message();
    if e.python_class() == "KeyError" {
        AgentError::KeyError(message)
    } else if matches!(e, JobError::Cae(_)) || e.is_value_error() {
        AgentError::Contract(message)
    } else {
        AgentError::Failed(message)
    }
}

fn auth(e: AuthoringError) -> AgentError {
    job(&JobError::from(e))
}

fn geo(e: implexity_geometry::GeometryError) -> AgentError {
    job(&JobError::from(e))
}

fn type_error(what: &str) -> AgentError {
    AgentError::failed(format!("'{what}' object is not a mapping"))
}

fn object(v: &Value) -> AgentResult<Map<String, Value>> {
    v.as_object().cloned().ok_or_else(|| type_error(python_type(v)))
}

fn python_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn text(v: &Value) -> String {
    implexity_core::pyobj::py_str(v)
}

pub struct KernelManagers {
    service: Weak<Service>,
    supervisors: Arc<Mutex<Vec<Arc<Supervisor>>>>,
}

impl std::fmt::Debug for KernelManagers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelManagers").finish_non_exhaustive()
    }
}

impl KernelManagers {
    #[must_use]
    pub fn new(service: &Arc<Service>) -> Self {
        Self { service: Arc::downgrade(service), supervisors: Arc::new(Mutex::new(Vec::new())) }
    }

    fn service(&self) -> AgentResult<Arc<Service>> {
        self.service.upgrade().ok_or_else(|| AgentError::failed("the service has shut down"))
    }

    fn authoring(svc: &Service) -> AgentResult<Arc<Authoring>> {
        implexity_authoring::services::authoring(svc).map_err(auth)
    }

    fn opt(svc: &Service) -> AgentResult<Arc<ModelOptimizeManager>> {
        implexity_jobs::api::opt_manager(svc).map_err(|e| job(&e))
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(svc: &Service, op: HostOp) -> AgentResult<Value> {
        let ctx = RouteCtx(svc);
        match op {

            HostOp::ImportModel(doc) => {
                let a = Self::authoring(svc)?;
                let doc = Value::Object(object(&doc)?);
                a.manipulation
                    .run_reserved_idle("replace the model", None, || a.models.put(&doc))
                    .map_err(auth)
            }
            HostOp::EditGraph(p) => {
                let a = Self::authoring(svc)?;
                a.edit_graph(&ctx, &p).map_err(auth)
            }
            HostOp::SetParameters(values) => {
                let a = Self::authoring(svc)?;
                let values: BTreeMap<String, Value> = object(&values)?.into_iter().collect();
                let stores = NativeStores { ctx: &ctx, a: &a };
                a.manipulation
                    .run_reserved_idle("set parameters", None, || {
                        a.interactions.record_mutation(
                            &stores,
                            &mut || a.models.set_parameters(&values),
                            "Edit named geometry parameters",
                            "public_agent_parameters",
                        )
                    })
                    .map_err(auth)
            }
            HostOp::InspectParameters => {
                let m = Self::authoring(svc)?.models.require().map_err(auth)?;
                Ok(json!({"kind": "implicit_parameters", "units": "mm", "parameters": m.parameter_table(),
                    "structure_id": m.structure_id(), "content_id": m.content_id()}))
            }
            HostOp::InitializeLatticeSeed(p) => {
                let a = Self::authoring(svc)?;
                let p = object(&p)?;
                let mut out = Err(AgentError::failed("lattice seed initialisation did not run"));
                let r = a.manipulation.run_reserved_idle("calibrate the lattice seed", None, || {
                    out = initialize_lattice_seed(&a, &p);
                    Ok(())
                });
                r.map_err(auth)?;
                out
            }
            HostOp::ValidateModel(doc) => {
                let a = Self::authoring(svc)?;
                let (problems, warnings) =
                    implexity_geometry::document::problems_of(&doc, Some(a.models.dir()), None)
                        .map_err(geo)?;
                Ok(json!({"valid": problems.is_empty(), "problems": problems, "warnings": warnings}))
            }

            HostOp::SeedCatalogue => Ok(implexity_authoring::seeds::catalogue()),
            HostOp::SeedPreview(p) => {
                implexity_authoring::seeds::preview(&Value::Object(object(&p)?)).map_err(auth)
            }
            HostOp::SeedCommit(p) => {
                Self::seed(svc, "commit geometry seed", &p, implexity_authoring::seeds::commit)
            }
            HostOp::SeedPreviewCurrent(p) => {
                Self::seed(svc, "preview current bake", &p, implexity_authoring::seeds::preview_current)
            }
            HostOp::SeedBakeCurrent(p) => {
                Self::seed(svc, "bake current geometry", &p, implexity_authoring::seeds::bake_current)
            }

            HostOp::GuidedSetupInspect => implexity_authoring::guided_setup::inspect(&ctx).map_err(auth),
            HostOp::GuidedSetupReview(p) => implexity_authoring::guided_setup::review(&ctx, &p).map_err(auth),
            HostOp::GuidedSetupApply(p) => implexity_authoring::guided_setup::apply(&ctx, &p).map_err(auth),
            HostOp::GuidedSetupValidateApply(p) => {
                implexity_authoring::guided_setup::validate_apply(&p).map_err(auth)?;
                Ok(Value::Null)
            }
            HostOp::OptimizationSetupInspect => {
                implexity_authoring::optimization_setup::inspect(&ctx).map_err(auth)
            }
            HostOp::OptimizationSetupSave(p) => {
                implexity_authoring::optimization_setup::save(&ctx, &p).map_err(auth)
            }
            HostOp::OptimizationSetupValidate(p) => {
                implexity_authoring::optimization_setup::validate_payload(&p).map_err(auth)?;
                Ok(Value::Null)
            }
            HostOp::ReviseProblem(p) => {
                implexity_authoring::optimization_setup::revise_problem(&ctx, &p).map_err(auth)
            }
            HostOp::ReviseProblemValidate(p) => {
                implexity_authoring::optimization_setup::validate_revision_payload(&p).map_err(auth)?;
                Ok(Value::Null)
            }

            HostOp::InteractionCapabilities => Ok(InteractionRuntime::capabilities()),
            HostOp::Interaction(method, payload) => {
                let a = Self::authoring(svc)?;
                let stores = NativeStores { ctx: &ctx, a: &a };
                let rt = &a.interactions;
                let r = match method.as_str() {
                    "history_state" if a.models.model().is_none() => Ok(rt.empty_history_state()),
                    "history_state" => rt.history_state(&stores),
                    "field" => rt.field(&stores, &payload),
                    "begin" => rt.begin(&stores, &payload),
                    "preview" => rt.preview(&stores, &payload),
                    "refine" => rt.refine(&stores, &payload),
                    "commit" => rt.commit(&stores, &payload),
                    "cancel" => rt.cancel(&payload),
                    "undo" => rt.undo(&stores, &payload),
                    "redo" => rt.redo(&stores, &payload),
                    other => {
                        return Err(AgentError::failed(format!(
                            "'InteractionRuntime' object has no attribute {}",
                            implexity_core::py_repr::repr_str(other)
                        )));
                    }
                };
                r.map_err(auth)
            }
            HostOp::PromoteCage(p) => {
                let a = Self::authoring(svc)?;
                let stores = NativeStores { ctx: &ctx, a: &a };
                a.interactions.promote_cage(&stores, &p).map_err(auth)
            }
            HostOp::RebindRegion(region, candidates) => {
                implexity_geometry::semantic_regions::rebind_semantic_region(
                    &region,
                    &candidates,
                    3.0,
                    1.15,
                    0.35,
                )
                .map_err(|e| AgentError::Contract(e.0))
            }
            HostOp::Manipulation(method, p) => {
                let a = Self::authoring(svc)?;
                let stores = NativeStores { ctx: &ctx, a: &a };
                let shared = stores.shared();
                let mm = &a.manipulation;
                let r = match method.as_str() {
                    "begin" => mm.begin(&p, Some(&shared)),
                    "preview" => mm.preview(&p),
                    "guidance" => mm.guidance(&p),
                    "commit" => mm.commit(&p, Some(&shared)),
                    "cancel" => mm.cancel(&p, Some(&shared)),
                    "undo" => mm.undo(&p, Some(&shared)),
                    "redo" => mm.redo(&p, Some(&shared)),
                    other => {
                        return Err(AgentError::failed(format!(
                            "'ManipulationManager' object has no attribute {}",
                            implexity_core::py_repr::repr_str(other)
                        )));
                    }
                };
                r.map_err(auth)
            }

            HostOp::CurrentProblem => {
                let a = Self::authoring(svc)?;
                if a.models.model().is_none() {
                    return Ok(Value::Null);
                }
                let call = a.problem_call(&ctx).map_err(auth)?;
                a.problems.get(&call).map_err(auth)
            }
            HostOp::SetEngineeringProblem { payload, provider_envelope } => {
                let a = Self::authoring(svc)?;
                let payload = Value::Object(object(&payload)?);
                a.manipulation
                    .run_reserved_idle("set engineering problem", None, || {
                        let mut call = a.problem_call(&ctx)?;
                        if provider_envelope {
                            call.interface = Some("public_agent_action".into());
                            call.actor = Some("engineering_agent".into());
                        }
                        a.problems.put(&payload, &call)
                    })
                    .map_err(auth)
            }

            HostOp::HistoryList(limit) => Self::authoring(svc)?.history.list(limit).map_err(geo),
            HostOp::HistorySnapshot(label, details) => {
                let a = Self::authoring(svc)?;
                let details = match &details {
                    Value::Object(m) => Some(m),
                    v if !implexity_core::pyobj::truthy(v) => None,
                    other => return Err(type_error(python_type(other))),
                };
                a.history.snapshot(&label, details, None).map_err(geo)
            }
            HostOp::HistoryRestore(entry) => {
                let a = Self::authoring(svc)?;
                let history = Arc::clone(&a.history);
                a.manipulation
                    .run_reserved_idle("restore an engineering branch point", None, || {
                        history.restore_snapshot(&entry).map_err(AuthoringError::from)
                    })
                    .map_err(auth)
            }
            HostOp::HistoryAppend(label, details) => {
                let a = Self::authoring(svc)?;
                a.history
                    .append("agent_action", &label, details.as_object(), None, Some("Engineering Agent"))
                    .map_err(geo)
            }

            HostOp::JobsList => Ok(Self::opt(svc)?.jobs_list()),
            HostOp::JobInfo(id) => Self::opt(svc)?.job_info(&id).map_err(|e| job(&e)),
            HostOp::ReadEpochField(p) => {
                let m = Self::opt(svc)?;
                let epoch = int_arg(&p["epoch"])?;

                let operating_point =
                    p["operating_point"].as_i64().filter(|_| is_int(&p["operating_point"])).unwrap_or(-1);
                let maximum =
                    p["maximum_bytes"].as_u64().filter(|_| is_int(&p["maximum_bytes"])).unwrap_or(0);
                m.read_epoch_field_mode(&text(&p["job_id"]), epoch, &text(&p["field"]), operating_point, maximum, p["mode"].as_str().unwrap_or("payload"))
                    .map_err(|e| job(&e))
            }
            HostOp::ExportEpoch(id, epoch) => {
                Self::opt(svc)?.export_epoch_document(&id, epoch).map_err(|e| job(&e))
            }
            HostOp::Results(p) => Self::opt(svc)?.results(&object(&p)?).map_err(|e| job(&e)),
            HostOp::Sensitivity(p) => Self::opt(svc)?.sensitivity(&object(&p)?).map_err(|e| job(&e)),
            HostOp::CheckGradients(p) => {
                let (_, spec) = implexity_jobs::optimize::spec_from_job_spec(&p["spec"]).map_err(|e| job(&e))?;
                let mut result = implexity_jobs::optimize::gradcheck::fd_vs_ad_probe(
                    &spec, p["slot"].as_str(), p["index"].as_i64().unwrap_or(0), p["step"].as_f64(),
                ).map_err(|e| job(&e))?;
                if !["x", "h", "value", "ad", "fd", "rel_err"].iter().all(|k| result[*k].as_f64().is_some_and(f64::is_finite)) {
                    return Err(AgentError::refused("gradient check returned nonfinite evidence"));
                }
                let tolerance = p["tolerance"].as_f64().unwrap_or(1e-3);
                result["tolerance"] = json!(tolerance);
                result["agreement"] = json!(result["rel_err"].as_f64().unwrap_or(f64::INFINITY) <= tolerance);
                result["nonzero_direction"] = json!(result["comparison"].as_str() != Some("zero_direction"));
                result["live_model_mutated"] = json!(false);
                Ok(result)
            }
            HostOp::Preflight(p) => Self::opt(svc)?.preflight(&object(&p)?).map_err(|e| job(&e)),
            HostOp::Start(p) => Self::opt(svc)?
                .start(&object(&p)?, &implexity_jobs::manager::run::StartArgs::default())
                .map_err(|e| job(&e)),
            HostOp::Steer(id, request) => Self::opt(svc)?.steer(&id, &object(&request)?).map_err(|e| job(&e)),
            HostOp::UseWorkingDesign(p) => Self::opt(svc)?.use_working_design(&p).map_err(|e| job(&e)),
            HostOp::ValidateWorkingResult(p) => implexity_jobs::working_result::validate_request(&p)
                .map(Value::Object)
                .map_err(AgentError::Contract),
            HostOp::JobOperation(id, op) => Self::opt(svc)?.op(&id, &op).map_err(|e| job(&e)),
            HostOp::BranchAfterIntervention(id, request) => {
                Self::opt(svc)?.branch_after_intervention(&id, request.as_object()).map_err(|e| job(&e))
            }
            HostOp::ResolveNumericalAttention(id, token, action) => {
                Self::opt(svc)?.resolve_numerical_attention(&id, &token, &action).map_err(|e| job(&e))
            }
            HostOp::BranchAfterNumericalAttention(id, token) => {
                Self::opt(svc)?.branch_after_numerical_attention(&id, &token).map_err(|e| job(&e))
            }

            HostOp::InspectStudy(id) => {
                let store = implexity_jobs::api::study_store(svc).map_err(|e| job(&e))?;
                let mut record = store.get(&id).map_err(|e| job(&study(e)))?;
                let m = Self::opt(svc)?;
                if let Some(runs) = record.get_mut("runs").and_then(Value::as_array_mut) {
                    for run in runs {
                        let jid = run.get("job_id").and_then(Value::as_str).map(str::to_string);
                        let status = jid.as_deref().and_then(|j| m.job_status(j));
                        if let Some(r) = run.as_object_mut() {
                            r.insert(
                                "status".into(),
                                json!(status.clone().unwrap_or_else(|| "not-loaded".into())),
                            );
                            if status.is_some() {
                                r.insert(
                                    "record".into(),
                                    json!(format!(
                                        "/v1/implicit/optimize/jobs/{}/record",
                                        jid.unwrap_or_default()
                                    )),
                                );
                            }
                        }
                    }
                }
                Ok(record)
            }
            HostOp::CreateStudy(p) => {
                let a = Self::authoring(svc)?;
                let identity = a.model_identity().map_err(auth)?;
                let norm = implexity_jobs::studies::normalise(&p, Some(&identity))
                    .map_err(|s| job(&study(implexity_jobs::studies::StudyStoreError::Study(s))))?;
                let m = Self::opt(svc)?;
                let mut validated = Vec::new();
                for v in norm.get("variants").and_then(Value::as_array).cloned().unwrap_or_default() {
                    let decl = m.declare(v.get("request").unwrap_or(&Value::Null)).map_err(|e| job(&e))?;
                    let free: Vec<Value> = decl
                        .meta
                        .get("plan")
                        .as_array()
                        .map(|p| p.iter().map(|x| x.get("ref").cloned().unwrap_or(Value::Null)).collect())
                        .unwrap_or_default();
                    validated.push(json!({"name": v.get("name").cloned().unwrap_or(Value::Null),
                        "solve_id": decl.meta.get("solve_id").clone(), "free": free,
                        "objective": decl.meta.get("objective_terms").clone()}));
                }
                let store = implexity_jobs::api::study_store(svc).map_err(|e| job(&e))?;
                let mut record = store.create(&p, norm.get("model")).map_err(|e| job(&study(e)))?;
                if let Some(r) = record.as_object_mut() {
                    r.insert("validated".into(), Value::Array(validated));
                }
                Ok(record)
            }
            HostOp::RunStudyVariant(p) => {
                let (sid, variant) = (text(&p["study_id"]), text(&p["variant"]));
                let store = implexity_jobs::api::study_store(svc).map_err(|e| job(&e))?;
                let mut req = object(&store.variant_request(&sid, &variant).map_err(|e| job(&study(e)))?)?;
                let overrides = p.get("overrides").filter(|v| implexity_core::pyobj::truthy(v)).cloned();
                if let Some(o) = overrides {
                    req.extend(object(&o)?);
                }
                let mut rep = Self::opt(svc)?
                    .start(&req, &implexity_jobs::manager::run::StartArgs::default())
                    .map_err(|e| job(&e))?;
                let job_id = text(&rep["job_id"]);
                let solve_id = rep.get("solve_id").and_then(Value::as_str).map(str::to_string);
                store.attach_run(&sid, &variant, &job_id, solve_id.as_deref()).map_err(|e| job(&study(e)))?;
                if let Some(r) = rep.as_object_mut() {
                    r.insert("study".into(), json!(sid));
                    r.insert("variant".into(), json!(variant));
                }
                Ok(rep)
            }

            HostOp::ReadResultArray(p) => read_result_array(&*Self::authoring(svc)?, &p),
            HostOp::AuthoredEvaluationArtifact { result, problem, design } => {
                let a = Self::authoring(svc)?;
                implexity_jobs::result_arrays::authored_evaluation_artifact_view(
                    &implexity_jobs::result_arrays::ResultTree::from_evaluation(&result),
                    &problem,
                    &design,
                    &a.models.dir().join("opt").join("result_artifacts_v24_provider"),
                    8 * 1024 * 1024,
                )
                .map_err(|e| job(&JobError::from(e)))
            }
        }
    }

    fn seed(
        svc: &Service,
        operation: &str,
        p: &Value,
        f: fn(
            &implexity_authoring::model_manager::ModelManager,
            &Value,
        ) -> implexity_authoring::error::AResult<Value>,
    ) -> AgentResult<Value> {
        let a = Self::authoring(svc)?;
        let p = Value::Object(object(p)?);
        a.manipulation.run_reserved_idle(operation, None, || f(&a.models, &p)).map_err(auth)
    }
}

fn study(e: implexity_jobs::studies::StudyStoreError) -> JobError {
    use implexity_jobs::studies::StudyStoreError as S;
    match e {
        S::Study(s) => {
            JobError::Problems { class: "StudyError".into(), message: s.to_string(), problems: s.problems }
        }
        S::Missing(m) => JobError::of("KeyError", m),
        S::Io(m) => JobError::of("OSError", m),
    }
}

fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

fn int_arg(v: &Value) -> AgentResult<i64> {
    match v {
        Value::Number(n) => {
            n.as_i64()
                .or_else(|| {
                    n.as_f64().filter(|f| f.is_finite()).map(|f| {
                        #[allow(clippy::cast_possible_truncation)]
                        let t = f.trunc() as i64;
                        t
                    })
                })
                .ok_or_else(|| {
                    AgentError::contract(format!(
                        "cannot convert {} to integer",
                        implexity_core::pyobj::repr(v)
                    ))
                })
        }
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| {
            AgentError::contract(format!(
                "invalid literal for int() with base 10: {}",
                implexity_core::py_repr::repr_str(s)
            ))
        }),
        other => Err(AgentError::failed(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            python_type(other)
        ))),
    }
}

fn initialize_lattice_seed(a: &Authoring, p: &Map<String, Value>) -> AgentResult<Value> {
    use implexity_geometry::document::Binding;
    use implexity_geometry::lattice::node::ControlledLattice;
    let model = a.models.require().map_err(auth)?;
    if let Some(expected) = p.get("expected_content_id").filter(|v| !v.is_null())
        && Some(text(expected)) != model.content_id()
    {
        return Err(AgentError::contract("the model changed; refresh it before calibrating the seed"));
    }
    let node_id = text(p.get("node").unwrap_or(&Value::Null));
    let node = model.node(&node_id).ok();
    let lattice = node.as_ref().and_then(|n| n.op().as_any().downcast_ref::<ControlledLattice>());
    let (Some(node), Some(lattice)) = (node.as_ref(), lattice) else {
        return Err(AgentError::contract(format!(
            "{} is not a controlled-lattice node",
            implexity_core::py_repr::repr_str(&node_id)
        )));
    };
    let array_key = match model.bindings().get(&node_id).and_then(|b| b.get("control")) {
        Some(Binding::Array { key, .. }) => key.clone(),
        _ => {
            return Err(AgentError::contract(
                "the lattice control field is not stored as a document array; save the model once so its control \
                 tensor is array-bound before calibrating",
            ));
        }
    };
    let specification = p.get("specification").cloned().unwrap_or(Value::Null);
    let (controls, report) = implexity_geometry::lattice::initialization::initialize_solid_fraction(
        &lattice.spec,
        node,
        &specification,
    )
    .map_err(|e| match e {
        implexity_geometry::GeometryError::Model(m) => AgentError::Contract(m),
        other => geo(other),
    })?;
    let reference = format!("{node_id}:control");
    let array_file = model
        .doc
        .get("arrays")
        .and_then(|a| a.get(&array_key))
        .and_then(|e| e.get("file"))
        .cloned()
        .unwrap_or(Value::Null);
    let plan = vec![json!({"kind": "spatial_array", "ref": reference, "node": node_id, "param": "control",
        "units": "-", "node_units": "-", "parameter": null, "lo": null, "hi": null, "start": null,
        "start_document": null, "array_key": array_key, "array_file": array_file})];
    let [nx, ny, nz] = controls.grid;
    let tensor = implexity_geometry::value::NdArray::from_f64(vec![20, nx, ny, nz], controls.data.clone())
        .ok_or_else(|| AgentError::failed("the calibrated control tensor has an inconsistent shape"))?;
    let mut values = BTreeMap::new();
    values.insert(reference, tensor);
    let applied =
        a.models.apply_values(&plan, &values, true, "lattice fraction seed initialization").map_err(auth)?;
    let after = a.models.require().map_err(auth)?;
    Ok(json!({"schema": "implexity-lattice-seed-initialization/1", "node": node_id, "report": report,
        "applied": applied, "content_id_before": model.content_id(), "content_id": after.content_id()}))
}

fn read_result_array(a: &Authoring, p: &Value) -> AgentResult<Value> {
    let id = text(&p["artifact_id"]);
    let field = text(&p["field"]);
    let offset = p.get("offset").and_then(Value::as_i64).unwrap_or(0);
    let store = implexity_jobs::result_arrays::resolve_store(a.models.dir(), &id).map_err(|e| match e {
        ArrayReadError::NotFound(m) => {
            AgentError::Failed(format!("[Errno 2] No such file or directory: {m}"))
        }
        ArrayReadError::Value(m) => AgentError::Contract(m),
    })?;
    let count = if let Some(c) = p.get("count").and_then(Value::as_i64) {
        c
    } else {
        {
            let manifest = store.get(&id).map_err(|e| job(&JobError::from(e)))?;
            let shape = manifest
                .get("fields")
                .and_then(|f| f.get(&field))
                .and_then(|m| m.get("shape"))
                .and_then(Value::as_array)
                .ok_or_else(|| AgentError::KeyError(implexity_core::py_repr::repr_str(&field)))?;
            let total: i64 = shape.iter().map(|v| v.as_i64().unwrap_or(0)).product();
            4096.min(total - offset)
        }
    };
    let (mut header, chunk) =
        implexity_jobs::result_arrays::read_array(&store, &id, &field, offset, Some(count), "json").map_err(
            |e| match e {
                ArrayReadError::NotFound(m) => AgentError::KeyError(m),
                ArrayReadError::Value(m) => AgentError::Contract(m),
            },
        )?;
    if let implexity_jobs::result_arrays::ArrayChunk::Json(values) = chunk {
        header.insert("values".into(), Value::Array(values));
    }
    Ok(Value::Object(header))
}

impl AgentManagers for KernelManagers {
    fn call(&self, op: HostOp) -> AgentResult<Value> {
        let svc = self.service()?;
        Self::dispatch(&svc, op)
    }

    fn model(&self) -> Option<Arc<dyn AgentModel>> {
        let svc = self.service().ok()?;
        let a = Self::authoring(&svc).ok()?;
        Some(Arc::new(LiveModel(a)))
    }

    fn detached_model(&self, document: &Value) -> AgentResult<Arc<dyn AgentModel>> {
        let view = DetachedModelView::from_document(document, None).map_err(|e| match auth(e) {
            AgentError::Failed(m) => AgentError::Contract(m),
            other => other,
        })?;
        Ok(Arc::new(DetachedModel(view)))
    }

    fn result_store(
        &self,
        artifact_id: &str,
    ) -> AgentResult<Arc<dyn implexity_render::artifact::ResultArtifactStore + Send + Sync>> {
        let svc = self.service()?;
        let a = Self::authoring(&svc)?;
        let store = implexity_jobs::result_arrays::resolve_store(a.models.dir(), artifact_id).map_err(
            |e| match e {
                ArrayReadError::NotFound(m) => {
                    AgentError::Failed(format!("[Errno 2] No such file or directory: {m}"))
                }
                ArrayReadError::Value(m) => AgentError::Contract(m),
            },
        )?;
        Ok(Arc::new(ArtifactStore(store)))
    }

    fn managed_supervisor(&self, private_root: &Path) -> AgentResult<Arc<dyn ManagedSupervisor>> {
        let manager =
            ManagedEvaluationManager::new(private_root, None).map_err(|e| job(&JobError::from(e)))?;
        let s = Arc::new(Supervisor { manager, controls: Mutex::new(BTreeMap::new()) });
        self.supervisors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(Arc::clone(&s));
        Ok(s)
    }

    fn prepare_managed_evaluation(&self, kind: &str, request: Value) -> AgentResult<Arc<dyn ManagedCapsule>> {
        let svc = self.service()?;
        let capsule = Self::opt(&svc)?.prepare_managed_evaluation(kind, &request).map_err(|e| job(&e))?;
        Ok(Arc::new(Capsule { capsule, supervisors: Arc::clone(&self.supervisors) }))
    }
}

struct LiveModel(Arc<Authoring>);

fn mesh(e: &AuthoringError) -> MeshError {
    MeshError::invalid(e.to_string())
}

impl ModelView for LiveModel {
    fn status(&self) -> Result<ModelStatus, MeshError> {
        ModelView::status(&*self.0.models)
    }
    fn live_lock(&self) -> LiveGuard<'_> {
        ModelView::live_lock(&*self.0.models)
    }
    fn evaluate_exact(&self, points: &[[f64; 3]]) -> Result<Evaluated, MeshError> {
        self.0.models.evaluate_exact(points)
    }
    fn geometry_sampling_hint(&self) -> Result<Option<SamplingHint>, MeshError> {
        self.0.models.geometry_sampling_hint()
    }
    fn registered_fields(&self) -> Result<Vec<Value>, MeshError> {
        self.0.models.registered_fields()
    }
    fn resolve_registered_field(&self, field: &str) -> Result<RegisteredGrid, MeshError> {
        self.0.models.resolve_registered_field(field)
    }
}

impl AgentModel for LiveModel {
    fn status_value(&self) -> AgentResult<Value> {
        self.0.models.status().map_err(auth)
    }
    fn document_sha256(&self) -> Option<String> {
        self.0.models.model().and_then(|m| m.sha256().ok())
    }
    fn geometry_model(&self) -> Option<Arc<implexity_geometry::document::model::Model>> {
        self.0.models.model()
    }
}

struct DetachedModel(DetachedModelView);

impl ModelView for DetachedModel {
    fn status(&self) -> Result<ModelStatus, MeshError> {
        ModelView::status(&self.0)
    }
    fn live_lock(&self) -> LiveGuard<'_> {
        ModelView::live_lock(&self.0)
    }
    fn evaluate_exact(&self, points: &[[f64; 3]]) -> Result<Evaluated, MeshError> {
        self.0.evaluate_exact(points)
    }
    fn geometry_sampling_hint(&self) -> Result<Option<SamplingHint>, MeshError> {
        self.0.geometry_sampling_hint()
    }
    fn registered_fields(&self) -> Result<Vec<Value>, MeshError> {
        self.0.registered_fields()
    }
    fn resolve_registered_field(&self, field: &str) -> Result<RegisteredGrid, MeshError> {
        self.0.resolve_registered_field(field)
    }
}

impl AgentModel for DetachedModel {
    fn status_value(&self) -> AgentResult<Value> {
        Ok(self.0.status())
    }
    fn document_sha256(&self) -> Option<String> {
        self.0.require().sha256().map_err(|e| mesh(&AuthoringError::from(e))).ok()
    }
    fn geometry_model(&self) -> Option<Arc<implexity_geometry::document::model::Model>> {
        Some(self.0.require())
    }
}

struct ArtifactStore(implexity_jobs::artifacts::ResultArtifactStore);

fn render(e: &implexity_jobs::artifacts::ArtifactError) -> implexity_render::RenderError {
    implexity_render::RenderError::Invalid(e.to_string())
}

impl implexity_render::artifact::ResultArtifactStore for ArtifactStore {
    fn inspect(&self, artifact_id: &str) -> Result<(Value, BTreeSet<String>), implexity_render::RenderError> {
        let (manifest, inspected) = self
            .0
            .inspect_bounded(artifact_id, &implexity_jobs::artifacts::Limits::default())
            .map_err(|e| render(&e))?;
        Ok((Value::Object(manifest), inspected.into_keys().collect()))
    }

    fn read_arrays(
        &self,
        artifact_id: &str,
        names: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, implexity_render::artifact::StoredArray>, implexity_render::RenderError>
    {
        let selected: Vec<String> = names.iter().cloned().collect();
        let arrays = self
            .0
            .read_arrays_bounded(artifact_id, Some(&selected), &implexity_jobs::artifacts::Limits::default())
            .map_err(|e| render(&e))?;
        Ok(arrays
            .into_iter()
            .map(|(name, a)| {
                use implexity_io::npy::NpyData as D;
                let real_numeric = !matches!(
                    a.data,
                    D::Bool(_) | D::C64(_) | D::C128(_) | D::Unicode { .. } | D::Bytes { .. }
                );
                let values = a.to_f64().map(|v| v.into_iter().collect()).unwrap_or_default();
                let stored = implexity_render::artifact::StoredArray {
                    shape: a.shape.clone(),
                    dtype: implexity_jobs::artifacts::dtype_name(&a.data),
                    bytes: implexity_jobs::result_arrays::data_bytes(&a),
                    values,
                    real_numeric,
                };
                (name, stored)
            })
            .collect())
    }

    fn registration_from_wire(
        &self,
        wire: &Value,
    ) -> Result<implexity_render::artifact::Registration, implexity_render::RenderError> {
        let r = implexity_geometry::field_registration::GridRegistration::from_wire(wire)
            .map_err(|e| implexity_render::RenderError::Invalid(e.to_string()))?;
        Ok(implexity_render::artifact::Registration {
            shape: r.shape,
            origin: r.origin,
            matrix: r.matrix(),
            centering: r.centering.clone(),
            axis_order: r.axis_order.clone(),
            frame: r.frame.clone(),
            wire: r.to_wire(),
        })
    }
}

struct Supervisor {
    manager: ManagedEvaluationManager,
    controls: Mutex<BTreeMap<String, ManagedEvaluationControl>>,
}

struct Control(String);

impl ManagedControl for Control {
    fn operation_id(&self) -> String {
        self.0.clone()
    }
}

fn status_of(s: &implexity_jobs::managed_evaluation::PublicManagedEvaluationStatus) -> ManagedStatus {
    ManagedStatus { state: s.state.clone(), wire: s.to_wire() }
}

impl Supervisor {
    fn control(&self, control: &dyn ManagedControl) -> AgentResult<ManagedEvaluationControl> {
        self.controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&control.operation_id())
            .cloned()
            .ok_or_else(|| AgentError::contract("managed operation control is not owned by this supervisor"))
    }
}

impl ManagedSupervisor for Supervisor {
    fn status(&self, operation_id: &str) -> AgentResult<ManagedStatus> {
        self.manager.status(operation_id).map(|s| status_of(&s)).map_err(|e| job(&JobError::from(e)))
    }
    fn request_cancel(&self, control: &dyn ManagedControl) -> AgentResult<ManagedStatus> {
        let c = self.control(control)?;
        self.manager.request_cancel(&c).map(|s| status_of(&s)).map_err(|e| job(&JobError::from(e)))
    }
    fn committed_directory(&self, control: &dyn ManagedControl) -> AgentResult<PathBuf> {
        let c = self.control(control)?;
        self.manager.committed_directory(&c).map_err(|e| job(&JobError::from(e)))
    }
    fn solver_recovery(&self, control: &dyn ManagedControl) -> AgentResult<Option<Value>> {
        let c = self.control(control)?;
        self.manager.solver_recovery(&c).map_err(|e| job(&JobError::from(e)))
    }
}

struct Capsule {
    capsule: Arc<ManagedProviderEvaluationCapsule>,
    supervisors: Arc<Mutex<Vec<Arc<Supervisor>>>>,
}

impl ManagedCapsule for Capsule {
    fn operation_kind(&self) -> String {
        self.capsule.operation_kind.clone()
    }

    fn start(&self, supervisor: &Arc<dyn ManagedSupervisor>) -> AgentResult<Arc<dyn ManagedControl>> {
        let wanted = Arc::as_ptr(supervisor).cast::<()>();
        let owned = self
            .supervisors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|s| Arc::as_ptr(s).cast::<()>() == wanted)
            .cloned()
            .ok_or_else(|| {
                AgentError::contract("managed evaluation supervisor is not a kernel supervisor")
            })?;
        let control = self.capsule.start(&owned.manager).map_err(|e| job(&e))?;
        let id = control.operation_id().to_string();
        owned.controls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.clone(), control);
        Ok(Arc::new(Control(id)))
    }

    fn finalize(&self, committed_directory: &Path) -> AgentResult<Value> {
        self.capsule.finalize(committed_directory).map_err(|e| job(&e))
    }
}

pub fn install(service: &Arc<Service>) {
    crate::agent::install_managers(service, Arc::new(KernelManagers::new(service)));
}
