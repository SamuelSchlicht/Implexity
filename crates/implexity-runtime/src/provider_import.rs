// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderProblem, Sensitivity,
};
use implexity_core::orchestration::{Fidelity, PublishedContract};
use implexity_core::package_session::{BUSY, PackageService, check_generation};
use implexity_core::plugins::ExternalProvider;
use implexity_core::sync::lock;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::NamedArrays;
use implexity_optim::provider_ops::{
    DesignOp, DesignOperations, DesignSensitivity, design_interface,
};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub const SCHEMA: &str = "implexity-provider-import/1";

fn fail<T>(text: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(text))
}
fn text<'a>(v: &'a Value, k: &str) -> CaeResult<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| CaeError::contract(format!("{k} must be nonempty text")))
}
fn number(v: &Value, k: &str) -> CaeResult<f64> {
    v.get(k)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .ok_or_else(|| CaeError::contract(format!("{k} must be finite")))
}
fn digest(v: &Value) -> String {
    implexity_core::json::sha256_of(
        v,
        &implexity_core::json::DumpOptions::default().sorted(true),
    )
}
fn file_hash(path: &Path) -> CaeResult<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| CaeError::contract(e.to_string()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = f
            .read(&mut buffer)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn canonical_path(raw: &str, base: Option<&Path>) -> CaeResult<PathBuf> {
    let p = Path::new(raw);
    if !p.is_absolute() && base.is_none() {
        return fail("Uploaded manifests require absolute executable and asset paths");
    }
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.unwrap().join(p)
    };
    let p = p
        .canonicalize()
        .map_err(|e| CaeError::contract(format!("{}: {e}", p.display())))?;
    if !p.is_file() {
        return fail("Executable and assets must be files");
    }
    Ok(p)
}
fn units(v: &Value, key: &str, nonempty: bool) -> CaeResult<BTreeMap<String, String>> {
    let rows = v
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| CaeError::contract(format!("{key} must map names to units")))?;
    if nonempty && rows.is_empty() {
        return fail(format!("{key} must not be empty"));
    }
    rows.iter()
        .map(|(id, row)| {
            if id.trim().is_empty() {
                return fail("Output identifiers must be nonempty");
            }
            Ok((id.clone(), text(row, "unit")?.to_string()))
        })
        .collect()
}

