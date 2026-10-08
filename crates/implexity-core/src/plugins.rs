// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use crate::contracts::{
    CaeProvider, Evaluation, FieldValue, LegacySingleArrayProviderCapabilities, ProviderCapabilities,
    ProviderDescriptor, ProviderProblem, Sensitivity,
};
use crate::error::{CaeError, CaeResult};
use crate::json::{DumpOptions, dumps, parse_strict};
use crate::orchestration::PublishedContract;
use crate::package_catalog::PackageDescriptor;
use crate::py_repr::repr_str;
use crate::sync::lock;

pub const ENV_VAR: &str = "IMPLEXITY_PLUGINS";
pub const PACKAGES_ENV_VAR: &str = "IMPLEXITY_PHYSICS_PACKAGES";
pub const PROTOCOL: &str = "implexity-external-provider/1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PluginError(pub String);

fn looks_like_physics_package(name: &str, descriptors: &[PackageDescriptor]) -> Option<&'static str> {
    if Path::new(name).exists() {
        return None;
    }
    if descriptors.iter().any(|d| d.package_id == name) {
        return Some("is a physics package id");
    }
    if name.starts_with("implexity.addins.") || descriptors.iter().any(|d| d.installer == name) {
        return Some("is a physics package installer module (inert on import)");
    }
    let normalised: String = name
        .chars()
        .map(|c| if matches!(c, '-' | '\u{2212}' | '\u{2013}' | '\u{2014}') { '_' } else { c })
        .collect();
    if normalised != name && descriptors.iter().any(|d| d.package_id == normalised) {
        return Some("is a physics package id spelled with a hyphen or dash");
    }
    None
}


pub fn env_entries(
    value: Option<&str>,
    descriptors: &[PackageDescriptor],
) -> Result<Vec<String>, PluginError> {
    let raw = value.unwrap_or_default();
    let names: Vec<String> = raw.replace(',', " ").split_whitespace().map(str::to_string).collect();
    for name in &names {
        let Some(why) = looks_like_physics_package(name, descriptors) else { continue };
        let normalised = name.replace('-', "_");
        let hint = if descriptors.iter().any(|d| d.package_id == *name) {
            Some(name.clone())
        } else if let Some(d) = descriptors.iter().find(|d| d.installer == *name) {
            Some(d.package_id.clone())
        } else if descriptors.iter().any(|d| d.package_id == normalised) {
            Some(normalised)
        } else {
            None
        };
        let setting = hint.map_or_else(
            || format!("{PACKAGES_ENV_VAR} to one of the installed package ids"),
            |h| format!("{PACKAGES_ENV_VAR}={h}"),
        );
        return Err(PluginError(format!(
            "{ENV_VAR} value {} looks like a physics package ID ({why}) -- did you mean {PACKAGES_ENV_VAR}?\n  {ENV_VAR} takes plugin provider executables and starts them; physics packages are activated, not started, so this entry would load nothing.\n  Set {setting} instead, and remove {} from {ENV_VAR}.",
            repr_str(name),
            repr_str(name)
        )));
    }
    Ok(names)
}

struct Channel {
    child: Child,
    requests: std::sync::mpsc::Sender<(String, std::sync::mpsc::Sender<std::io::Result<String>>)>,
    timeout: Option<std::time::Duration>,
    next_id: u64,
}

impl Drop for Channel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct ExternalProvider {
    path: PathBuf,
    name: String,
    implementation: String,
    capabilities: ProviderCapabilities,
    contract: Option<Value>,
    declarations: Map<String,Value>,
    scope: Vec<String>,
    channel: Mutex<Channel>,
}

impl std::fmt::Debug for ExternalProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalProvider")
            .field("path", &self.path)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

struct Handle(String);

fn array_value(a: &ArrayD<f64>) -> CaeResult<Value> {
    crate::numeric_contract::require_finite_array(a, "external provider array")?;
    Ok(json!({"shape": a.shape(), "data": a.iter().copied().collect::<Vec<f64>>()}))
}

fn array_from(value: &Value, label: &str) -> CaeResult<ArrayD<f64>> {
    let bad = || CaeError::contract(format!("{label} must be an array object with shape and data"));
    let m = value.as_object().ok_or_else(bad)?;
    let shape: Vec<usize> = m
        .get("shape")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .map(|v| v.as_u64().and_then(|u| usize::try_from(u).ok()).ok_or_else(bad))
        .collect::<CaeResult<_>>()?;
    let data: Vec<f64> = m
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .map(|v| v.as_f64().ok_or_else(bad))
        .collect::<CaeResult<_>>()?;
    crate::numeric_contract::require_finite(&data, label)?;
    ArrayD::from_shape_vec(IxDyn(&shape), data).map_err(|_| bad())
}

