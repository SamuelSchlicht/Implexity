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



use implexity_core::error::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use crate::certificate::norm2;
use crate::convergence::{FieldMembers, checked_members};

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return false };
    first.is_ascii_alphabetic()
        && name.len() <= 96
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidualPartition {
    size: usize,
    members: FieldMembers,
}

impl ResidualPartition {


    pub fn new(state_size: usize, members: &[(String, Vec<i64>)]) -> CaeResult<Self> {
        if state_size < 1 {
            return Err(CaeError::contract("residual diagnostic frame size must be a positive integer"));
        }
        if members.is_empty() || members.len() > 32 || members.iter().any(|(k, _)| !valid_name(k)) {
            return Err(CaeError::contract("residual diagnostic names must be bounded identifiers"));
        }
        let checked = checked_members(members, state_size)?;
        let mut all: Vec<usize> = checked.iter().flat_map(|(_, v)| v.iter().copied()).collect();
        all.sort_unstable();
        if all.len() != state_size || all.iter().enumerate().any(|(i, &v)| i != v) {
            return Err(CaeError::contract(
                "residual diagnostic partition must cover every row exactly once",
            ));
        }
        Ok(Self { size: state_size, members: checked })
    }

    #[must_use]
    pub fn state_size(&self) -> usize {
        self.size
    }

    #[must_use]
    pub fn members(&self) -> &FieldMembers {
        &self.members
    }



    pub fn norms(&self, residual: &[f64]) -> CaeResult<Map<String, Value>> {
        if residual.len() != self.size || residual.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "residual diagnostic vector does not match its finite real frame",
            ));
        }
        let mut out = Map::new();
        for (name, ids) in &self.members {
            let sub: Vec<f64> = ids.iter().map(|&i| residual[i]).collect();
            let n = norm2(&sub);
            if !n.is_finite() {
                return Err(CaeError::contract("residual diagnostic norm overflow"));
            }
            out.insert(name.clone(), json!(n));
        }
        Ok(out)
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        let counts: Map<String, Value> =
            self.members.iter().map(|(n, v)| (n.clone(), json!(v.len()))).collect();
        json!({"schema": "implexity-residual-partition/1", "state_size": self.size,
               "diagnostic_only": true, "norm": "L2_of_existing_residual_rows", "row_counts": counts})
    }
}



pub fn checked_partition(value: Option<&ResidualPartition>, state_size: Option<usize>) -> CaeResult<()> {
    if let (Some(p), Some(n)) = (value, state_size)
        && p.state_size() != n
    {
        return Err(CaeError::contract("residual diagnostic partition frame mismatch"));
    }
    Ok(())
}

