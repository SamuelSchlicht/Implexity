// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use implexity_io::fsguard::{self, Dir, FileKind, FileStat};
use implexity_io::npy::{NpyArray, NpyData};
use serde_json::{Map, Value};

use crate::private::{canonical_text, epoch_seconds, indented_sorted_text, sha256_hex, token_hex};

pub const ARTIFACT_SCHEMA: &str = "implexity-result-artifact/1";
pub const DEFAULT_MAX_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_UNCOMPRESSED_BYTES: u64 = 768 * 1024 * 1024;
pub const DEFAULT_MAX_FIELD_BYTES: u64 = 384 * 1024 * 1024;
pub const DEFAULT_MAX_FIELDS: usize = 4096;
pub const DEFAULT_MAX_COMPRESSION_RATIO: f64 = 20_000.0;
pub const DEFAULT_MAX_NPY_HEADER_BYTES: u64 = 64 * 1024;
pub const DEFAULT_MAX_CENTRAL_DIRECTORY_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_MAX_FIELD_NAME_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
}

impl ArtifactError {
    #[must_use]
    pub fn python_class(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "ArtifactError",
            Self::NotFound(_) => "FileNotFoundError",
        }
    }
}

type AResult<T> = Result<T, ArtifactError>;

fn fail<T>(message: impl Into<String>) -> AResult<T> {
    Err(ArtifactError::Invalid(message.into()))
}

fn repr(name: &str) -> String {
    implexity_core::py_repr::repr_str(name)
}

#[must_use]
pub fn dtype_name(data: &NpyData) -> String {
    match data {
        NpyData::Bool(_) => "bool".into(),
        NpyData::I8(_) => "int8".into(),
        NpyData::U8(_) => "uint8".into(),
        NpyData::I16(_) => "int16".into(),
        NpyData::U16(_) => "uint16".into(),
        NpyData::I32(_) => "int32".into(),
        NpyData::U32(_) => "uint32".into(),
        NpyData::I64(_) => "int64".into(),
        NpyData::U64(_) => "uint64".into(),
        NpyData::F32(_) => "float32".into(),
        NpyData::F64(_) => "float64".into(),
        NpyData::C64(_) => "complex64".into(),
        NpyData::C128(_) => "complex128".into(),
        NpyData::Unicode { width, .. } => format!("<U{width}"),
        NpyData::Bytes { width, .. } => format!("|S{width}"),
    }
}

fn is_numeric(data: &NpyData) -> bool {
    !matches!(data, NpyData::Bool(_) | NpyData::Unicode { .. } | NpyData::Bytes { .. })
}

#[must_use]
pub fn finite_count(data: &NpyData) -> Option<usize> {
    Some(match data {
        NpyData::F64(v) => v.iter().filter(|x| x.is_finite()).count(),
        NpyData::F32(v) => v.iter().filter(|x| x.is_finite()).count(),
        NpyData::C64(v) => v.iter().filter(|x| x[0].is_finite() && x[1].is_finite()).count(),
        NpyData::C128(v) => v.iter().filter(|x| x[0].is_finite() && x[1].is_finite()).count(),
        NpyData::Bool(_) | NpyData::Unicode { .. } | NpyData::Bytes { .. } => return None,
        other => other.len(),
    })
}


pub fn canonical_f8_sha256(array: &NpyArray) -> AResult<String> {
    if !is_numeric(&array.data) || matches!(array.data, NpyData::C64(_) | NpyData::C128(_)) {
        return fail("canonical design hashing requires a real numeric array");
    }
    let values = array.to_f64().ok_or_else(|| {
        ArtifactError::Invalid("canonical design hashing requires a real numeric array".into())
    })?;
    Ok(crate::epoch_capture::array_sha256(&values))
}

fn require_artifact_id(artifact_id: &str) -> AResult<&str> {
    let ok = artifact_id.strip_prefix("result-").is_some_and(|rest| {
        rest.len() == 32 && rest.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    });
    if ok { Ok(artifact_id) } else { fail("invalid artifact id") }
}


pub fn require_field_name(name: &str, max_bytes: usize) -> AResult<&str> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
    {
        return fail(format!("unsafe field name {}", repr(name)));
    }
    if name.len() > max_bytes {
        return fail(format!("field name {} is too long", repr(name)));
    }
    Ok(name)
}

fn open_directory(path: &Path) -> AResult<Dir> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = Dir::open(path)
        .map_err(|_| ArtifactError::Invalid(format!("artifact directory {} is unsafe", repr(&name))))?;
    let info = dir
        .stat()
        .map_err(|_| ArtifactError::Invalid(format!("artifact directory {} is unsafe", repr(&name))))?;
    if !info.is_dir() {
        return fail(format!("artifact directory {} is unsafe", repr(&name)));
    }
    Ok(dir)
}

fn openat_directory(parent: &Dir, name: &str) -> AResult<Dir> {
    let unsafe_dir = || ArtifactError::Invalid(format!("artifact directory {} is unsafe", repr(name)));
    let before = parent.stat_at(name).map_err(|_| unsafe_dir())?;
    let dir = parent.open_dir_at(name).map_err(|_| unsafe_dir())?;
    let info = dir.stat().map_err(|_| unsafe_dir())?;
    if !info.is_dir() || !before.same_object(&info) {
        return fail(format!("artifact directory {} changed while opening", repr(name)));
    }
    Ok(dir)
}

fn openat_regular(
    parent: &Dir,
    name: &str,
    min_bytes: u64,
    max_bytes: Option<u64>,
) -> AResult<(File, FileStat)> {
    let unsafe_file = || ArtifactError::Invalid(format!("artifact file {} is unsafe", repr(name)));
    let before = parent.stat_at(name).map_err(|_| unsafe_file())?;
    let file = parent.open_file_at(name).map_err(|_| unsafe_file())?;
    let info = fsguard::stat_file(&file).map_err(|_| unsafe_file())?;
    let size = info.size;
    if !info.is_file()
        || info.nlink != 1
        || !before.same_object(&info)
        || size < min_bytes
        || max_bytes.is_some_and(|m| size > m)
    {
        return fail(format!("artifact file {} is unsafe or too large", repr(name)));
    }
    Ok((file, info))
}

fn read_all(file: &File, max_bytes: u64) -> AResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let n =
            fsguard::read_at(file, &mut buffer, offset).map_err(|e| ArtifactError::Invalid(e.to_string()))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..n]);
        offset += n as u64;
        if offset > max_bytes {
            return fail("artifact file is too large");
        }
    }
    Ok(out)
}

