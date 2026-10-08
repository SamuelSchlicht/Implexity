// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

