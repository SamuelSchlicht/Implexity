// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use implexity_core::json::{DumpOptions, canonical_sha256, dumps, sha256_hex};
use implexity_core::{CaeError, CaeResult};
use implexity_geometry::phase_partition::{ComplementaryPhaseMap, HelmholtzDensityFilter};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_fields::host::{BoundFieldSource, BoundSource, FieldHost, HostRef};
use implexity_physics_solid::phase_stress_transfer::DensityJumpStressTransfer;
use implexity_physics_solid::solid_history::{SolidHistoryFactory, SolidKernel};
use implexity_solve::affine_history::{AffineHistoryReduction, InitialStateFactory, InitialStatePullback};
use implexity_solve::coupled_history::{
    CoupledHistoryAssembly, HistoryBlock, HistoryBlockCallbacks, HistoryInterface,
};
use implexity_solve::diagnostics::ResidualPartition;
use implexity_solve::local_assembly::Kind;
use implexity_solve::matrix::Jacobian;
use implexity_solve::native_history::{
    HistoryAdjoint, HistoryOptions, HistoryPreconditionerFactory, HistorySolution, HistorySolveOptions,
    matching_guess_fallback,
};
use implexity_solve::operation_context::{
    ContextRequirement, OperationExecutionContext, require_operation_context,
};

use super::observers::UnifiedObserver;
use super::wall_film::{self, FilmBlock, WallFilm};
use super::{COORDS, KINDS, RESPONSES, UNITS, dual_volume::FluidNodalDualVolume};
use crate::elastic_preload::{ElasticPressurePreload, PreloadHost};
use crate::incompressible_transport::{FluidKernel, GroupSet};
use crate::nodal_transport::CartesianNodalTransport;
use crate::unified_history_preconditioner::{
    PreparationFacts, UnifiedHistoryExactProfile, prepare_bounded_diagonal, prepare_bounded_sparse_ilu,
};

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn lin(e: impl std::fmt::Display) -> CaeError {
    CaeError::contract(e.to_string())
}

pub const RELAXED_TOLERANCE_FACTOR: f64 = 100.0;

pub const RELAXED_RESPONSE_RELATIVE_BOUND: f64 = 1e-6;

pub const STATIONARY_DIRECT_ITERATIONS: usize = 25;

pub(crate) struct SolidBlock(pub Arc<SolidKernel>);

impl HistoryBlockCallbacks for SolidBlock {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.0.assembled_residual(n, z, old, x)
    }
    fn current_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.0.jacobian(Kind::Current, n, z, old, x)?))
    }
    fn previous_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.0.jacobian(Kind::Previous, n, z, old, x)?))
    }
    fn design_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.0.jacobian(Kind::Design, n, z, old, x)?))
    }
    fn check(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<()> {
        self.0.check(n, z, old, x)
    }
    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        Some(self.0.current_action(n, z, old, x, v, transpose))
    }
}

pub(crate) struct FluidBlock {
    pub f: Arc<FluidKernel>,
    pub set: GroupSet,
}

impl HistoryBlockCallbacks for FluidBlock {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.f.residual(self.set, n, z, old, x)
    }
    fn current_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.f.jacobian(self.set, Kind::Current, n, z, old, x)?))
    }
    fn previous_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.f.jacobian(self.set, Kind::Previous, n, z, old, x)?))
    }
    fn design_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.f.jacobian(self.set, Kind::Design, n, z, old, x)?))
    }
    fn check(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<()> {
        self.f.check(n, z, old, x)
    }
    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        Some(self.f.current_action(self.set, n, z, old, x, v, transpose))
    }
}

pub(crate) struct TransferInterface(pub Arc<DensityJumpStressTransfer<FluidKernel>>);

impl HistoryInterface for TransferInterface {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.0.residual(n, z, old, x)
    }
    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.0.jacobian(kind, n, z, old, x)?))
    }
    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        Some(self.0.current_action(n, z, old, x, v, transpose))
    }
}

pub(crate) struct NodalTransportInterface(pub Arc<CartesianNodalTransport>);

impl HistoryInterface for NodalTransportInterface {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.0.residual(n, z, old, x)
    }
    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.0.jacobian(kind, n, z, old, x)?))
    }
    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        Some(self.0.current_action(n, z, old, x, v, transpose))
    }
}

pub(crate) struct UnifiedFieldHost {
    p: Value,
    s: Arc<SolidKernel>,
}

impl FieldHost for UnifiedFieldHost {
    fn problem(&self) -> &Value {
        &self.p
    }
    fn solid(&self) -> &Arc<SolidKernel> {
        &self.s
    }
}

pub struct UnifiedSource {
    pub component: String,
    pub settings: Value,
    pub owns_material_forcing: bool,
    pub source: Arc<dyn BoundFieldSource>,
}


pub fn solid_factory(name: &str) -> CaeResult<SolidHistoryFactory> {
    use implexity_physics_solid::components::{SolidComponent, registered};
    match registered(name)? {
        Some(SolidComponent::HistoryBlock) => Ok(SolidHistoryFactory),
        _ => contract(format!("{name}: incompatible solid_history_field")),
    }
}

#[derive(Debug, Clone)]
pub struct StagedGuess {
    pub design_key: Vec<u64>,
    pub states: Vec<Vec<f64>>,
    pub consumed: bool,
}

#[derive(Default)]
pub(crate) struct KernelState {
    pub last: Option<(Vec<u64>, Arc<HistorySolution>)>,
    pub warm: Option<(Vec<u64>, Arc<HistorySolution>)>,
    pub pending: Option<(Vec<u64>, Vec<Vec<f64>>, OperationExecutionContext)>,
    pub numerical_guess_consumed: bool,
    pub staged: Option<StagedGuess>,
    pub diagnostics: Option<((Vec<u64>, String), Value)>,
}

pub type SolveContext = OperationExecutionContext;

pub struct UnifiedKernel {
    pub p: Value,
    pub s: Arc<SolidKernel>,
    pub f: Arc<FluidKernel>,
    pub grid: [usize; 3],
    pub nc: usize,
    pub nt: usize,
    pub phase: ComplementaryPhaseMap,
    pub density_filter: Option<HelmholtzDensityFilter>,
    pub nodal_transport_selected: bool,
    pub assembly: Arc<CoupledHistoryAssembly>,
    pub transfer: Arc<DensityJumpStressTransfer<FluidKernel>>,
    pub volume_transfer: Arc<FluidNodalDualVolume>,
    pub nodal_transport: Option<Arc<CartesianNodalTransport>>,
    pub film: Option<Arc<WallFilm>>,
    pub film_slice: std::ops::Range<usize>,
    pub sources: Vec<UnifiedSource>,
    pub l: CsrMatrix,
    pub q: CsrMatrix,
    pub qfree: CsrMatrix,
    pub ft: usize,
    pub retained: Vec<usize>,
    pub state_size: usize,
    pub full_size: usize,
    pub solid_slice: std::ops::Range<usize>,
    pub fluid_slice: std::ops::Range<usize>,
    pub p_map: CsrMatrix,
    pub w_map: CsrMatrix,
    pub offsets: Vec<Vec<f64>>,
    pub reduction: Arc<AffineHistoryReduction>,
    pub preload: Option<Arc<ElasticPressurePreload>>,
    pub observers: Vec<UnifiedObserver>,
    pub response_units: Vec<(String, String)>,
    pub profile: UnifiedHistoryExactProfile,
    pub provider_profile_digest: String,
    pub(crate) state: Mutex<KernelState>,
    operation: Mutex<()>,
}

impl std::fmt::Debug for UnifiedKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnifiedKernel")
            .field("grid", &self.grid)
            .field("state_size", &self.state_size)
            .field("nt", &self.nt)
            .finish_non_exhaustive()
    }
}

#[must_use]
pub fn design_key(x: &[f64]) -> Vec<u64> {
    x.iter().map(|v| v.to_bits()).collect()
}

fn triplets_csr(rows: usize, cols: usize, r: &[usize], c: &[usize], v: &[f64]) -> CaeResult<CsrMatrix> {
    CsrMatrix::from_triplets(rows, cols, r, c, v).map_err(lin)
}

#[must_use]
pub fn provider_profile_digest(
    serialised: &str,
    binding: &Value,
    profile: &UnifiedHistoryExactProfile,
) -> String {
    let payload = json!({
        "schema": "implexity-native-unified-history-provider-profile/1",
        "problem_sha256": sha256_hex(serialised.as_bytes()),
        "component_binding": binding,
        "exact_numerical_profile_sha256": profile.sha256(),
    });
    canonical_sha256(&payload)
}

#[must_use]
pub fn serialise_problem(p: &Value) -> String {
    dumps(p, &DumpOptions { sort_keys: true, ..DumpOptions::default() })
}

impl UnifiedKernel {

