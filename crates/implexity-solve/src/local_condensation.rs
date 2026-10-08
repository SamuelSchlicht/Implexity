// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::collections::{BTreeSet,BTreeMap};
use std::sync::Arc;

use implexity_linalg::dense::{DenseLu, DenseMatrix};
use implexity_linalg::error::LinalgError;
use implexity_linalg::lu::{LuSymbolic, Parallelism, SparseLu};
use implexity_linalg::sparse::CscMatrix;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalEliminationPartition {
    size: usize,
    groups: Vec<Vec<usize>>,
    retained: Vec<usize>,
    positions: Vec<(usize, usize)>,
}

impl LocalEliminationPartition {
    pub fn new(size: usize, groups: Vec<Vec<usize>>) -> Result<Self, LinalgError> {
        let mut positions = vec![(usize::MAX, usize::MAX); size];
        if groups.is_empty() {
            return Err(LinalgError::Shape("local elimination requires index groups".into()));
        }
        for (g, indices) in groups.iter().enumerate() {
            if indices.is_empty() {
                return Err(LinalgError::Shape("local elimination group is empty".into()));
            }
            for (i, &index) in indices.iter().enumerate() {
                if index >= size || positions[index].0 != usize::MAX {
                    return Err(LinalgError::Shape("local elimination indices overlap or exceed the state".into()));
                }
                positions[index] = (g, i);
            }
        }
        let retained: Vec<usize> = (0..size).filter(|&i| positions[i].0 == usize::MAX).collect();
        for (i, &index) in retained.iter().enumerate() {
            positions[index].1 = i;
        }
        Ok(Self { size, groups, retained, positions })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn eliminated_size(&self) -> usize {
        self.size - self.retained.len()
    }
}

struct LocalBlock {
    indices: Vec<usize>,
    front: Vec<usize>,
    factor: DenseLu,
    b: Vec<f64>,
    c: Vec<f64>,
}

pub(crate) struct LocalCondensation {
    size: usize,
    retained: Vec<usize>,
    blocks: Vec<LocalBlock>,
    schur: Option<SparseLu>,
    retained_bytes: usize,
}

impl LocalCondensation {
    pub(crate) fn new(
        a: &CscMatrix,
        partition: &LocalEliminationPartition,
        symbolic: impl Fn(&CscMatrix) -> Result<Arc<LuSymbolic>, LinalgError>,
    ) -> Result<Self, LinalgError> {
        if a.shape() != (partition.size, partition.size) {
            return Err(LinalgError::Shape("local elimination partition has another matrix size".into()));
        }
        let ng = partition.groups.len();
        let mut ds: Vec<DenseMatrix> = partition.groups.iter().map(|g| DenseMatrix::zeros(g.len(), g.len())).collect();
        let mut bs = vec![Vec::<(usize, usize, f64)>::new(); ng];
        let mut cs = vec![Vec::<(usize, usize, f64)>::new(); ng];
        let nr=partition.retained.len();
        let mut entries=vec![BTreeMap::<usize,f64>::new();nr];
        for col in 0..partition.size {
            let (cg, ci) = partition.positions[col];
            for entry in a.indptr()[col]..a.indptr()[col + 1] {
                let row = a.indices()[entry];
                let value = a.data()[entry];
                let (rg, ri) = partition.positions[row];
                match (rg == usize::MAX, cg == usize::MAX) {
                    (true, true) => {
                        *entries[ci].entry(ri).or_insert(0.)+=value;
                    }
                    (true, false) => bs[cg].push((ri, ci, value)),
                    (false, true) => cs[rg].push((ri, ci, value)),
                    (false, false) if rg == cg => { let width = ds[rg].ncols; ds[rg].data[ri * width + ci] += value; },
                    (false, false) if value == 0.0 => {}
                    _ => return Err(LinalgError::Shape("local elimination groups have nonlocal matrix coupling".into())),
                }
            }
        }
        let mut blocks = Vec::with_capacity(ng);
        let mut retained_bytes = partition.size * size_of::<usize>();
        for (g, indices) in partition.groups.iter().enumerate() {
            let m = indices.len();
            let front: Vec<usize> = bs[g].iter().map(|e| e.0).chain(cs[g].iter().map(|e| e.1)).collect::<BTreeSet<_>>().into_iter().collect();
            let k = front.len();
            let mut b = vec![0.0; k * m];
            let mut c = vec![0.0; m * k];
            for &(r, q, v) in &bs[g] {
                b[front.binary_search(&r).map_err(|_| LinalgError::Shape("local front mismatch".into()))? * m + q] += v;
            }
            for &(r, q, v) in &cs[g] {
                c[r * k + front.binary_search(&q).map_err(|_| LinalgError::Shape("local front mismatch".into()))?] += v;
            }
            let factor = DenseLu::new(&ds[g])?;
            if k != 0 {
                let dc = factor.solve(&c, k, false)?;
                if dc.iter().any(|v| !v.is_finite()) {
                    return Err(LinalgError::NonFinite("non-finite local elimination solve".into()));
                }
                for r in 0..k {
                    for q in 0..k {
                        let v = (0..m).map(|j| b[r * m + j] * dc[j * k + q]).sum::<f64>();
                        *entries[front[q]].entry(front[r]).or_insert(0.)+=-v;
                    }
                }
            }
            retained_bytes += (m * m + b.len() + c.len()) * size_of::<f64>() + (3 * m + k) * size_of::<usize>();
            blocks.push(LocalBlock { indices: indices.clone(), front, factor, b, c });
        }
        let schur = if nr == 0 {
            None
        } else {
            let mut ptr=Vec::with_capacity(nr+1);let mut indices=Vec::new();let mut values=Vec::new();ptr.push(0);
            for column in entries{for(row,value)in column{indices.push(row);values.push(value);}ptr.push(indices.len());}
            let s = CscMatrix::try_new(nr,nr,ptr,indices,values)?;
            if s.data().iter().any(|v| !v.is_finite()) {
                return Err(LinalgError::NonFinite("non-finite condensed matrix".into()));
            }
            let lu = symbolic(&s)?.factor(&s, Parallelism::Sequential)?;
            retained_bytes += lu.nbytes() + s.nbytes();
            Some(lu)
        };
        Ok(Self { size: partition.size, retained: partition.retained.clone(), blocks, schur, retained_bytes })
    }

