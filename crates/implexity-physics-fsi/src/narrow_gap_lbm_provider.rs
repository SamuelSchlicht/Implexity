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
    narrow_gap_lbm::GapLbm,
    narrow_gap_provider::{COORDINATE, GapDocument},
};
use implexity_core::{
    CaeError, CaeResult,
    contracts::{
        CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor,
        ProviderProblem, Sensitivity,
    },
    packages::InstallContext,
};
use implexity_optim::{
    design::NamedArrays,
    optimizer::OptimizerLifecycleConfig,
    provider_ops::{
        DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, LifecycleDeclaration,
    },
};
use implexity_physics_lbm::moving::{
    carrier::RigidCarrier,
    field::{AnyMovingLbm, LatticeKind, MovingLbmConfig},
    port_exchange::PortExchange,
};
use ndarray::{ArrayD, IxDyn};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{any::Any, collections::BTreeMap, sync::Arc};
pub const NAME: &str = "conservative_narrow_gap_lbm";
const RESPONSES: [&str; 8] = [
    "gap_volume_m3",
    "outward_volume_flux_m3_s",
    "bulk_mass_increment_kg",
    "bulk_mean_pressure_pa",
    "port_x_minus_flux_m3_s",
    "port_x_plus_flux_m3_s",
    "port_y_minus_flux_m3_s",
    "port_y_plus_flux_m3_s",
];
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkGapPort {
    pub side: usize,
    pub cells: Vec<(usize, f64)>,
    pub velocity_m_s: [f64; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkGapDocument {
    pub schema: String,
    pub gap: GapDocument,
    pub bulk_shape: [usize; 3],
    pub bulk_spacing_m: f64,
    pub periodic: [bool; 3],
    pub solid_mask: Vec<bool>,
    pub excluded_gap_cells: Vec<usize>,
    pub ports: Vec<BulkGapPort>,
    pub initial_pressure_pa: f64,
    pub lattice: String,
    pub tolerance_pa: f64,
    pub maximum_iterations: usize,
}
fn fail(e: impl std::fmt::Display) -> CaeError {
    CaeError::contract(e.to_string())
}
fn problem(p: &ProviderProblem) -> CaeResult<&BulkGapDocument> {
    p.downcast_ref()
        .ok_or_else(|| fail("bulk-gap problem type"))
}
fn diagnostics() -> Map<String, Value> {
    json!({"physical_qualification":false,"surface_scope":"fixed planar gas patch","coordinate_array_axes":["y","x"],"bulk_model":"native three-dimensional LBM","interface_coupling":"implicit pressure and conservative volume exchange","derivative_scope":"native fluid reverse and implicit patch transpose","solid_attachment":false,"moving_interface_attachment":false,"port_momentum":"prescribed advected velocity with external momentum ledger","gap_inertia":"neglected within prescribed Reynolds validity bounds","maximum_patch_cells":256}).as_object().unwrap().clone()
}
impl BulkGapDocument {
    pub fn build(&self) -> CaeResult<GapLbm> {
        self.gap.validate()?;
        let cells = self
            .bulk_shape
            .iter()
            .try_fold(1usize, |a, b| a.checked_mul(*b))
            .ok_or_else(|| fail("bulk-gap shape overflow"))?;
        if self.schema != "implexity-narrow-gap-lbm/1"
            || self.bulk_shape.contains(&0)
            || cells != self.solid_mask.len()
            || cells.checked_mul(27).is_none()
            || !self.bulk_spacing_m.is_finite()
            || self.bulk_spacing_m <= 0.
            || !self.initial_pressure_pa.is_finite()
            || self.ports.is_empty()
            || self.ports.len() > 4
        {
            return Err(fail("invalid bulk-gap document"));
        }
        let lattice = match self.lattice.as_str() {
            "d3q19" => LatticeKind::D3Q19,
            "d3q27" => LatticeKind::D3Q27,
            _ => return Err(fail("bulk-gap lattice must be d3q19 or d3q27")),
        };
        let mut config = MovingLbmConfig::new(
            self.bulk_shape,
            self.bulk_spacing_m,
            self.periodic,
            self.gap.grid.density_kg_m3,
            self.gap.grid.viscosity_pa_s / self.gap.grid.density_kg_m3,
            self.gap.grid.time_step_s,
            1,
        );
        config.solid_mask = self.solid_mask.clone();
        config.initial_pressure_pa = self.initial_pressure_pa;
        let bulk = AnyMovingLbm::new(
            lattice,
            config,
            Arc::new(RigidCarrier::new(vec![], vec![], false)?),
        )?;
        let exchange = PortExchange::new(
            cells,
            self.excluded_gap_cells.clone(),
            self.ports.iter().map(|p| p.cells.clone()).collect(),
            self.bulk_spacing_m,
            self.gap.grid.density_kg_m3,
            self.gap.grid.time_step_s,
        )?;
        GapLbm::new(
            bulk,
            self.gap.clone(),
            exchange,
            self.ports.iter().map(|p| p.side).collect(),
            self.ports.iter().map(|p| p.velocity_m_s).collect(),
            self.tolerance_pa,
            self.maximum_iterations,
        )
    }
    fn gap_design(&self, d: &NamedArrays) -> CaeResult<Vec<f64>> {
        if d.names().len() != 1 {
            return Err(fail("bulk-gap provider requires one gap coordinate"));
        }
        let a = d
            .get(COORDINATE)
            .ok_or_else(|| fail("bulk-gap coordinate absent"))?;
        if a.shape() != self.gap.grid.array_shape() {
            return Err(fail("bulk-gap coordinate shape"));
        }
        Ok(a.iter().copied().collect())
    }
    pub fn evaluate_gap(&self, gap: &[f64]) -> CaeResult<Evaluation> {
        let model = self.build()?;
        let initial = model.initial_state()?;
        let step = model.advance(1, &initial, gap)?;
        let area = self.gap.grid.spacing_m[0] * self.gap.grid.spacing_m[1];
        let pressure = self.bulk_mean_pressure(&step.state.populations)?;
        let mut responses = BTreeMap::from([
            ("gap_volume_m3".into(), gap.iter().sum::<f64>() * area),
            (
                "outward_volume_flux_m3_s".into(),
                step.interface.linearization.flux_m3_s.iter().sum(),
            ),
            ("bulk_mass_increment_kg".into(), step.bulk_mass_increment_kg),
            ("bulk_mean_pressure_pa".into(), pressure),
        ]);
        for (side, name) in RESPONSES[4..].iter().enumerate() {
            responses.insert((*name).into(), step.interface.linearization.flux_m3_s[side]);
        }
        let mut fields = BTreeMap::new();
        fields.insert(
            "gap_m".into(),
            FieldValue::Array(
                ArrayD::from_shape_vec(IxDyn(&self.gap.grid.array_shape()), gap.to_vec())
                    .map_err(fail)?,
            ),
        );
        fields.insert(
            "gap_pressure_pa".into(),
            FieldValue::Array(
                ArrayD::from_shape_vec(
                    IxDyn(&self.gap.grid.array_shape()),
                    step.interface
                        .linearization
                        .solution
                        .state
                        .pressure_above_cavitation_pa
                        .clone(),
                )
                .map_err(fail)?,
            ),
        );
        fields.insert(
            "bulk_populations".into(),
            FieldValue::Array(
                ArrayD::from_shape_vec(
                    IxDyn(&[step.state.populations.len()]),
                    step.state.populations,
                )
                .map_err(fail)?,
            ),
        );
        fields.insert(
            "port_flux_m3_s".into(),
            FieldValue::Json(json!(step.interface.linearization.flux_m3_s)),
        );
        let mut info = diagnostics();
        info.insert(
            "exchange_mass_defect_kg".into(),
            json!(step.exchange_mass_defect_kg),
        );
        info.insert(
            "external_port_momentum_kg_m_s".into(),
            json!(step.external_port_momentum_kg_m_s),
        );
        info.insert(
            "interface_iterations".into(),
            json!(step.interface.iterations),
        );
        Ok(Evaluation {
            provider: NAME.into(),
            responses,
            fields,
            diagnostics: info,
        })
    }
    fn bulk_mean_pressure(&self, populations: &[f64]) -> CaeResult<f64> {
        let n = self.solid_mask.len();
        let q = if self.lattice == "d3q19" { 19 } else { 27 };
        if populations.len() != q * n {
            return Err(fail("bulk pressure field shape"));
        }
        let count = self.solid_mask.iter().filter(|v| !**v).count();
        if count == 0 {
            return Err(fail("bulk fluid domain is empty"));
        }
        let scale = self.gap.grid.density_kg_m3
            * (self.bulk_spacing_m / self.gap.grid.time_step_s).powi(2)
            / 3.;
        Ok((0..n)
            .filter(|i| !self.solid_mask[*i])
            .map(|i| ((0..q).map(|j| populations[j * n + i]).sum::<f64>() - 1.) * scale)
            .sum::<f64>()
            / count as f64)
    }
    pub fn gradient_gap(&self, gap: &[f64], name: &str) -> CaeResult<Vec<f64>> {
        let model = self.build()?;
        let initial = model.initial_state()?;
        let mut populations_bar = vec![0.; initial.populations.len()];
        let mut gap_bar = vec![0.; gap.len()];
        let mut flux_bar = [0.; 4];
        let area = self.gap.grid.spacing_m[0] * self.gap.grid.spacing_m[1];
        match name {
            "gap_volume_m3" => gap_bar.fill(area),
            "outward_volume_flux_m3_s" => flux_bar.fill(1.),
            "bulk_mass_increment_kg" => {
                flux_bar.fill(self.gap.grid.density_kg_m3 * self.gap.grid.time_step_s)
            }
            "bulk_mean_pressure_pa" => {
                let n = self.solid_mask.len();
                let count = self.solid_mask.iter().filter(|v| !**v).count();
                if count == 0 {
                    return Err(fail("bulk fluid domain is empty"));
                }
                let scale = self.gap.grid.density_kg_m3
                    * (self.bulk_spacing_m / self.gap.grid.time_step_s).powi(2)
                    / 3.
                    / count as f64;
                for (i, b) in populations_bar.iter_mut().enumerate() {
                    if !self.solid_mask[i % n] {
                        *b = scale;
                    }
                }
            }
            n if RESPONSES[4..].contains(&n) => {
                let side = RESPONSES[4..]
                    .iter()
                    .position(|s| *s == n)
                    .ok_or_else(|| fail("unknown bulk-gap response"))?;
                flux_bar[side] = 1.;
            }
            _ => return Err(fail("unknown bulk-gap response")),
        }
        Ok(model
            .advance_vjp(1, &initial, gap, &populations_bar, &gap_bar, flux_bar)?
            .gap_m)
    }
}
pub struct NarrowGapLbmProvider;
impl CaeProvider for NarrowGapLbmProvider {
    fn name(&self) -> &str {
        NAME
    }
    fn implementation(&self) -> &str {
        "implexity_physics_fsi::narrow_gap_lbm_provider::NarrowGapLbmProvider"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut d = ProviderDescriptor::new(NAME, vec![NAME.into()], vec![]);
        d.nonlinear = true;
        d.sensitivities = true;
        d.design_coordinates = vec![COORDINATE.into()];
        d.traits = diagnostics();
        Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(json!({"kind":"native_json","title":"Bulk flow and narrow gap","schema":editor_schema()}),"array")?)))
    }
    fn coupling_declaration(&self, _: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        Some(crate::narrow_gap_provider::gap_coupling_declaration(NAME, true))
    }
    fn normalise_problem(&self, v: &Value) -> CaeResult<ProviderProblem> {
        let p: BulkGapDocument = serde_json::from_value(v.clone()).map_err(fail)?;
        let _ = p.build()?;
        Ok(Arc::new(p))
    }
    fn preflight(
        &self,
        p: &ProviderProblem,
        _: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let _ = problem(p)?.build()?;
        Ok(diagnostics())
    }
    fn evaluate(&self, p: &ProviderProblem, g: &ArrayD<f64>) -> CaeResult<Evaluation> {
        let p = problem(p)?;
        if g.shape() != p.gap.grid.array_shape() {
            return Err(fail("bulk-gap coordinate shape"));
        }
        p.evaluate_gap(&g.iter().copied().collect::<Vec<_>>())
    }
    fn sensitivity(&self, p: &ProviderProblem, g: &ArrayD<f64>, n: &str) -> CaeResult<Sensitivity> {
        let p = problem(p)?;
        if g.shape() != p.gap.grid.array_shape() {
            return Err(fail("bulk-gap coordinate shape"));
        }
        let h = g.iter().copied().collect::<Vec<_>>();
        let e = p.evaluate_gap(&h)?;
        Ok(Sensitivity {
            provider: NAME.into(),
            response: n.into(),
            value: *e
                .responses
                .get(n)
                .ok_or_else(|| fail("unknown bulk-gap response"))?,
            gradient: ArrayD::from_shape_vec(
                IxDyn(&p.gap.grid.array_shape()),
                p.gradient_gap(&h, n)?,
            )
            .map_err(fail)?,
            diagnostics: e.diagnostics,
        })
    }
    fn interface(&self, n: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(n)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl DesignOperations for NarrowGapLbmProvider {
    fn problem_document(&self, p: &ProviderProblem) -> Option<CaeResult<Value>> {
        Some(problem(p).and_then(|p| serde_json::to_value(p).map_err(fail)))
    }
    fn provides(&self, o: DesignOp) -> bool {
        matches!(
            o,
            DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::OptimizerLifecycle
        )
    }
    fn authoring_design_coordinates(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(problem(p).map(|_| vec![COORDINATE.into()]))
    }
    fn problem_responses(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(problem(p).map(|_| RESPONSES.iter().map(|n| n.to_string()).collect()))
    }
    fn preflight_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let p = problem(p)?;
        p.gap.grid.validate(&p.gap_design(d)?, false)?;
        let _ = p.build()?;
        Ok(diagnostics())
    }
    fn evaluate_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        o: usize,
    ) -> CaeResult<Evaluation> {
        if o != 0 {
            return Err(fail("bulk-gap provider has one operating point"));
        }
        let p = problem(p)?;
        p.evaluate_gap(&p.gap_design(d)?)
    }
    fn sensitivity_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        n: &str,
        o: usize,
    ) -> CaeResult<DesignSensitivity> {
        let e = self.evaluate_design(p, d, o)?;
        let p = problem(p)?;
        let mut gradients = NamedArrays::new();
        gradients.insert(
            COORDINATE,
            ArrayD::from_shape_vec(
                IxDyn(&p.gap.grid.array_shape()),
                p.gradient_gap(&p.gap_design(d)?, n)?,
            )
            .map_err(fail)?,
        );
        Ok(DesignSensitivity {
            value: *e
                .responses
                .get(n)
                .ok_or_else(|| fail("unknown bulk-gap response"))?,
            gradients,
            diagnostics: e.diagnostics,
        })
    }
    fn sensitivities_design(
        &self,
        p: &ProviderProblem,
        d: &NamedArrays,
        n: &[String],
        o: usize,
    ) -> CaeResult<DesignSensitivities> {
        let mut responses = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in n {
            let r = self.sensitivity_design(p, d, name, o)?;
            responses.insert(name.clone(), r.value);
            gradients.insert(name.clone(), r.gradients);
        }
        Ok(DesignSensitivities {
            responses,
            gradients,
            diagnostics: diagnostics(),
        })
    }
    fn optimizer_lifecycle(&self, _: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
            vec![COORDINATE.into()],
            "sensitivity_design",
            "evaluate_design",
            None,
            None,
            false,
            false,
        )?))
    }
}
pub fn install(ctx: &InstallContext<'_>) -> CaeResult<()> {
    ctx.register_provider(Arc::new(NarrowGapLbmProvider))
        .map(|_| ())
}
pub fn editor_schema() -> Value {
    let array3 = |item: Value| json!({"type":"array","minItems":3,"maxItems":3,"items":item});
    json!({"type":"object","additionalProperties":false,"required":["schema","gap","bulk_shape","bulk_spacing_m","periodic","solid_mask","excluded_gap_cells","ports","initial_pressure_pa","lattice","tolerance_pa","maximum_iterations"],"properties":{
        "schema":{"type":"string","enum":["implexity-narrow-gap-lbm/1"]},"gap":crate::narrow_gap_provider::editor_schema(),"bulk_shape":array3(json!({"type":"integer","minimum":1})),"bulk_spacing_m":{"type":"number","exclusiveMinimum":0},"periodic":array3(json!({"type":"boolean"})),"solid_mask":{"type":"array","items":{"type":"boolean"}},"excluded_gap_cells":{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"integer","minimum":0}},
        "ports":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","additionalProperties":false,"required":["side","cells","velocity_m_s"],"properties":{"side":{"type":"integer","minimum":0,"maximum":3},"cells":{"type":"array","minItems":1,"items":{"type":"array","minItems":2,"maxItems":2,"prefixItems":[{"type":"integer","minimum":0},{"type":"number","exclusiveMinimum":0}]}},"velocity_m_s":array3(json!({"type":"number"}))}}},
        "initial_pressure_pa":{"type":"number"},"lattice":{"type":"string","enum":["d3q19","d3q27"]},"tolerance_pa":{"type":"number","exclusiveMinimum":0},"maximum_iterations":{"type":"integer","minimum":1}
    },"x-qualification":diagnostics()})
}
