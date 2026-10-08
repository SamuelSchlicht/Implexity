// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::error::{GResult, GeometryError};
use crate::value::{DType, NdArray};

pub const ARRAY_DTYPES: [(&str, usize); 8] = [
    ("bool", 1),
    ("float32", 4),
    ("float64", 8),
    ("int16", 2),
    ("int32", 4),
    ("int64", 8),
    ("int8", 1),
    ("uint8", 1),
];

#[must_use]
pub fn itemsize(name: &str) -> Option<usize> {
    ARRAY_DTYPES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

#[must_use]
pub fn dtype_list() -> String {
    ARRAY_DTYPES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
}

#[must_use]
pub fn sha256_hex(raw: &[u8]) -> String {
    hex::encode(Sha256::digest(raw))
}

#[must_use]
pub fn encode_array(a: &NdArray) -> (serde_json::Map<String, serde_json::Value>, Vec<u8>) {
    let raw = a.to_le_bytes();
    let mut e = serde_json::Map::new();
    e.insert("dtype".into(), a.dtype().name().into());
    e.insert("shape".into(), serde_json::json!(a.shape()));
    e.insert("sha256".into(), sha256_hex(&raw).into());
    (e, raw)
}

#[must_use]
pub fn inline_entry(entry: &serde_json::Map<String, serde_json::Value>, raw: &[u8]) -> serde_json::Value {
    let mut e = entry.clone();
    e.insert("b64".into(), base64::engine::general_purpose::STANDARD.encode(raw).into());
    serde_json::Value::Object(e)
}

#[must_use]
pub fn sidecar_entry(
    entry: &serde_json::Map<String, serde_json::Value>,
    filename: &str,
) -> serde_json::Value {
    let mut e = entry.clone();
    e.insert("file".into(), filename.into());
    serde_json::Value::Object(e)
}


pub fn decode_array(entry: &serde_json::Value, raw: &[u8]) -> GResult<NdArray> {
    let dt = entry.get("dtype").and_then(serde_json::Value::as_str).and_then(DType::from_name);
    let Some(dt) = dt else {
        return Err(GeometryError::Value(format!(
            "data type {} not understood",
            entry.get("dtype").map_or("None".to_string(), ToString::to_string)
        )));
    };
    let shape: Vec<usize> = entry
        .get("shape")
        .and_then(serde_json::Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_u64().and_then(|u| usize::try_from(u).ok())).collect())
        .unwrap_or_default();
    NdArray::from_le_bytes(dt, shape.clone(), raw).ok_or_else(|| {
        GeometryError::Value(format!(
            "cannot reshape array of size {} into shape {}",
            raw.len() / dt.itemsize(),
            crate::pyfmt::shape_str(&shape)
        ))
    })
}


pub fn b64decode_strict(s: &str) -> Result<Vec<u8>, String> {
    if s.bytes().any(|b| !(b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')) {
        return Err("Only base64 data is allowed".into());
    }
    base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| match e {
        base64::DecodeError::InvalidPadding | base64::DecodeError::InvalidLength(_) => {
            "Incorrect padding".to_string()
        }
        other => other.to_string(),
    })
}

pub fn npy_payload(blob: &[u8], where_: &str, problems: &mut Vec<String>) -> Option<Vec<u8>> {
    if !blob.starts_with(b"\x93NUMPY") {
        problems.push(format!("{where_}: the sidecar is not a .npy file"));
        return None;
    }
    let major = blob.get(6).copied().unwrap_or(0);
    let (hlen, start, off) = match major {
        1 => {
            let h = usize::from(u16::from_le_bytes([
                blob.get(8).copied().unwrap_or(0),
                blob.get(9).copied().unwrap_or(0),
            ]));
            (h, 10 + h, 10)
        }
        2 | 3 => {
            let b = |i: usize| blob.get(i).copied().unwrap_or(0);
            let h = u32::from_le_bytes([b(8), b(9), b(10), b(11)]) as usize;
            (h, 12 + h, 12)
        }
        m => {
            problems.push(format!("{where_}: .npy format version {m} is not supported"));
            return None;
        }
    };
    let header: String = blob.get(off..off + hlen).unwrap_or(&[]).iter().map(|b| char::from(*b)).collect();
    if header.replace('"', "'").contains("'fortran_order': True") {
        problems.push(format!(
            "{where_}: the sidecar is Fortran-ordered; write it with numpy.save from a C-contiguous array"
        ));
        return None;
    }
    Some(blob.get(start..).unwrap_or(&[]).to_vec())
}

#[must_use]
pub fn npy_bytes(a: &NdArray) -> Vec<u8> {
    let shape = match a.shape().len() {
        0 => "()".to_string(),
        1 => format!("({},)", a.shape()[0]),
        _ => format!("({})", a.shape().iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    };
    let mut header =
        format!("{{'descr': '{}', 'fortran_order': False, 'shape': {}, }}", a.dtype().npy_descr(), shape);

    if let Some(first) = a.shape().first() {
        header.push_str(&" ".repeat(21usize.saturating_sub(first.to_string().len())));
    }
    let base = 10 + header.len() + 1;
    let pad = 64 - base % 64;
    header.push_str(&" ".repeat(pad));
    header.push('\n');
    let mut out = b"\x93NUMPY\x01\x00".to_vec();
    out.extend_from_slice(&u16::try_from(header.len()).unwrap_or(u16::MAX).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&a.to_le_bytes());
    out
}
