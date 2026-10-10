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