fn object_of(value: Option<&Value>) -> Map<String, Value> {
    value.and_then(Value::as_object).cloned().unwrap_or_default()
}

impl ExternalProvider {

    pub fn start(path: &Path) -> Result<Self, PluginError> {
        Self::start_configured(path, &[], None, None)
    }

    pub fn start_configured(path: &Path, args: &[String], timeout: Option<std::time::Duration>, directory: Option<&Path>) -> Result<Self, PluginError> {
        let fail = |what: String| {
            PluginError(format!(
                "implexity plugin {} could not be loaded:\n  {what}",
                repr_str(&path.display().to_string())
            ))
        };
        let mut command = Command::new(path);
        if let Some(directory) = directory { command.current_dir(directory); }
        let mut child = command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| fail(format!("cannot start: {e}")))?;
        let mut stdin = child.stdin.take().ok_or_else(|| fail("no stdin".into()))?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or_else(|| fail("no stdout".into()))?);
        let (requests, incoming) = std::sync::mpsc::channel::<(String, std::sync::mpsc::Sender<std::io::Result<String>>) >();
        std::thread::spawn(move || {
            while let Ok((line, answer)) = incoming.recv() {
                let result = (|| {
                    writeln!(stdin, "{line}")?;
                    stdin.flush()?;
                    let mut reply = String::new();
                    stdout.read_line(&mut reply)?;
                    Ok(reply)
                })();
                let failed = result.is_err();
                let _ = answer.send(result);
                if failed { break; }
            }
        });
        let channel = Mutex::new(Channel { child, requests, timeout, next_id: 0 });
        let mut provider = Self {
            path: path.to_path_buf(),
            name: String::new(),
            implementation: String::new(),
            capabilities: ProviderCapabilities::Mapping(Map::new()),
            contract: None,
            declarations: Map::new(),
            scope: vec!["*".into()],
            channel,
        };
        let described = provider
            .call("describe", &json!({"protocol": PROTOCOL}))
            .map_err(|e| fail(e.message().to_string()))?;
        let d = described.as_object().ok_or_else(|| fail("describe did not return an object".into()))?;
        if d.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
            return Err(fail(format!("speaks an unsupported protocol (expected {PROTOCOL})")));
        }
        provider.name = d
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| fail("describe lacks a name".into()))?
            .to_string();
        provider.implementation = d
            .get("implementation")
            .and_then(Value::as_str)
            .map_or_else(|| format!("external:{}", path.display()), str::to_string);
        provider.capabilities = parse_capabilities(d.get("capabilities"), &provider.name)
            .map_err(|e| fail(e.message().to_string()))?;
        for key in ["coupling_declaration","mathematical_declaration","semantic_physics_contract","coupling_inventory"] {
            if let Some(v)=d.get(key).filter(|v|!v.is_null()) { provider.declarations.insert(key.into(),v.clone()); }
        }
        provider.contract = d.get("orchestration_contract").filter(|v| !v.is_null()).cloned();
        if let Some(scope) = d.get("application_scope").and_then(Value::as_array) {
            provider.scope = scope.iter().map(crate::pyobj::py_str).collect();
        }
        Ok(provider)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn call(&self, method: &str, params: &Value) -> CaeResult<Value> {
        let mut ch = lock(&self.channel);
        ch.next_id += 1;
        let id = ch.next_id;
        let line = dumps(&json!({"id": id, "method": method, "params": params}), &DumpOptions::compact());
        let (answer, incoming) = std::sync::mpsc::channel();
        ch.requests.send((line, answer)).map_err(|_| CaeError::contract("external provider channel closed"))?;
        let result = match ch.timeout {
            Some(timeout) => incoming.recv_timeout(timeout).map_err(|_| "external provider response timed out or channel closed"),
            None => incoming.recv().map_err(|_| "external provider channel closed"),
        };
        let reply = match result {
            Ok(Ok(reply)) if !reply.is_empty() => reply,
            other => {
                let _ = ch.child.kill();
                let _ = ch.child.wait();
                return Err(CaeError::contract(format!("external provider {method}: {}", match other {
                    Ok(Err(e)) => e.to_string(),
                    Ok(Ok(_)) => "process closed its output".into(),
                    Err(e) => e.into(),
                })));
            }
        };
        let v = parse_strict(reply.trim_end())
            .map_err(|e| CaeError::contract(format!("external provider {method}: invalid reply: {e}")))?;
        if v.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(CaeError::contract(format!("external provider {method}: reply id mismatch")));
        }
        if let Some(err) = v.get("error") {
            let message = err.get("message").map(crate::pyobj::py_str).unwrap_or_default();
            return Err(match err.get("kind").and_then(Value::as_str) {
                Some("CAEConvergenceError") => CaeError::convergence(message),
                Some("CAENewtonConvergenceError") => CaeError::newton(message),
                _ => CaeError::contract(message),
            });
        }
        v.get("result")
            .cloned()
            .ok_or_else(|| CaeError::contract(format!("external provider {method}: reply has no result")))
    }

    fn handle(problem: &ProviderProblem) -> CaeResult<String> {
        problem
            .downcast_ref::<Handle>()
            .map(|h| h.0.clone())
            .ok_or_else(|| CaeError::contract("external provider received a problem it did not normalise"))
    }
}

