// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::Arc;

use implexity_ad::Scalar;
use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::orchestration::PublishedContract;
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::array::nested_bool;
use crate::errors::PResult;
use crate::pyval::strs;
use crate::providers::quasi1d_euler::{evaluation_contract, no_design, presentation, response_metadata};

#[must_use]
pub fn boundary_editor_schema() -> Value {
    let pressure = json!({"title": "Absolute pressure", "unit": "Pa", "exclusiveMinimum": 0});
    let mut faces = Map::new();
    for axis in 0..3 {
        for (suffix, sign) in [("min", 1.0), ("max", -1.0)] {
            let mut direction = [0.0; 3];
            direction[axis] = sign;
            let mut state = [1.2, 0.0, 0.0, 0.0, 101_325.0];
            state[axis + 1] = 500.0 * sign;
            let mut pressure_total = pressure.clone();
            pressure_total["title"] = json!("Total absolute pressure");
            let options = json!([
                {"label": "Slip wall", "template": {"kind": "reflecting"}},
                {"label": "Zero-gradient open face", "template": {"kind": "transmissive"}},
                {"label": "Supersonic inlet", "template": {"kind": "supersonic_inflow", "primitive": state},
                 "properties": {"primitive": {"title": "Inlet state", "description": "Normal inflow must be supersonic.",
                    "prefixItems": [{"title": "Density", "unit": "kg/m\u{b3}", "exclusiveMinimum": 0},
                        {"title": "Velocity X", "unit": "m/s"}, {"title": "Velocity Y", "unit": "m/s"}, {"title": "Velocity Z", "unit": "m/s"}, pressure]}}},
                {"label": "Subsonic total-condition inlet", "template": {"kind": "subsonic_reservoir", "total_pressure_Pa": 105_000.0, "total_temperature_K": 300.0, "flow_direction": direction},
                 "properties": {"total_pressure_Pa": pressure_total,
                    "total_temperature_K": {"title": "Total temperature", "unit": "K", "exclusiveMinimum": 0},
                    "flow_direction": {"title": "Inward flow direction", "description": "A nonzero inward vector; normalized by the provider.",
                        "prefixItems": [{"title": "X component"}, {"title": "Y component"}, {"title": "Z component"}]}}},
                {"label": "Subsonic pressure outlet", "template": {"kind": "subsonic_pressure_outlet", "pressure_Pa": 101_325.0},
                 "properties": {"pressure_Pa": pressure}}
            ]);
            let axis_name = ["X", "Y", "Z"][axis];
            faces.insert(
                format!("{}{suffix}", ["x", "y", "z"][axis]),
                json!({"title": format!("{axis_name} {}", if suffix == "min" { "minimum" } else { "maximum" }) + " face",
                       "description": "Changing type resets this face to illustrative starter values. Check the values and initial flow; validate before applying.",
                       "x-object-variants": {"discriminator": "kind", "options": options}}),
            );
        }
    }
    json!({"title": "Outer-face flow conditions", "type": "object", "properties": faces})
}

pub const NAME: &str = "compressible_cartesian_euler3d";
pub const RESPONSE_UNITS: [(&str, &str); 3] =
    [("euler3d_peak_pressure_Pa", "Pa"), ("euler3d_peak_mach", "1"), ("euler3d_final_mass_kg", "kg")];

#[derive(Debug, Clone, Copy, Default)]
pub struct Euler3DProvider;

impl Euler3DProvider {
    #[must_use]
    pub fn template() -> Value {
        json!({"gamma": 1.4, "gas_constant_J_kgK": 287.0, "shape": [12, 6, 6], "spacing_m": [0.01, 0.01, 0.01],
               "fluid_mask": null, "geometry": null, "origin_m": [0.0, 0.0, 0.0], "export_wall_loads": false,
               "initial_primitive": [1.2, 0.0, 0.0, 0.0, 101_325.0],
               "boundaries": {"xmin": {"kind": "reflecting"}, "xmax": {"kind": "reflecting"}, "ymin": {"kind": "reflecting"},
                              "ymax": {"kind": "reflecting"}, "zmin": {"kind": "reflecting"}, "zmax": {"kind": "reflecting"}},
               "end_time_s": 0.0001, "cfl": 0.8, "max_steps": 20000,
               "provenance": "Synthetic uniform closed-box demonstration; not calibrated material data"})
    }


