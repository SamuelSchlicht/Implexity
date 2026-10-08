// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use serde_json::Value;

use crate::errors::PResult;
use crate::euler3d::{
    Problem, WallLayout,
};
use crate::euler3d_ad::AdmittedHistory;

pub static PROVIDER: Euler3DTopologyProvider = Euler3DTopologyProvider::muscl();

pub use crate::euler3d_muscl::{MusclLedger,METHOD,MusclSolidLoads,FORCE_SAMPLING};


#[allow(clippy::type_complexity)]
pub fn reference_rhs(
    q: &[[f64; 5]],
    p: &Problem,
    phi: Option<&[f64]>,
) -> PResult<(Vec<[f64; 5]>, Vec<f64>, [f64; 5], [f64; 5])> {
    crate::euler3d_muscl::reference_rhs(q,p,phi).map_err(Into::into)
}


#[allow(clippy::too_many_lines)]
pub fn checked_history(
    problem: &Value,
    step_s: &Value,
    step_count: &Value,
    budget: &Value,
    fluid_fraction: Option<&[f64]>,
) -> PResult<(Vec<Vec<[f64; 5]>>, MusclLedger)> {
    crate::euler3d_muscl::checked_history(problem,step_s,step_count,budget,fluid_fraction).map_err(Into::into)
}


pub fn admit_history(flow: &Value, timing: &Value, phi: &[f64]) -> PResult<AdmittedHistory> {
    crate::euler3d_muscl::admit_history(flow,timing,phi).map_err(Into::into)
}


pub fn step<S: implexity_ad::Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: Option<&[S]>,
    dt: f64,
) -> PResult<Vec<[S; 5]>> {
    crate::euler3d_muscl::step(q,p,phi,dt).map_err(Into::into)
}


pub fn fixed_history(
    q0: &[[f64; 5]],
    p: &Problem,
    dt: f64,
    count: usize,
    phi: Option<&[f64]>,
) -> PResult<Vec<Vec<[f64; 5]>>> {
    crate::euler3d_muscl::fixed_history(q0,p,dt,count,phi).map_err(Into::into)
}


pub fn history_responses(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    phi: &[f64],
    dt: f64,
) -> PResult<crate::euler3d_ad::HistoryResponses> {
    crate::euler3d_muscl::history_responses(states,p,phi,dt).map_err(Into::into)
}


pub fn conforming_solid_load_history(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    layout: &WallLayout,
    dt: f64,
    solid_nodes: &[[f64; 3]],
) -> PResult<MusclSolidLoads> {
    crate::euler3d_muscl::conforming_solid_load_history(states,p,layout,dt,solid_nodes).map_err(Into::into)
}

use crate::providers::euler3d_ad::Euler3DTopologyProvider;

pub const NAME: &str = "compressible_cartesian_euler3d_muscl_topology";
