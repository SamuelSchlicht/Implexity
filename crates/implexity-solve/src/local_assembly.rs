// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::sync::{Arc, Mutex, OnceLock};

use implexity_ad::{Dual, Scalar};
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::sparse::{AssemblyPlan, CsrMatrix, Format};
use rayon::prelude::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::matrix::eliminate_zeros;

const WIDTH: usize = 8;

pub trait LocalResidual: Send + Sync {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], previous: &[S], design: &[S], out: &mut [S]);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incidence {
    pub count: usize,
    pub width: usize,
    pub values: Vec<i64>,
}

impl Incidence {


    pub fn new(count: usize, width: usize, values: Vec<i64>) -> CaeResult<Self> {
        if values.len() != count * width {
            return Err(CaeError::contract("incidence values do not match count × width"));
        }
        Ok(Self { count, width, values })
    }

    #[must_use]
    pub fn item(&self, item: usize) -> &[i64] {
        &self.values[item * self.width..(item + 1) * self.width]
    }
}

fn check_indices(inc: &Incidence, size: usize, label: &str, prescribed: bool) -> CaeResult<()> {
    let low = if prescribed { -1 } else { 0 };
    let size = i64::try_from(size).unwrap_or(i64::MAX);
    if inc.values.iter().any(|&v| v < low || v >= size) {
        return Err(CaeError::contract(format!("{label}: incidence outside declared dimension")));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Current,
    Previous,
    Design,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Previous => "previous",
            Self::Design => "design",
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Current => 0,
            Self::Previous => 1,
            Self::Design => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssemblyOptions {
    pub batch_size: usize,
    pub sparse_template_cache_bytes: usize,
    pub sparse_template_kinds: Vec<Kind>,
    pub trace_contribution_id: Option<String>,
}

impl Default for AssemblyOptions {
    fn default() -> Self {
        Self {
            batch_size: 64,
            sparse_template_cache_bytes: 0,
            sparse_template_kinds: vec![Kind::Current],
            trace_contribution_id: None,
        }
    }
}

#[derive(Default)]
struct TemplateCache {
    plans: [Option<Arc<AssemblyPlan>>; 3],
    bytes: usize,
    hits: u64,
    misses: u64,
    stores: u64,
    evictions: u64,
    releases: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointInputs {
    current: Vec<f64>,
    previous: Vec<f64>,
    design: Vec<f64>,
    owner: usize,
}

pub struct LocalResidualAssembly<R: LocalResidual> {
    kernel: R,
    rows: Incidence,
    current: Incidence,
    previous: Incidence,
    previous_is_current: bool,
    design: Incidence,
    state_size: usize,
    design_size: usize,
    options: AssemblyOptions,
    template_min_threads:usize,
    pattern_sizes: [usize; 3],
    templates: Mutex<TemplateCache>,
    structures: [OnceLock<Arc<KindStructure>>; 3],
    contribution_id: String,
}

struct KindStructure {
    ptr: Vec<usize>,
    idx: Vec<usize>,
    row_ptr: Vec<usize>,
    row_items: Vec<usize>,
}

impl<R: LocalResidual> std::fmt::Debug for LocalResidualAssembly<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalResidualAssembly")
            .field("items", &self.rows.count)
            .field("state_size", &self.state_size)
            .field("design_size", &self.design_size)
            .finish_non_exhaustive()
    }
}

fn index_bytes(values: &[i64], width: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * width);
    for &v in values {
        if width == 4 {
            out.extend_from_slice(&i32::try_from(v).unwrap_or(i32::MAX).to_le_bytes());
        } else {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

fn gather(indices: &[i64], values: &[f64], template: &[f64]) -> Vec<f64> {
    indices
        .iter()
        .zip(template)
        .map(|(&i, &t)| if i >= 0 { values[usize::try_from(i).unwrap_or(0)] } else { t })
        .collect()
}

fn finite(values: &[f64], label: &str) -> CaeResult<()> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!("{label} contains nonfinite values")));
    }
    Ok(())
}

fn finite_output(values: &[f64], label: &str) -> CaeResult<()> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::convergence(format!("{label} contains nonfinite values")));
    }
    Ok(())
}

impl<R: LocalResidual> LocalResidualAssembly<R> {
    #[allow(clippy::too_many_arguments)]


