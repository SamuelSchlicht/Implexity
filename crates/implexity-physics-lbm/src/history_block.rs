// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::coupled_history::{HistoryBlock, HistoryBlockCallbacks, HistoryInterface};
use implexity_solve::local_assembly::Kind;
use implexity_solve::matrix::Jacobian;

use crate::d3q19::Q;
use crate::design::resistance;
use crate::history_partials::{CellCollision, PointwiseViscosity, partials, relaxation};
use crate::ports::{Cell, apply_ports_in_place};
use crate::solver::LbmProblem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Partial {
    Current = 0,
    Old = 1,
    Design = 2,
    Temperature = 3,
}

impl Partial {

    pub fn from_index(kind: i64) -> CaeResult<Self> {
        match kind {
            0 => Ok(Self::Current),
            1 => Ok(Self::Old),
            2 => Ok(Self::Design),
            3 => Ok(Self::Temperature),
            _ => Err(CaeError::contract("invalid partial kind")),
        }
    }
}

pub trait GenericInitial: Send + Sync {
    fn initial<S: Scalar>(&self, x: &[S]) -> Vec<S>;
}

pub trait InitialPopulations: Send + Sync {
    fn value(&self, x: &[f64]) -> Vec<f64>;
    fn jvp(&self, x: &[f64], v: &[f64]) -> Vec<f64>;
    fn vjp(&self, x: &[f64], w: &[f64]) -> Vec<f64>;
}

impl<G: GenericInitial> InitialPopulations for G {
    fn value(&self, x: &[f64]) -> Vec<f64> {
        self.initial(x)
    }

    fn jvp(&self, x: &[f64], v: &[f64]) -> Vec<f64> {
        let xs: Vec<Dual<1>> = x.iter().zip(v).map(|(a, t)| Dual::new(*a, [*t])).collect();
        self.initial(&xs).iter().map(|d| d.eps[0]).collect()
    }

    fn vjp(&self, x: &[f64], w: &[f64]) -> Vec<f64> {
        let rec = crate::rv::Recording::start();
        let xv = rec.inputs(x);
        let out = self.initial(&xv);
        rec.vjp(&out, w, &xv)
    }
}

pub struct LbmHistorySeam<C: CellCollision, V: PointwiseViscosity> {
    pub p: LbmProblem,
    pub identity: String,
    pub collision: C,
    pub viscosity: V,
    pub derivative_batch_size: usize,
    pub initial: Option<Arc<dyn InitialPopulations>>,
}

impl<C: CellCollision, V: PointwiseViscosity> std::fmt::Debug for LbmHistorySeam<C, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LbmHistorySeam")
            .field("identity", &self.identity)
            .field("derivative_batch_size", &self.derivative_batch_size)
            .field("initial", &self.initial.is_some())
            .finish_non_exhaustive()
    }
}

impl<C: CellCollision, V: PointwiseViscosity> LbmHistorySeam<C, V> {

    pub fn new(
        p: &LbmProblem,
        collision: C,
        viscosity: V,
        source_identity: &str,
        derivative_batch_size: usize,
    ) -> CaeResult<Self> {
        if source_identity.is_empty() {
            return Err(CaeError::contract("source identity required"));
        }
        if derivative_batch_size < 1 {
            return Err(CaeError::contract("positive derivative batch size required"));
        }
        Ok(Self {
            p: p.clone(),
            identity: source_identity.to_string(),
            collision,
            viscosity,
            derivative_batch_size,
            initial: None,
        })
    }

    #[must_use]
    pub fn with_initial(mut self, initial: Arc<dyn InitialPopulations>) -> Self {
        self.initial = Some(initial);
        self
    }

    fn initial_map(&self) -> CaeResult<&Arc<dyn InitialPopulations>> {
        self.initial.as_ref().ok_or_else(|| CaeError::contract("the seam has no initial-population map"))
    }


    pub fn initial_action(&self, x: &[f64], v: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let init = self.initial_map()?;
        Ok(if transpose { init.vjp(x, v) } else { init.jvp(x, v) })
    }

    #[must_use]
    pub fn size(&self) -> usize {
        Q * self.p.cells()
    }

    #[must_use]
    pub fn step(&self, old: &[f64], raw: &[f64], temperature: &[f64]) -> Vec<f64> {
        let p = &self.p;
        let nc = p.cells();
        let phi = p.fraction(raw);
        let a = p.acceleration_lattice();
        let mut post = vec![0.0; Q * nc];
        for c in 0..nc {
            let tau = relaxation(self.viscosity.viscosity(temperature[c], raw[c]), p.step_s, p.spacing_m);
            let alpha = resistance(p.step_s, p.drag_max_per_s, p.drag_shape, phi[c]);
            let cell: Cell<f64> = std::array::from_fn(|q| old[c * Q + q]);
            post[c * Q..(c + 1) * Q].copy_from_slice(&self.collision.collide(&cell, alpha, a, tau));
        }
        let mut out = vec![0.0; Q * nc];
        p.streaming.stream(&post, old, &mut out);
        apply_ports_in_place(&p.ports, &mut out, a);
        out
    }

