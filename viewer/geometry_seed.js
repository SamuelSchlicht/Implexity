// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(function geometrySeedWorkspace(global){
  "use strict";

  const byId=id=>document.getElementById(id);
  const dialog=byId("geometrySeedDialog");
  const openButton=byId("newgeometry");
  if(!dialog||!openButton)return;

  const state={catalogue:null,recipes:[],selected:null,values:new Map(),free:new Set(),freeBounds:new Map(),
    parametricFree:null,current:null,preview:null,previewSerial:0,controller:null,
    workspaceSerial:0,loading:false,committing:false,capabilityAvailable:null,
    workflowControlStates:null,commitControlStates:null,workflow:"create",returnFocus:null};

  const WORKFLOW_CONTROLS=".seed-body button,.seed-body input,.seed-body select,.seed-body textarea,#geometrySeedPreview,#geometrySeedCommit";
  const ALL_CONTROLS="button,input,select,textarea";

  const element=(tag,className,text)=>{
    const node=document.createElement(tag);
    if(className)node.className=className;
    if(text!==undefined)node.textContent=String(text);
    return node;
  };
  const human=(id,metadata={})=>global.ImplexityText?.present?.(id,metadata)?.label||
    String(metadata.label||metadata.title||id||"").replace(/[_./:-]+/g," ")
      .replace(/\b\w/g,c=>c.toUpperCase());
  const technical=value=>global.ImplexityUIPresentation?.technicalText?.(value)||(()=>{
    try{return typeof value==="string"?value:JSON.stringify(value,null,2);}catch(_){return String(value||"");}
  })();
  const setStatus=(message,kind="")=>{
    const host=byId("geometrySeedStatus");host.textContent=String(message||"");
    host.dataset.kind=kind;
  };
  const errorSummary=(error,fallback)=>global.ImplexityUIPresentation?.publicText?.(
    error?.problems?.[0]||error?.message||error?.error,fallback)||fallback;

  function setControlLock(slot,locked,controls=[]){
    if(!locked){
      const saved=state[slot];
      if(saved)for(const [control,disabled] of saved)if(control.isConnected!==false)control.disabled=disabled;
      state[slot]=null;return;
    }
    const saved=state[slot]||(state[slot]=new Map());
    for(const control of controls){
      if(!("disabled" in control))continue;
      if(!saved.has(control))saved.set(control,Boolean(control.disabled));
      control.disabled=true;
    }
  }

  async function request(path,{method="GET",body,signal}={}){
    const response=await fetch(path,{method,signal,headers:body?{"content-type":"application/json"}:undefined,
      body:body?JSON.stringify(body):undefined});
    let payload=null;try{payload=await response.json();}catch(_){payload=null;}
    if(!response.ok){
      const error=new Error(payload?.error||payload?.detail||`${response.status} ${response.statusText}`);
      error.status=response.status;error.problems=payload?.problems||[];error.payload=payload;throw error;
    }
    return payload;
  }

  function recipesFrom(payload){
    const rows=payload?.seeds||payload?.recipes||[];
    return Array.isArray(rows)?rows:[];
  }
  function parametersOf(recipe){
    const raw=recipe?.parameters||[];
    if(Array.isArray(raw))return raw.map(p=>({...p,key:String(p.key||p.id||p.name||"")}));
    return Object.entries(raw).map(([key,p])=>({key,...(p||{})}));
  }
  function modesFrom(payload){
    const modes=payload?.modes||["parametric"];
    return new Set(modes.map(mode=>typeof mode==="string"?mode:mode.id).filter(Boolean));
  }
  function updateRecipeModeAvailability(){
    if(state.workflow==="bake_current")return;
    const globalModes=modesFrom(state.catalogue||{});
    const recipeModes=new Set((state.selected?.modes||["parametric"]).map(mode=>
      typeof mode==="string"?mode:mode.id).filter(Boolean));
    const input=dialog.querySelector('input[value="bake_editable"]');
    const available=globalModes.has("bake_editable")&&recipeModes.has("bake_editable");
    input.disabled=!available;byId("geometrySeedBakeCard").classList.toggle("unavailable",!available);
    byId("geometrySeedBakeCard").title=available?"":"This recipe does not provide an editable topology bake.";
    if(!available&&input.checked){
      dialog.querySelector('input[value="parametric"]').checked=true;state.parametricFree=null;
    }
  }
  function recipeMark(recipe){
    const category=String(recipe.category||"").toLowerCase();
    if(category.includes("lattice"))return "▦";
    if(category.includes("tube")||category.includes("flow"))return "◎";
    if(category.includes("plate"))return "▤";
    if(category.includes("round"))return "●";
    return "◇";
  }

  function resetRecipeValues(){
    state.values.clear();state.free.clear();state.freeBounds.clear();
    for(const parameter of parametersOf(state.selected)){
      state.values.set(parameter.key,parameter.default);
      if(parameter.default_free||parameter.free_default)state.free.add(parameter.key);
    }
    if(selectedMode()==="bake_editable"){
      state.parametricFree=new Set(state.free);state.free.clear();
    }
    invalidatePreview("Defaults restored. Preview the recipe when ready.");
    renderParameters();updateControls();
  }

  function chooseRecipe(recipe,{focus=false}={}){
    if(!recipe)return;
    state.selected=recipe;state.preview=null;
    updateRecipeModeAvailability();resetRecipeValues();renderRecipes();renderRecipeSummary();updateControls();
    if(focus)byId("geometrySeedParameters")?.querySelector("input,select")?.focus();
  }

  function renderRecipes(){
    const host=byId("geometrySeedRecipes");host.replaceChildren();
    const query=byId("geometrySeedFilter").value.trim().toLowerCase();
    const shown=state.recipes.filter(recipe=>!query||[
      recipe.label,recipe.description,recipe.category,...(recipe.tags||[])
    ].join(" ").toLowerCase().includes(query));
    byId("geometrySeedCount").textContent=`${shown.length} of ${state.recipes.length}`;
    if(!shown.length){
      const copy=state.capabilityAvailable===false
        ? "Geometry recipes are unavailable from this service."
        : (query?"No recipes match this search.":"No geometry recipes are available.");
      host.append(element("div","seed-empty",copy));return;
    }
    for(const recipe of shown){
      const button=element("button","seed-recipe");button.type="button";
      button.setAttribute("aria-pressed",String(state.selected?.id===recipe.id));
      button.dataset.seedId=recipe.id;
      const mark=element("span","seed-recipe-mark",recipeMark(recipe));mark.setAttribute("aria-hidden","true");
      const copy=element("span","seed-recipe-copy");
      copy.append(element("strong","",human(recipe.id,recipe)));
      copy.append(element("small","",recipe.summary||recipe.description||"Editable parametric geometry"));
      button.append(mark,copy);
      button.addEventListener("click",()=>chooseRecipe(recipe,{focus:true}));
      host.append(button);
    }
  }

  function renderRecipeSummary(){
    const recipe=state.selected;
    byId("geometrySeedCategory").textContent=recipe?human(recipe.category||"Parametric seed"):"Select a recipe";
    byId("geometrySeedRecipeTitle").textContent=recipe?human(recipe.id,recipe):"Choose a starting geometry";
    byId("geometrySeedRecipeDescription").textContent=recipe?.description||
      "Recipes are supplied through the geometry add-in registry and produce ordinary editable model documents.";
    byId("geometrySeedGlyph").dataset.category=String(recipe?.category||"").toLowerCase();
    const features=byId("geometrySeedFeatures");features.replaceChildren();
    const labels=recipe?(recipe.features||recipe.tags||[
      "Dimensioned","Implicit DAG","Provenance retained"
    ]):[];
    labels.slice(0,5).forEach(label=>features.append(element("span","seed-feature",human(label))));
  }

  function setValue(parameter,input){
    let value;
    const type=String(parameter.type||"number").toLowerCase();
    if(type==="boolean"||type==="bool")value=Boolean(input.checked);
    else if(type==="number"||type==="float"||type==="integer"||type==="int"){
      value=input.value.trim()===""?null:Number(input.value);
      if((type==="integer"||type==="int")&&Number.isFinite(value))value=Math.trunc(value);
    }else value=input.value;
    state.values.set(parameter.key,value);invalidatePreview("Dimensions changed. Preview again to verify the new model.");
    validateParameter(parameter,input);updateControls();
  }

  function validateParameter(parameter,input){
    const card=input.closest(".seed-parameter"),error=card?.querySelector(".seed-field-error");
    const type=String(parameter.type||"number").toLowerCase();let message="";
    if(type==="number"||type==="float"||type==="integer"||type==="int"){
      const value=Number(input.value);
      if(input.value.trim()===""||!Number.isFinite(value))message="Enter a finite number.";
      else if(parameter.min!=null&&value<Number(parameter.min))message=`Use ${parameter.min} or more.`;
      else if(parameter.max!=null&&value>Number(parameter.max))message=`Use ${parameter.max} or less.`;
      else if((type==="integer"||type==="int")&&!Number.isInteger(value))message="Enter a whole number.";
    }else if(parameter.required&&String(input.value||"").trim()==="")message="This value is required.";
    card?.classList.toggle("invalid",Boolean(message));if(error)error.textContent=message;
    input.setAttribute("aria-invalid",String(Boolean(message)));return !message;
  }

  function inputFor(parameter){
    const type=String(parameter.type||"number").toLowerCase();let input;
    if(type==="choice"||type==="enum"||Array.isArray(parameter.choices)){
      input=element("select");
      for(const choice of parameter.choices||[]){
        const value=typeof choice==="object"?choice.value:choice;
        const option=element("option","",typeof choice==="object"?(choice.label||human(value)):human(value));
        option.value=String(value);input.append(option);
      }
      input.value=String(state.values.get(parameter.key)??parameter.default??"");
    }else if(type==="boolean"||type==="bool"){
      input=element("input");input.type="checkbox";input.checked=Boolean(state.values.get(parameter.key));
    }else{
      input=element("input");input.type=(type==="text"||type==="string")?"text":"number";
      const value=state.values.get(parameter.key);input.value=value===undefined||value===null?"":String(value);
      if(parameter.min!=null)input.min=String(parameter.min);
      if(parameter.max!=null)input.max=String(parameter.max);
      input.step=String(parameter.step??((type==="integer"||type==="int")?1:"any"));
      input.inputMode=input.type==="number"?"decimal":"text";
    }
    input.id=`geometrySeedParam_${parameter.key.replace(/[^A-Za-z0-9_-]/g,"_")}`;
    input.setAttribute("aria-label",human(parameter.key,parameter));
    input.addEventListener("input",()=>setValue(parameter,input));
    input.addEventListener("change",()=>setValue(parameter,input));
    return input;
  }

  function renderParameters(){
    const host=byId("geometrySeedParameters");host.replaceChildren();
    const parameters=parametersOf(state.selected);
    if(!parameters.length){
      host.append(element("div","seed-empty",state.selected?
        "This recipe has no configurable dimensions.":"Select a recipe to see its dimensions."));return;
    }
    for(const parameter of parameters){
      const card=element("div","seed-parameter");card.dataset.parameter=parameter.key;
      const label=element("label","",human(parameter.key,parameter));
      if(parameter.help||parameter.description)label.append(element("span","",parameter.help||parameter.description));
      const wrap=element("div","seed-input-wrap"),input=inputFor(parameter);
      label.htmlFor=input.id;wrap.append(input);
      if(parameter.units||parameter.unit)wrap.append(element("span","seed-unit",parameter.units||parameter.unit));
      card.append(label,wrap);
      const optimisable=parameter.optimisable!==false&&!["boolean","bool","choice","enum","text","string"]
        .includes(String(parameter.type||"number").toLowerCase());
      if(optimisable){
        const free=element("label","seed-free");const check=element("input");check.type="checkbox";
        check.checked=state.free.has(parameter.key);check.setAttribute("aria-label",`Keep ${human(parameter.key,parameter)} as an editable parametric design variable`);
        check.disabled=state.workflow==="bake_current"||selectedMode()!=="parametric";
        if(check.disabled)free.title="A baked topology field replaces these live parametric controls.";
        check.addEventListener("change",()=>{
          if(check.checked)state.free.add(parameter.key);else state.free.delete(parameter.key);
          invalidatePreview("Design freedom changed. Preview again to verify the new model.");updateControls();
        });
        free.append(check,document.createTextNode(" Parametric design variable"));card.append(free);
        const limits=element("div","seed-free-bounds");limits.hidden=!check.checked;
        const pair=state.freeBounds.get(parameter.key)||{min:parameter.min??"",max:parameter.max??""};
        state.freeBounds.set(parameter.key,pair);
        for(const side of ["min","max"]){
          const boundLabel=element("label","",side==="min"?"Optimization minimum":"Optimization maximum");
          const bound=element("input");bound.type="number";bound.step="any";bound.value=pair[side];
          bound.setAttribute("aria-label",`${human(parameter.key,parameter)} optimization ${side}`);
          bound.addEventListener("input",()=>{pair[side]=bound.value;invalidatePreview("Optimization bounds changed. Preview again.");updateControls();});
          boundLabel.append(bound);limits.append(boundLabel);
        }
        check.addEventListener("change",()=>{limits.hidden=!check.checked;updateControls();});
        card.append(limits);
      }
      const problem=element("div","seed-field-error");problem.id=`${input.id}_error`;
      input.setAttribute("aria-describedby",problem.id);card.append(problem);host.append(card);
      validateParameter(parameter,input);
    }
  }

  function selectedMode(){return state.workflow==="bake_current"?"bake_editable":
    (dialog.querySelector('input[name="geometrySeedMode"]:checked')?.value||"parametric");}
  function bakeOptions(){return {spacing_mm:Number(byId("geometrySeedSpacing").value),
    padding_cells:Number(byId("geometrySeedPadding").value),
    iso_mm:Number(byId("geometrySeedIso").value)};}
  function validBakeOptions(){
    const bake=bakeOptions();
    const checks=[
      ["geometrySeedSpacing",Number.isFinite(bake.spacing_mm)&&bake.spacing_mm>=0.05&&bake.spacing_mm<=10,"Use a finite spacing from 0.05 to 10 mm."],
      ["geometrySeedPadding",Number.isInteger(bake.padding_cells)&&bake.padding_cells>=0&&bake.padding_cells<=64,"Use a whole number from 0 to 64 cells."],
      ["geometrySeedIso",Number.isFinite(bake.iso_mm),"Enter a finite source isovalue."]
    ];
    for(const [id,valid,message] of checks){
      const input=byId(id),label=input.closest("label"),error=byId(`${id}Error`);
      input.setAttribute("aria-invalid",String(!valid));label.classList.toggle("invalid",!valid);
      error.textContent=valid?"":message;
    }
    return checks.every(([,valid])=>valid);
  }
  function validInputs(){
    if(state.workflow==="bake_current")return Boolean(state.current?.loaded)&&validBakeOptions();
    if(!state.selected)return false;
    const parametersValid=[...byId("geometrySeedParameters").querySelectorAll("input,select")]
      .filter(input=>!input.closest(".seed-free")).every(input=>{
        const parameter=parametersOf(state.selected).find(p=>input.id.endsWith(p.key.replace(/[^A-Za-z0-9_-]/g,"_")));
        return parameter?validateParameter(parameter,input):true;
      });
    const boundsValid=[...state.free].every(key=>{
      const pair=state.freeBounds.get(key),value=Number(state.values.get(key));
      return pair&&String(pair.min).trim()!==""&&String(pair.max).trim()!==""&&
        Number.isFinite(Number(pair.min))&&Number.isFinite(Number(pair.max))&&
        Number(pair.min)<Number(pair.max)&&Number(pair.min)<=value&&value<=Number(pair.max);
    });
    return parametersValid&&boundsValid&&(selectedMode()!=="bake_editable"||validBakeOptions());
  }
  function requestBody(){
    if(state.workflow==="bake_current")return {expected_content_id:state.current?.content_id,bake:bakeOptions()};
    const body={seed_id:state.selected.id,version:state.selected.version,
      parameters:Object.fromEntries(state.values),free:[...state.free].sort(),mode:selectedMode()};
    body.free_bounds=Object.fromEntries([...state.free].map(key=>{const p=state.freeBounds.get(key);return [key,{min:Number(p?.min),max:Number(p?.max)}]}));
    if(selectedMode()==="bake_editable")body.bake=bakeOptions();
    return body;
  }
  function invalidatePreview(message){
    state.preview=null;byId("geometrySeedCommit").disabled=true;
    byId("geometrySeedReviewTitle").textContent="Preview required";
    const review=byId("geometrySeedReviewBody");review.classList.remove("ready");review.textContent=message;
    byId("geometrySeedTechnicalBody").textContent="";byId("geometrySeedTechnical").open=false;
  }

  function metric(label,value){
    const node=element("div","seed-review-metric");node.append(element("small","",label),element("strong","",value));return node;
  }
  function renderPreview(payload){
    if(!payload?.content_id||!/^[0-9a-f]{64}$/.test(String(payload?.sha256||""))){
      throw new Error("The service preview did not include a complete content identity. Nothing can be committed safely.");
    }
    state.preview=payload;
    const doc=payload.document||payload.model?.document||{};
    const graph=payload.graph||payload.model?.graph||[];
    const parameters=Array.isArray(payload.parameters)?payload.parameters:Object.keys(doc.parameters||{});
    const nodeCount=graph.length||Object.keys(doc.nodes||{}).length;
    const native=Object.values(doc.nodes||{}).some(node=>["lattice.controlled", "lattice.controlled_volume"].includes(node.kind||node.type||node.op));
    const body=byId("geometrySeedReviewBody");body.replaceChildren();body.classList.add("ready");
    const metrics=element("div","seed-review-metrics");
    metrics.append(metric("Nodes",nodeCount),
      metric("Parameters",parameters.length),metric("Representation",native?"Native spatial controls":selectedMode()==="parametric"?"Parametric graph":"Baked occupancy field"),
      metric("Identity",String(payload.content_id||payload.model?.content_id||"validated").slice(0,12)));
    body.append(metrics);
    const bake=payload.bake||payload.registration;
    if(bake?.shape){
      body.append(element("div","seed-review-registration",
        `${bake.shape.join(" × ")} registered cells · ${Number(bake.cells||0).toLocaleString()} total · ${bake.spacing_mm.join(" × ")} mm spacing · cell centres`));
    }
    if(payload.deactivated_free_parameters?.length){
      body.append(element("div","seed-review-warning",
        `${payload.deactivated_free_parameters.length} live parametric control${payload.deactivated_free_parameters.length===1?" is":"s are"} archived by this explicit hand-off. The retained source remains inspectable, but topology edits will control the occupancy field.`));
    }
    const note=element("div","seed-review-note",native?
      "This is a native controlled volume with twenty component fields per volume. Recipe dimensions define its initial state. Subsequent edits and optimization use the live model:control fields, not a second independent set of recipe dimensions.":selectedMode()==="parametric"?
      "The named dimensions remain live. You can edit the tree directly, manipulate the implicit surface, or expose selected parameters to optimisation.":
      "The occupancy field uses explicit cell-centre registration. The complete source model and dimensions remain as provenance and a named output, but later field edits do not silently rebase when archived dimensions change.");
    body.append(note);
    byId("geometrySeedReviewTitle").textContent="Preview validated";
    byId("geometrySeedTechnicalBody").textContent=technical(payload);
    updateControls();
  }

  function renderFailure(error){
    state.preview=null;const body=byId("geometrySeedReviewBody");body.replaceChildren();body.classList.remove("ready");
    const summary=errorSummary(error,"The geometry recipe could not be built. Review the highlighted dimensions and try again.");
    for(const problem of error?.problems||[]){
      const raw=String(problem);
      for(const parameter of parametersOf(state.selected)){
        if(!raw.includes(`parameters.${parameter.key}`))continue;
        const card=[...(byId("geometrySeedParameters")?.children||[])].find(row=>row.dataset.parameter===parameter.key);
        if(card){card.classList.add("invalid");const message=card.querySelector(".seed-field-error");if(message)message.textContent=`Review ${human(parameter.key,parameter).toLowerCase()} and its allowed range.`;}
      }
    }
    body.append(element("div","",summary));
    byId("geometrySeedReviewTitle").textContent="Preview needs attention";
    byId("geometrySeedTechnicalBody").textContent=technical(error?.payload||error);
    setStatus(summary,"error");updateControls();
  }

  function renderCapabilityUnavailable(summary,technicalDetail){
    state.capabilityAvailable=false;state.preview=null;state.selected=null;state.recipes=[];
    renderRecipes();renderParameters();
    const current=state.workflow==="bake_current";
    byId("geometrySeedCategory").textContent=current?"Current geometry":"Geometry capability";
    byId("geometrySeedRecipeTitle").textContent=current?"Topology hand-off unavailable":"Geometry recipes unavailable";
    byId("geometrySeedRecipeDescription").textContent=summary;
    byId("geometrySeedFeatures").replaceChildren();
    const body=byId("geometrySeedReviewBody");body.replaceChildren(element("div","seed-unavailable",summary));
    body.classList.remove("ready");
    byId("geometrySeedReviewTitle").textContent=current?"Topology hand-off unavailable":"Geometry creation unavailable";
    byId("geometrySeedTechnicalBody").textContent=technical(technicalDetail);
    byId("geometrySeedTechnical").open=false;
    setStatus(summary,"error");updateControls();
  }

  async function preview(){
    if(state.loading||state.committing||!validInputs()){
      setStatus("Complete the highlighted dimensions before previewing.","error");return;
    }
    const serial=++state.previewSerial;state.controller?.abort();state.controller=new AbortController();
    state.loading=true;updateControls();setStatus(state.workflow==="bake_current"?
      "Sampling the current model without changing it…":"Building and validating the recipe…");
    try{
      const path=state.workflow==="bake_current"?
        "/v1/implicit/seeds/bake-current/preview":"/v1/implicit/seeds/preview";
      const payload=await request(path,{method:"POST",body:requestBody(),signal:state.controller.signal});
      if(serial!==state.previewSerial)return;renderPreview(payload);setStatus("Preview validated. The stored model is unchanged.","ok");
    }catch(error){if(error.name!=="AbortError"&&serial===state.previewSerial)renderFailure(error);}
    finally{if(serial===state.previewSerial){state.loading=false;updateControls();}}
  }

  async function commit(){
    if(state.committing||state.capabilityAvailable!==true||!state.preview)return;
    if((state.current?.loaded||state.workflow==="bake_current")&&!byId("geometrySeedReplace").checked){
      setStatus(state.workflow==="bake_current"?
        "Confirm the explicit topology hand-off before applying it.":
        "Confirm replacement of the current model before creating geometry.","error");return;
    }
    state.committing=true;updateControls();setStatus(state.workflow==="bake_current"?
      "Handing the current model to editable topology…":"Creating the authoritative model…");
    const previewGuard={expected_preview_content_id:state.preview.content_id,
      expected_preview_sha256:state.preview.sha256};
    const body=state.workflow==="bake_current"?
      {...requestBody(),...previewGuard}:
      {...requestBody(),expected_content_id:state.current?.loaded?state.current.content_id:null,...previewGuard};
    try{
      const path=state.workflow==="bake_current"?
        "/v1/implicit/seeds/bake-current":"/v1/implicit/seeds/commit";
      const payload=await request(path,{method:"POST",body});
      setStatus(state.workflow==="bake_current"?
        "Topology hand-off complete. Opening the shared editable field…":
        "Geometry created. Opening it in the model workspace…","ok");
      dialog.close("created");
      if(typeof global.loadModel==="function")await global.loadModel();
      await global.implexityWorkbench?.refresh?.();
      global.implexityWorkbench?.setStep?.("geometry");
      global.dispatchEvent(new CustomEvent("implexity-geometry-seed-created",{detail:payload}));
    }catch(error){renderFailure(error);}
    finally{state.committing=false;updateControls();}
  }

  function updateMode(){
    const mode=selectedMode();dialog.querySelectorAll(".seed-mode-card").forEach(card=>
      card.classList.toggle("selected",card.querySelector("input")?.checked));
    byId("geometrySeedBakeOptions").hidden=mode!=="bake_editable";
    if(state.workflow!=="bake_current"){
      if(mode==="bake_editable"){
        if(state.parametricFree===null)state.parametricFree=new Set(state.free);
        state.free.clear();
      }else if(state.parametricFree!==null){
        state.free=new Set(state.parametricFree);state.parametricFree=null;
      }
      renderParameters();
    }
    invalidatePreview(mode==="parametric"?
      "Parametric mode selected. Preview the editable DAG before creating it.":
      "Topology handoff selected. Preview the sampling registration and field size before creating it.");
    updateControls();
  }
  function updateControls(){
    const workflowBlocked=state.loading||state.capabilityAvailable===false||state.committing;
    if(!state.committing)setControlLock("commitControlStates",false);
    if(!workflowBlocked)setControlLock("workflowControlStates",false);
    if(!workflowBlocked&&state.capabilityAvailable===true)updateRecipeModeAvailability();
    const valid=state.capabilityAvailable===true&&
      (state.workflow==="bake_current"||state.selected)&&validInputs();
    byId("geometrySeedPreview").disabled=!valid||state.loading||state.committing;
    byId("geometrySeedPreview").textContent=state.loading?"Building preview…":
      (state.workflow==="bake_current"?"Preview hand-off":"Preview recipe");
    const replacementOK=!state.current?.loaded||byId("geometrySeedReplace").checked;
    byId("geometrySeedCommit").disabled=!state.preview||!replacementOK||state.loading||state.committing;
    byId("geometrySeedCommit").textContent=state.committing?
      (state.workflow==="bake_current"?"Handing off…":"Creating…"):
      (state.workflow==="bake_current"?"Hand off to topology editing":
        (state.current?.loaded?"Replace with this geometry":"Create geometry"));
    dialog.dataset.capability=state.capabilityAvailable===false?"unavailable":
      (state.loading?"loading":"available");
    dialog.setAttribute("aria-busy",String(state.committing));
    dialog.classList.toggle("committing",state.committing);
    if(workflowBlocked)setControlLock("workflowControlStates",true,dialog.querySelectorAll(WORKFLOW_CONTROLS));
    if(state.committing)setControlLock("commitControlStates",true,dialog.querySelectorAll(ALL_CONTROLS));
  }

  function setProgress(labels){
    dialog.querySelectorAll(".seed-progress span").forEach((step,index)=>{
      step.replaceChildren(element("b","",index+1),document.createTextNode(` ${labels[index]||""}`));
      step.classList.toggle("active",index===0);
    });
  }

  function configureWorkflow(){
    const current=state.workflow==="bake_current";
    dialog.classList.toggle("bake-current",current);
    byId("geometrySeedTitle").textContent=current?"Hand off current geometry":"Create initial geometry";
    byId("geometrySeedIntro").textContent=current?
      "Review how the current parametric model becomes the one occupancy field shared by manual tools and direct-gradient topology optimisation.":
      "Start with a dimensioned implicit model. Its parameters, nodes and provenance remain visible and editable.";
    setProgress(current?["Current model","Registration","Review","Hand off"]:
      ["Choose","Configure","Representation","Create"]);
    byId("geometrySeedParameterSection").hidden=current;
    const legend=byId("geometrySeedMode").querySelector("legend");
    legend.replaceChildren(element("span","seed-eyebrow",current?"Topology hand-off":"Representation"),
      document.createTextNode(current?" Sampling registration":" What should remain editable?"));
    byId("geometrySeedBakeOptions").hidden=!current&&selectedMode()!=="bake_editable";
    byId("geometrySeedReplaceRow").hidden=current?!state.current?.loaded:!state.current?.loaded;
    byId("geometrySeedConfirmTitle").textContent=current?"Confirm topology hand-off":"Replace the current model";
    byId("geometrySeedConfirmCopy").textContent=current?
      "The current DAG is retained as a named source and exact provenance, while its sampled occupancy becomes the authoritative editable root. The service refuses if the model changed after preview.":
      "This creates a new model identity. The service will refuse if the current model changed after this dialog opened.";
    byId("geometrySeedReplace").checked=false;
    if(current&&state.current?.loaded){
      const doc=state.current.document||{};
      byId("geometrySeedCategory").textContent="Current authoritative model";
      byId("geometrySeedRecipeTitle").textContent=human(state.current.name||doc.name||"Current geometry");
      byId("geometrySeedRecipeDescription").textContent=
        "Its complete parametric DAG remains recoverable as a named source. This hand-off only changes which representation subsequent local edits and topology updates control.";
      const features=byId("geometrySeedFeatures");features.replaceChildren();
      ["Identity guarded",`${Object.keys(doc.nodes||{}).length} nodes`,
        `${(state.current.parameters||[]).filter(p=>p.free).length} free parameters`,"Cell-centred field"]
        .forEach(label=>features.append(element("span","seed-feature",label)));
      byId("geometrySeedGlyph").dataset.category="topology";
      invalidatePreview("Choose the field registration, then preview the exact hand-off. The current model remains unchanged until you confirm it.");
    }
  }

  async function loadWorkspace(){
    const serial=++state.workspaceSerial;
    state.capabilityAvailable=null;state.loading=true;setStatus("Loading geometry recipes…");updateControls();
    try{
      const [catalogue,current]=await Promise.all([
        request("/v1/implicit/seeds"),request("/v1/implicit/model").catch(()=>({loaded:false}))
      ]);
      if(serial!==state.workspaceSerial)return;
      state.catalogue=catalogue;state.recipes=recipesFrom(catalogue);state.current=current;
      if(state.workflow==="bake_current"){
        configureWorkflow();
        if(!current?.loaded)throw Object.assign(new Error("No current geometry is available to hand off."),{status:409});
        if(!catalogue?.bake_current?.preview_endpoint)throw Object.assign(
          new Error("This service does not expose a safe current-model bake preview."),{status:409});
        state.capabilityAvailable=true;
        setStatus("Set the cell registration and preview the topology hand-off.");
      }else{
        configureWorkflow();
        if(!state.recipes.length){
          renderCapabilityUnavailable("No geometry recipes are registered.",catalogue);return;
        }
        state.capabilityAvailable=true;
        if(!state.selected||!state.recipes.some(recipe=>recipe.id===state.selected.id))
          chooseRecipe(state.recipes[0]);
        else{updateRecipeModeAvailability();renderRecipes();renderRecipeSummary();renderParameters();}
        setStatus("Choose a recipe and adjust its dimensions.");
      }
    }catch(error){
      if(serial!==state.workspaceSerial)return;
      state.catalogue=null;state.current=null;configureWorkflow();
      const summary=error.status===404?
        "This service does not yet expose the parametric geometry recipe capability.":
        errorSummary(error,"Geometry recipes could not be loaded from the service.");
      renderCapabilityUnavailable(summary,error?.payload||error);
    }finally{if(serial===state.workspaceSerial){state.loading=false;updateControls();}}
  }

  function open(){
    state.returnFocus=document.activeElement;state.workflow="create";state.preview=null;state.parametricFree=null;state.capabilityAvailable=null;
    if(!dialog.open)dialog.showModal();
    byId("geometrySeedFilter").value="";loadWorkspace();
    requestAnimationFrame(()=>byId("geometrySeedFilter").focus());
  }
  function openBakeCurrent(){
    state.returnFocus=document.activeElement;state.workflow="bake_current";state.preview=null;state.parametricFree=null;state.capabilityAvailable=null;
    if(!dialog.open)dialog.showModal();
    loadWorkspace();
    requestAnimationFrame(()=>byId("geometrySeedSpacing").focus());
  }
  function close(){
    if(state.committing){
      setStatus("The geometry change is being committed. Keep this window open until it finishes.");
      return false;
    }
    ++state.workspaceSerial;state.controller?.abort();state.loading=false;
    updateControls();
    if(dialog.open)dialog.close("cancel");
    const target=state.returnFocus?.isConnected&&typeof state.returnFocus.focus==="function"?state.returnFocus:openButton;
    target.focus();
    return true;
  }

  openButton.addEventListener("click",open);
  byId("geometrySeedClose").addEventListener("click",close);
  byId("geometrySeedCancel").addEventListener("click",close);
  byId("geometrySeedPreview").addEventListener("click",preview);
  byId("geometrySeedCommit").addEventListener("click",commit);
  byId("geometrySeedReset").addEventListener("click",resetRecipeValues);
  byId("geometrySeedFilter").addEventListener("input",renderRecipes);
  byId("geometrySeedReplace").addEventListener("change",updateControls);
  dialog.querySelectorAll('input[name="geometrySeedMode"]').forEach(input=>input.addEventListener("change",updateMode));
  ["geometrySeedSpacing","geometrySeedPadding","geometrySeedIso"].forEach(id=>
    byId(id).addEventListener("input",()=>{invalidatePreview("Sampling settings changed. Preview again to verify the topology field.");updateControls();}));
  dialog.addEventListener("click",event=>{if(event.target===dialog)close();});
  dialog.addEventListener("cancel",event=>{event.preventDefault();close();});

  global.addEventListener("implexity-request-topology-handoff",openBakeCurrent);
  global.ImplexityGeometrySeeds=Object.freeze({open,openBakeCurrent,close,preview,commit,state,requestBody});
})(window);
