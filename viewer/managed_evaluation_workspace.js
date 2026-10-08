// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

import {renderSolverRecovery} from "./numerical_progress.js";
(() => {
  'use strict';
  function boot(){
    const host=document.querySelector('#s_opt > .sect-b');
    if(!host||document.getElementById('implexity-managed-workspace'))return;
    const panel=document.createElement('details');panel.id='implexity-managed-workspace';
    panel.innerHTML=`<summary>Managed single evaluations</summary>
      <p>Run preflight, a result evaluation or sensitivity without a topology-optimisation trajectory. These operations can execute physics. Review the request and avoid overlapping an existing computation.</p>
      <label>Operation <select data-kind><option value="start_managed_preflight">Preflight</option><option value="start_managed_evaluation">Evaluate results</option><option value="start_managed_sensitivity">Evaluate sensitivity</option></select></label>
      <button type="button" data-capture>Copy current optimisation request</button>
      <label>Request JSON <textarea data-request rows="8" spellcheck="false"></textarea></label>
      <p>Advanced request editor: result evaluations may specify fields; sensitivity requests may specify response or sensitivity_responses. The same public MCP validators apply.</p>
      <button type="button" data-start>Start selected operation…</button>
      <label>Operation ID <input data-id autocomplete="off" spellcheck="false"></label>
      <button type="button" data-inspect>Inspect operation</button>
      <button type="button" data-cancel disabled>Cancel operation…</button>
      <button type="button" data-fields disabled>Show returned fields</button>
      <p data-status role="status" aria-live="polite">No operation attached. Nothing has been started.</p>
      <pre data-result aria-label="Managed operation response"></pre>`;
    host.append(panel);
    const q=s=>panel.querySelector(s),allowed=new Set(['start_managed_preflight','start_managed_evaluation','start_managed_sensitivity']);
    let busy=false,observed=null,uncertain=false,exactResult=null;
    const status=s=>{q('[data-status]').textContent=s;};
    const storageKey='implexity-managed-operation-v1';
    function remember(id,pending){try{window.sessionStorage?.setItem(storageKey,JSON.stringify({id,pending}));}catch(_){}}
    try{const saved=JSON.parse(window.sessionStorage?.getItem(storageKey)||'null');if(saved?.pending===true){uncertain=true;q('[data-id]').value=/^[0-9a-f]{48}$/.test(saved.id)?saved.id:'';status('An earlier operation may still be running. Inspect its ID before another start.');}}catch(_){}
    function controls(){
      panel.querySelectorAll('button').forEach(b=>b.disabled=busy);
      q('[data-start]').disabled=busy||uncertain||(observed&&!observed.terminal);
      q('[data-cancel]').disabled=busy||!observed||!observed.cancelable||q('[data-id]').value.trim()!==observed.operation_id;
      q('[data-fields]').disabled=busy||!exactResult||q('[data-id]').value.trim()!==observed?.operation_id;
    }
    async function action(name,payload){
      const response=await fetch('/v1/agent/action',{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json'},body:JSON.stringify({action:name,payload})});
      const data=await response.json();
      if(!response.ok||data.ok===false){
        const error=new Error([data.error||data.message||'Managed operation rejected',...(Array.isArray(data.problems)?data.problems:[])].join(' '));
        if(data.solver_recovery) renderSolverRecovery(q('[data-status]'),data.solver_recovery);
        error.managedStartNotAttempted=data.managed_start_outcome==='not_started';
        throw error;
      }
      return data.result??data;
    }
    async function perform(fn){if(busy)return;busy=true;controls();try{await fn();}catch(error){status(error.message||String(error));}finally{busy=false;controls();}}
    function operationId(){const id=q('[data-id]').value.trim();if(!/^[0-9a-f]{48}$/.test(id))throw new Error('Enter the 48-character operation ID returned by MCP.');return id;}
    function show(result,expected){
      const s=result?.status;
      if(!s||!/^[0-9a-f]{48}$/.test(s.operation_id)||typeof s.terminal!=='boolean'||typeof s.cancelable!=='boolean'||(expected&&s.operation_id!==expected))throw new Error('Invalid operation status. Do not submit another start; inspect the original operation.');
      observed=s;uncertain=false;q('[data-id]').value=s.operation_id;
      exactResult=s.terminal&&s.state==='succeeded'&&result.exact_result&&typeof result.exact_result==='object'&&!Array.isArray(result.exact_result)?result.exact_result:null;
      remember(s.operation_id,!s.terminal);
      q('[data-result]').textContent=JSON.stringify(result,null,2);
      renderSolverRecovery(q('[data-status]'),exactResult?.solver_recovery||result?.solver_recovery||null);
      status(`${s.state} · ${s.phase}${s.terminal_reason?' · '+s.terminal_reason:''}. ${s.terminal?'Operation finished.':'Use Inspect to refresh; closing this panel does not stop it.'}`);
    }
    q('[data-id]').oninput=controls;
    q('[data-fields]').onclick=()=>perform(async()=>{
      if(!exactResult||q('[data-id]').value.trim()!==observed?.operation_id)throw new Error('Inspect the successful operation first.');
      if(typeof window.ImplexityFieldStream?.ingestResponse!=='function')throw new Error('The result viewer is not ready.');
      const shown=window.ImplexityFieldStream.ingestResponse(exactResult);
      status(shown?'Returned fields are available in the results viewer. They belong to this operation, not a new evaluation.':'This operation returned no displayable field streams or exact arrays. Its response remains available below.');
    });
    q('[data-capture]').onclick=()=>perform(async()=>{
      if(typeof window.implexityCurrentOptimizationRequest!=='function')throw new Error('The optimisation editor is not ready.');
      q('[data-request]').value=JSON.stringify(window.implexityCurrentOptimizationRequest({report:true}),null,2);status('Copied the current request. Review it before starting.');
    });
    q('[data-start]').onclick=()=>perform(async()=>{
      if(uncertain||(observed&&!observed.terminal))throw new Error('Inspect the existing operation before starting another.');
      const name=q('[data-kind]').value;if(!allowed.has(name))throw new Error('Choose a supported managed operation.');
      const request=JSON.parse(q('[data-request]').value);if(!request||Array.isArray(request)||typeof request!=='object')throw new Error('Request must be a JSON object.');
      if(!window.confirm('Start this managed operation? It may execute physics using the current model and request. Ensure no other numerical solver is running.'))return;
      uncertain=true;exactResult=null;q('[data-result]').textContent='';
      remember('',true);
      try{show(await action(name,{request}));}catch(error){
        if(error.managedStartNotAttempted===true){uncertain=false;remember('',false);throw new Error(`${error.message} No operation was started. Correct the request before trying again.`);}
        throw new Error(`${error.message} Start outcome is unconfirmed. Do not retry blindly; recover and inspect its operation ID.`);
      }
    });
    q('[data-inspect]').onclick=()=>perform(async()=>{const id=operationId();show(await action('inspect_managed_evaluation',{operation_id:id}),id);});
    q('[data-cancel]').onclick=()=>perform(async()=>{
      const id=operationId();if(!observed||observed.operation_id!==id||!observed.cancelable)throw new Error('Inspect this operation and confirm it is cancellable first.');
      if(!window.confirm('Request cancellation of this managed operation?'))return;
      show(await action('cancel_managed_evaluation',{operation_id:id}),id);
    });
    controls();
  }
  if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot,{once:true});else boot();
})();
