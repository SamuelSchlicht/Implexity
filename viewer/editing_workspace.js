// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

 
(function(global){
  'use strict';
  const SCULPT='geometry_sculpt';
  const tools=[
    ['grab','Shape','Pull','Drag a point along the screen plane or a chosen axis.'],
    ['stretch','Shape','Stretch','Drag along the chosen axis to lengthen or shorten.'],
    ['scale','Shape','Scale','Drag up or down to scale about the pivot.'],
    ['twist','Shape','Twist','Drag horizontally to twist around the chosen axis.'],
    ['bend','Shape','Bend','Choose a root pivot and longitudinal axis, then drag horizontally.'],
    ['taper','Shape','Taper','Choose a root pivot and axis, then drag to taper.'],
    ['inflate','Sculpt','Thicken','Paint to increase the local thickness.'],
    ['deflate','Sculpt','Thin','Paint to reduce the local thickness.'],
    ['smooth','Sculpt','Smooth','Paint to smooth the selected geometry controls.'],
    ['add','Sculpt','Add material','Paint material into an occupancy field.'],
    ['subtract','Sculpt','Remove','Paint away material.'],
    ['flatten','Sculpt','Flatten','Paint towards the plane through the picked point.'],
    ['select','Select & hold','Paint selection','Paint a reusable region without changing the part.'],
    ['protect','Select & hold','Hold fixed','Paint a persistent shape or control hold.'],
    ['release','Select & hold','Release hold','Paint to release the same type of hold.']
  ];
  function vector(value){const a=String(value).trim().split(/[,\s]+/).filter(Boolean).map(Number);return a.length===3&&a.every(Number.isFinite)?a:null;}
  function inputIssue(values,selection,native=false){
    const positive=(v)=>String(v).trim()!==''&&Number.isFinite(Number(v))&&Number(v)>0;
    if(values.scope==='local'&&!positive(values.radius))return {key:'radius',text:'Enter a brush radius greater than zero.'};
    if(values.scope==='local'&&!positive(values.depth))return {key:'depth',text:'Enter a brush half-depth greater than zero.'};
    if(values.useSelection&&values.tool!=='select'&&(!selection?.valid||!(selection.selected_samples>0)))return {key:'useSelection',text:'This selection is empty or unavailable. Paint a region, choose All, or turn off the selection limit.'};
    if(native&&['add','flatten'].includes(values.tool))return {key:'tool',text:'This operation needs an occupancy field. Native controls are not converted.'};
    if(['grab','stretch','twist','bend','taper','flatten'].includes(values.tool)&&values.direction==='custom'){
      const a=vector(values.custom);if(!a||Math.hypot(...a)<1e-12)return {key:'custom',text:'Enter a nonzero X, Y, Z direction.'};
    }
    if(values.scope==='box'){
      const lo=vector(values.boxMin),hi=vector(values.boxMax);
      if(!lo||!hi||lo.some((v,i)=>v>=hi[i]))return {key:'boxMin',text:'Every box minimum must be smaller than its corresponding maximum.'};
    }
    if(['grab','stretch','scale','twist','bend','taper'].includes(values.tool)){
      if(values.pivotMode==='custom'&&!vector(values.pivot))return {key:'pivot',text:'Enter all three pivot coordinates or pick a pivot on the part.'};
      if(values.pivotMode==='selection'&&(!selection?.valid||!selection.centroid_mm))return {key:'pivotMode',text:'Paint a nonempty selection before choosing its centre as pivot.'};
    }
    if(['bend','taper'].includes(values.tool)&&!positive(values.axialLength))return {key:'axialLength',text:'Enter an axial length greater than zero.'};
    if(values.tool==='bend'){
      const b=vector(values.bendDirection);let a=null;
      if('xyz'.includes(values.direction)&&values.direction.length===1)a=[0,1,2].map(i=>i==='xyz'.indexOf(values.direction)?1:0);
      if(values.direction==='custom')a=vector(values.custom);
      if(!b||Math.hypot(...b)<1e-12)return {key:'bendDirection',text:'Enter a nonzero bend direction.'};
      if(a){const cross=[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];if(Math.hypot(...cross)<1e-9*Math.hypot(...a)*Math.hypot(...b))return {key:'bendDirection',text:'Bend towards a direction different from the longitudinal axis.'};}
    }
    return null;
  }
  function visibleExactKeys(tool){return new Set(['center',...(['grab'].includes(tool)?['delta','linearSnap']:[]),...(['stretch','scale','taper'].includes(tool)?['factor']:[]),...(['twist','bend'].includes(tool)?['angle','angleSnap']:[])]);}
  function toolbarKey(key,index,count){if(!count)return -1;if(key==='Home')return 0;if(key==='End')return count-1;if(['ArrowRight','ArrowDown'].includes(key))return(index+1)%count;if(['ArrowLeft','ArrowUp'].includes(key))return(index+count-1)%count;return -1;}
  global.ImplexityEditingUXRules=Object.freeze({tools,vector,inputIssue,visibleExactKeys,toolbarKey});
  if(typeof document==='undefined')return;
  const q=(sel,root=document)=>root.querySelector(sel);
  const node=(tag,cls,text)=>{const n=document.createElement(tag);if(cls)n.className=cls;if(text)n.textContent=text;return n;};
  const setText=(n,text)=>{if(n&&n.textContent!==text)n.textContent=text;};
  const labelText=(control,text)=>{const label=control?.closest('label');if(label?.firstChild?.nodeType===3)label.firstChild.textContent=text;};
  class EditingWorkspace {
    constructor(owner,workbench){
      this.owner=owner;this.w=workbench;this.c=owner.sculptControls;this.active=false;this.category='Shape';this.lastIssue=null;
      this.savedParent=owner.optionsPanel.parentElement;this.savedNext=owner.optionsPanel.nextSibling;
      this.host=node('section','implexity-editing-workspace');this.host.id='implexityEditingWorkspace';this.host.hidden=true;this.host.setAttribute('aria-label','Shape and sculpt workspace');
      this.host.innerHTML='<header class="ux-editor-head"><div><span class="ux-eyebrow">MANUAL GEOMETRY</span><h2>Shape & sculpt</h2></div><button type="button" data-ux-done title="Return to inspection without changing the part">Done</button></header><div class="ux-edit-state" role="status" aria-live="polite"><i aria-hidden="true"></i><span data-ux-state>Ready to edit</span></div><div class="ux-edit-body"></div><footer class="ux-edit-footer"></footer>';
      this.body=q('.ux-edit-body',this.host);this.footer=q('.ux-edit-footer',this.host);this.state=q('[data-ux-state]',this.host);this.done=q('[data-ux-done]',this.host);
      q('#implexityInspectorPane').prepend(this.host);
      this.done.addEventListener('click',()=>{if(this.busy())return;owner.setMode('select');owner.viewport.focus({preventScroll:true});});
      this.buildControls();
      this.legend=node('div','ux-edit-legend');this.legend.hidden=true;this.legend.innerHTML='<strong data-ux-legend-tool>Pull</strong><span data-ux-legend-hint>Drag a surface point</span><span><kbd>Esc</kbd> cancel</span><span><kbd>[</kbd><kbd>]</kbd> radius</span>';
      owner.viewport.append(this.legend);
      this.issue=node('p','ux-edit-issue');this.issue.hidden=true;this.issue.id='ux-edit-issue';this.issue.setAttribute('role','alert');owner.sculptPanel.prepend(this.issue);
      this.instructions=node('div','ux-edit-instructions');this.instructions.innerHTML='<strong data-ux-action>Drag a surface point</strong><p data-ux-help></p>';owner.sculptPanel.prepend(this.instructions);
       
      this.footer.append(owner.sculptPanel.querySelector('[data-sculpt-undo]').parentElement);
      this.evidence=q('[data-sculpt-evidence]',owner.sculptPanel);this.footer.append(this.evidence);
      this.review=node('div','ux-edit-review');this.review.hidden=true;this.review.innerHTML='<button type="button" class="primary" data-ux-apply>Apply edit</button><button type="button" data-ux-cancel>Cancel</button>';
      this.footer.prepend(this.review);
      q('[data-ux-apply]',this.review).addEventListener('click',()=>owner.sculptEnhancements.confirmExact());
      q('[data-ux-cancel]',this.review).addEventListener('click',()=>owner.sculptEnhancements.cancelExact());
      this.refreshButton=node('button','ux-edit-refresh','Refresh saved model');this.refreshButton.type='button';this.refreshButton.hidden=true;this.footer.append(this.refreshButton);
      this.refreshButton.addEventListener('click',async()=>{this.refreshButton.disabled=true;try{await owner._refreshCommittedGeometry();owner._editingRecoveryRequired=false;setText(this.evidence,'Current saved model reloaded.');}catch(e){owner._showError(e,'The model could not be refreshed.');}finally{this.refreshButton.disabled=false;this.update();}});
      this.config=node('fieldset','ux-config-fields');
      this.config.innerHTML='<legend class="visually-hidden">Geometry editing controls</legend>';
      const panel=owner.sculptPanel;
      for(const item of [...panel.children])this.config.append(item);
      panel.append(this.config,this.footer);
      this.config.prepend(this.palette,this.instructions,this.issue);
      this.issue.after(owner.optionsPanel.querySelector('.implexity-option-grid'));

      this.body.addEventListener('input',()=>this.update());this.body.addEventListener('change',()=>this.update());
      owner.sculptPanel.addEventListener('change',()=>this.update());
      global.addEventListener('implexity:interaction-mode',()=>this.syncMode());
      global.addEventListener('implexity:editing-review',e=>{if(e.detail.status==='error'){setText(this.evidence,e.detail.message);}this.update();});
      global.addEventListener('implexity:model-updated',()=>this.update());
      global.addEventListener('implexity:interaction-committed',()=>this.update());
      global.addEventListener('keydown',e=>this.keys(e),true);
      this.host.addEventListener('keydown',e=>{
        if(e.key==='Escape'&&owner.sculptEnhancements.pickMode){owner.sculptEnhancements.pickMode=null;e.preventDefault();owner.viewport.focus();this.update();}
      });
      global.addEventListener('beforeunload',e=>{if(owner.sculptEnhancements.exactReview){e.preventDefault();e.returnValue='';}});
       
       
      owner.viewport.addEventListener('pointerdown',e=>{
        if(!this.active||e.button!==0||e.target.closest('button,input,select,textarea,.implexity-options,.implexity-toolbar'))return;
        const issue=this.validation();if(issue&&!owner.sculptEnhancements.pickMode){e.preventDefault();e.stopImmediatePropagation();this.showIssue(issue);}
      },true);
       
      this.resizeObserver=typeof ResizeObserver==='function'?new ResizeObserver(()=>{
        if(this.resizeFrame)cancelAnimationFrame(this.resizeFrame);
        this.resizeFrame=requestAnimationFrame(()=>{global.VIEW?.resize?.();owner.renderOverlays();});
      }):null;
      this.resizeObserver?.observe(owner.viewport);
      document.addEventListener('click',e=>{
        if(!owner.sculptEnhancements.exactReview)return;
        if(e.target.closest('#implexity-workflow button,#optstart,#pfbtn,#optgo')){
          e.preventDefault();e.stopImmediatePropagation();setText(this.evidence,'Apply or cancel the geometry preview before changing workspaces or starting physics.');
        }
      },true);
      this.syncMode();
      this.timer=setInterval(()=>{if(!this.host.isConnected){clearInterval(this.timer);return;}if(this.active)this.update();},160);
    }
    buildControls(){
      const o=this.owner,c=this.c,panel=o.sculptPanel;
      const original=c.tool.closest('label');original.hidden=true;
      this.palette=node('div','ux-tool-picker');this.palette.innerHTML='<div class="ux-tool-categories" role="tablist" aria-label="Editing task"></div><div role="tabpanel" id="ux-tool-panel"><div class="ux-tool-grid" role="toolbar" aria-label="Shape and sculpt tools"></div></div>';
      const categories=q('.ux-tool-categories',this.palette),grid=q('.ux-tool-grid',this.palette);
      for(const name of ['Shape','Sculpt','Select & hold']){
        const b=node('button','',name);b.type='button';b.dataset.uxCategory=name;b.id='ux-category-'+name.split(' ')[0].toLowerCase();b.setAttribute('role','tab');b.setAttribute('aria-controls','ux-tool-panel');b.setAttribute('aria-selected',String(name===this.category));b.tabIndex=name===this.category?0:-1;
        b.addEventListener('click',()=>this.choose(tools.find(t=>t[1]===name)[0]));categories.append(b);
      }
      categories.addEventListener('keydown',e=>{const a=[...categories.children],i=a.indexOf(document.activeElement),n=toolbarKey(e.key,i,a.length);if(i>=0&&n>=0){e.preventDefault();a[n].click();a[n].focus();}});
      const paths={grab:'M5 12h14m-4-4 4 4-4 4M9 8l-4 4 4 4',stretch:'M6 5v14M18 5v14M8 12h8m-6-3-3 3 3 3m4-6 3 3-3 3',scale:'M6 9V5h4m4 0h4v4M6 15v4h4m4 0h4v-4M9 9h6v6H9z',twist:'M5 8c5-8 9 8 14 0M5 16c5-8 9 8 14 0M5 5v6m14 2v6',bend:'M5 19V9c0-5 10-6 14-3M9 19V9c0-2 7-3 10-1',taper:'M7 4h10l4 16H3z',inflate:'M4 17c0-12 16-12 16 0M8 12h8m-4-4v8',deflate:'M4 17c0-7 16-7 16 0M8 7h8',smooth:'M3 16c4-12 8 5 18-8',add:'M12 4v16M4 12h16',subtract:'M4 12h16',flatten:'M4 19h16M5 6l5 3 5-3 4 3M12 10v6m-3-3 3 3 3-3',select:'M5 7V4h4m6 0h4v3M5 17v3h4m6 0h4v-3M8 12l3 3 6-6',protect:'M6 11h12v10H6zM8 11V7a4 4 0 0 1 8 0v4',release:'M6 11h12v10H6zM8 11V7a4 4 0 0 1 7-3'};
      for(const [id,category,label,hint] of tools){const b=node('button','ux-tool-tile');b.type='button';b.dataset.uxTool=id;b.dataset.category=category;b.title=hint;b.setAttribute('aria-label',label);b.innerHTML=`<svg viewBox="0 0 24 24" aria-hidden="true"><path d="${paths[id]}"/></svg><span>${label}</span>`;b.addEventListener('click',()=>this.choose(id));grid.append(b);}
      grid.addEventListener('keydown',e=>{const a=[...grid.children].filter(x=>!x.hidden&&!x.disabled),i=a.indexOf(document.activeElement),n=toolbarKey(e.key,i,a.length);if(i>=0&&n>=0){e.preventDefault();a.forEach(x=>x.tabIndex=-1);a[n].tabIndex=0;a[n].focus();}});
      panel.prepend(this.palette);
      this.selection=q('[data-sculpt-selection-settings]',panel);this.selection.open=false;
      labelText(c.selectionId,'Selection name');labelText(c.depth,'Brush half-depth [mm]');labelText(c.hardness,'Uniform brush core [0–0.95]');
      this.selection.querySelector('summary').textContent='Saved selections';
      this.selectionHelp=node('p','ux-selection-help','A selection limits manual editing. Use Hold fixed to constrain later optimization.');this.selection.append(this.selectionHelp);
      this.limit=node('button','ux-use-selection','Edit this selection');this.limit.type='button';this.limit.addEventListener('click',()=>{const r=o.sculptEnhancements.record();if(!r?.valid||r.selected_samples<=0){this.showIssue({text:'Paint a region first or choose All.'});return;}c.useSelection.checked=true;c.scope.value='whole';this.choose('grab');this.selection.open=false;o._configureSculptOptions();this.update();});this.selection.append(this.limit);
      this.limitBadge=node('div','ux-selection-limit');this.limitBadge.innerHTML='<span data-ux-limit-text></span><button type="button" data-ux-remove-limit title="Remove the manual selection limit. The saved selection is retained.">Remove limit</button>';panel.querySelector('.implexity-sculpt-grid').prepend(this.limitBadge);
      q('button',this.limitBadge).addEventListener('click',()=>{c.useSelection.checked=false;o._configureSculptOptions();this.update();});
      this.falloff=node('details','ux-falloff');this.falloff.innerHTML='<summary>Brush falloff</summary>';c.hardness.closest('label').before(this.falloff);this.falloff.append(c.hardness.closest('label'));
      this.target=node('details','ux-representation');this.target.innerHTML='<summary>Representation & optimization</summary>';panel.append(this.target);this.target.append(q('[data-sculpt-target]',panel));
      this.help=q('[data-sculpt-help]',panel);this.help.hidden=true;
      this.pivot=q('[data-sculpt-pivot-settings]',panel);
      this.exact=q('[data-sculpt-exact]',panel);this.exact.querySelector('summary').textContent='Place influence in the volume';
      this.symmetry=c.symmetry.closest('details');this.symmetry.querySelector('summary').textContent='Symmetry & material phase';
      this.navigation=node('details','ux-navigation');this.navigation.innerHTML='<summary>Mouse & keyboard help</summary><dl><dt>Left drag on part</dt><dd>Edit with the active tool</dd><dt>Right drag</dt><dd>Orbit the camera</dd><dt>Middle or Shift + right drag</dt><dd>Pan the camera</dd><dt>Mouse wheel</dt><dd>Zoom</dd><dt>Escape</dt><dd>Cancel a drag, point pick, or numerical preview</dd><dt>Ctrl / Cmd + Z</dt><dd>Undo a committed edit</dd><dt>[ and ]</dt><dd>Smaller / larger brush, with the viewport focused</dd></dl><p>Use Exact values for an edit without dragging. Changes to loads and supports still require a fresh physics review.</p>';panel.append(this.navigation);
    }
    busy(){const o=this.owner;return Boolean(o.activeGesture||o.pendingGesture||o.selectionOperationPending||global.ImplexityManualHistory?.counts().busy||o._editingRecoveryRequired);}
    values(){const v={};for(const [k,c]of Object.entries(this.c))v[k]=c.type==='checkbox'?c.checked:c.value;v.radius=this.owner.radiusInput.value;return v;}
    validation(){return inputIssue(this.values(),this.owner.sculptEnhancements.record(),String(this.owner._activeFieldId()||'').includes('::component::'));}
    showIssue(issue){this.lastIssue=issue;this.issue.hidden=!issue;setText(this.issue,issue?.text||'');if(issue?.key){const c=issue.key==='radius'?this.owner.radiusInput:this.c[issue.key];c?.setAttribute('aria-invalid','true');c?.setAttribute('aria-describedby',this.issue.id);}}
    choose(id){if(this.busy())return;const entry=[...this.c.tool.options].find(x=>x.value===id);if(!entry||entry.disabled)return;this.c.tool.value=id;this.c.tool.dispatchEvent(new Event('change',{bubbles:true}));if(id==='select')this.selection.open=true;if(['bend','taper'].includes(id))this.pivot.open=true;this.update();}
    syncMode(){
      const active=this.owner.mode===SCULPT;if(active===this.active){if(active)this.update();return;}this.active=active;this.host.hidden=!active;this.legend.hidden=!active;document.body.classList.toggle('implexity-editing-active',active);
      if(active){this.body.append(this.owner.optionsPanel);if(!this.w._paneState.right)this.w.togglePane('right');this.host.parentElement.scrollTop=0;this.update();}
      else{this.savedParent.insertBefore(this.owner.optionsPanel,this.savedNext?.parentElement===this.savedParent?this.savedNext:null);}
      global.dispatchEvent(new CustomEvent('implexity:viewport-layout-changed'));
    }
    keys(e){
      const target=e.target,inEditor=this.host.contains(target),inView=this.owner.viewport.contains(target);if(!this.active||(!inEditor&&!inView))return;
      if(['input','select','textarea'].includes(target.tagName?.toLowerCase())||target.isContentEditable)return;
      if(e.key==='Escape'&&this.owner.sculptEnhancements.exactReview){e.preventDefault();this.owner.sculptEnhancements.cancelExact();return;}
      if((e.ctrlKey||e.metaKey)&&inEditor&&!this.busy()&&e.key.toLowerCase()==='z'){e.preventDefault();this.owner._requestHistory(e.shiftKey?'redo':'undo');return;}
      if(inView&&!this.busy()&&!e.ctrlKey&&!e.metaKey&&!e.altKey&&['x','y','z'].includes(e.key.toLowerCase())&&!this.c.direction.disabled){e.preventDefault();e.stopImmediatePropagation();this.c.direction.value=e.key.toLowerCase();this.owner._configureSculptOptions();this.update();}
    }
    update(){
      if(!this.active)return;const o=this.owner,c=this.c,t=tools.find(t=>t[0]===c.tool.value)||tools[0],busy=this.busy(),review=o.sculptEnhancements.exactReview,history=global.ImplexityManualHistory?.counts()||{};
      this.category=t[1];q('#ux-tool-panel',this.palette).setAttribute('aria-labelledby','ux-category-'+this.category.split(' ')[0].toLowerCase());
      for(const b of this.palette.querySelectorAll('[data-ux-category]')){const active=b.dataset.uxCategory===this.category;b.setAttribute('aria-selected',String(active));b.tabIndex=active?0:-1;}
      for(const b of this.palette.querySelectorAll('[data-ux-tool]')){const option=[...c.tool.options].find(x=>x.value===b.dataset.uxTool);b.hidden=b.dataset.category!==this.category;b.disabled=!!option?.disabled;b.setAttribute('aria-pressed',String(c.tool.value===b.dataset.uxTool));b.tabIndex=(this.palette.querySelector('.ux-tool-grid').contains(document.activeElement)?b===document.activeElement:c.tool.value===b.dataset.uxTool)?0:-1;b.title=option?.disabled?'Not supported by this representation. No hidden conversion is performed.':tools.find(t=>t[0]===b.dataset.uxTool)[3];}
      this.config.disabled=busy;this.done.disabled=busy;this.refreshButton.hidden=!o._editingRecoveryRequired;
      const undo=q('[data-sculpt-undo]',this.footer),redo=q('[data-sculpt-redo]',this.footer);undo.disabled=busy||!(history.undo>0);redo.disabled=busy||!(history.redo>0);undo.title=undo.disabled?(history.error||'No currently available shared undo.'):`Undo: ${history.nextUndo||'last shared edit'}`;redo.title=redo.disabled?(history.error||'No currently available shared redo.'):`Redo: ${history.nextRedo||'last shared edit'}`;
      this.review.hidden=!review;const apply=q('[data-ux-apply]',this.review),cancel=q('[data-ux-cancel]',this.review);apply.disabled=review?.status!=='ready';cancel.disabled=['committing','cancelling'].includes(review?.status);apply.textContent=review?.status==='committing'?'Applying…':review?.evidence?.selection?'Save selection':'Apply edit';
      const mode=o.sculptEnhancements.pickMode;
      const status=o._editingRecoveryRequired?'Refresh required before editing':review?({preparing:'Preparing preview…',ready:'Preview only · not saved',committing:'Saving edit…',cancelling:'Cancelling preview…'}[review.status]||'Preview pending'):o.pendingGesture?'Locating exact surface…':o.activeGesture?.finalizing?'Saving edit…':o.activeGesture?'Preview · release to save':mode?(mode==='pivot'?'Click the pivot on the part':'Click the end of the axis'):busy?'Refreshing saved geometry…':'Ready · edits remain optimizable';
      setText(this.state,status);this.host.dataset.state=review?.status|| (busy?'busy':mode?'picking':'ready');this.host.dataset.review=String(Boolean(review));
      setText(q('[data-ux-action]',this.instructions),mode?'Pick a point on the part':t[2]+(t[1]==='Shape'?' by dragging':' with the brush'));
      setText(q('[data-ux-help]',this.instructions),mode?'A point pick changes the tool setup, not the saved geometry. Escape cancels.':t[3]+' Release saves one edit. Escape cancels.');
      setText(q('[data-ux-legend-tool]',this.legend),review?'Numerical preview':t[2]);setText(q('[data-ux-legend-hint]',this.legend),review?'Apply or Cancel in the inspector':mode?'Click on the part · Esc cancels':c.useSelection.checked&&t[0]!=='select'?'Only the saved selection is editable':'Drag on the part · release to save');
      this.pivot.hidden=t[1]!=='Shape';this.falloff.hidden=c.scope.value==='whole';this.symmetry.hidden=t[0]==='select';c.carryPhase.closest('label').hidden=!String(o._activeFieldId()||'').includes('::component::');
      const exact=visibleExactKeys(t[0]);for(const key of ['center','delta','factor','angle','linearSnap','angleSnap'])c[key].closest('label').hidden=!exact.has(key);
      const shape=['grab','stretch','twist','scale','bend','taper'].includes(t[0]);c.pivotMode.closest('label').hidden=!shape;
      c.direction.closest('label').hidden=c.direction.disabled;
      const r=o.sculptEnhancements.record();this.limit.disabled=!r?.valid||!(r.selected_samples>0)||busy;
      this.limitBadge.hidden=!c.useSelection.checked||t[0]==='select';setText(q('[data-ux-limit-text]',this.limitBadge),`Selection: ${c.selectionId.value} · ${r?.selected_samples||0} samples`);
      if(!busy){for(const control of [o.radiusInput,...Object.values(c)]){control.removeAttribute('aria-invalid');control.removeAttribute('aria-describedby');}const issue=this.validation();this.showIssue(issue);q('[data-sculpt-apply-exact]',this.exact).disabled=!!issue;}
       
      const showRadius=c.scope.value==='local';o.radiusInput.hidden=!showRadius;o.optionControls.radius.label.hidden=!showRadius;
      this.palette.dataset.native=String(o._activeFieldId()||'').includes('::component::')?'true':'false';
    }
  }
  global.ImplexityEditingWorkspace=EditingWorkspace;
  function boot(){if(global.implexityEditingWorkspace)return;if(global.ImplexityInteraction?.sculptEnhancements&&global.implexityWorkbench&&q('#implexityInspectorPane'))global.implexityEditingWorkspace=new EditingWorkspace(global.ImplexityInteraction,global.implexityWorkbench);else setTimeout(boot,100);}
  if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot,{once:true});else boot();
})(globalThis);