    pub fn new(
        kernel: R,
        rows: Incidence,
        current: Incidence,
        previous: Incidence,
        design: Incidence,
        state_size: usize,
        design_size: usize,
        options: AssemblyOptions,
    ) -> CaeResult<Self> {
        if state_size < 1 || design_size < 1 || options.batch_size < 1 {
            return Err(CaeError::contract("assembly dimensions and batch size must be positive"));
        }
        check_indices(&rows, state_size, "residual rows", true)?;
        check_indices(&current, state_size, "current state", true)?;
        check_indices(&previous, state_size, "previous state", true)?;
        check_indices(&design, design_size, "design", false)?;
        let count = rows.count;
        if count == 0 || current.count != count || previous.count != count || design.count != count {
            return Err(CaeError::contract("all local incidence maps require the same positive item count"));
        }
        let previous_is_current = previous == current;
        if let Some(label) = &options.trace_contribution_id {
            let l = label.trim();
            if l.is_empty() || l.len() > 256 || l.chars().any(|c| matches!(c, '\n' | '\r' | '\t')) {
                return Err(CaeError::contract("local assembly trace contribution id is invalid"));
            }
        }
        let pattern_size = |cols: &Incidence| -> usize {
            (0..count)
                .map(|e| {
                    rows.item(e).iter().filter(|&&r| r >= 0).count()
                        * cols.item(e).iter().filter(|&&c| c >= 0).count()
                })
                .sum()
        };
        let pattern_sizes = [pattern_size(&current), pattern_size(&previous), pattern_size(&design)];
        let contribution_id = match &options.trace_contribution_id {
            Some(l) => l.trim().to_string(),
            None => structural_id(
                state_size,
                design_size,
                options.batch_size,
                [&rows, &current, &previous, &design],
            ),
        };
        Ok(Self {
            kernel,
            rows,
            current,
            previous,
            previous_is_current,
            design,
            state_size,
            design_size,
            options,
            template_min_threads:1,
            pattern_sizes,
            templates: Mutex::new(TemplateCache::default()),
            structures: [OnceLock::new(), OnceLock::new(), OnceLock::new()],
            contribution_id,
        })
    }

    #[must_use]
    pub fn kernel(&self) -> &R {
        &self.kernel
    }
    #[must_use]
    pub fn count(&self) -> usize {
        self.rows.count
    }
    #[must_use]
    pub fn state_size(&self) -> usize {
        self.state_size
    }
    #[must_use]
    pub fn design_size(&self) -> usize {
        self.design_size
    }
    #[must_use]
    pub fn contribution_id(&self) -> &str {
        &self.contribution_id
    }

    fn cols(&self, kind: Kind) -> &Incidence {
        match kind {
            Kind::Current => &self.current,
            Kind::Previous => &self.previous,
            Kind::Design => &self.design,
        }
    }



    pub fn gather_point(
        &self,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        current_values: &[f64],
        previous_values: &[f64],
    ) -> CaeResult<PointInputs> {
        for (a, size, label) in [
            (z, self.state_size, "state"),
            (old, self.state_size, "previous"),
            (x, self.design_size, "design"),
        ] {
            finite(a, &format!("local assembly {label}"))?;
            if a.len() != size {
                return Err(CaeError::contract(format!("local assembly {label}: invalid shape")));
            }
        }
        finite(current_values, "prescribed current local state data")?;
        finite(previous_values, "prescribed previous local state data")?;
        if current_values.len() != self.current.values.len()
            || previous_values.len() != self.previous.values.len()
        {
            return Err(CaeError::contract("invalid prescribed local state data"));
        }
        let design: Vec<f64> =
            self.design.values.iter().map(|&i| x[usize::try_from(i).unwrap_or(0)]).collect();
        Ok(PointInputs {
            current: gather(&self.current.values, z, current_values),
            previous: gather(&self.previous.values, old, previous_values),
            design,
            owner: std::ptr::from_ref(self).cast::<()>() as usize,
        })
    }

    fn check_owner(&self, point: &PointInputs) -> CaeResult<()> {
        if point.owner != std::ptr::from_ref(self).cast::<()>() as usize {
            return Err(CaeError::contract("point action-input local binding requires its active cache"));
        }
        Ok(())
    }

