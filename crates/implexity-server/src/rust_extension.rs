// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use implexity_runtime::dynamic_frames::catalogue::{Catalogue, SERVICE_ROOT};

use crate::http::Reply;
use crate::service::Service;

pub const EXTENSION: &str = "implexity-rust-extension/1";
const POLICY_KEY: &str = "rust_extension.availability";
pub const VIEWER_DIR: &str = "rust_ext/";
pub const VIEWER_LOADER: &str = "rust_ext/extensions.js";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Packages,
    Always,
    Never,
}

impl Availability {
    const fn code(self) -> u8 {
        match self {
            Self::Packages => 0,
            Self::Always => 1,
            Self::Never => 2,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Always,
            2 => Self::Never,
            _ => Self::Packages,
        }
    }
}

#[derive(Debug, Default)]
struct Policy(AtomicU8);

fn policy(service: &Service) -> Option<Arc<Policy>> {
    service.extension::<Policy, _>(POLICY_KEY, |_| Policy::default()).ok()
}

pub fn set_availability(service: &Service, availability: Availability) {
    if let Some(p) = policy(service) {
        p.0.store(availability.code(), Ordering::SeqCst);
    }
}

#[must_use]
pub fn availability(service: &Service) -> Availability {
    policy(service).map_or(Availability::Packages, |p| Availability::from_code(p.0.load(Ordering::SeqCst)))
}

#[must_use]
pub const fn compiled() -> bool {
    cfg!(feature = "dynamic-results")
}

#[must_use]
pub fn available(service: &Service) -> bool {
    compiled()
        && match availability(service) {
            Availability::Always => true,
            Availability::Never => false,
            Availability::Packages => implexity_runtime::dynamic_frames::availability::active(),
        }
}

#[must_use]
pub fn catalogue(service: &Service) -> Catalogue {
    let state = service.state_dir().unwrap_or_else(|_| crate::service::state_directory(service.workspace()));
    let jobs_root = implexity_jobs::api::opt_manager(service).ok().map(|m| m.dir().to_path_buf());
    Catalogue { service_root: Some(state.join(SERVICE_ROOT)), jobs_root }
}

fn resolved(name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn serve_viewer(service: &Service, name: &str) -> Reply {
    let path = resolved(name);
    let own = path.starts_with(VIEWER_DIR);
    if (!own && path != "model.html") || !service.viewer().is_present() {
        return crate::viewer::serve(service.viewer(), name);
    }
    let bootstrap = path == VIEWER_LOADER || path == "rust_ext/extensions.css";
    if !compiled() || (!available(service) && own && !bootstrap) {
        if own {
            return Reply::err(404, "no such viewer file", serde_json::json!(name));
        }
        return crate::viewer::serve(service.viewer(), name);
    }
    if own {
        let mut reply = crate::viewer::serve(service.viewer(), name);
        if path.ends_with(".html")
            && let Reply::Http(http) = &mut reply
            && http.status == 200
        {

            http.headers.push(("X-Frame-Options".to_owned(), "SAMEORIGIN".to_owned()));
            http.headers.push(("Content-Security-Policy".to_owned(), "frame-ancestors 'self'".to_owned()));
        }
        return reply;
    }
    match service.viewer().read(name) {
        Some(bytes) => Reply::send(200, with_loader(bytes), crate::viewer::content_type(&path), Vec::new()),
        None => crate::viewer::serve(service.viewer(), name),
    }
}

#[must_use]
pub fn with_loader(page: Vec<u8>) -> Vec<u8> {
    let tag =
        format!("<script src=\"{VIEWER_LOADER}\" defer data-implexity-extension=\"{EXTENSION}\"></script>\n");
    let at = page.windows(7).rposition(|w| w == b"</body>");
    let Some(at) = at else {
        let mut out = page;
        out.extend_from_slice(tag.as_bytes());
        return out;
    };
    let mut out = Vec::with_capacity(page.len() + tag.len());
    out.extend_from_slice(&page[..at]);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(&page[at..]);
    out
}

