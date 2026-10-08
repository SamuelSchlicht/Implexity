// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use implexity_io::fsguard;

use implexity_core::json::{DumpOptions, dumps};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[must_use]
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0_u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36_u8; BLOCK];
    let mut opad = [0x5c_u8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(message).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

#[must_use]
pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    hex::encode(hmac_sha256(key, message))
}

#[must_use]
pub fn compare_digest(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}


pub fn token_bytes(n: usize) -> std::io::Result<Vec<u8>> {
    implexity_io::atomic::os_random_bytes(n).map_err(|e| std::io::Error::other(e.to_string()))
}


pub fn token_hex(n: usize) -> std::io::Result<String> {
    Ok(hex::encode(token_bytes(n)?))
}

#[must_use]
pub fn monotonic_ns() -> i64 {
    fsguard::monotonic_ns()
}

#[must_use]
pub fn monotonic() -> f64 {
    monotonic_ns() as f64 / 1e9
}

#[must_use]
pub fn epoch_seconds() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

#[must_use]
pub fn perf_counter_ns() -> i64 {
    monotonic_ns()
}

#[must_use]
pub fn perf_counter() -> f64 {
    monotonic()
}

#[must_use]
pub fn canonical_text(value: &Value) -> String {
    dumps(value, &DumpOptions::canonical())
}

#[must_use]
pub fn compact_text(value: &Value) -> String {
    dumps(value, &DumpOptions::compact())
}

#[must_use]
pub fn indented_sorted_text(value: &Value) -> String {
    dumps(value, &DumpOptions::indented(2).sorted(true))
}

#[must_use]
pub fn indented_text(value: &Value) -> String {
    dumps(value, &DumpOptions::indented(2))
}


pub fn open_nofollow(path: &Path) -> std::io::Result<File> {
    fsguard::open_nofollow(path)
}


pub fn create_exclusive(path: &Path, mode: u32) -> std::io::Result<File> {
    fsguard::create_new_nofollow(path, mode)
}


pub fn write_all_sync(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}


pub fn fsync_dir(path: &Path) -> std::io::Result<()> {
    fsguard::fsync_dir(path)
}


pub fn read_limited(file: &mut File, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    file.take(limit + 1).read_to_end(&mut out)?;
    Ok(out)
}


pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[must_use]
pub fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

