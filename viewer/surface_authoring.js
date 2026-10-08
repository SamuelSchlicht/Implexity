// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(() => {
  "use strict";
  const MODES = ["select", "move", "size", "surface", "region", "pressure", "traction", "heat_flux", "temperature", "clamp", "brush"];
  const MODE_LABELS = Object.freeze({
    select:"Select", move:"Move geometry", size:"Change size or thickness",
    surface:"Differentiable surface drag", region:"Surface region",
    pressure:"Pressure", traction:"Traction", heat_flux:"Heat flux",
    temperature:"Prescribed temperature", clamp:"Clamp", brush:"Field brush"
  });
  const OPTION_PROFILES = Object.freeze({
    region:{title:"Surface region settings",radius:["Patch radius","mm"],snap:["Snap values to engineering increments",""]},
    pressure:{title:"Pressure settings",radius:["Patch radius","mm"],magnitude:["Pressure","Pa"],snap:["Snap values to engineering increments",""]},
    traction:{title:"Traction settings",radius:["Patch radius","mm"],magnitude:["Traction","Pa"],snap:["Snap values to engineering increments",""]},
    heat_flux:{title:"Heat flux settings",radius:["Patch radius","mm"],magnitude:["Heat flux","W/m²"],snap:["Snap values to engineering increments",""]},
    temperature:{title:"Temperature settings",radius:["Patch radius","mm"],magnitude:["Temperature","K"],snap:["Snap values to engineering increments",""]},
    clamp:{title:"Clamp settings",radius:["Patch radius","mm"],snap:["Snap values to engineering increments",""]},
    brush:{title:"Field brush settings",radius:["Brush radius","mm"]}
  });
  const state = {
    mode: "select", hit: null, problem: null, problemRevision: null,
    snap: true, linearSnap: 0.1, magnitudeSnap: 1.0,
    patchRadius: 2.0, magnitude: 1.0, vector: [0, 0, -1],
    activeObject: null, activePointer: null, baseline: null,
  };

  function q(sel, root=document) { return root.querySelector(sel); }
  function delegatedNative() { return Boolean(q("#stage[data-implexity-viewport], #stage") && q("script[data-implexity-interaction]")); }
  function el(tag, cls, text) { const n=document.createElement(tag); if(cls)n.className=cls; if(text!==undefined)n.textContent=text; return n; }
  function localLabel(identifier) {
    const raw=String(identifier==null?"":identifier).trim();
    if(!raw)return "";
    if(!/^[A-Za-z][A-Za-z0-9_.:-]*$/.test(raw))return raw;
    const words=raw.split(".").pop().replace(/([a-z0-9])([A-Z])/g,"$1 $2").replace(/[_:-]+/g," ").trim().split(/\s+/).filter(Boolean);
    if(!words.length)return raw;
    words[0]=words[0].charAt(0).toUpperCase()+words[0].slice(1);
    return words.join(" ");
  }
  function displayIdentifier(identifier, metadata={}) {
    const fallback=String(metadata.label||metadata.display_name||metadata.title||MODE_LABELS[identifier]||localLabel(identifier)||"Engineering object");
    const descriptor=window.ImplexityText?.present?.(identifier,Object.assign({},metadata,{label:metadata.label||metadata.display_name||metadata.title||MODE_LABELS[identifier]}));
    return localLabel(descriptor?.label||fallback);
  }
  function modeLabel(mode) { return displayIdentifier(mode,{label:MODE_LABELS[mode]}); }
  function friendlyErrorMessage(error,fallback="The viewport action could not be completed.") {
    const problem=error&&Array.isArray(error.problems)&&error.problems.length?error.problems[0]:error?.userMessage||error?.message||error;
    const clean=String(problem==null?"":problem).replace(/^\s*Error:\s*/i,"").trim();
    if(!clean)return fallback;
    if(/\r|\n|https?:\/\/|\/v\d+(?:\/|\b)|\/(?:Users|home|private|tmp|var)\/|[A-Za-z]:\\|Traceback|\bat\s+\S+\s*\(|\b[A-Za-z][A-Za-z0-9]*_[A-Za-z0-9_]+\b|[{}\[\]<>]/i.test(clean))return fallback;
    return clean.length<=220?clean:fallback;
  }
  function objectDisplayName(object={}) {
    const preferred=object.label||object.display_name||object.title;
    if(preferred)return displayIdentifier(object.type||object.kind||preferred,{label:preferred});
    const generated=String(object.name||"").trim();
    if(generated&&generated.toLowerCase()!==String(object.type||object.kind||"").replaceAll("_"," ").toLowerCase())return localLabel(generated);
    return displayIdentifier(object.type||object.kind||generated||"engineering_object");
  }
  function optionProfile(mode) {
    const source=OPTION_PROFILES[mode];
    if(!source)return null;
    return Object.fromEntries(Object.entries(source).map(([key,value])=>[key,Array.isArray(value)?value.slice():value]));
  }
  function uid(prefix) { return `${prefix}_${crypto.getRandomValues(new Uint32Array(2)).join("")}`; }
  function snap(v, step) { return (!state.snap || !step) ? Number(v) : Math.round(Number(v)/step)*step; }
  function finite3(v) { return Array.isArray(v) && v.length===3 && v.every(Number.isFinite); }

  async function jsonFetch(url, options={}) {
    const response = await fetch(url, {headers:{"Content-Type":"application/json", ...(options.headers||{})}, ...options});
    const body = await response.json().catch(()=>({}));
    if(!response.ok) throw new Error(body.error || body.message || `${response.status} ${response.statusText}`);
    return body;
  }
  async function loadProblem() {
    const body = await jsonFetch("/v1/implicit/problem");
    state.problem = body.problem || body;
    state.problemRevision = body.revision ?? state.problem.revision ?? null;
    return state.problem;
  }
  async function saveProblem(problem) {
    const headers = {};
    if(state.problemRevision !== null) headers["If-Match"] = String(state.problemRevision);
    const body = await jsonFetch("/v1/implicit/problem", {method:"PUT", headers, body:JSON.stringify(problem)});
    state.problem = body.problem || body;
    state.problemRevision = body.revision ?? state.problem.revision ?? null;
    window.dispatchEvent(new CustomEvent("implexity-engineering-problem-changed", {detail:body}));
    return body;
  }

  function collection(problem, key) {
    if(!problem[key]) problem[key]=[];
    if(Array.isArray(problem[key])) return problem[key];
    return problem[key];
  }
  function upsert(coll, obj) {
    if(Array.isArray(coll)) { const i=coll.findIndex(x=>x && x.id===obj.id); if(i>=0)coll[i]=obj; else coll.push(obj); }
    else coll[obj.id]=obj;
  }
  function patchFromHit(hit) {
    return {id:uid("surface_patch"), name:"Surface patch", type:"surface_patch", kind:"boundary",
      method:"implicit_surface_patch", model_id:hit.modelId||null, node_id:hit.nodeId||null,
      point:[...hit.point], normal:[...hit.normal], radius:snap(state.patchRadius,state.linearSnap),
      level:0, side:"visible", follows_geometry:true};
  }
  function objectFor(mode, patch) {
    const common={id:uid(mode), name:mode.replaceAll("_"," "), type:mode==="temperature"?"prescribed_temperature":mode,
      region:patch.id, enabled:true, source:"viewport"};
    if(mode==="pressure") return {...common, state_load:true, magnitude:snap(state.magnitude,state.magnitudeSnap), units:"Pa"};
    if(mode==="traction") return {...common, state_load:true, magnitude:snap(state.magnitude,state.magnitudeSnap), vector:[...state.vector], units:"Pa"};
    if(mode==="heat_flux") return {...common, magnitude:snap(state.magnitude,state.magnitudeSnap), units:"W/m^2"};
    if(mode==="temperature") return {...common, value:snap(state.magnitude,state.magnitudeSnap), units:"K"};
    if(mode==="clamp") return {...common, components:["x","y","z"]};
    return null;
  }

  async function author(hit) {
    if(!state.problem) await loadProblem();
    const next=structuredClone(state.problem);
    const patch=patchFromHit(hit);
    upsert(collection(next,"regions"),patch);
    if(state.mode!=="region") {
      const obj=objectFor(state.mode,patch);
      if(!obj) return;
      upsert(collection(next,["temperature","clamp"].includes(state.mode)?"boundary_conditions":"loads"),obj);
      state.activeObject=obj;
    } else state.activeObject=patch;
    await saveProblem(next);
    renderGlyphs();
  }

  function normalizeHit(raw) {
    const hit=raw?.detail || raw;
    if(!finite3(hit?.point)||!finite3(hit?.normal)) return null;
    return {point:hit.point.map(Number), normal:hit.normal.map(Number), modelId:hit.modelId||hit.model_id, nodeId:hit.nodeId||hit.node_id};
  }
  function onSurfaceHover(event) {
    const hit=normalizeHit(event); state.hit=hit;
    document.body.classList.toggle("implexity-authoring-hit",!!hit && !["select","move","size","surface"].includes(state.mode));
    const label=modeLabel(state.mode);
    updateHud(hit?`${label}: Surface ready`:`${label}: Point at a surface`);
  }
  async function onSurfacePick(event) {
    const hit=normalizeHit(event); if(!hit)return;
    state.hit=hit;
    if(["region","pressure","traction","heat_flux","temperature","clamp"].includes(state.mode)) {
      try { await author(hit); updateHud(`${modeLabel(state.mode)} placed`); }
      catch(err) { console.warn("Implexity viewport authoring failed",err);updateHud(friendlyErrorMessage(err,"The engineering object could not be created. Your model has not been changed."),true); }
    }
  }

  function setMode(mode) {
    if(!MODES.includes(mode)) return;
    state.mode=mode;
    document.querySelectorAll("[data-implexity-authoring-mode]").forEach(b=>b.classList.toggle("active",b.dataset.implexityAuthoringMode===mode));
    document.body.dataset.implexityAuthoringMode=mode;
    updateSettings(mode);
    updateHud(`${modeLabel(mode)} mode`);
    window.dispatchEvent(new CustomEvent("implexity-authoring-mode",{detail:{mode}}));
  }
  function updateHud(text,error=false) { const h=q("#implexity-authoring-hud"); if(h){h.textContent=text;h.classList.toggle("error",error);} }

  function numeric(label,value,step,onChange) {
    const wrap=el("label","implexity-authoring-field"); const caption=el("span","",label);wrap.append(caption);
    const input=el("input"); input.type="number"; input.value=String(value); input.step=String(step); input.addEventListener("change",()=>onChange(Number(input.value)));
    wrap.append(input); return {wrap,caption,input};
  }
  let settingsControls=null;
  function updateSettings(mode) {
    if(!settingsControls)return;
    const profile=optionProfile(mode);
    settingsControls.container.hidden=!profile;
    if(!profile)return;
    settingsControls.title.textContent=profile.title;
    for(const key of ["radius","magnitude","snap"]){
      const nodes=settingsControls[key],spec=profile[key];
      nodes.wrap.hidden=!spec;
      if(!spec)continue;
      const visible=spec[1]?`${spec[0]} [${spec[1]}]`:spec[0];
      nodes.caption.textContent=visible;
      nodes.input.setAttribute("aria-label",visible);
    }
  }
  function buildToolbar() {
    if(q("#implexity-authoring-toolbar") || delegatedNative()) return;
    const bar=el("div","implexity-authoring-toolbar"); bar.id="implexity-authoring-toolbar";
    const groups=[
      [["select","Select"],["move","Move"],["size","Size"],["surface","Surface"]],
      [["region","Region"],["pressure","Pressure"],["traction","Traction"],["heat_flux","Heat flux"],["temperature","Temperature"],["clamp","Clamp"]],
      [["brush","Field brush"]]
    ];
    groups.forEach(items=>{const g=el("div","implexity-authoring-group");items.forEach(([m,t])=>{const b=el("button","",t);b.type="button";b.dataset.implexityAuthoringMode=m;b.addEventListener("click",()=>setMode(m));g.append(b);});bar.append(g);});
    const settings=el("div","implexity-authoring-settings");
    const settingsTitle=el("div","implexity-authoring-settings-title","Interaction settings");settings.append(settingsTitle);
    const radius=numeric("Patch radius [mm]",state.patchRadius,0.1,v=>state.patchRadius=Math.max(1e-6,v));
    const magnitude=numeric("Magnitude",state.magnitude,1,v=>state.magnitude=v);
    const snapLabel=el("label","implexity-authoring-check");const cb=el("input");const snapCaption=el("span","","Snap values to engineering increments");cb.type="checkbox";cb.checked=state.snap;cb.addEventListener("change",()=>state.snap=cb.checked);snapLabel.append(cb,snapCaption);
    settings.append(radius.wrap,magnitude.wrap,snapLabel);
    settingsControls={container:settings,title:settingsTitle,radius,magnitude,snap:{wrap:snapLabel,caption:snapCaption,input:cb}};
    bar.append(settings);
    const hud=el("div","implexity-authoring-hud","Select mode");hud.id="implexity-authoring-hud";bar.append(hud);
    const host=q("#stage")||q("#viewport")||q(".viewport")||q("main")||document.body;
    host.prepend(bar);
    setMode("select");
  }

  function renderGlyphs() {
    window.dispatchEvent(new CustomEvent("implexity-render-engineering-glyphs",{detail:{problem:state.problem,active:state.activeObject}}));
  }
  function installPickerBridge() {
    const delegated = delegatedNative();
    if(!delegated) {
      window.addEventListener("implexity-surface-hover",onSurfaceHover);
      window.addEventListener("implexity-surface-pick",onSurfacePick);
    } else {
      window.addEventListener("implexity:interaction-mode", event => {
        state.mode = event.detail?.mode || "select";
      });
    }


    window.ImplexityViewportAuthoring = {state,setMode,onSurfaceHover,onSurfacePick,loadProblem,saveProblem,renderGlyphs,delegated};
  }
  function keyboard(event) {
    if(event.target && /INPUT|TEXTAREA|SELECT/.test(event.target.tagName)) return;
    const map={q:"select",w:"move",e:"size",r:"surface",g:"region",p:"pressure",t:"traction",h:"heat_flux",k:"temperature",c:"clamp",b:"brush"};
    if(map[event.key.toLowerCase()]) {event.preventDefault();setMode(map[event.key.toLowerCase()]);}
    if(event.key==="Escape") window.dispatchEvent(new CustomEvent("implexity-cancel-active-interaction"));
  }
  window.ImplexityViewportAuthoringPresentation = Object.freeze({displayIdentifier,modeLabel,friendlyErrorMessage,objectDisplayName,optionProfile});
   
  async function modelLoaded() {
    const response = await fetch("/v1/implicit/model", {cache:"no-store"});
    if(!response.ok) return false;
    return (await response.json().catch(()=>({}))).loaded === true;
  }
  function loadProblemWhenModelExists() {
    modelLoaded().then(loaded => loaded ? loadProblem().then(renderGlyphs) : null).catch(()=>{});
  }
  function init() {
    buildToolbar(); installPickerBridge(); if(!delegatedNative()) document.addEventListener("keydown",keyboard);
    loadProblemWhenModelExists();
    window.addEventListener("implexity:design-state-changed", event => {
      if(event.detail?.source === "model" && !state.problem) loadProblemWhenModelExists();
    });
  }
  if(document.readyState==="loading") document.addEventListener("DOMContentLoaded",init,{once:true}); else init();
})();


