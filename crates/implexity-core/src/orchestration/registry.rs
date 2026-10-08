// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::contracts::{CaeProvider, provider_key};
use crate::error::{CaeError, CaeResult};
use crate::json::{DumpOptions, sha256_of};
use crate::py_repr::repr_str;
use crate::sync::{ReLock, lock};

use super::compat;
use super::types::{AddInContract, PublishedContract, STRICT_CONTRACT_VERSION};


pub trait AddInAdapter: Send + Sync + 'static {
    fn implementation(&self) -> String;

    fn provider(&self) -> Option<Arc<dyn CaeProvider>> {
        None
    }

    fn registration_identity(&self) -> Option<String> {
        None
    }

    fn has_residual_contributions(&self) -> bool {
        false
    }

    fn has_algebraic_evaluate(&self) -> bool {
        false
    }

    fn runtime_support(&self) -> Option<Map<String, Value>> {
        None
    }

    fn component_kind(&self) -> Option<String> {
        None
    }

    fn component_slots(&self) -> Option<Map<String, Value>> {
        None
    }

    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        None
    }

    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        None
    }

    fn state_contract(&self) -> Option<String> {
        None
    }

    fn interface(&self, _name: &str) -> Option<&(dyn Any + Send + Sync)> {
        None
    }

    fn as_any(&self) -> &dyn Any;
}

#[must_use]
pub fn adapter_ptr(adapter: &Arc<dyn AddInAdapter>) -> usize {
    Arc::as_ptr(adapter).cast::<()>() as usize
}

#[must_use]
pub fn numerical_owner(adapter: &Arc<dyn AddInAdapter>) -> usize {
    adapter.provider().map_or_else(|| adapter_ptr(adapter), |p| crate::contracts::provider_ptr(&p))
}

#[must_use]
pub fn stable_implementation(adapter: Option<&Arc<dyn AddInAdapter>>) -> String {
    match adapter {
        None => "declaration-only".into(),
        Some(a) => match a.provider() {
            Some(p) => p.implementation().to_string(),
            None => a.implementation(),
        },
    }
}

pub struct ProviderBundleAdapter {
    provider: Arc<dyn CaeProvider>,
    identity: String,
}

impl ProviderBundleAdapter {
    #[must_use]
    pub fn new(provider: Arc<dyn CaeProvider>, addin_id: &str) -> Self {
        Self { provider, identity: format!("provider:{addin_id}") }
    }
}

impl AddInAdapter for ProviderBundleAdapter {
    fn implementation(&self) -> String {
        "implexity.cae.orchestration.ProviderBundleAdapter".into()
    }
    fn provider(&self) -> Option<Arc<dyn CaeProvider>> {
        Some(Arc::clone(&self.provider))
    }
    fn registration_identity(&self) -> Option<String> {
        Some(self.identity.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct LegacyProviderAdapter {
    provider: Arc<dyn CaeProvider>,
    identity: String,
}

impl LegacyProviderAdapter {
    #[must_use]
    pub fn new(provider: Arc<dyn CaeProvider>) -> Self {
        let identity = format!("legacy-provider:{}", provider_key(provider.as_ref()));
        Self { provider, identity }
    }
}

impl AddInAdapter for LegacyProviderAdapter {
    fn implementation(&self) -> String {
        "implexity.cae.compat.legacy_provider_bundle.LegacyProviderAdapter".into()
    }
    fn provider(&self) -> Option<Arc<dyn CaeProvider>> {
        Some(Arc::clone(&self.provider))
    }
    fn registration_identity(&self) -> Option<String> {
        Some(self.identity.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct RegisteredAddIn {
    pub contract: AddInContract,
    pub adapter: Option<Arc<dyn AddInAdapter>>,
    pub owner_identity: String,
    pub contract_fingerprint: String,
    pub compatibility_mode: bool,
}

impl std::fmt::Debug for RegisteredAddIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredAddIn")
            .field("addin_id", &self.contract.addin_id)
            .field("owner_identity", &self.owner_identity)
            .field("contract_fingerprint", &self.contract_fingerprint)
            .field("compatibility_mode", &self.compatibility_mode)
            .field("adapter", &self.adapter.as_ref().map(adapter_ptr))
            .finish()
    }
}

impl RegisteredAddIn {
    #[must_use]
    pub fn same_row(&self, other: &Self) -> bool {
        self.contract == other.contract
            && self.owner_identity == other.owner_identity
            && self.contract_fingerprint == other.contract_fingerprint
            && self.compatibility_mode == other.compatibility_mode
            && same_adapter(self.adapter.as_ref(), other.adapter.as_ref())
    }
}

#[must_use]
pub fn same_adapter(a: Option<&Arc<dyn AddInAdapter>>, b: Option<&Arc<dyn AddInAdapter>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegistryBindingToken {
    pub generation: u64,
    pub fingerprint: String,
}

impl RegistryBindingToken {
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"generation": self.generation, "fingerprint": self.fingerprint})
    }
}

#[derive(Debug, Clone)]
pub struct AddInRegistrySnapshot {
    pub entries: Vec<Arc<RegisteredAddIn>>,
    pub token: RegistryBindingToken,
}

#[derive(Debug, Clone)]
pub enum ContractInput {
    Typed(Box<AddInContract>),
    Mapping(Value),
}

#[derive(Default)]
struct AddInData {
    entries: BTreeMap<String, Arc<RegisteredAddIn>>,
    generation: u64,
}

#[derive(Default)]
pub struct AddInRegistry {
    relock: ReLock,
    data: Mutex<AddInData>,
}

impl std::fmt::Debug for AddInRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddInRegistry").field("generation", &self.generation()).finish()
    }
}

