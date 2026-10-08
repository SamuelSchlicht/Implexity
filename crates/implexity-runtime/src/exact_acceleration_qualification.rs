// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use crate::canonical::{canonical_sha256, is_digest, sha256_hex};

pub const PROMOTION_REGISTRY_SOURCE: &[u8] =
    include_bytes!("../data/exact_acceleration_promotion_registry.py");
pub const QUALIFICATION_FACTS_SCHEMA: &str = "implexity-exact-acceleration-facts/3";
pub const QUALIFICATION_SELECTION_SCHEMA: &str = "implexity-exact-acceleration-selection/3";
pub const PREEXECUTION_MECHANISM_EVIDENCE_SCHEMA: &str =
    "implexity-exact-acceleration-preexecution-mechanism-evidence/1";
pub const CANONICAL_ALLOWLIST_SCHEMA: &str = "implexity-exact-acceleration-promotions/1";
pub const EXACT_DIRECT_POLICY: &str = "exact_direct_default";
pub const NONDEFAULT_EXACT_PROFILE: &str = "nondefault_exact_profile";
pub const PARALLEL_LOGICAL_BATCH: &str = "parallel_logical_batch";
pub const BOUNDED_WORKER_THREADS: &str = "bounded_worker_threads";
pub const EXECUTABLE_RESIDENCY: &str = "executable_residency";
pub const SPARSE_STRUCTURE_REUSE: &str = "sparse_structure_reuse";
pub const PROVIDER_OWNED_EXACT_PRECONDITIONER: &str = "provider_owned_exact_preconditioner";
pub const TRANSACTIONAL_KRYLOV_RECYCLE: &str = "transactional_krylov_recycle";
pub const FEATURES: [&str; 7] = [
    BOUNDED_WORKER_THREADS,
    EXECUTABLE_RESIDENCY,
    NONDEFAULT_EXACT_PROFILE,
    PARALLEL_LOGICAL_BATCH,
    PROVIDER_OWNED_EXACT_PRECONDITIONER,
    SPARSE_STRUCTURE_REUSE,
    TRANSACTIONAL_KRYLOV_RECYCLE,
];
const MAX_SAFE_JSON_INTEGER: i64 = (1 << 53) - 1;
const MAX_WORKER_THREADS: i64 = 4096;
const MAX_LOGICAL_BATCH_CONCURRENCY: i64 = 4096;

fn qerr(m: impl Into<String>) -> CaeError {
    CaeError::contract(m.into())
}

#[derive(Debug, Clone, PartialEq)]
enum Literal {
    Str(String),
    Tuple(Vec<Literal>),
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    label: &'a str,
}

