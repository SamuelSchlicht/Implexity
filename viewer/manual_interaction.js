// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

const EPS = 1.0e-12;

function sharedPointerCoordinator() {
  if (globalThis.ImplexityPointerCoordinator) return globalThis.ImplexityPointerCoordinator;
  let owner=null, pointerId=null;
  const coordinator=Object.freeze({
    claim(candidate,id){if(owner!==null&&owner!==candidate)return false;owner=candidate;pointerId=id;return true;},
    release(candidate,id=pointerId){if(owner!==candidate||(pointerId!==null&&id!==pointerId))return false;owner=null;pointerId=null;return true;},
    owns(candidate,id=pointerId){return owner===candidate&&(pointerId===null||pointerId===id);},
    current(){return{owner,pointerId};}
  });
  globalThis.ImplexityPointerCoordinator=globalThis.ImplexityPointerCoordinator||coordinator;
  return coordinator;
}

function localLabel(identifier) {
  const raw = String(identifier == null ? "" : identifier).trim();
  if (!raw) return "";
  if (!/^[A-Za-z][A-Za-z0-9_.:-]*$/.test(raw)) return raw;
  const words = raw.split(".").pop().replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_:-]+/g, " ").trim().split(/\s+/).filter(Boolean);
  if (!words.length) return raw;
  words[0] = words[0].charAt(0).toUpperCase() + words[0].slice(1);
  return words.join(" ");
}


export function displayIdentifier(identifier, metadata={}) {
  const fallback = String(metadata.label || metadata.display_name || metadata.title ||
    localLabel(identifier) || "Engineering parameter");
  const descriptor = globalThis.ImplexityText?.present?.(identifier, metadata);
  return localLabel(descriptor?.label || fallback);
}


export function friendlyErrorMessage(error, fallback="The direct interaction could not be completed.") {
  const problem = error && Array.isArray(error.problems) && error.problems.length
    ? error.problems[0] : error?.userMessage || error?.message || error;
  const clean = String(problem == null ? "" : problem).replace(/^\s*Error:\s*/i, "").trim();
  if (!clean) return fallback;
  if (/\r|\n|https?:\/\/|\/v\d+(?:\/|\b)|\/(?:Users|home|private|tmp|var)\/|[A-Za-z]:\\|Traceback|\bat\s+\S+\s*\(|\b[A-Za-z][A-Za-z0-9]*_[A-Za-z0-9_]+\b|[{}\[\]<>]/i.test(clean)) return fallback;
  return clean.length <= 220 ? clean : fallback;
}


export function isEditableTarget(target) {
  if (!target) return false;
  if (target.isContentEditable) return true;
  const tag = String(target.tagName || "").toLowerCase();
  if (["input", "textarea", "select"].includes(tag)) return true;
  if (typeof target.closest !== "function") return false;
  return Boolean(target.closest('input,textarea,select,[contenteditable="true"],[contenteditable=""],[role="textbox"]'));
}


export function requirePreviewAcknowledgement(reply, sequence) {
  if (!reply || reply.accepted !== true || Number(reply.sequence) !== Number(sequence)) {
    throw new Error("the exact final direct-manipulation preview was not acknowledged");
  }
  return Number(sequence);
}

export function add(a, b) { return [a[0]+b[0], a[1]+b[1], a[2]+b[2]]; }
export function sub(a, b) { return [a[0]-b[0], a[1]-b[1], a[2]-b[2]]; }
export function scale(a, s) { return [a[0]*s, a[1]*s, a[2]*s]; }
export function dot(a, b) { return a[0]*b[0] + a[1]*b[1] + a[2]*b[2]; }
export function cross(a, b) {
  return [a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0]];
}
export function length(a) { return Math.hypot(a[0], a[1], a[2]); }
export function normalise(a, fallback=[0,0,1]) {
  const n = length(a);
  return n > EPS && Number.isFinite(n) ? scale(a, 1/n) : fallback.slice();
}
export function clamp(x, lo, hi) { return Math.max(lo, Math.min(hi, x)); }

export function cameraFrame(camera, aspect=1) {
  const target = (camera && camera.target ? camera.target : [0,0,0]).slice(0,3);
  let eye = camera && camera.eye ? camera.eye.slice(0,3) : null;
  if (!eye) {
    const yaw = +(camera && camera.yaw != null ? camera.yaw : -0.62);
    const pitch = +(camera && camera.pitch != null ? camera.pitch : 0.42);
    const dist = +(camera && camera.dist != null ? camera.dist : 20);
    const cp = Math.cos(pitch);
    eye = [target[0] + dist*cp*Math.cos(yaw),
           target[1] + dist*cp*Math.sin(yaw),
           target[2] + dist*Math.sin(pitch)];
  }
  const forward = normalise(sub(target, eye), [0,0,-1]);
  let right = normalise(cross(forward, [0,0,1]), [1,0,0]);
  if (length(right) < 1e-8) right = normalise(cross(forward, [0,1,0]), [1,0,0]);
  const up = normalise(cross(right, forward), [0,1,0]);
  const fov = +(camera && camera.fov != null ? camera.fov : 48.13);
  const projection=String(camera?.projection||camera?.type||(camera?.orthographic?"orthographic":"perspective")).toLowerCase();
  const orthoHeight=+(camera?.ortho_height_mm??camera?.orthoHeight??camera?.height_mm??camera?.height??camera?.scale??0);
  return {eye, target, forward, right, up, fov, aspect,
    projection:projection.includes("ortho")?"orthographic":"perspective",orthoHeight};
}

export function rayFromScreen(x, y, width, height, camera) {
  const w = Math.max(1, width), h = Math.max(1, height);
  const frame = cameraFrame(camera, w/h);
  const nx = (2*x/w - 1);
  const ny = (1 - 2*y/h);
  if(frame.projection==="orthographic"){
    if(!(frame.orthoHeight>0))throw new Error("orthographic camera requires a positive model-space height");
    const origin=add(frame.eye,add(scale(frame.right,nx*frame.orthoHeight*frame.aspect*0.5),scale(frame.up,ny*frame.orthoHeight*0.5)));
    return {origin,direction:frame.forward.slice(),frame};
  }
  const tanH = Math.tan(frame.fov*Math.PI/360);
  const direction = normalise(add(frame.forward,
    add(scale(frame.right, nx*tanH*frame.aspect), scale(frame.up, ny*tanH))));
  return {origin: frame.eye.slice(), direction, frame};
}

