// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_mesh::raster::RgbImage;
use implexity_render::dynamic::colormap::{Colormap, Scale};
use implexity_render::dynamic::flow::StreamStyle;
use implexity_render::dynamic::grid::{Derived, GridField, Reduce};
use implexity_render::dynamic::plane::{
    BodyLayer, FlowOverlay, FrameSize, FrameText, Layers, SolidLayer, Theme, Window, render_frame,
};
use implexity_render::dynamic::solid::{Body, Sites, Topology, Triangles};
use implexity_render::dynamic::volume::{Backdrop, BodySurface, MAX_TRIANGLES, VolumeView, render_volume};
use implexity_runtime::dynamic_frames::manifest::{FieldKind, FrameManifest, GridSpec, Location, MeshSpec};
use implexity_runtime::dynamic_frames::store::{DynamicStore, FrameEntry, MeshData};
use serde_json::{Map, Value, json};

use crate::error::{AgentError, AgentResult};

fn contract(e: impl std::fmt::Display) -> AgentError {
    AgentError::contract(e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Section,
    Iso,
    Kymograph,
    PhaseAverage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    pub field: String,
    pub lic: bool,
    pub separation: f64,
    pub length: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deformation {
    pub field: String,
    pub scale: f64,
    pub mask: Option<String>,
    pub mask_threshold: f64,
    pub colour: Option<String>,
    pub grid_every: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub field: String,
    pub component: Option<usize>,
    pub mode: Mode,
    pub axis: usize,
    pub position: f64,
    pub colormap: Option<Colormap>,
    pub value_range: Option<(f64, f64)>,
    pub range_frames: bool,
    pub occupancy: Option<String>,
    pub fill_occupancy: bool,
    pub flow: Option<Flow>,
    pub deformation: Option<Deformation>,
    pub iso_field: Option<String>,
    pub iso_value: f64,
    pub clip: Option<([f64; 3], [f64; 3])>,
    pub camera: String,
    pub line: Option<(Vec<f64>, Vec<f64>)>,
    pub samples: usize,
    pub phase_bins: usize,
    pub width: usize,
    pub height: usize,
    pub theme: Theme,
}

fn text<'a>(p: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

fn f(p: &Map<String, Value>, k: &str) -> Option<f64> {
    p.get(k).and_then(Value::as_f64)
}

fn vec3(v: Option<&Value>) -> Option<[f64; 3]> {
    let a = v?.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?])
}

fn nums(v: Option<&Value>) -> Option<Vec<f64>> {
    v?.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect())
}

impl View {


    pub fn from_payload(p: &Map<String, Value>, default_size: (usize, usize)) -> AgentResult<Self> {
        let field = text(p, "field").ok_or_else(|| AgentError::contract("field is required"))?.to_owned();
        let component = match text(p, "component") {
            Some("x") => Some(0),
            Some("y") => Some(1),
            Some("z") => Some(2),
            _ => None,
        };
        let mode = match text(p, "mode") {
            Some("iso") => Mode::Iso,
            Some("kymograph") => Mode::Kymograph,
            Some("phase_average") => Mode::PhaseAverage,
            _ => Mode::Section,
        };
        let axis = match text(p, "plane") {
            Some("x") => 0,
            Some("y") => 1,
            _ => 2,
        };
        let colormap = match text(p, "colormap") {
            None | Some("auto") => None,
            Some(name) => Some(Colormap::parse(name).map_err(contract)?),
        };
        let value_range = p.get("value_range").and_then(|v| {
            let a = v.as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
        });
        if value_range.is_some_and(|(lo, hi)| hi <= lo) {
            return Err(AgentError::contract("value_range must be increasing"));
        }
        let flow = p.get("flow").and_then(Value::as_object).map(|m| Flow {
            field: text(m, "field").unwrap_or_default().to_owned(),
            lic: text(m, "style") == Some("lic"),
            separation: f(m, "separation_px").unwrap_or(14.0),
            length: f(m, "length_px").unwrap_or(12.0),
        });
        let deformation = p.get("deformation").and_then(Value::as_object).map(|m| Deformation {
            field: text(m, "field").unwrap_or_default().to_owned(),
            scale: f(m, "scale").unwrap_or(1.0),
            mask: text(m, "mask").map(str::to_owned),
            mask_threshold: f(m, "mask_threshold").unwrap_or(0.5),
            colour: text(m, "colour").map(str::to_owned),
            grid_every: m
                .get("grid_every")
                .and_then(Value::as_u64)
                .and_then(|x| usize::try_from(x).ok())
                .unwrap_or(0),
        });
        let iso = p.get("iso").and_then(Value::as_object).cloned().unwrap_or_default();
        let clip = iso
            .get("clip")
            .and_then(Value::as_object)
            .and_then(|c| Some((vec3(c.get("point"))?, vec3(c.get("normal"))?)));
        if clip.is_some_and(|(_, n)| n.iter().map(|x| x * x).sum::<f64>() < 1e-24) {
            return Err(AgentError::contract("the clip normal must not vanish"));
        }
        let kymo = p.get("kymograph").and_then(Value::as_object).cloned().unwrap_or_default();
        let line = match (nums(kymo.get("from")), nums(kymo.get("to"))) {
            (Some(a), Some(b)) if a.len() == b.len() => Some((a, b)),
            (None, None) => None,
            _ => {
                return Err(AgentError::contract(
                    "kymograph from and to need the same number of coordinates",
                ));
            }
        };
        let size = |k: &str, d: usize| {
            p.get(k).and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(d)
        };
        Ok(Self {
            field,
            component,
            mode,
            axis,
            position: f(p, "position").unwrap_or(0.5),
            colormap,
            value_range,
            range_frames: text(p, "range") == Some("frames"),
            occupancy: text(p, "occupancy").map(str::to_owned),
            fill_occupancy: p.get("fill_occupancy").and_then(Value::as_bool).unwrap_or(false),
            flow,
            deformation,
            iso_field: text(&iso, "field").map(str::to_owned),
            iso_value: f(&iso, "value").unwrap_or(0.5),
            clip,
            camera: text(&iso, "camera").unwrap_or("iso").to_owned(),
            line,
            samples: kymo
                .get("samples")
                .and_then(Value::as_u64)
                .and_then(|x| usize::try_from(x).ok())
                .unwrap_or(256),
            phase_bins: p
                .get("phase_bins")
                .and_then(Value::as_u64)
                .and_then(|x| usize::try_from(x).ok())
                .unwrap_or(12),
            width: size("width_px", default_size.0),
            height: size("height_px", default_size.1),
            theme: Theme::parse(text(p, "background").unwrap_or("dark")).map_err(contract)?,
        })
    }

