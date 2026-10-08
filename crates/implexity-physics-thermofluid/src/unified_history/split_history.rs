// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::error::{CaeError,CaeResult};
use serde_json::{Value,json};

#[derive(Clone,Debug)]
pub struct SplitHistoryOptions {
    pub snapshot_times_s: Vec<f64>,
    pub local_substeps: usize,
    pub coupling_sweeps: usize,
    pub provenance: String,
    pub aged_condition: bool,
    pub stress_relaxing_condition: bool,
}
impl SplitHistoryOptions {
    pub fn validate(&self)->CaeResult<()> {
        if self.snapshot_times_s.len()<2 || self.snapshot_times_s[0]!=0. || self.snapshot_times_s.iter().any(|t|!t.is_finite()) || self.snapshot_times_s.windows(2).any(|w|w[1]<=w[0]) || self.local_substeps==0 || !matches!(self.coupling_sweeps,1|2) || self.provenance.trim().is_empty() {
            return Err(CaeError::contract("invalid explicitly approximate split history options"));
        }
        if self.aged_condition && (self.snapshot_times_s.len()!=2 || self.local_substeps!=1 || self.coupling_sweeps!=2) {return Err(CaeError::contract("aged condition requires one age interval and exactly reference plus aged equilibria"));}
        Ok(())
    }
    pub fn cache_key(&self,design_state_id:&str,runtime_source_sha256:&str,physical_problem_sha256:&str)->CaeResult<String> {
        self.validate()?;
        if design_state_id.is_empty() || runtime_source_sha256.is_empty() || physical_problem_sha256.is_empty() {return Err(CaeError::contract("split history cache identity requires design, physical problem and source binding"));}
        let value=json!({"approximation":self.identity(),"design_state_id":design_state_id,"runtime_source_sha256":runtime_source_sha256,"physical_problem_sha256":physical_problem_sha256});
        Ok(format!("approximate-split-history-{}",implexity_core::json::sha256_hex(implexity_core::json::canonical(&value).as_bytes())))
    }
    pub fn identity(&self)->Value {
        if self.stress_relaxing_condition {return json!({"schema":"implexity-prescribed-stress-relaxing-aged-condition/1","method":"differentiated_prescribed_frozen_total_strain_temperature_norton_relaxation_and_native_species_map","snapshot_times_s":self.snapshot_times_s,"age_s":self.snapshot_times_s[1],"global_equilibrium_solves":2,"aged_observation_states":1,"local_newton_solves":0,"startup_history_solves":0,"driving":"same_design_unaged_full_load_total_strain_and_temperature","norton":"scalar_monotone_regularized_deviatoric_relaxation","species":"single_native_backward_euler_closed_map","heat":"native_endpoint_stress_dissipation_over_age_once","provenance":self.provenance,"truth_status":"approximate_prescribed_aged_condition_map","canonical_lambda_one_full_guards":false,"exact_cache_admission":false,"physical_qualification":false});}
        if self.aged_condition {return json!({"schema":"implexity-frozen-reference-aged-condition/1","method":"differentiated_unaged_reference_norton_and_native_species_backward_euler_condition","snapshot_times_s":self.snapshot_times_s,"age_s":self.snapshot_times_s[1],"global_equilibrium_solves":2,"aged_observation_states":1,"local_newton_solves":0,"startup_history_solves":0,"driving":"same_design_unaged_full_load_temperature_and_native_stress","norton":"constant_reference_stress_temperature_native_increment","species":"single_native_backward_euler_closed_map","heat":"native_reference_stress_dissipation_over_age_once","provenance":self.provenance,"truth_status":"approximate_declared_aged_condition_map","canonical_lambda_one_full_guards":false,"exact_cache_admission":false,"physical_qualification":false});}
        json!({"schema":"implexity-operator-split-history/1","method":"local_implicit_frozen_global_driving","snapshot_times_s":self.snapshot_times_s,"local_substeps":self.local_substeps,"coupling_sweeps":self.coupling_sweeps,"provenance":self.provenance,"truth_status":"approximate_declared_split_map","canonical_lambda_one_full_guards":false,"exact_cache_admission":false,"initialization_only_auxiliary_history":false,"physical_qualification":false})
    }
}

