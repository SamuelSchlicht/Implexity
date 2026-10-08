// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(()=>{
"use strict";

const describe=(id,metadata={})=>window.ImplexityText?.present?.(id,metadata)||({
 id:String(id??""),
 label:String(metadata.label||metadata.display_name||metadata.title||id||""),
 unit:String(metadata.unit||metadata.units||""),
 description:String(metadata.description||metadata.help||"")
});
const human=(id,metadata={})=>window.ImplexityText?.humanize?.(id,metadata)||describe(id,metadata).label;
const escapeHTML=value=>String(value??"").replace(/[&<>"']/g,char=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[char]));
const FACES=["x_min","x_max","y_min","y_max","z_min","z_max"];
const PORT_QUANTITIES={volume_flow:{label:"Outward volume flow",unit:"m³/s"},mass_flow:{label:"Outward mass flow",unit:"kg/s"},mean_normal_velocity:{label:"Mean outward normal velocity",unit:"m/s"}};
const portResponse=id=>{
 for(const [quantity,metadata] of Object.entries(PORT_QUANTITIES))for(const face of FACES)if(id===`port_${quantity}_${face}`)return {...metadata,face,label:`${metadata.label} · ${face.replace("_"," ")}`};
 return null;
};
const $=(selector,root=document)=>root.querySelector(selector);
const el=(tag,className,text)=>{const node=document.createElement(tag);if(className)node.className=className;if(text!=null)node.textContent=text;return node};
const fetchJSON=async(url,options={})=>{const response=await fetch(url,{headers:{"content-type":"application/json",...(options.headers||{})},...options});const data=await response.json();if(!response.ok){const details=[data.message||data.error||response.statusText,...(Array.isArray(data.problems)?data.problems:[])];throw new Error(details.map(value=>typeof value==="string"?value:JSON.stringify(value)).join("\n"))}return data};
const failureSummary=(error,fallback)=>String(error?.message||"").includes("provider topology registration:")?"CFD needs a matching cell-centred geometry grid. Check its grid type, origin, extent and cell counts in Domain.":fallback;
const renderMessage=(host,summary,technical,severity="error")=>{
 const presenter=window.ImplexityUIPresentation;
 if(presenter?.render)return presenter.render(host,{summary,technical,severity});
 host.replaceChildren(el("span","implexity-ui-summary",summary));host.dataset.severity=severity;host.setAttribute("role",severity==="error"?"alert":"status");return summary;
};
const serviceSummary=severity=>({error:"Physical preflight found a condition that must be corrected.",warning:"Physical preflight completed with a warning.",success:"A physical preflight check passed.",info:"Physical preflight provided additional guidance."})[severity]||"Physical preflight returned additional information.";

class CFDWorkspace{
 constructor(viewer,options={}){
  this.viewer=viewer;this.options=options||{};this.step=0;this.catalog=null;this.loadError=null;this.loading=false;this.saving=false;this.providerId="resolved_stokes_brinkman";
  this.problem=this.defaultProblem();this.serviceIssues=[];this.validationIssues=new Map();this.invalidInputs=new Map();
  this.root=this.build();if(this.options.embedded)this.root.classList.add("implexity-cfd-embedded");
  (this.options.host||document.body).append(this.root);this.bind();if(this.options.embedded)this.root.hidden=false;
 }
 defaultProblem(){return {schema:"implexity-cfd-problem/3",topology_parameter:"model:control",fluid:{name:"Water",density_kg_m3:997,dynamic_viscosity_Pa_s:8.9e-4,reference_temperature_K:298.15},domain:{origin_m:[0,0,0],extent_m:[.1,.05,.02],cells:[40,20,12],reference_length_m:.02,reference_velocity_m_s:.2,gravity_m_s2:[0,0,0]},boundaries:FACES.map(face=>({face,kind:face==="x_min"?"velocity":face==="x_max"?"pressure":"no_slip",velocity_m_s:face==="x_min"?[.2,0,0]:null,static_pressure_Pa:face==="x_max"?0:null,port_buffer_cells:2,enabled:true})),gauge:{mode:"mean_zero",value_Pa:0},brinkman:{fluid_permeability_m2:1e6,solid_permeability_m2:1e-12,ramp_q:8,continuation:[.02,.1,.35,1],minimum_fluid_fraction:1e-4},solver:{model:"stokes_brinkman",convection_scheme:"smooth_upwind",nonlinear_tolerance:1e-8,linear_tolerance:1e-10,adjoint_tolerance:1e-9,maximum_nonlinear_iterations:50,maximum_linear_iterations:800,pressure_stabilization:0,use_matrix_free:true},objectives:[{response:"pumping_power",sense:"minimize",weight:1,scale:1,inlet_face:"x_min",outlet_face:"x_max"}],thermal:{enabled:false,volumetric_heat_W_m3:0}}}
 build(){
  const root=el("section","implexity-cfd-shell");root.hidden=true;
  root.setAttribute("aria-labelledby","implexity-cfd-workspace-title");
  root.innerHTML=`<header class="implexity-cfd-head"><span class="implexity-cfd-title" id="implexity-cfd-workspace-title">Resolved CFD & topology optimisation</span><span class="implexity-cfd-badge">3D · SI units</span><button type="button" data-close title="Close CFD setup" aria-label="Close CFD setup">×</button></header><main class="implexity-cfd-body"><div class="implexity-cfd-steps" aria-label="CFD setup steps"></div><div data-page></div><div data-issues role="status" aria-live="polite" aria-atomic="true"></div></main><footer class="implexity-cfd-footer"><button type="button" data-back>Back</button><button type="button" data-check>Physical preflight</button><button type="button" class="implexity-cfd-primary" data-next>Next</button></footer>`;
  return root;
 }
 bind(){
  this.root.addEventListener("click",event=>{const pick=event.target.closest?.("[data-pick-face]");if(pick)this.selectFace(pick.dataset.pickFace);if(event.target.closest?.("[data-match-geometry-grid]"))this.matchGeometryGrid();});
  this.root.addEventListener("input",event=>this.read(event.target));
  $("[data-close]",this.root).onclick=()=>this.close();$("[data-back]",this.root).onclick=()=>this.go(this.step-1);
  $("[data-next]",this.root).onclick=()=>this.step<6?this.go(this.step+1):this.optimise();
  $("[data-check]",this.root).onclick=()=>this.preflight();
  document.addEventListener("implexity:cfd-open",()=>this.open());document.addEventListener("implexity:surface-picked",event=>this.assignPickedFace(event.detail));
 }
 async open(){
  this.root.hidden=false;if(this.loading)return;this.loading=true;this.render();
  try{
   const catalogue=await fetchJSON("/v1/implicit/cae/catalogue");
   const catalog=catalogue.providers?.[this.providerId];
   if(!catalog||!Array.isArray(catalog.analyses)||!catalog.analyses.includes("stokes_brinkman"))throw new Error("The service does not advertise the supported Stokes flow provider.");
   const persistence=window.ImplexityProviderProblemPersistence;
   if(!persistence?.unpack)throw new Error("Provider problem persistence is unavailable.");
   const active=persistence.unpack(await fetchJSON("/v1/implicit/problem"));
   this.catalog=catalog;this.activeProviderId=active.providerId;
   if(active.providerId===this.providerId){this.problem=structuredClone(active.problem);this.invalidInputs.clear();this.validationIssues.clear();}
   this.loadError=null;this.showIssues([]);
  }catch(error){this.loadError=error;this.showIssues([{severity:"error",message:"The stored physics setup could not be loaded safely. Saving is blocked; resolve the error and reopen this editor.",technical:error}],{trusted:true})}
  finally{this.loading=false;this.render()}
 }
 close(){this.root.hidden=true}
 go(next){
  if(next>this.step&&!this.validateVisibleControls())return;
  this.step=Math.max(0,Math.min(6,next));this.render();
  const heading=$("[data-page] h3",this.root);
  if(heading){heading.tabIndex=-1;heading.focus({preventScroll:true});heading.scrollIntoView({block:"start"});}
 }
 render(){
  const names=["Model","Fluid","Domain","Boundaries","Topology","Objectives","Review"],steps=$(".implexity-cfd-steps",this.root);steps.replaceChildren();
  names.forEach((name,index)=>{const button=el("button","implexity-cfd-step",`${index+1} ${name}`);button.type="button";button.setAttribute("aria-current",index===this.step?"step":"false");button.onclick=()=>this.go(index);steps.append(button)});
  $("[data-page]",this.root).innerHTML=this.page();$("[data-back]",this.root).disabled=this.step===0||this.loading||this.saving;$("[data-next]",this.root).textContent=this.step===6?"Validate and save CFD setup":"Next";
  if(this.step===2){const grid=$(".implexity-cfd-grid",this.root),button=el("button","implexity-cfd-match-grid","Use geometry grid");button.type="button";button.dataset.matchGeometryGrid="";grid.before(button);}
  this.root.querySelectorAll("[data-path], [data-next], [data-check], [data-match-geometry-grid], .implexity-cfd-step").forEach(control=>control.disabled=Boolean(this.loading||this.saving||this.loadError));
 }
 async matchGeometryGrid(){
  if(this.loading||this.saving||this.loadError)return;
  this.loading=true;this.render();
  try{
   const state=await fetchJSON("/v1/implicit/model"),doc=state.document,node=doc?.nodes?.[doc.root],p=node?.params;
   const vector=value=>Array.isArray(value)&&value.length===3&&value.every(x=>typeof x==="number"&&Number.isFinite(x));
   const samples=p?.samples,shape=doc?.arrays?.[samples?.array]?.shape||(Array.isArray(samples)?[samples.length,samples[0]?.length,samples[0]?.[0]?.length]:null);
   if(!state.loaded||(doc.units||"mm")!=="mm"||node?.kind!=="cell_grid_field"||!vector(p?.origin)||!vector(p?.spacing)||p.spacing.some(x=>x<=0)||!vector(shape)||shape.some(x=>!Number.isInteger(x)||x<2))throw new Error("A root cell-centred geometry field with explicit origin, spacing and sample dimensions is required.");
   const origin=p.origin.map(x=>x/1000),extent=p.spacing.map((x,i)=>x*shape[i]/1000);
   if(!vector(origin)||!vector(extent)||extent.some(x=>x<=0))throw new Error("The geometry grid cannot be represented in CFD domain units.");
   Object.assign(this.problem.domain,{origin_m:origin,extent_m:extent,cells:[...shape]});
   for(const path of this.validationIssues.keys())if(/^domain\.(origin_m|extent_m|cells)\./.test(path)){this.validationIssues.delete(path);this.invalidInputs.delete(path);}
   this.showIssues([{severity:"info",message:"Domain copied from the geometry grid. Review boundary conditions and reference values before saving."}],{trusted:true});
   window.dispatchEvent(new CustomEvent("implexity:cfd-problem-draft",{detail:{providerId:this.providerId}}));
  }catch(error){this.showIssues([{severity:"error",message:"The domain was not changed. Use a root cell-centred geometry grid with explicit dimensions.",technical:error}],{trusted:true})}
  finally{this.loading=false;this.render()}
 }
 controlId(path){return `implexity-cfd-${String(path).replace(/[^A-Za-z0-9]+/g,"-")}`}
 field(label,path,value,unit,type="number",attributes=""){
  if(unit==="selected response unit"){
   const index=String(path).match(/^objectives\.(\d+)\./)?.[1];
   const response=this.problem.objectives?.[Number(index)]?.response;
   unit=portResponse(response)?.unit||({pumping_power:"W",pressure_drop:"Pa",volume_flow:"m³/s"})[response]||unit;
  }
  const id=this.controlId(path),numberAttributes=type==="number"?`${/\bstep=/.test(attributes)?"":"step=\"any\" "}required `:"";
  const boundary=String(path).match(/^boundaries\.(\d+)\./),face=boundary?this.problem.boundaries[Number(boundary[1])]?.face:null;
  const objective=String(path).match(/^objectives\.(\d+)\./);
  const accessibleLabel=`${face?human(face)+": ":objective?`Response ${Number(objective[1])+1}: `:""}${label}${unit?" ["+unit+"]":""}`;
  const displayed=this.invalidInputs.has(path)?this.invalidInputs.get(path):value;
  return `<label for="${escapeHTML(id)}">${escapeHTML(label)}<div class="implexity-cfd-unit">${escapeHTML(unit||"")}</div></label><input id="${escapeHTML(id)}" data-path="${escapeHTML(path)}" aria-label="${escapeHTML(accessibleLabel)}" ${this.invalidInputs.has(path)?'aria-invalid="true" ':""}type="${escapeHTML(type)}" ${numberAttributes}${attributes} value="${escapeHTML(displayed??"")}">`;
 }
 selectField(label,path,current,options){
  const id=this.controlId(path);
  if(current!=null&&!options.includes(current))options=[current,...options];
  return `<label for="${escapeHTML(id)}">${escapeHTML(label)}</label><select id="${escapeHTML(id)}" data-path="${escapeHTML(path)}" aria-label="${escapeHTML(label)}">${options.map(value=>`<option value="${escapeHTML(value)}" ${value===current?"selected":""}>${escapeHTML(portResponse(value)?.label||human(value))}</option>`).join("")}</select>`;
 }
 responseOptions(){
  const faces=this.problem.boundaries.filter(b=>b.enabled!==false&&["velocity","volume_flow","mass_flow","pressure","traction_outlet"].includes(b.kind)).map(b=>b.face);
  return ["pumping_power","pressure_drop","volume_flow",...Object.keys(PORT_QUANTITIES).flatMap(quantity=>FACES.filter(face=>faces.includes(face)).map(face=>`port_${quantity}_${face}`))];
 }
 responseFaceFields(item,index){
  const port=portResponse(item.response);
  if(port)return `<p class="implexity-cfd-help">Whole face ${escapeHTML(port.face.replace("_"," "))} · ${escapeHTML(port.unit)}. Positive is outward; negative is inward. Mass flow uses the fixed fluid density; mean normal velocity uses the whole face area. A prescribed flow is fixed by its boundary condition.</p>`;
  const path=`objectives.${index}`;
  const select=(key,label)=>this.selectField(`Response ${index+1}: ${label}`,`${path}.${key}`,item[key]??"",["",...FACES]).replace(/(<option value=""[^>]*>).*?(<\/option>)/,'$1Automatic$2');
  return `${select("inlet_face","Inlet face")}${select("outlet_face","Outlet face")}<p class="implexity-cfd-help">Choose both faces for a multi-port problem. Automatic requires an unambiguous pair. These faces define the reported pressure difference and inlet flow.</p>`;
 }
 page(){
  const problem=this.problem;
  if(this.step===0)return `<div class="implexity-cfd-card"><h3>Flow model</h3><div class="implexity-cfd-grid">${this.selectField("Resolved model","solver.model",problem.solver.model,["stokes_brinkman"])}</div><p class="implexity-cfd-help">This editor configures the resolved Stokes–Brinkman provider. Its topology coordinate uses the selected design volume and protected-cell constraints.</p></div>`;
  if(this.step===1)return `<div class="implexity-cfd-card"><h3>Newtonian fluid</h3><div class="implexity-cfd-grid">${this.field("Density","fluid.density_kg_m3",problem.fluid.density_kg_m3,"kg/m³")}${this.field("Dynamic viscosity","fluid.dynamic_viscosity_Pa_s",problem.fluid.dynamic_viscosity_Pa_s,"Pa·s")}${this.field("Reference temperature","fluid.reference_temperature_K",problem.fluid.reference_temperature_K,"K")}</div></div>`;
  if(this.step===2)return `<div class="implexity-cfd-card"><h3>Physical domain</h3><p class="implexity-cfd-help">Match this box and its cell counts to the geometry grid. Origin is the lower corner in world coordinates; lengths are in metres. These settings do not move or resample the geometry.</p><div class="implexity-cfd-grid">${[0,1,2].map(index=>this.field(`Origin ${"XYZ"[index]}`,`domain.origin_m.${index}`,problem.domain.origin_m[index],"m")).join("")}${[0,1,2].map(index=>this.field(`Extent ${"XYZ"[index]}`,`domain.extent_m.${index}`,problem.domain.extent_m[index],"m","number",'min="0"')).join("")}${[0,1,2].map(index=>this.field(`Cells ${"XYZ"[index]}`,`domain.cells.${index}`,problem.domain.cells[index],"","number",'min="2" step="1"')).join("")}${this.field("Reference length","domain.reference_length_m",problem.domain.reference_length_m,"m","number",'min="0"')}${this.field("Reference velocity","domain.reference_velocity_m_s",problem.domain.reference_velocity_m_s,"m/s","number",'min="0"')}</div></div>`;
  if(this.step===3)return this.boundaryPage();
  if(this.step===4)return `<div class="implexity-cfd-card"><h3>Topology–flow coupling</h3><div class="implexity-cfd-grid">${this.field("Solid permeability","brinkman.solid_permeability_m2",problem.brinkman.solid_permeability_m2,"m²","number",'min="0"')}${this.field("Fluid permeability","brinkman.fluid_permeability_m2",problem.brinkman.fluid_permeability_m2,"m²","number",'min="0"')}${this.field("RAMP parameter","brinkman.ramp_q",problem.brinkman.ramp_q,"1","number",'min="0"')}</div><p class="implexity-cfd-help">Solid permeability must be smaller than fluid permeability. Open ports are proposed as keep-void topology regions; preflight refuses blocked ports. Resistance is μ/K. This backend solves at full resistance, without a continuation sweep.</p></div>`;
  if(this.step===5)return `<div class="implexity-cfd-card"><h3>Differentiable responses</h3><p class="implexity-cfd-help">These are the saved provider response declarations. Configure the objective used by the optimiser in the Optimise stage.</p>${problem.objectives.length?problem.objectives.map((item,index)=>{const path=`objectives.${index}`,unit=describe(item.response).unit||"selected response unit";return `<section class="implexity-cfd-card"><h4>Response ${index+1}</h4><div class="implexity-cfd-grid">${this.selectField(`Response ${index+1}`,`${path}.response`,item.response,this.responseOptions())}${this.responseFaceFields(item,index)}<p class="implexity-cfd-help">Set weights, targets and minimisation or maximisation in the common Objectives stage. Values stored in this provider declaration do not control optimisation.</p></div></section>`}).join(""):"<p>No provider response declarations. Configure the common objective program in the Optimise stage.</p>"}</div>`;
  return `<div class="implexity-cfd-card"><h3>Review</h3><p><b>${escapeHTML(human(problem.solver.model))}</b> · ${problem.domain.cells.map(escapeHTML).join(" × ")} cells · topology <b>subject to design-volume and protection settings</b></p><p>${problem.boundaries.filter(item=>["velocity","volume_flow","mass_flow","pressure","traction_outlet"].includes(item.kind)).map(item=>`${escapeHTML(human(item.face))}: ${escapeHTML(human(item.kind))}`).join(" · ")}</p><p>Provider responses: ${problem.objectives.map(item=>`${escapeHTML(human(item.response))}`).join(" · ")||"None declared"}</p><p class="implexity-cfd-help">Validate and save checks this provider setup without starting a solver. Review the common objective program and design volume in the Optimise stage before launching a run.</p></div>`;
 }
 boundaryPage(){
  const roles={no_slip:"Stationary wall",moving_wall:"Moving wall",velocity:"Prescribed velocity",volume_flow:"Prescribed volume flow",mass_flow:"Prescribed mass flow",pressure:"Pressure opening",symmetry:"Symmetry",traction_outlet:"Open outlet (zero pressure)"};
  const help={no_slip:"Fluid velocity is zero on this face.",moving_wall:"Enter the wall velocity in global X, Y and Z directions.",velocity:"Enter velocity in global coordinates. On a minimum face, a positive normal-axis component points inward; on a maximum face, it points outward.",volume_flow:"Positive flow enters the domain; negative flow leaves it.",mass_flow:"Positive mass flow enters the domain; negative mass flow leaves it.",pressure:"Specify the pressure datum in Pa. The solver determines the flow direction.",symmetry:"No normal flow; tangential motion is not fixed.",traction_outlet:"Zero pressure and zero normal viscous derivative in the component-Laplacian flow model; not a general stress-traction boundary."};
  return `<div class="implexity-cfd-card"><h3>Assign flow boundaries</h3><p class="implexity-cfd-help">1 · Select a domain face. 2 · Choose its role. 3 · Enter its values in SI units. Then use Physical preflight and review the problem. These controls address the six domain faces, not arbitrary surface patches.</p>${this.problem.boundaries.map((b,i)=>{
   const path=`boundaries.${i}`;let values="";
   if(["velocity","moving_wall"].includes(b.kind))values=[0,1,2].map(axis=>this.field(`Velocity ${"XYZ"[axis]}`,`${path}.velocity_m_s.${axis}`,b.velocity_m_s?.[axis],"m/s")).join("");
   if(b.kind==="volume_flow")values=this.field("Inward volume flow",`${path}.volume_flow_m3_s`,b.volume_flow_m3_s,"m³/s");
   if(b.kind==="mass_flow")values=this.field("Inward mass flow",`${path}.mass_flow_kg_s`,b.mass_flow_kg_s,"kg/s");
   if(b.kind==="pressure")values=this.field("Static pressure",`${path}.static_pressure_Pa`,b.static_pressure_Pa,"Pa");
   const openPort=["velocity","volume_flow","mass_flow","pressure","traction_outlet"].includes(b.kind);
   const protection=openPort?this.field("Protected fluid layers",`${path}.port_buffer_cells`,b.port_buffer_cells,"cells","number",'min="0" step="1"'):"";
   return `<section class="implexity-cfd-card" data-boundary-face="${escapeHTML(b.face)}"><h4><button type="button" data-pick-face="${escapeHTML(b.face)}" aria-pressed="${this.selectedFace===b.face}">${escapeHTML(human(b.face))}${this.selectedFace===b.face?" · selected":""}</button></h4><label>Physical role<select data-path="${path}.kind" aria-label="${escapeHTML(human(b.face))}: physical role">${Object.entries(roles).map(([k,label])=>`<option value="${k}" ${b.kind===k?"selected":""}>${label}</option>`).join("")}</select></label><p class="implexity-cfd-help">${help[b.kind]||"Provider-specific role; inspect the original declaration."}</p><div class="implexity-cfd-grid">${values}${protection}</div><p class="implexity-cfd-help">${openPort?"Port layers are proposed as protected fluid cells during preflight.":"No open-port fluid protection applies to this wall or symmetry face."}</p></section>`;
  }).join("")}<p class="implexity-cfd-help">Defaults are already assigned to all faces; review them for your case. A pressure gauge sets only the datum, not an inlet. This flow panel does not assign heat-flux conditions; use the thermal provider's condition editor.</p></div>`;
 }
 selectFace(face){if(!FACES.includes(face))return;this.selectedFace=face;this.render();const row=this.root.querySelector(`[data-boundary-face="${face}"]`);row?.scrollIntoView?.({block:"nearest"});row?.querySelector("select")?.focus();}
 read(control){
  const path=control.dataset.path;if(!path)return;let value=control.value;
  if(control.type==="number"){
   control.setCustomValidity?.("");value=Number(control.value);
   const rule=this.numericRule(path,value),valid=control.value.trim()!==""&&Number.isFinite(value)&&!rule&&(!control.checkValidity||control.checkValidity());
   if(!valid){this.invalidInputs.set(path,control.value);const message=rule||`Enter a finite value within the allowed range for ${control.getAttribute("aria-label")||"this field"}.`;control.setAttribute("aria-invalid","true");control.setCustomValidity?.(message);this.validationIssues.set(path,{severity:"error",message,trusted:true});this.renderIssues();return}
   this.invalidInputs.delete(path);control.removeAttribute("aria-invalid");control.setCustomValidity?.("");this.validationIssues.delete(path);this.renderIssues();
  }
  if(/^objectives\.\d+\.(inlet_face|outlet_face)$/.test(path)&&value==="")value=null;
  const parts=path.split(".");let target=this.problem;parts.slice(0,-1).forEach(key=>target=target[Number.isInteger(+key)&&String(+key)===key?+key:key]);const key=parts.at(-1);target[Number.isInteger(+key)&&String(+key)===key?+key:key]=value;
  if(/^brinkman\.(solid|fluid)_permeability_m2$/.test(path)){this.validateDraft();this.renderIssues();}
  if(/^boundaries\.\d+\.kind$/.test(path)){
   for(const field of ["velocity_m_s","static_pressure_Pa","volume_flow_m3_s","mass_flow_kg_s"])target[field]=null;
   if(["velocity","moving_wall"].includes(value))target.velocity_m_s=[null,null,null];
   for(const issue of this.validationIssues.keys())if(issue.startsWith(`boundaries.${parts[1]}.`)){this.validationIssues.delete(issue);this.invalidInputs.delete(issue);}
   this.render();
  }
  if(/^objectives\.\d+\.response$/.test(path)&&portResponse(value)){
   for(const field of ["inlet_face","outlet_face","region_id"])target[field]=null;
  }
  if(/^objectives\.\d+\.(sense|response)$/.test(path))this.render();
  this.renderSoon();window.dispatchEvent(new CustomEvent("implexity:cfd-problem-draft",{detail:{providerId:this.providerId}}));
 }
 numericRule(path,value){
  const positive=/^(fluid\.(density_kg_m3|dynamic_viscosity_Pa_s|reference_temperature_K)|domain\.(extent_m\.[012]|reference_length_m|reference_velocity_m_s)|brinkman\.(solid_permeability_m2|fluid_permeability_m2)|objectives\.\d+\.scale)$/;
  const label=({"fluid.density_kg_m3":"Fluid density [kg/m³]","fluid.dynamic_viscosity_Pa_s":"Dynamic viscosity","fluid.reference_temperature_K":"Absolute temperature [K]","domain.reference_length_m":"Reference length [m]","domain.reference_velocity_m_s":"Reference velocity [m/s]","brinkman.solid_permeability_m2":"Solid permeability [m²]","brinkman.fluid_permeability_m2":"Fluid permeability [m²]"})[path]||(/^domain\.extent_m/.test(path)?"Domain extent [m]":"Response scale");
  if(positive.test(path)&&(!Number.isFinite(value)||value<=0))return `${label} must be strictly greater than zero.`;
  if(/^domain\.cells\.[012]$/.test(path)&&(!Number.isInteger(value)||value<2))return "Each grid direction needs at least two whole cells.";
  if(/^boundaries\.\d+\.port_buffer_cells$/.test(path)&&(!Number.isInteger(value)||value<0))return "Protected fluid layers must be a whole number, zero or greater.";
  if(path==="brinkman.ramp_q"&&(!Number.isFinite(value)||value<0))return "The RAMP parameter must be zero or greater.";
  return "";
 }
 validateDraft({focus=false}={}){
  const walk=(value,path="")=>{if(value&&typeof value==="object")Object.entries(value).forEach(([key,item])=>walk(item,path?`${path}.${key}`:key));else {const message=this.numericRule(path,value);if(message)this.validationIssues.set(path,{severity:"error",message,trusted:true});}};
  walk(this.problem);
  this.validationIssues.delete("brinkman.permeability_order");
  const {solid_permeability_m2:solid,fluid_permeability_m2:fluid}=this.problem.brinkman||{};
  if(Number.isFinite(solid)&&Number.isFinite(fluid)&&solid>=fluid)this.validationIssues.set("brinkman.permeability_order",{severity:"error",message:"Solid permeability must be smaller than fluid permeability.",trusted:true});
  if(this.validationIssues.size){this.renderIssues();if(focus)this.focusDraftIssue();return false}return true;
 }
 focusDraftIssue(){
  const path=this.validationIssues.keys().next().value;if(!path)return;
  const step={fluid:1,domain:2,boundaries:3,brinkman:4,objectives:5}[path.split(".")[0]];
  if(step===undefined)return;
  this.step=step;this.render();
  const target=path==="brinkman.permeability_order"?"brinkman.solid_permeability_m2":path;
  const control=Array.from(this.root.querySelectorAll("[data-path]")).find(item=>item.dataset.path===target);
  if(control){control.setAttribute("aria-invalid","true");control.focus({preventScroll:true});control.scrollIntoView({block:"center"});}
 }
 renderSoon(){clearTimeout(this._rt);this._rt=setTimeout(()=>{},80)}
 assignPickedFace(detail){if(this.root.hidden||this.step!==3||!detail?.domainFace)return;this.selectFace(detail.domainFace);}
 validateVisibleControls(){
  let firstInvalid=null;
  this.root.querySelectorAll('input[type="number"][data-path]').forEach(control=>{
   const path=control.dataset.path;control.setCustomValidity?.("");const value=Number(control.value);
   const valid=control.value.trim()!==""&&Number.isFinite(value)&&!this.numericRule(path,value)&&(!control.checkValidity||control.checkValidity());
   if(valid){control.removeAttribute("aria-invalid");this.validationIssues.delete(path);return}
   const message=this.numericRule(path,value)||`Enter a finite value within the allowed range for ${control.getAttribute("aria-label")||"this field"}.`;
   control.setAttribute("aria-invalid","true");control.setCustomValidity?.(message);this.validationIssues.set(path,{severity:"error",message,trusted:true});
   if(!firstInvalid)firstInvalid=control;
  });
  if(!firstInvalid)return true;
  this.renderIssues();firstInvalid?.focus?.();firstInvalid?.reportValidity?.();return false;
 }
 async preflight(){if(this.loading||this.loadError||!this.catalog||!this.validateVisibleControls()||!this.validateDraft({focus:true}))return null;try{const data=await fetchJSON("/v1/implicit/cae/preflight",{method:"POST",body:JSON.stringify({provider:this.providerId,problem:this.problem,design_coordinates:["model:control"]})});this.showIssues(data.issues||[]);return data}catch(error){this.showIssues([{severity:"error",message:failureSummary(error,"The CFD setup could not be validated. Review the validation details."),technical:error}],{trusted:true});return null}}
 showIssues(issues,{trusted=false}={}){this.serviceIssues=Array.isArray(issues)?issues.map(issue=>{const severity=["info","warning","error","success"].includes(String(issue?.severity))?String(issue.severity):"info";return {severity,message:trusted?String(issue?.message??serviceSummary(severity)):serviceSummary(severity),technical:trusted?issue?.technical:issue,trusted:true};}):[];this.renderIssues()}
 renderIssues(){
  const host=$("[data-issues]",this.root),issues=[...this.validationIssues.values(),...this.serviceIssues];host.replaceChildren(...issues.map(issue=>{const severity=["info","warning","error","success"].includes(String(issue.severity))?String(issue.severity):"info";const item=el("div","implexity-cfd-issue");renderMessage(item,issue.message,issue.technical,severity);return item}));host.hidden=issues.length===0;
 }
 async optimise(){
  if(this.loading||this.saving||this.loadError||!this.catalog||!this.validateVisibleControls()||!this.validateDraft({focus:true}))return false;
  const workbench=window.implexityWorkbench,persistence=window.ImplexityProviderProblemPersistence;
  if(!workbench||!persistence?.save||!persistence?.captureModel){this.showIssues([{severity:"error",message:"The workflow connection is unavailable; no setup was saved."}],{trusted:true});return false}
  if(workbench.currentProvider?.()!==this.providerId){this.showIssues([{severity:"error",message:"Select the resolved Stokes flow provider before saving this setup."}],{trusted:true});return false}
  this.saving=true;this.render();
  try{
   const snapshot=structuredClone(this.problem),expectedModel=await persistence.captureModel();
   const report=await fetchJSON("/v1/implicit/cae/preflight",{method:"POST",body:JSON.stringify({provider:this.providerId,problem:snapshot,design_coordinates:["model:control"]})});
   if(report.ok===false)throw new Error(JSON.stringify(report.issues||report.errors||report));
   if(workbench.currentProvider?.()!==this.providerId||JSON.stringify(this.problem)!==JSON.stringify(snapshot))throw new Error("The physics setup changed during validation. Review it and save again.");
   if(this.activeProviderId!==this.providerId&&!window.confirm("Save this CFD setup as the active physics problem? This replaces the active problem; it does not start a solver."))return false;
   const record=await persistence.save({providerId:this.providerId,problem:snapshot,expectedModel,provenance:{source:"native_gui",editor:"embedded_cfd",operation:"validate_and_apply"}});
   this.problem=structuredClone(record.problem);this.activeProviderId=this.providerId;
   window.dispatchEvent(new CustomEvent("implexity:provider-problem-changed",{detail:{providerId:this.providerId,problem:structuredClone(record.problem),record}}));
   workbench.setStep("optimize");workbench.note("CFD setup saved. Review objectives and start optimisation separately.");this.close();return true;
  }catch(error){this.showIssues([{severity:"error",message:failureSummary(error,"The CFD setup was not saved. Review the validation details."),technical:error}],{trusted:true});return false}
  finally{this.saving=false;this.render()}
 }
}

window.ImplexityCFDWorkspace=CFDWorkspace;
window.addEventListener("DOMContentLoaded",()=>{if(!window.IMPLEXITY_GUI_CENTRIC){const launch=el("button","implexity-cfd-launch","CFD setup");launch.type="button";launch.title="Resolved flow and direct-gradient topology optimisation";launch.onclick=()=>document.dispatchEvent(new CustomEvent("implexity:cfd-open"));const host=document.querySelector("[data-toolbar],.toolbar,#toolbar")||document.body;host.append(launch);window.implexityCFDWorkspace=new CFDWorkspace(window.implexityViewer||window.viewer||null)}window.dispatchEvent(new CustomEvent("implexity:cfd-workspace-ready",{detail:{Class:CFDWorkspace}}))});
})();
