// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::error::GeometryError;
use crate::node::{NodeRef, ParamRef};
use crate::pyfmt::str_repr;

pub const CUTOFF: f64 = 0.72;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}", display_file_error(.path.as_deref(), .problems))]
pub struct ModelFileError {
    pub problems: Vec<String>,
    pub path: Option<String>,
}

fn display_file_error(path: Option<&str>, problems: &[String]) -> String {
    let mut v = vec![format!("cannot read {}", path.unwrap_or("the document"))];
    v.extend(problems.iter().cloned());
    v.join("\n  ")
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() { key.to_string() } else { format!("{prefix}.{key}") }
}

struct Frame {
    prefix: String,
    object: bool,
    index: usize,
}

fn child_prefix(stack: &[Frame], pending: Option<&str>) -> Option<String> {
    let top = stack.last()?;
    if top.object {
        return pending.map(|p| join(&top.prefix, p));
    }
    Some(format!("{}[{}]", top.prefix, top.index))
}

#[must_use]
pub fn locate_keys(text: &str) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut line = 1usize;
    let mut i = 0usize;
    let mut stack: Vec<Frame> = Vec::new();
    let mut expect_key = false;
    let mut pending: Option<String> = None;
    while i < n {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if matches!(c, ' ' | '\t' | '\r') {
            i += 1;
            continue;
        }
        if c == '"' {
            let mut j = i + 1;
            while j < n {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == '"' {
                    break;
                }
                if chars[j] == '\n' {
                    line += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(n);
            let raw: String = chars[i..end].iter().collect();
            i = j + 1;
            if expect_key && stack.last().is_some_and(|f| f.object) {
                let key = match serde_json::from_str::<Value>(&raw) {
                    Ok(Value::String(s)) => s,
                    _ => raw.trim_matches('"').to_string(),
                };
                let path = join(&stack.last().map(|f| f.prefix.clone()).unwrap_or_default(), &key);
                out.entry(path).or_insert(line);
                pending = Some(key);
                expect_key = false;
            }
            continue;
        }
        if c == '{' || c == '[' {
            let prefix = child_prefix(&stack, pending.as_deref());
            if let Some(p) = &prefix {
                out.entry(p.clone()).or_insert(line);
            }
            stack.push(Frame { prefix: prefix.unwrap_or_default(), object: c == '{', index: 0 });
            expect_key = c == '{';
            pending = None;
            i += 1;
            continue;
        }
        if c == '}' || c == ']' {
            stack.pop();
            expect_key = false;
            pending = None;
            i += 1;
            continue;
        }
        if c == ':' {
            expect_key = false;
            i += 1;
            continue;
        }
        if c == ',' {
            if let Some(top) = stack.last_mut() {
                if top.object {
                    expect_key = true;
                } else {
                    top.index += 1;
                }
            }
            pending = None;
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && !matches!(chars[j], ',' | '}' | ']' | '\n' | ' ' | '\t' | '\r') {
            j += 1;
        }
        if let Some(p) = child_prefix(&stack, pending.as_deref()) {
            out.entry(p).or_insert(line);
        }
        pending = None;
        i = j;
    }
    out
}

fn path_prefix(p: &str) -> Option<String> {
    let mut chars = p.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let len = p
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '[' | ']' | '-')))
        .map_or(p.len(), |(i, _)| i);
    Some(p[..len].to_string())
}

fn strip_last_segment(cand: &str) -> String {

    if cand.ends_with(']')
        && let Some(open) = cand.rfind('[')
    {
        let inner = &cand[open + 1..cand.len() - 1];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            return cand[..open].to_string();
        }
    }
    if let Some(dot) = cand.rfind('.') {
        let tail = &cand[dot + 1..];
        if !tail.is_empty() && !tail.contains(['.', '[', ']']) {
            return cand[..dot].to_string();
        }
    }
    cand.to_string()
}

