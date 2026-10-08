// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(() => {
  'use strict';
  const $ = (selector, root=document) => root.querySelector(selector);
  const $$ = (selector, root=document) => Array.from(root.querySelectorAll(selector));
  const make = (tag, className='', text='') => {
    const element=document.createElement(tag);element.className=className;element.textContent=text;return element;
  };
  const text = (element, value) => {if(element && element.textContent!==String(value))element.textContent=String(value);};
  const clone = value => structuredClone(value);
  const defaults=Object.freeze({step_policy:'fixed_step',live_every:1,move_limit:.1,minimum_step_fraction:1e-5,backtracking:.5,armijo:1e-4,step_growth:2,stationarity_tolerance:1e-3,bound_tolerance:1e-3,penalty_growth:10,penalty_limit:1e6,violation_reduction:.25,multiplier_update_interval:10,metric:'gradient_adaptive',alpha:.5});
  const controls={step_policy:'runStepPolicy',live_every:'runLiveEvery',move_limit:'runMoveLimit',minimum_step_fraction:'runMinimumStep',backtracking:'runBacktracking',armijo:'runArmijo',step_growth:'runStepGrowth',stationarity_tolerance:'runStationarityTolerance',bound_tolerance:'runBoundTolerance',penalty_growth:'runPenaltyGrowth',penalty_limit:'runPenaltyLimit',violation_reduction:'runViolationReduction',multiplier_update_interval:'runMultiplierInterval',metric:'runUpdateMetric',alpha:'runMetricAlpha'};
   
  const searchSettings=Object.freeze(['step_policy','move_limit','minimum_step_fraction','backtracking','armijo','step_growth','stationarity_tolerance','bound_tolerance','penalty_growth','penalty_limit','violation_reduction','multiplier_update_interval']);
   
  const retiredSettings=Object.freeze({gradient_clip_norm:'gradient_clip_norm no longer exists: the projected search normalises its direction and its trust step, bounded by the move limit, sets every update. Remove it or set it to null.'});
   
  const removedConstraintKeys=Object.freeze(['constraint_search','design_feasibility','volume_fraction_upper','volume_fraction','bounded_mean_upper']);
  const removedConstraintMessage=key=>`${key} belonged to the removed hard-constraint machinery. Response bounds are penalty terms only; remove ${key} and express the limit as a response bound.`;
  const normalProvider = request => request?.provider || request?.physics?.provider || 'legacy_multiphysics_implicit';
  const canonical = value => JSON.stringify(value, (_key,item) => item && typeof item==='object'&&!Array.isArray(item)?Object.fromEntries(Object.keys(item).sort().map(key=>[key,item[key]])):item);

  function parseValues(raw, stepFraction) {
    const result={};
    for(const key of Object.keys(defaults)){
      if(key==='step_policy'){result.step_policy=raw.step_policy;if(!['fixed_step','backtracking'].includes(result.step_policy))throw new Error('Choose fixed-step or backtracking updates.');continue;}
      if(key==='metric'){result.metric=raw.metric;if(!['gradient_adaptive','global_max','family_balanced'].includes(result.metric))throw new Error('Choose a supported update-scaling method.');continue;}
      const value=typeof raw[key]==='number'?raw[key]:String(raw[key]??'').trim()===''?NaN:Number(raw[key]);
      if(typeof raw[key]==='boolean'||!Number.isFinite(value))throw new Error(`${key.replaceAll('_',' ')} requires a finite number.`);
      result[key]=value;
    }
    if(!Number.isSafeInteger(result.live_every)||result.live_every<1)throw new Error('Live preview cadence must be a positive whole number.');
    if(!(result.move_limit>0&&result.move_limit<=1))throw new Error('Move limit must lie in (0, 1] of the normalized coordinate range.');
    if(result.step_policy==='backtracking'&&(!(result.minimum_step_fraction>0&&result.minimum_step_fraction<=stepFraction)))throw new Error('Minimum trial step must be positive and no larger than the initial step.');
    if(result.step_policy==='backtracking'&&(!(result.backtracking>0&&result.backtracking<1)))throw new Error('Backtracking factor must lie strictly between 0 and 1.');
    if(result.step_policy==='backtracking'&&(!(result.armijo>=0&&result.armijo<1)))throw new Error('Armijo coefficient must lie in [0, 1).');
    if(!(result.alpha>=0&&result.alpha<=1))throw new Error('Adaptive scaling exponent must lie in [0, 1].');
    if(result.step_policy==='backtracking'&&(result.minimum_step_fraction>result.move_limit))throw new Error('Minimum trial step must not exceed the move limit.');
    if(result.step_policy==='backtracking'&&(!(result.step_growth>=1)))throw new Error('Trust-step growth must be at least 1.');
    if(!(result.stationarity_tolerance>0&&result.stationarity_tolerance<1))throw new Error('Stationarity tolerance must lie strictly between 0 and 1.');
    if(!(result.bound_tolerance>0))throw new Error('Bound tolerance must be positive.');
    if(!(result.penalty_growth>1))throw new Error('Penalty growth must be greater than 1.');
    if(!(result.penalty_limit>=1))throw new Error('Penalty limit must be at least 1.');
    if(!(result.violation_reduction>0&&result.violation_reduction<1))throw new Error('Required violation reduction must lie strictly between 0 and 1.');
    if(!Number.isSafeInteger(result.multiplier_update_interval)||result.multiplier_update_interval<1)throw new Error('Multiplier update interval must be a positive whole number.');
    return result;
  }

  function augment(request, values, hierarchyConfigured=false){
    if(normalProvider(request)==='legacy_multiphysics_implicit')return request;
    const settings={...(request.settings||{})};
    for(const key of removedConstraintKeys)if(Object.hasOwn(settings,key)||Object.hasOwn(request,key))throw new Error(removedConstraintMessage(key));
    for(const [key,message] of Object.entries(retiredSettings)){
      for(const owner of [settings,request])if(Object.hasOwn(owner,key)&&owner[key]!==null)throw new Error(message);
      delete settings[key];
    }
    for(const key of searchSettings){
      if(Object.hasOwn(settings,key)||Object.hasOwn(request,key))throw new Error(`Run setting ${key} is already declared by another setup owner.`);
      settings[key]=values[key];
    }
    if(Object.hasOwn(settings,'live_every'))throw new Error('Live cadence is already declared by another setup owner.');
    const result={...request,settings,live_every:values.live_every};
    for(const key of Object.keys(retiredSettings))delete result[key];
     
     
    delete result.grid;
    if(!hierarchyConfigured)result.update_metric={mode:values.metric,...(values.metric==='gradient_adaptive'?{alpha:values.alpha}:{})};
    return result;
  }

  function describeRequest(request,legacyTerms='the active provider default'){
    const native=normalProvider(request)!=='legacy_multiphysics_implicit';
    const schedule=Array.isArray(request.schedule)?request.schedule:[];
    const iterations=schedule.length?schedule.reduce((sum,row)=>sum+Number(row.iterations),0):Number(request.iters);
    const budget=`${iterations} requested update${iterations===1?'':'s'}${schedule.length?` across ${schedule.length} explicitly configured stages`:''}`;
    const location=native?'Spatial resolution: selected provider and authored geometry.':`Grid ${request.grid}.`;
    const responses=native?(request.responses||[]).map(row=>`${row.response}: ${row.sense}${row.target===undefined||row.target===null?'':` ${row.target}`} (weight ${row.weight??1}, scale ${row.scale??1})`).join('; '):legacyTerms;
    const exact=(request.computation_effort?.mode||'exact')==='exact';
    return {title:exact?'Review exact physics optimization':'Review requested approximation and exact-correction policy',
      budget,location,responses,coordinates:(request.design_coordinates||[]).map(row=>row.coordinate).join(', ')||'Authored model parameters',
      step:`${request.settings?.step_policy==='backtracking'?'Backtracking':'Fixed-step direct gradient descent'}, step fraction ${request.lr}${request.settings?.move_limit!==undefined?`, move limit ${request.settings.move_limit}`:''}.`};
  }

  function fieldUpdateSummary(job={},rows=[]){
    const row=rows.length?rows[rows.length-1]:(job.last_row||{});
    const declared=job.summary?.design_coordinates||row.active_coordinates||[];
    const native=declared.includes('model:control')||job.summary?.topology_coordinate==='model:control';
    const updates=Object.entries(row.coordinate_updates||{}).map(([coordinate,value])=>({
      coordinate,max_abs:typeof value?.max_abs==='number'&&Number.isFinite(value.max_abs)?value.max_abs:null,
      l2:typeof value?.l2==='number'&&Number.isFinite(value.l2)?value.l2:null,
      free_entries:Number.isSafeInteger(value?.free_entries)&&value.free_entries>=0?value.free_entries:null
    }));
    return {field:native||updates.length>0,updates,accepted:row.accepted===true,
      scope:'last_published_update_not_cumulative_movement'};
  }

  async function action(name, payload={}){
    const response=await fetch('/v1/agent/action',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({action:name,payload})});
    const result=await response.json();
    if(!response.ok||result.ok===false)throw new Error((result.problems||[result.error||'Service request failed.']).join('\n'));
    return result.result??result;
  }

  class RunWorkspace {
    constructor(workbench){
      this.workbench=workbench;this.lastStatus='';this.saved=null;this.saving=false;this.pending=false;
      this.build();this.bind();this.sync();
      this.timer=setInterval(()=>this.sync(),900);
      window.addEventListener('pagehide',()=>clearInterval(this.timer),{once:true});
    }
    button(label, handler, className=''){
      const button=make('button',className,label);button.type='button';button.onclick=handler;return button;
    }
    section(title, help){
      const section=make('section','run-section');section.append(make('h3','',title));if(help)section.append(make('p','run-help',help));return section;
    }
    field(parent,key,label,help,type='number',options=null){
      const holder=make('label','run-field');holder.htmlFor=controls[key];holder.append(make('span','',label));
      let input;
      if(options){input=make('select');for(const [value,caption] of options){const option=make('option','',caption);option.value=value;input.append(option);}}
      else {input=make('input');input.type=type;if(type==='number')input.step='any';}
      input.id=controls[key];input.value=defaults[key]??'';input.setAttribute('aria-label',label);
      holder.append(input);if(help){const note=make('small','run-help',help);note.id=input.id+'Help';input.setAttribute('aria-describedby',note.id);holder.append(note);}
      parent.append(holder);return input;
    }
    build(){
      const host=$('#s_opt > .sect-b');if(!host)throw new Error('Optimization workspace host unavailable.');
      this.root=make('section','run-workspace');this.root.id='implexityRunWorkspace';this.root.setAttribute('aria-label','Physics optimization workspace');
      this.overview=this.section('Physics-driven design','');
      this.facts=make('div','run-facts');this.overview.append(this.facts);
      for(const [id,label,step] of [['geometry','Geometry','geometry'],['physics','Physics','physics'],['goals','Goals','objectives']]){
        const item=this.button('',()=>this.workbench.setStep(step),'run-fact');item.dataset.runFact=id;
        item.append(make('strong','',label),make('span','run-fact-state','Not configured'));this.facts.append(item);
      }
      this.draftNotice=make('div','run-draft-notice');this.draftNotice.setAttribute('role','status');this.overview.append(this.draftNotice);
      this.tabs=make('div','run-tabs');this.tabs.setAttribute('role','tablist');this.tabs.setAttribute('aria-label','Optimization workspace pages');this.panels={};
      for(const [id,label] of [['setup','Run setup'],['resources','Resources'],['monitor','Monitor']]){
        const tab=this.button(label,()=>this.selectTab(id),'run-tab');tab.id='runTab_'+id;tab.dataset.runTab=id;
        tab.setAttribute('role','tab');tab.setAttribute('aria-controls','runPanel_'+id);this.tabs.append(tab);
        const panel=make('div','run-panel');panel.id='runPanel_'+id;panel.setAttribute('role','tabpanel');panel.setAttribute('aria-labelledby',tab.id);this.panels[id]=panel;
      }
      this.root.append(this.overview,this.tabs,...Object.values(this.panels));host.prepend(this.root);
      const setup=this.panels.setup;
      const steps=this.section('Design steps','Step sizes use the normalized design range.');
      const old=$('.opt-settings');if(old)steps.append(old);setup.append(steps);
      const rate=$('label[for="optlr"] span');text(rate,'Initial step fraction');$('#optlr')?.setAttribute('aria-label','Initial step fraction');
      this.gridNote=make('p','run-help');steps.append(this.gridNote);this.scopeHost=steps;
      this.nativeControls=make('div','run-native-controls');steps.append(this.nativeControls);
      const basic=make('div','run-field-grid');this.nativeControls.append(basic);
      this.field(basic,'step_policy','Step policy','Fixed-step direct gradient descent evaluates one candidate per epoch.','select',[['fixed_step','Fixed-step direct gradient descent'],['backtracking','Backtracking sufficient-decrease search']]);
      this.field(basic,'move_limit','Per-epoch move limit','Upper bound of the carried trust step, as a fraction of each coordinate span.');
      this.field(basic,'live_every','Publish every N epochs','Display cadence only. Does not change the physical time step.').step='1';
      this.field(basic,'metric','Update scaling','Hierarchy-specific settings take precedence when explicitly configured.','select',[
        ['gradient_adaptive','Gradient adaptive'],['global_max','Global maximum'],['family_balanced','Balanced coordinate families']]);
      this.field(basic,'alpha','Adaptive scaling exponent','0–1. Used only by gradient-adaptive scaling.');
      const advanced=make('details','run-advanced');advanced.append(make('summary','','Line search and trust step'));
      const fields=make('div','run-field-grid');advanced.append(fields);this.nativeControls.append(advanced);
      this.field(fields,'minimum_step_fraction','Minimum trial step','Backtracking stops below this step. Does not change field-solver tolerance.');
      this.field(fields,'backtracking','Backtracking factor','Multiply a rejected trial step by this factor.');
      this.field(fields,'armijo','Sufficient-decrease coefficient','Armijo coefficient along the actual projected step.');
      this.field(fields,'step_growth','Trust-step growth','The trust step grows by this factor after an accepted step, up to the move limit.');
      const convergence=make('details','run-advanced');convergence.append(make('summary','','Convergence and response bounds'));
      const convergenceFields=make('div','run-field-grid');convergence.append(convergenceFields);this.nativeControls.append(convergence);
      this.field(convergenceFields,'stationarity_tolerance','Stationarity tolerance','Projected-gradient norm relative to its first value at which a stage is stationary.');
      this.field(convergenceFields,'bound_tolerance','Bound tolerance','Admissible scaled bound violation (value ÷ scale) for a converged run.');
      this.field(convergenceFields,'penalty_growth','Penalty growth','A bound penalty grows by this factor when its violation does not shrink enough.');
      this.field(convergenceFields,'penalty_limit','Penalty limit','No bound penalty exceeds this multiple of its initial value 2 × weight.');
      this.field(convergenceFields,'violation_reduction','Required violation reduction','Penalties grow unless the bound violation fell below this factor times its previous value.');
      this.field(convergenceFields,'multiplier_update_interval','Multiplier update interval','Bound multipliers are also updated after this many accepted steps.').step='1';
      void this.applySettingsSchema();
      this.validation=make('p','run-validation');this.validation.setAttribute('role','status');steps.append(this.validation);
      const designButtons=make('div','run-inline-actions');designButtons.append(this.button('Editable coordinates & stages',()=>{this.workbench.setStep('optimize');window.ImplexityDesignFreedom?.open?.();}),this.button('Review loads and supports',()=>this.openPhysics()));steps.append(designButtons);
      this.epoch=this.section('Retained epoch fields','Keeps selected endpoint fields and computed sensitivities for published epochs without an extra solve. Nothing is retained when both field selection and sensitivity retention are disabled.');
      this.epochList=make('div','run-epoch-fields');this.epochList.id='runEpochFields';this.epochList.setAttribute('role','group');this.epochList.setAttribute('aria-label','Fields to retain per epoch');
      const epochGrid=make('div','run-field-grid');
      const epochInput=(label,help,attrs)=>{const holder=make('label','run-field');holder.append(make('span','',label));const input=make('input');Object.assign(input,attrs);input.setAttribute('aria-label',label);holder.append(input);if(help)holder.append(make('small','run-help',help));epochGrid.append(holder);return input;};
      this.epochExtra=epochInput('Additional field names','Comma-separated names the provider evaluation returns but does not list.',{type:'text',id:'runEpochExtra',value:''});
      this.epochEvery=epochInput('Retain every N epochs','1 retains every published epoch.',{type:'number',id:'runEpochEvery',min:'1',step:'1',value:'1'});
      this.epochBudget=epochInput('Array budget per epoch (MiB)','Leave empty for an automatic budget. Fields are retained on disk; an explicit budget is enforced.',{type:'number',id:'runEpochBudget',min:'1',step:'1',value:'',placeholder:'Automatic'});
      this.epochSensitivities=epochInput('Retain optimization sensitivities','Includes computed response and total gradients.',{type:'checkbox',id:'runEpochSensitivities',checked:true});
      this.epoch.append(this.epochList,epochGrid);setup.append(this.epoch);
      this.epochSignature='';
      this.goals=this.section('Applied objectives and constraints','Response bounds use augmented-Lagrangian penalties and are checked against the specified tolerance.');
      this.goalRows=make('div','run-goal-rows');this.goals.append(this.goalRows,this.button('Edit response goals',()=>this.workbench.setStep('objectives')));setup.append(this.goals);
      const saved=this.section('Saved run setup','Save or restore a reusable run setup.');
      this.savedLabel=make('input');this.savedLabel.type='text';this.savedLabel.maxLength=160;this.savedLabel.value='Current design run';this.savedLabel.setAttribute('aria-label','Saved run setup name');
      this.saveButton=this.button('Save current setup',()=>this.saveSetup());this.refreshButton=this.button('Inspect saved setup',()=>this.inspectSetup());
      this.restoreButton=this.button('Restore saved step controls',()=>this.restoreSteps());this.restoreButton.disabled=true;
      this.editExactButton=this.button('Edit complete saved request…',()=>this.editExactSetup().catch(error=>text(this.savedMessage,error.message)));this.editExactButton.disabled=true;
      this.exportButton=this.button('Export exact request',()=>this.exportRequest());
      const savedActions=make('div','run-inline-actions');savedActions.append(this.saveButton,this.refreshButton,this.restoreButton,this.editExactButton,this.exportButton);
      this.savedMessage=make('p','run-help');this.savedMessage.setAttribute('role','status');
      this.savedDetails=make('details','run-advanced');this.savedDetails.append(make('summary','','Inspect saved request'));this.savedJSON=make('pre','run-request');this.savedDetails.append(this.savedJSON);
      saved.append(this.savedLabel,savedActions,this.savedMessage,this.savedDetails);setup.append(saved);
      const resources=this.panels.resources;const effort=$('#implexityEffortPanel');if(effort)resources.append(effort);
      const monitor=this.panels.monitor;
       
      this.actions=make('div','run-actions');const launch=make('div','run-launch');
      for(const id of ['pfbtn','optstart']){const element=$('#'+id);if(element)launch.append(element);}
      text($('#pfbtn'),'Check current setup');text($('#optstart'),'Review & start…');
      const runtime=make('div','run-runtime-actions');
      for(const id of ['optpause','optresume','optintervene','optbranch','optstop','optaccept','optdiscard']){const element=$('#'+id);if(element)runtime.append(element);}
      this.actions.append(launch,runtime);const help=$('#optActionHelp');if(help)this.actions.append(help);this.root.append(this.actions);
      for(const id of ['optcost','pfout','optpfout','optbar','optmsg','numericalMonitor','publicJobObservation','optNumericalAttention','optcurvewrap','dvlive','acceptbox','s_terms','implexity-numerical-solver-health','implexity-multiphysics-readiness']){
        const element=$('#'+id);if(element)monitor.append(element);
      }
      this.epochViewer=make('details','run-advanced');this.epochViewer.id='runEpochViewer';
      this.epochViewer.open=true;this.epochViewer.append(make('summary','','Live epoch results'));
      const viewerGrid=make('div','run-field-grid');
      const pick=(label,id)=>{const holder=make('label','run-field');holder.append(make('span','',label));const select=make('select');select.id=id;select.setAttribute('aria-label',label);holder.append(select);viewerGrid.append(holder);return select;};
      this.epochSelect=pick('Epoch','runEpochSelect');
      this.epochView=pick('Display','runEpochView');
      for(const [value,label] of [['geometry','Geometry'],['physical','Physical fields'],['sensitivity','Sensitivity fields']]){const option=make('option','',label);option.value=value;this.epochView.append(option);}
      this.epochFieldSelect=pick('Field','runEpochFieldSelect');
      this.epochMetric=pick('Physical metric','runEpochMetric');
      const isoHolder=make('label','run-field');isoHolder.append(make('span','','Isosurface value'));this.epochIso=make('input');this.epochIso.type='number';this.epochIso.step='any';this.epochIso.setAttribute('aria-label','Isosurface value');isoHolder.append(this.epochIso);viewerGrid.append(isoHolder);
      this.epochFollow=make('input');this.epochFollow.type='checkbox';this.epochFollow.checked=true;this.epochFollow.id='runEpochFollow';this.epochFollow.setAttribute('aria-label','Follow latest epoch');
      const followLabel=make('label','run-check');followLabel.append(this.epochFollow,document.createTextNode(' Follow latest epoch'));viewerGrid.append(followLabel);
      const pointHolder=make('label','run-field');pointHolder.append(make('span','','Operating point'));this.epochPoint=make('input');this.epochPoint.type='number';this.epochPoint.min='0';this.epochPoint.step='1';this.epochPoint.value='0';this.epochPoint.id='runEpochPoint';this.epochPoint.setAttribute('aria-label','Operating point');pointHolder.append(this.epochPoint);viewerGrid.append(pointHolder);
      this.epochMessage=make('p','run-help');this.epochMessage.id='runEpochMessage';this.epochMessage.setAttribute('role','status');this.epochMessage.setAttribute('aria-live','polite');
      const epochActions=make('div','run-inline-actions');
      this.epochRefreshButton=this.button('Refresh epochs',()=>this.refreshEpochViewer());
      this.epochShowButton=this.button('Display epoch',()=>this.displayEpoch().catch(error=>text(this.epochMessage,error.message)));this.epochShowButton.id='runEpochShow';
       
      const epochPreset=()=>{const epoch=Number(this.epochSelect.value);return {source:{job_id:window.OPT?.job||'',...(Number.isSafeInteger(epoch)&&this.epochSelect.value!==''?{epoch}:{})}};};
      this.epochSTLButton=this.button('Export epoch as STL…',()=>{const preset=epochPreset();preset.source.kind='optimization_epoch';return window.ImplexityOutputDialogs?.open('export_stl',preset);});this.epochSTLButton.id='runEpochExportSTL';
      this.epochModelButton=this.button('Export epoch as model…',()=>window.ImplexityOutputDialogs?.open('export_optimization_epoch',epochPreset()));this.epochModelButton.id='runEpochExportModel';
      epochActions.append(this.epochRefreshButton,this.epochShowButton,this.epochSTLButton,this.epochModelButton);
      this.epochImage=make('img');this.epochImage.id='runEpochImage';this.epochImage.hidden=true;this.epochImage.style.cssText='width:100%;aspect-ratio:16/9;object-fit:contain;background:#fff';
      this.epochExpandButton=this.button('Expand epoch',()=>this.expandEpoch());this.epochExpandButton.id='runEpochExpand';this.epochExpandButton.disabled=true;epochActions.append(this.epochExpandButton);
      this.epochImage.onclick=()=>this.expandEpoch();this.epochImage.style.cursor='zoom-in';
      this.epochDialog=make('dialog');this.epochDialog.setAttribute('aria-label','Expanded epoch display');this.epochDialog.style.cssText='width:min(94vw,1440px);max-width:94vw;padding:12px;border:1px solid #aaa;border-radius:10px';
      this.epochDialogImage=make('img');this.epochDialogImage.style.cssText='display:block;width:100%;max-height:84vh;object-fit:contain';
      this.epochDialog.append(this.button('Close epoch display',()=>this.epochDialog.close()),this.epochDialogImage);document.body.append(this.epochDialog);
      this.epochDialog.addEventListener('close',()=>this.epochDialogReturnFocus?.focus?.());
      this.epochMetricValue=make('p','run-help');this.epochMetricValue.id='runEpochMetricValue';this.epochMetricValue.setAttribute('aria-live','polite');
      this.epochViewer.append(viewerGrid,epochActions,this.epochMessage,this.epochMetricValue,this.epochImage);
      this.boundMonitor=make('section','run-bound-monitor');this.boundMonitor.id='runBoundMonitor';this.boundMonitor.setAttribute('aria-label','Search convergence and response bounds');
      this.boundMonitor.append(make('h4','','Search convergence and response bounds'));
      this.boundSummary=make('p','run-help');this.boundSummary.id='runBoundSummary';this.boundSummary.setAttribute('role','status');this.boundSummary.setAttribute('aria-live','polite');
      this.boundTable=make('table','run-bound-table');this.boundTable.id='runBoundTable';
      const boundHead=make('thead');const headRow=make('tr');
      for(const label of ['Response','Point','Bound','Value','Scaled violation','Multiplier','Penalty'])headRow.append(make('th','',label));
      boundHead.append(headRow);this.boundBody=make('tbody');this.boundTable.append(boundHead,this.boundBody);
      this.boundMonitor.append(this.boundSummary,this.boundTable);this.boundSignature='';
      monitor.append(this.boundMonitor,this.epochViewer);
      this.epochSelect.onchange=()=>{this.epochFollow.checked=false;this.liveSelectionChanged();};
      this.epochView.onchange=()=>this.liveSelectionChanged();
      this.epochFieldSelect.onchange=()=>this.liveSelectionChanged();
      this.epochPoint.onchange=()=>this.liveSelectionChanged();
      this.epochIso.onchange=()=>this.liveSelectionChanged();
      this.epochFollow.onchange=()=>{this.liveEpochSignature='';this.updateLiveEpochs();};
      this.epochMetric.onchange=()=>this.renderEpochMetric();
      this.epochViewer.addEventListener('toggle',()=>{if(this.epochViewer.open)this.refreshEpochViewer();});
      const detail=make('details','run-advanced');detail.append(make('summary','','Request details and engineering history'));
      for(const id of ['s_spec','s_steer','implexity-engineering-history','implexity-engineering-agent']){const element=$('#'+id);if(element)detail.append(element);}
      monitor.append(detail);
       
       
      const readiness=$('#implexity-readiness');if(readiness){readiness.hidden=true;readiness.inert=true;}
      $$('.grid2',host).filter(element=>!element.children.length).forEach(element=>element.remove());
      this.stageNotice=make('p','run-help');setup.prepend(this.stageNotice);
      this.statusBar=make('div','run-status-bar');this.statusBar.setAttribute('role','status');this.statusBar.setAttribute('aria-live','polite');this.root.insertBefore(this.statusBar,this.tabs);
      this.selectTab('setup');
    }
    bind(){
      this.tabs.addEventListener('keydown',event=>{
        if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;
        const tabs=$$('[role="tab"]',this.tabs),index=tabs.indexOf(event.target);if(index<0)return;event.preventDefault();
        const next=event.key==='Home'?tabs[0]:event.key==='End'?tabs.at(-1):tabs[(index+(event.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length];
        this.selectTab(next.dataset.runTab);next.focus();
      });
      const changed=()=>{window.implexityInvalidateOptimizationPreflight?.('Optimizer settings changed.');this.sync();};
      this.nativeControls.addEventListener('input',changed);this.nativeControls.addEventListener('change',changed);
      this.epoch.addEventListener('input',changed);this.epoch.addEventListener('change',changed);
      for(const id of ['optiters','optlr','optgrid'])$('#'+id)?.addEventListener('input',changed);
      for(const name of ['implexity:physics-draft-state','implexity:response-program-draft','implexity:response-program-apply','implexity:provider-selection-changed','implexity:provider-catalogue-changed','implexity:physics-plan','implexity:design-state-changed','implexity:workbench-refreshed'])window.addEventListener(name,()=>this.sync());
      window.addEventListener('implexity:public-command-result',event=>{
        if(event.detail?.action==='save_optimization_setup')void this.inspectSetup();
      });
      window.addEventListener('implexity:preflight-state',event=>{
        this.pending=event.detail?.state==='loading';
        if(['loading','refused','blocked','error','ready'].includes(event.detail?.state))this.selectTab('monitor');
        this.sync();
      });
      window.addEventListener('implexity:optimization-progress',event=>{
        const status=event.detail?.status||'';
        if(status!==this.lastStatus&&['running','paused','intervening','done','failed','error','needs_attention'].includes(status))this.selectTab('monitor');
        this.lastStatus=status;this.sync();
      });
      $('#optstart')?.addEventListener('click',()=>this.selectTab('monitor'));
      window.addEventListener('beforeunload',event=>{
        if(this.workbench._physicsDrafts?.size||this.workbench._responseProgramDirty){event.preventDefault();event.returnValue='';}
      });
    }
    selectTab(id){
      if(!this.panels[id])return;this.tab=id;
      for(const tab of $$('[role="tab"]',this.tabs)){const active=tab.dataset.runTab===id;tab.setAttribute('aria-selected',String(active));tab.tabIndex=active?0:-1;}
      for(const [key,panel] of Object.entries(this.panels)){panel.hidden=key!==id;panel.inert=key!==id;}
    }
    openPhysics(){
      this.workbench.setStep('physics');const entry=this.workbench.providerEntries().find(row=>(row.id||row.name)===this.workbench.currentProvider());if(entry)this.workbench.configureProvider(entry);
    }
    values(){
      const raw=Object.fromEntries(Object.entries(controls).map(([key,id])=>[key,$('#'+id).value]));
      if(window.ImplexityDesignFreedom?.isConfigured?.()){raw.metric=defaults.metric;raw.alpha=defaults.alpha;}
      else if(raw.metric!=='gradient_adaptive')raw.alpha=defaults.alpha;
      return parseValues(raw,Number($('#optlr').value));
    }
    epochFieldSelection(){
      const fields=$$('input[type="checkbox"][data-epoch-field]',this.epochList).filter(box=>box.checked).map(box=>box.dataset.epochField);
      for(const name of this.epochExtra.value.split(',').map(value=>value.trim()).filter(Boolean)){
        if(!/^[A-Za-z][A-Za-z0-9_.:-]{0,255}$/.test(name))throw new Error(`Retained field name ${name} is not a valid field identifier.`);
        if(!fields.includes(name))fields.push(name);
      }
      if(!fields.length&&!this.epochSensitivities.checked)return null;
      if(fields.length>64)throw new Error('At most 64 fields can be retained per epoch.');
      const every=Number(this.epochEvery.value),rawBudget=this.epochBudget.value.trim(),budget=rawBudget===''?null:Number(rawBudget);
      if(!Number.isSafeInteger(every)||every<1)throw new Error('Retain every N epochs requires a positive whole number.');
      if(budget!==null&&(!Number.isSafeInteger(budget)||budget<1||!Number.isSafeInteger(budget*1048576)))throw new Error('The retained-array budget must be a positive whole number of MiB within the supported integer range.');
      return {schema:'implexity-epoch-field-selection/1',fields,include_sensitivities:this.epochSensitivities.checked,every,...(budget===null?{}:{max_bytes:budget*1048576})};
    }
    augmentRequest(request,{report=false}={}){
      if(normalProvider(request)==='legacy_multiphysics_implicit')return request;
      try {
        const result=augment(request,this.values(),Boolean(window.ImplexityDesignFreedom?.isConfigured?.()));
        const epoch=this.epochFieldSelection();
        if(epoch){if(Object.hasOwn(result,'epoch_fields'))throw new Error('Retained epoch fields are already declared by another setup owner.');result.epoch_fields=epoch;}
        text(this.validation,'');return result;
      }
      catch(error){text(this.validation,error.message);if(report){this.selectTab('setup');this.validation.focus?.();}throw error;}
    }
    renderEpochChoices(state){
       
      const delegated=Object.values(this.workbench.providerProblem?.()?.intent?.provider_overrides||{}).filter(id=>typeof id==='string');
      const entries=this.workbench.providerEntries?.()||[];
      const delegatedFields=delegated.flatMap(id=>entries.find(entry=>(entry.id||entry.name)===id)?.fields||[]);
      const names=new Set([...(state.providerEntry?.fields||[]),...delegatedFields,...(this.workbench.providerWorkspaceCapabilities?.[state.provider]?.result_fields||[])].filter(name=>typeof name==='string'&&name));
      const signature=canonical([...names].sort());
      if(signature===this.epochSignature)return;
      const checked=new Set($$('input[data-epoch-field]',this.epochList).filter(box=>box.checked).map(box=>box.dataset.epochField));
      this.epochSignature=signature;this.epochList.replaceChildren();
      if(!names.size)this.epochList.append(make('p','run-help','The selected physics declares no result fields; enter field names below.'));
      for(const name of [...names].sort()){
        const label=make('label','run-check');const box=make('input');box.type='checkbox';box.dataset.epochField=name;box.checked=checked.has(name);
        label.append(box,document.createTextNode(' '+(window.ImplexityText?.humanize?.(name)||name.replaceAll('_',' '))));label.title=name;this.epochList.append(label);
      }
    }
    epochCaptureRows(){
      const rows=(window.OPT?.rows||[]).filter(row=>Number.isSafeInteger(row?.i));
      return rows.map(row=>({epoch:row.i,capture:row.diagnostics?.epoch_field_capture})).filter(row=>row.capture&&typeof row.capture==='object');
    }
    publishedEpochRows(){
      return (window.OPT?.rows||[]).filter(row=>Number.isSafeInteger(row?.i)&&row.i>=0).sort((a,b)=>a.i-b.i);
    }
    updateLiveEpochs(){
      const rows=this.publishedEpochRows(),job=window.OPT?.job||'';
      const signature=canonical([job,rows.map(row=>[row.i,row.design_state_id,row.terms,row.diagnostic_responses,row.diagnostics?.epoch_field_capture?.status])]);
      if(signature===this.liveEpochSignature)return;
      const changedJob=job!==this.liveEpochJob;this.liveEpochJob=job;this.liveEpochSignature=signature;
      if(changedJob){this.epochReadGeneration=(this.epochReadGeneration||0)+1;this.epochImage.hidden=true;this.epochExpandButton.disabled=true;this.epochDialogImage.removeAttribute('src');this.epochFollow.checked=true;this.liveDisplaySignature='';}
      this.refreshEpochViewer();
      if(this.epochFollow.checked&&rows.length)this.scheduleEpochDisplay();
    }
    refreshEpochViewer(){
      const rows=this.publishedEpochRows(),previous=this.epochSelect.value;this.epochSelect.replaceChildren();
      for(const row of rows){const option=make('option','',`Epoch ${row.i}`);option.value=String(row.i);this.epochSelect.append(option);}
      if(!this.epochFollow.checked&&rows.some(row=>String(row.i)===previous))this.epochSelect.value=previous;
      else if(rows.length)this.epochSelect.value=String(rows.at(-1).i);
      this.renderEpochFields();this.renderEpochMetrics();
      if(!rows.length){this.epochImage.hidden=true;this.epochExpandButton.disabled=true;this.epochDialogImage.removeAttribute('src');text(this.epochMessage,window.OPT?.job?'Waiting for the first published epoch.':'Select an optimization run.');}
    }
    liveSelectionChanged(){
      this.epochReadGeneration=(this.epochReadGeneration||0)+1;this.epochImage.hidden=true;this.epochExpandButton.disabled=true;this.epochDialogImage.removeAttribute('src');
      this.renderEpochFields();this.renderEpochMetrics();this.scheduleEpochDisplay();
    }
    renderEpochFields(){
      const row=this.epochCaptureRows().find(item=>String(item.epoch)===this.epochSelect.value);
      const previous=this.epochFieldSelect.value;this.epochFieldSelect.replaceChildren();
      const records=Array.isArray(row?.capture?.records)?row.capture.records:[],point=Number(this.epochPoint.value);
      const candidates=records.filter(record=>record.identity?.scope==='aggregate'||record.identity?.operating_point===point);
      const specs=new Map();for(const record of candidates)for(const [name,spec] of Object.entries(record.field_specs||{}))specs.set(name,{...spec,...record.fields?.[name],scope:record.identity?.scope});
      const sensitivity=name=>name.startsWith('sensitivity_')||name.includes('__dR_d')||name.includes('__dL_d')||specs.get(name)?.sensitivity===true;
      const names=[...specs.keys()].filter(name=>this.epochView.value==='sensitivity'?sensitivity(name):!sensitivity(name)).sort();
      for(const name of names){const spec=specs.get(name);const label=spec.response&&spec.parameter?`${spec.response==='weighted_total'?'Combined gradient':spec.response} / ${spec.parameter}${spec.component_label?' · '+spec.component_label:spec.component_index!==undefined?' · component '+spec.component_index:''}`:window.ImplexityText?.humanize?.(name)||name;const option=make('option','',`${label} · ${(spec.shape||[]).join('×')}${spec.scope==='aggregate'?' · all operating points':''}`);option.value=name;this.epochFieldSelect.append(option);}
      if(names.includes(previous))this.epochFieldSelect.value=previous;else{const spatial=names.find(name=>specs.get(name)?.shape?.length===3&&specs.get(name)?.registration);if(spatial)this.epochFieldSelect.value=spatial;}
      const geometry=this.epochView.value==='geometry';this.epochFieldSelect.closest('label').hidden=geometry;this.epochPoint.closest('label').hidden=geometry;this.epochIso.closest('label').hidden=geometry;
      const selected=Boolean(window.OPT?.job)&&this.publishedEpochRows().some(row=>String(row.i)===this.epochSelect.value);
      this.epochShowButton.disabled=!selected||(!geometry&&!names.length);
      this.epochSTLButton.disabled=!selected;this.epochModelButton.disabled=!selected;
      if(!geometry&&!names.length&&this.epochSelect.value!=='')text(this.epochMessage,`Epoch ${this.epochSelect.value} has no retained ${this.epochView.value==='sensitivity'?'sensitivity':'physical'} fields for this operating point.`);
    }
    renderEpochMetrics(){
      const row=this.publishedEpochRows().find(item=>String(item.i)===this.epochSelect.value),selected=this.metricRows?.[Number(this.epochMetric.value)];
      const previous=selected?canonical([selected.response,selected.operating_point??0]):null;
      this.metricRows=(row?.diagnostic_responses?.length?row.diagnostic_responses:row?.terms||[]).filter(term=>typeof term.response==='string'&&Number.isFinite(term.value));
      this.epochMetric.replaceChildren();
      for(const [index,term] of this.metricRows.entries()){const option=make('option','',`${window.ImplexityText?.humanize?.(term.response)||term.response} · point ${term.operating_point??0}`);option.value=String(index);this.epochMetric.append(option);}
      const index=this.metricRows.findIndex(term=>canonical([term.response,term.operating_point??0])===previous);if(index>=0)this.epochMetric.value=String(index);
      this.epochMetric.disabled=!this.metricRows.length;this.renderEpochMetric();
    }
    renderEpochMetric(){
      const term=this.metricRows?.[Number(this.epochMetric.value)],row=this.publishedEpochRows().find(item=>String(item.i)===this.epochSelect.value);
      if(!row){text(this.epochMetricValue,'');return;}
      if(!term){text(this.epochMetricValue,'No physical response values were published for this epoch.');return;}
      const entry=this.workbench.providerEntries?.().find(item=>(item.id||item.name)===row.provider),meta=entry?.response_metadata?.[term.response];
      const unit=term.units||term.unit||meta?.units||meta?.unit;const label=meta?.label||window.ImplexityText?.humanize?.(term.response)||term.response;
      text(this.epochMetricValue,`${label} = ${term.value.toPrecision(7)}${unit?' '+unit:''} · epoch ${row.i} · operating point ${term.operating_point??0}`);
    }
    scheduleEpochDisplay(){
      this.pendingEpochDisplay=true;if(this.epochDisplayBusy)return;
      void (async()=>{this.epochDisplayBusy=true;try{while(this.pendingEpochDisplay){this.pendingEpochDisplay=false;try{await this.displayEpoch();}catch(error){text(this.epochMessage,error.message);}}}finally{this.epochDisplayBusy=false;}})();
    }
    expandEpoch(){
      if(this.epochImage.hidden)return;
      this.epochDialogReturnFocus=document.activeElement===this.epochImage?this.epochExpandButton:document.activeElement;
      this.epochDialogImage.src=this.epochImage.src;this.epochDialogImage.alt=this.epochImage.alt;this.epochDialog.showModal();
    }
    async displayEpoch(){
      if(this.epochShowButton.disabled)return;
      const key=canonical([window.OPT?.job,this.epochSelect.value,this.epochView.value,this.epochFieldSelect.value,this.epochPoint.value,this.epochIso.value]);
      if(key===this.liveDisplaySignature)return;
      this.epochImage.hidden=true;this.epochExpandButton.disabled=true;this.epochDialogImage.removeAttribute('src');
      if(this.epochView.value!=='geometry'){if(await this.showEpochField())this.liveDisplaySignature=key;return;}
      const job=window.OPT?.job,epoch=Number(this.epochSelect.value),generation=this.epochReadGeneration=(this.epochReadGeneration||0)+1;
      const current=()=>this.epochReadGeneration===generation&&window.OPT?.job===job&&Number(this.epochSelect.value)===epoch&&this.epochView.value==='geometry';
      text(this.epochMessage,`Rendering epoch ${epoch}…`);
      let result;try{result=await action('render_3d',{source:{kind:'optimization_epoch',job_id:job,epoch},width_px:960,height_px:540,quality:'preview',background:'white'});}catch(error){if(!current())return;throw error;}
      if(!current())return;
      const image=result.image;if(image?.mime_type!=='image/png'||typeof image.data_base64!=='string')throw new Error('The epoch render contains no PNG image.');
      const decoded=new Image();decoded.src='data:image/png;base64,'+image.data_base64;try{await decoded.decode();}catch(error){if(!current())return;throw error;}if(!current())return;
      this.epochImage.src=decoded.src;this.epochImage.alt=`Geometry at epoch ${epoch}`;this.epochImage.hidden=false;this.epochExpandButton.disabled=false;this.epochDialogImage.src=decoded.src;this.epochDialogImage.alt=this.epochImage.alt;this.liveDisplaySignature=key;
      text(this.epochMessage,`Epoch ${epoch} · saved geometry`);
    }
    async showEpochField(){
      const job=window.OPT?.job,epoch=Number(this.epochSelect.value),field=this.epochFieldSelect.value,point=Number(this.epochPoint.value);
      const generation=this.epochReadGeneration=(this.epochReadGeneration||0)+1;
      const view=this.epochView.value,isoInput=this.epochIso.value;
      const current=()=>this.epochReadGeneration===generation && window.OPT?.job===job &&
        Number(this.epochSelect.value)===epoch && this.epochFieldSelect.value===field && Number(this.epochPoint.value)===point&&this.epochView.value===view&&this.epochIso.value===isoInput;
      if(!job||!Number.isSafeInteger(epoch)||!field)throw new Error('Choose a retained epoch and field.');
      if(!Number.isSafeInteger(point)||point<0)throw new Error('The operating point must be a non-negative whole number.');
      const row=this.epochCaptureRows().find(item=>item.epoch===epoch);
      const record=(row?.capture?.records||[]).find(record=>(record?.identity?.scope==='aggregate'||record?.identity?.operating_point===point)&&record.field_specs?.[field]);
      const spec=record?.field_specs?.[field];
      if(!spec)throw new Error('The selected epoch did not retain this field for that operating point.');
      text(this.epochMessage,'Reading the retained field summary…');
      let result;
      try{result=await action('read_optimization_epoch_field',{job_id:job,epoch,field,operating_point:point,mode:'summary'});}
      catch(error){if(!current())return;throw error;}
      if(!current())return;
      if(result.available!==true)throw new Error(`Field not available: ${result.reason||'not retained'}.`);
      const metadata=result.metadata||{},lo=result.finite_min,hi=result.finite_max;
      const unit=metadata.units||metadata.unit||'';
      const summary=`${window.ImplexityText?.humanize?.(field)||field} · epoch ${epoch} · minimum = ${Number.isFinite(lo)?lo.toPrecision(5):'-'} · maximum = ${Number.isFinite(hi)?hi.toPrecision(5):'-'}${unit?' '+unit:''}${result.scope==='aggregate'?' · aggregate gradient':''}`;
      if(result.shape.length!==3||!metadata.registration){text(this.epochMessage,summary+' It has no scalar spatial registration for a 3D isosurface.');return;}
      if(!Number.isFinite(lo)||!Number.isFinite(hi))throw new Error('The retained field has no finite values.');
      const iso=isoInput===''?(lo+hi)/2:Number(isoInput);
      if(!Number.isFinite(iso))throw new Error('Enter a finite isosurface value.');
      if(hi===lo){text(this.epochMessage,summary+' · uniform field, no isosurface');return true;}
      let rendered;try{rendered=await action('render_3d',{source:{kind:'optimization_epoch_field',job_id:job,epoch,field,operating_point:point},surface_field:field,color_field:field,iso_value:iso,quality:'native',palette:metadata.signed?'diverging':'sequential',value_range:metadata.signed?[-Math.max(Math.abs(lo),Math.abs(hi)),Math.max(Math.abs(lo),Math.abs(hi))]:[lo,hi],width_px:960,height_px:540,background:'white'});}catch(error){if(!current())return false;throw error;}
      if(!current())return false;
      const image=rendered.image;if(image?.mime_type!=='image/png'||typeof image.data_base64!=='string')throw new Error('The field render contains no PNG image.');
      const decoded=new Image();decoded.src='data:image/png;base64,'+image.data_base64;try{await decoded.decode();}catch(error){if(!current())return false;throw error;}if(!current())return false;
      this.epochImage.src=decoded.src;this.epochImage.alt=`${field} isosurface at epoch ${epoch}`;this.epochImage.hidden=false;this.epochExpandButton.disabled=false;this.epochDialogImage.src=decoded.src;this.epochDialogImage.alt=this.epochImage.alt;
      text(this.epochMessage,summary+` · isosurface = ${iso.toPrecision(5)}`);return true;
    }

    async applySettingsSchema(){
       
      let schema=null;
      try{
        const response=await fetch('/v1/agent/capabilities');if(!response.ok)return;
        const caps=await response.json();
        schema=(caps.actions||[]).find(row=>row.name==='start_optimization')?.input_schema?.properties?.settings?.properties;
      }catch(_error){return;}
      if(!schema||typeof schema!=='object')return;
      for(const [key,id] of Object.entries(controls)){
        const row=schema[key],input=$('#'+id);if(!row||!input||input.tagName!=='INPUT')continue;
        const label=input.closest('label')?.querySelector('span');
        if(typeof row.title==='string'&&label)text(label,row.title);
        const help=$('#'+id+'Help');
        if(help&&typeof row.description==='string')text(help,row.description+(typeof row.unit==='string'?` Unit: ${row.unit}.`:''));
        const lower=row.minimum??row.exclusiveMinimum,upper=row.maximum??row.exclusiveMaximum;
        if(typeof lower==='number')input.min=String(lower);
        if(typeof upper==='number')input.max=String(upper);
        if(row.type==='integer')input.step='1';
        if(typeof row.default==='number'&&Object.hasOwn(defaults,key)&&row.default!==defaults[key])input.title=`Service default ${row.default}`;
      }
    }
    renderBoundMonitor(){
      const opt=window.OPT||{};const rows=Array.isArray(opt.rows)?opt.rows:[];const last=rows.at(-1);
      const outcome=opt.meta?.summary?.search_outcome||opt.summary?.search_outcome||null;
      const bounds=Array.isArray(last?.bound_multipliers)?last.bound_multipliers:[];
      const signature=canonical([last?.i,last?.search_state,outcome]);
      this.boundMonitor.hidden=!last||!('bound_multipliers' in last);
      if(signature===this.boundSignature)return;this.boundSignature=signature;
      if(!last||!('bound_multipliers' in last)){this.boundBody.replaceChildren();text(this.boundSummary,'');return;}
      const number=value=>typeof value==='number'&&Number.isFinite(value)?Number(value.toPrecision(4)).toString():'N/A';
      const stationarity=last.stationarity||{};
      let summary=`Update ${last.i}: projected-gradient norm ${number(last.gradient_norm)} (${number(stationarity.relative)} of its first value; tolerance ${number(stationarity.tolerance)}), trust step ${number(last.trust_step)}, maximum scaled bound violation ${number(last.max_scaled_bound_violation)}${last.bound_feasible?' (within the bound tolerance)':''}.`;
      if(Array.isArray(last.multiplier_updates)&&last.multiplier_updates.length)summary+=` Multipliers updated (${last.multiplier_updates.map(row=>String(row.trigger||'').replaceAll('_',' ')).join(', ')}).`;
      if(last.numerical_trial_rejections)summary+=` ${last.numerical_trial_rejections} candidate(s) rejected numerically or by the validity regime.`;
      if(outcome)summary+=outcome.optimization_converged?' The search converged: projected stationarity and every bound within tolerance.':` The search stopped (${String(outcome.termination_reason||'unknown').replaceAll('_',' ')}) without established convergence.`;
      text(this.boundSummary,summary);
      this.boundBody.replaceChildren(...bounds.map(row=>{
        const line=make('tr');const sense={upper:'≤',lower:'≥',equal:'='}[row.sense]||row.sense;
        for(const value of [window.ImplexityText?.humanize?.(row.response)||row.response,String(row.operating_point??0),`${sense} ${number(row.target)}`,number(row.value),number(row.scaled_violation),number(row.multiplier),number(row.penalty)])line.append(make('td','',value));
        return line;
      }));
      if(!bounds.length)this.boundBody.append((()=>{const line=make('tr');const cell=make('td','','No bounded responses in this run.');cell.colSpan=7;line.append(cell);return line;})());
    }
    sync(){
      if(!this.root.isConnected)return;
      this.renderBoundMonitor();this.updateLiveEpochs();
      const scope=$('.implexity-design-freedom-card');if(scope&&!this.root.contains(scope)){this.scopeHost.append(scope);scope.querySelector('[data-design-freedom-configure]').hidden=true;}
      const state=this.workbench.readinessState();const native=state.provider&&state.provider!=='legacy_multiphysics_implicit';
      this.nativeControls.hidden=!native;this.nativeControls.inert=!native;
      this.epoch.hidden=!native;this.epoch.inert=!native;if(native)this.renderEpochChoices(state);
      const grid=$('#optgrid');if(grid){grid.disabled=Boolean(native);grid.closest('label').hidden=Boolean(native);}
      const iterations=$('#optiters'),rate=$('#optlr');if(iterations)iterations.max=native?'1000000':'60';
      if(rate){rate.min=native?'0.000000001':'.005';rate.max=native?'1':'.5';rate.step=native?'any':'.005';}
      text(this.gridNote,native?'Spatial resolution belongs to the geometry and selected physics add-in. Change it in the corresponding setup, not with a display-grid slider.':'The legacy provider uses the selected grid resolution.');
      const fixed=$('#runStepPolicy').value==='fixed_step';for(const key of ['minimum_step_fraction','backtracking','armijo','step_growth']){$('#'+controls[key]).disabled=fixed;$('#'+controls[key]).closest('label').hidden=fixed;}
      const hierarchy=Boolean(window.ImplexityDesignFreedom?.isConfigured?.());$('#runUpdateMetric').disabled=hierarchy;$('#runMetricAlpha').disabled=hierarchy||$('#runUpdateMetric').value!=='gradient_adaptive';
      const iterationLabel=iterations?.closest('label');if(iterationLabel)iterationLabel.hidden=hierarchy;
      this.stageNotice.hidden=!hierarchy;
      if(hierarchy){try{const stages=window.ImplexityDesignFreedom.getDeclaration().schedule||[];text(this.stageNotice,`Explicit schedule: ${stages.length} stage${stages.length===1?'':'s'}, ${stages.reduce((total,row)=>total+Number(row.iterations||0),0)} epochs in total. Edit stage budgets and coordinate release in Editable coordinates & stages. The epoch field does not override this schedule.`);}catch(error){text(this.stageNotice,error.message);}}
      const descriptions={geometry:state.topology.available?(state.topology.nativeComponents?'Native controls · model:control':'Editable topology field'):'Topology hand-off needed',physics:state.physics?'Applied · preflight still required':this.workbench._physicsDrafts?.size?'Unapplied physics draft':state.provider?'Complete physical setup':'Activate a physics add-in',goals:state.objectives?`${state.objectives} applied objective${state.objectives===1?'':'s'}`:state.completeResponseCount?`${state.completeResponseCount} applied limit responses`:'Apply response goals'};
      for(const [key,value] of Object.entries(descriptions))text($(`[data-run-fact="${key}"] .run-fact-state`,this.root),value);
      const drafts=this.workbench._physicsDrafts?.size;
      this.draftNotice.hidden=!drafts;text(this.draftNotice,'Physics settings have unapplied edits. Apply or discard them before preflight or optimization.');
      const program=this.workbench.appliedResponseProgram;
      const signature=canonical([state.provider,program,this.workbench._responseProgramDirty]);
      if(this.goalSignature!==signature){
        this.goalSignature=signature;this.goalRows.replaceChildren();
        if(!program?.objectives?.length&&!program?.constraints?.length)this.goalRows.append(make('p','run-help','No objectives applied. Choose response goals.'));
        else {
          const entry=state.providerEntry||{},meta=entry.response_metadata||{};
          for(const [kind,rows] of [['Objective',program.objectives],['Constraint',program.constraints||[]]])for(const row of rows){
            const unit=meta[row.response_id]?.units||meta[row.response_id]?.unit||'';
            const item=make('div','run-goal');item.append(make('strong','',meta[row.response_id]?.label||row.response_id.replaceAll('_',' ').replace(/^./,letter=>letter.toUpperCase())));
            item.append(make('span','',`${kind} · ${row.relation||row.sense||'minimize'} ${row.bound??row.target??''}${unit?' '+unit:''} · weight ${row.weight??1} · scale ${row.scale??1}`));this.goalRows.append(item);
          }
        }
        if(program?.constraints?.length){
          this.goalRows.append(make('p','run-constraint-method',
            'Response limits are soft augmented-Lagrangian terms: the weight sets the initial penalty 2 × weight, and the search updates a multiplier and, when the violation stalls, the penalty of every limit at every selected operating point. A run that stops with a limit exceeded is reported as not converged. Limits act on the reported response, not automatically on a local maximum.'));
        }
        if(this.workbench._responseProgramDirty)this.goalRows.prepend(make('p','run-validation','Response goals have unapplied changes.'));
      }
      let invalid='';try{if(window.ImplexityGuidedSetup?.active)window.ImplexityGuidedSetup.requestForRun(0);else if(native)this.values();}catch(error){invalid=error.message;}
      if(this.lastInvalid!==invalid){this.lastInvalid=invalid;window.updateOptButtons?.();}
      window.ImplexityGuidedSetup?.update();
      text(this.validation,invalid);this.saveButton.disabled=this.saving||!state.ready||Boolean(invalid)||this.pending;
      if(invalid){for(const id of ['pfbtn','optstart','optbranch']){const button=$('#'+id);if(button){button.disabled=true;button.title=invalid;}}}
      const opt=window.OPT||{};const status=opt.status||this.lastStatus||'idle';
      const label=this.pending?'Checking the current model and request…':opt.preflightReady?'Current request checked. Review before starting.':status==='intervening'?'Manual intervention: commit edits, then resume as a fresh branch.':!['idle','unknown'].includes(status)?`Run state: ${status.replaceAll('_',' ')}. Acceptance remains a separate decision.`:state.ready?'Setup complete. Check the current request before starting.':this.workbench.nextReadinessAction(state).label+'.';
      text(this.statusBar,label);
      for(const button of $$('.run-runtime-actions button',this.root))button.hidden=button.disabled;
    }
    async inspectSetup(){
      try{this.saved=await action('inspect_optimization_setup');const record=this.saved.record;
        text(this.savedMessage,record?`Saved revision ${record.revision}: ${record.label}. ${this.saved.stale?'Model, problem or physics changed since capture.':'Binding matches the current model and physics.'} Fresh preflight is always required.`:'No shared run setup has been saved.');
        text(this.savedJSON,record?JSON.stringify(record.request,null,2):'No saved request.');this.restoreButton.disabled=!record;this.editExactButton.disabled=!record;return this.saved;
      }catch(error){text(this.savedMessage,error.message);return null;}
    }
    async saveSetup(){
      if(this.saving)return;this.saving=true;this.sync();
      try{
        const before=await action('inspect_optimization_setup');
        const request=window.implexityCurrentOptimizationRequest({report:true});
        const local=this.workbench.model||window.S?.model;
        if(local?.content_id!==before.current_binding.content_id||local?.structure_id!==before.current_binding.structure_id)throw new Error('Displayed model is out of date. Refresh the model and review the setup before saving.');
         
        const expected_revision=this.saved?this.saved.revision:before.revision;
        this.saved=await action('save_optimization_setup',{label:this.savedLabel.value,request,expected_revision,expected_binding:before.current_binding});
        await this.inspectSetup();
      }catch(error){text(this.savedMessage,error.message);}finally{this.saving=false;this.sync();}
    }
    async editExactSetup(){
      const inspected=await this.inspectSetup();
      const record=inspected?.record;
      if(!record)throw new Error('Inspect a saved setup first.');
      if(inspected.stale)throw new Error('The saved request belongs to a previous model or physics revision. Review its binding before saving a replacement.');
      if(window.implexityOptimizationBlocksNewRun?.())throw new Error('Resolve the active run before editing the saved request.');
      if(!window.ImplexityAdvancedCommands)throw new Error('The Advanced command workspace is not ready.');
      await window.ImplexityAdvancedCommands.open('save_optimization_setup',{
        label:record.label,request:clone(record.request),expected_revision:record.revision,
        expected_binding:clone(record.binding)});
      text(this.savedMessage,'The complete saved request is open in Advanced commands, including nested fields and extensions. Saving it does not populate the guided forms or authorize optimization.');
    }
    restoreSteps(){
      try{
        const request=this.saved?.record?.request;if(!request)throw new Error('Inspect a saved setup first.');
        if(window.implexityOptimizationBlocksNewRun?.())throw new Error('Resolve the active run or intervention before restoring controls.');
        const provider=normalProvider(request);if(provider!==this.workbench.currentProvider())throw new Error('The saved setup belongs to a different physics provider. Nothing was changed.');
        const settings=request.settings||{};const iterations=settings.iterations??settings.iters??request.iters??request.iterations;
        const step=settings.step_fraction??settings.lr??request.lr??request.step_fraction;
        if(!Number.isSafeInteger(iterations)||iterations<1||iterations>1000000||typeof step!=='number'||!(step>0&&step<=1))throw new Error('Saved iterations or step fraction cannot be represented by these controls.');
        const metric=request.update_metric||{mode:'gradient_adaptive',alpha:.5};
        const raw={...defaults,step_policy:settings.step_policy??defaults.step_policy,...Object.fromEntries(Object.keys(defaults).filter(key=>Object.hasOwn(settings,key)).map(key=>[key,settings[key]])),live_every:settings.live_every??request.live_every??1,metric:typeof metric==='string'?metric:metric.mode,alpha:typeof metric==='object'?(metric.alpha??.5):.5};
        for(const key of removedConstraintKeys)if(Object.hasOwn(settings,key)||Object.hasOwn(request,key))throw new Error(`${removedConstraintMessage(key)} No controls were changed.`);
        const values=parseValues(raw,step);
        if(provider==='legacy_multiphysics_implicit')throw new Error('Saved native step controls cannot be restored onto the legacy provider.');
        for(const key of ['topology_lower','topology_upper'])if(Object.hasOwn(settings,key)||Object.hasOwn(request,key))throw new Error('This saved setup contains design bounds that must be authored in the design-space editor. No controls were changed.');
        $('#optiters').value=iterations;$('#optlr').value=step;
        for(const [key,id] of Object.entries(controls))$('#'+id).value=values[key]??'';
        window.implexityInvalidateOptimizationPreflight?.('Saved numerical controls restored. Review current physics, goals and resources.');
        text(this.savedMessage,'Step controls restored. Review the remaining settings before running.');this.sync();
      }catch(error){text(this.savedMessage,error.message);}
    }
    exportRequest(){
      try{const request=window.implexityCurrentOptimizationRequest({report:true});const blob=new Blob([JSON.stringify(request,null,2)+'\n'],{type:'application/json'});const url=URL.createObjectURL(blob);const link=make('a');link.href=url;link.download='implexity_optimization_request.json';link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);text(this.savedMessage,'Exact current request exported. It is not a preflight result or accepted design.');}
      catch(error){text(this.savedMessage,error.message);}
    }
  }
  function sampledSurfaceStatus(range){
     
    if(!Array.isArray(range)||range.length!==2||!range.every(value=>typeof value==='number'&&Number.isFinite(value))||range[0]>range[1])return {state:'unknown',sampled_only:true};
    const [minimum,maximum]=range;
    return {state:minimum>0?'outside_only':maximum<0?'inside_only':minimum===0&&maximum===0?'all_zero':'zero_bracketed',minimum,maximum,sampled_only:true};
  }
  window.ImplexityRunConfiguration=Object.freeze({parseValues,augment,defaults,controls:Object.freeze({...controls}),searchSettings,canonical,describeRequest,fieldUpdateSummary,sampledSurfaceStatus});
  function boot(){if(!window.implexityWorkbench||!$('#optiters')){setTimeout(boot,50);return;}if(!window.ImplexityRunWorkspace)window.ImplexityRunWorkspace=new RunWorkspace(window.implexityWorkbench);}
  if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot,{once:true});else boot();
})();