export function intersectRayAabb(origin, direction, lo, hi) {
  let t0 = 0, t1 = Infinity;
  for (let axis=0; axis<3; axis++) {
    const o = origin[axis], d = direction[axis];
    if (Math.abs(d) < EPS) {
      if (o < lo[axis] || o > hi[axis]) return null;
      continue;
    }
    let a = (lo[axis]-o)/d, b = (hi[axis]-o)/d;
    if (a > b) { const q=a; a=b; b=q; }
    t0 = Math.max(t0, a); t1 = Math.min(t1, b);
    if (t1 < t0) return null;
  }
  if (t1 < 0) return null;
  return [Math.max(0,t0), t1];
}

function fieldNormal(sample, point, h) {
  const e = Math.max(h, 1e-6);
  const gx = sample(point[0]+e,point[1],point[2]) - sample(point[0]-e,point[1],point[2]);
  const gy = sample(point[0],point[1]+e,point[2]) - sample(point[0],point[1]-e,point[2]);
  const gz = sample(point[0],point[1],point[2]+e) - sample(point[0],point[1],point[2]-e);
  return normalise([gx,gy,gz]);
}


export function traceSampledSurface(ray, grid, sample, safeFactor=null, maxSteps=null) {
  if (!grid || !grid.lo || !grid.hi || !grid.n) return null;
  const hitBox = intersectRayAabb(ray.origin, ray.direction, grid.lo, grid.hi);
  if (!hitBox) return null;
  const spacing = Math.min(
    (grid.hi[0]-grid.lo[0])/Math.max(1,grid.n[0]-1),
    (grid.hi[1]-grid.lo[1])/Math.max(1,grid.n[1]-1),
    (grid.hi[2]-grid.lo[2])/Math.max(1,grid.n[2]-1));
  const eps = Math.max(spacing*0.27, 1e-5);
  const minStep = Math.max(spacing*(safeFactor == null ? 0.35 : 0.16), 1e-5);
  const maxStep = Math.max(spacing*1.35, minStep);
  let t = hitBox[0] + 1e-6;
   
   
  const steps=maxSteps===null?Math.ceil(Math.max(0,hitBox[1]-t)/minStep)+2:Math.max(0,Math.floor(maxSteps));
  let prevT = t;
  let prevF = sample(...add(ray.origin, scale(ray.direction, t)));
  let best = {t, f:Math.abs(prevF)};
  for (let i=0; i<steps && t<=hitBox[1]; i++) {
    const p = add(ray.origin, scale(ray.direction, t));
    const f = sample(p[0],p[1],p[2]);
    if (Math.abs(f) < best.f) best = {t, f:Math.abs(f)};
    const crossed=i>0&&((f<0)!==(prevF<0));
    if ((safeFactor===null ? f===0 : Math.abs(f)<=eps) || crossed) {
      let a = prevT, b = t, fa = prevF;
      if (a === b) a = Math.max(hitBox[0], t-minStep);
      for (let k=0; k<18; k++) {
        const m = 0.5*(a+b);
        const q = add(ray.origin, scale(ray.direction,m));
        const fm = sample(q[0],q[1],q[2]);
        if (safeFactor===null ? fm===0 || Math.abs(b-a)<spacing*1e-7 : Math.abs(fm)<eps*0.04) { a=b=m; break; }
        if ((fm<0)===(fa<0)) { a=m; fa=fm; } else b=m;
      }
      const th = 0.5*(a+b);
      const point = add(ray.origin, scale(ray.direction, th));
      let normal = fieldNormal(sample, point, spacing*0.60);
      if (dot(normal,ray.direction)>0) normal=scale(normal,-1);
      return {point, normal, t:th, spacing, value:f, approximate:true, exact:false, source:"sampled-interpolant"};
    }
    prevT=t; prevF=f;
    const step = safeFactor == null ? minStep
      : clamp(Math.abs(f)*safeFactor, minStep, maxStep);
    t += step;
  }


  if (safeFactor!==null && best.f <= eps*1.6) {
    const point=add(ray.origin,scale(ray.direction,best.t));
    let normal=fieldNormal(sample,point,spacing*0.60);
    if(dot(normal,ray.direction)>0) normal=scale(normal,-1);
    return {point,normal,t:best.t,spacing,value:best.f,approximate:true};
  }
  return null;
}

export function projectPoint(point, width, height, camera) {
  const frame = cameraFrame(camera, Math.max(1,width)/Math.max(1,height));
  const v = sub(point,frame.eye);
  const z = dot(v,frame.forward);
  if (z <= EPS) return null;
  if(frame.projection==="orthographic"){
    if(!(frame.orthoHeight>0))return null;
    const x=dot(v,frame.right)/(0.5*frame.orthoHeight*frame.aspect);
    const y=dot(v,frame.up)/(0.5*frame.orthoHeight);
    return{x:(x+1)*0.5*width,y:(1-y)*0.5*height,depth:z,frame};
  }
  const tanH=Math.tan(frame.fov*Math.PI/360);
  const x=dot(v,frame.right)/(z*tanH*frame.aspect);
  const y=dot(v,frame.up)/(z*tanH);
  return {x:(x+1)*0.5*width,y:(1-y)*0.5*height,depth:z,frame};
}

export function unprojectOnAnchorPlane(x,y,width,height,camera,anchor) {
  if(!Array.isArray(anchor)||anchor.length!==3||!anchor.every(Number.isFinite))throw new Error("Anchor must be a finite model-coordinate triple");
  const ray=rayFromScreen(x,y,width,height,camera);
  const normal=cameraFrame(camera,width/height).forward;
  const denominator=dot(ray.direction,normal);
  if(Math.abs(denominator)<1e-12)return null;
  const t=dot(sub(anchor,ray.origin),normal)/denominator;
  if(!Number.isFinite(t))return null;
  return add(ray.origin,scale(ray.direction,t));
}

