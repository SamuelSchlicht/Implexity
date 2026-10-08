// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use crate::error::{GResult, GeometryError};

pub const CONTROL_SCHEMA: &str = "implexity-spatial-controls/1";

pub const INITIAL_OCCUPANCY: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct ControlComponent {
    pub name: String,
    pub channel: &'static str,
    pub component: usize,
    pub units: &'static str,
    pub label: String,
    pub lower: f64,
    pub upper: f64,
    pub initial: f64,
}

#[must_use]
pub fn control_components() -> Vec<ControlComponent> {
    let pi = std::f64::consts::PI;
    let mut out = vec![
        ControlComponent {
            name: "thickness".into(),
            channel: "a",
            component: 0,
            units: "1",
            label: "Thickness coordinate".into(),
            lower: -6.0,
            upper: 6.0,
            initial: 0.0,
        },
        ControlComponent {
            name: "occupancy".into(),
            channel: "m",
            component: 0,
            units: "1",
            label: "Occupancy mask".into(),
            lower: -8.0,
            upper: 8.0,
            initial: INITIAL_OCCUPANCY,
        },
    ];
    for (i, axis) in ["x", "y", "z"].iter().enumerate() {
        out.push(ControlComponent {
            name: format!("phase_{axis}"),
            channel: "dphi",
            component: i,
            units: "rad",
            label: format!("Phase warp {}", axis.to_uppercase()),
            lower: -pi,
            upper: pi,
            initial: 0.0,
        });
    }
    for (i, axis) in ["x", "y", "z"].iter().enumerate() {
        out.push(ControlComponent {
            name: format!("stretch_{axis}"),
            channel: "s",
            component: i,
            units: "1",
            label: format!("Logarithmic stretch {}", axis.to_uppercase()),
            lower: -4.0,
            upper: 4.0,
            initial: 0.0,
        });
    }
    for i in 0..8 {
        out.push(ControlComponent {
            name: format!("basis_{i}"),
            channel: "w",
            component: i,
            units: "1",
            label: format!("Basis coefficient {}", i + 1),
            lower: -3.0,
            upper: 3.0,
            initial: if i == 1 { 1.0 } else { 0.0 },
        });
    }
    out.push(ControlComponent {
        name: "sheet_network".into(),
        channel: "nu",
        component: 0,
        units: "1",
        label: "Sheet / network blend".into(),
        lower: -6.0,
        upper: 6.0,
        initial: 0.0,
    });
    out.push(ControlComponent {
        name: "secondary".into(),
        channel: "w2",
        component: 0,
        units: "1",
        label: "Secondary structure amplitude".into(),
        lower: -4.0,
        upper: 4.0,
        initial: 0.0,
    });
    out.push(ControlComponent {
        name: "residual".into(),
        channel: "res",
        component: 0,
        units: "1",
        label: "Free-form residual".into(),
        lower: -6.0,
        upper: 6.0,
        initial: 0.0,
    });
    out.push(ControlComponent {
        name: "phase_fraction".into(),
        channel: "c",
        component: 0,
        units: "1",
        label: "Neutral phase coordinate".into(),
        lower: -8.0,
        upper: 8.0,
        initial: 0.0,
    });
    out
}

pub const CHANNEL_SLICES: [(&str, usize, usize); 9] = [
    ("a", 0, 1),
    ("m", 1, 2),
    ("dphi", 2, 5),
    ("s", 5, 8),
    ("w", 8, 16),
    ("nu", 16, 17),
    ("w2", 17, 18),
    ("res", 18, 19),
    ("c", 19, 20),
];

#[must_use]
pub fn control_contract() -> Value {
    let comps: Vec<Value> = control_components()
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            json!({"index": i, "name": c.name, "channel": c.channel, "component": c.component, "units": c.units,
                "label": c.label, "lower": c.lower, "upper": c.upper, "initial": c.initial})
        })
        .collect();
    json!({
        "schema": CONTROL_SCHEMA, "components": comps, "component_count": 20, "layout": "component,x,y,z",
        "centering": "node", "coordinate": "model:control", "physics_independent": true,
        "count_semantics": "19 geometric fields and one neutral phase field",
        "bounds_semantics": "recommended optimizer bounds; the job declares the actual admissible bounds",
        "interpolation": "trilinear", "out_of_domain": "bounded_by_design_volume",
        "basis_order": ["cos_sum", "gyroid", "cos_product", "sin_product", "cos_pair_sum", "cos_double_sum", "cos_double_pair_sum", "double_sin_mixed_sum"],
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Controls {
    pub grid: [usize; 3],
    pub data: Vec<f64>,
}

impl Controls {
    #[must_use]
    pub fn n(&self) -> usize {
        self.grid[0] * self.grid[1] * self.grid[2]
    }

    #[must_use]
    pub fn component(&self, i: usize) -> &[f64] {
        let n = self.n();
        &self.data[i * n..(i + 1) * n]
    }
}


pub fn validate_control(
    shape: &[usize],
    data: &[f64],
    dtype_kind: char,
    grid: Option<[usize; 3]>,
) -> GResult<Controls> {
    if shape.len() != 4 || shape[0] != 20 || shape[1..].iter().copied().min().unwrap_or(0) < 2 {
        return Err(GeometryError::Value(
            "spatial controls must have shape (20, nx>=2, ny>=2, nz>=2); global demo scalars cannot be relabelled".into(),
        ));
    }
    if let Some(g) = grid
        && shape[1..] != g
    {
        return Err(GeometryError::Value("control tensor and declared control grid disagree".into()));
    }
    if !matches!(dtype_kind, 'i' | 'u' | 'f') || !data.iter().all(|v| v.is_finite()) {
        return Err(GeometryError::Value("spatial controls must be real and finite".into()));
    }
    Ok(Controls { grid: [shape[1], shape[2], shape[3]], data: data.to_vec() })
}

#[must_use]
pub fn initial_controls(grid: [usize; 3]) -> Controls {
    let n = grid[0] * grid[1] * grid[2];
    let mut data = Vec::with_capacity(20 * n);
    for c in control_components() {
        data.extend(std::iter::repeat_n(c.initial, n));
    }
    Controls { grid, data }
}

#[must_use]
pub fn control_bounds(grid: [usize; 3]) -> (Controls, Controls) {
    let n = grid[0] * grid[1] * grid[2];
    let comps = control_components();
    let lo = comps.iter().flat_map(|c| std::iter::repeat_n(c.lower, n)).collect();
    let hi = comps.iter().flat_map(|c| std::iter::repeat_n(c.upper, n)).collect();
    (Controls { grid, data: lo }, Controls { grid, data: hi })
}