#[derive(Clone)]
struct Manifest {
    value: Value,
    id: String,
    label: String,
    version: String,
    identity: String,
    command: Vec<String>,
    timeout: Duration,
    shape: Vec<usize>,
    lo: f64,
    hi: f64,
    inputs: Vec<Value>,
    responses: BTreeMap<String, String>,
    fields: BTreeMap<String, String>,
    gradients: bool,
}
impl Manifest {
    fn parse(raw: &Value, base: Option<&Path>) -> CaeResult<Self> {
        let mut v = raw.clone();
        let m = v
            .as_object()
            .ok_or_else(|| CaeError::contract("Provider manifest must be an object"))?;
        let allowed = [
            "schema",
            "id",
            "label",
            "kind",
            "version",
            "command",
            "executable_sha256",
            "assets",
            "timeout_seconds",
            "working_directory",
            "inputs",
            "topology",
            "responses",
            "fields",
            "gradients",
            "reference",
        ];
        if m.keys().any(|k| !allowed.contains(&k.as_str())) {
            return fail("Unknown provider manifest field");
        }
        if text(&v, "schema")? != SCHEMA {
            return fail("Unsupported provider manifest schema");
        }
        let id = text(&v, "id")?.to_string();
        let label = text(&v, "label")?.to_string();
        let version = text(&v, "version")?.to_string();
        if !matches!(text(&v, "kind")?, "learned" | "reduced_order" | "external") {
            return fail("kind must be learned, reduced_order or external");
        }
        let mut command = v["command"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| {
                CaeError::contract("command must contain an executable and optional arguments")
            })?
            .iter()
            .map(|x| {
                x.as_str()
                    .filter(|s| !s.contains('\0'))
                    .map(str::to_string)
                    .ok_or_else(|| {
                        CaeError::contract("command entries must be text without null bytes")
                    })
            })
            .collect::<CaeResult<Vec<_>>>()?;
        if let Some(directory) = v
            .get("working_directory")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| base.map(|p| p.display().to_string()))
        {
            let path = Path::new(&directory);
            if !path.is_absolute() && base.is_none() {
                return fail("working_directory must be absolute for uploaded manifests");
            }
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.unwrap().join(path)
            };
            let path = path
                .canonicalize()
                .map_err(|e| CaeError::contract(e.to_string()))?;
            if !path.is_dir() {
                return fail("working_directory must be a directory");
            }
            v["working_directory"] = json!(path);
        }
        let executable = canonical_path(&command[0], base)?;
        let executable_hash = file_hash(&executable)?;
        if v.get("executable_sha256")
            .is_some_and(|h| h.as_str() != Some(&executable_hash))
        {
            return fail("Provider executable changed");
        }
        command[0] = executable.display().to_string();
        v["command"] = json!(command);
        v["executable_sha256"] = json!(executable_hash);
        let assets = v["assets"]
            .as_array()
            .ok_or_else(|| {
                CaeError::contract("assets must be an array of path and sha256 records")
            })?
            .clone();
        if v["kind"] == "learned" && assets.is_empty() {
            return fail("Learned providers require declared model assets");
        }
        let mut normalized = Vec::new();
        for row in assets {
            let path = canonical_path(text(&row, "path")?, base)?;
            let hash = file_hash(&path)?;
            if text(&row, "sha256")? != hash {
                return fail(format!("Model asset changed: {}", path.display()));
            }
            normalized.push(json!({"path":path,"sha256":hash}));
        }
        v["assets"] = json!(normalized);
        let seconds = number(&v, "timeout_seconds")?;
        if !(0.01..=86400.0).contains(&seconds) {
            return fail("timeout_seconds must be between 0.01 and 86400");
        }
        let topology = &v["topology"];
        if text(topology, "coordinate")? != "model:control" {
            return fail("Imported providers currently support model:control");
        }
        let shape = topology["shape"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| CaeError::contract("topology.shape must be nonempty"))?
            .iter()
            .map(|x| {
                x.as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| {
                        CaeError::contract("Topology dimensions must be positive integers")
                    })
            })
            .collect::<CaeResult<Vec<_>>>()?;
        if shape
            .iter()
            .try_fold(1usize, |a, n| a.checked_mul(*n))
            .is_none()
        {
            return fail("Topology size overflows");
        }
        let lo = number(topology, "min")?;
        let hi = number(topology, "max")?;
        if lo >= hi {
            return fail("topology.min must be less than topology.max");
        }
        let inputs = v["inputs"]
            .as_array()
            .ok_or_else(|| CaeError::contract("inputs must be an array"))?
            .clone();
        let mut pointers = std::collections::BTreeSet::new();
        for input in &inputs {
            let pointer = text(input, "pointer")?;
            if !pointer.starts_with('/')
                || !pointers.insert(pointer)
                || number(input, "min")? > number(input, "max")?
            {
                return fail("Inputs require unique JSON pointers and ordered bounds");
            }
            text(input, "unit")?;
        }
        let gradients = v["gradients"]
            .as_bool()
            .ok_or_else(|| CaeError::contract("gradients must be boolean"))?;
        let responses = units(&v, "responses", true)?;
        let fields = units(&v, "fields", false)?;
        if let Some(reference) = v.get("reference").filter(|x| !x.is_null()) {
            text(reference, "provider")?;
            if reference["check_every"]
                .as_u64()
                .filter(|n| *n > 0)
                .is_none()
            {
                return fail("reference.check_every must be a positive integer");
            }
            for k in ["value_atol", "value_rtol", "gradient_atol", "gradient_rtol"] {
                if number(reference, k)? < 0.0 {
                    return fail("Reference tolerances must be nonnegative");
                }
            }
            if !matches!(text(reference, "on_failure")?, "reference" | "refuse") {
                return fail("reference.on_failure must be reference or refuse");
            }
        }
        let identity = digest(&v);
        Ok(Self {
            value: v,
            id,
            label,
            version,
            identity,
            command,
            timeout: Duration::from_secs_f64(seconds),
            shape,
            lo,
            hi,
            inputs,
            responses,
            fields,
            gradients,
        })
    }
    fn check_files(&self) -> CaeResult<()> {
        Self::parse(&self.value, None).map(|_| ())
    }
    fn problem_valid(&self, v: &Value) -> bool {
        self.inputs.iter().all(|i| {
            v.pointer(i["pointer"].as_str().unwrap())
                .and_then(Value::as_f64)
                .is_some_and(|n| {
                    n.is_finite()
                        && n >= i["min"].as_f64().unwrap()
                        && n <= i["max"].as_f64().unwrap()
                })
        })
    }
    fn topology_valid(&self, t: &ArrayD<f64>) -> bool {
        t.shape() == self.shape
            && t.iter()
                .all(|n| n.is_finite() && *n >= self.lo && *n <= self.hi)
    }
}
struct ImportedProblem {
    document: Value,
    approximate: Option<ProviderProblem>,
    reference: Option<ProviderProblem>,
    checks: Mutex<BTreeMap<String, u64>>,
    reference_only: Mutex<Option<String>>,
}