    #[must_use]
    pub fn residual(&self, current: &[f64], old: &[f64], raw: &[f64], temperature: &[f64]) -> Vec<f64> {
        let s = self.step(old, raw, temperature);
        current.iter().zip(&s).map(|(z, v)| z - v).collect()
    }


    pub fn sparse_partial(
        &self,
        kind: Partial,
        current: &[f64],
        old: &[f64],
        raw: &[f64],
        temperature: &[f64],
    ) -> CaeResult<CsrMatrix> {
        if kind == Partial::Current {
            return Ok(CsrMatrix::identity(self.size()));
        }
        let [_, o, d, t] = partials(
            &self.p,
            &self.collision,
            &self.viscosity,
            current,
            old,
            raw,
            temperature,
            self.derivative_batch_size,
        )?;
        Ok(match kind {
            Partial::Old => o,
            Partial::Design => d,
            _ => t,
        })
    }


    pub fn action(
        &self,
        kind: Partial,
        current: &[f64],
        old: &[f64],
        raw: &[f64],
        temperature: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        let m = self.sparse_partial(kind, current, old, raw, temperature)?;
        let out = if transpose { m.matvec_transpose(v) } else { m.matvec(v) };
        out.map_err(|e| CaeError::contract(e.to_string()))
    }
}

struct SeamBlock<C: CellCollision, V: PointwiseViscosity> {
    seam: Arc<LbmHistorySeam<C, V>>,
    reference: Vec<f64>,
    initial_jacobian: Option<InitialJacobian>,
}

pub type InitialJacobian = Arc<dyn Fn(&[f64]) -> CaeResult<Jacobian> + Send + Sync>;

impl<C: CellCollision + 'static, V: PointwiseViscosity + 'static> HistoryBlockCallbacks for SeamBlock<C, V> {
    fn residual(&self, _n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(self.seam.residual(z, old, x, &self.reference))
    }

    fn current_jacobian(&self, _n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.seam.sparse_partial(Partial::Current, z, old, x, &self.reference)?))
    }

    fn previous_jacobian(&self, _n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.seam.sparse_partial(Partial::Old, z, old, x, &self.reference)?))
    }

    fn design_jacobian(&self, _n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.seam.sparse_partial(Partial::Design, z, old, x, &self.reference)?))
    }

    fn initial_jacobian(&self, x: &[f64]) -> Option<CaeResult<Jacobian>> {
        self.initial_jacobian.as_ref().map(|j| j(x))
    }
}

#[derive(Clone)]
pub enum TemperatureCallback<T> {
    Legacy(Arc<dyn Fn(&[f64], &[f64]) -> CaeResult<T> + Send + Sync>),
    StepAware(Arc<dyn Fn(usize, &[f64], &[f64]) -> CaeResult<T> + Send + Sync>),
}

impl<T> TemperatureCallback<T> {
    fn call(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<T> {
        match self {
            Self::Legacy(f) => f(z, x),
            Self::StepAware(f) => f(n, z, x),
        }
    }

    fn step_aware(&self) -> bool {
        matches!(self, Self::StepAware(_))
    }
}

pub struct ThermalInterface<C: CellCollision, V: PointwiseViscosity> {
    seam: Arc<LbmHistorySeam<C, V>>,
    slice: std::ops::Range<usize>,
    design_indices: Vec<usize>,
    total_size: usize,
    temperature: TemperatureCallback<Vec<f64>>,
    current: TemperatureCallback<CsrMatrix>,
    design: TemperatureCallback<CsrMatrix>,
}

struct BoundThermalInterface<C: CellCollision, V: PointwiseViscosity> {
    spec: Arc<ThermalInterface<C, V>>,
    reference: Vec<f64>,
}

impl<C: CellCollision + 'static, V: PointwiseViscosity + 'static> ThermalInterface<C, V> {
    #[must_use]
    pub fn bind(self: &Arc<Self>, reference_temperature: &[f64]) -> Arc<dyn HistoryInterface> {
        Arc::new(BoundThermalInterface { spec: Arc::clone(self), reference: reference_temperature.to_vec() })
    }

