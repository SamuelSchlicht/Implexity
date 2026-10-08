// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![deny(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str =
    "usage: implexity-egl-worker --packet PACKET --arrays ARRAYS --viewer VIEWER --output OUTPUT";

fn arguments() -> Result<[PathBuf; 4], String> {
    let names = ["--packet", "--arrays", "--viewer", "--output"];
    let mut values: [Option<PathBuf>; 4] = Default::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy().into_owned();
        let (flag, inline) = match text.split_once('=') {
            Some((f, v)) => (f.to_owned(), Some(PathBuf::from(v))),
            None => (text.clone(), None),
        };
        let Some(i) = names.iter().position(|n| *n == flag) else {
            return Err(format!("{USAGE}\nimplexity-egl-worker: error: unrecognized arguments: {text}"));
        };
        let value = match inline {
            Some(v) => v,
            None => args.next().map(PathBuf::from).ok_or_else(|| {
                format!("{USAGE}\nimplexity-egl-worker: error: argument {flag}: expected one argument")
            })?,
        };
        values[i] = Some(value);
    }
    let missing: Vec<&str> =
        names.iter().zip(&values).filter(|(_, v)| v.is_none()).map(|(n, _)| *n).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{USAGE}\nimplexity-egl-worker: error: the following arguments are required: {}",
            missing.join(", ")
        ));
    }
    Ok(values.map(Option::unwrap_or_default))
}

#[allow(unsafe_code)]
fn default_platform() {
    if std::env::var_os("EGL_PLATFORM").is_none() {
        
        unsafe { std::env::set_var("EGL_PLATFORM", "surfaceless") };
    }
}

fn main() -> ExitCode {
    default_platform();
    let [packet, arrays, viewer, output] = match arguments() {
        Ok(a) => a,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match implexity_render_egl::render(&packet, &arrays, &viewer, &output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}: {e}", e.class());
            ExitCode::FAILURE
        }
    }
}