    #[allow(clippy::too_many_lines)]
    pub fn new(
        p: Value,
        digest: Option<String>,
        profile: UnifiedHistoryExactProfile,
    ) -> CaeResult<Arc<Self>> {
        let profile = profile.validated()?;
        let digest = digest.unwrap_or_else(|| {
            let encoded = dumps(&p, &DumpOptions::canonical());
            let mut h = Sha256::new();
            h.update(b"implexity-native-unified-history-profile/1\0");
            h.update(encoded.as_bytes());
            hex::encode(h.finalize())
        });
        let validation = OperationExecutionContext::root(
            "provider-profile-validation",
            "profile_validation",
            &digest,
            false,
        )?;
        require_operation_context(
            Some(&validation),
            ContextRequirement { provider_profile_digest: Some(&digest), ..ContextRequirement::default() },
            "operation context",
        )?;
        let components = &p["components"];
        for (key, kind) in KINDS {
            super::selected(components[key].as_str().unwrap_or_default(), kind)?;
        }
        let fluid_factory = crate::incompressible_transport::selected_fluid_factory(
            components["fluid"].as_str().unwrap_or_default(),
        )?;
        if fluid_factory.shared_temperature_energy_contract()
            != crate::incompressible_transport::shared_temperature_energy_contract()
        {
            return contract(
                "shared-domain fluid component lacks the immutable exact shared-temperature energy contract",
            );
        }
        solid_factory(components["solid"].as_str().unwrap_or_default())?;
        let s = SolidHistoryFactory::create(&p["solid"])?;
        let f = fluid_factory.create_shared_temperature(&p["fluid"])?;
        let grid = s.grid;
        let (nc, nt) = (s.nc, s.nt);
        let phase =
            ComplementaryPhaseMap::new(p["phase_map"]["projection_beta"].as_f64().unwrap_or(f64::NAN))
                .map_err(lin)?;
        let density_filter = match p["phase_map"].get("density_filter") {
            Some(filter) if !filter.is_null() => Some(
                HelmholtzDensityFilter::new(filter["radius_mm"].as_f64().unwrap_or(f64::NAN), &grid)
                    .map_err(lin)?,
            ),
            _ => None,
        };
        let transport =
            p["temperature_transport"].as_str().unwrap_or("trilinear_cell_average_v1").to_string();
        let nodal_transport_selected = crate::nodal_transport::PROFILES.contains(&transport.as_str());
        if nodal_transport_selected
            && f.nodal_transport_capability() != Some(crate::nodal_transport::CAPABILITY)
        {
            return contract("selected fluid does not support explicit nodal transport");
        }
        let design_size = 2 * nc + 3;
        let fluid_set = if nodal_transport_selected { GroupSet::Mechanical } else { GroupSet::Retained };
        let film_settings = match p.get(wall_film::KEY) {
            Some(card) if !card.is_null() => Some(wall_film::validate(card)?),
            _ => None,
        };
        if film_settings.is_some() && !nodal_transport_selected {
            return contract("wall_film requires a nodal temperature transport profile (nodal_dual_*)");
        }
        let mut blocks = vec![
            HistoryBlock {
                name: "solid".into(),
                initial: s.initial_state(),
                design_indices: (0..design_size).collect(),
                callbacks: Arc::new(SolidBlock(Arc::clone(&s))),
                field: None,
            },
            HistoryBlock {
                name: "fluid".into(),
                initial: f.initial_state(),
                design_indices: (0..nc + 3).collect(),
                callbacks: Arc::new(FluidBlock { f: Arc::clone(&f), set: fluid_set }),
                field: None,
            },
        ];
        if film_settings.is_some() {
            blocks.push(HistoryBlock {
                name: wall_film::BLOCK.into(),
                initial: WallFilm::initial_state(&s),
                design_indices: (0..design_size).collect(),
                callbacks: Arc::new(FilmBlock { size: WallFilm::block_size(&s), design: design_size }),
                field: None,
            });
        }

        let host = HostRef(Arc::new(UnifiedFieldHost { p: p.clone(), s: Arc::clone(&s) }));
        let registry = &implexity_core::registries::global().addins;
        let catalog = super::source_catalog()?;
        let bound = implexity_core::history_field_sources::bind(
            registry,
            &catalog,
            p.get("field_sources"),
            &p as &dyn std::any::Any,
            &host as &dyn std::any::Any,
        )?;
        let mut sources = Vec::new();
        for row in bound {
            let source = row
                .source
                .downcast_ref::<BoundSource>()
                .map(|b| Arc::clone(&b.0))
                .ok_or_else(|| CaeError::contract("field source returned a foreign bound object"))?;
            blocks.extend(source.blocks());
            sources.push(UnifiedSource {
                component: row.component,
                settings: row.settings,
                owns_material_forcing: row.owns_material_forcing,
                source,
            });
        }
        let numerics = &p["numerics"];
        let tolerance = numerics["tolerance"].as_f64().unwrap_or(f64::NAN);
        let max_iterations = numerics["max_iterations"].as_f64().map_or(0, |v| v as usize);
        let assembly =
            Arc::new(CoupledHistoryAssembly::new(blocks, design_size, tolerance, max_iterations, None)?);
        let solid_slice = assembly.slice("solid").map(|(a, b)| a..b).unwrap_or(0..0);
        let fluid_slice = assembly.slice("fluid").map(|(a, b)| a..b).unwrap_or(0..0);
        let film_slice = assembly.slice(wall_film::BLOCK).map(|(a, b)| a..b).unwrap_or(0..0);
        let coolant_rows = film_settings.as_ref().map(|_| film_slice.start);
        let full_size = assembly.state_size();
        let transfer = Arc::new(DensityJumpStressTransfer::new(
            &s,
            Arc::clone(&f),
            solid_slice.clone(),
            fluid_slice.clone(),
            full_size,
            design_size,
        )?);
        assembly.add_interface(Arc::new(TransferInterface(Arc::clone(&transfer))))?;
        for source in &sources {
            source.source.attach(&assembly)?;
        }

        let mut lr = Vec::with_capacity(4 * s.ne);
        let mut lc = Vec::with_capacity(4 * s.ne);
        for (tet, owner) in s.mesh.tets.iter().zip(&s.mesh.owners) {
            for node in tet {
                lr.push(*owner);
                lc.push(*node);
            }
        }
        let lv = vec![1.0 / 24.0; lr.len()];
        let l = triplets_csr(nc, s.nn, &lr, &lc, &lv)?;
        let volume_transfer = Arc::new(FluidNodalDualVolume::new(
            full_size,
            design_size,
            (solid_slice.start, solid_slice.end),
            fluid_slice.start,
            &s,
            &f,
            &l,
            &transport,
            coolant_rows,
        )?);
        assembly.add_interface(Arc::clone(&volume_transfer) as Arc<dyn HistoryInterface>)?;
        let nodal_transport = if nodal_transport_selected {
            let smoothing = p.get("temperature_transport_smoothing_velocity_m_s").and_then(Value::as_f64);
            let nt_iface = Arc::new(CartesianNodalTransport::new(
                full_size,
                design_size,
                solid_slice.start,
                fluid_slice.start,
                &s,
                &f,
                &transport,
                smoothing,
                coolant_rows,
            )?);
            assembly.add_interface(Arc::new(NodalTransportInterface(Arc::clone(&nt_iface))))?;
            Some(nt_iface)
        } else {
            None
        };
        let film = match film_settings {
            Some(settings) => {
                let coupling = Arc::new(WallFilm::new(
                    settings,
                    full_size,
                    design_size,
                    solid_slice.start,
                    fluid_slice.start,
                    film_slice.start,
                    &s,
                    &f,
                )?);
                assembly.add_interface(Arc::clone(&coupling) as Arc<dyn HistoryInterface>)?;
                Some(coupling)
            }
            None => None,
        };

        let grid_i: Vec<i64> = grid.iter().map(|g| *g as i64).collect();
        let ijk: Vec<[i64; 3]> = s.mesh.ijk.iter().map(|q| [q[0] as i64, q[1] as i64, q[2] as i64]).collect();
        let q = implexity_physics_base::temperature_projection::cell_center_temperature_map(&grid_i, &ijk)?;
        let mut free_col = vec![usize::MAX; s.nn];
        for (k, node) in s.free_t.iter().enumerate() {
            free_col[*node] = k;
        }
        let (mut qr, mut qc, mut qv) = (Vec::new(), Vec::new(), Vec::new());
        for row in 0..nc {
            let (idx, val) = q.row(row);
            for (c, v) in idx.iter().zip(val) {
                if free_col[*c] != usize::MAX {
                    qr.push(row);
                    qc.push(free_col[*c]);
                    qv.push(*v);
                }
            }
        }
        let qfree = triplets_csr(nc, s.free_t.len(), &qr, &qc, &qv)?;
        let ft = fluid_slice.start + f.nv + f.nc;
        let retained: Vec<usize> = (0..ft).chain(ft + f.nc..full_size).collect();
        let reduced = retained.len();

        let tcol0 = match &film {
            Some(_) => retained
                .iter()
                .position(|r| *r == film_slice.start)
                .ok_or_else(|| CaeError::contract("wall_film block is not retained"))?,
            None => 0,
        };
        let (sts, fts) = (s.model.ts, f.ts);
        let (mut pr, mut pc, mut pv): (Vec<usize>, Vec<usize>, Vec<f64>) =
            (retained.clone(), (0..reduced).collect(), vec![1.0; reduced]);
        let (mut wr, mut wc, mut wv): (Vec<usize>, Vec<usize>, Vec<f64>) =
            ((0..reduced).collect(), retained.clone(), vec![1.0; reduced]);
        let wscale = f.hs / (s.model.ks * s.model.ts * s.model.ls);
        for ((r, c), v) in qr.iter().zip(&qc).zip(&qv) {
            pr.push(ft + r);
            pc.push(tcol0 + *c);
            pv.push(v * sts / fts);
            wr.push(tcol0 + *c);
            wc.push(ft + r);
            wv.push(v * wscale);
        }
        let p_map = triplets_csr(full_size, reduced, &pr, &pc, &pv)?;
        let w_map = triplets_csr(reduced, full_size, &wr, &wc, &wv)?;
        let mut offsets = vec![vec![0.0; full_size]; nt];
        let free_set: std::collections::HashSet<usize> = s.free_t.iter().copied().collect();
        for (n, row) in offsets.iter_mut().enumerate() {
            let prescribed: Vec<f64> = (0..s.nn)
                .map(|node| if free_set.contains(&node) { s.model.t0 } else { s.fixed_t[n][node] })
                .map(|t| t - f.t0)
                .collect();
            let mapped = q.matvec(&prescribed).map_err(lin)?;
            for (c, v) in mapped.iter().enumerate() {
                row[ft + c] = v / fts;
            }
        }
        let initial: Vec<f64> = retained.iter().map(|r| assembly.initial()[*r]).collect();

        let (n_t, n_u) = (s.n_t(), s.n_u());
        let ss = s.state_size;
        let film_end = ss + f.nv + f.nc + film_slice.len();
        let members: Vec<(String, Vec<i64>)> = [
            ("thermal_energy", 0..n_t),
            ("solid_force_balance", n_t..n_t + n_u),
            ("solid_internal_states", n_t + n_u..ss),
            ("fluid_momentum", ss..ss + f.nv),
            ("fluid_mass_balance", ss + f.nv..ss + f.nv + f.nc),
            ("coolant_film_temperature_and_bulk_speed", ss + f.nv + f.nc..film_end),
            ("additional_field_states", film_end..reduced),
        ]
        .into_iter()
        .filter(|(_, r)| !r.is_empty())
        .map(|(name, r)| (name.to_string(), r.map(|i| i as i64).collect()))
        .collect();
        let partition = ResidualPartition::new(reduced, &members)?;

        let preload = match p.get("initialization") {
            Some(policy) if !policy.is_null() => Some(Arc::new(ElasticPressurePreload::new(
                PreloadHost {
                    s: Arc::clone(&s),
                    f: Arc::clone(&f),
                    transfer: Arc::clone(&transfer),
                    volume_transfer: Arc::clone(&volume_transfer),
                    p: p.clone(),
                    p_map: p_map.clone(),
                    offset0: offsets[0].clone(),
                    base: initial.clone(),
                    retained: retained.clone(),
                    solid_start: solid_slice.start,
                    fluid_slice: fluid_slice.clone(),
                    design_size,
                    has_sources: !sources.is_empty(),
                    condition_limit: HistoryOptions::default().condition_limit,
                },
                policy,
            )?)),
            _ => None,
        };
        let (factory, pullback): (Option<InitialStateFactory>, Option<InitialStatePullback>) = match &preload
        {
            Some(pre) => {
                let a = Arc::clone(pre);
                let b = Arc::clone(pre);
                (
                    Some(Arc::new(move |x: &[f64]| a.state(x))),
                    Some(Arc::new(move |x: &[f64], c: &DenseMatrix| b.pullback(x, c))),
                )
            }
            None => (None, None),
        };
        let mut options = HistoryOptions {
            local_elimination_partition: s.local_elimination_partition(reduced)?,
            tolerance,
            max_iterations,
            krylov_policy: profile.krylov_policy(),
            lease_budget: profile.lease_budget(reduced)?,
            residual_partition: Some(partition),
            relaxed_tolerance: Some(RELAXED_TOLERANCE_FACTOR * tolerance),
            ..HistoryOptions::default()
        };
        if profile.enabled() {
            options.authority_policy_sha256 = Some(profile.sha256());
            options.preconditioner_factory =
                Some(Self::preconditioner_factory(&profile, digest.clone(), reduced, design_size, nt - 1));
        }
        let reduction = Arc::new(AffineHistoryReduction::new(
            Arc::clone(&assembly) as Arc<dyn implexity_solve::affine_history::FullHistoryAssembly>,
            p_map.clone(),
            w_map.clone(),
            offsets.clone(),
            initial,
            options,
            factory,
            pullback,
        )?);
        reduction.set_structural_state_pattern(Self::structural_state_pattern(
            &s,
            &retained,
            solid_slice.start,
            full_size,
        )?)?;
        let mut kernel = Self {
            p,
            s,
            f,
            grid,
            nc,
            nt,
            phase,
            density_filter,
            nodal_transport_selected,
            assembly,
            transfer,
            volume_transfer,
            nodal_transport,
            film,
            film_slice,
            sources,
            l,
            q,
            qfree,
            ft,
            retained,
            state_size: reduced,
            full_size,
            solid_slice,
            fluid_slice,
            p_map,
            w_map,
            offsets,
            reduction,
            preload,
            observers: Vec::new(),
            response_units: Vec::new(),
            profile,
            provider_profile_digest: digest,
            state: Mutex::new(KernelState::default()),
            operation: Mutex::new(()),
        };
        kernel.observers = super::observers::bind(&kernel)?;
        let mut units: Vec<(String, String)> =
            RESPONSES.iter().zip(UNITS).map(|(a, b)| ((*a).to_string(), b.to_string())).collect();
        for observer in &kernel.observers {
            for (name, unit) in observer.response_units() {
                if units.iter().any(|(k, _)| *k == name) {
                    return contract("history observer conflicts with a native response");
                }
                units.push((name, unit));
            }
        }
        for source in &kernel.sources {
            let rows = source.source.response_units();
            if rows.iter().any(|(name, _)| units.iter().any(|(k, _)| k == name)) {
                return contract("field-source response collision");
            }
            units.extend(rows);
        }
        kernel.response_units = units;
        Ok(Arc::new(kernel))
    }

