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
use implexity_physics_thermofluid::equation_of_state::EquationOfState;
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::plug_nozzle_expansion::{check_bounded, check_positive};
use crate::pyval::fmt_e;

pub const SIMP_PENALTY: i32 = 3;

const RPM_TO_RAD_S: f64 = 2.0 * std::f64::consts::PI / 60.0;

pub const RESULT_NAMES: [&str; 16] = [
    "shaft_power_W",
    "specific_work_J_kg",
    "p_out_Pa",
    "T_out_K",
    "rho_out_kg_m3",
    "h_in_J_kg",
    "h_out_J_kg",
    "h_out_eos_J_kg",
    "delta_p_Pa",
    "delta_h_J_kg",
    "delta_T_K",
    "U_tip_m_s",
    "eta_signed",
    "loading",
    "alpha_effective",
    "direction",
];

pub fn signed_efficiency<S: Scalar>(direction: S, eta_pump: S, eta_turbine: S) -> S {
    eta_pump * direction.max_f64(0.0) - eta_turbine * (-direction).max_f64(0.0)
}

#[derive(Debug, Clone)]
pub struct TurboInputs<S> {
    pub p_in: S,
    pub t_in: S,
    pub y_in: Vec<S>,
    pub mdot: S,
    pub shaft_speed_rpm: S,
    pub tip_radius_m: S,
    pub direction: S,
    pub alpha: S,
    pub efficiency: Option<S>,
    pub loading: Option<S>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurbomachineEulerWork {
    pub eos: EquationOfState,
    pub default_efficiency: f64,
    pub default_loading: f64,
    pub switch_certificate: f64,
}

impl TurbomachineEulerWork {

    pub fn new(
        eos: EquationOfState,
        default_efficiency: f64,
        default_loading: f64,
        switch_certificate: f64,
    ) -> PResult<Self> {
        if !default_efficiency.is_finite() || default_efficiency <= 0.0 || default_efficiency > 1.0 {
            return Err(ModelError::validation(
                "default_efficiency must be a finite float in (0, 1]",
                "turbomachine.default_efficiency",
            )
            .detail("value", json!(default_efficiency)));
        }
        if !default_loading.is_finite() || default_loading <= 0.0 || default_loading > 2.0 {
            return Err(ModelError::validation(
                "default_loading must be a finite float in (0, 2]",
                "turbomachine.default_loading",
            )
            .detail("value", json!(default_loading)));
        }
        if !switch_certificate.is_finite() || switch_certificate <= 0.0 {
            return Err(ModelError::validation(
                "switch_certificate must be a positive finite float",
                "turbomachine.switch_certificate",
            )
            .detail("value", json!(switch_certificate)));
        }
        Ok(Self { eos, default_efficiency, default_loading, switch_certificate })
    }


    pub fn with_defaults(eos: EquationOfState) -> PResult<Self> {
        Self::new(eos, 0.75, 0.5, 1.0e-8)
    }


    pub fn evaluate<S: Scalar>(&self, i: &TurboInputs<S>) -> PResult<[S; 16]> {
        let pre = "turbomachine";
        check_positive(pre, "p_in", i.p_in.value(), 0.0)?;
        check_positive(pre, "T_in", i.t_in.value(), 0.0)?;
        check_positive(pre, "mdot", i.mdot.value(), 0.0)?;
        check_positive(pre, "tip_radius_m", i.tip_radius_m.value(), 0.0)?;
        let rpm = i.shaft_speed_rpm.value();
        if !rpm.is_finite() {
            return Err(ModelError::validation(
                "shaft_speed_rpm must be finite",
                "turbomachine.shaft_speed_rpm",
            )
            .detail("value", json!(rpm)));
        }
        check_bounded(pre, "direction", i.direction.value(), -1.0, 1.0)?;
        check_bounded(pre, "alpha", i.alpha.value(), 0.0, 1.0)?;
        if let Some(e) = i.efficiency.map(|e| e.value())
            && (!e.is_finite() || e <= 0.0 || e > 1.0)
        {
            return Err(ModelError::validation(
                "efficiency must be finite and in (0, 1]",
                "turbomachine.efficiency",
            )
            .detail("value", json!(e)));
        }
        if let Some(l) = i.loading.map(|l| l.value())
            && (!l.is_finite() || l <= 0.0 || l > 2.0)
        {
            return Err(ModelError::validation(
                "loading must be finite and in (0, 2]",
                "turbomachine.loading",
            )
            .detail("value", json!(l)));
        }
        if i.y_in.len() != self.eos.species.len() {
            return Err(ModelError::validation(
                format!(
                    "y_in must have shape ({},) matching eos.species; got ({},)",
                    self.eos.species.len(),
                    i.y_in.len()
                ),
                "turbomachine.y_in",
            ));
        }
        let eta = i.efficiency.unwrap_or_else(|| S::from_f64(self.default_efficiency));
        let psi = i.loading.unwrap_or_else(|| S::from_f64(self.default_loading));
        let props_in = self.eos.evaluate(&i.y_in, i.p_in, i.t_in)?;
        let omega = i.shaft_speed_rpm * RPM_TO_RAD_S;
        let u_tip = omega * i.tip_radius_m;
        let eta_signed = signed_efficiency(i.direction, eta, eta);
        let alpha_eff = i.alpha.powi(SIMP_PENALTY);
        let w = eta_signed * alpha_eff * (psi * (u_tip * u_tip));
        let h_out = props_in.enthalpy + w;
        let delta_p = props_in.density * w;
        let p_out = i.p_in + delta_p;
        let delta_t = w / props_in.cp;
        let t_out = i.t_in + delta_t;
        let props_out = self.eos.evaluate(&i.y_in, p_out, t_out)?;
        Ok([
            i.mdot * w,
            w,
            p_out,
            t_out,
            props_out.density,
            props_in.enthalpy,
            h_out,
            props_out.enthalpy,
            delta_p,
            w,
            delta_t,
            u_tip,
            eta_signed,
            psi,
            alpha_eff,
            i.direction,
        ])
    }


    pub fn certify_sensitivity(&self, direction: Option<f64>) -> PResult<Value> {
        let Some(d) = direction else {
            return Err(ModelError::validation(
                "certify_sensitivity requires `direction`",
                "turbomachine.certify_sensitivity.direction",
            ));
        };
        if !d.is_finite() {
            return Err(ModelError::validation(
                "direction must be a finite concrete float for the switch-distance certificate",
                "turbomachine.certify_sensitivity.direction",
            )
            .detail("value", json!(d)));
        }
        let distance = d.abs();
        if distance <= self.switch_certificate {
            return Err(ModelError::contract(format!(
                "TurbomachineEulerWork sign transition: |direction| = {} <= switch_certificate = {}; the pump / turbine regime is undefined at this point and the shaft-work sensitivity is set by tie-breaking rather than by physics.  Move ``direction`` away from zero (or shrink the SIMP ``alpha`` toward 0 to fade the component) before requesting a derivative.  (switch-distance certificate)",
                fmt_e(distance, 3),
                fmt_e(self.switch_certificate, 0)
            )));
        }
        let mut m = Map::new();
        m.insert("direction_distance".into(), json!(distance));
        m.insert("switch_certificate".into(), json!(self.switch_certificate));
        m.insert("sensitivity_admissible".into(), json!(true));
        Ok(Value::Object(m))
    }
}

#[must_use]
pub fn to_value(values: &[f64; 16]) -> Value {
    Value::Object(RESULT_NAMES.iter().zip(values).map(|(k, v)| ((*k).to_string(), json!(v))).collect())
}