pub struct ImportedProvider {
    manifest: Manifest,
    model: ExternalProvider,
    reference: Option<Arc<dyn CaeProvider>>,
}
impl ImportedProvider {
    fn start(manifest: Manifest) -> CaeResult<Self> {
        let reference = match manifest.value.get("reference").filter(|x| !x.is_null()) {
            None => None,
            Some(r) => {
                let p = implexity_core::registries::global()
                    .providers
                    .get(text(r, "provider")?)?;
                if p.requires_explicit_selection() {
                    return fail("Reference cannot be another imported approximation");
                }
                let caps = p.capabilities()?.to_map();
                if p.capabilities()?.design_coordinates() != vec!["model:control".to_string()] {
                    return fail("Reference must support the same topology coordinate");
                }
                for (id, unit) in &manifest.responses {
                    if !caps["responses"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(id)))
                        || caps["response_metadata"][id]["unit"].as_str() != Some(unit)
                    {
                        return fail(format!("Reference response or unit mismatch: {id}"));
                    }
                }
                if manifest.gradients && caps["sensitivities"] != true {
                    return fail("Reference has no topology sensitivities");
                }
                Some(p)
            }
        };
        let model = ExternalProvider::start_configured(
            Path::new(&manifest.command[0]),
            &manifest.command[1..],
            Some(manifest.timeout),
            manifest
                .value
                .get("working_directory")
                .and_then(Value::as_str)
                .map(Path::new),
        )
        .map_err(|e| CaeError::contract(e.to_string()))?;
        if model.name() != manifest.id {
            return fail("Adapter name must match the manifest id");
        }
        let caps = model.capabilities()?.to_map();
        if model.capabilities()?.design_coordinates() != vec!["model:control".to_string()]
            || caps["sensitivities"].as_bool() != Some(manifest.gradients)
        {
            return fail("Adapter topology or gradient contract differs from the manifest");
        }
        for (key, names) in [
            ("responses", &manifest.responses),
            ("fields", &manifest.fields),
        ] {
            for id in names.keys() {
                if !caps[key]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(id)))
                {
                    return fail(format!("Adapter lacks declared {key}: {id}"));
                }
            }
        }
        for (id, unit) in &manifest.responses {
            if caps["response_metadata"][id]["unit"].as_str() != Some(unit) {
                return fail(format!("Adapter response unit mismatch: {id}"));
            }
        }
        let declaration = implexity_core::coupling_graph::validate_provider_couplings(
            &model,
            None,
            &implexity_core::registries::global().extensions,
            manifest.gradients,
        );
        if declaration["ok"] != true {
            return fail(format!(
                "Adapter coupling contract is invalid: {}",
                declaration["errors"]
            ));
        }
        Ok(Self {
            manifest,
            model,
            reference,
        })
    }
    fn problem<'a>(&self, p: &'a ProviderProblem) -> CaeResult<&'a ImportedProblem> {
        p.downcast_ref::<ImportedProblem>()
            .ok_or_else(|| CaeError::contract("Imported provider problem mismatch"))
    }
    fn reference_allowed(&self) -> bool {
        self.manifest.value["reference"]["on_failure"] == "reference"
    }
    fn reason(&self, p: &ImportedProblem, t: &ArrayD<f64>) -> Option<String> {
        lock(&p.reference_only).clone().or_else(|| {
            (!self.manifest.topology_valid(t)).then(|| "Outside the declared model range".into())
        })
    }
    fn annotate(
        &self,
        diagnostics: &mut Map<String, Value>,
        source: &str,
        reason: Option<&str>,
        checked: bool,
    ) {
        diagnostics.insert("response_units".into(), json!(self.manifest.responses));
        diagnostics.insert("field_units".into(), json!(self.manifest.fields));
        diagnostics.insert("execution_accuracy_scope".into(), json!("selected_provider_equations"));
        let metadata = diagnostics.entry("field_metadata").or_insert_with(|| json!({}));
        if let Some(fields) = metadata.as_object_mut() {
            for (id, unit) in &self.manifest.fields {
                let row = fields.entry(id.clone()).or_insert_with(|| json!({}));
                if let Some(row) = row.as_object_mut() {
                    row.insert("units".into(), json!(unit));
                    row.insert("source".into(), json!(if source == "reference" { "reference_provider" } else { "imported_model" }));
                }
            }
        }
        diagnostics.insert("physics_provider".into(),json!({"kind":self.manifest.value["kind"],"model_version":self.manifest.version,"model_identity":self.manifest.identity,"source":source,"reference_checked":checked,"reason":reason}));
    }
    fn reference_evaluation(
        &self,
        p: &ImportedProblem,
        t: &ArrayD<f64>,
        reason: &str,
    ) -> CaeResult<Evaluation> {
        if !self.reference_allowed() {
            return fail(reason);
        }
        let provider = self
            .reference
            .as_ref()
            .ok_or_else(|| CaeError::contract(reason))?;
        let mut e = provider.evaluate(
            p.reference
                .as_ref()
                .ok_or_else(|| CaeError::contract("Reference problem missing"))?,
            t,
        )?;
        e.provider = self.manifest.id.clone();
        self.annotate(&mut e.diagnostics, "reference", Some(reason), true);
        Ok(e)
    }
    fn reference_sensitivity(
        &self,
        p: &ImportedProblem,
        t: &ArrayD<f64>,
        response: &str,
        reason: &str,
    ) -> CaeResult<Sensitivity> {
        if !self.reference_allowed() {
            return fail(reason);
        }
        let provider = self
            .reference
            .as_ref()
            .ok_or_else(|| CaeError::contract(reason))?;
        let mut e = provider.sensitivity(
            p.reference
                .as_ref()
                .ok_or_else(|| CaeError::contract("Reference problem missing"))?,
            t,
            response,
        )?;
        e.provider = self.manifest.id.clone();
        self.annotate(&mut e.diagnostics, "reference", Some(reason), true);
        Ok(e)
    }
    fn due(&self, p: &ImportedProblem, key: &str) -> bool {
        if self.reference.is_none() {
            return false;
        }
        let mut counts = lock(&p.checks);
        let count = counts.entry(key.to_string()).or_default();
        let due = *count
            % self.manifest.value["reference"]["check_every"]
                .as_u64()
                .unwrap()
            == 0;
        *count += 1;
        due
    }
    fn close(&self, a: f64, b: f64, gradient: bool) -> bool {
        let r = &self.manifest.value["reference"];
        let prefix = if gradient { "gradient" } else { "value" };
        a.is_finite()
            && b.is_finite()
            && (a - b).abs()
                <= r[format!("{prefix}_atol")].as_f64().unwrap()
                    + r[format!("{prefix}_rtol")].as_f64().unwrap() * b.abs()
    }
    fn check_evaluation(&self, e: &Evaluation) -> CaeResult<()> {
        for id in self.manifest.responses.keys() {
            if !e.responses.get(id).is_some_and(|v| v.is_finite()) {
                return fail(format!("Missing or invalid response: {id}"));
            }
        }
        for id in self.manifest.fields.keys() {
            match e.fields.get(id) {
                Some(FieldValue::Array(a)) if a.iter().all(|x| x.is_finite()) => (),
                _ => return fail(format!("Missing or invalid numerical field: {id}")),
            }
        }
        Ok(())
    }
    fn named<'a>(&self, d: &'a NamedArrays, op: usize) -> CaeResult<&'a ArrayD<f64>> {
        if op != 0 || d.names() != vec!["model:control".to_string()] {
            return fail("Imported provider requires a single model:control operating point");
        }
        d.get("model:control")
            .ok_or_else(|| CaeError::contract("Topology missing"))
    }
}
impl CaeProvider for ImportedProvider {
    fn requires_explicit_selection(&self) -> bool {
        true
    }
    fn provider_dependencies(&self) -> Vec<String> {
        self.reference
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }
    fn name(&self) -> &str {
        &self.manifest.id
    }
    fn implementation(&self) -> &str {
        &self.manifest.identity
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut caps = match self.model.capabilities()? {
            ProviderCapabilities::Legacy(c) => *c,
            ProviderCapabilities::Descriptor(c) => {
                let mut caps = implexity_core::contracts::LegacySingleArrayProviderCapabilities::new(
                    self.manifest.id.clone(), Vec::new(), Vec::new(),
                );
                caps.base = *c;
                caps
            }
            ProviderCapabilities::Mapping(_) => return fail("Adapter requires typed capabilities"),
        };
        caps.base.name = self.manifest.id.clone();
        caps.base.notes.push("Imported approximation".into());
        caps.base.responses = self.manifest.responses.keys().cloned().collect();
        caps.base.fields = self.manifest.fields.keys().cloned().collect();
        caps.base.sensitivities = self.manifest.gradients;
        caps.base.response_metadata = self.manifest.responses.iter().map(|(id, unit)| (
            id.clone(), json!({"unit":unit,"differentiable":self.manifest.gradients}),
        )).collect();
        caps.base.traits.insert("imported".into(), json!(true));
        caps.base.traits.insert("explicit_selection_required".into(), json!(true));
        caps.base.traits.insert("approximate".into(), json!(true));
        caps.base.traits.insert("model_version".into(), json!(self.manifest.version));
        caps.base.traits.insert("model_identity".into(), json!(self.manifest.identity));
        caps.base.presentation.insert("label".into(), json!(self.manifest.label));
        caps.execution = "array".into();
        Ok(ProviderCapabilities::Legacy(Box::new(caps.checked()?)))
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        self.model.orchestration_contract().map(|result| {
            result.and_then(|contract| {
                let mut c = match contract {
                    PublishedContract::Contract(c) => *c,
                    PublishedContract::Mapping(v) => {
                        implexity_core::orchestration::AddInContract::from_mapping(&v, true)?
                    }
                };
                c.fidelity = Fidelity::Screening;
                c.notes
                    .push("Imported approximation; explicit selection required".into());
                c.validate()?;
                Ok(PublishedContract::Contract(Box::new(c)))
            })
        })
    }
    fn normalise_problem(&self, doc: &Value) -> CaeResult<ProviderProblem> {
        self.manifest.check_files()?;
        let valid = self.manifest.problem_valid(doc);
        if !valid && !self.reference_allowed() {
            return fail("Problem is outside the declared model range");
        }
        let reference = self
            .reference
            .as_ref()
            .map(|p| p.normalise_problem(doc))
            .transpose()?;
        let approximate = if valid {
            Some(self.model.normalise_problem(doc)?)
        } else {
            None
        };
        Ok(Arc::new(ImportedProblem {
            document: doc.clone(),
            approximate,
            reference,
            checks: Mutex::new(BTreeMap::new()),
            reference_only: Mutex::new(
                (!valid).then(|| "Problem is outside the declared model range".into()),
            ),
        }))
    }
    fn preflight(
        &self,
        p: &ProviderProblem,
        t: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let p = self.problem(p)?;
        if t.is_some_and(|t| t.shape() != self.manifest.shape) {
            return fail("Topology shape differs from the model contract");
        }
        let reason = lock(&p.reference_only).clone().or_else(|| {
            t.filter(|t| !self.manifest.topology_valid(t))
                .map(|_| "Outside the declared model range".to_string())
        });
        let mut result = if let Some(reason) = &reason {
            if !self.reference_allowed() {
                return fail(reason);
            }
            self.reference
                .as_ref()
                .ok_or_else(|| CaeError::contract(reason))?
                .preflight(p.reference.as_ref().unwrap(), t)?
        } else {
            self.model.preflight(p.approximate.as_ref().unwrap(), t)?
        };
        self.annotate(
            &mut result,
            if reason.is_some() {
                "reference"
            } else {
                "approximation"
            },
            reason.as_deref(),
            false,
        );
        Ok(result)
    }
    fn evaluate(&self, problem: &ProviderProblem, t: &ArrayD<f64>) -> CaeResult<Evaluation> {
        let p = self.problem(problem)?;
        if t.shape() != self.manifest.shape {
            return fail("Topology shape differs from the model contract");
        }
        if let Some(reason) = self.reason(p, t) {
            return self.reference_evaluation(p, t, &reason);
        }
        let result = self
            .model
            .evaluate(p.approximate.as_ref().unwrap(), t)
            .and_then(|e| {
                self.check_evaluation(&e)?;
                Ok(e)
            });
        let mut e = match result {
            Ok(e) => e,
            Err(error) => {
                *lock(&p.reference_only) = Some(error.message().into());
                return self.reference_evaluation(p, t, error.message());
            }
        };
        let checked = self.due(p, "evaluate");
        if checked {
            let reference = self
                .reference
                .as_ref()
                .unwrap()
                .evaluate(p.reference.as_ref().unwrap(), t)?;
            if self.manifest.responses.keys().any(|id| {
                !reference
                    .responses
                    .get(id)
                    .is_some_and(|v| self.close(e.responses[id], *v, false))
            }) {
                *lock(&p.reference_only) =
                    Some("Prediction differs from the integrated reference".into());
                if !self.reference_allowed() {
                    return fail("Prediction differs from the integrated reference");
                }
                e = reference;
                e.provider = self.manifest.id.clone();
                self.annotate(
                    &mut e.diagnostics,
                    "reference",
                    Some("Prediction differs from the integrated reference"),
                    true,
                );
                return Ok(e);
            }
        }
        e.provider = self.manifest.id.clone();
        self.annotate(&mut e.diagnostics, "approximation", None, checked);
        Ok(e)
    }
    fn sensitivity(
        &self,
        problem: &ProviderProblem,
        t: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        if !self.manifest.gradients || !self.manifest.responses.contains_key(response) {
            return fail("Model has no declared gradient for this response");
        }
        let p = self.problem(problem)?;
        if t.shape() != self.manifest.shape {
            return fail("Topology shape differs from the model contract");
        }
        if let Some(reason) = self.reason(p, t) {
            return self.reference_sensitivity(p, t, response, &reason);
        }
        let result = self
            .model
            .sensitivity(p.approximate.as_ref().unwrap(), t, response)
            .and_then(|s| {
                if s.gradient.shape() != t.shape()
                    || !s.gradient.iter().all(|n| n.is_finite())
                    || !s.value.is_finite()
                {
                    return fail("Model gradient shape or values are invalid");
                }
                Ok(s)
            });
        let mut e = match result {
            Ok(s) => s,
            Err(error) => {
                *lock(&p.reference_only) = Some(error.message().into());
                return self.reference_sensitivity(p, t, response, error.message());
            }
        };
        let checked = self.due(p, &format!("sensitivity:{response}"));
        if checked {
            let r = self.reference.as_ref().unwrap().sensitivity(
                p.reference.as_ref().unwrap(),
                t,
                response,
            )?;
            if r.gradient.shape() != t.shape()
                || !self.close(e.value, r.value, false)
                || !e
                    .gradient
                    .iter()
                    .zip(r.gradient.iter())
                    .all(|(a, b)| self.close(*a, *b, true))
            {
                *lock(&p.reference_only) =
                    Some("Gradient differs from the integrated reference".into());
                if !self.reference_allowed() {
                    return fail("Gradient differs from the integrated reference");
                }
                e = r;
                e.provider = self.manifest.id.clone();
                self.annotate(
                    &mut e.diagnostics,
                    "reference",
                    Some("Gradient differs from the integrated reference"),
                    true,
                );
                return Ok(e);
            }
        }
        e.provider = self.manifest.id.clone();
        self.annotate(&mut e.diagnostics, "approximation", None, checked);
        Ok(e)
    }
    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        self.model.coupling_declaration(None)
    }
    fn mathematical_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<Value> {
        self.model.mathematical_declaration(None)
    }
    fn semantic_physics_contract(&self, _problem: Option<&ProviderProblem>) -> Option<Value> {
        self.model.semantic_physics_contract(None)
    }
    fn coupling_inventory(&self, _problem: Option<&ProviderProblem>) -> Option<Value> {
        self.model.coupling_inventory(None)
    }
    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        design_interface::<Self>(name)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl DesignOperations for ImportedProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::Evaluate
                | DesignOp::EvaluateDesign
                | DesignOp::EvaluateResultsDesign
                | DesignOp::PreflightDesign
        ) || self.manifest.gradients
            && matches!(op, DesignOp::Sensitivity | DesignOp::SensitivityDesign)
    }
    fn evaluate_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        op: usize,
    ) -> CaeResult<Evaluation> {
        self.evaluate(p, self.named(d, op)?)
    }
    fn evaluate_results_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
    ) -> CaeResult<Evaluation> {
        if let Some(reference) = &self.reference {
            let problem = self.problem(p)?;
            let t = self.named(d, 0)?;
            let mut result = reference.evaluate(
                problem
                    .reference
                    .as_ref()
                    .ok_or_else(|| CaeError::contract("Reference problem missing"))?,
                t,
            )?;
            result.provider = self.manifest.id.clone();
            self.annotate(
                &mut result.diagnostics,
                "reference",
                Some("Result evaluation uses the reference provider"),
                true,
            );
            Ok(result)
        } else {
            self.evaluate_design(p, d, 0)
        }
    }
    fn preflight_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        self.preflight(p, Some(self.named(d, 0)?))
    }
    fn sensitivity_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        r: &str,
        op: usize,
    ) -> CaeResult<DesignSensitivity> {
        let s = self.sensitivity(p, self.named(d, op)?, r)?;
        Ok(DesignSensitivity {
            value: s.value,
            gradients: NamedArrays::single("model:control", s.gradient),
            diagnostics: s.diagnostics,
        })
    }
    fn problem_document(&self, p: &ProviderProblem) -> Option<CaeResult<Value>> {
        Some(self.problem(p).map(|p| p.document.clone()))
    }
}

