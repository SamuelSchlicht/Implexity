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



use implexity_ad::{AdError, Tape, Var};

use crate::d3q19::{C, Grid, OPPOSITE, POSITIVE, Q};
use crate::solver::LbmProblem;
use crate::thermal::{conductances, divergence, roll_axis};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConservativeStencil {
    pub velocities: Vec<[i32; 3]>,
    pub positive: Vec<usize>,
    pub opposite: Vec<usize>,
}

impl ConservativeStencil {

    pub fn new(velocities: &[[i64; 3]]) -> Result<Self, String> {
        if velocities.len() < 3 || velocities.iter().flatten().any(|v| !(-1..=1).contains(v)) {
            return Err("velocity stencil requires integer triples in [-1,1]".into());
        }
        #[allow(clippy::cast_possible_truncation)]
        let rows: Vec<[i32; 3]> = velocities.iter().map(|c| c.map(|v| v as i32)).collect();
        let unique: std::collections::BTreeSet<[i32; 3]> = rows.iter().copied().collect();
        if unique.len() != rows.len() || !unique.contains(&[0, 0, 0]) {
            return Err("velocity stencil requires unique velocities and one rest direction".into());
        }
        let find = |c: [i32; 3]| rows.iter().position(|r| *r == c);
        let mut opposite = Vec::with_capacity(rows.len());
        for c in &rows {
            match find(c.map(|v| -v)) {
                Some(o) => opposite.push(o),
                None => return Err("velocity stencil requires every opposite direction".into()),
            }
        }
        let positive = (0..rows.len())
            .filter(|&i| rows[i].iter().find(|v| **v != 0).copied().unwrap_or(0) > 0)
            .collect();
        Ok(Self { velocities: rows, positive, opposite })
    }

    #[must_use]
    pub fn d3q19() -> Self {
        Self { velocities: C.to_vec(), positive: POSITIVE.to_vec(), opposite: OPPOSITE.to_vec() }
    }

    fn offset(&self, i: usize) -> [i64; 3] {
        self.velocities[i].map(i64::from)
    }
}

#[must_use]
pub fn mass_rates_from_post(p: &LbmProblem, post: &[f64]) -> Vec<Vec<f64>> {
    let grid = p.grid;
    let blocked = p.streaming.blocked();
    let scale = p.density_kg_m3 * p.spacing_m.powi(3) / p.step_s;
    POSITIVE
        .iter()
        .map(|&i| {
            let o = OPPOSITE[i];
            let c = [i64::from(C[i][0]), i64::from(C[i][1]), i64::from(C[i][2])];
            (0..grid.cells())
                .map(|x| {
                    let valid = !blocked[x * Q + o] && !p.solid_mask[x];
                    if valid { (post[x * Q + i] - post[grid.wrap(x, c) * Q + o]) * scale } else { 0.0 }
                })
                .collect()
        })
        .collect()
}

pub fn mass_rates_from_post_vjp(p: &LbmProblem, rates_bar: &[Vec<f64>], post_bar: &mut [f64]) {
    let grid = p.grid;
    let blocked = p.streaming.blocked();
    let scale = p.density_kg_m3 * p.spacing_m.powi(3) / p.step_s;
    for (k, &i) in POSITIVE.iter().enumerate() {
        let o = OPPOSITE[i];
        let c = [i64::from(C[i][0]), i64::from(C[i][1]), i64::from(C[i][2])];
        for x in 0..grid.cells() {
            let valid = !blocked[x * Q + o] && !p.solid_mask[x];
            let g = rates_bar[k][x];
            if valid && g != 0.0 {
                post_bar[x * Q + i] += g * scale;
                post_bar[grid.wrap(x, c) * Q + o] -= g * scale;
            }
        }
    }
}