impl Lexer<'_> {
    fn not_literal(&self) -> CaeError {
        qerr(format!("{} is not a safe literal", self.label))
    }

    fn skip_space(&mut self, newlines: bool) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'\n' if newlines => self.i += 1,
                b'\\' if self.s.get(self.i + 1) == Some(&b'\n') => self.i += 2,
                b'#' => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn name(&mut self) -> Option<String> {
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn string(&mut self) -> CaeResult<String> {
        let quote = self.s[self.i];
        let triple = self.s.get(self.i..self.i + 3) == Some(&[quote, quote, quote][..]);
        self.i += if triple { 3 } else { 1 };
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return Err(self.not_literal()) };
            if triple && self.s.get(self.i..self.i + 3) == Some(&[quote, quote, quote][..]) {
                self.i += 3;
                break;
            }
            if !triple && c == quote {
                self.i += 1;
                break;
            }
            if !triple && c == b'\n' {
                return Err(self.not_literal());
            }
            if c == b'\\' {
                let Some(&n) = self.s.get(self.i + 1) else { return Err(self.not_literal()) };
                let mapped = match n {
                    b'\\' => b'\\',
                    b'\'' => b'\'',
                    b'"' => b'"',
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'\n' => {
                        self.i += 2;
                        continue;
                    }
                    _ => return Err(self.not_literal()),
                };
                out.push(mapped);
                self.i += 2;
                continue;
            }
            out.push(c);
            self.i += 1;
        }
        String::from_utf8(out).map_err(|_| self.not_literal())
    }

    fn literal(&mut self, label: &str) -> CaeResult<Literal> {
        self.skip_space(false);
        match self.s.get(self.i) {
            Some(b'\'' | b'"') => {
                let mut text = self.string()?;
                loop {
                    let save = self.i;
                    self.skip_space(false);
                    if matches!(self.s.get(self.i), Some(b'\'' | b'"')) {
                        text.push_str(&self.string()?);
                    } else {
                        self.i = save;
                        break;
                    }
                }
                Ok(Literal::Str(text))
            }
            Some(b'(') => {
                self.i += 1;
                let mut items = Vec::new();
                let mut trailing_comma = false;
                loop {
                    self.skip_space(true);
                    if self.s.get(self.i) == Some(&b')') {
                        self.i += 1;
                        break;
                    }
                    items.push(self.literal(label)?);
                    self.skip_space(true);
                    match self.s.get(self.i) {
                        Some(b',') => {
                            self.i += 1;
                            trailing_comma = true;
                        }
                        Some(b')') => {
                            self.i += 1;
                            trailing_comma = false;
                            break;
                        }
                        _ => return Err(qerr(format!("{label} is not literal"))),
                    }
                }

                if items.len() == 1 && !trailing_comma {
                    return Ok(items.remove(0));
                }
                Ok(Literal::Tuple(items))
            }
            _ => Err(qerr(format!("{label} is not literal"))),
        }
    }

    fn end_of_statement(&mut self, label: &str) -> CaeResult<()> {
        self.skip_space(false);
        match self.s.get(self.i) {
            None => Ok(()),
            Some(b'\n' | b';') => {
                self.i += 1;
                Ok(())
            }
            _ => Err(qerr(format!("{label} contains executable code"))),
        }
    }

    fn annotation(&mut self, label: &str) -> CaeResult<()> {
        let mut depth = 0i32;
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b'[' | b'(' => depth += 1,
                b']' | b')' => depth -= 1,
                b'=' if depth == 0 => return Ok(()),
                b'\n' if depth == 0 => return Err(qerr(format!("{label} has an invalid assignment"))),
                _ => {}
            }
            self.i += 1;
        }
        Err(qerr(format!("{label} has an invalid assignment")))
    }
}