    #[must_use]
    pub fn record(&self) -> Value {
        const AXES: [&str; 3] = ["x", "y", "z"];
        let component = self.component.map_or("magnitude_or_scalar", |c| AXES[c.min(2)]);
        let plane = AXES[self.axis.min(2)];
        let mode = match self.mode {
            Mode::Section => "section",
            Mode::Iso => "iso",
            Mode::Kymograph => "kymograph",
            Mode::PhaseAverage => "phase_average",
        };
        json!({
            "field": self.field,
            "component": component,
            "mode": mode,
            "plane": plane, "position": self.position,
            "occupancy": self.occupancy, "fill_occupancy": self.fill_occupancy,
            "flow": self.flow.as_ref().map(|f| json!({"field": f.field, "style": if f.lic { "lic" } else { "streamlines" }})),
            "deformation": self.deformation.as_ref().map(|d| json!({"field": d.field, "scale": d.scale, "mask": d.mask, "mask_threshold": d.mask_threshold, "colour": d.colour})),
            "width_px": self.width, "height_px": self.height,
            "background": match self.theme { Theme::Dark => "dark", Theme::White => "white" },
        })
    }
}

pub trait FrameValues {


    fn values(&self, name: &str) -> AgentResult<Vec<f64>>;
}

pub struct Stored<'a> {
    pub store: &'a DynamicStore,
    pub frame: &'a FrameEntry,
}

impl FrameValues for Stored<'_> {
    fn values(&self, name: &str) -> AgentResult<Vec<f64>> {
        self.store.read_named(self.frame, name).map_err(|e| AgentError::failed(e.to_string()))
    }
}

pub struct Precomputed(pub std::collections::BTreeMap<String, Vec<f64>>);

impl FrameValues for Precomputed {
    fn values(&self, name: &str) -> AgentResult<Vec<f64>> {
        self.0.get(name).cloned().ok_or_else(|| AgentError::failed(format!("no averaged values of {name}")))
    }
}

fn grid_field(grid: &GridSpec, components: usize, values: Vec<f64>) -> AgentResult<GridField> {
    GridField::new(grid.shape.clone(), grid.spacing.clone(), grid.origin.clone(), components, values)
        .map_err(contract)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    Grid(GridSpec),
    Nodes(MeshSpec),
    Cells(MeshSpec),
}

