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



#[cfg(feature = "dynamic-results")]
pub(crate) static FILES: &[(&str, &[u8])] = &[
    ("advanced_commands.css", include_bytes!("../../../viewer/advanced_commands.css")),
    ("advanced_commands.css.license", include_bytes!("../../../viewer/advanced_commands.css.license")),
    ("advanced_commands.js", include_bytes!("../../../viewer/advanced_commands.js")),
    ("advanced_commands.js.license", include_bytes!("../../../viewer/advanced_commands.js.license")),
    ("boundary_authoring.js", include_bytes!("../../../viewer/boundary_authoring.js")),
    ("cfd_workspace.css", include_bytes!("../../../viewer/cfd_workspace.css")),
    ("cfd_workspace.js", include_bytes!("../../../viewer/cfd_workspace.js")),
    ("design_bounds.js", include_bytes!("../../../viewer/design_bounds.js")),
    ("design_freedom.js", include_bytes!("../../../viewer/design_freedom.js")),
    ("editing_workspace.css", include_bytes!("../../../viewer/editing_workspace.css")),
    ("editing_workspace.js", include_bytes!("../../../viewer/editing_workspace.js")),
    ("field_tile_worker.js", include_bytes!("../../../viewer/field_tile_worker.js")),
    ("field_visibility.js", include_bytes!("../../../viewer/field_visibility.js")),
    ("geometry_seed.css", include_bytes!("../../../viewer/geometry_seed.css")),
    ("geometry_seed.js", include_bytes!("../../../viewer/geometry_seed.js")),
    ("gui_text.js", include_bytes!("../../../viewer/gui_text.js")),
    ("guided_setup.css", include_bytes!("../../../viewer/guided_setup.css")),
    ("guided_setup.css.license", include_bytes!("../../../viewer/guided_setup.css.license")),
    ("guided_setup.js", include_bytes!("../../../viewer/guided_setup.js")),
    ("guided_setup.js.license", include_bytes!("../../../viewer/guided_setup.js.license")),
    ("interaction.css", include_bytes!("../../../viewer/interaction.css")),
    ("interaction.js", include_bytes!("../../../viewer/interaction.js")),
    ("managed_evaluation_workspace.js", include_bytes!("../../../viewer/managed_evaluation_workspace.js")),
    ("manual_interaction.js", include_bytes!("../../../viewer/manual_interaction.js")),
    ("model.html", include_bytes!("../../../viewer/model.html")),
    ("numerical_progress.js", include_bytes!("../../../viewer/numerical_progress.js")),
    ("objective_composer.css", include_bytes!("../../../viewer/objective_composer.css")),
    ("objective_composer.js", include_bytes!("../../../viewer/objective_composer.js")),
    ("optimization_workspace.css", include_bytes!("../../../viewer/optimization_workspace.css")),
    ("optimization_workspace.js", include_bytes!("../../../viewer/optimization_workspace.js")),
    ("output_dialogs.css", include_bytes!("../../../viewer/output_dialogs.css")),
    ("output_dialogs.js", include_bytes!("../../../viewer/output_dialogs.js")),
    ("parametric_modeling.js", include_bytes!("../../../viewer/parametric_modeling.js")),
    ("physics_packages.css", include_bytes!("../../../viewer/physics_packages.css")),
    ("physics_packages.js", include_bytes!("../../../viewer/physics_packages.js")),
    ("progressive_field.css", include_bytes!("../../../viewer/progressive_field.css")),
    ("progressive_field.js", include_bytes!("../../../viewer/progressive_field.js")),
    ("provider_problem_persistence.js", include_bytes!("../../../viewer/provider_problem_persistence.js")),
    ("raymarch.js", include_bytes!("../../../viewer/raymarch.js")),
    ("rust_ext/dynamic_results.css", include_bytes!("../../../viewer/rust_ext/dynamic_results.css")),
    ("rust_ext/dynamic_results.html", include_bytes!("../../../viewer/rust_ext/dynamic_results.html")),
    ("rust_ext/dynamic_results.js", include_bytes!("../../../viewer/rust_ext/dynamic_results.js")),
    ("rust_ext/extensions.css", include_bytes!("../../../viewer/rust_ext/extensions.css")),
    ("rust_ext/extensions.js", include_bytes!("../../../viewer/rust_ext/extensions.js")),
    ("rust_ext/native.css", include_bytes!("../../../viewer/rust_ext/native.css")),
    ("sensitivity_authoring.css", include_bytes!("../../../viewer/sensitivity_authoring.css")),
    ("sensitivity_authoring.js", include_bytes!("../../../viewer/sensitivity_authoring.js")),
    ("shared_manual_history.js", include_bytes!("../../../viewer/shared_manual_history.js")),
    ("shared_manual_history.js.license", include_bytes!("../../../viewer/shared_manual_history.js.license")),
    ("study_workspace.js", include_bytes!("../../../viewer/study_workspace.js")),
    ("surface_authoring.css", include_bytes!("../../../viewer/surface_authoring.css")),
    ("surface_authoring.js", include_bytes!("../../../viewer/surface_authoring.js")),
    ("workbench.css", include_bytes!("../../../viewer/workbench.css")),
    ("workbench.js", include_bytes!("../../../viewer/workbench.js")),
];

