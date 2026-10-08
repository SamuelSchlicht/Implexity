// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::fmt::Write as _;
use std::path::Path;

use crate::MeshError;
use crate::cast::{f32_of, i32_wrap};
use crate::topology::{Tri, Vec3};
use crate::zip::{ZipMethod, read_zip, write_zip};

pub type Triangle32 = [[f32; 3]; 3];

pub const STL_HEADER: &[u8] = b"implexity watertight body, millimetres";

fn f32_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}



pub fn stl_bytes(
    vertices: &[Vec3],
    faces: &[Tri],
    unit_scale: f64,
    header: &[u8],
) -> Result<Vec<u8>, MeshError> {
    if header.len() > 80 || header.get(..5).is_some_and(|h| h.eq_ignore_ascii_case(b"solid")) {
        return Err(MeshError::invalid(
            "a binary STL header is at most 80 bytes and does not begin with 'solid'",
        ));
    }
    let tris: Vec<Triangle32> = faces
        .iter()
        .map(|f| {
            f.map(|i| {
                let v = vertices[i];
                [f32_of(v[0] * unit_scale), f32_of(v[1] * unit_scale), f32_of(v[2] * unit_scale)]
            })
        })
        .collect();
    Ok(stl_from_triangles(&tris, header))
}

#[must_use]
pub fn stl_from_triangles(tris: &[Triangle32], header: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(84 + 50 * tris.len());
    out.extend_from_slice(&header[..header.len().min(80)]);
    out.resize(80, 0);
    out.extend_from_slice(&u32::try_from(tris.len()).unwrap_or(u32::MAX).to_le_bytes());
    for t in tris {
        let e1 = [t[1][0] - t[0][0], t[1][1] - t[0][1], t[1][2] - t[0][2]];
        let e2 = [t[2][0] - t[0][0], t[2][1] - t[0][1], t[2][2] - t[0][2]];
        let n = f32_cross(e1, e2);
        let ln = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let d = if ln > 0.0 { ln } else { 1.0 };
        for c in [n[0] / d, n[1] / d, n[2] / d] {
            out.extend_from_slice(&c.to_le_bytes());
        }
        for v in t.iter().flatten() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}



pub fn read_stl_bytes(blob: &[u8]) -> Result<(Vec<Triangle32>, Vec<u8>), MeshError> {
    if blob.len() < 84 {
        return Err(MeshError::invalid("a binary STL has at least 84 bytes"));
    }
    let count = u32::from_le_bytes([blob[80], blob[81], blob[82], blob[83]]) as usize;
    if Some(blob.len()) != count.checked_mul(50).and_then(|v| v.checked_add(84)) {
        return Err(MeshError::invalid("binary STL length does not match its facet count"));
    }
    let f = |p: usize| f32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]);
    let tris = (0..count)
        .map(|i| {
            let base = 84 + 50 * i + 12;
            std::array::from_fn(|v| std::array::from_fn(|c| f(base + 12 * v + 4 * c)))
        })
        .collect();
    Ok((tris, blob[..80].to_vec()))
}



pub fn write_file(path: &Path, bytes: &[u8]) -> Result<u64, MeshError> {
    std::fs::write(path, bytes).map_err(|e| MeshError::io(format!("writing {}", path.display()), e))?;
    Ok(bytes.len() as u64)
}



pub fn write_stl(path: &Path, vertices: &[Vec3], faces: &[Tri], unit_scale: f64) -> Result<u64, MeshError> {
    write_file(path, &stl_bytes(vertices, faces, unit_scale, STL_HEADER)?)
}



