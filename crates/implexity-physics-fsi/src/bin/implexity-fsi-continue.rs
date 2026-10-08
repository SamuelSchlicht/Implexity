// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

#![forbid(unsafe_code)]
use std::{path::Path, process::ExitCode};
fn main() -> ExitCode {
    let args:Vec<_>=std::env::args().skip(1).collect();
    if args.len()!=3 {eprintln!("usage: implexity-fsi-continue MANIFEST.json MANIFEST_SHA256 NEW_OUTPUT_DIRECTORY");return ExitCode::from(2);}
    match implexity_physics_fsi::restart::continue_from(Path::new(&args[0]),&args[1],Path::new(&args[2])) {
        Ok(result)=>{println!("{result}");ExitCode::SUCCESS}
        Err(error)=>{eprintln!("{error}");ExitCode::FAILURE}
    }
}