struct Entry {
    manifest: Manifest,
    provider: Arc<dyn CaeProvider>,
}
static IMPORTS: LazyLock<Mutex<BTreeMap<String, Entry>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub fn snapshot() -> Value {
    json!(
        lock(&IMPORTS)
            .values()
            .map(|e| e.manifest.value.clone())
            .collect::<Vec<_>>()
    )
}
pub fn restore_snapshot(v: &Value) -> CaeResult<()> {
    let _hold = implexity_core::packages::global().hold();
    let rows = v
        .as_array()
        .ok_or_else(|| CaeError::contract("Imported provider snapshot must be an array"))?;
    let mut added = Vec::new();
    let result = (|| {
        for raw in rows {
            let manifest = Manifest::parse(raw, None)?;
            if let Some(e) = lock(&IMPORTS).get(&manifest.id) {
                if e.manifest.identity != manifest.identity {
                    return fail("Imported provider identity changed");
                }
                continue;
            }
            let id = manifest.id.clone();
            install(manifest)?;
            added.push(id);
        }
        Ok(())
    })();
    if result.is_err() {
        for id in added.into_iter().rev() {
            let _ = remove(&id);
        }
    }
    result
}
fn install(manifest: Manifest) -> CaeResult<()> {
    let provider: Arc<dyn CaeProvider> = Arc::new(ImportedProvider::start(manifest.clone())?);
    implexity_core::registries::global().register_provider(Arc::clone(&provider))?;
    lock(&IMPORTS).insert(manifest.id.clone(), Entry { manifest, provider });
    Ok(())
}
fn remove(id: &str) -> CaeResult<()> {
    let provider = lock(&IMPORTS)
        .get(id)
        .map(|e| Arc::clone(&e.provider))
        .ok_or_else(|| CaeError::contract("Unknown imported provider"))?;
    let registry = implexity_core::registries::global();
    registry
        .providers
        .unregister(&registry.addins, id, Some(&provider))?;
    lock(&IMPORTS).remove(id);
    Ok(())
}

