// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use ndarray::{Array3, ArrayView3, Axis, Slice};
use serde_json::{Map, Value, json};

use crate::pyfmt::fmt_g;
use crate::workspace_contract::{CfdProblem, face_axis, face_is_min, port_response};

type FacePair = (Option<String>, Option<String>);

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub field: Option<String>,
    pub action: Option<Value>,
}

impl Issue {
    fn new(severity: &str, code: &str, message: impl Into<String>, field: Option<&str>) -> Self {
        Self {
            severity: severity.into(),
            code: code.into(),
            message: message.into(),
            field: field.map(str::to_string),
            action: None,
        }
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({
            "severity": self.severity,
            "code": self.code,
            "message": self.message,
            "field": self.field,
            "action": self.action,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreflightReport {
    pub ok: bool,
    pub issues: Vec<Issue>,
    pub diagnostics: Map<String, Value>,
    pub required_topology_roles: Vec<Value>,
}

impl PreflightReport {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({
            "ok": self.ok,
            "issues": self.issues.iter().map(Issue::as_dict).collect::<Vec<_>>(),
            "diagnostics": Value::Object(self.diagnostics.clone()),
            "required_topology_roles": self.required_topology_roles,
        })
    }

    #[must_use]
    pub fn error_messages(&self, separator: &str) -> String {
        self.issues
            .iter()
            .filter(|i| i.severity == "error")
            .map(|i| i.message.as_str())
            .collect::<Vec<_>>()
            .join(separator)
    }
}

#[must_use]
pub fn port_mask(shape: [usize; 3], face: &str, layers: usize) -> Array3<bool> {
    let mut m = Array3::from_elem((shape[0], shape[1], shape[2]), false);
    let axis = face_axis(face);
    let n = layers.min(shape[axis]);
    if n == 0 {
        return m;
    }
    let (lo, hi) = if face_is_min(face) { (0, n) } else { (shape[axis] - n, shape[axis]) };
    m.slice_axis_mut(Axis(axis), Slice::from(lo..hi)).fill(true);
    m
}


#[must_use]
#[allow(clippy::too_many_lines)]
pub fn run_preflight(
    p: &CfdProblem,
    solid_fraction: Option<ndarray::ArrayViewD<'_, f64>>,
) -> PreflightReport {
    let mut issues = Vec::new();
    let mut required = Vec::new();
    let enabled: Vec<_> = p.enabled_boundaries().collect();
    let mut response_faces: Vec<(String, FacePair)> = Vec::new();
    for (index, objective) in p.objectives.iter().enumerate() {
        let field = format!("objectives.{index}");
        if let Some((face, _)) = port_response(&objective.response)
            && !enabled.iter().any(|b| b.face == face && crate::workspace_contract::is_open_kind(&b.kind))
        {
            issues.push(Issue::new(
                "error",
                "closed_response_port",
                format!("Named port objective requires an enabled open boundary on {face}"),
                Some(&field),
            ));
        }
        let pair = (objective.inlet_face.clone(), objective.outlet_face.clone());
        if p.solver.model == "stokes_brinkman" && objective.region_id.is_some() {
            issues.push(Issue::new(
                "error",
                "unsupported_response_region",
                "Resolved Stokes responses use inlet/outlet domain faces; region_id is not executed by this provider.",
                Some(&format!("{field}.region_id")),
            ));
        }
        if pair.0.is_some() != pair.1.is_some() {
            issues.push(Issue::new(
                "error",
                "incomplete_response_faces",
                "Specify both inlet and outlet faces, or neither for automatic inference.",
                Some(&field),
            ));
        } else if pair.0.is_some() && pair.0 == pair.1 {
            issues.push(Issue::new(
                "error",
                "identical_response_faces",
                "Response inlet and outlet must be distinct faces.",
                Some(&field),
            ));
        }
        if let Some((_, previous)) = response_faces.iter().find(|(r, _)| *r == objective.response)
            && *previous != pair
        {
            issues.push(Issue::new(
                "error",
                "conflicting_response_faces",
                "Use one inlet/outlet face pair per named response.",
                Some(&field),
            ));
        }
        if let Some(slot) = response_faces.iter_mut().find(|(r, _)| *r == objective.response) {
            slot.1 = pair;
        } else {
            response_faces.push((objective.response.clone(), pair));
        }
    }
    let open_bc: Vec<_> =
        enabled.iter().copied().filter(|b| crate::workspace_contract::is_open_kind(&b.kind)).collect();
    let mut prescribed: Vec<(String, f64)> = Vec::new();
    for b in &enabled {
        if let Some(q) = b.inward_volume_flow_m3_s(&p.domain, &p.fluid) {
            prescribed.push((b.face.clone(), q));
        }
        if crate::workspace_contract::is_open_kind(&b.kind) {
            required.push(json!({
                "role": "keep_void",
                "face": b.face,
                "layers": b.port_buffer_cells,
                "reason": "CFD open-port protection",
            }));
        }
    }
    let gravity_drive = p.domain.gravity_m_s2.iter().all(|g| g.is_finite())
        && p.domain.gravity_m_s2.iter().any(|g| *g != 0.0);
    if open_bc.is_empty() && !gravity_drive {
        issues.push(Issue::new(
            "error",
            "closed_without_drive",
            "all boundaries are closed; define an inlet/outlet, moving wall, or a physically meaningful body force",
            Some("boundaries"),
        ));
    }
    if !prescribed.is_empty() && open_bc.iter().all(|b| b.kind != "pressure" && b.kind != "traction_outlet") {
        let net = prescribed.iter().fold(0.0, |acc, (_, q)| acc + q);
        let scale = prescribed.iter().fold(0.0, |acc, (_, q)| acc + q.abs()).max(1e-30);
        if net.abs() > 1e-8 * scale {
            issues.push(Issue::new(
                "error",
                "incompatible_prescribed_flow",
                format!("prescribed inward volume flows do not balance: net {} m^3/s", fmt_g(net, 6)),
                Some("boundaries"),
            ));
        }
    }
    let pressures: Vec<f64> = open_bc
        .iter()
        .filter(|b| b.kind == "pressure")
        .map(|b| b.static_pressure_pa.unwrap_or(0.0))
        .collect();
    let gravity_norm = {
        let g = p.domain.gravity_m_s2;
        (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt()
    };
    if pressures.len() >= 2 {
        let max = pressures.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = pressures.iter().copied().fold(f64::INFINITY, f64::min);
        if max - min == 0.0 && prescribed.is_empty() && gravity_norm == 0.0 {
            issues.push(Issue::new(
                "warning",
                "zero_pressure_drive",
                "all pressure boundaries have the same pressure and no other flow driver is defined",
                Some("boundaries"),
            ));
        }
    }
    let dx = p.domain.spacing_m();
    let h = dx[0].min(dx[1]).min(dx[2]);
    let rho = p.fluid.density_kg_m3;
    let mu = p.fluid.dynamic_viscosity_pa_s;
    let re = rho * p.domain.reference_velocity_m_s * p.domain.reference_length_m / mu;
    let re_cell = rho * p.domain.reference_velocity_m_s * h / mu;
    let da_s = p.brinkman.solid_permeability_m2 / p.domain.reference_length_m.powi(2);
    let contrast = p.brinkman.fluid_permeability_m2 / p.brinkman.solid_permeability_m2;
    if p.solver.model == "stokes_brinkman" && re > 1.0 {
        issues.push(Issue::new(
            "warning",
            "stokes_reynolds",
            format!("reference Reynolds number is {}; inertial effects may not be negligible", fmt_g(re, 4)),
            Some("solver.model"),
        ));
    }
    if p.solver.model == "steady_laminar_navier_stokes_brinkman" && re > 2300.0 {
        issues.push(Issue::new(
            "error",
            "laminar_range",
            format!(
                "reference Reynolds number is {}; the declared steady laminar model is not justified without a case-specific stability assessment",
                fmt_g(re, 4)
            ),
            Some("solver.model"),
        ));
    }
    if p.solver.convection_scheme == "central" && re_cell > 2.0 {
        issues.push(Issue::new(
            "warning",
            "cell_peclet",
            format!("cell Reynolds number is {}; central convection may oscillate", fmt_g(re_cell, 4)),
            Some("solver.convection_scheme"),
        ));
    }
    if da_s > 1e-5 {
        issues.push(Issue::new(
            "warning",
            "solid_leakage",
            format!("solid Darcy number {} may permit visible leakage", fmt_g(da_s, 4)),
            Some("brinkman.solid_permeability_m2"),
        ));
    }
    if contrast > 1e18 {
        issues.push(Issue::new(
            "warning",
            "conditioning",
            format!(
                "permeability contrast {} is extreme; use continuation and a block preconditioner",
                fmt_g(contrast, 4)
            ),
            Some("brinkman"),
        ));
    }
    if let Some(s) = solid_fraction {
        let cells = p.domain.cells;
        if s.shape() != cells.as_slice() {
            let shape_repr = shape_tuple(s.shape());
            issues.push(Issue::new(
                "error",
                "topology_shape",
                format!("topology shape {shape_repr} does not match CFD cells {}", p.domain.cells_repr()),
                Some("model:control"),
            ));
        } else if let Ok(s3) = s.into_dimensionality::<ndarray::Ix3>() {
            for b in &open_bc {
                if blocked(&s3, cells, &b.face, b.port_buffer_cells) {
                    let mut issue = Issue::new(
                        "error",
                        "blocked_port",
                        format!(
                            "open boundary {} contains solid material in its protected port buffer",
                            b.face
                        ),
                        Some("model:control"),
                    );
                    issue.action = Some(json!({
                        "role": "keep_void",
                        "face": b.face,
                        "layers": b.raw()["port_buffer_cells"].clone(),
                    }));
                    issues.push(issue);
                }
            }
        }
    }
    let mut diagnostics = Map::new();
    diagnostics.insert("Re_reference".into(), json!(re));
    diagnostics.insert("Re_cell".into(), json!(re_cell));
    diagnostics.insert("Darcy_solid".into(), json!(da_s));
    diagnostics.insert("permeability_contrast".into(), json!(contrast));
    diagnostics.insert("cell_spacing_m".into(), json!(dx));
    diagnostics.insert(
        "prescribed_inward_flows_m3_s".into(),
        Value::Array(prescribed.iter().map(|(f, q)| json!([f, q])).collect()),
    );
    PreflightReport {
        ok: !issues.iter().any(|i| i.severity == "error"),
        issues,
        diagnostics,
        required_topology_roles: required,
    }
}

fn shape_tuple(shape: &[usize]) -> String {
    let parts: Vec<String> = shape.iter().map(ToString::to_string).collect();
    if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
}

fn blocked(s: &ArrayView3<'_, f64>, cells: [usize; 3], face: &str, layers: usize) -> bool {
    let mask = port_mask(cells, face, layers);
    let mut any = false;
    let mut max = f64::NEG_INFINITY;
    for (m, v) in mask.iter().zip(s.iter()) {
        if *m {
            any = true;
            if v.is_nan() || max.is_nan() {
                max = f64::NAN;
            } else if *v > max {
                max = *v;
            }
        }
    }
    any && max > 0.05
}