    pub(crate) fn nbytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn retained_size(&self) -> usize {
        self.retained.len()
    }

    pub(crate) fn solve(&self, rhs: &[f64], columns: usize, transpose: bool) -> Result<Vec<f64>, LinalgError> {
        if columns == 0 || rhs.len() != self.size * columns {
            return Err(LinalgError::Shape("local elimination right-hand side has another shape".into()));
        }
        let mut global = vec![0.0; self.retained.len() * columns];
        for (r, &index) in self.retained.iter().enumerate() {
            global[r * columns..(r + 1) * columns].copy_from_slice(&rhs[index * columns..(index + 1) * columns]);
        }
        for block in &self.blocks {
            let m = block.indices.len();
            let k = block.front.len();
            let mut local = vec![0.0; m * columns];
            for (r, &index) in block.indices.iter().enumerate() {
                local[r * columns..(r + 1) * columns].copy_from_slice(&rhs[index * columns..(index + 1) * columns]);
            }
            let dl = block.factor.solve(&local, columns, transpose)?;
            for r in 0..k {
                for column in 0..columns {
                    let v = (0..m).map(|j| {
                        let coefficient = if transpose { block.c[j * k + r] } else { block.b[r * m + j] };
                        coefficient * dl[j * columns + column]
                    }).sum::<f64>();
                    global[block.front[r] * columns + column] -= v;
                }
            }
        }
        if let Some(schur) = &self.schur {
            let n = self.retained.len();
            let mut col = vec![0.0; n * columns];
            for r in 0..n {
                for column in 0..columns {
                    col[column * n + r] = global[r * columns + column];
                }
            }
            schur.solve_many_in_place(&mut col, columns, transpose)?;
            for r in 0..n {
                for column in 0..columns {
                    global[r * columns + column] = col[column * n + r];
                }
            }
        }
        let mut result = vec![0.0; rhs.len()];
        for (r, &index) in self.retained.iter().enumerate() {
            result[index * columns..(index + 1) * columns].copy_from_slice(&global[r * columns..(r + 1) * columns]);
        }
        for block in &self.blocks {
            let m = block.indices.len();
            let k = block.front.len();
            let mut local = vec![0.0; m * columns];
            for r in 0..m {
                for column in 0..columns {
                    let correction = (0..k).map(|j| {
                        let coefficient = if transpose { block.b[j * m + r] } else { block.c[r * k + j] };
                        coefficient * global[block.front[j] * columns + column]
                    }).sum::<f64>();
                    local[r * columns + column] = rhs[block.indices[r] * columns + column] - correction;
                }
            }
            let solved = block.factor.solve(&local, columns, transpose)?;
            for (r, &index) in block.indices.iter().enumerate() {
                result[index * columns..(index + 1) * columns].copy_from_slice(&solved[r * columns..(r + 1) * columns]);
            }
        }
        if result.iter().any(|v| !v.is_finite()) {
            return Err(LinalgError::NonFinite("non-finite recovered local elimination solve".into()));
        }
        Ok(result)
    }
}
