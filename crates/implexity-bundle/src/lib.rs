// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![forbid(unsafe_code)]

pub const CRATE: &str = "implexity-bundle";

pub const LAYER: &str = "composition";

use std::sync::Once;

pub mod job_hooks;

use implexity_core::distributions::{DistributionSet, EmbeddedDistribution};

pub const ADDINS: EmbeddedDistribution = EmbeddedDistribution {
    name: "addins",
    files: &[
        ("implexity_distribution.json", include_str!("../distributions/addins/implexity_distribution.json")),
        ("package_catalog.json", include_str!("../distributions/addins/package_catalog.json")),
        ("field_source_catalog.json", include_str!("../distributions/addins/field_source_catalog.json")),
        (
            "history_response_catalog.json",
            include_str!("../distributions/addins/history_response_catalog.json"),
        ),
        ("agent_workflow_catalog.json", include_str!("../distributions/addins/agent_workflow_catalog.json")),
    ],
};

pub const EXTENSIONS: EmbeddedDistribution = EmbeddedDistribution {
    name: "extensions",
    files: &[
        (
            "implexity_distribution.json",
            include_str!("../distributions/extensions/implexity_distribution.json"),
        ),
        ("package_catalog.json", include_str!("../distributions/extensions/package_catalog.json")),
        (
            "history_response_catalog.json",
            include_str!("../distributions/extensions/history_response_catalog.json"),
        ),
    ],
};

#[cfg(feature = "dynamic")]
pub const DYNAMICS: EmbeddedDistribution = EmbeddedDistribution {
    name: "dynamics",
    files: &[
        (
            "implexity_distribution.json",
            include_str!("../distributions/dynamics/implexity_distribution.json"),
        ),
        ("package_catalog.json", include_str!("../distributions/dynamics/package_catalog.json")),
    ],
};

#[cfg(feature = "dynamic")]
pub const EMBEDDED: [EmbeddedDistribution; 3] = [ADDINS, EXTENSIONS, DYNAMICS];
#[cfg(not(feature = "dynamic"))]
pub const EMBEDDED: [EmbeddedDistribution; 2] = [ADDINS, EXTENSIONS];

pub const DYNAMIC_MODULES: bool = cfg!(feature = "dynamic");

pub fn register_distributions(set: &DistributionSet) {
    for d in EMBEDDED {
        set.register_embedded(d);
    }
}


pub fn link_packages() {

    implexity_physics_thermofluid::link();
    implexity_physics_compressible::package::link();
    implexity_physics_solid::addins::link();
    implexity_physics_fields::addins::link();
    implexity_physics_lbm::addins::link();

    #[cfg(feature = "dynamic")]
    {
        implexity_physics_fsi::addins::link();
    }
}

static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(|| {
        register_distributions(implexity_core::distributions::global());
        link_packages();
        job_hooks::register_job_hooks();
    });
}

