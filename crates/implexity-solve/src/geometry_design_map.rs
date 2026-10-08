// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_core::error::{CaeError, CaeResult};
use implexity_core::wire::fingerprint_value;
use implexity_geometry::field_registration::GridRegistration;
use implexity_geometry::lattice::assembly::ControlledAssembly;
use implexity_geometry::lattice::controls::{CONTROL_SCHEMA, control_contract, validate_control};
use implexity_geometry::lattice::node::ControlledLattice;
use implexity_geometry::node::ConstructArgs;
use implexity_geometry::{Attr, GeometryError, NdArray, Node, ParamValue};
use implexity_optim::design::{NamedArrays, design_identity};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

pub const SCHEMA: &str = "implexity-geometry-design-map/1";
pub const ASSEMBLY_MAP_SCHEMA: &str = "implexity-geometry-design-map/2";
pub const CONTROL_COORDINATE: &str = "model:control";

const LATTICE_KIND: &str = "lattice.controlled";
const ASSEMBLY_KIND: &str = "lattice.controlled_assembly";
const REGISTRATION_SCHEMA: &str = "implexity-grid-registration/1";

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn geo(e: &GeometryError) -> CaeError {
    CaeError::contract(e.to_string())
}

fn float(x: f64) -> Value {
    serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
}

fn floats(x: &[f64]) -> Value {
    Value::Array(x.iter().copied().map(float).collect())
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(n: usize) -> f64 {
    n as f64
}

fn nested(value: &Value) -> Option<(Vec<usize>, Vec<f64>, bool)> {
    match value {
        Value::Number(n) => Some((Vec::new(), vec![n.as_f64()?], true)),
        Value::Bool(b) => Some((Vec::new(), vec![if *b { 1.0 } else { 0.0 }], false)),
        Value::Array(items) => {
            let mut shape: Option<Vec<usize>> = None;
            let mut data = Vec::new();
            let mut number = false;
            for item in items {
                let (s, d, n) = nested(item)?;
                if shape.as_ref().is_some_and(|prev| *prev != s) {
                    return None;
                }
                shape = Some(s);
                data.extend(d);
                number |= n;
            }
            let mut out = vec![items.len()];
            out.extend(shape.unwrap_or_default());
            Some((out, data, number))
        }
        _ => None,
    }
}

fn to_nested_list(shape: &[usize], data: &[f64]) -> Value {
    if shape.is_empty() {
        return float(data.first().copied().unwrap_or(0.0));
    }
    let stride: usize = shape[1..].iter().product();
    Value::Array(
        (0..shape[0]).map(|i| to_nested_list(&shape[1..], &data[i * stride..(i + 1) * stride])).collect(),
    )
}

fn attrs_json(attrs: Vec<(String, Attr)>) -> Map<String, Value> {
    attrs.into_iter().map(|(k, a)| (k, a.to_json())).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Lattice,
    Assembly,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Lattice => LATTICE_KIND,
            Self::Assembly => ASSEMBLY_KIND,
        }
    }
}

fn build_node(raw: &Map<String, Value>) -> CaeResult<(Kind, Node)> {
    let kind = if raw.get("schema").and_then(Value::as_str) == Some(SCHEMA) {
        Kind::Lattice
    } else {
        match raw.get("kind").and_then(Value::as_str) {
            Some(LATTICE_KIND) => Kind::Lattice,
            Some(ASSEMBLY_KIND) => Kind::Assembly,
            _ => return Err(err("unsupported geometry-map kind")),
        }
    };
    let attrs: BTreeMap<String, Attr> = match raw.get("geometry") {
        Some(Value::Object(g)) => g.iter().map(|(k, v)| (k.clone(), Attr::from_json(v))).collect(),
        None | Some(Value::Null) => BTreeMap::new(),
        Some(_) => return Err(err("geometry design map geometry must be a mapping")),
    };
    let args = ConstructArgs { children: Vec::new(), names: None, params: BTreeMap::new(), attrs };
    let entry = match kind {
        Kind::Lattice => ControlledLattice::entry(),
        Kind::Assembly => ControlledAssembly::entry(),
    };
    Ok((kind, (entry.construct)(args).map_err(|e| geo(&e))?))
}


