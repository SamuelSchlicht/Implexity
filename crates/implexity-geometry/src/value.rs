// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use crate::pyfmt::{self, PyObj};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DType {
    F64,
    F32,
    I64,
    I32,
    I16,
    I8,
    U8,
    Bool,
}

impl DType {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::F64 => "float64",
            Self::F32 => "float32",
            Self::I64 => "int64",
            Self::I32 => "int32",
            Self::I16 => "int16",
            Self::I8 => "int8",
            Self::U8 => "uint8",
            Self::Bool => "bool",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "float64" => Self::F64,
            "float32" => Self::F32,
            "int64" => Self::I64,
            "int32" => Self::I32,
            "int16" => Self::I16,
            "int8" => Self::I8,
            "uint8" => Self::U8,
            "bool" => Self::Bool,
            _ => return None,
        })
    }

    #[must_use]
    pub fn itemsize(self) -> usize {
        match self {
            Self::F64 | Self::I64 => 8,
            Self::F32 | Self::I32 => 4,
            Self::I16 => 2,
            Self::I8 | Self::U8 | Self::Bool => 1,
        }
    }

    #[must_use]
    pub fn npy_descr(self) -> &'static str {
        match self {
            Self::F64 => "<f8",
            Self::F32 => "<f4",
            Self::I64 => "<i8",
            Self::I32 => "<i4",
            Self::I16 => "<i2",
            Self::I8 => "|i1",
            Self::U8 => "|u1",
            Self::Bool => "|b1",
        }
    }

    #[must_use]
    pub fn kind(self) -> char {
        match self {
            Self::F64 | Self::F32 => 'f',
            Self::I64 | Self::I32 | Self::I16 | Self::I8 => 'i',
            Self::U8 => 'u',
            Self::Bool => 'b',
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArrayData {
    F64(Vec<f64>),
    F32(Vec<f32>),
    I64(Vec<i64>),
    I32(Vec<i32>),
    I16(Vec<i16>),
    I8(Vec<i8>),
    U8(Vec<u8>),
    Bool(Vec<bool>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct NdArray {
    shape: Vec<usize>,
    data: ArrayData,
}

impl NdArray {
    #[must_use]
    pub fn from_f64(shape: Vec<usize>, data: Vec<f64>) -> Option<Self> {
        (shape.iter().product::<usize>() == data.len()).then_some(Self { shape, data: ArrayData::F64(data) })
    }

    #[must_use]
    pub fn new(shape: Vec<usize>, data: ArrayData) -> Option<Self> {
        let n = match &data {
            ArrayData::F64(v) => v.len(),
            ArrayData::F32(v) => v.len(),
            ArrayData::I64(v) => v.len(),
            ArrayData::I32(v) => v.len(),
            ArrayData::I16(v) => v.len(),
            ArrayData::I8(v) => v.len(),
            ArrayData::U8(v) => v.len(),
            ArrayData::Bool(v) => v.len(),
        };
        (shape.iter().product::<usize>() == n).then_some(Self { shape, data })
    }

    #[must_use]
    pub fn scalar(v: f64) -> Self {
        Self { shape: Vec::new(), data: ArrayData::F64(vec![v]) }
    }

    #[must_use]
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }

    #[must_use]
    pub fn ndim(&self) -> usize {
        self.shape.len()
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.shape.iter().product()
    }

    #[must_use]
    pub fn dtype(&self) -> DType {
        match &self.data {
            ArrayData::F64(_) => DType::F64,
            ArrayData::F32(_) => DType::F32,
            ArrayData::I64(_) => DType::I64,
            ArrayData::I32(_) => DType::I32,
            ArrayData::I16(_) => DType::I16,
            ArrayData::I8(_) => DType::I8,
            ArrayData::U8(_) => DType::U8,
            ArrayData::Bool(_) => DType::Bool,
        }
    }

    #[must_use]
    pub fn data(&self) -> &ArrayData {
        &self.data
    }

    #[must_use]
    pub fn to_f64_vec(&self) -> Vec<f64> {
        match &self.data {
            ArrayData::F64(v) => v.clone(),
            ArrayData::F32(v) => v.iter().map(|x| f64::from(*x)).collect(),
            #[allow(clippy::cast_precision_loss)]
            ArrayData::I64(v) => v.iter().map(|x| *x as f64).collect(),
            ArrayData::I32(v) => v.iter().map(|x| f64::from(*x)).collect(),
            ArrayData::I16(v) => v.iter().map(|x| f64::from(*x)).collect(),
            ArrayData::I8(v) => v.iter().map(|x| f64::from(*x)).collect(),
            ArrayData::U8(v) => v.iter().map(|x| f64::from(*x)).collect(),
            ArrayData::Bool(v) => v.iter().map(|x| if *x { 1.0 } else { 0.0 }).collect(),
        }
    }

    #[must_use]
    pub fn as_f64_slice(&self) -> Option<&[f64]> {
        match &self.data {
            ArrayData::F64(v) => Some(v),
            _ => None,
        }
    }

    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.size() * self.dtype().itemsize());
        match &self.data {
            ArrayData::F64(v) => v.iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ArrayData::F32(v) => v.iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ArrayData::I64(v) => v.iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ArrayData::I32(v) => v.iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ArrayData::I16(v) => v.iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ArrayData::I8(v) => v.iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ArrayData::U8(v) => out.extend_from_slice(v),
            ArrayData::Bool(v) => v.iter().for_each(|x| out.push(u8::from(*x))),
        }
        out
    }

    #[must_use]
    pub fn from_le_bytes(dtype: DType, shape: Vec<usize>, raw: &[u8]) -> Option<Self> {
        let n: usize = shape.iter().product();
        if raw.len() != n * dtype.itemsize() {
            return None;
        }
        let data = match dtype {
            DType::F64 => ArrayData::F64(
                raw.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).collect(),
            ),
            DType::F32 => ArrayData::F32(
                raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap_or([0; 4]))).collect(),
            ),
            DType::I64 => ArrayData::I64(
                raw.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).collect(),
            ),
            DType::I32 => ArrayData::I32(
                raw.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap_or([0; 4]))).collect(),
            ),
            DType::I16 => ArrayData::I16(
                raw.chunks_exact(2).map(|c| i16::from_le_bytes(c.try_into().unwrap_or([0; 2]))).collect(),
            ),
            DType::I8 => ArrayData::I8(raw.iter().map(|b| i8::from_le_bytes([*b])).collect()),
            DType::U8 => ArrayData::U8(raw.to_vec()),
            DType::Bool => ArrayData::Bool(raw.iter().map(|b| *b != 0).collect()),
        };
        Some(Self { shape, data })
    }

    #[must_use]
    pub fn to_json_list(&self) -> serde_json::Value {
        fn build(shape: &[usize], flat: &[serde_json::Value], offset: &mut usize) -> serde_json::Value {
            if shape.is_empty() {
                let v = flat.get(*offset).cloned().unwrap_or(serde_json::Value::Null);
                *offset += 1;
                return v;
            }
            let mut items = Vec::with_capacity(shape[0]);
            for _ in 0..shape[0] {
                items.push(build(&shape[1..], flat, offset));
            }
            serde_json::Value::Array(items)
        }
        let flat: Vec<serde_json::Value> = match &self.data {
            ArrayData::F64(v) => v.iter().map(|x| json_f64(*x)).collect(),
            ArrayData::F32(v) => v.iter().map(|x| json_f64(f64::from(*x))).collect(),
            ArrayData::I64(v) => v.iter().map(|x| serde_json::Value::from(*x)).collect(),
            ArrayData::I32(v) => v.iter().map(|x| serde_json::Value::from(*x)).collect(),
            ArrayData::I16(v) => v.iter().map(|x| serde_json::Value::from(*x)).collect(),
            ArrayData::I8(v) => v.iter().map(|x| serde_json::Value::from(*x)).collect(),
            ArrayData::U8(v) => v.iter().map(|x| serde_json::Value::from(*x)).collect(),
            ArrayData::Bool(v) => v.iter().map(|x| serde_json::Value::from(*x)).collect(),
        };
        let mut offset = 0;
        build(&self.shape, &flat, &mut offset)
    }
}

