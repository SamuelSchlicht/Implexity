// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value, json};

use implexity_core::json::parse_strict;
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::list_repr;

use super::verify::reread_mesh;
use super::{
    MF_PART, MF_RELATION, PLY_CHUNK, ProvResult, ProvenanceError, SCHEMA, STEP_CHUNK, STL_HEADER_BYTES,
    STL_PREFIX, canonical, compute_record_id, embed_err, embedded_copy, finalise, get, invalid, sha256_hex,
    sidecar_bytes,
};
use crate::npy::NpyArray;
use crate::zip::{MemberOptions, ZipArchive, ZipWriter};

fn io_err(path: &Path, e: &std::io::Error) -> ProvenanceError {
    invalid(format!("{}: {e}", path.display()))
}

fn read(path: &Path) -> ProvResult<Vec<u8>> {
    std::fs::read(path).map_err(|e| io_err(path, &e))
}

fn write(path: &Path, bytes: &[u8]) -> ProvResult<()> {
    std::fs::write(path, bytes).map_err(|e| io_err(path, &e))
}

fn record_id(record: &Value) -> String {
    match get(record, "record_id") {
        Value::String(s) => s.clone(),
        _ => compute_record_id(record),
    }
}

fn parse_record(text: &str) -> ProvResult<Value> {
    parse_strict(text).map_err(|e| invalid(e.to_string()))
}

#[must_use]
pub fn capability() -> Value {
    json!({
        "stl": {"carries": "pointer", "where": "the 80-byte binary header", "bytes_added": 0,
                "why": "there is no other place in a binary STL to put a byte, and 80 bytes does not hold a record"},
        "ply": {"carries": "record", "where": "header comments, 512-char chunks",
                "bytes_added": "the record, plus ~26 bytes per chunk",
                "why": "a PLY header takes any number of comment lines and the binary body is untouched"},
        "3mf": {"carries": "record", "where": format!("{MF_PART} (an OPC part)"),
                "bytes_added": "the record, deflated, plus one relationship",
                "why": "3MF is a zip; every other member, the model part included, is copied byte for byte"},
        "step": {"carries": "record",
                 "where": format!("ISO 10303-21 comment blocks in HEADER, {} characters each, plus the id in FILE_DESCRIPTION", *STEP_CHUNK),
                 "bytes_added": "the record, plus ~70 bytes per block",
                 "why": "comments are legal between tokens anywhere in a STEP file and FILE_DESCRIPTION's description is a LIST OF STRING; the blocks are small because OCCT's scanner reads a comment as one token into a 16 KiB buffer it cannot grow (measured: 16375 chars read, 16437 do not)"},
        "npz": {"carries": "record", "where": "a implexity_provenance member", "bytes_added": "the record, deflated",
                "why": "both design loaders key on channel names, so an extra member is invisible to them"},
        "vdb": {"carries": "record", "where": "per-grid metadata (NOT written by this service)",
                "bytes_added": "the record, per grid",
                "why": "OpenVDB grid metadata takes arbitrary named strings; VDB files are written by an external exchange tool"},
    })
}

#[must_use]
pub fn format_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "stl" => Some("stl"),
        "ply" => Some("ply"),
        "3mf" => Some("3mf"),
        "step" | "stp" => Some("step"),
        "npz" => Some("npz"),
        "vdb" => Some("vdb"),
        _ => None,
    }
}


pub fn stl_header(record_id: &str, url: Option<&str>) -> ProvResult<(Vec<u8>, Option<String>)> {
    let mut head = format!("{STL_PREFIX}{record_id}");
    let mut dropped = None;
    if let Some(u) = url.filter(|u| !u.is_empty()) {
        let cand = format!("{head} {u}");
        if cand.len() <= STL_HEADER_BYTES {
            head = cand;
        } else {
            dropped = Some(format!(
                "the URL {} does not fit in the 80-byte STL header beside the schema and the record id, so it was omitted rather than truncated",
                repr_str(u)
            ));
        }
    }
    let mut raw = head.into_bytes();
    if raw.len() > STL_HEADER_BYTES {
        return Err(embed_err("the record id alone does not fit in the 80-byte STL header"));
    }
    raw.resize(STL_HEADER_BYTES, 0);
    Ok((raw, dropped))
}

