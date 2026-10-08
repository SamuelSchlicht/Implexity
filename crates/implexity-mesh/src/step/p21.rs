// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::MeshError;
use crate::topology::{Vec3, cross, dot, norm};

use super::brep::{Bound, Brep, Face, Shell, Solid, plane_frame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Ap214,
    Ap203,
    Ap242,
}

impl Protocol {
    #[must_use]
    pub fn from_occt_name(name: &str) -> Option<Self> {
        match name {
            "AP214IS" => Some(Self::Ap214),
            "AP203" => Some(Self::Ap203),
            "AP242DIS" => Some(Self::Ap242),
            _ => None,
        }
    }

    fn file_schema(self) -> &'static str {
        match self {
            Self::Ap214 => "AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }",
            Self::Ap203 => "CONFIG_CONTROL_DESIGN",
            Self::Ap242 => "AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF { 1 0 10303 442 1 1 4 }",
        }
    }
}

#[derive(Clone, Debug)]
pub struct HeaderInfo<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub author: &'a str,
    pub organization: &'a str,
    pub originating_system: &'a str,
    pub timestamp: (i64, u32, u32, u32, u32, u32),
}

pub const PREPROCESSOR: &str = "implexity-mesh native ISO 10303-21 writer";

#[must_use]
pub fn string_literal(s: &str) -> String {
    let mut out = String::from("'");
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (' '..='~').contains(&c) {
            match c {
                '\'' => out.push_str("''"),
                '\\' => out.push_str("\\\\"),
                _ => out.push(c),
            }
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && !(' '..='~').contains(&chars[j]) {
            j += 1;
        }
        let run = &chars[i..j];
        if run.iter().all(|&c| u32::from(c) <= 0xFFFF) {
            out.push_str("\\X2\\");
            for &c in run {
                let _ = write!(out, "{:04X}", u32::from(c));
            }
        } else {
            out.push_str("\\X4\\");
            for &c in run {
                let _ = write!(out, "{:08X}", u32::from(c));
            }
        }
        out.push_str("\\X0\\");
        i = j;
    }
    out.push('\'');
    out
}

#[must_use]
pub fn real(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.".into() } else { "0.".into() };
    }
    let s = format!("{x:e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let mant = if mant.contains('.') { mant.to_string() } else { format!("{mant}.") };
    if exp == "0" { mant } else { format!("{mant}E{exp}") }
}

fn b(v: bool) -> &'static str {
    if v { ".T." } else { ".F." }
}

struct Writer {
    lines: Vec<String>,
}

impl Writer {
    fn reserve(&mut self) -> usize {
        self.lines.push(String::new());
        self.lines.len()
    }
    fn set(&mut self, id: usize, text: String) {
        self.lines[id - 1] = text;
    }
    fn add(&mut self, text: String) -> usize {
        self.lines.push(text);
        self.lines.len()
    }
    fn point(&mut self, p: [f64; 3]) -> usize {
        self.add(format!("CARTESIAN_POINT('',({},{},{}))", real(p[0]), real(p[1]), real(p[2])))
    }
    fn direction(&mut self, d: [f64; 3]) -> usize {
        self.add(format!("DIRECTION('',({},{},{}))", real(d[0]), real(d[1]), real(d[2])))
    }
}

fn unit(v: Vec3) -> Vec3 {
    let l = norm(v);
    [v[0] / l, v[1] / l, v[2] / l]
}