    pub fn capabilities_value() -> CaeResult<ProviderCapabilities> {
        let mut d = crate::providers::quasi1d_euler::Quasi1DEulerProvider::descriptor();
        d.name = NAME.into();
        d.analyses = vec!["cartesian_3d_compressible_euler".into()];
        d.responses = RESPONSE_UNITS.iter().map(|(k, _)| (*k).to_string()).collect();
        d.traits = json!({"evaluation_only": true, "spatial_preview": {"kind": "cartesian_cell_scalars", "mask": "fluid_mask",
            "fields": [{"key": "pressure_Pa", "label": "Pressure", "unit": "Pa"}, {"key": "temperature_K", "label": "Temperature", "unit": "K"},
                       {"key": "density_kg_m3", "label": "Density", "unit": "kg/m\u{b3}"}, {"key": "mach", "label": "Mach number", "unit": "1"}]}})
        .as_object()
        .cloned()
        .unwrap_or_default();
        d.fields = ["fluid_mask", "density_kg_m3", "velocity_m_s", "pressure_Pa", "temperature_K", "mach"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        d.response_metadata = response_metadata(
            &RESPONSE_UNITS,
            false,
            Some("Final-time 3-D Euler fluid-cell response; no topology derivative."),
        );
        d.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        let editor = json!({"kind": "native_json", "title": "3-D compressible flow \u{2014} inviscid evaluation", "problem_template": Self::template(),
            "schema": {"type": "object", "properties": {
                "gamma": {"title": "Specific heat ratio", "exclusiveMinimum": 1, "maximum": 2},
                "gas_constant_J_kgK": {"title": "Specific gas constant", "unit": "J/(kg K)", "exclusiveMinimum": 0},
                "end_time_s": {"title": "Simulation duration", "unit": "s", "exclusiveMinimum": 0},
                "shape": {"title": "Cells along X, Y and Z", "items": {"type": "integer", "minimum": 2, "maximum": 256}},
                "spacing_m": {"title": "Cell widths along X, Y and Z", "unit": "m", "items": {"exclusiveMinimum": 0}},
                "origin_m": {"title": "Grid origin X/Y/Z", "unit": "m"},
                "export_wall_loads": {"title": "Export spatial wall loads", "description": "Return numerical wall forces at face centres and conservative loads on Cartesian corner nodes. Increases result size. Frozen one-way export only; does not run mechanics or feed deformation back into flow."},
                "geometry": {"title": "Immutable CAD geometry snapshot", "format": "json",
                    "x-model-snapshot": [{"label": "Snapshot whole model as solid", "template": {"inside": "solid"}},
                                         {"label": "Snapshot whole model as fluid volume", "template": {"inside": "fluid"}}],
                    "description": "Capture the stored model below, or enter {model: complete inline model document, node: selected node or output name, inside: solid or fluid}. Solid means flow outside; fluid means flow inside. Set the explicit mask to null when using a snapshot. Match grid origin and cell widths to the CAD location and size. CAD is in mm; the flow grid is in m. Later CAD edits do not update this snapshot. Sidecar files are not loaded."},
                "fluid_mask": {"title": "Fluid and solid cells", "format": "json", "description": "null uses the CAD snapshot when supplied, otherwise all cells are fluid. An explicit nested [X][Y][Z] boolean mask uses true for fluid and false for slip-wall solids; with a snapshot it must match exactly. Cell centres are origin_m + (index + 0.5) times spacing_m."},
                "initial_primitive": {"title": "Initial gas state", "format": "json", "description": "[density kg/m\u{b3}, velocity X m/s, velocity Y m/s, velocity Z m/s, absolute pressure Pa], or nested [X][Y][Z][5] states."},
                "boundaries": boundary_editor_schema(),
                "cfl": {"title": "CFL time-step factor", "exclusiveMinimum": 0, "maximum": 0.8},
                "max_steps": {"title": "Maximum time steps", "type": "integer", "minimum": 1, "maximum": 1_000_000}}}});
        presentation(&d, editor, "array")
    }


    pub fn evaluate_value(problem: &Value) -> PResult<Evaluation> {
        let out = solve(problem)?;
        let fields = out.fields();
        let mask = &out.problem.fluid_mask;
        let peak = |name: &str| {
            fields.iter().find(|(k, _)| k == name).map_or(f64::NAN, |(_, f)| {
                f.values
                    .iter()
                    .zip(mask)
                    .filter(|(_, m)| **m)
                    .map(|(v, _)| *v)
                    .fold(f64::NEG_INFINITY, f64::max)
            })
        };
        let mut responses = std::collections::BTreeMap::new();
        responses.insert("euler3d_peak_pressure_Pa".into(), peak("pressure_Pa"));
        responses.insert("euler3d_peak_mach".into(), peak("mach"));
        responses.insert("euler3d_final_mass_kg".into(), out.ledger[1][0]);
        Ok(Evaluation {
            provider: NAME.into(),
            responses,
            diagnostics: out.diagnostics(),
            fields: fields
                .into_iter()
                .map(|(k, f)| {
                    let v = if k == "fluid_mask" {
                        FieldValue::Json(nested_bool(&f.shape, mask))
                    } else {
                        FieldValue::Array(f.to_array())
                    };
                    (k, v)
                })
                .collect(),
        })
    }
}

fn value_of(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("verification providers require their own problem mapping"))
}

