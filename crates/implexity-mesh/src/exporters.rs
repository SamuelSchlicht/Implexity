// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use serde_json::{Map, Value, json};

use crate::MeshError;
use crate::topology::{Tri, Vec3};

pub struct BodyWrite<'a> {
    pub path: PathBuf,
    pub vertices: &'a [Vec3],
    pub faces: &'a [Tri],
    pub name: &'a str,
    pub stats: &'a Value,
    pub provenance: &'a Value,
    pub blobs: &'a [(String, Vec<u8>)],
    pub cap_face_mask: Option<&'a [bool]>,
    pub options: &'a Map<String, Value>,
    pub report: &'a mut Map<String, Value>,
}

pub type WriteFn = Arc<dyn Fn(&mut BodyWrite<'_>) -> Result<u64, MeshError> + Send + Sync>;
pub type AvailableFn = Arc<dyn Fn() -> (bool, String) + Send + Sync>;

#[derive(Clone)]
pub struct Format {
    pub name: String,
    pub extension: String,
    pub mime: String,
    pub write: WriteFn,
    pub requires: Option<String>,
    pub available: Option<AvailableFn>,
    pub doc: String,
    pub order: i64,
}

impl std::fmt::Debug for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Format {} -> .{}>", self.name, self.extension)
    }
}

impl Format {
    #[must_use]
    pub fn is_available(&self) -> (bool, String) {
        self.available.as_ref().map_or_else(|| (true, "standard library".to_string()), |f| f())
    }
}

fn registry() -> &'static RwLock<Vec<Format>> {
    static REG: OnceLock<RwLock<Vec<Format>>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(builtins()))
}

fn read() -> std::sync::RwLockReadGuard<'static, Vec<Format>> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner)
}



pub fn register(fmt: Format) -> Result<(), MeshError> {
    let mut reg = registry().write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(old) = reg.iter().find(|f| f.name == fmt.name) {
        return Err(MeshError::invalid(format!(
            "export format {} is already registered (writing .{}); pick another name -- a registered format is \
             never silently replaced",
            implexity_core::py_repr::repr_str(&fmt.name),
            old.extension
        )));
    }
    reg.push(fmt);
    reg.sort_by_key(|f| f.order);
    Ok(())
}



pub fn get(name: &str) -> Result<Format, MeshError> {
    read().iter().find(|f| f.name == name).cloned().ok_or_else(|| {
        MeshError::invalid(format!(
            "format {}; formats are {}",
            implexity_core::py_repr::repr_str(name),
            names_repr()
        ))
    })
}

#[must_use]
pub fn names() -> Vec<String> {
    read().iter().map(|f| f.name.clone()).collect()
}

#[must_use]
pub fn names_repr() -> String {
    let n = names();
    let items: Vec<String> = n.iter().map(|s| implexity_core::py_repr::repr_str(s)).collect();
    if items.len() == 1 { format!("({},)", items[0]) } else { format!("({})", items.join(", ")) }
}

#[must_use]
pub fn mime_types() -> BTreeMap<String, String> {
    read().iter().map(|f| (f.extension.clone(), f.mime.clone())).collect()
}

#[must_use]
pub fn catalogue() -> Vec<Value> {
    read()
        .iter()
        .map(|f| {
            let (ok, detail) = f.is_available();
            json!({"format": f.name, "extension": f.extension, "mime": f.mime, "requires": f.requires,
                   "available": ok, "detail": detail, "doc": f.doc})
        })
        .collect()
}

pub use implexity_core::pyobj::{py_str, truthy};

fn fixed5(v: &Value) -> Result<String, MeshError> {
    match v.as_f64() {
        Some(x) => Ok(crate::pyfmt::fmt_f(x, 5)),
        None => Err(MeshError::invalid(format!(
            "unsupported format string passed to {}.__format__",
            if v.is_null() { "NoneType" } else { "object" }
        ))),
    }
}

fn write_stl(w: &mut BodyWrite<'_>) -> Result<u64, MeshError> {
    crate::formats::write_stl(&w.path, w.vertices, w.faces, 1e3)
}

