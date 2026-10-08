// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::GResult;
use crate::node::Node;

#[derive(Clone, Debug, PartialEq)]
pub struct CellFields {
    pub shape: [usize; 3],
    pub rho: Vec<f64>,
    pub phase_fraction: Vec<f64>,
}

pub type DerivedGrid = (String, [usize; 3], Vec<f64>);

pub trait OccupancySource: Send + Sync {
    fn control_representation(&self) -> &'static str {
        "occupancy"
    }

    fn derived_field_specs(&self) -> Vec<Value>;


    fn render_field_descriptors(&self, node: &Node) -> GResult<Vec<Value>>;


    fn render_derived_fields(
        &self,
        node: &Node,
        points_mm: &[[f64; 3]],
    ) -> GResult<BTreeMap<String, Vec<f64>>>;


    fn render_derived_grids(&self, node: &Node, names: &[String]) -> GResult<Vec<DerivedGrid>>;


    fn occupancy_at(&self, node: &Node, points_mm: &[[f64; 3]]) -> GResult<Vec<f64>> {
        Ok(self.render_derived_fields(node, points_mm)?.remove("occupancy").unwrap_or_default())
    }


    fn phase_at(&self, node: &Node, points_mm: &[[f64; 3]]) -> GResult<Vec<f64>> {
        Ok(self.render_derived_fields(node, points_mm)?.remove("phase_fraction").unwrap_or_default())
    }


    fn analysis_fields(&self, node: &Node) -> GResult<CellFields>;


    fn analysis_vjp(&self, node: &Node, adj_rho: &[f64], adj_phase: &[f64]) -> GResult<Vec<f64>>;
}