#[allow(clippy::too_many_lines)]
pub fn normalise(raw: &Value) -> CaeResult<Map<String, Value>> {
    let raw = raw.as_object().ok_or_else(|| err("geometry design map must be a mapping"))?;
    let schema = raw.get("schema").and_then(Value::as_str);
    let assembly_schema = schema == Some(ASSEMBLY_MAP_SCHEMA);
    let mut allowed = vec!["schema", "geometry", "outputs", "constants"];
    if assembly_schema {
        allowed.extend(["kind", "phase_role"]);
    }
    if !matches!(schema, Some(SCHEMA | ASSEMBLY_MAP_SCHEMA))
        || raw.keys().any(|k| !allowed.contains(&k.as_str()))
    {
        return Err(err("invalid or unsupported geometry design map"));
    }
    let (kind, node) = build_node(raw)?;
    let mapping = |key: &str, message: &str| -> CaeResult<Map<String, Value>> {
        match raw.get(key) {
            None | Some(Value::Null) => Ok(Map::new()),
            Some(Value::Object(m)) => Ok(m.clone()),
            Some(Value::Array(a)) if a.is_empty() => Ok(Map::new()),
            Some(_) => Err(err(message.to_string())),
        }
    };
    let outputs_message = "geometry outputs must be nonempty and distinct from constant coordinates";
    let outputs = mapping("outputs", outputs_message)?;
    let constants = mapping("constants", "constant provider coordinates must be named finite real arrays")?;
    if outputs.is_empty() || outputs.keys().any(|k| constants.contains_key(k)) {
        return Err(err(outputs_message));
    }
    if !outputs.values().any(|v| v.as_str() == Some("rho")) {
        return Err(err("a named physical coordinate must consume the common geometry occupancy"));
    }
    if outputs.iter().any(|(k, v)| {
        k.is_empty() || !matches!(v.as_str(), Some("rho" | "phase_fraction" | "cell_spacing_mm"))
    }) {
        return Err(err("geometry outputs require named coordinates and declared neutral fields"));
    }
    let role = if assembly_schema { raw.get("phase_role").filter(|v| !v.is_null()) } else { None };
    if outputs.values().any(|v| v.as_str() == Some("phase_fraction")) {
        if let Some(role) = role
            && *role != json!({"mode": "coupled"})
        {
            return Err(err("a consumed phase field cannot also be declared inactive"));
        }
    } else {
        let inactive = assembly_schema
            && role.and_then(Value::as_object).is_some_and(|r| {
                r.len() == 2
                    && r.get("mode").and_then(Value::as_str) == Some("inactive")
                    && r.get("reason")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 4096)
            });
        if !inactive {
            return Err(err("no phase consumer: explicitly declare phase_role inactive and its reason"));
        }
    }
    let mut checked = Map::new();
    for (key, value) in &constants {
        let parsed = nested(value).filter(|(shape, data, number)| {
            !key.is_empty()
                && !shape.is_empty()
                && !data.is_empty()
                && *number
                && data.iter().all(|v| v.is_finite())
        });
        let Some((shape, data, _)) = parsed else {
            return Err(err("constant provider coordinates must be named finite real arrays"));
        };
        checked.insert(key.clone(), to_nested_list(&shape, &data));
    }
    let mut result = Map::new();
    result.insert("schema".into(), json!(schema));
    result.insert("geometry".into(), Value::Object(attrs_json(node.op().doc_attrs())));
    result.insert("outputs".into(), Value::Object(outputs));
    result.insert("constants".into(), Value::Object(checked));
    if assembly_schema {
        result.insert("kind".into(), json!(kind.name()));
        result.insert("phase_role".into(), role.cloned().unwrap_or_else(|| json!({"mode": "coupled"})));
    }
    Ok(result)
}

struct View {
    origin_mm: [f64; 3],
    domain_mm: [f64; 3],
    analysis_grid: [usize; 3],
    control_grid: [usize; 3],
}

pub struct GeometryDesignMap {
    spec: Map<String, Value>,
    kind: Kind,
    node: Arc<Node>,
    identity: String,
}

impl std::fmt::Debug for GeometryDesignMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeometryDesignMap").field("identity", &self.identity).finish_non_exhaustive()
    }
}

impl GeometryDesignMap {

    pub fn new(raw: &Value) -> CaeResult<Self> {
        let spec = normalise(raw)?;
        let (kind, node) = build_node(&spec)?;
        let identity = fingerprint_value(&Value::Object(spec.clone()));
        Ok(Self { spec, kind, node: Arc::new(node), identity })
    }