#[allow(clippy::too_many_lines)]
pub fn write_brep(
    brep: &Brep,
    protocol: Protocol,
    header: &HeaderInfo<'_>,
    pcurves: bool,
) -> Result<String, MeshError> {
    if brep.solids.is_empty() {
        return Err(MeshError::invalid("no solid to write"));
    }
    let mut w = Writer { lines: Vec::new() };
    let (apd, ac, sdr, pds, pd, pdf, prod, pc, pdc, absr) = (
        w.reserve(),
        w.reserve(),
        w.reserve(),
        w.reserve(),
        w.reserve(),
        w.reserve(),
        w.reserve(),
        w.reserve(),
        w.reserve(),
        w.reserve(),
    );
    let ctx = w.reserve();
    let (y, mo, d, hh, mi, ss) = header.timestamp;
    let _ = ss;
    let name = string_literal(header.name);
    match protocol {
        Protocol::Ap214 => {
            w.set(
                apd,
                format!(
                    "APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,#{ac})"
                ),
            );
            w.set(ac, "APPLICATION_CONTEXT('core data for automotive mechanical design processes')".into());
        }
        Protocol::Ap203 => {
            w.set(apd, format!("APPLICATION_PROTOCOL_DEFINITION('international standard','config_control_design',1994,#{ac})"));
            w.set(ac, "APPLICATION_CONTEXT('configuration controlled 3D designs of mechanical parts and assemblies')".into());
        }
        Protocol::Ap242 => {
            w.set(
                apd,
                format!("APPLICATION_PROTOCOL_DEFINITION('international standard','ap242_managed_model_based_3d_engineering',2013,#{ac})"),
            );
            w.set(ac, "APPLICATION_CONTEXT('Managed model based 3d engineering')".into());
        }
    }
    w.set(sdr, format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{absr})"));
    w.set(pds, format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
    w.set(pd, format!("PRODUCT_DEFINITION('design','',#{pdf},#{pdc})"));
    if protocol == Protocol::Ap203 {
        w.set(pdf, format!("PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE('','',#{prod},.NOT_KNOWN.)"));
        w.set(pc, format!("MECHANICAL_CONTEXT('',#{ac},'mechanical')"));
        w.set(pdc, format!("DESIGN_CONTEXT('',#{ac},'design')"));
    } else {
        w.set(pdf, format!("PRODUCT_DEFINITION_FORMATION('','',#{prod})"));
        w.set(pc, format!("PRODUCT_CONTEXT('',#{ac},'mechanical')"));
        w.set(pdc, format!("PRODUCT_DEFINITION_CONTEXT('part definition',#{ac},'design')"));
    }
    w.set(prod, format!("PRODUCT({name},{name},'',(#{pc}))"));
    let origin = w.point([0.0; 3]);
    let (oz, ox) = (w.direction([0.0, 0.0, 1.0]), w.direction([1.0, 0.0, 0.0]));
    let placement = w.add(format!("AXIS2_PLACEMENT_3D('',#{origin},#{oz},#{ox})"));

    let mut plane_of = vec![0usize; brep.faces.len()];
    let mut frames = vec![([0.0; 3], [0.0; 3], [0.0; 3]); brep.faces.len()];
    let mut face_users: HashMap<usize, Vec<usize>> = HashMap::new();
    for (f, face) in brep.faces.iter().enumerate() {
        let n = unit(face.normal);
        let (u, v) = plane_frame(n);
        let p = w.point(face.point);
        let (dn, du) = (w.direction(n), w.direction(u));
        let ax = w.add(format!("AXIS2_PLACEMENT_3D('',#{p},#{dn},#{du})"));
        plane_of[f] = w.add(format!("PLANE('',#{ax})"));
        frames[f] = (face.point, u, v);
        for bound in &face.bounds {
            for &(e, _) in &bound.edges {
                let users = face_users.entry(e).or_default();
                if !users.contains(&f) {
                    users.push(f);
                }
            }
        }
    }
    let ctx2 = pcurves.then(|| {
        w.add("( GEOMETRIC_REPRESENTATION_CONTEXT(2) PARAMETRIC_REPRESENTATION_CONTEXT() REPRESENTATION_CONTEXT('2D SPACE','') )".into())
    });
    let mut vertex_id: HashMap<usize, usize> = HashMap::new();
    let mut edge_id: HashMap<usize, usize> = HashMap::new();
    let mut vertex = |w: &mut Writer, i: usize| -> usize {
        *vertex_id.entry(i).or_insert_with(|| {
            let p = w.point(brep.points[i]);
            w.add(format!("VERTEX_POINT('',#{p})"))
        })
    };
    let mut edge = |w: &mut Writer, e: usize| -> usize {
        if let Some(&id) = edge_id.get(&e) {
            return id;
        }
        let [a, bb] = brep.edges[e];
        let (va, vb) = (vertex(w, a), vertex(w, bb));
        let (pa, pb) = (brep.points[a], brep.points[bb]);
        let dir = unit([pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]]);
        let lp = w.point(pa);
        let ld = w.direction(dir);
        let lv = w.add(format!("VECTOR('',#{ld},1.)"));
        let line = w.add(format!("LINE('',#{lp},#{lv})"));
        let geometry = if let Some(ctx2) = ctx2 {
            let mut pcs = Vec::new();
            for &f in face_users.get(&e).map_or(&[][..], Vec::as_slice) {
                let (o, u, v) = frames[f];
                let rel = [pa[0] - o[0], pa[1] - o[1], pa[2] - o[2]];
                let (x, y) = (dot(rel, u), dot(rel, v));
                let (dx, dy) = (dot(dir, u), dot(dir, v));
                let l2 = dx.hypot(dy);
                let p2 = w.add(format!("CARTESIAN_POINT('',({},{}))", real(x), real(y)));
                let d2 = w.add(format!("DIRECTION('',({},{}))", real(dx / l2), real(dy / l2)));
                let v2 = w.add(format!("VECTOR('',#{d2},1.)"));
                let l2d = w.add(format!("LINE('',#{p2},#{v2})"));
                let dr = w.add(format!("DEFINITIONAL_REPRESENTATION('',(#{l2d}),#{ctx2})"));
                pcs.push(w.add(format!("PCURVE('',#{},#{dr})", plane_of[f])));
            }
            let list: Vec<String> = pcs.iter().map(|i| format!("#{i}")).collect();
            w.add(format!("SURFACE_CURVE('',#{line},({}),.CURVE_3D.)", list.join(",")))
        } else {
            line
        };
        let id = w.add(format!("EDGE_CURVE('',#{va},#{vb},#{geometry},.T.)"));
        edge_id.insert(e, id);
        id
    };
    let mut write_shell = |w: &mut Writer, shell: &Shell, void: bool| -> usize {
        let mut face_ids = Vec::new();
        for &f in &shell.faces {
            let face = &brep.faces[f];
            let flip = shell.reversed != void;
            let mut bound_ids = Vec::new();
            for (k, bound) in face.bounds.iter().enumerate() {
                let mut oe = Vec::new();
                for &(e, fwd) in &bound.edges {
                    let ec = edge(w, e);
                    oe.push(w.add(format!("ORIENTED_EDGE('',*,*,#{ec},{})", b(fwd))));
                }
                let list: Vec<String> = oe.iter().map(|i| format!("#{i}")).collect();
                let lp = w.add(format!("EDGE_LOOP('',({}))", list.join(",")));
                let kind = if k == 0 { "FACE_OUTER_BOUND" } else { "FACE_BOUND" };
                bound_ids.push(w.add(format!("{kind}('',#{lp},{})", b(bound.orientation != flip))));
            }
            let list: Vec<String> = bound_ids.iter().map(|i| format!("#{i}")).collect();
            face_ids.push(w.add(format!(
                "ADVANCED_FACE('',({}),#{},{})",
                list.join(","),
                plane_of[f],
                b(face.same_sense != flip)
            )));
        }
        let list: Vec<String> = face_ids.iter().map(|i| format!("#{i}")).collect();
        let cs = w.add(format!("CLOSED_SHELL('',({}))", list.join(",")));
        if void { w.add(format!("ORIENTED_CLOSED_SHELL('',*,#{cs},.F.)")) } else { cs }
    };
    let mut items = vec![placement];
    for solid in &brep.solids {
        let outer = write_shell(&mut w, &brep.shells[solid.shells[0]], false);
        if solid.shells.len() == 1 {
            items.push(w.add(format!("MANIFOLD_SOLID_BREP('',#{outer})")));
        } else {
            let voids: Vec<String> = solid.shells[1..]
                .iter()
                .map(|&s| format!("#{}", write_shell(&mut w, &brep.shells[s], true)))
                .collect();
            items.push(w.add(format!("BREP_WITH_VOIDS('',#{outer},({}))", voids.join(","))));
        }
    }
    let list: Vec<String> = items.iter().map(|i| format!("#{i}")).collect();
    w.set(absr, format!("ADVANCED_BREP_SHAPE_REPRESENTATION('',({}),#{ctx})", list.join(",")));
    let mm = w.add("( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) )".into());
    let rad = w.add("( NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.) )".into());
    let sr = w.add("( NAMED_UNIT(*) SI_UNIT($,.STERADIAN.) SOLID_ANGLE_UNIT() )".into());
    let unc = w.add(format!(
        "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-07),#{mm},'distance_accuracy_value','confusion accuracy')"
    ));
    w.set(
        ctx,
        format!(
            "( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc})) \
             GLOBAL_UNIT_ASSIGNED_CONTEXT((#{mm},#{rad},#{sr})) REPRESENTATION_CONTEXT('Context #1',\
             '3D Context with UNIT and UNCERTAINTY') )"
        ),
    );
    if protocol == Protocol::Ap203 {
        let cat = w.add(format!("PRODUCT_RELATED_PRODUCT_CATEGORY('detail',$,(#{prod}))"));
        let part = w.reserve();
        w.add(format!("PRODUCT_CATEGORY_RELATIONSHIP('','',#{part},#{cat})"));
        w.set(part, "PRODUCT_CATEGORY('part',$)".into());
        let author = string_literal(header.author);
        let org = string_literal(header.organization);
        let person = w.add(format!("PERSON({author},'',{author},$,$,$)"));
        let organization = w.add(format!("ORGANIZATION({org},{org},'')"));
        let po = w.add(format!("PERSON_AND_ORGANIZATION(#{person},#{organization})"));
        let assign = |w: &mut Writer, role: &str, items: &str| {
            let r = w.add(format!("PERSON_AND_ORGANIZATION_ROLE('{role}')"));
            w.add(format!("CC_DESIGN_PERSON_AND_ORGANIZATION_ASSIGNMENT(#{po},#{r},({items}))"));
        };
        assign(&mut w, "creator", &format!("#{pdf},#{pd}"));
        assign(&mut w, "design_owner", &format!("#{prod}"));
        assign(&mut w, "design_supplier", &format!("#{pdf}"));
        let level = w.add("SECURITY_CLASSIFICATION_LEVEL('unclassified')".into());
        let sec = w.add(format!("SECURITY_CLASSIFICATION('','',#{level})"));
        assign(&mut w, "classification_officer", &format!("#{sec}"));
        w.add(format!("CC_DESIGN_SECURITY_CLASSIFICATION(#{sec},(#{pdf}))"));
        let date = w.add(format!("CALENDAR_DATE({y},{d},{mo})"));
        let offset = w.add("COORDINATED_UNIVERSAL_TIME_OFFSET(0,$,.EXACT.)".into());
        let time = w.add(format!("LOCAL_TIME({hh},{mi},$,#{offset})"));
        let dt = w.add(format!("DATE_AND_TIME(#{date},#{time})"));
        let created = w.add("DATE_TIME_ROLE('creation_date')".into());
        w.add(format!("CC_DESIGN_DATE_AND_TIME_ASSIGNMENT(#{dt},#{created},(#{pd}))"));
        let classified = w.add("DATE_TIME_ROLE('classification_date')".into());
        w.add(format!("CC_DESIGN_DATE_AND_TIME_ASSIGNMENT(#{dt},#{classified},(#{sec}))"));
        let status = w.add("APPROVAL_STATUS('not_yet_approved')".into());
        let approval = w.add(format!("APPROVAL(#{status},'')"));
        w.add(format!("CC_DESIGN_APPROVAL(#{approval},(#{pdf},#{pd},#{sec}))"));
        let role = w.add("APPROVAL_ROLE('approver')".into());
        w.add(format!("APPROVAL_PERSON_ORGANIZATION(#{po},#{approval},#{role})"));
    } else {
        w.add(format!("PRODUCT_RELATED_PRODUCT_CATEGORY('part',$,(#{prod}))"));
    }
    let mut out = String::with_capacity(w.lines.iter().map(|l| l.len() + 12).sum::<usize>() + 1024);
    out.push_str("ISO-10303-21;\nHEADER;\n");
    let _ = writeln!(out, "FILE_DESCRIPTION(({}),'2;1');", string_literal(header.description));
    let _ = writeln!(
        out,
        "FILE_NAME({name},'{y:04}-{mo:02}-{d:02}T{hh:02}:{mi:02}:{ss:02}',({}),({}),{},{},'');",
        string_literal(header.author),
        string_literal(header.organization),
        string_literal(PREPROCESSOR),
        string_literal(header.originating_system),
    );
    let _ = writeln!(out, "FILE_SCHEMA(('{}'));", protocol.file_schema());
    out.push_str("ENDSEC;\nDATA;\n");
    for (i, line) in w.lines.iter().enumerate() {
        let _ = writeln!(out, "#{} = {line};", i + 1);
    }
    out.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    Ref(usize),
    Str(String),
    Enum(String),
    Int(i64),
    Real(f64),
    List(Vec<Param>),
    Typed(String, Box<Param>),
    Unset,
    Derived,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub parts: Vec<(String, Vec<Param>)>,
}