impl Place {
    fn of(l: Location<'_>) -> Self {
        match l {
            Location::Grid(g) => Self::Grid(g.clone()),
            Location::Nodes(m) => Self::Nodes(m.clone()),
            Location::Cells(m) => Self::Cells(m.clone()),
        }
    }

    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Grid(g) => g.name.clone(),
            Self::Nodes(m) => m.name.clone(),
            Self::Cells(m) => m.cell_location(),
        }
    }

    #[must_use]
    pub fn dims(&self) -> usize {
        match self {
            Self::Grid(g) => g.dims(),
            Self::Nodes(m) | Self::Cells(m) => m.cell.dims(),
        }
    }

    #[must_use]
    pub const fn grid(&self) -> Option<&GridSpec> {
        match self {
            Self::Grid(g) => Some(g),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub stored: String,
    pub derived: Option<Derived>,
    pub kind: FieldKind,
    pub components: usize,
    pub stored_components: usize,
    pub place: Place,
    pub label: String,
    pub unit: String,
    pub palette: &'static str,
}



pub fn resolve(m: &FrameManifest, name: &str) -> AgentResult<Resolved> {
    let unknown = || {
        let attributes = m.meshes.iter().flat_map(|x| x.attributes.iter().map(|a| a.name.as_str()));
        AgentError::contract(format!(
            "the store has no field {name:?}; it has {} (and curl:/q_criterion:/divergence: of its vector fields on grids)",
            m.fields.iter().map(|f| f.name.as_str()).chain(attributes).collect::<Vec<_>>().join(", ")
        ))
    };
    if let Some((i, spec)) = m.field(name) {
        let place =
            m.field_location(i).map(Place::of).ok_or_else(|| AgentError::failed("field location missing"))?;
        let c = m.components(i);
        return Ok(Resolved {
            stored: name.to_owned(),
            derived: None,
            kind: spec.kind,
            components: c,
            stored_components: c,
            place,
            label: spec.label.clone(),
            unit: spec.unit.clone(),
            palette: spec.palette.as_str(),
        });
    }
    if let Some((_, spec)) = m.attribute(name) {
        let place = m
            .location(&spec.grid)
            .map(Place::of)
            .ok_or_else(|| AgentError::failed("attribute location missing"))?;
        return Ok(Resolved {
            stored: name.to_owned(),
            derived: None,
            kind: spec.kind,
            components: 1,
            stored_components: 1,
            place,
            label: spec.label.clone(),
            unit: spec.unit.clone(),
            palette: spec.palette.as_str(),
        });
    }
    let (op, base) = name.split_once(':').ok_or_else(unknown)?;
    let op = Derived::parse(op).ok_or_else(unknown)?;
    let (i, spec) = m.field(base).ok_or_else(unknown)?;
    let Some(grid) = m.field_grid(i).cloned() else {
        return Err(AgentError::contract(format!(
            "{} needs a vector field on a grid; {base} lives on a mesh",
            op.name()
        )));
    };
    if !spec.kind.is_vector() {
        return Err(AgentError::contract(format!(
            "{} needs a vector field; {base} is {}",
            op.name(),
            spec.kind.as_str()
        )));
    }
    let dims = grid.dims();
    let vector_curl = op == Derived::Curl && dims == 3;
    Ok(Resolved {
        stored: base.to_owned(),
        derived: Some(op),
        kind: if vector_curl { FieldKind::Vector } else { FieldKind::Scalar },
        components: if vector_curl { 3 } else { 1 },
        stored_components: dims,
        label: format!("{} of {}", op.name().replace('_', " "), spec.label),
        unit: match op {
            Derived::QCriterion => format!("({})2/({})2", spec.unit, grid.unit),
            _ => format!("{}/{}", spec.unit, grid.unit),
        },
        palette: if vector_curl { "sequential" } else { "diverging" },
        place: Place::Grid(grid),
    })
}

#[must_use]
pub fn is_cell_grid(nodes: &GridSpec, cells: &GridSpec) -> bool {
    nodes.dims() == cells.dims()
        && (0..nodes.dims()).all(|a| {
            let h = nodes.spacing[a];
            cells.shape[a] + 1 == nodes.shape[a]
                && (cells.spacing[a] - h).abs() <= 1e-9 * h.abs()
                && (cells.origin[a] - (nodes.origin[a] + 0.5 * h)).abs() <= 1e-6 * h.abs()
        })
}

fn body_sites(reference: &Place, place: &Place) -> Option<bool> {
    match (reference, place) {
        (a, b) if a == b => Some(false),
        (Place::Grid(n), Place::Grid(c)) if is_cell_grid(n, c) => Some(true),
        (Place::Nodes(m), Place::Cells(c)) if m.name == c.name => Some(true),
        _ => None,
    }
}



pub fn with_defaults(m: &FrameManifest, mut view: View) -> AgentResult<View> {
    if view.mode == Mode::Kymograph {
        return Ok(view);
    }
    let main = resolve(m, &view.field)?;
    if view.deformation.is_none()
        && let Place::Nodes(mesh) | Place::Cells(mesh) = &main.place
    {
        let field = m
            .fields
            .iter()
            .find(|f| f.kind == FieldKind::Displacement && f.grid == mesh.name)
            .map(|f| f.name.clone());
        view.deformation = Some(Deformation {
            scale: if field.is_some() { 1.0 } else { 0.0 },
            field: field.unwrap_or_default(),
            mask: None,
            mask_threshold: 0.5,
            colour: None,
            grid_every: 0,
        });
    }
    if let Some(d) = view.deformation.as_mut()
        && d.mask.is_none()
    {
        let reference = if d.field.is_empty() {
            match &main.place {
                Place::Cells(mesh) => Some(Place::Nodes(mesh.clone())),
                other => Some(other.clone()),
            }
        } else {
            resolve(m, &d.field).ok().map(|r| r.place)
        };
        if let Some(reference) = reference {
            let candidates = m
                .fields
                .iter()
                .chain(m.meshes.iter().flat_map(|x| x.attributes.iter()))
                .filter(|f| f.kind == FieldKind::Occupancy);
            let mut best: Option<(bool, String)> = None;
            for f in candidates {
                let Some(place) = m.location(&f.grid).map(Place::of) else { continue };
                let on_mesh = matches!(reference, Place::Nodes(_));
                match body_sites(&reference, &place) {
                    Some(true) if best.as_ref().is_none_or(|b| !b.0) => best = Some((true, f.name.clone())),
                    Some(false) if on_mesh && best.is_none() => best = Some((false, f.name.clone())),
                    _ => {}
                }
            }
            d.mask = best.map(|b| b.1);
        }
    }
    Ok(view)
}

struct BodySpec {
    displacement: Option<Resolved>,
    scale: f64,
    reference: Place,
    mesh: Option<MeshData>,
    mask: Option<(String, bool)>,
    threshold: f64,
    colour: Option<(String, bool, Reduce, usize, Scale)>,
    colour_label: String,
}

impl BodySpec {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        let mut lo = [0.0; 3];
        let mut hi = [0.0; 3];
        match (&self.reference, &self.mesh) {
            (Place::Grid(g), _) => {
                for a in 0..g.dims() {
                    lo[a] = g.origin[a];
                    hi[a] = g.origin[a] + (g.shape[a] as f64 - 1.0) * g.spacing[a];
                }
            }
            (_, Some(mesh)) => {
                let d = self.reference.dims();
                lo = [f64::INFINITY; 3];
                hi = [f64::NEG_INFINITY; 3];
                for p in mesh.points.chunks_exact(d) {
                    for a in 0..d {
                        lo[a] = lo[a].min(p[a]);
                        hi[a] = hi[a].max(p[a]);
                    }
                }
                for a in d..3 {
                    lo[a] = 0.0;
                    hi[a] = 0.0;
                }
            }
            _ => {}
        }
        (lo, hi)
    }
}

