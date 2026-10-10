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
    gas_continuous_layout::{ContinuousGasLayout, ContinuousGasRepresentation},
    gas_continuum_fv::ContinuumFvPolicy,
    gas_quadratic_network::QuadraticGasNetworkPolicy,
    gas_quadratic_network_solver::QuadraticGasNetworkSolvePolicy,
    gas_quadratic_velocity::QuadraticVelocityBasis,
    native_gas_geometry_adapter::NativeGasGeometryPath,
};
use implexity_core::{CaeError, CaeResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeContinuousTransportDocument {
    pub schema: String,
    pub velocity_knots_m_s: [Vec<f64>; 3],
    pub reference_speed_m_s: f64,
    pub continuum_slots: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_kinetic_sgs: Option<crate::gas_continuous_mixed::KineticResolvedSgsRegion>,
    pub transport: QuadraticGasNetworkPolicy,
    pub continuum: ContinuumFvPolicy,
    pub solve: QuadraticGasNetworkSolvePolicy,
    #[serde(default)]
    pub use_selected_kkt_newton: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaussian_primal_cache_entries: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_preconditioner: Option<crate::gas_continuous_preconditioner::ContinuousTransportPreconditionerPolicy>,
}

pub struct NativeContinuousTransport {
    pub layout: Arc<ContinuousGasLayout>,
    pub resolved_kinetic_sgs: Option<crate::gas_continuous_mixed::KineticResolvedSgsRegion>,
    pub transport: QuadraticGasNetworkPolicy,
    pub continuum: ContinuumFvPolicy,
    pub solve: QuadraticGasNetworkSolvePolicy,
    pub use_selected_kkt_newton: bool,
    pub gaussian_primal_cache_entries: Option<usize>,
    pub transport_preconditioner: Option<crate::gas_continuous_preconditioner::ContinuousTransportPreconditionerPolicy>,
}

impl NativeContinuousTransportDocument {
    pub fn from_json(bytes: &[u8]) -> CaeResult<Self> {
        let document: Self =
            serde_json::from_slice(bytes).map_err(|e| CaeError::contract(e.to_string()))?;
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> CaeResult<()> {
        if self.schema != "implexity-native-continuous-transport/1" {
            return Err(CaeError::contract("continuous transport schema"));
        }
        self.solve.nonlinear.validate()?;
        if let Some(policy) = &self.transport_preconditioner {
            policy.validate()?;
        }
        if self.gaussian_primal_cache_entries == Some(0) {
            return Err(CaeError::contract(
                "Gaussian cache capacity must be positive",
            ));
        }
        QuadraticVelocityBasis::new(self.velocity_knots_m_s.clone(), self.reference_speed_m_s)?;
        let positive = [
            self.transport.entropy.moment_tolerance,
            self.transport.diffuse_wall.mass_flux_relative_tolerance,
            self.transport.geometric_relative_tolerance,
            self.continuum.maximum_knudsen_number,
            self.continuum.maximum_collision_time_ratio,
            self.continuum.maximum_mach_number,
            self.continuum.geometric_relative_tolerance,
            self.solve.cone.tolerance,
            self.solve.numerical_projection_step,
            self.solve.reference_density_kg_m3,
            self.solve.physical_moment_relative_tolerance,
        ];
        let nonnegative = [
            self.transport
                .diffuse_wall
                .mass_flux_absolute_tolerance_kg_m2_s,
            self.transport.geometric_absolute_volume_tolerance_m3,
            self.continuum.metric_absolute_tolerance_m2,
            self.continuum.geometric_absolute_tolerance_m3,
        ];
        if positive.iter().any(|v| !v.is_finite() || *v <= 0.)
            || nonnegative.iter().any(|v| !v.is_finite() || *v < 0.)
            || [
                self.transport.entropy.maximum_covariance_condition,
                self.continuum.maximum_gradient_condition_number,
                self.solve.cone.maximum_schur_condition,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v <= 1.)
            || self
                .solve
                .physical_moment_absolute_tolerances
                .iter()
                .any(|v| !v.is_finite() || *v < 0.)
            || !self
                .solve
                .cone
                .derivative_relative_dual_tolerance
                .is_finite()
            || !(0. ..1.).contains(&self.solve.cone.derivative_relative_dual_tolerance)
            || self.transport.entropy.maximum_iterations == 0
            || self.transport.diffuse_wall.maximum_iterations == 0
            || self.transport.maximum_operator_nonzeros == 0
            || self.solve.cone.maximum_iterations == 0
        {
            return Err(CaeError::contract("continuous transport policy domain"));
        }
        if let Some(wale) = &self.continuum.wale {
            wale.model.validate()?;
            if !wale.spacing_m.is_finite() || wale.spacing_m <= 0. {
                return Err(CaeError::contract("continuous WALE spacing"));
            }
        }
        Ok(())
    }

    pub fn build(self, path: &NativeGasGeometryPath<f64>) -> CaeResult<NativeContinuousTransport> {
        self.validate()?;
        path.certify_stationary_continuum_slots(&self.continuum_slots)?;
        let count = path.current_geometry.aggregated.volumes_m3.len();
        if self.continuum.characteristic_length_m.len() != count
            || self.continuum.forcing_timescale_s.len() != count
            || self
                .continuum
                .characteristic_length_m
                .iter()
                .chain(&self.continuum.forcing_timescale_s)
                .any(|v| !v.is_finite() || *v <= 0.)
        {
            return Err(CaeError::contract("continuous transport per-slot scales"));
        }
        let mut representations = vec![ContinuousGasRepresentation::Kinetic; count];
        for slot in self.continuum_slots {
            representations[slot] = ContinuousGasRepresentation::Continuum;
        }
        let basis = Arc::new(QuadraticVelocityBasis::new(
            self.velocity_knots_m_s,
            self.reference_speed_m_s,
        )?);
        Ok(NativeContinuousTransport {
            layout: Arc::new(ContinuousGasLayout::new(basis, representations)?),
            resolved_kinetic_sgs: self.resolved_kinetic_sgs,
            transport: self.transport,
            continuum: self.continuum,
            solve: self.solve,
            use_selected_kkt_newton: self.use_selected_kkt_newton,
            gaussian_primal_cache_entries: self.gaussian_primal_cache_entries,
            transport_preconditioner: self.transport_preconditioner,
        })
    }
}

pub(crate) fn object(properties: Value) -> Value {
    let required = properties
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}
pub(crate) fn number(unit: &str) -> Value {
    json!({"type":"number","unit":unit})
}
pub(crate) fn integer() -> Value {
    json!({"type":"integer","minimum":1})
}
pub(crate) fn array(item: Value, length: Option<usize>) -> Value {
    let mut value = json!({"type":"array","items":item});
    if let Some(n) = length {
        value["minItems"] = json!(n);
        value["maxItems"] = json!(n);
    }
    value
}

pub fn editor_schema() -> Value {
    let axes = array(json!({"type":"boolean"}), Some(3));
    let reconstruction = json!({"oneOf":[
        object(json!({"kind":{"const":"constant"}})),
        object(json!({"kind":{"const":"linear"},"axes":axes,"maximum_condition_number":number("1")})),
        object(json!({"kind":{"const":"quadratic"},"axes":axes,"maximum_condition_number":number("1")}))
    ]});
    let entropy = object(
        json!({"moment_tolerance":number("1"),"maximum_iterations":integer(),"maximum_covariance_condition":number("1")}),
    );
    let wall = object(
        json!({"mass_flux_absolute_tolerance_kg_m2_s":number("kg m-2 s-1"),"mass_flux_relative_tolerance":number("1"),"maximum_iterations":integer()}),
    );
    let transport = object(
        json!({"reconstruction":reconstruction,"entropy":entropy,"diffuse_wall":wall,"maximum_operator_nonzeros":integer(),"geometric_relative_tolerance":number("1"),"geometric_absolute_volume_tolerance_m3":number("m3")}),
    );
    let wale = object(
        json!({"model":object(json!({"constant":number("1"),"denominator_floor":number("s-5")})),"spacing_m":number("m")}),
    );
    let continuum = object(
        json!({"wale":{"oneOf":[{"type":"null"},wale]},"gradient_axes":axes,"maximum_gradient_condition_number":number("1"),"characteristic_length_m":array(number("m"),None),"forcing_timescale_s":array(number("s"),None),"maximum_knudsen_number":number("1"),"maximum_collision_time_ratio":number("1"),"maximum_mach_number":number("1"),"metric_absolute_tolerance_m2":number("m2"),"geometric_absolute_tolerance_m3":number("m3"),"geometric_relative_tolerance":number("1")}),
    );
    let cone = object(
        json!({"factorization":{"oneOf":[object(json!({"kind":{"const":"sparse"}})),object(json!({"kind":{"const":"dense_reference"},"maximum_unknowns":integer()}))]},"maximum_iterations":integer(),"tolerance":number("1"),"maximum_schur_condition":number("1"),"derivative_relative_dual_tolerance":number("1")}),
    );
    let nonlinear = object(
        json!({"relative_tolerance":number("1"),"absolute_tolerance":number("1"),"linear_relative_tolerance":number("1"),"linear_absolute_tolerance":number("1"),"maximum_newton_iterations":integer(),"maximum_linear_iterations":integer(),"krylov_restart":integer(),"maximum_backtracking_steps":integer(),"sufficient_decrease":number("1")}),
    );
    let solve = object(
        json!({"numerical_scaling":{"type":"string","enum":["equilibrium_coefficient","uniform_coefficient"]},"nonlinear":nonlinear,"cone":cone,"numerical_projection_step":number("1"),"reference_density_kg_m3":number("kg m-3"),"physical_moment_relative_tolerance":number("1"),"physical_moment_absolute_tolerances":array(json!({"type":"number"}),Some(10))}),
    );
    let mut region = object(json!({
        "cells":array(json!({"type":"integer","minimum":0}),None),
        "stencil_neighbors":{"type":"object","additionalProperties":array(object(json!({
            "slot":{"type":"integer","minimum":0},
            "periodic_image":array(json!({"type":"integer"}),Some(3))
        })),None)}
    }));
    region["required"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "stencil_neighbors");
    region["properties"]["stencil_neighbors"]["additionalProperties"]["items"]["required"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "periodic_image");
    let preconditioner = object(json!({
        "maximum_population_count":integer(),
        "maximum_local_matrix_entries":integer(),
        "maximum_coarse_dimension":integer(),
        "maximum_coarse_nonzeros":integer(),
        "vacuum_reference_density_kg_m3":number("kg m-3"),
        "freeze_within_solve":{"type":"boolean"}
    }));
    let mut schema = object(
        json!({"schema":{"const":"implexity-native-continuous-transport/1"},"velocity_knots_m_s":array(array(number("m s-1"),None),Some(3)),"reference_speed_m_s":number("m s-1"),"continuum_slots":array(json!({"type":"integer","minimum":0}),None),"resolved_kinetic_sgs":region,"transport":transport,"continuum":continuum,"solve":solve,"gaussian_primal_cache_entries":{"title":"Gas equilibrium cache entries","type":"integer","minimum":1},"use_selected_kkt_newton":{"title":"Selected KKT Newton","type":"boolean","default":false},"transport_preconditioner":preconditioner}),
    );
    schema["required"].as_array_mut().unwrap().retain(|v| {
        v != "use_selected_kkt_newton"
            && v != "resolved_kinetic_sgs"
            && v != "gaussian_primal_cache_entries"
            && v != "transport_preconditioner"
    });
    schema["title"] = json!("Continuous gap gas");
    schema
}