    fn local<'p>(&self, point: &'p PointInputs, e: usize) -> (&'p [f64], &'p [f64], &'p [f64]) {
        let (wc, wp, wd) = (self.current.width, self.previous.width, self.design.width);
        (
            &point.current[e * wc..(e + 1) * wc],
            &point.previous[e * wp..(e + 1) * wp],
            &point.design[e * wd..(e + 1) * wd],
        )
    }

    fn local_values(&self, point: &PointInputs) -> Vec<f64> {
        let wr = self.rows.width;
        let mut out = vec![0.0; self.count() * wr];
        out.par_chunks_mut(wr.max(1)).enumerate().for_each(|(e, dst)| {
            let (c, p, d) = self.local(point, e);
            self.kernel.residual::<f64>(e, c, p, d, dst);
        });
        out
    }



    pub fn residual(
        &self,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        current_values: &[f64],
        previous_values: &[f64],
    ) -> CaeResult<Vec<f64>> {
        let point = self.gather_point(z, old, x, current_values, previous_values)?;
        self.residual_at(&point)
    }



    pub fn residual_at(&self, point: &PointInputs) -> CaeResult<Vec<f64>> {
        self.check_owner(point)?;
        let values = self.local_values(point);
        finite_output(&values, "local residual/Jacobian callback output")?;
        let mut result = vec![0.0; self.state_size];
        for (&r, &v) in self.rows.values.iter().zip(&values) {
            if r >= 0 {
                result[usize::try_from(r).unwrap_or(0)] += v;
            }
        }
        Ok(result)
    }

    fn local_jacobians(&self, point: &PointInputs, kind: Kind) -> Vec<f64> {
        let wr = self.rows.width;
        let wk = self.cols(kind).width;
        let block = wr * wk;
        let mut out = vec![0.0; self.count() * block];
        if block == 0 {
            return out;
        }
        out.par_chunks_mut(block).enumerate().for_each_init(
            || (Vec::new(), Vec::new(), Vec::new(), vec![Dual::<WIDTH>::default(); wr]),
            |(cs, ps, ds, ys), (e, jac)| {
                let (c, p, d) = self.local(point, e);
                let lift = |v: &[f64], dst: &mut Vec<Dual<WIDTH>>| {
                    dst.clear();
                    dst.extend(v.iter().map(|&x| Dual::constant(x)));
                };
                lift(c, cs);
                lift(p, ps);
                lift(d, ds);
                let passes = wk.div_ceil(WIDTH);
                for pass in 0..passes {
                    let start = pass * WIDTH;
                    let end = (start + WIDTH).min(wk);
                    let seeded = match kind {
                        Kind::Current => &mut *cs,
                        Kind::Previous => &mut *ps,
                        Kind::Design => &mut *ds,
                    };
                    for (j, s) in seeded.iter_mut().enumerate() {
                        s.eps = [0.0; WIDTH];
                        if (start..end).contains(&j) {
                            s.eps[j - start] = 1.0;
                        }
                    }
                    ys.fill(Dual::default());
                    self.kernel.residual(e, cs, ps, ds, ys);
                    for (i, y) in ys.iter().enumerate() {
                        for j in start..end {
                            jac[i * wk + j] = y.eps[j - start];
                        }
                    }
                }
            },
        );
        out
    }

    fn pattern(&self, kind: Kind) -> (Vec<usize>, Vec<usize>) {
        let cols = self.cols(kind);
        let count = self.pattern_sizes[kind.index()];
        let mut rr = Vec::with_capacity(count);
        let mut cc = Vec::with_capacity(count);
        for e in 0..self.count() {
            for &r in self.rows.item(e) {
                for &c in cols.item(e) {
                    if r >= 0 && c >= 0 {
                        rr.push(usize::try_from(r).unwrap_or(0));
                        cc.push(usize::try_from(c).unwrap_or(0));
                    }
                }
            }
        }
        (rr, cc)
    }

    fn template_slot(&self, kind: Kind) -> usize {
        if kind == Kind::Previous && self.previous_is_current { 0 } else { kind.index() }
    }

    fn template_enabled(&self,kind:Kind)->bool {
        self.options.sparse_template_cache_bytes>0 && self.options.sparse_template_kinds.contains(&kind) && rayon::current_num_threads()>=self.template_min_threads
    }

    fn plan(&self, kind: Kind) -> CaeResult<(Arc<AssemblyPlan>, bool)> {
        let enabled = self.template_enabled(kind);
        let slot = self.template_slot(kind);
        if enabled {
            let mut cache = self.templates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(p) = cache.plans[slot].clone() {
                cache.hits += 1;
                return Ok((p, true));
            }
            cache.misses += 1;
        }
        let width = if kind == Kind::Design { self.design_size } else { self.state_size };
        let (rr, cc) = self.pattern(kind);
        let plan = Arc::new(
            AssemblyPlan::new(self.state_size, width, &rr, &cc, Format::Csr)
                .map_err(|e| CaeError::contract(e.to_string()))?,
        );
        if enabled {
            let bytes = plan.retained_bytes();
            let mut cache = self.templates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(p)=cache.plans[slot].clone() {cache.hits+=1;return Ok((p,true));}
            if bytes <= self.options.sparse_template_cache_bytes {
                for other in 0..3 {
                    if other != slot
                        && cache.bytes + bytes > self.options.sparse_template_cache_bytes
                        && let Some(old) = cache.plans[other].take()
                    {
                        cache.bytes -= old.retained_bytes();
                        cache.evictions += 1;
                    }
                }
                if cache.bytes + bytes <= self.options.sparse_template_cache_bytes {
                    cache.plans[slot] = Some(Arc::clone(&plan));
                    cache.bytes += bytes;
                    cache.stores += 1;
                }
            }
        }
        Ok((plan, false))
    }

    fn structure(&self, kind: Kind) -> Arc<KindStructure> {
        let slot = self.template_slot(kind);
        Arc::clone(self.structures[slot].get_or_init(|| Arc::new(self.build_structure(kind))))
    }

    fn build_structure(&self, kind: Kind) -> KindStructure {
        let n = self.state_size;
        let wr = self.rows.width.max(1);
        let cols = self.cols(kind);
        let row_of = |r: i64| usize::try_from(r).ok();
        let mut row_ptr = vec![0usize; n + 1];
        for r in self.rows.values.iter().filter_map(|&r| row_of(r)) {
            row_ptr[r + 1] += 1;
        }
        for r in 0..n {
            row_ptr[r + 1] += row_ptr[r];
        }
        let mut next = row_ptr.clone();
        let mut row_items = vec![0usize; row_ptr[n]];
        for (flat, &r) in self.rows.values.iter().enumerate() {
            if let Some(r) = row_of(r) {
                row_items[next[r]] = flat;
                next[r] += 1;
            }
        }
        let lists: Vec<Vec<usize>> = (0..n)
            .into_par_iter()
            .map(|r| {
                let mut list: Vec<usize> = row_items[row_ptr[r]..row_ptr[r + 1]]
                    .iter()
                    .flat_map(|&flat| cols.item(flat / wr).iter().filter_map(|&c| usize::try_from(c).ok()))
                    .collect();
                list.sort_unstable();
                list.dedup();
                list
            })
            .collect();
        let mut ptr = Vec::with_capacity(n + 1);
        ptr.push(0);
        let mut idx = Vec::with_capacity(lists.iter().map(Vec::len).sum());
        for list in lists {
            idx.extend(list);
            ptr.push(idx.len());
        }
        KindStructure { ptr, idx, row_ptr, row_items }
    }

    fn assemble_direct(&self, kind: Kind, local: &[f64]) -> CaeResult<CsrMatrix> {
        let structure = self.structure(kind);
        let st = structure.as_ref();
        let cols = self.cols(kind);
        let wk = cols.width;
        let width = if kind == Kind::Design { self.design_size } else { self.state_size };
        let wr = self.rows.width.max(1);
        let mut data = vec![0.0; st.idx.len()];
        let mut rows: Vec<&mut [f64]> = Vec::with_capacity(self.state_size);
        let mut rest = data.as_mut_slice();
        for r in 0..self.state_size {
            let (row, tail) = rest.split_at_mut(st.ptr[r + 1] - st.ptr[r]);
            rows.push(row);
            rest = tail;
        }
        rows.into_par_iter().enumerate().for_each_init(
            || vec![0usize; width],
            |slot_of, (r, row)| {
                for (k, &c) in st.idx[st.ptr[r]..st.ptr[r + 1]].iter().enumerate() {
                    slot_of[c] = k;
                }
                for &flat in &st.row_items[st.row_ptr[r]..st.row_ptr[r + 1]] {
                    let values = &local[flat * wk..(flat + 1) * wk];
                    for (&c, &v) in cols.item(flat / wr).iter().zip(values) {
                        if let Ok(c) = usize::try_from(c) {
                            row[slot_of[c]] += v;
                        }
                    }
                }
            },
        );
        CsrMatrix::try_new(self.state_size, width, st.ptr.clone(), st.idx.clone(), data)
            .map_err(|e| CaeError::contract(e.to_string()))
    }

    pub fn cache_sparse_templates(&mut self,kinds:&[Kind],minimum_threads:usize) {
        self.clear_sparse_template_cache();
        self.options.sparse_template_cache_bytes=self.sparse_template_capacity_bytes(kinds);
        self.options.sparse_template_kinds=kinds.to_vec();
        self.template_min_threads=minimum_threads.max(1);
    }

    pub fn clear_sparse_template_cache(&self) {
        let mut cache = self.templates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let released = cache.plans.iter().filter(|p| p.is_some()).count() as u64;
        cache.plans = [None, None, None];
        cache.bytes = 0;
        cache.releases += released;
    }



    pub fn jacobian(
        &self,
        kind: Kind,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        current_values: &[f64],
        previous_values: &[f64],
    ) -> CaeResult<CsrMatrix> {
        let point = self.gather_point(z, old, x, current_values, previous_values)?;
        self.jacobian_at(kind, &point)
    }



    pub fn jacobian_at(&self, kind: Kind, point: &PointInputs) -> CaeResult<CsrMatrix> {
        self.check_owner(point)?;
        let local = self.local_jacobians(point, kind);
        finite_output(&local, "local residual/Jacobian callback output")?;
        if !self.template_enabled(kind)
        {

            return Ok(eliminate_zeros(&self.assemble_direct(kind, &local)?));
        }
        let cols = self.cols(kind);
        let (wr, wk) = (self.rows.width, cols.width);
        let mut values = Vec::with_capacity(self.pattern_sizes[kind.index()]);
        for e in 0..self.count() {
            let r_idx = self.rows.item(e);
            let c_idx = cols.item(e);
            let base = e * wr * wk;
            for (i, &r) in r_idx.iter().enumerate() {
                if r < 0 {
                    continue;
                }
                for (j, &c) in c_idx.iter().enumerate() {
                    if c >= 0 {
                        values.push(local[base + i * wk + j]);
                    }
                }
            }
        }
        if values.len() != self.pattern_sizes[kind.index()] {
            return Err(CaeError::contract("local Jacobian sparse pattern size changed"));
        }
        let (plan, _) = self.plan(kind)?;
        let assembled = plan.assemble_csr(&values).map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(eliminate_zeros(&assembled))
    }



    #[allow(clippy::too_many_arguments)]
    pub fn current_action(
        &self,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        current_values: &[f64],
        previous_values: &[f64],
        vector: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        let point = self.gather_point(z, old, x, current_values, previous_values)?;
        self.current_action_at(&point, vector, transpose)
    }



    pub fn current_action_at(
        &self,
        point: &PointInputs,
        vector: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.check_owner(point)?;
        finite(vector, "local current-action operand")?;
        if vector.len() != self.state_size {
            return Err(CaeError::contract("local current-action operand has invalid shape"));
        }
        let wr = self.rows.width;
        let wc = self.current.width;
        let (source, target, out_w) =
            if transpose { (&self.rows, &self.current, wc) } else { (&self.current, &self.rows, wr) };
        let mut local_out = vec![0.0; self.count() * out_w];
        if out_w > 0 {
            if transpose {
                let jac = self.local_jacobians(point, Kind::Current);
                local_out.par_chunks_mut(out_w).enumerate().for_each(|(e, dst)| {
                    let rows = source.item(e);
                    for (i, &r) in rows.iter().enumerate() {
                        let w = if r >= 0 { vector[usize::try_from(r).unwrap_or(0)] } else { 0.0 };
                        if w == 0.0 {
                            continue;
                        }
                        for (j, d) in dst.iter_mut().enumerate() {
                            *d += jac[e * wr * wc + i * wc + j] * w;
                        }
                    }
                });
            } else {
                local_out.par_chunks_mut(out_w).enumerate().for_each_init(
                    || (Vec::new(), Vec::new(), Vec::new(), vec![Dual::<1>::default(); wr]),
                    |(cs, ps, ds, ys), (e, dst)| {
                        let (c, p, d) = self.local(point, e);
                        let idx = source.item(e);
                        cs.clear();
                        cs.extend(c.iter().zip(idx).map(|(&v, &i)| {
                            let t = if i >= 0 { vector[usize::try_from(i).unwrap_or(0)] } else { 0.0 };
                            Dual::new(v, [t])
                        }));
                        ps.clear();
                        ps.extend(p.iter().map(|&v| Dual::constant(v)));
                        ds.clear();
                        ds.extend(d.iter().map(|&v| Dual::constant(v)));
                        ys.fill(Dual::default());
                        self.kernel.residual(e, cs, ps, ds, ys);
                        for (o, y) in dst.iter_mut().zip(ys.iter()) {
                            *o = y.eps[0];
                        }
                    },
                );
            }
        }
        finite_output(&local_out, "local residual/Jacobian callback output")?;
        let mut output = vec![0.0; self.state_size];
        for (&t, &v) in target.values.iter().zip(&local_out) {
            if t >= 0 {
                output[usize::try_from(t).unwrap_or(0)] += v;
            }
        }
        if output.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("local current-action result contains nonfinite values"));
        }
        Ok(output)
    }

    #[must_use]
    pub fn sparse_template_capacity_bytes(&self, kinds: &[Kind]) -> usize {
        let mut seen = Vec::new();
        let mut total = 0;
        for &k in kinds {
            let slot = self.template_slot(k);
            if seen.contains(&slot) {
                continue;
            }
            seen.push(slot);
            let count = self.pattern_sizes[k.index()];

            total += (2 * count + 2 * count + 1 + self.state_size + 1) * size_of::<usize>();
        }
        total
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let local = self.rows.width * self.current.width.max(self.previous.width).max(self.design.width);
        let cache = self.templates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = cache.plans.iter().filter(|p| p.is_some()).count();

        let mut kinds = if self.options.sparse_template_cache_bytes == 0 {
            Vec::new()
        } else {
            self.options.sparse_template_kinds.clone()
        };
        kinds.sort();
        kinds.dedup();
        json!({
            "method": "local_ad_sparse_incidence",
            "local_items": self.count(),
            "batch_size": self.options.batch_size,
            "logical_batch_size": self.options.batch_size,
            "local_residual_size": self.rows.width,
            "maximum_local_derivative_entries_per_batch": self.options.batch_size * local,
            "local_derivative_method": "forward_mode_dual_numbers",
            "parallel_execution": "rayon_item_parallel_authored_order_reduction",
            "persistent_expanded_sparse_pattern_bytes": cache.bytes,
            "persistent_sparse_reduction_plan_bytes": cache.bytes,
            "sparse_pattern_generation": "chunked_exact_item_row_column_order",
            "sparse_template_cache_policy": if self.options.sparse_template_cache_bytes == 0 { "disabled" } else { "explicit_byte_bounded_lru" },
            "sparse_template_cache_limit_bytes": self.options.sparse_template_cache_bytes,
            "sparse_template_minimum_threads": self.template_min_threads,
            "sparse_template_active_for_current_pool": self.options.sparse_template_cache_bytes>0 && rayon::current_num_threads()>=self.template_min_threads,
            "sparse_template_cache_entries": entries,
            "sparse_template_cache_selected_kinds": kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            "sparse_template_cache_hits": cache.hits,
            "sparse_template_cache_misses": cache.misses,
            "sparse_template_cache_stores": cache.stores,
            "sparse_template_cache_evictions": cache.evictions,
            "sparse_template_cache_releases": cache.releases,
            "sparse_template_reuse_scope": "immutable_local_incidence_only",
            "sparse_template_duplicate_reduction": "authored_order_direct_to_canonical_csr_slots",
            "contribution_id": self.contribution_id,
            "global_dense_jacobian_allocated": false,
            "dense_state_jacobian_bytes_avoided": 8u128 * (self.state_size as u128) * (self.state_size as u128),
        })
    }
}

fn structural_id(
    state_size: usize,
    design_size: usize,
    batch_size: usize,
    arrays: [&Incidence; 4],
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"implexity-local-assembly-contribution/1\0");
    for number in [state_size, design_size, batch_size, arrays[0].count] {
        digest.update((number as u64).to_be_bytes());
    }
    for (label, (array, bound)) in ["rows", "current", "previous", "design"].iter().zip(arrays.iter().zip([
        state_size,
        state_size,
        state_size,
        design_size,
    ])) {
        digest.update(label.as_bytes());
        digest.update(b"\0");
        digest.update(2u16.to_be_bytes());
        digest.update((array.count as u64).to_be_bytes());
        digest.update((array.width as u64).to_be_bytes());
        let width = if i32::try_from(bound).is_ok() { 4 } else { 8 };
        digest.update(index_bytes(&array.values, width));
    }
    format!("structure:{}", hex::encode(digest.finalize()))
}