#[allow(clippy::too_many_lines)]
pub fn parse_exact_acceleration_promotion_registry(
    raw: &[u8],
    label: &str,
) -> CaeResult<Vec<(String, String)>> {
    if raw.is_empty() || raw.len() > 256 * 1024 {
        return Err(qerr(format!("{label} is not one bounded byte string")));
    }
    if std::str::from_utf8(raw).is_err() {
        return Err(qerr(format!("{label} is not a safe literal")));
    }
    let mut lx = Lexer { s: raw, i: 0, label };
    let mut promotion: Option<Literal> = None;
    let mut index = 0usize;
    loop {
        lx.skip_space(true);
        if lx.i >= raw.len() {
            break;
        }
        let statement_start = lx.i;
        if matches!(raw[lx.i], b'\'' | b'"') {
            if index != 0 {
                return Err(qerr(format!("{label} contains executable code")));
            }
            lx.literal(label)?;
            lx.end_of_statement(label)?;
            index += 1;
            continue;
        }
        let Some(word) = lx.name() else { return Err(qerr(format!("{label} contains executable code"))) };
        if word == "from" {
            let rest = raw[lx.i..].split(|b| *b == b'\n').next().unwrap_or_default();
            let text: Vec<&str> = std::str::from_utf8(rest).unwrap_or_default().split_whitespace().collect();
            if text != ["__future__", "import", "annotations"] {
                return Err(qerr(format!("{label} contains executable code")));
            }
            lx.i += rest.len();
            index += 1;
            continue;
        }
        lx.skip_space(false);
        let annotated = lx.s.get(lx.i) == Some(&b':');
        if annotated {
            lx.i += 1;
            lx.annotation(label)?;
        }
        if lx.s.get(lx.i) != Some(&b'=') || lx.s.get(lx.i + 1) == Some(&b'=') {
            let _ = statement_start;
            return Err(qerr(format!("{label} contains executable code")));
        }
        lx.i += 1;
        if word != "CANONICAL_EXACT_ACCELERATION_PROMOTIONS" && word != "__all__" {
            return Err(qerr(format!("{label} has an invalid assignment")));
        }
        let value = lx.literal(label)?;
        lx.end_of_statement(label)?;
        if word == "CANONICAL_EXACT_ACCELERATION_PROMOTIONS" {
            if promotion.is_some() {
                return Err(qerr(format!("{label} is duplicated")));
            }
            promotion = Some(value);
        } else if value
            != Literal::Tuple(vec![Literal::Str("CANONICAL_EXACT_ACCELERATION_PROMOTIONS".into())])
        {
            return Err(qerr(format!("{label} export drifted")));
        }
        index += 1;
    }
    let Some(Literal::Tuple(items)) = promotion else {
        return Err(qerr(format!("{label} is absent")));
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for item in items {
        let Literal::Tuple(pair) = item else { return Err(qerr(format!("{label} entry is malformed"))) };
        let [Literal::Str(a), Literal::Str(b)] = pair.as_slice() else {
            return Err(qerr(format!("{label} entry is malformed")));
        };
        if !is_digest(a) || !is_digest(b) {
            return Err(qerr(format!("{label} entry is malformed")));
        }
        let entry = (a.clone(), b.clone());
        if out.last().is_some_and(|prev| entry <= *prev) {
            return Err(qerr(format!("{label} is not canonical")));
        }
        out.push(entry);
    }
    Ok(out)
}

type Registry = (Vec<(String, String)>, String);

fn registry() -> &'static CaeResult<Registry> {
    static REGISTRY: OnceLock<CaeResult<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let entries = parse_exact_acceleration_promotion_registry(
            PROMOTION_REGISTRY_SOURCE,
            "exact acceleration promotion registry",
        )?;
        Ok((entries, sha256_hex(PROMOTION_REGISTRY_SOURCE)))
    })
}


pub fn canonical_promotions() -> CaeResult<Vec<(String, String)>> {
    registry().as_ref().map(|r| r.0.clone()).map_err(Clone::clone)
}


pub fn promotion_registry_sha256() -> CaeResult<String> {
    registry().as_ref().map(|r| r.1.clone()).map_err(Clone::clone)
}