(() => {
  "use strict";
  if (window.__implexitySpatialPickerBridgeInstalled) return;
  window.__implexitySpatialPickerBridgeInstalled = true;
  const seen = new WeakSet();
  function viewerCandidates() { return [window.ImplexityDirectInteraction, window.ImplexityViewer, window.implexityViewer, window.viewer, window.app?.viewer].filter(Boolean); }
  async function pick(event) {
    for (const api of viewerCandidates()) {
      for (const name of ["pickSurface", "pick", "raycast", "surfaceHitAt"]) {
        if (typeof api?.[name] !== "function") continue;
        try {
          const rect = event.currentTarget.getBoundingClientRect();
          const x = event.clientX - rect.left, y = event.clientY - rect.top;
          const hit = await api[name](x, y, event);
          if (hit?.point && hit?.normal) return hit;
          if (hit?.position && hit?.normal) return {...hit, point: hit.position};
        } catch (_) {                                   }
      }
    }
    return null;
  }
  function installCanvas(canvas) {
    if (!canvas || seen.has(canvas)) return; seen.add(canvas);
    let queued = null, busy = false;
    async function hover(event) {
      queued = event;
      if (busy) return;
      busy = true;
      while (queued) {
        const current = queued; queued = null;
        const hit = await pick(current);
        window.dispatchEvent(new CustomEvent("implexity-surface-hover", {detail: hit || {}}));
      }
      busy = false;
    }
    canvas.addEventListener("pointermove", hover, {passive:true});
    canvas.addEventListener("pointerdown", async event => {
      const mode = document.body.dataset.implexityAuthoringMode;
      if (!["region","pressure","traction","heat_flux","temperature","clamp","brush"].includes(mode)) return;
      const hit = await pick(event);
      if (hit) window.dispatchEvent(new CustomEvent("implexity-surface-pick", {detail: hit}));
    });
  }
  function scan() { document.querySelectorAll("canvas").forEach(installCanvas); }
  new MutationObserver(scan).observe(document.documentElement,{childList:true,subtree:true}); scan();

  function projector(point) {
    for (const api of viewerCandidates()) {
      for (const name of ["worldToScreen", "projectWorldToScreen", "project"]) {
        try { if (typeof api?.[name] === "function") { const p=api[name](point); if(p && Number.isFinite(p.x) && Number.isFinite(p.y)) return p; } } catch(_){}
      }
    }
    return null;
  }
  function overlay() {
    let svg=document.getElementById("implexity-engineering-overlay");
    if(svg)return svg;
    svg=document.createElementNS("http://www.w3.org/2000/svg","svg");svg.id="implexity-engineering-overlay";svg.setAttribute("aria-hidden","true");
    Object.assign(svg.style,{position:"absolute",inset:"0",width:"100%",height:"100%",pointerEvents:"none",zIndex:"45"});
    (document.querySelector("#stage")||document.querySelector("#viewport")||document.querySelector(".viewport")||document.body).append(svg);return svg;
  }
  window.addEventListener("implexity-render-engineering-glyphs", event => {
    const svg=overlay();svg.replaceChildren();const problem=event.detail?.problem||{};
    const regions=Array.isArray(problem.regions)?problem.regions:Object.values(problem.regions||{});
    const byId=Object.fromEntries(regions.map(r=>[r.id,r]));
    const objects=[...(Array.isArray(problem.loads)?problem.loads:Object.values(problem.loads||{})),...(Array.isArray(problem.boundary_conditions)?problem.boundary_conditions:Object.values(problem.boundary_conditions||{}))];
    for(const obj of objects){const region=byId[obj.region];if(!region?.point)continue;const p=projector(region.point);if(!p)continue;
      const g=document.createElementNS(svg.namespaceURI,"g");const c=document.createElementNS(svg.namespaceURI,"circle");c.setAttribute("cx",p.x);c.setAttribute("cy",p.y);c.setAttribute("r","7");c.setAttribute("fill","none");c.setAttribute("stroke","currentColor");c.setAttribute("stroke-width","2");g.append(c);
      const t=document.createElementNS(svg.namespaceURI,"text");t.setAttribute("x",p.x+10);t.setAttribute("y",p.y-8);t.setAttribute("fill","currentColor");t.setAttribute("font-size","12");
      const presenter=window.ImplexityViewportAuthoringPresentation;t.textContent=presenter?.objectDisplayName?.(obj)||String(obj.name||obj.type||"Engineering object");g.append(t);svg.append(g);}
  });
})();
