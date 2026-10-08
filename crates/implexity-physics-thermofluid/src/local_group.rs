// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::Value;

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::local_assembly::{
    AssemblyOptions, Incidence, Kind, LocalResidual, LocalResidualAssembly,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Retained,
    Caloric,
    Dissipation,
}

pub trait StepKernel: LocalResidual + Send + Sync + 'static {
    fn data_width(&self) -> usize {
        0
    }
    fn step_data(&self, _n: usize, _out: &mut [f64]) {}
    fn prescribed_state(&self, _n: usize, _current: &mut [f64], _previous: &mut [f64]) {}
}

pub trait GroupOps: Send + Sync {

    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>>;

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix>;

    #[allow(clippy::too_many_arguments)]
    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>>;

    fn local_values(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Vec<f64>>;
    fn rows(&self) -> &Incidence;
    fn report(&self) -> Value;
    fn template_capacity(&self, kinds: &[Kind]) -> usize;
}

pub struct Group<K: StepKernel> {
    asm: LocalResidualAssembly<K>,
    state_width: usize,
    rows: Incidence,
    current: Incidence,
    design: Incidence,
}


#[allow(clippy::too_many_arguments)]
pub fn group<K: StepKernel>(
    kernel: K,
    rows: &[Vec<i64>],
    current: &[Vec<i64>],
    design: &[Vec<i64>],
    state_size: usize,
    design_size: usize,
    batch_size: usize,
) -> CaeResult<Option<Box<dyn GroupOps>>> {
    Ok(typed_group(kernel, rows, current, design, state_size, design_size, batch_size)?
        .map(|g| Box::new(g) as Box<dyn GroupOps>))
}


#[allow(clippy::too_many_arguments)]
pub fn typed_group<K: StepKernel>(
    kernel: K,
    rows: &[Vec<i64>],
    current: &[Vec<i64>],
    design: &[Vec<i64>],
    state_size: usize,
    design_size: usize,
    batch_size: usize,
) -> CaeResult<Option<Group<K>>> {
    let count = rows.len();
    if count == 0 {
        return Ok(None);
    }
    let rw = rows[0].len();
    let sw = current[0].len();
    let dw = design[0].len();
    let extra = kernel.data_width();
    let width = sw + extra;
    let mut cur = Vec::with_capacity(count * width);
    for c in current {
        if c.len() != sw {
            return Err(CaeError::contract("ragged local residual group incidence"));
        }
        cur.extend_from_slice(c);
        cur.extend(std::iter::repeat_n(-1, extra));
    }
    let row_values: Vec<i64> = rows.iter().flatten().copied().collect();
    let design_values: Vec<i64> = design.iter().flatten().copied().collect();
    if row_values.len() != count * rw
        || design_values.len() != count * dw
        || current.len() != count
        || design.len() != count
    {
        return Err(CaeError::contract("ragged local residual group incidence"));
    }
    let current_inc = Incidence::new(count, width, cur)?;
    let rows_inc = Incidence::new(count, rw, row_values)?;
    let design_inc = Incidence::new(count, dw, design_values)?;
    let mut asm = LocalResidualAssembly::new(
        kernel,
        rows_inc.clone(),
        current_inc.clone(),
        current_inc.clone(),
        design_inc.clone(),
        state_size,
        design_size,
        AssemblyOptions { batch_size, ..AssemblyOptions::default() },
    )?;
    asm.cache_sparse_templates(&[Kind::Current,Kind::Previous],4);
    Ok(Some(Group { asm, state_width: sw, rows: rows_inc, current: current_inc, design: design_inc }))
}

impl<K: StepKernel> Group<K> {
    #[must_use]
    pub fn kernel(&self) -> &K {
        self.asm.kernel()
    }

    #[must_use]
    pub fn assembly(&self) -> &LocalResidualAssembly<K> {
        &self.asm
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.asm.count()
    }