    #[must_use]
    pub fn spec(&self) -> &Map<String, Value> {
        &self.spec
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    fn lattice(&self) -> Option<&ControlledLattice> {
        self.node.op().as_any().downcast_ref::<ControlledLattice>()
    }

    fn assembly(&self) -> Option<&ControlledAssembly> {
        self.node.op().as_any().downcast_ref::<ControlledAssembly>()
    }

    fn view(&self) -> CaeResult<View> {
        if let Some(l) = self.lattice() {
            let s = &l.spec;
            return Ok(View {
                origin_mm: s.origin_mm,
                domain_mm: s.domain_mm,
                analysis_grid: s.analysis_grid,
                control_grid: s.control_grid,
            });
        }
        let a = self.assembly().ok_or_else(|| err("unsupported geometry-map kind"))?;
        Ok(View {
            origin_mm: a.origin_mm,
            domain_mm: a.domain_mm,
            analysis_grid: a.analysis_grid,
            control_grid: a.control_grid,
        })
    }

    #[must_use]
    pub fn control_shape(&self) -> Vec<usize> {
        if let Some(a) = self.assembly() {
            return a.control_shape().to_vec();
        }
        let g = self.lattice().map_or([0; 3], |l| l.spec.control_grid);
        vec![20, g[0], g[1], g[2]]
    }

    #[must_use]
    pub fn analysis_shape(&self) -> [usize; 3] {
        self.view().map_or([0; 3], |v| v.analysis_grid)
    }


    pub fn validate_model(&self, node: &Node, parameter: &str) -> CaeResult<()> {
        let same = node.kind() == self.kind.name()
            && parameter == "control"
            && fingerprint_value(&Value::Object(attrs_json(node.op().doc_attrs())))
                == fingerprint_value(&self.spec["geometry"]);
        if same {
            Ok(())
        } else {
            Err(err(
                "geometry map does not match the authoritative model; re-author the problem and preflight",
            ))
        }
    }

    fn validate_control(&self, control: &ArrayD<f64>) -> CaeResult<()> {
        let data: Vec<f64> = control.iter().copied().collect();
        if let Some(a) = self.assembly() {
            let expected = a.control_shape();
            if control.shape() != expected {
                let dims: Vec<String> = expected.iter().map(ToString::to_string).collect();
                return Err(err(format!("controlled assembly requires shape ({})", dims.join(", "))));
            }
            let block = data.len() / a.volumes.len().max(1);
            for chunk in data.chunks(block.max(1)) {
                let shape = [20, a.control_grid[0], a.control_grid[1], a.control_grid[2]];
                validate_control(&shape, chunk, 'f', Some(a.control_grid)).map_err(|e| geo(&e))?;
            }
            return Ok(());
        }
        let grid = self.view()?.control_grid;
        validate_control(control.shape(), &data, 'f', Some(grid)).map(|_| ()).map_err(|e| geo(&e))
    }

    fn require_control<'a>(&self, design: &'a NamedArrays, message: &str) -> CaeResult<&'a ArrayD<f64>> {
        if design.len() != 1 || !design.contains(CONTROL_COORDINATE) {
            return Err(err(message.to_string()));
        }
        let control = design.get(CONTROL_COORDINATE).ok_or_else(|| err(message.to_string()))?;
        self.validate_control(control)?;
        Ok(control)
    }

    fn bound(&self, control: &ArrayD<f64>) -> CaeResult<Node> {
        let array = NdArray::from_f64(control.shape().to_vec(), control.iter().copied().collect())
            .ok_or_else(|| err("control tensor shape mismatch"))?;
        let mut params = self.node.params().clone();
        params.insert("control".into(), ParamValue::Array(Arc::new(array)));
        Ok(self.node.with_params(params))
    }

    fn source(&self) -> CaeResult<&dyn implexity_geometry::occupancy::OccupancySource> {
        self.node.op().occupancy_source().ok_or_else(|| err("geometry map node has no occupancy source"))
    }

    fn constant_arrays(&self) -> CaeResult<Vec<(String, ArrayD<f64>)>> {
        let constants = self.spec["constants"].as_object().cloned().unwrap_or_default();
        constants
            .iter()
            .map(|(k, v)| {
                let (shape, data, _) = nested(v)
                    .ok_or_else(|| err("constant provider coordinates must be named finite real arrays"))?;
                let a = ArrayD::from_shape_vec(IxDyn(&shape), data).map_err(|e| err(e.to_string()))?;
                Ok((k.clone(), a))
            })
            .collect()
    }

    fn outputs(&self) -> Vec<(String, String)> {
        self.spec["outputs"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect())
            .unwrap_or_default()
    }


    pub fn forward(&self, design: &NamedArrays) -> CaeResult<NamedArrays> {
        let control = self.require_control(
            design,
            "a mapped design must contain exactly the authoritative twenty-field model:control tensor",
        )?;
        let node = self.bound(control)?;
        let fields = self.source()?.analysis_fields(&node).map_err(|e| geo(&e))?;
        let view = self.view()?;
        let grid = IxDyn(&fields.shape);
        let spacing: Vec<f64> = (0..3).map(|a| view.domain_mm[a] / as_f64(view.analysis_grid[a])).collect();
        let mut out = NamedArrays::new();
        for (key, field) in self.outputs() {
            let array = match field.as_str() {
                "rho" => ArrayD::from_shape_vec(grid.clone(), fields.rho.clone()),
                "phase_fraction" => ArrayD::from_shape_vec(grid.clone(), fields.phase_fraction.clone()),
                _ => ArrayD::from_shape_vec(IxDyn(&[3]), spacing.clone()),
            }
            .map_err(|e| err(e.to_string()))?;
            out.insert(key, array);
        }
        for (key, array) in self.constant_arrays()? {
            out.insert(key, array);
        }
        Ok(out)
    }


    pub fn pullback(&self, design: &NamedArrays, gradients: &NamedArrays) -> CaeResult<NamedArrays> {
        let control =
            self.require_control(design, "a mapped derivative requires the authoritative control tensor")?;
        let values = self.forward(design)?;
        let mut names = values.names();
        let mut given = gradients.names();
        names.sort();
        given.sort();
        if names != given {
            return Err(err(
                "physical derivative omitted or added a decoded coordinate; a partial adjoint is not admissible",
            ));
        }
        for (key, value) in values.iter() {
            let g = gradients.get(key).ok_or_else(|| err("physical derivatives must be explicitly named"))?;
            if g.shape() != value.shape() || !g.iter().all(|v| v.is_finite()) {
                return Err(err(format!("invalid physical derivative shape/finiteness for {key}")));
            }
        }
        let cells = self.analysis_shape().iter().product::<usize>();
        let (mut adj_rho, mut adj_phase) = (vec![0.0; cells], vec![0.0; cells]);
        for (key, field) in self.outputs() {
            let target = match field.as_str() {
                "rho" => &mut adj_rho,
                "phase_fraction" => &mut adj_phase,
                _ => continue,
            };
            if let Some(g) = gradients.get(&key) {
                for (t, v) in target.iter_mut().zip(g.iter()) {
                    *t += v;
                }
            }
        }
        let node = self.bound(control)?;
        let grad = self.source()?.analysis_vjp(&node, &adj_rho, &adj_phase).map_err(|e| geo(&e))?;
        if !grad.iter().all(|v| v.is_finite()) {
            return Err(err("nonfinite complete geometry pullback"));
        }
        let array = ArrayD::from_shape_vec(IxDyn(control.shape()), grad).map_err(|e| err(e.to_string()))?;
        Ok(NamedArrays::single(CONTROL_COORDINATE, array))
    }

    fn reframe_registration(
        raw: &Map<String, Value>,
        view: &View,
        originals: &mut Map<String, Value>,
    ) -> CaeResult<Value> {
        let raw_value = Value::Object(raw.clone());
        let old = GridRegistration::from_wire(&raw_value)
            .map_err(|_| err("invalid provider result registration"))?;
        let canonical = old.to_wire();
        if let Some(id) = raw.get("registration_id") {
            let wire: Map<String, Value> = raw
                .iter()
                .filter(|(k, _)| *k != "registration_id")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if *id != Value::String(fingerprint_value(&Value::Object(wire))) {
                return Err(err("stale provider result registration identity"));
            }
        }
        if old.frame != "model" && old.frame != "provider_local" {
            return Err(err("unsupported provider result frame"));
        }
        if !old.origin.iter().all(|v| v.abs() <= 1e-12) {
            return Err(err("geometry-mapped provider did not return local-origin fields"));
        }
        let matrix = old.matrix();
        let expected: Vec<f64> = (0..3).map(|a| view.domain_mm[a] / as_f64(view.analysis_grid[a])).collect();
        let basis_ok = (0..3).all(|i| {
            (0..3).all(|j| {
                let e = if i == j { expected[i] } else { 0.0 };
                (matrix[i][j] - e).abs() <= 1e-12 + 1e-12 * e.abs()
            })
        });
        if old.axis_order != "xyz" || !basis_ok {
            return Err(err("provider registration basis disagrees with the authored analysis volume"));
        }
        if old.centering == "cell" && old.shape != view.analysis_grid {
            return Err(err("provider cell grid disagrees with the authored analysis volume"));
        }
        if old.centering == "node" && old.shape != view.analysis_grid.map(|n| n + 1) {
            return Err(err("provider node grid disagrees with the authored analysis volume"));
        }
        let new = GridRegistration::new(
            old.shape,
            view.origin_mm,
            old.basis,
            &old.centering,
            &old.axis_order,
            "model",
        )
        .map_err(|e| geo(&e))?
        .to_wire();
        let key = raw
            .get("registration_id")
            .or_else(|| canonical.get("registration_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        originals.insert(key, raw_value);
        Ok(new)
    }

    fn visit(value: &Value, view: &View, originals: &mut Map<String, Value>) -> CaeResult<Value> {
        match value {
            Value::Object(m) => {
                if m.get("schema").and_then(Value::as_str) == Some(REGISTRATION_SCHEMA) {
                    return Self::reframe_registration(m, view, originals);
                }
                let mut out = Map::new();
                for (k, v) in m {
                    out.insert(k.clone(), Self::visit(v, view, originals)?);
                }
                Ok(Value::Object(out))
            }
            Value::Array(items) => Ok(Value::Array(
                items.iter().map(|v| Self::visit(v, view, originals)).collect::<CaeResult<_>>()?,
            )),
            other => Ok(other.clone()),
        }
    }


    #[allow(clippy::too_many_lines)]
    pub fn reframe_result(
        &self,
        diagnostics: Option<&Map<String, Value>>,
        fields: Option<&NamedArrays>,
    ) -> CaeResult<(Map<String, Value>, NamedArrays)> {
        let view = self.view()?;
        let origin = view.origin_mm;
        let dg0 = diagnostics.cloned().unwrap_or_default();
        let mut values = fields.cloned().unwrap_or_default();
        if dg0.contains_key("geometry_result_frame") {
            return Err(err("result already has a geometry-frame transform; refusing double translation"));
        }
        let mut originals = Map::new();
        let Value::Object(mut dg) = Self::visit(&Value::Object(dg0), &view, &mut originals)? else {
            return Err(err("invalid provider diagnostics"));
        };
        if let Some(meta) = dg.get_mut("field_metadata") {
            let meta = meta.as_object_mut().ok_or_else(|| err("invalid field metadata"))?;
            for (name, row) in meta.iter_mut() {
                let row = row.as_object_mut().ok_or_else(|| err("invalid field metadata"))?;
                if row.get("geometric_role").and_then(Value::as_str) != Some("position") {
                    continue;
                }
                let Some(array) = values.get(name) else {
                    if fields.is_some() {
                        return Err(err("position metadata has no matching array"));
                    }
                    continue;
                };
                let units = if row.contains_key("coordinate_units") {
                    row.get("coordinate_units")
                } else {
                    row.get("units")
                };
                let scale = match units.and_then(Value::as_str) {
                    Some("m") => 1e-3,
                    Some("mm") => 1.0,
                    _ => return Err(err("position fields require explicit m or mm units")),
                };
                if array.ndim() < 2
                    || array.shape().last() != Some(&3)
                    || !array.iter().all(|v| v.is_finite())
                {
                    return Err(err("position fields must be finite xyz arrays"));
                }
                let mut moved = array.clone();
                for (i, v) in moved.iter_mut().enumerate() {
                    *v += origin[i % 3] * scale;
                }
                values.insert(name.clone(), moved);
                row.insert("frame".into(), json!("model"));
            }
        }
        if let Some(previous) = dg.shift_remove("design_field_registrations") {
            dg.insert("physical_design_field_registrations".into(), previous);
        }
        if let Some(a) = self.assembly() {
            dg.insert("design_field_registrations".into(), json!({}));
            let layout = a.control_layout().map_err(|e| geo(&e))?;
            dg.insert("design_field_layouts".into(), json!({CONTROL_COORDINATE: layout}));
        } else {
            let step: Vec<f64> =
                (0..3).map(|a| view.domain_mm[a] / (as_f64(view.control_grid[a]) - 1.0)).collect();
            let basis = [[step[0], 0.0, 0.0], [0.0, step[1], 0.0], [0.0, 0.0, step[2]]];
            let registration =
                GridRegistration::new(view.control_grid, origin, basis, "node", "xyz", "model")
                    .map_err(|e| geo(&e))?
                    .to_wire();
            dg.insert("design_field_registrations".into(), json!({CONTROL_COORDINATE: registration}));
            dg.insert(
                "design_field_layouts".into(),
                json!({CONTROL_COORDINATE: {
                    "schema": "implexity-component-field-layout/1", "shape": self.control_shape(),
                    "component_axis": 0, "spatial_axes": [1, 2, 3],
                    "components": control_contract()["components"].clone()}}),
            );
        }
        if let Some(meta) = dg.get_mut("field_metadata").and_then(Value::as_object_mut) {
            for name in ["design_control", "design_material"] {
                if let Some(row) = meta.get_mut(name).and_then(Value::as_object_mut) {
                    row.insert(
                        "design_coordinate_representation".into(),
                        json!("decoded_physical_field_not_free_control_tensor"),
                    );
                    row.insert("source_design_coordinate".into(), json!(CONTROL_COORDINATE));
                }
            }
        }
        dg.insert(
            "geometry_result_frame".into(),
            json!({
                "schema": "implexity-geometry-result-frame/1", "map_id": self.identity,
                "source_frame": "provider_local", "target_frame": "model",
                "translation_mm": floats(&origin),
                "basis_rotation": [floats(&[1.0, 0.0, 0.0]), floats(&[0.0, 1.0, 0.0]), floats(&[0.0, 0.0, 1.0])],
                "position_field_semantics": "translate_positions_only; vectors_and_tensors_unchanged",
                "provider_registrations": originals,
            }),
        );
        Ok((dg, values))
    }


    pub fn evidence(&self, design: &NamedArrays, physical: &NamedArrays) -> CaeResult<Map<String, Value>> {
        let view = self.view()?;
        let mut result = Map::new();
        result.insert("schema".into(), self.spec["schema"].clone());
        result.insert("map_id".into(), json!(self.identity));
        result.insert("control_schema".into(), json!(CONTROL_SCHEMA));
        result.insert("design_state_id".into(), json!(design_identity(design)?));
        result.insert("physical_design_state_id".into(), json!(design_identity(physical)?));
        result.insert("derivative".into(), json!("complete_discrete_geometry_vjp"));
        result.insert("control_shape".into(), json!(self.control_shape()));
        result.insert("analysis_shape".into(), json!(view.analysis_grid));
        result.insert("analysis_sampling".into(), json!("trilinear_at_cell_centers_not_exact_cell_averages"));
        result.insert("origin_mm".into(), floats(&view.origin_mm));
        let constants: Vec<String> =
            self.spec["constants"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        result.insert("fixed_problem_constants".into(), json!(constants));
        if let Some(a) = self.assembly() {
            if let Value::Object(extra) = a.sampling_evidence() {
                result.extend(extra);
            }
        } else if let Some(l) = self.lattice() {
            result.insert("geometry_shape".into(), json!(l.spec.geometry_shape()));
            result.insert(
                "geometry_sampling_mode".into(),
                json!(if l.spec.geometry_grid.is_some() {
                    "explicit_geometry_grid"
                } else {
                    "legacy_analysis_dependent"
                }),
            );
        }
        if self.spec["schema"] == ASSEMBLY_MAP_SCHEMA {
            result.insert("phase_role".into(), self.spec["phase_role"].clone());
        }
        Ok(result)
    }
}


pub fn from_context(context: &Map<String, Value>) -> CaeResult<Option<GeometryDesignMap>> {
    match context.get("geometry_design_map") {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => GeometryDesignMap::new(raw).map(Some),
    }
}

