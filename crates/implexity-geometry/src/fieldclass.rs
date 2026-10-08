// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use crate::error::GeometryError;
use crate::pyfmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClassKind {
    Exact,
    Bound,
    Lipschitz,
    Implicit,
}

impl ClassKind {
    pub const ORDER: [Self; 4] = [Self::Exact, Self::Bound, Self::Lipschitz, Self::Implicit];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Exact => "EXACT",
            Self::Bound => "BOUND",
            Self::Lipschitz => "LIPSCHITZ",
            Self::Implicit => "IMPLICIT",
        }
    }

    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "EXACT" => Self::Exact,
            "BOUND" => Self::Bound,
            "LIPSCHITZ" => Self::Lipschitz,
            "IMPLICIT" => Self::Implicit,
            _ => return None,
        })
    }

    #[must_use]
    pub fn rank(self) -> usize {
        match self {
            Self::Exact => 0,
            Self::Bound => 1,
            Self::Lipschitz => 2,
            Self::Implicit => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldClass {
    kind: ClassKind,
    k: f64,
    measured: bool,
    samples: i64,
    note: String,
}

impl PartialEq for FieldClass {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && (self.k - other.k).abs() < 1e-12 && self.measured == other.measured
    }
}

impl FieldClass {

    pub fn new(
        kind: ClassKind,
        k: f64,
        measured: bool,
        samples: i64,
        note: &str,
    ) -> Result<Self, GeometryError> {
        let mut kind = kind;
        let mut k = k;
        match kind {
            ClassKind::Exact | ClassKind::Bound => {
                if (k - 1.0).abs() >= 1e-12 || k.is_nan() {
                    return Err(GeometryError::Value(format!(
                        "{} implies k == 1, got {}",
                        kind.name(),
                        pyfmt::g(k)
                    )));
                }
            }
            ClassKind::Lipschitz => {
                if !(k > 0.0) || !k.is_finite() {
                    return Err(GeometryError::Value(format!(
                        "LIPSCHITZ needs a finite positive k, got {}",
                        pyfmt::float_repr(k)
                    )));
                }
                if (k - 1.0).abs() < 1e-12 {
                    kind = ClassKind::Bound;
                    k = 1.0;
                }
            }
            ClassKind::Implicit => {}
        }
        Ok(Self { kind, k, measured, samples, note: note.to_string() })
    }


    pub fn from_name(
        kind: &str,
        k: f64,
        measured: bool,
        samples: i64,
        note: &str,
    ) -> Result<Self, GeometryError> {
        let Some(kd) = ClassKind::from_name(kind) else {
            return Err(GeometryError::Value(format!(
                "field class {}; expected one of EXACT, BOUND, LIPSCHITZ, IMPLICIT",
                pyfmt::str_repr(kind)
            )));
        };
        Self::new(kd, k, measured, samples, note)
    }

    fn raw(kind: ClassKind, k: f64, measured: bool, samples: i64, note: &str) -> Self {
        Self { kind, k, measured, samples, note: note.to_string() }
    }

    #[must_use]
    pub fn exact() -> Self {
        Self::raw(ClassKind::Exact, 1.0, false, 0, "")
    }

    #[must_use]
    pub fn bound() -> Self {
        Self::raw(ClassKind::Bound, 1.0, false, 0, "")
    }

    #[must_use]
    pub fn implicit() -> Self {
        Self::raw(ClassKind::Implicit, 1.0, false, 0, "")
    }


    pub fn lipschitz(k: f64, note: &str) -> Result<Self, GeometryError> {
        Self::new(ClassKind::Lipschitz, k, false, 0, note)
    }


    pub fn from_measurement(grad_max: f64, samples: i64, note: &str) -> Result<Self, GeometryError> {
        if samples < 1 {
            return Err(GeometryError::Value("a measurement needs at least one sample".into()));
        }
        Self::new(ClassKind::Lipschitz, grad_max, true, samples, note)
    }

    #[must_use]
    pub fn kind(&self) -> ClassKind {
        self.kind
    }

    #[must_use]
    pub fn k(&self) -> f64 {
        self.k
    }

    #[must_use]
    pub fn measured(&self) -> bool {
        self.measured
    }

    #[must_use]
    pub fn samples(&self) -> i64 {
        self.samples
    }

    #[must_use]
    pub fn note(&self) -> &str {
        &self.note
    }

    #[must_use]
    pub fn rank(&self) -> usize {
        self.kind.rank()
    }

    #[must_use]
    pub fn weaker_of(&self, other: &Self) -> Self {
        let (worse, best) = if self.rank() >= other.rank() { (self, other) } else { (other, self) };
        if worse.kind != ClassKind::Lipschitz {
            return Self::raw(
                worse.kind,
                1.0,
                worse.measured || (best.measured && best.kind != ClassKind::Exact),
                worse.samples.max(best.samples),
                &worse.note,
            );
        }
        let ka = if self.kind == ClassKind::Lipschitz { self.k } else { 1.0 };
        let kb = if other.kind == ClassKind::Lipschitz { other.k } else { 1.0 };
        let k = ka.max(kb);
        let measured = self.measured || other.measured;
        let samples = self.samples.max(other.samples);
        Self::new(ClassKind::Lipschitz, k, measured, samples, &worse.note)
            .unwrap_or_else(|_| Self::raw(ClassKind::Lipschitz, k, measured, samples, &worse.note))
    }


    pub fn scaled(&self, s: f64, note: &str) -> Result<Self, GeometryError> {
        if s <= 0.0 || !s.is_finite() {
            return Err(GeometryError::Value(format!(
                "scale must be finite and positive, got {}",
                pyfmt::float_repr(s)
            )));
        }
        if self.kind == ClassKind::Implicit || (s - 1.0).abs() < 1e-12 {
            return Ok(self.clone());
        }
        let base = if self.kind == ClassKind::Lipschitz { self.k } else { 1.0 };
        Self::new(
            ClassKind::Lipschitz,
            base * s,
            self.measured,
            self.samples,
            if note.is_empty() { &self.note } else { note },
        )
    }


    pub fn demoted(&self, kind: ClassKind, note: &str) -> Result<Self, GeometryError> {
        let k = if kind == ClassKind::Lipschitz { self.k } else { 1.0 };
        let out =
            Self::new(kind, k, self.measured, self.samples, if note.is_empty() { &self.note } else { note })?;
        if out.rank() < self.rank() {
            return Err(GeometryError::Value(format!(
                "demoted() may not strengthen {} -> {}",
                self.kind.name(),
                kind.name()
            )));
        }
        Ok(out)
    }

    #[must_use]
    pub fn safe_step_factor(&self) -> Option<f64> {
        match self.kind {
            ClassKind::Exact | ClassKind::Bound => Some(1.0),
            ClassKind::Lipschitz => Some(1.0 / self.k),
            ClassKind::Implicit => None,
        }
    }

    #[must_use]
    pub fn surface_free(&self, f_abs: f64, cell_diagonal: f64) -> bool {
        self.safe_step_factor().is_some_and(|fac| f_abs * fac > 0.5 * cell_diagonal)
    }

    #[must_use]
    pub fn offset_is_exact(&self) -> bool {
        self.kind == ClassKind::Exact
    }

    #[must_use]
    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("kind".into(), serde_json::Value::from(self.kind.name()));
        m.insert("k".into(), crate::value::json_f64(self.k));
        if self.measured {
            m.insert("measured".into(), serde_json::Value::from(true));
            m.insert("samples".into(), serde_json::Value::from(self.samples));
        }
        if !self.note.is_empty() {
            m.insert("note".into(), serde_json::Value::from(self.note.clone()));
        }
        serde_json::Value::Object(m)
    }

    #[must_use]
    pub fn as_pyobj(&self) -> pyfmt::PyObj {
        use pyfmt::PyObj;
        let mut items = vec![
            ("kind".to_string(), PyObj::Str(self.kind.name().into())),
            ("k".into(), PyObj::Float(self.k)),
        ];
        if self.measured {
            items.push(("measured".into(), PyObj::Bool(true)));
            items.push(("samples".into(), PyObj::Int(self.samples)));
        }
        if !self.note.is_empty() {
            items.push(("note".into(), PyObj::Str(self.note.clone())));
        }
        PyObj::Dict(items)
    }


    pub fn from_json(d: &serde_json::Value) -> Result<Self, GeometryError> {
        let Some(obj) = d.as_object() else {
            return Err(GeometryError::Value("a field class must be an object".into()));
        };
        let Some(kind) = obj.get("kind") else {
            return Err(GeometryError::Value("'kind'".into()));
        };
        let kind = kind.as_str().map_or_else(|| kind.to_string(), str::to_string);
        let k = match obj.get("k") {
            None => 1.0,
            Some(v) => v
                .as_f64()
                .or_else(|| v.as_bool().map(|b| f64::from(u8::from(b))))
                .ok_or_else(|| GeometryError::Value(format!("could not convert {v} to float")))?,
        };
        let measured = obj.get("measured").is_some_and(json_truthy);
        let samples = match obj.get("samples") {
            None => 0,
            Some(v) => v
                .as_i64()
                .or_else(|| {
                    v.as_f64().map(|f| {
                        #[allow(clippy::cast_possible_truncation)]
                        let t = f.trunc() as i64;
                        t
                    })
                })
                .unwrap_or(0),
        };
        let note = match obj.get("note") {
            None => String::new(),
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
        };
        Self::from_name(&kind, k, measured, samples, &note)
    }

    #[must_use]
    pub fn repr(&self) -> String {
        let body = if self.kind == ClassKind::Lipschitz {
            format!(
                "LIPSCHITZ(k={}{})",
                pyfmt::g(self.k),
                if self.measured {
                    format!(", measured over {} samples", self.samples)
                } else {
                    String::new()
                }
            )
        } else {
            self.kind.name().to_string()
        };
        format!("<FieldClass {body}>")
    }
}

fn json_truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}


pub fn require(
    fc: &FieldClass,
    at_least: ClassKind,
    why: &str,
    allow_measured: bool,
) -> Result<FieldClass, GeometryError> {
    if fc.rank() > at_least.rank() {
        return Err(GeometryError::BoundViolation(format!(
            "{why} needs a field that is at least {}; this one is {}",
            at_least.name(),
            fc.kind.name()
        )));
    }
    if fc.measured && !allow_measured {
        return Err(GeometryError::BoundViolation(format!(
            "{why} needs a PROVEN bound; this field's Lipschitz constant {} was MEASURED over {} samples, which is \
             evidence about those samples and not a guarantee (pass allow_measured=True to accept it)",
            pyfmt::g(fc.k),
            fc.samples
        )));
    }
    Ok(fc.clone())
}