pub fn write_stl_named(path: &Path, vertices: &[Vec3], faces: &[Tri], name: &str) -> Result<u64, MeshError> {
    let mut out = Vec::with_capacity(84 + 50 * faces.len());
    let header: Vec<u8> = name.as_bytes().iter().copied().take(80).collect();
    out.extend_from_slice(&header);
    out.resize(80, 0);
    out.extend_from_slice(&u32::try_from(faces.len()).unwrap_or(u32::MAX).to_le_bytes());
    for f in faces {
        let (a, b, c) = (vertices[f[0]], vertices[f[1]], vertices[f[2]]);
        let n = crate::topology::cross(
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        let ln = crate::topology::norm(n).max(1e-300);
        for v in [n[0] / ln, n[1] / ln, n[2] / ln].iter().chain(a.iter()).chain(b.iter()).chain(c.iter()) {
            out.extend_from_slice(&f32_of(*v).to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    write_file(path, &out)
}

#[must_use]
pub fn ply_bytes(vertices: &[Vec3], faces: &[Tri], unit_scale: f64, comments: &[String]) -> Vec<u8> {
    let mut head = vec![
        "ply".to_string(),
        "format binary_little_endian 1.0".to_string(),
        "comment implexity watertight solid body".to_string(),
        "comment lengths in millimetres".to_string(),
    ];
    head.extend(comments.iter().map(|c| format!("comment {c}")));
    head.push(format!("element vertex {}", vertices.len()));
    head.extend(["property float x", "property float y", "property float z"].map(String::from));
    head.push(format!("element face {}", faces.len()));
    head.push("property list uchar int vertex_indices".to_string());
    head.push("end_header".to_string());
    let mut out = (head.join("\n") + "\n").into_bytes();
    for v in vertices.iter().flatten() {
        out.extend_from_slice(&f32_of(v * unit_scale).to_le_bytes());
    }
    for f in faces {
        out.push(3);
        for &i in f {
            out.extend_from_slice(&i32_wrap(i).to_le_bytes());
        }
    }
    out
}



pub fn write_ply(
    path: &Path,
    vertices: &[Vec3],
    faces: &[Tri],
    unit_scale: f64,
    comments: &[String],
) -> Result<u64, MeshError> {
    write_file(path, &ply_bytes(vertices, faces, unit_scale, comments))
}

pub const RECIPE_RELATIONSHIP: &str = "http://schemas.implexity/recipe";

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn f32_text(v: f32) -> String {
    if v == 0.0 { "0".to_string() } else { format!("{v}") }
}



pub fn threemf_bytes(
    vertices: &[Vec3],
    faces: &[Tri],
    unit_scale: f64,
    name: &str,
    attachments: &[(String, Vec<u8>)],
    metadata: &[(String, String)],
) -> Result<Vec<u8>, MeshError> {
    let mut model = String::with_capacity(64 * (vertices.len() + faces.len()) + 1024);
    model.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    model.push_str(
        "<model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" unit=\"millimeter\" \
         xml:lang=\"en-US\">\n",
    );
    for (k, v) in metadata {
        let _ = writeln!(
            model,
            "\t<metadata name=\"{}\" preserve=\"1\">{}</metadata>",
            xml_escape(k),
            xml_escape(v)
        );
    }
    let _ =
        writeln!(model, "\t<resources>\n\t\t<object id=\"1\" name=\"{}\" type=\"model\">", xml_escape(name));
    model.push_str("\t\t\t<mesh>\n\t\t\t\t<vertices>\n");
    for v in vertices {
        let p = v.map(|c| f32_of(c * unit_scale));
        let _ = writeln!(
            model,
            "\t\t\t\t\t<vertex x=\"{}\" y=\"{}\" z=\"{}\" />",
            f32_text(p[0]),
            f32_text(p[1]),
            f32_text(p[2])
        );
    }
    model.push_str("\t\t\t\t</vertices>\n\t\t\t\t<triangles>\n");
    for f in faces {
        let _ = writeln!(model, "\t\t\t\t\t<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\" />", f[0], f[1], f[2]);
    }
    model.push_str("\t\t\t\t</triangles>\n\t\t\t</mesh>\n\t\t</object>\n\t</resources>\n");
    model.push_str("\t<build>\n\t\t<item objectid=\"1\"/>\n\t</build>\n</model>\n");

    let mut extensions: Vec<String> = vec!["model".into(), "rels".into()];
    for (uri, _) in attachments {
        if !uri.starts_with('/') || uri.len() < 2 || uri.contains("..") {
            return Err(MeshError::invalid(format!(
                "3MF attachment path {uri:?} must be an absolute package path"
            )));
        }
        if let Some(ext) = uri.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
            && !extensions.contains(&ext)
        {
            extensions.push(ext);
        }
    }
    let mut types = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Types \
         xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n",
    );
    let mut sorted = extensions.clone();
    sorted.sort();
    for ext in &sorted {
        let ct = match ext.as_str() {
            "model" => "application/vnd.ms-package.3dmanufacturing-3dmodel+xml",
            "rels" => "application/vnd.openxmlformats-package.relationships+xml",
            "json" => "application/json",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            _ => "application/octet-stream",
        };
        let _ = writeln!(types, "\t<Default Extension=\"{}\" ContentType=\"{ct}\"/>", xml_escape(ext));
    }
    types.push_str("</Types>\n");
    let root_rels = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Relationships \
         xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n\t<Relationship \
         Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\" Target=\"/3D/3dmodel.model\" \
         Id=\"rel0\"/>\n</Relationships>\n";
    let mut entries = vec![
        ("3D/3dmodel.model".to_string(), model.into_bytes(), ZipMethod::Deflated),
        ("[Content_Types].xml".to_string(), types.into_bytes(), ZipMethod::Deflated),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec(), ZipMethod::Deflated),
    ];
    if !attachments.is_empty() {
        let mut rels = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Relationships \
             xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n",
        );
        for (i, (uri, blob)) in attachments.iter().enumerate() {
            let _ = writeln!(
                rels,
                "\t<Relationship Type=\"{RECIPE_RELATIONSHIP}\" Target=\"{}\" Id=\"rel{}\"/>",
                xml_escape(uri),
                i + 1
            );
            entries.push((uri.trim_start_matches('/').to_string(), blob.clone(), ZipMethod::Deflated));
        }
        rels.push_str("</Relationships>\n");
        entries.push(("3D/_rels/3dmodel.model.rels".to_string(), rels.into_bytes(), ZipMethod::Deflated));
    }
    write_zip(&entries)
}



pub fn write_3mf(
    path: &Path,
    vertices: &[Vec3],
    faces: &[Tri],
    unit_scale: f64,
    name: &str,
    attachments: &[(String, Vec<u8>)],
    metadata: &[(String, String)],
) -> Result<u64, MeshError> {
    write_file(path, &threemf_bytes(vertices, faces, unit_scale, name, attachments, metadata)?)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreeMfMesh {
    pub vertices: Vec<[f64; 3]>,
    pub triangles: Vec<[usize; 3]>,
    pub unit: String,
    pub parts: Vec<String>,
}



pub fn read_3mf(bytes: &[u8]) -> Result<ThreeMfMesh, MeshError> {
    let parts = read_zip(bytes)?;
    let names: Vec<String> = parts.iter().map(|(n, _)| n.clone()).collect();
    let model = parts
        .iter()
        .find(|(n, _)| n == "3D/3dmodel.model")
        .ok_or_else(|| MeshError::invalid("3MF package has no 3D/3dmodel.model part"))?;
    let text = std::str::from_utf8(&model.1).map_err(|_| MeshError::invalid("3MF model is not UTF-8"))?;
    let attr = |tag: &str, key: &str| -> Option<String> {
        let needle = format!("{key}=\"");
        let start = tag.find(&needle)? + needle.len();
        let end = tag[start..].find('"')? + start;
        Some(tag[start..end].to_string())
    };
    let unit =
        text.find("<model").and_then(|p| attr(&text[p..], "unit")).unwrap_or_else(|| "millimeter".into());
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    let bad = || MeshError::invalid("malformed 3MF vertex or triangle");
    for piece in text.split('<').skip(1) {
        if let Some(tag) = piece.strip_prefix("vertex ") {

            let g = |k| attr(tag, k).and_then(|s| s.parse::<f32>().ok()).map(f64::from).ok_or_else(bad);
            vertices.push([g("x")?, g("y")?, g("z")?]);
        } else if let Some(tag) = piece.strip_prefix("triangle ") {
            let g = |k| attr(tag, k).and_then(|s| s.parse::<usize>().ok()).ok_or_else(bad);
            triangles.push([g("v1")?, g("v2")?, g("v3")?]);
        }
    }
    Ok(ThreeMfMesh { vertices, triangles, unit, parts: names })
}

