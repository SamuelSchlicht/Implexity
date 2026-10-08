// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]
#![allow(clippy::print_stdout)]

mod args;
mod commands;
mod doctor;
mod model_cli;
mod study;
mod templates;
mod util;

use std::process::ExitCode;

fn main() -> ExitCode {

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if msg.contains("failed printing to stdout") && msg.contains("Broken pipe") {
            std::process::exit(0);
        }
        default_hook(info);
    }));
    let argv: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(commands::main(&argv))
}
