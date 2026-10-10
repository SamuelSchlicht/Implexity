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
use base64::Engine as _;
use implexity_render::mesh_scene::{self, MeshSource, SceneMesh, SceneRequest, SceneError};
use serde_json::{Map, Value, json};
use crate::{AgentManager, AgentError, AgentResult};

fn invalid(message: impl Into<String>) -> AgentError { AgentError::contract(message) }
fn parsed(p: &Map<String, Value>) -> AgentResult<SceneRequest> {
    if p.keys().any(|k| !["scene", "mesh_data", "view"].contains(&k.as_str())) { return Err(invalid("unknown mesh scene argument")); }
    let req=mesh_scene::parse_request(p.get("scene").ok_or_else(||invalid("scene is required"))?).map_err(|e| invalid(e.to_string()))?;
    let data=p.get("mesh_data").and_then(Value::as_object).ok_or_else(||invalid("mesh_data must contain inline STL entries"))?;
    if data.len()!=req.meshes.len() { return Err(invalid("mesh_data must contain exactly one inline entry per scene mesh")); }
    for (id,(token,_)) in &req.meshes {
        if token!=id { return Err(invalid("scene mesh stl values must equal their mesh ids; filesystem paths are not accepted")); }
        let d=data.get(id).and_then(Value::as_object).ok_or_else(||invalid(format!("missing inline mesh {id}")))?;
        if d.len()!=1 || !d.get("data_base64").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) { return Err(invalid("inline meshes contain only a nonempty data_base64 string")); }
    }
    if let Some(v)=p.get("view") { if !v.as_str().is_some_and(|name| req.views.iter().any(|v|v.name==name)) { return Err(invalid("view must name a declared scene view")); } }
    Ok(req)
}

pub(crate) fn validate(p: &Map<String, Value>) -> AgentResult<()> { parsed(p).map(|_|()) }

struct InlineMeshes(BTreeMap<String, Arc<SceneMesh>>);
impl MeshSource for InlineMeshes {
    fn mesh(&mut self,id:&str)->Result<Arc<SceneMesh>,SceneError> {
        self.0.get(id).cloned().ok_or_else(||SceneError::Invalid(format!("unknown inline mesh {id}")))
    }
}

impl AgentManager {
    pub(crate) fn render_mesh_scene(&self,p:&Map<String,Value>)->AgentResult<Value> {
        let req=parsed(p)?;
        let view=match p.get("view").and_then(Value::as_str) { Some(name)=>req.views.iter().find(|v|v.name==name).unwrap(),None=>&req.views[0] };
        let cut=!view.region.keep.is_empty() || !view.region.remove.is_empty();
        let mut meshes=BTreeMap::new();let mut identities=Vec::new();
        for layer in &view.layers {
            let id=&layer.mesh;
            if meshes.contains_key(id) { continue; }
            let encoded=p["mesh_data"][id]["data_base64"].as_str().unwrap();
            let data=base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|e|invalid(format!("mesh {id} has invalid base64: {e}")))?;
            let (triangles,_)=implexity_mesh::formats::read_stl_bytes(&data).map_err(|e|invalid(format!("mesh {id} is not a valid binary STL: {e}")))?;
            if triangles.is_empty() || triangles.iter().flatten().flatten().any(|v| !v.is_finite()) { return Err(invalid(format!("mesh {id} must contain finite triangles"))); }
            let mesh=mesh_scene::weld(&triangles,req.meshes[id].1);
            if mesh.is_empty() { return Err(invalid(format!("mesh {id} has no nondegenerate triangles"))); }
            let faces:Vec<[usize;3]>=mesh.triangles.iter().map(|t|t.map(|v|v as usize)).collect();
            let vertices:Vec<[f64;3]>=mesh.positions.iter().map(|p|p.map(f64::from)).collect();
            let topology=implexity_mesh::topology::topology_fast(&vertices,&faces);
            let orientation=implexity_mesh::topology::orientation_report(&faces);
            if cut && (topology.boundary_edges!=0 || topology.nonmanifold_edges!=0 || !orientation.consistent) { return Err(invalid(format!("mesh {id} must be closed, manifold and consistently oriented to render filled cuts"))); }
            identities.push(json!({"id":id,"bytes":data.len(),"sha256":implexity_io::digest::sha256_hex(&data),"triangles":triangles.len(),"topology":topology.to_json(),"orientation":orientation.to_json()}));
            meshes.insert(id.clone(),Arc::new(mesh));
        }
        let rendered=mesh_scene::render_view(&req,view,&mut InlineMeshes(meshes)).map_err(|e|AgentError::refused(e.to_string()))?;
        let png=rendered.png();
        Ok(json!({"schema":"implexity-mesh-scene-render/1","source":{"kind":"inline_binary_stl","meshes":identities,"live_model_mutated":false},"view":view.name,"report":rendered.report,
            "image":{"schema":"implexity-inline-file/1","filename":format!("{}.png",view.name),"mime_type":"image/png","width_px":rendered.width,"height_px":rendered.height,"bytes":png.len(),"sha256":implexity_io::digest::sha256_hex(&png),"data_base64":base64::engine::general_purpose::STANDARD.encode(&png)}}))
    }
}
