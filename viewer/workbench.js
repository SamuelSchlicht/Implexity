// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(()=>{"use strict";
const present=(id,metadata={})=>window.ImplexityText?.present?.(id,metadata)||({id:String(id??""),label:String(metadata.label||metadata.display_name||metadata.title||id||""),unit:String(metadata.unit||metadata.units||""),description:String(metadata.description||metadata.help||metadata.scope||""),explicit:Boolean(metadata.label||metadata.display_name||metadata.title)});
const human=(id,metadata={})=>present(id,metadata).label;
window.IMPLEXITY_GUI_CENTRIC=true;
window.IMPLEXITY_UNIFIED_GENERATIONS=true;

const STEPS=[
  {id:"geometry",label:"Geometry",hint:"Drag the implicit surface, edit parameters, or reshape the topology field with brushes and cages."},
  {id:"conditions",label:"Conditions",hint:"Create geometry-following regions, loads and boundary conditions directly in the viewport."},
  {id:"physics",label:"Physics",hint:"Choose physics and configure the analysis."},
  {id:"objectives",label:"Objectives",hint:"Compose differentiable objectives and constraints from provider responses."},
  {id:"optimize",label:"Optimise",hint:"Check the setup and run direct-gradient optimisation."},
  {id:"results",label:"Results",hint:"Inspect fields, histories and optimisation gradients."}
];
const STEP_BY_ID=Object.fromEntries(STEPS.map(s=>[s.id,s]));
const MIN_USABLE_STAGE_WIDTH=440;
const SHORT_STAGE_HEIGHT=680;
const COMPACT_STAGE_HEIGHT=440;
const COMPUTATION_EFFORT_SCHEMA="implexity-computation-effort-request/1";
const COMPUTATION_EFFORT_LIMITS=Object.freeze({wallMinutes:Object.freeze({min:1,max:43200,step:1}),memoryGiB:Object.freeze({min:.25,max:256,step:.25}),correctionCadence:Object.freeze({min:1,max:20,step:1})});
const COMPUTATION_EFFORT_PREVIEW_REASON="Approximate initialization and proposals have no authority; every coupling is restored and the exact solver must finish before commit.";
const clamp=(lo,value,hi)=>Math.max(lo,Math.min(value,hi));
const estimatedWideStageWidth=width=>Math.max(0,Number(width||0)-clamp(240,Number(width||0)*.22,300)-clamp(280,Number(width||0)*.24,340));
const compactPanesForWidth=width=>estimatedWideStageWidth(width)<MIN_USABLE_STAGE_WIDTH;
const stageHeightModeForHeight=height=>Number(height||0)<COMPACT_STAGE_HEIGHT?"compact":Number(height||0)<SHORT_STAGE_HEIGHT?"short":"regular";
const q=(s,r=document)=>r&&r.querySelector?s? r.querySelector(s):null:null;
const qa=(s,r=document)=>r&&r.querySelectorAll?Array.from(r.querySelectorAll(s)):[];
const node=(tag,cls,text)=>{const n=document.createElement(tag);if(cls)n.className=cls;if(text!=null)n.textContent=text;return n};
const renderMessage=(host,summary,technical,severity="error")=>{
  const presenter=window.ImplexityUIPresentation;
  if(presenter?.render)return presenter.render(host,{summary,technical:technical?.implexityTechnical||technical,severity});
  host.replaceChildren(node("span","implexity-ui-summary",summary));host.setAttribute("role",severity==="error"?"alert":"status");return summary;
};
const publicMessage=(value,fallback)=>window.ImplexityUIPresentation?.publicText?.(value,fallback)||fallback;
const NUMERICAL_SOLVER_RECORD_SCHEMA="implexity-numerical-solver-record/1";
const NUMERICAL_SOLVER_RECORD_SET_SCHEMA="implexity-numerical-solver-record-set/1";
function numericalSolverRecords(row){
  const records=[],diagnostics=row?.diagnostics||{};
  const append=value=>{if(value?.schema===NUMERICAL_SOLVER_RECORD_SET_SCHEMA&&Array.isArray(value.records))value.records.slice(0,64).forEach(record=>{if(record?.schema===NUMERICAL_SOLVER_RECORD_SCHEMA)records.push(record)})};
  append(diagnostics.numerical_solver_records);
  append(diagnostics.source_diagnostics?.numerical_solver_records);
  Object.values(diagnostics.providers||{}).slice(0,32).forEach(provider=>append(provider?.numerical_solver_records));
  Object.values(diagnostics.source_diagnostics?.providers||{}).slice(0,32).forEach(provider=>append(provider?.numerical_solver_records));
  return records;
}
function numericalSolverHealth(row){
  const records=numericalSolverRecords(row);if(!records.length)return null;
  const finite=value=>value!==null&&value!==undefined&&Number.isFinite(Number(value));
  const failed=records.filter(record=>record?.certification?.passed!==true);
  const finals=records.filter(record=>finite(record.final_relative_residual)).map(record=>Number(record.final_relative_residual));
  return {records,allCertified:failed.length===0,failedCount:failed.length,
    largestFinalResidual:finals.length?Math.max(...finals):null,
    retries:records.reduce((total,record)=>total+(finite(record.retry_count)?Number(record.retry_count):0),0)};
}
const store={
  get(k,d){try{const v=localStorage.getItem(k);return v==null?d:JSON.parse(v)}catch(_){return d}},
  set(k,v){try{localStorage.setItem(k,JSON.stringify(v))}catch(_){}}
};


function finiteEffortValue(value,label,{min,max,step}){
  if(value===""||value==null||typeof value==="boolean")throw new RangeError(`${label} is required.`);
  const number=Number(value);
  if(!Number.isFinite(number)||number<min||number>max)throw new RangeError(`${label} must be between ${min} and ${max}.`);
  const steps=(number-min)/step;
  if(Math.abs(steps-Math.round(steps))>1e-9)throw new RangeError(`${label} must use increments of ${step}.`);
  return number;
}
function cleanCouplingIds(value,{required=false}={}){
  const source=typeof value==="string"?value.split(/\r?\n/):value;
  if(!Array.isArray(source))throw new RangeError("Coupling IDs must be entered one per line.");
  const ids=source.map(item=>{
    if(typeof item!=="string")throw new RangeError("Each coupling ID must be text.");
    return item.trim();
  }).filter(Boolean);
  if(ids.length>32)throw new RangeError("Choose at most 32 coupling IDs.");
  if(ids.some(id=>id.length>160))throw new RangeError("Each coupling ID must contain at most 160 characters.");
  if(new Set(ids).size!==ids.length)throw new RangeError("Each coupling ID may be listed only once.");
  if(required&&!ids.length)throw new RangeError("Enter at least one provider-approved coupling ID.");
  return ids;
}
function couplingRequest(preset,mode,selected,rawIds){
  const approximate=mode!=="exact";
  const fallback=mode==="verified_preview"?"staged":mode==="interactive_preview"?"interactive":"exact";
  const choice=selected==null||selected===""?fallback:String(selected);
  if(!["exact","staged","interactive","explicit"].includes(choice))throw new RangeError("Choose an available initialization coupling policy.");
  const ids=cleanCouplingIds(rawIds||[],{required:choice==="explicit"});
  if(!approximate&&(choice!=="exact"||ids.length))throw new RangeError("Exact computation keeps every coupling active.");
  if(choice!=="explicit"&&ids.length)throw new RangeError("Choose Explicit coupling IDs before entering IDs.");
  return {preset:choice,lagged_coupling_ids:choice==="explicit"?ids:[]};
}
function buildComputationEffortRequest({preset="exact",wallTimeMinutes=180,memoryGiB=18,trustRadius=.15,correctionCadence=5,couplingPreset=null,laggedCouplingIds=[]}={}){
  if(["exact","staged","interactive","fast"].includes(preset)){
    if(couplingPreset==null)return {preset};
    const mode=preset==="exact"?"exact":preset==="staged"?"verified_preview":"interactive_preview";
    return {preset,coupling_approximation:couplingRequest(preset,mode,couplingPreset,laggedCouplingIds)};
  }
  if(!["exact_standard","exact_bounded","verified_preview","interactive_preview"].includes(preset))throw new RangeError("Choose an available computation mode.");
  const preview=preset==="verified_preview"||preset==="interactive_preview";
  const trust=preview?finiteEffortValue(trustRadius,"Preview trust radius",{min:.01,max:1,step:.01}):0;
  const cadence=preview?finiteEffortValue(correctionCadence,"Preview updates per exact correction",COMPUTATION_EFFORT_LIMITS.correctionCadence):1;
  let wall=null,memory=null;
  if(preset==="exact_bounded"||preview){
    const minutes=finiteEffortValue(wallTimeMinutes,"Wall-time limit",COMPUTATION_EFFORT_LIMITS.wallMinutes);
    const gib=finiteEffortValue(memoryGiB,"Memory limit",COMPUTATION_EFFORT_LIMITS.memoryGiB);
    wall=minutes*60;memory=gib*1024**3;
    if(!Number.isSafeInteger(memory))throw new RangeError("Memory limit cannot be represented as an exact byte count.");
  }
  return {
    schema:COMPUTATION_EFFORT_SCHEMA,
    mode:preview?preset:"exact",
    hard_budgets:{wall_time_s:wall,memory_bytes:memory},
    target_update_rate_hz:preview?(preset==="verified_preview"?5:20):null,
    error_limits:{response:preview?(preset==="verified_preview"?.05:.2):0,state:0,gradient:0},
    trust_radius:trust,
    exact_correction:{cadence_updates:cadence,deadline_s:preview?wall:0},
    ood_policy:preview?"require_exact":"refuse",
    coupling_approximation:couplingRequest(preset,preview?preset:"exact",couplingPreset,laggedCouplingIds)
  };
}
function effortBudget(policy){return policy?.hard_budgets||policy?.budgets||{wall_time_s:null,memory_bytes:null}}
function effortDuration(seconds){
  const value=Number(seconds);if(!Number.isFinite(value))return "N/A";
  if(value<120)return `${value.toLocaleString(undefined,{maximumFractionDigits:1})} s`;
  if(value<7200)return `${(value/60).toLocaleString(undefined,{maximumFractionDigits:1})} min`;
  return `${(value/3600).toLocaleString(undefined,{maximumFractionDigits:2})} h`;
}
function effortBytes(bytes){
  const value=Number(bytes);if(!Number.isFinite(value)||value<0)return "N/A";
  return `${(value/1024**3).toLocaleString(undefined,{maximumFractionDigits:2})} GiB`;
}
function effortMode(mode){return ({exact:"Exact",verified_preview:"Verified preview",interactive_preview:"Interactive preview"})[mode]||"Unknown"}
function normalizedCouplingPolicy(policy){
  const direct=policy?.coupling_approximation;
  if(direct&&typeof direct==="object"&&typeof direct.preset==="string")return {preset:direct.preset,lagged_coupling_ids:Array.isArray(direct.lagged_coupling_ids)?direct.lagged_coupling_ids:[]};
  if(policy?.preset==="staged")return {preset:"staged",lagged_coupling_ids:[]};
  if(policy?.preset==="interactive"||policy?.preset==="fast")return {preset:"interactive",lagged_coupling_ids:[]};
  if(policy?.mode==="verified_preview")return {preset:"staged",lagged_coupling_ids:[]};
  if(policy?.mode==="interactive_preview")return {preset:"interactive",lagged_coupling_ids:[]};
  return {preset:"exact",lagged_coupling_ids:[]};
}
function displayCouplingIds(value){
  if(!Array.isArray(value))return [];
  const ids=value.filter(item=>typeof item==="string"&&item.trim()&&item.trim().length<=160).map(item=>item.trim());
  return [...new Set(ids)].slice(0,32);
}
function couplingCatalogFromCapabilities(source){
  const candidate=source?.traits?.coupling_control||source?.coupling_control||source;
  if(!candidate||typeof candidate!=="object")return {available:false,providerId:String(source?.id||source?.name||""),scope:"unavailable",couplings:[],presets:{}};
  const providerId=String(candidate.provider_id||source?.id||source?.name||"");
  const rows=Array.isArray(candidate.couplings)?candidate.couplings:[];
  const couplings=[];const seen=new Set();
  for(const raw of rows){
    if(!raw||typeof raw!=="object")continue;
    const id=String(raw.id||"").trim();if(!id||seen.has(id))continue;seen.add(id);
    const allowed=displayCouplingIds(raw.allowed_states).filter(state=>["active","lagged","disabled"].includes(state));
    const fallback=String(raw.current_state||raw.default_state||"active");
    const currentState=allowed.includes(fallback)?fallback:(allowed[0]||"active");
    couplings.push({
      id,label:String(raw.label||id),description:String(raw.description||"Declared by the active provider."),
      defaultState:String(raw.default_state||currentState),currentState,allowedStates:allowed.length?allowed:[currentState],
      costTier:String(raw.cost_tier||"unknown"),costNote:String(raw.cost_note||"No cost estimate was published."),
      truthByState:raw.truth_status_by_state&&typeof raw.truth_status_by_state==="object"?{...raw.truth_status_by_state}:{},
      restorationRequired:raw.restoration_required_before_commit===true,
      configuration:raw.configuration&&typeof raw.configuration==="object"?{...raw.configuration}:null
    });
  }
  return {available:candidate.available!==false&&couplings.length>0,providerId,scope:String(candidate.selection_scope||"unavailable"),couplings,presets:candidate.presets&&typeof candidate.presets==="object"?{...candidate.presets}:{},reason:String(candidate.reason||"")};
}
function couplingStateLabel(state){return ({active:"Active",lagged:"Paused for initialization",disabled:"Excluded by provider"})[state]||"Unavailable"}
function couplingPolicyLabel(policy){
  const coupling=normalizedCouplingPolicy(policy),ids=displayCouplingIds(coupling.lagged_coupling_ids);
  if(coupling.preset==="exact")return "All couplings active";
  if(coupling.preset==="staged")return "Staged · provider-approved couplings paused for initialization";
  if(coupling.preset==="interactive")return "Interactive · provider minimum-work couplings paused for initialization";
  if(coupling.preset==="explicit")return ids.length?`Explicit · ${ids.length} selected coupling${ids.length===1?"":"s"}`:"Explicit · awaiting coupling IDs";
  return "Unknown coupling policy";
}
function effortPolicy(policy){
  if(!policy||typeof policy!=="object")return "Awaiting service selection";
  if(policy.preset==="exact")return "Exact · standard";
  if(policy.preset==="staged")return "Staged · approximate initializer, then exact correction";
  if(policy.preset==="interactive"||policy.preset==="fast")return "Interactive · approximate initializer and proposals, then exact correction";
  const mode=effortMode(policy.mode),budget=effortBudget(policy),wall=budget.wall_time_s,memory=budget.memory_bytes;
  const cadence=Number(policy?.exact_correction?.cadence_updates);
  const correction=(policy.mode!=="exact"&&Number.isInteger(cadence))?` · exact every ${cadence} update${cadence===1?"":"s"}`:"";
  if(wall==null&&memory==null)return `${mode} · standard${correction}`;
  const bounds=[];if(budget.wall_time_mode==="unlimited")bounds.push("No elapsed-time limit");else if(wall!=null)bounds.push(`≤ ${effortDuration(wall)}`);if(memory!=null)bounds.push(`≤ ${effortBytes(memory)}`);
  return `${mode} · ${bounds.join(" / ")}${correction}`;
}
function effortLimits(limits){
  if(!limits||typeof limits!=="object")return "Awaiting managed execution";
  const bounds=[];
  if(limits.wall_time_mode==="unlimited")bounds.push("No elapsed-time limit");
  else if(limits.wall_time_s!=null)bounds.push(`≤ ${effortDuration(limits.wall_time_s)}`);
  if(limits.memory_bytes!=null)bounds.push(`≤ ${effortBytes(limits.memory_bytes)}`);
  return bounds.length?bounds.join(" / "):"No returned hard clamp";
}
function effortState(value){
  if(typeof value!=="string"||!value.trim())return "Awaiting managed execution";
  return value.replace(/[_-]+/g," ").replace(/^./,letter=>letter.toUpperCase());
}
function effortEvidence(source){
  const row=source&&typeof source==="object"?source:{};
  const summary=row.summary&&typeof row.summary==="object"?row.summary:{};
  const exact=row.exact_result&&typeof row.exact_result==="object"?row.exact_result:{};
  const previewRows=[...(Array.isArray(summary.previews)?summary.previews:[]),...(Array.isArray(row.previews)?row.previews:[])];
  const liveCouplingPreview=[...previewRows].reverse().find(item=>item&&typeof item==="object"&&item.schema==="implexity-optimization-preview-trace/1"&&(item.phase==="cold_staged_initializer"||Array.isArray(item.inactive_coupling_ids)||Array.isArray(item.provider_laggable_coupling_ids)))||null;
  const evidence=row.computation_evidence||summary.computation_evidence||exact.computation_evidence||null;
  const publicEffort=row.computation_effort||summary.computation_effort||exact.computation_effort||null;
  const selection=evidence?.selection||publicEffort?.selection||null;
  const supervised=row.supervisor&&typeof row.supervisor==="object"?row.supervisor:{};
  const observed=evidence?.observed?.observed||row.observed_computation_effort?.observed||supervised.observed||null;
  const truth=evidence?.truth?.classification||row.truth?.classification||null;
  const couplingInitialization=row.coupling_initialization||summary.coupling_initialization||exact.coupling_initialization||evidence?.coupling_initialization||liveCouplingPreview||row.preview_trace||summary.preview_trace||exact.preview_trace||null;
  const couplingCapability=row.coupling_approximation_capability||summary.coupling_approximation_capability||exact.coupling_approximation_capability||row.provider?.traits?.coupling_approximation||summary.provider?.traits?.coupling_approximation||row.capabilities?.traits?.coupling_approximation||row.physics_provider?.capabilities?.traits?.coupling_approximation||null;
  const couplingRestorationSchedule=row.coupling_restoration_schedule||summary.coupling_restoration_schedule||exact.coupling_restoration_schedule||couplingInitialization?.restoration_schedule||null;
  return {row,summary,exact,evidence,publicEffort,selection,observed,truth,supervised,couplingInitialization,couplingCapability,couplingRestorationSchedule};
}
function effortTerminalReason(row,summary,supervised){
  const values=[supervised?.reason,row?.status?.terminal_reason,row?.terminal_reason,summary?.terminal_reason,row?.error?.code,summary?.error?.code];
  return values.find(value=>typeof value==="string"&&value.trim())||null;
}
function effortTerminalLabel(code){
  const labels={memory_budget_exceeded:"Memory budget exceeded",wall_time_budget_exceeded:"Wall-time budget exceeded",wall_time_exceeded:"Wall-time budget exceeded",hard_budget_exceeded:"Hard computation budget exceeded",deadline_exceeded:"Wall-time budget exceeded",timed_out:"Wall-time limit reached"};
  return labels[code]||String(code||"").replace(/[_-]+/g," ").replace(/^./,letter=>letter.toUpperCase());
}
function summarizeComputationEffort(source={},requestedFallback=null){
  const {row,summary,exact,evidence,publicEffort,selection,observed,truth,supervised,couplingInitialization,couplingCapability,couplingRestorationSchedule}=effortEvidence(source);
  const supervisedSelection=supervised?.requested?.selection||null;
  const requested=selection?.requested||supervisedSelection?.requested||requestedFallback;
  const effective=selection?.effective||supervisedSelection?.effective||null;
  const truthCode=truth?.truth_status||null;
  const truthLabel=truthCode?({exact:"Exact evidence",verified_preview:"Verified preview evidence",interactive_preview:"Interactive preview evidence",refused:"Result refused"}[truthCode]||effortTerminalLabel(truthCode)):"Awaiting exact evidence";
  const terminalCode=effortTerminalReason(row,summary,supervised);
  const elapsed=observed?.wall_time_s??observed?.elapsed_s??observed?.consumed_wall_time_s??row.elapsed_s??summary.elapsed_s??null;
  let peak=observed?.peak_owned_memory_bytes??observed?.peak_memory_bytes??observed?.peak_owned_rss_bytes??summary.peak_memory_bytes??null;
  if(peak==null&&Number.isFinite(Number(summary.peak_rss_mb)))peak=Number(summary.peak_rss_mb)*1024**2;
  const rate=observed?.published_update_rate_hz??null;
  const consumed=supervised?.observed?.consumed_wall_time_s??supervised?.observed?.elapsed_s??null;
  const remaining=supervised?.observed?.remaining_wall_time_s??supervised?.observed?.remaining_s??null;
  const requestedCoupling=row.coupling_approximation||summary.coupling_approximation||exact?.coupling_approximation||publicEffort?.coupling_approximation||evidence?.coupling_approximation||supervised?.requested?.coupling_approximation||normalizedCouplingPolicy(requested);
  const lagged=displayCouplingIds(couplingInitialization?.lagged_coupling_ids);
  const initiallyLagged=displayCouplingIds(couplingInitialization?.initially_lagged_coupling_ids);
  const initializerIds=initiallyLagged.length?initiallyLagged:lagged;
  const inactiveRaw=Array.isArray(couplingInitialization?.inactive_coupling_ids)?displayCouplingIds(couplingInitialization.inactive_coupling_ids):null;
  const exactReturned=truthCode==="exact"||couplingInitialization?.result_truth_status==="exact"||couplingInitialization?.all_couplings_restored===true||couplingInitialization?.exact_correction_complete===true||couplingInitialization?.exact_correction_completed===true;
  const requestedExact=normalizedCouplingPolicy(requestedCoupling).preset==="exact";
  let inactiveCouplings,inactiveState="pending";
  if(exactReturned){inactiveCouplings=initializerIds.length?`None now: exact result restored all couplings (initializer paused: ${initializerIds.join("; ")})`:"None: all couplings active for the exact result";inactiveState="exact";}
  else if(inactiveRaw){inactiveCouplings=inactiveRaw.length?inactiveRaw.join("; "):"None: all couplings currently active";inactiveState=inactiveRaw.length?"inactive":"exact";}
  else if(couplingInitialization?.available===false){inactiveCouplings="None: approximate initializer unavailable; exact cold start is required";inactiveState="exact";}
  else if(lagged.length){inactiveCouplings=lagged.join("; ");inactiveState="inactive";}
  else if(requestedExact){inactiveCouplings="None: Exact keeps all couplings active";inactiveState="exact";}
  else inactiveCouplings="Awaiting provider report of the exact coupling IDs";
  const schedule=Array.isArray(couplingRestorationSchedule)?couplingRestorationSchedule:[];
  const restorationPlanned=schedule.some(stage=>stage&&typeof stage==="object"&&stage.stage==="exact_correction"&&Array.isArray(stage.lagged_coupling_ids)&&stage.lagged_coupling_ids.length===0);
  let couplingRestoration;
  if(exactReturned)couplingRestoration="Complete: every coupling was restored for exact correction and result";
  else if(requestedExact)couplingRestoration="Not needed: all couplings remain active";
  else if(couplingInitialization?.available===false)couplingRestoration="Approximation refused: continuing from an exact cold start only";
  else if(restorationPlanned)couplingRestoration="Scheduled: every coupling is restored for the unchanged exact solve before acceptance";
  else couplingRestoration="Required: no approximate state can be accepted until every coupling is restored and solved exactly";
  const approvedSource=couplingCapability?.explicit_laggable_coupling_ids||couplingInitialization?.provider_laggable_coupling_ids||(couplingInitialization?.provider&&lagged.length?lagged:[]);
  const approved=displayCouplingIds(approvedSource);
  return {
    requested:effortPolicy(requested),
    effective:effortPolicy(effective),
    selectionReason:String(selection?.selection_reason||supervisedSelection?.selection_reason||"Preflight has not selected an effective policy for this request."),
    supervisorState:effortState(supervised?.state),
    enforcedLimits:effortLimits(supervised?.effective),
    budgetProgress:consumed==null&&remaining==null?"N/A":`${consumed==null?"N/A":effortDuration(consumed)} consumed / ${remaining==null?"N/A":effortDuration(remaining)} remaining`,
    truthCode:truthCode||"pending",
    truthLabel,
    elapsed:elapsed==null?"N/A":effortDuration(elapsed),
    peakMemory:peak==null?"N/A":effortBytes(peak),
    updateRate:rate==null?"N/A":`${Number(rate).toLocaleString(undefined,{maximumFractionDigits:2})} Hz`,
    couplingPolicy:couplingPolicyLabel(requestedCoupling),
    inactiveCouplings,
    inactiveCouplingState:inactiveState,
    couplingRestoration,
    laggableCouplings:approved.length?approved.join("; "):"Awaiting provider capability",
    terminalCode,
    terminalLabel:terminalCode?effortTerminalLabel(terminalCode):"",
    terminalKind:terminalCode&&/(?:budget|memory|wall.?time|deadline|timed_out)/i.test(terminalCode)?"budget":"terminal"
  };
}
let activeEffortBinding=null;
function bindComputationEffortControls({root=document,invalidate=()=>{},applyProviderCoupling=async()=>{throw new Error("Provider-owned coupling editing is unavailable.")}}={}){
  const byId=id=>root.getElementById?.(id)||q(`#${id}`,root);
  const presetExact=byId("effortPresetExact"),presetStaged=byId("effortPresetStaged"),presetInteractive=byId("effortPresetInteractive");
  const advancedEnabled=byId("effortAdvancedEnabled");
  const standard=byId("effortExactStandard"),bounded=byId("effortExactBounded");
  const wall=byId("effortWallMinutes"),memory=byId("effortMemoryGiB");
  const trust=byId("effortTrustRadius"),cadence=byId("effortCorrectionCadence");
  const couplingPreset=byId("effortCouplingPreset"),couplingIds=byId("effortLaggedCouplingIds");
  const advanced=byId("effortAdvanced"),validation=byId("effortValidation");
  if(!presetExact||!presetStaged||!presetInteractive||!advancedEnabled||!standard||!bounded||!wall||!memory||!cadence||!couplingPreset||!couplingIds)return null;
  const verified=byId("effortVerifiedPreview"),interactive=byId("effortInteractivePreview");
  const couplingList=byId("effortCouplingCatalogList"),couplingSummary=byId("effortCouplingCatalogSummary");
  let couplingCatalog=couplingCatalogFromCapabilities(null);
  const chosen=()=>bounded.checked?"exact_bounded":standard.checked?"exact_standard":verified?.checked?"verified_preview":interactive?.checked?"interactive_preview":"";
  const simplePreset=()=>presetInteractive.checked?"interactive":presetStaged.checked?"staged":"exact";
  const selectAdvancedDefault=preset=>{
    [standard,bounded,verified,interactive].filter(Boolean).forEach(control=>{control.checked=false});
    (preset==="interactive"||preset==="fast"?interactive:preset==="staged"?verified:standard).checked=true;
    couplingPreset.value=preset==="interactive"||preset==="fast"?"interactive":preset==="staged"?"staged":"exact";
  };
  const requestedLaggedIds=()=>{
    const preset=advancedEnabled.checked?couplingPreset.value:simplePreset();
    if(preset==="exact")return [];
    if(preset==="explicit")return displayCouplingIds(cleanCouplingIds(couplingIds.value||[]));
    return displayCouplingIds(couplingCatalog.presets?.[preset]?.lagged_coupling_ids);
  };
  const renderCouplingCatalog=()=>{
    if(!couplingList||!couplingSummary)return;
    couplingList.replaceChildren();
    if(!couplingCatalog.available){
      couplingSummary.textContent="Not advertised by this provider";
      couplingList.append(node("p","implexity-coupling-empty","This physics add-in has not published a selectable coupling graph. Exact execution remains available."));
      return;
    }
    const paused=new Set(requestedLaggedIds()),counts={active:0,lagged:0,disabled:0};
    for(const coupling of couplingCatalog.couplings){
      const providerConfigured=coupling.configuration?.kind==="engineering_problem"&&coupling.configuration?.action==="set_engineering_problem";
      const providerEditable=providerConfigured&&coupling.allowedStates.includes("active")&&coupling.allowedStates.includes("disabled");
      const requestEditable=coupling.configuration?.kind==="computation_effort_request"&&coupling.configuration?.field==="coupling_approximation.lagged_coupling_ids"&&coupling.allowedStates.includes("active")&&coupling.allowedStates.includes("lagged");
      const fixedDisabled=coupling.currentState==="disabled"&&!providerEditable;
      const state=providerEditable?coupling.currentState:fixedDisabled?"disabled":paused.has(coupling.id)&&coupling.allowedStates.includes("lagged")?"lagged":"active";counts[state]+=1;
      const row=node("article",`implexity-coupling-row ${state}`);row.dataset.couplingState=state;row.setAttribute("role","listitem");
      const copy=node("div","implexity-coupling-copy");
      const title=node("div","implexity-coupling-title");title.append(node("strong","",coupling.label),node("code","",coupling.id));
      const detail=node("p","",coupling.description);
      const facts=node("div","implexity-coupling-facts");facts.append(node("span","",`${human(coupling.costTier)} cost`),node("span","",coupling.costNote));
      if(coupling.restorationRequired)facts.append(node("span","restore","Restored before exact commit"));
      copy.append(title,detail,facts);
      const field=node("label","implexity-coupling-state");field.append(node("span","",providerEditable?"Run state":"Initialization state"));
      const select=node("select");select.setAttribute("aria-label",`${coupling.label} initialization state`);
      for(const value of coupling.allowedStates){
        const option=node("option","",couplingStateLabel(value));option.value=value;
        if(value==="disabled"&&state!=="disabled"&&!providerEditable)option.disabled=true;
        select.append(option);
      }
      select.value=state;const editable=(requestEditable||providerEditable)&&!fixedDisabled;
      select.disabled=!editable;select.setAttribute("aria-disabled",String(!editable));
      select.addEventListener("change",async()=>{
        if(!editable){renderCouplingCatalog();return;}
        if(providerEditable){
          if(!coupling.allowedStates.includes(select.value)){renderCouplingCatalog();return;}
          select.disabled=true;couplingSummary.textContent=`Applying ${coupling.label}…`;
          try{await applyProviderCoupling(coupling,select.value);}
          catch(error){couplingSummary.textContent=String(error?.message||error);renderCouplingCatalog();}
          return;
        }
        if(!["active","lagged"].includes(select.value)){renderCouplingCatalog();return;}
        const ids=new Set(requestedLaggedIds());if(select.value==="lagged")ids.add(coupling.id);else ids.delete(coupling.id);
        advancedEnabled.checked=true;
        if(!verified?.checked&&!interactive?.checked){[standard,bounded,verified,interactive].filter(Boolean).forEach(control=>{control.checked=false});(simplePreset()==="interactive"?interactive:verified).checked=true;}
        couplingPreset.value="explicit";couplingIds.value=[...ids].join("\n");changed();
      });
      field.append(select,node("small","",state==="disabled"?"This provider profile omits the coupling.":String(coupling.truthByState[state]||"").replace(/[_-]+/g," ")));
      row.append(copy,field);couplingList.append(row);
    }
    const scope=couplingCatalog.scope.replace(/[_-]+/g," ");
    couplingSummary.textContent=`${counts.active} active · ${counts.lagged} paused · ${counts.disabled} excluded · ${scope}`;
  };
  const setCouplingCatalog=source=>{couplingCatalog=couplingCatalogFromCapabilities(source);renderCouplingCatalog();return couplingCatalog};
  const setFieldError=(field,message)=>{field.setCustomValidity?.(message);field.setAttribute?.("aria-invalid",String(Boolean(message)))};
  const request=({report=false}={})=>{
    setFieldError(wall,"");setFieldError(memory,"");setFieldError(cadence,"");setFieldError(couplingIds,"");if(validation)validation.textContent="";
    try{
      if(!advancedEnabled.checked)return buildComputationEffortRequest({preset:simplePreset()});
      if(bounded.checked||chosen()==="verified_preview"||chosen()==="interactive_preview"){
        if(wall.checkValidity&&!wall.checkValidity())throw new RangeError("Wall-time limit must use the displayed finite range and increment.");
        if(memory.checkValidity&&!memory.checkValidity())throw new RangeError("Memory limit must use the displayed finite range and increment.");
      }
      if((chosen()==="verified_preview"||chosen()==="interactive_preview")&&cadence.checkValidity&&!cadence.checkValidity())throw new RangeError("Preview updates per exact correction must be a whole number from 1 to 20.");
      return buildComputationEffortRequest({preset:chosen(),wallTimeMinutes:wall.value,memoryGiB:memory.value,trustRadius:trust?.value??.15,correctionCadence:cadence.value,couplingPreset:couplingPreset.value,laggedCouplingIds:couplingPreset.value==="explicit"?couplingIds.value:[]});
    }catch(error){
      const message=String(error?.message||error);if(validation)validation.textContent=message;
      const target=/coupling/i.test(message)?couplingIds:/trust/i.test(message)?trust:/correction|updates/i.test(message)?cadence:/memory/i.test(message)?memory:wall;setFieldError(target,message);
      if(report){target.reportValidity?.();target.focus?.();}
      throw error;
    }
  };
  const render=(source={},options={})=>{
    let fallback=options.requested||null;if(!fallback){try{fallback=request()}catch(_){fallback=null}}
    const view=summarizeComputationEffort(source,fallback),card=byId("effortTruthCard");
    const write=(selector,value)=>{const target=q(selector,card||root);if(target)target.textContent=value};
    write("[data-effort-requested]",view.requested);write("[data-effort-effective]",view.effective);
    write("[data-effort-selection-reason]",view.selectionReason);write("[data-effort-truth-status]",view.truthLabel);
    write("[data-effort-supervisor-state]",view.supervisorState);write("[data-effort-enforced-limits]",view.enforcedLimits);write("[data-effort-budget-progress]",view.budgetProgress);
    write("[data-effort-elapsed]",view.elapsed);write("[data-effort-peak-memory]",view.peakMemory);write("[data-effort-update-rate]",view.updateRate);
    write("[data-effort-coupling-policy]",view.couplingPolicy);write("[data-effort-inactive-couplings]",view.inactiveCouplings);write("[data-effort-coupling-restoration]",view.couplingRestoration);write("[data-effort-laggable-couplings]",view.laggableCouplings);
    const terminal=q(".implexity-effort-terminal",card||root);if(terminal){terminal.hidden=!view.terminalCode;terminal.dataset.kind=view.terminalKind;}
    write("[data-effort-terminal-label]",view.terminalLabel);write("[data-effort-terminal-code]",view.terminalCode||"");
    if(card){card.dataset.truthStatus=view.truthCode;card.dataset.terminalReason=view.terminalCode||"";card.dataset.couplingState=view.inactiveCouplingState;}
    return view;
  };
  const sync=()=>{
    const custom=advancedEnabled.checked,isBounded=bounded.checked,isPreview=chosen()==="verified_preview"||chosen()==="interactive_preview",hasBudget=custom&&(isBounded||isPreview);
    [standard,bounded,verified,interactive].filter(Boolean).forEach(control=>{control.disabled=!custom;control.setAttribute?.("aria-disabled",String(!custom))});
    wall.disabled=!hasBudget;memory.disabled=!hasBudget;trust&&(trust.disabled=!(custom&&isPreview));cadence.disabled=!(custom&&isPreview);
    couplingPreset.disabled=!(custom&&isPreview);couplingIds.disabled=!(custom&&isPreview&&couplingPreset.value==="explicit");
    wall.setAttribute?.("aria-disabled",String(!hasBudget));memory.setAttribute?.("aria-disabled",String(!hasBudget));trust?.setAttribute?.("aria-disabled",String(!(custom&&isPreview)));cadence.setAttribute?.("aria-disabled",String(!(custom&&isPreview)));couplingPreset.setAttribute?.("aria-disabled",String(!(custom&&isPreview)));couplingIds.setAttribute?.("aria-disabled",String(couplingIds.disabled));
    if(custom&&advanced)advanced.open=true;
    renderCouplingCatalog();
  };
  const changed=()=>{
    sync();let requested=null;try{requested=request()}catch(_){}
    render({}, {requested});
    invalidate("The computation effort or accuracy limits changed.");
    window.dispatchEvent(new CustomEvent("implexity:computation-effort-changed",{detail:{valid:Boolean(requested),request:requested}}));
  };
  const simpleChanged=event=>{
    const control=event?.target;
    [presetExact,presetStaged,presetInteractive].forEach(item=>{item.checked=item===control});
    if(![presetExact,presetStaged,presetInteractive].includes(control))presetExact.checked=true;
    advancedEnabled.checked=false;
    selectAdvancedDefault(simplePreset());
    changed();
  };
  const advancedModeChanged=event=>{
    if(event?.target===verified)couplingPreset.value="staged";
    else if(event?.target===interactive)couplingPreset.value="interactive";
    else couplingPreset.value="exact";
    changed();
  };
  presetExact.addEventListener("change",simpleChanged);presetStaged.addEventListener("change",simpleChanged);presetInteractive.addEventListener("change",simpleChanged);
  advancedEnabled.addEventListener("change",changed);
  [standard,bounded,verified,interactive].filter(Boolean).forEach(control=>control.addEventListener("change",advancedModeChanged));
  couplingPreset.addEventListener("change",changed);
  [wall,memory,trust,cadence,couplingIds].filter(Boolean).forEach(control=>control.addEventListener("input",changed));
  sync();render({}, {requested:request()});
  activeEffortBinding=Object.freeze({request,render,sync,setCouplingCatalog});return activeEffortBinding;
}
const ImplexityComputationEffort=Object.freeze({
  schema:COMPUTATION_EFFORT_SCHEMA,
  limits:COMPUTATION_EFFORT_LIMITS,
  presets:Object.freeze(["exact","staged","interactive"]),
  compatibilityPresets:Object.freeze({fast:"interactive"}),
  qualifiedModes:Object.freeze(["exact","verified_preview","interactive_preview"]),
  previewAvailability:Object.freeze({enabled:true,reason:COMPUTATION_EFFORT_PREVIEW_REASON}),
  buildRequest:buildComputationEffortRequest,
  catalogFromCapabilities:couplingCatalogFromCapabilities,
  summarize:summarizeComputationEffort,
  bind:bindComputationEffortControls,
  request(options){return activeEffortBinding?activeEffortBinding.request(options):buildComputationEffortRequest()},
  render(source,options){return activeEffortBinding?activeEffortBinding.render(source,options):summarizeComputationEffort(source,options?.requested||null)},
  setCouplingCatalog(source){return activeEffortBinding?activeEffortBinding.setCouplingCatalog(source):couplingCatalogFromCapabilities(source)}
});
window.ImplexityComputationEffort=ImplexityComputationEffort;
const announceAuthoritativeChange=(reason,detail={})=>{
  if(typeof window.implexityAnnounceAuthoritativeDesignStateChange==="function")
    return window.implexityAnnounceAuthoritativeDesignStateChange(reason,detail);
  const value={...detail,reason:String(reason||"The authoritative design changed.")};
  window.dispatchEvent(new CustomEvent("implexity:design-state-changed",{detail:value}));
  return value;
};
async function json(url,opt={}){
  const init={credentials:"same-origin",headers:{"content-type":"application/json",...(opt.headers||{})},...opt};
  const r=await fetch(url,init);let d={};try{d=await r.json()}catch(_){d={}};
  if(!r.ok){const error=new Error("The service could not complete this request.");error.implexityTechnical={request:url,status:r.status,status_text:r.statusText,response:d};throw error;}
  return d;
}
function friendlyProvider(id,metadata={}){
  if(!id)return "No physics loaded";
  return human(id,metadata);
}
const NATIVE_FIELD_UNITS=[
  [/_W_m3$/i,"W/m³"],[/_W_m2$/i,"W/m²"],[/_kg_m3$/i,"kg/m³"],
  [/_m_s2$/i,"m/s²"],[/_m_s$/i,"m/s"],[/_Pa_s$/i,"Pa·s"],
  [/_Pa$/i,"Pa"],[/_m3_s$/i,"m³/s"],[/_kg_s$/i,"kg/s"],
  [/_mm$/i,"mm"],[/_m2$/i,"m²"],[/_m3$/i,"m³"],[/_K$/,"K"],[/_s$/i,"s"]
];
function nativeFieldPresentation(key){
  const raw=String(key??"");const declared=present(raw);
  let unit=declared.unit||"",stem=raw;
  if(!unit)for(const [pattern,value] of NATIVE_FIELD_UNITS)if(pattern.test(stem)){unit=value;stem=stem.replace(pattern,"");break;}
  let label=declared.label;
  if(!label||label===raw){
    const words=stem.replace(/([a-z0-9])([A-Z])/g,"$1 $2").replace(/[_./:-]+/g," ").trim();
    label=words?words.charAt(0).toUpperCase()+words.slice(1):"Value";
    label=label.replace(/\bBcs\b/g,"Boundary conditions").replace(/\bBc\b/g,"Boundary condition");
  }
  return {label,unit};
}
function nativeProblemFromSchema(schema){
  if(!schema||typeof schema!=="object")return null;
  if(Object.prototype.hasOwnProperty.call(schema,"default"))return structuredClone(schema.default);
  if(schema.type==="object"||schema.properties){
    const value={};
    for(const [key,child] of Object.entries(schema.properties||{})){
      const built=nativeProblemFromSchema(child);
      if(built!==null)value[key]=built;
    }
    return value;
  }
  if(schema.type==="array"&&Array.isArray(schema.default))return structuredClone(schema.default);
  return null;
}
 
 
 