fn token_of(data: &AddInData) -> RegistryBindingToken {
    let rows: Vec<Value> = data
        .entries
        .iter()
        .map(|(k, v)| json!([k, v.contract_fingerprint, v.owner_identity, v.compatibility_mode]))
        .collect();
    RegistryBindingToken {
        generation: data.generation,
        fingerprint: sha256_of(&Value::Array(rows), &DumpOptions::compact()),
    }
}


pub fn resolve_owner_identity(
    contract: &AddInContract,
    adapter: Option<&Arc<dyn AddInAdapter>>,
    explicit: Option<&str>,
) -> CaeResult<String> {
    if let Some(e) = explicit {
        if e.trim().is_empty() {
            return Err(CaeError::contract("owner identity must be non-empty text"));
        }
        return Ok(e.trim().to_string());
    }
    if !contract.owner_id.is_empty() {
        return Ok(contract.owner_id.clone());
    }
    if let Some(a) = adapter
        && let Some(marker) = a.registration_identity()
        && !marker.trim().is_empty()
    {
        return Ok(marker.trim().to_string());
    }
    match adapter {
        None => Ok(format!("contract:{}", contract.addin_id)),
        Some(a) => Ok(format!("object:{}:{}", a.implementation(), adapter_ptr(a))),
    }
}

