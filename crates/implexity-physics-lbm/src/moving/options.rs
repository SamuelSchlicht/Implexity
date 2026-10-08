// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

pub const SCHEMA: &str = "implexity-moving-lbm-options/1";

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn catalogue() -> Value {
    json!({
        "schema": SCHEMA,
        "lattice": [
            {"kind": "D2Q9", "dimensions": 2, "derivatives": "exact", "second_order": true,
             "validity": "planar flows on one periodic lattice layer; half the populations of D3Q19 per cell",
             "limitations": "no out-of-plane motion; cumulant collision unavailable"},
            {"kind": "D3Q19", "dimensions": 3, "derivatives": "exact", "second_order": true,
             "validity": "three-dimensional weakly compressible flow, low Mach number",
             "limitations": "cumulant collision unavailable (needs D3Q27)"},
            {"kind": "D3Q27", "dimensions": 3, "derivatives": "exact", "second_order": true,
             "validity": "three-dimensional flow; required by the cumulant collision",
             "limitations": "27 populations per cell (about 1.4x the cost of D3Q19)"}
        ],
        "collision": [
            {"kind": "bgk", "derivatives": "exact", "second_order": true,
             "validity": "laminar flow, tau+ well above 1/2",
             "limitations": "wall location of bounce-back and penalized walls depends on viscosity"},
            {"kind": "trt", "parameters": {"magic": 0.1875}, "derivatives": "exact", "second_order": true,
             "validity": "default; with magic 3/16 bounce-back walls sit at the half link independently of viscosity",
             "limitations": "stability at tau+ close to 1/2 limited as for BGK"},
            {"kind": "mrt", "parameters": {"bulk_rate": null, "odd_magic": 0.1875, "even_rate": null},
             "derivatives": "exact", "second_order": true,
             "validity": "weighted-orthogonal moment basis for every lattice; defaults reproduce TRT exactly; separate bulk and ghost-moment rates",
             "limitations": "rates in (0, 2); no Galilean correction of the bulk moment"},
            {"kind": "regularized", "derivatives": "exact", "second_order": true,
             "validity": "full second-order Hermite projection with Guo forcing; bulk and shear retain the physical viscosity",
             "limitations": "higher kinetic moments projected; wall, moving-coupling and resolution accuracy require validation; no positivity guarantee"},
            {"kind": "cumulant", "parameters": {"bulk_rate": 1.0}, "lattice": ["D3Q27"],
             "derivatives": "exact (transposed local Jacobian from one forward-dual pass)", "second_order": false,
             "validity": "improved stability at low viscosity; third- and higher-order cumulants relaxed to zero",
             "limitations": "D3Q27 only; Galilean correction terms of Geier et al. (2015) not applied; no second-order action (stability-eigenvalue gradients refused)"}
        ],
        "turbulence": [
            {"kind": "laminar", "derivatives": "exact", "second_order": true,
             "validity": "resolved laminar or periodic flows (the exact-gradient regime)", "limitations": "none"},
            {"kind": "smagorinsky", "parameters": {"constant": 0.17, "norm_floor": 1e-10},
             "derivatives": "exact for the regularized closure", "second_order": true,
             "validity": "local eddy viscosity from the non-equilibrium momentum flux (Hou et al. 1996)",
             "limitations": "the flux norm is regularized with norm_floor; in turbulent regimes long-time-average gradients do not exist (FSI_DYNAMIC_TOPOLOGY.md 4.8) and are refused by the regime detection"},
            {"kind": "wale", "parameters": {"constant": 0.5, "denominator_floor": 1e-12},
             "derivatives": "exact", "second_order": false,
             "validity": "wall-adapting eddy viscosity from finite-difference velocity gradients (Nicoud and Ducros 1999)",
             "limitations": "non-local (two passes per substep); invariant powers not twice differentiable at zero gradient: second-order action refused; same regime restriction as smagorinsky"}
        ],
        "coupling_law": [
            {"kind": "psm_superposition", "derivatives": "exact", "second_order": true,
             "validity": "default moving-solid coupling: partially saturated cells, superposition operator (non-equilibrium relaxed around the solid velocity)",
             "limitations": "diffuse interface of the push-forward kernel width; the fluid inside the body moves with it and adds its inertia to the solid"},
            {"kind": "psm", "derivatives": "exact", "second_order": true,
             "validity": "partially saturated cells with the Noble-Torczynski operator",
             "limitations": "the non-equilibrium is reflected, not damped, inside saturated regions: saturated bodies in sustained motion can become unstable (observed on a moving saturated plate); prefer psm_superposition"},
            {"kind": "brinkman", "parameters": {"drag_max_per_s": null, "drag_shape": null}, "derivatives": "exact", "second_order": true,
             "validity": "Brinkman penalization with the RAMP resistance and implicit half-step velocity",
             "limitations": "no-slip only as drag_max * dt_f becomes large; penalization error of order 1/drag"},
            {"kind": "interpolated_bounce_back", "derivatives": "refused", "second_order": false,
             "validity": "verification only: sharp rigid bodies in prescribed translation (Bouzidi-Firdaouss-Lallemand with refill)",
             "limitations": "links and refills are discrete events: every sensitivity request answers 'interpolated bounce-back is a non-differentiable verification mode'; no ports, sponges, observables or closures"},
            {"kind": "immersed_boundary", "parameters": {"iterations": 5}, "derivatives": "refused", "second_order": false,
             "validity": "verification only: rigid bodies with an explicit surface (circle or sphere markers, or authored markers with weights) in prescribed translation; multi-direct forcing (Luo et al. 2007, Wang et al. 2008) with the C2 cubic B-spline kernel of the push-forward; momentum between fluid and body conserved exactly",
             "limitations": "needs an explicit surface, which a density design does not have, so it is not part of the design chain: every sensitivity request answers 'the multi-direct-forcing immersed boundary is a verification mode without design sensitivities'; the enclosed fluid moves with the body (its inertia is in the exchanged force); diffuse interface of four cells; no ports, sponges, observables or closures"},
            {"kind": "interpolated_bounce_back_differentiable", "status": "not provided",
             "reason": "the interpolated bounce-back map is smooth only between link-crossing and refill events; a moving boundary crosses links every few substeps, so a derivative of the fixed-event map omits the event contributions and is not the derivative of the objective (misleading for gradient-based design); the verification mode keeps its typed refusal"}
        ],
        "psm_weighting": [
            {"kind": "noble_torczynski", "derivatives": "exact", "second_order": true,
             "validity": "B = eps (tau+ - 1/2) / ((1 - eps) + (tau+ - 1/2)) for both partially saturated operators",
             "limitations": "partially saturated cells block less at small tau+ - 1/2: the pushed-forward fill of a solid must saturate (preflight blocking_saturation)"},
            {"kind": "linear", "status": "not provided",
             "reason": "B = eps (tau-independent) is a candidate for a weaker viscosity dependence of the diffuse wall location; it changes the public coupling-law variants consumed by the FSI provider and needs its own F-1/F-2 evidence; recorded in docs/HANDOFF.md"}
        ],
        "entrained_inertia": [
            {"kind": "carried", "derivatives": "exact", "second_order": true,
             "validity": "default: the fluid inside a saturated region moves with the solid and its inertia (rho_f per saturated volume) is part of the solid's inertia",
             "limitations": "negligible for solids in a gas; for rho_s/rho_f near one it doubles the solid inertia unless the solid mass is reduced by rho_f times the blocking fraction (solid-side compensation of the FSI provider)"},
            {"kind": "compensated", "derivatives": "exact", "second_order": true,
             "validity": "the change of the entrained fluid momentum over the macro step, sum_c sigma(D_c) j_c at both endpoints (sigma = sat(D)/D), is returned to the solid through the transposed velocity map of the endpoint configurations (internal-mass correction with the smooth solid fraction)",
             "limitations": "use either this fluid-side compensation or the solid-mass reduction, never both; solid_force samples keep reporting the raw momentum exchange"}
        ],
        "time_scaling": [
            {"kind": "diffusive", "derivatives": "exact",
             "validity": "default: substeps chosen for accuracy/stability of the flow; the lattice sound speed dx/(sqrt(3) dt_f) only has to keep the Mach number below mach_limit (weakly compressible, incompressible limit)"},
            {"kind": "acoustic", "parameters": {"speed_of_sound_m_s": 343.0}, "derivatives": "exact",
             "validity": "substeps chosen so that the lattice sound speed equals the physical one (MovingLbmConfig::with_acoustic_scaling): the weakly compressible scheme then propagates sound directly (direct noise computation) for low-Mach flows; sponge layers are the non-reflecting boundaries (F-6)",
             "limitations": "isothermal sound speed; relative sound-speed error of the integer substep count reported (below 1/(2 m)); tau+ - 1/2 = sqrt(3) nu / (c0 dx) is tiny: air needs dx below about 75 micrometres for tau_min = 1e-3 (refused otherwise; prefer trt/mrt/cumulant), liquids are out of reach; with an autonomous period the time scale also scales the lattice sound speed, so acoustic scaling is exact for forced periods (time scale 1)"}
        ],
        "energy_equation": [
            {"kind": "isothermal", "derivatives": "exact", "validity": "every provided option: temperature does not enter the moving-occupancy flow"},
            {"kind": "thermal_double_distribution", "status": "not provided",
             "reason": "a temperature field on the moving occupancy (advection-diffusion populations or the finite-volume operators of crate::thermal, solid heat capacity and conductivity blended with the pushed-forward fraction, temperature-dependent viscosity) is feasible with exact derivatives but needs a third coupled field state (fluid, temperature, solid) and a thermal soft solid; recorded in docs/HANDOFF.md"}
        ],
        "compressibility": [
            {"kind": "weakly_compressible", "derivatives": "exact",
             "validity": "every provided option; far-field sound of a compact body also follows from the solid_force samples (Curle's dipole, a derivative-series functional of the kernel time functionals)"},
            {"kind": "multispeed_compressible", "status": "not provided",
             "reason": "the D3Q343 guided-equilibrium model of crate::compressible is experimental and couples to stationary solid storage only; a moving-occupancy variant needs a multispeed partially saturated operator and the implicit-function equilibrium in every substep (cost of hundreds of populations per cell); recorded in docs/HANDOFF.md"}
        ],
        "boundaries": [
            {"kind": "wall", "derivatives": "exact", "validity": "halfway bounce-back on fixed solid cells and non-periodic lattice faces"},
            {"kind": "velocity_port", "derivatives": "exact (including the time-scale dependence of the signal)",
             "validity": "non-equilibrium extrapolation (Guo, Zheng, Shi 2002); mean + amplitude sin(2 pi f t + phase) with a C2 ramp; per-cell profile",
             "limitations": "the inward neighbour must be a fluid, non-port cell; no corner closure"},
            {"kind": "pressure_port", "derivatives": "exact", "validity": "as velocity_port with prescribed density",
             "limitations": "as velocity_port"},
            {"kind": "symmetry", "derivatives": "exact",
             "validity": "free-slip mirror face of the lattice box (MovingLbmConfig::symmetry_faces): halfway specular reflection of the populations and the push-forward kernel folded onto the mirror image, so bodies may touch the face (half models)",
             "limitations": "non-periodic axes only, not on a port face; points beyond the face are refused; contact across the face is the solid's (rigid-plane barrier)"},
            {"kind": "sponge", "derivatives": "exact",
             "validity": "absorbing layer relaxing to a far-field equilibrium with a C2 ramp; measured reflection below 1 % at 16 cells; optional travelling far-field wave (convected gust: target u_inf + r(t) a sin(2 pi f (t - (x - x0).d/U_c) + phi), sponge.rs TravellingWave) so outlet and lateral layers absorb deviations from the convected disturbance instead of damping it",
             "limitations": "strength is a relaxation fraction per fluid step (lattice units); summed strength below one"}
        ],
        "sampling": [
            {"kind": "end", "validity": "samples of the last substep of every macro step"},
            {"kind": "macro_mean", "validity": "mean over the substeps of every macro step"}
        ],
        "observables": ["section_flux", "probe (pressure, velocity component, density)", "port_power", "kinetic_energy", "solid_force", "section_open_area (open flow area of lattice sections from the occupancy, smooth minimum over a range; exact partials through the push-forward)"],
        "admission": [
            "tau+ > 1/2 + tau_min at every time scale",
            "prescribed lattice velocities within lattice_velocity_limit",
            "Mach number of the flow within mach_limit",
            "positive and finite populations",
            "push-forward displacement per macro step at most half the kernel width",
            "push-forward kernel support inside the lattice along non-periodic axes"
        ],
        "preflight": [
            {"check": "blocking_saturation", "validity": "the fully dense solid must push forward to a fill D >= 1 + saturation_width somewhere (blocking weights scaled by a saturation margin, e.g. 1.2), otherwise partially saturated cells never block fully; MovingLbm::admit_blocking_saturation refuses with 'moving LBM admission refused (blocking_saturation)'"},
            {"check": "acoustic_scaling", "validity": "with acoustic time scaling the record states the substep count, the lattice sound speed and its relative error, and tau+"}
        ],
        "derivative_scope": "discrete_history_exact"
    })
}