function safeStepFactor(fieldClass) {
  if(!fieldClass) return null;
  if(fieldClass.kind==="EXACT" || fieldClass.kind==="BOUND") return 1;
  if(fieldClass.kind==="LIPSCHITZ") return 1/(fieldClass.k||1);
  return null;
}

function fmt(value, digits=3) {
  if(!Number.isFinite(+value)) return String(value);
  return (+value).toFixed(digits).replace(/\.0+$/," ").replace(/(\.\d*?)0+$/,"$1").trim();
}


export function mountDirectInteraction(options) {
  const {
    overlay, stage, VIEW, S, api, post, adoptModel, buildTree, renderParams,
    renderNodeInfo, renderSpecOut, renderDesignVars, recordTrace, renderProbe, uniformFor,
  } = options;
  if(!overlay || !stage || !VIEW || !S || !api || !post) throw new Error("direct interaction dependencies are incomplete");

  const ctx=overlay.getContext("2d");
  const ui={
    edit:document.getElementById("directedit"), orbit:document.getElementById("directorbit"),
    undo:document.getElementById("directundo"), redo:document.getElementById("directredo"),
    status:document.getElementById("directstatus"),
  };
  const state={mode:"edit",tool:"surface",hover:null,active:null,pendingPointer:null,cameraDrag:null,sequence:0,inflight:false,
               previewTask:null,latest:null,hoverRAF:0,lastEvent:null,undo:0,redo:0,disposed:false,enabled:true};
  const pointerOwner={kind:"direct-geometry"};
  const pointerCoordinator=sharedPointerCoordinator();

  function resize(){
    const r=stage.getBoundingClientRect(), dpr=Math.min(window.devicePixelRatio||1,2);
    const w=Math.max(64,Math.round(r.width*dpr)),h=Math.max(64,Math.round(r.height*dpr));
    if(overlay.width!==w||overlay.height!==h){overlay.width=w;overlay.height=h;draw();}
  }
  const ro=typeof ResizeObserver!=="undefined"?new ResizeObserver(resize):null;
  if(ro)ro.observe(stage); window.addEventListener("resize",resize); resize();

  function camera(){
    if(VIEW.gl&&VIEW.gl.camera&&VIEW.gl.camera.get)return VIEW.gl.camera.get();
    return {target:VIEW.cam.target.slice(),dist:VIEW.cam.dist,yaw:VIEW.cam.yaw,
            pitch:VIEW.cam.pitch,fov:48.13,eye:VIEW._eye()};
  }
  function local(e){const r=overlay.getBoundingClientRect();return{x:e.clientX-r.left,y:e.clientY-r.top,w:r.width,h:r.height};}
  function clipEvidence(){
    const raw=typeof VIEW.clipPlane==="function"?VIEW.clipPlane():null;
    if(!raw)return{active:false,plane:null,hit_on_clip_cap:false};
    return{active:true,plane:{axis:raw.axis,d:+raw.d,n:(raw.n||[]).slice?.()||raw.n},hit_on_clip_cap:false};
  }
  function cameraEvidence(){
    const rect=overlay.getBoundingClientRect(),raw=camera(),frame=cameraFrame(raw,Math.max(1,rect.width)/Math.max(1,rect.height));
    const evidence={eye_mm:frame.eye.slice(),target_mm:frame.target.slice(),up:frame.up.slice(),viewport_px:[rect.width,rect.height],projection:frame.projection};
    if(frame.projection==="orthographic"){
      if(!(frame.orthoHeight>0))throw new Error("orthographic camera height is unavailable");
      evidence.ortho_height_mm=frame.orthoHeight;
    }else evidence.fov_deg=frame.fov;
    return evidence;
  }
  function pickLocal(x,y,w,h){
    if(!VIEW.grid)return null;
    const ray=rayFromScreen(x,y,w,h,camera());
    const clip=typeof VIEW.clipPlane==="function"?VIEW.clipPlane():null;
    const rawSample=(a,b,c)=>VIEW.sample(a,b,c);
    const sample=clip?(a,b,c)=>Math.max(rawSample(a,b,c),(clip.n||[0,0,0])[0]*a+(clip.n||[0,0,0])[1]*b+(clip.n||[0,0,0])[2]*c-(+clip.d)):rawSample;
    const hit=traceSampledSurface(ray,VIEW.grid,sample,safeStepFactor(VIEW.fc));
    if(hit){
      hit.screen=[x,y];hit.ray=ray;
      const rawValue=rawSample(...hit.point);
      const clipValue=clip?dot(clip.n||[0,0,0],hit.point)-(+clip.d):-Infinity;
      hit.clip_evidence=clipEvidence();
      hit.clip_evidence.hit_on_clip_cap=Boolean(clip&&clipValue>=rawValue&&Math.abs(clipValue)<Math.max(1e-5,(hit.spacing||1)*0.3));
      hit.model_identity={structure_id:S.model?.structure_id||S.structure_id||null,content_id:S.model?.content_id||S.content_id||null,revision:S.model?.revision||S.revision||null};
    }
    return hit;
  }
  function pickAt(e){const p=local(e);return pickLocal(p.x,p.y,p.w,p.h);}
  function setStatus(text,kind=""){
    if(!ui.status)return;ui.status.textContent=text||"";ui.status.title=text||"";ui.status.dataset.kind=kind;
  }
  function setCounts(){
    const shared=globalThis.ImplexityManualHistory?.counts?.();
    const undoCount=shared?shared.undo:state.undo,redoCount=shared?shared.redo:state.redo;
    if(ui.undo)ui.undo.disabled=!!shared?.busy||undoCount<=0||!!state.active;
    if(ui.redo)ui.redo.disabled=!!shared?.busy||redoCount<=0||!!state.active;
    if(ui.undo)ui.undo.title=`Undo manual edit (${undoCount})`;
    if(ui.redo)ui.redo.title=`Redo manual edit (${redoCount})`;
  }
  function setMode(mode){
    if(state.active)return;
    state.mode=mode==="orbit"?"orbit":"edit";
    stage.classList.toggle("direct-edit",state.mode==="edit");
    overlay.style.pointerEvents=state.enabled&&state.mode==="edit"?"auto":"none";
    if(ui.edit)ui.edit.setAttribute("aria-pressed",state.mode==="edit"?"true":"false");
    if(ui.orbit)ui.orbit.setAttribute("aria-pressed",state.mode==="orbit"?"true":"false");
    state.hover=null;draw();
    setStatus(state.mode==="edit"?"Click and drag the implicit surface":"Orbit camera");
  }
  function setEnabled(enabled){
    state.enabled=Boolean(enabled);
    if(!state.enabled&&state.pendingPointer){
      const pending=state.pendingPointer;pending.cancelled=true;state.pendingPointer=null;
      pointerCoordinator.release(pointerOwner,pending.pointerId);
    }
    if(!state.enabled&&state.active)finish(false);
    overlay.style.pointerEvents=state.enabled&&state.mode==="edit"?"auto":"none";
    stage.classList.toggle("direct-edit",state.enabled&&state.mode==="edit");
    if(!state.enabled){state.hover=null;draw();}
  }

  function drawMarker(hit,active=false){
    if(!hit)return;
    const dpr=overlay.width/Math.max(1,overlay.getBoundingClientRect().width);
    const p=projectPoint(hit.point,overlay.width/dpr,overlay.height/dpr,camera());
    if(!p)return;
    const q=projectPoint(add(hit.point,scale(hit.normal,Math.max(hit.spacing||0.3,0.5))),overlay.width/dpr,overlay.height/dpr,camera());
    const x=p.x*dpr,y=p.y*dpr;
    ctx.save();ctx.lineWidth=2*dpr;ctx.strokeStyle=active?"#e34234":"#2a78d6";
    ctx.fillStyle="#fff";
    if(q){ctx.beginPath();ctx.moveTo(x,y);ctx.lineTo(q.x*dpr,q.y*dpr);ctx.stroke();}
    ctx.beginPath();ctx.arc(x,y,(active?6:5)*dpr,0,Math.PI*2);ctx.fill();ctx.stroke();
    if(active&&state.active&&state.active.label){
      const text=state.active.label;ctx.font=`${11*dpr}px Arial`;
      const tw=ctx.measureText(text).width+12*dpr;
      ctx.fillStyle="rgba(255,255,255,.94)";ctx.strokeStyle="#d3dde9";
      ctx.fillRect(x+10*dpr,y-27*dpr,tw,21*dpr);ctx.strokeRect(x+10*dpr,y-27*dpr,tw,21*dpr);
      ctx.fillStyle="#10151c";ctx.fillText(text,x+16*dpr,y-12*dpr);
    }
    ctx.restore();
  }
  function draw(){ctx.clearRect(0,0,overlay.width,overlay.height);drawMarker(state.active?state.active.hit:state.hover,!!state.active);}

  function hoverEvent(e){
    state.lastEvent=e;
    if(state.hoverRAF||state.active||state.mode!=="edit")return;
    state.hoverRAF=requestAnimationFrame(()=>{
      state.hoverRAF=0;const ev=state.lastEvent;if(!ev)return;
      state.hover=pickAt(ev);overlay.style.cursor=state.hover?"grab":"crosshair";
      draw();
    });
  }

  function chooseSemantic(parameters){
    if(!Array.isArray(parameters)||!parameters.length)return null;
    const rows=parameters.slice().sort((a,b)=>(b.influence||0)-(a.influence||0));
    const p=rows[0];
    if((p.influence||0)<0.52)return null;
    if(["radius","normal-offset","translate","axis-coordinate","endpoint-a","endpoint-b","length","period"].includes(p.semantic))return p.key;
    return null;
  }

  function pushPreviewUniforms(moved){
    if(!(VIEW.gl && VIEW.glSource==="service" && VIEW.gl.setUniforms && typeof uniformFor==="function")) return false;
    if(!Array.isArray(moved) || !moved.length) return false;
    const vals={};
    for(const mv of moved){
      const u=uniformFor(mv.node,mv.param);
      if(!u || !Number.isFinite(+mv.now)) return false;
      vals[u]=+mv.now;
    }
    if(!Object.keys(vals).length) return false;
    VIEW.gl.setUniforms(vals);
    if(VIEW.gl.redraw) VIEW.gl.redraw();
    return true;
  }

  function applyPreviewValues(values){
    if(!values || typeof values!=="object") return;
    for(const [key,value] of Object.entries(values)){
      if(!Number.isFinite(+value) || key.startsWith("node:")) continue;
      const row=Array.isArray(S.params)?S.params.find(p=>p&&p.name===key):null;
      if(row) row.value=+value;
      const pv=document.getElementById(`pv_${key}`); if(pv) pv.textContent=fmt(+value);
      const pr=document.getElementById(`pr_${key}`); if(pr) pr.value=String(+value);
      const pn=document.getElementById(`pn_${key}`); if(pn) pn.value=String(+value);
    }
  }

  async function refreshModel({preview=false,record=false,values=null,moved=null}={}){
    if(preview){
      applyPreviewValues(values);
      if(pushPreviewUniforms(moved)){ draw();return null; }
      if(S.sel){
        const box=(state.active&&state.active.previewBox)||VIEW._box||null;
        await VIEW.load(S.sel,box,{noframe:true,quality:Math.min(24,VIEW.quality),draft:true});
      }
      draw();return null;
    }
    const st=await api("/v1/implicit/model");
    adoptModel(st);buildTree();renderParams();renderNodeInfo();renderSpecOut();renderDesignVars();
    if(record)recordTrace();
    if(S.sel){
      VIEW._box=null;
      await VIEW.load(S.sel,null,{noframe:true,quality:VIEW.quality,draft:false});
      renderProbe();
    }
    draw();return st;
  }

  function normalDelta(active,e){
    const p=local(e),start=active.startScreen;
    const cam=camera(),proj=projectPoint(active.hit.point,p.w,p.h,cam);
    const tip=projectPoint(add(active.hit.point,scale(active.hit.normal,Math.max(active.hit.spacing||.3,1))),p.w,p.h,cam);
    let sx=0,sy=-1;
    if(proj&&tip){sx=tip.x-proj.x;sy=tip.y-proj.y;const n=Math.hypot(sx,sy);if(n>3){sx/=n;sy/=n;}else{sx=0;sy=-1;}}
    const pixel=(p.x-start[0])*sx+(p.y-start[1])*sy;
    const depth=proj?proj.depth:(cam.dist||10);
    const worldPerPixel=2*depth*Math.tan((cam.fov||48.13)*Math.PI/360)/Math.max(1,p.h);
    let gain=e.shiftKey?0.20:(e.altKey?4.0:1.0);
    return pixel*worldPerPixel*gain;
  }

  let parameterSensitivity=null;
  window.addEventListener("implexity:parameter-sensitivity",e=>{parameterSensitivity=e.detail||null;});

  async function begin(e,hit,pending=null){
    if(globalThis.ImplexityManualHistory?.counts?.().busy){
      if(pending && state.pendingPointer === pending) state.pendingPointer = null;
      pointerCoordinator.release(pointerOwner,e.pointerId);
      return;
    }
    const selectedNode=S.byId.get(S.sel||S.model.root);
    const visited=new Set();
    const containsRegisteredGrid=node=>{
      if(!node||visited.has(node.id))return false;
      visited.add(node.id);
      return ["grid_field","cell_grid_field"].includes(node.kind)||
        (node.children||[]).some(child=>containsRegisteredGrid(S.byId.get(child.node)));
    };
    const sampledMove=state.tool==="move"&&containsRegisteredGrid(selectedNode);
    if(state.tool==="move" && (selectedNode?.kind!=="translate"||sampledMove)){
      if(pending&&state.pendingPointer===pending)state.pendingPointer=null;
      pointerCoordinator.release(pointerOwner,e.pointerId);
      overlay.style.cursor="crosshair";
      setStatus(sampledMove
        ? "Moving this sampled geometry requires its physics grid and attached regions to move together. That operation is not yet supported; nothing changed."
        : "Move requires a translation node. This geometry has no selected translation control; nothing changed.","error");
      return;
    }
    const active={pointerId:e.pointerId,hit,startScreen:[local(e).x,local(e).y],
      lastEvent:pending?.lastEvent||e,releaseEvent:pending?.releaseEvent||null,
      released:Boolean(pending?.released),cancelled:Boolean(pending?.cancelled),session:null,
      semantic:null,label:"preparing derivative…",lastAcceptedSequence:-1,previewFailure:null,
      previewBox:VIEW._box&&VIEW._grown?VIEW._grown(VIEW._box,0.12):VIEW._box||null};
    if(pending&&state.pendingPointer===pending)state.pendingPointer=null;
    state.active=active;state.hover=null;overlay.style.cursor="grabbing";draw();
    try{overlay.setPointerCapture(e.pointerId);}catch(_){ }
    setStatus("Preparing differentiable surface control…");
    try{
      const reply=await post("/v1/implicit/manipulation/begin",{
        node:S.sel||S.model.root,point_mm:hit.point,normal:hit.normal,mode:"smooth",
        smooth_r_mm:Math.max(0.15,Math.min(0.8,(hit.spacing||0.35)*1.2)),
      });
      if(state.active!==active)return;
      active.session=reply.session;active.parameters=reply.parameters||[];
      if(state.tool==="move" && (!active.parameters.length || active.parameters.some(p=>p.semantic!=="translate"))){
        await post("/v1/implicit/manipulation/cancel",{session:active.session});
        throw new Error("Move requires translation-only controls; the selected controls would deform the geometry. Nothing changed.");
      }
      active.semantic=chooseSemantic(active.parameters);
      if(parameterSensitivity?.parameter_gradients){
        try{
          const guide=await post("/v1/implicit/manipulation/guidance",{session:active.session,response:parameterSensitivity.response||"Current objective",parameter_gradients:parameterSensitivity.parameter_gradients});
          active.guidance=guide;
          const g=guide.predicted_change_per_mm_world||[0,0,0];
          active.guidanceNormal=g[0]*hit.normal[0]+g[1]*hit.normal[1]+g[2]*hit.normal[2];
        }catch(_){active.guidance=null;}
      }
      const lead=active.parameters.slice().sort((a,b)=>(b.influence||0)-(a.influence||0))[0];
      active.label=lead?`${displayIdentifier(lead.key,{label:lead.label,display_name:lead.display_name})}${active.semantic?" · direct":" · differential"}`:"Differential surface";
      state.undo=+(reply.undo||state.undo);state.redo=+(reply.redo||state.redo);setCounts();draw();
      setStatus(`${displayIdentifier(reply.derivative_method||"derivative")}; ${active.parameters.length} influencing parameter${active.parameters.length===1?"":"s"}`);
      if(active.cancelled||active.released){
        await finish(!active.cancelled,active.releaseEvent||active.lastEvent);
        return;
      }
      const lp=local(active.lastEvent);
      if(Math.hypot(lp.x-active.startScreen[0],lp.y-active.startScreen[1])>0.5)
        schedulePreview(active.lastEvent);
    }catch(error){
      if(state.active===active)state.active=null;
      pointerCoordinator.release(pointerOwner,e.pointerId);
      console.warn("Implexity direct interaction could not start",error);
      overlay.style.cursor="crosshair";setStatus(friendlyErrorMessage(error,"Direct editing could not start. Try again or refresh the model."),"error");draw();
    }
  }

  function schedulePreview(e){
    const active=state.active;if(!active||!active.session)return;
    active.lastEvent=e;state.latest={active,event:e};
    if(state.inflight)return;
    state.previewTask=runPreviewLoop();
  }
  async function runPreviewLoop(){
    state.inflight=true;
    try{
      while(state.latest&&state.active){
        const item=state.latest;state.latest=null;
        if(item.active!==state.active||!item.active.session)continue;
        const delta=normalDelta(item.active,item.event);
        const sequence=++state.sequence;
        const payload={session:item.active.session,sequence,normal_delta_mm:delta};
        if(item.active.semantic)payload.semantic_parameter=item.active.semantic;
        try{
          const reply=await post("/v1/implicit/manipulation/preview",payload);
          requirePreviewAcknowledgement(reply,sequence);
          item.active.lastAcceptedSequence=sequence;
          item.active.previewFailure=null;
          if(state.active===item.active){
            const values=reply.values||{};
            const lead=item.active.semantic||Object.keys(values)[0];
            item.active.label=lead&&values[lead]!=null?`${displayIdentifier(lead)} = ${fmt(values[lead],4)}`:item.active.label;
            if(Number.isFinite(item.active.guidanceNormal)){
              const predicted=item.active.guidanceNormal*delta;
              const responseLabel=displayIdentifier(parameterSensitivity?.response||"current_objective",{
                label:parameterSensitivity?.response_label||(!parameterSensitivity?.response?"Current objective":undefined),
                display_name:parameterSensitivity?.response_display_name
              });
              setStatus(`First-order prediction for ${responseLabel}: ${predicted>=0?"+":""}${fmt(predicted,5)} for this drag. Fresh physics is required after commit.`);
            }
            await refreshModel({preview:true,values,moved:reply.moved||[]});
          }
        }catch(error){
          item.active.previewFailure=error;
          console.warn("Implexity direct-interaction preview failed",error);
          if(state.active===item.active)setStatus(friendlyErrorMessage(error,"The live geometry preview could not be updated. Your model has not been changed."),"error");
        }
      }
    }finally{state.inflight=false;state.previewTask=null;}
  }

  async function finish(commit,finalEvent=null){
    const active=state.active;if(!active)return;
    active.released=true;active.cancelled=!commit;
    if(finalEvent){active.releaseEvent=finalEvent;active.lastEvent=finalEvent;}
    if(!active.session)return;
    if(active.finishing)return;
    active.finishing=true;
    if(commit){


      state.latest={active,event:active.lastEvent};
      if(!state.inflight)state.previewTask=runPreviewLoop();
    }else state.latest=null;
    if(state.previewTask){try{await state.previewTask;}catch(_){ }}
    if(state.active!==active)return;
    let committed=false;
    try{
      if(commit&&(active.previewFailure||active.lastAcceptedSequence<0)){
        throw active.previewFailure||new Error("the release preview was not acknowledged");
      }
      const endpoint=commit?"/v1/implicit/manipulation/commit":"/v1/implicit/manipulation/cancel";
      const payload={session:active.session};
      if(commit)payload.final_sequence=active.lastAcceptedSequence;
      const reply=await post(endpoint,payload);
      committed=commit;
      state.undo=+(reply.undo||0);state.redo=+(reply.redo||0);
      await refreshModel({record:commit});
      if(commit)window.dispatchEvent(new CustomEvent("implexity:model-updated",{detail:{source:"direct-geometry"}}));
      setStatus(commit?(reply.changed?"Geometry gesture committed":"Geometry unchanged"):"Gesture cancelled",commit?"ok":"");
      if(commit&&reply.changed&&globalThis.ImplexityManualHistory?.record)globalThis.ImplexityManualHistory.record("direct",reply.label||"Direct geometry gesture");
    }catch(error){
      if(commit&&!committed){try{await post("/v1/implicit/manipulation/cancel",{session:active.session});}catch(_){                        }}
      console.warn("Implexity direct-interaction finish failed",error);
      setStatus(friendlyErrorMessage(error,"The geometry gesture could not be completed. Your previous model remains active."),"error");
      try{await refreshModel({record:false});}catch(_){                                }
    }
    finally{state.active=null;pointerCoordinator.release(pointerOwner,active.pointerId);overlay.style.cursor="crosshair";setCounts();draw();}
  }

  async function history(op,rethrow=false,payload={}){
    if(state.active)throw new Error("Finish the current gesture before undo or redo.");
    let restored=false;
    try{
      const reply=await post(`/v1/implicit/manipulation/${op}`,payload);
      restored=true;
      state.undo=+(reply.undo||0);state.redo=+(reply.redo||0);
      await refreshModel({record:true});window.dispatchEvent(new CustomEvent("implexity:model-updated",{detail:{source:`direct-${op}`}}));setCounts();setStatus(displayIdentifier(op,{label:reply.label}),"ok");return reply;
    }catch(error){if(restored)error.historyApplied=true;console.warn("Implexity edit-history action failed",error);setStatus(friendlyErrorMessage(error,"The edit-history action could not be completed."),"error");if(rethrow)throw error;}
  }
  function registerManualHistory(){
    globalThis.ImplexityManualHistory?.register?.("direct",{undo:payload=>history("undo",true,payload),redo:payload=>history("redo",true,payload)});
  }
  function requestHistory(op){
    const coordinator=globalThis.ImplexityManualHistory;
    return coordinator?.request?coordinator.request(op).catch(error=>setStatus(friendlyErrorMessage(error,"The edit-history action could not be completed."),"error")):history(op);
  }

  function cameraMove(dx,dy,pan){
    if(VIEW.gl&&VIEW.gl.camera){if(pan)VIEW.gl.camera.pan(dx,dy);else VIEW.gl.camera.orbit(dx,dy);}
    else if(pan){
      const gain=VIEW.cam.dist*0.0022,right=VIEW._right(),up=VIEW._up();
      for(let i=0;i<3;i++)VIEW.cam.target[i]+=(-dx*right[i]+dy*up[i])*gain;
      VIEW.invalidate(true);
    }else{VIEW.cam.yaw-=dx*0.008;VIEW.cam.pitch=clamp(VIEW.cam.pitch-dy*0.008,-1.5,1.5);VIEW.invalidate(true);}
    draw();
  }
  async function onDown(e){
    if(!state.enabled||state.mode!=="edit"||state.active||state.pendingPointer)return;
    if(!pointerCoordinator.claim(pointerOwner,e.pointerId))return;
    if(e.button===1||e.button===2){
      state.cameraDrag={pointerId:e.pointerId,x:e.clientX,y:e.clientY,pan:e.button===1||e.shiftKey};
      try{overlay.setPointerCapture(e.pointerId);}catch(_){ }
      overlay.style.cursor=(e.button===1||e.shiftKey)?"move":"grabbing";e.preventDefault();return;
    }
    if(e.button!==0){pointerCoordinator.release(pointerOwner,e.pointerId);return;}
    const sampled=pickAt(e);if(!sampled){pointerCoordinator.release(pointerOwner,e.pointerId);setStatus("No implicit surface was found below the cursor.");return;}
    e.preventDefault();
    const pending={pointerId:e.pointerId,downEvent:e,lastEvent:e,releaseEvent:null,released:false,cancelled:false};
    state.pendingPointer=pending;
    try{
      const exact=await viewerAdapter.refineSurfaceHit({hit:{point_mm:sampled.point,normal:sampled.normal,raw:sampled,clip_evidence:sampled.clip_evidence}});
      if(!exact||exact.exact!==true)throw new Error("exact surface refinement was unavailable");
      const hit={...sampled,point:(exact.point_mm||exact.point).slice(),normal:(exact.normal||sampled.normal).slice(),exact:true,approximate:false,
        model_identity:exact.model_identity,clip_evidence:exact.clip_evidence};
      if(!pointerCoordinator.owns(pointerOwner,e.pointerId)||state.pendingPointer!==pending||pending.cancelled)return;
      begin(e,hit,pending);
    }catch(error){
      if(state.pendingPointer===pending)state.pendingPointer=null;
      console.warn("Implexity exact surface refinement failed",error);
      pointerCoordinator.release(pointerOwner,e.pointerId);setStatus(friendlyErrorMessage(error,"The preview point could not be refined on the exact surface; nothing changed."),"error");
    }
  }
  function onMove(e){
    if(state.cameraDrag&&e.pointerId===state.cameraDrag.pointerId){
      const d=state.cameraDrag,dx=e.clientX-d.x,dy=e.clientY-d.y;d.x=e.clientX;d.y=e.clientY;cameraMove(dx,dy,d.pan);e.preventDefault();return;
    }
    if(state.pendingPointer&&e.pointerId===state.pendingPointer.pointerId){state.pendingPointer.lastEvent=e;e.preventDefault();return;}
    if(state.active){state.active.lastEvent=e;if(state.active.session&&!state.active.finishing)schedulePreview(e);e.preventDefault();return;}
    hoverEvent(e);
  }
  function onUp(e){
    if(state.cameraDrag&&e.pointerId===state.cameraDrag.pointerId){state.cameraDrag=null;pointerCoordinator.release(pointerOwner,e.pointerId);overlay.style.cursor="crosshair";e.preventDefault();return;}
    if(state.pendingPointer&&e.pointerId===state.pendingPointer.pointerId){
      state.pendingPointer.released=true;state.pendingPointer.releaseEvent=e;state.pendingPointer.lastEvent=e;e.preventDefault();return;
    }
    if(!state.active||e.pointerId!==state.active.pointerId)return;e.preventDefault();finish(true,e);
  }
  function onCancel(e){
    if(state.cameraDrag){pointerCoordinator.release(pointerOwner,state.cameraDrag.pointerId);state.cameraDrag=null;overlay.style.cursor="crosshair";}
    if(state.pendingPointer&&e.pointerId===state.pendingPointer.pointerId){
      const pending=state.pendingPointer;pending.cancelled=true;state.pendingPointer=null;
      pointerCoordinator.release(pointerOwner,pending.pointerId);e.preventDefault();return;
    }
    if(!state.active)return;e.preventDefault();finish(false,e);
  }
  function onKey(e){
    if(e.key==="Escape"&&state.active){e.preventDefault();finish(false);}
    if(e.key==="Escape"&&state.pendingPointer){
      const pending=state.pendingPointer;pending.cancelled=true;state.pendingPointer=null;
      pointerCoordinator.release(pointerOwner,pending.pointerId);e.preventDefault();return;
    }
    if(!(e.ctrlKey||e.metaKey)||isEditableTarget(e.target)||!state.enabled||state.mode!=="edit"||state.active||state.pendingPointer)return;
    const owner=pointerCoordinator.current().owner;
    if(owner&&owner!==pointerOwner)return;
    const doc=stage.ownerDocument||document,target=e.target;
    const localTarget=!target||target===window||target===doc||target===doc.body||target===doc.documentElement||target===stage||
      (typeof stage.contains==="function"&&stage.contains(target));
    if(!localTarget)return;
    if(!e.shiftKey&&e.key.toLowerCase()==="z"){e.preventDefault();requestHistory("undo");}
    if(e.key.toLowerCase()==="y"||(e.shiftKey&&e.key.toLowerCase()==="z")){e.preventDefault();requestHistory("redo");}
  }
  function onWheel(e){
    if(state.mode!=="edit")return;
    const f=Math.exp((e.deltaY>0?1:-1)*0.12);
    if(VIEW.gl&&VIEW.gl.camera)VIEW.gl.camera.zoom(f);else{VIEW.cam.dist=clamp(VIEW.cam.dist*f,.4,4000);VIEW.invalidate(true);}
    e.preventDefault();
  }

  overlay.addEventListener("pointerdown",onDown);
  overlay.addEventListener("pointermove",onMove);
  overlay.addEventListener("pointerup",onUp);
  overlay.addEventListener("pointercancel",onCancel);
  overlay.addEventListener("pointerleave",()=>{if(!state.active){state.hover=null;draw();}});
  overlay.addEventListener("wheel",onWheel,{passive:false});
  overlay.addEventListener("contextmenu",e=>e.preventDefault());
  window.addEventListener("keydown",onKey);
  if(ui.edit)ui.edit.addEventListener("click",()=>setMode("edit"));
  if(ui.orbit)ui.orbit.addEventListener("click",()=>setMode("orbit"));
  if(ui.undo)ui.undo.addEventListener("click",()=>requestHistory("undo"));
  if(ui.redo)ui.redo.addEventListener("click",()=>requestHistory("redo"));
  registerManualHistory();
  window.addEventListener("implexity:manual-history-ready",registerManualHistory);
  window.addEventListener("implexity:manual-history-changed",setCounts);

   
  const readManipulationCounts=()=>{
    if(!S.model?.loaded)return;
    api("/v1/implicit/manipulation").then(reply=>{
      state.undo=+(reply.undo||0);state.redo=+(reply.redo||0);setCounts();
    }).catch(()=>{});
  };
  readManipulationCounts();
  window.addEventListener("implexity:design-state-changed",event=>{if(event.detail?.source==="model")readManipulationCounts();});
  setMode("edit");setCounts();

  const viewerAdapter=Object.assign(globalThis.ImplexityViewerAdapter||{}, {
    coordinate_space:"viewport-css-pixels",
    canvas:overlay,
    async refreshAuthoritativeModel(){ return await refreshModel({record:false}); },
    pickSurface(x,y,detail={}){
      const rect=overlay.getBoundingClientRect();
       
       
      const event=detail.event;
      const px=Number.isFinite(event?.clientX)?event.clientX-rect.left:+x;
      const py=Number.isFinite(event?.clientY)?event.clientY-rect.top:+y;
      const hit=pickLocal(px,py,rect.width,rect.height);
      if(!hit)return null;
      return{point_mm:hit.point,normal:hit.normal,approximate:true,exact:false,
        ray:hit.ray,spacing_mm:hit.spacing,clip_evidence:hit.clip_evidence,
        model_identity:hit.model_identity,bounds_mm:{min_mm:VIEW.grid.lo,max_mm:VIEW.grid.hi},raw:hit};
    },
    async refineSurfaceHit(payload){
      const source=payload?.hit?.raw?.raw||payload?.hit?.raw||payload?.hit;
      const clip=payload?.hit?.clip_evidence||source?.clip_evidence;
      if(clip?.hit_on_clip_cap)return null;
      const response=await post("/v1/implicit/interactions/refine",{
        point_mm:payload?.hit?.point_mm||source?.point,
        normal:payload?.hit?.normal||source?.normal,
        ray:source?.ray||null,
        model_identity:source?.model_identity||payload?.hit?.raw?.model_identity||null,
        clip_evidence:clip||clipEvidence()
      });
      return response?.hit||response;
    },
    getCameraEvidence(){return cameraEvidence();},
    getClipEvidence(){return clipEvidence();},
    project(point){const rect=overlay.getBoundingClientRect(),p=projectPoint(point,rect.width,rect.height,camera());return p&&[p.x,p.y,p.depth];},
    unprojectAtDepth(x,y,depth,detail={}){
      const rect=overlay.getBoundingClientRect(),event=detail.event;
      const px=Number.isFinite(event?.clientX)?event.clientX-rect.left:+x;
      const py=Number.isFinite(event?.clientY)?event.clientY-rect.top:+y;
      if(Array.isArray(depth))return unprojectOnAnchorPlane(px,py,rect.width,rect.height,camera(),depth);
      const distance=Number(depth);if(!Number.isFinite(distance))return null;
      const ray=rayFromScreen(px,py,rect.width,rect.height,camera());
      return add(ray.origin,scale(ray.direction,distance));
    },
    getModelBounds(){return VIEW.grid?{min_mm:VIEW.grid.lo.slice(),max_mm:VIEW.grid.hi.slice()}:null;}
  });
  globalThis.ImplexityViewerAdapter=viewerAdapter;

  const modeListener=event=>{
    const previous=state.tool;
    state.tool=event?.detail?.mode||"surface";
    setEnabled(event?.detail?.directEnabled!==false);
    if(previous!==state.tool&&!state.active)
      setStatus(event?.detail?.help||"Selected tool ready.");
  };
  window.addEventListener("implexity:interaction-mode",modeListener);
   
   
  const currentInteraction=globalThis.ImplexityInteraction;
  const interactionModes=globalThis.ImplexityInteractionModes;
  if(currentInteraction&&interactionModes){
    const mode=currentInteraction.mode;
    modeListener({detail:{mode,directEnabled:[interactionModes.MOVE,interactionModes.SIZE,interactionModes.SURFACE].includes(mode)}});
  }

  const publicApi = {
    state,setMode,setEnabled,pickSurface:viewerAdapter.pickSurface.bind(viewerAdapter),
    refineSurfaceHit:viewerAdapter.refineSurfaceHit.bind(viewerAdapter),
    getCameraEvidence:viewerAdapter.getCameraEvidence.bind(viewerAdapter),getClipEvidence:viewerAdapter.getClipEvidence.bind(viewerAdapter),
    selectionChanged(){state.hover=null;draw();},
    cancel(){return finish(false);},undo(){return history("undo");},redo(){return history("redo");},
    dispose(){
      state.disposed=true;if(ro)ro.disconnect();window.removeEventListener("resize",resize);window.removeEventListener("keydown",onKey);window.removeEventListener("implexity:interaction-mode",modeListener);window.removeEventListener("implexity:manual-history-ready",registerManualHistory);window.removeEventListener("implexity:manual-history-changed",setCounts);
      overlay.removeEventListener("pointerdown",onDown);overlay.removeEventListener("pointermove",onMove);
      overlay.removeEventListener("pointerup",onUp);overlay.removeEventListener("pointercancel",onCancel);
    }
  };
  globalThis.ImplexityDirectInteraction=globalThis.ImplexityDirectInteraction||publicApi;

  return publicApi;
}
