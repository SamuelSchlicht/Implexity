// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use ndarray::{ArrayD, IxDyn};

pub const MAGIC: &[u8; 6] = b"\x93NUMPY";
pub const ARRAY_ALIGN: usize = 64;
pub const GROWTH_AXIS_MAX_DIGITS: usize = 21;
pub const MAX_HEADER_SIZE: usize = 10_000;

const MAX_HEADER_DEPTH: usize = 64;

const MAX_ZERO_WIDTH_ITEMS: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NpyError(pub String);

fn err(msg: impl Into<String>) -> NpyError {
    NpyError(msg.into())
}

#[derive(Debug, Clone, PartialEq)]
pub enum NpyData {
    Bool(Vec<bool>),
    I8(Vec<i8>),
    U8(Vec<u8>),
    I16(Vec<i16>),
    U16(Vec<u16>),
    I32(Vec<i32>),
    U32(Vec<u32>),
    I64(Vec<i64>),
    U64(Vec<u64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    C64(Vec<[f32; 2]>),
    C128(Vec<[f64; 2]>),
    Unicode {
        width: usize,
        values: Vec<String>,
    },
    Bytes {
        width: usize,
        values: Vec<Vec<u8>>,
    },
}

impl NpyData {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(v) => v.len(),
            Self::I8(v) => v.len(),
            Self::U8(v) => v.len(),
            Self::I16(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::I32(v) => v.len(),
            Self::U32(v) => v.len(),
            Self::I64(v) => v.len(),
            Self::U64(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::F64(v) => v.len(),
            Self::C64(v) => v.len(),
            Self::C128(v) => v.len(),
            Self::Unicode { values, .. } => values.len(),
            Self::Bytes { values, .. } => values.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn descr(&self) -> String {
        match self {
            Self::Bool(_) => "|b1".into(),
            Self::I8(_) => "|i1".into(),
            Self::U8(_) => "|u1".into(),
            Self::I16(_) => "<i2".into(),
            Self::U16(_) => "<u2".into(),
            Self::I32(_) => "<i4".into(),
            Self::U32(_) => "<u4".into(),
            Self::I64(_) => "<i8".into(),
            Self::U64(_) => "<u8".into(),
            Self::F32(_) => "<f4".into(),
            Self::F64(_) => "<f8".into(),
            Self::C64(_) => "<c8".into(),
            Self::C128(_) => "<c16".into(),
            Self::Unicode { width, .. } => format!("<U{width}"),
            Self::Bytes { width, .. } => format!("|S{width}"),
        }
    }

    #[must_use]
    pub fn itemsize(&self) -> usize {
        match self {
            Self::Bool(_) | Self::I8(_) | Self::U8(_) => 1,
            Self::I16(_) | Self::U16(_) => 2,
            Self::I32(_) | Self::U32(_) | Self::F32(_) => 4,
            Self::I64(_) | Self::U64(_) | Self::F64(_) | Self::C64(_) => 8,
            Self::C128(_) => 16,
            Self::Unicode { width, .. } => 4 * width,
            Self::Bytes { width, .. } => *width,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NpyArray {
    pub shape: Vec<usize>,
    pub data: NpyData,
}

fn product(shape: &[usize]) -> Option<usize> {
    shape.iter().try_fold(1usize, |acc, &d| acc.checked_mul(d))
}

impl NpyArray {

    pub fn new(shape: Vec<usize>, data: NpyData) -> Result<Self, NpyError> {
        if product(&shape) != Some(data.len()) {
            return Err(err(format!("shape {shape:?} does not hold {} elements", data.len())));
        }
        Ok(Self { shape, data })
    }

    #[must_use]
    pub fn from_f64(array: &ArrayD<f64>) -> Self {
        Self { shape: array.shape().to_vec(), data: NpyData::F64(array.iter().copied().collect()) }
    }

    #[must_use]
    pub fn from_i64(array: &ArrayD<i64>) -> Self {
        Self { shape: array.shape().to_vec(), data: NpyData::I64(array.iter().copied().collect()) }
    }

    #[must_use]
    pub fn vector_f64(values: Vec<f64>) -> Self {
        Self { shape: vec![values.len()], data: NpyData::F64(values) }
    }

    #[must_use]
    pub fn scalar_f64(value: f64) -> Self {
        Self { shape: Vec::new(), data: NpyData::F64(vec![value]) }
    }

    #[must_use]
    pub fn scalar_i64(value: i64) -> Self {
        Self { shape: Vec::new(), data: NpyData::I64(vec![value]) }
    }

    #[must_use]
    pub fn scalar_bool(value: bool) -> Self {
        Self { shape: Vec::new(), data: NpyData::Bool(vec![value]) }
    }

    #[must_use]
    pub fn scalar_str(value: &str) -> Self {
        Self {
            shape: Vec::new(),
            data: NpyData::Unicode { width: value.chars().count().max(1), values: vec![value.to_string()] },
        }
    }

    #[must_use]
    pub fn strings(values: &[String]) -> Self {
        let width = values.iter().map(|s| s.chars().count()).max().unwrap_or(0).max(1);
        Self { shape: vec![values.len()], data: NpyData::Unicode { width, values: values.to_vec() } }
    }

    #[must_use]
    pub fn as_f64(&self) -> Option<ArrayD<f64>> {
        match &self.data {
            NpyData::F64(v) => ArrayD::from_shape_vec(IxDyn(&self.shape), v.clone()).ok(),
            _ => None,
        }
    }

    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn to_f64(&self) -> Option<ArrayD<f64>> {
        let v: Vec<f64> = match &self.data {
            NpyData::Bool(v) => v.iter().map(|&b| if b { 1.0 } else { 0.0 }).collect(),
            NpyData::I8(v) => v.iter().map(|&x| f64::from(x)).collect(),
            NpyData::U8(v) => v.iter().map(|&x| f64::from(x)).collect(),
            NpyData::I16(v) => v.iter().map(|&x| f64::from(x)).collect(),
            NpyData::U16(v) => v.iter().map(|&x| f64::from(x)).collect(),
            NpyData::I32(v) => v.iter().map(|&x| f64::from(x)).collect(),
            NpyData::U32(v) => v.iter().map(|&x| f64::from(x)).collect(),
            NpyData::I64(v) => v.iter().map(|&x| x as f64).collect(),
            NpyData::U64(v) => v.iter().map(|&x| x as f64).collect(),
            NpyData::F32(v) => v.iter().map(|&x| f64::from(x)).collect(),
            NpyData::F64(v) => v.clone(),
            _ => return None,
        };
        ArrayD::from_shape_vec(IxDyn(&self.shape), v).ok()
    }

    #[must_use]
    pub fn to_i64(&self) -> Option<ArrayD<i64>> {
        let v: Vec<i64> = match &self.data {
            NpyData::I8(v) => v.iter().map(|&x| i64::from(x)).collect(),
            NpyData::U8(v) => v.iter().map(|&x| i64::from(x)).collect(),
            NpyData::I16(v) => v.iter().map(|&x| i64::from(x)).collect(),
            NpyData::U16(v) => v.iter().map(|&x| i64::from(x)).collect(),
            NpyData::I32(v) => v.iter().map(|&x| i64::from(x)).collect(),
            NpyData::U32(v) => v.iter().map(|&x| i64::from(x)).collect(),
            NpyData::I64(v) => v.clone(),
            NpyData::U64(v) => v.iter().map(|&x| i64::try_from(x).ok()).collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        ArrayD::from_shape_vec(IxDyn(&self.shape), v).ok()
    }

    #[must_use]
    pub fn as_scalar_str(&self) -> Option<&str> {
        match &self.data {
            NpyData::Unicode { values, .. } if self.shape.is_empty() => values.first().map(String::as_str),
            _ => None,
        }
    }


    pub fn to_bytes(&self) -> Result<Vec<u8>, NpyError> {
        let mut out = header_bytes(&self.data.descr(), false, &self.shape)?;
        write_data(&self.data, &mut out)?;
        Ok(out)
    }


    pub fn from_bytes(raw: &[u8]) -> Result<Self, NpyError> {
        let (header, offset) = split_header(raw)?;
        let h = parse_header(&header)?;
        let dtype = Dtype::parse(&h.descr)?;
        let count = product(&h.shape).ok_or_else(|| err("array size overflows"))?;
        if dtype.itemsize == 0 && count > MAX_ZERO_WIDTH_ITEMS {

            return Err(err("array of zero-width items is too large"));
        }
        let body = &raw[offset..];
        let need = count.checked_mul(dtype.itemsize).ok_or_else(|| err("array size overflows"))?;
        if body.len() < need {
            return Err(err(format!("EOF: reading array data, expected {need} bytes got {}", body.len())));
        }
        let data = dtype.decode(&body[..need], count)?;
        let data = if h.fortran_order && h.shape.len() > 1 { fortran_to_c(data, &h.shape) } else { data };
        Ok(Self { shape: h.shape, data })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpyHeader {
    pub descr: String,
    pub fortran_order: bool,
    pub shape: Vec<usize>,
}

fn shape_repr(shape: &[usize]) -> String {
    match shape.len() {
        0 => "()".into(),
        1 => format!("({},)", shape[0]),
        _ => format!("({})", shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}

#[must_use]
pub fn header_text(descr: &str, fortran_order: bool, shape: &[usize]) -> String {
    let mut header = format!(
        "{{'descr': {}, 'fortran_order': {}, 'shape': {}, }}",
        implexity_core::py_repr::repr_str(descr),
        if fortran_order { "True" } else { "False" },
        shape_repr(shape)
    );
    if !shape.is_empty() {
        let axis = if fortran_order { shape[shape.len() - 1] } else { shape[0] };
        let digits = axis.to_string().len();
        header.push_str(&" ".repeat(GROWTH_AXIS_MAX_DIGITS.saturating_sub(digits)));
    }
    header
}


pub fn header_bytes(descr: &str, fortran_order: bool, shape: &[usize]) -> Result<Vec<u8>, NpyError> {
    let text = header_text(descr, fortran_order, shape);
    let bytes = text.as_bytes();
    let hlen = bytes.len() + 1;
    for (major, len_size) in [(1u8, 2usize), (2u8, 4usize)] {
        let padlen = ARRAY_ALIGN - ((MAGIC.len() + 2 + len_size + hlen) % ARRAY_ALIGN);
        let total = hlen + padlen;
        let fits = if len_size == 2 { u16::try_from(total).is_ok() } else { u32::try_from(total).is_ok() };
        if !fits {
            continue;
        }
        let mut out = Vec::with_capacity(MAGIC.len() + 2 + len_size + total);
        out.extend_from_slice(MAGIC);
        out.push(major);
        out.push(0);
        if len_size == 2 {
            out.extend_from_slice(&u16::try_from(total).unwrap_or(0).to_le_bytes());
        } else {
            out.extend_from_slice(&u32::try_from(total).unwrap_or(0).to_le_bytes());
        }
        out.extend_from_slice(bytes);
        out.extend(std::iter::repeat_n(b' ', padlen));
        out.push(b'\n');
        return Ok(out);
    }
    Err(err(format!("Header length {hlen} too big for version=(2, 0)")))
}

fn write_data(data: &NpyData, out: &mut Vec<u8>) -> Result<(), NpyError> {
    match data {
        NpyData::Bool(v) => out.extend(v.iter().map(|&b| u8::from(b))),
        NpyData::I8(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::U8(v) => out.extend_from_slice(v),
        NpyData::I16(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::U16(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::I32(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::U32(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::I64(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::U64(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::F32(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::F64(v) => out.extend(v.iter().flat_map(|x| x.to_le_bytes())),
        NpyData::C64(v) => {
            out.extend(v.iter().flat_map(|[a, b]| a.to_le_bytes().into_iter().chain(b.to_le_bytes())));
        }
        NpyData::C128(v) => {
            out.extend(v.iter().flat_map(|[a, b]| a.to_le_bytes().into_iter().chain(b.to_le_bytes())));
        }
        NpyData::Unicode { width, values } => {
            for s in values {
                let n = s.chars().count();
                if n > *width {
                    return Err(err(format!("string of {n} code points exceeds <U{width}")));
                }
                for c in s.chars() {
                    out.extend_from_slice(&(c as u32).to_le_bytes());
                }
                out.extend(std::iter::repeat_n(0u8, 4 * (width - n)));
            }
        }
        NpyData::Bytes { width, values } => {
            for s in values {
                if s.len() > *width {
                    return Err(err(format!("bytes of length {} exceed |S{width}", s.len())));
                }
                out.extend_from_slice(s);
                out.extend(std::iter::repeat_n(0u8, width - s.len()));
            }
        }
    }
    Ok(())
}

fn split_header(raw: &[u8]) -> Result<(String, usize), NpyError> {
    if raw.len() < 10 || &raw[..6] != MAGIC {
        return Err(err("the magic string is not correct; expected b'\\x93NUMPY'"));
    }
    let (major, minor) = (raw[6], raw[7]);
    let (len, start): (usize, usize) = match (major, minor) {
        (1, 0) => (usize::from(u16::from_le_bytes([raw[8], raw[9]])), 10),
        (2 | 3, 0) => {
            if raw.len() < 12 {
                return Err(err("EOF: reading array header length"));
            }
            let l = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
            (usize::try_from(l).map_err(|_| err("header length overflows"))?, 12)
        }
        _ => {
            return Err(err(format!(
                "we only support format version (1,0), (2,0), and (3,0), not ({major}, {minor})"
            )));
        }
    };
    let end = start.checked_add(len).ok_or_else(|| err("header length overflows"))?;
    if raw.len() < end {
        return Err(err("EOF: reading array header"));
    }
    let text = if major == 3 {
        String::from_utf8(raw[start..end].to_vec()).map_err(|_| err("header is not UTF-8"))?
    } else {
        raw[start..end].iter().map(|&b| char::from(b)).collect()
    };
    if text.chars().count() > MAX_HEADER_SIZE {
        return Err(err(format!(
            "Header info length ({}) is large and may not be safe to load securely.",
            text.chars().count()
        )));
    }
    Ok((text, end))
}

#[derive(Debug, Clone, PartialEq)]
enum Lit {
    Str(String),
    Bool(bool),
    Int(i128),
    None,
    Tuple(Vec<Lit>),
    List(Vec<Lit>),
    Dict(Vec<(Lit, Lit)>),
}

struct LitParser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl LitParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Lit, NpyError> {
        self.ws();
        let bad = || err("Cannot parse header");
        match self.s.get(self.i).copied() {
            Some(q @ (b'\'' | b'"')) => {
                self.i += 1;
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i] != q {
                    if self.s[self.i] == b'\\' {
                        self.i += 1;
                    }
                    self.i += 1;
                }
                if self.i >= self.s.len() {
                    return Err(bad());
                }
                let text = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                self.i += 1;
                Ok(Lit::Str(text))
            }
            Some(b'{') => self.seq(b'}', true).map(|items| {
                Lit::Dict(
                    items.chunks(2).map(|p| (p[0].clone(), p.get(1).cloned().unwrap_or(Lit::None))).collect(),
                )
            }),
            Some(b'(') => self.seq(b')', false).map(Lit::Tuple),
            Some(b'[') => self.seq(b']', false).map(Lit::List),
            Some(b'T') if self.s[self.i..].starts_with(b"True") => {
                self.i += 4;
                Ok(Lit::Bool(true))
            }
            Some(b'F') if self.s[self.i..].starts_with(b"False") => {
                self.i += 5;
                Ok(Lit::Bool(false))
            }
            Some(b'N') if self.s[self.i..].starts_with(b"None") => {
                self.i += 4;
                Ok(Lit::None)
            }
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                self.i += 1;
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| bad())?;

                if matches!(self.s.get(self.i), Some(b'L' | b'l')) {
                    self.i += 1;
                }
                text.parse::<i128>().map(Lit::Int).map_err(|_| bad())
            }
            _ => Err(bad()),
        }
    }

    fn seq(&mut self, close: u8, dict: bool) -> Result<Vec<Lit>, NpyError> {
        if self.depth >= MAX_HEADER_DEPTH {
            return Err(err("Cannot parse header"));
        }
        self.depth += 1;
        let items = self.seq_items(close, dict);
        self.depth -= 1;
        items
    }

    fn seq_items(&mut self, close: u8, dict: bool) -> Result<Vec<Lit>, NpyError> {
        self.i += 1;
        let mut items = Vec::new();
        loop {
            self.ws();
            if self.s.get(self.i) == Some(&close) {
                self.i += 1;
                return Ok(items);
            }
            items.push(self.value()?);
            if dict {
                self.ws();
                if self.s.get(self.i) != Some(&b':') {
                    return Err(err("Cannot parse header"));
                }
                self.i += 1;
                items.push(self.value()?);
            }
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(c) if *c == close => {}
                _ => return Err(err("Cannot parse header")),
            }
        }
    }
}


pub fn parse_header(text: &str) -> Result<NpyHeader, NpyError> {
    let mut p = LitParser { s: text.as_bytes(), i: 0, depth: 0 };
    let lit = p
        .value()
        .map_err(|_| err(format!("Cannot parse header: {}", implexity_core::py_repr::repr_str(text))))?;
    let Lit::Dict(items) = lit else {
        return Err(err(format!("Header is not a dictionary: {}", implexity_core::py_repr::repr_str(text))));
    };
    let mut descr = None;
    let mut fortran = None;
    let mut shape = None;
    let mut keys = Vec::new();
    for (k, v) in items {
        let Lit::Str(k) = k else { return Err(err("Header does not contain the correct keys")) };
        keys.push(k.clone());
        match k.as_str() {
            "descr" => descr = Some(v),
            "fortran_order" => fortran = Some(v),
            "shape" => shape = Some(v),
            _ => {}
        }
    }
    keys.sort();
    if keys != ["descr", "fortran_order", "shape"] {
        return Err(err(format!("Header does not contain the correct keys: {keys:?}")));
    }
    let shape = match shape {
        Some(Lit::Tuple(items)) => items
            .into_iter()
            .map(|x| match x {
                Lit::Int(i) => usize::try_from(i).map_err(|_| err("shape is not valid")),
                _ => Err(err("shape is not valid")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(err("shape is not valid")),
    };
    let Some(Lit::Bool(fortran_order)) = fortran else {
        return Err(err("fortran_order is not a valid bool"));
    };
    let descr = match descr {
        Some(Lit::Str(s)) => s,
        Some(Lit::List(_) | Lit::Tuple(_)) => {
            return Err(err("structured dtypes are not supported without pickle-free field mapping"));
        }
        _ => return Err(err("descr is not a valid dtype descriptor")),
    };
    Ok(NpyHeader { descr, fortran_order, shape })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bool,
    Int,
    Uint,
    Float,
    Complex,
    Unicode,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dtype {
    kind: Kind,
    itemsize: usize,
    big: bool,
}

impl Dtype {
    fn parse(descr: &str) -> Result<Self, NpyError> {
        let (order, rest) = match descr.chars().next() {
            Some(c @ ('<' | '>' | '|' | '=')) => (c, &descr[1..]),
            _ => ('=', descr),
        };
        let big = order == '>';
        let (code, size) = rest.split_at(rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len()));
        let size: usize = if size.is_empty() {
            0
        } else {
            size.parse().map_err(|_| err(format!("data type {descr:?} not understood")))?
        };
        let (kind, itemsize) = match (code, size) {
            ("b", 1) | ("?", 0) => (Kind::Bool, 1),
            ("i", 1 | 2 | 4 | 8) => (Kind::Int, size),
            ("u", 1 | 2 | 4 | 8) => (Kind::Uint, size),
            ("f", 4 | 8) => (Kind::Float, size),
            ("c", 8 | 16) => (Kind::Complex, size),
            ("U", n) => (
                Kind::Unicode,
                n.checked_mul(4).ok_or_else(|| err(format!("data type {descr:?} not understood")))?,
            ),
            ("S" | "a", n) => (Kind::Bytes, n),
            ("O", _) => return Err(err("Object arrays cannot be loaded when allow_pickle=False")),
            _ => return Err(err(format!("unsupported dtype {descr:?} for pickle-free loading"))),
        };
        Ok(Self { kind, itemsize, big })
    }

    #[allow(clippy::too_many_lines)]
    fn decode(self, body: &[u8], count: usize) -> Result<NpyData, NpyError> {
        let chunks = body.chunks_exact(self.itemsize.max(1));
        macro_rules! nums {
            ($t:ty, $n:expr) => {{
                let v: Vec<$t> = chunks
                    .take(count)
                    .map(|c| {
                        let mut a = [0u8; $n];
                        a.copy_from_slice(c);
                        if self.big { <$t>::from_be_bytes(a) } else { <$t>::from_le_bytes(a) }
                    })
                    .collect();
                v
            }};
        }
        Ok(match (self.kind, self.itemsize) {
            (Kind::Bool, _) => NpyData::Bool(body.iter().take(count).map(|&b| b != 0).collect()),
            (Kind::Int, 1) => NpyData::I8(body.iter().take(count).map(|&b| i8::from_le_bytes([b])).collect()),
            (Kind::Uint, 1) => NpyData::U8(body[..count].to_vec()),
            (Kind::Int, 2) => NpyData::I16(nums!(i16, 2)),
            (Kind::Uint, 2) => NpyData::U16(nums!(u16, 2)),
            (Kind::Int, 4) => NpyData::I32(nums!(i32, 4)),
            (Kind::Uint, 4) => NpyData::U32(nums!(u32, 4)),
            (Kind::Int, 8) => NpyData::I64(nums!(i64, 8)),
            (Kind::Uint, 8) => NpyData::U64(nums!(u64, 8)),
            (Kind::Float, 4) => NpyData::F32(nums!(f32, 4)),
            (Kind::Float, 8) => NpyData::F64(nums!(f64, 8)),
            (Kind::Complex, 8) => {
                let parts: Vec<f32> = body
                    .chunks_exact(4)
                    .take(2 * count)
                    .map(|c| {
                        let a = [c[0], c[1], c[2], c[3]];
                        if self.big { f32::from_be_bytes(a) } else { f32::from_le_bytes(a) }
                    })
                    .collect();
                NpyData::C64(parts.chunks_exact(2).map(|p| [p[0], p[1]]).collect())
            }
            (Kind::Complex, 16) => {
                let parts: Vec<f64> = body
                    .chunks_exact(8)
                    .take(2 * count)
                    .map(|c| {
                        let mut a = [0u8; 8];
                        a.copy_from_slice(c);
                        if self.big { f64::from_be_bytes(a) } else { f64::from_le_bytes(a) }
                    })
                    .collect();
                NpyData::C128(parts.chunks_exact(2).map(|p| [p[0], p[1]]).collect())
            }
            (Kind::Unicode, size) => {
                let width = size / 4;
                let mut values = Vec::with_capacity(count);
                for i in 0..count {
                    let cell = &body[i * size..(i + 1) * size];
                    let mut s = String::new();
                    for c in cell.chunks_exact(4) {
                        let a = [c[0], c[1], c[2], c[3]];
                        let code = if self.big { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) };
                        if code == 0 {
                            continue;
                        }
                        s.push(char::from_u32(code).ok_or_else(|| err("invalid code point in <U array"))?);
                    }

                    values.push(s);
                }
                NpyData::Unicode { width, values }
            }
            (Kind::Bytes, size) => {
                let values = (0..count)
                    .map(|i| {
                        let cell = &body[i * size..(i + 1) * size];
                        let end = cell.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
                        cell[..end].to_vec()
                    })
                    .collect();
                NpyData::Bytes { width: size, values }
            }
            _ => return Err(err("unsupported dtype")),
        })
    }
}

fn fortran_to_c(data: NpyData, shape: &[usize]) -> NpyData {

    let n = shape.len();
    let count = shape.iter().product::<usize>();
    let mut perm = Vec::with_capacity(count);
    let mut idx = vec![0usize; n];
    for _ in 0..count {
        let mut off = 0;
        let mut stride = 1;
        for k in 0..n {
            off += idx[k] * stride;
            stride *= shape[k];
        }
        perm.push(off);
        for k in (0..n).rev() {
            idx[k] += 1;
            if idx[k] < shape[k] {
                break;
            }
            idx[k] = 0;
        }
    }
    macro_rules! remap {
        ($v:expr) => {
            perm.iter().map(|&p| $v[p].clone()).collect()
        };
    }
    match data {
        NpyData::Bool(v) => NpyData::Bool(remap!(v)),
        NpyData::I8(v) => NpyData::I8(remap!(v)),
        NpyData::U8(v) => NpyData::U8(remap!(v)),
        NpyData::I16(v) => NpyData::I16(remap!(v)),
        NpyData::U16(v) => NpyData::U16(remap!(v)),
        NpyData::I32(v) => NpyData::I32(remap!(v)),
        NpyData::U32(v) => NpyData::U32(remap!(v)),
        NpyData::I64(v) => NpyData::I64(remap!(v)),
        NpyData::U64(v) => NpyData::U64(remap!(v)),
        NpyData::F32(v) => NpyData::F32(remap!(v)),
        NpyData::F64(v) => NpyData::F64(remap!(v)),
        NpyData::C64(v) => NpyData::C64(remap!(v)),
        NpyData::C128(v) => NpyData::C128(remap!(v)),
        NpyData::Unicode { width, values } => NpyData::Unicode { width, values: remap!(values) },
        NpyData::Bytes { width, values } => NpyData::Bytes { width, values: remap!(values) },
    }
}