fn line_for(problem: &str, keys: &BTreeMap<String, usize>) -> Option<usize> {
    let m = path_prefix(problem)?;
    let mut cand = m.trim_end_matches(['.', ':']).to_string();
    while !cand.is_empty() {
        if let Some(l) = keys.get(&cand) {
            return Some(*l);
        }
        if !cand.contains('.') && !cand.contains('[') {
            return None;
        }
        let next = strip_last_segment(&cand);
        if next == cand {
            return None;
        }
        cand = next;
    }
    None
}

#[must_use]
pub fn annotate(problems: &[String], text: &str, path: Option<&str>) -> Vec<String> {
    if text.is_empty() {
        return problems.to_vec();
    }
    let keys = locate_keys(text);
    let name = path.map(|p| {
        Path::new(p).file_name().map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned())
    });
    problems
        .iter()
        .map(|p| match (line_for(p, &keys), &name) {
            (None, _) => p.clone(),
            (Some(l), Some(nm)) => format!("{nm}:{l}: {p}"),
            (Some(l), None) => format!("line {l}: {p}"),
        })
        .collect()
}

fn abspath(p: &Path) -> String {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned()
}


pub fn read_json(path: &Path, what: &str) -> Result<(String, Value), ModelFileError> {
    let spath = path.to_string_lossy().into_owned();
    if !path.exists() {
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let d = abs.parent().map_or_else(|| ".".to_string(), |p| p.to_string_lossy().into_owned());
        let mut near: Vec<String> = std::fs::read_dir(&d)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|f| Path::new(f).extension().is_some_and(|e| e == "json"))
                    .collect()
            })
            .unwrap_or_default();
        near.sort();
        let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let hint = did_you_mean(&base, &near, CUTOFF);
        let mut probs = vec![format!("there is no file {}", abspath(path))];
        if near.is_empty() {
            probs.push(format!("{d} holds no .json file at all.  `implexity model examples` lists the shipped models and where they are"));
        } else {
            let shown: Vec<&str> = near.iter().take(8).map(String::as_str).collect();
            probs.push(format!(
                "{d} holds {} .json file(s): {}{}",
                near.len(),
                shown.join(", "),
                if near.len() > 8 { " ..." } else { "" }
            ));
            if let Some(h) = hint {
                probs.push(format!("did you mean {h}?"));
            }
        }
        return Err(ModelFileError { problems: probs, path: Some(spath) });
    }
    if path.is_dir() {
        return Err(ModelFileError {
            problems: vec![format!("{} is a directory; a {what} is one .json file", abspath(path))],
            path: Some(spath),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| ModelFileError {
        problems: vec![format!("{} could not be read: {e}", abspath(path))],
        path: Some(spath.clone()),
    })?;
    let opts = implexity_core::json::ParseOptions { reject_duplicate_keys: false };
    match implexity_core::json::parse_with(&text, opts) {
        Ok(v) => Ok((text, v)),
        Err(e) => Err(ModelFileError { problems: syntax_problems(&text, &e, path), path: Some(spath) }),
    }
}

fn syntax_problems(text: &str, e: &implexity_core::json::JsonError, path: &Path) -> Vec<String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (line, col) = (e.line, e.column);
    let lines: Vec<&str> = text.lines().collect();
    let src = if line > 0 && line <= lines.len() { lines[line - 1] } else { "" };
    let mut out = vec![format!("{name}:{line}:{col}: this file is not valid JSON -- {}", e.message)];
    if !src.is_empty() {
        let clip: String = if src.chars().count() <= 96 {
            src.to_string()
        } else {
            format!("{}...", src.chars().take(93).collect::<String>())
        };
        let pad = col.saturating_sub(1).min(clip.chars().count());
        out.push(format!("{clip}\n{}^", " ".repeat(pad)));
    }
    out.push(
        "A model document is JSON, which has no comments and no trailing commas; the header a shipped model carries is its \"doc\" field, which is a string.  `implexity model examples` prints one that parses."
            .into(),
    );
    out
}

