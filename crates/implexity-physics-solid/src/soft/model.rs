// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use rayon::prelude::*;

use implexity_ad::{Dual, HyperDual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::lu::{LuSymbolic, Parallelism, SparseLu};
use implexity_linalg::sparse::{CscMatrix, CsrMatrix};

use super::design::Interpolation;
use super::jet::Jet2;
use super::laws::{Kin, Material, Volumetric, fibre_i4};
use crate::hyperelastic::kinematics::{Mat3, TetMesh, det, matmul, transpose};
use crate::util::contract;

pub const MAX_FIBRES: usize = 4;
const CHUNK: usize = 2048;
const GAUSS_A: f64 = 0.585_410_196_624_968_5;
const GAUSS_B: f64 = 0.138_196_601_125_010_5;

pub trait NodalPotential: Send + Sync + core::fmt::Debug {
    fn admissible_step(&self, u: &[f64], direction: &[f64], params: &[f64], trial: f64) -> Result<f64, CaeError> {
        let _ = (u, direction, params);
        Ok(trial)
    }

    fn energy(&self, u: &[f64], params: &[f64]) -> Result<f64, CaeError>;

    fn force(&self, u: &[f64], params: &[f64]) -> Result<Vec<f64>, CaeError>;

    fn tangent(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError>;

    fn force_params_vjp(&self, u: &[f64], params: &[f64], w: &[f64]) -> Result<Vec<f64>, CaeError>;

    fn force_params_jacobian(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError>;

    fn force_second_directional(
        &self,
        u: &[f64],
        params: &[f64],
        w: &[f64],
        du: &[f64],
        dparams: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), CaeError> {
        let _ = (u, params, w, du, dparams);
        Err(CaeError::contract(
            "a nodal potential of this model provides no third derivatives (force_second_directional); \
             second-order adjoints of the step are unavailable",
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Formulation {
    Displacement,
    Mixed {
        stabilization: f64,
    },
}

impl Formulation {
    #[must_use]
    pub fn mixed(self) -> bool {
        matches!(self, Self::Mixed { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ElemDesign<S> {
    pub e: S,
    pub gamma: S,
    pub dirs: [[S; 3]; MAX_FIBRES],
    pub nf: usize,
}

#[derive(Debug, Clone)]
pub struct SoftModel {
    pub mesh: TetMesh,
    pub materials: Vec<Material>,
    pub element_material: Vec<usize>,
    pub formulation: Formulation,
    pub fibres: Vec<Vec<[f64; 3]>>,
    pub fibre_axis: Vec<[f64; 3]>,
    pub interpolation: Interpolation,
    pub fixed: Vec<bool>,
    pub faces: Vec<[usize; 3]>,
    pub lumped_mass: bool,
    pub potentials: Vec<std::sync::Arc<dyn NodalPotential>>,
    pub unknown: Vec<usize>,
    pub n_unknowns: usize,
    pub n_free_u: usize,
    pattern: (Vec<usize>, Vec<usize>),
    contact_node_pairs: Vec<(usize,usize)>,
    symbolic: std::sync::OnceLock<LuSymbolic>,
}

pub fn rotate<S: Scalar>(a: &[f64; 3], n: &[f64; 3], theta: S) -> [S; 3] {
    let (c, s) = (theta.cos(), theta.sin());
    let cross = [n[1] * a[2] - n[2] * a[1], n[2] * a[0] - n[0] * a[2], n[0] * a[1] - n[1] * a[0]];
    let dot = n[0] * a[0] + n[1] * a[1] + n[2] * a[2];
    core::array::from_fn(|i| c * a[i] + s * cross[i] + (-c + 1.0) * (n[i] * dot))
}

fn sym_from6<S: Scalar>(h: &[S; 6]) -> Mat3<S> {
    [[h[0], h[5], h[4]], [h[5], h[1], h[3]], [h[4], h[3], h[2]]]
}

#[derive(Debug, Clone)]
pub struct ElementFull {
    pub energy: f64,
    pub gradient: [f64; 16],
    pub hessian: Box<[f64; 256]>,
    pub design: [[f64; 2]; 16],
}

#[derive(Debug, Clone)]
pub struct ElementLocal {
    pub energy: f64,
    pub gradient: [f64; 16],
    pub hessian: Box<[f64; 256]>,
}

impl SoftModel {

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mesh: TetMesh,
        materials: Vec<Material>,
        element_material: Vec<usize>,
        formulation: Formulation,
        fibres: Vec<Vec<[f64; 3]>>,
        fibre_axis: Vec<[f64; 3]>,
        interpolation: Interpolation,
        fixed: Vec<bool>,
        faces: Vec<[usize; 3]>,
        lumped_mass: bool,
    ) -> Result<Self, CaeError> {
        let n = mesh.node_count();
        let ne = mesh.elements.len();
        if ne == 0 || mesh.volumes.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
            return contract("the mesh requires positively oriented tetrahedra with nonzero volume");
        }
        if materials.is_empty()
            || element_material.len() != ne
            || element_material.iter().any(|m| *m >= materials.len())
        {
            return contract("element_material must assign one listed material to every tetrahedron");
        }
        interpolation.validate()?;
        if let Formulation::Mixed { stabilization } = formulation {
            if !(stabilization.is_finite() && stabilization > 0.0) {
                return contract("mixed formulation stabilization must be finite and positive");
            }
            if materials.iter().any(Material::coupled) {
                return contract(
                    "the mixed u-p formulation requires decoupled laws (not neo_hookean_coupled)",
                );
            }
        } else if materials.iter().any(|m| matches!(m.volumetric, Volumetric::Incompressible)) {
            return contract("exact incompressibility requires the mixed u-p formulation");
        }
        if materials.iter().any(|m| m.coupled() && m.prony.is_some()) {
            return contract("prony viscoelasticity requires a decoupled (isochoric) law");
        }
        if fibres.len() != ne || fibre_axis.len() != ne {
            return contract("fibre directions and axes are required per element");
        }
        for (e, dirs) in fibres.iter().enumerate() {
            let needs = materials[element_material[e]].fibres.is_some();
            if needs != !dirs.is_empty() || dirs.len() > MAX_FIBRES {
                return contract(
                    "fibre-reinforced materials need 1..4 fibre directions per element, other materials none",
                );
            }
            let unit = |a: &[f64; 3]| {
                a.iter().copied().all(f64::is_finite)
                    && ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt() - 1.0).abs() < 1e-9
            };
            if dirs.iter().any(|a| !unit(a)) || (!dirs.is_empty() && !unit(&fibre_axis[e])) {
                return contract("fibre directions and fibre axes must be unit vectors");
            }
        }
        if fixed.len() != 3 * n {
            return contract("fixed_dofs must be a node-by-XYZ Boolean mask");
        }
        if faces.iter().flatten().any(|v| *v >= n) {
            return contract("pressure faces reference unknown nodes");
        }
        let mixed = formulation.mixed();
        let total = 3 * n + if mixed { n } else { 0 };
        let mut unknown = vec![usize::MAX; total];
        let mut k = 0;
        for (i, f) in fixed.iter().enumerate() {
            if !*f {
                unknown[i] = k;
                k += 1;
            }
        }
        let n_free_u = k;
        if mixed {
            for slot in &mut unknown[3 * n..] {
                *slot = k;
                k += 1;
            }
        }
        if k == 0 {
            return contract("the model has no unknowns");
        }
        let mut model = Self {
            mesh,
            materials,
            element_material,
            formulation,
            fibres,
            fibre_axis,
            interpolation,
            fixed,
            faces,
            lumped_mass,
            potentials: Vec::new(),
            unknown,
            n_unknowns: k,
            n_free_u,
            pattern: (Vec::new(), Vec::new()),
            contact_node_pairs: Vec::new(),
            symbolic: std::sync::OnceLock::new(),
        };
        model.pattern = model.build_pattern();
        Ok(model)
    }


    pub fn with_interpolation(mut self, interpolation: Interpolation) -> Result<Self, CaeError> {
        interpolation.validate()?;
        self.interpolation = interpolation;
        Ok(self)
    }

    #[must_use]
    pub fn with_potential(mut self, potential: std::sync::Arc<dyn NodalPotential>) -> Self {
        self.potentials.push(potential);
        self
    }

    pub fn with_potential_couplings(mut self,potential:std::sync::Arc<dyn NodalPotential>,node_pairs:&[(usize,usize)])->Result<Self,CaeError> {
        if node_pairs.iter().any(|(a,b)|*a>=self.n()||*b>=self.n()) {return contract("contact sparsity references unknown nodes");}
        self.contact_node_pairs.extend_from_slice(node_pairs);
        self.contact_node_pairs.sort_unstable();self.contact_node_pairs.dedup();
        self.potentials.push(potential);
        self.pattern=self.build_pattern();self.symbolic=std::sync::OnceLock::new();
        Ok(self)
    }

    pub fn admissible_step(&self, u: &[f64], direction: &[f64], params: &[f64], trial: f64) -> Result<f64, CaeError> {
        if u.len()!=3*self.n() || direction.len()!=u.len() || !trial.is_finite() || trial<=0.0 {
            return contract("invalid potential path");
        }
        let mut limit=trial;
        for potential in &self.potentials {
            let next=potential.admissible_step(u,direction,params,limit)?;
            if !next.is_finite() || next<=0.0 || next>limit {
                return contract("potential returned invalid path limit");
            }
            limit=next;
        }
        Ok(limit)
    }

    #[must_use]
    pub fn n(&self) -> usize {
        self.mesh.node_count()
    }

    #[must_use]
    pub fn ne(&self) -> usize {
        self.mesh.elements.len()
    }

    #[must_use]
    pub fn nl(&self) -> usize {
        if self.formulation.mixed() { 16 } else { 12 }
    }

    #[must_use]
    pub fn element_dofs(&self, e: usize) -> [usize; 16] {
        let t = self.mesh.elements[e];
        let n = self.n();
        core::array::from_fn(|k| if k < 12 { 3 * t[k / 3] + k % 3 } else { 3 * n + t[k - 12] })
    }

    fn build_pattern(&self) -> (Vec<usize>, Vec<usize>) {
        let n = self.n();
        let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut link = |nodes: &[usize]| {
            for &a in nodes {
                for &b in nodes {
                    adjacency[a].push(b);
                }
            }
        };
        for t in &self.mesh.elements {
            link(t);
        }
        for f in &self.faces {
            link(f);
        }
        for &(a,b) in &self.contact_node_pairs {link(&[a,b]);}
        for row in &mut adjacency {
            row.sort_unstable();
            row.dedup();
        }
        let mixed = self.formulation.mixed();
        let node_unknowns = |b: usize, out: &mut Vec<usize>| {
            for i in 0..3 {
                let u = self.unknown[3 * b + i];
                if u != usize::MAX {
                    out.push(u);
                }
            }
            if mixed {
                out.push(self.unknown[3 * n + b]);
            }
        };
        let mut columns: Vec<Vec<usize>> = vec![Vec::new(); self.n_unknowns];
        for a in 0..n {
            let mut rows = Vec::new();
            for &b in &adjacency[a] {
                node_unknowns(b, &mut rows);
            }
            rows.sort_unstable();
            let mut own = Vec::new();
            node_unknowns(a, &mut own);
            for c in own {
                columns[c].clone_from(&rows);
            }
        }
        let mut indptr = Vec::with_capacity(self.n_unknowns + 1);
        indptr.push(0);
        let mut indices = Vec::new();
        for col in columns {
            indices.extend(col);
            indptr.push(indices.len());
        }
        (indptr, indices)
    }


    pub fn zero_matrix(&self) -> Result<CscMatrix, CaeError> {
        let (p, i) = &self.pattern;
        CscMatrix::try_new(self.n_unknowns, self.n_unknowns, p.clone(), i.clone(), vec![0.0; i.len()])
            .map_err(|e| CaeError::contract(format!("soft model pattern: {e}")))
    }

    fn slot(&self, row: usize, col: usize) -> Option<usize> {
        let (p, i) = &self.pattern;
        let range = p[col]..p[col + 1];
        i[range.clone()].binary_search(&row).ok().map(|k| range.start + k)
    }


    pub fn factor(&self, a: &CscMatrix) -> Result<SparseLu, CaeError> {
        let symbolic = if let Some(s) = self.symbolic.get() {
            s
        } else {
            let s = LuSymbolic::analyze(a)
                .map_err(|e| CaeError::contract(format!("soft model analysis: {e}")))?;
            self.symbolic.get_or_init(|| s)
        };
        symbolic
            .factor(a, Parallelism::Sequential)
            .map_err(|e| CaeError::convergence(format!("singular finite-deformation tangent: {e}")))
    }

    pub fn element_design<S: Scalar>(&self, e: usize, rho: S, theta: S) -> ElemDesign<S> {
        let dirs = &self.fibres[e];
        let mut out = ElemDesign {
            e: self.interpolation.stiffness(rho),
            gamma: self.interpolation.gamma(rho),
            dirs: [[S::zero(); 3]; MAX_FIBRES],
            nf: dirs.len(),
        };
        for (k, a) in dirs.iter().enumerate() {
            out.dirs[k] = rotate(a, &self.fibre_axis[e], theta);
        }
        out
    }

    pub fn point_energy<S: Scalar>(
        &self,
        e: usize,
        f: &Mat3<S>,
        pbar: S,
        d: &ElemDesign<S>,
        h: &Mat3<S>,
        g: f64,
    ) -> S {
        let mat = &self.materials[self.element_material[e]];
        let mixed = self.formulation.mixed();
        let wang = self.interpolation.wang_active();
        let fg: Mat3<S> = if wang {
            core::array::from_fn(|i| {
                core::array::from_fn(|j| {
                    let id = if i == j { 1.0 } else { 0.0 };
                    (f[i][j] - id) * d.gamma + id
                })
            })
        } else {
            *f
        };
        let k = Kin::of(&fg);
        let mut w = mat.isochoric(&k, &d.dirs[..d.nf]) * g;
        if !mixed {
            w += mat.volumetric_energy(&k);
        }
        if wang {
            w = w - self.linear_energy(mat, &fg, g) + self.linear_energy(mat, f, g);
        }
        let mut psi = w * d.e;
        let mut hc = S::zero();
        for i in 0..3 {
            for j in 0..3 {
                hc += h[i][j] * k.c[i][j];
            }
        }
        psi += hc * 0.5;
        if mixed {
            let g_vol = if wang {
                let tr_f = f[0][0] + f[1][1] + f[2][2];
                let tr_g = fg[0][0] + fg[1][1] + fg[2][2];
                k.j - 1.0 - tr_g + tr_f
            } else {
                k.j - 1.0
            };
            psi += pbar * g_vol;
        }
        psi
    }

    fn linear_energy<S: Scalar>(&self, mat: &Material, f: &Mat3<S>, g: f64) -> S {
        let eps: Mat3<S> = core::array::from_fn(|i| {
            core::array::from_fn(|j| (f[i][j] + f[j][i]) * 0.5 - if i == j { 1.0 } else { 0.0 })
        });
        let tr = eps[0][0] + eps[1][1] + eps[2][2];
        let mut ee = S::zero();
        for i in 0..3 {
            for j in 0..3 {
                ee += eps[i][j] * eps[i][j];
            }
        }
        let mut w = (ee - tr * tr / 3.0) * (g * mat.mu0);
        if !self.formulation.mixed() {
            w += tr * tr * (0.5 * mat.kappa0);
        }
        w
    }


    pub fn pressure_energy<S: Scalar>(&self, e: usize, p: &[S; 4], stiffness: S) -> Result<S, CaeError> {
        let Formulation::Mixed { stabilization } = self.formulation else {
            return Ok(S::zero());
        };
        let mat = &self.materials[self.element_material[e]];
        let v = self.mesh.volumes[e];
        let mut acc = S::zero();
        if !matches!(mat.volumetric, Volumetric::Incompressible) {
            let total = p[0] + p[1] + p[2] + p[3];
            for q in p {
                let pg = *q * (GAUSS_A - GAUSS_B) + total * GAUSS_B;
                acc -= mat.volumetric.conjugate(mat.bulk, pg / stiffness)? * stiffness * (0.25 * v);
            }
        }
        let sum = p[0] + p[1] + p[2] + p[3];
        let sq = p[0] * p[0] + p[1] * p[1] + p[2] * p[2] + p[3] * p[3];
        acc -= (sq / 20.0 - sum * sum / 80.0) * v / (stiffness * (2.0 * stabilization * mat.mu0));
        Ok(acc)
    }

    pub fn deformation_gradient<S: Scalar>(&self, e: usize, u: &[[S; 3]; 4]) -> Mat3<S> {
        self.mesh.deformation_gradient(e, u)
    }


    #[allow(clippy::too_many_arguments)]
    pub fn element_energy<S: Scalar>(
        &self,
        e: usize,
        d: &[S; 16],
        rho: S,
        theta: S,
        h: &[S; 6],
        g: f64,
    ) -> Result<S, CaeError> {
        let u: [[S; 3]; 4] = core::array::from_fn(|a| core::array::from_fn(|i| d[3 * a + i]));
        let f = self.deformation_gradient(e, &u);
        let des = self.element_design(e, rho, theta);
        let p: [S; 4] = core::array::from_fn(|a| d[12 + a]);
        let pbar = (p[0] + p[1] + p[2] + p[3]) * 0.25;
        let hm = sym_from6(h);
        let psi = self.point_energy(e, &f, pbar, &des, &hm, g) * self.mesh.volumes[e];
        Ok(psi + self.pressure_energy(e, &p, des.e)?)
    }


    #[allow(clippy::too_many_arguments)]
    pub fn element_energy_scaled<S: Scalar>(
        &self,
        e: usize,
        d: &[S; 16],
        rho: S,
        theta: S,
        h: &[S; 6],
        g: f64,
        scale: S,
    ) -> Result<S, CaeError> {
        let u: [[S; 3]; 4] = core::array::from_fn(|a| core::array::from_fn(|i| d[3 * a + i]));
        let f = self.deformation_gradient(e, &u);
        let mut des = self.element_design(e, rho, theta);
        des.e *= scale;
        let p: [S; 4] = core::array::from_fn(|a| d[12 + a]);
        let pbar = (p[0] + p[1] + p[2] + p[3]) * 0.25;
        let hm = sym_from6(h);
        let psi = self.point_energy(e, &f, pbar, &des, &hm, g) * self.mesh.volumes[e];
        Ok(psi + self.pressure_energy(e, &p, des.e)?)
    }


    pub fn element_local(
        &self,
        e: usize,
        d: &[f64; 16],
        rho: f64,
        theta: f64,
        h: &[f64; 6],
        g: f64,
        hessian: bool,
    ) -> Result<ElementLocal, CaeError> {
        let grads = &self.mesh.gradients[e];
        let v = self.mesh.volumes[e];
        let mut fv = [0.0; 10];
        for i in 0..3 {
            for j in 0..3 {
                let mut acc = if i == j { 1.0 } else { 0.0 };
                for a in 0..4 {
                    acc += d[3 * a + i] * grads[a][j];
                }
                fv[3 * i + j] = acc;
            }
        }
        fv[9] = (d[12] + d[13] + d[14] + d[15]) * 0.25;
        let mut out = ElementLocal { energy: 0.0, gradient: [0.0; 16], hessian: Box::new([0.0; 256]) };
        let (psi, gz, hz) = if hessian {
            let des = self.element_design(e, Jet2::<10>::constant(rho), Jet2::constant(theta));
            let z = Jet2::<10>::seed(&fv);
            let fm: Mat3<Jet2<10>> = core::array::from_fn(|i| core::array::from_fn(|j| z[3 * i + j]));
            let hm = sym_from6(&h.map(Jet2::constant));
            let r = self.point_energy(e, &fm, z[9], &des, &hm, g);
            (r.v, r.g, Some(r.hessian()))
        } else {
            let des = self.element_design(e, Dual::<10>::constant(rho), Dual::constant(theta));
            let z: [Dual<10>; 10] = core::array::from_fn(|k| Dual::variable(fv[k], k));
            let fm: Mat3<Dual<10>> = core::array::from_fn(|i| core::array::from_fn(|j| z[3 * i + j]));
            let hm = sym_from6(&h.map(Dual::constant));
            let r = self.point_energy(e, &fm, z[9], &des, &hm, g);
            (r.re, r.eps, None)
        };
        out.energy = v * psi;
        let mixed = self.formulation.mixed();
        let nz = |k: usize| -> [(usize, f64); 3] {

            if k < 12 {
                let (a, i) = (k / 3, k % 3);
                [(3 * i, grads[a][0]), (3 * i + 1, grads[a][1]), (3 * i + 2, grads[a][2])]
            } else {
                [(9, 0.25), (9, 0.0), (9, 0.0)]
            }
        };
        let nl = if mixed { 16 } else { 12 };
        for k in 0..nl {
            out.gradient[k] = v * nz(k).iter().map(|(z, c)| c * gz[*z]).sum::<f64>();
        }
        if let Some(hz) = hz {
            for a in 0..nl {
                let za = nz(a);
                for b in a..nl {
                    let zb = nz(b);
                    let mut s = 0.0;
                    for (i, ci) in &za {
                        for (j, cj) in &zb {
                            s += ci * cj * hz[*i][*j];
                        }
                    }
                    out.hessian[a * 16 + b] = v * s;
                    out.hessian[b * 16 + a] = v * s;
                }
            }
        }
        if mixed {
            let stiffness = self.interpolation.stiffness(rho);
            let p = [d[12], d[13], d[14], d[15]];
            if hessian {
                let pj = Jet2::<4>::seed(&p);
                let r = self.pressure_energy(e, &pj, Jet2::constant(stiffness))?;
                let hh = r.hessian();
                out.energy += r.v;
                for a in 0..4 {
                    out.gradient[12 + a] += r.g[a];
                    for b in 0..4 {
                        out.hessian[(12 + a) * 16 + 12 + b] += hh[a][b];
                    }
                }
            } else {
                let pd: [Dual<4>; 4] = core::array::from_fn(|k| Dual::variable(p[k], k));
                let r = self.pressure_energy(e, &pd, Dual::constant(stiffness))?;
                out.energy += r.re;
                for a in 0..4 {
                    out.gradient[12 + a] += r.eps[a];
                }
            }
        }
        Ok(out)
    }


    pub fn element_local_full(
        &self,
        e: usize,
        d: &[f64; 16],
        rho: f64,
        theta: f64,
        h: &[f64; 6],
        g: f64,
    ) -> Result<ElementFull, CaeError> {
        let grads = &self.mesh.gradients[e];
        let v = self.mesh.volumes[e];
        let mut z0 = [0.0; 12];
        for i in 0..3 {
            for j in 0..3 {
                let mut acc = if i == j { 1.0 } else { 0.0 };
                for a in 0..4 {
                    acc += d[3 * a + i] * grads[a][j];
                }
                z0[3 * i + j] = acc;
            }
        }
        z0[9] = (d[12] + d[13] + d[14] + d[15]) * 0.25;
        z0[10] = rho;
        z0[11] = theta;
        let z = Jet2::<12>::seed(&z0);
        let des = self.element_design(e, z[10], z[11]);
        let fm: Mat3<Jet2<12>> = core::array::from_fn(|i| core::array::from_fn(|j| z[3 * i + j]));
        let hm = sym_from6(&h.map(Jet2::constant));
        let r = self.point_energy(e, &fm, z[9], &des, &hm, g);
        let hz = r.hessian();
        let mixed = self.formulation.mixed();
        let nl = if mixed { 16 } else { 12 };
        let nz = |k: usize| -> [(usize, f64); 3] {
            if k < 12 {
                let (a, i) = (k / 3, k % 3);
                [(3 * i, grads[a][0]), (3 * i + 1, grads[a][1]), (3 * i + 2, grads[a][2])]
            } else {
                [(9, 0.25), (9, 0.0), (9, 0.0)]
            }
        };
        let mut out = ElementFull {
            energy: v * r.v,
            gradient: [0.0; 16],
            hessian: Box::new([0.0; 256]),
            design: [[0.0; 2]; 16],
        };
        for k in 0..nl {
            let zk = nz(k);
            out.gradient[k] = v * zk.iter().map(|(zi, c)| c * r.g[*zi]).sum::<f64>();
            for s in 0..2 {
                out.design[k][s] = v * zk.iter().map(|(zi, c)| c * hz[*zi][10 + s]).sum::<f64>();
            }
            for b in k..nl {
                let zb = nz(b);
                let mut acc = 0.0;
                for (i, ci) in &zk {
                    for (j, cj) in &zb {
                        acc += ci * cj * hz[*i][*j];
                    }
                }
                out.hessian[k * 16 + b] = v * acc;
                out.hessian[b * 16 + k] = v * acc;
            }
        }
        if mixed {
            let pj = Jet2::<5>::seed(&[d[12], d[13], d[14], d[15], rho]);
            let stiffness = self.interpolation.stiffness(pj[4]);
            let pr = self.pressure_energy(e, &[pj[0], pj[1], pj[2], pj[3]], stiffness)?;
            let hh = pr.hessian();
            out.energy += pr.v;
            for a in 0..4 {
                out.gradient[12 + a] += pr.g[a];
                out.design[12 + a][0] += hh[a][4];
                for b in 0..4 {
                    out.hessian[(12 + a) * 16 + 12 + b] += hh[a][b];
                }
            }
        }
        Ok(out)
    }

    #[must_use]
    pub fn element_von_mises_squared(
        &self,
        e: usize,
        d: &[f64; 16],
        rho: f64,
        theta: f64,
        h: &[f64; 6],
    ) -> (f64, [f64; 16], [f64; 2], [f64; 6]) {
        let grads = &self.mesh.gradients[e];
        let mut z0 = [0.0; 12];
        for i in 0..3 {
            for j in 0..3 {
                let mut acc = if i == j { 1.0 } else { 0.0 };
                for a in 0..4 {
                    acc += d[3 * a + i] * grads[a][j];
                }
                z0[3 * i + j] = acc;
            }
        }
        z0[9] = (d[12] + d[13] + d[14] + d[15]) * 0.25;
        z0[10] = rho;
        z0[11] = theta;
        let z = Jet2::<12>::seed(&z0);
        let des = self.element_design(e, z[10], z[11]);
        let fm: Mat3<Jet2<12>> = core::array::from_fn(|i| core::array::from_fn(|j| z[3 * i + j]));
        let hm = sym_from6(&h.map(Jet2::constant));
        let psi = self.point_energy(e, &fm, z[9], &des, &hm, 1.0);
        let hz = psi.hessian();

        let pf: [Dual<18>; 18] =
            core::array::from_fn(|k| Dual::variable(if k < 9 { psi.g[k] } else { z0[k - 9] }, k));
        let p: Mat3<Dual<18>> = core::array::from_fn(|i| core::array::from_fn(|j| pf[3 * i + j]));
        let f: Mat3<Dual<18>> = core::array::from_fn(|i| core::array::from_fn(|j| pf[9 + 3 * i + j]));
        let jdet = det(&f);
        let sigma = matmul(&p, &transpose(&f));
        let tr = (sigma[0][0] + sigma[1][1] + sigma[2][2]) / 3.0;
        let mut ss = Dual::<18>::constant(0.0);
        for i in 0..3 {
            for j in 0..3 {
                let s = (sigma[i][j] - if i == j { tr } else { Dual::constant(0.0) }) / jdet;
                ss += s * s;
            }
        }
        let q = ss * 1.5;
        let mut dz = [0.0; 12];
        for (m, slot) in dz.iter_mut().enumerate() {
            let mut acc = if m < 9 { q.eps[9 + m] } else { 0.0 };
            for k in 0..9 {
                acc += q.eps[k] * hz[k][m];
            }
            *slot = acc;
        }
        let mut dd = [0.0; 16];
        for a in 0..4 {
            for i in 0..3 {
                dd[3 * a + i] = (0..3).map(|j| dz[3 * i + j] * grads[a][j]).sum();
            }
        }
        if self.formulation.mixed() {
            for slot in &mut dd[12..] {
                *slot = 0.25 * dz[9];
            }
        }
        let gamma = if self.interpolation.wang_active() { self.interpolation.gamma(rho) } else { 1.0 };
        let fg: Mat3<f64> = core::array::from_fn(|i| {
            core::array::from_fn(|j| {
                let id = if i == j { 1.0 } else { 0.0 };
                (z0[3 * i + j] - id) * gamma + id
            })
        });
        let mut dh = [0.0; 6];
        for (k, slot) in dh.iter_mut().enumerate() {
            let mut basis = [0.0; 6];
            basis[k] = 1.0;
            let pk = matmul(&fg, &sym_from6(&basis));
            *slot = gamma * (0..9).map(|m| q.eps[m] * pk[m / 3][m % 3]).sum::<f64>();
        }
        (q.re, dd, [dz[10], dz[11]], dh)
    }

    #[must_use]
    pub fn element_history_derivative(&self, e: usize, d: &[f64; 16], rho: f64) -> [[f64; 6]; 12] {
        let grads = &self.mesh.gradients[e];
        let v = self.mesh.volumes[e];
        let u: [[f64; 3]; 4] = core::array::from_fn(|a| core::array::from_fn(|i| d[3 * a + i]));
        let f = self.deformation_gradient(e, &u);
        let gamma = if self.interpolation.wang_active() { self.interpolation.gamma(rho) } else { 1.0 };
        let fg: Mat3<f64> = core::array::from_fn(|i| {
            core::array::from_fn(|j| {
                let id = if i == j { 1.0 } else { 0.0 };
                (f[i][j] - id) * gamma + id
            })
        });
        let mut out = [[0.0; 6]; 12];
        for k in 0..6 {
            let mut basis = [0.0; 6];
            basis[k] = 1.0;
            let ek = sym_from6(&basis);
            let pk = matmul(&fg, &ek);
            for a in 0..4 {
                for i in 0..3 {
                    out[3 * a + i][k] = v * gamma * (0..3).map(|j| pk[i][j] * grads[a][j]).sum::<f64>();
                }
            }
        }
        out
    }

    #[must_use]
    pub fn element_mass(&self, e: usize, mass_factor: f64) -> [[f64; 4]; 4] {
        let rho = self.materials[self.element_material[e]].density * self.mesh.volumes[e] * mass_factor;
        core::array::from_fn(|a| {
            core::array::from_fn(|b| {
                if self.lumped_mass {
                    if a == b { rho / 4.0 } else { 0.0 }
                } else {
                    rho / 20.0 * if a == b { 2.0 } else { 1.0 }
                }
            })
        })
    }

    #[must_use]
    pub fn element_reference_stiffness(&self, e: usize) -> [f64; 144] {
        let mat = &self.materials[self.element_material[e]];
        let g = &self.mesh.gradients[e];
        let v = self.mesh.volumes[e];
        let mu = mat.mu0;
        let lam = if mat.kappa0.is_finite() { mat.kappa0 - 2.0 * mu / 3.0 } else { -2.0 * mu / 3.0 };
        let mut k = [0.0; 144];
        for a in 0..4 {
            for i in 0..3 {
                for b in 0..4 {
                    for j in 0..3 {
                        let gg = g[a][0] * g[b][0] + g[a][1] * g[b][1] + g[a][2] * g[b][2];
                        let val = lam * g[a][i] * g[b][j]
                            + mu * (if i == j { gg } else { 0.0 } + g[a][j] * g[b][i]);
                        k[(3 * a + i) * 12 + 3 * b + j] = v * val;
                    }
                }
            }
        }
        k
    }

    pub fn face_residual<S: Scalar>(x: &[[S; 3]; 3], pressure: f64) -> [S; 3] {
        let a: [S; 3] = core::array::from_fn(|i| x[1][i] - x[0][i]);
        let b: [S; 3] = core::array::from_fn(|i| x[2][i] - x[0][i]);
        let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        n.map(|c| c * (pressure / 6.0))
    }

    #[must_use]
    pub fn face_positions(&self, f: usize, u: &[f64]) -> [[f64; 3]; 3] {
        let face = self.faces[f];
        core::array::from_fn(|k| core::array::from_fn(|i| self.mesh.points[face[k]][i] + u[3 * face[k] + i]))
    }

    #[must_use]
    pub fn face_local(&self, f: usize, u: &[f64], pressure: f64) -> ([f64; 3], [[f64; 9]; 3]) {
        let x = self.face_positions(f, u);
        let xd: [[Dual<9>; 3]; 3] =
            core::array::from_fn(|k| core::array::from_fn(|i| Dual::variable(x[k][i], 3 * k + i)));
        let r = Self::face_residual(&xd, pressure);
        (r.map(|c| c.re), r.map(|c| c.eps))
    }

    #[must_use]
    pub fn gather(&self, e: usize, u: &[f64], p: &[f64]) -> [f64; 16] {
        let t = self.mesh.elements[e];
        core::array::from_fn(|k| {
            if k < 12 {
                u[3 * t[k / 3] + k % 3]
            } else if p.is_empty() {
                0.0
            } else {
                p[t[k - 12]]
            }
        })
    }


    pub fn for_elements<T: Send>(
        &self,
        f: impl Fn(usize) -> Result<T, CaeError> + Sync,
        mut sink: impl FnMut(usize, T) -> Result<(), CaeError>,
    ) -> Result<(), CaeError> {
        let ne = self.ne();
        let mut start = 0;
        while start < ne {
            let end = (start + CHUNK).min(ne);
            let results: Vec<Result<T, CaeError>> = (start..end).into_par_iter().map(&f).collect();
            for (k, r) in results.into_iter().enumerate() {
                sink(start + k, r?)?;
            }
            start = end;
        }
        Ok(())
    }

    pub fn scatter(&self, a: &mut CscMatrix, dofs: &[usize], block: &[f64], stride: usize) {
        let data = a.data_mut();
        for (r, gr) in dofs.iter().enumerate() {
            let row = self.unknown[*gr];
            if row == usize::MAX {
                continue;
            }
            for (c, gc) in dofs.iter().enumerate() {
                let col = self.unknown[*gc];
                if col == usize::MAX {
                    continue;
                }
                let v = block[r * stride + c];
                if v != 0.0
                    && let Some(s) = self.slot(row, col)
                {
                    data[s] += v;
                }
            }
        }
    }


    pub fn scatter_triplets(
        &self,
        a: &mut CscMatrix,
        triplets: &[(usize, usize, f64)],
        weight: f64,
    ) -> Result<(), CaeError> {
        for (r, c, v) in triplets {
            let (row, col) = (
                self.unknown.get(*r).copied().unwrap_or(usize::MAX),
                self.unknown.get(*c).copied().unwrap_or(usize::MAX),
            );
            if row == usize::MAX || col == usize::MAX || *v == 0.0 {
                if *r >= self.unknown.len() || *c >= self.unknown.len() {
                    return contract("nodal potential tangent references unknown dofs");
                }
                continue;
            }
            let Some(s) = self.slot(row, col) else {
                return contract("nodal potential tangent couples dofs of nodes that share no element");
            };
            a.data_mut()[s] += weight * v;
        }
        Ok(())
    }


    pub fn global_mass_and_reference(
        &self,
        mass: &[f64],
        stiffness: &[f64],
    ) -> Result<(CsrMatrix, CsrMatrix), CaeError> {
        let n = self.n();
        let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
        for t in &self.mesh.elements {
            for &a in t {
                adjacency[a].extend_from_slice(t);
            }
        }
        for row in &mut adjacency {
            row.sort_unstable();
            row.dedup();
        }
        let mut indptr = Vec::with_capacity(3 * n + 1);
        indptr.push(0);
        let mut indices = Vec::new();
        for row in &adjacency {
            for _ in 0..3 {
                for &b in row {
                    indices.extend([3 * b, 3 * b + 1, 3 * b + 2]);
                }
                indptr.push(indices.len());
            }
        }
        let mut dm = vec![0.0; indices.len()];
        let mut dk = vec![0.0; indices.len()];
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let m = self.element_mass(e, mass[e]);
            let k = self.element_reference_stiffness(e);
            for a in 0..4 {
                for b in 0..4 {
                    let Ok(pos) = adjacency[t[a]].binary_search(&t[b]) else {
                        return contract("internal: element pair outside the node adjacency");
                    };
                    for i in 0..3 {
                        let base = indptr[3 * t[a] + i] + 3 * pos;
                        dm[base + i] += m[a][b];
                        for j in 0..3 {
                            dk[base + j] += stiffness[e] * k[(3 * a + i) * 12 + 3 * b + j];
                        }
                    }
                }
            }
        }
        let err = |e: implexity_linalg::LinalgError| CaeError::contract(format!("soft model matrices: {e}"));
        Ok((
            CsrMatrix::try_new(3 * n, 3 * n, indptr.clone(), indices.clone(), dm).map_err(err)?,
            CsrMatrix::try_new(3 * n, 3 * n, indptr, indices, dk).map_err(err)?,
        ))
    }

    #[must_use]
    pub fn element_stress(
        &self,
        e: usize,
        d: &[f64; 16],
        rho: f64,
        theta: f64,
        h: &[f64; 6],
        g: f64,
    ) -> (Mat3<f64>, f64, Vec<f64>) {
        let u: [[f64; 3]; 4] = core::array::from_fn(|a| core::array::from_fn(|i| d[3 * a + i]));
        let f = self.deformation_gradient(e, &u);
        let pbar = (d[12] + d[13] + d[14] + d[15]) * 0.25;
        let des = self.element_design(e, Dual::<9>::constant(rho), Dual::constant(theta));
        let fd: Mat3<Dual<9>> =
            core::array::from_fn(|i| core::array::from_fn(|j| Dual::variable(f[i][j], 3 * i + j)));
        let hm = sym_from6(&h.map(Dual::constant));
        let psi = self.point_energy(e, &fd, Dual::constant(pbar), &des, &hm, g);
        let p: Mat3<f64> = core::array::from_fn(|i| core::array::from_fn(|j| psi.eps[3 * i + j]));
        let j = det(&f);
        let s = matmul(&p, &transpose(&f));
        let sigma = s.map(|r| r.map(|x| x / j));
        let c = matmul(&transpose(&f), &f);
        let des64 = self.element_design(e, rho, theta);
        let stretches = (0..des64.nf).map(|k| fibre_i4(&c, &des64.dirs[k]).sqrt()).collect();
        (sigma, j, stretches)
    }

    pub fn isochoric_stress_dot<S: Scalar>(
        &self,
        e: usize,
        u: &[[S; 3]; 4],
        rho: S,
        theta: S,
        w: &[f64; 6],
        t: S,
    ) -> S {
        let mat = &self.materials[self.element_material[e]];
        let des = self.element_design(e, rho, theta);
        let f = self.deformation_gradient(e, u);
        let fg: Mat3<S> = if self.interpolation.wang_active() {
            core::array::from_fn(|i| {
                core::array::from_fn(|j| {
                    let id = if i == j { 1.0 } else { 0.0 };
                    (f[i][j] - id) * des.gamma + id
                })
            })
        } else {
            f
        };
        let c0 = matmul(&transpose(&fg), &fg);
        let wm = sym_from6(w);
        let c: Mat3<S> = core::array::from_fn(|i| core::array::from_fn(|j| c0[i][j] + t * wm[i][j]));
        let k = Kin::from_c(c, det(&c).sqrt());
        mat.isochoric(&k, &des.dirs[..des.nf]) * des.e * 2.0
    }

    #[must_use]
    pub fn isochoric_stress(&self, e: usize, u: &[[f64; 3]; 4], rho: f64, theta: f64) -> [f64; 6] {
        let um = u.map(|r| r.map(HyperDual::constant));
        core::array::from_fn(|k| {
            let mut w = [0.0; 6];
            w[k] = 1.0;
            let t = HyperDual::new(0.0, 1.0, 0.0, 0.0);
            let v = self.isochoric_stress_dot(
                e,
                &um,
                HyperDual::constant(rho),
                HyperDual::constant(theta),
                &w,
                t,
            );
            if k < 3 { v.e1 } else { 0.5 * v.e1 }
        })
    }
}