impl AddInRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        let _g = self.relock.lock();
        lock(&self.data).generation
    }

    #[must_use]
    pub fn binding_token(&self) -> RegistryBindingToken {
        let _g = self.relock.lock();
        token_of(&lock(&self.data))
    }


    pub fn register(
        &self,
        contract: ContractInput,
        adapter: Option<Arc<dyn AddInAdapter>>,
        owner_identity: Option<&str>,
    ) -> CaeResult<AddInContract> {
        let (c, compatibility) = match contract {
            ContractInput::Typed(c) => {
                let c = *c;
                c.validate()?;
                if c.contract_version < STRICT_CONTRACT_VERSION {
                    (compat::translate_legacy_addin_contract(&c)?, true)
                } else {
                    (c, false)
                }
            }
            ContractInput::Mapping(raw) => (AddInContract::from_mapping(&raw, true)?, false),
        };
        let owner = resolve_owner_identity(&c, adapter.as_ref(), owner_identity)?;
        let fingerprint = c.fingerprint();
        let compatibility_mode = compatibility || c.compatibility_mode;
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        if let Some(existing) = data.entries.get(&c.addin_id) {
            if existing.contract == c
                && existing.owner_identity == owner
                && same_adapter(existing.adapter.as_ref(), adapter.as_ref())
            {
                return Ok(existing.contract.clone());
            }
            return Err(CaeError::contract(format!(
                "physics add-in {} identity collision: existing owner {}, incoming owner {}",
                repr_str(&c.addin_id),
                repr_str(&existing.owner_identity),
                repr_str(&owner)
            )));
        }
        let row = RegisteredAddIn {
            contract: c.clone(),
            adapter,
            owner_identity: owner,
            contract_fingerprint: fingerprint,
            compatibility_mode,
        };
        data.entries.insert(c.addin_id.clone(), Arc::new(row));
        data.generation += 1;
        Ok(c)
    }


    pub fn replace(
        &self,
        addin_id: &str,
        contract: AddInContract,
        adapter: Option<Arc<dyn AddInAdapter>>,
        expected: &RegistryBindingToken,
        owner_identity: Option<&str>,
    ) -> CaeResult<AddInContract> {
        contract.validate()?;
        let contract = if contract.contract_version < STRICT_CONTRACT_VERSION {
            compat::translate_legacy_addin_contract(&contract)?
        } else {
            contract
        };
        let owner = resolve_owner_identity(&contract, adapter.as_ref(), owner_identity)?;
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        if token_of(&data) != *expected {
            return Err(CaeError::contract("STALE add-in registry binding token"));
        }
        let Some(current) = data.entries.get(addin_id) else {
            return Err(CaeError::contract(format!("unknown physics add-in {}", repr_str(addin_id))));
        };
        let current_owner = current.adapter.as_ref().map(numerical_owner);
        let incoming_owner = adapter.as_ref().map(numerical_owner);
        if current_owner != incoming_owner || current.owner_identity != owner {
            return Err(CaeError::contract(format!(
                "physics add-in {} replacement owner mismatch",
                repr_str(addin_id)
            )));
        }
        let row = RegisteredAddIn {
            contract_fingerprint: contract.fingerprint(),
            compatibility_mode: contract.compatibility_mode,
            contract: contract.clone(),
            adapter,
            owner_identity: owner,
        };
        if !row.same_row(current) {
            data.entries.insert(addin_id.to_string(), Arc::new(row));
            data.generation += 1;
        }
        Ok(contract)
    }


    pub fn unregister(
        &self,
        addin_id: &str,
        expected_owner: Option<usize>,
    ) -> CaeResult<Arc<RegisteredAddIn>> {
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        let Some(current) = data.entries.get(addin_id) else {
            return Err(CaeError::contract(format!("unknown physics add-in {}", repr_str(addin_id))));
        };
        if let Some(expected) = expected_owner {
            let owner = current.adapter.as_ref().map(numerical_owner);
            if owner != Some(expected) {
                return Err(CaeError::contract(format!(
                    "physics add-in {} owner mismatch",
                    repr_str(addin_id)
                )));
            }
        }
        let row = data
            .entries
            .remove(addin_id)
            .ok_or_else(|| CaeError::contract("registry changed during removal"))?;
        data.generation += 1;
        Ok(row)
    }


    pub fn get(&self, addin_id: &str) -> CaeResult<Arc<RegisteredAddIn>> {
        let _g = self.relock.lock();
        lock(&self.data)
            .entries
            .get(addin_id)
            .cloned()
            .ok_or_else(|| CaeError::contract(format!("unknown physics add-in {}", repr_str(addin_id))))
    }

    #[must_use]
    pub fn snapshot(&self) -> AddInRegistrySnapshot {
        let _g = self.relock.lock();
        let data = lock(&self.data);
        AddInRegistrySnapshot { entries: data.entries.values().cloned().collect(), token: token_of(&data) }
    }

    #[must_use]
    pub fn catalogue(&self) -> Map<String, Value> {
        let _g = self.relock.lock();
        lock(&self.data).entries.iter().map(|(k, v)| (k.clone(), v.contract.to_value())).collect()
    }

    pub fn clear(&self) {
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        if !data.entries.is_empty() {
            data.entries.clear();
            data.generation += 1;
        }
    }


    pub fn restore(&self, snapshot: &AddInRegistrySnapshot) -> CaeResult<()> {
        let mut rows = BTreeMap::new();
        for row in &snapshot.entries {
            rows.insert(row.contract.addin_id.clone(), Arc::clone(row));
        }
        if rows.len() != snapshot.entries.len() {
            return Err(CaeError::contract("snapshot contains duplicate add-in ids"));
        }
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        data.entries = rows;
        data.generation += 1;
        Ok(())
    }


    pub fn transaction<T, E>(&self, body: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let _g = self.relock.lock();
        let before = lock(&self.data).entries.clone();
        let result = body();
        if result.is_err() {
            let mut data = lock(&self.data);
            data.entries = before;
            data.generation += 1;
        }
        result
    }

    pub fn hold(&self) -> crate::sync::ReLockGuard<'_> {
        self.relock.lock()
    }
}



