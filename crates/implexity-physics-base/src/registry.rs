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