fn embed_stl(path: &Path, record: &Value, url: Option<&str>) -> ProvResult<Value> {
    use std::io::{Seek, SeekFrom, Write};
    let (head, dropped) = stl_header(&record_id(record), url)?;
    let mut f =
        std::fs::OpenOptions::new().read(true).write(true).open(path).map_err(|e| io_err(path, &e))?;
    f.seek(SeekFrom::Start(0)).and_then(|_| f.write_all(&head)).map_err(|e| io_err(path, &e))?;
    let text = String::from_utf8_lossy(&head).trim_end_matches('\0').to_string();
    Ok(json!({"format": "stl", "carries": "pointer", "bytes_added": 0, "header": text, "url": url,
              "dropped": dropped, "note": "the STL header is fixed length, so this costs no bytes"}))
}

fn read_stl_pointer(path: &Path) -> ProvResult<Option<Value>> {
    let raw = read(path)?;
    let head = &raw[..raw.len().min(STL_HEADER_BYTES)];
    let text = String::from_utf8_lossy(head).trim_end_matches('\0').to_string();
    let Some(rest) = text.strip_prefix(STL_PREFIX) else { return Ok(None) };
    let (id, url) = match rest.split_once(' ') {
        Some((a, b)) => (a.to_string(), json!(b)),
        None => (rest.to_string(), Value::Null),
    };
    Ok(Some(json!({"schema": SCHEMA, "record_id": id, "url": url})))
}

const PLY_TAG: &str = "implexity-record";

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn chunks(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(size.max(1)).map(|c| c.iter().collect()).collect()
}

fn embed_ply(path: &Path, record: &Value) -> ProvResult<Value> {
    let blob = canonical(&embedded_copy(record));
    let raw = read(path)?;
    let marker = b"end_header\n";
    let i = find(&raw, marker).ok_or_else(|| embed_err(format!("{}: no PLY end_header", path.display())))?;
    let body_before = sha256_hex(&raw[i + marker.len()..]);
    let parts = chunks(&blob, PLY_CHUNK);
    let mut lines = vec![format!(
        "comment {PLY_TAG}-id {} {} {}",
        record_id(record),
        blob.chars().count(),
        sha256_hex(blob.as_bytes())
    )];
    for (n, c) in parts.iter().enumerate() {
        lines.push(format!("comment {PLY_TAG} {}/{} {c}", n + 1, parts.len()));
    }
    let extra = format!("{}\n", lines.join("\n")).into_bytes();
    let mut out = raw[..i].to_vec();
    out.extend_from_slice(&extra);
    out.extend_from_slice(&raw[i..]);
    write(path, &out)?;
    let raw2 = read(path)?;
    let j = find(&raw2, marker).unwrap_or(raw2.len());
    if sha256_hex(raw2.get(j + marker.len()..).unwrap_or(&[])) != body_before {
        return Err(embed_err(format!("{}: the PLY binary body moved", path.display())));
    }
    Ok(
        json!({"format": "ply", "carries": "record", "chunks": parts.len(), "record_bytes": blob.chars().count(),
              "bytes_added": extra.len(), "body_sha256_unchanged": true}),
    )
}

