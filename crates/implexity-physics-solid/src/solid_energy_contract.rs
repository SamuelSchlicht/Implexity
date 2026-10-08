// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::CaeError;
use serde_json::{Value, json};

use crate::inelastic::{PlasticLaw, WorkEnergy};
use crate::mandel::{self, Mandel};
use crate::material::MaterialLaw;
use crate::material::idx;
use crate::solid_elements::SolidModel;
use crate::solid_history::SolidKernel;

#[must_use]
pub fn missing_contracts(model: &SolidModel) -> Vec<String> {
    let mut missing = Vec::new();
    if model.viscoelastic.is_some() {
        missing.push("viscoelastic_joint_energy_contract".to_string());
    }
    if model.history.is_some() {
        missing.push("material_history_joint_energy_contract".to_string());
    }
    if model.plastic.is_some_and(|p| p != PlasticLaw::J2LinearHardening) {
        missing.push("plasticity_work_energy_contract".to_string());
    }

    missing
}

#[must_use]
pub fn support(model: &SolidModel) -> Value {
    let missing = missing_contracts(model);
    let reversible = model.law.reversible_thermoelastic();
    json!({"schema": "implexity-solid-discrete-work/1",
        "complete_selected_storage_decomposition": missing.is_empty(),
        "material_thermal_form": if reversible { "entropy" } else { "sensible_enthalpy" },
        "thermodynamic_potential_declared": reversible,
        "missing_contracts": missing,
        "scope": "native_small_strain_quasistatic_fixed_design_per_history",
        "mechanical_work_added_as_heat": false,
        "changes_solve_admission": false})
}

pub fn quadratic_energy<S: Scalar>(strain: &Mandel<S>, e_modulus: S, nu: S) -> S {
    let tr = strain[0] + strain[1] + strain[2];
    let dev: Mandel<S> = std::array::from_fn(|i| strain[i] - tr * mandel::IDENTITY[i] / 3.0);
    let g = e_modulus / ((nu + 1.0) * 2.0);
    let k = e_modulus / ((-(nu * 2.0) + 1.0) * 3.0);
    g * mandel::dot(&dev, &dev) + k * tr * tr * 0.5
}

pub type Terms<S> = Vec<(String, Option<S>)>;

fn push<S>(out: &mut Terms<S>, key: &str, value: S) {
    out.push((key.to_string(), Some(value)));
}

fn unknown<S: Scalar>(out: &mut Terms<S>, w: S) {
    for key in [
        "stored_energy_increment_J",
        "backward_euler_defect_J",
        "unmodeled_thermal_exchange_J",
        "nonthermal_dissipation_J",
        "constitutive_work_identity_defect_J",
    ] {
        out.push((key.to_string(), None));
    }
    push(out, "endpoint_stress_work_J", w);
}