impl CaeProvider for Euler3DProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn provider_id(&self) -> Option<&str> {
        Some(NAME)
    }

    fn implementation(&self) -> &'static str {
        "implexity.compressible.euler3d.Euler3DProvider"
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        Self::capabilities_value()
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        Some(
            evaluation_contract(NAME, &RESPONSE_UNITS, &LIMITATIONS)
                .map(|c| PublishedContract::Contract(Box::new(c))),
        )
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        normalize(problem).map_err(CaeError::from)?;
        Ok(Arc::new(problem.clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        no_design(topology).map_err(CaeError::from)?;
        let p = normalize(value_of(problem)?).map_err(CaeError::from)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("fluid_cells".into(), json!(p.fluid_mask.iter().filter(|m| **m).count()));
        out.insert("optimization_supported".into(), json!(false));
        out.insert("limitations".into(), strs(&LIMITATIONS));
        Ok(out)
    }

    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        no_design(Some(topology)).map_err(CaeError::from)?;
        Self::evaluate_value(value_of(problem)?).map_err(CaeError::from)
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(CaeError::contract("'Euler3DProvider' object has no attribute 'sensitivity'"))
    }

    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let d = CouplingDeclaration {
            provider: NAME.into(),
            active_physics: vec!["flow".into()],
            notes: LIMITATIONS.iter().map(|s| (*s).to_string()).collect(),
            ..CouplingDeclaration::default()
        };
        Some(Ok(d.to_value()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl implexity_optim::provider_ops::DesignOperations for Euler3DProvider {
    fn provides(&self, op: implexity_optim::provider_ops::DesignOp) -> bool {
        matches!(
            op,
            implexity_optim::provider_ops::DesignOp::Evaluate
                | implexity_optim::provider_ops::DesignOp::EvaluateWithoutDesign
        )
    }

    fn evaluate_without_design(&self, problem: &ProviderProblem) -> CaeResult<Evaluation> {
        Self::evaluate_value(value_of(problem)?).map_err(CaeError::from)
    }
}

pub use crate::euler3d::{LIMITATIONS,FACE_NAMES,Boundary,Problem,others,conservative,primitive,physical_flux,primitive_face_states,Faces,face_index,outward,divergence,rate,add_rate,initial_state,storage,WallLayout,Euler3dResult};


pub fn sample_geometry(
    geometry: &Value,
    shape: [usize; 3],
    spacing: [f64; 3],
    origin: [f64; 3],
) -> PResult<Vec<bool>> {
    crate::euler3d::sample_geometry(geometry,shape,spacing,origin).map_err(Into::into)
}


#[allow(clippy::too_many_lines)]
pub fn normalize(problem: &Value) -> PResult<Problem> {
    crate::euler3d::normalize(problem).map_err(Into::into)
}


pub fn check_state<S: Scalar>(q: [S; 5], w: [S; 5]) -> PResult<()> {
    crate::euler3d::check_state(q,w).map_err(Into::into)
}


pub fn primitive_checked<S: Scalar>(q: [S; 5], g: f64, check: bool) -> PResult<[S; 5]> {
    crate::euler3d::primitive_checked(q,g,check).map_err(Into::into)
}


pub fn characteristic<S: Scalar>(
    w: [S; 5],
    b: &Boundary,
    axis: usize,
    min_side: bool,
    p: &Problem,
    check: bool,
) -> PResult<[S; 5]> {
    crate::euler3d::characteristic(w,b,axis,min_side,p,check).map_err(Into::into)
}


#[allow(clippy::too_many_lines)]
pub fn faces<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    axis: usize,
    reconstruct: bool,
    check: bool,
) -> PResult<Faces<S>> {
    crate::euler3d::faces(q,p,axis,reconstruct,check).map_err(Into::into)
}


pub fn check_all(q: &[[f64; 5]], g: f64) -> PResult<Vec<[f64; 5]>> {
    crate::euler3d::check_all(q,g).map_err(Into::into)
}


#[allow(clippy::too_many_lines)]
pub fn boundary_exchange(q: &[[f64; 5]], p: &Problem) -> PResult<Value> {
    crate::euler3d::boundary_exchange(q,p).map_err(Into::into)
}


pub fn wall_loads(q: &[[f64; 5]], p: &Problem) -> PResult<WallLayout> {
    crate::euler3d::wall_loads(q,p).map_err(Into::into)
}


pub fn residual_diagnostics(q: &[[f64; 5]], p: &Problem) -> PResult<Value> {
    crate::euler3d::residual_diagnostics(q,p).map_err(Into::into)
}


pub fn solve(problem: &Value) -> PResult<Euler3dResult> {
    crate::euler3d::solve(problem).map_err(Into::into)
}