    fn structural_state_pattern(
        s: &SolidKernel,
        retained: &[usize],
        solid_start: usize,
        full_size: usize,
    ) -> CaeResult<CsrMatrix> {
        let mut reduced_of = vec![usize::MAX; full_size];
        for (r, f) in retained.iter().enumerate() {
            reduced_of[*f] = r;
        }
        let mut displacement = vec![usize::MAX; 3 * s.nn];
        for (k, dof) in s.free_u.iter().enumerate() {
            displacement[*dof] = reduced_of[solid_start + s.n_t() + k];
        }
        let (mut rows, mut cols) = (Vec::new(), Vec::new());
        let mut group = Vec::with_capacity(12);
        for tet in &s.mesh.tets {
            group.clear();
            group.extend(
                tet.iter()
                    .flat_map(|n| (0..3).map(move |c| 3 * n + c))
                    .map(|d| displacement[d])
                    .filter(|r| *r != usize::MAX),
            );
            for a in &group {
                for b in &group {
                    rows.push(*a);
                    cols.push(*b);
                }
            }
        }
        let zeros = vec![0.0; rows.len()];
        let n = retained.len();
        triplets_csr(n, n, &rows, &cols, &zeros)
    }

    fn preconditioner_factory(
        profile: &UnifiedHistoryExactProfile,
        digest: String,
        state_size: usize,
        design_size: usize,
        history_steps: usize,
    ) -> HistoryPreconditionerFactory {
        let sparse = profile.uses_sparse_ilu();
        let sha = profile.sha256();
        let bytes = profile.maximum_preconditioner_bytes;
        Arc::new(move |lease, binding, point| {
            require_operation_context(
                point.execution_context,
                ContextRequirement {
                    authority_eligible: Some(true),
                    provider_profile_digest: Some(&digest),
                    ..ContextRequirement::default()
                },
                "native exact preconditioner operation context",
            )?;
            let facts = PreparationFacts {
                expected_state_size: state_size,
                expected_design_size: design_size,
                history_steps,
                exact_profile_sha256: &sha,
                maximum_preconditioner_bytes: bytes,
            };
            if sparse {
                prepare_bounded_sparse_ilu(lease, binding, &point, &facts)
            } else {
                prepare_bounded_diagonal(lease, binding, &point, &facts)
            }
        })
    }

    pub(crate) fn exclusive(&self) -> MutexGuard<'_, ()> {
        self.operation.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, KernelState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn response_names(&self) -> Vec<String> {
        self.response_units.iter().map(|(k, _)| k.clone()).collect()
    }

    #[must_use]
    pub fn fx(&self, x: &[f64]) -> Vec<f64> {
        x[..self.nc + 3].to_vec()
    }


    pub fn expand(&self, n: usize, z: &[f64]) -> CaeResult<Vec<f64>> {
        self.reduction.expand(n, z)
    }