#[must_use]
pub fn json_f64(x: f64) -> serde_json::Value {
    serde_json::Number::from_f64(x).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Float(f64),
    Int(i64),
    Bool(bool),
    Str(String),
    List(Vec<ParamValue>),
    Tuple(Vec<ParamValue>),
    Array(Arc<NdArray>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArrayViewError {
    Ragged,
    NotNumeric,
}

impl ParamValue {
    #[must_use]
    pub fn array_f64(shape: Vec<usize>, data: Vec<f64>) -> Option<Self> {
        NdArray::from_f64(shape, data).map(|a| Self::Array(Arc::new(a)))
    }


    pub fn as_ndarray(&self) -> Result<NdArray, ArrayViewError> {
        match self {
            Self::Array(a) => Ok((**a).clone()),
            Self::Float(f) => Ok(NdArray::scalar(*f)),
            Self::Int(i) => Ok(NdArray { shape: Vec::new(), data: ArrayData::I64(vec![*i]) }),
            Self::Bool(b) => Ok(NdArray { shape: Vec::new(), data: ArrayData::Bool(vec![*b]) }),
            Self::Str(_) => Err(ArrayViewError::NotNumeric),
            Self::List(items) | Self::Tuple(items) => {
                let mut shape = Vec::new();
                let mut leaves = Vec::new();
                collect_leaves(items, 0, &mut shape, &mut leaves)?;
                let any_float = leaves.iter().any(|l| matches!(l, Leaf::F(_)));
                let all_bool = !leaves.is_empty() && leaves.iter().all(|l| matches!(l, Leaf::B(_)));
                let data = if leaves.is_empty() || any_float {
                    #[allow(clippy::cast_precision_loss)]
                    ArrayData::F64(
                        leaves
                            .iter()
                            .map(|l| match l {
                                Leaf::F(f) => *f,
                                Leaf::I(i) => *i as f64,
                                Leaf::B(b) => f64::from(u8::from(*b)),
                            })
                            .collect(),
                    )
                } else if all_bool {
                    ArrayData::Bool(leaves.iter().map(|l| matches!(l, Leaf::B(true))).collect())
                } else {
                    ArrayData::I64(
                        leaves
                            .iter()
                            .map(|l| match l {
                                Leaf::I(i) => *i,
                                Leaf::B(b) => i64::from(*b),
                                Leaf::F(_) => 0,
                            })
                            .collect(),
                    )
                };
                Ok(NdArray { shape, data })
            }
        }
    }


    pub fn to_f64_array(&self) -> Result<(Vec<usize>, Vec<f64>), ArrayViewError> {
        let a = self.as_ndarray()?;
        Ok((a.shape.clone(), a.to_f64_vec()))
    }

    #[must_use]
    pub fn scalar_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            #[allow(clippy::cast_precision_loss)]
            Self::Int(i) => Some(*i as f64),
            Self::Bool(b) => Some(f64::from(u8::from(*b))),
            _ => {
                let (_, v) = self.to_f64_array().ok()?;
                (v.len() == 1).then(|| v[0])
            }
        }
    }

    #[must_use]
    pub fn ndim(&self) -> Option<usize> {
        self.as_ndarray().ok().map(|a| a.ndim())
    }

    #[must_use]
    pub fn value_bytes(&self) -> Vec<u8> {
        if let Self::Str(s) = self {

            let n = s.chars().count();
            let mut out =
                format!("(){}", if n == 0 { "<U1".to_string() } else { format!("<U{n}") }).into_bytes();
            if n == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
            for c in s.chars() {
                out.extend_from_slice(&(c as u32).to_le_bytes());
            }
            return out;
        }
        match self.as_ndarray() {
            Ok(a) => {
                let mut out = pyfmt::shape_str(a.shape()).into_bytes();
                out.extend_from_slice(a.dtype().name().as_bytes());
                out.extend_from_slice(&a.to_le_bytes());
                out
            }
            Err(_) => self.py_obj().repr().into_bytes(),
        }
    }

    #[must_use]
    pub fn py_obj(&self) -> PyObj {
        match self {
            Self::Float(f) => PyObj::Float(*f),
            Self::Int(i) => PyObj::Int(*i),
            Self::Bool(b) => PyObj::Bool(*b),
            Self::Str(s) => PyObj::Str(s.clone()),
            Self::List(v) => PyObj::List(v.iter().map(Self::py_obj).collect()),
            Self::Tuple(v) => PyObj::Tuple(v.iter().map(Self::py_obj).collect()),
            Self::Array(a) => match a.to_json_list() {
                serde_json::Value::Array(_) => PyObj::Str(format!("array(shape={:?})", a.shape())),
                v => json_to_pyobj(&v),
            },
        }
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Float(f) => json_f64(*f),
            Self::Int(i) => serde_json::Value::from(*i),
            Self::Bool(b) => serde_json::Value::from(*b),
            Self::Str(s) => serde_json::Value::from(s.clone()),
            Self::List(v) | Self::Tuple(v) => serde_json::Value::Array(v.iter().map(Self::to_json).collect()),
            Self::Array(a) => a.to_json_list(),
        }
    }

    #[must_use]
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        match v {
            serde_json::Value::Bool(b) => Some(Self::Bool(*b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Some(Self::Int(i))
                } else {
                    n.as_f64().map(Self::Float)
                }
            }
            serde_json::Value::String(s) => Some(Self::Str(s.clone())),
            serde_json::Value::Array(items) => {
                items.iter().map(Self::from_json).collect::<Option<Vec<_>>>().map(Self::List)
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }
}

