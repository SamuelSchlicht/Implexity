// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::pyobj::PyNum;

use crate::model_errors::{PhysicsError, PhysicsResult};

#[derive(Debug, Clone, PartialEq)]
pub struct LatticeLaw {
    pub law: String,
    pub q: PyNum,
    pub eta: f64,
}

impl LatticeLaw {
    #[must_use]
    pub fn simp(q: PyNum) -> Self {
        Self { law: "simp".into(), q, eta: 0.5 }
    }
}



pub fn lattice_interp<S: Scalar>(rho_lat: S, femc: &LatticeLaw) -> PhysicsResult<S> {
    let qf = femc.q.as_f64();
    match femc.law.as_str() {
        "simp" => Ok(crate::models::pow_num(rho_lat, femc.q)),
        "ramp" => Ok(rho_lat / ((-rho_lat + 1.0) * qf + 1.0)),
        "proj" => {
            let eta = femc.eta;
            let t0 = (qf * eta).tanh();
            Ok((((rho_lat - eta) * qf).tanh() + t0) / (t0 + (qf * (1.0 - eta)).tanh()))
        }
        other => Err(PhysicsError::value(format!(
            "unknown lattice_law {}",
            implexity_core::py_repr::repr_str(other)
        ))),
    }
}