pub fn register_provider_bundle(
    provider: &Arc<dyn CaeProvider>,
    registry: &AddInRegistry,
    compatibility: Option<bool>,
) -> CaeResult<AddInContract> {
    let Some(published) = provider.orchestration_contract() else {
        if compatibility == Some(false) {
            return Err(CaeError::contract("provider must publish a strict orchestration_contract"));
        }
        return compat::register_legacy_provider_bundle(provider, registry);
    };
    let published = published?;
    let raw_version = match &published {
        PublishedContract::Contract(c) => Some(json!(c.contract_version)),
        PublishedContract::Mapping(m) => m
            .as_object()
            .and_then(|o| o.get("contract_version").or_else(|| o.get("contractVersion")).cloned()),
    };
    if raw_version.as_ref().is_some_and(|v| crate::pyobj::py_eq(v, &json!(1))) {
        if compatibility == Some(false) {
            return Err(CaeError::contract("provider publishes only a legacy v1 orchestration contract"));
        }
        let legacy = match published {
            PublishedContract::Contract(c) => *c,
            PublishedContract::Mapping(m) => AddInContract::from_mapping(&m, false)?,
        };
        return compat::register_legacy_published_contract(provider, &legacy, registry);
    }
    if compatibility == Some(true) {
        return Err(CaeError::contract("a strict provider contract cannot request legacy translation"));
    }
    let contract = match published {
        PublishedContract::Contract(c) => {
            c.validate()?;
            *c
        }
        PublishedContract::Mapping(m) => AddInContract::from_mapping(&m, true)?,
    };
    if contract.contract_version != STRICT_CONTRACT_VERSION || contract.compatibility_mode {
        return Err(CaeError::contract(
            "provider orchestration_contract must be a strict, non-compatibility v2 contract",
        ));
    }
    let aid = provider_key(provider.as_ref());
    if aid.is_empty() || aid != contract.addin_id {
        return Err(CaeError::contract(format!(
            "provider/add-in identity mismatch: provider {}, contract {}",
            repr_str(&aid),
            repr_str(&contract.addin_id)
        )));
    }
    let owner = format!("provider:{aid}");
    let existing = registry.get(&aid).ok();
    let Some(existing) = existing else {
        let adapter: Arc<dyn AddInAdapter> = Arc::new(ProviderBundleAdapter::new(Arc::clone(provider), &aid));
        return registry.register(ContractInput::Typed(Box::new(contract)), Some(adapter), Some(&owner));
    };
    let current_provider = existing.adapter.as_ref().and_then(|a| a.provider());
    let same = current_provider.as_ref().is_some_and(|p| Arc::ptr_eq(p, provider));
    if !same || existing.owner_identity != owner {
        return Err(CaeError::contract(format!("provider/add-in identity collision for {}", repr_str(&aid))));
    }
    if existing.contract == contract {
        return Ok(existing.contract.clone());
    }
    let adapter: Arc<dyn AddInAdapter> = Arc::new(ProviderBundleAdapter::new(Arc::clone(provider), &aid));
    registry.replace(&aid, contract, Some(adapter), &registry.binding_token(), Some(&owner))
}
