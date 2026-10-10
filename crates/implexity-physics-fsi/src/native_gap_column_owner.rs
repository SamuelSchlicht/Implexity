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

use crate::moving_contact::{
    mapped_pair_law::NodeBinding, native_surface_binding::NativeNodalBinding,
};
use implexity_ad::scalar::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use std::collections::{BTreeMap, BTreeSet};

fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).fold(S::zero(), |a,b|a+b)
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn primal<S: Scalar>(a: [S; 3]) -> [f64; 3] {
    a.map(|x| x.value())
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
#[derive(Clone, Copy)]
struct Vertex<S: Scalar> {
    point: [S; 3],
    density: S,
}
type Poly<S> = Vec<Vec<Vertex<S>>>;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NativeGapColumnGrid {
    pub origin_m: [f64; 3],
    pub spacing_m: f64,
    pub shape: [usize; 3],
    pub cells: Vec<usize>,
}
impl NativeGapColumnGrid {
    pub fn cell_bounds(&self, index: usize) -> CaeResult<[[f64; 3]; 2]> {
        let count = self
            .shape
            .iter()
            .try_fold(1usize, |a, &b| a.checked_mul(b))
            .ok_or_else(|| fail("column grid size overflow"))?;
        if index >= count
            || self.shape.contains(&0)
            || !self.spacing_m.is_finite()
            || self.spacing_m <= 0.
            || self.origin_m.iter().any(|v| !v.is_finite())
        {
            return Err(fail("invalid column cell"));
        }
        let ijk = [
            index / (self.shape[1] * self.shape[2]),
            (index / self.shape[2]) % self.shape[1],
            index % self.shape[2],
        ];
        let lo = std::array::from_fn(|a| self.origin_m[a] + ijk[a] as f64 * self.spacing_m);
        let hi = std::array::from_fn(|a| self.origin_m[a] + (ijk[a] + 1) as f64 * self.spacing_m);
        if (0..3).any(|a| !lo[a].is_finite() || !hi[a].is_finite() || hi[a] <= lo[a]) {
            return Err(fail("column cell extent domain"));
        }
        Ok([lo, hi])
    }
}
pub fn native_support_domain(
    binding: &NativeNodalBinding,
    trace: &[f64],
    origin_m: [f64; 3],
    spacing_m: f64,
    shape: [usize; 3],
    support_bounds_m: &[[[f64; 3]; 2]],
    halo_cells: usize,
) -> CaeResult<NativeGapColumnGrid> {
    let check = NativeGapColumnGrid {
        origin_m,
        spacing_m,
        shape,
        cells: vec![0],
    };
    check.cell_bounds(0)?;
    if trace.len() != binding.flux_size() || trace.iter().any(|v| !v.is_finite()) {
        return Err(fail("native support domain trace shape or value"));
    }
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    for node in binding.nodes().iter().flatten() {
        for k in 0..3 {
            let p = node.reference_m[k] + trace[node.force_flux[k]];
            low[k] = low[k].min(p);
            high[k] = high[k].max(p);
        }
    }
    for b in support_bounds_m {
        if (0..3).any(|k| !b[0][k].is_finite() || !b[1][k].is_finite() || b[0][k] > b[1][k]) {
            return Err(fail("invalid native carrier support bound"));
        }
        for k in 0..3 {
            low[k] = low[k].min(b[0][k]);
            high[k] = high[k].max(b[1][k]);
        }
    }
    let halo = halo_cells as f64 * spacing_m;
    if !halo.is_finite() {
        return Err(fail("native support halo overflow"));
    }
    let mut ranges = [[0usize; 2]; 3];
    for k in 0..3 {
        let first = ((low[k] - halo - origin_m[k]) / spacing_m).floor();
        let last = ((high[k] + halo - origin_m[k]) / spacing_m).ceil() - 1.;
        if !first.is_finite() || !last.is_finite() || last < 0. || first >= shape[k] as f64 {
            return Err(fail("native support domain is outside fluid lattice"));
        }
        ranges[k] = [
            first.max(0.) as usize,
            (last.max(0.) as usize).min(shape[k] - 1),
        ];
        if ranges[k][0] > ranges[k][1] {
            return Err(fail("empty native support domain"));
        }
    }
    let count = (ranges[0][1] - ranges[0][0] + 1)
        .checked_mul(ranges[1][1] - ranges[1][0] + 1)
        .and_then(|n| n.checked_mul(ranges[2][1] - ranges[2][0] + 1))
        .ok_or_else(|| fail("native support cell count overflow"))?;
    let mut cells = Vec::with_capacity(count);
    for x in ranges[0][0]..=ranges[0][1] {
        for y in ranges[1][0]..=ranges[1][1] {
            for z in ranges[2][0]..=ranges[2][1] {
                cells.push((x * shape[1] + y) * shape[2] + z);
            }
        }
    }
    Ok(NativeGapColumnGrid {
        origin_m,
        spacing_m,
        shape,
        cells,
    })
}
#[derive(Clone, Debug)]
pub struct NativeGasDomainFace {
    pub cell: usize,
    pub axis: usize,
    pub side: usize,
    pub outside_cell: Option<usize>,
    pub static_wall: bool,
}
#[derive(Clone, Debug)]
pub struct NativeGasSupportDomain {
    pub grid: NativeGapColumnGrid,
    pub exterior_faces: Vec<NativeGasDomainFace>,
    pub carrier_support_cells: Vec<usize>,
}
pub fn native_gas_support_domain(
    binding: &NativeNodalBinding,
    trace: &[f64],
    config: &implexity_physics_lbm::moving::field::MovingLbmConfig,
    cloud: &implexity_physics_lbm::moving::carrier::PointCloud,
    halo_cells: usize,
) -> CaeResult<NativeGasSupportDomain> {
    use implexity_physics_lbm::moving::pushforward::{BinGrid, Frame, Stencils, support_cells};
    if config.periodic.iter().any(|v| *v) {
        return Err(fail(
            "native whole-body support domain needs nonperiodic ownership",
        ));
    }
    let mut mirrored = [[false; 2]; 3];
    for f in &config.symmetry_faces {
        mirrored[f.axis()][usize::from(f.inward() < 0)] = true;
    }
    let frame = Frame::new(
        config.shape,
        config.spacing_m,
        config.origin_m,
        config.periodic,
        config.kernel_width_cells,
    )?
    .with_mirrors(mirrored)?
    .with_kernel(config.interpolation_kernel);
    if config.solid_mask.len() != frame.grid().cells()
        || cloud.positions.len() != cloud.weights.len()
        || cloud.weights.iter().any(|w| !w.is_finite() || *w < 0.)
    {
        return Err(fail("native gas support cloud or static mask shape"));
    }
    let xi: Vec<_> = cloud
        .positions
        .iter()
        .map(|&p| frame.to_lattice(p))
        .collect();
    let stencils = Stencils::new(&frame, &xi, false)?;
    let bins = BinGrid::new(&frame, &stencils);
    let support: Vec<_> = support_cells(&frame, &[&bins])
        .into_iter()
        .map(|c| c as usize)
        .collect();
    let bounds: Vec<_> = support
        .iter()
        .map(|&c| {
            let q = frame.grid().coords(c);
            let lo = std::array::from_fn(|k| config.origin_m[k] + q[k] as f64 * config.spacing_m);
            [lo, lo.map(|v| v + config.spacing_m)]
        })
        .collect();
    let mut grid = native_support_domain(
        binding,
        trace,
        config.origin_m,
        config.spacing_m,
        config.shape,
        &bounds,
        halo_cells,
    )?;
    grid.cells.retain(|&c| !config.solid_mask[c]);
    if grid.cells.is_empty() {
        return Err(fail("native gas support domain has no physical fluid"));
    }
    let owned: BTreeSet<_> = grid.cells.iter().copied().collect();
    if support
        .iter()
        .any(|&c| !config.solid_mask[c] && !owned.contains(&c))
    {
        return Err(fail("native carrier support is not wholly owned"));
    }
    let mut exterior_faces = Vec::new();
    for &cell in &grid.cells {
        let q = frame.grid().coords(cell);
        for axis in 0..3 {
            for side in 0..2 {
                let outside = if side == 0 && q[axis] == 0
                    || side == 1 && q[axis] + 1 == config.shape[axis]
                {
                    None
                } else {
                    let mut next = q;
                    if side == 0 {
                        next[axis] -= 1;
                    } else {
                        next[axis] += 1;
                    }
                    Some(frame.grid().index(next[0], next[1], next[2]))
                };
                if outside.is_some_and(|c| owned.contains(&c)) {
                    continue;
                }
                exterior_faces.push(NativeGasDomainFace {
                    cell,
                    axis,
                    side,
                    outside_cell: outside,
                    static_wall: outside.is_some_and(|c| config.solid_mask[c]),
                });
            }
        }
    }
    Ok(NativeGasSupportDomain {
        grid,
        exterior_faces,
        carrier_support_cells: support,
    })
}
#[derive(Clone)]
pub struct NativeGapColumnOwner {
    pub(crate) nodes: [Vec<NodeBinding>; 2],
    pub(crate) elements: [Vec<[usize; 4]>; 2],
    pub(crate) phase: CsrMatrix,
    pub(crate) trace_size: usize,
    pub(crate) iso: f64,
    pub(crate) tolerance_m: f64,
    pub(crate) grid: NativeGapColumnGrid,
    cells: BTreeMap<usize, usize>,
    pub(crate) bounds: Vec<[[f64; 3]; 2]>,
    reference_blocks: Vec<Vec<(usize, Vec<usize>)>>,
    pub(crate) boundary_faces: [Vec<[usize; 3]>; 2],
}
#[derive(Clone, Debug)]
pub struct NativeGapColumnCapacity<S: Scalar> {
    pub cell: usize,
    pub solid_volumes_m3: [S; 2],
    pub fluid_capacity_m3: S,
    pub certified_zero_air: bool,
    pub roundoff_bound_m3: f64,
}
fn interpolate<S: Scalar>(a: Vertex<S>, b: Vertex<S>, t: S) -> Vertex<S> {
    Vertex {
        point: std::array::from_fn(|i| a.point[i] + (b.point[i] - a.point[i]) * t),
        density: a.density + (b.density - a.density) * t,
    }
}
fn clip<S: Scalar>(
    poly: &Poly<S>,
    value: impl Fn(Vertex<S>) -> S,
    normal: [f64; 3],
    tolerance: f64,
) -> CaeResult<Poly<S>> {
    let roundoff = norm(normal) * tolerance;
    if !roundoff.is_finite() {
        return Err(fail("invalid native column clipping normal"));
    }
    let mut result = Vec::new();
    let mut cap: Vec<Vertex<S>> = Vec::new();
    for face in poly {
        if face.len() < 3 {
            continue;
        }
        let mut out = Vec::new();
        for i in 0..face.len() {
            let a = face[i];
            let b = face[(i + 1) % face.len()];
            let va = value(a);
            let vb = value(b);
            let va = if va.value().abs() <= roundoff {
                va - va.value()
            } else {
                va
            };
            let vb = if vb.value().abs() <= roundoff {
                vb - vb.value()
            } else {
                vb
            };
            if !va.value().is_finite() || !vb.value().is_finite() {
                return Err(fail("nonfinite native column clipping plane"));
            }
            if va.value() >= 0. {
                out.push(a);
            }
            if (va.value() >= 0.) != (vb.value() >= 0.) {
                let x = interpolate(a, b, va / (va - vb));
                out.push(x);
                if cap
                    .iter()
                    .all(|v| norm(sub(primal(v.point), primal(x.point))) > tolerance)
                {
                    cap.push(x);
                }
            }
        }
        if out.len() >= 3 {
            result.push(out);
        }
    }
    if cap.len() >= 3 {
        let mut centre = [0.; 3];
        for v in &cap {
            for i in 0..3 {
                centre[i] += v.point[i].value() / cap.len() as f64;
            }
        }
        let nn = norm(normal);
        if !nn.is_finite() || nn <= 0. {
            return Err(fail("invalid native column clipping normal"));
        }
        let n = normal.map(|v| v / nn);
        let axis = if n[0].abs() < 0.8 {
            [1., 0., 0.]
        } else {
            [0., 1., 0.]
        };
        let a = cross(n, axis);
        let a = a.map(|v| v / norm(a));
        let b = cross(n, a);
        cap.sort_by(|u, v| {
            let up = sub(primal(u.point), centre);
            let vp = sub(primal(v.point), centre);
            dot(up, b)
                .atan2(dot(up, a))
                .total_cmp(&dot(vp, b).atan2(dot(vp, a)))
        });
        result.push(cap);
    }
    Ok(result)
}
fn volume<S: Scalar>(poly: &Poly<S>) -> S {
    let count: usize = poly.iter().map(Vec::len).sum();
    if count == 0 {
        return S::zero();
    }
    let mut centre = [S::zero(); 3];
    for v in poly.iter().flatten() {
        for i in 0..3 {
            centre[i] += v.point[i] / count as f64;
        }
    }
    poly.iter()
        .map(|f| {
            (1..f.len().saturating_sub(1))
                .map(|i| {
                    dot(
                        sub(f[0].point, centre),
                        cross(sub(f[i].point, centre), sub(f[i + 1].point, centre)),
                    )
                    .abs()
                        / 6.
                })
                .fold(S::zero(), |a,b|a+b)
        })
        .fold(S::zero(), |a,b|a+b)
}
fn tetra<S: Scalar>(v: [Vertex<S>; 4]) -> Poly<S> {
    [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
        .map(|f| f.map(|i| v[i]).to_vec())
        .to_vec()
}
type Plane<S> = ([S; 3], [S; 3]);
fn tetra_planes<S: Scalar>(v: [Vertex<S>; 4]) -> Vec<Plane<S>> {
    [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]]
        .into_iter()
        .map(|f| {
            let mut n = cross(
                sub(v[f[1]].point, v[f[0]].point),
                sub(v[f[2]].point, v[f[0]].point),
            );
            if dot(n, sub(v[f[3]].point, v[f[0]].point)).value() < 0. {
                n = n.map(|x| -x);
            }
            (n, v[f[0]].point)
        })
        .collect()
}
fn intersection<S: Scalar>(mut a: Poly<S>, planes: &[Plane<S>], tol: f64) -> CaeResult<Poly<S>> {
    for &(normal, origin) in planes {
        a = clip(
            &a,
            |v| dot(normal, sub(v.point, origin)),
            primal(normal),
            tol,
        )?;
        if a.is_empty() {
            break;
        }
    }
    Ok(a)
}
fn solid_volume<S: Scalar>(
    v: [Vertex<S>; 4],
    iso: f64,
    b: [[f64; 3]; 2],
    tol: f64,
) -> CaeResult<S> {
    if v.iter().all(|p| p.density.value() < iso) {
        return Ok(S::zero());
    }
    let mut poly = tetra(v);
    if v.iter().any(|p| p.density.value() < iso) {
        let edges = [
            sub(v[1].point, v[0].point),
            sub(v[2].point, v[0].point),
            sub(v[3].point, v[0].point),
        ];
        let d = [
            v[1].density - v[0].density,
            v[2].density - v[0].density,
            v[3].density - v[0].density,
        ];
        let c = [
            cross(edges[1], edges[2]),
            cross(edges[2], edges[0]),
            cross(edges[0], edges[1]),
        ];
        let det = dot(edges[0], cross(edges[1], edges[2]));
        let normal = std::array::from_fn(|a| {
            ((d[0] * c[0][a] + d[1] * c[1][a] + d[2] * c[2][a]) / det).value()
        });
        poly = clip(&poly, |p| p.density - iso, normal, tol)?;
    }
    for a in 0..3 {
        let mut normal = [0.; 3];
        normal[a] = 1.;
        poly = clip(&poly, |p| p.point[a] - b[0][a], normal, tol)?;
        normal[a] = -1.;
        poly = clip(&poly, |p| S::from_f64(b[1][a]) - p.point[a], normal, tol)?;
    }
    Ok(volume(&poly))
}
impl NativeGapColumnOwner {
    pub fn from_native(
        binding: &NativeNodalBinding,
        grid: NativeGapColumnGrid,
        iso: f64,
        tolerance_m: f64,
    ) -> CaeResult<Self> {
        let mut owner = Self::from_parts(
            binding.nodes().clone(),
            std::array::from_fn(|b| binding.meshes()[b].mesh.elements.clone()),
            binding.phase().clone(),
            binding.flux_size(),
            grid,
            iso,
            tolerance_m,
        )?;
        for body in 0..2 {
            let mesh = &binding.meshes()[body];
            let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            for (e, &voxel) in mesh.owner.iter().enumerate() {
                groups.entry(voxel).or_default().push(e);
            }
            for group in groups.values() {
                if group.len() != 6 {
                    continue;
                }
                let nodes: std::collections::BTreeSet<_> =
                    group.iter().flat_map(|&e| mesh.mesh.elements[e]).collect();
                if nodes.len() != 8 {
                    continue;
                }
                let ids: Vec<_> = nodes.into_iter().collect();
                let low: [f64; 3] = std::array::from_fn(|a| {
                    ids.iter()
                        .map(|&i| owner.nodes[body][i].reference_m[a])
                        .fold(f64::INFINITY, f64::min)
                });
                let high: [f64; 3] = std::array::from_fn(|a| {
                    ids.iter()
                        .map(|&i| owner.nodes[body][i].reference_m[a])
                        .fold(f64::NEG_INFINITY, f64::max)
                });
                let coordinates: [f64; 3] = std::array::from_fn(|a| {
                    ((low[a] - owner.grid.origin_m[a]) / owner.grid.spacing_m).round()
                });
                if coordinates
                    .iter()
                    .enumerate()
                    .any(|(a, &v)| v < 0. || v >= owner.grid.shape[a] as f64)
                {
                    continue;
                }
                let ijk = coordinates.map(|v| v as usize);
                let cell = (ijk[0] * owner.grid.shape[1] + ijk[1]) * owner.grid.shape[2] + ijk[2];
                if let Some(&slot) = owner.cells.get(&cell) {
                    let b = &owner.bounds[slot];
                    let scale = b
                        .iter()
                        .flatten()
                        .map(|v| v.abs())
                        .fold(owner.grid.spacing_m, f64::max);
                    let roundoff = 16. * f64::EPSILON * scale;
                    if (0..3).all(|a| {
                        (low[a] - b[0][a]).abs() <= roundoff
                            && (high[a] - b[1][a]).abs() <= roundoff
                    }) {
                        owner.reference_blocks[slot].push((body, ids.clone()));
                    }
                }
            }
        }
        Ok(owner)
    }
    pub fn from_parts(
        nodes: [Vec<NodeBinding>; 2],
        elements: [Vec<[usize; 4]>; 2],
        phase: CsrMatrix,
        trace_size: usize,
        grid: NativeGapColumnGrid,
        iso: f64,
        tolerance_m: f64,
    ) -> CaeResult<Self> {
        if !iso.is_finite()
            || iso <= 0.
            || iso >= 1.
            || !tolerance_m.is_finite()
            || tolerance_m <= 0.
            || trace_size == 0
            || phase.nrows() != nodes[0].len() + nodes[1].len()
            || grid.cells.is_empty()
            || nodes.iter().flatten().any(|n| {
                n.reference_m.iter().any(|v| !v.is_finite())
                    || n.force_flux.iter().any(|&i| i >= trace_size)
            })
            || (0..2).any(|b| {
                elements[b]
                    .iter()
                    .any(|e| e.iter().any(|&i| i >= nodes[b].len()))
            })
        {
            return Err(fail("invalid native column ownership policy"));
        }
        let mut cells = BTreeMap::new();
        let mut bounds = Vec::new();
        for (slot, &index) in grid.cells.iter().enumerate() {
            if cells.insert(index, slot).is_some() {
                return Err(fail("duplicate native column cell"));
            }
            bounds.push(grid.cell_bounds(index)?);
        }
        let geometry_scale = bounds
            .iter()
            .flatten()
            .flatten()
            .map(|x| x.abs())
            .fold(grid.spacing_m, f64::max);
        if tolerance_m > 1024. * f64::EPSILON * geometry_scale {
            return Err(fail(
                "native column clipping tolerance exceeds arithmetic precision",
            ));
        }
        let boundary_faces = std::array::from_fn(|body| {
            let mut faces: BTreeMap<[usize; 3], usize> = BTreeMap::new();
            for e in &elements[body] {
                for f in [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]] {
                    let mut ids = f.map(|i| e[i]);
                    ids.sort();
                    *faces.entry(ids).or_default() += 1;
                }
            }
            faces
                .into_iter()
                .filter_map(|(f, count)| (count == 1).then_some(f))
                .collect()
        });
        Ok(Self {
            nodes,
            elements,
            phase,
            trace_size,
            iso,
            tolerance_m,
            grid,
            cells,
            reference_blocks: vec![Vec::new(); bounds.len()],
            boundary_faces,
            bounds,
        })
    }
    pub(crate) fn restricted_cells(&self, selected: &[usize]) -> CaeResult<Self> {
        if selected.is_empty() || selected.windows(2).any(|w| w[0] >= w[1]) {
            return Err(fail("invalid native capacity cell restriction"));
        }
        let mut cells = BTreeMap::new();
        let mut bounds = Vec::with_capacity(selected.len());
        let mut reference_blocks = Vec::with_capacity(selected.len());
        for (new, &cell) in selected.iter().enumerate() {
            let old = *self
                .cells
                .get(&cell)
                .ok_or_else(|| fail("native capacity restriction leaves owner"))?;
            cells.insert(cell, new);
            bounds.push(self.bounds[old]);
            reference_blocks.push(self.reference_blocks[old].clone());
        }
        let mut grid = self.grid.clone();
        grid.cells = selected.to_vec();
        Ok(Self {
            nodes: self.nodes.clone(),
            elements: self.elements.clone(),
            phase: self.phase.clone(),
            trace_size: self.trace_size,
            iso: self.iso,
            tolerance_m: self.tolerance_m,
            grid,
            cells,
            bounds,
            reference_blocks,
            boundary_faces: self.boundary_faces.clone(),
        })
    }
    pub fn grid(&self) -> &NativeGapColumnGrid {
        &self.grid
    }
    pub fn trace_size(&self) -> usize {
        self.trace_size
    }
    pub fn design_size(&self) -> usize {
        self.phase.ncols()
    }
    pub(crate) fn point_candidate_slots<S: Scalar>(
        &self,
        points: [[S; 3]; 4],
    ) -> CaeResult<Vec<usize>> {
        self.candidate_slots(points.map(|point| Vertex {
            point,
            density: S::zero(),
        }))
    }
    fn candidate_slots<S: Scalar>(&self, v: [Vertex<S>; 4]) -> CaeResult<Vec<usize>> {
        let mut ranges = [[0usize; 2]; 3];
        for a in 0..3 {
            let low = v
                .iter()
                .map(|p| p.point[a].value())
                .fold(f64::INFINITY, f64::min);
            let high = v
                .iter()
                .map(|p| p.point[a].value())
                .fold(f64::NEG_INFINITY, f64::max);
            if !low.is_finite() || !high.is_finite() {
                return Err(fail("nonfinite native column position"));
            }
            let first = ((low - self.grid.origin_m[a]) / self.grid.spacing_m).floor();
            let last = ((high - self.grid.origin_m[a]) / self.grid.spacing_m).floor();
            if last < 0. || first >= self.grid.shape[a] as f64 {
                return Ok(Vec::new());
            }
            ranges[a] = [
                first.max(0.) as usize,
                (last.max(0.) as usize).min(self.grid.shape[a] - 1),
            ];
        }
        let mut out = Vec::new();
        for x in ranges[0][0]..=ranges[0][1] {
            for y in ranges[1][0]..=ranges[1][1] {
                for z in ranges[2][0]..=ranges[2][1] {
                    if let Some(&slot) = self
                        .cells
                        .get(&((x * self.grid.shape[1] + y) * self.grid.shape[2] + z))
                    {
                        out.push(slot);
                    }
                }
            }
        }
        Ok(out)
    }
    fn nonsmooth_box_trace_columns(
        &self,
        trace: &[f64],
        design: &[f64],
        active: &[f64],
    ) -> CaeResult<std::collections::BTreeSet<usize>> {
        let mut rho = vec![0.; self.phase.nrows()];
        for (i, r) in rho.iter_mut().enumerate() {
            let (c, w) = self.phase.row(i);
            for (&j, &a) in c.iter().zip(w) {
                *r += design[j] * a;
            }
        }
        let mut columns = std::collections::BTreeSet::new();
        for body in 0..2 {
            let offset = if body == 0 { 0 } else { self.nodes[0].len() };
            for f in &self.boundary_faces[body] {
                let v = f.map(|i| Vertex {
                    point: std::array::from_fn(|a| {
                        self.nodes[body][i].reference_m[a]
                            + trace[self.nodes[body][i].force_flux[a]]
                    }),
                    density: rho[offset + i],
                });
                if v.iter().all(|p| p.density < self.iso) {
                    continue;
                }
                let mut polygon = vec![v.to_vec()];
                if v.iter().any(|p| p.density < self.iso) {
                    polygon = clip(
                        &polygon,
                        |p| p.density - self.iso,
                        [1., 0., 0.],
                        self.tolerance_m,
                    )?;
                }
                for slot in self.candidate_slots([v[0], v[1], v[2], v[2]])? {
                    if active[slot] == 0. {
                        continue;
                    }
                    let b = self.bounds[slot];
                    for axis in 0..3 {
                        for side in 0..2 {
                            let boundary = b[side][axis];
                            let scale = b
                                .iter()
                                .flatten()
                                .map(|x| x.abs())
                                .fold(self.grid.spacing_m, f64::max);
                            if !v.iter().all(|p| {
                                (p.point[axis] - boundary).abs() <= 16. * f64::EPSILON * scale
                            }) {
                                continue;
                            }
                            let mut clipped = polygon.clone();
                            for a in 0..3 {
                                if a == axis {
                                    continue;
                                }
                                let mut n = [0.; 3];
                                n[a] = 1.;
                                clipped =
                                    clip(&clipped, |p| p.point[a] - b[0][a], n, self.tolerance_m)?;
                                n[a] = -1.;
                                clipped =
                                    clip(&clipped, |p| b[1][a] - p.point[a], n, self.tolerance_m)?;
                            }
                            let area: f64 = clipped
                                .iter()
                                .map(|face| {
                                    (1..face.len().saturating_sub(1))
                                        .map(|i| {
                                            norm(cross(
                                                sub(face[i].point, face[0].point),
                                                sub(face[i + 1].point, face[0].point),
                                            )) * 0.5
                                        })
                                        .sum::<f64>()
                                })
                                .fold(0., |a,b|a+b);
                            if area > 0. {
                                for &i in f {
                                    columns.insert(self.nodes[body][i].force_flux[axis]);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(columns)
    }
    fn iso_face_rows(
        &self,
        trace: &[f64],
        design: &[f64],
        active: &[f64],
    ) -> CaeResult<std::collections::BTreeSet<usize>> {
        let mut rho = vec![0.; self.phase.nrows()];
        for (i, r) in rho.iter_mut().enumerate() {
            let (c, w) = self.phase.row(i);
            for (&j, &a) in c.iter().zip(w) {
                *r += design[j] * a;
            }
        }
        let mut rows = std::collections::BTreeSet::new();
        for body in 0..2 {
            let offset = if body == 0 { 0 } else { self.nodes[0].len() };
            for e in &self.elements[body] {
                let tied: Vec<_> = e
                    .iter()
                    .copied()
                    .filter(|&i| rho[offset + i] == self.iso)
                    .collect();
                if tied.len() < 3 {
                    continue;
                }
                let v = e.map(|i| Vertex {
                    point: std::array::from_fn(|a| {
                        self.nodes[body][i].reference_m[a]
                            + trace[self.nodes[body][i].force_flux[a]]
                    }),
                    density: rho[offset + i],
                });
                for slot in self.candidate_slots(v)? {
                    if active[slot] == 0. {
                        continue;
                    }
                    let b = self.bounds[slot];
                    let mut polygon = vec![
                        tied.iter()
                            .map(|&i| Vertex {
                                point: std::array::from_fn(|a| {
                                    self.nodes[body][i].reference_m[a]
                                        + trace[self.nodes[body][i].force_flux[a]]
                                }),
                                density: rho[offset + i],
                            })
                            .collect(),
                    ];
                    for a in 0..3 {
                        let mut n = [0.; 3];
                        n[a] = 1.;
                        polygon = clip(&polygon, |p| p.point[a] - b[0][a], n, self.tolerance_m)?;
                        n[a] = -1.;
                        polygon = clip(&polygon, |p| b[1][a] - p.point[a], n, self.tolerance_m)?;
                    }
                    let area: f64 = polygon
                        .iter()
                        .map(|f| {
                            (1..f.len().saturating_sub(1))
                                .map(|i| {
                                    norm(cross(
                                        sub(f[i].point, f[0].point),
                                        sub(f[i + 1].point, f[0].point),
                                    )) * 0.5
                                })
                                .sum::<f64>()
                        })
                        .fold(0., |a,b|a+b);
                    if area > 0. {
                        for &i in &tied {
                            rows.insert(offset + i);
                        }
                    }
                }
            }
        }
        Ok(rows)
    }
    pub fn iso_face_design_columns(
        &self,
        trace: &[f64],
        design: &[f64],
        active: &[f64],
    ) -> CaeResult<Vec<usize>> {
        if active.len() != self.bounds.len() || active.iter().any(|x| !x.is_finite()) {
            return Err(fail("invalid native phase activity"));
        }
        self.evaluate(trace, design)?;
        let mut columns = std::collections::BTreeSet::new();
        for row in self.iso_face_rows(trace, design, active)? {
            let (c, w) = self.phase.row(row);
            for (&j, &a) in c.iter().zip(w) {
                if a != 0. {
                    columns.insert(j);
                }
            }
        }
        Ok(columns.into_iter().collect())
    }
    pub fn coincident_face_trace_columns(
        &self,
        trace: &[f64],
        design: &[f64],
        active: &[f64],
    ) -> CaeResult<Vec<usize>> {
        if active.len() != self.bounds.len() || active.iter().any(|x| !x.is_finite()) {
            return Err(fail("invalid native face activity"));
        }
        self.evaluate(trace, design)?;
        Ok(self
            .nonsmooth_box_trace_columns(trace, design, active)?
            .into_iter()
            .collect())
    }
    pub fn capacity_jvp(
        &self,
        trace: &[f64],
        design: &[f64],
        trace_direction: &[f64],
        design_direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        if trace_direction.len() != trace.len()
            || design_direction.len() != design.len()
            || trace_direction
                .iter()
                .chain(design_direction)
                .any(|x| !x.is_finite())
        {
            return Err(fail("invalid native capacity tangent"));
        }
        let td: Vec<_> = trace
            .iter()
            .zip(trace_direction)
            .map(|(&x, &dx)| implexity_ad::Dual::<1>::new(x, [dx]))
            .collect();
        let dd: Vec<_> = design
            .iter()
            .zip(design_direction)
            .map(|(&x, &dx)| implexity_ad::Dual::<1>::new(x, [dx]))
            .collect();
        let result = self.evaluate(&td, &dd)?;
        let nonsmooth =
            self.nonsmooth_box_trace_columns(trace, design, &vec![1.; self.bounds.len()])?;
        for row in self.iso_face_rows(trace, design, &vec![1.; self.bounds.len()])? {
            let (c, w) = self.phase.row(row);
            let dr = c
                .iter()
                .zip(w)
                .map(|(&j, &a)| design_direction[j] * a)
                .sum::<f64>();
            if dr != 0. {
                return Err(fail(
                    "native capacity tangent crosses an isosurface face tie",
                ));
            }
        }
        if nonsmooth.iter().any(|&i| trace_direction[i] != 0.) {
            return Err(fail(
                "native capacity tangent crosses a coincident external face",
            ));
        }
        if result.iter().any(|c| {
            !c.fluid_capacity_m3.eps[0].is_finite()
                || (c.certified_zero_air && c.fluid_capacity_m3.eps[0] != 0.)
        }) {
            return Err(fail("native capacity tangent crosses a closed stratum"));
        }
        Ok(result
            .into_iter()
            .map(|c| c.fluid_capacity_m3.eps[0])
            .collect())
    }
    pub fn capacity_vjp(
        &self,
        trace: &[f64],
        design: &[f64],
        cotangent: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.capacity_vjp_on_trace_subspace(
            trace,
            design,
            cotangent,
            &(0..self.trace_size).collect::<Vec<_>>(),
        )
    }
    pub fn capacity_vjp_on_trace_subspace(
        &self,
        trace: &[f64],
        design: &[f64],
        cotangent: &[f64],
        free_trace_columns: &[usize],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.capacity_vjp_on_subspace(
            trace,
            design,
            cotangent,
            free_trace_columns,
            &(0..self.phase.ncols()).collect::<Vec<_>>(),
        )
    }
    pub fn capacity_vjp_on_subspace(
        &self,
        trace: &[f64],
        design: &[f64],
        cotangent: &[f64],
        free_trace_columns: &[usize],
        free_design_columns: &[usize],
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        if cotangent.len() != self.bounds.len() || cotangent.iter().any(|x| !x.is_finite()) {
            return Err(fail("invalid native capacity cotangent"));
        }
        let current = self.evaluate(trace, design)?;
        if current
            .iter()
            .zip(cotangent)
            .any(|(c, &bar)| c.certified_zero_air && bar != 0.)
        {
            return Err(fail("native capacity cotangent requires an open stratum"));
        }
        let free: std::collections::BTreeSet<_> = free_trace_columns.iter().copied().collect();
        if free.len() != free_trace_columns.len() || free.iter().any(|&i| i >= self.trace_size) {
            return Err(fail("invalid native capacity trace subspace"));
        }
        let free_design: std::collections::BTreeSet<_> =
            free_design_columns.iter().copied().collect();
        if free_design.len() != free_design_columns.len()
            || free_design.iter().any(|&i| i >= self.phase.ncols())
        {
            return Err(fail("invalid native capacity design subspace"));
        }
        for row in self.iso_face_rows(trace, design, cotangent)? {
            let (c, w) = self.phase.row(row);
            if c.iter()
                .zip(w)
                .any(|(&j, &a)| a != 0. && free_design.contains(&j))
            {
                return Err(fail(
                    "native capacity reverse crosses an isosurface face tie",
                ));
            }
        }
        let nonsmooth = self.nonsmooth_box_trace_columns(trace, design, cotangent)?;
        if nonsmooth.iter().any(|i| free.contains(i)) {
            return Err(fail(
                "native capacity reverse crosses a coincident external face",
            ));
        }
        let mut rho = vec![0.; self.phase.nrows()];
        for (i, r) in rho.iter_mut().enumerate() {
            let (c, w) = self.phase.row(i);
            for (&j, &a) in c.iter().zip(w) {
                *r += design[j] * a;
            }
        }
        let mut trace_bar = vec![0.; self.trace_size];
        let mut density_bar = vec![0.; self.phase.nrows()];
        for body in 0..2 {
            let offset = if body == 0 { 0 } else { self.nodes[0].len() };
            for e in &self.elements[body] {
                let v = e.map(|i| Vertex {
                    point: std::array::from_fn(|a| {
                        self.nodes[body][i].reference_m[a]
                            + trace[self.nodes[body][i].force_flux[a]]
                    }),
                    density: rho[offset + i],
                });
                let slots = self.candidate_slots(v)?;
                if slots.iter().all(|&slot| cotangent[slot] == 0.)
                    || v.iter().all(|p| p.density < self.iso)
                {
                    continue;
                }
                let local: [Vertex<implexity_ad::Dual<16>>; 4] = std::array::from_fn(|i| Vertex {
                    point: std::array::from_fn(|a| {
                        implexity_ad::Dual::variable(v[i].point[a], 3 * i + a)
                    }),
                    density: implexity_ad::Dual::variable(v[i].density, 12 + i),
                });
                let mut weighted = implexity_ad::Dual::<16>::constant(0.);
                for slot in slots {
                    if cotangent[slot] != 0. {
                        weighted +=
                            solid_volume(local, self.iso, self.bounds[slot], self.tolerance_m)?
                                * (-cotangent[slot]);
                    }
                }
                if weighted.eps.iter().any(|x| !x.is_finite()) {
                    return Err(fail("nonfinite native capacity reverse derivative"));
                }
                for i in 0..4 {
                    for a in 0..3 {
                        trace_bar[self.nodes[body][e[i]].force_flux[a]] += weighted.eps[3 * i + a];
                    }
                    density_bar[offset + e[i]] += weighted.eps[12 + i];
                }
            }
        }
        let mut design_bar = vec![0.; self.phase.ncols()];
        for (i, &bar) in density_bar.iter().enumerate() {
            if bar != 0. {
                let (c, w) = self.phase.row(i);
                for (&j, &a) in c.iter().zip(w) {
                    design_bar[j] += bar * a;
                }
            }
        }
        for (i, bar) in trace_bar.iter_mut().enumerate() {
            if !free.contains(&i) {
                *bar = 0.;
            }
        }
        for (i, bar) in design_bar.iter_mut().enumerate() {
            if !free_design.contains(&i) {
                *bar = 0.;
            }
        }
        Ok((trace_bar, design_bar))
    }
    pub fn evaluate<S: Scalar>(
        &self,
        trace: &[S],
        design: &[S],
    ) -> CaeResult<Vec<NativeGapColumnCapacity<S>>> {
        if trace.len() != self.trace_size
            || design.len() != self.phase.ncols()
            || trace.iter().chain(design).any(|v| !v.value().is_finite())
        {
            return Err(fail("invalid native column geometry input"));
        }
        let mut density = vec![S::zero(); self.phase.nrows()];
        for (i, v) in density.iter_mut().enumerate() {
            let (c, w) = self.phase.row(i);
            for (&j, &a) in c.iter().zip(w) {
                *v += design[j] * a;
            }
            if !v.value().is_finite() || !(0. ..=1.).contains(&v.value()) {
                return Err(fail("native column density domain"));
            }
        }
        let positions: [Vec<[S; 3]>; 2] = std::array::from_fn(|b| {
            self.nodes[b]
                .iter()
                .map(|n| std::array::from_fn(|a| trace[n.force_flux[a]] + n.reference_m[a]))
                .collect()
        });
        let mut pieces: Vec<Vec<(usize, Poly<S>, Vec<Plane<S>>, S)>> =
            vec![Vec::new(); self.bounds.len()];
        for body in 0..2 {
            let offset = if body == 0 { 0 } else { self.nodes[0].len() };
            for e in &self.elements[body] {
                let v = e.map(|i| Vertex {
                    point: positions[body][i],
                    density: density[offset + i],
                });
                let mut ranges = [[0usize; 2]; 3];
                let mut outside = false;
                for a in 0..3 {
                    let lo = v
                        .iter()
                        .map(|p| p.point[a].value())
                        .fold(f64::INFINITY, f64::min);
                    let hi = v
                        .iter()
                        .map(|p| p.point[a].value())
                        .fold(f64::NEG_INFINITY, f64::max);
                    if !lo.is_finite() || !hi.is_finite() {
                        return Err(fail("nonfinite native column position"));
                    }
                    let first = ((lo - self.grid.origin_m[a]) / self.grid.spacing_m).floor();
                    let last = ((hi - self.grid.origin_m[a]) / self.grid.spacing_m).floor();
                    if last < 0. || first >= self.grid.shape[a] as f64 {
                        outside = true;
                        break;
                    }
                    ranges[a] = [
                        first.max(0.) as usize,
                        (last.max(0.) as usize).min(self.grid.shape[a] - 1),
                    ];
                }
                if outside {
                    continue;
                }
                let det = dot(
                    sub(v[1].point, v[0].point),
                    cross(sub(v[2].point, v[0].point), sub(v[3].point, v[0].point)),
                )
                .value();
                let r = e.map(|i| self.nodes[body][i].reference_m);
                let rd = dot(sub(r[1], r[0]), cross(sub(r[2], r[0]), sub(r[3], r[0])));
                if !det.is_finite()
                    || !rd.is_finite()
                    || det == 0.
                    || rd == 0.
                    || det.is_sign_positive() != rd.is_sign_positive()
                {
                    return Err(fail("inverted native column tetrahedron"));
                }
                let density_normal = {
                    let edges = [
                        sub(v[1].point, v[0].point),
                        sub(v[2].point, v[0].point),
                        sub(v[3].point, v[0].point),
                    ];
                    let d = [
                        v[1].density - v[0].density,
                        v[2].density - v[0].density,
                        v[3].density - v[0].density,
                    ];
                    let c = [
                        cross(edges[1], edges[2]),
                        cross(edges[2], edges[0]),
                        cross(edges[0], edges[1]),
                    ];
                    std::array::from_fn(|a| {
                        (d[0] * c[0][a] + d[1] * c[1][a] + d[2] * c[2][a])
                            / dot(edges[0], cross(edges[1], edges[2]))
                    })
                };
                let domain = tetra(v);
                let mut domain_planes = tetra_planes(v);
                let solid = if v.iter().all(|p| p.density.value() >= self.iso) {
                    domain.clone()
                } else if v.iter().all(|p| p.density.value() < self.iso) {
                    Vec::new()
                } else {
                    clip(
                        &domain,
                        |p| p.density - self.iso,
                        primal(density_normal),
                        self.tolerance_m,
                    )?
                };
                if v.iter().all(|p| p.density.value() < self.iso) {
                    continue;
                }
                if v.iter().all(|p| p.density.value() == self.iso) {
                    return Err(fail(
                        "native material has a full-dimensional isosurface tie",
                    ));
                }
                if v.iter().any(|p| p.density.value() < self.iso) {
                    let norm2 = dot(density_normal, density_normal);
                    if !norm2.value().is_finite() || norm2.value() <= 0. {
                        return Err(fail("invalid native density plane"));
                    }
                    let origin = std::array::from_fn(|a| {
                        v[0].point[a]
                            + density_normal[a] * (S::from_f64(self.iso) - v[0].density) / norm2
                    });
                    domain_planes.push((density_normal, origin));
                }
                for x in ranges[0][0]..=ranges[0][1] {
                    for y in ranges[1][0]..=ranges[1][1] {
                        for z in ranges[2][0]..=ranges[2][1] {
                            let index = (x * self.grid.shape[1] + y) * self.grid.shape[2] + z;
                            let Some(&slot) = self.cells.get(&index) else {
                                continue;
                            };
                            let b = self.bounds[slot];
                            let mut p = solid.clone();

                            for a in 0..3 {
                                let mut n = [0.; 3];
                                n[a] = 1.;
                                p = clip(&p, |v| v.point[a] - b[0][a], n, self.tolerance_m)?;
                                n[a] = -1.;
                                p = clip(
                                    &p,
                                    |v| S::from_f64(b[1][a]) - v.point[a],
                                    n,
                                    self.tolerance_m,
                                )?;
                                if p.is_empty() {
                                    break;
                                }
                            }
                            let vol = volume(&p).value();
                            if !vol.is_finite() {
                                return Err(fail("native clipped volume overflow"));
                            }
                            if vol > 0. {
                                let solid_volume = volume(&p);
                                pieces[slot].push((body, p, domain_planes.clone(), solid_volume));
                            }
                        }
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(pieces.len());
        for (slot, cell_pieces) in pieces.into_iter().enumerate() {
            let b = self.bounds[slot];
            let cell_volume = (0..3).map(|a| b[1][a] - b[0][a]).product::<f64>();
            let coordinate_scale = b
                .iter()
                .flatten()
                .map(|x| x.abs())
                .fold(self.grid.spacing_m, f64::max);
            let bound = 4096.
                * f64::EPSILON
                * coordinate_scale
                * self.grid.spacing_m
                * self.grid.spacing_m
                * (cell_pieces.len() + 1) as f64;
            if !cell_volume.is_finite() || !bound.is_finite() {
                return Err(fail("native column volume domain"));
            }
            let mut solid = [S::zero(); 2];

            let mut overlap = 0.;

            for (i, (body, p, _domain_planes, solid_volume)) in cell_pieces.iter().enumerate() {
                solid[*body] += *solid_volume;

                for (_, _, q_planes, _) in &cell_pieces[..i] {
                    overlap +=
                        volume(&intersection(p.clone(), q_planes, self.tolerance_m)?).value();
                }
            }
            if !overlap.is_finite() || overlap > bound {
                return Err(fail(&format!(
                    "native solid regions overlap in column cell {} volume {overlap:e} bound {bound:e}",
                    self.grid.cells[slot]
                )));
            }
            let raw = S::from_f64(cell_volume) - solid[0] - solid[1];
            let empty = self.reference_blocks[slot].iter().any(|(body, ids)| {
                ids.iter().all(|&i| {
                    let offset = if *body == 0 { 0 } else { self.nodes[0].len() };
                    density[offset + i].value() >= self.iso
                        && self.nodes[*body][i]
                            .force_flux
                            .iter()
                            .all(|&j| trace[j].value() == 0.)
                })
            });
            if !empty && raw.value() <= bound {
                return Err(fail(
                    "native column air capacity is below arithmetic resolution",
                ));
            }
            let capacity = if empty { raw - raw.value() } else { raw };
            let defect = capacity.value() + solid[0].value() + solid[1].value() - cell_volume;
            if !capacity.value().is_finite()
                || capacity.value() < 0.
                || capacity.value() > cell_volume + bound
                || !defect.is_finite()
                || defect.abs() > bound
            {
                return Err(fail(&format!(
                    "native column partition defect cell {} air {:e} solid {:?} defect {defect:e} bound {bound:e}",
                    self.grid.cells[slot],
                    capacity.value(),
                    solid.map(|x| x.value())
                )));
            }
            out.push(NativeGapColumnCapacity {
                cell: self.grid.cells[slot],
                solid_volumes_m3: solid,
                fluid_capacity_m3: capacity,
                certified_zero_air: empty,
                roundoff_bound_m3: bound,
            });
        }
        Ok(out)
    }
}