    pub fn reduce_cotangent(&self, v: &[f64]) -> CaeResult<Vec<f64>> {
        self.p_map.matvec_transpose(v).map_err(lin)
    }


    pub fn filtered_control(&self, control: &[f64], spacing_mm: &[f64]) -> CaeResult<Vec<f64>> {
        match &self.density_filter {
            None => Ok(control.to_vec()),
            Some(filter) => filter.apply(control, spacing_mm).map_err(lin),
        }
    }


    pub fn physical(&self, control: &[f64], spacing_mm: &[f64], material: &[f64]) -> CaeResult<Vec<f64>> {
        let (rho, _) = self.phase.values(&self.filtered_control(control, spacing_mm)?).map_err(lin)?;
        let mut x = rho;
        x.extend_from_slice(spacing_mm);
        x.extend_from_slice(material);
        Ok(x)
    }


    pub fn occupancy_vjp(
        &self,
        control: &[f64],
        spacing_mm: &[f64],
        g: &[f64],
    ) -> CaeResult<(Vec<f64>, [f64; 3])> {
        let filtered = self.filtered_control(control, spacing_mm)?;
        let derivative = self.phase.derivative(&filtered).map_err(lin)?;
        let g: Vec<f64> = g.iter().zip(&derivative).map(|(a, b)| a * b).collect();
        match &self.density_filter {
            None => Ok((g, [0.0; 3])),
            Some(filter) => filter.vjp(&filtered, spacing_mm, &g).map_err(lin),
        }
    }

    #[must_use]
    pub fn fixed_geometry_validity(&self, displacement: &[[f64; 3]], x: &[f64]) -> Value {
        let spacing = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        super::fixed_geometry_validity(
            displacement,
            &spacing,
            &x[..self.nc],
            &self.s.mesh.tets,
            &self.s.mesh.owners,
            &self.p["validity"],
        )
    }