#[must_use]
pub fn sequence_ratio(a: &[char], b: &[char]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }

    let mut b2j: BTreeMap<char, Vec<usize>> = BTreeMap::new();
    for (j, c) in b.iter().enumerate() {
        b2j.entry(*c).or_default().push(j);
    }
    if b.len() >= 200 {
        let ntest = b.len() / 100 + 1;
        b2j.retain(|_, v| v.len() <= ntest);
    }
    let longest = |alo: usize, ahi: usize, blo: usize, bhi: usize| -> (usize, usize, usize) {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: BTreeMap<usize, usize> = BTreeMap::new();
        for (i, ai) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut new: BTreeMap<usize, usize> = BTreeMap::new();
            if let Some(js) = b2j.get(ai) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j.checked_sub(1).and_then(|p| j2len.get(&p)).copied().unwrap_or(0) + 1;
                    new.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = new;
        }
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] && b2j.contains_key(&b[bestj - 1]) {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
            && b2j.contains_key(&b[bestj + bestsize])
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    };
    let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
    let mut matches = 0usize;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest(alo, ahi, blo, bhi);
        if k > 0 {
            matches += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let r = 2.0 * matches as f64 / total as f64;
    r
}

fn quick_ratio(a: &[char], b: &[char]) -> f64 {
    let mut avail: BTreeMap<char, i64> = BTreeMap::new();
    for c in b {
        *avail.entry(*c).or_default() += 1;
    }
    let mut matches = 0usize;
    for c in a {
        let e = avail.entry(*c).or_default();
        if *e > 0 {
            matches += 1;
        }
        *e -= 1;
    }
    let total = a.len() + b.len();
    #[allow(clippy::cast_precision_loss)]
    if total == 0 { 1.0 } else { 2.0 * matches as f64 / total as f64 }
}

#[must_use]
pub fn did_you_mean(name: &str, candidates: &[String], cutoff: f64) -> Option<String> {
    if name.is_empty() || candidates.is_empty() {
        return None;
    }
    let word: Vec<char> = name.chars().collect();
    let mut best: Option<(f64, String)> = None;
    for x in candidates {
        let xa: Vec<char> = x.chars().collect();
        let total = xa.len() + word.len();
        #[allow(clippy::cast_precision_loss)]
        let rq = if total == 0 { 1.0 } else { 2.0 * xa.len().min(word.len()) as f64 / total as f64 };
        if rq < cutoff || quick_ratio(&xa, &word) < cutoff {
            continue;
        }
        let r = sequence_ratio(&xa, &word);
        if r < cutoff {
            continue;
        }
        let better = match &best {
            None => true,
            Some((s, bx)) => r > *s || (r == *s && x > bx),
        };
        if better {
            best = Some((r, x.clone()));
        }
    }
    best.map(|(_, x)| x)
}

fn near_miss_groups(p: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();

    if let Some(a) = p.find("kind '") {
        let rest = &p[a + 6..];
        if let Some(q) = rest.find("' is not registered; the registered kinds are ") {
            let tail = &rest[q + "' is not registered; the registered kinds are ".len()..];
            if let Some(end) = find_period_ws_then(tail, "A kind is registered") {
                out.push((rest[..q].to_string(), tail[..end].to_string()));
            }
        }
    }

    if let Some(a) = p.find("no node kind '") {
        let rest = &p[a + 14..];
        if let Some(q) = rest.find("'; known kinds are ") {
            out.push((rest[..q].to_string(), rest[q + 19..].to_string()));
        }
    }

    if let Some(a) = p.find("has no parameter ") {
        let rest = &p[a + 17..];
        let unq = rest.strip_prefix('\'').unwrap_or(rest);
        let name_len = unq
            .char_indices()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
            .map_or(unq.len(), |(i, _)| i);
        let name = &unq[..name_len];
        let after = unq[name_len..].strip_prefix('\'').unwrap_or(&unq[name_len..]);
        if !name.is_empty()
            && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && let Some(list) = after.strip_prefix("; it has ")
        {
            let end = find_period_ws(list).unwrap_or(list.len());
            if end > 0 {
                out.push((name.to_string(), list[..end].to_string()));
            }
        }
    }

    if let Some(a) = p.find("unknown term '") {
        let rest = &p[a + 14..];
        if let Some(q) = rest.find("'; the catalogue is [") {
            let tail = &rest[q + 21..];
            if let Some(end) = tail.rfind(']') {
                out.push((rest[..q].to_string(), tail[..end].to_string()));
            }
        }
    }

    if let Some(a) = p.find("binds to parameter '") {
        let rest = &p[a + 20..];
        if let Some(q) = rest.find("', which this document does not declare (it declares ") {
            let tail = &rest[q + "', which this document does not declare (it declares ".len()..];
            if let Some(end) = tail.rfind(')') {
                out.push((rest[..q].to_string(), tail[..end].to_string()));
            }
        }
    }
    out
}

