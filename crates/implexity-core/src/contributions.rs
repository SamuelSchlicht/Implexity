// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::json::{DumpOptions, sha256_of};
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;
use crate::sync::{ReLock, lock};

pub const KINDS: [(&str, &str); 6] = [
    ("objective_terms", "named objective terms (implexity.objective_terms.TermSpec)"),
    ("physics_backends", "named physics backends (implexity.backends.PhysicsBackend)"),
    (
        "implicit_physics",
        "Optimize-node physics bindings (implexity.implicit.physics_binding.ImplicitPhysics)",
    ),
    ("http_routes", "HTTP route tables (implexity.routes.Registry)"),
    ("body_recipes", "body-export design-recipe writers (callable(design, case_doc, path) -> dict)"),
    ("model_examples", "example model documents (implexity.implicit.examples.Example)"),
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ContributionError(pub String);

#[derive(Clone)]
pub struct ContributionValue {
    pub value: Arc<dyn Any + Send + Sync>,
    pub implementation: String,
    pub identity: usize,
}

impl ContributionValue {
    pub fn new<T: Any + Send + Sync>(value: Arc<T>, implementation: impl Into<String>) -> Self {
        let identity = Arc::as_ptr(&value).cast::<()>() as usize;
        Self { value, implementation: implementation.into(), identity }
    }

    pub fn with_identity<T: Any + Send + Sync>(
        value: Arc<T>,
        identity: usize,
        implementation: impl Into<String>,
    ) -> Self {
        Self { value, implementation: implementation.into(), identity }
    }

    #[must_use]
    pub fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        Arc::clone(&self.value).downcast::<T>().ok()
    }

    #[must_use]
    pub fn ptr(&self) -> usize {
        self.identity
    }
}

impl std::fmt::Debug for ContributionValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{} at {:#x}>", self.implementation, self.ptr())
    }
}

#[derive(Debug, Clone)]
pub struct Contribution {
    pub kind: String,
    pub key: String,
    pub owner_id: String,
    pub value: ContributionValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContributionToken {
    pub generation: u64,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct ContributionSnapshot {
    pub entries: Vec<((String, String), Arc<Contribution>)>,
    pub token: ContributionToken,
}

#[derive(Default, Clone)]
struct Data {

    entries: BTreeMap<String, Vec<(String, Arc<Contribution>)>>,
}

#[derive(Default)]
struct State {
    data: Data,
    generation: u64,
}

fn check_kind(kind: &str) -> Result<(), ContributionError> {
    if KINDS.iter().any(|(k, _)| *k == kind) {
        Ok(())
    } else {
        let mut kinds: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
        kinds.sort_unstable();
        Err(ContributionError(format!(
            "unknown contribution kind {}; the kernel declares {}",
            repr_str(kind),
            list_repr(&kinds)
        )))
    }
}

fn token_of(state: &State) -> ContributionToken {
    let mut kinds: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
    kinds.sort_unstable();
    let mut rows = Vec::new();
    for kind in kinds {
        let mut items: Vec<&(String, Arc<Contribution>)> =
            state.data.entries.get(kind).map(|v| v.iter().collect()).unwrap_or_default();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, row) in items {
            rows.push(json!([kind, key, row.owner_id, row.value.implementation, row.value.ptr()]));
        }
    }
    ContributionToken {
        generation: state.generation,
        fingerprint: sha256_of(&Value::Array(rows), &DumpOptions::compact()),
    }
}

#[derive(Default)]
pub struct ContributionRegistry {
    relock: ReLock,
    state: Mutex<State>,
}

impl std::fmt::Debug for ContributionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContributionRegistry").field("generation", &self.generation()).finish()
    }
}

