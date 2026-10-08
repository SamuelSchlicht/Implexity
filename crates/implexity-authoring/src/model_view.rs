// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
