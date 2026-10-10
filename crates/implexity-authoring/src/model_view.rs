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


use implexity_geometry::document::Model;
use implexity_mesh::MeshError;
use implexity_mesh::model_view::{
    Evaluated, LiveGuard, ModelStatus, ModelView, RegisteredGrid, SamplingHint,
};
use serde_json::Value;

use crate::error::AuthoringError;
use crate::model_manager::{DetachedModelView, ModelManager, evaluate_node, max_eval_points, over_budget};
use crate::py::py_str;

fn mesh(e: &AuthoringError) -> MeshError {
    MeshError::invalid(e.to_string())
}

fn render(e: &implexity_render::RenderError) -> MeshError {
    MeshError::invalid(e.to_string())
}

fn status_of(m: &Model) -> ModelStatus {
    let aabb = m.root().map(|r| crate::model_manager::extent_of(&r));
    ModelStatus {
        content_id: m.content_id().unwrap_or_else(|| "None".into()),
        structure_id: m.structure_id().unwrap_or_else(|| "None".into()),
        aabb,
    }
}

fn evaluate_exact_of(m: &Model, points: &[[f64; 3]]) -> Result<Evaluated, MeshError> {
    if points.len() > max_eval_points() {
        return Err(mesh(&over_budget(points.len())));
    }
    let root_name = m.doc.get("root").map(py_str).unwrap_or_default();
    let node = m.node(&root_name).map_err(|e| mesh(&AuthoringError::from(e)))?;
    let (values, used, _detail) =
        evaluate_node(&node, points, "exact", "auto", None).map_err(|e| mesh(&e))?;
    Ok(Evaluated {
        values,
        content_id: node.content_id(),
        evaluator: used.into(),
        mode: "exact".into(),
        units: "mm".into(),
    })
}

fn sampling_hint_of(m: &Model) -> Result<Option<SamplingHint>, MeshError> {
    match m.root() {
        Some(root) => implexity_render::model_fields::geometry_sampling_hint(&root).map_err(|e| render(&e)),
        None => Ok(None),
    }
}

impl ModelView for ModelManager {
    fn status(&self) -> Result<ModelStatus, MeshError> {
        Ok(status_of(&*self.require().map_err(|e| mesh(&e))?))
    }

    fn live_lock(&self) -> LiveGuard<'_> {
        Box::new(ModelManager::live_lock(self))
    }

    fn evaluate_exact(&self, points: &[[f64; 3]]) -> Result<Evaluated, MeshError> {
        evaluate_exact_of(&*self.require().map_err(|e| mesh(&e))?, points)
    }

    fn geometry_sampling_hint(&self) -> Result<Option<SamplingHint>, MeshError> {
        sampling_hint_of(&*self.require().map_err(|e| mesh(&e))?)
    }

    fn registered_fields(&self) -> Result<Vec<Value>, MeshError> {
        implexity_render::model_fields::registered_fields(&*self.require().map_err(|e| mesh(&e))?)
            .map_err(|e| render(&e))
    }

    fn resolve_registered_field(&self, field: &str) -> Result<RegisteredGrid, MeshError> {
        implexity_render::model_fields::resolve_registered_field(
            &*self.require().map_err(|e| mesh(&e))?,
            field,
        )
        .map_err(|e| render(&e))
    }
}

impl ModelView for DetachedModelView {
    fn status(&self) -> Result<ModelStatus, MeshError> {
        Ok(status_of(&self.require()))
    }

    fn live_lock(&self) -> LiveGuard<'_> {
        Box::new(DetachedModelView::live_lock(self))
    }

    fn evaluate_exact(&self, points: &[[f64; 3]]) -> Result<Evaluated, MeshError> {
        evaluate_exact_of(&self.require(), points)
    }

    fn geometry_sampling_hint(&self) -> Result<Option<SamplingHint>, MeshError> {
        sampling_hint_of(&self.require())
    }

    fn registered_fields(&self) -> Result<Vec<Value>, MeshError> {
        implexity_render::model_fields::registered_fields(&self.require()).map_err(|e| render(&e))
    }

    fn resolve_registered_field(&self, field: &str) -> Result<RegisteredGrid, MeshError> {
        implexity_render::model_fields::resolve_registered_field(&self.require(), field)
            .map_err(|e| render(&e))
    }
}