#[must_use]
pub fn link_divergence(grid: &Grid, rates: &[Vec<f64>], stencil: &ConservativeStencil) -> Vec<f64> {
    let mut out = vec![0.0; grid.cells()];
    for (&i, m) in stencil.positive.iter().zip(rates) {
        let back = grid.roll(m, stencil.offset(i));
        for x in 0..grid.cells() {
            out[x] += m[x] - back[x];
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transport {
    pub advective_power_divergence_w: Vec<f64>,
    pub boundary_power_w: Vec<f64>,
    pub net_mass_rate_kg_s: Vec<f64>,
    pub capacity_rate_w_k: Vec<f64>,
    pub outgoing_conductance_w_k: Vec<f64>,
}

#[must_use]
pub fn sensible_transport(
    grid: &Grid,
    temperature: &[f64],
    rates: &[Vec<f64>],
    cp: f64,
    boundary_mass_rate: &[f64],
    reservoir_temperature: &[f64],
    outgoing_seed: &[f64],
    stencil: &ConservativeStencil,
) -> Transport {
    let n = grid.cells();
    let mut outgoing = outgoing_seed.to_vec();
    let mut advective = Vec::with_capacity(rates.len());
    for (&i, m) in stencil.positive.iter().zip(rates) {
        let c = stencil.offset(i);
        let other = grid.roll(temperature, c.map(|v| -v));
        let m_back = grid.roll(m, c);
        advective.push(
            (0..n)
                .map(|x| cp * if m[x] >= 0.0 { m[x] * temperature[x] } else { m[x] * other[x] })
                .collect::<Vec<f64>>(),
        );
        for x in 0..n {
            outgoing[x] += cp * (m[x].max(0.0) + (-m_back[x]).max(0.0));
        }
    }
    let boundary: Vec<f64> = (0..n)
        .map(|x| {
            let e = boundary_mass_rate[x];
            cp * if e >= 0.0 { e * reservoir_temperature[x] } else { e * temperature[x] }
        })
        .collect();
    for x in 0..n {
        outgoing[x] += cp * (-boundary_mass_rate[x]).max(0.0);
    }
    let divergence = link_divergence(grid, rates, stencil);
    let net: Vec<f64> = (0..n).map(|x| boundary_mass_rate[x] - divergence[x]).collect();
    Transport {
        advective_power_divergence_w: link_divergence(grid, &advective, stencil),
        boundary_power_w: boundary,
        capacity_rate_w_k: net.iter().map(|v| cp * v).collect(),
        net_mass_rate_kg_s: net,
        outgoing_conductance_w_k: outgoing,
    }
}

#[derive(Clone, Debug)]
pub struct EnergyInputs<'a> {
    pub grid: Grid,
    pub periodic: [bool; 3],
    pub spacing_m: f64,
    pub step_s: f64,
    pub cp: f64,
    pub contact: Option<&'a [[f64; 3]]>,
    pub source_w: &'a [f64],
    pub boundary_mass_rate: &'a [f64],
    pub reservoir: &'a [f64],
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkEnergyStep {
    pub temperature_k: Vec<f64>,
    pub capacity_j_k: Vec<f64>,
    pub stored_energy_j: Vec<f64>,
    pub outgoing_fraction: Vec<f64>,
    pub boundary_power_w: Vec<f64>,
    pub wall_convection_power_w: Vec<f64>,
}

#[must_use]
pub fn energy_step(
    inputs: &EnergyInputs<'_>,
    temperature: &[f64],
    capacity: &[f64],
    conductivity: &[f64],
    rates: &[Vec<f64>],
    wall_conductance: &[f64],
    wall_reservoir_power: &[f64],
) -> LinkEnergyStep {
    let grid = inputs.grid;
    let n = grid.cells();
    let dt = inputs.step_s;
    let g = conductances(&grid, conductivity, inputs.spacing_m, inputs.periodic, inputs.contact);
    let conductive: [Vec<f64>; 3] = std::array::from_fn(|a| {
        let other = roll_axis(&grid, temperature, a, -1);
        (0..n).map(|x| g[a][x] * (temperature[x] - other[x])).collect()
    });
    let mut outgoing = vec![0.0; n];
    for (a, ga) in g.iter().enumerate() {
        let back = roll_axis(&grid, ga, a, 1);
        for x in 0..n {
            outgoing[x] += ga[x] + back[x];
        }
    }
    let transport = sensible_transport(
        &grid,
        temperature,
        rates,
        inputs.cp,
        inputs.boundary_mass_rate,
        inputs.reservoir,
        &outgoing,
        &ConservativeStencil::d3q19(),
    );
    let wall_power: Vec<f64> =
        (0..n).map(|x| wall_reservoir_power[x] - wall_conductance[x] * temperature[x]).collect();
    let outgoing: Vec<f64> =
        (0..n).map(|x| transport.outgoing_conductance_w_k[x] + wall_conductance[x]).collect();
    let next_capacity: Vec<f64> =
        (0..n).map(|x| capacity[x] + dt * inputs.cp * transport.net_mass_rate_kg_s[x]).collect();
    let cond_div = divergence(&grid, &conductive);
    let energy: Vec<f64> = (0..n)
        .map(|x| {
            capacity[x] * temperature[x]
                + dt * (inputs.source_w[x] + transport.boundary_power_w[x] + wall_power[x]
                    - cond_div[x]
                    - transport.advective_power_divergence_w[x])
        })
        .collect();
    LinkEnergyStep {
        temperature_k: (0..n).map(|x| energy[x] / next_capacity[x]).collect(),
        capacity_j_k: next_capacity,
        stored_energy_j: energy,
        outgoing_fraction: (0..n).map(|x| dt * outgoing[x] / capacity[x]).collect(),
        boundary_power_w: transport.boundary_power_w,
        wall_convection_power_w: wall_power,
    }
}

#[derive(Clone, Debug)]
pub struct RollIndex {
    pub forward: [Vec<usize>; 3],
    pub backward: [Vec<usize>; 3],
    pub link_forward: Vec<Vec<usize>>,
    pub link_backward: Vec<Vec<usize>>,
}

impl RollIndex {
    #[must_use]
    pub fn new(grid: &Grid) -> Self {
        let n = grid.cells();
        let axis = |a: usize, s: i64| {
            let mut o = [0i64; 3];
            o[a] = s;
            (0..n).map(|x| grid.wrap(x, o)).collect::<Vec<usize>>()
        };
        let link = |i: usize, s: i64| {
            let o = [i64::from(C[i][0]) * s, i64::from(C[i][1]) * s, i64::from(C[i][2]) * s];
            (0..n).map(|x| grid.wrap(x, o)).collect::<Vec<usize>>()
        };
        Self {
            forward: std::array::from_fn(|a| axis(a, 1)),
            backward: std::array::from_fn(|a| axis(a, -1)),
            link_forward: POSITIVE.iter().map(|&i| link(i, 1)).collect(),
            link_backward: POSITIVE.iter().map(|&i| link(i, -1)).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TapedStep {
    pub temperature: Var,
    pub capacity: Var,
}


pub fn energy_step_tape(
    tape: &mut Tape,
    inputs: &EnergyInputs<'_>,
    rolls: &RollIndex,
    temperature: Var,
    capacity: Var,
    conductance: [Var; 3],
    rates: &[Var],
    boundary_mass_rate: Var,
    wall_conductance: Var,
    wall_reservoir_power: Var,
) -> Result<TapedStep, AdError> {
    let dt = inputs.step_s;
    let cp = inputs.cp;
    let n = inputs.grid.cells();

    let mut cond_div: Option<Var> = None;
    for a in 0..3 {
        let other = tape.gather(temperature, rolls.forward[a].clone())?;
        let diff = tape.sub(temperature, other)?;
        let r = tape.mul(conductance[a], diff)?;
        let back = tape.gather(r, rolls.backward[a].clone())?;
        let term = tape.sub(r, back)?;
        cond_div = Some(match cond_div {
            None => term,
            Some(acc) => tape.add(acc, term)?,
        });
    }

    let mut adv_div: Option<Var> = None;
    let mut mass_div: Option<Var> = None;
    for (k, &m) in rates.iter().enumerate() {
        let m_values = tape.value(m)?.to_vec();
        let other = tape.gather(temperature, rolls.link_forward[k].clone())?;
        let upwind: Vec<bool> = m_values.iter().map(|v| *v >= 0.0).collect();
        let chosen = tape.select(&upwind, temperature, other)?;
        let power = tape.mul(m, chosen)?;
        let power = tape.scale(power, cp)?;
        let back = tape.gather(power, rolls.link_backward[k].clone())?;
        let term = tape.sub(power, back)?;
        adv_div = Some(match adv_div {
            None => term,
            Some(acc) => tape.add(acc, term)?,
        });
        let m_back = tape.gather(m, rolls.link_backward[k].clone())?;
        let mterm = tape.sub(m, m_back)?;
        mass_div = Some(match mass_div {
            None => mterm,
            Some(acc) => tape.add(acc, mterm)?,
        });
    }
    let zero = tape.constant(vec![0.0; n]);
    let adv_div = adv_div.unwrap_or(zero);
    let mass_div = mass_div.unwrap_or(zero);
    let cond_div = cond_div.unwrap_or(zero);

    let e_values = tape.value(boundary_mass_rate)?.to_vec();
    let inflow: Vec<bool> = e_values.iter().map(|v| *v >= 0.0).collect();
    let reservoir = tape.constant(inputs.reservoir.to_vec());
    let chosen = tape.select(&inflow, reservoir, temperature)?;
    let boundary = tape.mul(boundary_mass_rate, chosen)?;
    let boundary = tape.scale(boundary, cp)?;

    let gt = tape.mul(wall_conductance, temperature)?;
    let wall = tape.sub(wall_reservoir_power, gt)?;

    let net = tape.sub(boundary_mass_rate, mass_div)?;
    let dcap = tape.scale(net, dt * cp)?;
    let next_capacity = tape.add(capacity, dcap)?;
    let source = tape.constant(inputs.source_w.to_vec());
    let mut power = tape.add(source, boundary)?;
    power = tape.add(power, wall)?;
    power = tape.sub(power, cond_div)?;
    power = tape.sub(power, adv_div)?;
    let power = tape.scale(power, dt)?;
    let stored = tape.mul(capacity, temperature)?;
    let energy = tape.add(stored, power)?;
    let t_next = tape.div(energy, next_capacity)?;
    Ok(TapedStep { temperature: t_next, capacity: next_capacity })
}