    pub fn connectivity(&self, x: &[f64]) -> CaeResult<Value> {
        let Some(cfg) = self.p.get("connectivity").filter(|c| !c.is_null()) else {
            return Ok(json!({"status": "not_requested", "physical_connectivity_certified": false}));
        };
        let flow = self.f.flow_axis;
        let mut supports: Vec<(usize, String)> = self.s.p["displacement_bcs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| {
                (b["axis"].as_u64().unwrap_or(0) as usize, b["side"].as_str().unwrap_or_default().to_string())
            })
            .collect();
        supports.sort();
        supports.dedup();
        let support_refs: Vec<(usize, &str)> = supports.iter().map(|(a, s)| (*a, s.as_str())).collect();
        let mut masks = Vec::new();
        for side in ["lo", "hi"] {
            let mut mask = vec![false; self.nc];
            for cell in self.f.opening_cells(flow, side)? {
                mask[crate::incompressible_transport::flat(self.grid, cell)] = true;
            }
            masks.push(mask);
        }
        let thresholds: Vec<f64> =
            cfg["thresholds"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
        let spacing = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        let roles = implexity_geometry::phase_connectivity::LegacyRoles {
            support_faces: Some(&support_refs),
            inlet_mask: Some(&masks[0]),
            outlet_mask: Some(&masks[1]),
            ..Default::default()
        };
        let mut report = implexity_geometry::phase_connectivity::audit_phase_connectivity(
            &x[..self.nc],
            implexity_geometry::phase_connectivity::Shape3(self.grid),
            &spacing,
            &roles,
            Some(&thresholds),
            false,
        )
        .map_err(lin)?;
        let admission = cfg["admission_threshold"].as_f64().unwrap_or(f64::NAN);
        let row = report["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["threshold"].as_f64() == Some(admission))
            .cloned()
            .unwrap_or(Value::Null);
        let mut issues: Vec<String> = Vec::new();
        if cfg["require_through_path"] == json!(true) && row["fluid_through_path"] != json!(true) {
            issues.push("no face-connected fluid inlet-to-outlet path".into());
        }
        match row["closed_fluid_fraction_of_qualified"].as_f64() {
            None => issues.push("no fluid phase meeting the admission threshold".into()),
            Some(cf) if cf > cfg["max_closed_fluid_fraction"].as_f64().unwrap_or(f64::NAN) => {
                issues.push(
                    "closed fluid volume exceeds authored limit; trapped pressure is not modeled".into(),
                );
            }
            _ => {}
        }
        match row["disconnected_solid_fraction_of_qualified"].as_f64() {
            None => issues.push("no solid phase meeting the admission threshold".into()),
            Some(sf) if sf > cfg["max_disconnected_solid_fraction"].as_f64().unwrap_or(f64::NAN) => {
                issues.push("unsupported solid volume exceeds authored limit".into());
            }
            _ => {}
        }
        if let Value::Object(m) = &mut report {
            m.insert("policy".into(), cfg["policy"].clone());
            m.insert("admission_threshold".into(), cfg["admission_threshold"].clone());
            m.insert("issues".into(), json!(issues));
            m.insert("discrete_policy_passed".into(), json!(issues.is_empty()));
            m.insert("physical_connectivity_certified".into(), json!(false));
        }
        if !issues.is_empty() && cfg["policy"] == "reject" {
            return Err(CaeError::convergence(format!("shared phase connectivity: {}", issues.join("; "))));
        }
        Ok(report)
    }


    pub fn new_operation_context(
        &self,
        purpose: &str,
        authority_eligible: bool,
    ) -> CaeResult<OperationExecutionContext> {
        let id = implexity_solve::trace::new_trace_id("provider-operation")?;
        OperationExecutionContext::root(&id, purpose, &self.provider_profile_digest, authority_eligible)
    }


    pub fn canonical_context(
        &self,
        context: Option<&OperationExecutionContext>,
        purpose: &str,
    ) -> CaeResult<OperationExecutionContext> {
        let owned = match context {
            Some(c) => c.clone(),
            None => self.new_operation_context(purpose, true)?,
        };
        require_operation_context(
            Some(&owned),
            ContextRequirement {
                authority_eligible: Some(true),
                provider_profile_digest: Some(&self.provider_profile_digest),
                ..ContextRequirement::default()
            },
            "canonical provider operation context",
        )?;
        Ok(owned)
    }


    pub fn nonauthority_context(
        &self,
        context: Option<&OperationExecutionContext>,
        purpose: &str,
    ) -> CaeResult<OperationExecutionContext> {
        let owned = match context {
            Some(c) => c.clone(),
            None => self.new_operation_context(purpose, false)?,
        };
        require_operation_context(
            Some(&owned),
            ContextRequirement {
                authority_eligible: Some(false),
                provider_profile_digest: Some(&self.provider_profile_digest),
                ..ContextRequirement::default()
            },
            "non-authority provider operation context",
        )?;
        Ok(owned)
    }


    pub fn require_authoritative_solution(
        &self,
        sol: &HistorySolution,
        context: Option<&OperationExecutionContext>,
    ) -> CaeResult<OperationExecutionContext> {
        let bound = require_operation_context(
            sol.execution_context.as_ref(),
            ContextRequirement {
                authority_eligible: Some(true),
                provider_profile_digest: Some(&self.provider_profile_digest),
                ..ContextRequirement::default()
            },
            "authoritative history solution context",
        )?
        .clone();
        if let Some(c) = context {
            require_operation_context(
                Some(c),
                ContextRequirement {
                    authority_eligible: Some(true),
                    expected: Some(&bound),
                    ..ContextRequirement::default()
                },
                "authoritative provider operation context",
            )?;
        }
        Ok(bound)
    }

    fn owned_solution(sol: &HistorySolution, context: Option<OperationExecutionContext>) -> HistorySolution {
        HistorySolution {
            states: sol.states.clone(),
            residual_norms: sol.residual_norms.clone(),
            newton_iterations: sol.newton_iterations.clone(),
            execution_context: context.or_else(|| sol.execution_context.clone()),
            convergence_reports: sol.convergence_reports.clone(),
        }
    }


    pub fn mapped_solid_heat_loads(&self, x: &[f64]) -> CaeResult<Vec<Vec<f64>>> {
        if x.len() != 2 * self.nc + 3 || x.iter().any(|v| !v.is_finite()) {
            return contract("heat continuation requires the complete finite physical design");
        }
        let s = &self.s;
        let tl = s.thermal_row_slice();
        if tl.len() != s.n_t() || tl.end > s.state_size {
            return contract(format!(
                "heat continuation requires the solid kernel to declare a unit-step thermal_row_slice covering its nT={} free temperature rows inside state_size={}; declared slice({}, {}, None)",
                s.n_t(),
                s.state_size,
                tl.start,
                tl.end
            ));
        }
        if self.solid_slice.len() != s.state_size {
            return contract("solid residual slice disagrees with the selected add-in state layout");
        }
        let spacing = [x[self.nc], x[self.nc + 1], x[self.nc + 2]];
        let mut loads = Vec::with_capacity(self.nt);
        for n in 0..self.nt {
            let boundary = s.boundary_load(n, &spacing);
            if boundary.len() != s.state_size || boundary.iter().any(|v| !v.is_finite()) {
                return contract("selected solid boundary load has invalid shape or values");
            }
            let mut full = vec![0.0; self.full_size];
            for r in tl.clone() {
                full[self.solid_slice.start + r] = boundary[r];
            }
            let mapped = self.w_map.matvec(&full).map_err(lin)?;
            if mapped.len() != self.state_size || mapped.iter().any(|v| !v.is_finite()) {
                return contract("mapped solid heat-flux load has invalid shape or values");
            }
            loads.push(mapped);
        }
        Ok(loads)
    }


    pub fn canonical_solution(
        &self,
        x: &[f64],
        guesses: Option<&[Vec<f64>]>,
        context: &OperationExecutionContext,
    ) -> CaeResult<HistorySolution> {
        self.canonical_solution_capped(x, guesses, context, None)
    }

    fn canonical_solution_capped(
        &self,
        x: &[f64],
        guesses: Option<&[Vec<f64>]>,
        context: &OperationExecutionContext,
        max_iterations: Option<usize>,
    ) -> CaeResult<HistorySolution> {
        let ctx = self.canonical_context(Some(context), "canonical_lambda_one_internal")?;
        let sol = self.reduction.solve(
            x,
            self.nt - 1,
            &HistorySolveOptions {
                matching_time_states: guesses,
                execution_context: Some(&ctx),
                max_iterations,
            },
        )?;
        self.require_authoritative_solution(&sol, Some(&ctx))?;
        self.guard(&sol, x)?;
        Ok(sol)
    }

    fn guard(&self, sol: &HistorySolution, x: &[f64]) -> CaeResult<()> {
        let full: Vec<Vec<f64>> =
            sol.states.iter().enumerate().map(|(n, z)| self.expand(n, z)).collect::<CaeResult<_>>()?;
        let solid = HistorySolution {
            states: full.iter().map(|z| z[self.solid_slice.clone()].to_vec()).collect(),
            residual_norms: sol.residual_norms.clone(),
            newton_iterations: sol.newton_iterations.clone(),
            execution_context: None,
            convergence_reports: None,
        };
        self.s.validate_history(&solid, x)?;
        for source in &self.sources {
            source.source.validate(&full, x)?;
        }
        let fx = self.fx(x);
        let transport = self.p["temperature_transport"].as_str().unwrap_or_default();
        for n in 1..self.nt {
            let reg = self.f.validity(&full[n][self.fluid_slice.clone()], &fx)?;
            let report_only = self.p["applicability_policy"].as_str() == Some("report_only");
            let numerical_ok = ["finite_state_screen_passed", "positive_absolute_pressure_screen_passed",
                "mass_conservation_screen_passed"].iter().all(|key| reg[*key] == json!(true));
            if !numerical_ok || (!report_only && reg["regime_valid"] != json!(true))
            {
                return Err(CaeError::convergence(crate::fluid_admission::fluid_admission_failure_message(
                    &crate::fluid_admission::admission_report(&reg),
                    n as i64,
                    transport,
                )));
            }
            let u = self.s.nodal_displacement(n, &full[n][self.solid_slice.clone()]);
            let screen = self.fixed_geometry_validity(&u, x);
            if self.p["applicability_policy"].as_str() != Some("report_only")
                && (screen["displacement_over_cell_screen_passed"] != json!(true)
                    || screen["channel_width_screen_passed"] != json!(true))
            {
                return Err(CaeError::convergence(super::fixed_geometry_failure_message(&screen, n)));
            }
        }
        self.validate_observers(&sol.states, x)?;
        Ok(())
    }

    fn auxiliary_solution(
        &self,
        x: &[f64],
        loads: &[Vec<f64>],
        lambda: f64,
        guesses: Option<&[Vec<f64>]>,
        context: &OperationExecutionContext,
        index: usize,
    ) -> CaeResult<HistorySolution> {
        if !(0.0..1.0).contains(&lambda) || !lambda.is_finite() {
            return contract("auxiliary heat continuation lambda must lie in [0,1)");
        }
        if loads.len() < 2
            || loads.iter().any(|l| l.len() != self.state_size || l.iter().any(|v| !v.is_finite()))
        {
            return contract("auxiliary mapped heat loads require one uniform vector per history state");
        }
        let child =
            context.child("auxiliary_heat_continuation", &format!("{index}:{}", float_hex(lambda)), false)?;
        let exact = Arc::clone(self.reduction.system().problem());
        let problem = Arc::new(AdditiveHeatProblem { exact, loads: loads.to_vec(), lambda });
        let options = HistoryOptions {
            local_elimination_partition: self.reduction.system().options().local_elimination_partition.clone(),
            tolerance: self.reduction.system().options().tolerance,
            max_iterations: self.reduction.system().options().max_iterations,
            condition_limit: self.reduction.system().options().condition_limit,
            criterion: self.reduction.system().options().criterion.clone(),
            residual_partition: self.reduction.system().options().residual_partition.clone(),
            ..HistoryOptions::default()
        };
        let system = implexity_solve::native_history::NativeHistorySystem::new(problem, options)?;
        let solved = system.solve(
            x,
            &self.reduction.initial_for(x)?,
            self.nt - 1,
            &HistorySolveOptions {
                matching_time_states: guesses,
                execution_context: Some(&child),
                max_iterations: None,
            },
        )?;
        require_operation_context(
            solved.execution_context.as_ref(),
            ContextRequirement {
                authority_eligible: Some(false),
                expected: Some(&child),
                provider_profile_digest: Some(&self.provider_profile_digest),
            },
            "auxiliary continuation solution context",
        )?;
        Ok(solved)
    }


    pub fn heat_continuation_solution(
        &self,
        x: &[f64],
        guesses: Option<&[Vec<f64>]>,
        context: &OperationExecutionContext,
    ) -> CaeResult<HistorySolution> {
        let ctx = self.canonical_context(Some(context), "canonical_lambda_one_internal")?;
        let loads = self.mapped_solid_heat_loads(x)?;
        if !loads[1..].iter().any(|l| l.iter().any(|v| *v != 0.0)) {
            return self.canonical_solution(x, guesses, &ctx);
        }
        let mut attempt = 0usize;
        let zero = self.auxiliary_solution(x, &loads, 0.0, guesses, &ctx, attempt)?;
        attempt += 1;
        let _guard = matching_guess_fallback(false);
        self.solve_interval(x, &loads, &ctx, 0.0, zero.states, 1.0, 0, &mut attempt)
    }

    #[allow(clippy::too_many_arguments)]
    fn solve_interval(
        &self,
        x: &[f64],
        loads: &[Vec<f64>],
        ctx: &OperationExecutionContext,
        lower: f64,
        lower_states: Vec<Vec<f64>>,
        target: f64,
        depth: usize,
        attempt: &mut usize,
    ) -> CaeResult<HistorySolution> {
        let result = if target == 1.0 {
            self.canonical_solution(x, Some(&lower_states), ctx)
        } else {
            let index = *attempt;
            *attempt += 1;
            self.auxiliary_solution(x, loads, target, Some(&lower_states), ctx, index)
        };
        match result {
            Err(CaeError::NewtonConvergence(e)) => {
                if depth >= super::HEAT_CONTINUATION_MAX_BISECTIONS {
                    return Err(CaeError::NewtonConvergence(e));
                }
                let mid = 0.5 * (lower + target);
                let mid_solution =
                    self.solve_interval(x, loads, ctx, lower, lower_states, mid, depth + 1, attempt)?;
                self.solve_interval(x, loads, ctx, mid, mid_solution.states, target, depth + 1, attempt)
            }
            other => other,
        }
    }

    fn fallback_enabled(&self) -> bool {
        self.p.get("numerical_fallback").and_then(Value::as_str) == Some("heat_continuation")
    }

    fn solve_with_fallback(
        &self,
        x: &[f64],
        guesses: Option<&[Vec<f64>]>,
        ctx: &OperationExecutionContext,
    ) -> CaeResult<HistorySolution> {

        let stationary = self.p.get(super::STATIONARY_CONTINUATION_KEY).is_some();
        let direct = if stationary {
            let _no_cold_retry = matching_guess_fallback(false);
            self.canonical_solution_capped(x, guesses, ctx, Some(STATIONARY_DIRECT_ITERATIONS))
        } else {
            self.canonical_solution(x, guesses, ctx)
        };
        match direct {
            Err(CaeError::NewtonConvergence(e)) => {
                let mut failure = CaeError::NewtonConvergence(e);
                if stationary {
                    match self.stationary_continuation_solution(x, ctx, &failure) {
                        Err(CaeError::NewtonConvergence(e)) => failure = CaeError::NewtonConvergence(e),
                        other => return other,
                    }
                }
                if !self.fallback_enabled() {
                    return Err(failure);
                }
                self.heat_continuation_solution(x, guesses, ctx)
            }
            other => other,
        }
    }


    pub fn stationary_continuation_solution(
        &self,
        x: &[f64],
        ctx: &OperationExecutionContext,
        failure: &CaeError,
    ) -> CaeResult<HistorySolution> {
        let Some(card) = self.p.get(super::STATIONARY_CONTINUATION_KEY) else {
            return contract("the problem declares no stationary_continuation");
        };
        let started = std::time::Instant::now();
        let reason: String = failure.message().chars().take(500).collect();
        let aux =
            Self::new(super::stationary_continuation_problem(&self.p, card), None, self.profile.clone())?;
        if aux.state_size != self.state_size || aux.full_size != self.full_size {
            return contract("stationary continuation history has another state layout");
        }
        let aux_ctx = aux.new_operation_context("stationary_continuation", true)?;
        let pseudo = aux.solve_with_fallback(x, None, &aux_ctx)?;
        let guesses: Vec<Vec<f64>> = self
            .s
            .times
            .iter()
            .map(|t| {
                let k = aux.s.times.iter().rposition(|s| s <= t).unwrap_or(0);
                pseudo.states[k].clone()
            })
            .collect();
        let continuation_states = aux.nt;
        drop(aux);
        let _cold = matching_guess_fallback(false);
        let solved = self.canonical_solution(x, Some(&guesses), ctx);
        implexity_solve::trace::point("stationary_continuation", || {
            implexity_solve::trace_fields! {
                "stationary_failure" => reason,
                "continuation_states" => continuation_states,
                "continuation_newton_iterations" => pseudo.newton_iterations,
                "stationary_converged" => solved.is_ok(),
                "duration_s" => started.elapsed().as_secs_f64(),
            }
        })?;
        solved
    }


    pub fn solve(
        &self,
        x: &[f64],
        context: Option<&OperationExecutionContext>,
    ) -> CaeResult<HistorySolution> {
        let ctx = self.canonical_context(context, "canonical_lambda_one_internal")?;
        let key = design_key(x);
        let numerical_guess = self.take_numerical_initial_guess(x)?;
        let (staged, pending) = {
            let mut st = self.lock();
            let staged = match st.staged.as_mut() {
                None => None,
                Some(g) => {
                    if g.design_key != key {
                        return contract("staged coupling initial-guess identity drifted");
                    }
                    if g.consumed {
                        return contract("staged coupling initial guess is one-shot");
                    }
                    if g.states.len() != self.nt {
                        return contract("staged coupling initial-guess history is malformed");
                    }
                    if g.states.iter().any(|s| s.len() != self.state_size || s.iter().any(|v| !v.is_finite()))
                    {
                        return contract("staged coupling initial-guess state is malformed");
                    }
                    g.consumed = true;
                    Some(g.states.clone())
                }
            };
            (staged, st.pending.take())
        };
        if usize::from(pending.is_some()) + usize::from(staged.is_some()) + usize::from(numerical_guess.is_some()) > 1 {
            return contract("staged and imported matching-time guesses cannot overlap");
        }
        let guesses = match (&pending, &staged) {
            (Some((pending_key, states, guess_context)), _) => {
                require_operation_context(
                    Some(guess_context),
                    ContextRequirement {
                        authority_eligible: Some(false),
                        provider_profile_digest: Some(&self.provider_profile_digest),
                        ..ContextRequirement::default()
                    },
                    "matching-time Newton guess installation context",
                )?;
                if *pending_key != key {
                    return contract("matching-time Newton guess design identity drifted before use");
                }
                Some(states.clone())
            }
            (None, Some(states)) => Some(states.clone()),
            (None, None) => numerical_guess,
        };
        self.connectivity(x)?;
        if let Some(guesses) = guesses {
            {
                let mut st = self.lock();
                if st.last.as_ref().is_some_and(|(k, _)| *k == key) {
                    st.last = None;
                }
            }
            let solved = self.solve_with_fallback(x, Some(&guesses), &ctx)?;
            self.require_authoritative_solution(&solved, Some(&ctx))?;
            return Ok(Self::owned_solution(&solved, Some(ctx)));
        }
        let (last, warm) = {
            let st = self.lock();
            (
                st.last.as_ref().filter(|(k, _)| *k == key).map(|(_, s)| Arc::clone(s)),
                st.warm.as_ref().map(|(_, s)| Arc::clone(s)),
            )
        };
        if let Some(last) = last {
            self.require_authoritative_solution(&last, None)?;
            return Ok(Self::owned_solution(&last, Some(ctx)));
        }
        let solved = match warm {
            None => self.solve_with_fallback(x, None, &ctx)?,
            Some(warm) => {
                self.require_authoritative_solution(&warm, None)?;
                self.solve_with_fallback(x, Some(&warm.states), &ctx)?
            }
        };
        self.require_authoritative_solution(&solved, Some(&ctx))?;
        Ok(Self::owned_solution(&solved, Some(ctx)))
    }


    pub fn record_guarded_solution(
        &self,
        x: &[f64],
        sol: &HistorySolution,
        context: Option<&OperationExecutionContext>,
    ) -> CaeResult<()> {
        let context = context.cloned().or_else(|| sol.execution_context.clone());
        let ctx = self.canonical_context(context.as_ref(), "canonical_lambda_one_guarded_cache")?;
        self.require_authoritative_solution(sol, Some(&ctx))?;
        let key = design_key(x);
        {
            let st = self.lock();
            if let Some((k, last)) = &st.last
                && *k == key
            {
                self.require_authoritative_solution(last, None)?;
                return Ok(());
            }
        }
        let owned = Self::owned_solution(sol, None);
        self.reduction.system().commit_guarded_exact_linearization(x, &owned, Some(&ctx))?;
        self.lock().last = Some((key, Arc::new(owned)));
        Ok(())
    }


    pub fn begin_exact_factorization_reuse(
        &self,
        x: Option<&[f64]>,
        context: Option<&OperationExecutionContext>,
    ) -> CaeResult<()> {
        let ctx = self.canonical_context(context, "canonical_lambda_one_sensitivity")?;
        let retain = match x {
            Some(x) => {
                let key = design_key(x);
                self.lock().last.as_ref().filter(|(k, _)| *k == key).map(|(_, s)| (x.to_vec(), Arc::clone(s)))
            }
            None => None,
        };
        self.reduction.system().begin_exact_factorization_reuse(
            Some(&ctx),
            retain.as_ref().map(|(x, s)| (x.as_slice(), s.as_ref())),
        )?;
        Ok(())
    }

    pub fn discard_exact_factorization_reuse(&self) {
        self.reduction.system().discard_exact_factorization_reuse();
    }


    pub fn canonical_adjoint_many(
        &self,
        x: &[f64],
        sol: &HistorySolution,
        gu: &[DenseMatrix],
        gx: &DenseMatrix,
        context: &OperationExecutionContext,
    ) -> CaeResult<HistoryAdjoint> {
        let ctx = self.canonical_context(Some(context), "canonical_lambda_one_sensitivity")?;
        self.require_authoritative_solution(sol, Some(&ctx))?;
        self.reduction.adjoint_many(x, sol, gu, gx, None, Some(&ctx))
    }

    #[must_use]
    pub fn forward_convergence(&self, sol: &HistorySolution) -> Value {
        let options = self.reduction.system().options();
        let tolerance = options.tolerance;
        let relaxed: Vec<usize> = sol
            .residual_norms
            .iter()
            .enumerate()
            .filter(|(_, r)| **r > tolerance)
            .map(|(i, _)| i + 1)
            .collect();
        let max_residual = sol.residual_norms.iter().copied().fold(0.0_f64, f64::max);
        json!({
            "tier": if relaxed.is_empty() { "strict" } else { "relaxed" },
            "tolerance": tolerance,
            "relaxed_tolerance": options.relaxed_tolerance,
            "maximum_residual_norm": max_residual,
            "relaxed_steps": relaxed,
        })
    }


    pub fn certify_forward_convergence(
        &self,
        sol: &HistorySolution,
        adjoint: &HistoryAdjoint,
        names: &[String],
        values: &[f64],
    ) -> CaeResult<Value> {
        if names.len() != values.len() || adjoint.residual_error_bounds.len() != values.len() {
            return contract("forward convergence certificate needs one bound per response");
        }
        let mut record = self.forward_convergence(sol);
        let relaxed = record["tier"] == "relaxed";
        let mut bounds = serde_json::Map::new();
        let mut failed = Vec::new();
        for ((name, value), bound) in names.iter().zip(values).zip(&adjoint.residual_error_bounds) {
            let relative = if *value == 0.0 { f64::INFINITY } else { bound / value.abs() };
            let certified = *bound <= RELAXED_RESPONSE_RELATIVE_BOUND * value.abs();
            if relaxed && !certified {
                failed.push(format!("{name}: bound {bound:.3e}, value {value:.6e}"));
            }
            bounds.insert(
                name.clone(),
                json!({"bound": bound, "relative": if relative.is_finite() { json!(relative) } else { Value::Null }}),
            );
        }
        record["residual_error_bounds"] = Value::Object(bounds);
        record["relative_bound_limit"] = json!(RELAXED_RESPONSE_RELATIVE_BOUND);
        record["certified"] = json!(failed.is_empty());
        if !failed.is_empty() {
            return Err(CaeError::convergence(format!(
                "relaxed-tier history (maximum residual norm {:.3e}) is not certified for derivatives: {}",
                record["maximum_residual_norm"].as_f64().unwrap_or(f64::NAN),
                failed.join("; ")
            )));
        }
        Ok(record)
    }


    pub fn certify_sensitivity(&self, sol: &HistorySolution, x: &[f64]) -> CaeResult<Value> {
        let states: Vec<Vec<f64>> = sol
            .states
            .iter()
            .enumerate()
            .map(|(n, z)| self.expand(n, z).map(|f| f[self.solid_slice.clone()].to_vec()))
            .collect::<CaeResult<_>>()?;
        self.s.certify_sensitivity(&states, x)
    }


    pub fn promote_accepted(&self, x: &[f64], context: Option<&OperationExecutionContext>) -> CaeResult<()> {
        if let Some(c) = context {
            self.canonical_context(Some(c), "accept_design")?;
        }
        let key = design_key(x);
        let last = self.lock().last.as_ref().filter(|(k, _)| *k == key).map(|(_, s)| Arc::clone(s));
        let Some(last) = last else {
            return contract("accepted warm history requires a guarded same-design solution");
        };
        self.require_authoritative_solution(&last, None)?;
        self.lock().warm = Some((key, last));
        Ok(())
    }

    pub fn take_numerical_initial_guess(&self,x:&[f64])->CaeResult<Option<Vec<Vec<f64>>>> {
        let numerical_guess = if let Some(card) = self.p.get("numerical_initial_guess").filter(|c| !c.is_null()) {
            let guess = implexity_core::contracts::SourceBoundNumericalGuess::from_value(card)?;
            let design_bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
            if sha256_hex(&design_bytes) != guess.destination_design_sha256 {
                None
            } else {
                let initial = self.reduction.initial_for(x)?;
                let initial_bytes: Vec<u8> = initial.iter().flat_map(|v| v.to_le_bytes()).collect();
                if guess.destination_initial_state_sha256 != sha256_hex(&initial_bytes)
                    || guess.time_coordinates_s != self.s.times
                    || guess.states.len() != self.nt
                    || guess.states.iter().any(|s| s.len() != self.state_size)
                    || guess.states[0].iter().map(|v| v.to_bits()).ne(initial.iter().map(|v| v.to_bits()))
                { return contract("numerical initial guess destination layout or physical initial state differs"); }
                let mut st = self.lock();
                if st.numerical_guess_consumed { None } else {
                    st.numerical_guess_consumed = true;
                    Some(guess.states)
                }
            }
        } else { None };
        Ok(numerical_guess)
    }

    #[must_use]
    pub fn matching_time_guess_identity(&self) -> Value {
        let initial = self.reduction.initial_for(&vec![0.0; 2 * self.nc + 3]).ok();
        let initial = match &self.preload {

            Some(pre) => pre.base().to_vec(),
            None => initial.unwrap_or_default(),
        };
        let bytes: Vec<u8> = initial.iter().flat_map(|v| v.to_le_bytes()).collect();
        let options = self.reduction.system().options();
        let times: Vec<Value> = self.s.times.iter().map(|t| json!(t)).collect();
        let base = json!({
            "schema": "native-unified-history-matching-time-layout/1",
            "lifecycle_owner": "native_unified_history",
            "state_count": self.nt,
            "state_shapes": vec![vec![self.state_size]; self.nt],
            "state_dtype": "<f8",
            "time_coordinates_s": times,
            "initial_state_sha256": sha256_hex(&bytes),
            "initial_state_policy": self.p.get("initialization").filter(|v| !v.is_null()).cloned()
                .unwrap_or_else(|| json!({"method": "fixed_reference_legacy"})),
            "history_tolerance": options.tolerance,
            "history_maximum_newton_iterations": options.max_iterations,
            "history_condition_limit": options.condition_limit,
        });
        let layout = sha256_hex(dumps(&base, &DumpOptions::canonical()).as_bytes());
        let mut out = base;
        out["state_layout_id"] = json!(format!("layout-{layout}"));
        out
    }


    pub fn install_matching_time_guess(
        &self,
        x: &[f64],
        states: &[Vec<f64>],
        identity: &Value,
        context: Option<&OperationExecutionContext>,
    ) -> CaeResult<()> {
        let ctx = self.nonauthority_context(context, "install_matching_time_guess")?;
        let expected = self.matching_time_guess_identity();
        let expected_map = expected.as_object().cloned().unwrap_or_default();
        let identity_map = identity.as_object().cloned().unwrap_or_default();
        let mut keys: std::collections::BTreeSet<String> = expected_map.keys().cloned().collect();
        for k in ["provider_truth_status", "canonical_lambda_one", "auxiliary", "source_cache"] {
            keys.insert(k.into());
        }
        let actual: std::collections::BTreeSet<String> = identity_map.keys().cloned().collect();
        if actual != keys || expected_map.iter().any(|(k, v)| identity_map.get(k) != Some(v)) {
            return contract("matching-time Newton guess state layout or numerical policy is stale");
        }
        if identity_map["provider_truth_status"] != "canonical_lambda_one_full_guards"
            || identity_map["canonical_lambda_one"] != json!(true)
            || identity_map["auxiliary"] != json!(false)
            || !matches!(identity_map["source_cache"].as_str(), Some("guarded_same_design" | "accepted_warm"))
        {
            return contract("matching-time Newton guess lacks canonical guarded provider provenance");
        }
        if states.len() != self.nt || states.iter().any(|s| s.len() != self.state_size) {
            return contract("matching-time Newton guess history shape is incompatible");
        }
        let mut st = self.lock();
        if st.pending.is_some() {
            return contract("a matching-time Newton guess is already pending consumption");
        }
        st.pending = Some((design_key(x), states.to_vec(), ctx));
        Ok(())
    }


    pub fn export_matching_time_guess(
        &self,
        x: &[f64],
        require_accepted: bool,
        context: Option<&OperationExecutionContext>,
    ) -> CaeResult<(Vec<Vec<f64>>, Value, Value)> {
        if let Some(c) = context {
            self.canonical_context(Some(c), "export_matching_time_guess")?;
        }
        let key = design_key(x);
        let tier = if require_accepted { "accepted_warm" } else { "guarded_same_design" };
        let cache = {
            let st = self.lock();
            let c = if require_accepted { &st.warm } else { &st.last };
            c.as_ref().filter(|(k, _)| *k == key).map(|(_, s)| Arc::clone(s))
        };
        let Some(cache) = cache else {
            return contract(format!(
                "matching-time Newton guess export requires a same-design {tier} history"
            ));
        };
        self.require_authoritative_solution(&cache, None)?;
        let mut identity = self.matching_time_guess_identity();
        identity["provider_truth_status"] = json!("canonical_lambda_one_full_guards");
        identity["canonical_lambda_one"] = json!(true);
        identity["auxiliary"] = json!(false);
        identity["source_cache"] = json!(tier);
        Ok((
            cache.states.clone(),
            identity,
            json!({"producer": "native_unified_history", "source_cache": tier}),
        ))
    }

    #[must_use]
    pub fn staged_coupling_capability(&self) -> Value {
        let m = &self.s.model;
        let eligible = self.sources.is_empty()
            && m.history.is_none()
            && m.plastic.is_none()
            && m.creep.is_none()
            && self.assembly_block_count() == 2
            && self.state_size == self.s.state_size + self.f.nv + self.f.nc;
        json!({
            "schema": "implexity-provider-staged-coupling-capability/1",
            "eligible": eligible,
            "laggable_coupling_ids": if eligible { super::STAGED_COUPLING_IDS.to_vec() } else { Vec::new() },
            "exact_restoration_required": true,
            "authority": "preview_initial_guess_only",
        })
    }

    fn assembly_block_count(&self) -> usize {
        2 + usize::from(self.film.is_some())
            + self.sources.iter().map(|s| s.source.blocks().len()).sum::<usize>()
    }

    fn staged_block_correction(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        rows: &[usize],
    ) -> CaeResult<(Vec<f64>, f64)> {
        let inf =
            |v: &[f64]| v.iter().fold(0.0_f64, |a, b| if b.abs() > a || b.is_nan() { b.abs() } else { a });
        let residual = self.reduction.residual(n, z, old, x)?;
        let local: Vec<f64> = rows.iter().map(|r| residual[*r]).collect();
        let before = inf(&local);
        if !before.is_finite() {
            return Err(CaeError::convergence("staged coupling initializer produced a nonfinite residual"));
        }
        if before <= self.reduction.system().options().tolerance.max(1e-8) {
            return Ok((z.to_vec(), before));
        }
        let size = rows.len();
        let action = |v: &[f64]| -> Vec<f64> {
            let mut full = vec![0.0; self.state_size];
            for (r, value) in rows.iter().zip(v) {
                full[*r] = *value;
            }
            match self.reduction.current_action(n, z, old, x, &full, false) {
                Ok(out) => rows.iter().map(|r| out[*r]).collect(),
                Err(_) => vec![f64::NAN; size],
            }
        };
        let rhs: Vec<f64> = local.iter().map(|v| -v).collect();
        let op = implexity_linalg::FnOperator::new(size, |v: &[f64], y: &mut [f64]| {
            y.copy_from_slice(&action(v));
            Ok(())
        });
        let options = implexity_linalg::krylov::GmresOptions {
            rtol: 0.25,
            atol: 0.0,
            restart: Some(size.min(8)),
            maxiter: Some(1),
            ..implexity_linalg::krylov::GmresOptions::default()
        };
        let delta =
            implexity_linalg::krylov::gmres(&op, &rhs, None, None::<&implexity_linalg::Identity>, &options)
                .map_err(|_| CaeError::convergence("staged coupling matrix-free block correction failed"))?
                .x;
        if delta.len() != size || delta.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("staged coupling block correction is nonfinite"));
        }
        for alpha in [1.0, 0.5, 0.25, 0.125, 0.0625] {
            let mut trial = z.to_vec();
            for (r, d) in rows.iter().zip(&delta) {
                trial[*r] += alpha * d;
            }
            let value = match self.reduction.residual(n, &trial, old, x) {
                Ok(v) => v,
                Err(CaeError::Convergence(_) | CaeError::NewtonConvergence(_)) => continue,
                Err(e) => return Err(e),
            };
            let after = inf(&rows.iter().map(|r| value[*r]).collect::<Vec<_>>());
            if after.is_finite() && after < before {
                return Ok((trial, after));
            }
        }
        Err(CaeError::convergence("staged coupling block correction found no residual-reducing step"))
    }


    pub fn staged_coupling_guess(
        &self,
        x: &[f64],
        sweeps: usize,
        lagged: &[String],
    ) -> CaeResult<(Vec<Vec<f64>>, Value)> {
        if self.staged_coupling_capability()["eligible"] != json!(true) {
            return Err(CaeError::convergence(
                "selected provider composition has no safe staged initializer",
            ));
        }
        if !matches!(sweeps, 1 | 2) {
            return contract("staged coupling initializer requires one or two sweeps");
        }
        let unique: std::collections::BTreeSet<&String> = lagged.iter().collect();
        if lagged.is_empty()
            || unique.len() != lagged.len()
            || lagged.iter().any(|v| !super::STAGED_COUPLING_IDS.contains(&v.as_str()))
        {
            return contract("requested coupling id is not advertised as safely laggable");
        }
        let solid: Vec<usize> = (0..self.s.state_size).collect();
        let flow: Vec<usize> = (self.s.state_size..self.s.state_size + self.f.nv + self.f.nc).collect();
        let mut states = vec![self.reduction.initial_for(x)?];
        let mut sweep_residuals = Vec::new();
        let both = lagged.len() == 2;
        for n in 1..self.nt {
            let old = states[n - 1].clone();
            let mut z = old.clone();
            let mut rows = Vec::new();
            for sweep in 0..sweeps {
                let start = z.clone();
                let (flow_norm, solid_norm);
                if both {
                    let (fs, fnorm) = self.staged_block_correction(n, &start, &old, x, &flow)?;
                    let (ss, snorm) = self.staged_block_correction(n, &start, &old, x, &solid)?;
                    z.clone_from(&start);
                    for r in &flow {
                        z[*r] = fs[*r];
                    }
                    for r in &solid {
                        z[*r] = ss[*r];
                    }
                    flow_norm = fnorm;
                    solid_norm = snorm;
                } else if lagged[0] == super::STAGED_THERMAL_TO_FLOW {
                    let (a, fnorm) = self.staged_block_correction(n, &z, &old, x, &flow)?;
                    let (b, snorm) = self.staged_block_correction(n, &a, &old, x, &solid)?;
                    z = b;
                    flow_norm = fnorm;
                    solid_norm = snorm;
                } else {
                    let (a, snorm) = self.staged_block_correction(n, &z, &old, x, &solid)?;
                    let (b, fnorm) = self.staged_block_correction(n, &a, &old, x, &flow)?;
                    z = b;
                    flow_norm = fnorm;
                    solid_norm = snorm;
                }
                let full = self.reduction.residual(n, &z, &old, x)?;
                let full_norm = full.iter().fold(0.0_f64, |a, b| a.max(b.abs()));
                if !full_norm.is_finite() || full.iter().any(|v| !v.is_finite()) {
                    return Err(CaeError::convergence(
                        "staged coupling initializer produced a nonfinite full residual",
                    ));
                }
                rows.push(json!({"sweep": sweep + 1, "flow_block_residual": flow_norm,
                    "solid_shared_block_residual": solid_norm, "full_residual": full_norm}));
            }
            states.push(z);
            sweep_residuals.push(json!({"history_step": n, "sweeps": rows}));
        }
        Ok((states, Value::Array(sweep_residuals)))
    }

    #[must_use]
    pub fn history_digest(states: &[Vec<f64>]) -> String {
        let mut h = Sha256::new();
        h.update(b"implexity-unified-history-states/1\0");
        for s in states {
            h.update((s.len() as i64).to_le_bytes());
            for v in s {
                h.update(v.to_le_bytes());
            }
        }
        hex::encode(h.finalize())
    }
}

