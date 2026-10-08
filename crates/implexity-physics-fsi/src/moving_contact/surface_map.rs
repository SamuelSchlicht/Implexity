// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError, CaeResult};
fn fail(message: &str) -> CaeError {
    CaeError::contract(message)
}
#[derive(Clone, Debug)]
pub enum SurfaceFeature {
    Vertex(usize),
    FixedTetrahedron { nodes: [usize;4], weights: [f64;4], weight_tolerance:f64 },
    DensityEdge {
        nodes: [usize; 2],
        iso: f64,
        contrast_min: f64,
        interior_margin: f64,
    },
}
#[derive(Clone, Debug)]
pub struct PointMap {
    weights: Vec<(usize, f64)>,
    density_partials: Vec<(usize, Vec<(usize, f64)>)>,
}
impl SurfaceFeature {
    pub fn map(&self, density: &[f64]) -> CaeResult<PointMap> {
        match *self {
            Self::Vertex(node) => {
                if node >= density.len() || !density[node].is_finite() {
                    return Err(fail("invalid surface vertex"));
                }
                Ok(PointMap {
                    weights: vec![(node, 1.)],
                    density_partials: vec![],
                })
            }
            Self::FixedTetrahedron{nodes,weights,weight_tolerance}=>{
                if nodes.iter().any(|i|*i>=density.len()||!density[*i].is_finite())||!weight_tolerance.is_finite()||!(0. ..=32.*f64::EPSILON).contains(&weight_tolerance)||weights.iter().any(|v|!v.is_finite()||*v< -weight_tolerance)||(weights.iter().sum::<f64>()-1.).abs()>weight_tolerance||nodes.iter().enumerate().any(|(i,v)|nodes[..i].contains(v)){return Err(fail("invalid fixed tetrahedron surface trace"));}
                Ok(PointMap{weights:nodes.into_iter().zip(weights).collect(),density_partials:vec![]})
            }
            Self::DensityEdge {
                nodes: [a, b],
                iso,
                contrast_min,
                interior_margin,
            } => {
                if a == b
                    || a >= density.len()
                    || b >= density.len()
                    || ![iso, contrast_min, interior_margin, density[a], density[b]]
                        .iter()
                        .all(|x| x.is_finite())
                    || !(0. < iso && iso < 1.)
                    || contrast_min <= 0.
                    || !(0. < interior_margin && interior_margin < 0.5)
                    || !(0. <= density[a]
                        && density[a] <= 1.
                        && 0. <= density[b]
                        && density[b] <= 1.)
                {
                    return Err(fail("invalid density-edge input"));
                }
                let den = density[b] - density[a];
                if den.abs() <= contrast_min {
                    return Err(fail("density edge contrast refused"));
                }
                let t = (iso - density[a]) / den;
                if !t.is_finite() || t <= interior_margin || t >= 1. - interior_margin {
                    return Err(fail("density edge feature changed"));
                }
                let da = -(1. - t) / den;
                let db = -t / den;
                if ![da, db].iter().all(|x| x.is_finite()) {
                    return Err(fail("density edge derivative overflow"));
                }
                Ok(PointMap {
                    weights: vec![(a, 1. - t), (b, t)],
                    density_partials: vec![
                        (a, vec![(a, -da), (b, da)]),
                        (b, vec![(a, -db), (b, db)]),
                    ],
                })
            }
        }
    }
}
impl PointMap {
    pub fn density_partials(&self) -> &[(usize, Vec<(usize, f64)>)] { &self.density_partials }
    pub fn weights(&self) -> &[(usize, f64)] {
        &self.weights
    }
    pub fn position(&self, nodes: &[[f64; 3]]) -> CaeResult<[f64; 3]> {
        let mut q = [0.; 3];
        for &(node, w) in &self.weights {
            let p = nodes
                .get(node)
                .ok_or_else(|| fail("surface coordinate index"))?;
            for i in 0..3 {
                q[i] += w * p[i];
            }
        }
        if q.iter().any(|x| !x.is_finite()) {
            return Err(fail("surface coordinate overflow"));
        }
        Ok(q)
    }
    pub fn direction(
        &self,
        nodes: &[[f64; 3]],
        d_nodes: &[[f64; 3]],
        d_density: &[f64],
    ) -> CaeResult<[f64; 3]> {
        if nodes.len() != d_nodes.len()
            || nodes.len() != d_density.len()
            || nodes
                .iter()
                .chain(d_nodes)
                .flatten()
                .chain(d_density)
                .any(|x| !x.is_finite())
        {
            return Err(fail("invalid surface direction"));
        }
        let mut dq = self.position(d_nodes)?;
        for (density_node, row) in &self.density_partials {
            for &(node, dw) in row {
                for i in 0..3 {
                    dq[i] += dw * d_density[*density_node] * nodes[node][i];
                }
            }
        }
        if dq.iter().any(|x| !x.is_finite()) {
            return Err(fail("surface direction overflow"));
        }
        Ok(dq)
    }
    pub fn scatter(&self, force: [f64; 3], out: &mut [[f64; 3]]) -> CaeResult<()> {
        if force.iter().any(|x| !x.is_finite()) {
            return Err(fail("nonfinite surface force"));
        }
        for &(node, w) in &self.weights {
            let row = out
                .get_mut(node)
                .ok_or_else(|| fail("surface force index"))?;
            for i in 0..3 {
                row[i] += w * force[i];
            }
        }
        if out.iter().flatten().any(|x| !x.is_finite()) {
            return Err(fail("surface force overflow"));
        }
        Ok(())
    }
    pub fn scatter_direction(
        &self,
        force: [f64; 3],
        d_force: [f64; 3],
        d_density: &[f64],
        out: &mut [[f64; 3]],
    ) -> CaeResult<()> {
        if force.iter().any(|x| !x.is_finite()) {
            return Err(fail("nonfinite surface force"));
        }
        if out.len() != d_density.len() || d_density.iter().any(|x| !x.is_finite()) {
            return Err(fail("invalid force design direction"));
        }
        self.scatter(d_force, out)?;
        for (density_node, row) in &self.density_partials {
            for &(node, dw) in row {
                for i in 0..3 {
                    out[node][i] += dw * d_density[*density_node] * force[i];
                }
            }
        }
        if out.iter().flatten().any(|x| !x.is_finite()) {
            return Err(fail("force design overflow"));
        }
        Ok(())
    }
    pub fn density_pullback(
        &self,
        nodes: &[[f64; 3]],
        cotangent: [f64; 3],
        out: &mut [f64],
    ) -> CaeResult<()> {
        if nodes.len() != out.len() || cotangent.iter().any(|x| !x.is_finite()) {
            return Err(fail("invalid surface density cotangent"));
        }
        self.position(nodes)?;
        for (density_node, row) in &self.density_partials {
            for &(node, dw) in row {
                for i in 0..3 {
                    out[*density_node] += dw * nodes[node][i] * cotangent[i];
                }
            }
        }
        if out.iter().any(|x| !x.is_finite()) {
            return Err(fail("surface density pullback overflow"));
        }
        Ok(())
    }
}