#[must_use]
pub fn json_to_pyobj(v: &serde_json::Value) -> PyObj {
    match v {
        serde_json::Value::Null => PyObj::None,
        serde_json::Value::Bool(b) => PyObj::Bool(*b),
        serde_json::Value::Number(n) => {
            n.as_i64().map_or_else(|| PyObj::Float(n.as_f64().unwrap_or(f64::NAN)), PyObj::Int)
        }
        serde_json::Value::String(s) => PyObj::Str(s.clone()),
        serde_json::Value::Array(items) => PyObj::List(items.iter().map(json_to_pyobj).collect()),
        serde_json::Value::Object(m) => {
            PyObj::Dict(m.iter().map(|(k, v)| (k.clone(), json_to_pyobj(v))).collect())
        }
    }
}

enum Leaf {
    F(f64),
    I(i64),
    B(bool),
}

fn collect_leaves(
    items: &[ParamValue],
    depth: usize,
    shape: &mut Vec<usize>,
    leaves: &mut Vec<Leaf>,
) -> Result<(), ArrayViewError> {
    if shape.len() == depth {
        shape.push(items.len());
    } else if shape[depth] != items.len() {
        return Err(ArrayViewError::Ragged);
    }
    let nested = items
        .iter()
        .filter(|i| matches!(i, ParamValue::List(_) | ParamValue::Tuple(_) | ParamValue::Array(_)))
        .count();
    if nested != 0 && nested != items.len() {
        return Err(ArrayViewError::Ragged);
    }
    for it in items {
        match it {
            ParamValue::Float(f) => leaves.push(Leaf::F(*f)),
            ParamValue::Int(i) => leaves.push(Leaf::I(*i)),
            ParamValue::Bool(b) => leaves.push(Leaf::B(*b)),
            ParamValue::Str(_) => return Err(ArrayViewError::NotNumeric),
            ParamValue::List(v) | ParamValue::Tuple(v) => collect_leaves(v, depth + 1, shape, leaves)?,
            ParamValue::Array(a) => {
                let sub = ParamValue::List(json_list_to_values(&a.to_json_list()));
                if let ParamValue::List(v) = sub {
                    collect_leaves(&v, depth + 1, shape, leaves)?;
                }
            }
        }
    }
    Ok(())
}

fn json_list_to_values(v: &serde_json::Value) -> Vec<ParamValue> {
    match v {
        serde_json::Value::Array(items) => items.iter().filter_map(ParamValue::from_json).collect(),
        other => ParamValue::from_json(other).into_iter().collect(),
    }
}

