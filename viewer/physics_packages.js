// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(()=>{"use strict";
let state=null,host=null,busy=false,lastError=null,requestVersion=0,providers=null,importPanel=null;
const el=(tag,text)=>{const n=document.createElement(tag);if(text)n.textContent=String(text).replaceAll(String.fromCharCode(8212),": ");return n;};
const present=(id,metadata={})=>window.ImplexityText?.present?.(id,metadata)||({id:String(id??''),label:String(metadata.label||metadata.display_name||id||''),description:String(metadata.description||metadata.scope||'')});
async function request(method,body){const r=await fetch('/v1/physics/packages',{method,credentials:'same-origin',headers:{'Content-Type':'application/json'},...(body?{body:JSON.stringify(body)}:{})});const d=await r.json();if(!r.ok)throw new Error(d.error||`HTTP ${r.status}`);return d;}
async function providerRequest(method,body,check=false){const r=await fetch('/v1/physics/providers/imported'+(check?'/check':''),{method,credentials:'same-origin',headers:{'Content-Type':'application/json'},...(body?{body:JSON.stringify(body)}:{})});const d=await r.json();if(!r.ok)throw new Error(d.error||`HTTP ${r.status}`);return d;}
function friendlyError(summary,error){return {summary,detail:String(error?.message||error||'No technical detail was returned.').replace(/^Error:\s*/,'')}}
function renderStatus(status){
 status.replaceChildren();
 const restoration=state?.restoration_error?friendlyError('The saved physics selection could not be restored. Choose a valid package explicitly.',state.restoration_error):null;
 const problem=lastError||restoration;
 if(problem){status.append(document.createTextNode(problem.summary+' '));const details=el('details'),summary=el('summary','Technical details'),detail=el('span',problem.detail);details.dataset.tier='expert';details.append(summary,detail);status.append(details);return}
 const count=state?.loaded?.length||0;
 status.textContent=count?`${count} physics ${count===1?'package':'packages'} active. Active components still require compatible coupling, authored data and model preflight.`:'Geometry-only mode. Manual modelling is available; activate suitable physics before optimisation.';
}
function render(){if(!host||!state)return;const rows=host.querySelector('[data-package-rows]');
 const openPanels=new Set(Array.from(rows.querySelectorAll('details[data-package-panel]')).filter(panel=>panel.open).map(panel=>panel.dataset.packagePanel));
 const active=document.activeElement,focusedPackage=rows.contains(active)?active?.dataset?.package:null;
 rows.replaceChildren();
 host.setAttribute('aria-busy',String(busy));
 renderStatus(host.querySelector('[data-package-status]'));
 const blockers=(state.lifecycle_blockers||[]).map(String),locked=blockers.length>0;
 const lockReason=locked?`Package changes are locked while optimisation ${blockers.length===1?`job ${blockers[0].slice(0,12)}`:`jobs ${blockers.map(id=>id.slice(0,12)).join(', ')}`} ${blockers.length===1?'is':'are'} active.`:'';
 const loaded=state.packages.filter(p=>p.loaded),overview=el('div');overview.className='implexity-package-active-summary';
 overview.append(el('strong',loaded.length?'Active add-ins':'No physics add-ins active'));
 if(loaded.length){const chips=el('div');chips.className='implexity-package-active-chips';for(const p of loaded){const descriptor=present(p.id,{label:p.label,description:p.scope}),chip=el('span',descriptor.label);chip.title=descriptor.description||descriptor.label;chips.append(chip);}overview.append(chips);}
 else overview.append(el('p','Manual geometry remains available. Activate an add-in only when its declared physics is needed.'));
 if(locked)overview.append(el('p',lockReason));
 rows.append(overview);
 const catalogue=el('details'),catalogueSummary=el('summary',`Manage physics add-ins · ${state.packages.length} installed`),list=el('div');catalogue.className='implexity-package-package-catalogue';list.className='implexity-package-package-list';catalogue.append(catalogueSummary,list);
 for(const p of state.packages){const descriptor=present(p.id,{label:p.label,description:p.scope}),row=el('article'),copy=el('div'),label=el('strong',descriptor.label),scope=el('p',descriptor.description||'Contributes registered physics capabilities to the active engineering model.'),stateLabel=el('span',p.loaded?'Active':'Available'),action=p.loaded?'Deactivate':'Activate',button=el('button',action);row.className='implexity-package-package';stateLabel.className=`implexity-package-package-state ${p.loaded?'active':'available'}`;button.type='button';button.dataset.package=p.id;button.setAttribute('aria-label',`${action} ${descriptor.label}`);button.disabled=busy||locked;if(locked)button.title=lockReason;button.onclick=()=>change(p.id,p.loaded?'unload':'load');copy.append(label,stateLabel,scope);row.append(copy,button);list.append(row);}
 catalogue.dataset.packagePanel='catalogue';catalogue.open=openPanels.has('catalogue');rows.append(catalogue);
 const detail=el('details'),summary=el('summary','Active component execution status');detail.append(summary);
 for(const item of state.component_status?.components||[]){const line=el('p'),component=present(item.addin_id,{label:item.label,description:item.description});line.dataset.addinId=item.addin_id;line.title=component.description||`Technical identifier: ${item.addin_id}`;
  line.append(el('strong',component.label+': '),document.createTextNode(present(item.runtime_support?.status||'unknown').label));
  const integrations=item.compatible_field_integrations||[];
  if(integrations.length){const detail=el('small',': Field integration: '+integrations.map(x=>`${present(x.field_solver,{label:x.field_solver_label}).label} / ${present(x.slot,{label:x.slot_label}).label}`).join(', ')+' (model preflight required)');detail.dataset.integrationIds=integrations.map(x=>`${x.field_solver}/${x.slot}`).join(',');line.append(detail);}
  const limits=item.runtime_support?.limitations||[];if(limits.length)line.append(el('small',': '+limits.join(' ')));
  detail.append(line);
 }
 detail.dataset.packagePanel='execution';detail.open=openPanels.has('execution');rows.append(detail);
 if(focusedPackage){const replacement=Array.from(rows.querySelectorAll('button[data-package]')).find(button=>button.dataset.package===focusedPackage);if(replacement&&!replacement.disabled)replacement.focus({preventScroll:true});}
}
async function refresh(){
 if(busy)return state;
 const version=++requestVersion;
 try{const [next,imported]=await Promise.all([request('GET'),providerRequest('GET')]);if(version!==requestVersion)return state;state=next;providers=imported;lastError=null;render();renderProviders();return state;}
 catch(e){if(version!==requestVersion)return state;lastError=friendlyError('Physics packages could not be refreshed.',e);if(host&&state)render();else if(host)renderStatus(host.querySelector('[data-package-status]'));throw e;}
}
async function change(id,operation){
 if(busy)return;
 ++requestVersion;busy=true;lastError=null;render();
 try{
  state=await request('POST',{package:id,operation,expected_generation:state?.generation});
  const wb=window.implexityWorkbench;
  if(wb){
   wb.latestPhysicsPlan=null;wb.latestPhysicsIntent=null;wb.appliedResponseProgram=null;
   try{await wb.refresh();}catch(e){lastError=friendlyError('The physics selection changed, but the workspace could not refresh. Reload the workspace before continuing.',e);}
  }
  window.dispatchEvent(new CustomEvent('implexity:physics-packages-changed',{detail:state}));return state;
 }catch(e){lastError=friendlyError('The physics-package change could not be confirmed. Refresh packages before retrying.',e);}
 finally{busy=false;render();renderProviders();}
}
async function updateProviders(body){
 if(busy)return;busy=true;++requestVersion;lastError=null;render();renderProviders();
 try{providers=await providerRequest('POST',{...body,expected_generation:state?.generation,expected_provider_generation:providers?.provider_generation});
  const wb=window.implexityWorkbench;if(wb){wb.latestPhysicsPlan=null;wb.latestPhysicsIntent=null;wb.appliedResponseProgram=null;await wb.refresh();}
  window.dispatchEvent(new CustomEvent('implexity:physics-packages-changed',{detail:state}));
 }catch(e){lastError=friendlyError('Provider change could not be confirmed. Refresh before retrying.',e);}
 finally{busy=false;await refresh().catch(()=>{});render();renderProviders();}
}
function renderProviders(){
 if(!importPanel)return;const rows=importPanel.querySelector('[data-provider-rows]');rows.replaceChildren();
 const locked=busy||(state?.lifecycle_blockers||[]).length>0||(providers?.lifecycle_blockers||[]).length>0;
 for(const button of importPanel.querySelectorAll('button'))button.disabled=locked;
 if(providers?.restoration_error)rows.append(el('p',`Saved providers could not be restored: ${providers.restoration_error}`));
 for(const provider of providers?.providers||[]){
  const row=el('article');row.className='implexity-package-package';const copy=el('div');copy.append(el('strong',present(provider.id,{label:provider.label}).label),el('p',`Version ${provider.version}. ${provider.gradients?'Topology gradients available.':'Evaluation only.'}`));
  const declaration=el('details');declaration.append(el('summary','Model range'));
  for(const input of provider.manifest?.inputs||[])declaration.append(el('p',`${present(input.pointer.replace(/^\//,'')).label} / ${input.unit} = ${input.min} to ${input.max}`));
  const topology=provider.manifest?.topology;if(topology)declaration.append(el('p',`Topology grid = ${topology.shape.join(' × ')}. Values = ${topology.min} to ${topology.max}.`));
  const reference=provider.manifest?.reference;if(reference)declaration.append(el('p',`Reference = ${present(reference.provider).label}. Check interval = ${reference.check_every} calls per operation.`));
  copy.append(declaration);
  const actions=el('div');actions.className='implexity-provider-actions';const check=el('button','Check model'),remove=el('button','Remove');check.type=remove.type='button';check.disabled=remove.disabled=locked;
  const file=el('input');file.type='file';file.accept='.json,application/json';file.hidden=true;file.setAttribute('aria-label',`Check state for ${provider.label}`);
  check.onclick=()=>file.click();file.onchange=async()=>{const selected=file.files?.[0];if(!selected)return;busy=true;render();renderProviders();
   try{const request=JSON.parse(await selected.text());const report=await providerRequest('POST',{...request,provider:provider.id},true);const output=importPanel.querySelector('[data-provider-result]');output.replaceChildren();output.append(el('p',report.passed?'Checks passed at the supplied state.':'Differences found at the supplied state.'));const details=el('details');details.append(el('summary','Check results'),el('pre',JSON.stringify(report,null,2)));output.append(details);}
   catch(e){lastError=friendlyError('Model check could not finish.',e);}
   finally{busy=false;file.value='';render();renderProviders();}
  };
  remove.onclick=()=>updateProviders({operation:'remove',provider:provider.id});actions.append(check,remove,file);row.append(copy,actions);rows.append(row);
 }
 importPanel.querySelector('[data-provider-note]').textContent='Under Choose physics providers, select the model and set required fidelity to Approximation permitted.';
}
function providerControls(){
 importPanel=el('details');importPanel.className='implexity-provider-import';importPanel.append(el('summary','External physics models'));
 const note=el('p');note.dataset.providerNote='';const rows=el('div');rows.dataset.providerRows='';
 const path=el('input');path.type='text';path.placeholder='Local manifest path';path.setAttribute('aria-label','Local provider manifest path');
 const load=el('button','Import from path');load.type='button';load.onclick=()=>{if(path.value.trim())updateProviders({operation:'import',manifest_path:path.value.trim()});};
 const upload=el('button','Choose manifest');upload.type='button';const file=el('input');file.type='file';file.accept='.json,application/json';file.hidden=true;file.setAttribute('aria-label','Provider manifest');upload.onclick=()=>file.click();
 file.onchange=async()=>{const selected=file.files?.[0];if(!selected)return;try{const manifest=JSON.parse(await selected.text());await updateProviders({operation:'import',manifest});}catch(e){lastError=friendlyError('Manifest could not be read.',e);render();}finally{file.value='';}};
 const template=el('button','Manifest template');template.type='button';template.onclick=()=>{
  const manifest={schema:'implexity-provider-import/1',id:'external_model',label:'External model',kind:'learned',version:'1',command:['/absolute/path/to/adapter'],assets:[{path:'/absolute/path/to/weights',sha256:'replace_with_asset_sha256'}],timeout_seconds:60,inputs:[{pointer:'/load',unit:'-',min:0,max:1}],topology:{coordinate:'model:control',shape:[2,2,2],min:0,max:1},responses:{response:{unit:'-'}},fields:{field:{unit:'-'}},gradients:true};
  const url=URL.createObjectURL(new Blob([JSON.stringify(manifest,null,2)],{type:'application/json'}));const link=el('a');link.href=url;link.download='physics-provider.json';document.body.append(link);link.click();link.remove();setTimeout(()=>URL.revokeObjectURL(url),1000);
 };
 const form=el('div');form.className='implexity-provider-form';form.append(path,load,upload,template,file);const caution=el('p','Import starts the local adapter specified in the manifest.');const result=el('div');result.dataset.providerResult='';result.setAttribute('role','status');importPanel.append(note,rows,form,caution,result);host.append(importPanel);
}
function boot(){const parent=document.querySelector('#s_engsys .sect-b')||document.querySelector('#right');if(!parent)return;host=el('section');host.id='implexity-physics-packages';host.className='implexity-package-packages';const title=el('h3','Physics add-ins'),status=el('div'),rows=el('div'),refreshButton=el('button','Refresh packages');status.dataset.packageStatus='';status.className='implexity-package-package-status';status.setAttribute('role','status');rows.dataset.packageRows='';refreshButton.type='button';refreshButton.onclick=()=>refresh().catch(()=>{});host.append(title,status,rows,refreshButton);providerControls();parent.prepend(host);refresh().catch(()=>{});}
window.ImplexityPhysicsPackages={refresh,change,importProvider:manifest=>updateProviders({operation:"import",manifest}),isImported:id=>(providers?.providers||[]).some(p=>p.id===id),get generation(){return state?.generation;}};
window.addEventListener('implexity:optimization-progress',()=>refresh().catch(()=>{}));
if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot,{once:true});else boot();
})();