fn work_terms<S: Scalar>(w: &WorkEnergy<S>) -> [(&'static str, S); 8] {
    [
        ("stored_energy_J", w.stored),
        ("previous_stored_energy_J", w.previous_stored),
        ("stored_energy_increment_J", w.stored_increment),
        ("backward_euler_defect_J", w.backward_euler_defect),
        ("coefficient_exchange_J", w.coefficient_exchange),
        ("dissipation_increment_J", w.dissipation),
        ("endpoint_work_J", w.work),
        ("work_identity_defect_J", w.work_identity_defect),
    ]
}

#[allow(clippy::too_many_lines)]
pub fn element_step_terms<S: Scalar>(
    model: &SolidModel,
    grad0: &[[f64; 3]; 4],
    current: &[S],
    previous: &[S],
    design: &[S],
) -> Terms<S> {
    let f = model.fields(grad0, current, design);
    let o = model.fields(grad0, previous, design);
    let density = design[0];
    let stiffness = model.stiffness(density);
    let volume = f.volume;
    let integrate = |value: S| stiffness * value * volume;
    let strain_increment = mandel::sub(&f.strain, &o.strain);
    let w = integrate(mandel::dot(&f.stress, &strain_increment));
    let missing = missing_contracts(model);
    let mut out: Terms<S> = Vec::new();
    if model.law.reversible_thermoelastic() {
        if !missing.is_empty() {
            unknown(&mut out, w);
            return out;
        }
        let [a, b] = &model.materials;
        let terms = MaterialLaw::step_energy(
            a,
            b,
            design[4],
            density,
            stiffness,
            &f.temperature,
            &o.temperature,
            &f.strain,
            &o.strain,
        );
        let get = |key: &str| {
            let v = terms.get(key).copied().unwrap_or([S::zero(); 4]);
            (v[0] + v[1] + v[2] + v[3]) * volume / 4.0
        };

        for key in [
            "internal_energy_J_m3",
            "internal_energy_increment_J_m3",
            "endpoint_stress_work_J_m3",
            "numerical_energy_defect_J_m3",
            "entropy_thermal_storage_J_m3",
            "first_law_identity_residual_J_m3",
        ] {
            let value = if key == "endpoint_stress_work_J_m3" { w } else { get(key) };
            push(&mut out, key.trim_end_matches("_m3"), value);
        }
        push(&mut out, "stored_energy_increment_J", get("internal_energy_increment_J_m3"));
        push(&mut out, "backward_euler_defect_J", get("numerical_energy_defect_J_m3"));
        push(&mut out, "caloric_storage_separately_added_J", S::zero());
        push(&mut out, "unmodeled_thermal_exchange_J", S::zero());
        push(&mut out, "nonthermal_dissipation_J", S::zero());
        push(&mut out, "constitutive_work_identity_defect_J", -get("first_law_identity_residual_J_m3"));
        return out;
    }
    if model.viscoelastic.is_some() {
        unknown(&mut out, w);
        return out;
    }
    let (e_now, nu_now) = (f.prop.get(idx::E), f.prop.get(idx::NU));
    let (e_old, nu_old) = (o.prop.get(idx::E), o.prop.get(idx::NU));
    let delta_ee = mandel::sub(&f.elastic, &o.elastic);
    let elastic_now = quadratic_energy(&f.elastic, e_now, nu_now);
    let elastic_old = quadratic_energy(&o.elastic, e_old, nu_old);
    let numerical = quadratic_energy(&delta_ee, e_now, nu_now);
    let exchange = quadratic_energy(&o.elastic, e_now, nu_now) - elastic_old;
    let elastic_work = mandel::dot(&f.stress, &delta_ee);

    let delta_elastic = elastic_work - numerical + exchange;
    let mut eigen_change: Mandel<S> =
        std::array::from_fn(|i| (f.strain[i] - f.elastic[i]) - (o.strain[i] - o.elastic[i]));
    let layout = &model.layout;
    if model.plastic.is_some() {
        let r = layout.plastic_strain();
        for i in 0..6 {
            eigen_change[i] -= f.state[r.start + i] - o.state[r.start + i];
        }
    }
    if model.creep.is_some() {
        let r = layout.creep_strain();
        for i in 0..6 {
            eigen_change[i] -= f.state[r.start + i] - o.state[r.start + i];
        }
    }
    let eigen_work = mandel::dot(&f.stress, &eigen_change);
    push(&mut out, "elastic_stored_energy_J", integrate(elastic_now));
    push(&mut out, "previous_elastic_stored_energy_J", integrate(elastic_old));
    push(&mut out, "elastic_stored_energy_increment_J", integrate(delta_elastic));
    push(&mut out, "elastic_backward_euler_defect_J", integrate(numerical));
    push(&mut out, "elastic_coefficient_exchange_J", integrate(exchange));
    push(&mut out, "thermal_eigenstrain_work_J", integrate(eigen_work));
    push(&mut out, "endpoint_stress_work_J", w);
    let mut store = delta_elastic;
    let mut defect = numerical;
    let mut exchange_total = exchange;
    let mut unheated = S::zero();
    let mut identity = S::zero();
    let mut component = |name: &str, terms: WorkEnergy<S>, heat: S, out: &mut Terms<S>| {
        store += terms.stored_increment;
        defect += terms.backward_euler_defect;
        exchange_total += terms.coefficient_exchange;
        unheated += terms.dissipation - heat;
        identity += terms.work_identity_defect;
        for (key, value) in work_terms(&terms) {
            push(out, &format!("{name}_{key}"), integrate(value));
        }
        push(out, &format!("{name}_heat_increment_J"), integrate(heat));
        push(out, &format!("{name}_nonthermal_dissipation_J"), integrate(terms.dissipation - heat));
    };
    if let Some(plastic) = model.plastic {
        let r = layout.plastic();
        let (a, b) = (&f.state[r.clone()], &o.state[r]);
        if let Some(terms) = plastic.work_energy_increment(&f.stress, a, b, &f.prop, &o.prop) {
            let heat = plastic.heat(a, b, &f.prop, &o.prop);
            component("plasticity", terms, heat, &mut out);
        }
    }
    if let Some(creep) = model.creep {
        let r = layout.creep();
        let (a, b) = (&f.state[r.clone()], &o.state[r]);
        let terms = creep.work_energy_increment(&f.stress, a, b);
        let heat = creep.dissipated_increment(&f.stress, a, b);
        component("creep", terms, heat, &mut out);
    }
    if !missing.is_empty() {
        for key in [
            "stored_energy_increment_J",
            "backward_euler_defect_J",
            "unmodeled_thermal_exchange_J",
            "nonthermal_dissipation_J",
            "constitutive_work_identity_defect_J",
        ] {
            out.push((key.to_string(), None));
        }
        return out;
    }
    push(&mut out, "stored_energy_increment_J", integrate(store));
    push(&mut out, "backward_euler_defect_J", integrate(defect));
    push(&mut out, "unmodeled_thermal_exchange_J", integrate(eigen_work - exchange_total));
    push(&mut out, "nonthermal_dissipation_J", integrate(unheated));
    push(&mut out, "constitutive_work_identity_defect_J", integrate(identity));
    out
}



pub fn solid_step_terms(
    kernel: &SolidKernel,
    n: usize,
    current: &[f64],
    previous: &[f64],
    design: &[f64],
) -> Result<Terms<f64>, CaeError> {
    if !(1..kernel.nt).contains(&n) {
        return Err(CaeError::contract("solid energy requires a positive authored history interval"));
    }
    if current.len() != kernel.state_size
        || previous.len() != kernel.state_size
        || design.len() != 2 * kernel.nc + 3
    {
        return Err(CaeError::contract(
            "solid energy requires real current/previous/design vectors of the native shapes",
        ));
    }
    let (cur_data, prev_data) = kernel.local_data(n);
    let mut total: Terms<f64> = Vec::new();
    for e in 0..kernel.ne {
        let cur = kernel.element_local(n, e, current, &cur_data);
        let prev = kernel.element_local(n - 1, e, previous, &prev_data);
        let x = kernel.element_design(e, design);
        let terms = element_step_terms(&kernel.model, &kernel.mesh.gradients[e], &cur, &prev, &x);
        if total.is_empty() {
            total = terms;
        } else {
            for ((_, acc), (_, v)) in total.iter_mut().zip(terms) {
                if let (Some(a), Some(v)) = (acc.as_mut(), v) {
                    *a += v;
                }
            }
        }
    }
    Ok(total)
}

#[must_use]
pub fn terms_json(terms: &Terms<f64>) -> Value {
    Value::Object(terms.iter().map(|(k, v)| (k.clone(), v.map_or(Value::Null, |x| json!(x)))).collect())
}