struct AdditiveHeatProblem {
    exact: Arc<dyn implexity_solve::native_history::HistoryProblem>,
    loads: Vec<Vec<f64>>,
    lambda: f64,
}

impl implexity_solve::native_history::HistoryProblem for AdditiveHeatProblem {
    fn residual(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let Some(load) = self.loads.get(n) else {
            return contract("invalid auxiliary heat-continuation history index");
        };
        let exact = self.exact.residual(n, z, prev, x)?;
        if exact.len() != load.len() {
            return contract("canonical residual and mapped heat load disagree");
        }
        Ok(exact.iter().zip(load).map(|(r, l)| r - (1.0 - self.lambda) * l).collect())
    }
    fn state_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        self.exact.state_jacobian(n, z, prev, x)
    }
    fn previous_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        self.exact.previous_jacobian(n, z, prev, x)
    }
    fn design_jacobian(&self, _n: usize, _z: &[f64], _prev: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        contract("auxiliary heat continuation exposes no design Jacobian or adjoint")
    }
}

type CacheKey = (String, String, String);

fn cache() -> &'static Mutex<Vec<(CacheKey, Arc<UnifiedKernel>)>> {
    static CACHE: std::sync::OnceLock<Mutex<Vec<(CacheKey, Arc<UnifiedKernel>)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {

        implexity_solve::numerical_state::register_reset("native_unified_history.kernels", release_kernels);
        Mutex::new(Vec::new())
    })
}