    fn local(&self, x: &[f64]) -> Vec<f64> {
        self.design_indices.iter().map(|i| x[*i]).collect()
    }
}

#[allow(clippy::needless_pass_by_value)]
fn csr(e: implexity_linalg::error::LinalgError) -> CaeError {
    CaeError::contract(e.to_string())
}

impl<C: CellCollision + 'static, V: PointwiseViscosity + 'static> HistoryInterface
    for BoundThermalInterface<C, V>
{
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let s = &self.spec;
        let t = s.temperature.call(n, z, x)?;
        let local = s.local(x);
        let old_f = &old[s.slice.clone()];
        let a = s.seam.step(old_f, &local, &self.reference);
        let b = s.seam.step(old_f, &local, &t);
        let mut out = vec![0.0; s.total_size];
        for (k, i) in s.slice.clone().enumerate() {
            out[i] = a[k] - b[k];
        }
        Ok(out)
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        let s = &self.spec;
        let t = s.temperature.call(n, z, x)?;
        let local = s.local(x);
        let (zs, os) = (&z[s.slice.clone()], &old[s.slice.clone()]);
        let seam = &s.seam;
        let batch = seam.derivative_batch_size;
        let actual = partials(&seam.p, &seam.collision, &seam.viscosity, zs, os, &local, &t, batch)?;
        let reference =
            partials(&seam.p, &seam.collision, &seam.viscosity, zs, os, &local, &self.reference, batch)?;
        let size = seam.size();
        let rows: Vec<usize> = s.slice.clone().collect();
        let e = CsrMatrix::from_triplets(
            s.total_size,
            size,
            &rows,
            &(0..size).collect::<Vec<_>>(),
            &vec![1.0; size],
        )
        .map_err(csr)?;
        let nl = s.design_indices.len();
        let d = CsrMatrix::from_triplets(
            nl,
            x.len(),
            &(0..nl).collect::<Vec<_>>(),
            &s.design_indices,
            &vec![1.0; nl],
        )
        .map_err(csr)?;
        let out = match kind {
            Kind::Current => {
                let j = s.current.call(n, z, x)?;
                if j.shape() != (t.len(), s.total_size) {
                    return Err(CaeError::contract("temperature current map shape"));
                }
                e.matmul(&actual[3]).and_then(|m| m.matmul(&j)).map_err(csr)?
            }
            Kind::Previous => {
                let diff = actual[1].add_scaled(1.0, &reference[1], -1.0).map_err(csr)?;
                e.matmul(&diff).and_then(|m| m.matmul(&e.transpose())).map_err(csr)?
            }
            Kind::Design => {
                let j = s.design.call(n, z, x)?;
                if j.shape() != (t.len(), x.len()) {
                    return Err(CaeError::contract("temperature design map shape"));
                }
                let diff = actual[2].add_scaled(1.0, &reference[2], -1.0).map_err(csr)?;
                let inner = diff
                    .matmul(&d)
                    .map_err(csr)?
                    .add_scaled(1.0, &actual[3].matmul(&j).map_err(csr)?, 1.0)
                    .map_err(csr)?;
                e.matmul(&inner).map_err(csr)?
            }
        };
        if !out.data().iter().all(|v| f64::is_finite(*v)) {
            return Err(CaeError::contract("nonfinite sparse interface partial"));
        }
        Ok(Jacobian::Csr(out))
    }
}

impl<C: CellCollision + 'static, V: PointwiseViscosity + 'static> LbmHistorySeam<C, V> {

    pub fn block(
        self: &Arc<Self>,
        name: &str,
        design_indices: Vec<usize>,
        x0: &[f64],
        reference_temperature: &[f64],
        initial_jacobian: Option<InitialJacobian>,
        design_independent_initial: bool,
    ) -> CaeResult<HistoryBlock> {
        if initial_jacobian.is_none() == !design_independent_initial {
            return Err(CaeError::contract(
                "provide initial_jacobian OR explicit design-independent initial",
            ));
        }
        let initial = self.initial_map()?.value(x0);
        Ok(HistoryBlock {
            name: name.to_string(),
            initial,
            design_indices,
            callbacks: Arc::new(SeamBlock {
                seam: Arc::clone(self),
                reference: reference_temperature.to_vec(),
                initial_jacobian,
            }),
            field: Some("flow".into()),
        })
    }


    pub fn thermal_interface(
        self: &Arc<Self>,
        population_slice: std::ops::Range<usize>,
        temperature_map: TemperatureCallback<Vec<f64>>,
        design_indices: Vec<usize>,
        total_size: usize,
        temperature_current_jacobian: Option<TemperatureCallback<CsrMatrix>>,
        temperature_design_jacobian: Option<TemperatureCallback<CsrMatrix>>,
    ) -> CaeResult<Arc<ThermalInterface<C, V>>> {
        let (Some(current), Some(design)) = (temperature_current_jacobian, temperature_design_jacobian)
        else {
            return Err(CaeError::contract("explicit sparse shared temperature partials required"));
        };
        let aware = temperature_map.step_aware();
        if current.step_aware() != aware || design.step_aware() != aware {
            return Err(CaeError::contract("step_aware must be an explicit boolean"));
        }
        Ok(Arc::new(ThermalInterface {
            seam: Arc::clone(self),
            slice: population_slice,
            design_indices,
            total_size,
            temperature: temperature_map,
            current,
            design,
        }))
    }
}
