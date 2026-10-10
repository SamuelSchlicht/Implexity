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



use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::contracts::{CaeProvider, provider_key, provider_ptr};
use crate::error::{CaeError, CaeResult};
use crate::json::{DumpOptions, sha256_of};
use crate::orchestration::{AddInRegistry, register_provider_bundle};
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;
use crate::sync::{ReLock, lock};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderRegistryToken {
    pub generation: u64,
    pub fingerprint: String,
}

#[derive(Clone)]
pub struct ProviderRegistrySnapshot {
    pub entries: Vec<(String, Arc<dyn CaeProvider>)>,
    pub token: ProviderRegistryToken,
}

impl std::fmt::Debug for ProviderRegistrySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistrySnapshot")
            .field("names", &self.entries.iter().map(|(n, _)| n).collect::<Vec<_>>())
            .field("token", &self.token)
            .finish()
    }
}

#[derive(Default)]
struct Data {
    providers: BTreeMap<String, Arc<dyn CaeProvider>>,
    generation: u64,
}

fn token_of(data: &Data) -> ProviderRegistryToken {
    let rows: Vec<Value> =
        data.providers.iter().map(|(name, p)| json!([name, p.implementation(), provider_ptr(p)])).collect();
    ProviderRegistryToken {
        generation: data.generation,
        fingerprint: sha256_of(&Value::Array(rows), &DumpOptions::compact()),
    }
}

pub type CatalogueTraitHook = dyn Fn(
        &str,
        &crate::contracts::ProviderCapabilities,
        &Arc<dyn CaeProvider>,
        &mut Map<String, Value>,
    ) -> CaeResult<()>
    + Send
    + Sync;

