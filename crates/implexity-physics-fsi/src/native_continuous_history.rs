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

use crate::{
    moving_contact::{
        closed_surface_history::{ClosedSurfaceHistory, execute_pair_history_observed},
        closed_surface_problem::admit_pair_loose_coupling,
        sample_projection::from_observables,
    },
    native_continuous_problem::{NativeContinuousProblem, NativeContinuousProblemDocument},
};
use implexity_core::{CaeError, CaeResult};
use implexity_solve::{
    multirate_coupling::{FluxDrivenField, SubcycledField},
    state_store::StoreBudget,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA: &str = "implexity-native-continuous-fsi-history/1";
const DESIGN_KINDS: [&str; 4] = [
    "design_volume_fraction",
    "removed_volume_fraction",
    "removed_volume_m3",
    "removal_depth_m",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeContinuousHistoryDocument {
    pub schema: String,
    pub problem: NativeContinuousProblemDocument,
    pub observables: Value,
    pub responses: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry_outputs: Option<[String; 2]>,
    #[serde(default)]
    pub include_final_state: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_checkpoints: Option<NativeHistoryStateCheckpoints>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryStateCheckpoints {
    pub interval: usize,
    pub period: usize,
    pub disk_budget_bytes: u64,
}

pub struct NativeContinuousHistoryProblem {
    pub problem: NativeContinuousProblem,
    pub document: NativeContinuousHistoryDocument,
}

impl NativeContinuousHistoryDocument {
    pub fn from_json(bytes: &[u8]) -> CaeResult<Self> {
        serde_json::from_slice(bytes).map_err(|e| CaeError::contract(e.to_string()))
    }
    pub fn build(self) -> CaeResult<NativeContinuousHistoryProblem> {
        if self.schema != SCHEMA {
            return Err(CaeError::contract("native continuous history schema"));
        }
        if let Some(refs) = &self.geometry_outputs {
            if refs[0] == refs[1] || refs.iter().any(|v| v.is_empty() || v.trim() != v) {
                return Err(CaeError::contract(
                    "native continuous geometry output references",
                ));
            }
        }
        if self
            .state_checkpoints
            .as_ref()
            .is_some_and(|c| c.interval == 0 || c.period == 0 || c.disk_budget_bytes == 0)
        {
            return Err(CaeError::contract(
                "native history checkpoint intervals and disk budget must be positive",
            ));
        }
        let problem = self.problem.clone().build()?;
        let autonomous = problem.model.fixed_horizon_autonomous()?;
        let a = &problem.model.native_body(0)?.problem;
        let b = &problem.model.native_body(1)?.problem;
        if a.time.history_steps() != b.time.history_steps()
            || a.normal_form()["time"] != b.normal_form()["time"]
        {
            return Err(CaeError::contract(
                "native continuous history clocks or checkpoint policies differ",
            ));
        }
        let solid = problem.model.solid_field()?;
        let fluid = problem.model.fluid_field()?;
        let mut names = solid.sample_names().to_vec();
        names.extend(fluid.sample_names().iter().cloned());
        let projection = from_observables(
            &self.observables,
            [
                &self.problem.bodies[0]["observables"],
                &self.problem.bodies[1]["observables"],
            ],
            names,
        )?;
        let program = projection.bind_program(
            &self.responses,
            &DESIGN_KINDS,
            a.time.history_steps(),
            false,
            autonomous,
        )?;
        let count =
            problem.model.native_body(0)?.chain.len() + problem.model.native_body(1)?.chain.len();
        let _ = problem
            .model
            .event_design_terms(&vec![0.; count], &program)?;
        Ok(NativeContinuousHistoryProblem {
            problem,
            document: self,
        })
    }
}

impl NativeContinuousHistoryProblem {
    pub fn execute(
        &self,
        controls: [&[f64]; 2],
        budget: &StoreBudget,
        gradients: bool,
    ) -> CaeResult<ClosedSurfaceHistory> {
        self.execute_observed(controls, budget, gradients, None)
    }
    pub fn execute_observed(
        &self,
        controls: [&[f64]; 2],
        budget: &StoreBudget,
        gradients: bool,
        observer: Option<&mut dyn implexity_solve::checkpointed_history::HistoryObserver>,
    ) -> CaeResult<ClosedSurfaceHistory> {
        let design = self.problem.physical_design(controls)?;
        let stepper = self.problem.stepper(&design)?;
        execute_pair_history_observed(
            &self.problem.model,
            &stepper,
            controls,
            &design,
            &self.document.observables,
            [
                &self.document.problem.bodies[0]["observables"],
                &self.document.problem.bodies[1]["observables"],
            ],
            &self.document.responses,
            budget,
            gradients,
            || admit_pair_loose_coupling(&self.problem.model, &stepper, &design),
            self.problem.identity.clone(),
            observer,
        )
    }
}

pub fn editor_schema() -> Value {
    use serde_json::json;
    let mut schema = crate::native_continuous_document::object(json!({
        "schema":{"const":SCHEMA},
        "problem":crate::native_continuous_problem::editor_schema(),
        "observables":{"type":"array"},
        "responses":{"type":"object"},
        "geometry_outputs":{"type":"array","minItems":2,"maxItems":2,"uniqueItems":true,"items":{"type":"string","minLength":1}},
        "include_final_state":{"title":"Include final state","type":"boolean","default":false},
        "state_checkpoints":crate::native_continuous_document::object(json!({
            "interval":{"title":"Checkpoint interval","type":"integer","minimum":1},
            "period":{"title":"Period length","type":"integer","minimum":1},
            "disk_budget_bytes":{"title":"Checkpoint disk budget / bytes","type":"integer","minimum":1}
        }))
    }));
    schema["required"].as_array_mut().unwrap().retain(|v| {
        v != "geometry_outputs" && v != "include_final_state" && v != "state_checkpoints"
    });
    schema["title"] = json!("Dry contact flow history");
    schema
}