fn sha256_fd(file: &File) -> AResult<String> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let n =
            fsguard::read_at(file, &mut buffer, offset).map_err(|e| ArtifactError::Invalid(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        offset += n as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

fn assert_open_file_stable(
    parent: &Dir,
    name: &str,
    file: &File,
    original: &FileStat,
    expected_sha256: Option<&str>,
) -> AResult<FileStat> {
    let changed =
        || ArtifactError::Invalid(format!("artifact file {} changed during validation", repr(name)));
    let current = fsguard::stat_file(file).map_err(|_| changed())?;
    let bound = parent.stat_at(name).map_err(|_| changed())?;
    if current.data_identity() != original.data_identity()
        || !bound.same_object(&current)
        || !bound.is_file()
        || bound.nlink != 1
    {
        return Err(changed());
    }
    if current.ctime == original.ctime {
        return Ok(current);
    }
    match expected_sha256 {
        Some(d) if crate::private::is_sha256(d) && sha256_fd(file)? == d => {}
        _ => return Err(changed()),
    }
    let after = fsguard::stat_file(file).map_err(|_| changed())?;
    let rebound = parent.stat_at(name).map_err(|_| changed())?;
    if after.data_identity() != current.data_identity()
        || !rebound.same_object(&after)
        || !rebound.is_file()
        || rebound.nlink != 1
    {
        return Err(changed());
    }
    Ok(after)
}

fn decode_json_object(raw: &[u8]) -> AResult<Map<String, Value>> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| ArtifactError::Invalid("artifact manifest is invalid JSON".into()))?;
    let value = implexity_core::json::parse_with(
        text,
        implexity_core::json::ParseOptions { reject_duplicate_keys: false },
    )
    .map_err(|_| ArtifactError::Invalid("artifact manifest is invalid JSON".into()))?;
    match value {
        Value::Object(m) => Ok(m),
        _ => fail("artifact manifest is not an object"),
    }
}


