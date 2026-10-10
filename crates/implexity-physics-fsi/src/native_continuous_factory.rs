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
    moving_contact::{contact_field::ContactField, separate_body::PairSolidField},
    native_closed_release::{ClosedReleaseContactLaw, NativeClosedGeometryWitness},
    native_continuous_coupled_field::{
        NativeContinuousCoupledField, NativeContinuousCoupledFieldConfig,
    },
    native_continuous_problem::NativeContinuousProblem,
};
use implexity_core::CaeResult;
use implexity_solve::multirate_coupling::{FluxDrivenField, MultirateOptions, MultirateStepper};

pub type NativeContinuousStepper<'a> = MultirateStepper<
    NativeContinuousCoupledField,
    ContactField<PairSolidField<'a>, ClosedReleaseContactLaw>,
>;

impl NativeContinuousProblem {
    pub fn stepper(&self, design: &[f64]) -> CaeResult<NativeContinuousStepper<'_>> {
        let assembly = self.assemble(design)?;
        let contact = assembly
            .contact
            .map_law(|law| Ok(ClosedReleaseContactLaw::initially_closed(law)))?;
        let law = contact.law().shared_law();
        let witness = std::sync::Arc::new(NativeClosedGeometryWitness::from_patch_trace(
            &law,
            0,
            &assembly.initial_trace,
            &assembly.initial_trace,
            implexity_solve::time_stepper::StepParameters {
                design,
                time_scale: 1.,
            },
        )?);
        let initial_certificate = if witness.closed_start_charts().is_empty() {
            None
        } else {
            let initial_contact_state = contact.initial_state(design)?;
            let initial_extension=crate::native_closed_release::NativeClosedReleaseTrace::maintained_closed_from_patch(&law,contact.trace_operator(),0,&initial_contact_state,&initial_contact_state,implexity_solve::time_stepper::StepParameters {design,time_scale:1.},crate::contact_constraint_tangent::ContactTangentPolicy {
                rank_relative_tolerance:self.document.geometry.contact_tangent.rank_relative_tolerance,
                compatibility_relative_tolerance:self.document.geometry.contact_tangent.compatibility_relative_tolerance,
            })?;
            initial_extension.certificate().cloned()
        };
        let config = assembly.bulk.config();
        let path = assembly
            .geometry
            .assemble_geometry_path_with_closed_witness(
                &assembly.initial_trace,
                &assembly.initial_trace,
                design,
                config.macro_step_s / config.substeps as f64,
                &assembly.boundaries,
                &witness,
            )?;
        let transport = self.document.transport.clone().build(&path)?;
        let initial_density_kg_m3 =
            config.density_kg_m3 + config.initial_pressure_pa / assembly.gas_rt_m2_s2;
        let initial_velocity_m_s = config.initial_velocity_m_s;
        let field = NativeContinuousCoupledField::new(NativeContinuousCoupledFieldConfig {
            bulk: assembly.bulk,
            geometry: assembly.geometry,
            connector: assembly.connector,
            boundaries: assembly.boundaries,
            layout: transport.layout,
            transport_policy: transport.transport,
            continuum_policy: transport.continuum,
            resolved_kinetic_sgs: transport.resolved_kinetic_sgs,
            solve_policy: transport.solve,
            use_selected_kkt_newton: transport.use_selected_kkt_newton,
            gaussian_primal_cache_entries: transport.gaussian_primal_cache_entries,
            initial_trace: assembly.initial_trace,
            closed_contact_law: Some(law),
            initial_closed_witness: Some(witness),
            initial_closed_release_certificate: initial_certificate,
            initial_density_kg_m3,
            initial_velocity_m_s,
            gas_rt_m2_s2: assembly.gas_rt_m2_s2,
            viscosity_pa_s: assembly.viscosity_pa_s,
            reference_pressure_pa: assembly.reference_pressure_pa,
        })?;
        let field = match transport.transport_preconditioner {
            Some(policy) => field.with_transport_preconditioner(policy)?,
            None => field,
        };
        let coupling = &self.model.native_body(0)?.problem.coupling;
        MultirateStepper::new(
            field,
            contact,
            MultirateOptions {
                mode: coupling.mode.clone(),
                schur_ratio_limit: coupling.schur_ratio_limit,
                work_defect_limit: coupling.work_defect_limit,
            },
        )?
        .with_identity(self.identity.clone())
        .with_initial_prediction(coupling.initial_prediction)?
        .with_field_newton(coupling.field_newton)?
        .with_linear_solves(coupling.linear_solves)
        .map(|stepper| stepper.with_step_cache_bytes(coupling.step_cache_bytes))
    }
}
