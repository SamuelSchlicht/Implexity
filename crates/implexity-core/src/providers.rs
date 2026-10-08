// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