fn parse_capabilities(raw: Option<&Value>, name: &str) -> CaeResult<ProviderCapabilities> {
    let Some(m) = raw.and_then(Value::as_object) else {
        return Ok(ProviderCapabilities::Mapping(Map::new()));
    };
    let texts = |k: &str| -> Vec<String> {
        m.get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().map(crate::pyobj::py_str).collect())
            .unwrap_or_default()
    };
    let flag = |k: &str, d: bool| m.get(k).and_then(Value::as_bool).unwrap_or(d);
    let mut base = ProviderDescriptor::new(
        m.get("name").and_then(Value::as_str).unwrap_or(name),
        texts("analyses"),
        texts("responses"),
    );
    base.fields = texts("fields");
    base.sensitivities = flag("sensitivities", true);
    base.nonlinear = flag("nonlinear", false);
    base.notes = texts("notes");
    base.design_coordinates = texts("design_coordinates");
    base.distributable = flag("distributable", true);
    base.mathematical_structures = texts("mathematical_structures");
    base.response_metadata = object_of(m.get("response_metadata"));
    base.traits = object_of(m.get("traits"));
    if m.contains_key("provider_api") || m.contains_key("execution") || m.contains_key("topology_coordinate")
    {
        let mut legacy =
            LegacySingleArrayProviderCapabilities::new(base.name.clone(), Vec::new(), Vec::new());
        if base.design_coordinates.is_empty() {
            base.design_coordinates.clone_from(&legacy.base.design_coordinates);
        }
        legacy.base = base;
        if let Some(e) = m.get("execution").and_then(Value::as_str) {
            legacy.execution = e.to_string();
        }
        if let Some(t) = m.get("topology_coordinate").and_then(Value::as_str) {
            legacy.topology_coordinate = t.to_string();
        }
        if let Some(a) = m.get("provider_api").and_then(Value::as_i64) {
            legacy.provider_api = a;
        }
        legacy.editor = object_of(m.get("editor"));
        legacy.condition_types = texts("condition_types");
        legacy.material_model = m.get("material_model").and_then(Value::as_str).map(str::to_string);
        legacy.compatibility_routes = texts("compatibility_routes");
        return Ok(ProviderCapabilities::Legacy(Box::new(legacy.checked()?)));
    }
    Ok(ProviderCapabilities::Descriptor(Box::new(base.checked()?)))
}