#[derive(Default)]
pub struct ProviderRegistry {
    relock: ReLock,
    data: Mutex<Data>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry").field("generation", &self.generation()).finish()
    }
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn binding_token(&self) -> ProviderRegistryToken {
        let _g = self.relock.lock();
        token_of(&lock(&self.data))
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        let _g = self.relock.lock();
        lock(&self.data).generation
    }

    #[must_use]
    pub fn snapshot(&self) -> ProviderRegistrySnapshot {
        let _g = self.relock.lock();
        let data = lock(&self.data);
        ProviderRegistrySnapshot {
            entries: data.providers.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect(),
            token: token_of(&data),
        }
    }


    pub fn register(
        &self,
        addins: &AddInRegistry,
        provider: Arc<dyn CaeProvider>,
    ) -> CaeResult<Arc<dyn CaeProvider>> {
        let name = provider_key(provider.as_ref());
        if name.is_empty() {
            return Err(CaeError::contract("CAE provider requires a non-empty string name"));
        }
        let marker = provider.orchestration_meta();
        let _g = self.relock.lock();
        let existing = lock(&self.data).providers.get(&name).cloned();
        if let Some(existing) = existing {
            if Arc::ptr_eq(&existing, &provider) {
                if marker {
                    if addins.get(&name).is_ok() {
                        return Err(CaeError::contract(format!(
                            "provider/add-in identity collision for {}",
                            repr_str(&name)
                        )));
                    }
                } else {
                    register_provider_bundle(&provider, addins, None)?;
                }
                return Ok(provider);
            }
            return Err(CaeError::contract(format!("CAE provider {} identity collision", repr_str(&name))));
        }
        let outcome: CaeResult<()> = if marker {
            if addins.get(&name).is_ok() {
                Err(CaeError::contract(format!("provider/add-in identity collision for {}", repr_str(&name))))
            } else {
                lock(&self.data).providers.insert(name.clone(), Arc::clone(&provider));
                Ok(())
            }
        } else {
            addins.transaction(|| {
                register_provider_bundle(&provider, addins, None)?;
                lock(&self.data).providers.insert(name.clone(), Arc::clone(&provider));
                Ok(())
            })
        };
        match outcome {
            Ok(()) => {
                lock(&self.data).generation += 1;
                Ok(provider)
            }
            Err(e) => {
                lock(&self.data).providers.remove(&name);
                Err(e)
            }
        }
    }


    pub fn sync_orchestration_bundles(&self, addins: &AddInRegistry) -> CaeResult<()> {
        let _g = self.relock.lock();
        let providers: Vec<Arc<dyn CaeProvider>> = lock(&self.data).providers.values().cloned().collect();
        for provider in providers {
            if !provider.orchestration_meta() {
                register_provider_bundle(&provider, addins, None).map_err(|e| {
                    CaeError::contract(format!(
                        "unable to synchronize physics add-in catalogue: {}",
                        e.message()
                    ))
                })?;
            }
        }
        Ok(())
    }


    pub fn unregister(
        &self,
        addins: &AddInRegistry,
        name: &str,
        expected_provider: Option<&Arc<dyn CaeProvider>>,
    ) -> CaeResult<Arc<dyn CaeProvider>> {
        let _g = self.relock.lock();
        let Some(provider) = lock(&self.data).providers.get(name).cloned() else {
            return Err(CaeError::contract(format!("unknown CAE provider {}", repr_str(name))));
        };
        if let Some(expected) = expected_provider
            && !Arc::ptr_eq(expected, &provider)
        {
            return Err(CaeError::contract(format!("CAE provider {} owner mismatch", repr_str(name))));
        }
        for (id, dependent) in &lock(&self.data).providers {
            if id != name && dependent.provider_dependencies().iter().any(|dependency| dependency == name) {
                return Err(CaeError::contract(format!("Provider {name} is used by {id}; remove the dependent provider first")));
            }
        }
        addins.transaction(|| {
            if let Ok(entry) = addins.get(name) {
                let wrapped = entry.adapter.as_ref().and_then(|a| a.provider());
                if !wrapped.as_ref().is_some_and(|p| Arc::ptr_eq(p, &provider)) {
                    return Err(CaeError::contract(format!(
                        "provider/add-in identity collision for {}",
                        repr_str(name)
                    )));
                }
                addins.unregister(name, Some(provider_ptr(&provider)))?;
            }
            let mut data = lock(&self.data);
            data.providers.remove(name);
            data.generation += 1;
            Ok(())
        })?;
        Ok(provider)
    }

    pub fn restore(&self, state: &ProviderRegistrySnapshot) {
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        data.providers = state.entries.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect();
        data.generation += 1;
    }


    pub fn transaction<T, E>(
        &self,
        addins: &AddInRegistry,
        body: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let _g = self.relock.lock();
        let before = lock(&self.data).providers.clone();
        addins.transaction(|| {
            let result = body();
            if result.is_err() {
                let mut data = lock(&self.data);
                data.providers.clone_from(&before);
                data.generation += 1;
            }
            result
        })
    }


    pub fn get(&self, name: &str) -> CaeResult<Arc<dyn CaeProvider>> {
        let _g = self.relock.lock();
        let data = lock(&self.data);
        data.providers.get(name).cloned().ok_or_else(|| {
            let names: Vec<&String> = data.providers.keys().collect();
            CaeError::contract(format!(
                "unknown CAE provider {}; registered: {}",
                repr_str(name),
                list_repr(&names)
            ))
        })
    }


    pub fn names(&self, addins: &AddInRegistry) -> CaeResult<Vec<String>> {
        self.sync_orchestration_bundles(addins)?;
        let _g = self.relock.lock();
        Ok(lock(&self.data).providers.keys().cloned().collect())
    }


    pub fn catalogue(
        &self,
        addins: &AddInRegistry,
        hook: Option<&CatalogueTraitHook>,
    ) -> CaeResult<Map<String, Value>> {
        self.sync_orchestration_bundles(addins)?;
        let _g = self.relock.lock();
        let providers: Vec<(String, Arc<dyn CaeProvider>)> =
            lock(&self.data).providers.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect();
        let mut out = Map::new();
        for (name, provider) in providers {
            let caps = provider.capabilities()?;
            let mut row = caps.to_map();
            let mut traits = row.get("traits").and_then(Value::as_object).cloned().unwrap_or_default();
            if let Some(hook) = hook {
                hook(&name, &caps, &provider, &mut traits)?;
            }
            row.insert("traits".into(), Value::Object(traits));
            if !row.contains_key("name") {
                row.insert("name".into(), json!(name));
            }
            out.insert(name, Value::Object(row));
        }
        Ok(out)
    }

    pub fn hold(&self) -> crate::sync::ReLockGuard<'_> {
        self.relock.lock()
    }
}
