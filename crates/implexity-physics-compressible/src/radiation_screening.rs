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


use implexity_ad::Scalar;
use implexity_physics_base::PhysicsError;
use serde_json::{Map, Value};
use crate::errors::{PResult, ModelError};
use crate::pyval::{num, py_float};
use crate::roots::{Residual, temperature_root};

pub const SIGMA: f64 = 5.670_374_419e-8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadiationSpec {
    pub emissivity: f64,
    pub view_factor: f64,
}

impl RadiationSpec {
    #[must_use]
    pub fn from_map(m: &Map<String, Value>) -> Self {
        Self {
            emissivity: m.get("effectiveEmissivity").and_then(Value::as_f64).unwrap_or(0.5),
            view_factor: m.get("viewFactor").and_then(Value::as_f64).unwrap_or(1.0),
        }
    }
}


pub fn normalize_radiation(spec: Option<&Value>, path: &str) -> PResult<Option<Map<String, Value>>> {
    let Some(spec) = spec.filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(map) = spec.as_object() else {
        return Err(ModelError::validation("radiation screening declaration must be an object", path));
    };
    let mut out = map.clone();
    for (k, d) in [("effectiveEmissivity", 0.5), ("viewFactor", 1.0)] {
        let v = match out.get(k) {
            Some(v) => py_float(v).ok_or_else(|| {
                ModelError::from(PhysicsError::value(format!(
                    "could not convert string to float: {}",
                    implexity_core::pyobj::repr(v)
                )))
            })?,
            None => d,
        };
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return Err(ModelError::validation(format!("{k} must lie in [0,1]"), format!("{path}.{k}")));
        }
        out.insert(k.to_string(), num(v));
    }
    Ok(Some(out))
}

#[must_use]
pub fn radiative_flux<S: Scalar>(gas: S, wall: S, spec: &RadiationSpec) -> S {
    (gas.powi(4) - wall.powi(4)) * (spec.emissivity * spec.view_factor * SIGMA)
}

struct WallBalance(RadiationSpec);

impl Residual for WallBalance {
    fn eval<T: Scalar>(&self, tw: T, p: &[T]) -> T {
        let (gas, coolant, h_gas, thickness, k, h_cool) = (p[0], p[1], p[2], p[3], p[4], p[5]);
        let r_out = thickness / k + T::one() / h_cool;
        h_gas * (gas - tw) + radiative_flux(gas, tw, &self.0) - (tw - coolant) / r_out
    }
}

#[must_use]
pub fn wall_temperature<S: Scalar>(
    gas: S,
    coolant: S,
    h_gas: S,
    thickness: S,
    k: S,
    h_cool: S,
    spec: &RadiationSpec,
) -> (S, S) {
    let p = [gas, coolant, h_gas, thickness, k, h_cool];
    let tw = temperature_root(&WallBalance(*spec), gas, coolant, &p);
    let q = h_gas * (gas - tw) + radiative_flux(gas, tw, spec);
    (tw, q)
}

fn wall<T: Scalar>(tc: T, p: &[T], spec: Option<&RadiationSpec>) -> (T, T) {
    let (gas, h_gas, thickness, k, h_cool) = (p[0], p[4], p[5], p[6], p[7]);
    if let Some(s) = spec {
        wall_temperature(gas, tc, h_gas, thickness, k, h_cool, s)
    } else {
        let resistance = T::one() / h_gas + thickness / k + T::one() / h_cool;
        let flux = (gas - tc) / resistance;
        (gas - flux / h_gas, flux)
    }
}

struct SegmentBalance(Option<RadiationSpec>);

impl Residual for SegmentBalance {
    fn eval<T: Scalar>(&self, tout: T, p: &[T]) -> T {
        let (inlet, capacity, area) = (p[1], p[2], p[3]);
        capacity * (tout - inlet) - area * wall(tout, p, self.0.as_ref()).1
    }
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn coolant_section<S: Scalar>(
    gas: S,
    inlet: S,
    capacity: S,
    area: S,
    h_gas: S,
    thickness: S,
    k: S,
    h_cool: S,
    spec: Option<&RadiationSpec>,
) -> (S, S, S) {
    let p = [gas, inlet, capacity, area, h_gas, thickness, k, h_cool];
    let tout = temperature_root(&SegmentBalance(spec.copied()), inlet, gas, &p);
    let (tw, flux) = wall(tout, &p, spec);
    (tout, tw, flux)
}