fn int_list(v: &[usize]) -> String {
    format!("[{}]", v.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
}

fn assemble(
    path: &Path,
    parts: &std::collections::BTreeMap<usize, String>,
    total: usize,
    what: &str,
) -> ProvResult<String> {
    let keys: Vec<usize> = parts.keys().copied().collect();
    if keys != (1..=total).collect::<Vec<_>>() {
        return Err(embed_err(format!("{}: {what} {} of {total}", path.display(), int_list(&keys))));
    }
    Ok((1..=total).filter_map(|k| parts.get(&k).cloned()).collect())
}

fn read_ply_record(path: &Path) -> ProvResult<Option<Value>> {
    let raw = read(path)?;
    let mut parts = std::collections::BTreeMap::new();
    let (mut total, mut want_id, mut want_sha): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    let id_prefix = format!("comment {PLY_TAG}-id ");
    let chunk_prefix = format!("comment {PLY_TAG} ");
    for line in raw.split_inclusive(|&b| b == b'\n') {
        if line.trim_ascii() == b"end_header" {
            break;
        }
        let text = String::from_utf8_lossy(line);
        let s = text.trim_end_matches('\n');
        if s.starts_with(&id_prefix) {
            let bits: Vec<&str> = s.split_whitespace().collect();
            let bit = |i: usize| {
                bits.get(i).map(|b| (*b).to_string()).ok_or_else(|| invalid("list index out of range"))
            };
            want_id = Some(bit(2)?);
            want_sha = Some(bit(4)?);
        } else if s.starts_with(&chunk_prefix) {
            let bits: Vec<&str> = s.splitn(4, ' ').collect();
            let (n, t) = bits
                .get(2)
                .and_then(|b| b.split_once('/'))
                .ok_or_else(|| invalid("not enough values to unpack (expected 2, got 1)"))?;
            let n: usize = n
                .parse()
                .map_err(|_| invalid(format!("invalid literal for int() with base 10: {}", repr_str(n))))?;
            total = Some(t.to_string());
            parts.insert(n, bits.get(3).map(|b| (*b).to_string()).unwrap_or_default());
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    let t = total.unwrap_or_default();
    let total: usize = t
        .parse()
        .map_err(|_| invalid(format!("invalid literal for int() with base 10: {}", repr_str(&t))))?;
    let blob = assemble(path, &parts, total, "PLY record chunks")?;
    if want_sha.as_ref().is_some_and(|w| !w.is_empty() && sha256_hex(blob.as_bytes()) != *w) {
        return Err(embed_err(format!("{}: PLY record chunk hash mismatch", path.display())));
    }
    let rec = parse_record(&blob)?;
    if want_id.as_ref().is_some_and(|w| !w.is_empty() && get(&rec, "record_id").as_str() != Some(w)) {
        return Err(embed_err(format!("{}: PLY record id mismatch", path.display())));
    }
    Ok(Some(rec))
}

const RELS_NAME: &str = "3D/_rels/3dmodel.model.rels";

fn embed_3mf(path: &Path, record: &Value) -> ProvResult<Value> {
    let blob = canonical(&embedded_copy(record));
    let raw = read(path)?;
    let part = MF_PART.trim_start_matches('/');
    let z = ZipArchive::new(&raw).map_err(|e| invalid(e.0))?;
    let names = z.names();
    if names.iter().any(|n| n == part) {
        return Err(embed_err(format!("{} already carries {MF_PART}", path.display())));
    }
    let mut before: Vec<(String, String)> = Vec::new();
    let mut w = ZipWriter::new();
    for e in z.entries() {
        let mut data = z.read_entry(e).map_err(|x| invalid(x.0))?;
        if !before.iter().any(|(n, _)| *n == e.name) {
            before.push((e.name.clone(), sha256_hex(&data)));
        }
        if e.name == RELS_NAME {
            let add = format!(
                "\t<Relationship Type=\"{MF_RELATION}\" Target=\"{MF_PART}\" Id=\"relimplexityprov\"/>\n</Relationships>"
            );
            let close = b"</Relationships>";
            if find(&data, close).is_none() {
                return Err(embed_err(format!("{}: unexpected .rels part", path.display())));
            }
            data = replace_all(&data, close, add.as_bytes());
        }
        w.add_member(&e.name, &data, e.method, 6, MemberOptions::of(e)).map_err(|x| invalid(x.0))?;
    }
    w.add_member(part, blob.as_bytes(), crate::zip::DEFLATED, 6, MemberOptions::now())
        .map_err(|x| invalid(x.0))?;
    let bytes = w.finish().map_err(|x| invalid(x.0))?;
    let tmp = std::path::PathBuf::from(format!("{}.prov.tmp", path.display()));
    write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path).map_err(|e| io_err(path, &e))?;
    let raw2 = read(path)?;
    let z2 = ZipArchive::new(&raw2).map_err(|e| invalid(e.0))?;
    let after = |name: &str| z2.read(name).ok().map(|d| sha256_hex(&d));
    let moved: Vec<String> = before
        .iter()
        .filter(|(n, h)| n != RELS_NAME && after(n).as_deref() != Some(h.as_str()))
        .map(|(n, _)| n.clone())
        .collect();
    if !moved.is_empty() {
        return Err(embed_err(format!("{}: these 3MF parts moved: {}", path.display(), list_repr(&moved))));
    }
    let unchanged: Vec<&String> = before.iter().map(|(n, _)| n).filter(|n| *n != RELS_NAME).collect();
    Ok(json!({
        "format": "3mf", "carries": "record", "part": MF_PART, "relationship": MF_RELATION,
        "record_bytes": blob.len(), "parts_unchanged": unchanged,
        "model_part_sha256": after("3D/3dmodel.model"),
        "note": "the archive is REWRITTEN, so every member is re-deflated by python's zlib at its own level and the file's size change is the record MINUS whatever that recompression saves -- which is why the byte cost of a 3MF record can come out negative. What matters is that every member's CONTENT is unchanged, and that is checked above by hashing each one before and after.",
    }))
}

fn replace_all(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + to.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

fn read_3mf_record(path: &Path) -> ProvResult<Option<Value>> {
    let raw = read(path)?;
    let z = ZipArchive::new(&raw).map_err(|e| invalid(e.0))?;
    let part = MF_PART.trim_start_matches('/');
    if !z.names().iter().any(|n| n == part) {
        return Ok(None);
    }
    let data = z.read(part).map_err(|e| invalid(e.0))?;
    let text = String::from_utf8(data).map_err(|e| invalid(e.to_string()))?;
    parse_record(&text).map(Some)
}

const STEP_OPEN: &str = "/* implexity-provenance/1 ";
const STEP_CLOSE: &str = "*/";

#[must_use]
pub fn append_file_description(text: &str, extra: &str) -> Option<String> {
    let b = text.as_bytes();
    let i = text.find("FILE_DESCRIPTION")?;
    let j = text[i..].find('(').map(|x| x + i)?;
    let mut k = j + 1;
    while k < b.len() && matches!(b[k], b' ' | b'\t' | b'\r' | b'\n') {
        k += 1;
    }
    if k >= b.len() || b[k] != b'(' {
        return None;
    }
    let (mut depth, mut quoted, mut p) = (0i64, false, k);
    let mut closed = false;
    while p < b.len() {
        let c = b[p];
        if quoted {
            if c == b'\'' {
                if b.get(p + 1) == Some(&b'\'') {
                    p += 1;
                } else {
                    quoted = false;
                }
            }
        } else if c == b'\'' {
            quoted = true;
        } else if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth == 0 {
                closed = true;
                break;
            }
        }
        p += 1;
    }
    if !closed {
        return None;
    }
    Some(format!("{},\n  '{}'{}", &text[..p], extra.replace('\'', "''"), &text[p..]))
}

fn embed_step(path: &Path, record: &Value, url: Option<&str>) -> ProvResult<Value> {
    let raw = String::from_utf8(read(path)?).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    if raw.contains(STEP_OPEN) {
        return Err(embed_err(format!("{} already carries a implexity record", path.display())));
    }
    let i = raw
        .find("HEADER;")
        .ok_or_else(|| embed_err(format!("{}: no ISO 10303-21 HEADER section", path.display())))?
        + "HEADER;".len();
    let plain = canonical(&embedded_copy(record));
    let blob = plain.replace("*/", "*\\/");
    if blob.contains(STEP_CLOSE) {
        return Err(embed_err(format!("{}: the record still contains */ after escaping", path.display())));
    }
    let id = record_id(record);
    let parts = chunks(&blob, *STEP_CHUNK);
    let blob_sha = sha256_hex(blob.as_bytes());
    let mut comment = String::from("\n");
    for (n, c) in parts.iter().enumerate() {
        let _ = write!(
            comment,
            "{STEP_OPEN}{id} {}/{} sha256 {blob_sha}\n{c}\n{STEP_CLOSE}\n",
            n + 1,
            parts.len()
        );
    }
    let mut out = format!("{}{comment}{}", &raw[..i], &raw[i..]);
    let tag = format!(
        "implexity-provenance/1 record_id {id}{}",
        url.filter(|u| !u.is_empty()).map(|u| format!(" {u}")).unwrap_or_default()
    );
    let with_desc = append_file_description(&out, &tag);
    let desc_ok = with_desc.is_some();
    if let Some(w) = with_desc {
        out = w;
    }
    write(path, out.as_bytes())?;
    Ok(json!({
        "format": "step", "carries": "record",
        "comment_blocks": parts.len(), "comment_chunk_chars": *STEP_CHUNK,
        "comment_bytes": comment.chars().count(), "record_bytes": blob.chars().count(),
        "file_description_tag": if desc_ok { json!(tag) } else { Value::Null },
        "file_description_written": desc_ok,
        "file_description_note": if desc_ok { Value::Null } else {
            json!("FILE_DESCRIPTION was not shaped the way ISO 10303-21 describes, so it was left alone; the comment blocks carry the whole record either way")
        },
        "escaped_star_slash": plain.contains("*/"),
        "chunk_note": "OCCT reads a comment as ONE token into a 16 KiB buffer it cannot grow, so the record is written in blocks a quarter of that size; a single block would make OpenCASCADE refuse the file",
    }))
}

fn head_chars(path: &Path, max_chars: usize) -> ProvResult<String> {
    let raw = read(path)?;
    let text = String::from_utf8_lossy(&raw);
    Ok(text.chars().take(max_chars).collect())
}

fn read_step_record(path: &Path) -> ProvResult<Option<Value>> {
    let head = head_chars(path, 8 << 20)?;
    let mut parts = std::collections::BTreeMap::new();
    let (mut total, mut want_id, mut want_sha): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    let mut at = 0;
    while let Some(off) = head[at..].find(STEP_OPEN) {
        let i = at + off;
        let unterminated = || embed_err(format!("{}: unterminated implexity comment block", path.display()));
        let nl = head[i..].find('\n').map(|x| x + i).ok_or_else(unterminated)?;
        let bits: Vec<&str> = head[i + STEP_OPEN.len()..nl].split_whitespace().collect();
        let j = head[nl..].find(STEP_CLOSE).map(|x| x + nl).ok_or_else(unterminated)?;
        if bits.len() >= 4 {
            want_id = Some(bits[0].to_string());
            let (n, t) = bits[1]
                .split_once('/')
                .ok_or_else(|| invalid("not enough values to unpack (expected 2, got 1)"))?;
            total = Some(t.to_string());
            want_sha = Some(bits[3].to_string());
            let n: usize = n
                .parse()
                .map_err(|_| invalid(format!("invalid literal for int() with base 10: {}", repr_str(n))))?;
            parts.insert(n, head[nl + 1..j].trim_matches('\n').to_string());
        }
        at = j + STEP_CLOSE.len();
    }
    if parts.is_empty() {
        return Ok(None);
    }
    let t = total.unwrap_or_default();
    let total: usize = t
        .parse()
        .map_err(|_| invalid(format!("invalid literal for int() with base 10: {}", repr_str(&t))))?;
    let blob = assemble(path, &parts, total, "STEP record blocks")?;
    if want_sha.as_ref().is_some_and(|w| !w.is_empty() && sha256_hex(blob.as_bytes()) != *w) {
        return Err(embed_err(format!("{}: STEP record block hash mismatch", path.display())));
    }
    let rec = parse_record(&blob)?;
    if want_id.as_ref().is_some_and(|w| !w.is_empty() && get(&rec, "record_id").as_str() != Some(w)) {
        return Err(embed_err(format!("{}: STEP record id mismatch", path.display())));
    }
    Ok(Some(rec))
}


pub fn read_step_file_description(path: &Path) -> ProvResult<Option<String>> {
    let head = head_chars(path, 1 << 20)?;
    let Some(i) = head.find("FILE_DESCRIPTION") else { return Ok(None) };
    let j = head[i..]
        .find(';')
        .map_or_else(|| head.len().saturating_sub(head.chars().last().map_or(0, char::len_utf8)), |x| x + i);
    let slice = if j >= i { &head[i..j] } else { "" };
    Ok(Some(slice.split_whitespace().collect::<Vec<_>>().join(" ")))
}

const NPZ_MEMBER: &str = "implexity_provenance";

fn embed_npz(path: &Path, record: &Value) -> ProvResult<Value> {
    let blob = canonical(&embedded_copy(record));
    let before = crate::npz::load_file(path).map_err(|e| invalid(e.to_string()))?;
    if before.contains(NPZ_MEMBER) {
        return Err(embed_err(format!("{} already carries a implexity record", path.display())));
    }
    let provenance = NpyArray::scalar_str(&blob);
    let mut members: Vec<(&str, &NpyArray)> = vec![(NPZ_MEMBER, &provenance)];
    members.extend(before.members().iter().map(|(k, v)| (k.as_str(), v)));
    let bytes = crate::npz::save_compressed(&members).map_err(|e| invalid(e.to_string()))?;
    let tmp = std::path::PathBuf::from(format!("{}.prov.tmp.npz", path.display()));
    write(&tmp, &bytes)?;
    let reread = crate::npz::load_file(&tmp).map_err(|e| invalid(e.to_string()))?;
    for (k, v) in before.members() {
        if reread.get(k) != Some(v) {
            let _ = std::fs::remove_file(&tmp);
            return Err(embed_err(format!("{}: member {} moved", path.display(), repr_str(k))));
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| io_err(path, &e))?;
    let mut verified: Vec<&str> = before.files();
    verified.sort_unstable();
    Ok(
        json!({"format": "npz", "carries": "record", "member": NPZ_MEMBER, "record_bytes": blob.chars().count(),
              "members_verified": verified}),
    )
}

fn read_npz_record(path: &Path) -> ProvResult<Option<Value>> {
    let npz = crate::npz::load_file(path).map_err(|e| invalid(e.to_string()))?;
    let Some(a) = npz.get(NPZ_MEMBER) else { return Ok(None) };
    let text = a.as_scalar_str().ok_or_else(|| invalid("implexity_provenance is not a string scalar"))?;
    parse_record(text).map(Some)
}

#[must_use]
pub fn vdb_metadata(record: &Value, url: Option<&str>) -> Value {
    let blob = canonical(&embedded_copy(record));
    let mut md = Map::new();
    md.insert("implexity_provenance_schema".into(), json!(SCHEMA));
    md.insert("implexity_record_id".into(), json!(record_id(record)));
    let (bytes, sha) = (blob.chars().count(), sha256_hex(blob.as_bytes()));
    md.insert("implexity_record".into(), json!(blob));
    md.insert("implexity_record_bytes".into(), json!(bytes));
    md.insert("implexity_record_sha256".into(), json!(sha));
    if let Some(u) = url.filter(|u| !u.is_empty()) {
        md.insert("implexity_record_url".into(), json!(u));
    }
    Value::Object(md)
}

struct Cursor<'a> {
    raw: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> ProvResult<&[u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.raw.len());
        let Some(end) = end else {
            return Err(invalid(format!("unpack requires a buffer of {n} bytes")));
        };
        let s = &self.raw[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn skip(&mut self, n: usize) {
        self.at = (self.at + n).min(self.raw.len());
    }
    fn u32(&mut self) -> ProvResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn bstr(&mut self) -> ProvResult<String> {
        let n = self.u32()? as usize;
        let n = n.min(self.raw.len() - self.at);
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn meta_map(&mut self) -> ProvResult<Map<String, Value>> {
        let mut out = Map::new();
        for _ in 0..self.u32()? {
            let name = self.bstr()?;
            let typ = self.bstr()?;
            let n = self.u32()? as usize;
            let n = n.min(self.raw.len() - self.at);
            let payload = self.take(n)?.to_vec();
            let fixed = |len: usize| -> ProvResult<&[u8]> {
                if payload.len() == len {
                    Ok(&payload[..])
                } else {
                    Err(invalid(format!("unpack requires a buffer of {len} bytes")))
                }
            };
            let value = match typ.as_str() {
                "string" => json!(String::from_utf8_lossy(&payload)),
                "int64" => {
                    let b = fixed(8)?;
                    json!(i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
                }
                "float" => {
                    let b = fixed(4)?;
                    super::jsonable_f64(f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])))
                }
                "vec3i" => {
                    let b = fixed(12)?;
                    let at =
                        |k: usize| i32::from_le_bytes([b[4 * k], b[4 * k + 1], b[4 * k + 2], b[4 * k + 3]]);
                    json!([at(0), at(1), at(2)])
                }
                _ => json!({"type": typ, "bytes": payload.len()}),
            };
            out.insert(name, value);
        }
        Ok(out)
    }
}


pub fn read_vdb_metadata(path: &Path) -> ProvResult<Value> {
    let raw = read(path)?;
    let mut c = Cursor { raw: &raw, at: 0 };
    let m = c.take(8)?;
    let magic = u64::from_le_bytes([m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7]]);
    if magic != 0x5644_4220 {
        return Err(embed_err(format!("{}: not an OpenVDB archive (magic {magic:#x})", path.display())));
    }
    let file_version = c.u32()?;
    let library_major = c.u32()?;
    let library_minor = c.u32()?;
    let has_offsets = c.take(1)?[0] != 0;
    let uuid: String =
        c.take(36)?.iter().map(|&b| if b.is_ascii() { char::from(b) } else { '\u{fffd}' }).collect();
    let file_metadata = c.meta_map()?;
    let n_grids = c.u32()?;
    let mut grids = Vec::new();
    if n_grids > 0 {
        let name = c.bstr()?;
        let gtype = c.bstr()?;
        c.skip(4 + 3 * 8 + 4);
        let metadata = c.meta_map()?;
        grids.push(json!({"name": name, "type": gtype, "metadata": metadata}));
    }
    Ok(json!({
        "magic": format!("{magic:#x}"), "file_version": file_version, "library_major": library_major,
        "library_minor": library_minor, "has_grid_offsets": has_offsets, "uuid": uuid,
        "file_metadata": file_metadata, "grids": grids,
    }))
}

fn read_vdb_record(path: &Path) -> ProvResult<Option<Value>> {
    let info = read_vdb_metadata(path)?;
    for g in info["grids"].as_array().into_iter().flatten() {
        if let Some(Value::String(blob)) = g.get("metadata").and_then(|m| m.get("implexity_record"))
            && !blob.is_empty()
        {
            return parse_record(blob).map(Some);
        }
    }
    match info["file_metadata"].get("implexity_record") {
        Some(Value::String(blob)) if !blob.is_empty() => parse_record(blob).map(Some),
        _ => Ok(None),
    }
}


pub fn embed(path: &Path, record: &Value, fmt: Option<&str>, url: Option<&str>) -> ProvResult<Value> {
    let fmt = fmt.map(str::to_string).or_else(|| format_of(path).map(str::to_string));
    let result = match fmt.as_deref() {
        Some("stl") => embed_stl(path, record, url),
        Some("ply") => embed_ply(path, record),
        Some("3mf") => embed_3mf(path, record),
        Some("step") => embed_step(path, record, url),
        Some("npz") => embed_npz(path, record),
        other => {
            let shown = other.map_or_else(|| "None".to_string(), repr_str);
            return Ok(json!({"format": other, "carries": "nothing", "embedded": false,
                              "reason": format!("this service has no in-file embedding for {shown}; the record travels as the sidecar beside it")}));
        }
    };
    match result {
        Ok(mut out) => {
            if let Value::Object(m) = &mut out {
                m.insert("embedded".into(), json!(true));
            }
            Ok(out)
        }
        Err(ProvenanceError::Embed(reason)) => {
            Ok(json!({"format": fmt, "carries": "nothing", "embedded": false, "reason": reason}))
        }
        Err(e) => Err(e),
    }
}


pub fn read_embedded(path: &Path, fmt: Option<&str>) -> ProvResult<Option<Value>> {
    let fmt = fmt.map(str::to_string).or_else(|| format_of(path).map(str::to_string));
    match fmt.as_deref() {
        Some("stl") => read_stl_pointer(path),
        Some("ply") => read_ply_record(path),
        Some("3mf") => read_3mf_record(path),
        Some("step") => read_step_record(path),
        Some("npz") => read_npz_record(path),
        Some("vdb") => read_vdb_record(path),
        _ => Ok(None),
    }
}

fn file_size(path: &Path) -> ProvResult<u64> {
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| io_err(path, &e))
}

