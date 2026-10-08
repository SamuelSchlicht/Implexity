// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(() => {
  "use strict";
  function boot() {
    const host=document.querySelector('#s_opt > .sect-b');
    if(!host||document.getElementById('implexity-study-workspace'))return;
    const panel=document.createElement('details');panel.id='implexity-study-workspace';
    panel.innerHTML=`<summary>Design studies · saved variants</summary>
      <p>Capture the currently applied optimisation setup as named variants. Saving validates requests but does not start a solver. Running uses the current geometry; restore the intended branch point first when identical starts are required.</p>
      <label>Study name <input data-study-name value="Design study"></label>
      <label>Variant name <input data-variant-name value="baseline"></label>
      <button type="button" data-capture>Capture current setup</button>
      <ul data-draft aria-label="Captured study variants"></ul>
      <button type="button" data-save disabled>Save study</button>
      <hr><label>Saved study ID <input data-study-id placeholder="study_…"></label>
      <button type="button" data-load>Load study</button>
      <div data-record></div><div data-job></div>
      <p role="status" aria-live="polite" data-status>No study loaded.</p>
      <details><summary>Campaign analysis · no solver execution</summary>
        <p>Analyse explicitly supplied declarations or results. This does not verify their provenance or certify physical validity.</p>
        <label>Analysis <select data-analysis><option value="campaign_compare">Compare input declarations</option><option value="campaign_rank">Rank declared results</option><option value="campaign_budget">Allocate iteration budget</option></select></label>
        <p data-analysis-help></p>
        <button type="button" data-use-variants>Use captured study requests</button>
        <label>Analysis input JSON <textarea data-analysis-input rows="8" spellcheck="false"></textarea></label>
        <button type="button" data-analyse>Analyse declarations</button>
        <pre data-analysis-result aria-label="Campaign analysis result"></pre>
      </details>`;
    host.append(panel);
    const q=s=>panel.querySelector(s);let drafts=[],record=null,job=null,busy=false;
    const status=text=>{q('[data-status]').textContent=text;};
    function availability(){panel.querySelectorAll('button').forEach(b=>b.disabled=busy||b.dataset.unavailable==='true');q('[data-save]').disabled=busy||!drafts.length;}
    async function action(name,payload){
      const response=await fetch('/v1/agent/action',{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json'},body:JSON.stringify({action:name,payload})});
      const data=await response.json();
      if(!response.ok||!data.ok){
        const problems=Array.isArray(data.problems)?data.problems.filter(p=>typeof p==='string').join(' '):'';
        throw new Error([data.error||data.message||'The service rejected this action.',problems].filter(Boolean).join(' '));
      }
      return data.result;
    }
    async function perform(fn){if(busy)return;busy=true;availability();try{await fn();}catch(error){status(String(error.message||error));}finally{busy=false;availability();}}
    function button(label,handler,parent,disabled=false){const b=document.createElement('button');b.type='button';b.textContent=label;b.dataset.unavailable=String(disabled);b.onclick=()=>perform(handler);parent.append(b);return b;}
    function draftView(){
      q('[data-draft]').replaceChildren();
      drafts.forEach((variant,index)=>{const li=document.createElement('li');const text=document.createElement('span');text.textContent=variant.name+' ';li.append(text);button('Remove captured variant',async()=>{drafts.splice(index,1);draftView();},li);q('[data-draft]').append(li);});availability();
    }
    function jobView(info){
      const area=q('[data-job]');area.replaceChildren();
      const line=document.createElement('p');line.textContent=`Job ${job}: ${info.status||'status unavailable'}`;area.append(line);
      button('Refresh job',async()=>jobView(await action('inspect_optimization_job',{job_id:job,view:'monitor'})),area);
      const allowed={running:['pause','stop'],paused:['resume','stop'],intervening:['discard'],completed:['accept','discard'],stopped:['discard'],failed:['discard']}[info.status]||[];
      allowed.forEach(op=>button(op[0].toUpperCase()+op.slice(1)+' job',async()=>{
        if(['stop','discard','accept'].includes(op)&&!window.confirm(`${op} job ${job}? This changes the saved study run state.`))return;
        jobView(await action('optimization_operation',{job_id:job,op,view:'monitor'}));
      },area));availability();
    }
    function recordView(){
      const area=q('[data-record]');area.replaceChildren();if(!record)return;
      const heading=document.createElement('h4');heading.textContent=record.name+' · '+record.id;area.append(heading);
      for(const variant of record.variants||[]){
        const box=document.createElement('div');const label=document.createElement('strong');label.textContent=variant.name;box.append(label);
        const detail=document.createElement('details'),summary=document.createElement('summary'),pre=document.createElement('pre');summary.textContent='Inspect captured request';pre.textContent=JSON.stringify(variant.request,null,2);detail.append(summary,pre);box.append(detail);
        button('Run '+variant.name,async()=>{
          if(!window.confirm(`Start variant ${variant.name} from the CURRENT geometry? No saved geometry will be restored automatically.`))return;
          const result=await action('run_study_variant',{study_id:record.id,variant:variant.name});job=result.job_id;
          if(!job)throw new Error('No job identity returned. Inspect service state before trying again.');
          jobView(result);status('Study run submitted. Use its job controls below; do not start another solver.');
        },box);area.append(box);
      }
      for(const run of record.runs||[])button(`Inspect ${run.variant} · ${run.job_id}`,async()=>{job=run.job_id;jobView(await action('inspect_optimization_job',{job_id:job,view:'monitor'}));},area);
      availability();
    }
    q('[data-capture]').onclick=()=>perform(async()=>{
      const name=q('[data-variant-name]').value.trim();if(!name)throw new Error('Enter a variant name.');
      if(drafts.some(v=>v.name===name))throw new Error('Choose a unique variant name.');
      if(typeof window.implexityCurrentOptimizationRequest!=='function')throw new Error('The optimisation editor is not ready.');
      const request=JSON.parse(JSON.stringify(window.implexityCurrentOptimizationRequest({report:true})));
      drafts.push({name,request});draftView();status('Captured '+name+'. No model or solver was changed.');
    });
    q('[data-save]').onclick=()=>perform(async()=>{
      const name=q('[data-study-name]').value.trim();if(!name)throw new Error('Enter a study name.');
      record=await action('create_study',{name,variants:drafts});q('[data-study-id]').value=record.id;recordView();status('Study saved: '+record.id+'. No solver started.');
    });
    q('[data-load]').onclick=()=>perform(async()=>{
      const study_id=q('[data-study-id]').value.trim();if(!study_id)throw new Error('Enter a saved study ID.');
      record=await action('inspect_study',{study_id});recordView();status('Study loaded. Review each captured request before running.');
    });
    const analysisDrafts={campaign_compare:'',campaign_rank:'',campaign_budget:''};let analysis='campaign_compare';
    const help={
      campaign_compare:'Provide reference, variants [{name, inputs}], allowed_paths (scalar JSON pointers), invariant_paths and optionally required_resolved_paths. Equality of declarations is not solver admission.',
      campaign_rank:'Provide variants with name, status, an explicitly asserted physically_valid boolean and a finite response; set objective to that response key and sense to minimize or maximize. Use only a common comparison metric. Supplied validity claims are not independently verified.',
      campaign_budget:'Provide branches [{name, priority}], max_runs and max_iterations. Allocation only plans work; it starts nothing.'
    };
    function analysisView(){q('[data-analysis-help]').textContent=help[analysis];q('[data-analysis-input]').value=analysisDrafts[analysis];q('[data-analysis-result]').textContent='';q('[data-use-variants]').dataset.unavailable=String(analysis==='campaign_rank');availability();}
    q('[data-analysis]').onchange=()=>{const next=q('[data-analysis]').value;if(!Object.hasOwn(help,next)){q('[data-analysis]').value=analysis;return;}analysisDrafts[analysis]=q('[data-analysis-input]').value;analysis=next;analysisView();};
    q('[data-use-variants]').onclick=()=>perform(async()=>{
      const variants=record?.variants||drafts;if(!variants.length)throw new Error('Capture variants or load a saved study first.');
      const payload=analysis==='campaign_budget'?{branches:variants.map(v=>({name:v.name,priority:1})),max_runs:variants.length,max_iterations:Math.max(200,variants.length)}:
        {reference:variants[0].request,variants:variants.map(v=>({name:v.name,inputs:v.request})),allowed_paths:[],invariant_paths:[],required_resolved_paths:[]};
      q('[data-analysis-input]').value=JSON.stringify(payload,null,2);analysisDrafts[analysis]=q('[data-analysis-input]').value;
      status(analysis==='campaign_compare'?'Requests copied. Specify allowed scalar paths and protected invariant paths before comparison.':'Branches copied. Review priorities and total iterations before allocation.');
    });
    q('[data-analyse]').onclick=()=>perform(async()=>{
      q('[data-analysis-result]').textContent='';
      const selected=analysis;const payload=JSON.parse(q('[data-analysis-input]').value);if(!payload||Array.isArray(payload)||typeof payload!=='object')throw new Error('Analysis input must be a JSON object.');
      const result=await action(selected,payload);
      if(analysis===selected)q('[data-analysis-result]').textContent=JSON.stringify(result,null,2);
      status('Analysis returned. No solver was started and no physical validity was certified.');
    });
    analysisView();
  }
  if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot,{once:true});else boot();
})();
(() => {
  'use strict';
  function boot(){
    const host=document.querySelector('#s_opt > .sect-b');
    if(!host||document.getElementById('savedRunReplay'))return;
    const open=document.createElement('button');open.type='button';open.textContent='Replay saved runs';host.prepend(open);
    const dialog=document.createElement('dialog');dialog.id='savedRunReplay';dialog.setAttribute('aria-labelledby','savedRunReplayTitle');
    dialog.innerHTML=`<header><div><h2 id="savedRunReplayTitle">Saved-run comparison</h2><p data-description>Load saved renderings to replay and compare runs.</p></div><button type="button" data-close aria-label="Close saved-run comparison">Close</button></header>
      <div class="replay-toolbar"><button type="button" data-jobs>Load saved jobs</button><label>Jobs <select data-job-selection multiple size="2" aria-label="Saved jobs to compare"></select></label><button type="button" data-prepare disabled>Compare selected jobs</button><button type="button" data-load>Load saved-run bundle</button><input type="file" data-files accept="application/json,image/png,image/jpeg,image/webp" multiple hidden>
      <label>View 1 <select data-run aria-label="First replay run"></select></label><label>View 2 <select data-compare aria-label="Second replay run"></select></label><label>View 3 <select data-third-run aria-label="Third replay run"></select></label><label>View 4 <select data-fourth-run aria-label="Fourth replay run"></select></label>
      <label><input type="checkbox" data-smooth aria-label="Smooth transitions">Smooth transitions</label><label>Speed <select data-speed aria-label="Replay speed"><option value="120">0.12 s / epoch</option><option value="500" selected>0.5 s / epoch</option><option value="1500">1.5 s / epoch</option></select></label></div>
      <details style="padding:4px 18px"><summary>Shared camera and solid cutting settings</summary><textarea data-render-settings aria-label="Comparison render settings" rows="3" style="width:100%;box-sizing:border-box">{"width_px":960,"height_px":540,"quality":"preview","background":"white"}</textarea><p>Camera settings apply to every view. Cutting settings apply to solids.</p></details>
      <div class="replay-stage"><div class="replay-empty" role="status">Load saved runs.</div><figure data-first hidden><figcaption></figcaption><img alt="First saved run"><div class="replay-scale"><span></span><i></i></div></figure><figure data-second hidden><figcaption></figcaption><img alt="Second saved run"><div class="replay-scale"><span></span><i></i></div></figure><figure data-third hidden><figcaption></figcaption><img alt="Third saved run"><div class="replay-scale"><span></span><i></i></div></figure><figure data-fourth hidden><figcaption></figcaption><img alt="Fourth saved run"><div class="replay-scale"><span></span><i></i></div></figure></div>
      <footer><button type="button" data-previous aria-label="Previous replay epoch">◀</button><button type="button" data-play>Play replay</button><button type="button" data-next aria-label="Next replay epoch">▶</button><input type="range" data-epoch aria-label="Replay epoch" min="0" max="0" value="0"><strong data-epoch-label>Epoch = -</strong><span data-status role="status" aria-live="polite">No saved runs loaded.</span></footer>`;
    const style=document.createElement('style');style.textContent=`
      #savedRunReplay{width:96vw;max-width:1920px;height:96vh;max-height:96vh;padding:0;border:1px solid #ccd8d2;border-radius:8px;background:#fff;color:#233b34;font:13px Arial,sans-serif}
      #savedRunReplay[open]{display:flex;flex-direction:column}#savedRunReplay::backdrop{background:#152b2488}
      #savedRunReplay header{display:flex;justify-content:space-between;align-items:center;padding:12px 18px;border-bottom:1px solid #dbe4df}#savedRunReplay h2{margin:0;font-size:18px}#savedRunReplay p{margin:5px 0 0;font-size:12px}
      #savedRunReplay button,#savedRunReplay select{width:auto;padding:7px 11px;border:1px solid #ccd8d2;border-radius:5px;background:#f8fbf9;color:#234c3d;font:12px Arial,sans-serif}#savedRunReplay button:disabled{opacity:.4}
      #savedRunReplay .replay-toolbar{display:flex;flex-wrap:wrap;align-items:center;gap:12px;padding:10px 18px;border-bottom:1px solid #dbe4df}#savedRunReplay label{display:flex;align-items:center;gap:6px;margin:0}
      #savedRunReplay .replay-stage{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));grid-template-rows:minmax(0,1fr);gap:1px;background:#dbe4df;min-height:0;flex:1}#savedRunReplay .replay-empty{grid-column:1/-1;display:grid;place-items:center;background:#fff;color:#60786b;padding:20px;text-align:center}#savedRunReplay figure{margin:0;min-width:0;min-height:0;position:relative;overflow:hidden;background:#fff}#savedRunReplay [hidden]{display:none!important}#savedRunReplay .replay-toolbar select{max-width:230px}#savedRunReplay .replay-stage[data-views="1"]{grid-template-columns:minmax(0,1fr)}#savedRunReplay .replay-stage[data-views="3"],#savedRunReplay .replay-stage[data-views="4"]{grid-template-rows:repeat(2,minmax(0,1fr))}
      #savedRunReplay .replay-outgoing{position:absolute;inset:0;z-index:1;pointer-events:none}#savedRunReplay .replay-scale{z-index:2}#savedRunReplay [data-smooth]{width:auto;margin:0;accent-color:#367967}
      #savedRunReplay figcaption{position:absolute;top:12px;left:16px;background:#ffffffeb;padding:6px 9px;border:1px solid #dbe4df;border-radius:4px;z-index:2;max-width:calc(100% - 32px);box-sizing:border-box;overflow-wrap:anywhere}#savedRunReplay img{width:100%;height:100%;object-fit:contain}
      #savedRunReplay .replay-scale{position:absolute;bottom:15px;left:20px;text-align:center;font-size:11px}#savedRunReplay .replay-scale i{display:block;height:4px;border:1px solid #294b3d;border-top:0;margin-top:3px}
      #savedRunReplay footer{display:flex;align-items:center;gap:9px;padding:12px 18px;border-top:1px solid #dbe4df}#savedRunReplay [data-play]{background:#367967;color:#fff}#savedRunReplay [data-epoch]{flex:1;min-width:60px;accent-color:#367967}#savedRunReplay [data-status]{font-size:11px;color:#60786b;max-width:200px}#savedRunReplay [data-epoch-label]{white-space:nowrap}
      @media(max-width:600px){#savedRunReplay footer{flex-wrap:wrap}#savedRunReplay [data-epoch]{flex:1 1 100px}#savedRunReplay [data-status]{flex-basis:100%;max-width:none}#savedRunReplay [data-play]{white-space:nowrap}}
    `;
    document.head.append(style);document.body.append(dialog);
    const q=s=>dialog.querySelector(s),selectors=['[data-run]','[data-compare]','[data-third-run]','[data-fourth-run]'],panes=['[data-first]','[data-second]','[data-third]','[data-fourth]'];let runs=[],epochs=[],position=0,playing=false,timer=null,generation=0,request=0,urls=[],cache=new Map(),returnFocus=null,importGeneration=0;
    const transitions=new Map(),reducedMotion=window.matchMedia('(prefers-reduced-motion: reduce)');
    function clearTransitions(){for(const {animation,overlay} of transitions.values()){animation.cancel();overlay.remove()}transitions.clear()}
    function updateImage(pane,frame,run){
      const img=pane.querySelector('img:not(.replay-outgoing)'),active=transitions.get(pane);
      if(active){active.animation.cancel();active.overlay.remove();transitions.delete(pane)}
      const fade=q('[data-smooth]').checked&&!reducedMotion.matches&&!pane.hidden&&pane.dataset.replayRun===run.id&&img.getAttribute('src')&&img.src!==frame.image;
      const overlay=fade?img.cloneNode(false):null;
      img.src=frame.image;img.alt=run.label+' saved rendering at epoch '+frame.epoch;pane.dataset.replayRun=run.id;
      if(!overlay)return;
      overlay.className='replay-outgoing';overlay.alt='';overlay.setAttribute('aria-hidden','true');pane.append(overlay);
      const animation=overlay.animate([{opacity:1},{opacity:0}],{duration:Math.min(180,Number(q('[data-speed]').value)*0.65),easing:'ease-in-out'});
      const finish=()=>{overlay.remove();if(transitions.get(pane)?.overlay===overlay)transitions.delete(pane)};
      transitions.set(pane,{animation,overlay});animation.onfinish=finish;animation.oncancel=finish;
    }
    q('[data-smooth]').onchange=()=>{if(!q('[data-smooth]').checked)clearTransitions()};
    reducedMotion.addEventListener('change',()=>{if(reducedMotion.matches)clearTransitions()});
    function stop(clear=true){playing=false;generation++;request++;clearTimeout(timer);timer=null;q('[data-play]').textContent='Play replay';if(clear)clearTransitions()}
    function selected(){return selectors.map(selector=>runs.find(run=>run.id===q(selector).value))}
    function availability(){const ready=epochs.length>0;for(const s of ['[data-play]','[data-epoch]'])q(s).disabled=!ready;q('[data-previous]').disabled=!ready||position===0;q('[data-next]').disabled=!ready||position===epochs.length-1}
    async function replayAction(action,payload={}){const response=await fetch('/v1/agent/action',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({action,payload})});const reply=await response.json();if(!response.ok||reply.ok===false)throw new Error((reply.problems||[reply.error||'Service request failed.']).join('\n'));return reply.result??reply;}
    function imageFor(frame){
      const key=frame.source?JSON.stringify([frame.source,frame.render_settings]):frame.image;
      if(!cache.has(key))cache.set(key,(async()=>{
        if(frame.source&&!frame.image){const result=await replayAction('render_3d',{...frame.render_settings,source:frame.source});if(result.image?.mime_type!=='image/png'||typeof result.image.data_base64!=='string')throw new Error('The saved epoch has no PNG rendering.');frame.image='data:image/png;base64,'+result.image.data_base64;frame.source_identity=result.source;}
        return await new Promise((resolve,reject)=>{const img=new Image();img.onload=()=>img.decode().then(()=>resolve(img),reject);img.onerror=()=>reject(new Error('A saved rendering could not be loaded.'));img.src=frame.image});
      })().catch(error=>{cache.delete(key);throw error}));return cache.get(key);
    }
    function scale(pane,frame){const bar=pane.querySelector('.replay-scale');bar.hidden=!frame.scale_bar;if(!frame.scale_bar)return;const img=pane.querySelector('img'),r=img.getBoundingClientRect();bar.querySelector('span').textContent=frame.scale_bar.label;bar.querySelector('i').style.width=(frame.scale_bar.length_px*Math.min(r.width/img.naturalWidth,r.height/img.naturalHeight))+'px'}
    async function show(target){
      if(!epochs.length)return false;
      const token=++request,pair=selected(),epoch=epochs[target],frames=pair.map(run=>run?.frames.find(frame=>frame.epoch===epoch));
      try{await Promise.all(frames.filter(Boolean).map(imageFor));}catch(error){if(token===request){stop();position=target;q('[data-epoch]').value=target;for(const pane of dialog.querySelectorAll('figure'))pane.hidden=true;q('[data-epoch-label]').textContent='Epoch = '+epoch+' unavailable';q('[data-status]').textContent=error.message;q('.replay-empty').hidden=false;q('.replay-empty').textContent='Saved rendering unavailable.';availability()}return false}
      if(token!==request||!dialog.open)return false;
      position=target;q('.replay-empty').hidden=true;
      q('.replay-stage').dataset.views=String(pair.filter(Boolean).length);
      for(let i=0;i<panes.length;i++){const pane=q(panes[i]);pane.hidden=!pair[i];if(!pair[i])continue;pane.querySelector('figcaption').textContent=pair[i].label;updateImage(pane,frames[i],pair[i]);}
      q('[data-epoch]').value=position;q('[data-epoch-label]').textContent='Epoch = '+epoch;q('[data-status]').textContent=epochs.length+' shared saved epochs';availability();
      requestAnimationFrame(()=>{if(token===request&&dialog.open)frames.forEach((frame,i)=>{if(frame)scale(q(panes[i]),frame)})});
      if(target+1<epochs.length)for(const run of pair.filter(Boolean))imageFor(run.frames.find(frame=>frame.epoch===epochs[target+1])).catch(()=>{});
      return true;
    }
    function choose(){stop();for(const selector of panes)delete q(selector).dataset.replayRun;const old=epochs[position],pair=selected();epochs=pair[0]?pair[0].frames.map(f=>f.epoch).filter(e=>pair.filter(Boolean).every(run=>run.frames.some(f=>f.epoch===e))):[];q('[data-epoch]').max=Math.max(0,epochs.length-1);position=Math.max(0,epochs.indexOf(old));availability();if(!epochs.length){q('[data-status]').textContent='These runs have no common saved epoch.';for(const pane of dialog.querySelectorAll('figure'))pane.hidden=true;q('[data-epoch-label]').textContent='Epoch = -';q('.replay-empty').hidden=false;q('.replay-empty').textContent='No shared saved epochs.';return}void show(position)}
    function validate(bundle,files){
      if(bundle?.schema!=='implexity-saved-run-replay/1'||!Array.isArray(bundle.runs)||!bundle.runs.length)throw new Error('Expected an implexity-saved-run-replay/1 bundle.');
      const ids=new Set(),newUrls=[];
      try{const validated=bundle.runs.map(run=>{
        if(typeof run.id!=='string'||!run.id.trim()||ids.has(run.id)||typeof run.label!=='string'||!run.label.trim()||!Array.isArray(run.frames)||!run.frames.length)throw new Error('Each run requires a unique ID, a label and saved frames.');ids.add(run.id);
        const seen=new Set();const frames=run.frames.map(frame=>{
          if(!Number.isSafeInteger(frame.epoch)||frame.epoch<0||seen.has(frame.epoch))throw new Error('Saved epochs must be distinct nonnegative integers.');seen.add(frame.epoch);
          if(typeof frame.image!=='string')throw new Error('Each epoch requires an image.');
          let image=frame.image;
          if(!/^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(image)){
            const file=files.get(image);if(!file||!/^image\/(png|jpeg|webp)$/.test(file.type))throw new Error('Select the local image files referenced by the bundle.');image=URL.createObjectURL(file);newUrls.push(image);
          }
          let scale_bar=null;if(frame.scale_bar!==undefined){const s=frame.scale_bar;if(!s||typeof s.label!=='string'||!s.label.trim()||!Number.isFinite(s.length_px)||s.length_px<=0)throw new Error('Scale bars require a label and a positive image-pixel length.');scale_bar={label:s.label,length_px:s.length_px}}
          return {epoch:frame.epoch,image,scale_bar};
        }).sort((a,b)=>a.epoch-b.epoch);return {id:run.id,label:run.label,frames};
      });return {runs:validated,urls:newUrls};}catch(error){newUrls.forEach(url=>URL.revokeObjectURL(url));throw error}
    }
    q('[data-files]').onchange=async event=>{
      stop();const importToken=++importGeneration;q('[data-load]').disabled=true;q('[data-status]').textContent='Loading saved runs…';
      try{const selectedFiles=[...event.target.files],files=new Map(selectedFiles.map(f=>[f.name,f]));if(files.size!==selectedFiles.length)throw new Error('Selected filenames must be unique.');const documents=[...files.values()].filter(f=>f.name.toLowerCase().endsWith('.json'));if(documents.length!==1)throw new Error('Select one bundle JSON and its referenced images.');const text=await documents[0].text();if(importToken!==importGeneration||!dialog.open)return;const bundle=JSON.parse(text),next=validate(bundle,files);const initialRuns=next.runs.slice(0,4),initial=initialRuns[0].frames.find(frame=>initialRuns.every(run=>run.frames.some(other=>other.epoch===frame.epoch)));try{if(initial)await Promise.all(initialRuns.map(run=>imageFor(run.frames.find(frame=>frame.epoch===initial.epoch))))}catch(error){next.urls.forEach(url=>URL.revokeObjectURL(url));throw error}if(importToken!==importGeneration||!dialog.open){next.urls.forEach(url=>URL.revokeObjectURL(url));return}
        urls.forEach(url=>URL.revokeObjectURL(url));urls=next.urls;cache.clear();runs=next.runs;selectors.forEach((selector,i)=>{q(selector).replaceChildren(...(i?[new Option('Hidden','')]:[]));for(const run of runs)q(selector).append(new Option(run.label,run.id));q(selector).value=runs[i]?.id||''});q('[data-description]').textContent=typeof bundle.description==='string'?bundle.description:'Saved renderings · synchronized by epoch';position=0;epochs=[];choose();
      }catch(error){if(importToken===importGeneration)q('[data-status]').textContent=error.message}finally{if(importToken===importGeneration){event.target.value='';q('[data-load]').disabled=false}}
    };
    function schedule(g){timer=setTimeout(async()=>{if(!playing||g!==generation)return;const success=await show(position===epochs.length-1?0:position+1);if(!playing||g!==generation)return;if(!success||position===epochs.length-1){stop(false);return}schedule(g)},Number(q('[data-speed]').value))}
    q('[data-play]').onclick=async()=>{if(playing){stop();return}if(!epochs.length)return;playing=true;const g=++generation;q('[data-play]').textContent='Pause replay';const success=await show(position===epochs.length-1?0:position);if(!playing||g!==generation)return;if(success)schedule(g);else stop()};
    q('[data-previous]').onclick=()=>{stop();void show(Math.max(0,position-1))};q('[data-next]').onclick=()=>{stop();void show(position===epochs.length-1?0:position+1)};q('[data-epoch]').oninput=event=>{stop();void show(Number(event.target.value))};selectors.forEach(selector=>{q(selector).onchange=choose});
    q('[data-speed]').onchange=()=>{if(playing){clearTimeout(timer);generation++;request++;schedule(generation)}};
    q('[data-jobs]').onclick=async()=>{q('[data-jobs]').disabled=true;try{const state=await replayAction('inspect_state');const jobs=state.optimization_jobs?.jobs||[];const picker=q('[data-job-selection]');picker.replaceChildren();for(const job of jobs){const id=job.job_id||job.id;if(typeof id==='string')picker.append(new Option((job.name||id)+' / '+(job.status||''),id));}q('[data-prepare]').disabled=!picker.options.length;q('[data-status]').textContent=picker.options.length?'Select one to four saved jobs.':'No saved optimization jobs.';}catch(error){q('[data-status]').textContent=error.message;}finally{q('[data-jobs]').disabled=false;}};
    q('[data-prepare]').onclick=async()=>{stop();const token=++importGeneration;q('[data-prepare]').disabled=true;try{const ids=[...q('[data-job-selection]').selectedOptions].map(option=>option.value);if(!ids.length||ids.length>4)throw new Error('Select one to four saved jobs.');const render_settings=JSON.parse(q('[data-render-settings]').value);q('[data-status]').textContent='Reading saved epochs…';const comparison=await replayAction('prepare_run_comparison',{job_ids:ids,render_settings});if(token!==importGeneration||!dialog.open)return;if(comparison.schema!=='implexity-saved-job-comparison/1'||!Array.isArray(comparison.runs))throw new Error('Invalid saved-job comparison reply.');urls.forEach(url=>URL.revokeObjectURL(url));urls=[];cache.clear();runs=comparison.runs;selectors.forEach((selector,i)=>{q(selector).replaceChildren(...(i?[new Option('Hidden','')]:[]));for(const run of runs)q(selector).append(new Option(run.label,run.id));q(selector).value=runs[i]?.id||''});q('[data-description]').textContent='Saved job epochs rendered from their original geometry.';position=0;epochs=[];choose();}catch(error){if(token===importGeneration)q('[data-status]').textContent=error.message;}finally{q('[data-prepare]').disabled=false;}};
    q('[data-load]').onclick=()=>q('[data-files]').click();q('[data-close]').onclick=()=>dialog.close();dialog.addEventListener('close',()=>{stop();importGeneration++;q('[data-load]').disabled=false;returnFocus?.focus({preventScroll:true})});dialog.addEventListener('cancel',stop);
    open.onclick=()=>{returnFocus=document.activeElement;dialog.showModal();if(epochs.length)void show(position)};
    window.addEventListener('resize',()=>{if(dialog.open&&epochs.length)selected().forEach((run,i)=>{if(run)scale(q(panes[i]),run.frames.find(frame=>frame.epoch===epochs[position]))})});availability();
  }
  if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot,{once:true});else boot();
})();