function nativeOptionalFieldStarter(schema){
  if(!schema||typeof schema!=="object")return undefined;
  if(Object.prototype.hasOwnProperty.call(schema,"default"))return finiteJsonCopy(schema.default);
  if(Object.prototype.hasOwnProperty.call(schema,"const"))return finiteJsonCopy(schema.const);
  if(Array.isArray(schema.enum)&&schema.enum.length)return finiteJsonCopy(schema.enum[0]);
  if(schema.properties&&typeof schema.properties==="object"){
    const built=nativeProblemFromSchema(schema);
    if(built&&typeof built==="object"&&!Array.isArray(built)&&Object.keys(built).length)return built;
  }
  return undefined;
}
function nativeOptionalFields(schema,value){
  const properties=schema?.properties;
  if(!properties||typeof properties!=="object"||!value||typeof value!=="object"||Array.isArray(value))return {missing:[],removable:[]};
  const required=new Set(Array.isArray(schema.required)?schema.required:[]),missing=[],removable=[];
  for(const [key,child] of Object.entries(properties)){
    if(["__proto__","constructor","prototype"].includes(key))continue;
    const label=nativeSchemaDescriptor(key,child).label;
    if(!Object.prototype.hasOwnProperty.call(value,key)){
      const starter=nativeOptionalFieldStarter(child);
      if(starter!==undefined)missing.push({key,label,starter,description:typeof child?.description==="string"?child.description:""});
    }else if(!required.has(key)&&child&&Object.prototype.hasOwnProperty.call(child,"default"))removable.push({key,label});
  }
  return {missing,removable};
}
function nativeNumericArrayCount(value){
  if(!Array.isArray(value))return null;
  let count=0;const pending=[value];
  while(pending.length){const item=pending.pop();if(Array.isArray(item)){for(const child of item)pending.push(child);}else if(typeof item==="number"&&Number.isFinite(item))count++;else return null;}
  return count;
}
function nativeNumericArraySchemaError(value,schema){
  const pending=[[value,schema,"Array"]],finite=x=>typeof x==="number"&&Number.isFinite(x);
  while(pending.length){
    const [v,s,path]=pending.pop();if(!s||typeof s!=="object")continue;
    const types=Array.isArray(s.type)?s.type:[s.type];
    if(s.type!=null){
      const matches=types.some(type=>type==="array"?Array.isArray(v):type==="number"?finite(v):type==="integer"?Number.isSafeInteger(v):false);
      if(!matches)return `${path} must match the declared ${types.join(" or ")} type.`;
    }
    if(Array.isArray(v)){
      if(Number.isInteger(s.minItems)&&v.length<s.minItems)return `${path} needs at least ${s.minItems} entries.`;
      if(Number.isInteger(s.maxItems)&&v.length>s.maxItems)return `${path} permits at most ${s.maxItems} entries.`;
      v.forEach((item,i)=>pending.push([item,s.prefixItems?.[i]??s.items,`${path}[${i}]`]));
    }else{
      if(s.type==="integer"&&!Number.isSafeInteger(v))return `${path} must be an exact whole number.`;
      if(finite(s.minimum)&&v<s.minimum)return `${path} must be at least ${s.minimum}.`;
      if(finite(s.maximum)&&v>s.maximum)return `${path} must be at most ${s.maximum}.`;
      if(finite(s.exclusiveMinimum)&&v<=s.exclusiveMinimum)return `${path} must exceed ${s.exclusiveMinimum}.`;
      if(finite(s.exclusiveMaximum)&&v>=s.exclusiveMaximum)return `${path} must be below ${s.exclusiveMaximum}.`;
      if(finite(s.multipleOf)&&s.multipleOf>0){const q=v/s.multipleOf;if(!Number.isFinite(q)||Math.abs(q-Math.round(q))>8*Number.EPSILON*Math.max(1,Math.abs(q)))return `${path} must be a multiple of ${s.multipleOf}.`;}
    }
  }
  return "";
}
 
 
 