fn sha256_path(path: &Path) -> ProvResult<String> {
    crate::digest::sha256_file(path).map_err(|e| io_err(path, &e))
}


pub fn stamp(
    record: &mut Value,
    files: &[(String, Option<String>)],
    sidecar_path: Option<&Path>,
    urls: &Map<String, Value>,
    reread: bool,
) -> ProvResult<()> {
    let mut before: Map<String, Value> = Map::new();
    for (fmt, path) in files {
        let Some(p) = path.as_deref().filter(|p| !p.is_empty() && Path::new(p).is_file()) else { continue };
        let (bytes, sha) = (file_size(Path::new(p))?, sha256_path(Path::new(p))?);
        before.insert(fmt.clone(), json!({"bytes": bytes, "sha256": sha}));
        if let Some(Value::Object(a)) = record.get_mut("artefacts").and_then(|a| a.get_mut(fmt.as_str())) {
            a.insert("sha256_as_written".into(), json!(sha));
            a.insert("bytes_as_written".into(), json!(bytes));
        }
    }
    if let Some(side) = sidecar_path {
        let name = side.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Value::Object(m) = record else { return Err(invalid("a record must be an object")) };
        match m.entry("links").or_insert_with(|| json!({})) {
            Value::Object(links) => {
                links.insert("sidecar".into(), json!(name));
            }
            _ => return Err(invalid("record links must be an object")),
        }
    }
    finalise(record);
    let mut stamped = Map::new();
    for (fmt, path) in files {
        let Some(b) = before.get(fmt) else {
            let shown = path.as_deref().map_or_else(|| "None".to_string(), repr_str);
            stamped.insert(
                fmt.clone(),
                json!({"format": fmt, "embedded": false, "reason": format!("no such file: {shown}")}),
            );
            continue;
        };
        let p = Path::new(path.as_deref().unwrap_or_default());
        let url = urls.get(fmt).and_then(Value::as_str);
        let mut entry = embed(p, record, Some(fmt), url)?;
        let bytes = file_size(p)?;
        let before_bytes = b["bytes"].as_u64().unwrap_or(0);
        if let Value::Object(m) = &mut entry {
            m.insert("bytes_before_record".into(), b["bytes"].clone());
            m.insert("sha256_before_record".into(), b["sha256"].clone());
            m.insert("bytes".into(), json!(bytes));
            m.insert("sha256".into(), json!(sha256_path(p)?));
            #[allow(clippy::cast_possible_wrap)]
            m.insert("bytes_cost_of_record".into(), json!(bytes as i64 - before_bytes as i64));
            if reread && matches!(fmt.as_str(), "stl" | "ply" | "3mf") {
                let r = reread_mesh(p, fmt).unwrap_or_else(|e| json!({"error": e.py_repr()}));
                m.insert("reread".into(), r);
            }
        }
        stamped.insert(fmt.clone(), entry);
    }
    if let Value::Object(m) = record {
        m.insert("stamped".into(), Value::Object(stamped));
    }
    if let Some(side) = sidecar_path {
        let tmp = std::path::PathBuf::from(format!("{}.tmp", side.display()));
        write(&tmp, &sidecar_bytes(record))?;
        std::fs::rename(&tmp, side).map_err(|e| io_err(side, &e))?;
    }
    Ok(())
}