    #[must_use]
    pub fn prescribed(&self, n: usize) -> (Vec<f64>, Vec<f64>) {
        let kernel = self.asm.kernel();
        let extra = kernel.data_width();
        let count = self.asm.count();
        let sw = self.state_width;
        let width = sw + extra;
        let mut state_c = vec![0.0; count * sw];
        let mut state_p = vec![0.0; count * sw];
        kernel.prescribed_state(n, &mut state_c, &mut state_p);
        if extra == 0 {
            return (state_c, state_p);
        }
        let mut data = vec![0.0; count * extra];
        if extra > 0 {
            kernel.step_data(n, &mut data);
        }
        let mut current = vec![0.0; count * width];
        let mut previous = vec![0.0; count * width];
        for e in 0..count {
            current[e * width..e * width + sw].copy_from_slice(&state_c[e * sw..(e + 1) * sw]);
            previous[e * width..e * width + sw].copy_from_slice(&state_p[e * sw..(e + 1) * sw]);
            current[e * width + sw..(e + 1) * width].copy_from_slice(&data[e * extra..(e + 1) * extra]);
            previous[e * width + sw..(e + 1) * width].copy_from_slice(&data[e * extra..(e + 1) * extra]);
        }
        (current, previous)
    }


    pub fn gathered(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let (c, _) = self.prescribed(n);
        if z.len() != self.asm.state_size() || x.len() != self.asm.design_size() {
            return Err(CaeError::contract("local group gather: invalid shape"));
        }
        let current = self
            .current
            .values
            .iter()
            .zip(&c)
            .map(|(i, v)| usize::try_from(*i).map_or(*v, |i| z[i]))
            .collect();
        let design = self.design.values.iter().map(|i| usize::try_from(*i).map_or(0.0, |i| x[i])).collect();
        Ok((current, design))
    }

    #[must_use]
    pub fn current_incidence(&self) -> &Incidence {
        &self.current
    }

    #[must_use]
    pub fn design_incidence(&self) -> &Incidence {
        &self.design
    }
}

impl<K: StepKernel> GroupOps for Group<K> {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let (c, p) = self.prescribed(n);
        self.asm.residual(z, old, x, &c, &p)
    }
    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        let (c, p) = self.prescribed(n);
        self.asm.jacobian(kind, z, old, x, &c, &p)
    }
    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        let (c, p) = self.prescribed(n);
        self.asm.current_action(z, old, x, &c, &p, v, transpose)
    }
    fn local_values(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let (current, design) = self.gathered(n, z, x)?;
        let count = self.asm.count();
        let (cw, dw, rw) = (current.len() / count, design.len() / count, self.rows().width);
        let mut out = vec![0.0; count * rw];
        for e in 0..count {
            let c = &current[e * cw..(e + 1) * cw];
            let d = &design[e * dw..(e + 1) * dw];
            self.asm.kernel().residual::<f64>(e, c, c, d, &mut out[e * rw..(e + 1) * rw]);
        }
        Ok(out)
    }
    fn rows(&self) -> &Incidence {
        &self.rows
    }
    fn report(&self) -> Value {

        let mut out = self.asm.report();
        let local = self.rows.width * self.state_width.max(self.design.width);
        if let Some(batch) = out["batch_size"].as_u64() {
            out["maximum_local_derivative_entries_per_batch"] = Value::from(batch as usize * local);
        }
        out
    }
    fn template_capacity(&self, kinds: &[Kind]) -> usize {
        self.asm.sparse_template_capacity_bytes(kinds)
    }
}


pub fn sum_jacobians<'a>(
    groups: impl IntoIterator<Item = &'a dyn GroupOps>,
    kind: Kind,
    n: usize,
    z: &[f64],
    old: &[f64],
    x: &[f64],
    shape: (usize, usize),
) -> CaeResult<CsrMatrix> {
    let mut rows = Vec::new();
    let mut columns = Vec::new();
    let mut values = Vec::new();
    for g in groups {
        let m = g.jacobian(kind, n, z, old, x)?;
        for i in 0..m.nrows() {
            let (idx, val) = m.row(i);
            rows.extend(std::iter::repeat_n(i, idx.len()));
            columns.extend_from_slice(idx);
            values.extend_from_slice(val);
        }
    }
    let m = CsrMatrix::from_triplets(shape.0, shape.1, &rows, &columns, &values)
        .map_err(|e| CaeError::contract(e.to_string()))?;
    Ok(implexity_solve::matrix::eliminate_zeros(&m))
}