impl Instance {
    fn simple(&self) -> Option<(&str, &[Param])> {
        (self.parts.len() == 1).then(|| (self.parts[0].0.as_str(), self.parts[0].1.as_slice()))
    }
    fn part(&self, name: &str) -> Option<&[Param]> {
        self.parts.iter().find(|(n, _)| n == name).map(|(_, p)| p.as_slice())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Exchange {
    pub header: Vec<(String, Vec<Param>)>,
    pub data: BTreeMap<usize, Instance>,
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

fn perr(msg: &str) -> MeshError {
    MeshError::invalid(format!("STEP parse error: {msg}"))
}

impl Lexer<'_> {
    fn skip(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"/*") {
                match self.s[self.i + 2..].windows(2).position(|w| w == b"*/") {
                    Some(p) => self.i += p + 4,
                    None => self.i = self.s.len(),
                }
            } else {
                return;
            }
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.s.get(self.i).copied()
    }
    fn expect(&mut self, c: u8) -> Result<(), MeshError> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(perr(&format!("expected '{}' at byte {}", c as char, self.i)))
        }
    }
    fn keyword(&mut self) -> Result<String, MeshError> {
        self.skip();
        let st = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_' || self.s[self.i] == b'-')
        {
            self.i += 1;
        }
        if st == self.i {
            return Err(perr(&format!("expected a keyword at byte {st}")));
        }
        Ok(String::from_utf8_lossy(&self.s[st..self.i]).to_ascii_uppercase())
    }
    fn integer(&mut self) -> usize {
        let st = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[st..self.i]).ok().and_then(|t| t.parse().ok()).unwrap_or(0)
    }
    fn string(&mut self) -> Result<String, MeshError> {
        self.expect(b'\'')?;
        let mut raw = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return Err(perr("unterminated string")) };
            self.i += 1;
            if c == b'\'' {
                if self.s.get(self.i) == Some(&b'\'') {
                    raw.push(b'\'');
                    self.i += 1;
                    continue;
                }
                break;
            }
            raw.push(c);
        }
        Ok(decode_string(&raw))
    }
    fn params(&mut self) -> Result<Vec<Param>, MeshError> {
        self.expect(b'(')?;
        let mut out = Vec::new();
        if self.peek() == Some(b')') {
            self.i += 1;
            return Ok(out);
        }
        loop {
            out.push(self.param()?);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b')') => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => return Err(perr(&format!("expected ',' or ')' at byte {}", self.i))),
            }
        }
    }
    fn param(&mut self) -> Result<Param, MeshError> {
        match self.peek() {
            Some(b'#') => {
                self.i += 1;
                Ok(Param::Ref(self.integer()))
            }
            Some(b'\'') => Ok(Param::Str(self.string()?)),
            Some(b'.') => {
                self.i += 1;
                let st = self.i;
                while self.i < self.s.len() && self.s[self.i] != b'.' {
                    self.i += 1;
                }
                let name = String::from_utf8_lossy(&self.s[st..self.i]).to_string();
                self.i += 1;
                Ok(Param::Enum(name))
            }
            Some(b'$') => {
                self.i += 1;
                Ok(Param::Unset)
            }
            Some(b'*') => {
                self.i += 1;
                Ok(Param::Derived)
            }
            Some(b'(') => Ok(Param::List(self.params()?)),
            Some(b'"') => {
                self.i += 1;
                let st = self.i;
                while self.i < self.s.len() && self.s[self.i] != b'"' {
                    self.i += 1;
                }
                let t = String::from_utf8_lossy(&self.s[st..self.i]).to_string();
                self.i += 1;
                Ok(Param::Str(t))
            }
            Some(c) if c == b'-' || c == b'+' || c.is_ascii_digit() => {
                let st = self.i;
                while self.i < self.s.len()
                    && matches!(self.s[self.i], b'0'..=b'9' | b'.' | b'E' | b'e' | b'+' | b'-')
                {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[st..self.i]).map_err(|_| perr("bad number"))?;
                if t.contains('.') {
                    let norm = if t.ends_with('.') {
                        format!("{t}0")
                    } else {
                        t.replace(".E", ".0E").replace(".e", ".0e")
                    };
                    norm.parse().map(Param::Real).map_err(|_| perr(&format!("bad real {t}")))
                } else {
                    t.parse().map(Param::Int).map_err(|_| perr(&format!("bad integer {t}")))
                }
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let name = self.keyword()?;
                let mut inner = self.params()?;
                let v = if inner.len() == 1 { inner.remove(0) } else { Param::List(inner) };
                Ok(Param::Typed(name, Box::new(v)))
            }
            _ => Err(perr(&format!("unexpected byte at {}", self.i))),
        }
    }
}