pub struct ImportSession {
    host: &'static dyn PackageService,
    path: PathBuf,
    error: Mutex<Option<String>>,
}
impl ImportSession {
    pub fn new(host: &'static dyn PackageService) -> Self {
        let session = Self {
            path: host.state_dir().join("imported_providers.json"),
            host,
            error: Mutex::new(None),
        };
        if session.path.exists() {
            let result = implexity_core::json::read_file(&session.path)
                .map_err(CaeError::contract)
                .and_then(|v| restore_snapshot(&v));
            if let Err(e) = result {
                *lock(&session.error) = Some(e.message().into());
            }
        }
        session
    }
    pub fn status(&self) -> Value {
        let packages = implexity_core::packages::global();
        json!({"schema":"implexity-imported-providers/1","generation":packages.generation(),"provider_generation":packages.registries().providers.generation(),"providers":lock(&IMPORTS).values().map(|e|json!({"id":e.manifest.id,"label":e.manifest.label,"kind":e.manifest.value["kind"],"version":e.manifest.version,"identity":e.manifest.identity,"manifest":e.manifest.value,"selection":"explicit","gradients":e.manifest.gradients})).collect::<Vec<_>>(),"restoration_error":lock(&self.error).clone(),"lifecycle_blockers":self.blockers()})
    }
    fn blockers(&self) -> Vec<Value> {
        let jobs = self.host.jobs();
        let rows = jobs
            .get("jobs")
            .and_then(Value::as_array)
            .or_else(|| jobs.as_array());
        rows.into_iter()
            .flatten()
            .filter(|j| j["status"].as_str().is_some_and(|s| BUSY.contains(&s)))
            .map(|j| {
                j.get("job_id")
                    .or_else(|| j.get("id"))
                    .cloned()
                    .unwrap_or(Value::Null)
            })
            .collect()
    }
    fn persist(&self) -> CaeResult<()> {
        std::fs::create_dir_all(self.path.parent().unwrap())
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let tmp = self.path.with_extension("pending");
        std::fs::write(
            &tmp,
            implexity_core::json::dumps(
                &snapshot(),
                &implexity_core::json::DumpOptions::default().sorted(true),
            ),
        )
        .map_err(|e| CaeError::contract(e.to_string()))?;
        std::fs::rename(tmp, &self.path).map_err(|e| CaeError::contract(e.to_string()))
    }
    pub fn change(&self, request: &Value) -> CaeResult<Value> {
        let packages = implexity_core::packages::global();
        let _hold = packages.hold();
        check_generation(request.get("expected_generation"), packages.generation())?;
        check_generation(
            request.get("expected_provider_generation"),
            packages.registries().providers.generation(),
        )?;
        if !self.blockers().is_empty() {
            return fail("PHYSICS_IN_USE: a running or paused job owns the physics snapshot");
        }
        let _eval = self
            .host
            .try_acquire_evaluation()
            .ok_or_else(|| CaeError::contract("PHYSICS_IN_USE: a solver is evaluating"))?;
        match text(request, "operation")? {
            "import" => {
                let (raw, base) = if let Some(path) =
                    request.get("manifest_path").and_then(Value::as_str)
                {
                    if request.get("manifest").is_some() {
                        return fail("Provide manifest or manifest_path, not both");
                    }
                    let path = canonical_path(path, None)?;
                    let raw = implexity_core::json::read_file(&path).map_err(CaeError::contract)?;
                    (raw, path.parent().map(Path::to_path_buf))
                } else {
                    (
                        request
                            .get("manifest")
                            .cloned()
                            .ok_or_else(|| CaeError::contract("Provider manifest missing"))?,
                        None,
                    )
                };
                let manifest = Manifest::parse(&raw, base.as_deref())?;
                let id = manifest.id.clone();
                if lock(&IMPORTS).contains_key(&id) {
                    return fail("Provider already imported; remove it before replacing it");
                }
                install(manifest)?;
                if let Err(e) = self.persist() {
                    remove(&id)?;
                    return Err(e);
                }
            }
            "remove" => {
                let id = text(request, "provider")?;
                let saved = lock(&IMPORTS)
                    .get(id)
                    .map(|e| e.manifest.clone())
                    .ok_or_else(|| CaeError::contract("Unknown imported provider"))?;
                remove(id)?;
                if let Err(e) = self.persist() {
                    install(saved)?;
                    return Err(e);
                }
            }
            _ => return fail("operation must be import or remove"),
        }
        *lock(&self.error) = None;
        let out = self.status();
        self.host
            .broadcast(json!({"kind":"physics_providers_changed","state":out}));
        Ok(out)
    }
    pub fn verify(&self, request: &Value) -> CaeResult<Value> {
        let packages = implexity_core::packages::global();
        let _hold = packages.hold();
        let _eval = self
            .host
            .try_acquire_evaluation()
            .ok_or_else(|| CaeError::contract("PHYSICS_IN_USE: a solver is evaluating"))?;
        if !self.blockers().is_empty() {
            return fail("PHYSICS_IN_USE: a job is active");
        }
        let id = text(request, "provider")?;
        let provider = lock(&IMPORTS)
            .get(id)
            .map(|e| Arc::clone(&e.provider))
            .ok_or_else(|| CaeError::contract("Unknown imported provider"))?;
        let p = provider
            .as_any()
            .downcast_ref::<ImportedProvider>()
            .unwrap();
        let doc = request
            .get("problem")
            .ok_or_else(|| CaeError::contract("problem missing"))?;
        if !p.manifest.problem_valid(doc) {
            return fail("Verification problem is outside the declared model range");
        }
        let topo = &request["topology"];
        let shape = topo["shape"]
            .as_array()
            .ok_or_else(|| CaeError::contract("topology requires shape and data"))?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| CaeError::contract("Invalid topology shape"))
            })
            .collect::<CaeResult<Vec<_>>>()?;
        let data = topo["data"]
            .as_array()
            .ok_or_else(|| CaeError::contract("topology.data must be an array"))?
            .iter()
            .map(|v| {
                v.as_f64()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| CaeError::contract("Invalid topology value"))
            })
            .collect::<CaeResult<Vec<_>>>()?;
        let t = ArrayD::from_shape_vec(IxDyn(&shape), data)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        if !p.manifest.topology_valid(&t) {
            return fail("Verification topology is outside the declared model range");
        }
        let problem = p.model.normalise_problem(doc)?;
        let evaluated = p.model.evaluate(&problem, &t)?;
        p.check_evaluation(&evaluated)?;
        let step = number(request, "step")?;
        let atol = number(request, "atol")?;
        let rtol = number(request, "rtol")?;
        if step <= 0.0 || atol < 0.0 || rtol < 0.0 {
            return fail("Verification requires a positive step and nonnegative tolerances");
        }
        let indices = request["indices"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 64)
            .ok_or_else(|| CaeError::contract("indices must contain 1 to 64 topology indices"))?;
        let mut report = Vec::new();
        let mut passed = true;
        for response in p.manifest.responses.keys() {
            if !p.manifest.gradients {
                continue;
            }
            let sensitivity = p.model.sensitivity(&problem, &t, response)?;
            if sensitivity.gradient.shape() != t.shape()
                || !sensitivity.gradient.iter().all(|n| n.is_finite())
            {
                return fail("Invalid model gradient");
            }
            let value_ok = (sensitivity.value - evaluated.responses[response]).abs()
                <= atol + rtol * evaluated.responses[response].abs();
            passed &= value_ok;
            for index in indices {
                let index = index
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n < t.len())
                    .ok_or_else(|| CaeError::contract("Verification index out of range"))?;
                let mut minus = t.clone();
                let mut plus = t.clone();
                minus.as_slice_mut().unwrap()[index] -= step;
                plus.as_slice_mut().unwrap()[index] += step;
                if !p.manifest.topology_valid(&minus) || !p.manifest.topology_valid(&plus) {
                    return fail("Finite-difference perturbation leaves the model range");
                }
                let a = p.model.evaluate(&problem, &minus)?;
                p.check_evaluation(&a)?;
                let b = p.model.evaluate(&problem, &plus)?;
                p.check_evaluation(&b)?;
                let fd = (b.responses[response] - a.responses[response]) / (2.0 * step);
                let derivative = sensitivity.gradient.iter().nth(index).copied().unwrap();
                let ok = fd.is_finite() && (derivative - fd).abs() <= atol + rtol * fd.abs();
                passed &= ok;
                report.push(json!({"response":response,"index":index,"analytic":derivative,"finite_difference":fd,"passed":ok,"response_value_consistent":value_ok}));
            }
        }
        let mut reference_report = Vec::new();
        if let Some(reference) = &p.reference {
            let rp = reference.normalise_problem(doc)?;
            let re = reference.evaluate(&rp, &t)?;
            for response in p.manifest.responses.keys() {
                let value = *re
                    .responses
                    .get(response)
                    .ok_or_else(|| CaeError::contract("Reference response missing"))?;
                let value_ok = p.close(evaluated.responses[response], value, false);
                let mut gradient_ok = Value::Null;
                if p.manifest.gradients {
                    let a = p.model.sensitivity(&problem, &t, response)?;
                    let r = reference.sensitivity(&rp, &t, response)?;
                    let ok = a.gradient.shape() == t.shape()
                        && r.gradient.shape() == t.shape()
                        && a.gradient
                            .iter()
                            .zip(r.gradient.iter())
                            .all(|(a, b)| p.close(*a, *b, true));
                    passed &= ok;
                    gradient_ok = json!(ok);
                }
                passed &= value_ok;
                reference_report.push(json!({"response":response,"prediction":evaluated.responses[response],"reference":value,"value_passed":value_ok,"gradient_passed":gradient_ok}));
            }
        }
        Ok(
            json!({"schema":"implexity-imported-provider-check/1","provider":id,"identity":p.manifest.identity,"passed":passed,"gradient_checks":report,"reference_checks":reference_report,"scope":"Supplied problem, topology and sampled derivative indices only","globally_validated":false}),
        )
    }
}