pub fn derive_preexecution_mechanism_evidence_sha256(
    implementation_closure_sha256: &str,
) -> CaeResult<String> {
    if !is_digest(implementation_closure_sha256) {
        return Err(qerr("implementation_closure_sha256 must be a lowercase SHA-256 digest"));
    }
    Ok(canonical_sha256(&json!({
        "schema": PREEXECUTION_MECHANISM_EVIDENCE_SCHEMA,
        "implementation_closure_sha256": implementation_closure_sha256,
        "facts_schema": QUALIFICATION_FACTS_SCHEMA,
        "selection_schema": QUALIFICATION_SELECTION_SCHEMA,
        "closed_features": FEATURES,
        "admission_semantics": "source_owned_pair_registry_and_child_local_exact_authority",
        "terminal_evidence_excluded": true,
    })))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactAccelerationQualificationFacts {
    text: BTreeMap<&'static str, String>,
    provider_registry_generation: i64,
    features: Vec<String>,
    resources: [i64; 6],
}

const FACT_TEXT: [&str; 4] = ["provider_id", "solver_policy", "operation_class", "problem_size_class"];
const FACT_DIGESTS: [&str; 8] = [
    "provider_descriptor_sha256",
    "provider_registry_fingerprint",
    "provider_load_order_sha256",
    "exact_profile_sha256",
    "policy_source_sha256",
    "compile_sha256",
    "runtime_profile_sha256",
    "preexecution_mechanism_evidence_sha256",
];
const RESOURCE_NAMES: [&str; 6] = [
    "worker_thread_limit",
    "logical_batch_concurrency",
    "logical_batch_memory_limit_bytes",
    "executable_residency_memory_limit_bytes",
    "sparse_structure_memory_limit_bytes",
    "solver_workspace_memory_limit_bytes",
];

impl ExactAccelerationQualificationFacts {

    #[allow(clippy::too_many_lines)]
    pub fn new(
        text: &BTreeMap<String, String>,
        provider_registry_generation: i64,
        features: Vec<String>,
        resources: [i64; 6],
    ) -> CaeResult<Self> {
        let mut fields = BTreeMap::new();
        for name in FACT_TEXT {
            let v = text.get(name).map_or("", String::as_str);
            if v.is_empty() || v != v.trim() || v.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
                return Err(qerr(format!("{name} must be a non-empty canonical string")));
            }
            fields.insert(name, v.to_string());
        }
        for name in FACT_DIGESTS {
            let v = text.get(name).map_or("", String::as_str);
            if !is_digest(v) {
                return Err(qerr(format!("{name} must be a lowercase SHA-256 digest")));
            }
            fields.insert(name, v.to_string());
        }
        let bounded = |v: i64, label: &str, minimum: i64, maximum: i64| -> CaeResult<()> {
            if (minimum..=maximum).contains(&v) {
                Ok(())
            } else {
                Err(qerr(format!("{label} must be an exact integer in [{minimum},{maximum}]")))
            }
        };
        bounded(provider_registry_generation, "provider_registry_generation", 0, MAX_SAFE_JSON_INTEGER)?;
        let mut sorted = features.clone();
        sorted.sort();
        sorted.dedup();
        if features.is_empty()
            || sorted != features
            || !features.iter().all(|f| FEATURES.contains(&f.as_str()))
        {
            return Err(qerr("features must be a non-empty sorted unique closed tuple"));
        }
        let [threads, concurrency, batch_memory, residency, sparse, workspace] = resources;
        bounded(threads, "worker_thread_limit", 1, MAX_WORKER_THREADS)?;
        bounded(concurrency, "logical_batch_concurrency", 1, MAX_LOGICAL_BATCH_CONCURRENCY)?;
        for (name, v) in RESOURCE_NAMES[2..].iter().zip([batch_memory, residency, sparse, workspace]) {
            bounded(v, name, 0, MAX_SAFE_JSON_INTEGER)?;
        }
        let has = |f: &str| features.iter().any(|x| x == f);
        let nondefault = has(NONDEFAULT_EXACT_PROFILE);
        if (fields["solver_policy"] == EXACT_DIRECT_POLICY) == nondefault {
            return Err(qerr("solver policy and nondefault-profile feature disagree"));
        }
        if has(BOUNDED_WORKER_THREADS) != (threads >= 2) {
            return Err(qerr("worker-thread resources disagree with enabled features"));
        }
        if has(PARALLEL_LOGICAL_BATCH) {
            if concurrency < 2 || batch_memory < 1 {
                return Err(qerr("parallel logical-batch resources are incomplete"));
            }
        } else if concurrency != 1 || batch_memory != 0 {
            return Err(qerr("serial logical-batch facts must retain a zero memory budget"));
        }
        let resident = has(EXECUTABLE_RESIDENCY);
        if resident != (residency > 0) {
            return Err(qerr("executable-residency resources disagree with enabled features"));
        }
        let sparse_reuse = has(SPARSE_STRUCTURE_REUSE);
        if sparse_reuse != (sparse > 0) {
            return Err(qerr("sparse-structure resources disagree with enabled features"));
        }
        if sparse_reuse && (!resident || sparse > residency) {
            return Err(qerr("sparse-structure reuse requires a residency sub-budget"));
        }
        if nondefault != (workspace > 0) {
            return Err(qerr("nondefault solver workspace disagrees with enabled features"));
        }
        Ok(Self { text: fields, provider_registry_generation, features, resources })
    }

    fn resources_value(&self) -> Map<String, Value> {
        RESOURCE_NAMES.iter().zip(self.resources).map(|(k, v)| ((*k).to_string(), Value::from(v))).collect()
    }

    #[must_use]
    pub fn to_canonical(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(QUALIFICATION_FACTS_SCHEMA.into()));
        for name in ["provider_id", "provider_descriptor_sha256"] {
            m.insert(name.into(), Value::String(self.text[name].clone()));
        }
        m.insert("provider_registry_generation".into(), Value::from(self.provider_registry_generation));
        for name in [
            "provider_registry_fingerprint",
            "provider_load_order_sha256",
            "solver_policy",
            "exact_profile_sha256",
            "policy_source_sha256",
            "compile_sha256",
            "runtime_profile_sha256",
            "preexecution_mechanism_evidence_sha256",
            "operation_class",
            "problem_size_class",
        ] {
            m.insert(name.into(), Value::String(self.text[name].clone()));
        }
        m.insert("features".into(), json!(self.features));
        m.insert("resources".into(), Value::Object(self.resources_value()));
        Value::Object(m)
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        canonical_sha256(&self.to_canonical())
    }

    #[must_use]
    pub fn solver_policy(&self) -> &str {
        &self.text["solver_policy"]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactAccelerationQualificationSelection {
    brand: u64,
    facts_sha256: String,
    allowlist_sha256: String,
    admitted: bool,
    solver_policy: String,
    features: Vec<String>,
    resources: Map<String, Value>,
    reason: String,
    terminal_evidence_sha256: Option<String>,
    sha256: String,
}

impl ExactAccelerationQualificationSelection {
    fn create(
        brand: u64,
        facts: &ExactAccelerationQualificationFacts,
        admitted: bool,
        allowlist_sha256: &str,
        terminal: Option<String>,
    ) -> Self {
        let (solver_policy, features, resources, reason) = if admitted {
            (
                facts.solver_policy().to_string(),
                facts.features.clone(),
                facts.resources_value(),
                "canonical_qualification_match",
            )
        } else {
            let resources: Map<String, Value> = RESOURCE_NAMES
                .iter()
                .zip([1, 1, 0, 0, 0, 0])
                .map(|(k, v)| ((*k).to_string(), Value::from(v)))
                .collect();
            (EXACT_DIRECT_POLICY.to_string(), Vec::new(), resources, "no_canonical_qualification_match")
        };
        let row = json!({
            "schema": QUALIFICATION_SELECTION_SCHEMA,
            "facts_sha256": facts.sha256(),
            "allowlist_sha256": allowlist_sha256,
            "admitted": admitted,
            "solver_policy": solver_policy,
            "features": features,
            "resources": resources,
            "reason": reason,
            "terminal_qualification_evidence_sha256": terminal,
        });
        Self {
            brand,
            facts_sha256: facts.sha256(),
            allowlist_sha256: allowlist_sha256.to_string(),
            admitted,
            solver_policy,
            features,
            resources,
            reason: reason.to_string(),
            terminal_evidence_sha256: terminal,
            sha256: canonical_sha256(&row),
        }
    }

    #[must_use]
    pub fn admitted(&self) -> bool {
        self.admitted
    }

    #[must_use]
    pub fn solver_policy(&self) -> &str {
        &self.solver_policy
    }

    #[must_use]
    pub fn features(&self) -> &[String] {
        &self.features
    }

    #[must_use]
    pub fn resources(&self) -> &Map<String, Value> {
        &self.resources
    }

    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    #[must_use]
    pub fn terminal_qualification_evidence_sha256(&self) -> Option<&str> {
        self.terminal_evidence_sha256.as_deref()
    }

    #[must_use]
    pub fn to_provenance(&self) -> Value {
        json!({
            "schema": QUALIFICATION_SELECTION_SCHEMA,
            "facts_sha256": self.facts_sha256,
            "allowlist_sha256": self.allowlist_sha256,
            "admitted": self.admitted,
            "solver_policy": self.solver_policy,
            "features": self.features,
            "resources": self.resources,
            "reason": self.reason,
            "terminal_qualification_evidence_sha256": self.terminal_evidence_sha256,
            "selection_sha256": self.sha256,
        })
    }
}

static GATE_BRANDS: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct ExactAccelerationQualificationGate {
    brand: u64,
    admissions: BTreeMap<String, String>,
    allowlist_sha256: String,
}

impl ExactAccelerationQualificationGate {
    fn create(expected_registry_sha256: Option<&str>) -> CaeResult<Self> {
        if let Some(expected) = expected_registry_sha256 {
            if !is_digest(expected) {
                return Err(qerr("verified promotion registry identity must be a lowercase SHA-256 digest"));
            }
            if expected != promotion_registry_sha256()? {
                return Err(qerr(
                    "verified promotion registry bytes differ from the loaded literal allowlist",
                ));
            }
        }
        let pairs = canonical_promotions()?;
        let mut admissions = BTreeMap::new();
        for (index, (facts, evidence)) in pairs.iter().enumerate() {
            if !is_digest(facts) {
                return Err(qerr(format!(
                    "canonical qualification facts entry {index} must be a lowercase SHA-256 digest"
                )));
            }
            if !is_digest(evidence) {
                return Err(qerr(format!(
                    "canonical terminal qualification evidence entry {index} must be a lowercase SHA-256 digest"
                )));
            }
            if admissions.insert(facts.clone(), evidence.clone()).is_some() {
                return Err(qerr("canonical qualification facts identity is duplicated"));
            }
        }
        let allowlist_sha256 = canonical_sha256(&json!({
            "schema": CANONICAL_ALLOWLIST_SCHEMA,
            "admissions": pairs.iter().map(|(f, e)| json!({
                "facts_sha256": f,
                "terminal_qualification_evidence_sha256": e,
            })).collect::<Vec<_>>(),
        }));
        Ok(Self { brand: GATE_BRANDS.fetch_add(1, Ordering::Relaxed), admissions, allowlist_sha256 })
    }

    #[must_use]
    pub fn allowlist_sha256(&self) -> &str {
        &self.allowlist_sha256
    }


    pub fn select(
        &self,
        facts: &ExactAccelerationQualificationFacts,
        administratively_required: bool,
    ) -> CaeResult<ExactAccelerationQualificationSelection> {
        let terminal = self.admissions.get(&facts.sha256()).cloned();
        let admitted = terminal.is_some();
        if administratively_required && !admitted {
            return Err(qerr("required exact acceleration has no canonical qualification match"));
        }
        Ok(ExactAccelerationQualificationSelection::create(
            self.brand,
            facts,
            admitted,
            &self.allowlist_sha256,
            terminal,
        ))
    }


    pub fn require<'s>(
        &self,
        selection: &'s ExactAccelerationQualificationSelection,
        facts: &ExactAccelerationQualificationFacts,
        acceleration: bool,
    ) -> CaeResult<&'s ExactAccelerationQualificationSelection> {
        if selection.brand != self.brand || selection.facts_sha256 != facts.sha256() {
            return Err(qerr("qualification selection is foreign or bound to different facts"));
        }
        if acceleration && !selection.admitted {
            return Err(qerr("exact acceleration was not admitted by the canonical gate"));
        }
        Ok(selection)
    }
}


pub fn create_canonical_exact_acceleration_gate() -> CaeResult<ExactAccelerationQualificationGate> {
    ExactAccelerationQualificationGate::create(None)
}


pub fn create_manifest_bound_exact_acceleration_gate(
    expected_registry_sha256: &str,
    expected_registry_entries: &[(String, String)],
) -> CaeResult<ExactAccelerationQualificationGate> {
    if expected_registry_entries != canonical_promotions()?.as_slice() {
        return Err(qerr("verified promotion registry entries differ from the loaded literal allowlist"));
    }
    ExactAccelerationQualificationGate::create(Some(expected_registry_sha256))
}