fn decode_string(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw).to_string();
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(p) = rest.find('\\') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        if let Some(r) = rest.strip_prefix("\\\\") {
            out.push('\\');
            rest = r;
        } else if let Some(r) = rest.strip_prefix("\\X2\\").or_else(|| rest.strip_prefix("\\X4\\")) {
            let width = if rest.starts_with("\\X2\\") { 4 } else { 8 };
            let end = r.find("\\X0\\").unwrap_or(r.len());
            let hex = &r[..end];
            let mut units = Vec::new();
            for k in (0..hex.len()).step_by(width) {
                if let Some(Ok(v)) = hex.get(k..k + width).map(|h| u32::from_str_radix(h, 16)) {
                    units.push(v);
                }
            }
            if width == 4 {
                let u16s: Vec<u16> = units.iter().filter_map(|&u| u16::try_from(u).ok()).collect();
                out.push_str(&String::from_utf16_lossy(&u16s));
            } else {
                out.extend(units.iter().filter_map(|&u| char::from_u32(u)));
            }
            rest = r.get(end + 4..).unwrap_or("");
        } else if let Some(r) = rest.strip_prefix("\\X\\") {
            if let Some(Ok(v)) = r.get(..2).map(|h| u8::from_str_radix(h, 16)) {
                out.push(char::from(v));
            }
            rest = r.get(2..).unwrap_or("");
        } else {
            out.push('\\');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}



