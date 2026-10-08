// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::*;
use super::super::split_history::SplitHistoryOptions;
use super::super::split_snapshot::{NativeSplitTrajectory,solve_native_split_history_with_context};
use implexity_physics_solid::history::local_advance::LocalAdvanceOptions;

pub(super) struct ApproximateRecord {
    key:String,
    design:String,
    pub trajectory:NativeSplitTrajectory,
    responses:BTreeMap<String,f64>,
}

impl NativeUnifiedHistoryProvider {
    pub(super) fn split_selected(&self,parts:&Parts)->bool {
        parts.p.get("operator_split_history").is_some_and(|v|!v.is_null()) && (parts.k.s.model.creep.is_some() || parts.k.s.model.history.is_some() || parts.k.s.model.plastic.is_some() || parts.k.s.model.viscoelastic.is_some())
    }
    fn split_options(&self,parts:&Parts)->CaeResult<(SplitHistoryOptions,LocalAdvanceOptions)> {
        let card=&parts.p["operator_split_history"];
        let stress_relaxing_condition=card["schema"]=="implexity-prescribed-stress-relaxing-aged-condition/1";
        let aged_condition=stress_relaxing_condition || card["schema"]=="implexity-frozen-reference-aged-condition/1";
        let options=SplitHistoryOptions{aged_condition,stress_relaxing_condition,snapshot_times_s:parts.k.s.times.clone(),local_substeps:card["local_substeps"].as_u64().unwrap_or(if aged_condition {1} else {0}) as usize,coupling_sweeps:card["coupling_sweeps"].as_u64().unwrap_or(if aged_condition {2} else {0}) as usize,provenance:card["provenance"].as_str().unwrap_or("").to_string()};
        options.validate()?;
        let o=parts.k.reduction.system().options();
        Ok((options,LocalAdvanceOptions{tolerance:o.tolerance,maximum_iterations:o.max_iterations,condition_limit:o.condition_limit,..Default::default()}))
    }
    fn split_key(&self,parts:&Parts,design:&NamedArrays)->CaeResult<String> {
        let (options,local)=self.split_options(parts)?;
        let token=implexity_core::registries::global().addins.binding_token();
        Ok(implexity_core::json::canonical_sha256(&json!({"schema":"implexity-native-operator-split-provider/1","design":design_identity(design)?,"problem":parts.p,"binding":[token.generation,token.fingerprint],"approximation":options.identity(),"local_numerics":{"tolerance":local.tolerance,"maximum_iterations":local.maximum_iterations,"condition_limit":local.condition_limit}})))
    }
    pub(super) fn split_record(&self,parts:&Parts,design:&NamedArrays,solve:bool)->CaeResult<Option<Arc<ApproximateRecord>>> {
        let key=self.split_key(parts,design)?;
        if let Some(record)=self.approximate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|r|r.key==key).cloned() {return Ok(Some(record));}
        if !solve {return Ok(None);}
        let (options,local)=self.split_options(parts)?;
        let operation=parts.k.new_operation_context("declared_operator_split_history",true)?;
        let committed=self.approximate_committed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let reference_record=if options.stress_relaxing_condition {
            let cache=self.approximate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let compatible=|r:&&Arc<ApproximateRecord>|Arc::ptr_eq(&r.trajectory.kernel,&parts.k)
                && r.trajectory.kernel.state_size==parts.k.state_size
                && r.trajectory.kernel.nt==parts.k.nt
                && r.trajectory.identity["approximation"]==options.identity()
                && r.trajectory.sweeps.first().and_then(|s|s.first()).is_some_and(|s|s.solved.converged && !s.solved.relaxed && s.solved.state.len()==parts.k.state_size && s.solved.state.iter().all(|v|v.is_finite()));
            cache.iter().filter(compatible).find(|r|committed.as_ref()==Some(&r.key))
                .or_else(||cache.iter().rev().filter(compatible).next()).cloned()
        }else{None};
        let reference_guess=reference_record.as_ref().and_then(|r|r.trajectory.sweeps.first()).and_then(|s|s.first()).map(|s|s.solved.state.as_slice());
        let mut trajectory=if options.stress_relaxing_condition {
            super::super::split_snapshot::solve_native_age_condition_with_reference_guess(Arc::clone(&parts.k),&parts.x,&options,Some(&operation),reference_guess)?
        }else{solve_native_split_history_with_context(Arc::clone(&parts.k),&parts.x,&options,&local,Some(&operation))?};
        if let Some(record)=&reference_record {trajectory.identity["previous_reference_numerical_hint"]=json!({"design_state_id":record.design,"source_history_cache_key":record.key,"usage":"unaged_reference_newton_guess_only","physical_initial_changed":false,"canonical_cache_admission":false});}
        let values=parts.k.response_values(&trajectory.states,&parts.x)?;
        let mut responses:BTreeMap<String,f64>=parts.k.response_names().into_iter().zip(values).collect();
        let heat=trajectory.integrated_heat_value()?;
        if responses.contains_key("unified_inelastic_heat_J") {responses.insert("unified_inelastic_heat_J".into(),heat);}
        let record=Arc::new(ApproximateRecord{key,design:design_identity(design)?,trajectory,responses});
        let committed=self.approximate_committed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let mut cache=self.approximate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|r|committed.as_ref()==Some(&r.key));cache.push(Arc::clone(&record));
        Ok(Some(record))
    }
    fn split_solution(record:&ApproximateRecord)->HistorySolution {
        HistorySolution{states:record.trajectory.states.clone(),residual_norms:record.trajectory.sweeps.iter().map(|s|s.last().unwrap().solved.residual_norm).collect(),newton_iterations:record.trajectory.sweeps.iter().map(|s|s.iter().map(|v|v.solved.iterations).sum()).collect(),execution_context:record.trajectory.sweeps.first().and_then(|s|s.first()).and_then(|s|s.map.operation.clone()),convergence_reports:None}
    }
    fn split_diagnostics(&self,parts:&Parts,design:&NamedArrays,record:&ApproximateRecord,sol:&HistorySolution)->CaeResult<Map<String,Value>> {
        let mut dg=parts.k.diagnostics(&parts.x,sol,&parts.control,&parts.spacing,&design_identity(design)?)?;
        let aged=parts.p["operator_split_history"]["schema"]=="implexity-frozen-reference-aged-condition/1";
        let stress_relaxing=parts.p["operator_split_history"]["schema"]=="implexity-prescribed-stress-relaxing-aged-condition/1";
        let method=if stress_relaxing {"declared_prescribed_stress_relaxing_aged_condition"}else if aged {"declared_frozen_reference_aged_condition"}else{"declared_operator_split"};
        let numerical_screens=dg.get("regime_valid")==Some(&json!(true));
        if !numerical_screens {return Err(CaeError::convergence("declared split history failed finite-state, pressure or mass-conservation screens"));}
        if parts.p["applicability_policy"].as_str()!=Some("report_only") && dg.get("applicability_screens_passed")!=Some(&json!(true)) {return Err(CaeError::convergence("declared split history failed authored applicability screens"));}
        let endpoint=dg.remove("coupling_history").unwrap_or(Value::Null);
        let mut ledger=endpoint.as_array().cloned().ok_or_else(||CaeError::convergence("missing native split field diagnostic rows"))?;
        if ledger.len()!=record.trajectory.sweeps.len() {return contract("split field ledger interval count mismatch");}
        for (i,(row,sweeps)) in ledger.iter_mut().zip(&record.trajectory.sweeps).enumerate() {
            let step=i+1;let last=sweeps.last().ok_or_else(||CaeError::contract("split field ledger missing sweep"))?;
            let full=parts.k.expand(step,&record.trajectory.states[step])?;let (data,_)=parts.k.s.local_data(step);let mut heat=0.;
            for e in 0..parts.k.s.ne {
                let current=parts.k.s.element_local(step,e,&full[parts.k.solid_slice.clone()],&data);let x=parts.k.s.element_design(e,&parts.x);
                heat+=last.map.local.heat_density_J_m3[e]*parts.k.s.model.fields(&parts.k.s.mesh.gradients[e],&current,&x).volume;
            }
            let rate=heat/(parts.k.s.times[step]-parts.k.s.times[step-1]);
            let old=row["inelastic_heat_W"].as_f64().ok_or_else(||CaeError::contract("missing native endpoint inelastic heat rate"))?;
            for key in ["total_thermal_balance_W","fields_and_finite_reservoirs_thermal_balance_W","total_internal_energy_with_work_and_numerical_defect_balance_W"] {
                if let Some(value)=row[key].as_f64() {row[key]=json!(value+old-rate);}
            }
            row["endpoint_formula_inelastic_heat_W"]=json!(old);row["inelastic_heat_W"]=json!(rate);
            row["integrated_inelastic_heat_J"]=json!(heat);row["integrated_inelastic_heat_density_J_m3"]=json!(last.map.local.heat_density_J_m3);
            row["heat_method"]=json!(if stress_relaxing {"native_endpoint_stress_dissipation_over_age_replaces_endpoint_once"}else if aged {"native_reference_stress_age_dissipation_replaces_endpoint_once"}else{"native_local_substep_integral_replaces_endpoint_formula_once"});
            row["history_method"]=json!(method);row["approximate"]=json!(true);
            row["global_sweeps"]=json!(sweeps.len());row["declared_map_residual_norm"]=json!(last.solved.residual_norm);
            row["maximum_declared_map_condition"]=json!(sweeps.iter().map(|v|v.solved.condition_number).fold(0.,f64::max));
            row["physical_qualification"]=json!(false);row["monolithic_constitutive_work_balance_certified"]=json!(false);
        }
        dg.insert("endpoint_formula_diagnostics_not_split_energy_ledger".into(),endpoint);
        dg.insert("history_method".into(),record.trajectory.identity.clone());
        dg.insert("residual_scope".into(),json!(if stress_relaxing {"declared_prescribed_frozen_total_strain_temperature_stress_relaxing_aged_equilibrium_map"}else if aged {"declared_frozen_reference_aged_equilibrium_map"}else{"declared_operator_split_snapshot_map"}));
        dg["coupled_assembly"]["coupling"]=json!(if stress_relaxing {"differentiated_unaged_reference_and_one_prescribed_stress_relaxing_aged_equilibrium"}else if aged {"differentiated_unaged_reference_and_one_prescribed_aged_equilibrium"}else{"declared_operator_split_native_global_fields_and_local_implicit_history"});
        dg["shared_state_reduction"]["coupling"]=json!(if stress_relaxing {"differentiated_unaged_reference_and_one_prescribed_stress_relaxing_aged_equilibrium"}else if aged {"differentiated_unaged_reference_and_one_prescribed_aged_equilibrium"}else{"declared_operator_split_native_global_fields_and_local_implicit_history"});
        dg.insert("provider_truth_status".into(),json!("declared_approximate_source_guarded_map"));
        dg.insert("canonical_lambda_one_full_guards".into(),json!(false));
        dg.insert("physical_qualification".into(),json!(false));
        dg.insert("history_cache_key".into(),json!(record.key));
        dg.insert("regime_valid".into(),json!(numerical_screens && record.trajectory.sweeps.iter().flatten().all(|s|s.solved.converged && !s.solved.relaxed && s.solved.residual_norm.is_finite())));
        dg.insert("regime_valid_interpretation".into(),json!("numerical_admission_of_declared_approximate_map_only"));
        dg.insert("coupling_history".into(),json!(ledger));
        dg.insert("forward_convergence".into(),json!({"history_method":method,"strict":true,"residual_norms":sol.residual_norms,"condition_guard_scope":"full_normalized_declared_snapshot_jacobian","original_monolithic_history_certification":false}));
        Ok(dg)
    }
    pub(super) fn split_evaluation(&self,parts:&Parts,design:&NamedArrays,include_history:bool)->CaeResult<Evaluation> {
        let kernel=Arc::clone(&parts.k);let _operation=kernel.exclusive();
        let record=self.split_record(parts,design,true)?.ok_or_else(||CaeError::contract("split history unavailable"))?;
        self.split_evaluation_record(parts,design,&record,include_history)
    }
    fn split_evaluation_record(&self,parts:&Parts,design:&NamedArrays,record:&ApproximateRecord,include_history:bool)->CaeResult<Evaluation> {
        let sol=Self::split_solution(record);let dg=self.split_diagnostics(parts,design,record,&sol)?;
        let mut evaluation=self.evaluation_from_solution(parts,design,&sol,include_history,dg,Some(record.responses.clone()))?;
        if matches!(parts.p["operator_split_history"]["schema"].as_str(),Some("implexity-frozen-reference-aged-condition/1"|"implexity-prescribed-stress-relaxing-aged-condition/1")) {
            let reference=&record.trajectory.sweeps[0][0].solved;
            let name="unaged_reference_state_nondimensional";
            evaluation.fields.insert(name.into(),arr(reference.state.clone(),&[parts.k.state_size])?);
            let bytes:Vec<u8>=reference.state.iter().flat_map(|value|value.to_le_bytes()).collect();
            if let Some(Value::Object(fields))=evaluation.diagnostics.get_mut("field_metadata") {
                fields.insert(name.into(),json!({"units":"1","association":"native_reduced_state","rank":"vector","shape":[parts.k.state_size],"source":"same_design_unaged_full_load_reference_equilibrium","normalization":"same_native_unified_state_layout_as_aged_observation","time_s":parts.k.s.times[1],"design_state_id":record.design,"state_sha256":implexity_core::json::sha256_hex(&bytes),"residual_norm":reference.residual_norm,"full_condition":reference.condition_number,"physical_qualification":false}));
            } else {return contract("approximate reference field metadata unavailable");}
        }
        if let Some(Value::Object(fields))=evaluation.diagnostics.get_mut("field_metadata") {
            for metadata in fields.values_mut() {
                metadata["history_method"]=json!(if parts.p["operator_split_history"]["schema"]=="implexity-prescribed-stress-relaxing-aged-condition/1" {"declared_prescribed_stress_relaxing_aged_condition"}else if parts.p["operator_split_history"]["schema"]=="implexity-frozen-reference-aged-condition/1" {"declared_frozen_reference_aged_condition"}else{"declared_operator_split"});metadata["canonical_lambda_one"]=json!(false);
                metadata["provider_truth_status"]=json!("declared_approximate_source_guarded_map");
            }
        }
        Ok(evaluation)
    }
    pub(super) fn split_cached(&self,parts:&Parts,design:&NamedArrays)->CaeResult<CachedEvaluation> {
        match self.split_record(parts,design,false)? {Some(record)=>Ok(CachedEvaluation::Available(self.split_evaluation_record(parts,design,&record,false)?)),None=>Ok(CachedEvaluation::Unavailable(json!({"available":false,"reason":"declared_approximate_cache_missing"}).as_object().cloned().unwrap_or_default()))}
    }
    pub(super) fn split_sensitivities(&self,parts:&Parts,design:&NamedArrays,names:&[String])->CaeResult<DesignSensitivities> {
        let record=self.split_record(parts,design,true)?.ok_or_else(||CaeError::contract("split history unavailable"))?;
        let (_,gu,gx)=parts.k.response_partials(&record.trajectory.states,&parts.x,names)?;
        let sol=Self::split_solution(&record);
        let mut out=DesignSensitivities{diagnostics:self.split_diagnostics(parts,design,&record,&sol)?,..DesignSensitivities::default()};
        let mut state_seeds=Vec::with_capacity(names.len());let mut direct_design=Vec::with_capacity(names.len());let mut heat_seeds=Vec::with_capacity(names.len());
        for (j,name) in names.iter().enumerate() {
            if name=="unified_inelastic_heat_J" {
                let (_,direct,heat)=record.trajectory.integrated_heat_partials()?;
                state_seeds.push(vec![vec![0.;parts.k.state_size];record.trajectory.states.len()]);direct_design.push(direct);heat_seeds.push(heat);
            } else {
                state_seeds.push(gu.iter().map(|m|(0..m.nrows).map(|i|m.data[i*m.ncols+j]).collect()).collect());
                direct_design.push((0..gx.nrows).map(|i|gx.data[i*gx.ncols+j]).collect());heat_seeds.push(vec![vec![0.;parts.k.s.ne];record.trajectory.sweeps.len()]);
            }
        }
        let gradients=record.trajectory.pullback_block(&state_seeds,&direct_design,&heat_seeds)?;
        for (name,gradient) in names.iter().zip(gradients) {out.responses.insert(name.clone(),record.responses[name]);out.gradients.insert(name.clone(),self.split(&gradient,parts)?);}
        out.diagnostics.insert("history_derivative".into(),json!("exact_reverse_of_declared_approximate_local_implicit_snapshot_map"));
        Ok(out)
    }
    pub(super) fn split_commit(&self,parts:&Parts,design:&NamedArrays)->CaeResult<Map<String,Value>> {
        let record=self.split_record(parts,design,false)?.ok_or_else(||CaeError::contract("cannot commit an unsolved approximate design"))?;
        *self.approximate_committed.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(record.key.clone());
        Ok(json!({"design_state_id":record.design,"history_cache_key":record.key,"history_method":record.trajectory.identity,"physical_qualification":false}).as_object().cloned().unwrap_or_default())
    }
    pub(super) fn split_export(&self,parts:&Parts,design:&NamedArrays,require_committed:bool)->CaeResult<MatchingTimeNewtonGuess> {
        let record=self.split_record(parts,design,false)?.ok_or_else(||CaeError::contract("approximate history export requires the same solved design"))?;
        if require_committed && self.approximate_committed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()!=Some(&record.key) {return contract("approximate history export requires its committed design");}
        let mut identity=parts.k.matching_time_guess_identity().as_object().cloned().unwrap_or_default();
        identity.insert("operator_split_history".into(),parts.p["operator_split_history"].clone());
        let mut provenance=json!({"design_state_id":record.design,"history_cache_key":record.key,"provider_truth_status":"declared_approximate_source_guarded_map","canonical_lambda_one":false,"auxiliary":false,"history_method":record.trajectory.identity,"observation_times_s":parts.k.s.times,"integrated_heat_density_J_m3_by_interval":record.trajectory.sweeps.iter().map(|s|s.last().unwrap().map.local.heat_density_J_m3.clone()).collect::<Vec<_>>(),"local_history_layout":{"per_tetrahedron_rows":parts.k.s.internal_size,"normalization_scales":parts.k.s.model.scales,"owner":"native_solid_element_layout","histories_are_in_exported_snapshot_states":true},"physical_qualification":false}).as_object().cloned().unwrap_or_default();
        if matches!(parts.p["operator_split_history"]["schema"].as_str(),Some("implexity-frozen-reference-aged-condition/1"|"implexity-prescribed-stress-relaxing-aged-condition/1")) {
            let reference=&record.trajectory.sweeps[0][0].solved;
            let bytes:Vec<u8>=reference.state.iter().flat_map(|value|value.to_le_bytes()).collect();
            provenance.insert("unaged_reference_state_field".into(),json!("unaged_reference_state_nondimensional"));
            provenance.insert("unaged_reference_state_sha256".into(),json!(implexity_core::json::sha256_hex(&bytes)));
            provenance.insert("unaged_reference_state_layout".into(),json!({"shape":[parts.k.state_size],"dtype":"<f8","normalization":"same_native_unified_state_layout_as_aged_observation","physical_age_s":parts.k.s.times[1],"ownership":"same_design_unaged_full_load_reference_equilibrium_not_initial_cold_state_or_previous_design_seed"}));
            provenance.insert("unaged_reference_residual_norm".into(),json!(reference.residual_norm));
            provenance.insert("unaged_reference_full_condition".into(),json!(reference.condition_number));
        }
        MatchingTimeNewtonGuess::new(record.trajectory.states.clone(),identity,provenance)
    }
    pub(super) fn split_install(&self,parts:&Parts,design:&NamedArrays,guess:&MatchingTimeNewtonGuess)->CaeResult<Map<String,Value>> {
        let mut expected=parts.k.matching_time_guess_identity().as_object().cloned().unwrap_or_default();expected.insert("operator_split_history".into(),parts.p["operator_split_history"].clone());
        if &expected!=guess.provider_identity() || guess.states().len()!=parts.k.nt || guess.states().iter().any(|s|s.len()!=parts.k.state_size || s.iter().any(|v|!v.is_finite())) {return contract("operator-split warm payload identity or state shape mismatch");}
        Ok(json!({"design_state_id":design_identity(design)?,"installed_as":"validated_matching_time_export_only","used_as_driving_history":false,"canonical_cache_admission":false,"reason":"the declared approximate map recomputes its driving fields; exports are not admitted to the exact cache"}).as_object().cloned().unwrap_or_default())
    }
}