function nativeSchemaParts(schema){
  const parts=[],seen=new Set();
  const visit=node=>{
    if(!node||typeof node!=="object"||Array.isArray(node)||seen.has(node)||parts.length>=16)return;
    seen.add(node);parts.push(node);
    if(Array.isArray(node.allOf))node.allOf.forEach(visit);
    for(const key of ["anyOf","oneOf"]){
      const branches=Array.isArray(node[key])?node[key].filter(branch=>branch&&typeof branch==="object"&&branch.type!=="null"):[];
      if(branches.length===1)visit(branches[0]);
    }
  };
  visit(schema);return parts;
}
function nativeSchemaKeyword(schema,keyword){
  for(const part of nativeSchemaParts(schema))if(part[keyword]!==undefined)return part[keyword];
  return undefined;
}
function nativeSchemaAtPath(schema,path){
  let current=schema;
  for(const part of path){
    if(!current||typeof current!=="object")return null;
    const parts=nativeSchemaParts(current);
    current=typeof part==="number"
      ?(parts.map(item=>item.prefixItems?.[part]).find(Boolean)??parts.map(item=>item.items).find(item=>item&&typeof item==="object"))
      :parts.map(item=>item.properties?.[part]).find(Boolean);
  }
  return current&&typeof current==="object"?current:null;
}
function nativeSchemaDescriptor(key,schema){
  const fallback=typeof key==="number"?{label:`Item ${key+1}`,unit:""}:nativeFieldPresentation(key);
  const text=(value,maximum=4096)=>typeof value==="string"&&value.trim()&&value===value.trim()&&value.length<=maximum?value:"";
  const keyword=name=>nativeSchemaKeyword(schema,name);
  const title=text(keyword("title"),320)||fallback.label;
  const unit=text(keyword("unit")??keyword("unit_si")??keyword("units"),160)||fallback.unit;
  const description=text(keyword("description"));
  const finite=value=>typeof value==="number"&&Number.isFinite(value)?value:null;
  const minimum=finite(keyword("minimum")),maximum=finite(keyword("maximum"));
  const step=keyword("multipleOf");
  const multipleOf=typeof step==="number"&&Number.isFinite(step)&&step>0?step:null;
  let choices=null;const options=keyword("enum");
  if(Array.isArray(options)&&options.length&&options.length<=256){
    const scalar=value=>value===null||typeof value==="string"||typeof value==="boolean"||(typeof value==="number"&&Number.isFinite(value));
    const keys=options.map(value=>JSON.stringify(value));
    if(options.every(scalar)&&new Set(keys).size===keys.length)choices=structuredClone(options);
  }
  return {label:title,unit,description,minimum,maximum,exclusiveMinimum:finite(keyword("exclusiveMinimum")),exclusiveMaximum:finite(keyword("exclusiveMaximum")),multipleOf,choices,integer:keyword("type")==="integer"};
}
function nativeObjectVariants(schema,value){
  const declaration=schema?.["x-object-variants"];
  if(!declaration)return null;
  const key=declaration.discriminator,options=declaration.options;
  if(typeof key!=="string"||!key||["__proto__","constructor","prototype"].includes(key)||!Array.isArray(options)||!options.length||options.length>32)throw new Error("Invalid provider object variants.");
  const clean=finiteJsonCopy(options),identities=new Set();
  for(const option of clean){
    const identity=option?.template?.[key];
    if(typeof option?.label!=="string"||!option.label.trim()||option.label.length>320||typeof identity!=="string"||!identity||identities.has(identity))throw new Error("Invalid or duplicate provider object variant.");
    identities.add(identity);
  }
  return {key,options:clean,selected:clean.findIndex(option=>option.template[key]===value?.[key])};
}
function workspaceCapabilityManifest(response,provider){
  const manifest=response?.result||response;
  if(!manifest||manifest.schema!=="implexity-workspace-capabilities/1"||manifest.selection_required!==false||manifest.selected_provider!==provider)
    throw new Error("The selected provider returned an invalid workspace capability snapshot.");
  if(!/^[0-9a-f]{64}$/.test(String(manifest.content_sha256||"")))throw new Error("The workspace capability snapshot is not content-bound.");
  if(!Array.isArray(manifest.design_coordinates)||manifest.design_coordinates.some(value=>typeof value!=="string"||!value.trim())||new Set(manifest.design_coordinates).size!==manifest.design_coordinates.length)
    throw new Error("The provider capability snapshot contains invalid design coordinates.");
  if(!manifest.request_contract||typeof manifest.request_contract!=="object")throw new Error("The provider capability snapshot omitted its request contract.");
  return structuredClone(manifest);
}
function finiteJsonCopy(value,state={count:0,seen:new WeakSet()},depth=0){
  if(depth>64)throw new Error("The provider template exceeds the safe JSON nesting limit.");
  state.count+=1;if(state.count>100000)throw new Error("The provider template exceeds the safe JSON value limit.");
  if(value===null||typeof value==="boolean")return value;
  if(typeof value==="number"){
    if(!Number.isFinite(value))throw new Error("Provider templates may contain only finite numbers.");
    return value;
  }
  if(typeof value==="string"){
    if(value.length>1000000)throw new Error("A provider-template string exceeds the safe size limit.");
    return value;
  }
  if(typeof value!=="object")throw new Error("Provider templates must contain JSON values only.");
  if(state.seen.has(value))throw new Error("Provider templates may not contain cyclic values.");
  state.seen.add(value);
  let copy;
  if(Array.isArray(value)){
    if(value.length>100000)throw new Error("A provider-template array exceeds the safe size limit.");
    copy=value.map(item=>finiteJsonCopy(item,state,depth+1));
  }else{
    const prototype=Object.getPrototypeOf(value);
    if(prototype!==Object.prototype&&prototype!==null)throw new Error("Provider templates must use plain JSON objects.");
    copy={};
    for(const [key,item] of Object.entries(value)){
      if(key==="__proto__"||key==="prototype"||key==="constructor")throw new Error("A provider template contains an unsafe object key.");
      copy[key]=finiteJsonCopy(item,state,depth+1);
    }
  }
  state.seen.delete(value);return copy;
}
function authoredModelSnapshot(state,template){
  if(state?.loaded!==true||!state.document||typeof state.document!=="object"||Array.isArray(state.document))throw new Error("Load and save a model before capturing its snapshot.");
  const model=finiteJsonCopy(state.document),node=model.root;
  if(typeof node!=="string"||!node||!Object.hasOwn(model.nodes||{},node))throw new Error("The stored model has no valid root node.");
  const extra=finiteJsonCopy(template);
  if(!extra||Array.isArray(extra)||typeof extra!=="object"||Object.hasOwn(extra,"model")||Object.hasOwn(extra,"node"))throw new Error("Invalid model-snapshot starter declaration.");
  return {...extra,model,node};
}
function cartesianScalarData(result,metadata){
  if(metadata?.kind!=="cartesian_cell_scalars")return null;
  const shape=result.diagnostics?.shape,spacing=result.diagnostics?.spacing_m,fields=result.fields;
  const origin=result.diagnostics?.origin_m??[0,0,0];
  if(!Array.isArray(origin)||origin.length!==3||origin.some(v=>!Number.isFinite(v)))throw new Error("Invalid grid origin.");
  if(!Array.isArray(shape)||shape.length!==3||shape.some(n=>!Number.isInteger(n)||n<1||n>256)||shape.reduce((a,b)=>a*b,1)>262144||!Array.isArray(spacing)||spacing.length!==3||spacing.some(h=>!Number.isFinite(h)||h<=0))throw new Error("Invalid spatial grid metadata.");
  const volume=(value,predicate)=>Array.isArray(value)&&value.length===shape[0]&&value.every(row=>Array.isArray(row)&&row.length===shape[1]&&row.every(line=>Array.isArray(line)&&line.length===shape[2]&&line.every(predicate)));
  const mask=fields?.[metadata.mask];if(!volume(mask,x=>typeof x==="boolean"))throw new Error("Invalid fluid/solid mask.");
  if(!Array.isArray(metadata.fields)||!metadata.fields.length||metadata.fields.length>16)throw new Error("No bounded scalar field list was declared.");
  const scalars=metadata.fields.map(entry=>{
    const data=fields?.[entry.key];if(!volume(data,x=>typeof x==="number"&&Number.isFinite(x)))throw new Error("A scalar field has invalid shape or values.");
    let min=Infinity,max=-Infinity;for(let i=0;i<shape[0];i++)for(let j=0;j<shape[1];j++)for(let k=0;k<shape[2];k++)if(mask[i][j][k]){min=Math.min(min,data[i][j][k]);max=Math.max(max,data[i][j][k]);}
    if(!Number.isFinite(min)||!Number.isFinite(max))throw new Error("The spatial result contains no fluid cells.");
    return {...entry,data,min,max};
  });
  return {shape,spacing,origin,mask,scalars};
}
function evaluatedCaseExport(provider,request,result){
  return JSON.stringify({schema:"implexity-authored-evaluation-export/1",exported_at:new Date().toISOString(),
    provider_descriptor_snapshot:provider,request,result,
    scope:"Authored-case evaluation snapshot. Not a saved geometry document, optimization checkpoint, or engineering qualification."},
    (key,value)=>{if(typeof value==="number"&&!Number.isFinite(value))throw new Error("The evaluation contains a nonfinite number and cannot be exported faithfully.");return value;},2);
}
function downloadEvaluationSnapshot(text,providerId){
  const blob=new Blob([text],{type:"application/json"}),url=URL.createObjectURL(blob),link=document.createElement("a");
  link.href=url;link.download=`${String(providerId).replace(/[^a-z0-9_-]/gi,"_")}-evaluation-${new Date().toISOString().replace(/[:.]/g,"-")}.json`;
  document.body.append(link);
  try{link.click();}finally{link.remove();setTimeout(()=>URL.revokeObjectURL(url),30000);}
}
function responseHistoryData(result,metadata){
  if(metadata?.kind!=="scalar_time_histories")return null;
  const time=result.fields?.[metadata.time_field];
  if(!Array.isArray(time)||time.length<2||time.length>100001||time.some((v,i)=>!Number.isFinite(v)||(i&&v<=time[i-1])))throw new Error("History times must be finite and strictly increasing.");
  if(!Array.isArray(metadata.series)||!metadata.series.length||metadata.series.length>12)throw new Error("Invalid declared history series.");
  return {time,series:metadata.series.map(entry=>{
    const values=result.fields?.[entry.key];
    if(!Array.isArray(values)||values.length!==time.length||values.some(v=>!Number.isFinite(v)))throw new Error("History values do not match their time grid.");
    return {label:String(entry.label||entry.key),unit:String(entry.unit||""),values};
  })};
}
function renderResponseHistories(host,result,metadata){
  const data=responseHistoryData(result,metadata);if(!data)return;
  const namespace="http://www.w3.org/2000/svg";
  const svgNode=(tag,attrs,text)=>{const el=document.createElementNS(namespace,tag);for(const [key,value] of Object.entries(attrs))el.setAttribute(key,String(value));if(text!=null)el.textContent=text;return el;};
  for(const series of data.series){
    const figure=node("figure",""),caption=node("figcaption","",`${series.label}${series.unit?` (${series.unit})`:""}`);
    let low=Infinity,high=-Infinity;for(const v of series.values){low=Math.min(low,v);high=Math.max(high,v);}
    if(low===high){const pad=Math.max(Math.abs(low)*.05,1e-12);low-=pad;high+=pad;}
    const start=data.time[0],span=data.time.at(-1)-start;
    const x=t=>80+460*(t-start)/span,y=v=>180-150*(v-low)/(high-low);
    const svg=svgNode("svg",{viewBox:"0 0 580 230",role:"img","aria-label":`${series.label} versus time; ${data.time.length} computed samples`});
    svg.style.width="100%";svg.style.background="white";svg.style.color="#172e39";
    svg.append(svgNode("path",{d:"M80 30 V180 H540",fill:"none",stroke:"#637780"}));
    for(let i=0;i<=3;i++){
      const t=start+span*i/3,v=low+(high-low)*i/3;
      svg.append(svgNode("text",{x:x(t),y:202,"text-anchor":"middle","font-size":11,fill:"#172e39"},t.toPrecision(3)),
        svgNode("text",{x:72,y:y(v)+4,"text-anchor":"end","font-size":11,fill:"#172e39"},v.toPrecision(3)));
    }
    svg.append(svgNode("polyline",{points:series.values.map((v,i)=>`${x(data.time[i])},${y(v)}`).join(" "),fill:"none",stroke:"#147b83","stroke-width":1.7}),
      svgNode("text",{x:310,y:224,"text-anchor":"middle","font-size":12,fill:"#172e39"},"Time (s)"));
    figure.append(caption,svg);host.append(figure);
  }
}
function renderCartesianScalarPreview(host,result,metadata){
  host.replaceChildren();const grid=cartesianScalarData(result,metadata);if(!grid)return;
  const field=node("select",""),axis=node("select",""),slice=node("input",""),caption=node("p",""),canvas=node("canvas",""),legend=node("p","");
  field.setAttribute("aria-label","Result field");axis.setAttribute("aria-label","Slice normal");slice.type="range";slice.min="0";slice.step="1";slice.setAttribute("aria-label","Slice cell index");
  grid.scalars.forEach((v,i)=>{const option=node("option","",`${v.label} (${v.unit})`);option.value=String(i);field.append(option);});
  ["X","Y","Z"].forEach((v,i)=>{const option=node("option","",v);option.value=String(i);axis.append(option);});field.value="0";axis.value="0";
  canvas.style.width="100%";canvas.style.display="block";canvas.style.background="white";
  const draw=()=>{
    const a=Number(axis.value),other=[0,1,2].filter(v=>v!==a),u=other[0],v=other[1],index=Number(slice.value),scalar=grid.scalars[Number(field.value)];
    const width=grid.shape[u]*grid.spacing[u],height=grid.shape[v]*grid.spacing[v],scale=Math.max(480/Math.max(width,height),grid.shape[u]/width,grid.shape[v]/height);
    if(!Number.isFinite(width)||!Number.isFinite(height)||!Number.isFinite(scale)||Math.max(width*scale,height*scale)>4096)throw new Error("The grid aspect ratio exceeds the spatial preview limit.");
    canvas.width=Math.max(1,Math.round(width*scale));canvas.height=Math.max(1,Math.round(height*scale));
    const ctx=canvas.getContext("2d");if(!ctx)throw new Error("Canvas rendering is unavailable.");ctx.fillStyle="white";ctx.fillRect(0,0,canvas.width,canvas.height);
    for(let i=0;i<grid.shape[u];i++)for(let j=0;j<grid.shape[v];j++){
      const pos=[0,0,0];pos[a]=index;pos[u]=i;pos[v]=j;
      if(!grid.mask[pos[0]][pos[1]][pos[2]])continue;
      const value=scalar.data[pos[0]][pos[1]][pos[2]],t=scalar.max===scalar.min ? .5 : (value-scalar.min)/(scalar.max-scalar.min);
      ctx.fillStyle=`hsl(${240-240*t},70%,45%)`;
      const x0=Math.round(i*canvas.width/grid.shape[u]),x1=Math.round((i+1)*canvas.width/grid.shape[u]);
      const y0=Math.round((grid.shape[v]-1-j)*canvas.height/grid.shape[v]),y1=Math.round((grid.shape[v]-j)*canvas.height/grid.shape[v]);ctx.fillRect(x0,y0,x1-x0,y1-y0);
    }
    const description=`${scalar.label}: ${"XYZ"[a]} = ${(grid.origin[a]+(index+.5)*grid.spacing[a]).toPrecision(5)} m; cell ${index+1}/${grid.shape[a]}. Right: +${"XYZ"[u]}, up: +${"XYZ"[v]}.`;
    caption.textContent=description;canvas.setAttribute("role","img");canvas.setAttribute("aria-label",description);
    legend.textContent=`Blue ${scalar.min.toPrecision(6)} → red ${scalar.max.toPrecision(6)} ${scalar.unit}. Fixed full-field range; white cells are solid. Cell values are not interpolated.`;
  };
  const reset=()=>{slice.max=String(grid.shape[Number(axis.value)]-1);slice.value=String(Math.floor(Number(slice.max)/2));draw();};
  field.onchange=draw;axis.onchange=reset;slice.oninput=draw;
  host.append(node("h4","","Computed field slices"),field,axis,slice,caption,canvas,legend);reset();
}
function workspaceTemplateProblemPatch(template){
  if(!template||typeof template!=="object"||Array.isArray(template))throw new Error("The selected study template is invalid.");
  const patch=finiteJsonCopy(template.problem_patch);
  if(!patch||typeof patch!=="object"||Array.isArray(patch))throw new Error("The selected study template did not declare an object problem patch.");
  return patch;
}
function mergeWorkspaceProblemPatch(problem,template){
  const patch=workspaceTemplateProblemPatch(template),base=finiteJsonCopy(problem&&typeof problem==="object"&&!Array.isArray(problem)?problem:{});
  if(template.problem_requirements!==undefined){
    if(!Array.isArray(template.problem_requirements))throw new Error("Invalid template problem requirements.");
    for(const requirement of template.problem_requirements){
      if(!Array.isArray(requirement?.path)||!requirement.path.length||requirement.path.some(key=>typeof key!=="string"))throw new Error("Invalid template requirement path.");
      let actual=base;for(const key of requirement.path)actual=actual&&Object.hasOwn(actual,key)?actual[key]:undefined;
      if(actual===undefined&&requirement.missing_equals_null===true)actual=null;
      if(JSON.stringify(actual)!==JSON.stringify(requirement.value))throw new Error(`The ${requirement.path.join(" / ")} setup changed. Refresh provider templates before staging this template.`);
    }
  }
  const merge=(current,overlay)=>{
    const object=value=>value&&typeof value==="object"&&!Array.isArray(value);
    if(!object(overlay))return finiteJsonCopy(overlay);
    const output=object(current)?finiteJsonCopy(current):{};
    for(const [key,value] of Object.entries(overlay))output[key]=object(value)?merge(output[key],value):finiteJsonCopy(value);
    return output;
  };
  return merge(base,patch);
}
function capabilityStateLabel(value){
  const raw=String(value||"provider_owned");
  return raw.replace(/[_-]+/g," ").replace(/^./,letter=>letter.toUpperCase());
}
function workspaceConstraintDeclaration(manifest){
  const declaration=manifest?.constraint_controls;
  if(!declaration||typeof declaration!=="object"||Array.isArray(declaration))return {available:false,reason:"No provider constraint-control declaration.",groups:[],statusGroups:[],assuranceLevels:[],controlCount:0,statusCount:0};
  const rows=value=>Array.isArray(value)?value.filter(row=>row&&typeof row==="object"&&!Array.isArray(row)):[];
  let groups=rows(declaration.groups).map((group,index)=>({
    ...group,
    id:String(group.id||`group_${index+1}`),
    label:String(group.label||human(group.id||`Group ${index+1}`)),
    controls:rows(group.controls),
  }));
  if(!groups.length&&rows(declaration.controls).length)groups=[{
    id:"provider_controls",label:String(declaration.label||"Provider controls"),
    description:String(declaration.description||""),controls:rows(declaration.controls),
  }];
  const statusGroups=rows(declaration.status_groups).map((group,index)=>({
    ...group,
    id:String(group.id||`status_group_${index+1}`),
    label:String(group.label||human(group.id||`Status group ${index+1}`)),
    outputs:rows(group.outputs),
  }));
  const assuranceLevels=rows(declaration.assurance_levels).map((level,index)=>({
    ...level,
    id:String(level.id||`assurance_${index+1}`),
    label:String(level.label||human(level.id||`Assurance ${index+1}`)),
    control_ids:Array.isArray(level.control_ids)?level.control_ids.filter(value=>typeof value==="string"&&value.trim()):[],
  }));
  return {
    available:declaration.available!==false,
    reason:String(declaration.reason||""),groups,statusGroups,assuranceLevels,
    controlCount:groups.reduce((count,group)=>count+group.controls.length,0),
    statusCount:statusGroups.reduce((count,group)=>count+group.outputs.length,0),
  };
}
function workspaceConstraintValue(value,unit=""){
  let text;
  if(value===null)text="Not set";
  else if(typeof value==="boolean")text=value?"Enabled":"Disabled";
  else if(typeof value==="number"&&Number.isFinite(value))text=String(value);
  else if(typeof value==="string")text=value;
  else if(Array.isArray(value))text=value.map(item=>workspaceConstraintValue(item)).join(" × ");
  else if(value&&typeof value==="object"){
    try{text=JSON.stringify(value);}catch(_){text="Provider-owned value";}
  }else text="Not reported";
  return `${text}${unit&&text!=="Not set"&&text!=="Not reported"?` ${unit}`:""}`;
}
function workspaceConstraintControlPresentation(row){
  const unit=String(row?.unit_si||row?.unit||row?.units||"");
  const hasCurrent=Object.prototype.hasOwnProperty.call(row||{},"current");
  const hasDefault=Object.prototype.hasOwnProperty.call(row||{},"default");
  const parts=[];
  if(row?.assurance_stage)parts.push(`Stage: ${capabilityStateLabel(row.assurance_stage)}`);
  if(row?.role)parts.push(`Role: ${capabilityStateLabel(row.role)}`);
  if(hasCurrent)parts.push(`Current: ${workspaceConstraintValue(row.current,unit)}`);
  else if(hasDefault)parts.push(`Default: ${workspaceConstraintValue(row.default,unit)}`);
  const minimum=typeof row?.minimum==="number"&&Number.isFinite(row.minimum)?row.minimum:null;
  const maximum=typeof row?.maximum==="number"&&Number.isFinite(row.maximum)?row.maximum:null;
  if(minimum!==null||maximum!==null)parts.push(`Range: ${minimum??"−∞"} to ${maximum??"∞"}${unit?` ${unit}`:""}`);
  if(Array.isArray(row?.choices)&&row.choices.length)parts.push(`Choices: ${row.choices.map(choice=>workspaceConstraintValue(choice)).join(", ")}`);
  if(row?.truth_status)parts.push(`Truth: ${capabilityStateLabel(row.truth_status)}`);
  if(row?.admission)parts.push(`Admission: ${capabilityStateLabel(typeof row.admission==="object"?(row.admission.status||row.admission.policy||"provider_owned"):row.admission)}`);
  if(row?.description)parts.push(String(row.description));
  return {id:String(row?.id||row?.path||""),label:String(row?.label||human(row?.id||row?.path||"Provider control")),state:parts.join(" · ")||String(row?.description||"Provider-owned constraint control.")};
}
function workspaceConstraintStatusPresentation(row){
  const unit=String(row?.unit_si||row?.unit||row?.units||"");
  const hasValue=Object.prototype.hasOwnProperty.call(row||{},"value");
  const parts=[];
  if(row?.assurance_stage)parts.push(`Stage: ${capabilityStateLabel(row.assurance_stage)}`);
  if(hasValue)parts.push(workspaceConstraintValue(row.value,unit));
  if(row?.status)parts.push(capabilityStateLabel(row.status));
  if(row?.truth_status)parts.push(`Truth: ${capabilityStateLabel(row.truth_status)}`);
  if(row?.admission)parts.push(`Admission: ${capabilityStateLabel(typeof row.admission==="object"?(row.admission.status||row.admission.policy||"provider_owned"):row.admission)}`);
  if(row?.description)parts.push(String(row.description));
  return {id:String(row?.id||row?.path||""),label:String(row?.label||human(row?.id||row?.path||"Provider status")),state:parts.join(" · ")||String(row?.description||"Provider-owned status output.")};
}
function workspaceConstraintAssurancePresentation(row,controls=[]){
  const controlById=new Map(controls.map(control=>[String(control?.id||""),control]));
  const parts=[];
  if(row?.kind)parts.push(`Stage: ${capabilityStateLabel(row.kind)}`);
  if(Object.prototype.hasOwnProperty.call(row||{},"active"))parts.push(`Current: ${row.active===true?"Active":"Inactive"}`);
  if(Object.prototype.hasOwnProperty.call(row||{},"guarantee"))parts.push(`Exact guarantee: ${row.guarantee===true?"Yes":"No"}`);
  if(row?.truth_status)parts.push(`Truth: ${capabilityStateLabel(row.truth_status)}`);
  const parameters=(Array.isArray(row?.control_ids)?row.control_ids:[]).map(id=>controlById.get(String(id))).filter(Boolean).map(control=>{
    const unit=String(control.unit_si||control.unit||control.units||"");
    const hasCurrent=Object.prototype.hasOwnProperty.call(control,"current"),hasDefault=Object.prototype.hasOwnProperty.call(control,"default");
    const value=hasCurrent?control.current:(hasDefault?control.default:undefined);
    return `${String(control.label||human(control.id||"Parameter"))}: ${workspaceConstraintValue(value,unit)}`;
  });
  if(parameters.length)parts.push(`Parameters: ${parameters.join("; ")}`);
  if(row?.description)parts.push(String(row.description));
  return {id:String(row?.id||""),label:String(row?.label||human(row?.id||"Provider assurance")),state:parts.join(" · ")||"Provider-owned constraint assurance declaration."};
}
function renderWorkspaceCapabilities(host,{state="ready",manifest=null,error=null,provider="",onApplyTemplate=null}={}){
  if(!host)return;host.replaceChildren();host.dataset.loadState=state;host.setAttribute("aria-live","polite");host.setAttribute("aria-busy",String(state==="loading"));
  if(state==="loading"){
    host.append(node("strong","","Loading provider capabilities…"),node("span","","Design, coupling, boundary, resolution and truth declarations come from the selected physics add-in."));return;
  }
  if(state==="error"||!manifest){
    host.dataset.loadState="error";renderMessage(host,"Provider capabilities are temporarily unavailable.",error||new Error(`No capability snapshot was returned for ${provider||"the selected provider"}.`));return;
  }
  const contract=manifest.request_contract||{},coordinates=manifest.design_coordinates||[];
  const coupling=manifest.coupling_control&&typeof manifest.coupling_control==="object"?manifest.coupling_control:{};
  const couplings=Array.isArray(coupling.couplings)?coupling.couplings:[];
  const boundary=manifest.boundary_control&&typeof manifest.boundary_control==="object"?manifest.boundary_control:{};
  const roles=Array.isArray(boundary.roles)?boundary.roles:[];
  const constraints=workspaceConstraintDeclaration(manifest);
  const constraintControls=constraints.groups.flatMap(group=>group.controls);
  const discretization=manifest.discretization_control&&typeof manifest.discretization_control==="object"?manifest.discretization_control:{};
  const templates=Array.isArray(manifest.study_templates)?manifest.study_templates:[];
  const resolutionRows=["analysis_mesh","design_basis","render_lod"].filter(key=>discretization[key]&&typeof discretization[key]==="object").map(key=>({id:key,...discretization[key]}));
  const heading=node("div","implexity-capability-heading"),headingCopy=node("div","");
  headingCopy.append(node("strong","","Provider-owned study capabilities"),node("span","",friendlyProvider(manifest.selected_provider,manifest.provider||{})));
  const truth=node("span","implexity-capability-truth",capabilityStateLabel(manifest.truth_status));truth.dataset.truthStatus=String(manifest.truth_status||"provider_owned");heading.append(headingCopy,truth);
  const grid=node("div","implexity-capability-grid");grid.setAttribute("role","list");
  const fact=(kind,label,value,detail)=>{
    const item=node("div","implexity-capability-fact");
    item.dataset.capability=kind;item.setAttribute("role","listitem");
    const raw=String(detail||"");
    const text=/^[a-z][a-z0-9_]*_[a-z0-9_]+$/.test(raw)?human(raw):raw;
    const explanation=node("small","",text);
    if(text!==raw)explanation.title=raw;
    item.append(node("span","",label),node("strong","",value),explanation);grid.append(item);
  };
  fact("design","Design space",!coordinates.length?"No design coordinates":contract.all_provider_design_coordinates_active_by_default===true?`All ${coordinates.length} active by default`:`${coordinates.length} declared`,!coordinates.length?"This provider declares no topology or geometry optimization inputs.":contract.explicit_scope_limitation_required_to_deactivate_coordinates===true?"Only an explicit user or MCP scope may deactivate one.":"Activation policy is provider-owned.");
  const activeCouplings=couplings.filter(row=>String(row?.current_state||row?.default_state||"active")==="active").length;
  fact("couplings","Couplings",coupling.available===false?"Not published":`${activeCouplings} of ${couplings.length} active`,coupling.available===false?String(coupling.reason||"No selectable coupling declaration."):"States and approximation authority are provider-owned.");
  fact("boundaries","Boundaries",boundary.available===false?"Not published":`${roles.length} role${roles.length===1?"":"s"}`,boundary.available===false?String(boundary.reason||"No boundary-control declaration."):"Roles, condition types and selectors are provider-owned.");
  fact("constraints","Constraint controls",constraints.available===false?"Not published":`${constraints.controlCount} control${constraints.controlCount===1?"":"s"}${constraints.statusCount?` · ${constraints.statusCount} status output${constraints.statusCount===1?"":"s"}`:""}`,constraints.available===false?constraints.reason:"Values, ranges, truth and admission semantics are provider-owned.");
  const activeAssurance=constraints.assuranceLevels.filter(level=>level.active===true).length,exactAssurance=constraints.assuranceLevels.filter(level=>level.guarantee===true).length;
  fact("assurance","Constraint assurance",constraints.assuranceLevels.length?`${activeAssurance} active · ${exactAssurance} exact final guarantee${exactAssurance===1?"":"s"}`:"Not published",constraints.assuranceLevels.length?"Search guidance, constrained targets and exact guarantees remain separate provider declarations.":"No provider assurance stages were published.");
  fact("resolution","Resolution",contract.analysis_design_and_render_resolution_are_distinct===true?"Analysis · design · render":"Provider-owned",discretization.available===false?"No editable discretization control was published.":resolutionRows.length?`${resolutionRows.length} distinct provider declarations.`:String(discretization.description||discretization.label||"Independent controls are published by this provider."));
  fact("templates","Study templates",`${templates.length} available`,templates.length?"Provider-owned starting points; values remain editable before use.":"No study template was published.");
  fact("truth","Truth",capabilityStateLabel(manifest.truth_status),"The add-in's declared truth status; the GUI does not reinterpret it.");
  const details=node("details","implexity-capability-details"),summary=node("summary","","Show provider declarations"),lists=node("div","implexity-capability-lists");
  const list=(label,rows,presentation)=>{const section=node("section",""),title=node("strong","",label),ul=node("ul","");if(rows.length)for(const row of rows){const item=node("li",""),shown=presentation(row);item.append(node("span","",shown.label),node("code","",shown.id));if(shown.state)item.append(node("small","",shown.state));ul.append(item);}else ul.append(node("li","implexity-capability-none","None published"));section.append(title,ul);lists.append(section);};
  list("Design coordinates",coordinates,value=>({id:String(value),label:human(value),state:"Active by default"}));
  list("Couplings",couplings,row=>({id:String(row?.id||""),label:String(row?.label||human(row?.id||"Unknown coupling")),state:capabilityStateLabel(row?.current_state||row?.default_state)}));
  list("Boundary roles",roles,row=>({id:String(row?.id||""),label:String(row?.label||human(row?.id||"Unknown boundary")),state:row?.required_by_default===true?"Required by default":"Available"}));
  for(const group of constraints.groups)list(`Constraint controls: ${group.label}`,group.controls,workspaceConstraintControlPresentation);
  for(const group of constraints.statusGroups)list(`Constraint status: ${group.label}`,group.outputs,workspaceConstraintStatusPresentation);
  list("Constraint assurance",constraints.assuranceLevels,row=>workspaceConstraintAssurancePresentation(row,constraintControls));
  list("Resolution controls",resolutionRows,row=>({id:String(row.id),label:String(row.label||human(row.id)),state:String(row.description||"Provider-owned resolution declaration.")}));
  list("Study templates",templates,row=>({id:String(row?.id||""),label:String(row?.label||human(row?.id||"Study template")),state:[String(row?.description||""),`Truth: ${capabilityStateLabel(row?.truth_status)}`].filter(Boolean).join(" · ")}));
  details.append(summary,lists);host.append(heading,grid,details);
  if(templates.length&&typeof onApplyTemplate==="function"){
    const chooser=node("section","implexity-capability-template"),copy=node("div",""),label=node("label","","Start from a provider template"),select=node("select",""),apply=node("button","","Stage selected template"),status=node("small","","Choose a provider-owned starting point. Nothing is changed until you stage it, and provider preflight is still required.");
    select.setAttribute("aria-label","Provider study template");select.dataset.capabilityTemplate="chooser";
    const prompt=node("option","","Choose a template…");prompt.value="";select.append(prompt);
    templates.forEach((template,index)=>{const option=node("option","",String(template?.label||human(template?.id||`Template ${index+1}`)));option.value=String(index);select.append(option);});
    apply.type="button";apply.disabled=true;apply.dataset.capabilityTemplate="apply";status.dataset.capabilityTemplate="status";status.setAttribute("aria-live","polite");
    let staging=false;
    const selectedTemplate=()=>{const index=select.value.trim()===""?NaN:Number(select.value);return Number.isInteger(index)?templates[index]:null;};
    select.onchange=()=>{const template=selectedTemplate();apply.disabled=staging||!template;status.dataset.truthStatus=String(template?.truth_status||"");status.textContent=template?`${String(template.description||"Provider-owned starting point")} Truth: ${capabilityStateLabel(template.truth_status)}. Stage it to edit; validation and preflight still follow.`:"Choose a provider-owned starting point. Nothing is changed until you stage it, and provider preflight is still required.";};
    apply.onclick=async()=>{const template=selectedTemplate();if(staging||!template)return;staging=true;apply.disabled=true;select.disabled=true;try{const checked={...template,problem_patch:workspaceTemplateProblemPatch(template)};await onApplyTemplate(checked);status.dataset.truthStatus=String(template.truth_status||"");status.textContent=`${String(template.label||template.id||"Template")} is staged as an editable draft. Truth: ${capabilityStateLabel(template.truth_status)}. Run provider preflight before use.`;}catch(templateError){status.dataset.truthStatus="error";status.textContent=`Template not staged: ${String(templateError?.message||templateError)}`;}finally{staging=false;select.disabled=false;apply.disabled=!selectedTemplate();}};
    label.append(select);copy.append(label,status);chooser.append(copy,apply);host.append(chooser);
  }
}
function sectionIsOpen(sec){return !!sec&&sec.classList.contains("open")&&!sec.classList.contains("shut")}

class ImplexityWorkbench{
  constructor(){
    this.step=store.get("implexity.step","geometry");
    if(!STEP_BY_ID[this.step])this.step="geometry";
    this.model=null;this.problem=null;this.cae=null;this.interactions=null;this.cfd=null;
    this.lastError=null;this._note="";this._dirty=false;this._stageHeightMode=null;
    this._providerLoad={status:"idle",error:null};this._preflightOriginStep=null;
    this._seedAuthoringPhase=0;this._seedProgressObserver=null;
    this.appliedResponseProgram=null;this.objectiveCompatibility=null;this._responseProgramDirty=false;this._physicsDrafts=new Set();
    this.latestPhysicsPlan=null;this.latestPhysicsIntent=null;this.providerProblems={};this.providerProblemRecords={};this.providerCouplingCatalogs={};this.providerWorkspaceCapabilities={};this._workspaceCapabilitySerial=0;
    this.nativeModel=!!q("#s_engsys");
    this._selectedProvider=store.get("implexity.provider",null);
    const legacyPanes=store.get("implexity.panes",{left:true,right:true});
    this._widePaneState=store.get("implexity.panes.wide",legacyPanes);
    this._compactPaneState=store.get("implexity.panes.compact",this.defaultCompactPaneState());
    this._compactMode=compactPanesForWidth(document.documentElement?.clientWidth||window.innerWidth||0);
    this._paneState={...(this._compactMode?this._compactPaneState:this._widePaneState)};
    this.build();
    this.computationEffort=ImplexityComputationEffort.bind({
      root:document,
      invalidate:reason=>this.invalidateRunAuthorization(reason),
      applyProviderCoupling:(coupling,state)=>this.applyProviderCoupling(coupling,state)
    });
    this.installOptimizationAdapter();
    this.bind();
    this.installObserver();
    this.normaliseLegacySurfaces();
    this.refresh().finally(()=>this.setStep(this.step,{initial:true}));
  }

  build(){
    document.body.classList.add("implexity-workbench");
    document.body.dataset.implexityStep=this.step;
    const top=q("#topbar")||document.body;
    const title=q("h1",top);
    if(title){title.innerHTML='<span class="implexity-brand-mark" aria-hidden="true"></span><span>Implexity</span>';title.title="Implexity implicit CAE workbench";}
    const tagline=q(".tagline",top);
    if(tagline)tagline.textContent="implicit CAE · manual design and direct-gradient topology optimisation";

    const rail=node("nav","implexity-workflow");
    rail.id="implexity-workflow";rail.setAttribute("aria-label","Implicit CAE workflow");rail.setAttribute("aria-describedby","implexity-workflow-help");
    const workflowHelp=node("span","visually-hidden","Use Left and Right Arrow to move between workflow stages, then Enter to open a stage.");workflowHelp.id="implexity-workflow-help";rail.append(workflowHelp);
    STEPS.forEach((s,i)=>{
      const b=node("button","implexity-step");b.type="button";b.dataset.implexityStep=s.id;
      b.tabIndex=s.id===this.step?0:-1;
      if(s.id===this.step)b.setAttribute("aria-current","step");
      b.setAttribute("aria-label",`${i+1}. ${s.label}`);b.title=s.hint;
      b.innerHTML=`<span class="implexity-step-index">${i+1}</span><span>${s.label}</span>`;rail.append(b);
    });
    this.rail=rail;

    const status=node("div","implexity-status");status.id="implexity-status";status.setAttribute("role","status");status.setAttribute("aria-live","polite");status.setAttribute("aria-atomic","true");this.status=status;

    const shell=node("div","implexity-shell-controls");shell.id="implexity-shell-controls";
    shell.innerHTML='<button type="button" data-implexity-pane="left" aria-label="Toggle model panel" aria-controls="implexityModelPane" title="Show or hide the model tree"><svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3 3.5h14v13H3zM7 3.5v13"/></svg><span>Model</span></button><button type="button" data-implexity-focus aria-label="Toggle viewport focus" aria-keyshortcuts="Control+Shift+F Meta+Shift+F" title="Toggle a viewport-focused layout"><svg viewBox="0 0 20 20" aria-hidden="true"><path d="M7 3H3v4M13 3h4v4M7 17H3v-4M13 17h4v-4"/></svg><span>Focus</span></button><button type="button" data-implexity-pane="right" aria-label="Toggle engineering inspector" aria-controls="implexityInspectorPane" title="Show or hide the engineering inspector"><svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3 3.5h14v13H3zM13 3.5v13"/></svg><span>Inspector</span></button><span class="implexity-shell-sep" aria-hidden="true"></span><button type="button" data-implexity-history="undo" aria-label="Undo the last committed implicit edit" aria-keyshortcuts="Control+Z Meta+Z" title="Undo the last committed implicit edit (Ctrl+Z)"><svg viewBox="0 0 20 20" aria-hidden="true"><path d="M8 5 3 9l5 4M4 9h7a5 5 0 0 1 5 5"/></svg><span class="visually-hidden">Undo</span></button><button type="button" data-implexity-history="redo" aria-label="Redo the implicit edit" aria-keyshortcuts="Control+Y Meta+Shift+Z" title="Redo the last committed implicit edit (Ctrl+Y)"><svg viewBox="0 0 20 20" aria-hidden="true"><path d="m12 5 5 4-5 4M16 9H9a5 5 0 0 0-5 5"/></svg><span class="visually-hidden">Redo</span></button>';
    this.shellControls=shell;
    const updateHistory=()=>{const counts=window.ImplexityManualHistory?.counts?.()||{};shell.querySelectorAll('[data-implexity-history]').forEach(button=>{const action=button.dataset.implexityHistory;button.disabled=Boolean(counts.busy)||!(counts[action]>0);button.setAttribute('aria-label',`${human(action)} last manual gesture`);button.title=`${human(action)}: ${counts[action==='undo'?'nextUndo':'nextRedo']||'last shared edit'} (GUI and MCP)`;});};
    window.addEventListener('implexity:manual-history-changed',updateHistory);
    window.addEventListener('implexity:manual-history-ready',updateHistory);updateHistory();

    const dens=q(".densctl",top);
    top.insertBefore(rail,dens||null);top.insertBefore(status,dens||null);top.insertBefore(shell,dens||null);

    const stage=q("#stage")||q("#viewport")||q(".stage")||document.body;
    this.stage=stage;requestAnimationFrame(()=>this.syncStageHeight());
    const context=node("div","implexity-context");context.id="implexity-context";
    context.innerHTML='<div><strong data-implexity-context-title></strong><span data-implexity-context-hint></span></div><div class="implexity-context-actions"><button type="button" data-implexity-quick="next" data-implexity-context-next>Continue setup</button><button type="button" data-implexity-quick="preflight">Preflight</button><button type="button" data-implexity-quick="run" class="primary">Run optimisation</button></div>';
    stage.append(context);this.context=context;
    const app=q("#app");
    const backdrop=node("button","implexity-pane-backdrop");backdrop.type="button";backdrop.hidden=true;backdrop.setAttribute("aria-label","Close open panel");backdrop.setAttribute("aria-hidden","true");
    app?.append(backdrop);this.paneBackdrop=backdrop;

    if(this.nativeModel)this.installNativePhysicsPanel();else this.installFallbackInspector();
    this.installReadinessCard();
    this.installMultiphysicsReadiness();
    this.installNumericalSolverHealth();
    this.installEngineeringHistory();
    this.installEngineeringAgent();
    this.applyPaneState();
  }

  installNativePhysicsPanel(){
    const sec=q("#s_engsys .sect-b");if(!sec)return;
    let host=q("#implexity-native-physics");
    if(!host){host=node("div","implexity-native-provider-card");host.id="implexity-native-physics";host.innerHTML='<div class="implexity-native-head"><div><strong>Modular physics</strong><span>Choose a physics provider. Supported analyses, design inputs and derivatives are listed below.</span></div><span class="implexity-core-coordinate">Physics setup</span></div><div id="implexity-provider-list"></div><section id="implexity-provider-capabilities" class="implexity-capability-summary" aria-label="Selected provider capabilities" hidden></section><div id="implexity-physics-editor" hidden></div>';sec.prepend(host);}
    this.physicsHost=host;
  }

  installFallbackInspector(){
    const right=q(".pane.right")||q("aside")||document.body;
    let ins=q("#implexity-inspector");
    if(!ins){ins=node("section","sect open");ins.id="implexity-inspector";ins.innerHTML='<h2 class="sect-h"><span class="cv">▾</span><span class="tt">CAE workspace</span></h2><div class="sect-b"><div id="implexity-provider-list"></div><section id="implexity-provider-capabilities" class="implexity-capability-summary" aria-label="Selected provider capabilities" hidden></section><div id="implexity-physics-editor"></div></div>';right.prepend(ins);}
    this.inspector=ins;
  }