pub fn parse(text: &str) -> Result<Exchange, MeshError> {
    let mut lx = Lexer { s: text.as_bytes(), i: 0 };
    let magic = lx.keyword()?;
    if magic != "ISO-10303-21" {
        return Err(perr("missing ISO-10303-21 magic"));
    }
    lx.expect(b';')?;
    let mut ex = Exchange::default();
    if lx.keyword()? != "HEADER" {
        return Err(perr("missing HEADER section"));
    }
    lx.expect(b';')?;
    loop {
        let k = lx.keyword()?;
        if k == "ENDSEC" {
            lx.expect(b';')?;
            break;
        }
        let p = lx.params()?;
        lx.expect(b';')?;
        ex.header.push((k, p));
    }
    loop {
        let k = lx.keyword()?;
        if k == "END-ISO-10303-21" {
            break;
        }
        if k != "DATA" {
            return Err(perr(&format!("unexpected section {k}")));
        }
        if lx.peek() == Some(b'(') {
            let _ = lx.params()?;
        }
        lx.expect(b';')?;
        loop {
            if lx.peek() != Some(b'#') {
                let k = lx.keyword()?;
                if k != "ENDSEC" {
                    return Err(perr(&format!("unexpected keyword {k} in DATA")));
                }
                lx.expect(b';')?;
                break;
            }
            lx.i += 1;
            let id = lx.integer();
            lx.expect(b'=')?;
            let inst = if lx.peek() == Some(b'(') {
                lx.i += 1;
                let mut parts = Vec::new();
                while lx.peek() != Some(b')') {
                    let n = lx.keyword()?;
                    parts.push((n, lx.params()?));
                }
                lx.i += 1;
                Instance { parts }
            } else {
                let n = lx.keyword()?;
                Instance { parts: vec![(n, lx.params()?)] }
            };
            lx.expect(b';')?;
            ex.data.insert(id, inst);
        }
    }
    Ok(ex)
}