#[cfg(not(feature = "dynamic-results"))]
pub(crate) static FILES: &[(&str, &[u8])] = &[
    ("advanced_commands.css", include_bytes!("../../../viewer/advanced_commands.css")),
    ("advanced_commands.css.license", include_bytes!("../../../viewer/advanced_commands.css.license")),
    ("advanced_commands.js", include_bytes!("../../../viewer/advanced_commands.js")),
    ("advanced_commands.js.license", include_bytes!("../../../viewer/advanced_commands.js.license")),
    ("boundary_authoring.js", include_bytes!("../../../viewer/boundary_authoring.js")),
    ("cfd_workspace.css", include_bytes!("../../../viewer/cfd_workspace.css")),
    ("cfd_workspace.js", include_bytes!("../../../viewer/cfd_workspace.js")),
    ("design_bounds.js", include_bytes!("../../../viewer/design_bounds.js")),
    ("design_freedom.js", include_bytes!("../../../viewer/design_freedom.js")),
    ("editing_workspace.css", include_bytes!("../../../viewer/editing_workspace.css")),
    ("editing_workspace.js", include_bytes!("../../../viewer/editing_workspace.js")),
    ("field_tile_worker.js", include_bytes!("../../../viewer/field_tile_worker.js")),
    ("field_visibility.js", include_bytes!("../../../viewer/field_visibility.js")),
    ("geometry_seed.css", include_bytes!("../../../viewer/geometry_seed.css")),
    ("geometry_seed.js", include_bytes!("../../../viewer/geometry_seed.js")),
    ("gui_text.js", include_bytes!("../../../viewer/gui_text.js")),
    ("guided_setup.css", include_bytes!("../../../viewer/guided_setup.css")),
    ("guided_setup.css.license", include_bytes!("../../../viewer/guided_setup.css.license")),
    ("guided_setup.js", include_bytes!("../../../viewer/guided_setup.js")),
    ("guided_setup.js.license", include_bytes!("../../../viewer/guided_setup.js.license")),
    ("interaction.css", include_bytes!("../../../viewer/interaction.css")),
    ("interaction.js", include_bytes!("../../../viewer/interaction.js")),
    ("managed_evaluation_workspace.js", include_bytes!("../../../viewer/managed_evaluation_workspace.js")),
    ("manual_interaction.js", include_bytes!("../../../viewer/manual_interaction.js")),
    ("model.html", include_bytes!("../../../viewer/model.html")),
    ("numerical_progress.js", include_bytes!("../../../viewer/numerical_progress.js")),
    ("objective_composer.css", include_bytes!("../../../viewer/objective_composer.css")),
    ("objective_composer.js", include_bytes!("../../../viewer/objective_composer.js")),
    ("optimization_workspace.css", include_bytes!("../../../viewer/optimization_workspace.css")),
    ("optimization_workspace.js", include_bytes!("../../../viewer/optimization_workspace.js")),
    ("output_dialogs.css", include_bytes!("../../../viewer/output_dialogs.css")),
    ("output_dialogs.js", include_bytes!("../../../viewer/output_dialogs.js")),
    ("parametric_modeling.js", include_bytes!("../../../viewer/parametric_modeling.js")),
    ("physics_packages.css", include_bytes!("../../../viewer/physics_packages.css")),
    ("physics_packages.js", include_bytes!("../../../viewer/physics_packages.js")),
    ("progressive_field.css", include_bytes!("../../../viewer/progressive_field.css")),
    ("progressive_field.js", include_bytes!("../../../viewer/progressive_field.js")),
    ("provider_problem_persistence.js", include_bytes!("../../../viewer/provider_problem_persistence.js")),
    ("raymarch.js", include_bytes!("../../../viewer/raymarch.js")),
    ("sensitivity_authoring.css", include_bytes!("../../../viewer/sensitivity_authoring.css")),
    ("sensitivity_authoring.js", include_bytes!("../../../viewer/sensitivity_authoring.js")),
    ("shared_manual_history.js", include_bytes!("../../../viewer/shared_manual_history.js")),
    ("shared_manual_history.js.license", include_bytes!("../../../viewer/shared_manual_history.js.license")),
    ("study_workspace.js", include_bytes!("../../../viewer/study_workspace.js")),
    ("surface_authoring.css", include_bytes!("../../../viewer/surface_authoring.css")),
    ("surface_authoring.js", include_bytes!("../../../viewer/surface_authoring.js")),
    ("workbench.css", include_bytes!("../../../viewer/workbench.css")),
    ("workbench.js", include_bytes!("../../../viewer/workbench.js")),
];
