// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(function (global) {
  "use strict";
  const present=(id,metadata={})=>window.ImplexityText?.present?.(id,metadata)||({id:String(id??""),label:String(metadata.label||metadata.title||id||""),unit:String(metadata.unit||metadata.units||""),description:String(metadata.description||metadata.help||""),explicit:Boolean(metadata.label||metadata.title)});
  const human=(id,metadata={})=>present(id,metadata).label;
  const SCHEMA = "implexity-response-program/2";


  const uiPresentation=global.ImplexityUIPresentation||(global.ImplexityUIPresentation=(()=>{
    const DEFAULT="The request could not be completed. Review the current setup and try again.";
    const unsafe=value=>{
      const text=String(value??"");
      const structured=/\[\s*[\[{]/.test(text)||Array.from(text.matchAll(/\[[^\]]*\]/g)).some(match=>{try{return Array.isArray(JSON.parse(match[0]))}catch(_){return false}});
      return structured||/https?:\/\/|file:\/\/|\/v\d+\/|(?:^|\s)\/(?:Users|home|private|tmp|var|opt|etc)\/|[A-Za-z]:\\|\b[A-Za-z][A-Za-z0-9]*_[A-Za-z0-9_]+\b|(?:^|\s)\{.*\}(?:$|\s)|\[object Object\]|Traceback|\n\s*at\s+/i.test(text);
    };
    const publicText=(value,fallback=DEFAULT)=>{
      const text=String(value??"").replace(/\s+/g," ").trim();
      return text&&!unsafe(text)?text:String(fallback||DEFAULT);
    };
    const technicalText=value=>{
      if(value===undefined||value===null)return "";
      let text="";
      if(value instanceof Error)text=[value.name&&`Type: ${value.name}`,value.message&&`Message: ${value.message}`,value.stack&&`Stack:\n${value.stack}`].filter(Boolean).join("\n");
      else if(typeof value==="string")text=value;
      else try{text=JSON.stringify(value,null,2);}catch(_){text=String(value);}
      return String(text).slice(0,24000);
    };
    const render=(host,{summary,fallback=DEFAULT,technical,severity="error",role}={})=>{
      if(!host)return null;
      const safe=publicText(summary,fallback),lead=document.createElement("span");
      lead.className="implexity-ui-summary";lead.textContent=safe;
      host.replaceChildren(lead);
      const diagnostic=technicalText(technical);
      if(diagnostic){
        const details=document.createElement("details"),label=document.createElement("summary"),pre=document.createElement("pre");
        details.className="implexity-ui-technical";details.open=false;
        label.textContent="Technical details";pre.textContent=diagnostic;pre.style.whiteSpace="pre-wrap";pre.style.overflowWrap="anywhere";
        details.append(label,pre);host.append(details);
      }
      host.dataset.severity=severity;
      host.setAttribute("role",role||(severity==="error"?"alert":"status"));
      return safe;
    };
    return Object.freeze({publicText,technicalText,render});
  })());

  function clone(value) { return global.structuredClone ? global.structuredClone(value) : JSON.parse(JSON.stringify(value)); }
  function id(prefix) { return `${prefix}_${Date.now().toString(36)}_${Math.random().toString(36).slice(2,8)}`; }
  function number(value, fallback = 0) { const out = Number(value); return Number.isFinite(out) ? out : fallback; }
  function finiteNumber(value) {
    return typeof value==="number"&&Number.isFinite(value)?value:null;
  }
  function controlNumber(input) {
    if(!input)return null;
    const raw=String(input.value??"").trim(),value=raw===""?null:finiteNumber(Number(raw)),validity=input.validity;
    if(value===null||validity?.badInput||validity?.rangeUnderflow||validity?.rangeOverflow||validity?.stepMismatch)return null;
    return value;
  }
  function element(tag, className, text) { const node=document.createElement(tag); if(className)node.className=className; if(text!==undefined)node.textContent=text; return node; }

  class ObjectiveComposer {
    constructor(options = {}) {
      this.options = options;
      this.viewport = options.viewport || document.querySelector("[data-implexity-viewport], #viewport, #viewer, .viewport, .viewer") || (document.querySelector("canvas") && document.querySelector("canvas").parentElement);
      if (!this.viewport) throw new Error("Objective composer could not locate the Implexity viewport");
      if (getComputedStyle(this.viewport).position === "static") this.viewport.style.position = "relative";
      this.catalogue = [];
      this.program = { schema: SCHEMA, objectives: [], constraints: [], normalisation: "response_scale" };
      this._applying=false;
      this._applyPromise=null;
      this._sensitivityPending=false;
      this._physicsPlanSequence=0;
      this._providerOverrides={};
      this._physicsPlanController=null;
      this._returnFocus=null;
      this._build();
      this._bind();
      global.ImplexityObjectiveComposer = this;
      this.loadCatalogue();
    }

    _build() {
      this.toggle = element("button", "implexity-objective-toggle", "Objectives & constraints");
      this.toggle.type="button";
      this.toggle.setAttribute("aria-controls","implexity-objective-panel");
      this.toggle.setAttribute("aria-expanded","false");
      this.panel = element("section", "implexity-objective-panel");
      this.panel.id="implexity-objective-panel";
      this.panel.setAttribute("role","region");
      this.panel.setAttribute("aria-labelledby","implexity-objective-title");
      this.panel.setAttribute("aria-describedby","implexity-objective-subtitle");
      this.panel.hidden=true;
      this.panel.innerHTML=`
        <header class="implexity-objective-header">
          <div><div class="implexity-objective-title" id="implexity-objective-title">Differentiable response program</div><div class="implexity-objective-subtitle" id="implexity-objective-subtitle">The same responses feed inspection, sensitivities and optimisation.</div></div>
          <button type="button" class="implexity-objective-close" aria-label="Close objectives and constraints">×</button>
        </header>
        <div class="implexity-objective-body">
          <section class="implexity-objective-section"><div><button type="button" data-action="save-program">Save program</button> <button type="button" data-action="load-program">Load program</button><input type="file" data-program-file accept=".json,application/json" aria-label="Load a saved response program (JSON)" hidden></div><p class="implexity-objective-subtitle">Save objectives and constraints as JSON before closing. This does not save geometry or physics conditions. After loading, review and apply the program.</p></section>
          <section class="implexity-objective-section"><div class="implexity-objective-section-title"><span>Objectives</span><button type="button" class="implexity-objective-add" data-add="objective" aria-label="Add objective">Add</button></div><div class="implexity-objective-list" data-list="objectives"></div></section>
          <section class="implexity-objective-section"><div class="implexity-objective-section-title"><span>Constraints</span><button type="button" class="implexity-objective-add" data-add="constraint" aria-label="Add constraint">Add</button></div><p class="implexity-objective-subtitle">Response bounds are squared-violation penalty terms weighted by their penalty weight. Check final bound satisfaction.</p><div class="implexity-objective-list" data-list="constraints"></div></section>
          <section class="implexity-objective-section" data-physics-plan><div class="implexity-objective-section-title"><span>Automatic physics orchestration</span></div><div><button type="button" data-action="save-providers">Save provider choices</button> <button type="button" data-action="load-providers">Load provider choices</button><input type="file" data-provider-file accept=".json,application/json" aria-label="Load saved provider choices (JSON)" hidden></div><p class="implexity-objective-subtitle">Provider-choice files contain selections only, not geometry, material data or boundary conditions.</p><div data-physics-plan-content>Define at least one target response. Implexity will infer the required installed physics add-ins and coupling graph.</div></section>
          <section class="implexity-objective-section" data-sensitivity hidden><div class="implexity-objective-section-title"><span>Local sensitivity</span></div><div data-sensitivity-content></div></section>
        </div>
        <footer class="implexity-objective-footer"><div class="implexity-objective-status" id="implexity-objective-status">Loading engineering responses…</div><button type="button" data-action="sensitivity" aria-describedby="implexity-objective-status">Preview sensitivity</button><button type="button" data-primary="true" data-action="apply" aria-describedby="implexity-objective-status">Apply response program</button></footer>`;
      this.viewport.append(this.toggle,this.panel);
      this.status=this.panel.querySelector(".implexity-objective-status");
      this.status.setAttribute("role","status");
      this.status.setAttribute("aria-live","polite");
      this.status.setAttribute("aria-atomic","true");
      this.status.style.whiteSpace="pre-line";
      this.status.style.overflow="visible";
      this.status.style.textOverflow="clip";
    }

    _bind() {
      global.addEventListener("implexity:provider-problem-changed",()=>this._schedulePhysicsPlan());
      this.toggle.addEventListener("click",()=>this.setOpen(this.panel.hidden));
      this.panel.querySelector(".implexity-objective-close").addEventListener("click",()=>this.setOpen(false,{focus:true}));
      this.panel.addEventListener("click",event=>{
        const add=event.target.closest("[data-add]");
        if(add){this.add(add.dataset.add);return;}
        const remove=event.target.closest("[data-remove]");
        if(remove){this.remove(remove.dataset.kind,remove.dataset.remove);return;}
        const action=event.target.closest("[data-action]");
        if(action && action.dataset.action==="apply")this.apply();
        if(action && action.dataset.action==="sensitivity")this.sensitivity();
        if(action && action.dataset.action==="save-program"){
          try{this.saveProgram();}
          catch(error){uiPresentation.render(this.status,{summary:"The program could not be saved.",technical:error});}
        }
        if(action && action.dataset.action==="load-program")this.panel.querySelector("[data-program-file]").click();
        if(action && action.dataset.action==="save-providers"){
          try{this.saveProviderChoices();}catch(error){uiPresentation.render(this.status,{summary:"Provider choices could not be saved.",technical:error});}
        }
        if(action && action.dataset.action==="load-providers")this.panel.querySelector("[data-provider-file]").click();
      });
      this.panel.querySelector("[data-provider-file]").addEventListener("change",async event=>{
        const file=event.target.files?.[0];if(!file)return;
        try{this.setProviderChoices(JSON.parse(await file.text()));this._setStatus("Provider choices loaded. Review the new physics plan before running.");}
        catch(error){uiPresentation.render(this.status,{summary:"Provider choices were not loaded. The current choices are unchanged.",technical:error});}
        finally{event.target.value="";}
      });
      this.panel.querySelector("[data-program-file]").addEventListener("change",async event=>{
        const file=event.target.files?.[0];if(!file)return;
        try{this.setProgram(JSON.parse(await file.text()));this._emitDraft();this._setStatus("Program loaded as a draft. Review the selected physics and apply it.");}
        catch(error){uiPresentation.render(this.status,{summary:"The program could not be loaded.",technical:error});}
        finally{event.target.value="";}
      });
      this.panel.addEventListener("change",event=>this._updateFromInput(event.target));
      this.panel.addEventListener("input",event=>this._updateFromInput(event.target));
      global.addEventListener("implexity:add-response-to-objective",event=>{
        const detail=event.detail||{};
        this.add(detail.kind==="constraint"?"constraint":"objective",detail.response_id||detail.id,detail);
        this.setOpen(true);
      });
      global.addEventListener("implexity:project-opened",event=>{
        const program=event.detail && (event.detail.response_program||event.detail.optimization_spec);
        if(program){this.setProgram(program);this._emitDraft();}
      });
    }

    setOpen(open,{focus=false}={}){
      const visible=Boolean(open),wasVisible=!this.panel.hidden,active=document.activeElement;
      if(visible&&!wasVisible&&active&&!this.panel.contains(active))this._returnFocus=active;
      this.panel.hidden=!visible;this.toggle.setAttribute("aria-expanded",String(visible));
      if(visible&&focus)this.panel.querySelector("select,input,button")?.focus?.();
      if(!visible&&(focus||this.panel.contains(active))){const target=this._returnFocus||this.toggle;requestAnimationFrame(()=>target?.focus?.({preventScroll:true}));}
      if(!visible)this._returnFocus=null;
      return visible;
    }

    _setStatus(message,kind="status"){
      this.status.replaceChildren();this.status.textContent=String(message||"");this.status.dataset.kind=kind;
      this.status.setAttribute("role",kind==="error"?"alert":"status");this.status.setAttribute("aria-live",kind==="error"?"assertive":"polite");this.status.setAttribute("aria-atomic","true");
    }

    _reportValidation(errors){
      const message=errors[0]||"Complete the required response values.";this.status.dataset.validationError="true";this._setStatus(message,"error");
      const target=this.panel.querySelector('[aria-invalid="true"]')||this.panel.querySelector('[data-add="objective"]');target?.focus?.({preventScroll:true});
    }

    async loadCatalogue() {
      const workbench=global.implexityWorkbench;
      if(workbench?.currentProvider?.()&&workbench.currentProvider()!=="legacy_multiphysics_implicit"){
        workbench.syncObjectiveCatalogue();return;
      }
      const sequence=this._catalogueSequence=(this._catalogueSequence||0)+1;
      try {
        const adapters=[global.ImplexityEngineering,global.ImplexityOptimization,global.implexityEngineering].filter(Boolean);
        let value=null;
        for(const adapter of adapters){
          for(const name of ["responses","getResponses","engineeringResponses","catalogue"]){
            if(typeof adapter[name]==="function"){value=await adapter[name]();break;}
            if(Array.isArray(adapter[name])){value=adapter[name];break;}
          }
          if(value)break;
        }
        if(!value){
          const response=await fetch("/v1/implicit/engineering",{credentials:"same-origin"});
          if(!response.ok)throw new Error(`${response.status} ${response.statusText}`);
          value=await response.json();
        }
        if(sequence!==this._catalogueSequence)return;
        const activeWorkbench=global.implexityWorkbench;
        if(activeWorkbench?.currentProvider?.()&&activeWorkbench.currentProvider()!=="legacy_multiphysics_implicit"){
          activeWorkbench.syncObjectiveCatalogue();return;
        }
        const raw=Array.isArray(value)?value:(value.responses||value.engineering||value.items||[]);
        this.catalogue=raw.map(item=>this._normaliseResponse(item)).filter(Boolean);
        this._setStatus(`${this.catalogue.length} differentiable responses available`);
        this.render();this._schedulePhysicsPlan();
      }catch(error){
        if(sequence!==this._catalogueSequence)return;
        uiPresentation.render(this.status,{summary:"Engineering responses could not be loaded. Check the service connection and try again.",technical:error});this.status.setAttribute("aria-live","assertive");this.status.setAttribute("aria-atomic","true");
        this.catalogue=[];
        this.render();
      }
    }

    _normaliseResponse(item) {
      if(!item)return null;
      const identifier=String(item.id||item.name||item.key||"").trim();
      if(!identifier)return null;
      const descriptor=present(identifier,item);
      return {
        id:identifier,
        label:descriptor.label,
        units:descriptor.unit,
        description:descriptor.description,
        module:item.module||item.license_module||"core_differentiable_design",
        differentiable:item.differentiable!==false,
        scale:number(item.scale||item.reference_scale,1)
      };
    }

    add(kind,responseId=null,seed={}) {
      const workbench=global.implexityWorkbench;
      if(workbench?.currentProvider?.()!=="legacy_multiphysics_implicit")workbench?.syncObjectiveCatalogue?.();
      const collection=kind==="constraint"?this.program.constraints:this.program.objectives;
      const selected=responseId||((this.catalogue.find(item=>item.differentiable)||this.catalogue[0]||{}).id)||"";
      if(!selected){this._setStatus("No response is available yet. Wait for the selected physics provider to load.");return null;}
      const row=kind==="constraint"?{
        id:id("con"),response_id:selected,relation:seed.relation||"<=",bound:number(seed.bound,0),weight:number(seed.weight,1),scale:number(seed.scale,1),enabled:true
      }:{
        id:id("obj"),response_id:selected,sense:seed.sense||"minimize",weight:number(seed.weight,1),target:seed.target??null,scale:number(seed.scale,1),enabled:true
      };
      collection.push(row);this.render();this._emitDraft();return row;
    }

    remove(kind,rowId){
      const key=kind==="constraint"?"constraints":"objectives";
      const active=document.activeElement;
      const activeRow=this.panel.contains(active)?active?.closest?.('.implexity-response-row'):null;
      const restore=activeRow?.dataset.rowId===rowId&&activeRow?.dataset.kind===kind;
      const index=this.program[key].findIndex(item=>item.id===rowId);
      this.program[key]=this.program[key].filter(item=>item.id!==rowId);this.render();this._emitDraft();
      if(restore){
        const next=this.program[key][Math.min(index,this.program[key].length-1)];
        const row=next&&[...this.panel.querySelectorAll('.implexity-response-row')].find(item=>item.dataset.rowId===next.id&&item.dataset.kind===kind);
        const control=row&&[...row.querySelectorAll('[data-field]')].find(item=>!item.disabled);
        (control||this.panel.querySelector(`[data-add="${kind}"]`))?.focus?.({preventScroll:true});
      }
    }

    saveProgram(){
      const program=this.specification(),provider=global.implexityWorkbench?.currentProvider?.();
      if(program.provider_id&&provider&&program.provider_id!==provider)throw new Error(`This program belongs to ${program.provider_id}. Select that physics provider before saving it.`);
      if(provider)program.provider_id=provider;
      const url=URL.createObjectURL(new Blob([JSON.stringify(program,null,2)],{type:"application/json"}));
      const link=document.createElement("a");link.href=url;link.download="implexity-response-program.json";
      document.body.appendChild(link);link.click();link.remove();setTimeout(()=>URL.revokeObjectURL(url),1000);
      this._setStatus("Program download requested. Geometry and physics conditions are not included.");
    }

    setProgram(value){
      const source=clone(value);
      if(!source||typeof source!=="object"||Array.isArray(source))throw new Error("The response program must be an object.");
      if(source.schema!==undefined&&source.schema!==SCHEMA)throw new Error("This response-program schema is not supported.");
      if(source.normalisation!==undefined&&source.normalisation!=="response_scale")throw new Error("Only per-response scale normalisation is supported.");
      if(source.provider_id!==undefined){
        if(typeof source.provider_id!=="string"||!source.provider_id.trim())throw new Error("The saved provider identity must be a nonempty string.");
        const active=global.implexityWorkbench?.currentProvider?.();
        if(active&&source.provider_id!==active)throw new Error(`This program belongs to ${source.provider_id}. Select that physics provider before loading it.`);
      }
      for(const key of ["objectives","constraints"])if(source[key]!==undefined&&!Array.isArray(source[key]))throw new Error(`${human(key)} must be an array.`);
      const seen=new Set();
      const rows=(items,kind)=>(Array.isArray(items)?items:[]).map(row=>{
        if(!row||typeof row!=="object"||Array.isArray(row))throw new Error("Each response-program row must be an object.");
        if(Object.hasOwn(row,"enforcement")||Object.hasOwn(row,"tolerance")){
           
          if((row.enforcement??"soft")!=="soft"||Number(row.tolerance??0)!==0)
            throw new Error(`The saved limit on ${human(row.response_id)||"a response"} was authored as a hard constraint. Hard constraints were removed from Implexity and response bounds are penalty terms only. Remove its enforcement and tolerance fields and review its penalty weight before loading.`);
          delete row.enforcement;delete row.tolerance;
        }
        if(typeof row.id!=="string"||!row.id.trim()||seen.has(row.id)){
          let candidate=`${kind}_import_${seen.size+1}`;while(seen.has(candidate))candidate+="_";row.id=candidate;
        }
        seen.add(row.id);return row;
      });
      this.program={schema:SCHEMA,objectives:rows(source.objectives,"objective"),constraints:rows(source.constraints,"constraint"),normalisation:source.normalisation||"response_scale"};
      if(source.provider_id)this.program.provider_id=source.provider_id;
      this.render();this._schedulePhysicsPlan();
    }

    render(){
      const active=document.activeElement;
      const row=this.panel.contains(active)?active?.closest?.('.implexity-response-row'):null;
      const focus=row&&active.dataset?.field?{id:row.dataset.rowId,kind:row.dataset.kind,field:active.dataset.field}:null;
      this._renderList("objectives","objective");
      this._renderList("constraints","constraint");
      this._updateActionAvailability();
      if(focus){
        const replacement=[...this.panel.querySelectorAll('.implexity-response-row')].find(item=>item.dataset.rowId===focus.id&&item.dataset.kind===focus.kind);
        const control=replacement&&[...replacement.querySelectorAll('[data-field]')].find(item=>item.dataset.field===focus.field);
        if(control&&!control.disabled)control.focus({preventScroll:true});
      }
    }

    _renderList(key,kind){
      const list=this.panel.querySelector(`[data-list="${key}"]`);list.replaceChildren();
      const rows=this.program[key];
      if(!rows.length){list.appendChild(element("div","implexity-objective-empty",kind==="objective"?"Add at least one response to optimise.":"No explicit response constraints."));return;}
      rows.forEach((row,index)=>list.appendChild(this._row(row,kind,index)));
    }

    _row(row,kind,index){
      const node=element("div","implexity-response-row");node.dataset.rowId=row.id;node.dataset.kind=kind;
      const response=this.catalogue.find(item=>item.id===row.response_id);const unit=response?.units||"",responseLabel=response?.label||human(row.response_id)||`${human(kind)} ${index+1}`,context=`${kind} ${index+1}: ${responseLabel}`;
      const responseSelect=document.createElement("select");responseSelect.dataset.field="response_id";responseSelect.setAttribute("aria-label",`${kind==="objective"?"Objective":"Constraint"} ${index+1} response`);
      for(const item of this.catalogue){const option=document.createElement("option");option.value=item.id;option.textContent=item.label;option.title=item.description||item.id;option.selected=item.id===row.response_id;option.disabled=!item.differentiable;responseSelect.appendChild(option);}
      if(row.response_id && !this.catalogue.some(item=>item.id===row.response_id)){const option=document.createElement("option");option.value=row.response_id;option.textContent=`${human(row.response_id)} (unavailable)`;option.selected=true;responseSelect.appendChild(option);node.dataset.invalid="true";}
      const mode=document.createElement("select");mode.dataset.field=kind==="objective"?"sense":"relation";
      mode.setAttribute("aria-label",`${kind==="objective"?"Objective direction":"Constraint relation"} for ${responseLabel}`);
      const modes=kind==="objective"?[["minimize","Minimise"],["maximize","Maximise"],["target","Target"]]:[["<=","≤ upper"],[">=","≥ lower"],["=","= target"]];
      modes.forEach(([value,label])=>{const option=document.createElement("option");option.value=value;option.textContent=label;option.selected=value===row[mode.dataset.field];mode.appendChild(option);});
      const primary=document.createElement("input");primary.type="number";primary.required=true;primary.step="any";primary.className="implexity-row-value";primary.dataset.field=kind==="objective"?(row.sense==="target"?"target":"weight"):"bound";if(primary.dataset.field==="weight")primary.min="0";const primaryValue=finiteNumber(row[primary.dataset.field]);primary.value=primaryValue===null?"":String(primaryValue);primary.setAttribute("aria-label",`${primary.dataset.field==="weight"?"Objective weight":primary.dataset.field==="target"?"Target value":"Constraint bound"} for ${responseLabel}${unit?` (${unit})`:""}`);
      const scale=document.createElement("input");scale.type="number";scale.required=true;scale.min="0";scale.step="any";scale.dataset.field="scale";const scaleValue=finiteNumber(row.scale);scale.value=scaleValue===null?"":String(scaleValue);scale.setAttribute("aria-label",`Positive response normalisation scale for ${responseLabel}`);
      const remove=document.createElement("button");remove.type="button";remove.textContent="×";remove.dataset.remove=row.id;remove.dataset.kind=kind;remove.title=`Remove ${context}`;remove.setAttribute("aria-label",`Remove ${context}`);
      const field=(label,control,column)=>{const wrapper=element("label","implexity-response-field");wrapper.dataset.column=column;wrapper.append(element("span","implexity-response-field-label",label),control);return wrapper;};
      const enabled=document.createElement("input");enabled.type="checkbox";enabled.dataset.field="enabled";enabled.checked=row.enabled!==false;enabled.setAttribute("aria-label",`Enable ${context}`);
      enabled.style.width="auto";enabled.style.justifySelf="start";
      const enableField=field("Include in optimisation",enabled,"enabled");enableField.style.gridColumn="1 / -1";node.appendChild(enableField);
      const valueLabel=primary.dataset.field==="weight"?"Weight":primary.dataset.field==="target"?`Target${unit?` (${unit})`:""}`:`Bound${unit?` (${unit})`:""}`;
      node.append(
        field("Response",responseSelect,"response"),
        field(kind==="objective"?"Direction":"Relation",mode,"mode"),
        field(valueLabel,primary,"value"),
        field("Reference scale",scale,"scale"),
        remove
      );
      if(kind==="constraint"||row.sense==="target"){
        const weight=document.createElement("input");weight.type="number";weight.required=true;weight.min="0";weight.step="any";weight.dataset.field="weight";
        const value=finiteNumber(row.weight??1);weight.value=value===null?"":String(value);
        weight.setAttribute("aria-label",`${kind==="constraint"?"Penalty":"Objective"} weight for ${responseLabel}`);
        const wrapper=field(kind==="constraint"?"Penalty weight":"Target objective weight",weight,"target-weight");wrapper.style.gridColumn="1 / -1";node.appendChild(wrapper);
      }
      if(kind==="constraint"){
        const note=element("p","implexity-response-meta","This bound is a differentiable penalty term: its squared scaled violation, times the penalty weight, is added to the objective. It steers every update but is not enforced exactly; raise the weight to tighten it.");
        node.appendChild(note);
      }
      const meta=element("div","implexity-response-meta");meta.id=`implexity-response-meta-${row.id}`;primary.setAttribute("aria-describedby",meta.id);scale.setAttribute("aria-describedby",meta.id);
      if(response){
        const unitText=response.units==="1"?"Dimensionless":(response.units||"Unit not supplied");
        const description=response.description||"Description not supplied by this physics add-in.";
        const module=element("span","implexity-response-module",human(response.module));module.title=response.module;
        meta.append(element("span","",unitText),element("span","",description),module);
      }else{meta.textContent="The response is not available in the active capability set.";}
      const error=element("span","implexity-response-error");error.id=`implexity-response-error-${row.id}`;error.setAttribute("role","alert");error.setAttribute("aria-live","assertive");error.setAttribute("aria-atomic","true");error.hidden=true;node.appendChild(error);
      node.appendChild(meta);this._validateRow(node,row);return node;
    }

    _updateFromInput(input){
      const rowNode=input.closest(".implexity-response-row");if(!rowNode||!input.dataset.field)return;
      const key=rowNode.dataset.kind==="constraint"?"constraints":"objectives";
      const row=this.program[key].find(item=>item.id===rowNode.dataset.rowId);if(!row)return;
      const field=input.dataset.field;
      if(field==="enabled"){row.enabled=input.checked;this.render();this._emitDraft();return;}
      if(input.type==="number"){
        const candidate=input.value.trim()===""?NaN:Number(input.value);
        if(!Number.isFinite(candidate)){
          row[field]=null;
          this._validateRow(rowNode,row);this._updateActionAvailability();
          this._emitDraft();
          return;
        }
        row[field]=candidate;
      }else row[field]=input.value;
      if(field==="sense"||field==="response_id")this.render();else this._validateRow(rowNode,row);
      this._updateActionAvailability();
      this._emitDraft();
    }

    _validateRow(node,row){
      if(row.enabled===false){
        node.dataset.invalid="false";
        node.querySelectorAll('select,input:not([data-field="enabled"])').forEach(control=>{control.disabled=true;control.setAttribute("aria-invalid","false");control.removeAttribute("aria-errormessage");});
        const error=node.querySelector(".implexity-response-error");if(error)error.hidden=true;
        return true;
      }
      const responseMissing=!row.response_id||!this.catalogue.some(item=>item.id===row.response_id&&item.differentiable!==false);
      const responseControl=node.querySelector('[data-field="response_id"]'),scaleControl=node.querySelector('[data-field="scale"]'),valueControl=node.querySelector(".implexity-row-value");
      const scale=controlNumber(scaleControl),scaleInvalid=scale===null||scale<=0;
      const valueField=valueControl?.dataset.field,value=controlNumber(valueControl);
      const valueInvalid=value===null||(valueField==="weight"&&value<0);
      const weightControl=node.querySelector('[data-field="weight"]'),weight=controlNumber(weightControl),weightInvalid=Boolean(weightControl)&&(weight===null||weight<0);
      weightControl?.setAttribute("aria-invalid",String(weightInvalid));
      const invalid=responseMissing||scaleInvalid||valueInvalid||weightInvalid;
      responseControl?.setAttribute("aria-invalid",String(responseMissing));scaleControl?.setAttribute("aria-invalid",String(scaleInvalid));valueControl?.setAttribute("aria-invalid",String(valueInvalid));
      const error=node.querySelector(".implexity-response-error");if(error){error.hidden=!invalid;error.textContent=responseMissing?"Choose an available response.":scaleInvalid?"Response scale must be a finite value greater than zero.":weightInvalid?"Enter a finite non-negative objective weight.":valueInvalid?`Enter a valid finite ${human(valueField)}.`:"";for(const [control,bad] of [[responseControl,responseMissing],[scaleControl,scaleInvalid],[valueControl,valueInvalid],[weightControl,weightInvalid]])if(control){if(bad)control.setAttribute("aria-errormessage",error.id);else control.removeAttribute("aria-errormessage");}}
      node.dataset.invalid=String(invalid);return !invalid;
    }

    validate(){
      const errors=[];
      if(!this.program.objectives.some(row=>row.enabled!==false))errors.push("At least one enabled objective is required.");
      for(const [kind,rows] of [["objective",this.program.objectives],["constraint",this.program.constraints]])for(const row of rows){
        if(row.enabled===false)continue;
        const response=this.catalogue.find(item=>item.id===row.response_id),label=response?.label||human(row.response_id)||human(kind),node=[...this.panel.querySelectorAll(".implexity-response-row")].find(item=>item.dataset.rowId===row.id);
        if(node&&!this._validateRow(node,row))errors.push(`${human(kind)} for ${label} contains an invalid visible value.`);
        if(!response)errors.push(`${human(kind)} for ${label} uses an unavailable response.`);
        else if(response.differentiable===false)errors.push(`${label} does not provide a derivative for optimisation.`);
        const scale=finiteNumber(row.scale);if(scale===null||scale<=0)errors.push(`${human(kind)} for ${label} requires a positive scale.`);
        if(kind==="objective"){
          if(!["minimize","maximize","target"].includes(row.sense))errors.push(`Objective for ${label} has an unsupported direction.`);
          if(row.sense==="target"&&finiteNumber(row.target)===null)errors.push(`Objective for ${label} requires a finite target.`);
          if(finiteNumber(row.weight)===null||finiteNumber(row.weight)<0)errors.push(`Objective for ${label} requires a finite non-negative weight.`);
        }else{
          if(!["<=",">=","="].includes(row.relation))errors.push(`Constraint for ${label} has an unsupported relation.`);
          if(finiteNumber(row.bound)===null)errors.push(`Constraint for ${label} requires a finite bound.`);
          if(finiteNumber(row.weight??1)===null||finiteNumber(row.weight??1)<0)errors.push(`Constraint for ${label} requires a finite non-negative penalty weight.`);
        }
      }
      return errors;
    }

    _updateActionAvailability(){
      const errors=this.validate(),invalid=errors.length>0;
      for(const name of ["apply","sensitivity"]){const control=this.panel.querySelector(`[data-action="${name}"]`);if(control){const busy=name==="apply"?this._applying:this._sensitivityPending,unavailable=invalid||busy;control.disabled=Boolean(busy);control.setAttribute("aria-disabled",String(unavailable));control.setAttribute("aria-description",busy?(name==="apply"?"The response program is currently being applied.":"Sensitivity is being evaluated."):invalid?errors[0]:name==="apply"?"Apply this valid response program to the authoritative workflow.":"Evaluate sensitivity for this valid response program.");}}
      if(!invalid&&this.status.dataset.validationError==="true"){delete this.status.dataset.validationError;this._setStatus("Response program is valid and ready to apply.");}
    }

    specification(){return clone(this.program);}

    _emitDraft(){global.dispatchEvent(new CustomEvent("implexity:response-program-draft",{detail:{program:this.specification()}}));this._schedulePhysicsPlan();}

    _intentPayload(){
      const goals=(this.program.objectives||[]).filter(r=>r.enabled!==false&&r.response_id).map(r=>({response:r.response_id,relation:r.sense==="target"?"equal":(r.sense||"minimize"),value:r.sense==="target"?finiteNumber(r.target):undefined,weight:number(r.weight,1)}));
      const relation={"<=":"less_equal",">=":"greater_equal","=":"equal"};
      const constraints=(this.program.constraints||[]).filter(r=>r.enabled!==false&&r.response_id).map(r=>({response:r.response_id,relation:relation[r.relation]||r.relation||"less_equal",value:number(r.bound)}));
      const authored=global.implexityWorkbench?.providerProblems?.intent_orchestrated;
      const base=authored?.intent?structuredClone(authored.intent):{};
       
       
      if(authored?.context?.provider_problems)base.authoring={...(base.authoring||{}),provider_problems:structuredClone(authored.context.provider_problems)};
      return {...base,goals,constraints,fidelity:this._providerFidelity||base.fidelity||"intermediate",provider_overrides:{...(base.provider_overrides||{}),...(this._providerOverrides||{})}};
    }

    _providerChoiceRows(plan){
      const choices=this._providerOverrides||{},candidates=plan.provider_candidates||{};
      return [...new Set([...Object.keys(candidates),...Object.keys(choices)])].sort().map(key=>({key,current:choices[key]||"",options:[...new Set([...(Array.isArray(candidates[key])?candidates[key]:[]),...(choices[key]?[choices[key]]:[])])]})).filter(row=>row.options.length>1||row.current||row.options.some(id=>window.ImplexityPhysicsPackages?.isImported?.(id)));
    }

    providerChoiceSpecification(){return {schema:"implexity-provider-choices/1",...(this._providerFidelity?{fidelity:this._providerFidelity}:{}),provider_overrides:{...(this._providerOverrides||{})}};}

    clearProviderChoices(){this._providerOverrides={};this._providerFidelity=null;this._providersChanged();}

    setProviderChoices(value){
      if(!value||typeof value!=="object"||Array.isArray(value)||value.schema!=="implexity-provider-choices/1")throw new Error("Unsupported provider-choice file.");
      const choices=value.provider_overrides;
      if(!choices||typeof choices!=="object"||Array.isArray(choices))throw new Error("Provider choices must be a mapping.");
      const rows=Object.entries(choices);
      if(rows.some(([key,id])=>!key.trim()||typeof id!=="string"||!id.trim()||["__proto__","constructor","prototype"].includes(key)))throw new Error("Provider choices require nonempty input/response keys and add-in identifiers.");
      if(value.fidelity!==undefined&&!["screening","intermediate","high"].includes(value.fidelity))throw new Error("Unsupported provider fidelity.");
      this._providerFidelity=value.fidelity||null;
      this._providerOverrides=Object.fromEntries(rows);
      this._providersChanged();
    }

    _providersChanged(){this._schedulePhysicsPlan();}

    saveProviderChoices(){
      const url=URL.createObjectURL(new Blob([JSON.stringify(this.providerChoiceSpecification(),null,2)],{type:"application/json"}));
      const link=document.createElement("a");link.href=url;link.download="implexity-provider-choices.json";
      document.body.appendChild(link);link.click();link.remove();setTimeout(()=>URL.revokeObjectURL(url),1000);
      this._setStatus("Provider-choice download requested. This is not a complete study backup.");
    }

    _renderProviderChoices(host,plan){
      const rows=this._providerChoiceRows(plan);if(!rows.length)return;
      const group=element("details","implexity-objective-section");group.append(element("summary","","Choose physics providers"));
      group.append(element("p","implexity-objective-subtitle","Optional choices for responses and coupled inputs. Changes replan without starting a solver. Use Save provider choices to retain them; Save program contains only objectives and constraints."));
      if(rows.some(row=>row.options.some(id=>window.ImplexityPhysicsPackages?.isImported?.(id)))){
        const label=element("label","","Required fidelity"),select=document.createElement("select");select.setAttribute("aria-label","Required physics fidelity");
        for(const [value,text] of [["screening","Approximation permitted"],["intermediate","Intermediate"],["high","High"]]){const option=document.createElement("option");option.value=value;option.textContent=text;select.append(option);}
        select.value=this._providerFidelity||this._intentPayload().fidelity;select.onchange=()=>{this._providerFidelity=select.value;this._providersChanged();};label.append(select);group.append(label);
      }
      for(const row of rows){
        const label=element("label","",human(row.key)),select=document.createElement("select");select.setAttribute("aria-label",`Physics provider for ${human(row.key)}`);
        const automatic=document.createElement("option");automatic.value="";automatic.textContent="Automatic selection";select.append(automatic);
        for(const id of row.options){const option=document.createElement("option");option.value=id;option.textContent=human(id)+(window.ImplexityPhysicsPackages?.isImported?.(id)?" (approximation)":"");select.append(option);}
        select.value=row.current;select.addEventListener("change",()=>{if(select.value)this._providerOverrides[row.key]=select.value;else delete this._providerOverrides[row.key];this._providersChanged();});
        label.append(select);group.append(label);
      }
      host.append(group);
    }

    _schedulePhysicsPlan(){
      clearTimeout(this._physicsPlanTimer);const sequence=++this._physicsPlanSequence;
      this._physicsPlanController?.abort?.();this._physicsPlanController=null;
      global.dispatchEvent(new CustomEvent("implexity:physics-plan",{detail:{plan:null,intent:this._intentPayload()}}));
      this._physicsPlanTimer=setTimeout(()=>this._refreshPhysicsPlan(sequence),180);
    }

    async _refreshPhysicsPlan(sequence=null){
      let requestSequence=sequence;
      if(!Number.isInteger(requestSequence)){
        requestSequence=++this._physicsPlanSequence;
        this._physicsPlanController?.abort?.();this._physicsPlanController=null;
      }
      if(requestSequence!==this._physicsPlanSequence)return;
      const host=this.panel.querySelector("[data-physics-plan-content]");if(!host)return;
      const errors=this.validate();if(errors.length){host.textContent=this.program.objectives.length?"Complete all required response values before physics planning.":"Define at least one target response. Implexity will infer the required installed physics add-ins and coupling graph.";return;}
      const intent=this._intentPayload();if(!(intent.goals.length||intent.constraints.length)){host.textContent="Define at least one target response. Implexity will infer the required installed physics add-ins and coupling graph.";return;}
      const controller=typeof global.AbortController==="function"?new global.AbortController():null;
      this._physicsPlanController=controller;
      const current=()=>requestSequence===this._physicsPlanSequence;
      host.textContent="Inferring required physics add-ins…";
      try{
        const response=await fetch("/v1/implicit/cae/orchestration/plan",{method:"POST",credentials:"same-origin",headers:{"Content-Type":"application/json"},body:JSON.stringify({intent}),...(controller?{signal:controller.signal}:{})});
        if(!current())return;
        if(!response.ok)throw new Error(`${response.status} ${response.statusText}: ${await response.text()}`);
        const plan=await response.json();if(!current())return;const status=String(plan.status||"unknown");const selected=Array.isArray(plan.selected_addins)?plan.selected_addins:[];
        const missingPhysics=Array.isArray(plan.missing_physics)?plan.missing_physics:[];const missingAuthoring=Array.isArray(plan.missing_authoring)?plan.missing_authoring:[];
        const title=status==="ready"?"Physics graph ready":status==="needs_authoring"?"Physics identified: boundary/model authoring required":"Physics graph incomplete";
        host.replaceChildren(element("strong","",title));
        host.append(document.createElement("br"),element("span","",selected.length?selected.map(human).join(" · "):"No executable add-in set selected."));
        if(missingPhysics.length)host.append(document.createElement("br"),element("span","",`Missing physics: ${missingPhysics.map(human).join(", ")}`));
        if(missingAuthoring.length)host.append(document.createElement("br"),element("span","",`Still to define: ${missingAuthoring.map(human).join(", ")}`));
        host.append(document.createElement("br"),element("span","","The solver/coupling graph is inferred by Implexity; expert overrides remain optional."));
        this._renderProviderChoices(host,plan);
        global.dispatchEvent(new CustomEvent("implexity:physics-plan",{detail:{plan,intent}}));
      }catch(error){if(current()&&error?.name!=="AbortError")uiPresentation.render(host,{summary:"Automatic physics planning is unavailable. Review the current responses and try again.",technical:error});}
      finally{if(current()&&this._physicsPlanController===controller)this._physicsPlanController=null;}
    }

    async apply(){
      if(this._applying)return this._applyPromise;
      const errors=this.validate();if(errors.length){this._reportValidation(errors);return;}
      this._applying=true;this._setStatus("Applying the objective and constraint program…");this._updateActionAvailability();
      const operation=(async()=>{
      try{
        const program=this.specification();let handled=false;
        adapters: for(const adapter of [global.ImplexityOptimization,global.ImplexityProject,global.implexityOptimization].filter(Boolean))for(const name of ["setResponseProgram","setObjectiveSpecification","updateOptimization","setOptimizationSpec"]){if(typeof adapter[name]==="function"){
          const result=await adapter[name](program);if(result?.ok===false)throw new Error(result.error||"The response program was rejected.");handled=true;break adapters;
        }}
        if(!handled){
          const response=await new Promise(resolve=>{
            let done=false;const finish=value=>{if(!done){done=true;resolve({handled:true,value});}};
            const event=new CustomEvent("implexity:response-program-apply",{detail:{program,respond:finish},cancelable:true});
            global.dispatchEvent(event);if(!event.defaultPrevented)queueMicrotask(()=>{if(!done)resolve({handled:false});});
          });
          handled=response.handled;
          if(response.value?.ok===false)throw new Error(response.value.error||"The response program was rejected.");
        }
        if(!handled)throw new Error("The active project controller did not accept the response program.");
        this._setStatus("Objective and constraint program applied.");
        return true;
      }catch(error){uiPresentation.render(this.status,{summary:"The objective and constraint program was not applied. Review the highlighted values and try again.",technical:error});this.status.setAttribute("aria-live","assertive");this.status.setAttribute("aria-atomic","true");return false;}
      })();
      this._applyPromise=operation;
      try{return await operation;}finally{if(this._applyPromise===operation)this._applyPromise=null;this._applying=false;this._updateActionAvailability();}
    }

    async sensitivity(){
      if(this._sensitivityPending)return;
      const errors=this.validate();if(errors.length){this._reportValidation(errors);return;}
      this._sensitivityPending=true;this._setStatus("Evaluating sensitivity…");this._updateActionAvailability();
      try{
        const build=()=>{const base={response_program:this.specification()};return typeof global.implexityBuildImplicitSensitivityRequest==="function"?global.implexityBuildImplicitSensitivityRequest(base):base;};
        const request=build(),identity=JSON.stringify(request);
        const response=await fetch("/v1/implicit/sensitivity",{method:"POST",credentials:"same-origin",headers:{"Content-Type":"application/json"},body:JSON.stringify(request)});
        if(!response.ok)throw new Error(`${response.status} ${response.statusText}: ${await response.text()}`);
        const value=await response.json();
        if(JSON.stringify(build())!==identity){this._setStatus("Sensitivity finished for an earlier setup. Evaluate again to use the current settings.");return;}
        const shown=this._renderSensitivity(value);this._setStatus(shown?"Sensitivity evaluated for the submitted setup.":"The service returned no displayable sensitivity data.");
      }catch(error){uiPresentation.render(this.status,{summary:"Sensitivity could not be evaluated. Confirm the current objective and physics setup, then try again.",technical:error});this.status.setAttribute("aria-live","assertive");this.status.setAttribute("aria-atomic","true");}
      finally{this._sensitivityPending=false;this._updateActionAvailability();}
    }

    _renderSensitivity(value){
      const section=this.panel.querySelector("[data-sensitivity]");const content=section.querySelector("[data-sensitivity-content]");section.hidden=false;
      content.replaceChildren();
      const rows=[];const matrix=value.response_jacobian||value.jacobian||value.derivatives||{};
      const maximum=row=>{
        let count=0,result=0;const pending=[row];
        while(pending.length){const item=pending.pop();if(Array.isArray(item)){for(const entry of item)pending.push(entry);}else{
          if(typeof item!=="number"||!Number.isFinite(item))throw new Error("Sensitivity data contain a non-finite or non-numeric derivative.");
          result=Math.max(result,Math.abs(item));count++;
        }}
        if(!count)throw new Error("Sensitivity data contain an empty derivative array.");
        return result;
      };
      if(value.gradient_coordinates||value.sensitivities){
        const members=value.sensitivities||{[value.response||"response"]:value};
        for(const [name,member] of Object.entries(members))for(const [coordinate,summary] of Object.entries(member.gradient_coordinates||{})){
          if(typeof summary.max_abs!=="number"||!Number.isFinite(summary.max_abs)||summary.max_abs<0)throw new Error("Sensitivity summaries contain an invalid derivative magnitude.");
          rows.push([`${name} / ${coordinate}`,summary.max_abs]);
        }
      }
      else if(Array.isArray(matrix)){matrix.slice(0,12).forEach((row,index)=>rows.push([`response ${index+1}`,maximum(row)]));}
      else Object.entries(matrix).slice(0,12).forEach(([name,row])=>rows.push([name,maximum(row)]));
      content.replaceChildren();
      if(!rows.length){content.textContent="No sensitivity arrays or coordinate summaries were returned.";return false;}
      const table=element("table","implexity-sensitivity-table"),head=document.createElement("thead"),headRow=document.createElement("tr"),body=document.createElement("tbody");
      headRow.append(element("th","","Response"),element("th","","max |∂R/∂z|"));head.append(headRow);
      for(const [name,value] of rows){const row=document.createElement("tr");row.append(element("td","",human(name)),element("td","",Number(value).toExponential(3)));body.append(row);}
      table.append(head,body);content.append(table);return true;
    }
  }

  function start(){if(global.IMPLEXITY_DISABLE_OBJECTIVES)return;try{if(!global.ImplexityObjectiveComposer)new ObjectiveComposer(global.IMPLEXITY_OBJECTIVE_OPTIONS||{});}catch(error){console.warn("Implexity objective composer did not start:",error);}}
  global.ImplexityObjectiveComposerClass=ObjectiveComposer;
  if(document.readyState==="loading")document.addEventListener("DOMContentLoaded",start,{once:true});else start();
})(window);
