// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

pub struct ContactStepForce {
    pub force_n: [f64; 12],
    pub current_jacobian_n_per_m: [[f64; 12]; 12],
    pub previous_jacobian_n_per_m: [[f64; 12]; 12],
    pub area_derivative_n_per_m2: [f64; 12],
    pub previous_energy_j: f64,
    pub current_energy_j: f64,
    pub work_defect_j: f64,
    pub net_force_n: [f64; 3],
    pub midpoint_torque_nm: [f64; 3],
}

