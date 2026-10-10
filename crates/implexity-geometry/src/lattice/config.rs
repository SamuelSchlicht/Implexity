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


#[derive(Clone, Debug, PartialEq)]
pub struct LatticeConfig {
    pub period: f64,
    pub t_min: f64,
    pub t_max: f64,
    pub interface_eps: f64,
    pub grad_floor_rel: f64,
    pub soft_eps_rel: f64,
    pub det_floor: f64,
    pub cond_max: f64,
    pub min_elems_per_period: f64,
    pub control_dx: [f64; 3],
    pub s_max: f64,
    pub sec_scale: f64,
    pub sec_ratio: f64,
    pub res_scale: f64,
    pub w_eps: f64,
}

impl Default for LatticeConfig {
    fn default() -> Self {
        Self {
            period: 4.0e-3,
            t_min: 0.2e-3,
            t_max: 1.4e-3,
            interface_eps: 1e-3,
            grad_floor_rel: 0.05,
            soft_eps_rel: 1e-2,
            det_floor: 0.25,
            cond_max: 4.0,
            min_elems_per_period: 8.0,
            control_dx: [1.0e-3, 2.0e-3, 2.0e-3],
            s_max: 0.12,
            sec_scale: 0.5,
            sec_ratio: 2.0,
            res_scale: 0.3,
            w_eps: 1e-6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Channels {
    pub stretch: bool,
    pub family: bool,
    pub mode: bool,
    pub secondary: bool,
    pub residual: bool,
    pub material: bool,
    pub phase: bool,
}

impl Default for Channels {
    fn default() -> Self {
        Self {
            stretch: true,
            family: true,
            mode: true,
            secondary: false,
            residual: false,
            material: true,
            phase: true,
        }
    }
}

impl Channels {
    #[must_use]
    pub fn all() -> Self {
        Self { secondary: true, residual: true, ..Self::default() }
    }

    #[must_use]
    pub fn active(&self) -> Vec<&'static str> {
        let mut names = vec!["a", "m"];
        for (on, name) in [
            (self.phase, "dphi"),
            (self.stretch, "s"),
            (self.family, "w"),
            (self.mode, "nu"),
            (self.secondary, "w2"),
            (self.residual, "res"),
            (self.material, "c"),
        ] {
            if on {
                names.push(name);
            }
        }
        names
    }
}
