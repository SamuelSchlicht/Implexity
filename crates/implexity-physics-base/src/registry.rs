// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use implexity_core::py_repr::repr_str;

use crate::contracts::AddinContract;
use crate::model_errors::{PhysicsError, PhysicsResult};

#[derive(Default)]
struct Inner {
    items: BTreeMap<String, AddinContract>,
    loaded: bool,
}

#[derive(Default)]
pub struct PhysicsAddinRegistry {
    inner: Mutex<Inner>,
}

impl PhysicsAddinRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }


    pub fn register(&self, contract: AddinContract, replace: bool) -> PhysicsResult<()> {
        let issues: Vec<String> =
            contract.validate().into_iter().filter(|i| i.blocking).map(|i| i.message).collect();
        if !issues.is_empty() {
            return Err(PhysicsError::value(issues.join("; ")));
        }
        let mut inner = self.lock();
        if let Some(existing) = inner.items.get(&contract.addin_id)
            && !replace
        {
            if *existing == contract {
                return Ok(());
            }
            return Err(PhysicsError::Key(repr_str(&format!(
                "Add-in {} is already registered.",
                repr_str(&contract.addin_id)
            ))));
        }
        inner.items.insert(contract.addin_id.clone(), contract);
        Ok(())
    }


    pub fn get(&self, addin_id: &str) -> PhysicsResult<AddinContract> {
        self.lock().items.get(addin_id).cloned().ok_or_else(|| PhysicsError::Key(repr_str(addin_id)))
    }

    #[must_use]
    pub fn list(&self) -> Vec<AddinContract> {
        self.lock().items.values().cloned().collect()
    }

    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.items.clear();
        inner.loaded = false;
    }

    #[must_use]
    pub fn by_output(&self, quantity: &str) -> Vec<AddinContract> {
        self.list()
            .into_iter()
            .filter(|c| c.produces.iter().any(|p| p.quantity == quantity || p.name == quantity))
            .collect()
    }

    #[must_use]
    pub fn loaded(&self) -> bool {
        self.lock().loaded
    }

    pub fn set_loaded(&self, loaded: bool) {
        self.lock().loaded = loaded;
    }
}

pub static REGISTRY: LazyLock<PhysicsAddinRegistry> = LazyLock::new(PhysicsAddinRegistry::new);


pub fn ensure_default_addins(
    registry: Option<&PhysicsAddinRegistry>,
) -> PhysicsResult<&PhysicsAddinRegistry> {
    let reg = registry.unwrap_or(&REGISTRY);
    if reg.loaded() {
        return Ok(reg);
    }
    crate::addins::register_all(reg)?;
    reg.set_loaded(true);
    Ok(reg)
}


pub fn list_addins() -> PhysicsResult<Vec<AddinContract>> {
    Ok(ensure_default_addins(None)?.list())
}
