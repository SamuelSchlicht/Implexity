// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_ad::revolve::{Action, BinomialSchedule};
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_solve::multirate_coupling::{
    SecondOrderSubcycle, SubcycleCotangent, SubcycleRecord, SubcycleTangent, SubcycledField,
};
use implexity_solve::time_stepper::{StepDiagnostics, StepParameters};
use rayon::prelude::*;

use super::boundary::{
    Face, PortKind, PortSpec, PortValue, Topology, port_populations, port_populations_vjp,
};
use super::carrier::LagrangianCarrier;
use super::collision::{Collision, CollisionKind};
use super::kernels::{self, RUN};
use super::lattice::{Lattice, equilibrium, equilibrium_vjp, moments};
use super::observables::{Observable, ObservableSpec, SampleScales};
use super::psm::{
    CellParameters, CouplingLaw, Smagorinsky, SolidInput, cell_update, cell_update_vjp, saturate,
    saturate_derivative, saturated_ratio,
};
use super::pushforward::{self, Endpoint, Frame, NONE, Occupancy, PointBar, PointSet};
use super::sponge::{SpongeField, SpongeSpec};
use super::wale::{self, Wale};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Turbulence {
    Laminar,
    Smagorinsky(Smagorinsky),
    Wale(Wale),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sampling {
    End,
    MacroMean,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntrainedInertia {
    Carried,
    Compensated,
}

#[derive(Clone, Debug)]
pub struct MovingLbmConfig {
    pub shape: [usize; 3],
    pub spacing_m: f64,
    pub origin_m: [f64; 3],
    pub periodic: [bool; 3],
    pub density_kg_m3: f64,
    pub kinematic_viscosity_m2_s: f64,
    pub collision: CollisionKind,
    pub turbulence: Turbulence,
    pub coupling_law: CouplingLaw,
    pub saturation_width: f64,
    pub kernel_width_cells: usize,
    pub interpolation_kernel: pushforward::InterpolationKernel,
    pub solid_mask: Vec<bool>,
    pub ports: Vec<PortSpec>,
    pub symmetry_faces: Vec<Face>,
    pub sponges: Vec<SpongeSpec>,
    pub body_acceleration_m_s2: [f64; 3],
    pub macro_step_s: f64,
    pub substeps: usize,
    pub mach_limit: f64,
    pub lattice_velocity_limit: f64,
    pub tau_min: f64,
    pub observables: Vec<(String, ObservableSpec)>,
    pub sampling: Sampling,
    pub initial_velocity_m_s: [f64; 3],
    pub initial_pressure_pa: f64,
    pub inner_checkpoint_bytes: u64,
    pub entrained_inertia: EntrainedInertia,
    pub adjoint_cache: bool,
}

impl MovingLbmConfig {

    pub fn with_acoustic_scaling(mut self, speed_of_sound_m_s: f64) -> CaeResult<(Self, AcousticScaling)> {
        if !(speed_of_sound_m_s.is_finite() && speed_of_sound_m_s > 0.0) {
            return Err(CaeError::contract("acoustic scaling needs a finite positive speed of sound"));
        }
        positive(self.spacing_m, "spacing_m")?;
        positive(self.macro_step_s, "macro_step_s")?;
        let exact = 3f64.sqrt() * speed_of_sound_m_s * self.macro_step_s / self.spacing_m;
        if !(exact.is_finite() && exact >= 0.5) {
            return Err(CaeError::contract(format!(
                "acoustic scaling: the macro step {} s is shorter than half an acoustic lattice step",
                self.macro_step_s
            )));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let substeps = (exact.round() as usize).max(1);
        let dt_f = self.macro_step_s / substeps as f64;
        let lattice = self.spacing_m / (3f64.sqrt() * dt_f);
        let tau_plus = 0.5 + 3.0 * self.kinematic_viscosity_m2_s * dt_f / (self.spacing_m * self.spacing_m);
        if tau_plus - 0.5 <= self.tau_min {
            return Err(CaeError::contract(format!(
                "moving LBM admission refused (acoustic_scaling): {substeps} substeps give tau_plus - 1/2 = \
                 {:.3e}, not above tau_min = {}",
                tau_plus - 0.5,
                self.tau_min
            )));
        }
        self.substeps = substeps;
        let record = AcousticScaling {
            speed_of_sound_m_s,
            substeps,
            lattice_sound_speed_m_s: lattice,
            relative_error: (lattice - speed_of_sound_m_s).abs() / speed_of_sound_m_s,
            tau_plus,
        };
        Ok((self, record))
    }

    #[must_use]
    pub fn new(
        shape: [usize; 3],
        spacing_m: f64,
        periodic: [bool; 3],
        density_kg_m3: f64,
        kinematic_viscosity_m2_s: f64,
        macro_step_s: f64,
        substeps: usize,
    ) -> Self {
        Self {
            shape,
            spacing_m,
            origin_m: [0.0; 3],
            periodic,
            density_kg_m3,
            kinematic_viscosity_m2_s,
            collision: CollisionKind::Trt { magic: super::collision::MAGIC_DEFAULT },
            turbulence: Turbulence::Laminar,
            coupling_law: CouplingLaw::PsmSuperposition,
            saturation_width: 0.05,
            kernel_width_cells: 1,
            interpolation_kernel: pushforward::InterpolationKernel::Cubic,
            solid_mask: vec![false; shape[0] * shape[1] * shape[2]],
            ports: Vec::new(),
            symmetry_faces: Vec::new(),
            sponges: Vec::new(),
            body_acceleration_m_s2: [0.0; 3],
            macro_step_s,
            substeps,
            mach_limit: 0.15,
            lattice_velocity_limit: 0.1,
            tau_min: 1e-3,
            observables: Vec::new(),
            sampling: Sampling::End,
            initial_velocity_m_s: [0.0; 3],
            initial_pressure_pa: 0.0,
            inner_checkpoint_bytes: 256 << 20,
            entrained_inertia: EntrainedInertia::Carried,
            adjoint_cache: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AcousticScaling {
    pub speed_of_sound_m_s: f64,
    pub substeps: usize,
    pub lattice_sound_speed_m_s: f64,
    pub relative_error: f64,
    pub tau_plus: f64,
}

impl AcousticScaling {
    #[must_use]
    pub fn record(&self) -> serde_json::Value {
        serde_json::json!({
            "check": "acoustic_scaling",
            "speed_of_sound_m_s": self.speed_of_sound_m_s,
            "substeps": self.substeps,
            "lattice_sound_speed_m_s": self.lattice_sound_speed_m_s,
            "relative_error": self.relative_error,
            "tau_plus": self.tau_plus,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockingSaturation {
    pub max_fill: f64,
    pub required_fill: f64,
    pub saturated_cells: usize,
    pub active_cells: usize,
    pub max_blocking: f64,
    pub tau_plus: f64,
}

impl BlockingSaturation {
    #[must_use]
    pub fn admitted(&self) -> bool {
        self.max_fill >= self.required_fill
    }

    #[must_use]
    pub fn record(&self) -> serde_json::Value {
        serde_json::json!({
            "check": "blocking_saturation",
            "admitted": self.admitted(),
            "max_fill": self.max_fill,
            "required_fill": self.required_fill,
            "saturated_cells": self.saturated_cells,
            "active_cells": self.active_cells,
            "max_blocking": self.max_blocking,
            "tau_plus": self.tau_plus,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SubcycleOutput {
    pub state: Vec<f64>,
    pub flux: Vec<f64>,
    pub samples: Vec<f64>,
    pub pairing: f64,
    pub ledger: BTreeMap<String, f64>,
}

#[derive(Clone, Debug)]
pub struct SubcycleTangentOutput {
    pub state: Vec<f64>,
    pub flux: Vec<f64>,
    pub samples: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct SubcycleBars {
    pub previous: Vec<f64>,
    pub trace_start: Vec<f64>,
    pub trace_end: Vec<f64>,
    pub design: Vec<f64>,
    pub time_scale: f64,
    pub absolute_origin_s:f64,
}

trait Lane: Scalar {
    const DIRECTIONAL: bool;
    fn make(value: f64, tangent: f64) -> Self;
    fn tangent(&self) -> f64;
    fn cached(_cache: &AdjointCache, _key: &PreparedKey) -> Option<Arc<Prepared<Self>>> {
        None
    }
    fn keep(_cache: &AdjointCache, _key: PreparedKey, _prepared: Arc<Prepared<Self>>, _pool: &BufferPool) {}
    fn pooled(_pool: &BufferPool, len: usize) -> Vec<Self> {
        vec![Self::zero(); len]
    }
    fn recycle(_pool: &BufferPool, _v: Vec<Self>) {}
    fn into_values(v: Vec<Self>) -> Vec<f64> {
        v.iter().map(Scalar::value).collect()
    }
}

impl Lane for f64 {
    const DIRECTIONAL: bool = false;
    #[inline]
    fn make(value: f64, _tangent: f64) -> Self {
        value
    }
    #[inline]
    fn tangent(&self) -> f64 {
        0.0
    }
    fn cached(cache: &AdjointCache, key: &PreparedKey) -> Option<Arc<Prepared<Self>>> {
        let guard = cache.lock().ok()?;
        guard.as_ref().filter(|(k, _)| k == key).map(|(_, p)| Arc::clone(p))
    }
    fn keep(cache: &AdjointCache, key: PreparedKey, prepared: Arc<Prepared<Self>>, pool: &BufferPool) {
        let old = match cache.lock() {
            Ok(mut guard) => guard.replace((key, prepared)),
            Err(_) => None,
        };
        if let Some((_, old)) = old
            && let Ok(p) = Arc::try_unwrap(old)
            && let Checkpoints::All(states) = p.checkpoints
        {
            for v in states {
                Self::recycle(pool, v);
            }
        }
    }
    fn pooled(pool: &BufferPool, len: usize) -> Vec<Self> {
        let got = pool.lock().ok().and_then(|mut p| p.pool.pop());
        match got {
            Some(mut v) => {
                v.resize(len, 0.0);
                v
            }
            None => vec![0.0; len],
        }
    }
    fn recycle(pool: &BufferPool, v: Vec<Self>) {
        if let Ok(mut p) = pool.lock()
            && p.pool.len() < p.cap
            && v.capacity() > 0
        {
            p.pool.push(v);
        }
    }
    fn into_values(v: Vec<Self>) -> Vec<f64> {
        v
    }
}

impl Lane for Dual<1> {
    const DIRECTIONAL: bool = true;
    #[inline]
    fn make(value: f64, tangent: f64) -> Self {
        Dual::new(value, [tangent])
    }
    #[inline]
    fn tangent(&self) -> f64 {
        self.eps[0]
    }
}

const RED: usize = 4096;

#[derive(Clone, Copy, Debug)]
struct Target<S> {
    rho: S,
    mean: [S; 3],
    sin: [S; 3],
    cos: [S; 3],
}

impl<S: Scalar> Target<S> {
    fn zero() -> Self {
        Self { rho: S::zero(), mean: [S::zero(); 3], sin: [S::zero(); 3], cos: [S::zero(); 3] }
    }

    fn map<T: Scalar>(&self, f: impl Fn(S) -> T) -> Target<T> {
        Target { rho: f(self.rho), mean: self.mean.map(&f), sin: self.sin.map(&f), cos: self.cos.map(&f) }
    }

    #[inline]
    fn velocity(&self, phase: [f64; 2]) -> [S; 3] {
        std::array::from_fn(|d| self.mean[d] + self.sin[d] * phase[0] - self.cos[d] * phase[1])
    }

    #[inline]
    fn add_bar(&mut self, rho_bar: S, u_bar: [S; 3], phase: [f64; 2]) {
        self.rho += rho_bar;
        for d in 0..3 {
            self.mean[d] += u_bar[d];
            self.sin[d] += u_bar[d] * phase[0];
            self.cos[d] -= u_bar[d] * phase[1];
        }
    }
}

#[derive(Clone, Debug)]
struct Params<S> {
    cell: CellParameters<S>,
    ports: Vec<PortValue<S>>,
    targets: Vec<Target<S>>,
    scales: SampleScales<S>,
    impulse_scale: S,
    theta: f64,
}

impl<S: Scalar> Params<S> {
    fn map<T: Scalar>(&self, f: impl Fn(S) -> T) -> Params<T> {
        Params {
            cell: CellParameters {
                omega: f(self.cell.omega),
                accel: self.cell.accel.map(&f),
                c_alpha: f(self.cell.c_alpha),
            },
            ports: self
                .ports
                .iter()
                .map(|p| match p {
                    PortValue::Velocity(u) => PortValue::Velocity(u.map(&f)),
                    PortValue::Density(r) => PortValue::Density(f(*r)),
                })
                .collect(),
            targets: self.targets.iter().map(|t| t.map(&f)).collect(),
            scales: SampleScales {
                density: self.scales.density,
                spacing: self.scales.spacing,
                velocity: f(self.scales.velocity),
                macro_step: f(self.scales.macro_step),
                fluid_step: f(self.scales.fluid_step),
            },
            impulse_scale: f(self.impulse_scale),
            theta: self.theta,
        }
    }
}

#[derive(Clone, Debug)]
struct ParamBar<S> {
    omega: S,
    accel: [S; 3],
    c_alpha: S,
    ports: Vec<[S; 3]>,
    targets: Vec<Target<S>>,
}

impl<S: Scalar> ParamBar<S> {
    fn zero(ports: usize, targets: usize) -> Self {
        Self {
            omega: S::zero(),
            accel: [S::zero(); 3],
            c_alpha: S::zero(),
            ports: vec![[S::zero(); 3]; ports],
            targets: vec![Target::zero(); targets],
        }
    }

    fn add(&mut self, o: &Self) {
        self.omega += o.omega;
        self.c_alpha += o.c_alpha;
        for d in 0..3 {
            self.accel[d] += o.accel[d];
        }
        for (a, b) in self.ports.iter_mut().zip(&o.ports) {
            for d in 0..3 {
                a[d] += b[d];
            }
        }
        for (a, b) in self.targets.iter_mut().zip(&o.targets) {
            a.rho += b.rho;
            for d in 0..3 {
                a.mean[d] += b.mean[d];
                a.sin[d] += b.sin[d];
                a.cos[d] += b.cos[d];
            }
        }
    }

    fn contract(&self, dp: &Params<f64>) -> S {
        let mut acc = self.omega * dp.cell.omega + self.c_alpha * dp.cell.c_alpha;
        for d in 0..3 {
            acc += self.accel[d] * dp.cell.accel[d];
        }
        for (b, p) in self.ports.iter().zip(&dp.ports) {
            match p {
                PortValue::Velocity(u) => {
                    for d in 0..3 {
                        acc += b[d] * u[d];
                    }
                }
                PortValue::Density(r) => acc += b[0] * *r,
            }
        }
        for (b, t) in self.targets.iter().zip(&dp.targets) {
            acc += b.rho * t.rho;
            for d in 0..3 {
                acc += b.mean[d] * t.mean[d] + b.sin[d] * t.sin[d] + b.cos[d] * t.cos[d];
            }
        }
        acc
    }
}

struct Macro<S> {
    ends: [Endpoint<S>; 2],
    v: Vec<[S; 3]>,
    occ: Occupancy<S>,
    displacement: f64,
}

struct Accum<S> {
    g: [Vec<[S; 3]>; 2],
    samples: Vec<S>,
    pairing: f64,
    impulse: [S; 3],
    max_speed: f64,
    kinetic: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct PreparedKey {
    absolute_origin:Option<u64>,
    interpolation_kernel: pushforward::InterpolationKernel,
    n: usize,
    time_scale: u64,
    trace_start: Vec<f64>,
    trace_end: Vec<f64>,
    design: Vec<f64>,
    previous: Vec<f64>,
}

enum Checkpoints<S> {
    All(Vec<Vec<S>>),
    Binomial {
        store: Vec<Option<(usize, Vec<S>)>>,
        cursor: Option<(usize, Vec<S>)>,
        actions: Vec<Action>,
        reverse_start: usize,
    },
}

struct Prepared<S> {
    mac: Macro<S>,
    acc: Accum<S>,
    entrained: Option<(Vec<[S; 3]>, Vec<[S; 3]>)>,
    checkpoints: Checkpoints<S>,
}

type AdjointCache = std::sync::Mutex<Option<(PreparedKey, Arc<Prepared<f64>>)>>;

struct Pool {
    pool: Vec<Vec<f64>>,
    cap: usize,
}

type BufferPool = std::sync::Mutex<Pool>;

fn take_cursor<S>(c: &mut Option<(usize, Vec<S>)>) -> CaeResult<(usize, Vec<S>)> {
    c.take().ok_or_else(|| CaeError::contract("moving LBM inner schedule: empty cursor"))
}

struct Fields<S> {
    rho: Vec<S>,
    u: Vec<[S; 3]>,
}

struct Chunk<'a, S> {
    start: usize,
    q: Vec<&'a mut [S]>,
    rho: Option<&'a mut [S]>,
    u: Option<&'a mut [[S; 3]]>,
    slot_start: usize,
    slots: &'a mut [[S; 3]],
    extra: Option<&'a mut [S]>,
}

const CHUNK: usize = 2048;

fn split_state<'a, S>(
    data: &'a mut [S],
    n: usize,
    q: usize,
    rho: Option<&'a mut [S]>,
    u: Option<&'a mut [[S; 3]]>,
    slots: &'a mut [[S; 3]],
    cells: &[u32],
    extra: Option<&'a mut [S]>,
) -> Vec<Chunk<'a, S>> {
    let count = n.div_ceil(CHUNK);
    let mut per_q: Vec<std::slice::ChunksMut<'a, S>> =
        data.chunks_mut(n).take(q).map(|s| s.chunks_mut(CHUNK)).collect();
    let mut rho_it = rho.map(|r| r.chunks_mut(CHUNK));
    let mut u_it = u.map(|r| r.chunks_mut(CHUNK));
    let mut extra_it = extra.map(|r| r.chunks_mut(CHUNK));
    let mut rest = slots;
    let mut out = Vec::with_capacity(count);
    for c in 0..count {
        let start = c * CHUNK;
        let end = (start + CHUNK).min(n);
        #[allow(clippy::cast_possible_truncation)]
        let (lo, hi) = (
            cells.partition_point(|&x| (x as usize) < start),
            cells.partition_point(|&x| (x as usize) < end),
        );
        let (mine, tail) = std::mem::take(&mut rest).split_at_mut(hi - lo);
        rest = tail;
        out.push(Chunk {
            start,
            q: per_q.iter_mut().filter_map(Iterator::next).collect(),
            rho: rho_it.as_mut().and_then(Iterator::next),
            u: u_it.as_mut().and_then(Iterator::next),
            slot_start: lo,
            slots: mine,
            extra: extra_it.as_mut().and_then(Iterator::next),
        });
    }
    out
}

#[derive(Clone,Copy,Debug)]
pub struct AbsoluteIntervalOrigin {origin_s:f64,direction_s:f64}
impl AbsoluteIntervalOrigin {
    pub fn new(origin_s:f64)->CaeResult<Self>{if !origin_s.is_finite(){return Err(CaeError::contract("absolute interval origin must be finite"));}Ok(Self{origin_s,direction_s:0.})}
    pub fn with_direction(mut self,direction_s:f64)->CaeResult<Self>{if !direction_s.is_finite(){return Err(CaeError::contract("absolute interval direction must be finite"));}self.direction_s=direction_s;Ok(self)}
    pub fn origin_s(&self)->f64{self.origin_s}
    pub fn direction_s(&self)->f64{self.direction_s}
}
pub struct MovingLbm<const Q: usize, L: Lattice<Q>> {
    absolute_clock: Option<AbsoluteIntervalOrigin>,
    config: MovingLbmConfig,
    frame: Frame,
    topology: Topology<Q>,
    sponge: SpongeField,
    collision: Collision<Q, L>,
    carrier: Arc<dyn LagrangianCarrier>,
    observables: Vec<Observable>,
    names: Vec<String>,
    fluid: Vec<bool>,
    stencil: Option<wale::Stencil>,
    observed: Vec<usize>,
    plain: Vec<bool>,
    runs: bool,
    adjoint_cache: AdjointCache,
    buffers: BufferPool,
}

impl<const Q: usize, L: Lattice<Q>> std::fmt::Debug for MovingLbm<Q, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MovingLbm")
            .field("lattice", &L::NAME)
            .field("shape", &self.config.shape)
            .field("collision", &self.config.collision)
            .field("coupling_law", &self.config.coupling_law)
            .finish_non_exhaustive()
    }
}

fn positive(v: f64, what: &str) -> CaeResult<()> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(CaeError::contract(format!("{what} must be finite and positive")))
    }
}

impl<const Q: usize, L: Lattice<Q>> MovingLbm<Q, L> {

    pub fn new(config: MovingLbmConfig, carrier: Arc<dyn LagrangianCarrier>) -> CaeResult<Self> {
        positive(config.spacing_m, "spacing_m")?;
        positive(config.density_kg_m3, "density_kg_m3")?;
        positive(config.kinematic_viscosity_m2_s, "kinematic_viscosity_m2_s")?;
        positive(config.macro_step_s, "macro_step_s")?;
        positive(config.mach_limit, "mach_limit")?;
        positive(config.lattice_velocity_limit, "lattice_velocity_limit")?;
        positive(config.tau_min, "tau_min")?;
        if config.substeps == 0 {
            return Err(CaeError::contract("substeps must be at least 1"));
        }
        if !(config.saturation_width > 0.0 && config.saturation_width < 0.5) {
            return Err(CaeError::contract("saturation_width must lie in (0, 0.5)"));
        }
        if L::DIMENSIONS == 2 && !(config.shape[2] == 1 && config.periodic[2]) {
            return Err(CaeError::contract(format!(
                "{} requires one periodic lattice layer along z",
                L::NAME
            )));
        }
        if config.body_acceleration_m_s2.iter().chain(&config.initial_velocity_m_s).any(|v| !v.is_finite())
            || !config.initial_pressure_pa.is_finite()
        {
            return Err(CaeError::contract("body acceleration and initial state must be finite"));
        }
        if let CouplingLaw::Brinkman { drag_max_per_s, drag_shape } = config.coupling_law {
            positive(drag_max_per_s, "drag_max_per_s")?;
            positive(drag_shape, "drag_shape")?;
        }
        match config.turbulence {
            Turbulence::Laminar => {}
            Turbulence::Smagorinsky(s) => {
                if !(s.constant.is_finite() && s.constant >= 0.0) {
                    return Err(CaeError::contract("Smagorinsky constant must be finite and non-negative"));
                }
                positive(s.norm_floor, "Smagorinsky norm_floor")?;
            }
            Turbulence::Wale(w) => w.validate()?,
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
        let grid = frame.grid();
        let topology = Topology::with_symmetry::<L>(
            grid,
            config.periodic,
            config.solid_mask.clone(),
            &config.ports,
            &config.symmetry_faces,
        )?;
        let sponge =
            SpongeField::new(grid, config.periodic, &config.sponges, config.origin_m, config.spacing_m)?;
        let collision = Collision::<Q, L>::new(config.collision.clone())?;
        let mut observables = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for (name, spec) in &config.observables {
            if name.trim().is_empty() || names.contains(name) {
                return Err(CaeError::contract(format!(
                    "observable names must be unique and non-empty ({name:?})"
                )));
            }
            if let ObservableSpec::PortPower { port } = spec
                && *port >= config.ports.len()
            {
                return Err(CaeError::contract(format!("observable {name:?}: unknown port")));
            }
            observables.push(Observable::new(
                name,
                spec.clone(),
                &topology,
                config.spacing_m,
                config.origin_m,
            )?);
            names.push(name.clone());
        }
        let fluid: Vec<bool> = topology.wall.iter().map(|w| !w).collect();
        let stencil = matches!(config.turbulence, Turbulence::Wale(_)).then(|| wale::Stencil::new(&topology));
        let mut observed: Vec<usize> = Vec::new();
        if observables.iter().any(|o| matches!(o.spec, ObservableSpec::KineticEnergy)) {
            observed = (0..grid.cells()).filter(|&x| fluid[x]).collect();
        } else {
            for o in &observables {
                observed.extend(o.cells.iter().map(|c| c.0));
            }
            observed.sort_unstable();
            observed.dedup();
        }
        let plain = (0..grid.cells())
            .map(|x| {
                fluid[x] && topology.simple[x] && topology.port_slot[x] == NONE && sponge.slot[x] == u32::MAX
            })
            .collect();
        let this = Self {
            config,
            frame,
            topology,
            sponge,
            collision,
            carrier,
            observables,
            names,
            fluid,
            stencil,
            observed,
            plain,
            absolute_clock: None,
            runs: true,
            adjoint_cache: std::sync::Mutex::new(None),
            buffers: std::sync::Mutex::new(Pool { pool: Vec::new(), cap: 4 }),
        };

        let state_bytes = (this.state_size() * size_of::<f64>()) as u64;
        let stored = usize::try_from(this.config.inner_checkpoint_bytes / state_bytes.max(1))
            .unwrap_or(usize::MAX)
            .min(this.config.substeps);
        if let Ok(mut p) = this.buffers.lock() {
            p.cap = 4 + stored;
        }
        this.admit_time_scale(1.0)?;
        Ok(this)
    }

    #[must_use]
    pub fn with_absolute_interval_origin(mut self,clock:AbsoluteIntervalOrigin)->Self{self.absolute_clock=Some(clock);if let Ok(mut cache)=self.adjoint_cache.lock(){*cache=None;}self}

    pub fn config(&self) -> &MovingLbmConfig {
        &self.config
    }

    #[must_use]
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    #[must_use]
    pub fn topology(&self) -> &Topology<Q> {
        &self.topology
    }

    #[must_use]
    pub fn carrier(&self) -> &Arc<dyn LagrangianCarrier> {
        &self.carrier
    }

    #[must_use]
    pub fn cells(&self) -> usize {
        self.topology.cells()
    }

    #[must_use]
    pub fn state_size(&self) -> usize {
        Q * self.cells()
    }

    #[must_use]
    pub fn trace_size(&self) -> usize {
        self.carrier.trace_size()
    }

    #[must_use]
    pub fn design_size(&self) -> usize {
        self.carrier.design_size()
    }

    #[must_use]
    pub fn sample_names(&self) -> &[String] {
        &self.names
    }

    #[must_use]
    pub fn has_second_order(&self) -> bool {
        self.config.interpolation_kernel == pushforward::InterpolationKernel::Cubic
            && self.collision.has_second_order()
            && !matches!(self.config.turbulence, Turbulence::Wale(_))
            && self.carrier.second_order().is_some()
    }

    pub fn interpolation_identity(&self) -> serde_json::Value {
        serde_json::json!({"version": 1, "owner": "moving_lbm_pushforward", "kernel": self.config.interpolation_kernel.as_str(), "width_cells": self.config.kernel_width_cells, "second_order": self.config.interpolation_kernel == pushforward::InterpolationKernel::Cubic})
    }

    #[must_use]
    pub fn nominal_fluid_step_s(&self) -> f64 {
        self.config.macro_step_s / self.config.substeps as f64
    }

    #[must_use]
    pub fn tau_plus(&self, time_scale: f64) -> f64 {
        let c = &self.config;
        0.5 + 3.0 * c.kinematic_viscosity_m2_s * self.nominal_fluid_step_s() * time_scale
            / (c.spacing_m * c.spacing_m)
    }


    pub fn admit_time_scale(&self, time_scale: f64) -> CaeResult<()> {
        if !(time_scale.is_finite() && time_scale > 0.0) {
            return Err(CaeError::contract("time scale must be finite and positive"));
        }
        if let Some(clock)=self.absolute_clock {if !(clock.origin_s+self.config.macro_step_s*time_scale).is_finite(){return Err(CaeError::contract("absolute interval end overflow"));}}
        let tau = self.tau_plus(time_scale);
        if tau <= 0.5 + self.config.tau_min {
            return Err(CaeError::contract(format!(
                "moving LBM admission refused (relaxation_time): tau_plus = {tau:.6} is not above 1/2 + tau_min = {}",
                0.5 + self.config.tau_min
            )));
        }
        let c = self.config.spacing_m / (self.nominal_fluid_step_s() * time_scale);
        let limit = self.config.lattice_velocity_limit;
        let mut worst: f64 = self.config.initial_velocity_m_s.iter().map(|v| v * v).sum::<f64>().sqrt();
        for p in &self.config.ports {
            if let PortKind::Velocity { mean_m_s, amplitude_m_s, profile } = &p.kind {
                let pmax = profile.iter().fold(1.0f64, |a, &b| a.max(b.abs()));
                let v: f64 =
                    (0..3).map(|d| (mean_m_s[d].abs() + amplitude_m_s[d].abs()).powi(2)).sum::<f64>().sqrt();
                worst = worst.max(v * pmax);
            }
        }
        for s in &self.config.sponges {
            let a = s.wave.as_ref().map_or([0.0; 3], |w| w.amplitude_m_s);
            worst =
                worst.max((0..3).map(|d| (s.velocity_m_s[d].abs() + a[d].abs()).powi(2)).sum::<f64>().sqrt());
        }
        if worst / c > limit {
            return Err(CaeError::contract(format!(
                "moving LBM admission refused (lattice_velocity): prescribed velocity {worst:.6e} m/s is {:.4} in \
                 lattice units, above the limit {limit}",
                worst / c
            )));
        }
        Ok(())
    }


    pub fn blocking_saturation(&self, trace: &[f64], design: &[f64]) -> CaeResult<BlockingSaturation> {
        let end = self.endpoint::<f64>(trace, design, None, false)?;
        let v = vec![[0.0; 3]; end.points.a.len()];
        let occ = pushforward::occupancy(&self.frame, [&end, &end], &v);
        let kappa = self.config.saturation_width;
        let t = self.tau_plus(1.0) - 0.5;
        let max_fill = occ.d[0].iter().copied().fold(0.0f64, f64::max);
        let saturated_cells = occ.d[0].iter().filter(|&&d| d >= 1.0 + kappa).count();
        let active_cells = occ.d[0].iter().filter(|&&d| d > 0.0).count();
        let max_blocking = if max_fill > 0.0 {
            let (b, _, _) = super::psm::beta(max_fill, t, kappa);
            b * max_fill
        } else {
            0.0
        };
        Ok(BlockingSaturation {
            max_fill,
            required_fill: 1.0 + kappa,
            saturated_cells,
            active_cells,
            max_blocking,
            tau_plus: t + 0.5,
        })
    }


    pub fn admit_blocking_saturation(
        &self,
        trace: &[f64],
        full_design: &[f64],
    ) -> CaeResult<BlockingSaturation> {
        let r = self.blocking_saturation(trace, full_design)?;
        if !r.admitted() {
            return Err(CaeError::contract(format!(
                "moving LBM admission refused (blocking_saturation): the fully dense solid pushes forward to a \
                 largest fill of {:.4}, below 1 + saturation_width = {:.4}; its cells never block fully \
                 (largest blocking weight {:.4} at tau_plus {:.4}); scale the blocking weights (saturation \
                 margin of at least {:.3})",
                r.max_fill,
                r.required_fill,
                r.max_blocking,
                r.tau_plus,
                r.required_fill / r.max_fill.max(f64::MIN_POSITIVE)
            )));
        }
        Ok(r)
    }

    #[must_use]
    pub fn initial_state(&self) -> Vec<f64> {
        let n = self.cells();
        let c = self.config.spacing_m / self.nominal_fluid_step_s();
        let rho = 1.0 + 3.0 * self.config.initial_pressure_pa / (self.config.density_kg_m3 * c * c);
        let u = self.config.initial_velocity_m_s.map(|v| v / c);
        let eq = equilibrium::<f64, Q, L>(rho, u);
        let mut out = vec![0.0; Q * n];
        for i in 0..Q {
            out[i * n..(i + 1) * n].fill(eq[i]);
        }
        out
    }

    #[must_use]
    pub fn macroscopic(&self, state: &[f64]) -> (Vec<f64>, Vec<[f64; 3]>) {
        let n = self.cells();
        (0..n)
            .into_par_iter()
            .map(|x| {
                if !self.fluid[x] || state.len() != Q * n {
                    return (0.0, [0.0; 3]);
                }
                let f: [f64; Q] = std::array::from_fn(|i| state[i * n + x]);
                let (rho, j) = moments::<f64, Q, L>(&f);
                (rho, j.map(|v| v / rho))
            })
            .unzip()
    }

    fn params_fixed_grid<S: Scalar>(&self, tau_s: S, n: usize, k: usize) -> Params<S> {
        let cfg = &self.config;
        let m = cfg.substeps;
        let dx = cfg.spacing_m;
        let dt_f = tau_s * self.nominal_fluid_step_s();
        let c = S::one() / dt_f * dx;
        let tau = S::from_f64(0.5) + dt_f * (3.0 * cfg.kinematic_viscosity_m2_s / (dx * dx));
        let omega = S::one() / tau;
        let accel = cfg.body_acceleration_m_s2.map(|a| dt_f * dt_f * (a / dx));
        let c_alpha = match cfg.coupling_law {
            CouplingLaw::Brinkman { drag_max_per_s, drag_shape } => dt_f * (drag_max_per_s * drag_shape),
            CouplingLaw::Psm | CouplingLaw::PsmSuperposition => S::zero(),
        };
        let index = (n.saturating_sub(1) * m + k) as f64;
        let t = dt_f * index;
        let to_density = |p: S| S::one() + p * 3.0 / (c * c * cfg.density_kg_m3);
        let ports = cfg
            .ports
            .iter()
            .map(|p| {
                let (ramp, wave) = p.signal.factors(t);
                match &p.kind {
                    PortKind::Velocity { mean_m_s, amplitude_m_s, .. } => {
                        PortValue::Velocity(std::array::from_fn(|d| {
                            ramp * (wave * amplitude_m_s[d] + mean_m_s[d]) / c
                        }))
                    }
                    PortKind::Pressure { mean_pa, amplitude_pa } => {
                        PortValue::Density(to_density(ramp * (wave * *amplitude_pa + *mean_pa)))
                    }
                }
            })
            .collect();
        let targets = cfg
            .sponges
            .iter()
            .map(|s| {
                let (sin, cos) = match &s.wave {
                    None => ([S::zero(); 3], [S::zero(); 3]),
                    Some(w) => {
                        let (ramp, _) = w.signal.factors(t);
                        let arg =
                            t * (2.0 * std::f64::consts::PI * w.signal.frequency_hz) + w.signal.phase_rad;
                        let (sw, cw) = (ramp * arg.sin() / c, ramp * arg.cos() / c);
                        (w.amplitude_m_s.map(|a| sw * a), w.amplitude_m_s.map(|a| cw * a))
                    }
                };
                Target {
                    rho: to_density(S::from_f64(s.pressure_pa)),
                    mean: s.velocity_m_s.map(|v| S::from_f64(v) / c),
                    sin,
                    cos,
                }
            })
            .collect();
        Params {
            cell: CellParameters { omega, accel, c_alpha },
            ports,
            targets,
            scales: SampleScales {
                density: cfg.density_kg_m3,
                spacing: dx,
                velocity: c,
                macro_step: dt_f * m as f64,
                fluid_step: dt_f,
            },
            impulse_scale: S::one() / dt_f * (cfg.density_kg_m3 * dx.powi(4)),
            theta: (k as f64 - 0.5) / m as f64,
        }
    }

    fn params_absolute<S: Scalar>(&self, tau_s: S, origin: S, k: usize) -> Params<S> {
        let cfg = &self.config;
        let m = cfg.substeps;
        let dx = cfg.spacing_m;
        let dt_f = tau_s * self.nominal_fluid_step_s();
        let c = S::one() / dt_f * dx;
        let tau = S::from_f64(0.5) + dt_f * (3.0 * cfg.kinematic_viscosity_m2_s / (dx * dx));
        let omega = S::one() / tau;
        let accel = cfg.body_acceleration_m_s2.map(|a| dt_f * dt_f * (a / dx));
        let c_alpha = match cfg.coupling_law {
            CouplingLaw::Brinkman { drag_max_per_s, drag_shape } => dt_f * (drag_max_per_s * drag_shape),
            CouplingLaw::Psm | CouplingLaw::PsmSuperposition => S::zero(),
        };
        let t = origin + dt_f * k as f64;
        let to_density = |p: S| S::one() + p * 3.0 / (c * c * cfg.density_kg_m3);
        let ports = cfg
            .ports
            .iter()
            .map(|p| {
                let (ramp, wave) = p.signal.factors(t);
                match &p.kind {
                    PortKind::Velocity { mean_m_s, amplitude_m_s, .. } => {
                        PortValue::Velocity(std::array::from_fn(|d| {
                            ramp * (wave * amplitude_m_s[d] + mean_m_s[d]) / c
                        }))
                    }
                    PortKind::Pressure { mean_pa, amplitude_pa } => {
                        PortValue::Density(to_density(ramp * (wave * *amplitude_pa + *mean_pa)))
                    }
                }
            })
            .collect();
        let targets = cfg
            .sponges
            .iter()
            .map(|s| {
                let (sin, cos) = match &s.wave {
                    None => ([S::zero(); 3], [S::zero(); 3]),
                    Some(w) => {
                        let (ramp, _) = w.signal.factors(t);
                        let arg =
                            t * (2.0 * std::f64::consts::PI * w.signal.frequency_hz) + w.signal.phase_rad;
                        let (sw, cw) = (ramp * arg.sin() / c, ramp * arg.cos() / c);
                        (w.amplitude_m_s.map(|a| sw * a), w.amplitude_m_s.map(|a| cw * a))
                    }
                };
                Target {
                    rho: to_density(S::from_f64(s.pressure_pa)),
                    mean: s.velocity_m_s.map(|v| S::from_f64(v) / c),
                    sin,
                    cos,
                }
            })
            .collect();
        Params {
            cell: CellParameters { omega, accel, c_alpha },
            ports,
            targets,
            scales: SampleScales {
                density: cfg.density_kg_m3,
                spacing: dx,
                velocity: c,
                macro_step: dt_f * m as f64,
                fluid_step: dt_f,
            },
            impulse_scale: S::one() / dt_f * (cfg.density_kg_m3 * dx.powi(4)),
            theta: (k as f64 - 0.5) / m as f64,
        }
    }

    fn params<S: Lane>(&self, tau_s: S, n: usize, k: usize) -> Params<S> {
        match self.absolute_clock {
            None => self.params_fixed_grid(tau_s,n,k),
            Some(clock) => self.params_absolute(tau_s,S::make(clock.origin_s,clock.direction_s),k),
        }
    }
    fn params_with_derivative<S: Scalar>(&self, tau_s: f64, n: usize, k: usize) -> (Params<S>, Params<f64>) {
        let p = match self.absolute_clock {None=>self.params_fixed_grid(Dual::<1>::new(tau_s,[1.0]),n,k),Some(clock)=>self.params_absolute(Dual::<1>::new(tau_s,[1.0]),Dual::<1>::new(clock.origin_s,[0.]),k)};
        (p.map(|v| S::from_f64(v.re)), p.map(|v| v.eps[0]))
    }

    fn endpoint<S: Lane>(
        &self,
        trace: &[f64],
        design: &[f64],
        direction: Option<(&[f64], Option<&[f64]>)>,
        gradients: bool,
    ) -> CaeResult<Endpoint<S>> {
        if trace.len() != self.trace_size() || design.len() != self.design_size() {
            return Err(CaeError::contract("moving LBM: trace or design length does not match the carrier"));
        }
        let cloud = self.carrier.points(trace, design)?;
        let count = self.carrier.point_count();
        if cloud.positions.len() != count || cloud.weights.len() != count {
            return Err(CaeError::contract("moving LBM: carrier returned a point cloud of the wrong size"));
        }
        let tangent = match direction {
            Some((dt, dd)) if S::DIRECTIONAL => Some(self.carrier.points_jvp(trace, design, dt, dd)?),
            _ => None,
        };
        let dx = self.config.spacing_m;
        let vol = dx * dx * dx;
        let xi: Vec<[S; 3]> = (0..count)
            .map(|q| {
                let x: [S; 3] = std::array::from_fn(|a| {
                    S::make(cloud.positions[q][a], tangent.as_ref().map_or(0.0, |t| t.positions[q][a]))
                });
                self.frame.to_lattice(x)
            })
            .collect();
        let a: Vec<S> = (0..count)
            .map(|q| {
                let w = cloud.weights[q];
                if !(w.is_finite() && w >= 0.0) {
                    return S::make(f64::NAN, 0.0);
                }
                S::make(w / vol, tangent.as_ref().map_or(0.0, |t| t.weights[q] / vol))
            })
            .collect();
        if a.iter().any(|v| !v.value().is_finite()) {
            return Err(CaeError::contract("moving LBM: carrier weights must be finite and non-negative"));
        }
        Endpoint::new(&self.frame, PointSet { xi, a }, gradients)
    }

    #[allow(clippy::too_many_arguments)]
    fn setup<S: Lane>(
        &self,
        trace_start: &[f64],
        trace_end: &[f64],
        design: &[f64],
        d_start: Option<&[f64]>,
        d_end: Option<&[f64]>,
        d_design: Option<&[f64]>,
        gradients: bool,
    ) -> CaeResult<Macro<S>> {
        let zeros = vec![0.0; trace_start.len()];
        let e0 =
            self.endpoint::<S>(trace_start, design, Some((d_start.unwrap_or(&zeros), d_design)), gradients)?;
        let e1 =
            self.endpoint::<S>(trace_end, design, Some((d_end.unwrap_or(&zeros), d_design)), gradients)?;
        let scale = self.config.substeps as f64 * self.config.spacing_m;
        let delta: Vec<f64> = trace_end.iter().zip(trace_start).map(|(e, s)| e - s).collect();
        let vv = self.carrier.flux_to_impulses(&delta, scale);
        let vt = if S::DIRECTIONAL {
            let dd: Vec<f64> = (0..delta.len())
                .map(|i| d_end.map_or(0.0, |d| d[i]) - d_start.map_or(0.0, |d| d[i]))
                .collect();
            Some(self.carrier.flux_to_impulses(&dd, scale))
        } else {
            None
        };
        if vv.len() != self.carrier.point_count() {
            return Err(CaeError::contract("moving LBM: carrier velocity map has the wrong size"));
        }
        let v: Vec<[S; 3]> = (0..vv.len())
            .map(|q| std::array::from_fn(|d| S::make(vv[q][d], vt.as_ref().map_or(0.0, |t| t[q][d]))))
            .collect();
        let mut displacement: f64 = 0.0;
        for q in 0..vv.len() {
            let mut s = 0.0;
            for d in 0..3 {
                let r = e1.points.xi[q][d].value() - e0.points.xi[q][d].value();
                s += r * r;
            }
            displacement = displacement.max(s.sqrt());
        }
        let limit = 0.5 * self.config.kernel_width_cells as f64;
        if displacement > limit {
            return Err(CaeError::contract(format!(
                "moving LBM admission refused (displacement_per_step): a push-forward point moves {displacement:.4} \
                 cells in one macro step, above half the kernel width ({limit})"
            )));
        }
        let occ = pushforward::occupancy(&self.frame, [&e0, &e1], &v);
        for o in &self.observables {
            for &x in &o.exterior_cells {
                let slot = occ.slot[x];
                if slot != NONE && (occ.d[0][slot as usize].value() != 0.0 || occ.d[1][slot as usize].value() != 0.0) {
                    return Err(CaeError::contract("section_mass_flux requires a wholly exterior face at both carrier endpoints"));
                }
            }
        }
        Ok(Macro { ends: [e0, e1], v, occ, displacement })
    }

    #[inline]
    fn port_value<S: Scalar>(&self, p: &Params<S>, r: usize) -> PortValue<S> {
        let pc = &self.topology.ports[r];
        match p.ports[pc.port] {
            PortValue::Velocity(u) => PortValue::Velocity(u.map(|v| v * pc.profile)),
            PortValue::Density(d) => PortValue::Density(d),
        }
    }

    #[inline]
    fn pre<S: Scalar>(&self, g: &[S], x: usize, p: &Params<S>) -> [S; Q] {
        let r = self.topology.port_slot[x];
        if r == NONE {
            self.topology.pull_cell(g, x)
        } else {
            let pc = &self.topology.ports[r as usize];
            let nb = self.topology.pull_cell(g, pc.neighbour);
            port_populations::<S, Q, L>(&nb, self.port_value(p, r as usize))
        }
    }

    #[inline]
    fn solid_at<S: Scalar>(occ: &Occupancy<S>, x: usize, theta: f64) -> Option<(usize, SolidInput<S>)> {
        let s = occ.slot[x];
        if s == NONE {
            return None;
        }
        let s = s as usize;
        let d = occ.d[0][s] * (1.0 - theta) + occ.d[1][s] * theta;
        let m = std::array::from_fn(|e| occ.m[0][s][e] * (1.0 - theta) + occ.m[1][s][e] * theta);
        Some((s, SolidInput { d, m }))
    }

    #[inline]
    fn solid_fraction<S: Scalar>(occ: &Occupancy<S>, x: usize, theta: f64, kappa: f64) -> S {
        let s = occ.slot[x];
        if s == NONE {
            return S::zero();
        }
        let s = s as usize;
        saturate(occ.d[0][s] * (1.0 - theta) + occ.d[1][s] * theta, kappa)
    }

    fn les(&self) -> Option<&Smagorinsky> {
        match &self.config.turbulence {
            Turbulence::Smagorinsky(s) => Some(s),
            _ => None,
        }
    }

    fn wale_rates<S: Scalar>(&self, g: &[S], p: &Params<S>) -> (Vec<S>, Vec<[S; 3]>) {
        let (Turbulence::Wale(model), Some(stencil)) = (&self.config.turbulence, &self.stencil) else {
            return (Vec::new(), Vec::new());
        };
        let n = self.cells();
        let u: Vec<[S; 3]> = (0..n)
            .into_par_iter()
            .map(|x| {
                if !self.fluid[x] {
                    return [S::zero(); 3];
                }
                let f = self.pre(g, x, p);
                let (rho, j) = moments::<S, Q, L>(&f);
                j.map(|v| v / rho)
            })
            .collect();
        let tau0 = S::one() / p.cell.omega;
        let rates = (0..n)
            .into_par_iter()
            .map(|x| {
                if !self.fluid[x] {
                    return p.cell.omega;
                }
                wale::omega(model, &stencil.gradient(&u, x), tau0)
            })
            .collect();
        (rates, u)
    }

    fn run_rates<S: Scalar>(&self, p: &Params<S>) -> Option<kernels::Rates<S>> {
        if !self.runs || !matches!(self.config.turbulence, Turbulence::Laminar) {
            return None;
        }
        let omega = p.cell.omega;
        match &self.config.collision {
            CollisionKind::Bgk => Some(kernels::Rates { plus: omega, minus: omega, dminus: S::one() }),
            CollisionKind::Trt { magic } => {
                let tau = S::one() / omega;
                let tp = tau - 0.5;
                let tm = S::from_f64(0.5) + S::from_f64(*magic) / tp;
                Some(kernels::Rates {
                    plus: omega,
                    minus: S::one() / tm,
                    dminus: -(tau * tau * *magic) / (tm * tm * tp * tp),
                })
            }
            _ => None,
        }
    }

    #[inline]
    fn run_length<S>(
        &self,
        occ: &Occupancy<S>,
        start: usize,
        local: usize,
        len: usize,
        solids: bool,
        max: usize,
    ) -> usize {
        let x = start + local;
        let inside = occ.slot[x] != NONE;
        if !self.plain[x] || (inside && !solids) {
            return 0;
        }
        let mut end = local + 1;
        while end < len
            && end - local < max
            && self.plain[start + end]
            && (occ.slot[start + end] != NONE) == inside
        {
            end += 1;
        }
        end - local
    }

    #[allow(clippy::too_many_lines)]
    fn substep<S: Lane>(
        &self,
        prev: &[S],
        out: &mut [S],
        p: &Params<S>,
        occ: &Occupancy<S>,
        fields: Option<&mut Fields<S>>,
        dp: &mut [[S; 3]],
    ) -> f64 {
        let n = self.cells();
        let theta = p.theta;
        let kappa = self.config.saturation_width;
        let law = self.config.coupling_law;
        let les = self.les();
        let (rates, _) = self.wale_rates(prev, p);

        let fast = self.run_rates(p);
        let psm_runs = matches!(law, CouplingLaw::Psm | CouplingLaw::PsmSuperposition);
        let (rho_f, u_f) = match fields {
            Some(fl) => (Some(fl.rho.as_mut_slice()), Some(fl.u.as_mut_slice())),
            None => (None, None),
        };
        let chunks = split_state(out, n, Q, rho_f, u_f, dp, &occ.cells, None);
        let speeds: Vec<f64> = chunks
            .into_par_iter()
            .map(|mut ch| {
                let mut vmax: f64 = 0.0;
                let len = ch.q.first().map_or(0, |s| s.len());
                let mut local = 0;
                while local < len {
                    let x = ch.start + local;
                    if let Some(rates) = fast {
                        let run = self.run_length(
                            occ,
                            ch.start,
                            local,
                            len,
                            psm_runs,
                            kernels::forward_lanes::<S>(),
                        );
                        if run > 0 {
                            let solid = occ.slot[x] != NONE;
                            let s0 = occ.slot[x] as usize;
                            let sr = solid.then(|| kernels::SolidRun {
                                d: [&occ.d[0][s0..s0 + run], &occ.d[1][s0..s0 + run]],
                                m: [&occ.m[0][s0..s0 + run], &occ.m[1][s0..s0 + run]],
                                theta,
                                kappa,
                                superposition: law == CouplingLaw::PsmSuperposition,
                            });
                            let dp_run = if solid {
                                let r = s0 - ch.slot_start;
                                Some(&mut ch.slots[r..r + run])
                            } else {
                                None
                            };
                            let v = kernels::forward_run::<S, Q, L>(
                                prev,
                                n,
                                x,
                                run,
                                &self.topology.offsets,
                                rates,
                                p.cell.accel,
                                &mut ch.q,
                                local,
                                ch.rho.as_deref_mut(),
                                ch.u.as_deref_mut(),
                                sr.as_ref().zip(dp_run),
                            );
                            vmax = vmax.max(v);
                            local += run;
                            continue;
                        }
                    }
                    local += 1;
                    let local = local - 1;
                    if !self.fluid[x] {
                        for i in 0..Q {
                            ch.q[i][local] = prev[i * n + x];
                        }
                        continue;
                    }
                    let f = self.pre(prev, x, p);
                    let solid = Self::solid_at(occ, x, theta);
                    let mut cp = p.cell;
                    if !rates.is_empty() {
                        cp.omega = rates[x];
                    }
                    let res = cell_update(&self.collision, law, kappa, les, &f, &cp, solid.map(|s| s.1));
                    let mut post = res.post;
                    let sv = self.sponge.slot[x];
                    if sv != u32::MAX {
                        let sigma = self.sponge.sigma[x];
                        let mut target = [S::zero(); Q];
                        let phases = &self.sponge.phases[sv as usize];
                        for (&(l, sl), &phase) in self.sponge.parts[sv as usize].iter().zip(phases) {
                            let t = &p.targets[l];
                            let e = equilibrium::<S, Q, L>(t.rho, t.velocity(phase));
                            for i in 0..Q {
                                target[i] += e[i] * sl;
                            }
                        }
                        for i in 0..Q {
                            post[i] = post[i] * (1.0 - sigma) + target[i];
                        }
                    }
                    for i in 0..Q {
                        ch.q[i][local] = post[i];
                    }
                    if let Some((s, _)) = solid {
                        ch.slots[s - ch.slot_start] = res.dp;
                    }
                    if let Some(r) = ch.rho.as_deref_mut() {
                        r[local] = res.rho;
                    }
                    if let Some(u) = ch.u.as_deref_mut() {
                        u[local] = res.u;
                    }
                    let sp = res.u.iter().map(|v| v.value() * v.value()).sum::<f64>();
                    vmax = vmax.max(sp);
                }
                vmax.sqrt()
            })
            .collect();
        speeds.into_iter().fold(0.0, f64::max)
    }

    fn accumulate<S: Scalar>(
        &self,
        acc: &mut Accum<S>,
        occ: &Occupancy<S>,
        p: &Params<S>,
        dp: &[[S; 3]],
        fields: Option<&Fields<S>>,
        k: usize,
    ) {
        let theta = p.theta;

        let [g0, g1] = &mut acc.g;
        let partial: Vec<(f64, [S; 3])> = g0
            .par_chunks_mut(RED)
            .zip(g1.par_chunks_mut(RED))
            .enumerate()
            .map(|(chunk, (a0, a1))| {
                let mut pairing = 0.0;
                let mut impulse = [S::zero(); 3];
                for (r, (e0, e1)) in a0.iter_mut().zip(a1.iter_mut()).enumerate() {
                    let s = chunk * RED + r;
                    let d = occ.d[0][s] * (1.0 - theta) + occ.d[1][s] * theta;
                    let mut work = S::zero();
                    for e in 0..3 {
                        let gv = -dp[s][e] / d;
                        e0[e] += gv * (1.0 - theta);
                        e1[e] += gv * theta;
                        impulse[e] += dp[s][e];
                        let m = occ.m[0][s][e] * (1.0 - theta) + occ.m[1][s][e] * theta;
                        work += gv * m;
                    }
                    pairing += work.value();
                }
                (pairing, impulse)
            })
            .collect();
        let mut pairing = 0.0;
        for (w, imp) in partial {
            pairing += w;
            for e in 0..3 {
                acc.impulse[e] += imp[e];
            }
        }

        let c = p.scales.velocity.value();
        let dx = self.config.spacing_m;
        acc.pairing += pairing * self.config.density_kg_m3 * dx * dx * dx * c * c;
        let m = self.config.substeps;
        let sampled = match self.config.sampling {
            Sampling::End => k == m,
            Sampling::MacroMean => true,
        };
        let weight = match self.config.sampling {
            Sampling::End => 1.0,
            Sampling::MacroMean => 1.0 / m as f64,
        };
        if let Some(fl) = fields {
            let mut dpsum = [S::zero(); 3];
            for v in dp {
                for e in 0..3 {
                    dpsum[e] += v[e];
                }
            }
            if sampled {
                for (o, s) in self.observables.iter().zip(acc.samples.iter_mut()) {
                    if !o.reads_cells() {
                        continue;
                    }
                    *s += o.value(&p.scales, &fl.rho, &fl.u, &self.fluid, dpsum) * weight;
                }
            }
            if k == m {
                let ke = kinetic(&fl.rho, &fl.u, &self.fluid);
                acc.kinetic = ke * self.config.density_kg_m3 * c * c * dx * dx * dx;
            }
        }
    }

    fn new_accum<S: Scalar>(&self, occ: &Occupancy<S>) -> Accum<S> {
        Accum {
            g: [vec![[S::zero(); 3]; occ.cells.len()], vec![[S::zero(); 3]; occ.cells.len()]],
            samples: vec![S::zero(); self.observables.len()],
            pairing: 0.0,
            impulse: [S::zero(); 3],
            max_speed: 0.0,
            kinetic: 0.0,
        }
    }

    fn needs_fields(&self, k: usize) -> bool {
        k == self.config.substeps
            || (self.config.sampling == Sampling::MacroMean && !self.observables.is_empty())
    }

    #[allow(clippy::too_many_arguments)]
    fn advance<S: Lane>(
        &self,
        n: usize,
        state: &mut Vec<S>,
        from: usize,
        to: usize,
        tau_s: S,
        mac: &Macro<S>,
        acc: Option<&mut Accum<S>>,
        scratch: &mut Vec<S>,
    ) {
        self.advance_from(n, None, state, from, to, tau_s, mac, acc, scratch);
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_from<S: Lane>(
        &self,
        n: usize,
        initial: Option<&[S]>,
        state: &mut Vec<S>,
        from: usize,
        to: usize,
        tau_s: S,
        mac: &Macro<S>,
        mut acc: Option<&mut Accum<S>>,
        scratch: &mut Vec<S>,
    ) {
        let cells = self.cells();
        let mut dp = vec![[S::zero(); 3]; mac.occ.cells.len()];
        let mut buffer: Option<Fields<S>> = None;
        for k in from + 1..=to {
            let p = self.params(tau_s, n, k);
            let want = acc.is_some() && self.needs_fields(k);
            if want && buffer.is_none() {
                buffer = Some(Fields { rho: vec![S::zero(); cells], u: vec![[S::zero(); 3]; cells] });
            }
            let mut fields = if want { buffer.take() } else { None };
            let speed = match initial {
                Some(input) if k == from + 1 => {
                    state.resize(input.len(), S::zero());
                    self.substep(input, state, &p, &mac.occ, fields.as_mut(), &mut dp)
                }
                _ => {
                    scratch.resize(state.len(), S::zero());
                    let v = self.substep(state, scratch, &p, &mac.occ, fields.as_mut(), &mut dp);
                    std::mem::swap(state, scratch);
                    v
                }
            };
            if let Some(a) = acc.as_deref_mut() {
                a.max_speed = a.max_speed.max(speed);

                let sampled = match self.config.sampling {
                    Sampling::End => k == self.config.substeps,
                    Sampling::MacroMean => true,
                };
                if sampled {
                    let weight = if self.config.sampling == Sampling::MacroMean {
                        1.0 / self.config.substeps as f64
                    } else {
                        1.0
                    };
                    let mut dpsum = [S::zero(); 3];
                    for v in &dp {
                        for e in 0..3 {
                            dpsum[e] += v[e];
                        }
                    }
                    for (o, s) in self.observables.iter().zip(a.samples.iter_mut()) {
                        if o.reads_populations() {
                            let prior = match initial {
                                Some(input) if k == from + 1 => input,
                                _ => scratch.as_slice(),
                            };
                            *s += o.population_value(&p.scales, prior) * weight;
                        } else if o.reads_occupancy() {
                            let kappa = self.config.saturation_width;
                            *s += o.open_area(
                                |x| Self::solid_fraction(&mac.occ, x, p.theta, kappa),
                                None,
                                |_, _| {},
                            ) * weight;
                        } else if !o.reads_cells() {
                            *s += o.value(&p.scales, &[], &[], &self.fluid, dpsum) * weight;
                        }
                    }
                }
                self.accumulate(a, &mac.occ, &p, &dp, fields.as_ref(), k);
            }
            if fields.is_some() {
                buffer = fields;
            }
        }
    }

    fn flux_of<S: Lane>(&self, mac: &Macro<S>, acc: &Accum<S>, tau_s: S) -> CaeResult<(Vec<S>, Vec<[S; 3]>)> {
        let p = self.params(tau_s, 1, 1);
        let i0 = pushforward::endpoint_impulses(&self.frame, &mac.ends[0], &mac.occ.slot, &acc.g[0]);
        let i1 = pushforward::endpoint_impulses(&self.frame, &mac.ends[1], &mac.occ.slot, &acc.g[1]);
        let impulses: Vec<[S; 3]> = i0
            .iter()
            .zip(&i1)
            .map(|(a, b)| std::array::from_fn(|e| (a[e] + b[e]) * p.impulse_scale))
            .collect();
        let dt_s = p.scales.macro_step;
        let iv: Vec<[f64; 3]> = impulses.iter().map(|v| v.map(|s| s.value())).collect();
        let fv = self.carrier.impulses_to_flux(&iv, dt_s.value());
        if fv.len() != self.trace_size() {
            return Err(CaeError::contract("moving LBM: carrier flux has the wrong size"));
        }
        let flux = if S::DIRECTIONAL {
            let it: Vec<[f64; 3]> = impulses.iter().map(|v| v.map(|s| s.tangent())).collect();
            let ft = self.carrier.impulses_to_flux(&it, dt_s.value());
            let rate = dt_s.tangent() / dt_s.value();
            fv.iter().zip(&ft).map(|(v, t)| S::make(*v, t - v * rate)).collect()
        } else {
            fv.iter().map(|v| S::make(*v, 0.0)).collect()
        };
        Ok((flux, impulses))
    }

    fn check_state(&self, state: &[f64], max_speed: f64) -> CaeResult<f64> {
        let n = self.cells();
        let (min, finite) = state
            .par_chunks(n)
            .map(|row| {
                row.iter()
                    .zip(&self.fluid)
                    .filter(|(_, f)| **f)
                    .fold((f64::INFINITY, true), |(m, ok), (v, _)| (m.min(*v), ok && v.is_finite()))
            })
            .collect::<Vec<(f64, bool)>>()
            .into_iter()
            .fold((f64::INFINITY, true), |(m, ok), (v, o)| (m.min(v), ok && o));
        if !finite {
            return Err(CaeError::convergence(
                "moving LBM admission refused (populations): non-finite population",
            ));
        }
        if min <= 0.0 {
            let index = state.iter().enumerate().filter(|(i, _)| self.fluid[*i % n])
                .min_by(|(_, a), (_, b)| a.total_cmp(b)).map_or(0, |(i, _)| i);
            let cell = index % n;
            let direction = index / n;
            let shape = self.config.shape;
            let ijk = [cell / (shape[1] * shape[2]), (cell / shape[2]) % shape[1], cell % shape[2]];
            let position: [f64; 3] = std::array::from_fn(|a| self.config.origin_m[a] + (ijk[a] as f64 + 0.5) * self.config.spacing_m);
            let density: f64 = (0..Q).map(|i| state[i * n + cell]).sum();
            let velocity: [f64; 3] = std::array::from_fn(|a| (0..Q).map(|i| state[i * n + cell] * L::CF[i][a]).sum::<f64>() / density);
            return Err(CaeError::convergence(format!(
                "moving LBM admission refused (populations): population {min:.3e} is not positive; direction={direction}; cell={cell}; ijk={ijk:?}; position_m={position:?}; density_lattice={density:.16e}; velocity_lattice={velocity:?}; max_speed_lattice={max_speed:.16e}"
            )));
        }
        let mach = max_speed * 3f64.sqrt();
        if mach > self.config.mach_limit {
            return Err(CaeError::convergence(format!(
                "moving LBM admission refused (mach): Mach number {mach:.4} above the limit {}",
                self.config.mach_limit
            )));
        }
        Ok(min)
    }

    fn entrained<S: Scalar>(&self, state: &[S], occ: &Occupancy<S>, e: usize) -> Vec<[S; 3]> {
        let n = self.cells();
        let kappa = self.config.saturation_width;
        occ.cells
            .par_iter()
            .enumerate()
            .map(|(s, &c)| {
                let x = c as usize;
                if !self.fluid[x] {
                    return [S::zero(); 3];
                }
                let f: [S; Q] = std::array::from_fn(|i| state[i * n + x]);
                let (_, j) = moments::<S, Q, L>(&f);
                let (sigma, _) = saturated_ratio(occ.d[e][s], kappa);
                j.map(|v| v * sigma)
            })
            .collect()
    }

    fn compensate<S: Scalar>(
        &self,
        acc: &mut Accum<S>,
        occ: &Occupancy<S>,
        start: &[[S; 3]],
        end: &[[S; 3]],
        velocity_scale: f64,
    ) {
        let mut work = 0.0;
        for s in 0..occ.cells.len() {
            for e in 0..3 {
                acc.g[1][s][e] += end[s][e];
                acc.g[0][s][e] -= start[s][e];
                work +=
                    end[s][e].value() * occ.m[1][s][e].value() - start[s][e].value() * occ.m[0][s][e].value();
            }
        }
        let dx = self.config.spacing_m;
        acc.pairing += work * self.config.density_kg_m3 * dx * dx * dx * velocity_scale * velocity_scale;
    }

    fn compensated(&self) -> bool {
        self.config.entrained_inertia == EntrainedInertia::Compensated
    }

    #[allow(clippy::too_many_arguments)]
    fn entrained_vjp<S: Scalar>(
        &self,
        state_bar: &mut [S],
        occ: &Occupancy<S>,
        e: usize,
        kernel_bar: &[[S; 3]],
        sign: S,
        d_bar: &mut [S],
        value: Option<&[[S; 3]]>,
    ) {
        let n = self.cells();
        let kappa = self.config.saturation_width;
        for (s, &c) in occ.cells.iter().enumerate() {
            let x = c as usize;
            if !self.fluid[x] {
                continue;
            }
            let (sigma, dsigma) = saturated_ratio(occ.d[e][s], kappa);
            let gb = kernel_bar[s];
            for i in 0..Q {
                let cf = L::CF[i];
                state_bar[i * n + x] += sign * sigma * (gb[0] * cf[0] + gb[1] * cf[1] + gb[2] * cf[2]);
            }
            if let Some(v) = value {
                let vg = v[s][0] * gb[0] + v[s][1] * gb[1] + v[s][2] * gb[2];
                d_bar[s] += sign * vg * dsigma / sigma;
            }
        }
    }

    fn check_inputs(
        &self,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        design: &[f64],
    ) -> CaeResult<()> {
        if previous.len() != self.state_size()
            || trace_start.len() != self.trace_size()
            || trace_end.len() != self.trace_size()
            || design.len() != self.design_size()
        {
            return Err(CaeError::contract("moving LBM: state, trace or design length mismatch"));
        }
        Ok(())
    }


    pub fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        design: &[f64],
        time_scale: f64,
    ) -> CaeResult<SubcycleOutput> {
        self.check_inputs(previous, trace_start, trace_end, design)?;
        self.admit_time_scale(time_scale)?;
        let mac = self.setup::<f64>(trace_start, trace_end, design, None, None, None, false)?;
        let mut acc = self.new_accum(&mac.occ);
        let mut state = f64::pooled(&self.buffers, previous.len());
        let mut scratch = f64::pooled(&self.buffers, previous.len());
        let m = self.config.substeps;
        self.advance_from(
            n,
            Some(previous),
            &mut state,
            0,
            m,
            time_scale,
            &mac,
            Some(&mut acc),
            &mut scratch,
        );
        f64::recycle(&self.buffers, scratch);
        let min = self.check_state(&state, acc.max_speed).map_err(|e| CaeError::convergence(format!("{}; macro_step={n}; completed_fluid_substeps={m}", e.message())))?;
        if self.compensated() {
            let start = self.entrained(previous, &mac.occ, 0);
            let end = self.entrained(&state, &mac.occ, 1);
            let c = self.params(time_scale, n, 1).scales.velocity;
            self.compensate(&mut acc, &mac.occ, &start, &end, c);
        }
        let (flux, _) = self.flux_of(&mac, &acc, time_scale)?;
        let mut ledger = BTreeMap::new();
        ledger.insert("fluid_kinetic_energy_J".to_string(), acc.kinetic);
        ledger.insert("interface_work_J".to_string(), acc.pairing);
        let p = self.params(time_scale, n, 1);
        let imp = p.impulse_scale;
        for (e, name) in ["x", "y", "z"].iter().enumerate() {
            ledger.insert(format!("solid_impulse_{name}_N_s"), -acc.impulse[e] * imp);
        }
        ledger.insert("max_mach".to_string(), acc.max_speed * 3f64.sqrt());
        ledger.insert("min_population".to_string(), min);
        ledger.insert("active_cells".to_string(), mac.occ.cells.len() as f64);
        ledger.insert("max_displacement_cells".to_string(), mac.displacement);
        Ok(SubcycleOutput { state, flux, samples: acc.samples, pairing: acc.pairing, ledger })
    }


    #[allow(clippy::too_many_arguments)]
    pub fn subcycle_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        design: &[f64],
        time_scale: f64,
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<SubcycleTangentOutput> {
        self.check_inputs(previous, trace_start, trace_end, design)?;
        if d_previous.len() != previous.len()
            || d_trace_start.len() != trace_start.len()
            || d_trace_end.len() != trace_end.len()
            || d_design.is_some_and(|d| d.len() != design.len())
        {
            return Err(CaeError::contract("moving LBM tangent: direction length mismatch"));
        }
        self.admit_time_scale(time_scale)?;
        let mac = self.setup::<Dual<1>>(
            trace_start,
            trace_end,
            design,
            Some(d_trace_start),
            Some(d_trace_end),
            d_design,
            false,
        )?;
        let tau = Dual::new(time_scale, [d_time_scale]);
        let mut acc = self.new_accum(&mac.occ);
        let mut state: Vec<Dual<1>> =
            previous.iter().zip(d_previous).map(|(v, t)| Dual::new(*v, [*t])).collect();
        let mut scratch = Vec::new();
        let start = self.compensated().then(|| self.entrained(&state, &mac.occ, 0));
        self.advance(n, &mut state, 0, self.config.substeps, tau, &mac, Some(&mut acc), &mut scratch);
        if let Some(start) = start {
            let end = self.entrained(&state, &mac.occ, 1);
            let c = self.params(time_scale, n, 1).scales.velocity;
            self.compensate(&mut acc, &mac.occ, &start, &end, c);
        }
        let (flux, _) = self.flux_of(&mac, &acc, tau)?;
        Ok(SubcycleTangentOutput {
            state: state.iter().map(|v| v.eps[0]).collect(),
            flux: flux.iter().map(|v| v.eps[0]).collect(),
            samples: acc.samples.iter().map(|v| v.eps[0]).collect(),
        })
    }


    #[allow(clippy::too_many_arguments)]
    pub fn subcycle_adjoint(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        design: &[f64],
        time_scale: f64,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleBars> {
        let out = self.reverse::<f64>(
            n,
            Cow::Borrowed(previous),
            trace_start,
            trace_end,
            design,
            time_scale,
            None,
            Cow::Borrowed(state_bar),
            flux_bar,
            sample_bar,
        )?;
        Ok(SubcycleBars {
            previous: out.previous,
            trace_start: out.trace_start,
            trace_end: out.trace_end,
            design: out.design,
            time_scale: out.time_scale,
            absolute_origin_s:out.absolute_origin_s,
        })
    }


    #[allow(clippy::too_many_arguments)]
    pub fn subcycle_adjoint_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        design: &[f64],
        time_scale: f64,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<SubcycleBars> {
        if self.absolute_clock.is_some_and(|c|c.direction_s!=0.) {return Err(CaeError::contract("clock-direction Hessian action is not implemented"));}
        if !self.has_second_order() {
            return Err(CaeError::contract(
                "moving LBM: the second-order action is not available for this configuration (cumulant collision, \
                 WALE closure or a carrier without second-order maps)",
            ));
        }
        if d_previous.len() != previous.len() {
            return Err(CaeError::contract("moving LBM adjoint tangent: direction length mismatch"));
        }
        let prev: Vec<Dual<1>> = previous.iter().zip(d_previous).map(|(v, t)| Dual::new(*v, [*t])).collect();
        let lift = |v: &[f64]| -> Vec<Dual<1>> { v.iter().map(|x| Dual::new(*x, [0.0])).collect() };
        let out = self.reverse::<Dual<1>>(
            n,
            Cow::Owned(prev),
            trace_start,
            trace_end,
            design,
            time_scale,
            Some((d_trace_start, d_trace_end, d_design)),
            Cow::Owned(lift(state_bar)),
            &lift(flux_bar),
            &lift(sample_bar),
        )?;
        Ok(SubcycleBars {
            previous: out.previous_eps,
            trace_start: out.trace_start_eps,
            trace_end: out.trace_end_eps,
            design: out.design_eps,
            time_scale: out.time_scale_eps,
            absolute_origin_s:out.absolute_origin_s_eps,
        })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn reverse<S: Lane>(
        &self,
        n: usize,
        previous: Cow<'_, [S]>,
        trace_start: &[f64],
        trace_end: &[f64],
        design: &[f64],
        time_scale: f64,
        direction: Option<(&[f64], &[f64], Option<&[f64]>)>,
        state_bar: Cow<'_, [S]>,
        flux_bar: &[S],
        sample_bar: &[S],
    ) -> CaeResult<ReverseOut> {
        if previous.len() != self.state_size()
            || trace_start.len() != self.trace_size()
            || trace_end.len() != self.trace_size()
            || design.len() != self.design_size()
        {
            return Err(CaeError::contract("moving LBM: state, trace or design length mismatch"));
        }
        if state_bar.len() != self.state_size()
            || flux_bar.len() != self.trace_size()
            || sample_bar.len() != self.observables.len()
        {
            return Err(CaeError::contract("moving LBM adjoint: cotangent length mismatch"));
        }
        if let Some((a, b, d)) = direction
            && (a.len() != trace_start.len()
                || b.len() != trace_end.len()
                || d.is_some_and(|d| d.len() != design.len()))
        {
            return Err(CaeError::contract("moving LBM adjoint tangent: direction length mismatch"));
        }
        self.admit_time_scale(time_scale)?;
        let m = self.config.substeps;
        let (d_start, d_end, d_design) = match direction {
            Some((a, b, d)) => (Some(a), Some(b), d),
            None => (None, None, None),
        };
        let tau = S::from_f64(time_scale);

        let key = (!S::DIRECTIONAL && self.config.adjoint_cache).then(|| PreparedKey {
            absolute_origin:self.absolute_clock.map(|c|c.origin_s.to_bits()),
            interpolation_kernel: self.config.interpolation_kernel,
            n,
            time_scale: time_scale.to_bits(),
            trace_start: trace_start.to_vec(),
            trace_end: trace_end.to_vec(),
            design: design.to_vec(),
            previous: S::into_values(previous.to_vec()),
        });
        let cached = key.as_ref().and_then(|k| S::cached(&self.adjoint_cache, k));
        let prepared: Arc<Prepared<S>> = if let Some(p) = cached {
            p
        } else {
            let mac = self.setup::<S>(trace_start, trace_end, design, d_start, d_end, d_design, true)?;
            let p = Arc::new(self.prepare(n, previous.into_owned(), time_scale, tau, mac)?);
            if let Some(k) = key
                && self.config.adjoint_cache
            {
                S::keep(&self.adjoint_cache, k, Arc::clone(&p), &self.buffers);
            }
            p
        };
        let mac = &prepared.mac;
        let acc = &prepared.acc;
        let (entrained_start, entrained_end) = match &prepared.entrained {
            Some((a, b)) => (Some(a), Some(b.as_slice())),
            None => (None, None),
        };
        let p1 = self.params(Dual::<1>::new(time_scale, [1.0]), n, 1);
        let impulse_scale = S::from_f64(p1.impulse_scale.re);
        let dt_s = p1.scales.macro_step.re;
        let (flux, impulses) = self.flux_of(mac, acc, tau)?;
        let fb: Vec<f64> = flux_bar.iter().map(Scalar::value).collect();
        let fbt: Vec<f64> = flux_bar.iter().map(Lane::tangent).collect();
        let ib_v = self.carrier.flux_to_impulses(&fb, dt_s);
        let ib_t = if S::DIRECTIONAL {
            self.carrier.flux_to_impulses(&fbt, dt_s)
        } else {
            vec![[0.0; 3]; ib_v.len()]
        };
        let i_bar: Vec<[S; 3]> = (0..ib_v.len())
            .map(|q| std::array::from_fn(|e| S::make(ib_v[q][e], ib_t[q][e]) * impulse_scale))
            .collect();

        let mut origin_bar=S::zero();
        let mut tau_bar = S::zero();
        for (f, b) in flux.iter().zip(flux_bar) {
            tau_bar -= *f * *b * (2.0 / time_scale);
        }
        let _ = impulses;
        let (pb0, gbar0) = pushforward::endpoint_impulses_vjp(
            &self.frame,
            &mac.ends[0],
            &mac.occ.slot,
            &mac.occ.cells,
            &acc.g[0],
            &i_bar,
        );
        let (pb1, gbar1) = pushforward::endpoint_impulses_vjp(
            &self.frame,
            &mac.ends[1],
            &mac.occ.slot,
            &mac.occ.cells,
            &acc.g[1],
            &i_bar,
        );

        for (o, (s, b)) in self.observables.iter().zip(acc.samples.iter().zip(sample_bar)) {
            tau_bar -= *s * *b * (f64::from(o.degree) / time_scale);
        }

        let rows = mac.occ.cells.len();
        let mut d_bar = [vec![S::zero(); rows], vec![S::zero(); rows]];
        let mut m_bar = [vec![[S::zero(); 3]; rows], vec![[S::zero(); 3]; rows]];
        let mut fbar_next: Vec<S> = Vec::new();
        let mut fbar_cur = S::pooled(&self.buffers, self.state_size());
        let mut state_bar = state_bar;
        if entrained_start.is_some() {
            self.entrained_vjp(
                state_bar.to_mut(),
                &mac.occ,
                1,
                &gbar1,
                S::one(),
                &mut d_bar[1],
                entrained_end,
            );
        }
        let mut reverse_step = |st: &[S], step: usize| {
            let (p, dp) = self.params_with_derivative::<S>(time_scale, n, step);
            let source = if fbar_next.is_empty() {
                Cotangent::Direct(&state_bar)
            } else {
                Cotangent::Pulled(&fbar_next)
            };
            let bars = self.reverse_substep(
                st,
                source,
                &mut fbar_cur,
                &p,
                &mac.occ,
                [&gbar0, &gbar1],
                sample_bar,
                step,
                &mut d_bar,
                &mut m_bar,
            );
            tau_bar += bars.contract(&dp);
            if let Some(clock)=self.absolute_clock {let p=self.params_absolute(Dual::<1>::new(time_scale,[0.]),Dual::<1>::new(clock.origin_s,[1.]),step);let dp=p.map(|v|v.eps[0]);origin_bar+=bars.contract(&dp);}

            if fbar_next.is_empty() {
                fbar_next = S::pooled(&self.buffers, self.state_size());
            }
            std::mem::swap(&mut fbar_cur, &mut fbar_next);
        };
        match &prepared.checkpoints {
            Checkpoints::All(states) => {
                for step in (1..=m).rev() {
                    reverse_step(&states[step - 1], step);
                }
            }
            Checkpoints::Binomial { store, cursor, actions, reverse_start } => {
                let mut store = store.clone();
                let mut cursor = cursor.clone();
                let mut scratch = Vec::new();
                for action in &actions[*reverse_start..] {
                    match *action {
                        Action::Snapshot { slot, .. } => store[slot].clone_from(&cursor),
                        Action::Restore { slot, .. } => cursor.clone_from(&store[slot]),
                        Action::Release { slot } => store[slot] = None,
                        Action::Advance { from, to } => {
                            let (pos, mut st) = take_cursor(&mut cursor)?;
                            if pos != from {
                                return Err(CaeError::contract(
                                    "moving LBM inner schedule: cursor position mismatch",
                                ));
                            }
                            self.advance(n, &mut st, from, to, tau, mac, None, &mut scratch);
                            cursor = Some((to, st));
                        }
                        Action::Reverse { step } => {
                            let (pos, st) = take_cursor(&mut cursor)?;
                            if pos + 1 != step {
                                return Err(CaeError::contract(
                                    "moving LBM inner schedule: reverse out of order",
                                ));
                            }
                            reverse_step(&st, step);
                        }
                    }
                }
            }
        }
        S::recycle(&self.buffers, fbar_cur);
        let mut gbar =
            self.streaming_transpose(&fbar_next, &state_bar, S::pooled(&self.buffers, self.state_size()));
        S::recycle(&self.buffers, fbar_next);
        if let Some(start) = entrained_start {
            self.entrained_vjp(&mut gbar, &mac.occ, 0, &gbar0, -S::one(), &mut d_bar[0], Some(start));
        }

        let mut v_bar = vec![[S::zero(); 3]; mac.v.len()];
        let e0 = pushforward::endpoint_vjp(
            &self.frame,
            &mac.ends[0],
            &mac.occ.slot,
            &mac.v,
            &d_bar[0],
            &m_bar[0],
            &mut v_bar,
        );
        let e1 = pushforward::endpoint_vjp(
            &self.frame,
            &mac.ends[1],
            &mac.occ.slot,
            &mac.v,
            &d_bar[1],
            &m_bar[1],
            &mut v_bar,
        );
        let (ts_bar, des0) = self.points_pullback(trace_start, design, &[&e0, &pb0], d_start, d_design)?;
        let (te_bar, des1) = self.points_pullback(trace_end, design, &[&e1, &pb1], d_end, d_design)?;
        let scale = m as f64 * self.config.spacing_m;
        let vv: Vec<[f64; 3]> = v_bar.iter().map(|v| v.map(|s| s.value())).collect();
        let dv = self.carrier.impulses_to_flux(&vv, scale);
        let dvt = if S::DIRECTIONAL {
            let vt: Vec<[f64; 3]> = v_bar.iter().map(|v| v.map(|s| s.tangent())).collect();
            self.carrier.impulses_to_flux(&vt, scale)
        } else {
            vec![0.0; dv.len()]
        };
        let out = ReverseOut {
            previous_eps: if S::DIRECTIONAL { gbar.iter().map(Lane::tangent).collect() } else { Vec::new() },
            previous: S::into_values(gbar),
            trace_start: (0..dv.len()).map(|i| ts_bar.0[i] - dv[i]).collect(),
            trace_start_eps: (0..dv.len()).map(|i| ts_bar.1[i] - dvt[i]).collect(),
            trace_end: (0..dv.len()).map(|i| te_bar.0[i] + dv[i]).collect(),
            trace_end_eps: (0..dv.len()).map(|i| te_bar.1[i] + dvt[i]).collect(),
            design: des0.0.iter().zip(&des1.0).map(|(a, b)| a + b).collect(),
            design_eps: des0.1.iter().zip(&des1.1).map(|(a, b)| a + b).collect(),
            time_scale: tau_bar.value(),
            time_scale_eps: tau_bar.tangent(),
            absolute_origin_s:origin_bar.value(),
            absolute_origin_s_eps:origin_bar.tangent(),
        };
        Ok(out)
    }

    fn prepare<S: Lane>(
        &self,
        n: usize,
        previous: Vec<S>,
        time_scale: f64,
        tau: S,
        mac: Macro<S>,
    ) -> CaeResult<Prepared<S>> {
        let m = self.config.substeps;
        let state_bytes = (self.state_size() * size_of::<S>()) as u64;
        let slots = usize::try_from(self.config.inner_checkpoint_bytes / state_bytes.max(1))
            .unwrap_or(usize::MAX)
            .clamp(1, m);
        let entrained_start = self.compensated().then(|| self.entrained(&previous, &mac.occ, 0));
        let mut entrained_end: Option<Vec<[S; 3]>> = None;
        let mut acc = self.new_accum(&mac.occ);
        let mut scratch = Vec::new();
        let checkpoints = if slots >= m {

            let mut states = Vec::with_capacity(m);
            let mut st = previous;
            for k in 0..m {
                let mut next = S::pooled(&self.buffers, st.len());
                self.advance_from(n, Some(&st), &mut next, k, k + 1, tau, &mac, Some(&mut acc), &mut scratch);
                states.push(st);
                st = next;
            }
            if entrained_start.is_some() {
                entrained_end = Some(self.entrained(&st, &mac.occ, 1));
            }
            Checkpoints::All(states)
        } else {
            let schedule = BinomialSchedule::new(m, slots, 0)
                .map_err(|e| CaeError::contract(format!("moving LBM inner checkpoint schedule: {e}")))?;
            let mut store: Vec<Option<(usize, Vec<S>)>> = vec![None; slots.max(1)];
            let mut cursor: Option<(usize, Vec<S>)> = Some((0, previous));
            let actions = schedule.actions().to_vec();
            let reverse_start = schedule.reverse_start();
            for action in &actions[..reverse_start] {
                match *action {
                    Action::Snapshot { slot, .. } => store[slot].clone_from(&cursor),
                    Action::Restore { slot, .. } => cursor.clone_from(&store[slot]),
                    Action::Release { slot } => store[slot] = None,
                    Action::Advance { from, to } => {
                        let (pos, mut st) = take_cursor(&mut cursor)?;
                        if pos != from {
                            return Err(CaeError::contract(
                                "moving LBM inner schedule: cursor position mismatch",
                            ));
                        }
                        if to == m {
                            self.advance(n, &mut st, from, m - 1, tau, &mac, Some(&mut acc), &mut scratch);
                            let mut last = S::pooled(&self.buffers, st.len());
                            self.advance_from(n, Some(&st), &mut last, m - 1, m, tau, &mac, Some(&mut acc), &mut scratch);
                            if entrained_start.is_some() {
                                entrained_end = Some(self.entrained(&last, &mac.occ, 1));
                            }
                            S::recycle(&self.buffers, last);
                            cursor = Some((m - 1, st));
                        } else {
                            self.advance(n, &mut st, from, to, tau, &mac, Some(&mut acc), &mut scratch);
                            cursor = Some((to, st));
                        }
                    }
                    Action::Reverse { .. } => {}
                }
            }
            Checkpoints::Binomial { store, cursor, actions, reverse_start }
        };
        let entrained = match (entrained_start, entrained_end) {
            (Some(a), Some(b)) => {
                let c = self.params(time_scale, n, 1).scales.velocity;
                self.compensate(&mut acc, &mac.occ, &a, &b, c);
                Some((a, b))
            }
            _ => None,
        };
        Ok(Prepared { mac, acc, entrained, checkpoints })
    }

    pub fn clear_adjoint_cache(&self) {
        if let Ok(mut guard) = self.adjoint_cache.lock() {
            *guard = None;
        }
    }

    #[allow(clippy::type_complexity)]
    fn points_pullback<S: Lane>(
        &self,
        trace: &[f64],
        design: &[f64],
        parts: &[&PointBar<S>],
        d_trace: Option<&[f64]>,
        d_design: Option<&[f64]>,
    ) -> CaeResult<((Vec<f64>, Vec<f64>), (Vec<f64>, Vec<f64>))> {
        let dx = self.config.spacing_m;
        let vol = dx * dx * dx;
        let count = self.carrier.point_count();
        let mut pos = vec![[0.0; 3]; count];
        let mut w = vec![0.0; count];
        let mut pos_t = vec![[0.0; 3]; count];
        let mut w_t = vec![0.0; count];
        for part in parts {
            for q in 0..count {
                for d in 0..3 {
                    pos[q][d] += part.xi[q][d].value() / dx;
                    pos_t[q][d] += part.xi[q][d].tangent() / dx;
                }
                w[q] += part.a[q].value() / vol;
                w_t[q] += part.a[q].tangent() / vol;
            }
        }
        let (tv, dv) = self.carrier.points_vjp(trace, design, &pos, &w)?;
        if !S::DIRECTIONAL {
            let nt = tv.len();
            let nd = dv.len();
            return Ok(((tv, vec![0.0; nt]), (dv, vec![0.0; nd])));
        }
        let (mut tt, mut dt) = self.carrier.points_vjp(trace, design, &pos_t, &w_t)?;
        let second = self
            .carrier
            .second_order()
            .ok_or_else(|| CaeError::contract("moving LBM: the carrier has no second-order maps"))?;
        let zeros = vec![0.0; trace.len()];
        let (t2, d2) =
            second.points_vjp_tangent(trace, design, &pos, &w, d_trace.unwrap_or(&zeros), d_design)?;
        for (a, b) in tt.iter_mut().zip(&t2) {
            *a += b;
        }
        for (a, b) in dt.iter_mut().zip(&d2) {
            *a += b;
        }
        Ok(((tv, tt), (dv, dt)))
    }

    #[inline]
    fn post_bar<S: Scalar>(&self, source: Cotangent<'_, S>, x: usize) -> [S; Q] {
        let n = self.cells();
        match source {
            Cotangent::Direct(v) => std::array::from_fn(|i| v[i * n + x]),
            Cotangent::Pulled(v) => std::array::from_fn(|i| v[self.topology.reader(x, i)]),
        }
    }

    fn streaming_transpose<S: Scalar>(&self, fbar: &[S], state_bar: &[S], mut next: Vec<S>) -> Vec<S> {
        let n = self.cells();
        next.resize(fbar.len(), S::zero());
        let mut empty: [[S; 3]; 0] = [];
        let chunks = split_state(&mut next, n, Q, None, None, &mut empty, &[], None);
        chunks.into_par_iter().for_each(|mut ch| {
            let len = ch.q.first().map_or(0, |s| s.len());
            for local in 0..len {
                let y = ch.start + local;
                if self.fluid[y] {
                    for i in 0..Q {
                        ch.q[i][local] = fbar[self.topology.reader(y, i)];
                    }
                } else {
                    for i in 0..Q {
                        ch.q[i][local] = state_bar[i * n + y];
                    }
                }
            }
        });
        next
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn reverse_substep<S: Lane>(
        &self,
        prev: &[S],
        source: Cotangent<'_, S>,
        fbar: &mut [S],
        p: &Params<S>,
        occ: &Occupancy<S>,
        gk: [&[[S; 3]]; 2],
        sample_bar: &[S],
        k: usize,
        d_bar: &mut [Vec<S>; 2],
        m_bar: &mut [Vec<[S; 3]>; 2],
    ) -> ParamBar<S> {
        let n = self.cells();
        let m = self.config.substeps;
        let theta = p.theta;
        let kappa = self.config.saturation_width;
        let law = self.config.coupling_law;
        let les = self.les();
        let sampled = match self.config.sampling {
            Sampling::End => k == m,
            Sampling::MacroMean => true,
        };
        let weight = if self.config.sampling == Sampling::MacroMean { 1.0 / m as f64 } else { 1.0 };

        let mut dp_bar_global = [S::zero(); 3];
        let mut rho_bar_ext: Vec<S> = Vec::new();
        let mut u_bar_ext: Vec<[S; 3]> = Vec::new();
        if sampled && !self.observables.is_empty() {
            let reads = self.observables.iter().any(Observable::reads_cells);
            let (mut rho, mut u) = (Vec::new(), Vec::new());
            if reads {
                rho = vec![S::zero(); n];
                u = vec![[S::zero(); 3]; n];
                let vals: Vec<(S, [S; 3])> = self
                    .observed
                    .par_iter()
                    .map(|&x| {
                        let f = self.pre(prev, x, p);
                        let solid = Self::solid_at(occ, x, theta);
                        let mut cp = p.cell;
                        if matches!(self.config.turbulence, Turbulence::Wale(_)) {

                            cp.omega = p.cell.omega;
                        }
                        let r = cell_update(&self.collision, law, kappa, les, &f, &cp, solid.map(|s| s.1));
                        (r.rho, r.u)
                    })
                    .collect();
                for (&x, (r, v)) in self.observed.iter().zip(vals) {
                    rho[x] = r;
                    u[x] = v;
                }
                rho_bar_ext = vec![S::zero(); n];
                u_bar_ext = vec![[S::zero(); 3]; n];
            }
            for (o, b) in self.observables.iter().zip(sample_bar) {
                let bw = *b * weight;
                if o.reads_populations() { continue; }
                if o.reads_occupancy() {
                    let _ = o.open_area(
                        |x| Self::solid_fraction(occ, x, theta, kappa),
                        Some(bw),
                        |x, v| {
                            let s = occ.slot[x];
                            if s != NONE {
                                let s = s as usize;
                                let d = occ.d[0][s] * (1.0 - theta) + occ.d[1][s] * theta;
                                let dd = saturate_derivative(d, kappa) * v;
                                d_bar[0][s] += dd * (1.0 - theta);
                                d_bar[1][s] += dd * theta;
                            }
                        },
                    );
                    continue;
                }
                let g = o.vjp(&p.scales, &rho, &u, &self.fluid, bw, &mut rho_bar_ext, &mut u_bar_ext);
                for e in 0..3 {
                    dp_bar_global[e] += g[e];
                }
            }
        }

        let (rates, u_w) = self.wale_rates(prev, p);
        let wale_on = !rates.is_empty();
        let mut rate_bar = if wale_on { vec![S::zero(); n] } else { Vec::new() };

        let fast = self.run_rates(p);
        let src = match source {
            Cotangent::Direct(v) => kernels::CotangentSource::Direct(v),
            Cotangent::Pulled(v) => kernels::CotangentSource::Pulled(v),
        };
        let solid_runs = law == CouplingLaw::PsmSuperposition;
        let external = if rho_bar_ext.is_empty() { None } else { Some((&rho_bar_ext[..], &u_bar_ext[..])) };

        let rows = occ.cells.len();
        let mut slot_bar = vec![[S::zero(); 3]; rows];
        let mut slot_d_bar = vec![S::zero(); rows];
        let chunk_bars: Vec<(ParamBar<S>, Vec<(usize, S)>)> = {
            let chunks = split_state(
                fbar,
                n,
                Q,
                None,
                None,
                &mut slot_bar,
                &occ.cells,
                if wale_on { Some(rate_bar.as_mut_slice()) } else { None },
            );
            chunks
                .into_par_iter()
                .map(|mut ch| {
                    let mut pb = ParamBar::zero(p.ports.len(), p.targets.len());
                    let mut dextra: Vec<(usize, S)> = Vec::new();
                    let len = ch.q.first().map_or(0, |s| s.len());
                    let mut local = 0;
                    while local < len {
                        let x = ch.start + local;
                        if let Some(rates) = fast {
                            let run = self.run_length(
                                occ,
                                ch.start,
                                local,
                                len,
                                solid_runs,
                                kernels::reverse_lanes::<S>(),
                            );
                            if run > 0 {
                                let solid = occ.slot[x] != NONE;
                                let s0 = occ.slot[x] as usize;
                                let mut d_run = [S::zero(); RUN];
                                let rs = if solid {
                                    let r = s0 - ch.slot_start;
                                    Some(kernels::ReverseSolid {
                                        run: kernels::SolidRun {
                                            d: [&occ.d[0][s0..s0 + run], &occ.d[1][s0..s0 + run]],
                                            m: [&occ.m[0][s0..s0 + run], &occ.m[1][s0..s0 + run]],
                                            theta,
                                            kappa,
                                            superposition: true,
                                        },
                                        gk: [&gk[0][s0..s0 + run], &gk[1][s0..s0 + run]],
                                        dp_bar: dp_bar_global,
                                        m_bar: &mut ch.slots[r..r + run],
                                        d_bar: &mut d_run[..run],
                                    })
                                } else {
                                    None
                                };
                                let (ob, ab) = kernels::reverse_run::<S, Q, L>(
                                    prev,
                                    n,
                                    x,
                                    run,
                                    &self.topology.offsets,
                                    src,
                                    rates,
                                    p.cell.accel,
                                    external,
                                    rs,
                                    &mut ch.q,
                                    local,
                                );
                                pb.omega += ob;
                                for d in 0..3 {
                                    pb.accel[d] += ab[d];
                                }
                                if solid {
                                    for (r, v) in d_run[..run].iter().enumerate() {
                                        dextra.push((s0 + r, *v));
                                    }
                                }
                                local += run;
                                continue;
                            }
                        }
                        local += 1;
                        let local = local - 1;
                        if !self.fluid[x] {
                            for i in 0..Q {
                                ch.q[i][local] = S::zero();
                            }
                            continue;
                        }
                        let f = self.pre(prev, x, p);
                        let mut g: [S; Q] = self.post_bar(source, x);
                        let sv = self.sponge.slot[x];
                        if sv != u32::MAX {
                            let sigma = self.sponge.sigma[x];
                            let phases = &self.sponge.phases[sv as usize];
                            for (&(l, sl), &phase) in self.sponge.parts[sv as usize].iter().zip(phases) {
                                let t = &p.targets[l];
                                let tb: [S; Q] = std::array::from_fn(|i| g[i] * sl);
                                let (mut rb, mut ub) = (S::zero(), [S::zero(); 3]);
                                equilibrium_vjp::<S, Q, L>(t.rho, t.velocity(phase), &tb, &mut rb, &mut ub);
                                pb.targets[l].add_bar(rb, ub, phase);
                            }
                            for gi in &mut g {
                                *gi = *gi * (1.0 - sigma);
                            }
                        }
                        let solid = Self::solid_at(occ, x, theta);
                        let mut cp = p.cell;
                        if wale_on {
                            cp.omega = rates[x];
                        }
                        let mut dp_bar = dp_bar_global;
                        let mut extra_d = S::zero();
                        if let Some((s, si)) = solid {
                            let fwd = cell_update(&self.collision, law, kappa, les, &f, &cp, Some(si));
                            for e in 0..3 {
                                let gt = gk[0][s][e] * (1.0 - theta) + gk[1][s][e] * theta;
                                dp_bar[e] -= gt / si.d;
                                extra_d += gt * fwd.dp[e] / (si.d * si.d);
                            }
                        } else {
                            dp_bar = [S::zero(); 3];
                        }
                        let (rb, ub) = if rho_bar_ext.is_empty() {
                            (S::zero(), [S::zero(); 3])
                        } else {
                            (rho_bar_ext[x], u_bar_ext[x])
                        };
                        let bar = cell_update_vjp(
                            &self.collision,
                            law,
                            kappa,
                            les,
                            &f,
                            &cp,
                            solid.map(|s| s.1),
                            &g,
                            dp_bar,
                            rb,
                            ub,
                        );
                        for i in 0..Q {
                            ch.q[i][local] = bar.f[i];
                        }
                        if wale_on {
                            if let Some(r) = ch.extra.as_deref_mut() {
                                r[local] = bar.omega;
                            }
                        } else {
                            pb.omega += bar.omega;
                        }
                        for d in 0..3 {
                            pb.accel[d] += bar.accel[d];
                        }
                        pb.c_alpha += bar.c_alpha;
                        if let Some((s, _)) = solid {
                            ch.slots[s - ch.slot_start] = bar.m;
                            dextra.push((s, bar.d + extra_d));
                        }
                    }
                    (pb, dextra)
                })
                .collect()
        };
        let mut params_bar = ParamBar::zero(p.ports.len(), p.targets.len());
        for (pb, dextra) in &chunk_bars {
            params_bar.add(pb);
            for &(s, v) in dextra {
                slot_d_bar[s] = v;
            }
        }
        for s in 0..rows {
            d_bar[0][s] += slot_d_bar[s] * (1.0 - theta);
            d_bar[1][s] += slot_d_bar[s] * theta;
            for e in 0..3 {
                m_bar[0][s][e] += slot_bar[s][e] * (1.0 - theta);
                m_bar[1][s][e] += slot_bar[s][e] * theta;
            }
        }

        if let (true, Turbulence::Wale(model), Some(stencil)) =
            (wale_on, &self.config.turbulence, &self.stencil)
        {
            let tau0 = S::one() / p.cell.omega;
            let parts: Vec<([[S; 3]; 3], S)> = (0..n)
                .into_par_iter()
                .map(|x| {
                    if !self.fluid[x] {
                        return ([[S::zero(); 3]; 3], S::zero());
                    }
                    wale::omega_vjp(model, &stencil.gradient(&u_w, x), tau0, rate_bar[x])
                })
                .collect();
            let grad_bar: Vec<[[S; 3]; 3]> = parts.iter().map(|v| v.0).collect();
            let mut tau0_bar = S::zero();
            for v in &parts {
                tau0_bar += v.1;
            }
            params_bar.omega -= tau0_bar / (p.cell.omega * p.cell.omega);
            let grid = self.topology.grid;
            let periodic = self.topology.periodic;
            let add: Vec<[S; Q]> = (0..n)
                .into_par_iter()
                .map(|y| {
                    if !self.fluid[y] {
                        return [S::zero(); Q];
                    }
                    let mut nbrs: Vec<usize> = Vec::with_capacity(6);
                    for a in 0..3 {
                        for s in [-1i64, 1] {
                            let mut o = [0i64; 3];
                            o[a] = s;
                            if grid.inside(y, o, periodic) {
                                let z = grid.wrap(y, o);
                                if z != y && self.fluid[z] && !nbrs.contains(&z) {
                                    nbrs.push(z);
                                }
                            }
                        }
                    }
                    let ub = stencil.gradient_transpose(&grad_bar, y, &nbrs);
                    let f = self.pre(prev, y, p);
                    let (rho, _) = moments::<S, Q, L>(&f);
                    let udot = ub[0] * u_w[y][0] + ub[1] * u_w[y][1] + ub[2] * u_w[y][2];
                    std::array::from_fn(|i| {
                        let c = L::CF[i];
                        let mut v = -udot;
                        for d in 0..3 {
                            if c[d] != 0.0 {
                                v += ub[d] * c[d];
                            }
                        }
                        v / rho
                    })
                })
                .collect();
            for y in 0..n {
                for i in 0..Q {
                    fbar[i * n + y] += add[y][i];
                }
            }
        }

        for (r, pc) in self.topology.ports.iter().enumerate() {
            let x = pc.cell;
            let bar: [S; Q] = std::array::from_fn(|i| fbar[i * n + x]);
            let nb = self.topology.pull_cell(prev, pc.neighbour);
            let value = self.port_value(p, r);
            let (nb_bar, v_bar) = port_populations_vjp::<S, Q, L>(&nb, value, &bar);
            for i in 0..Q {
                fbar[i * n + pc.neighbour] += nb_bar[i];
                fbar[i * n + x] = S::zero();
            }
            match value {
                PortValue::Velocity(_) => {
                    for d in 0..3 {
                        params_bar.ports[pc.port][d] += v_bar[d] * pc.profile;
                    }
                }
                PortValue::Density(_) => params_bar.ports[pc.port][0] += v_bar[0],
            }
        }
        if sampled {
            for (o, b) in self.observables.iter().zip(sample_bar) {
                if o.reads_populations() { o.population_pull_vjp(&p.scales, *b * weight, fbar); }
            }
        }
        params_bar
    }
}

#[derive(Clone, Copy)]
enum Cotangent<'a, S> {
    Direct(&'a [S]),
    Pulled(&'a [S]),
}

fn kinetic<S: Scalar>(rho: &[S], u: &[[S; 3]], fluid: &[bool]) -> f64 {
    let mut acc = 0.0;
    for x in 0..rho.len() {
        if fluid[x] {
            let r = rho[x].value();
            let uu: f64 = u[x].iter().map(|v| v.value() * v.value()).sum();
            acc += 0.5 * r * uu;
        }
    }
    acc
}

#[derive(Default)]
struct ReverseOut {
    previous: Vec<f64>,
    previous_eps: Vec<f64>,
    trace_start: Vec<f64>,
    trace_start_eps: Vec<f64>,
    trace_end: Vec<f64>,
    trace_end_eps: Vec<f64>,
    design: Vec<f64>,
    design_eps: Vec<f64>,
    time_scale: f64,
    time_scale_eps: f64,
    absolute_origin_s:f64,
    absolute_origin_s_eps:f64,
}

impl<const Q: usize, L: Lattice<Q>> SubcycledField for MovingLbm<Q, L> {
    fn state_size(&self) -> usize {
        Q * self.cells()
    }

    fn trace_size(&self) -> usize {
        self.carrier.trace_size()
    }

    fn design_size(&self) -> usize {
        self.carrier.design_size()
    }

    fn sample_names(&self) -> &[String] {
        &self.names
    }

    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        if design.len() != self.carrier.design_size() {
            return Err(CaeError::contract("moving LBM: design length does not match the carrier"));
        }
        Ok(MovingLbm::initial_state(self))
    }

    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        if design.len() != self.carrier.design_size() || cotangent.len() != Q * self.cells() {
            return Err(CaeError::contract("moving LBM: design or cotangent length mismatch"));
        }
        Ok(vec![0.0; design.len()])
    }

    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        let out = MovingLbm::subcycle(self, n, previous, trace_start, trace_end, p.design, p.time_scale)?;
        Ok(SubcycleRecord {
            state: out.state,
            flux: out.flux,
            samples: out.samples,
            pairing: out.pairing,
            ledger: out.ledger,
            diagnostics: StepDiagnostics::default(),
        })
    }

    fn subcycle_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<SubcycleTangent> {
        let t = MovingLbm::subcycle_tangent(
            self,
            n,
            previous,
            trace_start,
            trace_end,
            p.design,
            p.time_scale,
            d_previous,
            d_trace_start,
            d_trace_end,
            d_design,
            d_time_scale,
        )?;
        Ok(SubcycleTangent { state: t.state, flux: t.flux, samples: t.samples })
    }

    fn subcycle_adjoint(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        let b = MovingLbm::subcycle_adjoint(
            self,
            n,
            previous,
            trace_start,
            trace_end,
            p.design,
            p.time_scale,
            state_bar,
            flux_bar,
            sample_bar,
        )?;
        Ok(SubcycleCotangent {
            previous: b.previous,
            trace_start: b.trace_start,
            trace_end: b.trace_end,
            design: b.design,
            time_scale: b.time_scale,
        })
    }

    fn second_order(&self) -> Option<&dyn SecondOrderSubcycle> {
        if self.has_second_order() { Some(self) } else { None }
    }
}

impl<const Q: usize, L: Lattice<Q>> SecondOrderSubcycle for MovingLbm<Q, L> {
    fn subcycle_adjoint_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<SubcycleCotangent> {
        let b = MovingLbm::subcycle_adjoint_tangent(
            self,
            n,
            previous,
            trace_start,
            trace_end,
            p.design,
            p.time_scale,
            state_bar,
            flux_bar,
            sample_bar,
            d_previous,
            d_trace_start,
            d_trace_end,
            d_design,
        )?;
        Ok(SubcycleCotangent {
            previous: b.previous,
            trace_start: b.trace_start,
            trace_end: b.trace_end,
            design: b.design,
            time_scale: b.time_scale,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatticeKind {
    D2Q9,
    D3Q19,
    D3Q27,
}

impl LatticeKind {

    pub fn parse(name: &str) -> CaeResult<Self> {
        match name {
            "D2Q9" => Ok(Self::D2Q9),
            "D3Q19" => Ok(Self::D3Q19),
            "D3Q27" => Ok(Self::D3Q27),
            _ => Err(CaeError::contract(format!("lattice must be D2Q9, D3Q19 or D3Q27 (got {name:?})"))),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::D2Q9 => "D2Q9",
            Self::D3Q19 => "D3Q19",
            Self::D3Q27 => "D3Q27",
        }
    }
}

#[derive(Debug)]
pub enum AnyMovingLbm {
    D2Q9(MovingLbm<9, super::lattice::D2Q9>),
    D3Q19(MovingLbm<19, super::lattice::D3Q19>),
    D3Q27(MovingLbm<27, super::lattice::D3Q27>),
}

macro_rules! dispatch {
    ($self:expr, $f:ident => $body:expr) => {
        match $self {
            AnyMovingLbm::D2Q9($f) => $body,
            AnyMovingLbm::D3Q19($f) => $body,
            AnyMovingLbm::D3Q27($f) => $body,
        }
    };
}

impl AnyMovingLbm {
    pub fn event_tick(&self,previous:&[f64],design:&[f64],trace:event_trace::EventTickTrace<'_>)->CaeResult<event_trace::EventTickOutput>{dispatch!(self,f=>f.event_tick(previous,design,trace))}
    pub fn event_tick_tangent(&self,previous:&[f64],design:&[f64],trace:event_trace::EventTickTrace<'_>,direction:event_trace::EventTickDirection<'_>)->CaeResult<event_trace::EventTickTangent>{dispatch!(self,f=>f.event_tick_tangent(previous,design,trace,direction))}
    pub fn event_tick_adjoint(&self,previous:&[f64],design:&[f64],trace:event_trace::EventTickTrace<'_>,bar:event_trace::EventTickCotangent<'_>)->CaeResult<event_trace::EventTickBars>{dispatch!(self,f=>f.event_tick_adjoint(previous,design,trace,bar))}
    pub fn with_absolute_interval_origin(self,clock:AbsoluteIntervalOrigin)->Self{match self{Self::D2Q9(f)=>Self::D2Q9(f.with_absolute_interval_origin(clock)),Self::D3Q19(f)=>Self::D3Q19(f.with_absolute_interval_origin(clock)),Self::D3Q27(f)=>Self::D3Q27(f.with_absolute_interval_origin(clock))}}
    pub fn absolute_interval_origin(&self)->Option<AbsoluteIntervalOrigin>{dispatch!(self,f=>f.absolute_clock)}
    pub fn nominal_fluid_step_s(&self)->f64{dispatch!(self,f=>f.nominal_fluid_step_s())}
    pub fn subcycle_adjoint_with_origin(&self,n:usize,previous:&[f64],start:&[f64],end:&[f64],design:&[f64],time_scale:f64,state_bar:&[f64],flux_bar:&[f64],sample_bar:&[f64])->CaeResult<SubcycleBars>{dispatch!(self,f=>f.subcycle_adjoint(n,previous,start,end,design,time_scale,state_bar,flux_bar,sample_bar))}

    pub fn new(
        lattice: LatticeKind,
        config: MovingLbmConfig,
        carrier: Arc<dyn LagrangianCarrier>,
    ) -> CaeResult<Self> {
        Ok(match lattice {
            LatticeKind::D2Q9 => Self::D2Q9(MovingLbm::new(config, carrier)?),
            LatticeKind::D3Q19 => Self::D3Q19(MovingLbm::new(config, carrier)?),
            LatticeKind::D3Q27 => Self::D3Q27(MovingLbm::new(config, carrier)?),
        })
    }

    #[must_use]
    pub fn lattice(&self) -> LatticeKind {
        match self {
            Self::D2Q9(_) => LatticeKind::D2Q9,
            Self::D3Q19(_) => LatticeKind::D3Q19,
            Self::D3Q27(_) => LatticeKind::D3Q27,
        }
    }

    #[must_use]
    pub fn config(&self) -> &MovingLbmConfig {
        dispatch!(self, f => f.config())
    }


    pub fn admit_time_scale(&self, time_scale: f64) -> CaeResult<()> {
        dispatch!(self, f => f.admit_time_scale(time_scale))
    }

    #[must_use]
    pub fn cells(&self) -> usize {
        dispatch!(self, f => f.cells())
    }

    #[must_use]
    pub fn macroscopic(&self, state: &[f64]) -> (Vec<f64>, Vec<[f64; 3]>) {
        dispatch!(self, f => f.macroscopic(state))
    }


    pub fn blocking_saturation(&self, trace: &[f64], design: &[f64]) -> CaeResult<BlockingSaturation> {
        dispatch!(self, f => f.blocking_saturation(trace, design))
    }


    pub fn admit_blocking_saturation(
        &self,
        trace: &[f64],
        full_design: &[f64],
    ) -> CaeResult<BlockingSaturation> {
        dispatch!(self, f => f.admit_blocking_saturation(trace, full_design))
    }
}

impl SubcycledField for AnyMovingLbm {
    fn state_size(&self) -> usize {
        dispatch!(self, f => SubcycledField::state_size(f))
    }
    fn trace_size(&self) -> usize {
        dispatch!(self, f => SubcycledField::trace_size(f))
    }
    fn design_size(&self) -> usize {
        dispatch!(self, f => SubcycledField::design_size(f))
    }
    fn sample_names(&self) -> &[String] {
        dispatch!(self, f => SubcycledField::sample_names(f))
    }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        dispatch!(self, f => SubcycledField::initial_state(f, design))
    }
    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        dispatch!(self, f => SubcycledField::initial_state_vjp(f, design, cotangent))
    }
    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        dispatch!(self, f => SubcycledField::subcycle(f, n, previous, trace_start, trace_end, p))
    }
    fn subcycle_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<SubcycleTangent> {
        dispatch!(self, f => SubcycledField::subcycle_tangent(
            f, n, previous, trace_start, trace_end, p, d_previous, d_trace_start, d_trace_end, d_design, d_time_scale
        ))
    }
    fn subcycle_adjoint(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        dispatch!(self, f => SubcycledField::subcycle_adjoint(
            f, n, previous, trace_start, trace_end, p, state_bar, flux_bar, sample_bar
        ))
    }
    fn second_order(&self) -> Option<&dyn SecondOrderSubcycle> {
        dispatch!(self, f => SubcycledField::second_order(f))
    }
}

pub mod event_trace;