fn sites(x: &(Vec<f64>, bool)) -> Sites<'_> {
    if x.1 { Sites::Cells(&x.0) } else { Sites::Nodes(&x.0) }
}

fn reduce_values(values: Vec<f64>, components: usize, reduce: Reduce) -> Vec<f64> {
    if components <= 1 {
        return values;
    }
    values
        .chunks_exact(components)
        .map(|c| match reduce {
            Reduce::Magnitude => c.iter().map(|x| x * x).sum::<f64>().sqrt(),
            Reduce::Component(k) => c.get(k).copied().unwrap_or(f64::NAN),
        })
        .collect()
}

pub struct Renderer<'a> {
    store: &'a DynamicStore,
    view: View,
    reduce: Reduce,
    scale: Scale,
    window: Option<Window>,
    slice_at: Option<f64>,
    label: String,
    unit: String,
    store_id: String,
    iso_field: Option<String>,
    range_source: &'static str,
    main: Resolved,
    body: Option<BodySpec>,
}

impl<'a> Renderer<'a> {
    fn check_field(&self, name: &str, kinds: &[FieldKind], what: &str) -> AgentResult<Resolved> {
        let r = resolve(self.store.manifest(), name)
            .map_err(|e| AgentError::contract(format!("{what}: {}", e.message())))?;
        if !kinds.contains(&r.kind) {
            return Err(AgentError::contract(format!(
                "{what} {name} must be a {} field, not {}",
                kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(" or "),
                r.kind.as_str()
            )));
        }
        Ok(r)
    }