struct Reader<'a> {
    ex: &'a Exchange,
    scale: f64,
    brep: Brep,
    points: HashMap<usize, usize>,
    edges: HashMap<usize, usize>,
}

fn rerr(msg: impl std::fmt::Display) -> MeshError {
    MeshError::invalid(format!("STEP read: {msg}"))
}

impl Reader<'_> {
    fn get(&self, id: usize, want: &[&str]) -> Result<(&str, &[Param]), MeshError> {
        let inst = self.ex.data.get(&id).ok_or_else(|| rerr(format!("#{id} is missing")))?;
        let (n, p) = inst.simple().ok_or_else(|| rerr(format!("#{id} is a complex instance")))?;
        if !want.contains(&n) {
            return Err(rerr(format!("#{id} is {n}, expected {}", want.join("/"))));
        }
        Ok((n, p))
    }
    fn reference(p: Option<&Param>) -> Result<usize, MeshError> {
        match p {
            Some(Param::Ref(r)) => Ok(*r),
            other => Err(rerr(format!("expected an instance reference, found {other:?}"))),
        }
    }
    fn list(p: Option<&Param>) -> Result<&[Param], MeshError> {
        match p {
            Some(Param::List(v)) => Ok(v),
            other => Err(rerr(format!("expected a list, found {other:?}"))),
        }
    }
    fn logical(p: Option<&Param>) -> Result<bool, MeshError> {
        match p {
            Some(Param::Enum(e)) if e == "T" => Ok(true),
            Some(Param::Enum(e)) if e == "F" => Ok(false),
            other => Err(rerr(format!("expected .T./.F., found {other:?}"))),
        }
    }
    fn coords(&self, id: usize, want: &str) -> Result<Vec<f64>, MeshError> {
        let (_, p) = self.get(id, &[want])?;
        Self::list(p.get(1))?
            .iter()
            .map(|v| match v {
                Param::Real(x) => Ok(*x),
                #[allow(clippy::cast_precision_loss)]
                Param::Int(i) => Ok(*i as f64),
                other => Err(rerr(format!("non-numeric coordinate {other:?}"))),
            })
            .collect()
    }
    fn vec3(&self, id: usize, want: &str, scale: f64) -> Result<Vec3, MeshError> {
        let c = self.coords(id, want)?;
        if c.len() != 3 {
            return Err(rerr(format!("#{id} is not three-dimensional")));
        }
        Ok([c[0] * scale, c[1] * scale, c[2] * scale])
    }
    fn vertex(&mut self, id: usize) -> Result<usize, MeshError> {
        if let Some(&v) = self.points.get(&id) {
            return Ok(v);
        }
        let (_, p) = self.get(id, &["VERTEX_POINT"])?;
        let pt = Self::reference(p.get(1))?;
        let xyz = self.vec3(pt, "CARTESIAN_POINT", self.scale)?;
        self.brep.points.push(xyz);
        let v = self.brep.points.len() - 1;
        self.points.insert(id, v);
        Ok(v)
    }
    fn edge(&mut self, id: usize) -> Result<usize, MeshError> {
        if let Some(&e) = self.edges.get(&id) {
            return Ok(e);
        }
        let (_, p) = self.get(id, &["EDGE_CURVE"])?;
        let (v1, v2, geom) =
            (Self::reference(p.get(1))?, Self::reference(p.get(2))?, Self::reference(p.get(3))?);
        let (kind, gp) = self.get(geom, &["LINE", "SURFACE_CURVE", "SEAM_CURVE", "POLYLINE"])?;
        if kind != "LINE" {
            let curve = Self::reference(gp.get(1))?;
            self.get(curve, &["LINE"]).map_err(|_| {
                rerr(format!("edge #{id} is not straight: only planar polyhedral solids are read natively"))
            })?;
        }
        let (a, b) = (self.vertex(v1)?, self.vertex(v2)?);
        self.brep.edges.push([a, b]);
        let e = self.brep.edges.len() - 1;
        self.edges.insert(id, e);
        Ok(e)
    }
    fn face(&mut self, id: usize) -> Result<usize, MeshError> {
        let (_, p) = self.get(id, &["ADVANCED_FACE", "FACE_SURFACE"])?;
        let bounds = Self::list(p.get(1))?.to_vec();
        let surf = Self::reference(p.get(2))?;
        let same_sense = Self::logical(p.get(3))?;
        let (_, sp) = self.get(surf, &["PLANE"]).map_err(|_| {
            rerr(format!("face #{id} is not planar: only planar polyhedral solids are read natively"))
        })?;
        let ax = Self::reference(sp.get(1))?;
        let (_, ap) = self.get(ax, &["AXIS2_PLACEMENT_3D"])?;
        let loc = self.vec3(Self::reference(ap.get(1))?, "CARTESIAN_POINT", self.scale)?;
        let normal = match ap.get(2) {
            Some(Param::Ref(d)) => unit(self.vec3(*d, "DIRECTION", 1.0)?),
            _ => [0.0, 0.0, 1.0],
        };
        let mut out_bounds: Vec<(bool, Bound)> = Vec::new();
        for bref in &bounds {
            let bid = Self::reference(Some(bref))?;
            let (bk, bp) = self.get(bid, &["FACE_OUTER_BOUND", "FACE_BOUND"])?;
            let outer = bk == "FACE_OUTER_BOUND";
            let lid = Self::reference(bp.get(1))?;
            let orientation = Self::logical(bp.get(2))?;
            let (_, lp) = self.get(lid, &["EDGE_LOOP"])?;
            let oes = Self::list(lp.get(1))?.to_vec();
            let mut edges = Vec::new();
            for oe in &oes {
                let oid = Self::reference(Some(oe))?;
                let (_, op) = self.get(oid, &["ORIENTED_EDGE"])?;
                let ec = Self::reference(op.get(3))?;
                let fwd = Self::logical(op.get(4))?;
                edges.push((self.edge(ec)?, fwd));
            }
            out_bounds.push((outer, Bound { edges, orientation }));
        }
        let mut face = Face { point: loc, normal, same_sense, bounds: Vec::new() };

        let outer_idx = out_bounds.iter().position(|(o, _)| *o).unwrap_or_else(|| {
            let shell = Shell::default();
            let n = Brep::effective_normal(&shell, &face);
            out_bounds
                .iter()
                .position(|(_, b)| {
                    let lp = self.brep.loop_vertices(&shell, b);
                    signed_area(&self.brep.points, &lp, n) > 0.0
                })
                .unwrap_or(0)
        });
        let mut bounds_sorted = vec![out_bounds[outer_idx].1.clone()];
        bounds_sorted.extend(
            out_bounds.iter().enumerate().filter(|(i, _)| *i != outer_idx).map(|(_, (_, b))| b.clone()),
        );
        face.bounds = bounds_sorted;
        self.brep.faces.push(face);
        Ok(self.brep.faces.len() - 1)
    }
    fn shell(&mut self, id: usize) -> Result<usize, MeshError> {
        let (kind, p) = self.get(id, &["CLOSED_SHELL", "ORIENTED_CLOSED_SHELL", "OPEN_SHELL"])?;
        let (target, reversed) = if kind == "ORIENTED_CLOSED_SHELL" {
            (Self::reference(p.get(2))?, !Self::logical(p.get(3))?)
        } else {
            (id, false)
        };
        let (_, sp) = self.get(target, &["CLOSED_SHELL", "OPEN_SHELL"])?;
        let refs = Self::list(sp.get(1))?.to_vec();
        let mut faces = Vec::new();
        for r in &refs {
            faces.push(self.face(Self::reference(Some(r))?)?);
        }
        self.brep.shells.push(Shell { faces, reversed });
        Ok(self.brep.shells.len() - 1)
    }
}