pub fn read_json_object(path: &Path, max_bytes: u64) -> AResult<Map<String, Value>> {
    let parent_path = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = open_directory(parent_path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (file, original) = openat_regular(&parent, &name, 2, Some(max_bytes))?;
    let raw = read_all(&file, max_bytes)?;
    assert_open_file_stable(&parent, &name, &file, &original, Some(&sha256_hex(&raw)))?;
    decode_json_object(&raw)
}


pub fn scan_tree_bounded(root: &Path, max_entries: usize, max_depth: usize) -> AResult<BTreeSet<String>> {
    if max_entries == 0 || max_depth == 0 {
        return fail("artifact tree limits must be positive integers");
    }
    let root_fd = open_directory(root)?;
    let root_info =
        root_fd.stat().map_err(|_| ArtifactError::Invalid("artifact tree root changed".into()))?;
    let mut pending: Vec<(Dir, Vec<String>, usize)> = vec![(root_fd, Vec::new(), 0)];
    let mut paths: BTreeSet<String> = std::iter::once(".".to_string()).collect();
    let mut count = 0;
    let changed = |m: &str| ArtifactError::Invalid(m.into());
    while let Some((fd, prefix, depth)) = pending.pop() {
        let before = fd.stat().map_err(|_| changed("artifact tree cannot be inspected"))?;
        let entries = fd.entries().map_err(|_| changed("artifact tree cannot be inspected"))?;
        for name in entries {
            count += 1;
            if count > max_entries {
                return fail("artifact tree entry budget exceeded");
            }
            if name.is_empty() || name.contains('/') || name.contains('\0') {
                return fail("artifact tree contains an unsafe name");
            }
            let mut parts = prefix.clone();
            parts.push(name.clone());
            let relative = parts.join("/");
            let first = fd.stat_at(name.as_str()).map_err(|_| changed("artifact tree entry changed"))?;
            match first.kind {
                FileKind::Symlink => return fail("artifact tree contains a symlink"),
                FileKind::Directory => {
                    if depth >= max_depth {
                        return fail("artifact tree nesting is too deep");
                    }
                    let child = openat_directory(&fd, &name)?;
                    pending.push((child, parts, depth + 1));
                }
                FileKind::Regular => {
                    let (_child, info) = openat_regular(&fd, &name, 0, None)?;
                    if !first.same_object(&info) {
                        return fail("artifact tree entry changed");
                    }
                }
                FileKind::Other => return fail("artifact tree contains an unsafe entry"),
            }
            paths.insert(relative);
        }
        let after = fd.stat().map_err(|_| changed("artifact tree changed during inspection"))?;
        if after.data_identity() != before.data_identity() || after.ctime != before.ctime {
            return fail("artifact tree changed during inspection");
        }
    }
    let rebound = fsguard::stat_nofollow(root).map_err(|_| changed("artifact tree root changed"))?;
    if !rebound.same_object(&root_info) || !rebound.is_dir() {
        return fail("artifact tree root changed");
    }
    Ok(paths)
}

struct Bundle {
    root_fd: Dir,
    folder_fd: Dir,
    folder_info: FileStat,
    folder_entries: BTreeSet<String>,
    manifest_file: File,
    manifest_info: FileStat,
    manifest_sha256: String,
    manifest: Map<String, Value>,
    payload_file: File,
    payload_info: FileStat,
    payload_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InspectedField {
    pub shape: Vec<usize>,
    pub dtype: String,
    pub descr: String,
    pub size: u64,
    pub nbytes: u64,
    pub archive_bytes: u64,
    pub compressed_bytes: u64,
    pub header_bytes: u64,
    pub fortran_order: bool,
    pub zip_filename: String,
}

impl InspectedField {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("shape".into(), Value::Array(self.shape.iter().map(|d| Value::from(*d)).collect()));
        m.insert("dtype".into(), Value::String(self.dtype.clone()));
        m.insert("size".into(), Value::from(self.size));
        m.insert("nbytes".into(), Value::from(self.nbytes));
        m.insert("archive_bytes".into(), Value::from(self.archive_bytes));
        m.insert("compressed_bytes".into(), Value::from(self.compressed_bytes));
        m.insert("header_bytes".into(), Value::from(self.header_bytes));
        m.insert("fortran_order".into(), Value::Bool(self.fortran_order));
        m.insert("zip_filename".into(), Value::String(self.zip_filename.clone()));
        Value::Object(m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub max_manifest_bytes: u64,
    pub max_payload_bytes: u64,
    pub max_uncompressed_bytes: u64,
    pub max_field_bytes: u64,
    pub max_fields: usize,
    pub max_compression_ratio: f64,
    pub max_header_bytes: u64,
    pub max_central_directory_bytes: u64,
    pub max_field_name_bytes: usize,
    pub require_numeric: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 64 * 1024 * 1024,
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            max_uncompressed_bytes: DEFAULT_MAX_UNCOMPRESSED_BYTES,
            max_field_bytes: DEFAULT_MAX_FIELD_BYTES,
            max_fields: DEFAULT_MAX_FIELDS,
            max_compression_ratio: DEFAULT_MAX_COMPRESSION_RATIO,
            max_header_bytes: DEFAULT_MAX_NPY_HEADER_BYTES,
            max_central_directory_bytes: DEFAULT_MAX_CENTRAL_DIRECTORY_BYTES,
            max_field_name_bytes: DEFAULT_MAX_FIELD_NAME_BYTES,
            require_numeric: true,
        }
    }
}

fn zip_directory_contract(
    payload: &[u8],
    max_fields: usize,
    max_central_directory_bytes: u64,
) -> AResult<()> {
    let payload_bytes = payload.len();
    let tail_bytes = payload_bytes.min(65_557);
    let tail = &payload[payload_bytes - tail_bytes..];
    let Some(marker) = tail.windows(4).rposition(|w| w == b"PK\x05\x06") else {
        return fail("artifact NPZ has no bounded ZIP directory");
    };
    if tail.len() - marker < 22 {
        return fail("artifact NPZ has no bounded ZIP directory");
    }
    let eocd_offset = (payload_bytes - tail.len() + marker) as u64;
    let u16_at = |o: usize| u16::from_le_bytes([tail[marker + o], tail[marker + o + 1]]);
    let u32_at = |o: usize| {
        u32::from_le_bytes([
            tail[marker + o],
            tail[marker + o + 1],
            tail[marker + o + 2],
            tail[marker + o + 3],
        ])
    };
    let (disk, directory_disk, disk_entries, total_entries) = (u16_at(4), u16_at(6), u16_at(8), u16_at(10));
    let (directory_bytes, directory_offset, comment_bytes) = (u32_at(12), u32_at(16), u16_at(20));
    if marker + 22 + usize::from(comment_bytes) != tail.len()
        || disk != 0
        || directory_disk != 0
        || disk_entries != total_entries
        || total_entries == 0
        || total_entries == 0xFFFF
        || directory_bytes == 0xFFFF_FFFF
        || directory_offset == 0xFFFF_FFFF
        || usize::from(total_entries) > max_fields
        || u64::from(directory_bytes) > max_central_directory_bytes
        || u64::from(directory_offset) + u64::from(directory_bytes) != eocd_offset
    {
        return fail("artifact NPZ ZIP directory exceeds its closed bounds");
    }
    if payload.get(..4) != Some(b"PK\x03\x04".as_slice()) {
        return fail("artifact NPZ contains an unsafe prepended payload");
    }
    Ok(())
}

fn npy_header(member: &[u8], max_header_bytes: u64) -> Option<(Vec<usize>, bool, String, usize)> {
    if member.len() < 10 || &member[..6] != b"\x93NUMPY" {
        return None;
    }
    let (length, start) = match (member[6], member[7]) {
        (1, 0) => (usize::from(u16::from_le_bytes([member[8], member[9]])), 10),
        (2, 0) => {
            let b = member.get(8..12)?;
            (usize::try_from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok()?, 12)
        }
        _ => return None,
    };
    if length as u64 > max_header_bytes {
        return None;
    }
    let text = std::str::from_utf8(member.get(start..start + length)?).ok()?;
    let header = implexity_io::npy::parse_header(text).ok()?;
    Some((header.shape, header.fortran_order, header.descr, start + length))
}

fn descr_facts(descr: &str) -> Option<(u64, String, bool)> {
    Some(match descr {
        "|b1" => (1, "bool".into(), false),
        "|i1" => (1, "int8".into(), true),
        "|u1" => (1, "uint8".into(), true),
        "<i2" => (2, "int16".into(), true),
        "<u2" => (2, "uint16".into(), true),
        "<i4" => (4, "int32".into(), true),
        "<u4" => (4, "uint32".into(), true),
        "<i8" => (8, "int64".into(), true),
        "<u8" => (8, "uint64".into(), true),
        "<f4" => (4, "float32".into(), true),
        "<f8" => (8, "float64".into(), true),
        "<c8" => (8, "complex64".into(), true),
        "<c16" => (16, "complex128".into(), true),
        other => {
            if let Some(width) = other.strip_prefix("|S") {
                let width: u64 = width.parse().ok()?;
                return Some((width, format!("|S{width}"), false));
            }
            let width: u64 = other.strip_prefix("<U")?.parse().ok()?;
            (width * 4, format!("<U{width}"), false)
        }
    })
}

fn safe_array_extent(shape: &[usize], itemsize: u64, max_bytes: u64) -> AResult<(u64, u64)> {
    if itemsize == 0 {
        return fail("artifact array dtype has invalid extent");
    }
    let mut size = 1_u64;
    for value in shape {
        let value = *value as u64;
        if value == 0 {
            return Ok((0, 0));
        }
        if size > max_bytes / itemsize / value {
            return fail("artifact array allocation exceeds its byte limit");
        }
        size *= value;
    }
    Ok((size, size * itemsize))
}

struct Snapshot {
    manifest: Map<String, Value>,
    inspected: BTreeMap<String, InspectedField>,
    payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ResultArtifactStore {
    pub root: PathBuf,
}

impl ResultArtifactStore {

    pub fn new(root: &Path) -> AResult<Self> {
        std::fs::create_dir_all(root)
            .map_err(|_| ArtifactError::Invalid("artifact store root is unsafe".into()))?;
        let meta = std::fs::symlink_metadata(root)
            .map_err(|_| ArtifactError::Invalid("artifact store root is unsafe".into()))?;
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            return fail("artifact store root is unsafe");
        }
        Ok(Self { root: root.to_path_buf() })
    }


    pub fn create(
        &self,
        fields: &BTreeMap<String, NpyArray>,
        identities: &Map<String, Value>,
        metadata: &Map<String, Value>,
    ) -> AResult<Map<String, Value>> {
        if fields.is_empty() {
            return fail("at least one result field is required");
        }
        let mut field_meta = Map::new();
        for (name, array) in fields {
            require_field_name(name, DEFAULT_MAX_FIELD_NAME_BYTES)?;
            let mut m = Map::new();
            m.insert("shape".into(), Value::Array(array.shape.iter().map(|d| Value::from(*d)).collect()));
            m.insert("dtype".into(), Value::String(dtype_name(&array.data)));
            m.insert("size".into(), Value::from(array.data.len()));
            m.insert("finite".into(), finite_count(&array.data).map_or(Value::Null, Value::from));
            field_meta.insert(name.clone(), Value::Object(m));
        }
        let io = |e: std::io::Error| ArtifactError::Invalid(e.to_string());
        let payload = implexity_io::npz::save_deterministic(fields)
            .map_err(|e| ArtifactError::Invalid(e.to_string()))?;
        let payload_sha = sha256_hex(&payload);
        let mut identity_doc = Map::new();
        identity_doc.insert("schema".into(), Value::String(ARTIFACT_SCHEMA.into()));
        identity_doc.insert("identities".into(), Value::Object(identities.clone()));
        identity_doc.insert("fields".into(), Value::Object(field_meta));
        identity_doc.insert("metadata".into(), Value::Object(metadata.clone()));
        identity_doc.insert("payload_sha256".into(), Value::String(payload_sha.clone()));
        let canonical = canonical_text(&Value::Object(identity_doc.clone()));
        let Ok(Value::Object(identity_doc)) = serde_json::from_str::<Value>(&canonical) else {
            return fail("artifact manifest is not finite JSON");
        };
        let artifact_id = format!("result-{}", &sha256_hex(canonical.as_bytes())[..32]);
        let folder = self.root.join(&artifact_id);
        let mut manifest = identity_doc;
        manifest.insert("artifact_id".into(), Value::String(artifact_id.clone()));
        manifest.insert("created_unix_s".into(), Value::from(epoch_seconds()));
        let mut payload_meta = Map::new();
        payload_meta.insert("format".into(), Value::String("npz".into()));
        payload_meta.insert("filename".into(), Value::String("fields.npz".into()));
        payload_meta.insert("bytes".into(), Value::from(payload.len()));
        payload_meta.insert("sha256".into(), Value::String(payload_sha));
        manifest.insert("payload".into(), Value::Object(payload_meta));
        let keys = ["schema", "artifact_id", "identities", "fields", "metadata", "payload_sha256", "payload"];
        let same = |previous: &Map<String, Value>| keys.iter().all(|k| previous.get(*k) == manifest.get(*k));
        if folder.exists() {
            let previous = self.get(&artifact_id)?;
            if !same(&previous) {
                return fail(format!("existing artifact {artifact_id} differs"));
            }
            return Ok(previous);
        }
        let temporary = self.root.join(format!(".publish-{}", token_hex(8).map_err(io)?));
        std::fs::create_dir(&temporary).map_err(io)?;
        let publish = (|| -> AResult<Map<String, Value>> {
            fsguard::set_owner_only(&temporary, true).map_err(io)?;
            let mut npz =
                crate::private::create_exclusive(&temporary.join("fields.npz"), 0o600).map_err(io)?;
            crate::private::write_all_sync(&mut npz, &payload).map_err(io)?;
            drop(npz);
            let encoded = indented_sorted_text(&Value::Object(manifest.clone()));
            let mut file =
                crate::private::create_exclusive(&temporary.join("manifest.json"), 0o600).map_err(io)?;
            crate::private::write_all_sync(&mut file, encoded.as_bytes()).map_err(io)?;
            drop(file);
            crate::private::fsync_dir(&temporary).map_err(io)?;
            if std::fs::rename(&temporary, &folder).is_err() {
                if !folder.exists() {
                    return fail(format!("artifact {artifact_id} could not be published"));
                }
                let previous = self.get(&artifact_id)?;
                if !same(&previous) {
                    return fail(format!("racing artifact {artifact_id} differs"));
                }
                return Ok(previous);
            }
            crate::private::fsync_dir(&self.root).map_err(io)?;
            self.get(&artifact_id)
        })();
        if temporary.exists() {
            let _ = std::fs::remove_dir_all(&temporary);
        }
        publish
    }

    pub fn create_f64_streamed(
        &self,
        fields: &BTreeMap<String, ndarray::ArrayD<f64>>,
        identities: &Map<String, Value>,
        metadata: &Map<String, Value>,
        limits: &Limits,
    ) -> AResult<Map<String, Value>> {
        if fields.is_empty() || fields.len() > limits.max_fields { return fail("nonempty bounded result fields required"); }
        let mut field_meta = Map::new();
        let mut total = 0u64;
        for (name,array) in fields {
            require_field_name(name, limits.max_field_name_bytes)?;
            let bytes=(array.len() as u64).checked_mul(8).ok_or_else(||ArtifactError::Invalid("field byte length overflow".into()))?;
            total=total.checked_add(bytes).ok_or_else(||ArtifactError::Invalid("artifact byte length overflow".into()))?;
            if bytes > limits.max_field_bytes || total > limits.max_uncompressed_bytes || array.is_empty() || array.iter().any(|v|!v.is_finite()) {
                return fail(format!("result field {name} exceeds the configured finite-field budget"));
            }
            field_meta.insert(name.clone(),serde_json::json!({"shape":array.shape(),"dtype":"float64","size":array.len(),"finite":array.len()}));
        }
        let io = |e: std::io::Error| ArtifactError::Invalid(e.to_string());
        let temporary = self.root.join(format!(".publish-{}",token_hex(8).map_err(io)?));
        std::fs::create_dir(&temporary).map_err(io)?;
        let publish=(|| -> AResult<Map<String,Value>> {
            fsguard::set_owner_only(&temporary,true).map_err(io)?;
            let file=crate::private::create_exclusive(&temporary.join("fields.npz"),0o600).map_err(io)?;
            let file=implexity_io::npz::save_f64_streamed(file,fields).map_err(|e|ArtifactError::Invalid(e.to_string()))?;
            file.sync_all().map_err(io)?;drop(file);
            let file=fsguard::open_nofollow(&temporary.join("fields.npz")).map_err(io)?;
            let size=file.metadata().map_err(io)?.len();
            if size > limits.max_payload_bytes {return fail(format!("streamed epoch artifact needs {size} bytes; payload budget is {}",limits.max_payload_bytes));}
            let payload_sha=sha256_fd(&file)?;drop(file);
            let mut identity=Map::new();
            identity.insert("schema".into(),Value::String(ARTIFACT_SCHEMA.into()));
            identity.insert("identities".into(),Value::Object(identities.clone()));
            identity.insert("fields".into(),Value::Object(field_meta));
            identity.insert("metadata".into(),Value::Object(metadata.clone()));
            identity.insert("payload_sha256".into(),Value::String(payload_sha.clone()));
            let canonical=canonical_text(&Value::Object(identity.clone()));
            let Ok(Value::Object(mut manifest))=serde_json::from_str::<Value>(&canonical) else {return fail("artifact manifest is not finite JSON");};
            let artifact_id=format!("result-{}",&sha256_hex(canonical.as_bytes())[..32]);
            manifest.insert("artifact_id".into(),Value::String(artifact_id.clone()));
            manifest.insert("created_unix_s".into(),Value::from(epoch_seconds()));
            manifest.insert("payload".into(),serde_json::json!({"format":"npz","filename":"fields.npz","bytes":size,"sha256":payload_sha}));
            let encoded=indented_sorted_text(&Value::Object(manifest.clone()));
            let manifest_limit=limits.max_manifest_bytes;
            if encoded.len() as u64 > manifest_limit { return fail("streamed artifact manifest exceeds its configured byte budget"); }
            let file=crate::private::create_exclusive(&temporary.join("manifest.json"),0o600).map_err(io)?;
            let mut file=file;crate::private::write_all_sync(&mut file,encoded.as_bytes()).map_err(io)?;drop(file);
            crate::private::fsync_dir(&temporary).map_err(io)?;
            let folder=self.root.join(&artifact_id);
            let keys=["schema","artifact_id","identities","fields","metadata","payload_sha256","payload"];
            let same=|previous:&Map<String,Value>|keys.iter().all(|k|previous.get(*k)==manifest.get(*k));
            if folder.exists() {
                let previous=self.get_with_budget(&artifact_id,manifest_limit,limits.max_payload_bytes)?;
                if !same(&previous){return fail("existing streamed artifact differs");}return Ok(previous);
            }
            if std::fs::rename(&temporary,&folder).is_err() {
                if !folder.exists(){return fail("streamed artifact could not be published");}
                let previous=self.get_with_budget(&artifact_id,manifest_limit,limits.max_payload_bytes)?;
                if !same(&previous){return fail("racing streamed artifact differs");}return Ok(previous);
            }
            crate::private::fsync_dir(&self.root).map_err(io)?;
            self.get_with_budget(&artifact_id,manifest_limit,limits.max_payload_bytes)
        })();
        if temporary.exists(){let _=std::fs::remove_dir_all(&temporary);}
        publish
    }

    pub fn get_with_budget(&self, artifact_id: &str, manifest_bytes: u64, payload_bytes: u64) -> AResult<Map<String,Value>> {
        let mut bundle=self.open_bundle(artifact_id,manifest_bytes,Some(payload_bytes))?;
        let actual=sha256_fd(&bundle.payload_file)?;
        Self::validate_manifest_identity(artifact_id,&bundle.manifest,bundle.payload_info.size,&actual,false)?;
        Self::assert_bundle_stable(artifact_id,&mut bundle)?;
        Ok(bundle.manifest.clone())
    }

    fn open_bundle(
        &self,
        artifact_id: &str,
        max_manifest_bytes: u64,
        max_payload_bytes: Option<u64>,
    ) -> AResult<Bundle> {
        require_artifact_id(artifact_id)?;
        let root_fd = open_directory(&self.root)?;
        match root_fd.stat_at(artifact_id) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ArtifactError::NotFound(artifact_id.into()));
            }
            Err(_) => return fail(format!("artifact directory {} is unsafe", repr(artifact_id))),
        }
        let folder_fd = openat_directory(&root_fd, artifact_id)?;
        let changed =
            || ArtifactError::Invalid(format!("artifact {artifact_id} directory changed during validation"));
        let folder_info = folder_fd.stat().map_err(|_| changed())?;
        let folder_entries = folder_fd.entries().map_err(|_| changed())?;
        let after = folder_fd.stat().map_err(|_| changed())?;
        if after.data_identity() != folder_info.data_identity() {
            return Err(changed());
        }
        let (manifest_file, manifest_info) =
            openat_regular(&folder_fd, "manifest.json", 2, Some(max_manifest_bytes))?;
        let manifest_raw = read_all(&manifest_file, max_manifest_bytes)?;
        let manifest_sha256 = sha256_hex(&manifest_raw);
        let manifest = decode_json_object(&manifest_raw)?;
        let payload_ok = manifest
            .get("payload")
            .and_then(Value::as_object)
            .is_some_and(|p| p.get("filename").and_then(Value::as_str) == Some("fields.npz"));
        if !payload_ok {
            return fail(format!("artifact {artifact_id} payload path is unsafe"));
        }
        let (payload_file, payload_info) = openat_regular(&folder_fd, "fields.npz", 1, max_payload_bytes)?;
        let payload_sha256 =
            manifest.get("payload").and_then(|p| p.get("sha256")).and_then(Value::as_str).map(str::to_string);
        let mut bundle = Bundle {
            root_fd,
            folder_fd,
            folder_info: after,
            folder_entries,
            manifest_file,
            manifest_info,
            manifest_sha256,
            manifest,
            payload_file,
            payload_info,
            payload_sha256,
        };
        Self::assert_bundle_stable(artifact_id, &mut bundle)?;
        Ok(bundle)
    }

    fn assert_bundle_stable(artifact_id: &str, bundle: &mut Bundle) -> AResult<()> {
        bundle.manifest_info = assert_open_file_stable(
            &bundle.folder_fd,
            "manifest.json",
            &bundle.manifest_file,
            &bundle.manifest_info,
            Some(&bundle.manifest_sha256),
        )?;
        bundle.payload_info = assert_open_file_stable(
            &bundle.folder_fd,
            "fields.npz",
            &bundle.payload_file,
            &bundle.payload_info,
            bundle.payload_sha256.as_deref(),
        )?;
        let changed =
            || ArtifactError::Invalid(format!("artifact {artifact_id} directory changed during validation"));
        let folder_now = bundle.folder_fd.stat().map_err(|_| changed())?;
        let folder_bound = bundle.root_fd.stat_at(artifact_id).map_err(|_| changed())?;
        if folder_now.data_identity() != bundle.folder_info.data_identity()
            || !folder_bound.same_object(&folder_now)
            || !folder_bound.is_dir()
        {
            return Err(changed());
        }
        if folder_now.ctime != bundle.folder_info.ctime {
            let entries_now = bundle.folder_fd.entries().map_err(|_| changed())?;
            let folder_after = bundle.folder_fd.stat().map_err(|_| changed())?;
            let rebound = bundle.root_fd.stat_at(artifact_id).map_err(|_| changed())?;
            if entries_now != bundle.folder_entries
                || folder_after.data_identity() != folder_now.data_identity()
                || !rebound.same_object(&folder_after)
                || !rebound.is_dir()
            {
                return Err(changed());
            }
            bundle.folder_info = folder_after;
        }
        Ok(())
    }

    fn validate_manifest_identity(
        artifact_id: &str,
        manifest: &Map<String, Value>,
        payload_bytes: u64,
        payload_sha256: &str,
        require_closed: bool,
    ) -> AResult<()> {
        let required = [
            "schema",
            "identities",
            "fields",
            "metadata",
            "payload_sha256",
            "artifact_id",
            "created_unix_s",
            "payload",
        ];
        let closed = if require_closed {
            manifest.len() == required.len() && required.iter().all(|k| manifest.contains_key(*k))
        } else {
            required.iter().all(|k| manifest.contains_key(*k))
        };
        if !closed {
            return fail(format!("artifact {artifact_id} manifest schema is not closed"));
        }
        let payload = manifest.get("payload").and_then(Value::as_object);
        let payload_ok = payload.is_some_and(|p| {
            p.len() == 4
                && ["format", "filename", "bytes", "sha256"].iter().all(|k| p.contains_key(*k))
                && p.get("format").and_then(Value::as_str) == Some("npz")
                && p.get("filename").and_then(Value::as_str) == Some("fields.npz")
                && p.get("bytes")
                    .filter(|v| v.is_u64())
                    .and_then(Value::as_u64)
                    .is_some_and(|b| b > 0 && b == payload_bytes)
                && p.get("sha256").and_then(Value::as_str) == Some(payload_sha256)
        });
        let created_ok = manifest
            .get("created_unix_s")
            .is_some_and(|v| v.is_number() && v.as_f64().is_some_and(f64::is_finite));
        let ok = manifest.get("schema").and_then(Value::as_str) == Some(ARTIFACT_SCHEMA)
            && manifest.get("artifact_id").and_then(Value::as_str) == Some(artifact_id)
            && manifest.get("identities").is_some_and(Value::is_object)
            && manifest.get("fields").and_then(Value::as_object).is_some_and(|f| !f.is_empty())
            && manifest.get("metadata").is_some_and(Value::is_object)
            && payload_ok
            && manifest.get("payload_sha256").and_then(Value::as_str) == Some(payload_sha256)
            && created_ok;
        if !ok {
            return fail(format!("artifact {artifact_id} manifest contract drifted"));
        }
        let mut identity_doc = Map::new();
        for key in ["schema", "identities", "fields", "metadata"] {
            identity_doc.insert(key.into(), manifest[key].clone());
        }
        identity_doc.insert("payload_sha256".into(), Value::String(payload_sha256.into()));
        let expected =
            format!("result-{}", &sha256_hex(canonical_text(&Value::Object(identity_doc)).as_bytes())[..32]);
        if expected != artifact_id {
            return fail(format!("artifact {artifact_id} manifest identity mismatch"));
        }
        Ok(())
    }


    pub fn get(&self, artifact_id: &str) -> AResult<Map<String, Value>> {
        let mut bundle = self.open_bundle(artifact_id, 64 * 1024 * 1024, Some(DEFAULT_MAX_PAYLOAD_BYTES))?;
        let actual = sha256_fd(&bundle.payload_file)?;
        #[allow(clippy::cast_sign_loss)]
        let size = bundle.payload_info.size;
        Self::validate_manifest_identity(artifact_id, &bundle.manifest, size, &actual, false)?;
        Self::assert_bundle_stable(artifact_id, &mut bundle)?;
        Ok(bundle.manifest.clone())
    }


    pub fn payload_path(&self, artifact_id: &str) -> AResult<PathBuf> {
        let manifest = self.get(artifact_id)?;
        let name = manifest["payload"]["filename"].as_str().unwrap_or("fields.npz").to_string();
        Ok(self.root.join(artifact_id).join(name))
    }

    #[allow(clippy::too_many_lines)]
    fn bounded_archive(&self, artifact_id: &str, limits: &Limits) -> AResult<(Snapshot, Bundle)> {
        if limits.max_payload_bytes == 0
            || limits.max_uncompressed_bytes == 0
            || limits.max_field_bytes == 0
            || limits.max_fields == 0
            || limits.max_header_bytes == 0
            || limits.max_central_directory_bytes == 0
            || limits.max_field_name_bytes == 0
        {
            return fail("artifact inspection limits must be positive integers");
        }
        if !limits.max_compression_ratio.is_finite() || limits.max_compression_ratio < 1.0 {
            return fail("artifact inspection policy is invalid");
        }
        require_artifact_id(artifact_id)?;
        let max_manifest_bytes = (64 * 1024 * 1024_u64).min(
            (64 * 1024_u64).max(limits.max_fields as u64 * (limits.max_field_name_bytes as u64 + 16 * 1024)),
        );
        let mut bundle = self.open_bundle(artifact_id, max_manifest_bytes, Some(limits.max_payload_bytes))?;
        let payload = read_all(&bundle.payload_file, limits.max_payload_bytes)?;
        let actual = sha256_hex(&payload);
        Self::validate_manifest_identity(artifact_id, &bundle.manifest, payload.len() as u64, &actual, true)?;
        let fields_meta = bundle.manifest["fields"].as_object().cloned().unwrap_or_default();
        if fields_meta.len() > limits.max_fields {
            return fail(format!("artifact {artifact_id} declares too many fields"));
        }
        let mut expected_entries: BTreeMap<String, String> = BTreeMap::new();
        for (name, field) in &fields_meta {
            require_field_name(name, limits.max_field_name_bytes)?;
            let closed = field.as_object().is_some_and(|f| {
                f.len() == 4 && ["shape", "dtype", "size", "finite"].iter().all(|k| f.contains_key(*k))
            });
            if !closed {
                return fail(format!("artifact {artifact_id} field manifest drifted"));
            }
            expected_entries.insert(format!("{name}.npy"), name.clone());
        }
        zip_directory_contract(&payload, limits.max_fields, limits.max_central_directory_bytes)?;
        let archive = implexity_io::zip::ZipArchive::new(&payload).map_err(|_| {
            ArtifactError::Invalid(format!("artifact {artifact_id} payload is not a valid NPZ"))
        })?;
        let names: Vec<String> = archive.entries().iter().map(|e| e.name.clone()).collect();
        let unique: BTreeSet<&String> = names.iter().collect();
        let expected: BTreeSet<&String> = expected_entries.keys().collect();
        if unique.len() != names.len() || unique != expected || names.len() > limits.max_fields {
            return fail(format!("artifact {artifact_id} NPZ member set drifted"));
        }
        let mut inspected = BTreeMap::new();
        let mut total = 0_u64;
        for entry in archive.entries() {
            let name = expected_entries[&entry.name].clone();
            let unix_mode = (entry.external_attr >> 16) & 0xFFFF;
            let unsafe_meta = || {
                ArtifactError::Invalid(format!(
                    "artifact {artifact_id} field {} has unsafe archive metadata",
                    repr(&name)
                ))
            };
            if entry.name.ends_with('/')
                || (unix_mode & 0o170_000) == 0o120_000
                || entry.method != implexity_io::zip::DEFLATED
                || entry.flags & !0x800 != 0
                || entry.size == 0
                || entry.size > limits.max_field_bytes + limits.max_header_bytes
                || entry.compressed_size == 0
                || entry.name.len() > limits.max_field_name_bytes + 4
            {
                return Err(unsafe_meta());
            }
            #[allow(clippy::cast_precision_loss)]
            let ratio = entry.size as f64 / entry.compressed_size as f64;
            if ratio > limits.max_compression_ratio {
                return fail(format!(
                    "artifact {artifact_id} field {} exceeds the compression-ratio limit",
                    repr(&name)
                ));
            }
            total += entry.size;
            if total > limits.max_uncompressed_bytes {
                return fail(format!("artifact {artifact_id} exceeds the uncompressed-size limit"));
            }
            let member = archive.read_entry(entry).map_err(|_| {
                ArtifactError::Invalid(format!(
                    "artifact {artifact_id} field {} has an invalid NPY header",
                    repr(&name)
                ))
            })?;
            let Some((shape, fortran, descr, header_end)) = npy_header(&member, limits.max_header_bytes)
            else {
                return fail(format!(
                    "artifact {artifact_id} field {} has an invalid NPY header",
                    repr(&name)
                ));
            };
            let Some((itemsize, dtype, numeric)) = descr_facts(&descr) else {
                return fail(format!(
                    "artifact {artifact_id} field {} has an unsafe dtype or layout",
                    repr(&name)
                ));
            };
            if limits.require_numeric && !numeric {
                return fail(format!(
                    "artifact {artifact_id} field {} has an unsafe dtype or layout",
                    repr(&name)
                ));
            }
            let (size, nbytes) =
                safe_array_extent(&shape, itemsize, limits.max_field_bytes).map_err(|_| {
                    ArtifactError::Invalid(format!(
                        "artifact {artifact_id} field {} has an unsafe allocation extent",
                        repr(&name)
                    ))
                })?;
            if size == 0
                || nbytes == 0
                || header_end as u64 > limits.max_header_bytes + 16
                || header_end as u64 + nbytes != entry.size
            {
                return fail(format!("artifact {artifact_id} field {} byte extent drifted", repr(&name)));
            }
            let declared = &fields_meta[&name];
            let expected_finite = if numeric { Value::from(size) } else { Value::Null };
            let shape_value = Value::Array(shape.iter().map(|d| Value::from(*d)).collect());
            if declared.get("shape") != Some(&shape_value)
                || declared.get("dtype").and_then(Value::as_str) != Some(dtype.as_str())
                || !declared.get("size").is_some_and(|v| v.is_u64() && v.as_u64() == Some(size))
                || declared.get("finite") != Some(&expected_finite)
            {
                return fail(format!(
                    "artifact {artifact_id} field {} declared allocation size metadata drifted",
                    repr(&name)
                ));
            }
            inspected.insert(
                name,
                InspectedField {
                    shape,
                    dtype,
                    descr,
                    size,
                    nbytes,
                    archive_bytes: entry.size,
                    compressed_bytes: entry.compressed_size,
                    header_bytes: header_end as u64,
                    fortran_order: fortran,
                    zip_filename: entry.name.clone(),
                },
            );
        }
        Self::assert_bundle_stable(artifact_id, &mut bundle)?;
        let manifest = bundle.manifest.clone();
        Ok((Snapshot { manifest, inspected, payload }, bundle))
    }

    fn streamed_archive(&self, artifact_id: &str, limits: &Limits) -> AResult<(Snapshot, Bundle)> {
        if limits.max_payload_bytes == 0
            || limits.max_uncompressed_bytes == 0
            || limits.max_field_bytes == 0
            || limits.max_fields == 0
            || limits.max_header_bytes == 0
            || limits.max_central_directory_bytes == 0
            || limits.max_field_name_bytes == 0
        {
            return fail("artifact inspection limits must be positive integers");
        }
        if !limits.max_compression_ratio.is_finite() || limits.max_compression_ratio < 1.0 {
            return fail("artifact inspection policy is invalid");
        }
        require_artifact_id(artifact_id)?;
        let mut bundle = self.open_bundle(artifact_id, limits.max_manifest_bytes, Some(limits.max_payload_bytes))?;
        let actual = sha256_fd(&bundle.payload_file)?;
        Self::validate_manifest_identity(artifact_id, &bundle.manifest, bundle.payload_info.size, &actual, true)?;
        let fields_meta = bundle.manifest["fields"].as_object().cloned().unwrap_or_default();
        if fields_meta.len() > limits.max_fields {
            return fail(format!("artifact {artifact_id} declares too many fields"));
        }
        let mut expected_entries: BTreeMap<String, String> = BTreeMap::new();
        for (name, field) in &fields_meta {
            require_field_name(name, limits.max_field_name_bytes)?;
            let closed = field.as_object().is_some_and(|f| {
                f.len() == 4 && ["shape", "dtype", "size", "finite"].iter().all(|k| f.contains_key(*k))
            });
            if !closed {
                return fail(format!("artifact {artifact_id} field manifest drifted"));
            }
            expected_entries.insert(format!("{name}.npy"), name.clone());
        }
        let archive=implexity_io::zip::FileZipIndex::new(&bundle.payload_file,limits.max_fields,limits.max_central_directory_bytes)
            .map_err(|e|ArtifactError::Invalid(e.to_string()))?;
        let names: Vec<String> = archive.entries().iter().map(|e| e.name.clone()).collect();
        let unique: BTreeSet<&String> = names.iter().collect();
        let expected: BTreeSet<&String> = expected_entries.keys().collect();
        if unique.len() != names.len() || unique != expected || names.len() > limits.max_fields {
            return fail(format!("artifact {artifact_id} NPZ member set drifted"));
        }
        let mut inspected = BTreeMap::new();
        let mut total = 0_u64;
        for entry in archive.entries() {
            let name = expected_entries[&entry.name].clone();
            let unix_mode = (entry.external_attr >> 16) & 0xFFFF;
            let unsafe_meta = || {
                ArtifactError::Invalid(format!(
                    "artifact {artifact_id} field {} has unsafe archive metadata",
                    repr(&name)
                ))
            };
            if entry.name.ends_with('/')
                || (unix_mode & 0o170_000) == 0o120_000
                || entry.method != implexity_io::zip::DEFLATED
                || entry.flags & !0x800 != 0
                || entry.size == 0
                || entry.size > limits.max_field_bytes + limits.max_header_bytes
                || entry.compressed_size == 0
                || entry.name.len() > limits.max_field_name_bytes + 4
            {
                return Err(unsafe_meta());
            }
            #[allow(clippy::cast_precision_loss)]
            let ratio = entry.size as f64 / entry.compressed_size as f64;
            if ratio > limits.max_compression_ratio {
                return fail(format!(
                    "artifact {artifact_id} field {} exceeds the compression-ratio limit",
                    repr(&name)
                ));
            }
            total += entry.size;
            if total > limits.max_uncompressed_bytes {
                return fail(format!("artifact {artifact_id} exceeds the uncompressed-size limit"));
            }
            let mut member=Vec::new();
            let header_limit=usize::try_from(limits.max_header_bytes.saturating_add(16)).map_err(|_|ArtifactError::Invalid("NPY header budget exceeds platform size".into()))?;
            archive.visit(&bundle.payload_file,entry,|chunk|{
                let n=chunk.len().min(header_limit.saturating_sub(member.len()));member.extend_from_slice(&chunk[..n]);Ok(())
            }).map_err(|e|ArtifactError::Invalid(e.to_string()))?;
            let Some((shape, fortran, descr, header_end)) = npy_header(&member, limits.max_header_bytes)
            else {
                return fail(format!(
                    "artifact {artifact_id} field {} has an invalid NPY header",
                    repr(&name)
                ));
            };
            let Some((itemsize, dtype, numeric)) = descr_facts(&descr) else {
                return fail(format!(
                    "artifact {artifact_id} field {} has an unsafe dtype or layout",
                    repr(&name)
                ));
            };
            if limits.require_numeric && !numeric {
                return fail(format!(
                    "artifact {artifact_id} field {} has an unsafe dtype or layout",
                    repr(&name)
                ));
            }
            let (size, nbytes) =
                safe_array_extent(&shape, itemsize, limits.max_field_bytes).map_err(|_| {
                    ArtifactError::Invalid(format!(
                        "artifact {artifact_id} field {} has an unsafe allocation extent",
                        repr(&name)
                    ))
                })?;
            if size == 0
                || nbytes == 0
                || header_end as u64 > limits.max_header_bytes + 16
                || header_end as u64 + nbytes != entry.size
            {
                return fail(format!("artifact {artifact_id} field {} byte extent drifted", repr(&name)));
            }
            let declared = &fields_meta[&name];
            let expected_finite = if numeric { Value::from(size) } else { Value::Null };
            let shape_value = Value::Array(shape.iter().map(|d| Value::from(*d)).collect());
            if declared.get("shape") != Some(&shape_value)
                || declared.get("dtype").and_then(Value::as_str) != Some(dtype.as_str())
                || !declared.get("size").is_some_and(|v| v.is_u64() && v.as_u64() == Some(size))
                || declared.get("finite") != Some(&expected_finite)
            {
                return fail(format!(
                    "artifact {artifact_id} field {} declared allocation size metadata drifted",
                    repr(&name)
                ));
            }
            inspected.insert(
                name,
                InspectedField {
                    shape,
                    dtype,
                    descr,
                    size,
                    nbytes,
                    archive_bytes: entry.size,
                    compressed_bytes: entry.compressed_size,
                    header_bytes: header_end as u64,
                    fortran_order: fortran,
                    zip_filename: entry.name.clone(),
                },
            );
        }
        Self::assert_bundle_stable(artifact_id, &mut bundle)?;
        let manifest = bundle.manifest.clone();
        Ok((Snapshot { manifest, inspected, payload: Vec::new() }, bundle))
    }


    pub fn inspect_streamed(&self,artifact_id:&str,limits:&Limits)->AResult<(Map<String,Value>,BTreeMap<String,InspectedField>)>{
        let(snapshot,mut bundle)=self.streamed_archive(artifact_id,limits)?;Self::assert_bundle_stable(artifact_id,&mut bundle)?;Ok((snapshot.manifest,snapshot.inspected))
    }

    pub fn read_arrays_streamed(&self,artifact_id:&str,selected:Option<&[String]>,limits:&Limits)->AResult<Vec<(String,NpyArray)>>{
        let(snapshot,mut bundle)=self.streamed_archive(artifact_id,limits)?;
        let names:Vec<String>=selected.map_or_else(||snapshot.inspected.keys().cloned().collect(),|names|names.to_vec());
        let unique:BTreeSet<&String>=names.iter().collect();
        if names.is_empty()||unique.len()!=names.len()||names.iter().any(|n|!snapshot.inspected.contains_key(n)){return fail("explicit unique retained field names required");}
        let index=implexity_io::zip::FileZipIndex::new(&bundle.payload_file,limits.max_fields,limits.max_central_directory_bytes).map_err(|e|ArtifactError::Invalid(e.to_string()))?;
        let mut out=Vec::with_capacity(names.len());
        for name in names {
            let details=&snapshot.inspected[&name];let entry=index.entries().iter().find(|e|e.name==details.zip_filename).ok_or_else(||ArtifactError::Invalid("retained archive member missing".into()))?;
            let size=usize::try_from(entry.size).map_err(|_|ArtifactError::Invalid("selected field exceeds platform allocation size".into()))?;
            let mut bytes=Vec::new();bytes.try_reserve_exact(size).map_err(|e|ArtifactError::Invalid(e.to_string()))?;
            index.visit(&bundle.payload_file,entry,|chunk|{bytes.extend_from_slice(chunk);Ok(())}).map_err(|e|ArtifactError::Invalid(e.to_string()))?;
            let array=NpyArray::from_bytes(&bytes).map_err(|e|ArtifactError::Invalid(e.to_string()))?;
            if array.shape!=details.shape||dtype_name(&array.data)!=details.dtype||details.fortran_order||finite_count(&array.data).is_some_and(|n|n!=array.data.len()){return fail("retained field payload drifted");}
            out.push((name,array));
        }
        Self::assert_bundle_stable(artifact_id,&mut bundle)?;Ok(out)
    }


    pub fn inspect_bounded(
        &self,
        artifact_id: &str,
        limits: &Limits,
    ) -> AResult<(Map<String, Value>, BTreeMap<String, InspectedField>)> {
        let (snapshot, mut bundle) = self.bounded_archive(artifact_id, limits)?;
        Self::assert_bundle_stable(artifact_id, &mut bundle)?;
        Ok((snapshot.manifest, snapshot.inspected))
    }


    pub fn read_arrays_bounded(
        &self,
        artifact_id: &str,
        selected: Option<&[String]>,
        limits: &Limits,
    ) -> AResult<Vec<(String, NpyArray)>> {
        if let Some(sel) = selected {
            let unique: BTreeSet<&String> = sel.iter().collect();
            if sel.is_empty() || sel.iter().any(String::is_empty) || unique.len() != sel.len() {
                return fail("selected fields must be explicit unique names");
            }
        }
        let (snapshot, mut bundle) = self.bounded_archive(artifact_id, limits)?;
        let names: Vec<String> = match selected {
            None => snapshot.inspected.keys().cloned().collect(),
            Some(sel) => sel.to_vec(),
        };
        if names.iter().any(|n| !snapshot.inspected.contains_key(n)) {
            return fail("selected artifact field is missing");
        }
        let archive = implexity_io::zip::ZipArchive::new(&snapshot.payload).map_err(|_| {
            ArtifactError::Invalid(format!("artifact {artifact_id} payload changed or is invalid"))
        })?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let details = &snapshot.inspected[&name];
            let undecodable = || {
                ArtifactError::Invalid(format!(
                    "artifact {artifact_id} field {} cannot be decoded safely",
                    repr(&name)
                ))
            };
            let member = archive.read(&details.zip_filename).map_err(|_| undecodable())?;
            let array = NpyArray::from_bytes(&member).map_err(|_| undecodable())?;
            let finite = finite_count(&array.data).is_none_or(|c| c == array.data.len());
            if array.shape != details.shape
                || dtype_name(&array.data) != details.dtype
                || details.fortran_order
                || !finite
            {
                return fail(format!("artifact {artifact_id} field {} payload drifted", repr(&name)));
            }
            out.push((name, array));
        }
        Self::assert_bundle_stable(artifact_id, &mut bundle)?;
        Ok(out)
    }


    pub fn load(&self, artifact_id: &str) -> AResult<BTreeMap<String, NpyArray>> {
        Ok(self.read_arrays_bounded(artifact_id, None, &Limits::default())?.into_iter().collect())
    }


    pub fn import_verified(&self, source: &Self, artifact_id: &str) -> AResult<Map<String, Value>> {
        require_artifact_id(artifact_id)?;
        let keys = ["schema", "artifact_id", "identities", "fields", "metadata", "payload_sha256", "payload"];
        let equivalent =
            |a: &Map<String, Value>, b: &Map<String, Value>| keys.iter().all(|k| a.get(*k) == b.get(*k));
        let destination = self.root.join(artifact_id);
        let io = |e: std::io::Error| ArtifactError::Invalid(e.to_string());
        let (snapshot, mut bundle) = source.bounded_archive(artifact_id, &Limits::default())?;
        if destination.exists() {
            let current = self.get(artifact_id)?;
            if !equivalent(&current, &snapshot.manifest) {
                return fail(format!("existing artifact {artifact_id} differs from verified source"));
            }
            return Ok(current);
        }
        let temporary = self.root.join(format!(".import-{}", token_hex(8).map_err(io)?));
        std::fs::create_dir(&temporary).map_err(io)?;
        let result = (|| -> AResult<Map<String, Value>> {
            fsguard::set_owner_only(&temporary, true).map_err(io)?;
            let encoded = indented_sorted_text(&Value::Object(snapshot.manifest.clone()));
            let mut manifest =
                crate::private::create_exclusive(&temporary.join("manifest.json"), 0o600).map_err(io)?;
            crate::private::write_all_sync(&mut manifest, encoded.as_bytes()).map_err(io)?;
            drop(manifest);
            let copied = read_all(&bundle.payload_file, DEFAULT_MAX_PAYLOAD_BYTES)?;
            let mut payload =
                crate::private::create_exclusive(&temporary.join("fields.npz"), 0o600).map_err(io)?;
            crate::private::write_all_sync(&mut payload, &copied).map_err(io)?;
            drop(payload);
            let declared_bytes = snapshot.manifest["payload"]["bytes"].as_u64();
            if declared_bytes != Some(copied.len() as u64)
                || snapshot.manifest["payload_sha256"].as_str() != Some(sha256_hex(&copied).as_str())
            {
                return fail(format!("artifact {artifact_id} changed during import"));
            }
            crate::private::fsync_dir(&temporary).map_err(io)?;
            Self::assert_bundle_stable(artifact_id, &mut bundle)?;
            if std::fs::rename(&temporary, &destination).is_err() {
                if !destination.exists() {
                    return fail(format!("artifact {artifact_id} could not be imported"));
                }
                let current = self.get(artifact_id)?;
                if !equivalent(&current, &snapshot.manifest) {
                    return fail(format!("racing artifact {artifact_id} differs from verified source"));
                }
                return Ok(current);
            }
            crate::private::fsync_dir(&self.root).map_err(io)?;
            let published = self.get(artifact_id)?;
            if !equivalent(&published, &snapshot.manifest) {
                return fail(format!("published artifact {artifact_id} drifted"));
            }
            Ok(published)
        })();
        if temporary.exists() {
            let _ = std::fs::remove_dir_all(&temporary);
        }
        result
    }

    #[must_use]
    pub fn list(&self) -> Vec<Map<String, Value>> {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("result-") && self.root.join(n).join("manifest.json").exists())
            .collect();
        names.sort();
        names.iter().filter_map(|n| self.get(n).ok()).collect()
    }
}

