// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