fn signed_area(points: &[Vec3], lp: &[usize], n: Vec3) -> f64 {
    if lp.len() < 3 {
        return 0.0;
    }
    let q0 = points[lp[0]];
    let mut a = [0.0; 3];
    for w in lp[1..].windows(2) {
        let (p, q) = (points[w[0]], points[w[1]]);
        let c = cross([p[0] - q0[0], p[1] - q0[1], p[2] - q0[2]], [q[0] - q0[0], q[1] - q0[1], q[2] - q0[2]]);
        for x in 0..3 {
            a[x] += 0.5 * c[x];
        }
    }
    dot(a, n)
}

fn length_scale(ex: &Exchange) -> Result<f64, MeshError> {
    for inst in ex.data.values() {
        if inst.part("LENGTH_UNIT").is_none() {
            continue;
        }
        if let Some(si) = inst.part("SI_UNIT") {
            let prefix = match si.first() {
                Some(Param::Enum(p)) => p.as_str(),
                _ => "",
            };
            return match prefix {
                "" => Ok(1000.0),
                "MILLI" => Ok(1.0),
                "CENTI" => Ok(10.0),
                "DECI" => Ok(100.0),
                "MICRO" => Ok(1e-3),
                "KILO" => Ok(1e6),
                other => Err(rerr(format!("unsupported SI length prefix {other}"))),
            };
        }
        if let Some(cb) = inst.part("CONVERSION_BASED_UNIT") {
            let name = match cb.first() {
                Some(Param::Str(s)) => s.to_ascii_uppercase(),
                _ => String::new(),
            };
            return match name.as_str() {
                "INCH" => Ok(25.4),
                "FOOT" => Ok(304.8),
                _ => Err(rerr(format!("unsupported conversion-based length unit {name}"))),
            };
        }
    }
    Ok(1.0)
}



