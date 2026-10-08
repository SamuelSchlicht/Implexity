// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderSources {
    pub vertex: String,
    pub fragment: String,
    pub hashes: Value,
}

fn err(m: impl Into<String>) -> String {
    m.into()
}

#[must_use]
pub fn js_number(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    if !x.is_finite() {
        return if x.is_nan() {
            "NaN".into()
        } else if x > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    let a = x.abs();
    if (1e-6..1e21).contains(&a) {
        return format!("{x}");
    }

    let e = format!("{x:e}");
    match e.split_once('e') {
        Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp}"),
        _ => e,
    }
}

fn template<'a>(module: &'a str, name: &str) -> Result<&'a str, String> {
    let head = format!("const {name} = `");
    let mut found = module.match_indices(&head);
    let (start, _) =
        found.next().ok_or_else(|| err(format!("canonical {name} shader source is unavailable")))?;
    if found.next().is_some() {
        return Err(err(format!("canonical {name} shader source is ambiguous")));
    }
    let body = &module[start + head.len()..];
    let end = body.find('`').ok_or_else(|| err(format!("canonical {name} shader source is unterminated")))?;
    if !body[end + 1..].starts_with(';') {
        return Err(err(format!("canonical {name} shader source is not a plain template literal")));
    }
    Ok(&body[..end])
}

fn palette(module: &str) -> Result<BTreeMap<String, Vec<f64>>, String> {
    let start =
        module.find("const PALETTE = {").ok_or_else(|| err("canonical shader palette is unavailable"))?;
    let rest = &module[start + "const PALETTE = {".len()..];
    let end = rest.find("};").ok_or_else(|| err("canonical shader palette is unterminated"))?;
    let mut out = BTreeMap::new();
    for line in rest[..end].lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (name, values) =
            line.split_once(':').ok_or_else(|| err(format!("unreadable palette row {line:?}")))?;
        let values = values.trim().trim_end_matches(',').trim();
        let inner = values
            .strip_prefix('[')
            .and_then(|v| v.strip_suffix(']'))
            .ok_or_else(|| err(format!("unreadable palette row {line:?}")))?;
        let numbers = inner
            .split(',')
            .map(|n| {
                n.trim().parse::<f64>().map_err(|_| err(format!("unreadable palette value in {line:?}")))
            })
            .collect::<Result<Vec<f64>, String>>()?;
        out.insert(name.trim().to_owned(), numbers);
    }
    Ok(out)
}

fn evaluate(body: &str, palette: &BTreeMap<String, Vec<f64>>) -> Result<String, String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '$' if body[i..].starts_with("${") => {
                let close = body[i..].find('}').ok_or_else(|| err("unterminated template expression"))?;
                let expr = body[i + 2..i + close].trim();
                let name = expr
                    .strip_prefix("PALETTE.")
                    .and_then(|r| r.strip_suffix(".join(\", \")"))
                    .ok_or_else(|| err(format!("unsupported shader template expression {expr:?}")))?;
                let values =
                    palette.get(name).ok_or_else(|| err(format!("unknown palette entry {name:?}")))?;
                out.push_str(&values.iter().map(|v| js_number(*v)).collect::<Vec<_>>().join(", "));
                while chars.peek().is_some_and(|(j, _)| *j < i + close + 1) {
                    chars.next();
                }
            }
            '\\' => {
                let (_, e) = chars.next().ok_or_else(|| err("unterminated escape in shader template"))?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '\\' | '`' | '$' | '\'' | '"' => out.push(e),
                    '\n' => {}
                    other => return Err(err(format!("unsupported escape \\{other} in shader template"))),
                }
            }
            _ => out.push(c),
        }
    }
    Ok(out)
}



pub fn shader_sources(raymarch_js: &str, model_html: &str) -> Result<ShaderSources, String> {
    let sampled = {
        let head = "const SAMPLED_SDF = `";
        let mut found = model_html.match_indices(head);
        let (start, _) =
            found.next().ok_or_else(|| err("canonical sampled shader unavailable or ambiguous"))?;
        let body = &model_html[start + head.len()..];
        let end = body.find("`;").ok_or_else(|| err("canonical sampled shader unavailable or ambiguous"))?;
        if found.next().is_some() {
            return Err(err("canonical sampled shader unavailable or ambiguous"));
        }
        body[..end].to_owned()
    };
    let pal = palette(raymarch_js)?;
    let vertex = evaluate(template(raymarch_js, "VERT")?, &pal)?;
    let head = evaluate(template(raymarch_js, "FRAG_HEAD")?, &pal)?;
    let tail = evaluate(template(raymarch_js, "FRAG_TAIL")?, &pal)?;
    let model = if sampled.is_empty() { "float sdf(vec3 p){return 1.0;}" } else { sampled.as_str() };
    let fragment = format!("{head}\n{model}\n{tail}");
    let sha = |s: &str| implexity_io::digest::sha256_hex(s.as_bytes());
    let hashes = json!({"raymarch_file_sha256": sha(raymarch_js), "sampled_field_sha256": sha(&sampled),
                        "vertex_sha256": sha(&vertex), "fragment_sha256": sha(&fragment)});
    Ok(ShaderSources { vertex, fragment, hashes })
}