fn release_kernels() -> usize {
    let mut kernels = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let n = kernels.len();
    kernels.clear();
    n
}


pub fn build_kernel(
    p: &Value,
    binding: &Value,
    profile: &UnifiedHistoryExactProfile,
) -> CaeResult<Arc<UnifiedKernel>> {
    let serialised = serialise_problem(p);
    let digest = provider_profile_digest(&serialised, binding, profile);
    UnifiedKernel::new(p.clone(), Some(digest), profile.clone())
}


pub fn kernel(
    p: &Value,
    binding: &Value,
    profile: &UnifiedHistoryExactProfile,
) -> CaeResult<Arc<UnifiedKernel>> {
    let key = (
        serialise_problem(p),
        dumps(binding, &DumpOptions::canonical()),
        dumps(&profile.to_wire(), &DumpOptions::canonical()),
    );
    {
        let mut c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(i) = c.iter().position(|(k, _)| *k == key) {
            let entry = c.remove(i);
            let out = Arc::clone(&entry.1);
            c.push(entry);
            return Ok(out);
        }
    }
    let built = build_kernel(p, binding, profile)?;
    let mut c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.len() >= 6 {
        c.remove(0);
    }
    c.push((key, Arc::clone(&built)));
    Ok(built)
}

pub fn clear_kernel_cache() {
    cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
}