    fn layer_scale(&self, r: &Resolved) -> (Reduce, Scale) {
        if r.stored == self.main.stored && r.derived.is_none() {
            return (self.reduce, self.scale);
        }
        let (lo, hi, mag) = self.store.range(&r.stored).unwrap_or((0.0, 1.0, 1.0));
        let map = Colormap::for_hint(r.palette);
        let (reduce, lo, hi) =
            if r.components > 1 { (Reduce::Magnitude, 0.0, mag) } else { (Reduce::Component(0), lo, hi) };
        let scale =
            if map == Colormap::CoolWarm { Scale::symmetric(map, lo, hi) } else { Scale::new(map, lo, hi) };
        (reduce, scale)
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_body(&self) -> AgentResult<Option<BodySpec>> {
        let Some(d) = &self.view.deformation else { return Ok(None) };
        let (displacement, reference) = if d.field.is_empty() {
            let reference = match &self.main.place {
                Place::Cells(mesh) | Place::Nodes(mesh) => Place::Nodes(mesh.clone()),
                Place::Grid(_) => return Err(AgentError::contract("deformation.field is required")),
            };
            (None, reference)
        } else {
            let di = self.check_field(
                &d.field,
                &[FieldKind::Displacement, FieldKind::Vector],
                "deformation field",
            )?;
            if matches!(di.place, Place::Cells(_)) || di.derived.is_some() {
                return Err(AgentError::contract(format!(
                    "the deformation field {} must be a stored nodal displacement",
                    d.field
                )));
            }
            let reference = di.place.clone();
            (Some(di), reference)
        };
        let classify = |name: &str, kinds: &[FieldKind], what: &str| -> AgentResult<(Resolved, bool)> {
            let r = self.check_field(name, kinds, what)?;
            let sites = if r.derived.is_some() { None } else { body_sites(&reference, &r.place) };
            let Some(cells) = sites else {
                return Err(AgentError::contract(format!(
                    "{name} must live on the reference {} of the displacement, on its cells ({}), not on {}",
                    reference.name(),
                    match &reference {
                        Place::Grid(_) => "the cell grid with one cell fewer per axis".to_owned(),
                        other => format!("{}/cells", other.name()),
                    },
                    r.place.name()
                )));
            };
            Ok((r, cells))
        };
        let mask = d
            .mask
            .as_ref()
            .map(|n| classify(n, &[FieldKind::Occupancy, FieldKind::Scalar], "deformation mask"))
            .transpose()?;

        let main_on_body = self.main.derived.is_none() && body_sites(&reference, &self.main.place).is_some();
        let colour_name = d.colour.clone().or_else(|| main_on_body.then(|| self.view.field.clone()));
        let colour = colour_name
            .as_ref()
            .map(|n| {
                classify(
                    n,
                    &[FieldKind::Scalar, FieldKind::Occupancy, FieldKind::Displacement, FieldKind::Vector],
                    "deformation colour",
                )
            })
            .transpose()?;
        let is_mesh = matches!(reference, Place::Nodes(_));
        let on_cells = mask.as_ref().is_some_and(|m| m.1) || colour.as_ref().is_some_and(|c| c.1);
        if !(is_mesh || on_cells || self.view.mode == Mode::Iso) {
            return Ok(None);
        }
        if let Place::Grid(g) = &reference
            && g.shape.iter().any(|n| *n < 2)
        {
            return Err(AgentError::contract(format!(
                "the reference grid {} of a deformed body needs two nodes per axis",
                g.name
            )));
        }
        let mesh = match &reference {
            Place::Nodes(mesh) => {
                Some(self.store.mesh(&mesh.name).map_err(|e| AgentError::failed(e.to_string()))?)
            }
            _ => None,
        };
        let (colour, colour_label) = match colour {
            Some((r, cells)) => {
                let (reduce, scale) = self.layer_scale(&r);
                let label =
                    if r.components > 1 { format!("|{}| [{}]", r.stored, r.unit) } else { r.unit.clone() };
                (Some((r.stored.clone(), cells, reduce, r.components, scale)), label)
            }
            None => (None, String::new()),
        };
        Ok(Some(BodySpec {
            displacement,
            scale: d.scale,
            reference,
            mesh,
            mask: mask.map(|(r, cells)| (r.stored, cells)),
            threshold: d.mask_threshold,
            colour,
            colour_label,
        }))
    }



    #[allow(clippy::too_many_lines)]
    pub fn new(
        store: &'a DynamicStore,
        store_id: &str,
        view: View,
        frames: &[&dyn FrameValues],
    ) -> AgentResult<Self> {
        let m = store.manifest();
        let view = with_defaults(m, view)?;
        let spec = resolve(m, &view.field)?;
        let comps = spec.components;
        let reduce = match view.component {
            Some(c) if comps > 1 && c < comps => Reduce::Component(c),
            Some(c) if comps > 1 => {
                return Err(AgentError::contract(format!(
                    "field {} has {comps} components; component {c} does not exist",
                    view.field
                )));
            }
            _ if comps > 1 => Reduce::Magnitude,
            _ => Reduce::Component(0),
        };
        let mut r = Self {
            store,
            view,
            reduce,
            scale: Scale::new(Colormap::Viridis, 0.0, 1.0),
            window: None,
            slice_at: None,
            label: spec.label.clone(),
            unit: spec.unit.clone(),
            store_id: store_id.to_owned(),
            iso_field: None,
            range_source: "run",
            main: spec.clone(),
            body: None,
        };
        let dims = spec.place.dims();
        if let Some(o) = &r.view.occupancy {
            let oi = r.check_field(o, &[FieldKind::Occupancy, FieldKind::Scalar], "occupancy")?;
            if oi.place.grid().is_none() || oi.place.dims() != dims {
                return Err(AgentError::contract(
                    "the occupancy contour field must live on a grid of the view field's dimension",
                ));
            }
        }
        if let Some(fl) = &r.view.flow {
            let fi = r.check_field(&fl.field, &[FieldKind::Vector], "flow field")?;
            if fi.place.grid().is_none() {
                return Err(AgentError::contract("the flow field must live on a grid"));
            }
        }

        let map = r.view.colormap.unwrap_or_else(|| Colormap::for_hint(spec.palette));
        let diverging = map == Colormap::CoolWarm && r.view.colormap.is_none();

        let run =
            store.range(&spec.stored).filter(|_| spec.derived.is_none()).map(|(lo, hi, mag)| match reduce {
                Reduce::Magnitude => (0.0, mag),
                Reduce::Component(_) => (lo, hi),
            });
        let range_from_frames = r.view.value_range.is_none() && (r.view.range_frames || run.is_none());
        if let Some(v) = r.view.value_range {
            r.range_source = "request";
            r.scale = Scale::new(map, v.0, v.1);
        } else if let (false, Some(v)) = (r.view.range_frames, run) {
            r.scale = if diverging { Scale::symmetric(map, v.0, v.1) } else { Scale::new(map, v.0, v.1) };
        }
        if let Some(d) = &r.view.deformation
            && r.body.is_none()
        {
            r.body = r.resolve_body()?;
            if let Some(b) = &r.body
                && spec.place.grid().is_some()
                && b.reference.dims() != dims
            {
                return Err(AgentError::contract(format!(
                    "the deformed body is {}-D but the view field {} is {dims}-D",
                    b.reference.dims(),
                    r.view.field
                )));
            }
            if r.body.is_none() {

                let di = r.check_field(
                    &d.field,
                    &[FieldKind::Displacement, FieldKind::Vector],
                    "deformation field",
                )?;
                let rg = di.place.name();
                for (name, kinds) in [
                    (&d.mask, &[FieldKind::Occupancy, FieldKind::Scalar][..]),
                    (&d.colour, &[FieldKind::Scalar, FieldKind::Occupancy][..]),
                ] {
                    if let Some(n) = name {
                        let i = r.check_field(n, kinds, "deformation layer")?;
                        if i.place.name() != rg {
                            return Err(AgentError::contract(format!(
                                "{n} must live on the reference grid {rg} of the displacement"
                            )));
                        }
                    }
                }
            }
        }
        let body_bounds = r.body.as_ref().map(BodySpec::bounds);
        let field_bounds: (Vec<f64>, Vec<f64>) = match (&spec.place, body_bounds) {
            (Place::Grid(g), _) => g.bounds(),
            (_, Some((lo, hi))) => (lo[..dims].to_vec(), hi[..dims].to_vec()),
            _ => return Err(AgentError::contract("a field on a mesh is drawn on its mesh")),
        };
        match r.view.mode {
            Mode::Iso => {
                if dims != 3 {
                    return Err(AgentError::contract("mode iso needs a field on a 3-D grid or mesh"));
                }
                let iso = match &r.view.iso_field {
                    Some(n) => {
                        let ii = r.check_field(n, &[FieldKind::Occupancy, FieldKind::Scalar], "iso field")?;
                        if ii.place.grid().is_none_or(|g| g.dims() != 3) {
                            return Err(AgentError::contract("the iso field must live on a 3-D grid"));
                        }
                        Some(n.clone())
                    }
                    None if r.body.is_some() => None,
                    None => Some(
                        m.fields
                            .iter()
                            .find(|f| f.kind == FieldKind::Occupancy && spec.place.grid().is_some_and(|g| f.grid == g.name))
                            .map(|f| f.name.clone())
                            .ok_or_else(|| {
                                AgentError::contract(
                                    "mode iso needs iso.field (the grid has no occupancy field) or a deformation",
                                )
                            })?,
                    ),
                };
                if iso.is_some() && spec.place.grid().is_none() {
                    return Err(AgentError::contract("an iso-surface is coloured by a field on a grid"));
                }
                r.iso_field = iso;
                let (lo, hi) = &field_bounds;
                let a = r.view.axis;
                r.slice_at = Some(lo[a] + r.view.position * (hi[a] - lo[a]));
            }
            Mode::Section | Mode::PhaseAverage | Mode::Kymograph => {
                if dims == 3 {
                    let (lo, hi) = &field_bounds;
                    let a = r.view.axis;
                    r.slice_at = Some(lo[a] + r.view.position * (hi[a] - lo[a]));
                }
            }
        }

        if matches!(r.view.mode, Mode::Section | Mode::PhaseAverage) {
            let keep: Vec<usize> =
                if dims == 3 { (0..3).filter(|&a| a != r.view.axis).collect() } else { vec![0, 1] };
            let (lo, hi) = &field_bounds;
            let mut w = Window { lo: [lo[keep[0]], lo[keep[1]]], hi: [hi[keep[0]], hi[keep[1]]] };
            if let Some(d) = &r.view.deformation {
                let reach = if d.field.is_empty() {
                    0.0
                } else {
                    store.range(&d.field).map_or(0.0, |x| x.2) * d.scale
                };
                let (rlo, rhi) = match (&r.body, m.field(&d.field).and_then(|(di, _)| m.field_grid(di))) {
                    (Some(b), _) => {
                        let (lo, hi) = b.bounds();
                        (lo.to_vec(), hi.to_vec())
                    }
                    (None, Some(rg)) => rg.bounds(),
                    _ => (lo.clone(), hi.clone()),
                };
                let rd = rlo.len().min(3);
                let k: Vec<usize> = if rd == 3 && dims == 3 { keep.clone() } else { vec![0, 1] };
                let solid =
                    Window { lo: [rlo[k[0]], rlo[k[1]]], hi: [rhi[k[0]], rhi[k[1]]] }.padded(reach.min(1e12));
                w = if r.field_on_reference() { solid } else { w.union(solid) };
            }
            r.window = Some(w);
        }
        if range_from_frames {
            r.range_source = "frames";
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for fv in frames {
                let values = r.scalar_values(*fv)?;
                for x in values.into_iter().filter(|x| x.is_finite()) {
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
            let (lo, hi) = if lo > hi { (0.0, 1.0) } else { (lo, hi) };
            r.scale = if diverging { Scale::symmetric(map, lo, hi) } else { Scale::new(map, lo, hi) };

            if let Some(b) = r.body.as_mut()
                && let Some(c) = b.colour.as_mut()
                && c.0 == r.main.stored
            {
                c.4 = r.scale;
            }
        }
        Ok(r)
    }

    fn scalar_values(&self, fv: &dyn FrameValues) -> AgentResult<Vec<f64>> {
        if self.main.place.grid().is_some() {
            let g = self.base(fv)?;
            let mut out = Vec::with_capacity(g.values.len() / g.components.max(1));
            for c in g.values.chunks_exact(g.components.max(1)) {
                out.push(match self.reduce {
                    Reduce::Magnitude if c.len() > 1 => c.iter().map(|x| x * x).sum::<f64>().sqrt(),
                    Reduce::Magnitude => c[0],
                    Reduce::Component(k) => c.get(k).copied().unwrap_or(f64::NAN),
                });
            }
            return Ok(out);
        }
        Ok(reduce_values(fv.values(&self.main.stored)?, self.main.components, self.reduce))
    }

    #[must_use]
    pub const fn scale(&self) -> &Scale {
        &self.scale
    }

    #[must_use]
    pub const fn range_source(&self) -> &'static str {
        self.range_source
    }

    #[must_use]
    pub const fn view(&self) -> &View {
        &self.view
    }

    #[must_use]
    pub fn field_record(&self) -> Value {
        let spec = &self.main;
        let body = self.body.as_ref().map(|b| {
            json!({"reference": b.reference.name(), "displacement": b.displacement.as_ref().map(|d| d.stored.clone()),
                   "scale": b.scale, "mask": b.mask.as_ref().map(|m| m.0.clone()), "mask_threshold": b.threshold,
                   "colour": b.colour.as_ref().map(|c| json!({"field": c.0, "on": if c.1 { "cells" } else { "nodes" },
                        "colormap": c.4.map.name(), "display_range": [c.4.lo, c.4.hi]})),
                   "simplices": match b.reference { Place::Grid(_) => "freudenthal_kuhn", _ => "mesh" }})
        });
        json!({"name": self.view.field, "label": self.label, "unit": self.unit,
               "kind": spec.kind.as_str(), "grid": spec.place.name(),
               "derived": spec.derived.map(Derived::name),
               "colormap": self.scale.map.name(), "display_range": [self.scale.lo, self.scale.hi],
               "range_source": self.range_source, "iso_field": self.iso_field, "body": body})
    }

    fn named(&self, fv: &dyn FrameValues, name: &str, in_plane: bool) -> AgentResult<GridField> {
        let r = resolve(self.store.manifest(), name)?;
        let Some(grid) = r.place.grid() else {
            return Err(AgentError::contract(format!("{name} lives on a mesh; it is drawn on its body")));
        };
        let mut g = grid_field(grid, r.stored_components, fv.values(&r.stored)?)?;
        if let Some(op) = r.derived {
            g = g.derive(op).map_err(contract)?;
        }
        match (self.slice_at, g.dims()) {
            (Some(at), 3) if self.view.mode != Mode::Iso => {
                let s = g.slice(self.view.axis, at).map_err(contract)?;
                Ok(if in_plane { s.in_plane(Some(self.view.axis)) } else { s })
            }
            _ => Ok(g),
        }
    }

    fn field_on_reference(&self) -> bool {
        if let Some(b) = &self.body {
            return self.main.derived.is_none() && body_sites(&b.reference, &self.main.place).is_some();
        }
        let m = self.store.manifest();
        let grid_of = |n: &str| resolve(m, n).ok().map(|r| r.place.name());
        self.view
            .deformation
            .as_ref()
            .is_some_and(|d| grid_of(&d.field).is_some() && grid_of(&d.field) == grid_of(&self.view.field))
    }

    fn base(&self, fv: &dyn FrameValues) -> AgentResult<GridField> {
        self.named(fv, &self.view.field, false)
    }

    fn texts(&self, subtitle: &str) -> FrameText {
        let comp = match self.reduce {
            Reduce::Component(c) if self.main.components > 1 => format!(" {}", ["x", "y", "z"][c.min(2)]),
            Reduce::Magnitude => " |.|".to_owned(),
            Reduce::Component(_) => String::new(),
        };
        FrameText {
            title: format!("{}{comp} [{}]", self.label, self.unit),
            subtitle: format!("{subtitle}  {}", self.store_id),
            bar_label: self.unit.clone(),
        }
    }

    #[must_use]
    pub fn caption(&self, frame: &FrameEntry) -> String {
        let unit = &self.store.manifest().time.unit;
        match frame.phase {
            Some(p) => {
                format!("t={} {unit} phase={:.3}", implexity_render::dynamic::draw::fmt_num(frame.t), p)
            }
            None => format!("t={} {unit}", implexity_render::dynamic::draw::fmt_num(frame.t)),
        }
    }

    fn body_triangles(
        &self,
        b: &BodySpec,
        fv: &dyn FrameValues,
    ) -> AgentResult<(Triangles, Option<Triangles>)> {
        let u = b.displacement.as_ref().map(|d| fv.values(&d.stored)).transpose()?;
        let mask = b.mask.as_ref().map(|(n, cells)| fv.values(n).map(|v| (v, *cells))).transpose()?;
        let colour = b
            .colour
            .as_ref()
            .map(|(n, cells, reduce, comps, _)| {
                fv.values(n).map(|v| (reduce_values(v, *comps, *reduce), *cells))
            })
            .transpose()?;
        let topology = match (&b.reference, &b.mesh) {
            (Place::Grid(g), _) => Topology::Grid { nodes: &g.shape, spacing: &g.spacing, origin: &g.origin },
            (_, Some(mesh)) => {
                Topology::Mesh { dims: b.reference.dims(), points: &mesh.points, cells: &mesh.cells }
            }
            _ => return Err(AgentError::failed("body mesh missing")),
        };
        let body = Body {
            topology,
            displacement: u.as_deref(),
            scale: b.scale,
            mask: mask.as_ref().map(|m| (sites(m), b.threshold)),
            colour: colour.as_ref().map(sites),
        };
        if self.view.mode == Mode::Iso {
            let surface = body.surface(MAX_TRIANGLES).map_err(contract)?;
            let cap = self.view.clip.map(|(p, n)| body.section(p, n)).transpose().map_err(contract)?;
            return Ok((surface, cap));
        }
        let mut normal = [0.0; 3];
        normal[self.view.axis.min(2)] = 1.0;
        let mut point = [0.0; 3];
        point[self.view.axis.min(2)] = self.slice_at.unwrap_or(0.0);
        Ok((body.section(point, normal).map_err(contract)?, None))
    }



    #[allow(clippy::too_many_lines)]
    pub fn render(&self, fv: &dyn FrameValues, subtitle: &str) -> AgentResult<RgbImage> {
        let text = self.texts(subtitle);
        let body = self.body.as_ref().map(|b| self.body_triangles(b, fv).map(|t| (b, t))).transpose()?;
        let on_reference = self.field_on_reference();
        if self.view.mode == Mode::Iso {
            let surface = self.iso_field.as_ref().map(|n| self.named(fv, n, false)).transpose()?;
            let colour = match (&surface, self.main.place.grid()) {
                (Some(_), Some(_)) => Some(self.base(fv)?),
                _ => None,
            };

            let backdrop_field = match (&surface, self.main.place.grid(), &body) {
                (None, Some(g), Some(_)) if g.dims() == 3 && !on_reference => Some(
                    self.base(fv)?.slice(self.view.axis, self.slice_at.unwrap_or(0.0)).map_err(contract)?,
                ),
                _ => None,
            };
            let body_surface = body.as_ref().map(|(b, (surface, cap))| BodySurface {
                surface,
                cap: cap.as_ref(),
                colour: b.colour.as_ref().map(|c| c.4),
                label: &b.colour_label,
            });
            return render_volume(&VolumeView {
                surface: surface.as_ref(),
                iso: self.view.iso_value,
                colour: colour.as_ref().map(|c| (c, self.reduce, self.scale)),
                body: body_surface,
                backdrop: backdrop_field.as_ref().map(|f| Backdrop {
                    field: f,
                    reduce: self.reduce,
                    scale: self.scale,
                    axis: self.view.axis,
                    at: self.slice_at.unwrap_or(0.0),
                }),
                clip: self.view.clip,
                camera: self.view.camera.clone(),
                width: self.view.width,
                height: self.view.height,
                theme: self.view.theme,
                title: (text.title, text.subtitle),
                bar_label: text.bar_label,
            })
            .map_err(contract);
        }
        let base = if on_reference && body.is_some() { None } else { Some(self.base(fv)?) };
        let occupancy = match &self.view.occupancy {
            Some(o) => Some(self.named(fv, o, false)?),
            None => None,
        };
        let flow_field = match &self.view.flow {
            Some(fl) => Some(self.named(fv, &fl.field, true)?),
            None => None,
        };

        let colour_name = match (&self.view.deformation, &body) {
            (Some(d), None) => d
                .colour
                .clone()
                .or_else(|| (on_reference && d.field != self.view.field).then(|| self.view.field.clone())),
            _ => None,
        };
        let (disp, mask, colour) = match (&self.view.deformation, &body) {
            (Some(d), None) => (
                Some(self.named(fv, &d.field, true)?),
                d.mask.as_ref().map(|n| self.named(fv, n, false)).transpose()?,
                colour_name.as_ref().map(|n| self.named(fv, n, false)).transpose()?,
            ),
            _ => (None, None, None),
        };
        let colour_scale = match &colour_name {
            Some(n) if *n == self.view.field => self.scale,
            Some(n) => resolve(self.store.manifest(), n).map_or(self.scale, |r| self.layer_scale(&r).1),
            None => self.scale,
        };
        let axes: [usize; 2] = match (self.slice_at, self.main.place.dims()) {
            (Some(_), 3) => {
                let k: Vec<usize> = (0..3).filter(|&a| a != self.view.axis).collect();
                [k[0], k[1]]
            }
            _ => [0, 1],
        };
        let body_colour = body.as_ref().and_then(|(b, _)| b.colour.as_ref().map(|c| c.4));
        let layers = Layers {
            base: match &base {
                Some(g) if !on_reference => Some((g, self.reduce, self.scale)),
                _ => None,
            },
            occupancy: occupancy.as_ref().map(|o| (o, self.view.fill_occupancy)),
            flow: match (&self.view.flow, &flow_field) {
                (Some(fl), Some(ff)) if fl.lic => Some(FlowOverlay::Lic(ff, fl.length)),
                (Some(fl), Some(ff)) => Some(FlowOverlay::Streamlines(
                    ff,
                    StreamStyle { separation: fl.separation, ..StreamStyle::default() },
                )),
                _ => None,
            },
            solid: match (&self.view.deformation, &disp) {
                (Some(d), Some(dg)) => Some(SolidLayer {
                    displacement: dg,
                    scale: d.scale,
                    mask: mask.as_ref(),
                    colour: colour.as_ref().map(|c| (c, colour_scale)),
                    grid_every: d.grid_every,
                }),
                _ => None,
            },
            body: body.as_ref().map(|(b, (t, _))| BodyLayer {
                triangles: t,
                axes,
                colour: body_colour,
                label: if on_reference { &self.unit } else { &b.colour_label },
            }),
        };
        let window = match (self.window, &base) {
            (Some(w), _) => w,
            (None, Some(g)) => Window::of(g),
            (None, None) => return Err(AgentError::contract("the frame has no window")),
        };
        let size = FrameSize {
            width: self.view.width,
            height: self.view.height,
            theme: self.view.theme,
            colorbar: true,
        };
        render_frame(&layers, window, &size, &text).map_err(contract)
    }
}
