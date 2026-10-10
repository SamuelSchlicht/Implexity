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

#[derive(Clone)]
enum ColumnOffsets {
    U8(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    Usize(Vec<usize>),
}
#[derive(Clone)]
pub struct CompactSymmetricRows {
    row_offsets: Vec<usize>,
    columns: ColumnOffsets,
    values: Vec<f64>,
    nonzeros: usize,
}
impl CompactSymmetricRows {
    pub fn new(rows: &[Vec<(usize, f64)>]) -> Result<Self, crate::LinalgError> {
        let dimension = rows.len();
        let mut offsets = vec![0];
        let mut values = vec![];
        let mut columns = vec![];
        let mut maximum_offset = 0;
        let mut nonzeros = 0usize;
        for (i, row) in rows.iter().enumerate() {
            let mut previous = None;
            for &(j, value) in row {
                if j < i || j >= dimension || !value.is_finite() || previous.is_some_and(|p| j <= p)
                {
                    return Err(crate::LinalgError::Invalid(
                        "invalid symmetric upper row".into(),
                    ));
                }
                previous = Some(j);
                let offset = j - i;
                maximum_offset = maximum_offset.max(offset);
                nonzeros = nonzeros
                    .checked_add(if j == i { 1 } else { 2 })
                    .ok_or_else(|| {
                        crate::LinalgError::Shape("symmetric nonzero count overflow".into())
                    })?;
                columns.push(offset);
                values.push(value);
            }
            offsets.push(values.len());
        }
        let columns = if maximum_offset <= u8::MAX as usize {
            ColumnOffsets::U8(columns.into_iter().map(|v| v as u8).collect())
        } else if maximum_offset <= u16::MAX as usize {
            ColumnOffsets::U16(columns.into_iter().map(|v| v as u16).collect())
        } else if maximum_offset <= u32::MAX as usize {
            ColumnOffsets::U32(columns.into_iter().map(|v| v as u32).collect())
        } else {
            ColumnOffsets::Usize(columns)
        };
        offsets.shrink_to_fit();
        values.shrink_to_fit();
        Ok(Self {
            row_offsets: offsets,
            columns,
            values,
            nonzeros,
        })
    }
    pub fn dimension(&self) -> usize {
        self.row_offsets.len() - 1
    }
    pub fn nonzeros(&self) -> usize {
        self.nonzeros
    }
    pub fn visit_row(
        &self,
        row: usize,
        mut visit: impl FnMut(usize, f64),
    ) -> Result<(), crate::LinalgError> {
        if row >= self.dimension() {
            return Err(crate::LinalgError::Invalid(
                "symmetric row outside matrix".into(),
            ));
        }
        let range = self.row_offsets[row]..self.row_offsets[row + 1];
        let values = &self.values[range.clone()];
        match &self.columns {
            ColumnOffsets::U8(c) => {
                for (&j, &v) in c[range].iter().zip(values) {
                    visit(row + j as usize, v);
                }
            }
            ColumnOffsets::U16(c) => {
                for (&j, &v) in c[range].iter().zip(values) {
                    visit(row + j as usize, v);
                }
            }
            ColumnOffsets::U32(c) => {
                for (&j, &v) in c[range].iter().zip(values) {
                    visit(row + j as usize, v);
                }
            }
            ColumnOffsets::Usize(c) => {
                for (&j, &v) in c[range].iter().zip(values) {
                    visit(row + j, v);
                }
            }
        }
        Ok(())
    }
    pub fn allocated_storage_bytes(&self) -> usize {
        self.row_offsets.capacity() * std::mem::size_of::<usize>()
            + self.values.capacity() * std::mem::size_of::<f64>()
            + match &self.columns {
                ColumnOffsets::U8(v) => v.capacity(),
                ColumnOffsets::U16(v) => v.capacity() * 2,
                ColumnOffsets::U32(v) => v.capacity() * 4,
                ColumnOffsets::Usize(v) => v.capacity() * std::mem::size_of::<usize>(),
            }
    }
    pub fn apply(&self, input: &[f64]) -> Result<Vec<f64>, crate::LinalgError> {
        if input.len() != self.dimension() || input.iter().any(|v| !v.is_finite()) {
            return Err(crate::LinalgError::Invalid(
                "symmetric input shape or finite domain".into(),
            ));
        }
        let mut out = vec![0.; input.len()];
        for i in 0..input.len() {
            self.visit_row(i, |j, v| {
                out[i] += v * input[j];
                if j != i {
                    out[j] += v * input[i];
                }
            })?;
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(crate::LinalgError::Invalid(
                "symmetric action overflow".into(),
            ));
        }
        Ok(out)
    }
}
