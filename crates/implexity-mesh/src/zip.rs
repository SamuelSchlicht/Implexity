// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::io::{Read as _, Write as _};

use flate2::Compression;
use flate2::write::DeflateEncoder;

use crate::MeshError;

const DOS_DATE_1980_01_01: u16 = (1 << 5) | 1;

fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

fn u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZipMethod {
    Stored,
    Deflated,
}



pub fn write_zip(entries: &[(String, Vec<u8>, ZipMethod)]) -> Result<Vec<u8>, MeshError> {
    let too_big = || MeshError::invalid("zip entry exceeds 4 GiB");
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, method) in entries {
        let payload = match method {
            ZipMethod::Stored => data.clone(),
            ZipMethod::Deflated => {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(6));
                enc.write_all(data).map_err(|e| MeshError::io("deflate", e))?;
                enc.finish().map_err(|e| MeshError::io("deflate", e))?
            }
        };
        let crc = crc32(data);
        let offset = u32::try_from(out.len()).map_err(|_| too_big())?;
        let method_id: u16 = if *method == ZipMethod::Deflated { 8 } else { 0 };
        let flags: u16 = if name.is_ascii() { 0 } else { 0x800 };
        let version: u16 = if *method == ZipMethod::Deflated { 20 } else { 10 };
        let nlen = u16::try_from(name.len()).map_err(|_| too_big())?;
        let csize = u32::try_from(payload.len()).map_err(|_| too_big())?;
        let usize_ = u32::try_from(data.len()).map_err(|_| too_big())?;

        u32le(&mut out, 0x0403_4b50);
        u16le(&mut out, version);
        u16le(&mut out, flags);
        u16le(&mut out, method_id);
        u16le(&mut out, 0);
        u16le(&mut out, DOS_DATE_1980_01_01);
        u32le(&mut out, crc);
        u32le(&mut out, csize);
        u32le(&mut out, usize_);
        u16le(&mut out, nlen);
        u16le(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&payload);

        u32le(&mut central, 0x0201_4b50);
        u16le(&mut central, (3 << 8) | version);
        u16le(&mut central, version);
        u16le(&mut central, flags);
        u16le(&mut central, method_id);
        u16le(&mut central, 0);
        u16le(&mut central, DOS_DATE_1980_01_01);
        u32le(&mut central, crc);
        u32le(&mut central, csize);
        u32le(&mut central, usize_);
        u16le(&mut central, nlen);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u32le(&mut central, 0o644 << 16);
        u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = u32::try_from(out.len()).map_err(|_| too_big())?;
    let cd_size = u32::try_from(central.len()).map_err(|_| too_big())?;
    let count = u16::try_from(entries.len()).map_err(|_| too_big())?;
    out.extend_from_slice(&central);
    u32le(&mut out, 0x0605_4b50);
    u16le(&mut out, 0);
    u16le(&mut out, 0);
    u16le(&mut out, count);
    u16le(&mut out, count);
    u32le(&mut out, cd_size);
    u32le(&mut out, cd_offset);
    u16le(&mut out, 0);
    Ok(out)
}



pub fn read_zip(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, MeshError> {
    let bad = |m: &str| MeshError::invalid(format!("zip archive: {m}"));
    let rd16 = |p: usize| -> Result<u16, MeshError> {
        bytes.get(p..p + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| bad("truncated"))
    };
    let rd32 = |p: usize| -> Result<u32, MeshError> {
        bytes
            .get(p..p + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .ok_or_else(|| bad("truncated"))
    };
    let eocd = (0..bytes.len().saturating_sub(21)).rev().find(|&p| rd32(p).ok() == Some(0x0605_4b50));
    let eocd = eocd.ok_or_else(|| bad("no end of central directory"))?;
    let count = usize::from(rd16(eocd + 10)?);
    let mut p = rd32(eocd + 16)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if rd32(p)? != 0x0201_4b50 {
            return Err(bad("bad central directory"));
        }
        let method = rd16(p + 10)?;
        let crc = rd32(p + 16)?;
        let csize = rd32(p + 20)? as usize;
        let nlen = usize::from(rd16(p + 28)?);
        let elen = usize::from(rd16(p + 30)?);
        let clen = usize::from(rd16(p + 32)?);
        let local = rd32(p + 42)? as usize;
        let name = String::from_utf8(bytes.get(p + 46..p + 46 + nlen).ok_or_else(|| bad("name"))?.to_vec())
            .map_err(|_| bad("name is not UTF-8"))?;
        let lnlen = usize::from(rd16(local + 26)?);
        let lelen = usize::from(rd16(local + 28)?);
        let start = local + 30 + lnlen + lelen;
        let raw = bytes.get(start..start + csize).ok_or_else(|| bad("entry data"))?;
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut v = Vec::new();
                flate2::read::DeflateDecoder::new(raw)
                    .read_to_end(&mut v)
                    .map_err(|e| bad(&e.to_string()))?;
                v
            }
            _ => return Err(bad("unsupported compression method")),
        };
        if crc32(&data) != crc {
            return Err(bad("CRC mismatch"));
        }
        out.push((name, data));
        p += 46 + nlen + elen + clen;
    }
    Ok(out)
}

