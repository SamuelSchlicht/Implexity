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



use std::path::PathBuf;
use std::sync::Arc;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::{Window, WindowBuilder};
use wry::{WebContext, WebView, WebViewBuilder};

use crate::host::{APP_NAME, Log};

#[derive(Debug, Clone, Copy)]
pub(crate) enum UserEvent {
    Activate,
    Dismiss,
}

pub(crate) struct Ui {
    event_loop: EventLoop<UserEvent>,
}

pub(crate) struct WindowSpec {
    pub url: String,
    pub size: (f64, f64),
    pub private: bool,
    pub storage: Option<PathBuf>,
    pub devtools: bool,
    pub log: Option<Arc<Log>>,
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "the window toolkit could not start".to_owned())
}

impl Ui {


    pub(crate) fn new() -> Result<Self, String> {
        std::panic::catch_unwind(|| EventLoopBuilder::<UserEvent>::with_user_event().build())
            .map(|event_loop| Self { event_loop })
            .map_err(|p| panic_text(p.as_ref()))
    }

    pub(crate) fn proxy(&self) -> EventLoopProxy<UserEvent> {
        self.event_loop.create_proxy()
    }

    fn webview(builder: WebViewBuilder<'_>, window: &Window) -> Result<WebView, String> {
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "ios", target_os = "android"))]
        let built = builder.build(window);
        #[cfg(not(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "ios",
            target_os = "android"
        )))]
        let built = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;
            let vbox = window.default_vbox().ok_or("the window has no content area")?;
            builder.build_gtk(vbox)
        };
        built.map_err(|e| e.to_string())
    }



    pub(crate) fn run_window(&mut self, spec: &WindowSpec) -> Result<(), String> {
        let window = WindowBuilder::new()
            .with_title(APP_NAME)
            .with_inner_size(LogicalSize::new(spec.size.0, spec.size.1))
            .with_min_inner_size(LogicalSize::new(1080.0, 700.0))
            .build(&self.event_loop)
            .map_err(|e| e.to_string())?;
        let mut context = WebContext::new(spec.storage.clone());
        let download_log = spec.log.clone();
        let builder = WebViewBuilder::new_with_web_context(&mut context)
            .with_url(&spec.url)
            .with_incognito(spec.private)
            .with_devtools(spec.devtools)
            .with_clipboard(true)
            .with_download_started_handler(move |url, path| {
                if let Some(log) = &download_log {
                    log.info(&format!("download {} -> {}", clip(&url, 120), path.display()));
                }
                true
            });
        let webview = Self::webview(builder, &window)?;
        let log = spec.log.clone();
        self.event_loop.run_return(|event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            match event {
                Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                    if let Some(log) = &log {
                        log.info("workbench window closed");
                    }
                    *control_flow = ControlFlow::Exit;
                }
                Event::UserEvent(UserEvent::Activate) => {
                    window.set_minimized(false);
                    window.set_visible(true);
                    window.set_focus();
                }
                _ => {}
            }
        });
        drop(webview);
        drop(window);
        Ok(())
    }



    pub(crate) fn message_box(&mut self, text: &str, title: &str) -> Result<(), String> {
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(560.0, 280.0))
            .with_resizable(false)
            .build(&self.event_loop)
            .map_err(|e| e.to_string())?;
        let proxy = self.proxy();
        let html = format!(
            "<!doctype html><meta charset=utf-8><body style=\"font:14px system-ui,sans-serif;margin:20px;\
             display:flex;flex-direction:column;height:calc(100vh - 40px)\"><div style=\"flex:1;overflow:auto;\
             white-space:pre-wrap\">{}</div><div style=\"text-align:right\"><button autofocus \
             onclick=\"window.ipc.postMessage('ok')\" style=\"min-width:88px;padding:6px\">OK</button></div></body>",
            escape(text)
        );
        let builder = WebViewBuilder::new().with_html(html).with_ipc_handler(move |_| {
            let _ = proxy.send_event(UserEvent::Dismiss);
        });
        let webview = Self::webview(builder, &window)?;
        self.event_loop.run_return(|event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            if matches!(
                event,
                Event::WindowEvent { event: WindowEvent::CloseRequested, .. }
                    | Event::UserEvent(UserEvent::Dismiss)
            ) {
                *control_flow = ControlFlow::Exit;
            }
        });
        drop(webview);
        drop(window);
        Ok(())
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() } else { s.chars().take(n).collect::<String>() + "..." }
}

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

