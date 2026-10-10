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
    contact_constraint_tangent::ContactTangentPolicy,
    moving_contact::separate_body::MovingFsiPairModel,
    native_continuous_document::NativeContinuousTransportDocument,
    native_dry_gap_factory::{NativeDryGapAssembly, NativeDryGapSettings, assemble_native_dry_gap},
    patch_unilateral_contact::PatchContactSite,
    surface_gap::SurfaceGapCell,
};
use implexity_core::{CaeError, CaeResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeContactTangentDocument {
    pub rank_relative_tolerance: f64,
    pub compatibility_relative_tolerance: f64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDryGapGeometryDocument {
    pub gap_cells: Vec<SurfaceGapCell>,
    pub paired_vertex_ids: Vec<[usize; 3]>,
    pub contact_sites: Vec<PatchContactSite>,
    pub layers: usize,
    pub residual_slots_per_cell: usize,
    pub halo_cells: usize,
    pub iso: f64,
    pub geometry_length_tolerance_m: f64,
    pub reference_pressure_pa: f64,
    pub gap_scale_m: f64,
    pub force_scale_n: f64,
    pub initial_residual_tolerance: f64,
    pub contact_tangent: NativeContactTangentDocument,
}

impl NativeDryGapGeometryDocument {
    fn settings(&self, coefficient_count: usize) -> NativeDryGapSettings {
        NativeDryGapSettings {
            gap_cells: self.gap_cells.clone(),
            paired_vertex_ids: self.paired_vertex_ids.clone(),
            contact_sites: self.contact_sites.clone(),
            layers: self.layers,
            residual_slots_per_cell: self.residual_slots_per_cell,
            populations: coefficient_count,
            halo_cells: self.halo_cells,
            iso: self.iso,
            geometry_length_tolerance_m: self.geometry_length_tolerance_m,
            reference_pressure_pa: self.reference_pressure_pa,
            gap_scale_m: self.gap_scale_m,
            force_scale_n: self.force_scale_n,
            initial_residual_tolerance: self.initial_residual_tolerance,
            contact_tangent: ContactTangentPolicy {
                rank_relative_tolerance: self.contact_tangent.rank_relative_tolerance,
                compatibility_relative_tolerance: self
                    .contact_tangent
                    .compatibility_relative_tolerance,
            },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeContinuousProblemDocument {
    pub schema: String,
    pub bodies: [Value; 2],
    pub geometry: NativeDryGapGeometryDocument,
    pub transport: NativeContinuousTransportDocument,
}

pub struct NativeContinuousProblem {
    pub model: MovingFsiPairModel,
    pub document: NativeContinuousProblemDocument,
    pub identity: String,
}

impl NativeContinuousProblemDocument {
    pub fn from_json(bytes: &[u8]) -> CaeResult<Self> {
        serde_json::from_slice(bytes).map_err(|e| CaeError::contract(e.to_string()))
    }

    pub fn build(self) -> CaeResult<NativeContinuousProblem> {
        if self.schema != "implexity-native-continuous-fsi-pair/1" {
            return Err(CaeError::contract("native continuous pair schema"));
        }
        self.transport.validate()?;
        let geometry = &self.geometry;
        if geometry.gap_cells.is_empty()
            || geometry.gap_cells.len() != geometry.paired_vertex_ids.len()
            || geometry.contact_sites.is_empty()
            || geometry.contact_sites.iter().any(|site| site.cell >= geometry.gap_cells.len() || site.corner >= 3)
            || geometry.contact_sites.iter().map(|site|(site.cell,site.corner)).collect::<std::collections::BTreeSet<_>>().len() != geometry.contact_sites.len()
        {
            return Err(CaeError::contract("native continuous contact geometry layout"));
        }
        let model = MovingFsiPairModel::from_bodies(
            crate::problem::normalise(&self.bodies[0])?,
            crate::problem::normalise(&self.bodies[1])?,
        )?;
        let _ = model.solid_field()?;
        if model.native_body(0)?.problem.normal_form()["coupling"]
            != model.native_body(1)?.problem.normal_form()["coupling"]
        {
            return Err(CaeError::contract(
                "native continuous pair coupling policies differ",
            ));
        }
        let value = serde_json::to_value(&self).map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(NativeContinuousProblem {
            model,
            document: self,
            identity: implexity_core::json::canonical_sha256(&value),
        })
    }
}

impl NativeContinuousProblem {
    pub fn physical_design(&self, controls: [&[f64]; 2]) -> CaeResult<Vec<f64>> {
        self.model.event_physical_design(controls)
    }

    pub fn assemble(&self, design: &[f64]) -> CaeResult<NativeDryGapAssembly<'_>> {
        let basis = crate::gas_quadratic_velocity::QuadraticVelocityBasis::new(
            self.document.transport.velocity_knots_m_s.clone(),
            self.document.transport.reference_speed_m_s,
        )?;
        assemble_native_dry_gap(
            &self.model,
            design,
            self.document.geometry.settings(basis.coefficient_count()),
        )
    }
}

pub fn editor_schema() -> Value {
    use crate::native_continuous_document::{array, integer, number, object};
    use serde_json::json;
    let index = json!({"type":"integer","minimum":0});
    let feature = json!({"oneOf":[
        object(json!({"kind":{"const":"vertex"},"data":index})),
        object(json!({"kind":{"const":"fixed_tetrahedron"},"data":object(json!({"nodes":array(index.clone(),Some(4)),"weights":array(number("1"),Some(4)),"weight_tolerance":number("1")}))})),
        object(json!({"kind":{"const":"density_edge"},"data":object(json!({"nodes":array(index.clone(),Some(2)),"iso":number("1"),"contrast_min":number("1"),"interior_margin":number("1")}))}))
    ]});
    let cell = object(
        json!({"features":array(array(feature,Some(3)),Some(2)),"minimum_area_m2":number("m2"),"minimum_wedge_jacobian_m3":number("m3"),"maximum_tangent_offset_ratio":number("1")}),
    );
    let geometry = object(json!({
        "gap_cells":array(cell,None),"paired_vertex_ids":array(array(index.clone(),Some(3)),None),
        "contact_sites":array(object(json!({"cell":index,"corner":{"type":"integer","minimum":0,"maximum":2}})),None),
        "layers":integer(),"residual_slots_per_cell":integer(),"halo_cells":{"type":"integer","minimum":0},
        "iso":number("1"),"geometry_length_tolerance_m":number("m"),"reference_pressure_pa":number("Pa"),
        "gap_scale_m":number("m"),"force_scale_n":number("N"),"initial_residual_tolerance":number("1"),
        "contact_tangent":object(json!({"rank_relative_tolerance":number("1"),"compatibility_relative_tolerance":number("1")}))
    }));
    object(
        json!({"schema":{"const":"implexity-native-continuous-fsi-pair/1"},"bodies":array(crate::editor::schema(None),Some(2)),"geometry":geometry,"transport":crate::native_continuous_document::editor_schema()}),
    )
}
