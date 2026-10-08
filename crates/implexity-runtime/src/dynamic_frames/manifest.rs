// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use serde_json::{Map, Value, json};

use super::{DynamicError, DynamicResult, SCHEMA, is_identifier};

pub const MAX_GRID_CELLS: usize = 1 << 24;
pub const MAX_MESH_NODES: usize = 1 << 24;
pub const MAX_MESH_CELLS: usize = 1 << 26;
pub const MAX_MESHES: usize = 8;
pub const MAX_MESH_ATTRIBUTES: usize = 8;
pub const CELLS_SUFFIX: &str = "/cells";
pub const MAX_FIELDS: usize = 32;
pub const MAX_SERIES: usize = 64;
pub const MAX_PHASE_BINS: usize = 1024;
pub const MIN_BYTE_LIMIT: u64 = 64 * 1024;
pub const MAX_BYTE_LIMIT: u64 = 64 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Scalar,
    Vector,
    Displacement,
    Occupancy,
}

impl FieldKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Vector => "vector",
            Self::Displacement => "displacement",
            Self::Occupancy => "occupancy",
        }
    }



    pub fn parse(s: &str) -> DynamicResult<Self> {
        match s {
            "scalar" => Ok(Self::Scalar),
            "vector" => Ok(Self::Vector),
            "displacement" => Ok(Self::Displacement),
            "occupancy" => Ok(Self::Occupancy),
            other => Err(DynamicError::invalid(format!(
                "field kind must be scalar, vector, displacement or occupancy, not {other:?}"
            ))),
        }
    }

    #[must_use]
    pub const fn is_vector(self) -> bool {
        matches!(self, Self::Vector | Self::Displacement)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteHint {
    Sequential,
    Diverging,
    Cyclic,
}

impl PaletteHint {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Diverging => "diverging",
            Self::Cyclic => "cyclic",
        }
    }



    pub fn parse(s: &str) -> DynamicResult<Self> {
        match s {
            "sequential" => Ok(Self::Sequential),
            "diverging" => Ok(Self::Diverging),
            "cyclic" => Ok(Self::Cyclic),
            other => Err(DynamicError::invalid(format!(
                "palette hint must be sequential, diverging or cyclic, not {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    F32,
    F64,
}

impl Precision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "float32",
            Self::F64 => "float64",
        }
    }

    #[must_use]
    pub const fn width(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridSpec {
    pub name: String,
    pub shape: Vec<usize>,
    pub spacing: Vec<f64>,
    pub origin: Vec<f64>,
    pub unit: String,
}

impl GridSpec {
    #[must_use]
    pub fn dims(&self) -> usize {
        self.shape.len()
    }

    #[must_use]
    pub fn cells(&self) -> usize {
        self.shape.iter().product()
    }

    #[must_use]
    pub fn bounds(&self) -> (Vec<f64>, Vec<f64>) {
        let lo: Vec<f64> = (0..self.dims()).map(|a| self.origin[a] - 0.5 * self.spacing[a]).collect();
        let hi: Vec<f64> = (0..self.dims())
            .map(|a| self.origin[a] + (self.shape[a] as f64 - 0.5) * self.spacing[a])
            .collect();
        (lo, hi)
    }

    fn to_value(&self) -> Value {
        json!({"name": self.name, "shape": self.shape, "spacing": self.spacing,
               "origin": self.origin, "unit": self.unit})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldSpec {
    pub name: String,
    pub label: String,
    pub unit: String,
    pub grid: String,
    pub kind: FieldKind,
    pub palette: PaletteHint,
}

impl FieldSpec {
    fn to_value(&self) -> Value {
        json!({"name": self.name, "label": self.label, "unit": self.unit, "grid": self.grid,
               "kind": self.kind.as_str(), "palette": self.palette.as_str()})
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellShape {
    Triangle,
    Tetrahedron,
}

impl CellShape {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Triangle => "triangle",
            Self::Tetrahedron => "tetrahedron",
        }
    }



    pub fn parse(s: &str) -> DynamicResult<Self> {
        match s {
            "triangle" => Ok(Self::Triangle),
            "tetrahedron" => Ok(Self::Tetrahedron),
            other => Err(DynamicError::invalid(format!(
                "mesh cell must be triangle or tetrahedron, not {other:?}"
            ))),
        }
    }

    #[must_use]
    pub const fn dims(self) -> usize {
        match self {
            Self::Triangle => 2,
            Self::Tetrahedron => 3,
        }
    }

    #[must_use]
    pub const fn nodes_per_cell(self) -> usize {
        self.dims() + 1
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeshSpec {
    pub name: String,
    pub cell: CellShape,
    pub nodes: usize,
    pub cells: usize,
    pub unit: String,
    pub attributes: Vec<FieldSpec>,
}

impl MeshSpec {
    #[must_use]
    pub fn cell_location(&self) -> String {
        format!("{}{CELLS_SUFFIX}", self.name)
    }

    fn to_value(&self) -> Value {
        json!({"name": self.name, "cell": self.cell.as_str(), "nodes": self.nodes, "cells": self.cells,
               "unit": self.unit, "attributes": self.attributes.iter().map(FieldSpec::to_value).collect::<Vec<_>>()})
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Location<'a> {
    Grid(&'a GridSpec),
    Nodes(&'a MeshSpec),
    Cells(&'a MeshSpec),
}

impl Location<'_> {
    #[must_use]
    pub fn dims(&self) -> usize {
        match self {
            Self::Grid(g) => g.dims(),
            Self::Nodes(m) | Self::Cells(m) => m.cell.dims(),
        }
    }

    #[must_use]
    pub fn sites(&self) -> usize {
        match self {
            Self::Grid(g) => g.cells(),
            Self::Nodes(m) => m.nodes,
            Self::Cells(m) => m.cells,
        }
    }

    #[must_use]
    pub const fn mesh(&self) -> Option<&MeshSpec> {
        match self {
            Self::Grid(_) => None,
            Self::Nodes(m) | Self::Cells(m) => Some(m),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeriesSpec {
    pub name: String,
    pub label: String,
    pub unit: String,
    pub role: String,
}

impl SeriesSpec {
    fn to_value(&self) -> Value {
        json!({"name": self.name, "label": self.label, "unit": self.unit, "role": self.role})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimeBase {
    pub unit: String,
    pub step: f64,
    pub period: Option<f64>,
    pub phase_origin: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Retention {
    pub byte_limit: u64,
    pub phase_bins: usize,
    pub retain_segments: usize,
    pub segment_length: Option<f64>,
    pub precision: Precision,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            byte_limit: 256 << 20,
            phase_bins: 24,
            retain_segments: 4,
            segment_length: None,
            precision: Precision::F32,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameManifest {
    pub grids: Vec<GridSpec>,
    pub fields: Vec<FieldSpec>,
    pub series: Vec<SeriesSpec>,
    pub time: TimeBase,
    pub retention: Retention,
    pub provenance: Map<String, Value>,
    pub meshes: Vec<MeshSpec>,
}

fn string(map: &Map<String, Value>, key: &str, what: &str) -> DynamicResult<String> {
    match map.get(key) {
        Some(Value::String(s)) if s.chars().count() <= 256 => Ok(s.clone()),
        Some(Value::String(_)) => {
            Err(DynamicError::invalid(format!("{what}.{key} is longer than 256 characters")))
        }
        Some(_) => Err(DynamicError::invalid(format!("{what}.{key} must be a string"))),
        None => Err(DynamicError::invalid(format!("{what}.{key} is required"))),
    }
}

fn opt_string(map: &Map<String, Value>, key: &str, what: &str, default: &str) -> DynamicResult<String> {
    if map.contains_key(key) { string(map, key, what) } else { Ok(default.to_owned()) }
}

fn identifier(map: &Map<String, Value>, key: &str, what: &str) -> DynamicResult<String> {
    let s = string(map, key, what)?;
    if !is_identifier(&s) {
        return Err(DynamicError::invalid(format!(
            "{what}.{key} must be 1-64 characters of [A-Za-z0-9_.:-] starting with a letter or digit"
        )));
    }
    Ok(s)
}

fn finite(v: Option<&Value>, what: &str) -> DynamicResult<f64> {
    match v.and_then(Value::as_f64) {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(DynamicError::invalid(format!("{what} must be a finite number"))),
    }
}

fn count(v: Option<&Value>, what: &str, lo: u64, hi: u64) -> DynamicResult<u64> {
    match v.and_then(Value::as_u64) {
        Some(x) if (lo..=hi).contains(&x) => Ok(x),
        _ => Err(DynamicError::invalid(format!("{what} must be an integer in [{lo}, {hi}]"))),
    }
}

fn count_usize(v: Option<&Value>, what: &str, lo: usize, hi: usize) -> DynamicResult<usize> {
    match v.and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()) {
        Some(x) if (lo..=hi).contains(&x) => Ok(x),
        _ => Err(DynamicError::invalid(format!("{what} must be an integer in [{lo}, {hi}]"))),
    }
}

fn object<'a>(v: &'a Value, what: &str, allowed: &[&str]) -> DynamicResult<&'a Map<String, Value>> {
    let Some(map) = v.as_object() else {
        return Err(DynamicError::invalid(format!("{what} must be an object")));
    };
    if let Some(k) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(DynamicError::invalid(format!("{what} has the unknown key {k:?}")));
    }
    Ok(map)
}

fn array<'a>(v: Option<&'a Value>, what: &str) -> DynamicResult<&'a [Value]> {
    v.and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| DynamicError::invalid(format!("{what} must be an array")))
}

fn field_spec(f: &Value, what: &str) -> DynamicResult<FieldSpec> {
    let f = object(f, what, &["name", "label", "unit", "grid", "kind", "palette"])?;
    let name = identifier(f, "name", what)?;
    let grid = string(f, "grid", what)?;
    if !is_identifier(grid.strip_suffix(CELLS_SUFFIX).unwrap_or(&grid)) {
        return Err(DynamicError::invalid(format!(
            "{what}.grid must name a grid, a mesh or the cells of a mesh (<mesh>{CELLS_SUFFIX})"
        )));
    }
    Ok(FieldSpec {
        label: opt_string(f, "label", what, &name)?,
        name,
        unit: opt_string(f, "unit", what, "1")?,
        grid,
        kind: FieldKind::parse(&opt_string(f, "kind", what, "scalar")?)?,
        palette: PaletteHint::parse(&opt_string(f, "palette", what, "sequential")?)?,
    })
}

impl FrameManifest {


    #[allow(clippy::too_many_lines)]
    pub fn validate(&self) -> DynamicResult<()> {
        if self.grids.is_empty() && self.meshes.is_empty() && !self.fields.is_empty() {
            return Err(DynamicError::invalid("a store with fields needs at least one grid or mesh"));
        }
        if self.meshes.len() > MAX_MESHES {
            return Err(DynamicError::invalid(format!("a store holds at most {MAX_MESHES} meshes")));
        }
        if self.fields.len() > MAX_FIELDS || self.series.len() > MAX_SERIES {
            return Err(DynamicError::invalid(format!(
                "a store holds at most {MAX_FIELDS} fields and {MAX_SERIES} series"
            )));
        }
        if self.fields.is_empty() && self.series.is_empty() {
            return Err(DynamicError::invalid("a store needs at least one field or series"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for g in &self.grids {
            if !is_identifier(&g.name) || !seen.insert(format!("grid:{}", g.name)) {
                return Err(DynamicError::invalid(format!("grid name {:?} is invalid or repeated", g.name)));
            }
            if !(2..=3).contains(&g.dims()) || g.spacing.len() != g.dims() || g.origin.len() != g.dims() {
                return Err(DynamicError::invalid(format!(
                    "grid {} needs 2 or 3 axes with one spacing and one origin per axis",
                    g.name
                )));
            }
            if g.shape.contains(&0)
                || g.shape
                    .iter()
                    .try_fold(1usize, |a, &n| a.checked_mul(n))
                    .is_none_or(|c| c > MAX_GRID_CELLS)
            {
                return Err(DynamicError::invalid(format!(
                    "grid {} must have 1..{MAX_GRID_CELLS} cells and no empty axis",
                    g.name
                )));
            }
            if g.spacing.iter().any(|s| !(s.is_finite() && *s > 0.0))
                || g.origin.iter().any(|o| !o.is_finite())
            {
                return Err(DynamicError::invalid(format!(
                    "grid {} needs positive finite spacings and a finite origin",
                    g.name
                )));
            }
        }
        for m in &self.meshes {
            if !is_identifier(&m.name) || !seen.insert(format!("grid:{}", m.name)) {
                return Err(DynamicError::invalid(format!(
                    "mesh name {:?} is invalid or repeats a grid or mesh",
                    m.name
                )));
            }
            if !(m.cell.nodes_per_cell()..=MAX_MESH_NODES).contains(&m.nodes)
                || !(1..=MAX_MESH_CELLS).contains(&m.cells)
            {
                return Err(DynamicError::invalid(format!(
                    "mesh {} needs {}..{MAX_MESH_NODES} nodes and 1..{MAX_MESH_CELLS} cells",
                    m.name,
                    m.cell.nodes_per_cell()
                )));
            }
            if m.attributes.len() > MAX_MESH_ATTRIBUTES {
                return Err(DynamicError::invalid(format!(
                    "mesh {} has more than {MAX_MESH_ATTRIBUTES} attributes",
                    m.name
                )));
            }
        }
        for f in &self.fields {
            if !is_identifier(&f.name) || !seen.insert(format!("value:{}", f.name)) {
                return Err(DynamicError::invalid(format!("field name {:?} is invalid or repeated", f.name)));
            }
            if self.location(&f.grid).is_none() {
                return Err(DynamicError::invalid(format!(
                    "field {} names the undeclared grid {}",
                    f.name, f.grid
                )));
            }
        }
        for m in &self.meshes {
            for a in &m.attributes {
                if !is_identifier(&a.name) || !seen.insert(format!("value:{}", a.name)) {
                    return Err(DynamicError::invalid(format!(
                        "mesh attribute name {:?} is invalid or repeats a field",
                        a.name
                    )));
                }
                if a.grid != m.name && a.grid != m.cell_location() {
                    return Err(DynamicError::invalid(format!(
                        "attribute {} of mesh {} must live on {} or {}",
                        a.name,
                        m.name,
                        m.name,
                        m.cell_location()
                    )));
                }
                if a.kind.is_vector() {
                    return Err(DynamicError::invalid(format!(
                        "mesh attribute {} must be a scalar or an occupancy",
                        a.name
                    )));
                }
            }
        }
        for s in &self.series {
            if !is_identifier(&s.name) || !seen.insert(format!("value:{}", s.name)) {
                return Err(DynamicError::invalid(format!(
                    "series name {:?} is invalid or repeats a field or series",
                    s.name
                )));
            }
        }
        let t = &self.time;
        if !(t.step.is_finite() && t.step > 0.0 && t.phase_origin.is_finite()) {
            return Err(DynamicError::invalid("time.step must be positive and time.phase_origin finite"));
        }
        if t.period.is_some_and(|p| !(p.is_finite() && p > 0.0)) {
            return Err(DynamicError::invalid("time.period must be positive when given"));
        }
        let r = &self.retention;
        if !(MIN_BYTE_LIMIT..=MAX_BYTE_LIMIT).contains(&r.byte_limit) {
            return Err(DynamicError::invalid(format!(
                "retention.byte_limit must be in [{MIN_BYTE_LIMIT}, {MAX_BYTE_LIMIT}]"
            )));
        }
        if r.phase_bins > MAX_PHASE_BINS || !(1..=100_000).contains(&r.retain_segments) {
            return Err(DynamicError::invalid(format!(
                "retention.phase_bins must be at most {MAX_PHASE_BINS} and retention.retain_segments in [1, 100000]"
            )));
        }
        if r.segment_length.is_some_and(|s| !(s.is_finite() && s > 0.0)) {
            return Err(DynamicError::invalid("retention.segment_length must be positive when given"));
        }
        let text =
            serde_json::to_string(&self.provenance).map_err(|e| DynamicError::invalid(e.to_string()))?;
        if text.len() > 16 * 1024 {
            return Err(DynamicError::invalid("provenance is larger than 16 KiB"));
        }
        Ok(())
    }

    #[must_use]
    pub fn grid(&self, name: &str) -> Option<&GridSpec> {
        self.grids.iter().find(|g| g.name == name)
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Option<(usize, &FieldSpec)> {
        self.fields.iter().enumerate().find(|(_, f)| f.name == name)
    }

    #[must_use]
    pub fn field_grid(&self, index: usize) -> Option<&GridSpec> {
        self.fields.get(index).and_then(|f| self.grid(&f.grid))
    }

    #[must_use]
    pub fn mesh(&self, name: &str) -> Option<&MeshSpec> {
        self.meshes.iter().find(|m| m.name == name)
    }

    #[must_use]
    pub fn location(&self, name: &str) -> Option<Location<'_>> {
        if let Some(g) = self.grid(name) {
            return Some(Location::Grid(g));
        }
        if let Some(m) = self.mesh(name) {
            return Some(Location::Nodes(m));
        }
        name.strip_suffix(CELLS_SUFFIX).and_then(|m| self.mesh(m)).map(Location::Cells)
    }

    #[must_use]
    pub fn field_location(&self, index: usize) -> Option<Location<'_>> {
        self.fields.get(index).and_then(|f| self.location(&f.grid))
    }

    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<(&MeshSpec, &FieldSpec)> {
        self.meshes.iter().find_map(|m| m.attributes.iter().find(|a| a.name == name).map(|a| (m, a)))
    }

    #[must_use]
    pub fn components(&self, index: usize) -> usize {
        match (self.fields.get(index), self.field_location(index)) {
            (Some(f), Some(l)) if f.kind.is_vector() => l.dims(),
            _ => 1,
        }
    }

    #[must_use]
    pub fn values_per_frame(&self, index: usize) -> usize {
        self.field_location(index).map_or(0, |l| l.sites()) * self.components(index)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let r = &self.retention;
        let mut v = json!({
            "schema": SCHEMA,
            "extension": super::EXTENSION,
            "grids": self.grids.iter().map(GridSpec::to_value).collect::<Vec<_>>(),
            "fields": self.fields.iter().map(FieldSpec::to_value).collect::<Vec<_>>(),
            "series": self.series.iter().map(SeriesSpec::to_value).collect::<Vec<_>>(),
            "time": {"unit": self.time.unit, "step": self.time.step, "period": self.time.period,
                     "phase_origin": self.time.phase_origin},
            "retention": {"byte_limit": r.byte_limit, "phase_bins": r.phase_bins,
                          "retain_segments": r.retain_segments, "segment_length": r.segment_length,
                          "precision": r.precision.as_str()},
            "provenance": self.provenance,
        });
        if !self.meshes.is_empty() {
            v["meshes"] = json!(self.meshes.iter().map(MeshSpec::to_value).collect::<Vec<_>>());
        }
        v
    }



    #[allow(clippy::too_many_lines)]
    pub fn from_value(v: &Value) -> DynamicResult<Self> {
        let m = object(
            v,
            "manifest",
            &[
                "schema",
                "extension",
                "grids",
                "fields",
                "series",
                "time",
                "retention",
                "provenance",
                "meshes",
            ],
        )?;
        if m.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
            return Err(DynamicError::invalid(format!("manifest.schema must be {SCHEMA}")));
        }
        if m.get("extension").is_some_and(|e| e.as_str() != Some(super::EXTENSION)) {
            return Err(DynamicError::invalid(format!("manifest.extension must be {}", super::EXTENSION)));
        }
        let mut grids = Vec::new();
        for (i, g) in array(m.get("grids").or(Some(&json!([]))), "manifest.grids")?.iter().enumerate() {
            let what = format!("grids[{i}]");
            let g = object(g, &what, &["name", "shape", "spacing", "origin", "unit"])?;
            let shape = array(g.get("shape"), &format!("{what}.shape"))?
                .iter()
                .map(|n| count_usize(Some(n), &format!("{what}.shape[]"), 1, MAX_GRID_CELLS))
                .collect::<DynamicResult<Vec<_>>>()?;
            let nums = |key: &str| -> DynamicResult<Vec<f64>> {
                array(g.get(key), &format!("{what}.{key}"))?
                    .iter()
                    .map(|x| finite(Some(x), &format!("{what}.{key}[]")))
                    .collect()
            };
            grids.push(GridSpec {
                name: identifier(g, "name", &what)?,
                spacing: nums("spacing")?,
                origin: nums("origin")?,
                shape,
                unit: opt_string(g, "unit", &what, "1")?,
            });
        }
        let mut fields = Vec::new();
        for (i, f) in array(m.get("fields").or(Some(&json!([]))), "manifest.fields")?.iter().enumerate() {
            fields.push(field_spec(f, &format!("fields[{i}]"))?);
        }
        let mut meshes = Vec::new();
        for (i, x) in array(m.get("meshes").or(Some(&json!([]))), "manifest.meshes")?.iter().enumerate() {
            let what = format!("meshes[{i}]");
            let x = object(x, &what, &["name", "cell", "nodes", "cells", "unit", "attributes"])?;
            let mut attributes = Vec::new();
            for (j, a) in array(x.get("attributes").or(Some(&json!([]))), &format!("{what}.attributes"))?
                .iter()
                .enumerate()
            {
                attributes.push(field_spec(a, &format!("{what}.attributes[{j}]"))?);
            }
            meshes.push(MeshSpec {
                name: identifier(x, "name", &what)?,
                cell: CellShape::parse(&string(x, "cell", &what)?)?,
                nodes: count_usize(x.get("nodes"), &format!("{what}.nodes"), 1, MAX_MESH_NODES)?,
                cells: count_usize(x.get("cells"), &format!("{what}.cells"), 1, MAX_MESH_CELLS)?,
                unit: opt_string(x, "unit", &what, "1")?,
                attributes,
            });
        }
        let mut series = Vec::new();
        for (i, s) in array(m.get("series").or(Some(&json!([]))), "manifest.series")?.iter().enumerate() {
            let what = format!("series[{i}]");
            let s = object(s, &what, &["name", "label", "unit", "role"])?;
            let name = identifier(s, "name", &what)?;
            series.push(SeriesSpec {
                label: opt_string(s, "label", &what, &name)?,
                name,
                unit: opt_string(s, "unit", &what, "1")?,
                role: opt_string(s, "role", &what, "response")?,
            });
        }
        let t = object(
            m.get("time").unwrap_or(&Value::Null),
            "manifest.time",
            &["unit", "step", "period", "phase_origin"],
        )?;
        let time = TimeBase {
            unit: opt_string(t, "unit", "time", "s")?,
            step: finite(t.get("step"), "time.step")?,
            period: match t.get("period") {
                None | Some(Value::Null) => None,
                p => Some(finite(p, "time.period")?),
            },
            phase_origin: if t.contains_key("phase_origin") {
                finite(t.get("phase_origin"), "time.phase_origin")?
            } else {
                0.0
            },
        };
        let d = Retention::default();
        let retention = match m.get("retention") {
            None => d,
            Some(r) => {
                let r = object(
                    r,
                    "manifest.retention",
                    &["byte_limit", "phase_bins", "retain_segments", "segment_length", "precision"],
                )?;
                Retention {
                    byte_limit: if r.contains_key("byte_limit") {
                        count(r.get("byte_limit"), "retention.byte_limit", MIN_BYTE_LIMIT, MAX_BYTE_LIMIT)?
                    } else {
                        d.byte_limit
                    },
                    phase_bins: if r.contains_key("phase_bins") {
                        count_usize(r.get("phase_bins"), "retention.phase_bins", 0, MAX_PHASE_BINS)?
                    } else {
                        d.phase_bins
                    },
                    retain_segments: if r.contains_key("retain_segments") {
                        count_usize(r.get("retain_segments"), "retention.retain_segments", 1, 100_000)?
                    } else {
                        d.retain_segments
                    },
                    segment_length: match r.get("segment_length") {
                        None | Some(Value::Null) => None,
                        s => Some(finite(s, "retention.segment_length")?),
                    },
                    precision: match r.get("precision").map(|p| p.as_str()) {
                        None | Some(Some("float32")) => Precision::F32,
                        Some(Some("float64")) => Precision::F64,
                        _ => {
                            return Err(DynamicError::invalid(
                                "retention.precision must be float32 or float64",
                            ));
                        }
                    },
                }
            }
        };
        let provenance = match m.get("provenance") {
            None => Map::new(),
            Some(Value::Object(p)) => p.clone(),
            Some(_) => return Err(DynamicError::invalid("manifest.provenance must be an object")),
        };
        let out = Self { grids, fields, series, time, retention, provenance, meshes };
        out.validate()?;
        Ok(out)
    }
}

