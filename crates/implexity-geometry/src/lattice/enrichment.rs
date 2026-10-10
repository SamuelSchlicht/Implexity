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


use crate::error::{GResult, GeometryError};
use crate::lattice::controls::Controls;
use crate::lattice::numerics::Interp;
use serde_json::{Value, json};

pub const SCHEMA: &str = "implexity-local-residual-enrichment/1";
pub const PARAMETER: &str = "residual_coefficients";
pub const COORDINATE: &str = "model:residual_coefficients";

#[derive(Clone, Debug, PartialEq)]
pub struct ResidualSupport {
    pub center_mm: [f64; 3],
    pub radius_mm: [f64; 3],
}

struct Wire {
    schema: String,
    fine_control_grid: [usize; 3],
    supports: Vec<ResidualSupport>,
    residual_bounds: [f64; 2],
    coefficient_bounds: [f64; 2],
}

impl Wire {
    fn parse(v: &Value) -> GResult<Self> {
        let m = v
            .as_object()
            .ok_or_else(|| invalid("local residual enrichment must be an object"))?;
        if m.len() != 5
            || m.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "schema"
                        | "fine_control_grid"
                        | "supports"
                        | "residual_bounds"
                        | "coefficient_bounds"
                )
            })
        {
            return Err(invalid("local residual enrichment keys invalid"));
        }
        fn numbers<const N: usize>(v: Option<&Value>) -> GResult<[f64; N]> {
            let a = v
                .and_then(Value::as_array)
                .filter(|a| a.len() == N)
                .ok_or_else(|| invalid("local residual metadata number array shape invalid"))?;
            let mut out = [0.; N];
            for (i, v) in a.iter().enumerate() {
                out[i] = v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| {
                    invalid("local residual metadata must be finite real numbers")
                })?;
            }
            Ok(out)
        }
        let grid = m
            .get("fine_control_grid")
            .and_then(Value::as_array)
            .filter(|a| a.len() == 3)
            .ok_or_else(|| invalid("local residual fine grid must be an integer triple"))?;
        let mut fine_control_grid = [0; 3];
        for (i, v) in grid.iter().enumerate() {
            fine_control_grid[i] = v
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| {
                    invalid("local residual fine grid must be a representable integer triple")
                })?;
        }
        let supports = m
            .get("supports")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("local residual supports must be an array"))?
            .iter()
            .map(|v| {
                let s = v
                    .as_object()
                    .filter(|s| {
                        s.len() == 2 && s.contains_key("center_mm") && s.contains_key("radius_mm")
                    })
                    .ok_or_else(|| invalid("local residual support keys invalid"))?;
                Ok(ResidualSupport {
                    center_mm: numbers(s.get("center_mm"))?,
                    radius_mm: numbers(s.get("radius_mm"))?,
                })
            })
            .collect::<GResult<Vec<_>>>()?;
        Ok(Self {
            schema: m
                .get("schema")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("local residual enrichment schema missing"))?
                .into(),
            fine_control_grid,
            supports,
            residual_bounds: numbers(m.get("residual_bounds"))?,
            coefficient_bounds: numbers(m.get("coefficient_bounds"))?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ResidualEnrichment {
    fine_control_grid: [usize; 3],
    supports: Vec<ResidualSupport>,
    residual_bounds: [f64; 2],
    coefficient_bounds: [f64; 2],
    coarse_grid: [usize; 3],
    weights: Vec<Vec<(usize, f64)>>,
    interpolation: Interp,
}

impl PartialEq for ResidualEnrichment {
    fn eq(&self, other: &Self) -> bool {
        self.fine_control_grid == other.fine_control_grid
            && self.supports == other.supports
            && self.residual_bounds == other.residual_bounds
            && self.coefficient_bounds == other.coefficient_bounds
            && self.coarse_grid == other.coarse_grid
            && self.weights == other.weights
    }
}

fn invalid(message: impl Into<String>) -> GeometryError {
    GeometryError::Value(message.into())
}

impl ResidualEnrichment {
    pub fn from_wire(
        value: &Value,
        coarse_grid: [usize; 3],
        origin: [f64; 3],
        domain: [f64; 3],
    ) -> GResult<Self> {
        let w = Wire::parse(value)?;
        if w.schema != SCHEMA
            || w.supports.is_empty()
            || w.supports.len() > 65536
            || (0..3).any(|a| {
                coarse_grid[a] < 2
                    || w.fine_control_grid[a] < coarse_grid[a]
                    || (w.fine_control_grid[a] - 1) % (coarse_grid[a] - 1) != 0
            })
            || w.fine_control_grid == coarse_grid
            || !origin.iter().all(|v| v.is_finite())
            || !domain.iter().all(|v| v.is_finite() && *v > 0.)
            || (0..3).any(|a| !(origin[a] + domain[a]).is_finite())
            || w.fine_control_grid
                .iter()
                .try_fold(20usize, |p, v| p.checked_mul(*v))
                .is_none_or(|n| n > 8_000_000)
            || [w.residual_bounds, w.coefficient_bounds]
                .iter()
                .any(|b| !b.iter().all(|v| v.is_finite()) || b[0] >= b[1] || b[0] > 0. || b[1] < 0.)
        {
            return Err(invalid(
                "local residual enrichment needs a nested finer grid, nonempty supports, finite ordered bounds including zero, and a finite volume",
            ));
        }
        let mut weights = Vec::with_capacity(w.supports.len());
        let mut nonzeros = 0usize;
        for support in &w.supports {
            if (0..3).any(|a| {
                !support.center_mm[a].is_finite()
                    || !support.radius_mm[a].is_finite()
                    || support.radius_mm[a] <= 0.
                    || support.center_mm[a] - support.radius_mm[a] < origin[a]
                    || support.center_mm[a] + support.radius_mm[a] > origin[a] + domain[a]
            }) {
                return Err(invalid(
                    "local residual support must be a finite positive ellipsoid contained in the declared volume in mm",
                ));
            }
            let mut row = Vec::new();
            let bounds: [(usize, usize); 3] = std::array::from_fn(|a| {
                let scale = (w.fine_control_grid[a] - 1) as f64 / domain[a];
                let lo = ((support.center_mm[a] - support.radius_mm[a] - origin[a]) * scale)
                    .floor()
                    .max(0.) as usize;
                let hi = ((support.center_mm[a] + support.radius_mm[a] - origin[a]) * scale)
                    .ceil()
                    .min((w.fine_control_grid[a] - 1) as f64) as usize;
                (lo, hi)
            });
            for i in bounds[0].0..=bounds[0].1 {
                for j in bounds[1].0..=bounds[1].1 {
                    for k in bounds[2].0..=bounds[2].1 {
                        let idx = [i, j, k];
                        let r2 = (0..3)
                            .map(|a| {
                                let x = origin[a]
                                    + domain[a] * idx[a] as f64
                                        / (w.fine_control_grid[a] - 1) as f64;
                                ((x - support.center_mm[a]) / support.radius_mm[a]).powi(2)
                            })
                            .sum::<f64>();
                        if r2 < 1. {
                            let r = r2.sqrt();
                            let weight = (1. - r).powi(4) * (4. * r + 1.);
                            row.push((
                                (i * w.fine_control_grid[1] + j) * w.fine_control_grid[2] + k,
                                weight,
                            ));
                        }
                    }
                }
            }
            if row.is_empty() {
                return Err(invalid(
                    "local residual support is unresolved on the declared finer control grid",
                ));
            }
            nonzeros = nonzeros
                .checked_add(row.len())
                .ok_or_else(|| invalid("local residual basis size overflow"))?;
            if nonzeros > 8_000_000 {
                return Err(invalid(
                    "local residual basis exceeds the eight-million-value authoring limit",
                ));
            }
            weights.push(row);
        }
        Ok(Self {
            fine_control_grid: w.fine_control_grid,
            supports: w.supports,
            residual_bounds: w.residual_bounds,
            coefficient_bounds: w.coefficient_bounds,
            coarse_grid,
            weights,
            interpolation: Self::make_interpolation(coarse_grid, w.fine_control_grid),
        })
    }
    pub fn to_wire(&self) -> Value {
        json!({"schema":SCHEMA,"fine_control_grid":self.fine_control_grid,"supports":self.supports.iter().map(|s|json!({"center_mm":s.center_mm,"radius_mm":s.radius_mm})).collect::<Vec<_>>(),
        "residual_bounds":self.residual_bounds,"coefficient_bounds":self.coefficient_bounds})
    }
    pub fn fine_control_grid(&self) -> [usize; 3] {
        self.fine_control_grid
    }
    pub fn coefficient_bounds(&self) -> [f64; 2] {
        self.coefficient_bounds
    }
    pub fn coefficient_count(&self) -> usize {
        self.supports.len()
    }
    pub fn coefficients(&self, values: &[f64]) -> GResult<()> {
        if values.len() != self.coefficient_count()
            || values.iter().any(|v| {
                !v.is_finite() || *v < self.coefficient_bounds[0] || *v > self.coefficient_bounds[1]
            })
        {
            return Err(invalid(
                "local residual coefficients must match the support count and declared finite bounds",
            ));
        }
        Ok(())
    }
    fn make_interpolation(coarse_grid: [usize; 3], fine_control_grid: [usize; 3]) -> Interp {
        let n = fine_control_grid.iter().product::<usize>();
        let mut ci: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::with_capacity(n));
        for i in 0..fine_control_grid[0] {
            for j in 0..fine_control_grid[1] {
                for k in 0..fine_control_grid[2] {
                    let idx = [i, j, k];
                    for a in 0..3 {
                        ci[a].push(
                            idx[a] as f64 * (coarse_grid[a] - 1) as f64
                                / (fine_control_grid[a] - 1) as f64,
                        );
                    }
                }
            }
        }
        Interp::new(coarse_grid, &ci)
    }
    fn linear_decode(&self, control: &Controls, coefficients: &[f64]) -> GResult<Controls> {
        if control.grid != self.coarse_grid
            || control.data.len() != 20 * control.n()
            || !control
                .data
                .iter()
                .chain(coefficients)
                .all(|v| v.is_finite())
            || coefficients.len() != self.coefficient_count()
        {
            return Err(invalid(
                "local residual base or coefficient shape/finiteness invalid",
            ));
        }
        let interp = &self.interpolation;
        let n = self.fine_control_grid.iter().product::<usize>();
        let mut data = Vec::with_capacity(20 * n);
        for c in 0..20 {
            data.extend(interp.forward(control.component(c)));
        }
        for (coefficient, row) in coefficients.iter().zip(&self.weights) {
            for &(i, w) in row {
                data[18 * n + i] += *coefficient * w;
            }
        }
        if !data.iter().all(|v| v.is_finite()) {
            return Err(invalid("local residual decode overflow"));
        }
        Ok(Controls {
            grid: self.fine_control_grid,
            data,
        })
    }
    pub fn decode(&self, control: &Controls, coefficients: &[f64]) -> GResult<Controls> {
        self.coefficients(coefficients)?;
        let decoded = self.linear_decode(control, coefficients)?;
        if decoded
            .component(18)
            .iter()
            .any(|v| *v < self.residual_bounds[0] || *v > self.residual_bounds[1])
        {
            return Err(invalid(
                "decoded local residual exceeds its declared fine-field bounds",
            ));
        }
        Ok(decoded)
    }
    pub fn tangent(&self, control: &Controls, coefficients: &[f64]) -> GResult<Controls> {
        self.linear_decode(control, coefficients)
    }
    pub fn transpose(&self, bar: &[f64]) -> GResult<(Vec<f64>, Vec<f64>)> {
        let n = self.fine_control_grid.iter().product::<usize>();
        if bar.len() != 20 * n || !bar.iter().all(|v| v.is_finite()) {
            return Err(invalid(
                "local residual fine adjoint shape/finiteness invalid",
            ));
        }
        let interp = &self.interpolation;
        let mut base = Vec::with_capacity(20 * self.coarse_grid.iter().product::<usize>());
        for c in 0..20 {
            base.extend(interp.transpose(&bar[c * n..(c + 1) * n]));
        }
        let coefficients = self
            .weights
            .iter()
            .map(|row| row.iter().fold(0., |v, (i, w)| v + bar[18 * n + *i] * w))
            .collect::<Vec<_>>();
        if !base.iter().chain(&coefficients).all(|v| v.is_finite()) {
            return Err(invalid("local residual adjoint overflow"));
        }
        Ok((base, coefficients))
    }
    pub fn compact_layout(&self) -> Value {
        json!({"schema":"implexity-compact-design-layout/1","shape":[self.coefficient_count()],
        "coordinate":COORDINATE,"parameter":PARAMETER,"label":"Local shape detail","representation":"additive_compact_residual_coefficients",
        "units":"1","support_units":"mm","basis":"wendland_c2_ellipsoid","supports":self.supports.iter().map(|s|json!({"center_mm":s.center_mm,"radius_mm":s.radius_mm})).collect::<Vec<_>>(),"fine_control_grid":self.fine_control_grid,
        "coefficient_bounds":self.coefficient_bounds,"residual_bounds":self.residual_bounds,"orthogonal":false})
    }
}
