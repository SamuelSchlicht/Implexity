// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

 
(function (global) {
  'use strict';
  const ACTION_PATH = '/v1/agent/action';
  const MAX_BYTES = 64 * 1024 * 1024;
  const copy = value => structuredClone(value);
  const own = (value, key) => Object.prototype.hasOwnProperty.call(value, key);
  const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);

  function parseValue(text) {
    if (new TextEncoder().encode(text).length > MAX_BYTES) throw new Error('Arguments exceed 64 MiB.');
     
    const stack = []; let index = 0;
    function stringToken() {
      const start = index++;
      while (index < text.length) {
        if (text[index] === '\\') { index += 2; continue; }
        if (text[index++] === '"') return JSON.parse(text.slice(start, index));
      }
      throw new Error('Unterminated JSON string.');
    }
    while (index < text.length) {
      const char = text[index];
      if (char === '"') {
        const key = stringToken();
        let next = index; while (/\s/.test(text[next] || '') && next < text.length) next++;
        if (text[next] === ':' && stack.at(-1)?.keys) {
          if (stack.at(-1).keys.has(key)) throw new Error(`Duplicate JSON key: ${key}`);
          stack.at(-1).keys.add(key);
        }
      } else {
        if (char === '{') stack.push({keys: new Set()});
        else if (char === '[') stack.push({});
        else if (char === '}' || char === ']') stack.pop();
        if (stack.length > 128) throw new Error('Arguments exceed the supported nesting depth.');
        index++;
      }
    }
    const result = JSON.parse(text);
    function finite(value) {
      if (typeof value === 'number' && !Number.isFinite(value))
        throw new Error('Numbers must be finite.');
      if (value && typeof value === 'object') Object.values(value).forEach(finite);
    }
    finite(result);
    return result;
  }

  function parsePayload(text) {
    const result = parseValue(text);
    if (!object(result)) throw new Error('Command arguments must be a JSON object.');
    return result;
  }

  function catalogue(manifest) {
    if (manifest?.schema !== 'implexity-agent-tool-manifest/1' || !Array.isArray(manifest.tools))
      throw new Error('The service did not return its public command catalogue.');
    const names = new Set();
    return manifest.tools.map(row => {
      if (!object(row) || typeof row.action !== 'string' || !/^[a-z][a-z0-9_]*$/.test(row.action) ||
          row.name !== 'implexity_' + row.action || row.input_schema?.type !== 'object' ||
          row.invoke?.method !== 'POST' || row.invoke?.path !== ACTION_PATH ||
          row.invoke?.envelope?.action !== row.action || names.has(row.action))
        throw new Error('The service command binding is malformed or duplicated.');
      names.add(row.action);
      return {...copy(row), label: row.label || row.action.replaceAll('_', ' '),
        allowed: row.allowed === true, mutates: row.mutates !== false,
        requires_model: row.requires_model !== false};
    });
  }

  function visibleCommands(rows, query = '', permittedOnly = false) {
    const words = query.toLowerCase().trim().split(/\s+/).filter(Boolean);
    return rows.filter(row => (!permittedOnly || row.allowed) && words.every(word =>
      `${row.action} ${row.label} ${row.description || ''} ${row.permission || ''}`.toLowerCase().includes(word)));
  }

  async function executeCommand(tool, payload, io) {
    if (!tool?.allowed) throw new Error('This command is not permitted by the current service policy.');
     
    const seen = new Set();
    function checkNumbers(value) {
      if (typeof value === 'number' && !Number.isFinite(value)) throw new Error('Numbers must be finite.');
      if (value && typeof value === 'object') {
        if (seen.has(value)) throw new Error('Command arguments cannot contain a cycle.');
        seen.add(value); Object.values(value).forEach(checkNumbers); seen.delete(value);
      }
    }
    checkNumbers(payload);
    const exact = parsePayload(JSON.stringify(payload));
    const envelope = {action: tool.action, payload: exact};
     
    await io.validate(copy(envelope));
    if (tool.mutates && !(await io.confirm(copy(envelope)))) return {status: 'cancelled', submitted: false};
    let response;
    try { response = await io.submit(copy(envelope)); }
    catch (error) {
       
      let refreshError = null;
      try { await io.refresh(); } catch (failure) { refreshError = failure.message; }
      return {status: error.refused ? 'refused' : 'unconfirmed', submitted: true,
        message: error.message, refresh_error: refreshError, response: error.response || null};
    }
    if (response?.ok !== true || response?.action !== tool.action) {
      let refreshError = null;
      try { await io.refresh(); } catch (error) { refreshError = error.message; }
      return {status: 'unconfirmed', submitted: true, response,
        message: 'The reply did not acknowledge this exact command.', refresh_error: refreshError};
    }
    let refreshError = null;
    if (tool.mutates) try { await io.refresh(); } catch (error) { refreshError = error.message; }
    return {status: 'succeeded', submitted: true, response, refresh_error: refreshError};
  }

  const IMAGE_MIME = /^image\/(png|jpeg|webp)$/;
  const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/;
  const DATA_URL = /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/]+={0,2}$/;

   
  function resultImages(value) {
    const images = []; const seen = new Set();
    function visit(node, path) {
      if (images.length >= 32 || node === null || typeof node !== 'object' || seen.has(node)) {
        if (typeof node === 'string' && DATA_URL.test(node) && images.length < 32)
          images.push({src: node, path: path || 'result', mime_type: node.slice(5, node.indexOf(';'))});
        return;
      }
      seen.add(node);
      if (object(node) && typeof node.data_base64 === 'string' && IMAGE_MIME.test(String(node.mime_type || '')) &&
          BASE64.test(node.data_base64)) {
        images.push({src: `data:${node.mime_type};base64,${node.data_base64}`, path: path || 'result',
          mime_type: node.mime_type, width_px: node.width_px, height_px: node.height_px, sha256: node.sha256});
        return;
      }
      for (const [key, item] of Object.entries(node)) visit(item, path ? `${path}.${key}` : key);
    }
    visit(value, '');
    return images;
  }

  const FILE_SCHEMA = 'implexity-inline-file/1';
  const FILE_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/;

   
  function resultFiles(value) {
    const files = []; const seen = new Set();
    function visit(node, path) {
      if (files.length >= 64 || node === null || typeof node !== 'object' || seen.has(node)) return;
      seen.add(node);
      if (object(node) && node.schema === FILE_SCHEMA && typeof node.data_base64 === 'string' &&
          BASE64.test(node.data_base64) && FILE_NAME.test(String(node.filename || '')) &&
          typeof node.mime_type === 'string') {
        files.push({path: path || 'result', filename: node.filename, mime_type: node.mime_type,
          bytes: node.bytes, sha256: node.sha256, data_base64: node.data_base64});
        return;
      }
      for (const [key, item] of Object.entries(node)) visit(item, path ? `${path}.${key}` : key);
    }
    visit(value, '');
    return files;
  }

  function fileBlob(file) {
    const binary = atob(file.data_base64); const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
    if (Number.isSafeInteger(file.bytes) && file.bytes !== bytes.length)
      throw new Error(`${file.filename} does not have its declared ${file.bytes} bytes.`);
    return new Blob([bytes], {type: file.mime_type});
  }

   
  function displayJSON(value) {
    return JSON.stringify(value, function (key, item) {
      if (key === 'data_base64' && typeof item === 'string' && IMAGE_MIME.test(String(this?.mime_type || '')))
        return `<${item.length} base64 characters shown as image above; complete in JSON download>`;
      if (key === 'data_base64' && typeof item === 'string' && this?.schema === FILE_SCHEMA)
        return `<${item.length} base64 characters; save the file above; complete in JSON download>`;
      if (typeof item === 'string' && item.length > 256 && DATA_URL.test(item))
        return `<${item.length}-character data URL shown as image above; complete in JSON download>`;
      return item;
    }, 2);
  }

  global.ImplexityAdvancedCommandCore = Object.freeze({parsePayload, parseValue, catalogue, visibleCommands, executeCommand, resultImages, resultFiles, fileBlob, displayJSON});
  function meshSceneDraft(entries) {
    if (!Array.isArray(entries) || entries.length < 1 || entries.length > 32) throw new Error('Select 1 to 32 binary STL files.');
    const mesh_data={}, meshes={}, layers=[];const lo=[Infinity,Infinity,Infinity],hi=[-Infinity,-Infinity,-Infinity];
    for (let index=0;index<entries.length;index++) {
      const bytes=new Uint8Array(entries[index].buffer);if(bytes.byteLength<84)throw new Error('A binary STL needs its header and triangle count.');
      const view=new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength), count=view.getUint32(80,true);
      if(!count || 84+50*count>bytes.byteLength)throw new Error('Invalid binary STL triangle count.');
      for(let k=0;k<count;k++)for(let vertex=0;vertex<3;vertex++)for(let axis=0;axis<3;axis++) {
        const value=view.getFloat32(84+50*k+12+12*vertex+4*axis,true);if(!Number.isFinite(value))throw new Error('STL coordinates must be finite.');
        lo[axis]=Math.min(lo[axis],value);hi[axis]=Math.max(hi[axis],value);
      }
      const id=`mesh_${index+1}`;let binary='';for(let start=0;start<bytes.length;start+=16384)binary+=String.fromCharCode(...bytes.subarray(start,start+16384));
      mesh_data[id]={data_base64:btoa(binary)};meshes[id]={stl:id,crease_deg:40};
      layers.push({mesh:id,color_rgb:index%2?[80,180,210]:[190,145,101],cap_rgb:index%2?[55,140,170]:[161,109,62]});
    }
    const span=Math.max(...hi.map((v,i)=>v-lo[i]),.1);for(let axis=0;axis<3;axis++)if(hi[axis]===lo[axis]){lo[axis]-=.05*span;hi[axis]+=.05*span;}
    return {scene:{schema:'implexity-mesh-scene/1',meshes,cameras:{isometric:{view_direction:[1,1,1],up:[0,0,1],frame_box_mm:[lo,hi],width_px:960}},
      views:[{name:'mesh_scene',camera:'isometric',layers,keep:[],remove:[],shadows:true,ambient_occlusion:true,outline:true}],render:{background_rgb:[255,255,255],supersample:1}},mesh_data,view:'mesh_scene'};
  }
  global.ImplexityMeshSceneImportCore=Object.freeze({meshSceneDraft});
  if (typeof document === 'undefined') return;
  const make = (tag, text = '', className = '') => {
    const node = document.createElement(tag); node.textContent = text; node.className = className; return node;
  };
  async function request(method, path, body) {
    const response = await fetch(path, {method, credentials: 'same-origin', cache: 'no-store',
      ...(body === undefined ? {} : {headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)})});
    let value;
    try { value = await response.json(); }
    catch (_) { throw new Error('The service reply could not be decoded. Command status may be unknown.'); }
    if (!response.ok || value?.ok === false) {
      const error = new Error([value?.error || `HTTP ${response.status}`, ...(value?.problems || [])].join(' · '));
      error.refused = response.status >= 400 && response.status < 500;
      error.response = value; throw error;
    }
    return value;
  }

   
  function viewportEditPending() {
    const interaction = global.ImplexityInteraction;
    return Boolean(interaction?.activeGesture || interaction?.pendingGesture || interaction?.selectionOperationPending);
  }

  class CommandPanel {
    constructor() {
      this.rows = []; this.drafts = new Map(); this.tool = null; this.busy = false; this.unconfirmed = false;
      this.build();
    }
    button(label, fn, id) {
      const button = make('button', label); button.type = 'button'; if (id) button.id = id;
      button.addEventListener('click', () => Promise.resolve().then(fn).catch(error => this.message(error.message, 'error')));
      return button;
    }
    build() {
      this.launch = this.button('Advanced commands…', () => this.open(), 'advancedCommandsOpen');
      this.launch.title = 'Search and use the same public commands and argument schemas as MCP.';
      this.toolbar = make('span', '', 'implexity-topbar-commands'); this.toolbar.id = 'implexityTopbarCommands';
      this.toolbar.append(this.launch); document.querySelector('#topbar').append(this.toolbar);
      this.dialog = make('dialog', '', 'advanced-commands'); this.dialog.id = 'advancedCommandsDialog';
      this.dialog.setAttribute('aria-labelledby', 'advancedCommandsTitle');
      const head = make('header'); const title = make('h2', 'Advanced commands'); title.id = 'advancedCommandsTitle';
      this.closeButton = this.button('Close', () => this.close(), 'advancedCommandsClose'); head.append(title, this.closeButton);
      const note = make('p', 'Public operations use the service’s existing permissions, validation and numerical runtime. Checking arguments is not physics preflight.', 'advanced-help');
      const body = make('div', '', 'advanced-command-columns'); const side = make('nav'); side.setAttribute('aria-label', 'Public commands');
      this.search = make('input'); this.search.type = 'search'; this.search.placeholder = 'Search commands…'; this.search.setAttribute('aria-label', 'Search public commands');
      this.search.addEventListener('input', () => this.renderList());
      this.count = make('p', '', 'advanced-help'); this.list = make('div', '', 'advanced-command-list');
      this.reloadButton = this.button('Refresh catalogue', () => this.reload(), 'advancedCommandsReload');
      side.append(this.search, this.count, this.reloadButton, this.list);
      const main = make('section', '', 'advanced-command-editor');
      this.heading = make('h3', 'Choose a command'); this.description = make('p', '', 'advanced-help');
      this.policy = make('p', '', 'advanced-command-policy');
      this.fields = make('div', '', 'advanced-command-fields'); this.fields.id = 'advancedCommandFields';
      const exact = make('details'); const summary = make('summary', 'Exact JSON arguments and extensions'); exact.append(summary);
      this.json = make('textarea'); this.json.id = 'advancedCommandJSON'; this.json.spellcheck = false; this.json.rows = 12;
      this.json.setAttribute('aria-label', 'Exact command arguments'); this.json.value = '{}';
      this.json.addEventListener('input', () => {this.draftChanged();});
      this.applyJSON = this.button('Update fields from JSON', () => {const value = parsePayload(this.json.value); this.renderFields(value); this.message('Fields updated. No command executed.');}, 'advancedCommandApplyJSON');
      exact.append(this.json, this.applyJSON, make('p', 'Nested objects and arrays remain exact JSON. Undisplayed extension fields are preserved, never dropped.', 'advanced-help'));
      const schemaDetails = make('details'); schemaDetails.append(make('summary', 'Service input schema'));
      this.schema = make('pre'); schemaDetails.append(this.schema);
      this.status = make('p', '', 'advanced-command-status'); this.status.id = 'advancedCommandStatus'; this.status.setAttribute('role', 'status'); this.status.setAttribute('aria-live', 'polite');
      this.confirmLabel = make('label', '', 'advanced-confirm'); this.confirm = make('input'); this.confirm.type = 'checkbox'; this.confirm.id = 'advancedCommandConfirm';
      this.confirmLabel.append(this.confirm, document.createTextNode(' I have reviewed the exact arguments and authorize this state-changing command.'));
      this.confirm.addEventListener('change', () => this.updateButtons());
      const buttons = make('div', '', 'advanced-command-actions');
      this.checkButton = this.button('Check arguments', () => this.check(), 'advancedCommandCheck');
      this.runButton = this.button('Execute command', () => this.run(), 'advancedCommandRun'); this.runButton.className = 'primary';
      this.refreshButton = this.button('Refresh saved state', async () => {await this.refreshSaved(); this.unconfirmed = false; this.updateButtons(); this.message('Saved state refreshed. An unconfirmed command was not repeated. Inspect it before issuing another mutation.');}, 'advancedCommandRefresh');
      buttons.append(this.checkButton, this.runButton, this.refreshButton);
      const resultDetails = make('details'); resultDetails.open = true; resultDetails.append(make('summary', 'Last command result'));
      this.images = make('div', '', 'advanced-command-images'); this.images.id = 'advancedCommandImages'; this.images.hidden = true;
      this.files = make('div', '', 'advanced-command-files'); this.files.id = 'advancedCommandFiles'; this.files.hidden = true;
      this.downloadButton = this.button('Download result JSON', () => this.download(), 'advancedCommandDownload'); this.downloadButton.hidden = true;
      this.result = make('pre'); this.result.id = 'advancedCommandResult'; resultDetails.append(this.images, this.files, this.downloadButton, this.result);
      this.meshFiles=make('input');this.meshFiles.type='file';this.meshFiles.accept='.stl';this.meshFiles.multiple=true;this.meshFiles.hidden=true;
      this.meshFiles.addEventListener('change',()=>this.importMeshes());
      this.meshImport=this.button('Load STL files for this scene…',()=>this.meshFiles.click(),'advancedMeshSceneImport');this.meshImport.hidden=true;
      main.append(this.heading, this.description, this.policy, this.meshImport, this.meshFiles, this.fields, exact, schemaDetails, this.confirmLabel, buttons, this.status, resultDetails);
      body.append(side, main); this.dialog.append(head, note, body); document.body.append(this.dialog);
      this.dialog.addEventListener('cancel', event => {event.preventDefault(); if (!this.busy) this.close();});
      this.dialog.addEventListener('close', () => this.returnFocus?.focus?.({preventScroll: true}));
      this.updateButtons();
    }
    message(text, level = 'info') {this.status.textContent = text; this.status.dataset.level = level;}
    showResult(value, action) {
      this.lastResult = {action, value};
      const images = resultImages(value);
      this.images.replaceChildren();
      for (const image of images) {
        const figure = make('figure', '', 'advanced-command-image'); const img = make('img');
        img.src = image.src; img.alt = `${action} ${image.path}`; img.loading = 'lazy';
        const size = Number.isSafeInteger(image.width_px) && Number.isSafeInteger(image.height_px) ? ` · ${image.width_px}×${image.height_px} px` : '';
        const caption = make('figcaption', `${image.path} · ${image.mime_type}${size}${image.sha256 ? ' · sha256 ' + String(image.sha256).slice(0, 12) + '…' : ''}`);
        const save = make('a', 'Save image'); save.href = image.src; save.download = `${action}-${image.path.replace(/[^A-Za-z0-9_.-]+/g, '_')}.${image.mime_type.split('/')[1]}`;
        caption.append(' · ', save); figure.append(img, caption); this.images.append(figure);
      }
      this.images.hidden = images.length === 0;
      const files = resultFiles(value);
      this.files.replaceChildren();
      for (const file of files) {
        const size = Number.isSafeInteger(file.bytes) ? ` · ${file.bytes.toLocaleString()} bytes` : '';
        const hash = file.sha256 ? ` · sha256 ${String(file.sha256).slice(0, 12)}…` : '';
        const save = this.button(`Save ${file.filename}`, () => this.saveFile(file));
        save.title = `${file.path} · ${file.mime_type}${size}${hash}`;
        const row = make('p', '', 'advanced-command-file'); row.append(save, make('span', `${file.mime_type}${size}${hash}`));
        this.files.append(row);
      }
      this.files.hidden = files.length === 0;
      this.downloadButton.hidden = value === undefined;
      this.result.textContent = displayJSON(value);
    }
    saveFile(file) {
      const url = URL.createObjectURL(fileBlob(file)); const link = make('a'); link.href = url;
      link.download = file.filename; document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 0);
    }
    download() {
      if (!this.lastResult) return;
      const blob = new Blob([JSON.stringify(this.lastResult.value, null, 2)], {type: 'application/json'});
      const url = URL.createObjectURL(blob); const link = make('a'); link.href = url;
      link.download = `${this.lastResult.action}-result.json`; document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 0);
    }
    updateButtons() {
      this.checkButton.disabled = this.busy || !this.tool?.allowed;
      this.runButton.disabled = this.busy || !this.tool?.allowed || (this.tool.mutates && (!this.confirm.checked || this.unconfirmed));
      this.closeButton.disabled = this.busy; this.reloadButton.disabled = this.busy; this.refreshButton.disabled = this.busy;
      this.json.disabled = this.busy; this.applyJSON.disabled = this.busy;
      this.confirm.disabled = this.busy;
      this.confirmLabel.hidden = !this.tool?.mutates;
       
      const app = document.querySelector('#app'); if (app) app.inert = this.busy || this.unconfirmed;
      this.launch.textContent = this.unconfirmed ? 'Refresh saved state…' : 'Advanced commands…';
      for (const node of this.list.querySelectorAll('button')) node.disabled = this.busy;
      for (const node of this.fields.querySelectorAll('input,select,textarea')) node.disabled = this.busy || node.dataset.omitted === 'true';
      global.ImplexityApplicationCase?.update?.();
    }
    draftChanged() {this.confirm.checked = false; this.updateButtons(); this.message('Arguments changed. Review before execution.');}
    async importMeshes() {
      if(this.busy)return;const files=Array.from(this.meshFiles.files||[]);if(!files.length)return;if(files.length>32){this.message('Choose at most 32 STL files.','error');this.meshFiles.value='';return;}
      this.busy=true;this.meshImport.disabled=true;this.updateButtons();let draft;
      try{const entries=await Promise.all(files.map(async file=>({name:file.name,buffer:await file.arrayBuffer()})));draft=meshSceneDraft(entries);}
      catch(error){this.message(error.message,'error');}
      finally{this.busy=false;this.meshImport.disabled=false;this.meshFiles.value='';this.updateButtons();}
      if(draft){this.select('render_mesh_scene',draft);this.message('STL coordinates are interpreted in mm. Review the camera, layer colours and keep/remove regions before rendering.');}
    }
    async reload() {
      this.rows = catalogue(await request('GET', '/v1/agent/tools'));
      this.renderList();
      if (this.tool) this.select(this.tool.action);
      return this.rows;
    }
    renderList() {
      const rows = visibleCommands(this.rows, this.search.value); this.list.replaceChildren();
      this.count.textContent = `${rows.length} of ${this.rows.length} commands · ${this.rows.filter(row => row.allowed).length} permitted`;
      for (const row of rows) {
        const button = this.button(row.label + (row.allowed ? '' : ' · permission required'), () => this.select(row.action));
        button.dataset.command = row.action; button.setAttribute('aria-pressed', String(this.tool?.action === row.action));
        button.title = row.action; this.list.append(button);
      }
      this.updateButtons();
    }
    select(action, payload) {
      if (this.busy) return;
      if (this.tool) this.drafts.set(this.tool.action, this.json.value);
      const tool = this.rows.find(row => row.action === action); if (!tool) throw new Error('Command is not in the live catalogue.');
      this.tool = tool; this.confirm.checked = false;
      this.meshImport.hidden=action!=='render_mesh_scene';
      this.heading.textContent = tool.label; this.description.textContent = tool.description || '';
      this.policy.textContent = `${tool.allowed ? 'Permitted' : 'Not permitted'} · ${tool.permission || 'service policy'} · ${tool.mutates ? 'Changes state' : 'Read / analysis'} · ${tool.requires_model ? 'Model required' : 'Available before creating a model'}`;
      this.schema.textContent = JSON.stringify(tool.input_schema, null, 2);
      this.json.value = payload === undefined ? this.drafts.get(action) || '{}' : JSON.stringify(payload, null, 2);
      try {this.renderFields(parsePayload(this.json.value));} catch (error) {this.fields.replaceChildren(); this.message(error.message, 'error');}
      this.renderList(); this.updateButtons();
    }
    renderFields(value) {
      this.fields.replaceChildren();
      const properties = this.tool?.input_schema.properties || {};
      const required = new Set(this.tool?.input_schema.required || []);
      for (const [key, schema] of Object.entries(properties)) {
        const holder = make('div', '', 'advanced-argument'); const label = make('label', key.replaceAll('_', ' ') + (required.has(key) ? ' *' : ''));
        const use = make('input'); use.type = 'checkbox'; use.checked = own(value, key); use.disabled = required.has(key); use.hidden = required.has(key);
        use.setAttribute('aria-label', `Include ${key}`); label.prepend(use);
        let input; const scalar = ['string', 'number', 'integer', 'boolean'].includes(schema.type) && !schema.oneOf && !schema.anyOf;
        if (schema.enum || schema.type === 'boolean') {
          input = make('select'); const options = schema.enum || [true, false];
          input.append(new Option('Choose…', ''));
          for (const item of options) input.append(new Option(String(item), JSON.stringify(item)));
          input.value = own(value, key) ? JSON.stringify(value[key]) : '';
        } else if (scalar) {
          input = make('input'); input.type = ['number', 'integer'].includes(schema.type) ? 'number' : key.includes('token') ? 'password' : 'text';
          if (input.type === 'number') input.step = schema.type === 'integer' ? '1' : 'any';
          input.value = own(value, key) ? String(value[key]) : '';
        } else {input = make('textarea'); input.rows = 3; input.spellcheck = false; input.value = own(value, key) ? JSON.stringify(value[key], null, 2) : '';}
        input.setAttribute('aria-label', `Argument ${key}`); if (required.has(key)) input.required = true; input.dataset.argument = key;
        input.dataset.omitted = String(!own(value, key) && !required.has(key)); input.disabled = input.dataset.omitted === 'true';
        const read = () => {
          if (input.tagName === 'SELECT') {if (!input.value) throw new Error(`Choose ${key}.`); return JSON.parse(input.value);}
          if (input.type === 'number') {if (!input.value.trim()) throw new Error(`Enter ${key}.`); const number = Number(input.value); if (!Number.isFinite(number) || (schema.type === 'integer' && !Number.isSafeInteger(number))) throw new Error(`Enter a valid ${schema.type} for ${key}.`); return number;}
          if (input.tagName === 'TEXTAREA') return parseValue(input.value);
          return input.value;
        };
        const write = () => {
          try {
            const next = parsePayload(this.json.value);
            if (!use.checked && !required.has(key)) delete next[key];
            else Object.defineProperty(next, key, {value: read(), writable: true, enumerable: true, configurable: true});
            this.json.value = JSON.stringify(next, null, 2); this.draftChanged();
            input.setCustomValidity('');
          } catch (error) {input.setCustomValidity(error.message); this.message(error.message, 'error'); this.confirm.checked = false; this.runButton.disabled = true;}
        };
        input.addEventListener('input', () => {if (required.has(key)) use.checked = true; write();});
        use.addEventListener('change', () => {input.dataset.omitted = String(!use.checked); input.disabled = !use.checked; write();});
        holder.append(label, input); if (schema.description) holder.append(make('small', schema.description)); this.fields.append(holder);
      }
      const extra = Object.keys(value).filter(key => !own(properties, key));
      if (extra.length) this.fields.append(make('p', `Additional arguments retained in Exact JSON: ${extra.join(', ')}`, 'advanced-help'));
      this.updateButtons();
    }
    async open(action, payload) {
      if (!this.dialog.open) {this.returnFocus = document.activeElement; this.dialog.showModal();}
      await this.reload();
      if (action) {this.search.value = ''; this.select(action, payload);}
      else if (!this.tool && this.rows.length) this.select('inspect_state');
      this.search.focus();
    }
    close() {if (!this.busy) this.dialog.close();}
    async check() {
      if (!this.tool?.allowed) throw new Error('Command is not permitted.');
      const payload = this.arguments(); this.busy = true; this.updateButtons();
      try {
        const result = await request('POST', '/v1/agent/validate', {action: this.tool.action, payload});
        this.showResult(result, `${this.tool.action}-check`);
        this.message('Arguments and permission checked. Model readiness, physical validity and current revisions are still checked by the actual operation.');
      } finally {this.busy = false; this.updateButtons();}
    }
    arguments() {
      for (const field of this.fields.querySelectorAll('input,textarea,select')) {
        if (!field.disabled && !field.checkValidity()) throw new Error(field.validationMessage || 'Correct the highlighted argument.');
      }
      return parsePayload(this.json.value);
    }
    async refreshSaved() {
      const status = await request('GET', '/v1/implicit/model');
      const model = await global.loadModel?.();
      if (!model || model.loaded !== status.loaded || (status.loaded && model.content_id !== status.content_id))
        throw new Error('The displayed model could not be synchronized.');
      if (status.loaded === true) await global.ImplexityManualHistory?.refresh?.();
       
      await global.implexityWorkbench?.reloadProviders?.();
      await global.ImplexityPhysicsPackages?.refresh?.();
      if (status.loaded === true) {
        await global.implexityWorkbench?.refresh?.();
        global.dispatchEvent(new CustomEvent('implexity:model-updated'));
      }
      global.implexityInvalidateOptimizationPreflight?.('Public command completed. Review the current model and physics before running.');
      return status;
    }
    async run() {
      if (this.busy || !this.tool?.allowed) return;
      if (this.tool.mutates && (!this.confirm.checked || this.unconfirmed)) throw new Error('Review and confirm the command first.');
      if (this.tool.mutates && viewportEditPending())
        throw new Error('Apply or cancel the viewport edit before issuing another mutation.');
      const payload = this.arguments(), tool = this.tool; this.busy = true; this.updateButtons(); this.message('Checking and executing one public command…');
      try {
        const outcome = await executeCommand(tool, payload, {
          validate: body => request('POST', '/v1/agent/validate', body),
          submit: body => request('POST', ACTION_PATH, body),
          confirm: async () => this.confirm.checked,
          refresh: () => this.refreshSaved(),
        });
        this.showResult(outcome.response ?? outcome, tool.action);
        if (outcome.status === 'succeeded') {
          this.unconfirmed = Boolean(outcome.refresh_error);
          this.message(outcome.refresh_error ? `Command succeeded. Saved-state refresh needs attention: ${outcome.refresh_error}. Do not repeat the command.` : 'Command succeeded. Its actual service result is shown below.', outcome.refresh_error ? 'warning' : 'success');
        } else {
          this.unconfirmed = outcome.status === 'unconfirmed' || Boolean(outcome.refresh_error);
          this.message(`${outcome.status === 'unconfirmed' ? 'Command outcome unconfirmed. It may already have completed. Do not repeat it blindly.' : 'Command refused or cancelled.'} ${outcome.message || ''}`, 'warning');
        }
        global.dispatchEvent(new CustomEvent('implexity:public-command-result', {detail: {action: tool.action, status: outcome.status}}));
      } finally {this.busy = false; this.confirm.checked = false; this.updateButtons();}
    }
  }
   
   
  function scalarText(value) {
    if (typeof value === 'number') return Number.isInteger(value) ? String(value) : Number(value.toPrecision(6)).toString();
    if (typeof value === 'boolean') return value ? 'yes' : 'no';
    if (value === null) return 'N/A';
    return String(value);
  }
  function flattenRow(value, prefix = '', out = {}) {
    for (const [key, item] of Object.entries(value || {})) {
      const name = prefix ? `${prefix}.${key}` : key;
      if (object(item)) flattenRow(item, name, out);
      else if (Array.isArray(item)) out[name] = item.length <= 6 && item.every(v => !object(v) && !Array.isArray(v)) ? `[${item.map(scalarText).join(', ')}]` : `${item.length} values`;
      else out[name] = scalarText(item);
    }
    return out;
  }
  function renderReport(value) {
    if (Array.isArray(value) && value.every(item => typeof item === 'string')) {
      const list = make('ul', '', 'application-case-list'); for (const item of value) list.append(make('li', item)); return list;
    }
    if (Array.isArray(value) && value.every(object)) {
      if (!value.length) return make('p', 'None reported.', 'advanced-help');
      const rows = value.map(item => flattenRow(item)); const columns = [...new Set(rows.flatMap(row => Object.keys(row)))];
      const wrap = make('div', '', 'application-case-table'); const table = make('table'); const head = make('tr');
      for (const column of columns) head.append(make('th', column.replaceAll('_', ' ')));
      table.append(head);
      for (const row of rows) {const tr = make('tr'); for (const column of columns) tr.append(make('td', row[column] ?? '')); table.append(tr);}
      wrap.append(table); return wrap;
    }
    if (object(value)) {
      const list = make('dl', '', 'application-case-facts');
      for (const [key, item] of Object.entries(flattenRow(value))) list.append(make('dt', key.replaceAll('_', ' ')), make('dd', item));
      return list;
    }
    return make('p', value === null || value === undefined ? 'Not reported.' : scalarText(value), 'advanced-help');
  }

  class ApplicationCasePanel {
    constructor(commands) {
      this.commands = commands; this.case = null; this.last = null; this.busy = false;
      this.startUncertain = false; this.startedJob = null;
      try{const saved=JSON.parse(global.sessionStorage?.getItem('implexity-case-start-v1')||'null');
        this.startUncertain=Boolean(saved?.pending);this.startedJob=saved?.job||null;}catch(_){}
      this.build();
    }
    button(label, fn, id) {
      const button = make('button', label); button.type = 'button'; if (id) button.id = id;
      button.addEventListener('click', () => Promise.resolve().then(fn).catch(error => this.message(error.message, 'error')));
      return button;
    }
    build() {
      this.launch = this.button('Application case…', () => this.open(), 'applicationCaseOpen');
      this.launch.title = 'Import a versioned application case document and build it with the builder its package registered.';
      this.commands.toolbar.append(this.launch);
      this.dialog = make('dialog', '', 'advanced-commands application-case'); this.dialog.id = 'applicationCaseDialog';
      this.dialog.setAttribute('aria-labelledby', 'applicationCaseTitle');
      const head = make('header'); const title = make('h2', 'Application case'); title.id = 'applicationCaseTitle';
      this.closeButton = this.button('Close', () => this.close(), 'applicationCaseClose'); head.append(title, this.closeButton);
      const note = make('p', 'A loaded application package builds the case into ordinary public actions (package loading, model import, engineering problem, optimization preflight). Building runs no physics solve; executing uses each action’s own permission and validation and never starts an optimization.', 'advanced-help');
      this.builders = make('div', '', 'application-case-builders'); this.builders.id = 'applicationCaseBuilders';
      const fileLabel = make('label', 'Case document (JSON)', 'application-case-file');
      this.file = make('input'); this.file.type = 'file'; this.file.accept = '.json,application/json'; this.file.id = 'applicationCaseFile';
      this.file.setAttribute('aria-label', 'Import application case JSON'); fileLabel.append(this.file);
      this.file.addEventListener('change', () => this.load().catch(error => this.message(error.message, 'error')));
      this.summary = make('p', 'No case imported.', 'advanced-command-policy'); this.summary.id = 'applicationCaseSummary';
      const actions = make('div', '', 'advanced-command-actions');
      this.buildButton = this.button('Build case', () => this.run(false), 'applicationCaseBuild'); this.buildButton.className = 'primary';
      this.stopLabel = make('label', 'Stop before ', 'application-case-stop'); this.stop = make('select'); this.stop.id = 'applicationCaseStopBefore';
      this.stop.setAttribute('aria-label', 'Stop execution before action'); this.stopLabel.append(this.stop);
      this.stop.addEventListener('change', () => {this.stopChosen = true;});
      this.confirmLabel = make('label', '', 'advanced-confirm'); this.confirm = make('input'); this.confirm.type = 'checkbox'; this.confirm.id = 'applicationCaseConfirm';
      this.confirmLabel.append(this.confirm, document.createTextNode(' I have reviewed the built actions and authorize them to change the model, loaded packages and engineering problem.'));
      this.confirm.addEventListener('change', () => this.update());
      this.executeButton = this.button('Execute authoring actions', () => this.run(true), 'applicationCaseExecute');
      this.downloadButton = this.button('Download builder output JSON', () => this.download(), 'applicationCaseDownload');
      this.refreshButton = this.button('Refresh saved state', () => this.refreshSavedState(), 'applicationCaseRefresh');
      actions.append(this.buildButton, this.stopLabel, this.executeButton, this.downloadButton, this.refreshButton);
      this.status = make('p', '', 'advanced-command-status'); this.status.id = 'applicationCaseStatus'; this.status.setAttribute('role', 'status'); this.status.setAttribute('aria-live', 'polite');
      this.reports = make('div', '', 'application-case-reports'); this.reports.id = 'applicationCaseReports';
      const launch = make('section', '', 'application-case-report');
      launch.append(make('h3', 'Exact authored optimization request'), make('p', 'Execute the case authoring actions first. Review this unchanged request, check readiness, then start separately. Readiness may execute physics.', 'advanced-help'));
      this.exactRequest = make('textarea'); this.exactRequest.readOnly = true; this.exactRequest.rows = 10; this.exactRequest.id = 'applicationCaseExactRequest';
      this.exactRequest.setAttribute('aria-label', 'Exact authored optimization request');
      this.checkRequestButton = this.button('Check exact request', () => this.checkExactRequest(), 'applicationCaseCheckRequest');
      this.startRequestButton = this.button('Start exact request…', () => this.startExactRequest(), 'applicationCaseStartRequest');
      this.runId = make('input'); this.runId.id = 'applicationCaseRunId'; this.runId.setAttribute('aria-label', 'Returned or recovered run ID');
      this.runId.placeholder = 'Returned or recovered run ID'; this.runId.value = this.startedJob || '';
      this.runId.addEventListener('input', () => this.update());
      this.adoptRequestButton = this.button('Adopt run controls', () => this.adoptExactRun(), 'applicationCaseAdoptRequest');
      this.inspectRequestButton = this.button('Inspect returned run', () => this.inspectExactRun(), 'applicationCaseInspectRequest');
      this.requestReport = make('pre'); this.requestReport.setAttribute('aria-label', 'Exact request readiness report');
      if(this.startUncertain)this.requestReport.textContent='An earlier start may have succeeded. Recover its run ID from service runs before another start.';
      launch.append(this.exactRequest, this.checkRequestButton, this.startRequestButton, this.runId, this.inspectRequestButton, this.adoptRequestButton, this.requestReport);
      this.dialog.append(head, note, this.builders, fileLabel, this.summary, this.confirmLabel, actions, this.status, this.reports, launch);
      document.body.append(this.dialog);
      this.dialog.addEventListener('cancel', event => {event.preventDefault(); if (!this.busy) this.close();});
      this.update();
    }
    message(text, level = 'info') {this.status.textContent = text; this.status.dataset.level = level;}
    tool(action) {return this.commands.rows.find(row => row.action === action);}
    update() {
      const tool = this.tool('author_application_case'); const actions = this.last?.output?.authoring_actions || [];
      this.buildButton.disabled = this.busy || !this.case || !tool?.allowed;
      this.executeButton.disabled = this.busy || !this.case || !tool?.allowed || !this.last || !this.confirm.checked || this.commands.unconfirmed;
      this.refreshButton.hidden = !this.commands.unconfirmed; this.refreshButton.disabled = this.busy;
      this.downloadButton.disabled = this.busy || !this.last; this.file.disabled = this.busy; this.closeButton.disabled = this.busy;
      this.stop.disabled = this.busy || !actions.length; this.confirm.disabled = this.busy || !this.last;
      this.checkRequestButton.disabled = this.busy || !this.requestBinding || this.commands.unconfirmed || this.startUncertain || Boolean(this.startedJob) || !this.tool('preflight_optimization')?.allowed;
      this.startRequestButton.disabled = this.busy || !this.checkedRequest || this.commands.unconfirmed || this.startUncertain || Boolean(this.startedJob) || !this.tool('start_optimization')?.allowed;
      this.adoptRequestButton.disabled = this.busy || !/^[0-9a-f]{12}$/.test(this.runId.value.trim());
      this.inspectRequestButton.disabled = this.adoptRequestButton.disabled || !this.tool("inspect_optimization_job")?.allowed;
    }
    async open() {
      if (!this.dialog.open) {this.returnFocus = document.activeElement; this.dialog.showModal();}
      this.commands.rows = catalogue(await request('GET', '/v1/agent/tools'));
      const reply = await request('POST', ACTION_PATH, {action: 'inspect_application_cases', payload: {}});
      const rows = reply.result?.builders || []; this.registered = rows;
      this.builders.replaceChildren(make('h3', 'Registered case builders'));
      if (!rows.length) this.builders.append(make('p', 'No loaded package registers a case builder. Load the application package that provides your case schema under physics packages, then reopen this dialog.', 'advanced-help'));
      for (const row of rows) {
        const item = make('div', '', 'application-case-builder'); item.dataset.schema = row.schema;
        item.append(make('strong', row.label), make('code', row.schema), make('p', row.description || '', 'advanced-help'));
        this.builders.append(item);
      }
      if (!this.tool('author_application_case')?.allowed) this.message('The current service policy does not permit application case authoring.', 'warning');
      this.update();
    }
    close() {if (!this.busy) {this.dialog.close(); this.returnFocus?.focus?.({preventScroll: true});}}
    async load() {
      const file = this.file.files?.[0]; if (!file) return;
      this.case = null; this.last = null; this.requestBinding = null; this.checkedRequest = null; this.exactRequest.value = ""; this.confirm.checked = false; this.reports.replaceChildren(); this.stop.replaceChildren();
      try {
        const value = parseValue(await file.text());
        if (!object(value) || typeof value.schema !== 'string' || !value.schema) throw new Error('The file is not a versioned case document (missing schema).');
        const builder = (this.registered || []).find(row => row.schema === value.schema);
        this.case = value;
        this.summary.textContent = `${file.name} · schema ${value.schema}${typeof value.name === 'string' ? ' · ' + value.name : ''} · ${builder ? 'builder: ' + builder.label : 'no loaded builder for this schema'}`;
        this.message(builder ? 'Case imported. Build it to review its reports and actions.' : 'No loaded package registers this schema; building will be refused until its package is loaded.', builder ? 'info' : 'warning');
      } finally {this.file.value = ''; this.update();}
    }
    async run(execute) {
      if (this.busy || !this.case) return;
      const payload = {case: this.case, execute};
      if (execute) {
        if (!this.confirm.checked) throw new Error('Review and confirm the actions first.');
        if (this.commands.unconfirmed) throw new Error('A previous command outcome is unconfirmed. Refresh the saved state before executing again.');
        if (viewportEditPending()) throw new Error('Apply or cancel the viewport edit before executing the authoring actions.');
        if (this.stop.value) payload.stop_before = this.stop.value;
      }
      this.requestBinding = null; this.checkedRequest = null;
      this.busy = true; this.update();
      this.message(execute ? 'Building and executing the authoring actions…' : 'Building the case (seed calibration can take a minute)…');
      try {
         
        await request('POST', '/v1/agent/validate', {action: 'author_application_case', payload});
        let reply;
        try {reply = await request('POST', ACTION_PATH, {action: 'author_application_case', payload});}
        catch (error) {
          if (!execute) throw error;
           
           
          const refreshError = await this.refreshAfterExecution();
          this.commands.unconfirmed = !error.refused || Boolean(refreshError); this.commands.updateButtons();
          this.message(`${error.refused ? 'Execution refused.' : 'Execution outcome unconfirmed. Steps may already have completed. Do not repeat it blindly.'} ${error.message}${refreshError ? ' Saved-state refresh needs attention: ' + refreshError : ''}`, 'warning');
          global.dispatchEvent(new CustomEvent('implexity:public-command-result', {detail: {action: 'author_application_case', status: error.refused ? 'refused' : 'unconfirmed'}}));
          return;
        }
        if (execute && (reply?.ok !== true || reply?.action !== 'author_application_case')) {
          const refreshError = await this.refreshAfterExecution();
          this.commands.unconfirmed = true; this.commands.updateButtons();
          this.message(`The reply did not acknowledge the execution. Do not repeat it blindly.${refreshError ? ' Saved-state refresh needs attention: ' + refreshError : ''}`, 'warning');
          global.dispatchEvent(new CustomEvent('implexity:public-command-result', {detail: {action: 'author_application_case', status: 'unconfirmed'}}));
          return;
        }
        this.last = reply.result; this.show(this.last);
        const execution = this.last.execution || {};
        if (execute) {
          const refreshError = await this.refreshAfterExecution();
          this.commands.unconfirmed = Boolean(refreshError); this.commands.updateButtons();
          const failed = (execution.steps || []).find(step => !step.ok);
          if(!failed && !refreshError)await this.bindExactRequest();
          this.message(failed ? `Execution stopped at ${failed.action}: ${failed.error}` : `Execution ${execution.status}.${refreshError ? ' Saved-state refresh needs attention: ' + refreshError + '. Do not repeat the execution.' : ''}`, failed || refreshError ? 'warning' : 'success');
          global.dispatchEvent(new CustomEvent('implexity:public-command-result', {detail: {action: 'author_application_case', status: execution.status}}));
        } else this.message('Case built. Review the reports and actions; nothing was executed.', 'success');
      } finally {this.busy = false; this.confirm.checked = false; this.update();}
    }
    async exactIdentity() {
      const [model, problem] = await Promise.all([request('GET', '/v1/implicit/model'), request('GET', '/v1/implicit/problem')]);
      if(!model.loaded || !model.content_id || !model.sha256)throw new Error('Load and refresh the authored model first.');
      return JSON.stringify({content_id:model.content_id,sha256:model.sha256,problem});
    }
    async bindExactRequest() {
      const output=this.last?.output,execution=this.last?.execution;
      if(!object(output?.optimization_request) || execution?.requested!==true)return;
      const required=(output.authoring_actions||[]).map((row,index)=>({row,index})).filter(({row})=>row.action!=='preflight_optimization');
      if(!required.length || !required.every(({row,index})=>(execution.steps||[]).some(step=>step.index===index && step.action===row.action && step.ok===true)))return;
      const imported=(execution.steps||[]).find(step=>step.action==='import_model')?.result;
      if(imported?.content_id && imported.content_id!==global.S?.model?.content_id)
        throw new Error('The saved model no longer matches the authored model.');
      this.requestBinding={identity:await this.exactIdentity(),request:copy(output.optimization_request)};
    }
    async assertExactRequest() {
      if(!this.requestBinding || this.commands.unconfirmed || viewportEditPending())
        throw new Error('Execute and refresh this case, and finish viewport edits before using its request.');
      if(global.implexityOptimizationBlocksNewRun?.())throw new Error('Resolve the current run before starting another.');
      if(await this.exactIdentity()!==this.requestBinding.identity){
        this.checkedRequest=null;
        throw new Error('The saved model or engineering problem changed. Execute and review the case again.');
      }
      return copy(this.requestBinding.request);
    }
    async checkExactRequest() {
      if(this.busy || this.startUncertain || this.startedJob)return;
      this.busy=true;this.checkedRequest=null;this.update();
      try{
        const payload=await this.assertExactRequest();
        await request('POST', '/v1/agent/validate', {action:'preflight_optimization',payload});
        const reply=await request('POST', ACTION_PATH, {action:'preflight_optimization',payload});
        if(reply?.ok!==true || reply?.action!=='preflight_optimization')throw new Error('Readiness reply was not acknowledged.');
        await this.assertExactRequest();
        this.requestReport.textContent=JSON.stringify(reply.result,null,2);
        if(reply.result?.ok!==true)throw new Error('This exact request did not pass readiness. Review its report.');
        this.checkedRequest=JSON.stringify(payload);
        this.message('Exact authored request checked. Starting remains a separate action.');
      }finally{this.busy=false;this.update();}
    }
    rememberStart() {
      try{global.sessionStorage?.setItem('implexity-case-start-v1',JSON.stringify({pending:this.startUncertain,job:this.startedJob}));}catch(_){}
    }
    async startExactRequest() {
      if(this.busy || this.startUncertain || this.startedJob || !this.checkedRequest)return;
      this.busy=true;this.update();
      let submitted=false;
      try{
        const payload=await this.assertExactRequest();
        if(JSON.stringify(payload)!==this.checkedRequest)throw new Error('The exact request changed. Check readiness again.');
        await request('POST', '/v1/agent/validate', {action:'start_optimization',payload});
        if(!global.confirm('Start optimization using the exact authored JSON shown above? This executes physics.'))return;
        await this.assertExactRequest();
        this.startUncertain=true;this.rememberStart();submitted=true;
        const reply=await request('POST', ACTION_PATH, {action:'start_optimization',payload});
        const id=reply?.result?.job_id;
        if(reply?.ok!==true || reply?.action!=='start_optimization' || typeof id!=='string' || !/^[0-9a-f]{12}$/.test(id))
          throw new Error('The start reply did not confirm a run ID.');
        this.startedJob=id;this.runId.value=id;this.startUncertain=false;this.rememberStart();
        this.message(`Run ${id} started. Use Adopt run controls to monitor and control it.`,'success');
      }catch(error){
        if(submitted){this.startUncertain=true;this.rememberStart();this.message(`Start outcome unconfirmed: ${error.message} Recover the run ID from service runs; do not repeat this start.`,'warning');}
        else throw error;
      }finally{this.busy=false;this.update();}
    }
    async inspectExactRun() {
      if(this.busy)return;
      const id=this.runId.value.trim();
      if(!/^[0-9a-f]{12}$/.test(id))throw new Error('Enter the returned or recovered run ID.');
      this.busy=true;this.update();
      try{
        const reply=await request('POST', ACTION_PATH, {action:'inspect_optimization_job',payload:{job_id:id}});
        if(reply?.ok!==true || reply.result?.job_id!==id)throw new Error('The service did not confirm this run.');
        this.requestReport.textContent=JSON.stringify(reply.result,null,2);
        if(this.startedJob===id && ['accepted','discarded'].includes(reply.result.status)){
          this.startedJob=null;this.startUncertain=false;this.checkedRequest=null;this.requestBinding=null;this.rememberStart();
          this.message('The previous run is resolved. Execute and review the next case before starting.');
        }else this.message(`Run ${id}: ${reply.result.status}. Adopt its controls to resolve it.`);
      }finally{this.busy=false;this.update();}
    }
    async adoptExactRun() {
      if(this.busy)return;
      const id=this.runId.value.trim();
      if(!/^[0-9a-f]{12}$/.test(id))throw new Error('Enter the returned or recovered run ID.');
      if(typeof global.implexityAdoptOptimizationJob!=='function')throw new Error('Run controls are unavailable.');
      this.busy=true;this.update();
      try{
        await global.implexityAdoptOptimizationJob(id);
        this.startedJob=id;this.startUncertain=false;this.rememberStart();
        this.message(`This panel now controls run ${id}.`,'success');
      }finally{this.busy=false;this.update();}
    }
    async refreshAfterExecution() {
      try {await this.commands.refreshSaved(); return null;} catch (error) {return error.message;}
    }
    async refreshSavedState() {
      this.busy = true; this.update();
      try {
        await this.commands.refreshSaved(); this.commands.unconfirmed = false; this.commands.updateButtons();
        this.message('Saved state refreshed. The unconfirmed execution was not repeated. Inspect it before executing again.');
      } finally {this.busy = false; this.update();}
    }
    show(result) {
      this.reports.replaceChildren();
      const output = result.output || {}; const actions = output.authoring_actions || [];
      this.exactRequest.value = object(output.optimization_request) ? JSON.stringify(output.optimization_request, null, 2) : "";
       
       
      const previous = this.stopChosen ? this.stop.value : 'preflight_optimization';
      this.stop.replaceChildren(new Option('Run all actions', ''));
      for (const name of [...new Set(actions.map(row => row.action))]) this.stop.append(new Option(name.replaceAll('_', ' '), name));
      this.stop.value = [...this.stop.options].some(option => option.value === previous) ? previous : '';
      const header = make('p', `${result.builder?.label || 'Builder'} · ${result.case_schema} · ${actions.length} authoring actions`, 'advanced-command-policy');
      this.reports.append(header);
      for (const [key, value] of Object.entries(result.reports || {})) {
        const section = make('details', '', 'application-case-report'); section.open = true; section.dataset.report = key;
        section.append(make('summary', key.replaceAll('_', ' ')), renderReport(value)); this.reports.append(section);
      }
      const planned = make('details', '', 'application-case-report'); planned.dataset.report = 'authoring_actions';
      planned.append(make('summary', 'Authoring actions'));
      const list = make('ol', '', 'application-case-list');
      for (const row of actions) list.append(make('li', `${row.action.replaceAll('_', ' ')}${row.payload?.package ? ' · ' + row.payload.package : ''}`));
      planned.append(list); this.reports.append(planned);
      const steps = result.execution?.steps || [];
      if (steps.length) {
        const executed = make('details', '', 'application-case-report'); executed.open = true; executed.dataset.report = 'execution';
        executed.append(make('summary', `Execution · ${result.execution.status}`));
        const rows = make('ol', '', 'application-case-list');
        for (const step of steps) {const item = make('li', `${step.action.replaceAll('_', ' ')} · ${step.ok ? 'succeeded' : 'failed: ' + step.error}`); item.dataset.ok = String(step.ok); rows.append(item);}
        executed.append(rows); this.reports.append(executed);
      }
    }
    download() {
      if (!this.last) return;
      const blob = new Blob([JSON.stringify(this.last, null, 2)], {type: 'application/json'});
      const url = URL.createObjectURL(blob); const link = make('a'); link.href = url; link.download = 'application-case-output.json';
      document.body.append(link); link.click(); link.remove(); setTimeout(() => URL.revokeObjectURL(url), 0);
    }
  }

  function boot() {
    if (!document.querySelector('#topbar')) return;
    global.ImplexityAdvancedCommands = new CommandPanel();
    global.ImplexityApplicationCase = new ApplicationCasePanel(global.ImplexityAdvancedCommands);
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, {once: true}); else boot();
})(globalThis);