impl CaeProvider for ExternalProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn implementation(&self) -> &str {
        &self.implementation
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        Ok(self.capabilities.clone())
    }
    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        self.contract.clone().map(|c| Ok(PublishedContract::Mapping(c)))
    }
    fn application_scope(&self) -> Vec<String> {
        self.scope.clone()
    }
    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        let r = self.call("normalise_problem", &json!({"problem": problem}))?;
        let h = r.get("problem_handle").and_then(Value::as_str).ok_or_else(|| {
            CaeError::contract("external provider normalise_problem returned no problem_handle")
        })?;
        Ok(Arc::new(Handle(h.to_string())))
    }
    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let topo = topology.map(array_value).transpose()?.unwrap_or(Value::Null);
        let r =
            self.call("preflight", &json!({"problem_handle": Self::handle(problem)?, "topology": topo}))?;
        r.as_object()
            .cloned()
            .ok_or_else(|| CaeError::contract("external provider preflight must return a mapping"))
    }
    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        let r = self.call(
            "evaluate",
            &json!({"problem_handle": Self::handle(problem)?, "topology": array_value(topology)?}),
        )?;
        let mut responses = BTreeMap::new();
        for (k, v) in object_of(r.get("responses")) {
            let f = v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| {
                CaeError::contract(format!(
                    "external provider response {} must be a finite number",
                    repr_str(&k)
                ))
            })?;
            responses.insert(k, f);
        }
        let mut fields = BTreeMap::new();
        for (k, v) in object_of(r.get("fields")) {
            let field = if v.get("shape").is_some() && v.get("data").is_some() {
                FieldValue::Array(array_from(&v, &format!("external provider field {k}"))?)
            } else {
                FieldValue::Json(v)
            };
            fields.insert(k, field);
        }
        Ok(Evaluation {
            provider: r.get("provider").and_then(Value::as_str).unwrap_or(&self.name).to_string(),
            responses,
            diagnostics: object_of(r.get("diagnostics")),
            fields,
        })
    }
    fn sensitivity(
        &self,
        problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        let r = self.call(
            "sensitivity",
            &json!({"problem_handle": Self::handle(problem)?, "topology": array_value(topology)?, "response": response}),
        )?;
        let value = r.get("value").and_then(Value::as_f64).filter(|f| f.is_finite()).ok_or_else(|| {
            CaeError::contract("external provider sensitivity value must be a finite number")
        })?;
        let gradient = array_from(r.get("gradient").unwrap_or(&Value::Null), "external provider gradient")?;
        Ok(Sensitivity {
            provider: r.get("provider").and_then(Value::as_str).unwrap_or(&self.name).to_string(),
            response: response.to_string(),
            value,
            gradient,
            diagnostics: object_of(r.get("diagnostics")),
        })
    }
    fn coupling_declaration(&self,_problem:Option<&ProviderProblem>)->Option<CaeResult<Value>> { self.declarations.get("coupling_declaration").cloned().map(Ok) }
    fn mathematical_declaration(&self,_problem:Option<&ProviderProblem>)->Option<Value> { self.declarations.get("mathematical_declaration").cloned() }
    fn semantic_physics_contract(&self,_problem:Option<&ProviderProblem>)->Option<Value> { self.declarations.get("semantic_physics_contract").cloned() }
    fn coupling_inventory(&self,_problem:Option<&ProviderProblem>)->Option<Value> { self.declarations.get("coupling_inventory").cloned() }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for ExternalProvider {
    fn drop(&mut self) {
        let mut ch = lock(&self.channel);
        let _ = ch.child.kill();
        let _ = ch.child.wait();
    }
}

pub struct LoadedPlugin {
    pub source: String,
    pub entry: String,
    pub provider: Arc<dyn CaeProvider>,
}

static LOADED: Mutex<Vec<LoadedPlugin>> = Mutex::new(Vec::new());


pub fn load(
    value: Option<&str>,
    descriptors: &[PackageDescriptor],
    registries: &crate::registries::Registries,
) -> Result<Vec<String>, PluginError> {
    let entries = env_entries(value, descriptors)?;
    let mut out = Vec::new();
    for entry in entries {
        if lock(&LOADED).iter().any(|p| p.entry == entry) {
            continue;
        }
        let provider: Arc<dyn CaeProvider> = Arc::new(ExternalProvider::start(Path::new(&entry))?);
        registries.register_provider(Arc::clone(&provider)).map_err(|e| {
            PluginError(format!(
                "implexity plugin {} (named by ${ENV_VAR}) could not be loaded:\n  {}: {}\n  -- the service will not start with a plugin it was told to load and could not. Fix the plugin, or remove it from ${ENV_VAR}.",
                repr_str(&entry),
                e.python_class(),
                e.message()
            ))
        })?;
        lock(&LOADED).push(LoadedPlugin { source: format!("${ENV_VAR}"), entry: entry.clone(), provider });
        out.push(entry);
    }
    Ok(out)
}

#[must_use]
pub fn describe() -> Vec<String> {
    lock(&LOADED).iter().map(|p| format!("plugin:        {} (from {})", p.entry, p.source)).collect()
}

