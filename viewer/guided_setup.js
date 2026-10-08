// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(() => {
'use strict';
const clone = value => structuredClone(value);
const own = (value, key) => value != null && Object.hasOwn(value, key);
const canonical = value => JSON.stringify(value, (_k, v) => v && typeof v === 'object' && !Array.isArray(v)
  ? Object.fromEntries(Object.keys(v).sort().map(k => [k, v[k]])) : v);
function get(root, path) {
  let value = root;
  for (const key of path) {if (!own(value, key)) throw new Error('The field no longer exists. Refresh this editor.'); value = value[key];}
  return value;
}
function put(root, path, value) {
  if (!path.length) throw new Error('The complete request must be an object.');
  const parent = get(root, path.slice(0, -1));
  Object.defineProperty(parent, path.at(-1), {value, writable:true, enumerable:true, configurable:true});
}
function scalar(text, previous) {
  if (typeof previous === 'boolean') {if (!['true','false'].includes(text)) throw new Error('Choose Yes or No.'); return text === 'true';}
  if (typeof previous === 'number') {if (!String(text).trim()) throw new Error('A number is required.'); const n = Number(text); if (!Number.isFinite(n)) throw new Error('Enter a finite number.'); return n;}
  if (previous === null) {if (!String(text).trim() || text === 'null') return null; return window.ImplexityAdvancedCommandCore.parseValue(text);}
  return String(text);
}
function bindingStale(state, active) {
  return !state?.applied_current || state.stale || state.revision!==active?.revision || state.record?.applied?.request_sha256!==active?.applied?.request_sha256;
}
function authoringAvailable(blocksNewRun, status) {
   
  return !blocksNewRun || status === 'intervening';
}
function currentPhysicsDraft(request, state) {
  const problem=state?.problem_record;
  if(!problem?.problem||!problem.provider||request.physics?.provider!==problem.provider)throw new Error('The current applied physics uses another provider or is missing. Switch providers explicitly first.');
  const next=clone(request);next.physics.problem=clone(problem.problem);
  if(own(next,'physics_generation'))next.physics_generation=state.current_binding.physics_generation;
  return next;
}
function launchRequest(state, seq) {
  if (!state?.applied_current || !state.record?.applied) throw new Error('Apply a reviewed complete setup before running it.');
  return {...clone(state.record.request), seq, applied_setup:{revision:state.record.revision, request_sha256:state.record.applied.request_sha256}};
}
const groups = Object.freeze([
  ['physics','Physics, materials & history',['physics','provider','physics_generation']],
  ['responses','Objectives & limits',['responses']],
  ['design','Design coordinates & bounds',['design_coordinates','inactive_design_coordinates','design_coordinate_selection','topology','design_freedom']],
  ['run','Optimization controls',['iters','iterations','lr','step_fraction','live_every','settings','update_metric','response_normalization']],
  ['resources','Resources & coupling effort',['computation_effort']],
  ['stages','Operating points & stages',['schedule','operating_points','robust_mode']],
  ['extensions','Other settings & extensions',[]]
]);
function groupKeys(request, id) {
  const row = groups.find(x => x[0] === id);
  const known = new Set(groups.flatMap(x=>x[2]));
  return Object.keys(request).filter(k=>id==='extensions' ? !known.has(k) : row[2].includes(k));
}
function leaves(value, path=[]) {
  if (Array.isArray(value)) return value.flatMap((v,i)=>leaves(v,[...path,i]));
  return [{path, value}];
}
function formDraft(value, authoringFields) {
  const result = clone(value);
  if (Array.isArray(authoringFields) && result.physics?.problem) {
    result.physics.problem = Object.fromEntries(Object.entries(result.physics.problem).filter(([key])=>authoringFields.includes(key)));
  }
  return result;
}
window.ImplexityGuidedSetupCore = Object.freeze({get,put,scalar,launchRequest,groupKeys,groups,canonical,leaves,formDraft,authoringAvailable,currentPhysicsDraft,bindingStale});
if (typeof document === 'undefined') return;
const make = (tag, text='', css='') => {const n=document.createElement(tag);n.textContent=text;n.className=css;return n;};
const human = value => String(value).replaceAll('_',' ').replace(/\b\w/g,c=>c.toUpperCase());
const pathText = path => path.join(' / ');
async function action(name, payload={}) {
  const response = await fetch('/v1/agent/action',{method:'POST',credentials:'same-origin',cache:'no-store',
    headers:{'Content-Type':'application/json'},body:JSON.stringify({action:name,payload})});
  let value;
  try {value=await response.json();} catch (_) {throw new Error('The reply is unreadable. A submitted change may have succeeded.');}
  if (!response.ok || value.ok===false) {
    const details = value.problems || value.details?.problems || value.error?.problems || [];
    const error=new Error([typeof value.error==='string'?value.error:`HTTP ${response.status}`, ...details].join(' · '));
    error.refused=response.status>=400&&response.status<500;throw error;
  }
  if (value.action!==name || value.ok!==true) throw new Error('The reply did not acknowledge the requested setup operation.');
  return value.result;
}
class GuidedSetup {
  constructor() {
    this.panelMode=false;this.busy=false;this.dirty=false;this.review=null;this.active=null;this.unconfirmed=false;this.stale=false;this.tab='run';this.invalidFields=new Map();
    this.build();
    window.addEventListener('beforeunload',e=>{if(this.dirty){e.preventDefault();e.returnValue='';}});
    window.addEventListener('focus',()=>{void this.poll();});
    window.addEventListener('implexity:design-state-changed',()=>{this.update();void this.poll();});
    window.addEventListener('implexity:public-command-result',e=>{if(['apply_guided_setup','save_optimization_setup','revise_engineering_problem','set_engineering_problem'].includes(e.detail?.action))void this.poll();});
    this.timer=setInterval(()=>{if(!document.hidden)void this.poll();},2500);
  }
  button(text, fn, id) {const b=make('button',text);b.type='button';if(id)b.id=id;b.addEventListener('click',()=>Promise.resolve().then(fn).catch(e=>this.message(e.message,true)));return b;}
  message(text,error=false) {this.status.textContent=text;this.status.dataset.error=String(error);}
  build() {
    this.card=make('section','','guided-setup-card');this.card.id='guidedSetupCard';
    this.card.append(make('h3','All run settings'),make('p','Edit physics, objectives, design bounds and solver settings.'));
    this.openButton=this.button('Edit all settings…',()=>this.open('saved'),'guidedSetupOpen');
    this.currentButton=this.button('Use panel settings…',()=>this.open('panels'),'guidedSetupCapture');
    this.badge=make('p','Panel settings are active.','guided-setup-badge');this.badge.setAttribute('role','status');
    this.panelSwitch=this.button('Use individual panels instead',()=>this.usePanels(),'guidedSetupPanelSwitch');
    this.panelSwitch.hidden=true;this.card.append(this.openButton,this.currentButton,this.panelSwitch,this.badge);
    this.links=make('div','','guided-setup-shortcuts');this.links.hidden=true;
    for(const [id,label] of groups)this.links.append(this.button(label,()=>{this.tab=id;return this.open('saved');}));
    this.card.append(this.links);
    this.resourceLink=make('section','','guided-setup-resource-link');this.resourceLink.hidden=true;
    this.resourceLink.append(make('h3','Applied complete setup'),make('p','Resource limits and coupling effort come from the reviewed setup, not from the separate panel defaults.'),this.button('Edit applied resources…',()=>{this.tab='resources';return this.open('saved');}));
    document.querySelector('#runPanel_resources').prepend(this.resourceLink);
    document.querySelector('#runPanel_setup').prepend(this.card);
    this.dialog=make('dialog','','guided-setup-dialog');this.dialog.id='guidedSetupDialog';this.dialog.setAttribute('aria-labelledby','guidedSetupTitle');
    const header=make('header');const title=make('h2','Complete run setup');title.id='guidedSetupTitle';
    this.closeButton=this.button('Close',()=>this.close(),'guidedSetupClose');header.append(title,this.closeButton);
    const description=make('p','One service-owned problem and request. Review is non-mutating. Apply replaces both saved records only after the revision check. It never grants physical preflight or final acceptance.','guided-setup-help');
    this.name=make('input');this.name.maxLength=160;this.name.setAttribute('aria-label','Complete setup name');
    this.search=make('input');this.search.type='search';this.search.placeholder='Find a setting in this section…';this.search.setAttribute('aria-label','Search complete setup fields');this.search.oninput=()=>this.render();
    this.tabs=make('div','','guided-setup-tabs');this.tabs.setAttribute('role','tablist');
    for(const [id,label] of groups){const b=this.button(label,()=>{this.tab=id;this.search.value='';this.render();});b.dataset.setupTab=id;b.id='guidedTab_'+id;b.setAttribute('role','tab');b.setAttribute('aria-controls','guidedSetupFields');this.tabs.append(b);}
    this.tabs.addEventListener('keydown',event=>{if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;const tabs=[...this.tabs.children],i=tabs.indexOf(event.target);if(i<0)return;event.preventDefault();const next=event.key==='Home'?0:event.key==='End'?tabs.length-1:(i+(event.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length;tabs[next].click();tabs[next].focus();});
    this.fields=make('div','','guided-setup-fields');this.fields.id='guidedSetupFields';this.fields.setAttribute('role','tabpanel');
    const raw=make('details','','guided-setup-exact');this.raw=raw;raw.append(make('summary','Advanced: exact request and extension fields'));
    this.json=make('textarea');this.json.rows=10;this.json.spellcheck=false;this.json.setAttribute('aria-label','Complete setup JSON');
    this.json.oninput=()=>{this.rawDirty=true;this.changed();};raw.append(this.json,this.button('Load JSON into guided fields',()=>{const v=window.ImplexityAdvancedCommandCore.parsePayload(this.json.value);delete v.applied_setup;this.draft=v;this.invalidFields.clear();this.rawDirty=false;this.changed();this.render();}));
    this.diff=make('section','','guided-setup-differences');this.diff.id='guidedSetupDiff';this.diff.hidden=true;
    this.status=make('p','','guided-setup-status');this.status.id='guidedSetupStatus';this.status.setAttribute('role','status');this.status.setAttribute('aria-live','polite');
    const footer=make('footer');this.reviewButton=this.button('Review changes',()=>this.reviewChanges(),'guidedSetupReview');this.applyButton=this.button('Apply reviewed setup',()=>this.applyReview(),'guidedSetupApply');
    this.recoverButton=this.button('Refresh saved state',()=>this.recover(),'guidedSetupRecover');
    this.panelsButton=this.button('Use individual panels instead',()=>this.usePanels(),'guidedSetupUsePanels');
    footer.append(this.reviewButton,this.applyButton,this.recoverButton,this.panelsButton);
    this.dialog.append(header,description,this.name,this.tabs,this.search,this.fields,raw,this.diff,this.status,footer);document.body.append(this.dialog);
    this.dialog.addEventListener('cancel',e=>{e.preventDefault();if(!this.busy)this.close();});
    this.name.addEventListener('input',()=>this.changed());this.update();
  }
  update() {
    this.applyButton.disabled=this.busy||!this.review||this.unconfirmed||this.invalidFields.size>0;
    this.reviewButton.disabled=this.busy||this.unconfirmed||!this.draft;
    this.closeButton.disabled=this.busy;this.recoverButton.disabled=this.busy;this.panelsButton.disabled=this.busy||this.unconfirmed;
    for(const input of this.fields.querySelectorAll('input,select,textarea,button'))input.disabled=this.busy||input.dataset.readOnly==='true';
    this.name.disabled=this.busy;this.json.disabled=this.busy;
     
    const active=!!this.active;
    this.badge.textContent=this.unconfirmed?'Outcome unconfirmed. Refresh the saved state before continuing.':this.stale?'Saved settings are out of date. Review them before running.':active?`Complete setup revision ${this.active.revision} is the run source. Panel drafts are inactive.`:'Panel settings are active.';
    this.currentButton.disabled=this.busy||this.unconfirmed||active;
    this.panelSwitch.hidden=!active;this.panelSwitch.disabled=this.busy||this.unconfirmed;
    this.links.hidden=!active;this.resourceLink.hidden=!active;
    for(const section of document.querySelectorAll('#runPanel_setup > .run-section, #runPanel_resources > :not(.guided-setup-resource-link)')) {
       
      if(active){if(!own(section.dataset,'guidedPreviousHidden'))section.dataset.guidedPreviousHidden=String(section.hidden);section.hidden=true;section.inert=true;}
      else if(own(section.dataset,'guidedPreviousHidden')){section.hidden=section.dataset.guidedPreviousHidden==='true';delete section.dataset.guidedPreviousHidden;section.inert=false;}
    }
     
    for(const selector of ['#implexity-physics-editor','.implexity-objective-panel','.implexity-design-freedom-card']){
      for(const section of document.querySelectorAll(selector)){section.inert=active;section.classList.toggle('guided-setup-readonly-summary',active);}
    }
    for(const [selector,tab] of [['#implexity-physics-editor','physics'],['.implexity-objective-panel','responses']]){
      const section=document.querySelector(selector);if(!section)continue;
      let notice=section.previousElementSibling;
      if(!notice?.classList.contains('guided-setup-panel-note')){notice=make('aside','','guided-setup-panel-note');notice.append(make('p','These values mirror the applied complete setup. Use its guided editor to change them.'),this.button('Edit applied '+(tab==='physics'?'physics':'objectives and limits')+'…',()=>{this.tab=tab;return this.open('saved');}));section.before(notice);}
      notice.hidden=!active;
    }
    if(!window.implexityOptimizationBlocksNewRun?.()){
      const blocked=this.unconfirmed||this.dirty||this.stale;
      for(const id of ['pfbtn','optstart']){const b=document.getElementById(id);if(b&&blocked)b.disabled=true;}
    }
  }
  changed() {this.dirty=true;this.review=null;this.diff.hidden=true;this.fields.hidden=false;this.search.hidden=false;this.raw.hidden=false;window.implexityInvalidateOptimizationPreflight?.('Complete setup has unapplied changes.');this.update();}
  async open(mode='saved') {
    if(!authoringAvailable(window.implexityOptimizationBlocksNewRun?.(),window.OPT?.status))throw new Error('Pause and acquire a manual intervention, or finish the active run, before replacing its complete setup.');
    if(this.busy)return;
    if(this.dirty && this.dialog.open){this.message('Your draft is retained. Review it or refresh explicitly.');return;}
    const state=await action('inspect_guided_setup');
    let request=mode==='panels'?window.implexityCurrentOptimizationRequest({report:true}):state.record?.request;
    if(!request) request=window.implexityCurrentOptimizationRequest({report:true});
    this.snapshot=state;this.draft=clone(request);delete this.draft.applied_setup;this.invalidFields.clear();
    this.name.value=state.record?.label||'Complete design run';this.rawDirty=false;this.dirty=false;this.review=null;
    if(!this.dialog.open){this.returnFocus=document.activeElement;this.dialog.showModal();}
    this.message(state.stale?`Saved binding differs in ${state.changed_binding_fields.join(', ')}. Inspect geometry-dependent fields before review.`:'Loaded the complete request. Nothing has been applied.');
    this.render();this.update();
  }
  close() {
    if(this.busy)return;
    if(this.dirty&&!window.confirm('Discard this unapplied complete-setup draft? The saved setup is unchanged.'))return;
    this.dirty=false;this.review=null;this.invalidFields.clear();this.dialog.close();window.updateOptButtons?.();this.update();this.returnFocus?.focus?.();
  }
  isAlias(path) {
    if(path[0]!=='physics'||path[1]!=='problem')return false;
    const entry=window.implexityWorkbench?.providerEntries().find(p=>(p.id||p.name)===this.draft.physics?.provider);
    const relative=path.slice(2);
    return (entry?.editor?.authoring_aliases||[]).some(a=>Array.isArray(a.path)&&a.path.every((key,i)=>relative[i]===key)&&(()=>{try{get(this.draft.physics.problem,a.source);return true;}catch(_){return false;}})());
  }
  isReadOnly(path) {
    if(['provider','physics_generation','seq','channel'].includes(String(path.at(-1)))&&path.length<3)return true;
    if(path[0]!=='physics'||path[1]!=='problem')return false;
    const id=this.draft.physics?.provider,w=window.implexityWorkbench;
    const entry=w?.providerEntries().find(p=>(p.id||p.name)===id);
    const fields=entry?.editor?.authoring_fields;
    const relative=path.slice(2), aliases=entry?.editor?.authoring_aliases||[];
    if(aliases.some(a=>a.path.every((key,i)=>relative[i]===key)&&(()=>{try{get(this.draft.physics.problem,a.source);return true;}catch(_){return false;}})()))return true;
    if(Array.isArray(fields)&&path.length>2&&!fields.includes(path[2]))return true;
    return /(^|_)(fingerprint|signature|seal|sha256)$/.test(String(path.at(-1)));
  }
  field(parent,path,value) {
    const readonly=this.isReadOnly(path),label=make('label','','guided-setup-field');label.dataset.setupField=pathText(path);
    const caption=make('span',human(path.at(-1)));const small=make('small',pathText(path));
    let input;
    const key=String(path.at(-1));const options=key==='sense'?['minimise','maximise','upper','lower','equal','target']:null;
    if(typeof value==='boolean'||options){input=make('select');const choices=options||[true,false];if(!choices.includes(value))choices.unshift(value);for(const v of choices)input.append(new Option(v===true?'Yes':v===false?'No':String(v),String(v)));input.value=String(value);}
    else{input=make('input');input.type=typeof value==='number'?'number':'text';if(input.type==='number')input.step='any';input.value=value===null?'':String(value);if(value===null)input.placeholder='Not set (or enter a JSON value)';}
    input.setAttribute('aria-label',pathText(path));input.dataset.setupPath=JSON.stringify(path);input.dataset.readOnly=String(readonly);input.disabled=readonly;
    input.addEventListener('input',()=>{this.changed();try{put(this.draft,path,scalar(input.value,value));input.setCustomValidity('');this.invalidFields.delete(JSON.stringify(path));this.json.value=JSON.stringify(this.draft,null,2);}catch(e){input.setCustomValidity(e.message);this.invalidFields.set(JSON.stringify(path),pathText(path));this.message(`${pathText(path)}: ${e.message}`,true);}this.update();});
    label.append(caption,input,small);parent.append(label);
  }
  tree(parent,path,value,depth=0) {
    if(this.isAlias(path))return;
    if(value===null||typeof value!=='object'){this.field(parent,path,value);return;}
    const wrapper=make('details','','guided-setup-group');wrapper.open=depth<1||!!this.search.value;
    const entries=Array.isArray(value)?value.map((v,i)=>[i,v]):Object.entries(value);
    wrapper.append(make('summary',`${human(path.at(-1))} · ${entries.length} ${Array.isArray(value)?'entries':'fields'}`));parent.append(wrapper);
    if(this.isReadOnly(path)) {wrapper.open=false;wrapper.append(make('p','Provider-generated declaration. Preserved exactly and rebuilt only by the provider during review.'));const pre=make('pre',JSON.stringify(value,null,2));wrapper.append(pre);return;}
    const numeric=Array.isArray(value)&&value.length&&leaves(value).every(x=>typeof x.value==='number');
    if(numeric&&leaves(value).length>64){
      const all=leaves(value,path);wrapper.append(make('p',`${all.length} numeric entries retained. Edit indexed entries in pages; shape is unchanged.`));
      let offset=0;const box=make('div'),status=make('span');const draw=()=>{box.replaceChildren();status.textContent=`${offset+1}–${Math.min(offset+32,all.length)} of ${all.length}`;for(const item of all.slice(offset,offset+32))this.field(box,item.path,get(this.draft,item.path));};
      wrapper.append(this.button('Previous entries',()=>{offset=Math.max(0,offset-32);draw();}),status,this.button('Next entries',()=>{offset=Math.min(Math.floor((all.length-1)/32)*32,offset+32);draw();}),box);draw();return;
    }
    let visible=0;
    for(const [key,child] of entries){
      const childPath=[...path,key];const filter=this.search.value.toLowerCase().trim();
      if(filter&&!(`${pathText(childPath)} ${JSON.stringify(child)}`).toLowerCase().includes(filter))continue;
      visible++;this.tree(wrapper,childPath,child,depth+1);
    }
    if(!visible)wrapper.append(make('p',this.search.value?'No matching fields.':'Empty. Add a field through the exact request editor if the provider supports it.'));
    if(Array.isArray(value)&&path.length===1&&['responses','schedule','design_coordinates','operating_points'].includes(path[0])){
      wrapper.append(this.button('Duplicate last entry',()=>{if(!value.length)throw new Error('Add the first entry through its provider editor or exact request.');value.push(clone(value.at(-1)));this.changed();this.render();}),this.button('Remove last entry',()=>{if(!value.length)return;value.pop();this.changed();this.render();}));
    }
  }
  render() {
    if(!this.draft)return;
    this.fields.hidden=false;this.search.hidden=false;this.raw.hidden=false;this.diff.hidden=true;
    for(const b of this.tabs.children){const on=b.dataset.setupTab===this.tab;b.setAttribute('aria-selected',String(on));b.tabIndex=on?0:-1;}
    this.fields.setAttribute('aria-labelledby','guidedTab_'+this.tab);this.fields.replaceChildren();
    if(this.tab==='physics'){
      const latest=make('section','','guided-setup-current-physics');latest.append(make('p','After separate physics or geometry-binding edits, load the current service problem into this draft. Other run settings remain unchanged; Review and Apply are still required.'),this.button('Use current applied physics',()=>{this.draft=currentPhysicsDraft(this.draft,this.snapshot);this.changed();this.render();this.message('The current service physics from this review baseline is in the draft. No saved record changed. Review before applying.');},'guidedSetupCurrentPhysics'));this.fields.append(latest);
    }
    for(const key of groupKeys(this.draft,this.tab))this.tree(this.fields,[key],this.draft[key]);
    if(!this.fields.children.length)this.fields.append(make('p','No explicit settings in this section. Provider defaults apply. Optional declarations can be added through the exact request editor.'));
    if(!this.rawDirty)this.json.value=JSON.stringify(this.draft,null,2);
    this.update();
  }
  async reviewChanges() {
    if(this.rawDirty)throw new Error('Load the edited JSON into the guided fields before review.');
    if(this.invalidFields.size)throw new Error('Correct invalid settings before review: '+[...this.invalidFields.values()].join(', '));
    const invalid=[...this.fields.querySelectorAll('input')].find(n=>!n.checkValidity());if(invalid){invalid.reportValidity();throw new Error('Correct the highlighted setting before review.');}
    this.busy=true;this.update();
    try {
      this.review=await action('review_guided_setup',{label:this.name.value,request:clone(this.draft),expected_revision:this.snapshot.revision,expected_binding:clone(this.snapshot.current_binding)});
      this.diff.replaceChildren(make('h3','Review before applying'));
      for(const [label,diff] of [['Saved request changes',this.review.diff],['Provider normalization changes',this.review.normalization_diff]]){
        this.diff.append(make('h4',label));
        if(!diff.changes.length)this.diff.append(make('p','No changes.'));
        for(const row of diff.changes){const r=make('div','','guided-setup-diff-row');r.append(make('strong',row.path),make('span',row.operation));r.append(make('pre',`${own(row,'before')?JSON.stringify(row.before):'(absent)'}  →  ${own(row,'after')?JSON.stringify(row.after):'(removed)'}`));this.diff.append(r);}
        if(diff.possibly_truncated)this.diff.append(make('p','The display is capped. Inspect the complete normalized request below.'));
      }
      const exact=make('details');exact.append(make('summary','Complete reviewed request'),make('pre',JSON.stringify(this.review.candidate_request,null,2)));this.diff.append(exact);this.diff.hidden=false;this.fields.hidden=true;this.search.hidden=true;this.raw.hidden=true;this.diff.scrollTop=0;
      this.message(`Review ready. ${this.review.physics_changed?'Physics will be revised and replanned.':'Physics remains unchanged.'} Apply saves both records. Fresh physical preflight is still required.`);
    } catch(e) {this.review=null;throw e;} finally {this.busy=false;this.update();}
  }
  async applyReview() {
    if(!this.review)throw new Error('Review changes first.');
    this.busy=true;this.update();
    try {
      const result=await action('apply_guided_setup',{review_id:this.review.review_id,request_sha256:this.review.request_sha256});
       
      this.snapshot=result;this.draft=clone(result.record.request);this.dirty=false;this.review=null;this.rawDirty=false;this.active=clone(result.record);this.stale=false;
      try{this.adopt(result);this.message(`Applied complete setup revision ${result.revision}. Close this editor, then Check current setup and Review & start.`);}
      catch(e){this.unconfirmed=true;this.message('Setup was applied, but display synchronization failed. Refresh saved state before continuing. '+e.message,true);}
      this.diff.hidden=true;this.render();
    } catch(e) {if(!e.refused){this.unconfirmed=true;this.message('Apply outcome is unconfirmed. Do not repeat it. Refresh the saved state to reconcile. '+e.message,true);}else this.message(e.message,true);}
    finally{this.busy=false;this.update();}
  }
  adopt(state) {
    if(!state.applied_current)throw new Error('Saved setup is stale or has not been applied.');
    this.panelMode=false;this.active=clone(state.record);this.stale=false;
    const w=window.implexityWorkbench,p=state.problem_record;
    w._selectedProvider=p.provider;w.latestPhysicsPlan=null;w.providerProblems[p.provider]=clone(p.problem);w.providerProblemRecords[p.provider]=clone(p);w._physicsDrafts.clear();
    w.syncObjectiveCatalogue();
    const program={schema:'implexity-response-program/2',normalisation:'response_scale',provider_id:p.provider,objectives:[],constraints:[]};
    for(const r of state.record.request.responses||[]){
      if(['upper','lower','equal','target','<=','>=','='].includes(r.sense))program.constraints.push({response_id:r.name||r.response||r.response_id,relation:({upper:'<=',lower:'>=',equal:'=',target:'='})[r.sense]||r.sense,bound:r.target??r.bound,weight:r.weight??1,scale:r.scale??1});
      else program.objectives.push({response_id:r.name||r.response||r.response_id,sense:({minimise:'minimize',maximise:'maximize'})[r.sense]||r.sense,weight:r.weight??1,scale:r.scale??1,target:r.target??null});
    }
     
    window.ImplexityObjectiveComposer?.setProgram?.(clone(program));
    w.appliedResponseProgram=clone(program);w._responseProgramDirty=false;
    const provider=w.providerEntries().find(entry=>(entry.id||entry.name)===p.provider);
    if(provider?.editor?.kind==='native_json')w.openNativeJsonEditor(provider);
    const request=state.record.request,settings=request.settings||{};
    const set=(id,value)=>{const el=document.getElementById(id);if(el&&value!=null)el.value=String(value);};
    set('optiters',settings.iterations??settings.iters??request.iters??request.iterations);
    set('optlr',settings.step_fraction??request.lr??request.step_fraction);
    set('runLiveEvery',settings.live_every??request.live_every??1);
     
    const run=window.ImplexityRunConfiguration;
    for(const key of run?.searchSettings||[])set(run.controls[key],settings[key]??request[key]??run.defaults[key]);
    window.implexityInvalidateOptimizationPreflight?.('A complete setup was applied. Fresh model-aware preflight is required.');
    w.renderProviders();w.renderReadiness();w.renderStatus();window.ImplexityRunWorkspace?.sync();
    window.dispatchEvent(new CustomEvent('implexity:guided-setup-applied',{detail:{revision:state.revision}}));
  }
  requestForRun(seq) {
    if(this.unconfirmed)throw new Error('Refresh saved setup state before running. The last apply outcome is unconfirmed.');
    if(this.dirty)throw new Error('Apply or discard the complete setup draft before preflight or optimization.');
    if(!this.active)return null;
    if(window.implexityWorkbench?._physicsDrafts?.size||window.implexityWorkbench?._responseProgramDirty)throw new Error('Individual physics or response panels contain another draft. Discard that draft or explicitly switch request sources.');
    if(this.stale)throw new Error('The applied complete setup is stale. Review it against current geometry and physics.');
    const model=window.implexityWorkbench?.model||window.S?.model;
    if(model?.content_id!==this.active.binding.content_id||model?.structure_id!==this.active.binding.structure_id)throw new Error('Geometry changed after setup application. Reopen and review the complete setup.');
    return launchRequest({applied_current:true,record:this.active},seq);
  }
  async poll() {
    const workbench=window.implexityWorkbench;
    if(this.busy||this.polling||this.dirty||this.unconfirmed||this.panelMode||!workbench?.model?.content_id||!authoringAvailable(window.implexityOptimizationBlocksNewRun?.(),window.OPT?.status))return;
    this.polling=true;
    try {const state=await action('inspect_guided_setup');
      if(!this.active){
         
        if(state.applied_current&&!this.dialog.open&&!workbench._physicsDrafts?.size&&!workbench._responseProgramDirty){this.adopt(state);this.update();}
      }else {
        const staleNow=bindingStale(state,this.active);
        if(staleNow!==this.stale){
          this.stale=staleNow;
          window.implexityInvalidateOptimizationPreflight?.(staleNow?'Another client changed the complete setup or its binding.':'The exact applied binding is restored. Fresh preflight is still required.');
          workbench.renderReadiness?.();window.ImplexityRunWorkspace?.sync();this.update();
        }
      }
    }
    catch(_){ }
    finally{this.polling=false;}
  }
  async recover() {
    if(this.busy)return;
    if(this.dirty&&!window.confirm('Replace your unapplied draft with the currently saved request?'))return;
    this.busy=true;this.update();
    try {const state=await action('inspect_guided_setup');this.snapshot=state;this.review=null;this.dirty=false;this.rawDirty=false;this.invalidFields.clear();
      if(state.record){this.draft=clone(state.record.request);this.name.value=state.record.label;}
      if(state.applied_current){this.adopt(state);this.message('Read-only refresh recovered the applied setup. No mutation was repeated.');}
      else{this.active=state.record?clone(state.record):null;this.stale=!!state.record;this.panelMode=false;this.message('Saved request is not an applied current setup. Review before applying it. Individual panels have not replaced this saved request.');}
      this.unconfirmed=false;this.render();
    } finally{this.busy=false;this.update();}
  }
  usePanels() {
    if(this.unconfirmed)throw new Error('Resolve the uncertain outcome first.');
    if(!window.confirm('Use the individual panel configuration for the next request? The complete saved request is retained, but any settings not represented in those panels are not applied.'))return;
    this.panelMode=true;this.active=null;this.stale=false;this.dirty=false;this.review=null;this.invalidFields.clear();this.dialog.close();window.implexityInvalidateOptimizationPreflight?.('Request source changed to individual panels.');this.update();
  }
}
function boot(){if(!window.ImplexityRunWorkspace||!document.querySelector('#runPanel_setup')){setTimeout(boot,60);return;}window.ImplexityGuidedSetup=new GuidedSetup();}
if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot,{once:true});else boot();
})();
