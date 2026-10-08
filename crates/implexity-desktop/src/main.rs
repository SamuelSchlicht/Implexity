// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





#![forbid(unsafe_code)]
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
#![allow(clippy::print_stderr, clippy::print_stdout)]

mod host;
mod service;
mod url;
mod window;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use host::{APP_NAME, Log, SingleInstance};
use window::{Ui, UserEvent, WindowSpec};

const PROG: &str = "implexity-workbench";
const USAGE: &str = "usage: implexity-workbench [-h] [--connect http://127.0.0.1:PORT]";

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Help,
    Connect(String),
    Managed,
}

fn parser_error(message: &str) -> String {
    format!("{USAGE}\n{PROG}: error: {message}\n")
}

fn parse(argv: &[String]) -> Result<Mode, String> {
    let mut connect = None;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "-h" || (arg.len() > 2 && "--help".starts_with(arg.as_str())) {
            return Ok(Mode::Help);
        }
        let (flag, inline) =
            arg.split_once('=').map_or((arg.as_str(), None), |(f, v)| (f, Some(v.to_owned())));
        if flag.len() > 2 && "--connect".starts_with(flag) {
            let value = if let Some(v) = inline {
                v
            } else {
                i += 1;
                argv.get(i)
                    .cloned()
                    .ok_or_else(|| parser_error("argument --connect: expected one argument"))?
            };
            let url = url::local_viewer_url(&value)
                .map_err(|m| parser_error(&format!("argument --connect: {m}")))?;
            connect = Some(url);
        } else {
            return Err(parser_error(&format!("unrecognized arguments: {}", argv[i..].join(" "))));
        }
        i += 1;
    }
    Ok(connect.map_or(Mode::Managed, Mode::Connect))
}

fn help() -> String {
    format!(
        "{USAGE}\n\nImplexity desktop window\n\noptions:\n  -h, --help            show this help message and exit\n  \
         --connect http://127.0.0.1:PORT\n                        Open an existing local service; closing the window \
         leaves it running.\n"
    )
}

fn message_box(ui: &mut Option<Ui>, text: &str) {
    if cfg!(windows) {
        if ui.is_none() {
            *ui = Ui::new().ok();
        }
        if let Some(ui) = ui.as_mut()
            && ui.message_box(text, APP_NAME).is_ok()
        {
            return;
        }
    }
    eprintln!("{APP_NAME}: {text}");
}

fn connected_window(url: String) -> u8 {
    let mut ui = None;
    let opened = Ui::new().and_then(|mut u| {
        let spec = WindowSpec {
            url,
            size: (1440.0, 900.0),
            private: true,
            storage: None,
            devtools: false,
            log: None,
        };
        let result = u.run_window(&spec);
        ui = Some(u);
        result
    });
    match opened {
        Ok(()) => 0,
        Err(e) => {
            message_box(
                &mut ui,
                &format!("The desktop window could not start: {e}\nThe existing service was left running."),
            );
            1
        }
    }
}

fn managed() -> u8 {
    let mut ui: Option<Ui> = None;
    let root = match host::user_root() {
        Ok(r) => r,
        Err(e) => {
            message_box(&mut ui, &format!("Implexity could not start.\n\n{e}"));
            return 1;
        }
    };
    let (mut instance, alone) = match SingleInstance::acquire(&root) {
        Ok(v) => v,
        Err(e) => {
            message_box(&mut ui, &format!("Implexity could not start.\n\n{e}"));
            return 1;
        }
    };
    if !alone {
        if !instance.activate_existing() {
            message_box(&mut ui, "Implexity is already running.");
        }
        return 2;
    }
    let log = Arc::new(Log::open(&root));
    let install = host::install_root();
    let mut service = None;
    let outcome = (|| -> Result<(), String> {
        let port = service::free_port().map_err(|e| e.to_string())?;
        let url = format!("http://127.0.0.1:{port}");
        let started = service.insert(service::Service::start(&install, &root, port, &log)?);
        let catalogue = started.wait_ready(&url, Duration::from_secs(90))?;
        let routes = catalogue.get("endpoints").and_then(serde_json::Value::as_array).map_or(0, Vec::len);
        log.info(&format!("service ready; {routes} routes"));
        let u = match ui.as_mut() {
            Some(u) => u,
            None => ui.insert(Ui::new()?),
        };
        let proxy = u.proxy();
        instance.serve_activation(move || {
            let _ = proxy.send_event(UserEvent::Activate);
        });
        let spec = WindowSpec {
            url: format!("{url}/viewer/model.html"),
            size: (1600.0, 1000.0),
            private: false,
            storage: Some(root.join("webview")),
            devtools: std::env::var_os("IMPLEXITY_DEBUG").is_some_and(|v| !v.is_empty()),
            log: Some(Arc::clone(&log)),
        };
        u.run_window(&spec)
    })();
    let code = match outcome {
        Ok(()) => 0,
        Err(e) => {
            log.error("workbench startup failed", &e);
            let logs = root.join("logs");
            message_box(&mut ui, &format!("Implexity could not start.\n\n{e}\n\nLogs: {}", logs.display()));
            1
        }
    };
    if let Some(s) = service {
        s.stop();
    }
    instance.close();
    code
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse(&argv) {
        Ok(Mode::Help) => {
            print!("{}", help());
            0
        }
        Ok(Mode::Connect(url)) => connected_window(url),
        Ok(Mode::Managed) if !cfg!(windows) => {
            eprint!(
                "{}",
                parser_error(
                    "On macOS/Linux, start the service first and use --connect http://127.0.0.1:PORT"
                )
            );
            2
        }
        Ok(Mode::Managed) => managed(),
        Err(message) => {
            eprint!("{message}");
            2
        }
    };
    ExitCode::from(code)
}

