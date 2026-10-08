// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_geometry::field_registration::GridRegistration;
use serde_json::Value;

use crate::error::{AResult, AuthoringError};
use crate::py::{canonical_ascii_spaced, sha256_hex};

fn verr(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("ValueError", message)
}

const ACTIONS: [&str; 6] =
    ["designable", "fixed_solid", "fixed_void", "preserve_current", "refine", "control_proposal"];

#[derive(Clone, Debug, PartialEq)]
pub struct SensitivitySelection {
    pub mask: Vec<bool>,
    pub response: String,
    pub objective_direction: String,
    pub threshold: f64,
    pub sign: String,
    pub registration: GridRegistration,
}

impl SensitivitySelection {

    pub fn new(
        mask: Vec<bool>,
        response: &str,
        objective_direction: &str,
        threshold: f64,
        sign: &str,
        registration: GridRegistration,
    ) -> AResult<Self> {
        if mask.len() != registration.shape.iter().product::<usize>() {
            return Err(verr("selection mask and registration shape differ"));
        }
        Ok(Self {
            mask,
            response: response.into(),
            objective_direction: objective_direction.into(),
            threshold,
            sign: sign.into(),
            registration,
        })
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.mask.iter().filter(|m| **m).count()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TopologyAuthoringState {
    pub revision: i64,
    pub registration: GridRegistration,
    pub designable: Vec<bool>,
    pub fixed_solid: Vec<bool>,
    pub fixed_void: Vec<bool>,
    pub preserve_current: Vec<bool>,
    pub refinement: Vec<bool>,
    pub control: Option<Vec<f64>>,
}

impl TopologyAuthoringState {

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        revision: i64,
        registration: GridRegistration,
        designable: Vec<bool>,
        fixed_solid: Vec<bool>,
        fixed_void: Vec<bool>,
        preserve_current: Vec<bool>,
        refinement: Vec<bool>,
        control: Option<Vec<f64>>,
    ) -> AResult<Self> {
        let n: usize = registration.shape.iter().product();
        for (name, m) in [
            ("designable", &designable),
            ("fixed_solid", &fixed_solid),
            ("fixed_void", &fixed_void),
            ("preserve_current", &preserve_current),
            ("refinement", &refinement),
        ] {
            if m.len() != n {
                return Err(verr(format!("{name} does not match registration shape")));
            }
        }
        if control.as_ref().is_some_and(|c| c.len() != n) {
            return Err(verr("control does not match registration shape"));
        }
        let s = Self {
            revision,
            registration,
            designable,
            fixed_solid,
            fixed_void,
            preserve_current,
            refinement,
            control,
        };
        s.validate()?;
        Ok(s)
    }


    pub fn validate(&self) -> AResult<()> {
        for i in 0..self.designable.len() {
            let (s, v, p) = (self.fixed_solid[i], self.fixed_void[i], self.preserve_current[i]);
            if (s && v) || (s && p) || (v && p) {
                return Err(verr("topology authoring masks conflict"));
            }
        }
        for i in 0..self.designable.len() {
            if self.designable[i] && (self.fixed_solid[i] || self.fixed_void[i] || self.preserve_current[i]) {
                return Err(verr("designable mask overlaps a fixed topology role"));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut bytes = self.revision.to_string().into_bytes();
        bytes.extend_from_slice(canonical_ascii_spaced(&self.registration.to_wire()).as_bytes());
        for m in
            [&self.designable, &self.fixed_solid, &self.fixed_void, &self.preserve_current, &self.refinement]
        {
            bytes.extend(m.iter().map(|b| u8::from(*b)));
        }
        if let Some(c) = &self.control {
            for v in c {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        sha256_hex(&bytes)
    }
}

fn connected_component(mask: &[bool], shape: [usize; 3], seed: [usize; 3]) -> Vec<bool> {
    let idx = |p: [usize; 3]| (p[0] * shape[1] + p[1]) * shape[2] + p[2];
    let mut result = vec![false; mask.len()];
    if !mask[idx(seed)] {
        return result;
    }
    let mut stack = vec![seed];
    result[idx(seed)] = true;
    let steps: [[i64; 3]; 6] = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];
    while let Some(p) = stack.pop() {
        for d in steps {
            #[allow(clippy::cast_possible_wrap)]
            let q = [p[0] as i64 + d[0], p[1] as i64 + d[1], p[2] as i64 + d[2]];
            #[allow(clippy::cast_possible_wrap)]
            if (0..3).all(|i| q[i] >= 0 && q[i] < shape[i] as i64) {
                #[allow(clippy::cast_sign_loss)]
                let qu = [q[0] as usize, q[1] as usize, q[2] as usize];
                let k = idx(qu);
                if mask[k] && !result[k] {
                    result[k] = true;
                    stack.push(qu);
                }
            }
        }
    }
    result
}


pub fn select_sensitivity_region(
    values: &[f64],
    registration: &GridRegistration,
    response: &str,
    objective_direction: &str,
    sign: &str,
    percentile: f64,
    seed_world: Option<[f64; 3]>,
) -> AResult<SensitivitySelection> {
    if values.len() != registration.shape.iter().product::<usize>() {
        return Err(verr("sensitivity field and registration differ"));
    }
    if !values.iter().all(|v| v.is_finite()) {
        return Err(verr("sensitivity field contains non-finite values"));
    }
    let direction = match objective_direction {
        "minimize" => 1.0,
        "maximize" => -1.0,
        _ => return Err(verr("objective_direction must be minimize or maximize")),
    };
    let score: Vec<f64> = match sign {
        "favourable_add" => values.iter().map(|g| (-(direction * g)).max(0.0)).collect(),
        "favourable_remove" => values.iter().map(|g| (direction * g).max(0.0)).collect(),
        "magnitude" => values.iter().map(|g| (direction * g).abs()).collect(),
        _ => return Err(verr("unsupported sensitivity sign selector")),
    };
    let positive: Vec<f64> = score.iter().copied().filter(|s| *s > 0.0).collect();
    let threshold = if positive.is_empty() {
        f64::INFINITY
    } else {
        implexity_mesh::numeric::percentile(&positive, percentile)
    };
    let mut mask: Vec<bool> = score.iter().map(|s| *s >= threshold).collect();
    if let Some(w) = seed_world {
        let s = registration.nearest_index(w, false)?;
        let seed = s.map(|v| usize::try_from(v).unwrap_or(0));
        mask = connected_component(&mask, registration.shape, seed);
    }
    SensitivitySelection::new(mask, response, objective_direction, threshold, sign, registration.clone())
}

#[must_use]
pub fn smooth(values: &[f64], shape: [usize; 3], iterations: usize) -> Vec<f64> {
    let mut out = values.to_vec();
    let (nx, ny, nz) = (shape[0], shape[1], shape[2]);
    let idx = |i: usize, j: usize, k: usize| (i * ny + j) * nz + k;
    for _ in 0..iterations {
        let mut acc = out.clone();
        for axis in 0..3 {
            for i in 0..nx {
                for j in 0..ny {
                    for k in 0..nz {
                        let (minus, plus) = match axis {
                            0 => (idx((i + nx - 1) % nx, j, k), idx((i + 1) % nx, j, k)),
                            1 => (idx(i, (j + ny - 1) % ny, k), idx(i, (j + 1) % ny, k)),
                            _ => (idx(i, j, (k + nz - 1) % nz), idx(i, j, (k + 1) % nz)),
                        };

                        acc[idx(i, j, k)] += out[minus] + out[plus];
                    }
                }
            }
        }
        out = acc.iter().map(|a| a / 7.0).collect();
    }
    out
}


pub fn apply_selection(
    state: &TopologyAuthoringState,
    selection: &SensitivitySelection,
    action: &str,
    proposal_step: f64,
    sensitivity_values: Option<&[f64]>,
) -> AResult<TopologyAuthoringState> {
    if !ACTIONS.contains(&action) {
        return Err(verr(format!("unsupported topology authoring action: {action}")));
    }
    if state.registration.to_wire()["registration_id"] != selection.registration.to_wire()["registration_id"]
    {
        return Err(verr("selection and topology state use different registrations"));
    }
    let mut result = state.clone();
    let mask = &selection.mask;
    match action {
        "designable" | "fixed_solid" | "fixed_void" | "preserve_current" => {
            for (i, m) in mask.iter().enumerate() {
                if *m {
                    result.designable[i] = action == "designable";
                    result.fixed_solid[i] = action == "fixed_solid";
                    result.fixed_void[i] = action == "fixed_void";
                    result.preserve_current[i] = action == "preserve_current";
                }
            }
        }
        "refine" => {
            for (i, m) in mask.iter().enumerate() {
                if *m {
                    result.refinement[i] = true;
                }
            }
        }
        _ => {
            let Some(control) = result.control.clone() else {
                return Err(verr("control proposal requires a topology control field"));
            };
            let Some(gradient) = sensitivity_values else {
                return Err(verr("control proposal requires the source sensitivity field"));
            };
            if gradient.len() != control.len() {
                return Err(verr("source sensitivity shape differs from topology state"));
            }
            let direction = if selection.objective_direction == "minimize" { -1.0 } else { 1.0 };
            let selected: Vec<f64> =
                gradient.iter().zip(mask).filter(|(_, m)| **m).map(|(g, _)| g.abs()).collect();
            let scale =
                if selected.is_empty() { 0.0 } else { implexity_mesh::numeric::percentile(&selected, 90.0) };
            if scale > 0.0 {
                let update: Vec<f64> = gradient
                    .iter()
                    .zip(mask)
                    .map(|(g, m)| if *m { direction * proposal_step * g / scale } else { 0.0 })
                    .collect();
                let update = smooth(&update, result.registration.shape, 2);
                let mut next = control.clone();
                for i in 0..next.len() {
                    let free = result.designable[i]
                        && !result.fixed_solid[i]
                        && !result.fixed_void[i]
                        && !result.preserve_current[i];
                    if free {
                        next[i] = control[i] + update[i];
                    }
                }
                result.control = Some(next);
            }
        }
    }
    result.revision += 1;
    result.validate()?;
    Ok(result)
}

#[derive(Clone, Debug)]
pub struct SensitivityAuthoringTransaction {
    initial: TopologyAuthoringState,
    expected_revision: i64,
    preview: TopologyAuthoringState,
    closed: bool,
}

impl SensitivityAuthoringTransaction {

    pub fn new(initial: TopologyAuthoringState, expected_revision: i64) -> AResult<Self> {
        if initial.revision != expected_revision {
            return Err(verr("stale topology authoring revision"));
        }
        Ok(Self { preview: initial.clone(), initial, expected_revision, closed: false })
    }


    pub fn preview(
        &mut self,
        selection: &SensitivitySelection,
        action: &str,
        proposal_step: f64,
        sensitivity_values: Option<&[f64]>,
    ) -> AResult<TopologyAuthoringState> {
        if self.closed {
            return Err(AuthoringError::runtime("RuntimeError", "transaction is closed"));
        }
        self.preview = apply_selection(&self.initial, selection, action, proposal_step, sensitivity_values)?;
        Ok(self.preview.clone())
    }


    pub fn commit(&mut self, current_revision: i64) -> AResult<TopologyAuthoringState> {
        if self.closed {
            return Err(AuthoringError::runtime("RuntimeError", "transaction is closed"));
        }
        if current_revision != self.expected_revision {
            return Err(verr("topology authoring commit conflicts with a newer revision"));
        }
        self.closed = true;
        Ok(self.preview.clone())
    }


    pub fn cancel(&mut self) -> AResult<TopologyAuthoringState> {
        if self.closed {
            return Err(AuthoringError::runtime("RuntimeError", "transaction is closed"));
        }
        self.closed = true;
        Ok(self.initial.clone())
    }
}


pub fn registration(value: &Value) -> AResult<GridRegistration> {
    Ok(GridRegistration::from_wire(value)?)
}