fn find_period_ws(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    (1..b.len()).find(|&i| b[i] == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_whitespace))
}

fn find_period_ws_then(s: &str, then: &str) -> Option<usize> {
    let b = s.as_bytes();
    for i in 1..b.len() {
        if b[i] == b'.' {
            let rest = s[i + 1..].trim_start();
            if rest.len() < s[i + 1..].len() && rest.starts_with(then) {
                return Some(i);
            }
        }
    }
    None
}

#[must_use]
pub fn with_near_misses(problems: &[String]) -> Vec<String> {
    problems
        .iter()
        .map(|p| {
            let mut add = None;
            for (name, list) in near_miss_groups(p) {
                let cands: Vec<String> = list
                    .split(',')
                    .map(|c| c.trim().trim_matches(['\'', '"']).to_string())
                    .filter(|c| !c.is_empty())
                    .collect();
                add = did_you_mean(&name, &cands, CUTOFF);
                if add.is_some() {
                    break;
                }
            }
            let Some(add) = add else { return p.clone() };
            let (head, rest) = located_head(p);
            format!("{head}did you mean {}?  -- {rest}", str_repr(&add))
        })
        .collect()
}

fn located_head(p: &str) -> (&str, &str) {
    let first_ws = p.find(char::is_whitespace).unwrap_or(p.len());
    let token = &p[..first_ws];
    let parts: Vec<&str> = token.split(':').collect();
    let ok = (parts.len() == 3 || parts.len() == 4)
        && !parts[0].is_empty()
        && parts[1..parts.len() - 1].iter().all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
        && parts[parts.len() - 1].is_empty()
        && p[first_ws..].starts_with(' ');
    if ok { (&p[..=first_ws], &p[first_ws + 1..]) } else { ("", p) }
}


pub fn parse_ref(text: &str) -> Result<ParamRef, ModelFileError> {
    let s = text.trim();
    let fail = |m: String| ModelFileError { problems: vec![m], path: None };
    let Some((path, name)) = s.rsplit_once(':') else {
        return Err(fail(format!(
            "{} is not a parameter reference: it needs a colon between the path and the parameter name, as 'lattice/thickness:scale' (a path segment is a CHILD NAME, and the parameter follows the colon).  `implexity model params <file> --refs` lists every reference this model has.",
            str_repr(s)
        )));
    };
    if name.trim().is_empty() {
        return Err(fail(format!("{} names no parameter after the colon", str_repr(s))));
    }
    let nt = name.trim();
    let valid = nt.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && nt.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        return Err(fail(format!(
            "{}: {} is not a parameter name (letters, digits, _ . - and it starts with a letter or _)",
            str_repr(s),
            str_repr(name)
        )));
    }
    Ok(ParamRef::new(path.split('/').filter(|p| !p.is_empty()).map(str::to_string).collect(), nt))
}


