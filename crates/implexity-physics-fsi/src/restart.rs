// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use crate::{model::FsiModel, problem::normalise};
use implexity_core::{CaeError, CaeResult};
use implexity_solve::time_stepper::{StepParameters, TimeStepper};
use serde_json::{Value, json};
use std::{io::Write, path::{Path, PathBuf}, time::Instant};
fn fail(e: impl std::fmt::Display) -> CaeError { CaeError::contract(e.to_string()) }
fn integer(v: &Value) -> CaeResult<usize> { v.as_u64().and_then(|v|usize::try_from(v).ok()).ok_or_else(||fail("restart index must be an integer")) }
fn number(v: &Value) -> CaeResult<f64> { v.as_f64().filter(|v|v.is_finite()).ok_or_else(||fail("restart clock must be finite")) }
fn bound_bytes(base: &Path, entry: &Value) -> CaeResult<Vec<u8>> {
    let file=entry["path"].as_str().ok_or_else(||fail("restart file path missing"))?;
    let p=PathBuf::from(file);let p=if p.is_absolute(){p}else{base.join(p)};
    let bytes=std::fs::read(p).map_err(fail)?;
    if entry["sha256"].as_str()!=Some(implexity_core::json::sha256_hex(&bytes).as_str()) {return Err(fail("restart file digest differs"));}
    Ok(bytes)
}
fn bound_json(base: &Path, entry: &Value) -> CaeResult<Value> { serde_json::from_slice(&bound_bytes(base,entry)?).map_err(fail) }
fn save(path: &Path, value: &Value) -> CaeResult<()> {
    let temporary=path.with_extension("partial");let mut file=std::fs::File::create(&temporary).map_err(fail)?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(fail)?).map_err(fail)?;file.sync_all().map_err(fail)?;
    std::fs::rename(temporary,path).map_err(fail)
}
fn state_file(path: &Path, state: &[f64]) -> CaeResult<()> {
    let temporary=path.with_extension("partial");let mut file=std::io::BufWriter::new(std::fs::File::create(&temporary).map_err(fail)?);
    for value in state {file.write_all(&value.to_le_bytes()).map_err(fail)?;}
    file.flush().map_err(fail)?;file.get_ref().sync_all().map_err(fail)?;std::fs::rename(temporary,path).map_err(fail)
}
pub fn continue_from(manifest_path: &Path, manifest_sha256: &str, output: &Path) -> CaeResult<Value> {
    let raw=std::fs::read(manifest_path).map_err(fail)?;
    if implexity_core::json::sha256_hex(&raw)!=manifest_sha256 {return Err(fail("restart manifest digest differs"));}
    let manifest:Value=serde_json::from_slice(&raw).map_err(fail)?;
    if manifest["schema"]!="implexity-fsi-restart/1" || manifest["accepted_state"]!=true {return Err(fail("restart manifest contract differs"));}
    let base=manifest_path.parent().ok_or_else(||fail("restart manifest parent missing"))?;
    let problem=bound_json(base,&manifest["problem"])?;
    let density_document=bound_json(base,&manifest["physical_density"])?;
    let density:Vec<f64>=serde_json::from_value(density_document["density"].clone()).map_err(fail)?;
    if density.iter().any(|v|!v.is_finite()){return Err(fail("restart density is nonfinite"));}
    let saved_layout=bound_json(base,&manifest["layout"])?;
    let callback=bound_json(base,&manifest["callback"])?;
    let accepted=integer(&manifest["accepted_step"])?;let end=integer(&manifest["requested_end_step"])?;
    let dt=number(&manifest["macro_step_s"])?;let time=number(&manifest["time_s"])?;
    if accepted==0 || dt<=0. || end<=accepted {return Err(fail("restart step range differs"));}
    let row=callback.get("progress").unwrap_or(&callback);
    if integer(&row["step"])?!=accepted || number(&row["time_s"])?!=time || (time-accepted as f64*dt).abs()>2e-15*time.abs().max(1.) {return Err(fail("restart accepted callback clock differs"));}
    let model=FsiModel::new(normalise(&problem)?)?;
    if model.problem.time.macro_step_s()!=dt || end>model.problem.time.history_steps() {return Err(fail("restart physical time grid differs"));}
    let expected_density=model.chain.forward(&model.problem.design.initial_density)?;
    if expected_density!=density {return Err(fail("restart physical density differs from saved problem"));}
    let stepper=model.stepper()?;let layout=stepper.layout();let entries=layout.lags.end;
    if saved_layout["identity"]!=stepper.identity() || integer(&saved_layout["state_entries"])?!=entries
        || saved_layout["field_b"]!=json!([layout.field_b.start,layout.field_b.end])
        || saved_layout["field_a"]!=json!([layout.field_a.start,layout.field_a.end])
        || saved_layout["lags"]!=json!([layout.lags.start,layout.lags.end]) {return Err(fail("restart current model identity or layout differs"));}
    let state_bytes=bound_bytes(base,&manifest["state"])?;
    if state_bytes.len()!=entries.checked_mul(8).ok_or_else(||fail("restart state length overflow"))? {return Err(fail("restart full state length differs"));}
    if let Some(sha)=callback.get("state_sha256").and_then(Value::as_str) {if manifest["state"]["sha256"]!=sha{return Err(fail("restart snapshot state differs"));}}
    let mut state:Vec<f64>=state_bytes.chunks_exact(8).map(|b|f64::from_le_bytes(b.try_into().expect("eight-byte chunk"))).collect();
    if state.iter().any(|v|!v.is_finite()){return Err(fail("restart full state is nonfinite"));}
    drop(state_bytes);
    let schur_admission=if matches!(model.problem.coupling.mode, implexity_solve::multirate_coupling::CouplingMode::Loose { .. }) {
        crate::run::schur_preflight(&model,&stepper,&density)?
    } else { Value::Null };
    std::fs::create_dir(output).map_err(fail)?;std::fs::create_dir(output.join("states")).map_err(fail)?;
    save(&output.join("restart_binding.json"),&json!({"manifest_sha256":manifest_sha256,"accepted_before":accepted,"state_sha256":manifest["state"]["sha256"],"identity":stepper.identity(),"prior_provenance":manifest["prior_provenance"],"history_reverified":false,"state_scope":"complete solid memory, fluid populations and lag traces","new_runtime":true,"schur_preflight":schur_admission}))?;
    save(&output.join("problem.json"),&problem)?;save(&output.join("physical_density.json"),&density_document)?;save(&output.join("state_layout.json"),&saved_layout)?;
    let parameters=StepParameters{design:&density,time_scale:1.};let started=Instant::now();let mut ledger=Vec::new();let mut snapshots=Vec::new();
    for n in accepted+1..=end {
        let record=stepper.advance(n,&state,parameters)?;
        if record.state.len()!=entries || record.state.iter().any(|v|!v.is_finite()) {return Err(fail("accepted continued state is invalid"));}
        let filename=format!("states/step_{n:04}.f64le");state_file(&output.join(&filename),&record.state)?;
        let bytes_sha=implexity_core::json::sha256_hex(&record.state.iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>());
        let row=json!({"step":n,"time_s":n as f64*dt,"wall_s":started.elapsed().as_secs_f64(),"samples":record.samples,"ledger":record.ledger,"residual":record.diagnostics.residual_norm,"newton_iterations":record.diagnostics.newton_iterations});
        save(&output.join("progress.json"),&row)?;let mut journal=std::fs::OpenOptions::new().create(true).append(true).open(output.join("steps.jsonl")).map_err(fail)?;
        writeln!(journal,"{row}").map_err(fail)?;journal.sync_all().map_err(fail)?;
        snapshots.push(json!({"step":n,"time_s":n as f64*dt,"state_file":filename,"state_sha256":bytes_sha,"state_bytes":entries*8}));save(&output.join("states/manifest.json"),&json!(snapshots))?;
        state_file(&output.join("latest_state.f64le"),&record.state)?;ledger.push(record.ledger);state=record.state;
        println!("accepted step {n}: t={} s",n as f64*dt);
    }
    let result=json!({"status":"requested forward continuation completed","completed_before":accepted,"last_step":end,"completed_steps":end-accepted,"requested_end_step":end,"requested_window_completed":true,"sample_names":stepper.sample_names(),"ledger":crate::run::ledger_summary(&ledger),"physical_qualification":false,"study_completed":false,"identity":stepper.identity()});
    save(&output.join("result.json"),&result)?;Ok(result)
}