  installReadinessCard(){
    const host=q("#s_opt .sect-b");if(!host||q("#implexity-readiness"))return;
    const card=node("div","implexity-readiness");card.id="implexity-readiness";
    card.innerHTML='<div class="implexity-readiness-head"><div><strong>Direct-gradient hand-off</strong><span>Manual edits and optimisation share one design after an explicit topology hand-off.</span></div><span class="implexity-core-coordinate" data-implexity-topology-badge>Checking topology…</span></div><div class="implexity-readiness-grid" data-implexity-readiness-grid role="list" aria-label="Optimisation readiness"></div><div class="implexity-readiness-actions"><button type="button" data-implexity-ready-action="next" aria-describedby="implexity-readiness-help">Continue setup</button><button type="button" class="primary" data-implexity-ready-action="run" aria-describedby="implexity-readiness-help">Start direct-gradient optimisation</button></div><p id="implexity-readiness-help" class="implexity-readiness-help" role="status" aria-live="polite" aria-atomic="true">Readiness is being checked.</p>';
    host.prepend(card);this.readiness=card;
  }

  installMultiphysicsReadiness(){
    const host=q("#s_opt .sect-b");if(!host||q("#implexity-multiphysics-readiness"))return;
    const card=node("section","implexity-multiphysics-readiness");card.id="implexity-multiphysics-readiness";
    card.innerHTML='<div class="implexity-mp-head"><div><strong>Physics status</strong></div><span data-implexity-mp-badge>Waiting for an optimisation run</span></div><div class="implexity-mp-grid" data-implexity-mp-grid></div><div class="implexity-mp-recommendation" data-implexity-mp-recommendation></div>';
    const readiness=q("#implexity-readiness");if(readiness?.nextSibling)host.insertBefore(card,readiness.nextSibling);else host.prepend(card);
    this.multiphysicsReadiness=card;
    this.renderMultiphysicsReadiness(null);
  }

  renderMultiphysicsReadiness(row){
    const card=this.multiphysicsReadiness;if(!card)return;
    const badge=q("[data-implexity-mp-badge]",card),grid=q("[data-implexity-mp-grid]",card),rec=q("[data-implexity-mp-recommendation]",card);
    const adaptive=row?.adaptive||null, mp=adaptive?.multiphysics||{}, values=mp.values||{};
    const fmtValue=(value,unit="",digits=3)=>Number.isFinite(Number(value))?`${Number(value).toLocaleString(undefined,{maximumFractionDigits:digits})}${unit?` ${unit}`:""}`:"Not available";
    const items=[
      ["Temperature margin",values.temperature_margin_K,"K",v=>v>0],
      ["Stress utilisation",values.stress_utilization,"",v=>v<=1],
      ["Cooling margin",values.cooling_margin_K,"K",v=>v>0],
      ["Cycle damage",values.maximum_cycle_damage,"",v=>v<=1],
      ["Creep damage",values.creep_damage,"",v=>v<=1],
      ["Fatigue damage",values.fatigue_damage,"",v=>v<=1],
      ["Oxidation damage",values.oxidation_damage,"",v=>v<=1]
    ].filter(([,v])=>v!==undefined&&v!==null);
    if(!row){
      badge.textContent="Waiting for an optimisation run";badge.className="";
      grid.replaceChildren(node("div","implexity-mp-empty","No physics results yet."));
      rec.textContent="";return;
    }
    if(!items.length){
      grid.replaceChildren(node("div","implexity-mp-empty","The active provider has not supplied thermal, structural, cooling or ageing margins for this iterate."));
    }else{
      grid.replaceChildren(...items.map(([label,value,unit,ok])=>{const within=ok(Number(value)),item=node("div",`implexity-mp-item ${within?"ok":"warn"}`);item.dataset.state=within?"within-limit":"needs-attention";item.setAttribute("role","listitem");item.append(node("span","",label),node("strong","",fmtValue(value,unit)),node("span","visually-hidden",within?"Within the declared limit.":"Needs attention."));return item;}));
    }
    if(adaptive){
      badge.textContent=adaptive.ready?"Ready for the next declared fidelity":"Continue at the current fidelity";
      badge.className=adaptive.ready?"ok":"pending";
      const reasons=(adaptive.reasons||[]).map(human);
      if(adaptive.ready){
        const next=adaptive.next_stage?` Recommended next stage: ${human(adaptive.next_stage)}.`:"";
        rec.textContent=`Current numerical, regime and multiphysics criteria are satisfied.${next}`;
      }else if(reasons.length){
        rec.textContent=`Remain at the current fidelity because ${reasons.join("; ")}.`;
      }else{
        rec.textContent="The current fidelity remains appropriate for this design.";
      }
    }else{
      badge.textContent="No adaptive qualification declared";badge.className="pending";
      rec.textContent="The current schedule does not define adaptive fidelity criteria for this stage.";
    }
  }

  installNumericalSolverHealth(){
    const host=q("#s_opt .sect-b");if(!host||q("#implexity-numerical-solver-health"))return;
    const card=node("section","implexity-multiphysics-readiness implexity-solver-health solver-progress");card.id="implexity-numerical-solver-health";card.hidden=true;
    card.innerHTML='<div class="implexity-mp-head"><div><strong>Numerical solver health</strong><span>Live residual certification reported by the active provider.</span></div><span data-implexity-solver-badge>Waiting for solver records</span></div><details class="solver-details"><summary>Convergence records</summary><div class="implexity-mp-grid" data-implexity-solver-grid role="list" aria-label="Numerical solver certification"></div><div class="implexity-mp-recommendation" data-implexity-solver-summary></div></details>';
    const readiness=this.multiphysicsReadiness;if(readiness?.nextSibling)host.insertBefore(card,readiness.nextSibling);else host.prepend(card);
    this.numericalSolverHealth=card;
  }

  renderNumericalSolverHealth(row,event={}){
    const card=this.numericalSolverHealth;if(!card)return;
    const attention=event?.numerical_attention;
    const failure=event?.error?.numerical_solver_failure;
    const terminalRecord=attention?.solver_record||failure?.solver_record||null;
    const health=terminalRecord?numericalSolverHealth({diagnostics:{numerical_solver_records:{schema:NUMERICAL_SOLVER_RECORD_SET_SCHEMA,records:[terminalRecord]}}}):numericalSolverHealth(row);card.hidden=!health;if(!health)return;
    const badge=q("[data-implexity-solver-badge]",card),grid=q("[data-implexity-solver-grid]",card),summary=q("[data-implexity-solver-summary]",card);
    const sci=value=>value!==null&&value!==undefined&&Number.isFinite(Number(value))?Number(value).toExponential(3):"Not finite";
    badge.textContent=attention?"Numerical decision needed":failure?"Numerical solve failed closed":health.allCertified?"All reported forward solves certified":`${health.failedCount} reported forward solve${health.failedCount===1?"":"s"} failed certification`;
    badge.className=health.allCertified?"ok":"failed";
    grid.replaceChildren(...health.records.map((record,index)=>{
      const passed=record?.certification?.passed===true,item=node("div",`implexity-mp-item ${passed?"ok":"warn"}`);
      item.dataset.state=passed?"certified":"failed-certification";item.setAttribute("role","listitem");
      const limit=record?.certification?.relative_residual_limit;
      item.append(node("span","",`Reported forward solve ${index+1}`),node("strong","",`${sci(record.final_relative_residual)} / ${sci(limit)}`),node("span","",`${record.attempt_count} attempt${record.attempt_count===1?"":"s"} · ${record.retry_count} retr${record.retry_count===1?"y":"ies"} · budget ${record.max_iterations_per_attempt} per attempt / ${record.total_iteration_budget} total`));
      return item;
    }));
    const tolerances=[...new Set(health.records.map(record=>sci(record.requested_relative_tolerance)))];
    summary.textContent=`Requested relative tolerance ${tolerances.join(", ")}; largest final residual ${sci(health.largestFinalResidual)}; ${health.retries} total retr${health.retries===1?"y":"ies"}. Actual iteration counts are unavailable unless explicitly reported by the solver.`;
  }

  installEngineeringAgent(){
    const host=q("#s_opt .sect-b");if(!host||q("#implexity-engineering-agent"))return;
    const card=node("section","implexity-engineering-agent");card.id="implexity-engineering-agent";
    card.innerHTML='<div class="implexity-agent-head"><div><strong>Engineering agent</strong><span>Optional autonomous operation through the same validated engineering actions as the native GUI.</span></div><span data-implexity-agent-policy>Loading policy…</span></div><div class="implexity-agent-grid" data-implexity-agent-grid></div><section class="implexity-agent-guidance"><div class="implexity-agent-guidance-head"><strong>Context guidance</strong><span>Live classification of the next admissible engineering actions.</span></div><div class="implexity-agent-guidance-groups" data-implexity-agent-guidance><span>Loading context-sensitive guidance…</span></div></section><div class="implexity-agent-note" data-implexity-agent-note>Declared engineering actions are used. External model import starts the selected local adapter.</div><details class="implexity-agent-help"><summary>Agent operating manual</summary><p>The agent receives a live, self-describing manual containing its permitted engineering actions, input contracts, installed physics, workflow rules, current Engineering Intent and context-sensitive next-action guidance. It must re-inspect the authoritative state after consequential changes.</p><code>/v1/agent/manual · /v1/agent/context · /v1/agent/guidance</code></details>';
    const hist=q("#implexity-engineering-history");if(hist)hist.after(card);else host.prepend(card);
    this.agentCard=card;this.refreshEngineeringAgent();
  }

  async refreshEngineeringAgent(){
    const card=this.agentCard;if(!card)return;
    try{
      const [policy,caps,guidance]=await Promise.all([json("/v1/agent/policy"),json("/v1/agent/capabilities"),json("/v1/agent/guidance")]);
      q("[data-implexity-agent-policy]",card).textContent=`Autonomy: ${human(policy.autonomy)}`;
      const allowed=(caps.actions||[]).filter(x=>x.allowed);
      const groups=[
        ["Inspect and analyse",allowed.filter(x=>["read","analyze"].includes(x.permission)).length],
        ["Modify engineering model",allowed.filter(x=>x.permission==="author").length],
        ["Operate direct-gradient optimisation",allowed.filter(x=>x.permission==="optimize").length],
        ["Create optimisation branches",allowed.filter(x=>x.permission==="branch").length]
      ];
      const agentGrid=q("[data-implexity-agent-grid]",card);agentGrid.replaceChildren(...groups.map(([label,count])=>{const item=node("div","");item.append(node("span","",label),node("strong","",count?"Available":"Not permitted"));return item;}));
      const guide=q("[data-implexity-agent-guidance]",card);const categoryOrder=["Required next","Useful now","Blocked now"];
      const categoryRows=categoryOrder.map(category=>[category,(guidance.categories?.[category]||[]).slice(0,4)]).filter(([,rows])=>rows.length);
      guide.replaceChildren();
      if(!categoryRows.length)guide.append(node("span","","No special next-action guidance is currently required."));
      for(const [category,rows] of categoryRows){const group=node("div","implexity-agent-guidance-group");group.dataset.category=category;group.append(node("strong","",category));for(const row of rows){const item=node("div","");item.append(node("b","",publicMessage(row.label,human(row.action))),node("span","",publicMessage(row.reason,"Review the current engineering state before continuing.")));group.append(item);}guide.append(group);}
      q("[data-implexity-agent-note]",card).textContent=policy.restore_requires_out_of_band_approval?"Restoring an earlier engineering branch always requires separate user approval. The agent uses the same model, physics providers, preflight and direct-gradient job runtime as you do.":"The agent uses the same authoritative engineering runtime as the native GUI.";
    }catch(e){q("[data-implexity-agent-policy]",card).textContent="Agent interface unavailable";renderMessage(q("[data-implexity-agent-note]",card),"Engineering-agent guidance could not be loaded. The manual workflow remains available.",e);}
  }