pub fn read_brep(text: &str) -> Result<(Brep, Exchange), MeshError> {
    let ex = parse(text)?;
    let scale = length_scale(&ex)?;
    let mut rd =
        Reader { ex: &ex, scale, brep: Brep::default(), points: HashMap::new(), edges: HashMap::new() };
    let solids: Vec<(usize, String)> = ex
        .data
        .iter()
        .filter_map(|(id, inst)| inst.simple().map(|(n, _)| (*id, n.to_string())))
        .filter(|(_, n)| n == "MANIFOLD_SOLID_BREP" || n == "BREP_WITH_VOIDS")
        .collect();
    for (id, kind) in &solids {
        let (_, p) = rd.get(*id, &[kind.as_str()])?;
        let outer = Reader::reference(p.get(1))?;
        let voids: Vec<usize> = if kind == "BREP_WITH_VOIDS" {
            Reader::list(p.get(2))?.iter().map(|v| Reader::reference(Some(v))).collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        let mut shells = vec![rd.shell(outer)?];
        for v in voids {
            shells.push(rd.shell(v)?);
        }
        rd.brep.solids.push(Solid { shells });
    }
    if rd.brep.solids.is_empty() {
        return Err(rerr("the file carries no manifold_solid_brep"));
    }
    let brep = rd.brep;
    Ok((brep, ex))
}
