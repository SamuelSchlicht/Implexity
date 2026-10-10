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