  installEngineeringHistory(){
    const host=q("#s_opt .sect-b");if(!host||q("#implexity-engineering-history"))return;
    const card=node("section","implexity-engineering-history");card.id="implexity-engineering-history";
    card.innerHTML='<div class="implexity-history-head"><div><strong>Engineering history</strong><span>Manual edits and direct-gradient branches in one timeline.</span></div><div><button type="button" data-implexity-history-snapshot>Save branch point</button> <button type="button" data-implexity-history-refresh>Refresh</button></div></div><div data-implexity-history-list class="implexity-history-list"><span>History will appear after the first committed engineering action.</span></div>';
    host.append(card);this.engineeringHistory=card;
    q("[data-implexity-history-refresh]",card).onclick=()=>this.refreshEngineeringHistory();
    q("[data-implexity-history-snapshot]",card).onclick=async()=>{try{await json("/v1/implicit/history/snapshot",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({label:"Manual branch point",details:{objective_setup:this.captureObjectiveSetup()}})});this.note("Geometry branch point and objective setup saved; physics conditions are not included");this.refreshEngineeringHistory();}catch(e){this.fail("The engineering branch point could not be saved. Try again after checking the service connection.",e)}};
    this.refreshEngineeringHistory();
  }

  captureObjectiveSetup(){
    return {provider:this.currentProvider(),draft:this.clone(window.ImplexityObjectiveComposer?.specification?.()||this.appliedResponseProgram),applied:this.clone(this.appliedResponseProgram)};
  }

  restoreObjectiveSetup(snapshot){
    this.appliedResponseProgram=null;this._dirty=true;this._responseProgramDirty=true;
    if(window.S){window.S.objective=[];window.renderObjective?.();}
    this.invalidateRunAuthorization("A saved geometry was restored; review conditions and objectives before running.");
    const saved=snapshot?.details?.objective_setup;
    const composer=window.ImplexityObjectiveComposer;
    composer?.setProgram?.({schema:"implexity-response-program/2",objectives:[],constraints:[],normalisation:"response_scale"});
    if(saved?.provider===this.currentProvider()&&(saved.draft||saved.applied)){
      composer?.setProgram?.(saved.draft||saved.applied);
      return "Geometry restored; saved objectives loaded as a draft. Review physics conditions and apply objectives before recomputing.";
    }
    return saved?.provider?`Geometry restored; saved objectives belong to ${friendlyProvider(saved.provider)}. The active objective program was cleared. Physics conditions must be reviewed.`:"Geometry restored without objective settings; configure objectives and review physics conditions before recomputing.";
  }

  async refreshEngineeringHistory(){
    const card=this.engineeringHistory;if(!card)return;
    const list=q("[data-implexity-history-list]",card);
    try{
      const data=await json("/v1/implicit/history");const rows=(data.entries||[]).slice(-12).reverse();
      if(!rows.length){list.replaceChildren(node("span","","No committed engineering actions yet."));return;}
      const labels={manual_manipulation:"Manual geometry edit",optimization_started:"Direct-gradient optimisation started",optimization_intervention:"Manual intervention opened",optimization_branch:"Optimisation resumed from modified design",optimization_accepted:"Optimised design accepted",optimization_discarded:"Optimisation result discarded"};
      list.replaceChildren(...rows.map(r=>{const item=node("div","implexity-history-row"),when=node("span","",new Date((r.time||0)*1000).toLocaleTimeString([], {hour:'2-digit',minute:'2-digit',second:'2-digit'})),title=node("strong","",labels[r.kind]||human(r.label||r.kind));let detail=r.actor&&r.actor!=="User"?`${human(r.actor)} · `:"";detail+=r.details?.job_id?`Optimisation job ${String(r.details.job_id).slice(0,8)}`:(r.details?.node?`Geometry ${human(r.details.node)}`:"");item.append(when,title,node("small","",detail));if(r.kind==="state_snapshot"){const restore=node("button","","Restore");restore.type="button";restore.dataset.implexityRestoreSnapshot=String(r.id);restore.setAttribute("aria-label",`Restore branch point ${title.textContent}`);item.append(restore);}return item;}));
      list.querySelectorAll("[data-implexity-restore-snapshot]").forEach(b=>b.onclick=async()=>{if(!confirm("Restore this engineering branch point? Current unsaved model changes will be replaced. Review physics conditions and reapply objectives before recomputing."))return;let restored=false;try{const result=await json(`/v1/implicit/history/${b.dataset.implexityRestoreSnapshot}/restore`,{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});restored=true;this.note(this.restoreObjectiveSetup(result.snapshot));this.refreshModelSoon();this.refreshEngineeringHistory();}catch(e){this.fail(restored?"Geometry was restored, but its objective setup could not be loaded. Review and reapply the setup before running.":"Branch-point restoration was not confirmed. Refresh the model before taking another action.",e)}});
    }catch(_){list.replaceChildren(node("span","","Engineering history is unavailable on this service."));}
  }

  bind(){
    const invalidate=reason=>this.invalidateRunAuthorization(reason);
    this.rail.addEventListener("click",e=>{const b=e.target.closest("button[data-implexity-step]");if(b)this.setStep(b.dataset.implexityStep)});
    this.rail.addEventListener("keydown",e=>{
      if(!["ArrowLeft","ArrowRight","Home","End"].includes(e.key))return;
      const buttons=qa("button[data-implexity-step]",this.rail),current=e.target.closest?.("button[data-implexity-step]");
      if(!current||!buttons.length)return;e.preventDefault();
      const index=buttons.indexOf(current),next=e.key==="Home"?buttons[0]:e.key==="End"?buttons[buttons.length-1]:
        buttons[(index+(e.key==="ArrowRight"?1:-1)+buttons.length)%buttons.length];
      buttons.forEach(button=>button.tabIndex=button===next?0:-1);next?.focus?.();
    });
    this.shellControls.addEventListener("click",e=>{
      const pane=e.target.closest("[data-implexity-pane]");if(pane)this.togglePane(pane.dataset.implexityPane,{trigger:pane,focusInside:e.detail===0});
      if(e.target.closest("[data-implexity-focus]"))this.toggleFocus();
      const hist=e.target.closest("[data-implexity-history]");if(hist)this.history(hist.dataset.implexityHistory);
    });
    this.context.addEventListener("click",e=>{const b=e.target.closest("[data-implexity-quick]");if(b)this.quick(b.dataset.implexityQuick)});
    this.readiness?.addEventListener("click",e=>{const b=e.target.closest("[data-implexity-ready-action]");if(b)this.quick(b.dataset.implexityReadyAction)});
    this.paneBackdrop?.addEventListener("click",()=>this.closeCompactPanes({restoreFocus:true}));
    this.stage?.addEventListener("pointerdown",event=>{
      if(event.target.closest?.('button,input,select,textarea,a,[contenteditable="true"]'))return;
      this.stage.focus?.({preventScroll:true});
    },{capture:true});

    document.addEventListener("keydown",e=>{
      const target=e.target instanceof Element?e.target:null;
      if(e.key==="Escape"&&this._compactMode&&(this._paneState.left||this._paneState.right)&&!document.querySelector("dialog[open]")){
        e.preventDefault();const openSide=this._paneState.left?"left":"right";this.closeCompactPanes({restoreFocus:true,fallback:q(`[data-implexity-pane="${openSide}"]`,this.shellControls)});return;
      }
      if(document.querySelector("dialog[open]")||target?.closest('input,textarea,select,button,[contenteditable="true"],[role="textbox"],[role="dialog"]'))return;
      const directHistory=window.ImplexityDirectInteraction;
      if(!directHistory&&(e.ctrlKey||e.metaKey)&&!e.shiftKey&&e.key.toLowerCase()==="z"){e.preventDefault();this.history("undo")}
      if(!directHistory&&(e.ctrlKey||e.metaKey)&&(e.key.toLowerCase()==="y"||(e.shiftKey&&e.key.toLowerCase()==="z"))){e.preventDefault();this.history("redo")}
      if((e.ctrlKey||e.metaKey)&&e.shiftKey&&e.key.toLowerCase()==="f"){e.preventDefault();this.toggleFocus()}
    });

    window.addEventListener("implexity:interaction-ready",()=>{this.scopeInteractionShortcuts();this.normaliseLegacySurfaces();this.prepareInteractionStep()});
    window.addEventListener("implexity-authoring-mode",e=>this.note(`Viewport tool: ${human(e.detail?.mode||"select")}`));
    window.addEventListener("implexity:interaction-mode",e=>this.note(`Viewport tool: ${e.detail?.label||human(e.detail?.mode||"select")}`));
    window.addEventListener("implexity:interaction-committed",e=>{announceAuthoritativeChange("A manual geometry edit changed the design.",{source:"manual-geometry",legacy_event:e.type});this._dirty=true;this.note("Manual edit committed to the authoritative implicit state");this.refreshModelSoon();this.refreshEngineeringHistory()});
    window.addEventListener("implexity:model-updated",e=>{announceAuthoritativeChange("The authoritative model changed.",{source:"model-update",legacy_event:e.type});this._dirty=true;this.refreshModelSoon()});
    window.addEventListener("implexity-authoritative-state-changed",e=>{if(!e.detail?.canonicalDispatched)announceAuthoritativeChange("The authoritative design history changed.",{source:"history-compatibility",legacy_event:e.type});this.refreshModelSoon()});
    window.addEventListener("implexity:design-state-changed",()=>{this._dirty=true;this.refreshModelSoon();this.refreshEngineeringHistory()});
    window.addEventListener("implexity:optimization-job",e=>{this.setStep("optimize");this.note(`Optimisation ${e.detail?.job_id?"job started":"started"}`)});
    window.addEventListener("implexity:optimization-progress",e=>{const row=e.detail?.row||null;this.renderMultiphysicsReadiness(row);this.renderNumericalSolverHealth(row,e.detail||{});this.refreshEngineeringAgent();if(e.detail?.status==="completed"){this.setStep("results");this.refreshEngineeringHistory();}});
    window.addEventListener("implexity:preflight-state",e=>this.handlePreflightState(e.detail||{}));
    window.addEventListener("implexity:optimization-intervention",()=>{this.refreshEngineeringHistory();this.refreshEngineeringAgent();});
    window.addEventListener("implexity:optimization-branch",()=>{this.refreshEngineeringHistory();this.refreshEngineeringAgent();});
    window.addEventListener("implexity:provider-problem-changed",e=>{
      invalidate("The physics problem changed.");
      const id=String(e.detail?.providerId||"");if(id&&e.detail?.problem)this.providerProblems[id]=this.clone(e.detail.problem);
      if(id&&e.detail?.record)this.providerProblemRecords[id]=this.clone(e.detail.record);
      if(id==="intent_orchestrated"){this.latestPhysicsPlan=null;this.latestPhysicsIntent=null;}
      if(id==="legacy_multiphysics_implicit")this.problem=this.clone(e.detail?.problem||this.problem);
      if(id)this.refreshCouplingCatalog(id).catch(()=>{});
      if(id===this.currentProvider()||this.currentProvider()==="intent_orchestrated"){this.renderStatus();this.renderReadiness();}
    });
    window.addEventListener("implexity:physics-plan",e=>{
      invalidate("The selected physics add-in plan changed.");
      this.latestPhysicsPlan=this.clone(e.detail?.plan||null);this.latestPhysicsIntent=this.clone(e.detail?.intent||null);
      if(this.latestPhysicsPlan&&this.currentProvider()==="intent_orchestrated")this.syncObjectiveCatalogue();this.renderStatus();this.renderReadiness();
    });
    window.addEventListener("implexity-engineering-problem-changed",e=>{
      invalidate("The engineering conditions or physics problem changed.");
      const id=String(e.detail?.providerId||this.currentProvider()||"");
      const problem=this.clone(e.detail?.problem||e.detail||null);
      if(id&&problem)this.providerProblems[id]=problem;
      if(id==="legacy_multiphysics_implicit")this.problem=problem||this.problem;
      this._dirty=true;this.renderStatus();this.renderProviders();this.renderReadiness();
    });
    window.addEventListener("implexity:response-program-draft",()=>{invalidate("The objective or constraint program changed.");this._responseProgramDirty=true;this._dirty=true;this.renderReadiness();this.updateObjectiveCompatibility()});
    window.addEventListener("implexity:response-program-apply",event=>{
      if(!event.detail?.program)return;
      event.preventDefault();
      try{const value=this.applyResponseProgram(event.detail.program);event.detail.respond?.(value)}
      catch(error){event.detail.respond?.({ok:false,error:String(error?.message||error)});this.fail("The response program could not be applied. Review the objective setup and try again.",error);return;}
      invalidate("The applied objective or constraint program changed.");
      this.renderReadiness();this.updateObjectiveCompatibility();
    });
    window.addEventListener("implexity-render-engineering-glyphs",()=>this.loadInteractions());
    window.addEventListener("implexity:cfd-workspace-ready",e=>{this.cfdClass=e.detail?.Class||window.ImplexityCFDWorkspace;this.renderProviders()});
    window.addEventListener("implexity:density-changed",()=>this.focusNative(this.step,{initial:true}));
    window.addEventListener("implexity:provider-selection-changed",()=>invalidate("The physics provider changed."));
    window.addEventListener("implexity:physics-packages-changed",()=>invalidate("The active physics add-ins changed."));
    window.addEventListener("implexity:design-freedom",()=>invalidate("The design-freedom hierarchy changed."));
    window.addEventListener("implexity:design-freedom-cleared",()=>invalidate("The design-freedom hierarchy changed."));
    window.addEventListener("resize",()=>{this.normaliseLegacySurfaces();this.syncPaneMode();this.syncStageHeight()});
  }

  installObserver(){
    const observer=new MutationObserver(()=>{
      if(this._observerScheduled)return;
      this._observerScheduled=true;
      requestAnimationFrame(()=>{
        this._observerScheduled=false;
        this.normaliseLegacySurfaces();
      });
    });
    observer.observe(document.body,{childList:true,subtree:true,attributes:true,attributeFilter:["data-active"]});this.observer=observer;
    if(typeof ResizeObserver==="function"){
      this.layoutObserver=new ResizeObserver(()=>{this.syncPaneMode();this.syncStageHeight()});
      const app=q("#app");if(app)this.layoutObserver.observe(app);
      if(this.stage&&this.stage!==app)this.layoutObserver.observe(this.stage);
    }
  }

  normaliseLegacySurfaces(){
    if(!this.stage)return;
    const duplicate=q("#implexity-authoring-toolbar");if(duplicate)duplicate.dataset.implexityCompatibility="true";

    const toolbar=q(".implexity-toolbar");
    if(toolbar&&!q(".implexity-tool-context",toolbar)){
      const label=node("div","implexity-tool-context");label.innerHTML='<strong>Geometry tools</strong><span>same implicit design state</span>';toolbar.prepend(label);
    }

    const sensitivity=q("#implexity-sensitivity-authoring");
    if(sensitivity){
      const sensitivityDock=q("#s_sens .sect-b")||q("#implexityInspectorPane");
      if(sensitivityDock&&sensitivity.parentElement!==sensitivityDock)sensitivityDock.append(sensitivity);
      if(!sensitivity.dataset.implexityPrepared){
        sensitivity.dataset.implexityPrepared="true";sensitivity.classList.add("collapsed");
        const btn=q('[data="collapse"]',sensitivity);if(btn){btn.textContent="+";btn.setAttribute("aria-expanded","false");btn.title="Open sensitivity-guided design space";btn.addEventListener("click",()=>queueMicrotask(()=>this.syncSensitivityButton()));}
      }
      this.syncSensitivityButton();
    }

    const objective=q(".implexity-objective-panel");
    const objectiveDock=q("#s_obj .sect-b")||q("#implexityInspectorPane");
    if(objective&&objectiveDock&&objective.parentElement!==objectiveDock)objectiveDock.append(objective);
    if(objective){objective.dataset.implexityDocked="true";objective.setAttribute("role","region");objective.removeAttribute("aria-modal");objective.setAttribute("aria-label","Objectives and constraints");}
    const legacyObjective=q("#implexityLegacyObjectiveEditor");
    if(legacyObjective){
      const canonicalObjectiveReady=Boolean(objective&&objective.dataset.implexityDocked==="true");
      legacyObjective.hidden=canonicalObjectiveReady;
      legacyObjective.inert=canonicalObjectiveReady;
      legacyObjective.setAttribute("aria-hidden",String(canonicalObjectiveReady));
      legacyObjective.dataset.compatibilityOnly=String(canonicalObjectiveReady);
    }
    const toggle=q(".implexity-objective-toggle");if(toggle)toggle.dataset.implexityCompatibility="true";
    this.scopeInteractionShortcuts();
    this.normaliseAccessibleSurfaces();
    this.installSeedProgress();
    this.prepareInteractionStep();
  }

  scopeInteractionShortcuts(){
    const controller=window.ImplexityInteraction;
    if(!controller||typeof controller._onKeyDown!=="function"||controller.__implexityScopedKeyboard)return;
    const original=controller._onKeyDown.bind(controller),stage=this.stage;
    controller._onKeyDown=event=>{
      if(event.key==="Escape")return original(event);
      if(event.ctrlKey||event.metaKey||event.altKey)return;
      const path=typeof event.composedPath==="function"?event.composedPath():[];
      const active=document.activeElement;
      if(active?.closest?.('[data-viewport-shortcuts="suspend"]'))return;
      const scoped=Boolean(stage&&(active===stage||stage.contains?.(active)||path.includes(stage)));
      if(!scoped)return;
      return original(event);
    };
    controller.__implexityScopedKeyboard=true;
  }

  normaliseAccessibleSurfaces(){
    const toolbar=q(".implexity-toolbar");
    if(toolbar){
      toolbar.setAttribute("role","toolbar");toolbar.setAttribute("aria-label","Viewport editing tools");
      qa(".implexity-tool",toolbar).forEach(button=>{
        const key=String(button.querySelector?.("kbd,.implexity-tool-key")?.textContent||"").trim();
        if(key)button.setAttribute("aria-keyshortcuts",key.toUpperCase());
      });
      if(!toolbar.dataset.implexityKeyboard){
        toolbar.dataset.implexityKeyboard="true";
        toolbar.addEventListener("keydown",event=>{
          if(!["ArrowLeft","ArrowRight","Home","End"].includes(event.key)||event.altKey||event.ctrlKey||event.metaKey)return;
          const items=qa(".implexity-tool",toolbar).filter(button=>!button.disabled&&button.getClientRects().length);
          const index=items.indexOf(document.activeElement);if(index<0)return;
          event.preventDefault();event.stopPropagation();
          const next=event.key==="Home"?items[0]:event.key==="End"?items[items.length-1]:items[(index+(event.key==="ArrowRight"?1:-1)+items.length)%items.length];
          next.focus({preventScroll:true});next.scrollIntoView({block:"nearest",inline:"nearest"});
        });
      }
    }
    const gestureHud=q(".implexity-hud");
    if(gestureHud){gestureHud.setAttribute("role","status");gestureHud.setAttribute("aria-live","polite");gestureHud.setAttribute("aria-atomic","true");gestureHud.setAttribute("aria-label","Current viewport interaction");}
    const notifications=q(".implexity-toast-stack");
    if(notifications){notifications.setAttribute("role","region");notifications.setAttribute("aria-label","Viewport notifications");}
    const menu=q(".implexity-context-menu");
    if(menu){
      const active=menu.dataset.active==="true";
      menu.setAttribute("role","menu");menu.setAttribute("aria-label","Viewport selection actions");menu.setAttribute("aria-hidden",String(!active));
      const actions=qa(".implexity-context-action",menu);
      actions.forEach((button,index)=>{button.setAttribute("role","menuitem");button.tabIndex=index===0?0:-1;});
      if(!menu.dataset.implexityKeyboard){
        menu.dataset.implexityKeyboard="true";
        menu.addEventListener("keydown",event=>{
          if(!["ArrowUp","ArrowDown","Home","End","Escape"].includes(event.key))return;
          event.preventDefault();
          if(event.key==="Escape"){menu.dataset.active="false";this.stage?.focus?.({preventScroll:true});return;}
          const items=qa('[role="menuitem"]',menu);if(!items.length)return;
          const index=Math.max(0,items.indexOf(document.activeElement));
          const next=event.key==="Home"?items[0]:event.key==="End"?items[items.length-1]:items[(index+(event.key==="ArrowDown"?1:-1)+items.length)%items.length];
          items.forEach(item=>item.tabIndex=item===next?0:-1);next.focus?.();
        });
      }
      if(active&&!menu.contains(document.activeElement))requestAnimationFrame(()=>actions[0]?.focus?.({preventScroll:true}));
      if(!active&&menu.contains(document.activeElement))this.stage?.focus?.({preventScroll:true});
    }
    qa("#grapheditout,#checkout,#pfout,#sensout,#acceptbox,#optmsg").forEach(output=>{
      output.setAttribute("role","status");output.setAttribute("aria-live","polite");output.setAttribute("aria-atomic","true");
    });
    qa(".problems").forEach(output=>{output.setAttribute("role","alert");output.setAttribute("aria-live","assertive");output.setAttribute("aria-atomic","true");});
    qa(".warnbox").forEach(output=>{output.setAttribute("role","status");output.setAttribute("aria-live","polite");output.setAttribute("aria-atomic","true");});
  }

  syncSensitivityButton(){
    const s=q("#implexity-sensitivity-authoring");if(!s)return;
    const collapsed=s.classList.contains("collapsed");const b=q('[data="collapse"]',s);if(!b)return;
    const text=collapsed?"+":"−";const expanded=String(!collapsed);const title=collapsed?"Open sensitivity-guided design space":"Collapse sensitivity-guided design space";
    if(b.textContent!==text)b.textContent=text;
    if(b.getAttribute("aria-expanded")!==expanded)b.setAttribute("aria-expanded",expanded);
    if(b.title!==title)b.title=title;
  }

  installSeedProgress(){
    const dialog=q("#geometrySeedDialog");if(!dialog||dialog.dataset.implexityProgressBound==="true")return;
    dialog.dataset.implexityProgressBound="true";
    const classify=event=>{
      const target=event?.target;
      if(target?.closest?.(".seed-recipe"))this._seedAuthoringPhase=1;
      else if(target?.closest?.("#geometrySeedMode,#geometrySeedBakeOptions"))this._seedAuthoringPhase=2;
      else if(target?.closest?.("#geometrySeedParameterSection"))this._seedAuthoringPhase=1;
      else if(target?.closest?.("#geometrySeedPreview"))this._seedAuthoringPhase=2;
      else if(target?.closest?.("#geometrySeedCommit"))this._seedAuthoringPhase=3;
      queueMicrotask(()=>this.syncSeedProgress());
    };
    dialog.addEventListener("click",classify,true);dialog.addEventListener("input",classify,true);dialog.addEventListener("change",classify,true);
    dialog.addEventListener("close",()=>{this._seedAuthoringPhase=0;this.syncSeedProgress();});
    q("#newgeometry")?.addEventListener("click",()=>{this._seedAuthoringPhase=0;queueMicrotask(()=>this.syncSeedProgress());},{capture:true});
    this._seedProgressObserver=new MutationObserver(()=>this.syncSeedProgress());
    this._seedProgressObserver.observe(dialog,{subtree:true,childList:true,characterData:true,attributes:true,attributeFilter:["data-capability","disabled","aria-invalid","checked"]});
    this.syncSeedProgress();
  }

  syncSeedProgress(){
    const dialog=q("#geometrySeedDialog"),steps=qa(".seed-progress span",dialog);if(!dialog||steps.length!==4)return;
    const state=window.ImplexityGeometrySeeds?.state||null;
    let phase=0,condition="upcoming";
    if(state){
      const hasSource=state.workflow==="bake_current"?Boolean(state.current?.loaded):Boolean(state.selected);
      if(state.committing||state.preview)phase=3;
      else if(state.loading&&state.capabilityAvailable!==null)phase=2;
      else if(hasSource)phase=Math.max(1,Math.min(2,this._seedAuthoringPhase||1));
      if(state.capabilityAvailable===false)condition="unavailable";
      else if(state.loading||state.committing)condition="busy";
      else condition="ready";
    }
    dialog.dataset.seedPhase=String(phase+1);dialog.dataset.seedPhaseState=condition;
    steps.forEach((step,index)=>{
      const status=index<phase?"complete":index===phase?(condition==="unavailable"?"unavailable":"current"):"upcoming";
      step.classList.toggle("active",index===phase);step.classList.toggle("complete",status==="complete");
      step.dataset.state=status;
      if(index===phase)step.setAttribute("aria-current","step");else step.removeAttribute("aria-current");
      step.setAttribute("aria-label",`${step.textContent.trim()}: ${status}`);
    });
  }

  async refresh(){
    this.lastError=null;
    await Promise.allSettled([this.loadModel().then(()=>this.loadProblem()),this.loadCAE(),this.loadInteractions()]);
    const capabilities=await this.refreshWorkspaceCapabilitySummary().catch(()=>null);
    if(!capabilities)await this.refreshCouplingCatalog().catch(()=>{});
    window.ImplexityDesignFreedom?.refreshAvailability?.();
    this.syncObjectiveCatalogue();this.renderStatus();this.renderProviders();this.renderReadiness();this.normaliseLegacySurfaces();this.refreshEngineeringAgent();window.dispatchEvent(new CustomEvent("implexity:workbench-refreshed"));
  }
  async loadModel(){try{this.model=await json("/v1/implicit/model")}catch(_){try{this.model=await json("/v1/model")}catch(e){this.lastError={summary:"The current model could not be loaded.",technical:e}}}}
  async loadProblem(){
     
    if(this.model&&this.model.loaded===false){this.problem=null;delete this.providerProblems.legacy_multiphysics_implicit;return;}
    try{
      const d=await json("/v1/implicit/problem");
      const persistence=window.ImplexityProviderProblemPersistence;
      const loaded=persistence?.unpack?persistence.unpack(d):{providerId:"legacy_multiphysics_implicit",problem:d.problem||d,record:null};
      this.providerProblems[loaded.providerId]=this.clone(loaded.problem);
      if(loaded.record){
        this.providerProblemRecords[loaded.providerId]=this.clone(loaded.record);
        window.ImplexityProviderProblemRegistry=window.ImplexityProviderProblemRegistry||new Map();
        window.ImplexityProviderProblemRegistry.set(loaded.providerId,this.clone(loaded.problem));
        this._selectedProvider=loaded.providerId;store.set("implexity.provider",loaded.providerId);
      }
      this.problem=loaded.providerId==="legacy_multiphysics_implicit"?this.clone(loaded.problem):null;
    }catch(_){this.problem=null;delete this.providerProblems.legacy_multiphysics_implicit}
  }
  async loadCAE(){
    this._providerLoad={status:"loading",error:null};this.renderProviders();this.renderStatus();
    try{
      this.cae=await json("/v1/implicit/cae/catalogue");this._providerLoad={status:"ready",error:null};
      this.syncCouplingCatalog();
      window.dispatchEvent(new CustomEvent("implexity:provider-catalogue-changed",{detail:{providers:this.providerEntries().map(row=>row.id||row.name)}}));
      if(this.lastError?.source==="providers")this.lastError=null;
    }catch(e){
      this.cae=null;this._providerLoad={status:"error",error:e};
      this.syncCouplingCatalog();
      this.lastError={source:"providers",summary:"The physics catalogue could not be loaded.",technical:e};
    }
  }
  async loadInteractions(){try{this.interactions=await json("/v1/implicit/interactions")}catch(_){this.interactions=null}}
  refreshModelSoon(){clearTimeout(this._refreshTimer);this._refreshTimer=setTimeout(()=>this.refresh(),180)}
  async reloadProviders(){
    await this.loadCAE();await this.refreshWorkspaceCapabilitySummary().catch(()=>null);this.renderProviders();this.renderStatus();this.renderReadiness();this.syncObjectiveCatalogue();
  }

  setStep(step,{initial=false}={}){
    if(!STEP_BY_ID[step])return;this.step=step;store.set("implexity.step",step);
    if(!initial&&step!=="geometry"){
      this._paneState.right=true;
      if(this._compactMode)this._paneState.left=false;
      this.persistPaneState();this.applyPaneState();
    }
    document.body.dataset.implexityStep=step;
    qa("[data-implexity-step]",this.rail).forEach(b=>{const current=b.dataset.implexityStep===step;b.tabIndex=current?0:-1;if(current)b.setAttribute("aria-current","step");else b.removeAttribute("aria-current");});
    const meta=STEP_BY_ID[step];q("[data-implexity-context-title]",this.context).textContent=meta.label;
    q("[data-implexity-context-hint]",this.context).textContent=meta.hint;


    this.manageConditionsGuide();this.focusNative(step,{initial});this.prepareInteractionStep();this.manageObjectivePanel();this.manageSensitivityPanel();this.renderStatus();
    if(!initial){this.announce(`${meta.label} workflow stage`);window.dispatchEvent(new CustomEvent("implexity:presentation-changed",{detail:{kind:"workflow-step",step}}));}
  }

  manageConditionsGuide(){
    let guide=q("#implexity-conditions-guide");
    if(!guide){const anchor=q("#s_engsys");if(!anchor)return;guide=node("section","implexity-eng-card");guide.id="implexity-conditions-guide";guide.setAttribute("aria-label","How to assign a physical condition");anchor.before(guide);}
    guide.hidden=this.step!=="conditions";if(guide.hidden)return;
    guide.replaceChildren(node("h3","","Assign an area and its condition"));
    const steps=node("ol","");
    ["Choose the physics first: it determines which conditions and selectors are supported.","Select the area: use a surface region on the model, or a domain face in the flow setup.","Choose the role and enter values with units: for example heat flux [W/m²], temperature [K], or inlet flow [m³/s].", "Review the assignment, then validate and apply it in the provider setup. Creating a region alone does not apply a load."].forEach(text=>steps.append(node("li","",text)));
    guide.append(steps);
    const active=this.providerEntries().find(p=>(p.id||p.name)===this.currentProvider());
    const setup=node("button","",active?"Open condition setup":"Choose physics");setup.type="button";
    setup.addEventListener("click",()=>{this.setStep("physics");const selected=this.providerEntries().find(p=>(p.id||p.name)===this.currentProvider());if(selected)this.configureProvider(selected);else this.note("Choose a physics provider below, then use Open setup to assign its regions, boundary roles and values.");});guide.append(setup);
    if(active){const choose=node("button","","Change physics provider");choose.type="button";choose.addEventListener("click",()=>this.setStep("physics"));guide.append(choose);}
    const region=node("button","","Select a surface region");region.type="button";
    region.addEventListener("click",()=>{const tool=q('.implexity-tool[data-mode="surface_patch"]');if(tool&&!tool.disabled){tool.click();this.note("Select the surface area in the viewport, then review its name and extent in the tool. Apply a physical condition separately in the provider setup.");}else this.note("Surface selection is unavailable for this model. Open the provider setup to choose a supported region selector.");});guide.append(region);
    guide.append(node("p","implexity-eng-help","Geometry-only models have no physical inlet, outlet or heat boundary until a physics problem is configured. Flow-face controls and surface-region controls are different selectors; use the one advertised by your provider."));
  }

  focusNative(step,{initial=false}={}){
    if(!document.body.classList.contains("dens-everything")){
      const keep={geometry:new Set(["#s_tree","#s_params"]),conditions:new Set(["#s_engsys"]),physics:new Set(["#s_engsys"]),objectives:new Set(["#s_opt","#s_obj"]),optimize:new Set(["#s_opt"]),results:new Set(["#s_opt","#s_terms"])}[step]||new Set();
      for(const selector of ["#s_tree","#s_params","#s_engsys","#s_opt","#s_obj","#s_terms"]){if(!keep.has(selector))this.closeSection(selector);}
    }
    const open=(sel)=>this.openSection(sel);
    if(step==="geometry"){open("#s_tree");open("#s_params");}
    if(step==="conditions"||step==="physics"){open("#s_engsys");}
    if(step==="objectives"){open("#s_opt");open("#s_obj");}
    if(step==="optimize"){open("#s_opt");}
    if(step==="results"){open("#s_opt");open("#s_terms");}
    if(initial&&step!=="conditions")return;
    const hasResults=Boolean(window.OPT?.rows?.length);
    const targets={geometry:q("#s_tree"),conditions:q("#implexity-conditions-guide")||q("#s_engsys"),physics:q("#s_engsys"),objectives:q("#s_obj"),optimize:q("#s_opt"),results:hasResults?q("#s_terms"):q("#s_opt")};
    const t=targets[step];if(t){
      t.classList.remove("implexity-focus-flash");void t.offsetWidth;t.classList.add("implexity-focus-flash");
      const pane=t.closest?.(".pane")||q("#implexityInspectorPane");
      if(pane?.scrollTo&&pane.getBoundingClientRect&&t.getBoundingClientRect){
        const top=Math.max(0,pane.scrollTop+t.getBoundingClientRect().top-pane.getBoundingClientRect().top);
        pane.scrollTo({top,behavior:"auto"});
      }else t.scrollIntoView?.({block:"start",behavior:"auto"});
    }
  }

  openSection(sel){
    const sec=q(sel);if(!sec||sectionIsOpen(sec))return;
    if(window.implexitySetSectionOpen?.(sec,true))return;
    sec.classList.add("open");sec.classList.remove("shut");
    const head=q(":scope > .sect-h",sec);if(head)head.setAttribute("aria-expanded","true");
  }

  closeSection(sel){
    const sec=q(sel);if(!sec||!sectionIsOpen(sec))return;
    if(window.implexitySetSectionOpen?.(sec,false))return;
    sec.classList.remove("open");sec.classList.add("shut");
    const head=q(":scope > .sect-h",sec);if(head)head.setAttribute("aria-expanded","false");
  }

  prepareInteractionStep(){
    const toolbar=q(".implexity-toolbar");if(!toolbar)return;
    const ctx=q(".implexity-tool-context",toolbar);const labels={geometry:["Geometry tools","surface drag · field brush · control lattice · cage"],conditions:["Conditions","surface regions · loads · boundary conditions"],physics:["Physics inspection","select engineering objects in the viewport"],objectives:["Objective inspection","select geometry or result regions"],optimize:["Optimisation","manual editing remains available between runs"],results:["Result-guided editing","select sensitivity regions or return them to the topology field"]};
    const pair=labels[this.step]||labels.geometry;if(ctx){const a=q("strong",ctx),b=q("span",ctx);if(a&&a.textContent!==pair[0])a.textContent=pair[0];if(b&&b.textContent!==pair[1])b.textContent=pair[1];}
    const allowed={
      geometry:new Set(["select","selection_box","selection_lasso","move","size","surface","geometry_sculpt","field_brush","control_lattice","deformation_cage"]),
      conditions:new Set(["select","surface_patch","pressure","traction","heat_flux","temperature","clamp"]),
      physics:new Set(["select"]),objectives:new Set(["select"]),optimize:new Set(["select","selection_box","selection_lasso","field_brush"]),results:new Set(["select","selection_box","selection_lasso","field_brush"])
    }[this.step]||new Set(["select"]);
    qa(".implexity-tool",toolbar).forEach(b=>{const visible=String(allowed.has(b.dataset.mode));if(b.dataset.implexityVisible!==visible)b.dataset.implexityVisible=visible;});
    const ctrl=window.ImplexityInteraction;
    if(ctrl&&ctrl.mode&&!allowed.has(ctrl.mode))ctrl.setMode("select");
  }

  manageObjectivePanel(){
    const panel=q(".implexity-objective-panel");if(!panel)return;
    const composer=window.ImplexityObjectiveComposer;
    const shouldOpen=this.step==="objectives";
    if(composer?.setOpen)composer.setOpen(shouldOpen,{focus:false});
    else if(shouldOpen||!panel.matches(":focus-within")){
      panel.hidden=!shouldOpen;
      const toggle=q(".implexity-objective-toggle");if(toggle)toggle.setAttribute("aria-expanded",String(shouldOpen));
    }
    this.updateObjectiveCompatibility();
  }
  manageSensitivityPanel(){
    const s=q("#implexity-sensitivity-authoring");if(!s)return;
    if(!["optimize","results"].includes(this.step))s.classList.add("collapsed");
    this.syncSensitivityButton();
  }

  installOptimizationAdapter(){
    const existing=window.ImplexityOptimization||{};
    if(typeof existing.setResponseProgram!=="function"){
      existing.setResponseProgram=program=>this.applyResponseProgram(program);
      existing.__implexityOwned=true;
    }
    window.ImplexityOptimization=existing;
  }

  clone(value){return value==null?value:JSON.parse(JSON.stringify(value))}

  applyResponseProgram(program){
    if(!program||!Array.isArray(program.objectives))throw new Error("The response program is malformed.");
    if(program.normalisation!==undefined&&program.normalisation!=="response_scale")throw new Error("Only per-response scale normalisation is supported.");
    program={...program,objectives:program.objectives.filter(row=>row.enabled!==false),constraints:(program.constraints||[]).filter(row=>row.enabled!==false)};
    const provider=this.currentProvider();
    if(!program.objectives.length)throw new Error("At least one objective is required.");
    if(program.provider_id!==undefined&&program.provider_id!==provider)throw new Error("The response program belongs to a different physics provider.");
    if(provider!=="legacy_multiphysics_implicit"){
      const entry=this.providerEntries().find(x=>(x.id||x.name)===provider)||{};
      const available=new Set((entry.responses||[]).map(value=>String(typeof value==="string"?value:(value.response||value.id||""))));
      const responseMetadata=entry.response_metadata||entry.responseMetadata||{};
      const responseLabel=id=>human(id,responseMetadata[id]||{});
      const providerLabel=friendlyProvider(provider,entry);
      if(entry.sensitivities===false)throw new Error(`${providerLabel} does not provide optimisation sensitivities.`);
      const objectives=program.objectives||[],constraints=program.constraints||[];
      const all=[...objectives,...constraints];
      for(const [index,row] of all.entries()){
        if(!available.has(row.response_id))throw new Error(`Response ${index+1}: ${responseLabel(row.response_id)} is not supplied by ${providerLabel}.`);
        const descriptor=(entry.responses||[]).find(value=>typeof value==="object"&&(value.response||value.id)===row.response_id);
        if((descriptor?.differentiable??responseMetadata[row.response_id]?.differentiable)===false)throw new Error(`${responseLabel(row.response_id)} does not provide a derivative for optimisation.`);
        if(Object.hasOwn(row,"enforcement")||Object.hasOwn(row,"tolerance"))throw new Error(`${responseLabel(row.response_id)} carries enforcement/tolerance fields. Hard constraints were removed from Implexity; response bounds are penalty terms only. Remove these fields and review the penalty weight.`);
        if(Object.keys(row).some(key=>!["id","enabled","response_id","sense","relation","target","bound","weight","scale"].includes(key)))throw new Error("Response rows contain unsupported fields; no constraint flag may be silently discarded.");
        const isConstraint=index>=objectives.length;const sense=String(isConstraint?(row.relation||"<="):(row.sense||"minimize")).toLowerCase();
        if(!["minimize","minimise","maximize","maximise","target","upper","lower","equal","<=",">=","="].includes(sense))
          throw new Error(`${responseLabel(row.response_id)} uses an unsupported differentiable direction (${human(sense)}).`);
        const finite=value=>typeof value==="number"&&Number.isFinite(value);
        if(!finite(row.scale??1)||(row.scale??1)<=0)throw new Error(`${responseLabel(row.response_id)} requires a finite positive response scale.`);
        if(!finite(row.weight??1)||(row.weight??1)<0)throw new Error(`${responseLabel(row.response_id)} requires a finite non-negative weight.`);
        if(isConstraint&&!finite(row.bound))throw new Error(`${responseLabel(row.response_id)} requires a finite constraint bound.`);
        if(!isConstraint&&["target","upper","lower","equal","<=",">=","="].includes(sense)&&!finite(row.target))throw new Error(`${responseLabel(row.response_id)} requires a finite target.`);
      }
      this.appliedResponseProgram=this.clone(program);this._responseProgramDirty=false;this._dirty=true;this.lastError=null;
      this.note(`${all.length} differentiable response${all.length===1?"":"s"} bound to ${friendlyProvider(provider)} through the mature implicit job lifecycle`);
      this.renderReadiness();this.updateObjectiveCompatibility();
      return {ok:true,provider,objectives:(program.objectives||[]).length,constraints:(program.constraints||[]).length};
    }
    if((program.constraints||[]).length)throw new Error("The LEGACY_MULTIPHYSICS implicit job runtime does not yet accept response constraints. Use its native volume/bound constraints; no constraint was silently discarded.");
    const catalogue=window.S?.objcat?.terms||[];
    const mapped=program.objectives.map((row,index)=>{
      const term=catalogue.find(item=>item.term===row.response_id);
      const label=human(row.response_id,term||{});
      if(!term)throw new Error(`Objective ${index+1}: ${label} is not an LEGACY_MULTIPHYSICS objective term.`);
      const sense=String(row.sense||"minimize").toLowerCase();
      if(sense==="target")throw new Error(`${label}: target sense is not losslessly representable by the LEGACY_MULTIPHYSICS term runtime.`);
      if(!["minimize","minimise","maximize","maximise"].includes(sense))throw new Error(`${label}: unsupported direction (${human(sense)}).`);
      const scale=row.scale??1,rawWeight=row.weight??1;
      if(typeof scale!=="number"||!Number.isFinite(scale)||scale<=0||typeof rawWeight!=="number"||!Number.isFinite(rawWeight)||rawWeight<0)throw new Error(`${label}: weight must be finite and non-negative; scale must be finite and positive.`);
      const knobs={};
      for(const knob of (term.knobs||[]))if(knob.name!=="weight"&&knob.default!=null)knobs[knob.name]=knob.default;
      const sign=(sense==="maximize"||sense==="maximise")?-1:1;
      return {term:row.response_id,weight:sign*rawWeight/scale,knobs};
    });
    if(!window.S||typeof window.renderObjective!=="function")throw new Error("The mature implicit objective runtime is not available in this GUI.");
    window.S.objective=mapped;window.renderObjective();
    this.appliedResponseProgram=this.clone(program);this._responseProgramDirty=false;this._dirty=true;this.lastError=null;
    this.note(`${mapped.length} response objective${mapped.length===1?"":"s"} applied to the mature implicit job runtime`);
    this.renderReadiness();this.updateObjectiveCompatibility();
    return {ok:true,provider,objectives:mapped.length,constraints:0};
  }

  providerProblem(){
    const provider=this.currentProvider();
    if(!provider)return null;
    if(provider==="intent_orchestrated"){
       
       
      const authored=this.providerProblems[provider];
      if(authored&&!this.latestPhysicsPlan)return this.clone(authored);
      const plan=this.latestPhysicsPlan||{};const selected=Array.isArray(plan.selected_addins)?plan.selected_addins:[];
      const providerProblems={...(authored?.context?.provider_problems||{}),...this.providerProblems};
      delete providerProblems.intent_orchestrated;
      return {intent:this.clone(this.latestPhysicsIntent||{}),context:{...this.clone(authored?.context||{}),provider_problems:this.clone(providerProblems)}};
    }
    if(provider==="resolved_stokes_brinkman"&&this.cfd?.problem)return this.clone(this.cfd.problem);
    return this.clone(this.providerProblems[provider]||(provider==="legacy_multiphysics_implicit"?this.problem:null));
  }

  markPhysicsDraft(id,dirty=true){
    if(!id)return;
    if(dirty)this._physicsDrafts.add(id);else this._physicsDrafts.delete(id);
    this.invalidateRunAuthorization(dirty?"Physics settings have unapplied changes.":"Physics draft resolved; fresh preflight is required.");
    this.renderReadiness();
    window.dispatchEvent(new CustomEvent("implexity:physics-draft-state",{detail:{providerId:id,dirty}}));
  }

  buildOptimizationRequest(base){
    if(this._physicsDrafts.size)throw new Error("Apply or discard the pending physics changes before optimisation.");
    if(this._responseProgramDirty)throw new Error("Objectives or constraints have unapplied changes. Apply the response program before optimisation.");
    let provider=this.currentProvider();
    if(!provider)throw new Error("Load suitable physics add-ins before optimisation. Manual modelling remains available.");
    const topology=this.topologyState();
    if(!topology.available)throw new Error(topology.action);
    const autoPlan=this.latestPhysicsPlan;
    if(!this._selectedProvider&&autoPlan?.status==="ready")provider="intent_orchestrated";
    if(provider==="legacy_multiphysics_implicit"){
      if(window.ImplexityDesignFreedom?.isConfigured?.())throw new Error("Hierarchical topology stages are not supported by the LEGACY_MULTIPHYSICS parametric job runtime. Select a compatible topology physics provider or clear the saved hierarchy; it will not be silently ignored.");
      if(!topology.sourceRef)throw new Error("The editable topology field has no authoritative model binding. Repeat the topology hand-off before optimisation.");
      const existing=(Array.isArray(base.free)?base.free:[]).filter(item=>{
        const name=typeof item==="string"?item:String(item?.ref||item?.parameter||item?.name||"");
        return name!=="model:control"&&name!==topology.sourceRef;
      });
      const topologyFree={ref:topology.sourceRef,lo:Number(topology.lower??0),hi:Number(topology.upper??1)};
      return {...base,free:[...existing,topologyFree],physics_generation:window.ImplexityPhysicsPackages?.generation};
    }
    const program=this.appliedResponseProgram;
    if(!program?.objectives?.length)throw new Error(`Apply at least one ${friendlyProvider(provider)} objective before starting optimisation.`);
    const problem=this.providerProblem();
    if(!problem)throw new Error(`Complete the ${friendlyProvider(provider)} physics setup before starting optimisation.`);
    const mapSense=s=>({minimize:"minimise",maximize:"maximise","<=":"upper",">=":"lower","=":"equal"}[String(s||"minimize").toLowerCase()]||String(s||"minimise").toLowerCase());
    const rows=[];
    for(const row of (program.objectives||[]))rows.push({response:row.response_id,sense:mapSense(row.sense),weight:Number(row.weight??1),scale:Number(row.scale??1),target:row.target??null});
    for(const row of (program.constraints||[]))rows.push({response:row.response_id,sense:mapSense(row.relation),weight:Number(row.weight??1),scale:Number(row.scale??1),target:Number(row.bound)});
    const hierarchy=window.ImplexityDesignFreedom?.isConfigured?.()?(window.ImplexityDesignFreedom.getDeclaration()||{}):{};
    const coordinateSelection=this.resolveDesignCoordinateSelection({providerId:provider,hierarchy,base,topology});
    return {...base,physics_generation:window.ImplexityPhysicsPackages?.generation,provider,physics:{provider,problem},responses:rows,free:[],objective:undefined,steerable:false,topologyAlwaysFree:true,
      design_coordinates:coordinateSelection.rows,inactive_design_coordinates:coordinateSelection.inactive,design_coordinate_selection:coordinateSelection.selection,update_metric:hierarchy.update_metric||base.update_metric||{mode:"gradient_adaptive",alpha:0.5},design_freedom:hierarchy.design_freedom||undefined,schedule:hierarchy.schedule||undefined};
  }

  appliedObjectiveCount(){
    if(this._responseProgramDirty)return 0;
    if(this.appliedResponseProgram?.objectives?.length)return this.appliedResponseProgram.objectives.length;
    return Array.isArray(window.S?.objective)?window.S.objective.length:0;
  }

  topologyState(){
    const loaded=this.model?.loaded!==false&&!!this.model;
    const action="Bake or hand off the parametric geometry to one editable cell-grid topology field before starting direct-gradient optimisation.";
    if(!loaded)return {available:false,label:"Missing",detail:"load or create an implicit model",action};
    const document=this.model?.document||this.model||{};
    const nodes=document.nodes&&typeof document.nodes==="object"?document.nodes:{};
    const root=String(document.root||"");
    const declared=String(document.meta?.implexity?.topology?.ref||"").trim();
    if(declared==='model:control'&&['lattice.controlled','lattice.controlled_assembly'].includes(nodes[root]?.kind)){
      const key=nodes[root]?.params?.control?.array,shape=document.arrays?.[key]?.shape;
      const meta=document.meta.implexity.topology;
      const lower=meta.lower,upper=meta.upper;
      const volumes=nodes[root]?.kind==='lattice.controlled_assembly'?nodes[root]?.attrs?.volumes?.length:1;
      if(!Number.isInteger(volumes)||volumes<1||!Array.isArray(shape)||shape.length!==4||shape[0]!==20*volumes||shape.slice(1).some(n=>!Number.isInteger(n)||n<2))
        return {available:false,label:'Invalid native controls',detail:'native control tensor shape is invalid',action:'Validate the native geometry control tensor.'};
      if(!window.ImplexityDesignBounds?.valid(lower,upper,shape))
        return {available:false,label:'Native bounds required',detail:'finite scalar or exact-shape component bounds are required',action:'Declare finite ordered bounds for the native control tensor. Density bounds are not a substitute.'};
      return {available:true,label:'Native controls',detail:'native geometry coordinate retained; solver preflight is still required',action:'',sourceRef:declared,lower:this.clone(lower),upper:this.clone(upper),shape:[...shape],nativeComponents:true};
    }
    const credibleParameter=name=>["samples","control","occupancy","density"].includes(String(name||"").toLowerCase());
    const reachable=new Map();
    const visit=(id,path=[])=>{id=String(id||"");if(!id||reachable.has(id)||!nodes[id])return;reachable.set(id,path);for(const [index,child] of (nodes[id].children||[]).entries()){const next=typeof child==="string"?child:child?.node;const name=typeof child==="object"&&child?.name?String(child.name):String(index);visit(next,[...path,name])}};
    visit(root,[]);
    const resolveDeclared=ref=>{
      const match=/^model(?:\/([^:]+))?:([^:]+)$/.exec(ref);if(!match||!credibleParameter(match[2]))return null;
      let id=root;for(const segment of (match[1]?match[1].split("/").filter(Boolean):[])){const current=nodes[id];const child=(current?.children||[]).find(item=>typeof item==="object"&&String(item.name||"")===segment);if(!child)return null;id=String(child.node||"");}
      return nodes[id]?.kind==="cell_grid_field"&&reachable.has(id)?{id,parameter:match[2]}:null;
    };
    if(declared){
      if(resolveDeclared(declared))return {available:true,label:"Ready",detail:"the declared editable topology field is shared by manual tools and optimisation",action:"",sourceRef:declared,lower:Number(document.meta?.implexity?.topology?.lower??0),upper:Number(document.meta?.implexity?.topology?.upper??1)};
      return {available:false,label:"Invalid hand-off",detail:"the declared topology reference does not resolve to an editable cell-grid field",action};
    }
    const candidates=[...reachable.keys()].filter(id=>nodes[id]?.kind==="cell_grid_field");
    if(candidates.length===1){const id=candidates[0],parameter=["samples","control","occupancy","density"].find(name=>Object.hasOwn(nodes[id]?.params||{},name));const path=reachable.get(id)||[];if(parameter){const sourceRef=`model${path.length?`/${path.join("/")}`:""}:${parameter}`;return {available:true,label:"Ready",detail:"one editable cell-grid topology field was detected for the shared design",action:"",sourceRef,lower:0,upper:1};}}
    if(candidates.length>1)return {available:false,label:"Choose topology",detail:"several cell-grid fields exist; select the one shared by manual tools and optimisation",action};
    return {available:false,label:"Bake required",detail:"parametric geometry remains editable, but needs an explicit cell-grid topology hand-off",action};
  }

  readinessState(){
    const loaded=this.model?.loaded!==false&&!!this.model;
    const free=Number(this.model?.parameters?.filter?.(p=>p.free).length ?? window.S?.params?.filter?.(p=>p.free).length ?? 0);
    const provider=this.currentProvider();const providerEntry=this.providerEntries().find(x=>(x.id||x.name)===provider);
    const autoReady=provider!=="intent_orchestrated"||this.latestPhysicsPlan?.status==="ready"||(!this.latestPhysicsPlan&&Boolean(this.providerProblemRecords[provider]));
    const physics=Boolean(this.providerProblem()&&providerEntry&&autoReady&&!this._physicsDrafts.size&&providerEntry.traits?.evaluation_only!==true&&providerEntry.sensitivities!==false);
    const objectives=this.appliedObjectiveCount();
    const topology=this.topologyState();
    const complete=window.ImplexityGuidedSetup?.active?.request;
    const completeResponseCount=Array.isArray(complete?.responses)?complete.responses.length:0;
     
    const hierarchyError=complete?"":window.ImplexityDesignFreedom?.validationError?.()||"";
    return {loaded,free,provider,providerEntry,physics,objectives,completeResponseCount,topology,hierarchyError,physicsPlan:this.latestPhysicsPlan,ready:loaded&&topology.available&&physics&&(objectives>0||completeResponseCount>0)&&!hierarchyError};
  }

  nextReadinessAction(state=this.readinessState()){
    if(!state.loaded)return {action:"geometry",label:"Load or create geometry"};
    if(!state.topology.available)return {action:"handoff",label:"Hand off to topology editing"};
    if(state.hierarchyError)return {action:"hierarchy",label:"Resolve optimisation scope"};
    if(!state.physics)return {action:"physics",label:"Configure physics"};
    if(!(state.objectives>0||state.completeResponseCount>0))return {action:"objectives",label:"Apply an objective"};
    return {action:"preflight",label:"Preflight current design"};
  }

  updateObjectiveCompatibility(){
    const panel=q(".implexity-objective-panel");if(!panel)return;
    let note=q(".implexity-objective-compat",panel);
    if(!note){note=node("div","implexity-objective-compat");const body=q(".implexity-objective-body",panel)||panel;body.prepend(note)}
    const provider=this.currentProvider();const legacy_multiphysics=provider==="legacy_multiphysics_implicit";
    note.dataset.kind=legacy_multiphysics?"limited":"provider";
    const providerEntry=this.providerEntries().find(item=>(item.id||item.name)===provider)||{};
    const heading=legacy_multiphysics?"LEGACY_MULTIPHYSICS job-runtime compatibility":provider==="intent_orchestrated"?"Automatic multiphysics orchestration":friendlyProvider(provider,providerEntry);
    const copy=legacy_multiphysics?"Objective terms are applied directly to the mature implicit optimiser. Response constraints are not accepted by this execution path and are never silently dropped.":provider==="intent_orchestrated"?"Implexity selects compatible installed add-ins from the requested responses, validates their coupled feedback and exact topology derivative paths, then uses the same mature direct-gradient job lifecycle.":"Responses and exact adjoints run through the same mature implicit job lifecycle as manual model editing, live visualisation, accept and discard.";
    note.replaceChildren(node("b","",heading),node("span","",copy));
    const add=q(".implexity-objective-add.constraint button",panel);
    if(add){add.disabled=legacy_multiphysics;add.title=legacy_multiphysics?"LEGACY_MULTIPHYSICS response constraints are not yet supported by the mature job runtime":"Add a differentiable provider constraint"}
    qa(".implexity-objective-entry.constraint",panel).forEach(row=>row.dataset.implexityIncompatible=String(legacy_multiphysics));
    panel.dataset.implexityProgramState=this._responseProgramDirty?"draft":this.appliedResponseProgram?"applied":(this._dirty?"draft":"empty");
  }

  providerEntries(){const raw=this.cae?.providers||{};const entries=Array.isArray(raw)?raw:Object.entries(raw).map(([id,v])=>({id,...(v||{})}));return entries.map(p=>({...p,editor:p.editor||p.traits?.editor})).filter(p=>(p.id||p.name)!=="intent_orchestrated"||(p.responses||[]).length>0);}
  syncCouplingCatalog(){
    const provider=this.currentProvider();
    const entry=this.providerCouplingCatalogs[provider]||this.providerEntries().find(row=>(row.id||row.name)===provider)||null;
    return this.computationEffort?.setCouplingCatalog(entry);
  }
  async refreshCouplingCatalog(provider=this.currentProvider()){
    if(!provider)return null;
    const problem=this.providerProblems[provider]||(provider==="legacy_multiphysics_implicit"?this.problem:null);
    if(!problem){this.syncCouplingCatalog();return null;}
    const response=await json("/v1/agent/action",{method:"POST",body:JSON.stringify({action:"inspect_couplings",payload:{provider,problem}})});
    const catalogue=response?.result||response;
    const rows=Array.isArray(catalogue?.providers)?catalogue.providers:[];
    const entry=rows.find(row=>String(row?.provider_id||"")===provider);
    if(!entry)throw new Error(`The ${friendlyProvider(provider)} provider did not return its coupling catalogue.`);
    this.providerCouplingCatalogs[provider]=this.clone(entry);
    if(provider===this.currentProvider())this.syncCouplingCatalog();
    return entry;
  }
  async inspectWorkspaceCapabilities(provider=this.currentProvider(),problem=this.providerProblem()){
    if(!provider)throw new Error("Select an installed physics provider before inspecting workspace capabilities.");
    const payload={provider};
    if(problem&&typeof problem==="object"&&!Array.isArray(problem))payload.problem=this.clone(problem);
    else payload.use_current_problem=true;
    const response=await json("/v1/agent/action",{method:"POST",body:JSON.stringify({action:"inspect_workspace_capabilities",payload})});
    const manifest=workspaceCapabilityManifest(response,provider);
    this.providerWorkspaceCapabilities=this.providerWorkspaceCapabilities||{};this.providerCouplingCatalogs=this.providerCouplingCatalogs||{};
    this.providerWorkspaceCapabilities[provider]=this.clone(manifest);
    if(manifest.coupling_control&&typeof manifest.coupling_control==="object")this.providerCouplingCatalogs[provider]=this.clone(manifest.coupling_control);
    if(provider===this.currentProvider())this.syncCouplingCatalog();
    window.dispatchEvent(new CustomEvent("implexity:workspace-capabilities-changed",{detail:{providerId:provider,manifest:this.clone(manifest)}}));
    return manifest;
  }
  async refreshWorkspaceCapabilitySummary(provider=this.currentProvider(),problem=this.providerProblem(),target=q("#implexity-provider-capabilities"),renderOptions={}){
    if(!target||!provider)return null;
    const serial=++this._workspaceCapabilitySerial;target.hidden=false;target.dataset.provider=provider;renderWorkspaceCapabilities(target,{state:"loading",provider,...renderOptions});
    try{
      const manifest=await this.inspectWorkspaceCapabilities(provider,problem);
      if(serial!==this._workspaceCapabilitySerial||this.currentProvider()!==provider||target.dataset.provider!==provider)return null;
      renderWorkspaceCapabilities(target,{state:"ready",manifest,provider,...renderOptions});return manifest;
    }catch(error){
      if(serial!==this._workspaceCapabilitySerial||this.currentProvider()!==provider||target.dataset.provider!==provider)return null;
      renderWorkspaceCapabilities(target,{state:"error",error,provider,...renderOptions});return null;
    }
  }
  async applyProviderCoupling(coupling,state){
    if(this._physicsDrafts.size)throw new Error("Apply or discard the physics draft before changing couplings.");
    const provider=this.currentProvider(),configuration=coupling?.configuration;
    if(!provider||!configuration||configuration.kind!=="engineering_problem"||configuration.action!=="set_engineering_problem")throw new Error("This coupling is not editable through the engineering problem.");
    if(!Array.isArray(coupling.allowedStates)||!coupling.allowedStates.includes(state))throw new Error("The provider does not allow that coupling state.");
    const path=String(configuration.field||"").split(".");
    if(!path.length||path.some(part=>!part||part==="__proto__"||part==="prototype"||part==="constructor"))throw new Error("The provider published an unsafe coupling field path.");
    const problem=this.providerProblems[provider];if(!problem)throw new Error("Store the provider problem before changing its couplings.");
    const originalProblem=JSON.stringify(problem),draft=this.clone(problem);let target=draft;
    for(const part of path.slice(0,-1)){const value=target[part];if(value==null)target=target[part]={};else if(typeof value!=="object"||Array.isArray(value))throw new Error("The provider coupling field conflicts with the stored problem.");else target=value;}
    target[path.at(-1)]=state;
    const root=String(configuration.selection_root||"").trim();
    if(configuration.selection_schema&&root){
      if(!draft[root]||typeof draft[root]!=="object"||Array.isArray(draft[root]))draft[root]={};
      draft[root].schema=String(configuration.selection_schema);
    }
    const persistence=window.ImplexityProviderProblemPersistence;if(!persistence?.save||!persistence?.captureModel)throw new Error("Provider problem persistence is unavailable.");
    const expectedModel=await persistence.captureModel();
    if(this.currentProvider()!==provider||JSON.stringify(this.providerProblems[provider])!==originalProblem)throw new Error("The selected physics problem changed. Refresh its couplings before applying this selection.");
    const record=await persistence.save({providerId:provider,problem:draft,expectedModel,provenance:{source:"native_gui",editor:"coupling_catalogue",operation:"provider_owned_selection",coupling_id:String(coupling.id),requested_state:state}});
    this.providerProblems[provider]=this.clone(record.problem);this.providerProblemRecords[provider]=this.clone(record);
    window.ImplexityProviderProblemRegistry=window.ImplexityProviderProblemRegistry||new Map();window.ImplexityProviderProblemRegistry.set(provider,this.clone(record.problem));
    window.dispatchEvent(new CustomEvent("implexity:provider-problem-changed",{detail:{providerId:provider,problem:this.clone(record.problem),record:this.clone(record)}}));
    await this.refreshCouplingCatalog(provider);void this.refreshWorkspaceCapabilitySummary(provider,record.problem);this.note(`${coupling.label} set to ${couplingStateLabel(state)}`);this.renderReadiness();return record;
  }
  providerDesignCoordinates(providerId=this.currentProvider()){
    if(providerId==null||providerId==="")return [];
    const entry=this.providerEntries().find(row=>(row.id||row.name)===providerId);
    if(!entry)throw new Error("Select an installed physics provider before configuring design coordinates.");
    const coordinates=entry.design_coordinates||entry.designCoordinates;
    if(!Array.isArray(coordinates)||(!coordinates.length&&entry.traits?.evaluation_only!==true)||coordinates.some(name=>typeof name!=="string"||!name.trim())||new Set(coordinates).size!==coordinates.length)
      throw new Error(`${friendlyProvider(providerId,entry)} has not declared a valid design-coordinate set.`);
    return coordinates.map(name=>String(name));
  }
  providerDesignCoordinateDefaults(providerId=this.currentProvider(),problem=this.providerProblem()){
    if(providerId==null||providerId==="")return [];
    const entry=this.providerEntries().find(row=>(row.id||row.name)===providerId)||{};
    const coordinates=this.providerDesignCoordinates(providerId);
    const declaration=entry.traits?.design_variable_default||entry.traits?.designVariableDefault||entry.design_variable_default||{};
    const raw=Array.isArray(declaration.bindings)?declaration.bindings:Array.isArray(entry.default_design_bindings)?entry.default_design_bindings:[];
    const byCoordinate=new Map();
    for(const value of raw){
      if(!value||typeof value!=="object")throw new Error(`${friendlyProvider(providerId,entry)} supplied an invalid default design binding.`);
      const coordinate=String(value.coordinate||value.name||"").trim();
      if(!coordinate||byCoordinate.has(coordinate)||!coordinates.includes(coordinate))throw new Error(`${friendlyProvider(providerId,entry)} supplied an invalid or duplicate default binding for ${human(coordinate||"unknown coordinate")}.`);
      byCoordinate.set(coordinate,value);
    }
    const problemDesign=problem?.design&&typeof problem.design==="object"?problem.design:{};
    const topology=this.topologyState();
    const selected=window.ImplexityDesignBounds?.mappedControl(problem)?coordinates.filter(name=>name==='model:control'):coordinates;
    if(!selected.length)throw new Error('The geometry map requires the model:control coordinate from its provider.');
    return selected.map(coordinate=>{
      const declared=byCoordinate.get(coordinate)||{};
      const problemBinding=problemDesign.bindings?.[coordinate];
      const problemBounds=problemDesign.bounds?.[coordinate]||{};
      const topologyCoordinate=coordinate==="model:control"&&topology.available;
      const topologyBinding=topologyCoordinate?topology.sourceRef:"";
      const ref=String(topologyBinding||declared.ref||problemBinding||"").trim();
      const lower=topologyCoordinate?this.clone(topology.lower):this.clone(declared.lower??declared.lo??problemBounds.lower??problemBounds.lo??0);
      const upper=topologyCoordinate?this.clone(topology.upper):this.clone(declared.upper??declared.hi??problemBounds.upper??problemBounds.hi??1);
      window.ImplexityDesignBounds.validate(lower,upper,topologyCoordinate?topology.shape:null);
      const rawStepScale=declared.step_scale??declared.stepScale??1;
      if(typeof rawStepScale!=="number"||!Number.isFinite(rawStepScale)||rawStepScale<=0)throw new Error(`${friendlyProvider(providerId,entry)} supplied a non-positive or non-finite step scale for ${human(coordinate)}.`);
      const step_scale=rawStepScale;
      return {coordinate,ref,lower,upper,step_scale,binding_source:topologyBinding?"authoritative_topology":declared.ref?"provider_default":problemBinding?"provider_problem":"missing"};
    });
  }
  resolveDesignCoordinateSelection({providerId=this.currentProvider(),hierarchy={},base={},topology=this.topologyState()}={}){
    const declared=this.providerDesignCoordinates(providerId);
    const defaults=this.providerDesignCoordinateDefaults(providerId,this.providerProblem());
    const defaultByName=new Map(defaults.map(row=>[row.coordinate,row]));
    const configured=Boolean(window.ImplexityDesignFreedom?.isConfigured?.());
    const baseExplicit=Object.prototype.hasOwnProperty.call(base||{},"design_coordinates");
    const raw=configured?(hierarchy.design_coordinates||[]):baseExplicit?base.design_coordinates:defaults;
    if(!Array.isArray(raw)||!raw.length)throw new Error("At least one provider-declared design coordinate must be explicitly selected.");
    const seen=new Set();
    const rows=raw.map(value=>{
      const supplied=typeof value==="string"?{coordinate:value}:{...(value||{})};
      const coordinate=String(supplied.coordinate||supplied.name||"").trim();
      if(!coordinate||seen.has(coordinate))throw new Error("Active design coordinates must be unique non-empty provider identifiers.");
      if(!declared.includes(coordinate))throw new Error(`${friendlyProvider(providerId)} does not declare ${human(coordinate)} as a design coordinate.`);
      seen.add(coordinate);
      const fallback=defaultByName.get(coordinate)||{};
      const ref=String(supplied.ref||fallback.ref||(coordinate==="model:control"&&topology.available?topology.sourceRef:"")).trim();
      if(!ref)throw new Error(`${human(coordinate)} is active but has no authoritative model-parameter binding. Bind it explicitly before optimisation.`);
      const lower=this.clone(supplied.lower??supplied.lo??fallback.lower),upper=this.clone(supplied.upper??supplied.hi??fallback.upper);
      window.ImplexityDesignBounds.validate(lower,upper,coordinate==='model:control'?topology.shape:null);
      if(window.ImplexityDesignBounds.mappedControl(this.providerProblem())&&coordinate!=='model:control')throw new Error('Mapped physical outputs are derived from model:control and cannot be optimized independently.');
      const step_scale=supplied.step_scale??supplied.stepScale??fallback.step_scale??1;
      if(typeof step_scale!=="number"||!Number.isFinite(step_scale)||step_scale<=0)throw new Error(`${human(coordinate)} requires a positive finite step scale.`);
      const row={coordinate,ref,lower,upper,step_scale};
      if(Object.prototype.hasOwnProperty.call(supplied,"designable"))row.designable=supplied.designable;
      if(supplied.designable_ref)row.designable_ref=String(supplied.designable_ref);
      const selectionIds=(Array.isArray(supplied.designable_selection_ids)?supplied.designable_selection_ids:(supplied.designable_selection_id?[supplied.designable_selection_id]:[])).map(String);
      if(selectionIds.length){if(selectionIds.some(id=>!id.trim())||new Set(selectionIds).size!==selectionIds.length)throw new Error(`${human(coordinate)} designable regions must have unique non-empty IDs.`);row.designable_selection_ids=selectionIds;row.combine=String(supplied.combine||"union");if(!["union","intersection"].includes(row.combine))throw new Error(`${human(coordinate)} region combination must be union or intersection.`);}
      return row;
    });
    const active=rows.map(row=>row.coordinate),inactive=declared.filter(coordinate=>!seen.has(coordinate));
    return {rows,inactive,selection:{source:configured||baseExplicit?"explicit_gui":window.ImplexityDesignBounds?.mappedControl(this.providerProblem())?"authoritative_geometry_map":"all_provider_declared",explicit:configured||baseExplicit,provider_declared:declared,active,inactive}};
  }
  syncObjectiveCatalogue(){
    const composer=window.ImplexityObjectiveComposer;if(!composer)return;
    const provider=this.currentProvider();
    if(provider==="legacy_multiphysics_implicit"){composer.loadCatalogue?.();return;}
    composer._catalogueSequence=(composer._catalogueSequence||0)+1;
    const entry=this.providerEntries().find(x=>(x.id||x.name)===provider)||{};
    const metadata=entry.response_metadata||entry.responseMetadata||{};
    const previousCatalogue=JSON.stringify(composer.catalogue);
    composer.catalogue=(entry.responses||[]).map(value=>{const id=String(typeof value==="string"?value:(value.response||value.id||"")),meta={...(metadata[id]||{}),...(typeof value==="object"?value:{})},descriptor=present(id,meta);return {id,label:descriptor.label,units:descriptor.unit,description:descriptor.description,module:meta.owner_addin||meta.ownerAddin||provider,differentiable:meta.differentiable!==false&&entry.sensitivities!==false,availability:meta.availability||"available",required_component:meta.required_component||null,scale:Number(meta.scale||1)};}).filter(item=>item.id);
    composer.status.textContent=`${composer.catalogue.length} differentiable ${friendlyProvider(provider,entry)} responses available`;
    if(previousCatalogue!==JSON.stringify(composer.catalogue))composer.render();
  }
  buildSensitivityRequest(base){
    const topology=this.topologyState();
    if(!topology.available)throw new Error(topology.action);
    const provider=this.currentProvider();if(provider==="legacy_multiphysics_implicit"){
      if(!topology.sourceRef)throw new Error("The editable topology field has no authoritative model binding. Repeat the topology hand-off before evaluating sensitivity.");
      const existing=(Array.isArray(base.free)?base.free:[]).filter(item=>{
        const name=typeof item==="string"?item:String(item?.ref||item?.parameter||item?.name||"");
        return name!=="model:control"&&name!==topology.sourceRef;
      });
      return {...base,free:[...existing,{ref:topology.sourceRef,lo:Number(topology.lower??0),hi:Number(topology.upper??1)}],physics_generation:window.ImplexityPhysicsPackages?.generation};
    }
    const program=base.response_program||this.appliedResponseProgram;const objective=program?.objectives?.find(row=>row.enabled!==false);
    if(!objective)throw new Error("Apply an objective before evaluating provider sensitivity.");
    const hierarchy=window.ImplexityDesignFreedom?.isConfigured?.()?(window.ImplexityDesignFreedom.getDeclaration()||{}):{};
    const coordinateSelection=this.resolveDesignCoordinateSelection({providerId:provider,hierarchy,base,topology});
    return {provider,physics:{provider,problem:this.providerProblem()},responses:[{response:objective.response_id,sense:objective.sense||"minimize",weight:Number(objective.weight??1),scale:Number(objective.scale??1),target:objective.target??null}],response:objective.response_id,stream:true,design_coordinates:coordinateSelection.rows,inactive_design_coordinates:coordinateSelection.inactive,design_coordinate_selection:coordinateSelection.selection};
  }
  currentProvider(){
    const entries=this.providerEntries();
    if(this._selectedProvider&&entries.some(x=>(x.id||x.name)===this._selectedProvider))return this._selectedProvider;
    if(entries.some(x=>(x.id||x.name)==="intent_orchestrated"))return "intent_orchestrated";
    return entries.length?(entries[0].id||entries[0].name):null;
  }

  renderStatus(){
    if(!this.status)return;const id=this.currentProvider();const p=this.providerEntries().find(x=>(x.id||x.name)===id);
    const providerState=this._providerLoad?.status||"idle";
    const label=providerState==="loading"?"Loading…":providerState==="error"?"Catalogue unavailable":friendlyProvider(p?.id||p?.name||id,p||{});const err=this.lastError;
    const loaded=this.model?.loaded!==false&&!!this.model;
    const design=node("span",`implexity-status-chip ${loaded?"ok":"warn"}`);design.append(node("i",""),node("b","","Design"),document.createTextNode(loaded?"Ready":"Empty"));
    const topologyState=this.topologyState();const topology=node("span",`implexity-status-chip ${topologyState.available?"ok":"warn"}`);topology.append(node("i",""),node("b","","Topology"),document.createTextNode(topologyState.label));
    const physics=node("span","implexity-status-chip implexity-status-provider");physics.append(node("b","","Physics"),document.createTextNode(label));
    const truthKind=!loaded?"absent":this._dirty?"stale":"authoritative";
    const truth=node("span",`implexity-status-chip implexity-status-truth ${truthKind==="authoritative"?"ok":"warn"}`);truth.dataset.truthStatus=truthKind;truth.append(node("i",""),node("b","","Setup"),document.createTextNode(truthKind==="authoritative"?"Current":truthKind==="stale"?"Unsaved changes":"Empty"));
    const message=node("div",`implexity-status-message ${err?"bad":""}`);
    if(err)renderMessage(message,typeof err==="object"?err.summary:"The requested operation could not be completed.",typeof err==="object"?err.technical:err);
    else message.textContent=String(this._note||(this._dirty?"Unsaved workflow changes":""));
    this.status.replaceChildren(design,topology,physics,truth,message);
  }

  renderProviders(){
    const legacy=q("#engsysout");
    const selected=this.currentProvider();
    const legacyResponses=q("#s_refuse");
    if(legacyResponses){legacyResponses.hidden=selected!=="legacy_multiphysics_implicit";legacyResponses.inert=legacyResponses.hidden;}
    if(legacy){legacy.hidden=selected!=="legacy_multiphysics_implicit";const intro=legacy.previousElementSibling;if(intro?.classList.contains("sub"))intro.hidden=legacy.hidden;}
    const entry=this.providerEntries().find(p=>(p.id||p.name)===selected);
    document.querySelectorAll('[data-sum="engsys"]').forEach(el=>{el.textContent=selected?friendlyProvider(selected,entry||{}):"No physics configured";});
    const host=q("#implexity-provider-list");if(!host)return;const entries=this.providerEntries(),state=this._providerLoad?.status||"idle";host.replaceChildren();
    host.setAttribute("aria-live","polite");host.setAttribute("aria-atomic","true");host.setAttribute("aria-busy",String(state==="loading"));
    if(state==="loading"){
      host.dataset.loadState="loading";
      const item=node("div","implexity-provider-state loading"),copy=node("div","");item.setAttribute("role","status");
      copy.append(node("strong","","Loading installed physics add-ins…"),node("span","","Manual geometry editing remains available while the catalogue is checked."));item.append(node("span","implexity-provider-state-mark"),copy);host.append(item);return;
    }
    if(state==="error"){
      host.dataset.loadState="error";
      const item=node("div","implexity-provider-state error"),copy=node("div","");
      copy.append(node("strong","","Physics add-ins could not be loaded"),node("span","","This is a service or connection error, not an empty installation. Manual geometry editing remains available."));
      const retry=node("button","implexity-provider-retry","Try again");retry.type="button";retry.addEventListener("click",()=>this.reloadProviders());
      item.append(node("span","implexity-provider-state-mark"),copy,retry);host.append(item);return;
    }
    if(!entries.length){
      host.dataset.loadState="empty";
      const item=node("div","implexity-provider-state empty"),copy=node("div","");item.setAttribute("role","status");
      copy.append(node("strong","","No active physics provider"),node("span","","Manual geometry editing remains available. Activate a suitable installed add-in using Manage physics add-ins above."));item.append(node("span","implexity-provider-state-mark"),copy);host.append(item);return;
    }
    host.dataset.loadState="ready";
    const active=this.currentProvider();
    for(const p of entries){
      const id=p.id||p.name;const row=node("article","implexity-provider");row.dataset.active=String(id===active);row.dataset.provider=id;
      const header=node("div","implexity-provider-main");const title=node("strong","",friendlyProvider(id,p));
      const technical=node("details","implexity-provider-technical"),technicalSummary=node("summary","","Technical details"),code=node("code","implexity-provider-id",id);code.dataset.machineToken=id;technical.append(technicalSummary,code);header.append(title,technical);
      const analyses=(p.analyses||[]).map(human).slice(0,5);
      const caps=node("div","implexity-provider-caps");
      const coordinateCount=Array.isArray(p.design_coordinates)?p.design_coordinates.length:0;
      [p.traits?.evaluation_only===true?"evaluation only":p.execution==="implicit_job"?"full job lifecycle":p.execution==="array"?"array-based provider":"named-design provider",p.sensitivities===true?"derivatives available":p.sensitivities===false?"state only":"derivatives undeclared",p.traits?.experimental===true?"experimental":null,p.distributable?"distributable plug-in":null,coordinateCount?`${coordinateCount} declared design coordinate${coordinateCount===1?"":"s"}`:p.traits?.evaluation_only===true?"authored case · no topology input":"design space undeclared",...analyses].filter(Boolean).forEach(x=>caps.append(node("span","",x)));
      const note=node("p","",p.description||(p.notes||[])[0]||"Registered differentiable CAE provider");
      const text=node("div","implexity-provider-copy");text.append(header,caps,note);
      const b=node("button","implexity-provider-action",id===active?"Open setup":"Use provider");b.type="button";b.setAttribute("aria-label",`${id===active?"Open setup for":"Use"} ${friendlyProvider(id,p)}`);b.addEventListener("click",()=>this.configureProvider(p));
      row.append(text,b);host.append(row);
    }
  }

  configureProvider(p){
    if(this._activeNativeEditor?.dirty?.()){
      if(!window.confirm("The physics editor has unapplied changes. Discard that draft and open the requested setup?"))return;
      this._activeNativeEditor.discard();
    }
    const id=p.id||p.name,previous=this.currentProvider(),changed=id!==previous;
    const existing=this.appliedResponseProgram||window.ImplexityObjectiveComposer?.specification?.();
    const hasProviderChoices=Object.keys(window.ImplexityObjectiveComposer?.providerChoiceSpecification?.().provider_overrides||{}).length>0;
    if(changed&&(existing?.objectives?.length||existing?.constraints?.length||hasProviderChoices)&&!window.confirm("Changing the physics provider clears the current objectives, constraints and optional provider choices. Save them first if you need them later. Continue?"))return;
    this._selectedProvider=id;store.set("implexity.provider",id);
    if(changed){this.appliedResponseProgram=null;const composer=window.ImplexityObjectiveComposer;composer?.clearProviderChoices?.();composer?.setProgram?.({schema:"implexity-response-program/2",objectives:[],constraints:[],normalisation:"response_scale"});}
    if(changed)window.dispatchEvent(new CustomEvent("implexity:provider-selection-changed",{detail:{providerId:id,previousProviderId:previous}}));
    this.syncCouplingCatalog();this.syncObjectiveCatalogue();this.renderProviders();this.renderStatus();this.renderReadiness();const editor=p.editor||{};
    const capabilityProblem=this.providerProblems[id]||(id==="legacy_multiphysics_implicit"?this.problem:null);
    const capabilityTask=editor.kind==="native_json"?null:this.refreshWorkspaceCapabilitySummary(id,capabilityProblem);
    if(editor.kind==="legacy_engineering_dialog"){
      const b=q(`#${editor.action||"implexityEngineeringOpen"}`);if(b){b.click();this.note(`${friendlyProvider(id)} setup opened`);return;}
      this.note(`${friendlyProvider(id)} editor is unavailable in this surface`);return;
    }
    if(editor.kind==="embedded_cfd"){this.attachCFD(window[editor.global||"ImplexityCFDWorkspace"]||this.cfdClass,true);return;}
    if(editor.kind==="native_json"||p.traits?.imported===true){this.openNativeJsonEditor(p);return;}
    const modularEditor=window.ImplexityProviderEditors?.get?.(id);
    if(modularEditor?.open){modularEditor.open({providerId:id,problem:this.providerProblem(),capabilityManifest:this.providerWorkspaceCapabilities?.[id]||null,capabilityPromise:capabilityTask});this.note(`${friendlyProvider(id)} setup opened`);return;}
    this.note(`${friendlyProvider(id)} is registered, but did not declare a GUI editor`);
  }

  openNativeJsonEditor(p){
    const id=p.id||p.name,host=q("#implexity-physics-editor");if(!host)return;
    host.hidden=false;host.replaceChildren();
    let schema=p.editor?.schema||p.problem_schema||null;
    const title=node("h3","",p.editor?.title||schema?.title||friendlyProvider(id));
    const note=node("p","",p.traits?.evaluation_only===true?"Evaluate the authored case below. The viewport geometry is not automatically included, and this provider cannot optimise it. Use the add-in's declared inputs and validity limits, including a geometry snapshot where supported.":"Complete the guided physical setup below. Materials, loads, boundary conditions and histories remain owned and validated by the selected physics add-in.");
    const authoringFields=p.editor?.authoring_fields;
    const authoringAliases=Array.isArray(p.editor?.authoring_aliases)?p.editor.authoring_aliases:[];
    const hasPath=(value,path)=>Array.isArray(path)&&path.every(key=>{
      if(!value||typeof value!=="object"||!Object.hasOwn(value,key))return false;
      value=value[key];return true;
    });
    const hiddenAuthoringAlias=path=>authoringAliases.some(alias=>
      JSON.stringify(alias.path)===JSON.stringify(path)&&hasPath(draft,alias.source));
    const form=value=>Array.isArray(authoringFields)?Object.fromEntries(authoringFields.filter(key=>Object.hasOwn(value,key)).map(key=>[key,this.clone(value[key])])):this.clone(value);
    const seeded=form(this.providerProblems[id]||p.editor?.problem_template||p.problem_template||nativeProblemFromSchema(schema)||{});
    let expectedProblemId=this.providerProblemRecords[id]?.problem_id;
    let draft=this.clone(seeded),rawDirty=false,applying=false;
    const dirty=()=>this.markPhysicsDraft(id,true);
    const sharedCapabilityHost=q("#implexity-provider-capabilities"),capabilityHost=sharedCapabilityHost||node("section","implexity-capability-summary");capabilityHost.dataset.provider=id;capabilityHost.setAttribute("aria-label",`${friendlyProvider(id)} capabilities`);
    let applyTemplatePatch=null,applyCapabilitySchema=null;
    const loadCapabilities=context=>{const requested=JSON.stringify(context);return this.refreshWorkspaceCapabilitySummary(id,context,capabilityHost,{onApplyTemplate:template=>{
      if(!applyTemplatePatch)throw new Error("The guided provider editor is not ready to stage a template.");
      return applyTemplatePatch(template);
    }}).then(manifest=>{if(manifest&&applyCapabilitySchema)applyCapabilitySchema(manifest,requested);return manifest;});};
    const capabilityPromise=loadCapabilities(draft);
    const guided=node("div","implexity-native-guided");guided.setAttribute("aria-label",`${friendlyProvider(id)} guided setup`);
    guided.addEventListener("input",dirty);guided.addEventListener("change",dirty);
    guided.addEventListener("click",event=>{if(event.target.closest("button"))dirty();});
    const controls=[];
    const rawDetails=node("details","implexity-native-expert");rawDetails.dataset.tier="expert";
    const rawSummary=node("summary","","Expert: edit the complete provider declaration");
    const rawNote=node("p","","Advanced declarations use the add-in's exact field names and JSON structure. After editing JSON, load it into the guided controls or validate and apply it directly.");
    const raw=node("textarea","implexity-native-problem-json");raw.rows=16;raw.spellcheck=false;
    raw.setAttribute("aria-label",`${friendlyProvider(id)} expert problem declaration`);
    raw.value=JSON.stringify(draft,null,2);
    const status=node("p","");status.setAttribute("role","status");status.setAttribute("aria-live","polite");
    const setAtPath=(root,path,value)=>{let target=root;for(let index=0;index<path.length-1;index++)target=target[path[index]];target[path[path.length-1]]=value;};
    const pathLabel=path=>path.map((part,index)=>nativeSchemaDescriptor(part,nativeSchemaAtPath(schema,path.slice(0,index+1))).label).join(": ");
    const addScalar=(parent,key,value,path,fieldSchema=nativeSchemaAtPath(schema,path))=>{
      const descriptor=nativeSchemaDescriptor(key,fieldSchema),label=node("label","implexity-native-field");
      const caption=node("span","implexity-native-label",descriptor.label+(descriptor.unit?` (${descriptor.unit})`:""));
      let input,type;
      if(descriptor.choices){
        type="enum";input=node("select","implexity-native-input");input.dataset.problemEnum=JSON.stringify(descriptor.choices);
        const selected=descriptor.choices.findIndex(choice=>JSON.stringify(choice)===JSON.stringify(value));
        if(selected<0){const option=node("option","","Choose an allowed value");option.value="";input.append(option);input.value="";}
        descriptor.choices.forEach((choice,index)=>{const text=choice===null?"Not set":typeof choice==="boolean"?(choice?"Yes":"No"):String(choice);const option=node("option","",text);option.value=String(index);input.append(option);});
        if(selected>=0)input.value=String(selected);
      }else if(typeof value==="boolean"){
        type="boolean";input=node("select","implexity-native-input");
        for(const [choice,text] of [["true","Yes"],["false","No"]]){const option=node("option","",text);option.value=choice;input.append(option);}
        input.value=String(value);
      }else{
        input=node("input","implexity-native-input");
        if(typeof value==="number"){type="number";input.type="number";input.step=descriptor.multipleOf==null?(descriptor.integer?"1":"any"):String(descriptor.multipleOf);input.required=true;input.value=String(value);if(descriptor.minimum!=null)input.min=String(descriptor.minimum);if(descriptor.maximum!=null)input.max=String(descriptor.maximum);}
        else{type=value===null?"nullable":"string";input.type="text";input.value=value==null?"":String(value);if(value===null)input.placeholder="Not set";}
      }
      if(descriptor.minimum!=null)input.dataset.problemMinimum=String(descriptor.minimum);if(descriptor.maximum!=null)input.dataset.problemMaximum=String(descriptor.maximum);
      if(descriptor.exclusiveMinimum!=null)input.dataset.problemExclusiveMinimum=String(descriptor.exclusiveMinimum);if(descriptor.exclusiveMaximum!=null)input.dataset.problemExclusiveMaximum=String(descriptor.exclusiveMaximum);
      if(descriptor.integer)input.dataset.problemInteger="true";
      if(descriptor.multipleOf!=null)input.dataset.problemMultipleOf=String(descriptor.multipleOf);
      input.dataset.problemPath=JSON.stringify(path);input.dataset.problemType=type;
      input.setAttribute("aria-label",pathLabel(path));
      input.oninput=()=>{rawDirty=false;status.textContent="Guided changes are ready to validate and apply.";};
      input.onchange=input.oninput;
      label.append(caption,input);
      const range=descriptor.minimum==null&&descriptor.maximum==null?"":`Allowed range: ${descriptor.minimum??"−∞"} to ${descriptor.maximum??"∞"}${descriptor.unit?` ${descriptor.unit}`:""}.`;
      const strictRange=[descriptor.exclusiveMinimum==null?"":`Must be greater than ${descriptor.exclusiveMinimum}.`,descriptor.exclusiveMaximum==null?"":`Must be less than ${descriptor.exclusiveMaximum}.`].filter(Boolean).join(" ");
      if(descriptor.description||range||strictRange)label.append(node("small","implexity-native-help",[descriptor.description,range,strictRange].filter(Boolean).join(" ")));
      parent.append(label);controls.push(input);
    };
    const addListEntryControl=(parent,path,fieldSchema)=>{
      const template=fieldSchema?.items?.default;
      if(!template||typeof template!=="object"||Array.isArray(template)||Array.isArray(fieldSchema?.prefixItems))return;
      const button=node("button","","Add entry");button.type="button";
      button.setAttribute("aria-label",`Add ${pathLabel(path)} entry`);
      button.onclick=()=>{
        try{
          if(rawDirty||applying||button.disabled||button.isConnected===false)throw new Error("Finish the current editor operation before adding an entry.");
          const next=readGuided();let list=next;for(const part of path)list=list[part];
          if(!Array.isArray(list))throw new Error("This section is not a list.");
          if(Number.isInteger(fieldSchema.maxItems)&&list.length>=fieldSchema.maxItems)throw new Error(`This declaration allows at most ${fieldSchema.maxItems} entries.`);
          list.push(finiteJsonCopy(template));draft=next;raw.value=JSON.stringify(draft,null,2);renderGuided();
          status.textContent="Provider starter entry added to the draft. Review its face, values and history length before validating.";
          loadCapabilities(draft).catch(error=>{if(guided.isConnected!==false)renderMessage(status,"The entry was added, but updated provider labels could not be loaded.",error);});
        }catch(error){renderMessage(status,"The entry was not added.",error);}
      };
      parent.append(button);
    };
    const addOptionalFieldControl=(parent,path,fieldSchema,value)=>{
      const allowed=!path.length&&Array.isArray(authoringFields)?new Set(authoringFields):null;
      const {missing,removable}=nativeOptionalFields(fieldSchema,value);
      const offered=missing.filter(row=>!allowed||allowed.has(row.key)),dropped=removable.filter(row=>!allowed||allowed.has(row.key));
      if(!offered.length&&!dropped.length)return;
      const where=path.length?pathLabel(path):"problem";
      const row=node("div","implexity-native-optional");row.dataset.optionalPath=JSON.stringify(path);
      const edit=(mutate,message)=>{
        try{
          if(rawDirty||applying||row.isConnected===false)throw new Error("Finish the current editor operation before changing optional fields.");
          const next=readGuided();let target=next;for(const part of path)target=target[part];
          if(!target||typeof target!=="object"||Array.isArray(target))throw new Error("This section changed. Reopen the editor before editing its optional fields.");
          mutate(target);draft=next;raw.value=JSON.stringify(draft,null,2);renderGuided();dirty();
          status.textContent=message;
          loadCapabilities(draft).catch(error=>{if(guided.isConnected!==false)renderMessage(status,"The draft was changed, but updated provider labels could not be loaded.",error);});
        }catch(error){renderMessage(status,"The optional field was not changed.",error);}
      };
      if(offered.length){
        const select=node("select","implexity-native-input"),add=node("button","","Add optional field");add.type="button";
        select.setAttribute("aria-label",`Optional field to add to ${where}`);select.dataset.optionalAdd=JSON.stringify(path);
        offered.forEach((entry,index)=>{const option=node("option","",`${entry.label} (${entry.key})`);option.value=String(index);option.title=entry.description;select.append(option);});
        const help=node("small","implexity-native-help","");
        const describe=()=>{help.textContent=offered[Number(select.value)]?.description||"Inserted with the add-in's declared starter value.";};
        select.onchange=describe;describe();
        add.setAttribute("aria-label",`Add optional field to ${where}`);
        add.onclick=()=>{const entry=offered[Number(select.value)];if(!entry)return;
          edit(target=>{if(Object.prototype.hasOwnProperty.call(target,entry.key))throw new Error("The field is already present.");target[entry.key]=finiteJsonCopy(entry.starter);},
            `${entry.label} added with the add-in's declared starter value. Review it, then validate and apply.`);};
        row.append(node("span","implexity-native-label","Optional fields"),select,add,help);
      }
      if(dropped.length){
        const select=node("select","implexity-native-input"),remove=node("button","","Remove optional field");remove.type="button";
        select.setAttribute("aria-label",`Optional field to remove from ${where}`);select.dataset.optionalRemove=JSON.stringify(path);
        dropped.forEach((entry,index)=>{const option=node("option","",`${entry.label} (${entry.key})`);option.value=String(index);select.append(option);});
        remove.setAttribute("aria-label",`Remove optional field from ${where}`);
        remove.onclick=()=>{const entry=dropped[Number(select.value)];if(!entry)return;
          edit(target=>{delete target[entry.key];},`${entry.label} removed from the draft. Validate and apply to save the change.`);};
        row.append(select,remove);
      }
      parent.append(row);
    };
    const addValue=(parent,key,value,path,override=null)=>{
      if(hiddenAuthoringAlias(path))return;
      const fieldSchema=override||nativeSchemaAtPath(schema,path),descriptor=nativeSchemaDescriptor(key,fieldSchema);
      const variants=nativeObjectVariants(fieldSchema,value);
      if(variants){
        const group=node("fieldset","implexity-native-group"),label=node("label","implexity-native-field"),choice=node("select","implexity-native-input");
        group.append(node("legend","",descriptor.label));
        choice.setAttribute("aria-label",`${pathLabel(path)} condition type`);
        choice.setAttribute("data-variant-path",JSON.stringify(path));
        if(variants.selected<0){const option=node("option","","Custom declaration: use Expert to edit");option.value="";choice.append(option);}
        variants.options.forEach((entry,index)=>{const option=node("option","",entry.label);option.value=String(index);choice.append(option);});
        choice.value=variants.selected<0?"":String(variants.selected);
        label.append(node("span","implexity-native-label","Condition type"),choice);group.append(label);
        group.append(node("small","implexity-native-help",descriptor.description||"Changing type replaces this section with the add-in's starter values. Validate before applying."));
        choice.onchange=()=>{
          try{
            if(rawDirty||applying||choice.isConnected===false)throw new Error("Finish the current editor operation before changing condition type.");
            const index=choice.value.trim()===""?NaN:Number(choice.value);
            if(!Number.isInteger(index)||index<0||index>=variants.options.length)throw new Error("Choose a declared condition type.");
            const next=readGuided();setAtPath(next,path,finiteJsonCopy(variants.options[index].template));
            draft=next;raw.value=JSON.stringify(draft,null,2);renderGuided();
            qa("select[data-variant-path]",guided).find(input=>input.getAttribute("data-variant-path")===JSON.stringify(path))?.focus();
            status.textContent="Condition type changed in the draft. Check its starter values, then validate and apply.";
          }catch(error){choice.value=variants.selected<0?"":String(variants.selected);renderMessage(status,"The condition type was not changed.",error);}
        };
        if(variants.selected>=0){
          const fields=variants.options[variants.selected].properties||{};
          for(const [childKey,child] of Object.entries(value||{}))if(childKey!==variants.key)addValue(group,childKey,child,[...path,childKey],fields[childKey]||{});
        }
        parent.append(group);return;
      }
      if(fieldSchema?.format==="json"){
        const label=node("label","implexity-native-field"),input=node("textarea","implexity-native-problem-json");
        input.rows=5;input.spellcheck=false;input.value=JSON.stringify(value);input.dataset.problemPath=JSON.stringify(path);input.dataset.problemType="json_value";input.setAttribute("aria-label",pathLabel(path));
        input.oninput=()=>{rawDirty=false;status.textContent="JSON field changes are ready for provider validation.";};
        label.append(node("span","implexity-native-label",descriptor.label),node("small","implexity-native-help",descriptor.description||"Enter JSON; physical shape and values are checked by the provider."),input);
        const snapshotOptions=fieldSchema["x-model-snapshot"];
        if(Array.isArray(snapshotOptions)&&value?.model&&typeof value.node==="string"){
          const details=node("details","implexity-native-expert"),selected=snapshotOptions.find(option=>option?.template&&Object.entries(option.template).every(([key,item])=>JSON.stringify(value[key])===JSON.stringify(item)));
          parent.append(node("p","implexity-native-help",`${selected?.label||"Captured model"}: ${String(value.model.name||"Unnamed model")} · node ${value.node}`));
          details.append(node("summary","","Inspect or edit snapshot JSON"),label);parent.append(details);
        }else parent.append(label);
        controls.push(input);
        if(Array.isArray(snapshotOptions)&&snapshotOptions.length<=8){
          for(const option of snapshotOptions){
            if(!option||typeof option.label!=="string"||!option.label.trim())continue;
            const capture=node("button","",option.label);capture.type="button";
            capture.onclick=async()=>{
              const initialDraft=draft,initialRaw=raw.value,initialValues=controls.map(control=>control.value);
              try{
                if(rawDirty||applying||capture.disabled||capture.isConnected===false)throw new Error("Finish the current editor operation before capturing geometry.");
                capture.disabled=true;
                const state=await json("/v1/implicit/model");
                if(rawDirty||applying||draft!==initialDraft||raw.value!==initialRaw||capture.isConnected===false||controls.some((control,index)=>control.value!==initialValues[index]))throw new Error("The setup changed during capture. Capture again to update the current draft.");
                const next=readGuided();setAtPath(next,path,authoredModelSnapshot(state,option.template));
                draft=next;raw.value=JSON.stringify(draft,null,2);renderGuided();
                status.textContent="Stored whole-model snapshot captured into this draft. Review the grid and any explicit mask, then validate. Later geometry edits will not change this snapshot.";
              }catch(error){renderMessage(status,"The model snapshot was not changed.",error);}
              finally{capture.disabled=false;}
            };
            parent.append(capture);
          }
        }
        return;
      }
      if(Array.isArray(value)&&value.length===0){
        const label=node("label","implexity-native-field"),input=node("textarea","implexity-native-problem-json");
        input.rows=4;input.spellcheck=false;input.value="[]";input.dataset.problemPath=JSON.stringify(path);input.dataset.problemType="json_array";input.setAttribute("aria-label",pathLabel(path));
        input.oninput=()=>{rawDirty=false;status.textContent="List changes are ready to validate and apply.";};
        label.append(node("span","implexity-native-label",descriptor.label),node("small","implexity-native-help",descriptor.description||"Empty list. Paste the add-in's declared list entries here; keep [] to leave it empty. Validate and apply checks the entries before saving."),input);
        parent.append(label);controls.push(input);addListEntryControl(parent,path,fieldSchema);return;
      }
      const numericCount=nativeNumericArrayCount(value);
      if(numericCount!==null&&numericCount>16){
        const label=node("label","implexity-native-field"),input=node("textarea","implexity-native-problem-json");
        input.rows=8;input.spellcheck=false;input.value=JSON.stringify(value);input.dataset.problemPath=JSON.stringify(path);input.dataset.problemType="numeric_array";input.setAttribute("aria-label",pathLabel(path));
        input.dataset.problemArraySchema=JSON.stringify(fieldSchema||{});
        input.oninput=()=>{rawDirty=false;status.textContent="Array changes are ready to validate and apply.";};
        label.append(node("span","implexity-native-label",descriptor.label+(descriptor.unit?` (${descriptor.unit})`:"")),node("small","implexity-native-help",`${numericCount} numeric values. Edit or paste a JSON array, retaining the add-in's ordering and dimensions. Provider validation checks the physical shape.`),input);
        if(descriptor.description)label.append(node("small","implexity-native-help",descriptor.description));
        const fileLabel=node("label","implexity-native-field"),fileInput=node("input","implexity-native-input");fileInput.type="file";fileInput.accept=".json,application/json";fileInput.setAttribute("aria-label",`Import ${pathLabel(path)} numeric array`);
        fileLabel.append(node("span","implexity-native-label","Import numeric array from JSON"),fileInput);
        fileInput.onchange=async()=>{
          const file=fileInput.files?.[0];if(!file)return;
          const previous=input.value,initialDraft=draft;
          try{
            if(rawDirty||applying||input.disabled)throw new Error("Finish the current editor operation before importing an array.");
            const loaded=JSON.parse(await file.text());
            if(nativeNumericArrayCount(loaded)===null)throw new Error("The file must contain a JSON array of finite numbers.");
            const issue=nativeNumericArraySchemaError(loaded,fieldSchema);if(issue)throw new Error(issue);
            if(rawDirty||applying||input.disabled||input.isConnected===false||draft!==initialDraft||input.value!==previous)throw new Error("The setup changed while the file loaded. Import again to replace the current value.");
            input.value=JSON.stringify(loaded);input.oninput();status.textContent="Numeric array imported as a draft. Validate and apply to check its physical dimensions and save it.";
          }catch(error){renderMessage(status,"Array import did not replace the current value.",error);}
          finally{fileInput.value="";}
        };
        parent.append(label,fileLabel);controls.push(input);return;
      }
      if(value&&typeof value==="object"){
        const group=node("fieldset","implexity-native-group"),legend=node("legend","",descriptor.label+(descriptor.unit?` (${descriptor.unit})`:""));
        group.append(legend);
        if(descriptor.description)group.append(node("p","implexity-native-help",descriptor.description));
        const entries=Array.isArray(value)?value.map((item,index)=>[index,item]):Object.entries(value);
        if(entries.length&&entries.every(([childKey])=>hiddenAuthoringAlias([...path,childKey])))return;
        if(!entries.length)group.append(node("p","implexity-native-empty","No guided values are declared in this section. The physics add-in can provide them through its template or expert declaration."));
        for(const [childKey,child] of entries){
          if(Array.isArray(value)&&numericCount===null){
            const item=node("div","implexity-native-list-item"),remove=node("button","","Remove entry");
            remove.type="button";remove.setAttribute("aria-label",`Remove ${pathLabel([...path,childKey])}`);
            const minimumItems=Number.isInteger(fieldSchema?.minItems)&&fieldSchema.minItems>=0?fieldSchema.minItems:0;
            remove.disabled=value.length<=minimumItems;
            if(remove.disabled)remove.title=`This declaration requires at least ${minimumItems} entries.`;
            remove.onclick=()=>{
              try{
                if(rawDirty||applying||remove.isConnected===false)throw new Error("Finish the current editor operation before removing an entry.");
                const next=readGuided();let list=next;for(const part of path)list=list[part];
                if(!Array.isArray(list)||childKey>=list.length)throw new Error("The list changed. Reopen the editor before removing this entry.");
                if(list.length<=minimumItems)throw new Error(`This declaration requires at least ${minimumItems} entries.`);
                const nextSchema=this.clone(schema),listSchema=nativeSchemaAtPath(nextSchema,path);
                if(Array.isArray(listSchema?.prefixItems))listSchema.prefixItems.splice(childKey,1);
                list.splice(childKey,1);draft=next;schema=nextSchema;raw.value=JSON.stringify(draft,null,2);renderGuided();
                status.textContent="Entry removed from the draft. Validate and apply to check references and save the change.";
                loadCapabilities(draft).catch(error=>{if(guided.isConnected!==false)renderMessage(status,"The draft was changed, but updated provider labels could not be loaded.",error);});
              }catch(error){renderMessage(status,"The entry was not removed.",error);}
            };
            addValue(item,childKey,child,[...path,childKey]);item.append(remove);group.append(item);
          }else addValue(group,childKey,child,[...path,childKey],fieldSchema?.properties?.[childKey]||(Array.isArray(value)?fieldSchema?.prefixItems?.[childKey]??fieldSchema?.items:null));
        }
        if(Array.isArray(value))addListEntryControl(group,path,fieldSchema);
        else addOptionalFieldControl(group,path,fieldSchema,value);
        parent.append(group);return;
      }
      addScalar(parent,key,value,path,fieldSchema);
    };
    const renderGuided=()=>{
      guided.replaceChildren();controls.length=0;
      const entries=draft&&typeof draft==="object"&&!Array.isArray(draft)?Object.entries(draft):[];
      if(!entries.length)guided.append(node("div","implexity-native-empty","This add-in has not supplied guided starter values yet. Load its declared problem template or use the Expert declaration; nothing incomplete will be applied silently."));
      for(const [key,value] of entries)addValue(guided,key,value,[key]);
      if(draft&&typeof draft==="object"&&!Array.isArray(draft))addOptionalFieldControl(guided,[],schema,draft);
    };
    applyTemplatePatch=template=>{
      if(applying)throw new Error("Wait for the current problem validation and save to finish before applying a template.");
      const current=rawDirty?JSON.parse(raw.value):readGuided();
      if(!current||Array.isArray(current)||typeof current!=="object")throw new Error("The physical problem must be a JSON object.");
      const next=mergeWorkspaceProblemPatch(current,template);
      const nextSchema=template.editor_schema_patch?mergeWorkspaceProblemPatch(schema||{},{problem_patch:template.editor_schema_patch}):schema;
      draft=next;schema=nextSchema;raw.value=JSON.stringify(draft,null,2);rawDirty=false;renderGuided();
      dirty();status.textContent=`${String(template.label||template.id||"Provider template")} staged as an editable draft (${capabilityStateLabel(template.truth_status)}). Validate and apply to run provider preflight.`;
      return this.clone(draft);
    };
    const readGuided=()=>{
      const next=this.clone(draft);let firstInvalid=null;
      for(const input of controls){
        const path=JSON.parse(input.dataset.problemPath),type=input.dataset.problemType;
        let value=input.value;
        input.setCustomValidity?.("");input.setAttribute("aria-invalid","false");
        if(type==="number"){
          value=value.trim()===""?NaN:Number(value);
          if(!Number.isFinite(value)){input.setCustomValidity?.("Enter a finite number.");input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          if(input.dataset.problemInteger==="true"&&!Number.isSafeInteger(value)){input.setCustomValidity?.("Enter a whole number within the supported exact integer range.");input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          if(input.dataset.problemMultipleOf!=null){
            const step=Number(input.dataset.problemMultipleOf),ratio=value/step;
            if(!Number.isFinite(ratio)||Math.abs(ratio-Math.round(ratio))>8*Number.EPSILON*Math.max(1,Math.abs(ratio))){input.setCustomValidity?.(`Enter a multiple of ${step}.`);input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          }
          const minimum=input.dataset.problemMinimum==null?null:Number(input.dataset.problemMinimum),maximum=input.dataset.problemMaximum==null?null:Number(input.dataset.problemMaximum);
          if(minimum!=null&&value<minimum){input.setCustomValidity?.(`Enter a value greater than or equal to ${minimum}.`);input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          if(maximum!=null&&value>maximum){input.setCustomValidity?.(`Enter a value less than or equal to ${maximum}.`);input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          const exclusiveMinimum=input.dataset.problemExclusiveMinimum==null?null:Number(input.dataset.problemExclusiveMinimum),exclusiveMaximum=input.dataset.problemExclusiveMaximum==null?null:Number(input.dataset.problemExclusiveMaximum);
          if(exclusiveMinimum!=null&&value<=exclusiveMinimum){input.setCustomValidity?.(`Enter a value greater than ${exclusiveMinimum}.`);input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          if(exclusiveMaximum!=null&&value>=exclusiveMaximum){input.setCustomValidity?.(`Enter a value less than ${exclusiveMaximum}.`);input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
        }else if(type==="json_value"){
          try{value=finiteJsonCopy(JSON.parse(value));}
          catch(error){input.setCustomValidity?.("Enter valid JSON with finite numbers.");input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
        }else if(type==="json_array"){
          try{value=JSON.parse(value);if(!Array.isArray(value))throw new Error("not an array");value=finiteJsonCopy(value);}
          catch(error){input.setCustomValidity?.("Enter a JSON list with finite values, or [] for an empty list.");input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
        }else if(type==="numeric_array"){
          try{value=JSON.parse(value);if(nativeNumericArrayCount(value)===null)throw new Error("not a numeric array");}
          catch(error){input.setCustomValidity?.("Enter a JSON array containing only finite numbers.");input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          const issue=nativeNumericArraySchemaError(value,JSON.parse(input.dataset.problemArraySchema||"{}"));
          if(issue){input.setCustomValidity?.(issue);input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
        }else if(type==="enum"){
          const choices=JSON.parse(input.dataset.problemEnum||"[]"),index=input.value.trim()===""?NaN:Number(input.value);
          if(!Number.isInteger(index)||index<0||index>=choices.length){input.setCustomValidity?.("Choose an allowed value.");input.setAttribute("aria-invalid","true");firstInvalid=firstInvalid||input;continue;}
          value=structuredClone(choices[index]);
        }else if(type==="boolean")value=value==="true";
        else if(type==="nullable")value=value.trim()===""?null:value;
        setAtPath(next,path,value);
      }
      if(firstInvalid){firstInvalid.reportValidity?.();firstInvalid.focus?.();throw new Error(`${firstInvalid.getAttribute?.("aria-label")||"A guided value"} contains an invalid value. Correct the highlighted control.`);}
      return next;
    };
    applyCapabilitySchema=(manifest,requested)=>{
      if(rawDirty||applying||guided.isConnected===false||!manifest.editor_schema?.properties)return;
      let current;try{current=readGuided();}catch(error){return;}
      if(JSON.stringify(current)!==requested)return;
      try{
        const nextSchema=mergeWorkspaceProblemPatch(schema||{},{problem_patch:manifest.editor_schema});
        draft=current;schema=nextSchema;renderGuided();
      }catch(error){renderMessage(status,"Provider field labels could not be loaded. Existing values are unchanged.",error);}
    };
    renderGuided();
    raw.oninput=()=>{dirty();rawDirty=true;for(const control of controls)control.disabled=true;status.textContent="Expert declaration edited. Load it into guided controls or validate and apply; guided editing is paused to preserve your JSON changes.";};
    rawDetails.ontoggle=()=>{if(rawDetails.open&&!rawDirty&&!applying){try{raw.value=JSON.stringify(readGuided(),null,2);}catch(error){renderMessage(status,"Correct the guided values before editing the expert declaration.",error);}}};
    const loadGuided=node("button","","Load JSON into guided controls");loadGuided.type="button";loadGuided.addEventListener("click",dirty);
    loadGuided.onclick=()=>{try{const next=JSON.parse(raw.value);if(!next||Array.isArray(next)||typeof next!=="object")throw new Error("The physical problem must be a JSON object.");draft=this.clone(next);rawDirty=false;renderGuided();status.textContent="JSON loaded into guided controls as a draft. Validate and apply to save it.";}catch(error){renderMessage(status,"The JSON could not be loaded into guided controls. Your editor contents are unchanged.",error);}};
    rawDetails.append(rawSummary,rawNote,raw,loadGuided);
    const apply=node("button","primary","Validate and apply to workflow");apply.type="button";
    const evaluationOnly=p.traits?.evaluation_only===true;
    const explicitDesign=p.traits?.requires_explicit_design===true;
    const canEvaluate=evaluationOnly||explicitDesign;
    const designInput=explicitDesign?node("textarea","",""):null;
    if(designInput){designInput.value="{}";designInput.rows=8;designInput.setAttribute("aria-label","Explicit design snapshot JSON");designInput.spellcheck=false;}
    const evaluate=canEvaluate?node("button","","Evaluate authored case"):null;
    const evaluationOutput=canEvaluate?node("pre","implexity-native-result",""):null;
    const spatialOutput=canEvaluate?node("section","implexity-native-spatial-result"):null;
    const download=canEvaluate?node("button","","Download last evaluated case (JSON)"):null;
    let evaluationSnapshot=null;
    const setEditorBusy=value=>{applying=value;apply.disabled=value;discard.disabled=value;next.disabled=value;if(evaluate)evaluate.disabled=value;if(designInput)designInput.disabled=value;guided.inert=value;raw.disabled=value;loadGuided.disabled=value;host.setAttribute("aria-busy",String(value));};
    apply.onclick=async()=>{if(applying)return;let saved=false;setEditorBusy(true);try{
      const problem=rawDirty?JSON.parse(raw.value):readGuided();
      if(!problem||Array.isArray(problem)||typeof problem!=="object")throw new Error("The physical problem must be a JSON object.");
      const designCoordinates=this.providerDesignCoordinates(id);
      const persistence=window.ImplexityProviderProblemPersistence;
      if(!persistence?.save||!persistence?.captureModel)throw new Error("Provider problem persistence is unavailable.");
      const expectedModel=await persistence.captureModel();
      if(guided.isConnected===false)throw new Error("This physics editor was closed or replaced. Reopen it before saving.");
      let record;
      if(expectedProblemId){
        const response=await json('/v1/agent/action',{method:'POST',body:JSON.stringify({action:'revise_engineering_problem',payload:{provider:id,problem,expected_problem_id:expectedProblemId,expected_model:expectedModel}})});
        record=persistence.recordFromResponse(response);
      }else{
        const report=await json("/v1/implicit/cae/preflight",{method:"POST",body:JSON.stringify({provider:id,problem,design_coordinates:designCoordinates})});
        if(report.ok===false)throw new Error(JSON.stringify(report.issues||report.errors||report));
        record=await persistence.save({providerId:id,problem,expectedModel,provenance:{source:"native_gui",editor:"native_json",operation:"validate_and_apply"}});
      }
      expectedProblemId=record.problem_id;
      saved=true;this.markPhysicsDraft(id,false);
      const stored=this.clone(record.problem);
      this.providerProblems[id]=stored;this.providerProblemRecords[id]=this.clone(record);if(id==="legacy_multiphysics_implicit")this.problem=this.clone(stored);
      window.dispatchEvent(new CustomEvent("implexity:provider-problem-changed",{detail:{providerId:id,problem:stored,record}}));
      draft=form(stored);raw.value=JSON.stringify(draft,null,2);rawDirty=false;renderGuided();
      void loadCapabilities(draft);
      status.textContent=evaluationOnly?`Problem stored (${record.problem_id}). Use Evaluate authored case; topology optimisation is unavailable.`:`Problem stored for this model (${record.problem_id}). Model-aware optimisation preflight is still required.`;
      this.note(`${friendlyProvider(id)} authoring applied`);this.renderReadiness();return true;
    }catch(e){renderMessage(status,saved?"The provider problem was saved, but the editor could not refresh. Reopen the setup to inspect the stored values.":"The provider problem save was not confirmed. Review the declaration and refresh stored settings before trying again.",e);}finally{setEditorBusy(false);}};
    const discard=node("button","","Discard physics draft");discard.type="button";
    const discardDraft=()=>{
      if(applying)throw new Error("Finish validation before discarding the draft.");
      draft=form(this.providerProblems[id]||seeded);raw.value=JSON.stringify(draft,null,2);rawDirty=false;
      renderGuided();this.markPhysicsDraft(id,false);status.textContent="Stored values restored in the editor. Geometry was not changed.";
    };
    discard.onclick=discardDraft;
    const next=node("button","","Apply and set objectives");next.type="button";
    next.onclick=async()=>{if(await apply.onclick())this.setStep("objectives");};
    this._activeNativeEditor={providerId:id,dirty:()=>this._physicsDrafts.has(id),discard:discardDraft};
    const search=node("input","implexity-physics-search");search.type="search";search.placeholder="Find a physical parameter, material or boundary…";search.setAttribute("aria-label","Find physics setting");
    const found=node("span","implexity-native-help");found.setAttribute("role","status");
    const searchRow=node("div","implexity-physics-search-row");searchRow.append(search,found);
    search.oninput=()=>{
      const term=search.value.trim().toLowerCase();let count=0,first=null;
      for(const element of qa("label.implexity-native-field",guided)){
        const match=!term||element.textContent.toLowerCase().includes(term)||qa("[aria-label]",element).some(x=>x.getAttribute("aria-label").toLowerCase().includes(term));
        element.hidden=!match;if(match){count++;first=first||element;}
      }
       
       
      for(const element of qa("fieldset,.implexity-native-list-item,details",guided).reverse()){
        element.hidden=Boolean(term&&!qa("label.implexity-native-field",element).some(label=>!label.hidden));
      }
      found.textContent=term?`${count} matching settings`:"";
      if(term&&first)first.scrollIntoView({block:"nearest"});
    };
    const reload=node("button","","Reload stored physics");reload.type="button";
    reload.onclick=async()=>{
      if(applying)return;
      if(this._physicsDrafts.has(id)&&!window.confirm("Discard the local physics draft and reload the current stored problem?"))return;
      try{
        setEditorBusy(true);
        const record=await json("/v1/implicit/problem");
        if(record.provider!==id||!record.problem)throw new Error("The stored physics provider changed. Refresh the workbench and choose its setup.");
        this.providerProblems[id]=this.clone(record.problem);this.providerProblemRecords[id]=this.clone(record);
        expectedProblemId=record.problem_id;draft=form(record.problem);raw.value=JSON.stringify(draft,null,2);rawDirty=false;
        renderGuided();search.value="";search.oninput();this.markPhysicsDraft(id,false);
        window.dispatchEvent(new CustomEvent("implexity:provider-problem-changed",{detail:{providerId:id,problem:this.clone(record.problem),record:this.clone(record)}}));
        status.textContent="Current stored physics reloaded. Review it before fresh preflight.";
      }catch(error){renderMessage(status,"Stored physics could not be reloaded.",error);}
      finally{setEditorBusy(false);}
    };
    const footer=node("div","implexity-physics-editor-footer");footer.append(apply,next,discard,reload,status);
    host.append(title,note);
    if(p.editor?.authoring_notes)host.append(node("p","implexity-native-help",String(p.editor.authoring_notes)));
    if(!sharedCapabilityHost)host.append(capabilityHost);host.append(searchRow,guided,rawDetails,footer);
    host.dataset.providerEditor=id;
    window.dispatchEvent(new CustomEvent("implexity:physics-editor-opened",{detail:{providerId:id}}));
    if(evaluate){
      if(designInput){
        host.append(node("h4","","Explicit design snapshot"),node("p","",`Supply named coordinate snapshots for ${this.providerDesignCoordinates(id).join(", ")}. Each entry contains value, lower, upper and designable. Array shapes must match the provider problem. This evaluates the supplied arrays, not the viewport geometry; it does not start optimization.`),designInput);
        if(p.editor?.design_template){
          const starter=node("button","","Load provider starter design");starter.type="button";
          starter.onclick=()=>{if(applying)return;designInput.value=JSON.stringify(finiteJsonCopy(p.editor.design_template),null,2);status.textContent="Starter design loaded into the draft. It matches the provider's original problem template; if you changed the grid or protected cells, update the design before evaluating.";};
          host.append(starter);
        }
      }
      evaluate.type="button";
      download.type="button";download.disabled=true;
      download.onclick=()=>{if(!evaluationSnapshot)return;try{downloadEvaluationSnapshot(evaluationSnapshot,id);}catch(error){renderMessage(status,"The evaluated case could not be downloaded.",error);}};
      evaluate.onclick=async()=>{if(applying)return;setEditorBusy(true);evaluationSnapshot=null;download.disabled=true;evaluationOutput.textContent="";spatialOutput.replaceChildren();try{
        const problem=rawDirty?JSON.parse(raw.value):readGuided();
        const design=designInput?finiteJsonCopy(JSON.parse(designInput.value)):{};
        if(!design||Array.isArray(design)||typeof design!=="object")throw new Error("The explicit design snapshot must be a JSON object.");
        if(explicitDesign){
          const names=this.providerDesignCoordinates(id);
          if(Object.keys(design).length!==names.length||names.some(name=>!Object.hasOwn(design,name)))throw new Error(`Provide exactly these design coordinates: ${names.join(", ")}.`);
          for(const name of names){const entry=design[name];if(!entry||Array.isArray(entry)||typeof entry!=="object"||["value","lower","upper","designable"].some(key=>!Object.hasOwn(entry,key)))throw new Error(`${name} requires value, lower, upper and designable.`);}
        }
        const request={problem:{physics:{provider:id,problem}},design};
        const requestText=JSON.stringify(request);
        const result=await json("/v1/implicit/cae/evaluate",{method:"POST",body:requestText});
        if(evaluate.isConnected===false)return;
        evaluationOutput.textContent=JSON.stringify({responses:result.responses,diagnostics:result.diagnostics},null,2);
        status.textContent="Authored-case evaluation completed. The draft was not saved or optimized.";
        try{renderCartesianScalarPreview(spatialOutput,result,p.traits?.spatial_preview);}
        catch(error){renderMessage(spatialOutput,"Evaluation completed, but the spatial preview is unavailable.",error);}
        try{renderResponseHistories(spatialOutput,result,p.traits?.history_preview);}
        catch(error){renderMessage(spatialOutput,"Evaluation completed, but the response histories could not be plotted.",error);}
        try{evaluationSnapshot=evaluatedCaseExport(p,JSON.parse(requestText),result);download.disabled=false;}
        catch(error){renderMessage(status,"Evaluation completed, but its downloadable snapshot could not be prepared.",error);}
      }catch(error){renderMessage(status,"Evaluation failed; no completed result is claimed.",error);}finally{setEditorBusy(false);}};
      host.append(evaluate,download,spatialOutput,evaluationOutput);
    }
    return capabilityPromise;
  }

  attachCFD(Class,force=false){
    if(!Class)return;const host=q("#implexity-physics-editor");if(!host)return;
    if(this.cfd?.root){host.hidden=false;host.replaceChildren(this.cfd.root);this.cfd.root.hidden=false;this.cfd.open();this.note("Resolved Stokes–Brinkman setup opened");return;}
    if(this.step!=="physics"&&!force)return;host.hidden=false;host.innerHTML="";
    this.cfd=new Class(window.implexityViewer||window.viewer||null,{host,embedded:true});
    this.cfd.root.classList.add("implexity-cfd-embedded");this.cfd.root.hidden=false;host.append(this.cfd.root);this.cfd.open();this.note("Resolved Stokes–Brinkman setup opened");
  }

  renderReadiness(){
    window.ImplexityDesignFreedom?.refreshAvailability?.();
    const grid=q("[data-implexity-readiness-grid]",this.readiness);if(!grid)return;
    const state=this.readinessState();
    const cards=[
      ["Geometry",state.loaded?"ready":"missing",state.loaded?`${state.free} additional free coordinate${state.free===1?"":"s"}`:"load or create an implicit model",state.loaded],
      ["Topology",state.topology.label,state.topology.detail,state.topology.available],
      ["Physics",friendlyProvider(state.provider,state.providerEntry||{}),state.physics?"physical problem stored":"open provider setup to complete the problem",state.physics],
      [state.completeResponseCount?"Responses":"Objectives",state.objectives?`${state.objectives} applied`:state.completeResponseCount?`${state.completeResponseCount} limit responses`:(this._dirty?"draft":"not configured"),state.objectives?"mature implicit objective is ready":state.completeResponseCount?"Complete response constraints are applied. Preflight checks their physical meaning.":(this._dirty?"apply the response program":"add and apply at least one differentiable response"),state.objectives>0||state.completeResponseCount>0]
    ];
    if(state.hierarchyError)cards.push(["Optimisation scope","needs attention",state.hierarchyError,false]);
    grid.replaceChildren(...cards.map(([key,value,detail,ok])=>{const item=node("div",`implexity-ready-item ${ok?"ok":"pending"}`);item.setAttribute("role","listitem");item.dataset.state=ok?"ready":"needs-attention";item.append(node("span","",key),node("strong","",String(value)),node("small","",detail),node("span","visually-hidden",ok?"Ready.":"Needs attention."));return item;}));
    const lifecycleBlocked=Boolean(window.implexityOptimizationBlocksNewRun?.());
    const nextAction=this.nextReadinessAction(state);
    const blockedReason=lifecycleBlocked?"Resolve the current optimisation or manual intervention first.":!state.ready?`${nextAction.label} before starting direct-gradient optimisation.`:"";
    const help=q("#implexity-readiness-help",this.readiness);if(help)help.textContent=blockedReason||"The current authoritative design is ready for direct-gradient optimisation.";
    const run=q('[data-implexity-ready-action="run"]',this.readiness);if(run){run.disabled=!state.ready||lifecycleBlocked;run.title=blockedReason;run.setAttribute("aria-description",blockedReason||"Starts direct-gradient optimisation for the current authoritative design.");}
    window.updateOptButtons?.();
    if(!state.ready){
      const optstart=q("#optstart"),preflight=q("#pfbtn");
      if(optstart){optstart.disabled=true;optstart.title=`${nextAction.label} before optimisation`;}
      if(preflight){preflight.disabled=true;preflight.title=`${nextAction.label} before preflight`;}
    }
    const nextButtons=[q('[data-implexity-ready-action="next"]',this.readiness),q('[data-implexity-context-next]',this.context)].filter(Boolean);
    for(const next of nextButtons){next.dataset.implexityNextAction=nextAction.action;next.textContent=nextAction.label;next.disabled=!state.loaded&&nextAction.action!=="geometry";}
    const quickRun=q('[data-implexity-quick="run"]',this.context);if(quickRun){quickRun.disabled=!state.ready||lifecycleBlocked;quickRun.title=blockedReason;quickRun.setAttribute("aria-description",blockedReason||"Starts direct-gradient optimisation for the current authoritative design.");}
    const quickPreflight=q('[data-implexity-quick="preflight"]',this.context);if(quickPreflight){quickPreflight.disabled=!state.ready||lifecycleBlocked;quickPreflight.title=blockedReason;quickPreflight.setAttribute("aria-description",blockedReason||"Checks the current authoritative design without starting optimisation.");}
    const badge=q("[data-implexity-topology-badge]",this.readiness);if(badge){badge.textContent=state.topology.available?"Shared topology field":"Topology hand-off required";badge.classList.toggle("pending",!state.topology.available);}
  }

  async quick(action){
    if(action==="next")action=q('[data-implexity-ready-action="next"]',this.readiness)?.dataset.implexityNextAction||this.nextReadinessAction().action;
    if(action==="geometry"){this.setStep("geometry");this.note("Create or load the geometry before continuing to physics");return;}
    if(action==="handoff"){
      if(window.ImplexityGeometrySeeds?.openBakeCurrent)window.ImplexityGeometrySeeds.openBakeCurrent();
      else window.dispatchEvent(new CustomEvent("implexity-request-topology-handoff"));
      this.note("Topology hand-off review opened for the current model");return;
    }
    if(action==="hierarchy"){
      this.setStep("optimize");
      if(!window.ImplexityDesignFreedom?.open?.())this.note("Clear the saved hierarchy or select a compatible topology physics provider before continuing.");
      return;
    }
    if(action==="physics"){this.setStep("physics");this.note("Complete the selected physics provider before adding objectives");return;}
    if(action==="objectives"){this.setStep("objectives");this.note("Add and apply at least one differentiable response");return;}
    if(action==="preflight"){
      const state=this.readinessState();if(!state.ready){return this.quick(this.nextReadinessAction(state).action);}
      if(window.implexityOptimizationBlocksNewRun?.()){this.setStep("optimize");this.note("Resolve the current optimisation or manual intervention before checking another run");return;}
      this._preflightOriginStep=this.step;const b=q("#pfbtn");
      if(b){b.click();this.note("Preflight requested for the current implicit design")}
      else{this._preflightOriginStep=null;this.note("Preflight control is unavailable")}
      return;
    }
    if(action==="run"){
      const state=this.readinessState();this.setStep("optimize");const b=q("#optstart");
      if(window.implexityOptimizationBlocksNewRun?.()){this.note("Resolve the current optimisation or manual intervention before starting another run");return;}
      if(state.ready&&b&&!b.disabled){b.click();this.note("Direct-gradient optimisation launch opened")}
      else this.note(state.topology.available?"Apply an objective and complete the physical problem before starting":state.topology.action);
    }
  }

  handlePreflightState(detail){
    const state=String(detail?.state||"");
    if(state==="invalidated"||state==="consumed"){
      this._preflightOriginStep=null;
       
       
      if(this._noteKind==="preflight"){
        const message=state==="consumed"
          ? "The preflight check was used for this launch. A new run requires a fresh check."
          : "The previous preflight is no longer current. Check the updated design and setup before starting another run.";
        this.note(message,"preflight-stale");
      }
      return;
    }
    if(state==="loading"){
      if(!this._preflightOriginStep)this._preflightOriginStep=this.step;
      this._note="Checking the current design without changing it…";this._noteKind="preflight";this.lastError=null;this.renderStatus();return;
    }
    if(!["ready","refused","blocked","error"].includes(state))return;
    const declared=String(detail?.workflow_step||detail?.report?.workflow_step||"");
    const target=STEP_BY_ID[declared]?declared:(this._preflightOriginStep||this.step||"optimize");
    if(this.step!==target)this.setStep(target,{initial:true});
    this._preflightOriginStep=null;
    if(state==="ready")this.note("Preflight passed for the current authoritative design","preflight");
    else{
      const fallback=state==="blocked"?"Preflight is blocked by the current workflow state.":state==="refused"?"Preflight found readiness issues in the current design.":"Preflight could not be completed. The current design was not changed.";
      const summary=publicMessage(detail?.problems?.[0]||detail?.summary,fallback);
      this.fail(summary,detail?.problems||detail?.report||detail,"preflight");
    }
  }

  announce(message,assertive=false){
    const target=q(assertive?"#implexityErrorAnnouncer":"#implexityStatusAnnouncer");if(!target)return;
    target.textContent="";requestAnimationFrame(()=>{target.textContent=String(message||"");});
  }
  note(msg,kind=null){this._note=msg;this._noteKind=kind;this.lastError=null;this.renderStatus();this.announce(msg)}
  fail(summary,error,kind=null){this._note="";this._noteKind=kind;this.lastError={summary,technical:error};this.renderStatus();this.announce(summary,true)}
  invalidateRunAuthorization(reason){window.implexityInvalidateOptimizationPreflight?.(reason)}

  async history(op){
    try{const coordinator=window.ImplexityManualHistory;if(!coordinator)throw new Error("Manual gesture history is unavailable");const d=await coordinator.request(op);this.note(`${human(op)}: ${d.label}`);await globalThis.loadModel?.();await this.refresh()}
    catch(e){this.fail("The design history action could not be completed. The current design was left unchanged.",e)}
  }

  togglePane(which,{trigger=null,focusInside=false}={}){
    if(!["left","right"].includes(which))return;
    const opening=!this._paneState[which];
    if(opening&&this._compactMode)this._compactReturnFocus=trigger||document.activeElement;
    this._paneState[which]=opening;
    if(opening&&this._compactMode)this._paneState[which==="left"?"right":"left"]=false;
    this.persistPaneState();this.applyPaneState();
    if(opening&&this._compactMode&&focusInside){
      const pane=q(which==="left"?"#implexityModelPane":"#implexityInspectorPane");
      requestAnimationFrame(()=>{const target=pane?.querySelector?.('.sect-h,a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])')||pane;target?.focus?.({preventScroll:true});});
    }
    if(!opening&&this._compactMode)this.restoreCompactFocus(trigger);
  }
  toggleFocus(){
    const both=this._paneState.left||this._paneState.right;
    if(both){this._paneBeforeFocus={...this._paneState};this._paneState={left:false,right:false}}
    else this._paneState=this._paneBeforeFocus||{left:true,right:true};
    this.persistPaneState();this.applyPaneState();
  }
  restoreCompactFocus(fallback=null){
    const target=fallback||this._compactReturnFocus||q('[data-implexity-focus]',this.shellControls)||this.stage;
    this._compactReturnFocus=null;requestAnimationFrame(()=>target?.focus?.({preventScroll:true}));
  }
  closeCompactPanes({restoreFocus=false,fallback=null}={}){
    if(!this._compactMode)return;
    this._paneState={left:false,right:false};this.persistPaneState();this.applyPaneState();
    if(restoreFocus)this.restoreCompactFocus(fallback);
  }
  defaultCompactPaneState(step=this.step){return step==="geometry"?{left:false,right:false}:{left:false,right:true}}
  persistPaneState(){
    const value={left:Boolean(this._paneState.left),right:Boolean(this._paneState.right)};
    if(this._compactMode){this._compactPaneState=value;store.set("implexity.panes.compact",value)}
    else{this._widePaneState=value;store.set("implexity.panes.wide",value);store.set("implexity.panes",value)}
  }
  syncPaneMode(){
    const width=document.documentElement?.clientWidth||window.innerWidth||0;
    const compact=compactPanesForWidth(width);if(compact===this._compactMode)return;
    this.persistPaneState();this._compactMode=compact;
    this._paneState={...(compact?this.defaultCompactPaneState():this._widePaneState||{left:true,right:true})};
    this.persistPaneState();this.applyPaneState();
  }
  syncStageHeight(){
    const height=Number(this.stage?.getBoundingClientRect?.().height||0);if(!(height>0))return;
    const mode=stageHeightModeForHeight(height);if(mode===this._stageHeightMode)return;
    this._stageHeightMode=mode;document.body.dataset.implexityStageHeight=mode;
    window.dispatchEvent(new CustomEvent("implexity:presentation-changed",{detail:{kind:"stage-height",mode,height}}));
  }
  applyPaneState(){
    if(this._compactMode&&this._paneState.left&&this._paneState.right){
      this._paneState=this.defaultCompactPaneState();
      this.persistPaneState();
    }
    document.body.classList.toggle("implexity-left-hidden",!this._paneState.left);
    document.body.classList.toggle("implexity-right-hidden",!this._paneState.right);
    document.body.classList.toggle("implexity-compact-panes",this._compactMode);
    qa("[data-implexity-pane]",this.shellControls).forEach(b=>{const open=!!this._paneState[b.dataset.implexityPane];b.setAttribute("aria-pressed",String(open));b.setAttribute("aria-expanded",String(open));});
    const left=q("#implexityModelPane"),right=q("#implexityInspectorPane");
    for(const [pane,visible] of [[left,this._paneState.left],[right,this._paneState.right]])if(pane){pane.setAttribute("aria-hidden",String(!visible));pane.inert=!visible;}
    const focus=q("[data-implexity-focus]",this.shellControls);if(focus)focus.setAttribute("aria-pressed",String(!this._paneState.left&&!this._paneState.right));
    const hasCompactPane=this._compactMode&&(this._paneState.left||this._paneState.right);
    if(this.paneBackdrop){this.paneBackdrop.hidden=!hasCompactPane;this.paneBackdrop.setAttribute("aria-hidden",String(!hasCompactPane));}
    const syncViewport=()=>{window.VIEW?.resize?.();window.dispatchEvent(new CustomEvent("implexity:viewport-layout-changed"));};
    requestAnimationFrame(syncViewport);
    setTimeout(syncViewport,220);
    window.dispatchEvent(new CustomEvent("implexity:presentation-changed",{detail:{kind:"pane-layout",compact:this._compactMode,left:Boolean(this._paneState.left),right:Boolean(this._paneState.right),estimated_stage_width:estimatedWideStageWidth(document.documentElement?.clientWidth||window.innerWidth||0)}}));
  }
}

window.ImplexityWorkbenchLayout=Object.freeze({minimumUsableStageWidth:MIN_USABLE_STAGE_WIDTH,shortStageHeight:SHORT_STAGE_HEIGHT,compactStageHeight:COMPACT_STAGE_HEIGHT,estimatedWideStageWidth,compactPanesForWidth,stageHeightModeForHeight});
window.ImplexityNumericalSolverHealth=Object.freeze({recordSetSchema:NUMERICAL_SOLVER_RECORD_SET_SCHEMA,recordSchema:NUMERICAL_SOLVER_RECORD_SCHEMA,records:numericalSolverRecords,summarize:numericalSolverHealth});
window.ImplexityWorkspaceCapabilityUI=Object.freeze({manifest:workspaceCapabilityManifest,render:renderWorkspaceCapabilities,constraintDeclaration:workspaceConstraintDeclaration,constraintControlPresentation:workspaceConstraintControlPresentation,constraintStatusPresentation:workspaceConstraintStatusPresentation,constraintAssurancePresentation:workspaceConstraintAssurancePresentation,templatePatch:workspaceTemplateProblemPatch,mergeProblemPatch:mergeWorkspaceProblemPatch,schemaAtPath:nativeSchemaAtPath,schemaDescriptor:nativeSchemaDescriptor});
window.ImplexityWorkbenchController=ImplexityWorkbench;
function boot(){if(!window.implexityWorkbench)window.implexityWorkbench=new ImplexityWorkbench();window.implexityBuildImplicitOptimizationRequest=base=>window.implexityWorkbench.buildOptimizationRequest(base);window.implexityBuildImplicitSensitivityRequest=base=>window.implexityWorkbench.buildSensitivityRequest(base)}
if(document.readyState==="loading")document.addEventListener("DOMContentLoaded",boot,{once:true});else boot();
})();
