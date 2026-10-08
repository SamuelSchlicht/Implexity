// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(()=>{
"use strict";
const ROLES=["topology_free","phase_free","cooling_only","material_free","shape_free","parametric_geometry","fixed_geometry","preserve_interface","manufacturing_protected","manual_locked"];
const FIDELITIES=["screening","intermediate","high","qualification"];
const S={coords:[],blocks:[],stages:[],updateMetric:"gradient_adaptive",updateAlpha:0.5,committed:null,loaded:false,dialog:null,
  configured:false,modelStructure:null,providerSignature:null,selectionExplicit:false,card:null};
const DIALOG_CSS=`
#implexity-design-freedom{color-scheme:light}
#implexity-design-freedom[open]{display:flex;flex-direction:column}
#implexity-design-freedom *{box-sizing:border-box}
#implexity-design-freedom .implexity-df-head{display:flex;flex-shrink:0;align-items:flex-start;justify-content:space-between;gap:16px;padding:18px 20px;border-bottom:1px solid var(--line, #ccd5df)}
#implexity-design-freedom .implexity-df-title-copy{min-width:0}
#implexity-design-freedom .implexity-df-head>[data-close]{flex:0 0 auto;width:auto;min-width:80px}
#implexity-design-freedom .implexity-df-title-copy p{overflow-wrap:anywhere}
#implexity-design-freedom .implexity-df-body{padding:18px 20px;overflow:auto;flex:1 1 auto;min-height:0}
#implexity-design-freedom .implexity-df-coordinate-row{display:grid;grid-template-columns:minmax(165px,1fr) minmax(240px,2fr) repeat(2,minmax(76px,.5fr));gap:12px;margin:18px 0;align-items:start}
#implexity-design-freedom .implexity-df-coordinate-row>:first-child{grid-column:1/-1}
#implexity-design-freedom .implexity-df-coordinate-row [role=alert]{border-left:3px solid #a66a22;padding:8px 12px;background:#fff8eb;margin:10px 0 0}
#implexity-design-freedom .implexity-df-coordinate-row label{display:block;min-width:0}
#implexity-design-freedom .implexity-df-coordinate-row input,#implexity-design-freedom .implexity-df-coordinate-row select{box-sizing:border-box;width:100%;min-width:0}
#implexity-design-freedom .implexity-df-block-row{display:grid;grid-template-columns:minmax(170px,1.3fr) minmax(150px,1.2fr) minmax(190px,1.5fr);gap:8px;margin:8px 0;align-items:center}
#implexity-design-freedom .implexity-df-stage{border:1px solid var(--line, #ccd5df);border-radius:8px;padding:10px;margin:10px 0}
#implexity-design-freedom .implexity-df-stage-grid{display:grid;grid-template-columns:minmax(150px,1.2fr) minmax(180px,1.5fr) minmax(130px,1fr) minmax(90px,.65fr);gap:8px}
#implexity-design-freedom .implexity-df-release{margin-top:8px}
#implexity-design-freedom .implexity-df-release-list{display:flex;flex-wrap:wrap;gap:10px;margin-top:6px}
#implexity-design-freedom .implexity-df-release-list label{display:flex;gap:5px;align-items:center}
#implexity-design-freedom .implexity-df-transition-grid{display:grid;grid-template-columns:repeat(3,minmax(150px,1fr));gap:8px;margin-top:10px}
#implexity-design-freedom .implexity-df-limit-grid{display:grid;grid-template-columns:repeat(4,minmax(140px,1fr));gap:8px;margin-top:8px}
#implexity-design-freedom .implexity-df-actions{display:flex;flex-wrap:wrap;gap:8px;margin-top:14px}
#implexity-design-freedom>.implexity-df-actions{flex-shrink:0;background:#fff;border-top:1px solid var(--line,#ccd5df);padding:10px 20px;margin:0}
#implexity-design-freedom .implexity-df-apply{margin-left:auto}
#implexity-design-freedom label{min-width:0;color:var(--text, #192536);overflow-wrap:anywhere}
#implexity-design-freedom input,#implexity-design-freedom select,#implexity-design-freedom button{max-width:100%;border:1px solid var(--line, #ccd5df);border-radius:5px;background:var(--panel, #ffffff);color:var(--text, #192536);padding:7px 8px;font:inherit}
#implexity-design-freedom input,#implexity-design-freedom select{width:100%;min-width:0}
#implexity-design-freedom input:disabled{opacity:1;background:#edf1f5;color:#506176;border-color:#b8c5d4}
#implexity-design-freedom button:focus-visible,#implexity-design-freedom input:focus-visible,#implexity-design-freedom select:focus-visible{outline:2px solid #1769aa;outline-offset:2px}
#implexity-design-freedom .implexity-df-error{margin-top:12px;padding:9px 10px;border:1px solid #b84c43;border-radius:6px;background:#fff2f0;color:#852c25;line-height:1.4}
#implexity-design-freedom .implexity-df-error .implexity-ui-technical{margin-top:7px;color:#43536a}
#implexity-design-freedom .implexity-df-error .implexity-ui-technical summary{cursor:pointer;color:#43536a}
#implexity-design-freedom .implexity-df-error .implexity-ui-technical pre{max-height:160px;overflow:auto;margin:6px 0 0;padding:8px;border-radius:5px;background:var(--panel, #ffffff);color:#192536;font:11px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace}
@media(max-width:720px){
 #implexity-design-freedom .implexity-df-coordinate-row{grid-template-columns:1fr 1fr;align-items:end}
 #implexity-design-freedom .implexity-df-coordinate-row>:first-child,#implexity-design-freedom .implexity-df-coordinate-row>.implexity-df-binding{grid-column:1/-1}
 #implexity-design-freedom .implexity-df-block-row,#implexity-design-freedom .implexity-df-transition-grid{grid-template-columns:1fr}
 #implexity-design-freedom .implexity-df-stage-grid,#implexity-design-freedom .implexity-df-limit-grid{grid-template-columns:1fr 1fr}
}
@media(max-width:480px){
 #implexity-design-freedom .implexity-df-head{flex-direction:column;padding:14px}
 #implexity-design-freedom .implexity-df-head>[data-close]{align-self:flex-end;order:-1}
 #implexity-design-freedom .implexity-df-body{padding:14px;max-height:72vh}
 #implexity-design-freedom .implexity-df-coordinate-row,#implexity-design-freedom .implexity-df-stage-grid,#implexity-design-freedom .implexity-df-limit-grid{grid-template-columns:1fr}
 #implexity-design-freedom .implexity-df-coordinate-row>*{grid-column:1!important}
 #implexity-design-freedom .implexity-df-actions{display:grid;grid-template-columns:1fr}
 #implexity-design-freedom .implexity-df-apply{margin-left:0}
}
`;
const esc=s=>String(s??"").replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const present=(id,metadata={})=>window.ImplexityText?.present?.(id,metadata)||({id:String(id??""),label:String(metadata.label||metadata.display_name||id||"")});
const human=(id,metadata={})=>present(id,metadata).label;
function showError(box,summary,technical=null){
 if(!box)return;
 box.hidden=false;
 if(technical&&window.ImplexityUIPresentation?.render){window.ImplexityUIPresentation.render(box,{summary,technical,severity:"error"});return;}
 box.replaceChildren();const message=document.createElement("span");message.className="implexity-ui-summary";message.textContent=summary;box.append(message);box.setAttribute("role","alert");
}
function numericValue(input,{optional=false,integer=false,min=null}={}){
 const raw=String(input?.value??"").trim();
 if(optional&&raw==="")return {valid:true,value:null};
 const value=raw===""?NaN:Number(raw),validity=input?.validity;
 const valid=Number.isFinite(value)&&(!integer||Number.isInteger(value))&&(min===null||value>=min)&&!validity?.badInput&&!validity?.rangeUnderflow&&!validity?.rangeOverflow&&!validity?.stepMismatch;
 return {valid,value:valid?value:null};
}
function setControlValidity(input,valid,message=""){
 input.setAttribute("aria-invalid",String(!valid));
 input.setCustomValidity?.(valid?"":message);
}
function controlLabel(input,fallback){return input.getAttribute("aria-label")||fallback}
function visibleValidation(dialog=S.dialog){
 if(!dialog)return "";
 let first="";
 const inspect=(input,options,fallback)=>{
  const result=numericValue(input,options),label=controlLabel(input,fallback),message=result.valid?"":`${label} must be ${options.integer?`a whole number${options.min!==null?` of at least ${options.min}`:""}`:"a finite number"}.`;
  setControlValidity(input,result.valid,message);if(!result.valid&&!first)first=message;
 };
 dialog.querySelectorAll('[data-c][data-k="lower"], [data-c][data-k="upper"]').forEach(input=>inspect(input,{},"Coordinate bound"));
 dialog.querySelectorAll('[data-s][data-k="iterations"]').forEach(input=>inspect(input,{integer:true,min:1},"Iterations"));
 const alpha=dialog.querySelector("[data-update-alpha]");if(alpha)inspect(alpha,{min:0},"Gradient-adaptive strength");
 dialog.querySelectorAll("[data-limit-stage]").forEach(input=>inspect(input,{optional:true},"Adaptive qualification limit"));
 return first;
}
function workbench(){return window.implexityWorkbench||null}
function provider(){return workbench()?.currentProvider?.()||""}
function providerEntries(){return workbench()?.providerEntries?.()||[]}
function providerLabel(id){const row=providerEntries().find(item=>(item.id||item.name)===id)||{};return human(id,{label:row.display_name||row.displayName||row.label})}
function savedRegions(){
 try{return (window.ImplexitySpatialSelections?.all?.()||[]).filter(item=>item?.id&&Array.isArray(item.selected_runs)).map(item=>({id:String(item.id),label:String(item.region_name||item.id),count:Number(item.counts?.selected||0),fieldId:String(item.field_id||"")}));}
 catch{return []}
}
function coordinateCopy(id){
 return {label:human(id),description:"Local provider-declared design coordinate."};
}
function coordinateField(row){
 const document=workbench()?.model?.document||workbench()?.model||{},nodes=document.nodes||{};
 const match=/^model(?:\/([^:]+))?:([^:]+)$/.exec(String(row?.ref||""));if(!match)return null;
 let id=String(document.root||"");
 for(const segment of (match[1]?match[1].split("/").filter(Boolean):[])){const child=(nodes[id]?.children||[]).find(item=>typeof item==="object"&&String(item.name||"")===segment);if(!child)return null;id=String(child.node||"");}
 const arrayKey=nodes[id]?.params?.[match[2]]?.array;if(!arrayKey)return null;
 const shape=(document.arrays?.[arrayKey]?.shape||[]).map(Number),field=window.ImplexitySpatialFields?.field?.(String(arrayKey));
 return {fieldId:String(arrayKey),shape,registrationId:String(field?.identity?.registration_id||"")};
}
function compatibleSavedRegions(row){
 const target=coordinateField(row);if(!target)return [];
 return savedRegions().filter(region=>{const selection=(window.ImplexitySpatialSelections?.all?.()||[]).find(item=>String(item.id)===region.id);if(!selection)return false;const sameShape=JSON.stringify((selection.shape||[]).map(Number))===JSON.stringify(target.shape);if(!sameShape)return false;const registration=String(selection.field_identity?.registration_id||"");return region.fieldId===target.fieldId||(Boolean(target.registrationId)&&registration===target.registrationId);});
}
function providerCoordinates(){
 try{return workbench()?.providerDesignCoordinates?.(provider())||[]}
 catch{return []}
}
function providerSignature(){
 const id=provider(),coordinates=providerCoordinates();if(!id||!coordinates.length)return null;
 let defaults=[];try{defaults=workbench()?.providerDesignCoordinateDefaults?.(id,problem()||{})||[]}catch{}
 return `${id}\u0000${JSON.stringify(defaults.map(row=>[row.coordinate,row.ref,row.lower,row.upper,row.step_scale??1]))}`;
}
function coordinateSelection(state=draftState()){
 const declared=providerCoordinates(),active=(state?.coords||[]).map(row=>String(row.coordinate||""));
 const unsupported=active.filter(name=>!declared.includes(name));
 const inactive=declared.filter(name=>!active.includes(name));
 const error=!declared.length
  ?"The selected physics provider has not declared a usable design space."
  :unsupported.length
   ?`The current design includes coordinates not declared by ${providerLabel(provider())}: ${unsupported.map(human).join(", ")}.`
   :"";
 return {declared,active,inactive,unsupported,error,explicit:Boolean(state?.selectionExplicit)};
}
function stageProviderError(state){
 const available=new Set(providerEntries().map(item=>String(item.id||item.name||"")).filter(Boolean));
 const invalid=(state?.stages||[]).filter(stage=>!available.has(String(stage.provider||"")));
 if(!invalid.length)return "";
 const labels=invalid.map((stage,index)=>`“${String(stage.label||`Stage ${index+1}`)}”`);
 return invalid.length===1
  ? `Stage ${labels[0]} uses a physics provider that is no longer active. Choose an active provider before saving or running optimisation.`
  : `Stages ${labels.join(", ")} use physics providers that are no longer active. Choose active providers before saving or running optimisation.`;
}
function problem(){return workbench()?.providerProblem?.()||null}
function currentModelStructure(){
 const model=workbench()?.model||window.S?.model||null;
 const value=model?.structure_id||model?.model_identity?.structure_id||null;
 return value==null||String(value).trim()===""?null:String(value);
}
function hierarchySupported(){return Boolean(provider())&&provider()!=="legacy_multiphysics_implicit"}
function defaultState(){
 const p=problem()||{},definitions=workbench()?.providerDesignCoordinateDefaults?.(provider(),p)||[];
 const coords=definitions.map(row=>({coordinate:row.coordinate,ref:String(row.ref||""),lower:structuredClone(row.lower),upper:structuredClone(row.upper),step_scale:Number(row.step_scale??row.stepScale??1),designableSelectionIds:[]}));
 const names=coords.map(row=>row.coordinate);
 const blocks=names.map(name=>({id:name==="model:control"?"primary_topology":name.split(":").pop().replace(/[^a-z0-9]+/gi,"_"),role:name==="model:control"?"topology_free":(name.includes("coolant")?"cooling_only":name.includes("phase")?"phase_free":"parametric_geometry"),coordinates:[name]}));
 const stages=provider()?[{id:"concept",provider:provider(),fidelity:p.fidelity||"",iterations:30,releasedBlocks:blocks.map(block=>block.id),operatingPoints:[0],robustMode:"nominal",transition:"carry",adaptive:{action:"recommend"}}]:[];
 return {coords,blocks,stages,updateMetric:"gradient_adaptive",updateAlpha:0.5,selectionExplicit:false};
}
function stateFromDeclaration(value){
 if(!value||!Array.isArray(value.design_coordinates)||!Array.isArray(value.design_freedom?.blocks)||!Array.isArray(value.schedule))return null;
 const coords=structuredClone(value.design_coordinates).map(row=>({...row,designableSelectionIds:(Array.isArray(row.designable_selection_ids)?row.designable_selection_ids:(row.designable_selection_id?[row.designable_selection_id]:[])).map(String)}));
 const blocks=structuredClone(value.design_freedom.blocks);
 const stages=value.schedule.map((row,index)=>{
  const adaptive=structuredClone(row.adaptive||{action:"recommend"});
  if(adaptive.multiphysics_limits&&!adaptive.multiphysicsLimits){adaptive.multiphysicsLimits=adaptive.multiphysics_limits;delete adaptive.multiphysics_limits;}
  return {...structuredClone(row),label:row.label||`Stage ${index+1}`,releasedBlocks:[...(row.released_blocks||row.releasedBlocks||[])],operatingPoints:[...(row.operating_points||row.operatingPoints||[0])],robustMode:row.robust_mode||row.robustMode||"nominal",transition:row.transition||"carry",validityLimits:structuredClone(row.validity_limits||row.validityLimits),adaptive};
 });
 if(!coords.length||!blocks.length||!stages.length)return null;
 if(coords.some(row=>!window.ImplexityDesignBounds?.valid(row.lower,row.upper)||!Number.isFinite(Number(row.step_scale??1))||Number(row.step_scale??1)<=0))return null;
 if(stages.some(row=>!String(row.label||"").trim()||!Number.isInteger(Number(row.iterations))||Number(row.iterations)<1||(row.fidelity&&!FIDELITIES.includes(row.fidelity))))return null;
 const rawMetric=value.update_metric;
 const updateMetric=String((rawMetric&&typeof rawMetric==="object"?rawMetric.mode:rawMetric)||"gradient_adaptive");
 if(!["global_max","gradient_adaptive","family_balanced"].includes(updateMetric))return null;
 const updateAlpha=updateMetric==="gradient_adaptive"?Number(rawMetric?.alpha??0.5):0.5;
 if(!Number.isFinite(updateAlpha)||updateAlpha<0||updateAlpha>1)return null;
 return {coords,blocks,stages,updateMetric,updateAlpha,selectionExplicit:value.design_coordinate_selection?.explicit!==false};
}
function declarationFrom(state){
 const selection=coordinateSelection(state);
 const updateMetric=state.updateMetric==="gradient_adaptive"?{mode:"gradient_adaptive",alpha:Number(state.updateAlpha)}:(state.updateMetric||"gradient_adaptive");
 const declaration={design_coordinates:state.coords.map(x=>({coordinate:x.coordinate,ref:x.ref,lower:structuredClone(x.lower),upper:structuredClone(x.upper),step_scale:Number(x.step_scale??1),...(x.designableSelectionIds?.length?{designable_selection_ids:x.designableSelectionIds.map(String),combine:"union"}:{})})),update_metric:updateMetric,inactive_design_coordinates:[...selection.inactive],design_coordinate_selection:{source:state.selectionExplicit?"explicit_gui":"all_provider_declared",explicit:Boolean(state.selectionExplicit),provider_declared:[...selection.declared],active:[...selection.active],inactive:[...selection.inactive]},design_freedom:{blocks:structuredClone(state.blocks)},schedule:state.stages.map(x=>({id:x.id,provider:x.provider,fidelity:x.fidelity,iterations:Number(x.iterations),released_blocks:[...x.releasedBlocks],operating_points:[...(x.operatingPoints||[0])],robust_mode:x.robustMode,transition:x.transition||"carry",validity_limits:x.validityLimits||undefined,adaptive:(()=>{const a=structuredClone(x.adaptive||{action:"recommend"});if(a.multiphysicsLimits){a.multiphysics_limits=a.multiphysicsLimits;delete a.multiphysicsLimits;}return a})()}))};
 const structure=currentModelStructure();
 if(structure)declaration.model_identity={structure_id:structure};
 return declaration;
}
function setDraft(state){
 const next=structuredClone(state);S.coords=next.coords;S.blocks=next.blocks;S.stages=next.stages;S.updateMetric=next.updateMetric||"gradient_adaptive";S.updateAlpha=Number.isFinite(Number(next.updateAlpha))?Number(next.updateAlpha):0.5;S.selectionExplicit=Boolean(next.selectionExplicit);
}
function draftState(){return {coords:S.coords,blocks:S.blocks,stages:S.stages,updateMetric:S.updateMetric,updateAlpha:S.updateAlpha,selectionExplicit:S.selectionExplicit}}
function persistedState(){
 try{
  const value=JSON.parse(localStorage.getItem("implexity.designFreedom"));
  const state=stateFromDeclaration(value);if(!state)return null;
  const stored=value?.model_identity?.structure_id?String(value.model_identity.structure_id):null;
  const current=currentModelStructure();
  if(current&&stored!==current){localStorage.removeItem?.("implexity.designFreedom");return null;}
  return {state,structure:stored||current,configured:true};
 }catch{return null}
}
function ensureCommitted(){
 const current=currentModelStructure();
 if(S.loaded){
  if(current&&S.modelStructure!==current){
   localStorage.removeItem?.("implexity.designFreedom");
   S.committed=defaultState();S.configured=false;S.modelStructure=current;S.providerSignature=providerSignature();setDraft(S.committed);
  }
  return;
 }
 const persisted=persistedState();
 S.committed=persisted?.state||defaultState();S.configured=Boolean(persisted?.configured);
 S.modelStructure=persisted?.structure||current||null;S.providerSignature=providerSignature();S.loaded=true;setDraft(S.committed);
}
function synchronizeProviderDefault(){
 ensureCommitted();
 const signature=providerSignature();
 if(S.configured||!signature||signature===S.providerSignature)return;
 S.committed=defaultState();S.modelStructure=currentModelStructure()||null;S.providerSignature=signature;S.loaded=true;setDraft(S.committed);
}
function initialize(){ensureCommitted();setDraft(S.committed)}
function discardDraft(){if(S.committed)setDraft(S.committed)}
function closeDraft(){discardDraft();if(S.dialog?.open)S.dialog.close()}
function commitDraft(){
 const candidate=structuredClone(draftState()),providerError=stageProviderError(candidate);
 if(providerError)throw new Error(providerError);
 candidate.selectionExplicit=true;
 const declaration=declarationFrom(candidate);
 localStorage.setItem("implexity.designFreedom",JSON.stringify(declaration));
 S.committed=candidate;S.loaded=true;S.configured=true;S.providerSignature=providerSignature();
 S.modelStructure=declaration.model_identity?.structure_id||currentModelStructure()||null;
 setDraft(S.committed);return declaration;
}
function ensure(){
 if(S.dialog)return S.dialog;
 const d=document.createElement("dialog");d.id="implexity-design-freedom";
 d.setAttribute("aria-labelledby","implexity-design-freedom-title");
 d.setAttribute("aria-describedby","implexity-design-freedom-description");
 d.style.cssText="width:min(1160px,94vw);max-height:90vh;padding:0;border:1px solid #ccd5df;border-radius:10px;background:#ffffff;color:#192536";
 d.addEventListener("close",discardDraft);
 document.body.append(d);S.dialog=d;return d;
}
function savedVolumeError(state){
 for(const coordinate of state?.coords||[]){
  const available=new Set(compatibleSavedRegions(coordinate).map(region=>region.id));
  if((coordinate.designableSelectionIds||[]).some(id=>!available.has(id)))return `${coordinateCopy(coordinate.coordinate).label}: saved volume unavailable on the current grid. Choose a replacement volume or explicitly use the entire eligible volume.`;
 }
 return "";
}
function coordinateRows(){
 return S.coords.map((r,i)=>{const copy=coordinateCopy(r.coordinate),regions=compatibleSavedRegions(r),selected=new Set(r.designableSelectionIds||[]),missing=[...selected].filter(id=>!regions.some(region=>region.id===id));return `<div class="implexity-df-row implexity-df-coordinate-row">
   <div><strong>${esc(copy.label)}</strong><code style="display:block;color:#596b80;font-size:11px">${esc(r.coordinate)}</code><div style="color:#506176;font-size:12px">${esc(copy.description)}</div>${missing.length?'<p role="alert">Saved volume unavailable for the current grid. Select a new volume, or explicitly choose “Use entire eligible volume”. The saved restriction will not be discarded automatically.</p>':""}</div>
   <label class="implexity-df-binding">Model parameter reference<input data-c="${i}" data-k="ref" value="${esc(r.ref)}" aria-label="Model parameter reference for ${esc(human(r.coordinate))}" placeholder="Authoritative model parameter reference"></label>
   <div><label style="font-size:12px">Where may this coordinate change?<select multiple size="3" data-c="${i}" data-k="designableSelectionIds" aria-label="Saved optimisable volumes for ${esc(copy.label)}"><option value="" disabled ${selected.size?"":"selected"}>Entire eligible volume (default)</option>${regions.map(region=>`<option value="${esc(region.id)}" ${selected.has(region.id)?"selected":""}>${esc(region.label)} · ${esc(region.count)} cells</option>`).join("")}${!regions.length?'<option value="" disabled>No saved cell volumes on this grid</option>':""}${missing.map(id=>`<option value="${esc(id)}" selected disabled>Unavailable saved region · ${esc(id)}</option>`).join("")}</select></label><button type="button" data-volume-select="${i}" ${coordinateField(r)?"":"disabled"}>Select a volume in the viewer</button><button type="button" data-volume-all="${i}">Use entire eligible volume</button><p style="font-size:12px">${coordinateField(r)?"Select interior cells, name the selection, then return here and choose it. Multiple selections are combined. Cells outside stay fixed; protected cells remain fixed inside too.":"This coordinate is not bound to an editable cell grid. Its parameter bounds apply globally; a boundary surface cannot restrict it."}</p></div>
   ${Array.isArray(r.lower)||Array.isArray(r.upper)?`<div class="implexity-component-bounds"><strong>Per-component bounds retained</strong><p>The full native tensor remains authoritative. It is not replaced by one scalar range.</p><details><summary>Inspect exact bounds</summary><pre>${esc(JSON.stringify({lower:r.lower,upper:r.upper},null,2))}</pre></details></div>`:`<label>Lower bound<input data-c="${i}" data-k="lower" aria-label="Lower bound for ${esc(human(r.coordinate))}" title="Lower bound" type="number" required step="any" value="${esc(r.lower)}"></label><label>Upper bound<input data-c="${i}" data-k="upper" aria-label="Upper bound for ${esc(human(r.coordinate))}" title="Upper bound" type="number" required step="any" value="${esc(r.upper)}"></label>`}
 </div>`}).join("");
}
function blockRows(){
 return S.blocks.map((r,i)=>`<div class="implexity-df-block-row">
   <div><strong>${esc(human(r.id))}</strong><div style="color:#596b80;font-size:12px">${esc(r.coordinates.map(human).join(", "))}</div></div>
   <select data-b="${i}" data-k="role" aria-label="Design role for ${esc(human(r.id))}">${ROLES.map(x=>`<option value="${x}" ${x===r.role?"selected":""}>${esc(human(x))}</option>`).join("")}</select>
   <div style="color:#506176">${r.role==="manual_locked"?"Changed only by the user":"Physics remains active even when this block is frozen."}</div>
 </div>`).join("");
}
function stageRows(){
 const ps=providerEntries().map(item=>item.id||item.name);
 return S.stages.map((r,i)=>{const stageLabel=r.label||human(r.id)||`Stage ${i+1}`,providerUnavailable=r.provider&&!ps.includes(r.provider);return `<section class="implexity-df-stage" aria-label="Stage ${i+1}: ${esc(stageLabel)}">
   <div class="implexity-df-stage-grid">
    <label>Stage name<input data-s="${i}" data-k="label" aria-label="Name for stage ${i+1}" value="${esc(stageLabel)}"></label>
    <label>Physics provider<select data-s="${i}" data-k="provider" aria-label="Physics provider for ${esc(stageLabel)}">${providerUnavailable?`<option value="${esc(r.provider)}" selected disabled>Unavailable saved provider: choose another</option>`:""}${ps.map(x=>`<option value="${esc(x)}" ${x===r.provider?"selected":""}>${esc(providerLabel(x))}</option>`).join("")}</select></label>
    <label>Fidelity<select data-s="${i}" data-k="fidelity" aria-label="Fidelity for ${esc(stageLabel)}"><option value="" ${!r.fidelity?"selected":""}>Use provider default</option>${FIDELITIES.map(x=>`<option value="${x}" ${x===r.fidelity?"selected":""}>${esc(human(x))}</option>`).join("")}</select></label>
    <label>Iterations<input data-s="${i}" data-k="iterations" aria-label="Iterations for ${esc(stageLabel)}" type="number" required min="1" step="1" value="${esc(r.iterations)}"></label>
   </div>
   <div class="implexity-df-release"><strong>Design freedom in this stage</strong><div class="implexity-df-release-list">${S.blocks.map(b=>`<label><input type="checkbox" data-release-stage="${i}" data-release-block="${esc(b.id)}" ${r.releasedBlocks.includes(b.id)?"checked":""}>${esc(human(b.id))}</label>`).join("")}</div></div>
   <div class="implexity-df-transition-grid">
    <label>Robustness<select data-s="${i}" data-k="robustMode" aria-label="Robustness for ${esc(stageLabel)}"><option value="nominal" ${r.robustMode==="nominal"?"selected":""}>Nominal</option><option value="expected" ${r.robustMode==="expected"?"selected":""}>Expected performance</option><option value="smooth_worst_case" ${r.robustMode==="smooth_worst_case"?"selected":""}>Smooth worst case</option></select></label>
    <label>Physics-state transition<select data-s="${i}" data-k="transition" aria-label="Physics-state transition for ${esc(stageLabel)}"><option value="carry" ${r.transition!=="reinitialize_physics"?"selected":""}>Carry existing physical state where supported</option><option value="reinitialize_physics" ${r.transition==="reinitialize_physics"?"selected":""}>Reinitialize the new physical state</option></select></label>
    <label>Adaptive fidelity<select data-adaptive="${i}" aria-label="Adaptive fidelity for ${esc(stageLabel)}"><option value="recommend" ${(r.adaptive?.action||"recommend")==="recommend"?"selected":""}>Recommend transition</option><option value="manual_only" ${r.adaptive?.action==="manual_only"?"selected":""}>Manual transition only</option><option value="auto_authorized" ${r.adaptive?.action==="auto_authorized"?"selected":""}>Automatically transition when qualified</option></select></label>
   </div>
   <details style="margin-top:10px"><summary>Adaptive qualification criteria</summary>
    <div class="implexity-df-limit-grid">
     <label>Minimum temperature margin, K<input data-limit-stage="${i}" data-limit="temperature_margin_K" data-bound="min" aria-label="Minimum temperature margin for ${esc(stageLabel)}, K" type="number" step="any" value="${esc(r.adaptive?.multiphysicsLimits?.temperature_margin_K?.min??"")}"></label>
     <label>Maximum stress utilisation<input data-limit-stage="${i}" data-limit="stress_utilization" data-bound="max" aria-label="Maximum stress utilisation for ${esc(stageLabel)}" type="number" step="any" value="${esc(r.adaptive?.multiphysicsLimits?.stress_utilization?.max??"")}"></label>
     <label>Minimum cooling margin, K<input data-limit-stage="${i}" data-limit="cooling_margin_K" data-bound="min" aria-label="Minimum cooling margin for ${esc(stageLabel)}, K" type="number" step="any" value="${esc(r.adaptive?.multiphysicsLimits?.cooling_margin_K?.min??"")}"></label>
     <label>Maximum accumulated cycle damage<input data-limit-stage="${i}" data-limit="maximum_cycle_damage" data-bound="max" aria-label="Maximum accumulated cycle damage for ${esc(stageLabel)}" type="number" step="any" value="${esc(r.adaptive?.multiphysicsLimits?.maximum_cycle_damage?.max??"")}"></label>
    </div>
   </details>
 </section>`}).join("");
}
function render(){
 const d=ensure();
 const selection=coordinateSelection();
 const selectionText=selection.error||(!selection.inactive.length
  ?`All ${selection.declared.length} provider-declared design coordinate${selection.declared.length===1?" is":"s are"} active by default.`
  :`Active: ${selection.active.map(human).join(", ")}. Explicitly inactive: ${selection.inactive.map(human).join(", ")}.`);
 d.innerHTML=`<style>${DIALOG_CSS}</style><div class="implexity-df-head"><div class="implexity-df-title-copy"><h2 id="implexity-design-freedom-title" style="margin:0">Optimisation scope</h2><p id="implexity-design-freedom-description" style="margin:6px 0 0;color:#506176">Manual editing and direct-gradient optimisation act on the same implicit design. Freeze or release design blocks as the engineering workflow develops.</p></div><button type="button" data-close aria-label="Cancel changes to optimisation scope">Cancel</button></div>
 <div class="implexity-df-body">
 <h3>Choose the volume that may change</h3><p data-design-coordinate-selection style="color:#506176">${esc(selectionText)} A saved region limits where a family may react; it does not disable that family. Only selections on that coordinate's exact registered grid are offered-no hidden resampling. Unselected means the full designable domain. Fixed/protected cells always remain immutable.</p>${coordinateRows()}<details><summary>Advanced: update scaling</summary><div style="display:grid;grid-template-columns:minmax(220px,1fr) minmax(150px,.55fr);gap:10px;max-width:680px;margin:10px 0 14px"><label>Cross-family update scaling<select data-update-metric aria-label="Cross-family update scaling"><option value="global_max" ${S.updateMetric==="global_max"?"selected":""}>Established global scaling</option><option value="gradient_adaptive" ${S.updateMetric==="gradient_adaptive"?"selected":""}>Gradient-adaptive (recommended)</option><option value="family_balanced" ${S.updateMetric==="family_balanced"?"selected":""}>Fully family-balanced</option></select></label><label>Adaptive strength α<input data-update-alpha aria-label="Gradient-adaptive strength" type="number" min="0" max="1" step="0.05" value="${esc(S.updateAlpha)}" ${S.updateMetric!=="gradient_adaptive"?"disabled":""}></label><span style="grid-column:1/-1;display:block;color:#506176;font-size:12px">Adaptive α=0 keeps raw/global proportionality; α=0.5 uses square-root balancing; α=1 fully balances family gradient scales. Provider step scales and local patterns remain intact; move limits, coordinate boxes and exact line search remain authoritative.</span></div></details>
 <details><summary>Advanced: design blocks and staged optimisation</summary><h3>Design blocks</h3>${blockRows()}
 <h3>Regime and fidelity schedule</h3>${stageRows()}
 <div class="implexity-df-actions"><button type="button" data-add-stage>Add stage</button><button type="button" data-template>Use Chamber → cooling → coupled template</button></div></details>
 <div class="implexity-df-error" data-design-freedom-error role="alert" aria-live="polite" hidden></div>
 </div><div class="implexity-df-actions"><button type="button" class="implexity-df-apply" data-apply>Apply optimisation scope</button></div>`;
 d.querySelector("[data-close]").onclick=closeDraft;
 d.querySelectorAll("[data-volume-all]").forEach(button=>button.onclick=()=>{const error=syncVisibleState(d);if(error)return showError(d.querySelector("[data-design-freedom-error]"),error);S.coords[+button.dataset.volumeAll].designableSelectionIds=[];render()});
 d.querySelectorAll("[data-volume-select]").forEach(button=>button.onclick=async()=>{try{const error=syncVisibleState(d);if(error)throw new Error(error);const row=S.coords[+button.dataset.volumeSelect],target=coordinateField(row);if(!target)throw new Error("Create an editable topology field first.");if(!window.ImplexityInteraction?.prepareVolumeSelection)throw new Error("The viewport volume selector is unavailable.");await window.ImplexityInteraction.prepareVolumeSelection(target.fieldId,`${coordinateCopy(row.coordinate).label} volume`);S.viewportDraft=true;d.close()}catch(error){showError(d.querySelector("[data-design-freedom-error]"),error.message)}});
 d.querySelector("[data-update-metric]").onchange=e=>{S.updateMetric=e.target.value;const alpha=d.querySelector("[data-update-alpha]");if(alpha)alpha.disabled=S.updateMetric!=="gradient_adaptive"};
 d.querySelector("[data-update-alpha]").onchange=e=>{const value=numericValue(e.target,{min:0});if(value.valid&&value.value<=1)S.updateAlpha=value.value};
 const finite=(input,label,options={})=>{const result=numericValue(input,options),message=result.valid?"":`${label} must be ${options.integer?`a whole number${options.min!==undefined?` of at least ${options.min}`:""}`:"a finite number"}.`;setControlValidity(input,result.valid,message);if(!result.valid){showError(d.querySelector("[data-design-freedom-error]"),message);return null}return result.value};
 d.querySelectorAll("[data-c]").forEach(e=>e.onchange=()=>{const r=S.coords[+e.dataset.c],k=e.dataset.k;if(k==="lower"||k==="upper"){const value=finite(e,controlLabel(e,`${human(k)} bound`));if(value!==null)r[k]=value}else if(k==="designableSelectionIds"){r[k]=[...e.selectedOptions].map(option=>option.value).filter(Boolean);const warning=e.closest(".implexity-df-coordinate-row")?.querySelector('[role="alert"]');if(warning)warning.hidden=!savedVolumeError({coords:[r]});}else r[k]=e.value});
 d.querySelectorAll("[data-b]").forEach(e=>e.onchange=()=>S.blocks[+e.dataset.b][e.dataset.k]=e.value);
 d.querySelectorAll("[data-s]").forEach(e=>e.onchange=()=>{const r=S.stages[+e.dataset.s],k=e.dataset.k;if(k==="label")r.label=e.value;else if(k==="iterations"){const value=finite(e,controlLabel(e,"Iterations"),{integer:true,min:1});if(value!==null)r[k]=value}else r[k]=e.value});
 d.querySelectorAll("[data-release-stage]").forEach(e=>e.onchange=()=>{const r=S.stages[+e.dataset.releaseStage],id=e.dataset.releaseBlock;r.releasedBlocks=e.checked?[...new Set([...r.releasedBlocks,id])]:r.releasedBlocks.filter(x=>x!==id)});
 d.querySelectorAll("[data-adaptive]").forEach(e=>e.onchange=()=>{const r=S.stages[+e.dataset.adaptive];r.adaptive={...(r.adaptive||{}),action:e.value}});
 d.querySelectorAll("[data-limit-stage]").forEach(e=>e.onchange=()=>{const r=S.stages[+e.dataset.limitStage];const key=e.dataset.limit,bound=e.dataset.bound;r.adaptive={...(r.adaptive||{}),multiphysicsLimits:{...(r.adaptive?.multiphysicsLimits||{})}};if(e.value.trim()===""){setControlValidity(e,true);delete r.adaptive.multiphysicsLimits[key];return;}const value=finite(e,controlLabel(e,human(key)));if(value!==null)r.adaptive.multiphysicsLimits[key]={...(r.adaptive.multiphysicsLimits[key]||{}),[bound]:value};});
 d.querySelector("[data-add-stage]").onclick=()=>{S.stages.push({id:`stage_${S.stages.length+1}`,label:`Stage ${S.stages.length+1}`,provider:provider(),fidelity:"",iterations:20,releasedBlocks:[],operatingPoints:[0],robustMode:"nominal",transition:"carry",adaptive:{action:"recommend"}});render()};
 d.querySelector("[data-template]").onclick=()=>{const chamber=S.blocks.find(x=>x.coordinates.includes("model:control"))?.id,cooling=S.blocks.find(x=>x.role==="cooling_only")?.id;S.stages=[
  {id:"chamber",label:"Chamber design",provider:provider(),fidelity:"screening",iterations:40,releasedBlocks:[chamber].filter(Boolean),operatingPoints:[0],robustMode:"nominal",transition:"carry",adaptive:{action:"recommend"}},
  {id:"cooling",label:"Cooling design",provider:provider(),fidelity:"screening",iterations:30,releasedBlocks:[cooling].filter(Boolean),operatingPoints:[0],robustMode:"nominal",transition:"carry",adaptive:{action:"recommend"}},
  {id:"coupled",label:"Coupled refinement",provider:provider(),fidelity:"screening",iterations:40,releasedBlocks:[chamber,cooling].filter(Boolean),operatingPoints:[0],robustMode:"expected",transition:"carry",adaptive:{action:"recommend"}}];render()};
 d.querySelector("[data-apply]").onclick=()=>{const error=syncVisibleState(d)||validate();const box=d.querySelector("[data-design-freedom-error]");if(error){showError(box,error);d.querySelector('[aria-invalid="true"]')?.focus();return}try{const declaration=commitDraft();box.hidden=true;box.replaceChildren();d.close();window.dispatchEvent(new CustomEvent("implexity:design-freedom",{detail:structuredClone(declaration)}))}catch(error){showError(box,"The design-freedom hierarchy could not be saved. Check browser storage access and try again.",error)}};
}
function syncVisibleState(dialog){
 const error=visibleValidation(dialog);if(error)return error;
 S.updateMetric=dialog.querySelector("[data-update-metric]")?.value||"gradient_adaptive";
 S.updateAlpha=numericValue(dialog.querySelector("[data-update-alpha]"),{min:0}).value;
 dialog.querySelectorAll("[data-c]").forEach(e=>{const r=S.coords[+e.dataset.c],k=e.dataset.k;r[k]=(k==="lower"||k==="upper")?numericValue(e).value:k==="designableSelectionIds"?[...e.selectedOptions].map(option=>option.value).filter(Boolean):e.value});
 dialog.querySelectorAll("[data-b]").forEach(e=>S.blocks[+e.dataset.b][e.dataset.k]=e.value);
 dialog.querySelectorAll("[data-s]").forEach(e=>{const r=S.stages[+e.dataset.s],k=e.dataset.k;r[k]=k==="iterations"?numericValue(e,{integer:true,min:1}).value:e.value});
 dialog.querySelectorAll("[data-release-stage]").forEach(e=>{const r=S.stages[+e.dataset.releaseStage],block=e.dataset.releaseBlock;r.releasedBlocks=e.checked?[...new Set([...r.releasedBlocks,block])]:r.releasedBlocks.filter(id=>id!==block)});
 dialog.querySelectorAll("[data-adaptive]").forEach(e=>{const r=S.stages[+e.dataset.adaptive];r.adaptive={...(r.adaptive||{}),action:e.value}});
 dialog.querySelectorAll("[data-limit-stage]").forEach(e=>{const r=S.stages[+e.dataset.limitStage],key=e.dataset.limit,bound=e.dataset.bound,value=numericValue(e,{optional:true}).value;r.adaptive={...(r.adaptive||{}),multiphysicsLimits:{...(r.adaptive?.multiphysicsLimits||{})}};if(value===null)delete r.adaptive.multiphysicsLimits[key];else r.adaptive.multiphysicsLimits[key]={...(r.adaptive.multiphysicsLimits[key]||{}),[bound]:value}});
 return "";
}
function validate(){
 const visibleError=visibleValidation();if(visibleError)return visibleError;
 const selection=coordinateSelection();if(selection.error)return selection.error;
 if(!S.coords.length)return "At least one provider-declared design coordinate must remain active.";
 if(!["global_max","gradient_adaptive","family_balanced"].includes(S.updateMetric))return "Choose a supported cross-family update scaling.";
 if(!Number.isFinite(Number(S.updateAlpha))||Number(S.updateAlpha)<0||Number(S.updateAlpha)>1)return "Gradient-adaptive strength must be between 0 and 1.";
 const volumeError=savedVolumeError(draftState());if(volumeError)return volumeError;
 for(const coordinate of S.coords){if(!window.ImplexityDesignBounds?.valid(coordinate.lower,coordinate.upper))return `${human(coordinate.coordinate)} requires a finite lower bound below its upper bound.`;if(!Number.isFinite(Number(coordinate.step_scale??1))||Number(coordinate.step_scale??1)<=0)return `${human(coordinate.coordinate)} requires a positive finite step scale.`;}
 const providerError=stageProviderError(draftState());if(providerError)return providerError;
 for(const stage of S.stages){if(!String(stage.label||"").trim())return "Every stage requires a name.";if(!Number.isInteger(Number(stage.iterations))||Number(stage.iterations)<1)return `${stage.label||human(stage.id)} requires at least one whole iteration.`;if(stage.fidelity&&!FIDELITIES.includes(stage.fidelity))return `${stage.label||human(stage.id)} uses an unsupported fidelity.`;}
 return "";
}
function clearSavedHierarchy(){
 ensureCommitted();
 localStorage.removeItem("implexity.designFreedom");
 S.committed=defaultState();S.configured=false;S.modelStructure=currentModelStructure()||null;S.providerSignature=providerSignature();S.loaded=true;
 setDraft(S.committed);if(S.dialog?.open)S.dialog.close();refreshAvailability();
 workbench()?.renderReadiness?.();
 window.dispatchEvent?.(new CustomEvent("implexity:design-freedom-cleared",{detail:{modelStructure:S.modelStructure}}));
 return true;
}
const api={
 open(){refreshAvailability();if(!hierarchySupported())return false;if(S.viewportDraft)S.viewportDraft=false;else initialize();render();ensure().showModal();return true},
 getDeclaration(){ensureCommitted();const error=stageProviderError(S.committed)||savedVolumeError(S.committed)||coordinateSelection(S.committed).error;if(error)throw new Error(error);return structuredClone(declarationFrom(S.committed))},
 isConfigured(){ensureCommitted();return S.configured},
 validationError(){ensureCommitted();if(!provider()&&!S.configured)return "";return stageProviderError(S.committed)||savedVolumeError(S.committed)||coordinateSelection(S.committed).error},
 clear(options={}){if(options.confirm&&window.confirm&&!window.confirm("Clear the saved hierarchy for this model? This removes its staged design-freedom schedule."))return false;return clearSavedHierarchy()},
 refreshAvailability,
 load(){const loaded=persistedState();if(!loaded)return false;S.committed=loaded.state;S.configured=true;S.modelStructure=loaded.structure||currentModelStructure()||null;S.loaded=true;setDraft(S.committed);if(S.dialog?.open)render();refreshAvailability();return true}
};
window.ImplexityDesignFreedom=api;
function refreshAvailability(){
 const card=S.card;if(!card)return;
 synchronizeProviderDefault();
 const supported=hierarchySupported(),configure=card.querySelector("[data-design-freedom-configure]"),clear=card.querySelector("[data-design-freedom-clear]"),copy=card.querySelector("p");
 const providerError=S.configured?stageProviderError(S.committed):"";
 const volumeError=S.configured?savedVolumeError(S.committed):"";
 const selection=coordinateSelection(S.committed);
 configure.disabled=!supported;configure.setAttribute("aria-disabled",String(!supported));
 configure.title=supported?"":"Hierarchical topology schedules require an array-level physics provider.";
 clear.hidden=!S.configured;
 copy.textContent=providerError||volumeError||selection.error||(!supported
  ?(S.configured?"A saved hierarchy cannot run with the LEGACY_MULTIPHYSICS parametric job runtime. Clear it here or select a compatible topology physics provider.":"The LEGACY_MULTIPHYSICS parametric job runtime does not execute hierarchical topology schedules. Select a compatible topology physics provider to configure stages; nothing will be silently ignored.")
  :window.ImplexityDesignBounds?.mappedControl(problem())&&selection.active.length===1&&selection.active[0]==="model:control"
   ?"Authoritative native design: model:control. Density and phase fields are derived outputs, not independent optimization coordinates."
   :selection.inactive.length
   ?`Explicit subset active. Inactive provider coordinates: ${selection.inactive.map(human).join(", ")}.`
   :`All ${selection.declared.length} provider-declared design coordinate${selection.declared.length===1?" is":"s are"} active by default.`);
 card.dataset.available=String(supported);
 card.dataset.invalid=String(Boolean(providerError||volumeError||selection.error||(!supported&&S.configured)));
}
document.addEventListener("DOMContentLoaded",()=>{const host=document.querySelector("#s_opt .sect-b");if(host&&!host.querySelector("[data-design-freedom]")){const card=document.createElement("section");card.dataset.designFreedom="1";card.className="implexity-design-freedom-card";card.innerHTML='<div class="implexity-design-freedom-copy"><strong>Optimisation scope</strong><p aria-live="polite">Choose which volume and parameters may change. Advanced staged settings remain available.</p></div><div class="implexity-design-freedom-actions"><button type="button" data-design-freedom-configure>Choose optimisable volume and parameters</button><button type="button" data-design-freedom-clear hidden>Clear saved hierarchy</button></div>';card.querySelector("[data-design-freedom-configure]").onclick=()=>api.open();card.querySelector("[data-design-freedom-clear]").onclick=()=>{try{api.clear({confirm:true})}catch(error){const copy=card.querySelector("p");copy.textContent="The saved hierarchy could not be cleared. Check browser storage access and try again.";card.dataset.invalid="true"}};S.card=card;host.prepend(card);refreshAvailability()}})
window.addEventListener?.("implexity:provider-selection-changed",refreshAvailability);
window.addEventListener?.("implexity:provider-catalogue-changed",refreshAvailability);
window.addEventListener?.("implexity:physics-packages-changed",refreshAvailability);
function refreshSelections(){if(S.dialog?.open)render();refreshAvailability();workbench()?.renderReadiness?.();}
window.addEventListener?.("implexity:selection-updated",refreshSelections);
window.addEventListener?.("implexity:selections-hydrated",refreshSelections);
})();
