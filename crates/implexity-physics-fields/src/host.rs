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
use std::sync::Arc;

use serde_json::{Map, Value};

use implexity_core::contracts::FieldValue;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::{CoupledHistoryAssembly, HistoryBlock, HistoryInterface};

pub trait FieldHost: Send + Sync {
    fn problem(&self) -> &Value;
    fn solid(&self) -> &Arc<SolidKernel>;
}

#[derive(Clone)]
pub struct HostRef(pub Arc<dyn FieldHost>);

#[derive(Clone)]
pub struct BoundSource(pub Arc<dyn BoundFieldSource>);

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct SourceEnergy {
    pub deposition_w: f64,
    pub sensible_deposition_w: f64,
    pub material_production_w: f64,
}

impl SourceEnergy {
    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("deposition_W".into(), Value::from(self.deposition_w));
        m.insert("sensible_deposition_W".into(), Value::from(self.sensible_deposition_w));
        m.insert("material_production_W".into(), Value::from(self.material_production_w));
        m
    }
}

pub type ResponseVjp = (Vec<Vec<f64>>, Vec<f64>);

pub trait BoundFieldSource: Send + Sync {
    fn response_units(&self) -> Vec<(String, String)>;
    fn state_contract(&self) -> Option<&'static str> {
        None
    }
    fn thermal_placement(&self) -> Option<&'static str> {
        None
    }
    fn blocks(&self) -> Vec<HistoryBlock>;

    fn attach(&self, assembly: &CoupledHistoryAssembly) -> CaeResult<()>;
    fn exchange(&self) -> Option<Arc<dyn HistoryInterface>>;

    fn forcing(&self, _n: usize, _z: &[f64], _x: &[f64]) -> CaeResult<Option<Vec<f64>>> {
        Ok(None)
    }

    fn forcing_jacobians(
        &self,
        _n: usize,
        _z: &[f64],
        _x: &[f64],
    ) -> CaeResult<Option<(CsrMatrix, CsrMatrix)>> {
        Ok(None)
    }

    fn energy(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<SourceEnergy>;

    fn diagnostics(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Map<String, Value>>;

    fn validate(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<()> {
        for n in 1..history.len() {
            self.diagnostics(n, &history[n], &history[n - 1], x)?;
        }
        Ok(())
    }

    fn responses(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>>;

    fn response_vjp(&self, history: &[Vec<f64>], x: &[f64], weights: &[f64]) -> CaeResult<ResponseVjp>;

    fn fields(
        &self,
        history: &[Vec<f64>],
        x: &[f64],
    ) -> CaeResult<(BTreeMap<String, FieldValue>, Map<String, Value>)>;
}


pub fn host_of(host: &dyn std::any::Any) -> CaeResult<Arc<dyn FieldHost>> {
    host.downcast_ref::<HostRef>()
        .map(|h| Arc::clone(&h.0))
        .ok_or_else(|| CaeError::contract("field source requires a native coupled history host"))
}


pub fn block_start(assembly: &CoupledHistoryAssembly, name: &str) -> CaeResult<usize> {
    assembly
        .slice(name)
        .map(|(start, _)| start)
        .ok_or_else(|| CaeError::contract(format!("coupled history assembly has no {name} block")))
}

pub const AUTHORING_INTERFACE: &str = "field_source_authoring";

pub trait FieldSourceAuthoring: Send + Sync {
    fn editor_schema(&self, settings: &Value, context: &Value) -> Value;
    fn study_templates(&self, _context: &Value) -> Vec<Value> {
        Vec::new()
    }
}