fn write_ply(w: &mut BodyWrite<'_>) -> Result<u64, MeshError> {
    let (p, st) = (w.provenance, w.stats);
    let comments = vec![
        format!("design_version {}", py_str(&p["design_version"])),
        format!("case_hash {}", py_str(&p["case_hash"])),
        format!("tolerance_achieved_mm {}", py_str(&p["tolerance_achieved_mm"])),
        format!("triangles {} watertight {}", py_str(&st["triangles"]), py_str(&st["watertight"])),
    ];
    crate::formats::write_ply(&w.path, w.vertices, w.faces, 1e3, &comments)
}

fn write_3mf(w: &mut BodyWrite<'_>) -> Result<u64, MeshError> {
    let prov = w.provenance;
    let mut blobs: Vec<(String, Vec<u8>)> = w.blobs.to_vec();
    let text =
        implexity_core::json::dumps(prov, &implexity_core::json::DumpOptions::indented(1).sorted(true));
    let key = "/Metadata/provenance.json".to_string();
    if let Some(slot) = blobs.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = text.into_bytes();
    } else {
        blobs.push((key, text.into_bytes()));
    }
    let description = format!(
        "watertight lattice body, design v{}, tolerance {} mm",
        py_str(&prov["design_version"]),
        fixed5(&prov["tolerance_achieved_mm"])?
    );
    let metadata = vec![
        ("Title".to_string(), w.name.to_string()),
        ("Designer".to_string(), "implexity".to_string()),
        ("Application".to_string(), "implexity bodyexport".to_string()),
        ("Description".to_string(), description),
    ];
    crate::formats::write_3mf(&w.path, w.vertices, w.faces, 1e3, w.name, &blobs, &metadata)
}

fn write_step(w: &mut BodyWrite<'_>) -> Result<u64, MeshError> {
    let description = format!(
        "implexity body, design v{}, chord {} mm, {} source triangles",
        py_str(&w.provenance["design_version"]),
        fixed5(&w.provenance["tolerance_achieved_mm"])?,
        py_str(&w.stats["triangles"])
    );
    let schema = w
        .options
        .get("step_schema")
        .and_then(Value::as_str)
        .unwrap_or(crate::step::DEFAULT_SCHEMA)
        .to_string();
    let merge = w.options.get("step_merge").is_none_or(truthy);
    let opts = crate::step::StepOptions {
        merge,
        schema: &schema,
        name: w.name,
        description: Some(&description),
        cap_mask: w.cap_face_mask,
        ..crate::step::StepOptions::default()
    };
    let rep = crate::step::mesh_to_step(w.vertices, w.faces, &w.path, &opts, None)?;
    let bytes = rep["write"]["bytes"].as_u64().unwrap_or(0);
    w.report.insert("step".into(), rep);
    Ok(bytes)
}

fn builtins() -> Vec<Format> {
    let f =
        |name: &str, ext: &str, mime: &str, write: WriteFn, order: i64, requires: Option<&str>, doc: &str| {
            Format {
                name: name.into(),
                extension: ext.into(),
                mime: mime.into(),
                write,
                requires: requires.map(Into::into),
                available: None,
                doc: doc.into(),
                order,
            }
        };
    let mut threemf = f(
        "3mf",
        "3mf",
        "model/3mf",
        Arc::new(write_3mf),
        30,
        None,
        "core 3MF <mesh> -- the mainstream-consumable kind, not the Volumetric/Implicit extension -- carrying the \
         recipe and the provenance manifest as attachments",
    );
    threemf.available = Some(Arc::new(|| (true, "native OPC/3MF writer (implexity-mesh)".to_string())));
    let mut step = f(
        "step",
        "step",
        "application/step",
        Arc::new(write_step),
        40,
        None,
        "ISO 10303 boundary-representation solid: planar advanced_face on polygonal wires, coplanar facets merged \
         and the merge guarded by a native validity check",
    );
    step.available = Some(Arc::new(|| {
        let (ok, detail) = crate::step::have_writer();
        (ok, detail.to_string())
    }));
    vec![
        f(
            "stl",
            "stl",
            "model/stl",
            Arc::new(write_stl),
            10,
            None,
            "binary STL, millimetres: triangles and nothing else",
        ),
        f(
            "ply",
            "ply",
            "application/octet-stream",
            Arc::new(write_ply),
            20,
            None,
            "binary little-endian PLY, millimetres, with the design version, case hash and achieved chord in the header \
             comments",
        ),
        threemf,
        step,
    ]
}