#[must_use]
pub fn float_hex(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let mantissa = bits & ((1u64 << 52) - 1);
    if exponent == 0 && mantissa == 0 {
        return format!("{sign}0x0.0p+0");
    }
    let (lead, exp) = if exponent == 0 { (0, -1022) } else { (1, exponent - 1023) };
    let exp_sign = if exp < 0 { "-" } else { "+" };
    format!("{sign}0x{lead}.{mantissa:013x}p{exp_sign}{}", exp.abs())
}


pub fn coordinates(
    grid: [usize; 3],
    design: &implexity_optim::design::NamedArrays,
) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> {
    let names = design.names();
    if names.len() != 3 || COORDS.iter().any(|c| !design.contains(c)) {
        return contract("all three shared-domain coordinate families required");
    }
    let get =
        |name: &str| design.get(name).map(|a| (a.shape().to_vec(), a.iter().copied().collect::<Vec<f64>>()));
    let (Some((cs, control)), Some((hs, h)), Some((ms, c))) =
        (get(COORDS[0]), get(COORDS[1]), get(COORDS[2]))
    else {
        return contract("all three shared-domain coordinate families required");
    };
    let g = grid.to_vec();
    let bad = cs != g
        || ms != g
        || hs != [3]
        || control.iter().chain(&h).chain(&c).any(|v| !v.is_finite())
        || h.iter().any(|v| *v <= 0.0)
        || c.iter().any(|v| *v < 0.0 || *v > 1.0);
    if bad {
        return contract("invalid shared-domain coordinates");
    }
    Ok((control, h, c))
}