#[must_use]
pub fn explain(err: &GeometryError, doing: Option<&str>) -> (Vec<String>, Vec<String>) {
    let _doing = doing.unwrap_or("this operation");
    let probs = match err {
        GeometryError::ModelDoc(p) if !p.is_empty() => p.clone(),
        other => {
            let s = other.to_string();
            vec![if s.is_empty() { "GeometryError".to_string() } else { s }]
        }
    };
    if matches!(err, GeometryError::BoundViolation(_)) {
        return (
            probs,
            vec![
                "`implexity model show <file>` prints the field class of every node, so the one that weakened the promise is visible".into(),
                "docs/IMPLICIT_CAE.md section 2 is the table of what each class buys".into(),
            ],
        );
    }
    (probs, Vec::new())
}

pub const SMOOTH_TRAP: &str = "the Optimize node's smooth_r is 0.0 mm and this model contains {n} node(s) that smooth with the CONTEXT's radius ({kinds}).  In mode 'smooth' those compute clip(0.5 + 0.5*(b - a)/smooth_r, 0, 1): the VALUE is finite -- it is the sharp boolean -- and the GRADIENT is NaN, so every iteration after the first reports L = nan and the design never moves.  Measured on this tree: 61.9 s of coupled physics to produce three L = nan iterations and a design identical to the one it started from.";

#[must_use]
pub fn smoothing_trap(model: &NodeRef, settings: &serde_json::Map<String, Value>) -> Vec<String> {
    let mode = settings.get("eval_mode").map_or_else(|| "smooth".to_string(), crate::document::py_str);
    if mode != "smooth" {
        return Vec::new();
    }
    let r = match settings.get("smooth_r") {
        None | Some(Value::Null) => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => match s.trim().parse::<f64>() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        },
        Some(_) => return Vec::new(),
    };
    if r > 0.0 {
        return Vec::new();
    }
    let walk = model.walk();
    let smoothers: Vec<String> = walk
        .iter()
        .filter(|(_, n)| n.info().param("k_scale").is_some())
        .map(|(_, n)| n.kind().to_string())
        .collect();
    if smoothers.is_empty() {
        return Vec::new();
    }
    let kinds: std::collections::BTreeSet<&str> = smoothers.iter().map(String::as_str).collect();
    let text = SMOOTH_TRAP
        .replace("{n}", &smoothers.len().to_string())
        .replace("{kinds}", &kinds.into_iter().collect::<Vec<_>>().join(", "));
    vec![format!(
        "{text}  Set smooth_r to a fraction of the smallest feature you care about (0.25 mm is typical for millimetre-scale fillets), or set eval_mode to 'exact' and accept a subgradient at every seam."
    )]
}

pub const NUMPY_SHAPE_PARAMS: [(&str, &str); 3] =
    [("cell_grid_field", "samples"), ("mesh_sdf", "samples"), ("grid_field", "samples")];

#[must_use]
pub fn array_free_refusals(model: &NodeRef, free: &[(ParamRef, usize)]) -> Vec<String> {
    let mut out = Vec::new();
    for (r, size) in free {
        let kind = model.at(&r.path).ok().map(|n| n.kind().to_string());
        let hit = kind.as_deref().is_some_and(|k| NUMPY_SHAPE_PARAMS.contains(&(k, r.name.as_str())));
        if *size > 1 && hit {
            out.push(format!(
                "free {} is an ARRAY of {size} values on a {} node, and that kind's f() reads its shape through numpy, which raises TracerArrayConversionError the first time a gradient is taken through it.  A scalar parameter of the same node traces cleanly.",
                r.as_str(),
                kind.as_deref().unwrap_or("that")
            ));
        }
    }
    out
}

#[must_use]
pub fn binding_text(doc: &Value, node_id: &str, param: &str) -> Option<String> {
    let raw = doc.get("nodes")?.get(node_id)?.get("params")?.get(param)?;
    let m = raw.as_object()?;
    if let Some(b) = m.get("bind") {
        return Some(format!("the named parameter {}", crate::document::py_repr(b)));
    }
    if let Some(e) = m.get("expr") {
        return Some(format!("the expression {}", crate::document::py_repr(e)));
    }
    if let Some(a) = m.get("array") {
        return Some(format!("the array table entry {}", crate::document::py_repr(a)));
    }
    None
}

