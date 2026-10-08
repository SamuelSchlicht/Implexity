// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::error::{CaeError, CaeResult};

pub const REQUIRED_MANTISSA_DIGITS: u32 = 53;


pub fn require_double_precision(component: &str) -> CaeResult<()> {
    if f64::MANTISSA_DIGITS == REQUIRED_MANTISSA_DIGITS {
        Ok(())
    } else {
        Err(CaeError::contract(format!(
            "{component} requires IEEE-754 binary64 arithmetic; this build does not provide it"
        )))
    }
}