impl ContributionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }


    pub fn register(
        &self,
        kind: &str,
        key: &str,
        value: ContributionValue,
        owner_id: &str,
    ) -> Result<ContributionValue, ContributionError> {
        check_kind(kind)?;
        if key.trim().is_empty() || key != key.trim() {
            return Err(ContributionError(format!("{kind}: contribution key must be nonempty trimmed text")));
        }
        if owner_id.trim().is_empty() {
            return Err(ContributionError(format!("{kind} {}: owner_id is required", repr_str(key))));
        }
        let owner = owner_id.trim().to_string();
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        let rows = s.data.entries.entry(kind.to_string()).or_default();
        if let Some((_, existing)) = rows.iter().find(|(k, _)| k == key) {
            if existing.value.ptr() == value.ptr() && existing.owner_id == owner {
                return Ok(value);
            }
            return Err(ContributionError(format!(
                "{kind} {} is already contributed by {}; a contribution is never silently replaced",
                repr_str(key),
                repr_str(&existing.owner_id)
            )));
        }
        rows.push((
            key.to_string(),
            Arc::new(Contribution {
                kind: kind.into(),
                key: key.into(),
                owner_id: owner,
                value: value.clone(),
            }),
        ));
        s.generation += 1;
        Ok(value)
    }


    pub fn unregister(
        &self,
        kind: &str,
        key: &str,
        expected_value_ptr: usize,
    ) -> Result<(), ContributionError> {
        check_kind(kind)?;
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        let Some(rows) = s.data.entries.get_mut(kind) else { return Ok(()) };
        let Some(index) = rows.iter().position(|(k, _)| k == key) else { return Ok(()) };
        if rows[index].1.value.ptr() != expected_value_ptr {
            return Err(ContributionError(format!(
                "{kind} {} ownership drifted; refusing removal",
                repr_str(key)
            )));
        }
        rows.remove(index);
        s.generation += 1;
        Ok(())
    }


    pub fn get(&self, kind: &str, key: &str) -> Result<Option<ContributionValue>, ContributionError> {
        check_kind(kind)?;
        let _g = self.relock.lock();
        Ok(lock(&self.state)
            .data
            .entries
            .get(kind)
            .and_then(|rows| rows.iter().find(|(k, _)| k == key))
            .map(|(_, r)| r.value.clone()))
    }


    pub fn require(&self, kind: &str, key: &str) -> Result<ContributionValue, ContributionError> {
        check_kind(kind)?;
        let _g = self.relock.lock();
        let s = lock(&self.state);
        let rows = s.data.entries.get(kind);
        if let Some((_, r)) = rows.and_then(|rows| rows.iter().find(|(k, _)| k == key)) {
            return Ok(r.value.clone());
        }
        let mut available: Vec<&String> =
            rows.map(|r| r.iter().map(|(k, _)| k).collect()).unwrap_or_default();
        available.sort();
        let listed = if available.is_empty() { "none".to_string() } else { list_repr(&available) };
        Err(ContributionError(format!(
            "no loaded package contributes {kind} {}; available: {listed}. Load the package that provides it.",
            repr_str(key)
        )))
    }


    pub fn entries(&self, kind: &str) -> Result<Vec<(String, ContributionValue)>, ContributionError> {
        check_kind(kind)?;
        let _g = self.relock.lock();
        Ok(lock(&self.state)
            .data
            .entries
            .get(kind)
            .map(|rows| rows.iter().map(|(k, r)| (k.clone(), r.value.clone())).collect())
            .unwrap_or_default())
    }


    pub fn rows(&self, kind: &str) -> Result<Vec<Arc<Contribution>>, ContributionError> {
        check_kind(kind)?;
        let _g = self.relock.lock();
        Ok(lock(&self.state)
            .data
            .entries
            .get(kind)
            .map(|rows| rows.iter().map(|(_, r)| Arc::clone(r)).collect())
            .unwrap_or_default())
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        let _g = self.relock.lock();
        lock(&self.state).generation
    }

    #[must_use]
    pub fn binding_token(&self) -> ContributionToken {
        let _g = self.relock.lock();
        token_of(&lock(&self.state))
    }

    #[must_use]
    pub fn snapshot(&self) -> ContributionSnapshot {
        let _g = self.relock.lock();
        let s = lock(&self.state);
        let mut entries = Vec::new();
        for (kind, _) in KINDS {
            for (key, row) in s.data.entries.get(kind).map(Vec::as_slice).unwrap_or_default() {
                entries.push(((kind.to_string(), key.clone()), Arc::clone(row)));
            }
        }
        ContributionSnapshot { entries, token: token_of(&s) }
    }


    pub fn restore(&self, state: &ContributionSnapshot) -> Result<(), ContributionError> {
        let mut data = Data::default();
        for ((kind, key), row) in &state.entries {
            check_kind(kind)?;
            data.entries.entry(kind.clone()).or_default().push((key.clone(), Arc::clone(row)));
        }
        let _g = self.relock.lock();
        let mut s = lock(&self.state);
        s.data = data;
        s.generation += 1;
        Ok(())
    }


    pub fn transaction<T, E>(&self, body: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let _g = self.relock.lock();
        let before = lock(&self.state).data.clone();
        let result = body();
        if result.is_err() {
            let mut s = lock(&self.state);
            s.data = before;
            s.generation += 1;
        }
        result
    }

    pub fn hold(&self) -> crate::sync::ReLockGuard<'_> {
        self.relock.lock()
    }
}

#[must_use]
pub fn kinds_value() -> Value {
    let mut m = serde_json::Map::new();
    for (k, d) in KINDS {
        m.insert(k.into(), json!(d));
    }
    Value::Object(m)
}

