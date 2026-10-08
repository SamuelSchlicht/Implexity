// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

 
(function (global) {
  'use strict';
  const ACTION_PATH = '/v1/agent/action';
  const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
  const make = (tag, text = '', className = '') => {
    const node = document.createElement(tag); if (text) node.textContent = text; if (className) node.className = className; return node;
  };
  const human = id => global.ImplexityText?.humanize?.(id) || String(id).replaceAll('_', ' ');

  async function request(method, path, body) {
    const response = await fetch(path, {method, credentials: 'same-origin', cache: 'no-store',
      ...(body === undefined ? {} : {headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)})});
    let value;
    try { value = await response.json(); }
    catch (_) { throw new Error(`The service reply could not be decoded (HTTP ${response.status}).`); }
    if (!response.ok || value?.ok === false) {
      const detail = value?.detail;
      const error = new Error([value?.error || (typeof detail === 'string' ? detail : '') || `HTTP ${response.status}`,
        ...(Array.isArray(value?.problems) ? value.problems : [])].filter(Boolean).join(' · '));
      error.response = value; throw error;
    }
    return value;
  }
  async function validateAction(action, payload) {
    return request('POST', '/v1/agent/validate', {action, payload});
  }
   
  async function runAction(action, payload) {
    await validateAction(action, payload);
    const reply = await request('POST', ACTION_PATH, {action, payload});
    if (reply?.ok !== true || reply?.action !== action) throw new Error('The service reply did not acknowledge this request.');
    return reply.result;
  }
  function saveBlob(blob, filename) {
    const url = URL.createObjectURL(blob); const link = make('a'); link.href = url; link.download = filename;
    document.body.append(link); link.click(); link.remove(); setTimeout(() => URL.revokeObjectURL(url), 0);
  }
  function saveJSON(value, filename) {
    saveBlob(new Blob([JSON.stringify(value, null, 2) + '\n'], {type: 'application/json'}), filename);
  }
  function fileSafe(text) { return String(text).replace(/[^A-Za-z0-9_.-]+/g, '_').slice(0, 96) || 'implexity'; }
  function rgb255(hex) { return hex.slice(1).match(/../g).map(part => parseInt(part, 16)); }
  function finite(value, label) {
    const number = typeof value === 'number' ? value : String(value ?? '').trim() === '' ? NaN : Number(value);
    if (!Number.isFinite(number)) throw new Error(`${label} requires a finite number.`);
    return number;
  }
  function numberList(text, label) {
    const values = String(text).split(/[\s,;]+/).filter(Boolean).map(item => finite(item, label));
    if (!values.length) throw new Error(`${label} requires at least one number.`);
    for (let index = 1; index < values.length; index++)
      if (!(values[index] > values[index - 1])) throw new Error(`${label} must be strictly increasing.`);
    return values;
  }
  function modelLoaded() { return global.S?.model?.loaded === true; }

   
  const service = {
    tools: null,
    async catalogue() {
      const manifest = await request('GET', '/v1/agent/tools');
      const rows = global.ImplexityAdvancedCommandCore?.catalogue
        ? global.ImplexityAdvancedCommandCore.catalogue(manifest) : manifest.tools;
      this.tools = new Map(rows.map(row => [row.action, row]));
      return this.tools;
    },
    async tool(action) {
      if (!this.tools) await this.catalogue();
      const row = this.tools.get(action);
      if (!row) throw new Error(`The service does not publish ${action}.`);
      return row;
    },
    async renderables() {
      if (!modelLoaded()) return null;
      return runAction('inspect_renderables', {});
    },
  };
   
  function schemaAt(schema, path) {
    let node = schema;
    for (const key of path) {
      if (!node) return null;
      if (key === '*') node = node.items; else node = node.properties?.[key];
    }
    return node || null;
  }
  function enumOf(schema, path, fallback) {
    const node = schemaAt(schema, path);
    const values = node?.enum || node?.oneOf?.flatMap(item => item.enum || []) || null;
    return values?.length ? values : fallback;
  }

   
   
   
  class EpochPicker {
    constructor(host, {allowCurrent = false, label = 'Optimisation update (epoch)'} = {}) {
      this.allowCurrent = allowCurrent;
      this.root = make('div', '', 'output-epoch-picker');
      this.job = make('input'); this.job.type = 'text'; this.job.pattern = '[0-9a-f]{12}'; this.job.maxLength = 12;
      this.job.spellcheck = false; this.job.placeholder = '12-hex run id';
      this.select = make('select'); this.number = make('input'); this.number.type = 'number'; this.number.min = '0'; this.number.step = '1';
      host.field(this.root, 'Optimisation run', this.job, 'Defaults to the run shown in the optimisation monitor.');
      host.field(this.root, label, this.select, 'The published updates of that run, as in the retained-field viewer.');
      host.field(this.root, 'Update number', this.number);
      this.job.addEventListener('input', () => this.populate());
      this.select.addEventListener('change', () => this.sync());
    }
    published() {
      const job = this.job.value.trim();
      const rows = global.OPT?.job && global.OPT.job === job ? (global.OPT.rows || []) : [];
      return [...new Set(rows.filter(row => Number.isSafeInteger(row?.i)).map(row => row.i))].sort((a, b) => a - b);
    }
    refresh(preset = {}) {
      this.job.value = preset.job_id || global.OPT?.job || this.job.value || '';
      if (Number.isSafeInteger(preset.epoch)) this.number.value = String(preset.epoch);
      this.populate(preset.epoch);
    }
    populate(preferred) {
      const epochs = this.published(); const previous = preferred ?? this.select.value;
      this.select.replaceChildren();
      if (this.allowCurrent) this.select.append(new Option('Current applied iterate', 'current'));
      for (const epoch of epochs) this.select.append(new Option(`Update ${epoch}`, String(epoch)));
      this.select.append(new Option('Other update number…', 'other'));
      const monitor = document.querySelector('#runEpochSelect')?.value;
      const has = value => [...this.select.options].some(option => option.value === value && !option.disabled);
      const wanted = [previous, monitor, epochs.length ? epochs.at(-1) : null, this.allowCurrent ? 'current' : null]
        .filter(value => value !== undefined && value !== null && value !== '').map(String).find(has);
      this.select.value = wanted || 'other';
      this.sync();
    }
     
    allowCurrentChoice(allowed) {
      const option = [...this.select.options].find(item => item.value === 'current');
      if (option) option.disabled = !allowed;
      if (!allowed && this.select.value === 'current') this.populate(this.published().at(-1) ?? 'other');
    }
    sync() { this.number.closest('label').hidden = this.select.value !== 'other'; }
    value() {
      const job = this.job.value.trim();
      if (!/^[0-9a-f]{12}$/.test(job)) throw new Error('Enter the 12-character run id of an optimisation run.');
      const choice = this.select.value;
      if (choice === 'current') return {job_id: job, epoch: 'current'};
      const epoch = Number(choice === 'other' ? this.number.value : choice);
      if (!Number.isSafeInteger(epoch) || epoch < 0 || (choice === 'other' && !this.number.value.trim()))
        throw new Error('Choose a published update (a non-negative whole number).');
      return {job_id: job, epoch};
    }
  }

   
  class OutputDialog {
    constructor(id, title, help) {
      this.id = id; this.busy = false;
      this.dialog = make('dialog', '', 'advanced-commands output-dialog'); this.dialog.id = id;
      this.dialog.setAttribute('aria-labelledby', id + 'Title');
      const head = make('header'); const heading = make('h2', title); heading.id = id + 'Title';
      this.closeButton = this.button('Close', () => this.close()); head.append(heading, this.closeButton);
      this.form = make('div', '', 'output-form');
      this.actions = make('div', '', 'advanced-command-actions');
      this.status = make('p', '', 'advanced-command-status'); this.status.setAttribute('role', 'status'); this.status.setAttribute('aria-live', 'polite');
      this.status.id = id + 'Status';
      this.result = make('div', '', 'output-result'); this.result.id = id + 'Result';
      this.dialog.append(head, make('p', help, 'advanced-help'), this.form, this.actions, this.status, this.result);
      document.body.append(this.dialog);
      this.dialog.addEventListener('cancel', event => {event.preventDefault(); this.close();});
      this.dialog.addEventListener('close', () => this.returnFocus?.focus?.({preventScroll: true}));
    }
    button(label, fn, className = '') {
      const node = make('button', label, className); node.type = 'button';
      node.addEventListener('click', () => Promise.resolve().then(fn).catch(error => this.message(error.message, 'error')));
      return node;
    }
    message(text, level = 'info') { this.status.textContent = text; this.status.dataset.level = level; }
    reveal() { this.status.scrollIntoView?.({block: 'start'}); }
     
    field(host, label, input, help = '') {
      const holder = make('label', '', 'output-field'); holder.append(make('span', label), input);
      if (help) {
        const small = make('small', help); small.id = `${this.id}-help-${++OutputDialog.serial}`;
        input.setAttribute('aria-describedby', small.id); holder.append(small);
      }
      host.append(holder); return input;
    }
    check(host, label, checked = false) {
      const holder = make('label', '', 'output-check'); const box = make('input'); box.type = 'checkbox'; box.checked = checked;
      holder.append(box, document.createTextNode(' ' + label)); host.append(holder); return box;
    }
    select(options, value) {
      const node = make('select');
      for (const option of options) node.append(Array.isArray(option) ? new Option(option[1], option[0]) : new Option(human(option), String(option)));
      if (value !== undefined) node.value = String(value);
      return node;
    }
    input(type, attrs = {}) { const node = make('input'); node.type = type; Object.assign(node, attrs); return node; }
    section(title) {
      const box = make('fieldset', '', 'output-section'); box.append(make('legend', title)); this.form.append(box); return box;
    }
    setBusy(value) {
      this.busy = value; this.closeButton.disabled = value;
      for (const node of this.dialog.querySelectorAll('.output-form input,.output-form select,.output-form textarea,.advanced-command-actions button')) {
        if (value) {node.dataset.wasDisabled = String(node.disabled); node.disabled = true;}
        else if (node.dataset.wasDisabled !== undefined) {node.disabled = node.dataset.wasDisabled === 'true'; delete node.dataset.wasDisabled;}
      }
      this.dialog.setAttribute('aria-busy', String(value));
    }
    async open(preset = {}) {
      if (!this.dialog.open) {this.returnFocus = document.activeElement; this.dialog.showModal();}
      this.message('');
      try { await this.prepare(preset); this.sync?.(); }
      catch (error) { this.message(error.message, 'error'); }
    }
    close() { if (!this.busy) this.dialog.close(); }
    async perform(label, fn) {
      if (this.busy) return;
      this.setBusy(true); this.message(label);
      try { return await fn(); }
      finally { this.setBusy(false); this.sync?.(); }
    }
    async checkRequest(action) {
      const payload = this.payload();
      await this.perform('Checking the request…', () => validateAction(action, payload));
      this.message('Request accepted by the service schema and policy. Nothing was computed.', 'success');
      return payload;
    }
  }
  OutputDialog.serial = 0;

  function sourceChoices(schema, path, labels) {
    return enumOf(schema, path, Object.keys(labels)).filter(kind => labels[kind]).map(kind => [kind, labels[kind]]);
  }
  function fieldOptions(rows, {includeBoundary = false} = {}) {
    const options = includeBoundary ? [['model_boundary', 'Model boundary (signed geometry)']] : [];
    for (const row of rows || []) if (row?.field && row.field !== 'model_boundary')
      options.push([row.field, `${row.label || row.field}${row.units && row.units !== 'provider_native' ? ' [' + row.units + ']' : ''}${row.read_only ? ' · declared grid' : ''}`]);
    return options;
  }
  function refill(select, options, keep) {
    const previous = keep ?? select.value; select.replaceChildren();
    for (const [value, label] of options) select.append(new Option(label, value));
    if ([...select.options].some(option => option.value === previous)) select.value = previous;
  }
   
  function viewportClip() { return global.implexityViewportClip?.() || null; }
  function viewportCamera() {
    const camera = global.ImplexityViewerAdapter?.camera;
    const state = camera?.get?.(); const frustum = camera?.frustum?.();
    if (!state || !frustum || !Array.isArray(state.eye)) return null;
    return {eye_mm: state.eye.slice(0, 3), target_mm: state.target.slice(0, 3), up: frustum.up.slice(0, 3),
      fov_deg: Math.min(90, Math.max(15, Number(state.fov) || 32))};
  }

   
  class ExportSTLDialog extends OutputDialog {
    constructor() {
      super('outputExportSTL', 'Export STL', 'Closed binary STL files in millimetres, meshed from one partition of the chosen source: material files superpose exactly to the solid, and solid plus complement fills the export box. Read-only; never applies an optimisation update. Not manufacturing qualification.');
      const source = this.section('Source');
      this.source = this.field(source, 'Geometry source', this.select([]), 'A saved epoch is exported read-only without applying it.');
      this.epoch = new EpochPicker(this, {allowCurrent: true});
      source.append(this.epoch.root);
      const resolution = this.section('Resolution');
      this.resolutionMode = this.field(resolution, 'Sampling', this.select([['refinement', 'Declared geometry resolution × refinement'], ['spacing', 'Explicit cell spacing']]));
      this.refinement = this.field(resolution, 'Refinement', this.select([]), 'Divides the model’s declared geometry spacing.');
      this.spacing = this.field(resolution, 'Cell spacing (mm)', this.input('number', {step: 'any'}));
      const parts = this.section('Parts');
      this.includeSolid = this.check(parts, 'Solid', true);
      this.materialsOn = this.check(parts, 'Split the solid into materials by a registered field');
      this.materials = make('div', '', 'output-subsection'); parts.append(this.materials);
      this.materialField = this.field(this.materials, 'Material field', this.select([]), 'A registered field of the current model, e.g. a phase fraction.');
      this.thresholds = this.field(this.materials, 'Thresholds', this.input('text', {value: '0.5'}), 'Increasing values; a value equal to a threshold belongs to the upper category.');
      this.categories = make('div', '', 'output-rows'); this.materials.append(this.categories);
      this.complementOn = this.check(parts, 'Complement inside the export box (e.g. a fluid domain)');
      this.complement = make('div', '', 'output-subsection'); parts.append(this.complement);
      this.complementId = this.field(this.complement, 'Complement file id', this.input('text', {value: 'complement', pattern: '[A-Za-z0-9][A-Za-z0-9_.-]{0,63}'}));
      this.complementLabel = this.field(this.complement, 'Complement label', this.input('text', {value: '', maxLength: 160}));
      const box = this.section('Export box');
      this.extentOn = this.check(box, 'Use an explicit box instead of the model extent');
      this.extent = make('div', '', 'output-vector-grid'); box.append(this.extent);
      this.extentInputs = ['x min', 'y min', 'z min', 'x max', 'y max', 'z max'].map(axis => this.field(this.extent, `${axis} (mm)`, this.input('number', {step: 'any'})));
      const delivery = this.section('Delivery');
      this.bundle = this.field(delivery, 'Files', this.select([]));
      this.stem = this.field(delivery, 'File name stem', this.input('text', {value: 'implexity', pattern: '[A-Za-z0-9][A-Za-z0-9_.-]{0,63}'}));
      this.checkButton = this.button('Check request', () => this.checkRequest('export_stl'));
      this.runButton = this.button('Export STL', () => this.run(), 'primary'); this.runButton.id = 'outputExportSTLRun';
      this.actions.append(this.checkButton, this.runButton);
      this.form.addEventListener('change', () => this.sync());
      this.thresholds.addEventListener('input', () => this.renderCategories());
    }
    async prepare(preset) {
      const tool = await service.tool('export_stl'); this.schema = tool.input_schema;
      refill(this.source, sourceChoices(this.schema, ['source', 'kind'], {
        current_model: 'Current model', current_optimization_state: 'Live optimisation state (applied iterate)',
        optimization_epoch: 'Saved optimization epoch (read-only)'}), preset.source?.kind || this.source.value || 'current_model');
      refill(this.refinement, enumOf(this.schema, ['resolution', 'refinement'], [1, 2, 3, 4]).map(value => [String(value), `× ${value}`]), this.refinement.value || '1');
      const spacing = schemaAt(this.schema, ['resolution', 'spacing_mm']) || {};
      if (spacing.minimum !== undefined) this.spacing.min = String(spacing.minimum);
      if (spacing.maximum !== undefined) this.spacing.max = String(spacing.maximum);
      refill(this.bundle, enumOf(this.schema, ['bundle'], ['files', 'zip']).map(value => [value, value === 'zip' ? 'One ZIP with every STL and a manifest' : 'One STL file per part']), this.bundle.value || 'files');
      this.epoch.refresh(preset.source || {});
      const catalogue = await service.renderables();
      this.fields = catalogue?.three_dimensional_rendering?.color_fields || [];
      refill(this.materialField, fieldOptions(this.fields));
      if (!this.categories.children.length) this.renderCategories();
      if (!modelLoaded()) this.message('Load a model first; STL export samples the stored model.', 'warning');
    }
    renderCategories() {
      let count = 2;
      try { count = numberList(this.thresholds.value, 'Thresholds').length + 1; } catch (_) { count = this.categories.children.length || 2; }
      const previous = [...this.categories.querySelectorAll('.output-row')].map(row => ({id: row.querySelector('[data-role=id]').value, label: row.querySelector('[data-role=label]').value}));
      this.categories.replaceChildren();
      for (let index = 0; index < count; index++) {
        const row = make('div', '', 'output-row');
        const id = this.input('text', {value: previous[index]?.id || `material_${index + 1}`, pattern: '[A-Za-z0-9][A-Za-z0-9_.-]{0,63}'}); id.dataset.role = 'id';
        const label = this.input('text', {value: previous[index]?.label || '', maxLength: 160, placeholder: 'optional label'}); label.dataset.role = 'label';
        this.field(row, `Category ${index + 1} file id`, id); this.field(row, `Category ${index + 1} label`, label);
        this.categories.append(row);
      }
    }
    sync() {
      const kind = this.source.value;
      this.epoch.root.hidden = kind === 'current_model';
      if (!this.epoch.root.hidden) {this.epoch.allowCurrentChoice(kind === 'current_optimization_state'); this.epoch.sync();}
      this.refinement.closest('label').hidden = this.resolutionMode.value !== 'refinement';
      this.spacing.closest('label').hidden = this.resolutionMode.value !== 'spacing';
      this.materialsOn.disabled = !this.materialField.options.length;
      if (this.materialsOn.disabled) {this.materialsOn.checked = false; this.materialsOn.closest('label').title = 'The current model registers no field to split materials by.';}
      this.materials.hidden = !this.materialsOn.checked; this.complement.hidden = !this.complementOn.checked;
      this.extent.hidden = !this.extentOn.checked;
      this.runButton.disabled = this.busy || !modelLoaded();
    }
    payload() {
      const payload = {};
      const kind = this.source.value;
      if (kind === 'current_optimization_state') payload.source = {kind, ...this.epoch.value()};
      else if (kind === 'optimization_epoch') {
        const picked = this.epoch.value(); if (picked.epoch === 'current') throw new Error('Choose a published update number.');
        payload.source = {kind, ...picked};
      } else payload.source = {kind: 'current_model'};
      if (this.resolutionMode.value === 'spacing') payload.resolution = {spacing_mm: finite(this.spacing.value, 'Cell spacing')};
      else payload.resolution = {refinement: Number(this.refinement.value)};
      payload.include_solid = this.includeSolid.checked;
      if (this.materialsOn.checked) {
        if (!this.materialField.value) throw new Error('The current model registers no field to split materials by.');
        const thresholds = numberList(this.thresholds.value, 'Thresholds');
        const rows = [...this.categories.querySelectorAll('.output-row')];
        if (rows.length !== thresholds.length + 1) this.renderCategories();
        payload.materials = {field: this.materialField.value, thresholds, categories: [...this.categories.querySelectorAll('.output-row')].map(row => {
          const id = row.querySelector('[data-role=id]').value.trim(); const label = row.querySelector('[data-role=label]').value.trim();
          return label ? {id, label} : {id};
        })};
      }
      if (this.complementOn.checked) {
        payload.complement = {id: this.complementId.value.trim() || 'complement'};
        if (this.complementLabel.value.trim()) payload.complement.label = this.complementLabel.value.trim();
      }
      if (!payload.include_solid && !payload.materials && !payload.complement) throw new Error('Choose at least one part to export.');
      if (this.extentOn.checked) {
        const values = this.extentInputs.map((input, index) => finite(input.value, `Export box ${['x', 'y', 'z'][index % 3]}`));
        payload.extent_mm = [values.slice(0, 3), values.slice(3)];
        if (payload.extent_mm[0].some((value, axis) => !(payload.extent_mm[1][axis] > value))) throw new Error('The export box needs positive extents.');
      }
      payload.bundle = this.bundle.value; payload.file_stem = this.stem.value.trim() || 'implexity';
      return payload;
    }
    async run() {
      const payload = this.payload();
      const result = await this.perform('Sampling and meshing the export…', () => runAction('export_stl', payload));
      this.show(result, payload);
      this.message(`Exported ${result.files?.length || 0} closed part file(s). Save them below.`, 'success'); this.reveal();
    }
    show(result, payload) {
      this.result.replaceChildren();
      const table = make('table', '', 'output-table'); const head = make('tr');
      for (const column of ['File', 'Part', 'Triangles', 'Watertight', 'Volume (mm³)', 'Area (mm²)']) head.append(make('th', column));
      table.append(head);
      for (const row of result.files || []) {
        const tr = make('tr');
        for (const value of [row.filename || row.id, row.label || row.part || '', (row.triangles ?? '').toLocaleString(), row.watertight ? 'yes' : 'NO',
          Number.isFinite(row.volume_mm3) ? row.volume_mm3.toPrecision(6) : 'N/A', Number.isFinite(row.area_mm2) ? row.area_mm2.toPrecision(6) : 'N/A']) tr.append(make('td', String(value)));
        table.append(tr);
      }
      this.result.append(table);
      const checks = result.checks || {};
      if (Object.keys(checks).length) {
        const list = make('dl', '', 'application-case-facts');
        for (const [key, value] of Object.entries(checks)) list.append(make('dt', human(key)), make('dd', typeof value === 'object' ? JSON.stringify(value) : String(value)));
        const details = make('details'); details.append(make('summary', 'Partition checks'), list); this.result.append(details);
      }
      const core = global.ImplexityAdvancedCommandCore;
      const files = core.resultFiles(result.delivery || {});
      const saves = make('div', '', 'advanced-command-actions');
      for (const file of files) saves.append(this.button(`Save ${file.filename}`, () => saveBlob(core.fileBlob(file), file.filename)));
      if (files.length > 1) saves.append(this.button('Save all files', () => files.forEach((file, index) => setTimeout(() => saveBlob(core.fileBlob(file), file.filename), index * 250))));
      saves.append(this.button('Save export report (JSON)', () => {
        const report = structuredClone(result); delete report.delivery; saveJSON({request: payload, report}, `${fileSafe(payload.file_stem)}_stl_report.json`);
      }));
      this.result.append(saves);
    }
  }

   
  class RenderImageDialog extends OutputDialog {
    constructor() {
      super('outputRenderImage', 'Render image', 'An identity-bound PNG of the model boundary or a registered field iso-surface, rendered by the service. Geometry is never silently smoothed; the returned record states sampling, camera and truth status. Render-only: never an export or acceptance authority.');
      const source = this.section('Source');
      this.source = this.field(source, 'Geometry source', this.select([]));
      this.epoch = new EpochPicker(this, {allowCurrent: true}); source.append(this.epoch.root);
      this.artifact = this.field(source, 'Result artifact id', this.input('text', {pattern: 'result-[0-9a-f]{32}', placeholder: 'result-…'}));
      const surface = this.section('Surface');
      this.surfaceField = this.field(surface, 'Surface', this.select([['model_boundary', 'Model boundary (signed geometry)']]));
      this.iso = this.field(surface, 'Iso value', this.input('number', {step: 'any', value: '0.5'}), 'Level of the registered field drawn as the surface.');
      this.region = this.field(surface, 'Region', this.select([]), 'Complement draws the region outside the solid inside the model extent (e.g. a fluid domain).');
      const colour = this.section('Colouring');
      this.colouring = this.field(colour, 'Colouring', this.select([['default', 'Renderer default'], ['uniform', 'Uniform colour'], ['field', 'By a registered field']]));
      this.surfaceColor = this.field(colour, 'Surface colour', this.input('color', {value: '#9aa7b8'}));
      this.colorField = this.field(colour, 'Colour field', this.select([]), 'Registered fields and node-declared grids such as a phase fraction.');
      this.mapping = this.field(colour, 'Mapping', this.select([['palette', 'Named palette'], ['stops', 'Explicit colour stops'], ['categories', 'Threshold categories']]));
      this.palette = this.field(colour, 'Palette', this.select([]));
      this.rangeOn = this.check(colour, 'Explicit value range');
      this.range = make('div', '', 'output-vector-grid'); colour.append(this.range);
      this.rangeLow = this.field(this.range, 'Range minimum', this.input('number', {step: 'any', value: '0'}));
      this.rangeHigh = this.field(this.range, 'Range maximum', this.input('number', {step: 'any', value: '1'}));
      this.stops = make('div', '', 'output-rows'); colour.append(this.stops);
      this.addStop = this.button('Add colour stop', () => this.stopRow()); colour.append(this.addStop);
      this.catThresholds = this.field(colour, 'Category thresholds', this.input('text', {value: '0.5'}), 'Increasing values; one fewer than categories.');
      this.catRows = make('div', '', 'output-rows'); colour.append(this.catRows);
      const clip = this.section('Clip plane');
      this.clipMode = this.field(clip, 'Clip', this.select([['off', 'No clipping'], ['viewport', 'Viewport clip plane'], ['custom', 'Explicit plane']]));
      this.clipCustom = make('div', '', 'output-vector-grid'); clip.append(this.clipCustom);
      this.clipPoint = ['x', 'y', 'z'].map(axis => this.field(this.clipCustom, `Point ${axis} (mm)`, this.input('number', {step: 'any', value: '0'})));
      this.clipNormal = ['x', 'y', 'z'].map((axis, index) => this.field(this.clipCustom, `Normal ${axis}`, this.input('number', {step: 'any', value: index === 2 ? '1' : '0'})));
      this.keep = this.field(clip, 'Keep side', this.select([]));
      this.capOn = this.check(clip, 'Cap the cut plane');
      this.capInside = this.field(clip, 'Solid side of the iso value', this.select([]));
      this.capColorOn = this.check(clip, 'Fill the complement on the cap');
      this.capColor = this.field(clip, 'Cap complement colour', this.input('color', {value: '#5aa9e6'}));
      const camera = this.section('Camera and image');
      this.camera = this.field(camera, 'Camera', this.select([]));
      this.fov = this.field(camera, 'Field of view (°)', this.input('number', {min: '15', max: '90', step: '1', value: '32'}));
      this.quality = this.field(camera, 'Quality', this.select([]), 'geometry samples at the source’s declared resolution, geometry_fine at twice it; native uses a registered cell lattice without resampling.');
      this.antialias = this.field(camera, 'Supersampling', this.select([]));
      this.width = this.field(camera, 'Width (px)', this.input('number', {step: '1', value: '1024'}));
      this.height = this.field(camera, 'Height (px)', this.input('number', {step: '1', value: '768'}));
      this.background = this.field(camera, 'Background', this.select([]));
      this.checkButton = this.button('Check request', () => this.checkRequest('render_3d'));
      this.runButton = this.button('Render', () => this.run(), 'primary'); this.runButton.id = 'outputRenderImageRun';
      this.actions.append(this.checkButton, this.runButton);
      this.form.addEventListener('change', () => this.sync());
      this.catThresholds.addEventListener('input', () => this.categoryRows());
    }
    async prepare(preset) {
      const tool = await service.tool('render_3d'); const schema = this.schema = tool.input_schema;
      refill(this.source, sourceChoices(schema, ['source', 'kind'], {current_model: 'Current model',
        current_optimization_state: 'Live optimisation state (applied iterate)', result_artifact: 'Registered result artifact'}), preset.source?.kind || this.source.value || 'current_model');
      this.epoch.refresh(preset.source || {});
      refill(this.region, enumOf(schema, ['region'], ['solid', 'complement']).map(value => [value, human(value)]), this.region.value || 'solid');
      refill(this.palette, enumOf(schema, ['palette'], ['auto']).map(value => [value, human(value)]), this.palette.value || 'auto');
      refill(this.keep, enumOf(schema, ['clip', 'keep'], ['negative', 'positive']).map(value => [value, value === 'negative' ? 'Behind the normal' : 'In front of the normal']), this.keep.value || 'negative');
      refill(this.capInside, enumOf(schema, ['clip', 'cap', 'inside'], ['auto']).map(value => [value, human(value)]), this.capInside.value || 'auto');
      const presets = (schema.properties?.camera?.oneOf || []).flatMap(item => item.properties?.preset?.enum || []);
      refill(this.camera, [...presets.map(value => [value, human(value)]), ['viewport', 'Current viewport camera']], this.camera.value || 'isometric');
      refill(this.quality, enumOf(schema, ['quality'], ['standard']).map(value => [value, human(value)]), this.quality.value || 'geometry');
      refill(this.antialias, enumOf(schema, ['antialias'], [1]).map(value => [String(value), value === 1 ? 'Off' : `${value}×${value}`]), this.antialias.value || '2');
      refill(this.background, enumOf(schema, ['background'], ['dark', 'white']).map(value => [value, human(value)]), this.background.value || 'white');
      for (const [input, key] of [[this.width, 'width_px'], [this.height, 'height_px']]) {
        const node = schema.properties?.[key] || {}; input.min = String(node.minimum ?? 128); input.max = String(node.maximum ?? 2048);
      }
      const catalogue = await service.renderables();
      const rendering = catalogue?.three_dimensional_rendering || {};
      this.fieldRows = rendering.color_fields || [];
      refill(this.surfaceField, fieldOptions((rendering.surface_fields || []).filter(row => row.field !== 'model_boundary'), {includeBoundary: true}));
      refill(this.colorField, fieldOptions(this.fieldRows));
      if (!this.stops.children.length) {this.stopRow(0, '#2b4c7e'); this.stopRow(1, '#e2a13b');}
      if (!this.catRows.children.length) this.categoryRows();
      if (!modelLoaded()) this.message('Load a model first; rendering reads the stored model.', 'warning');
    }
    stopRow(value = 1, color = '#c78551') {
      const row = make('div', '', 'output-row'); const index = this.stops.children.length + 1;
      this.field(row, `Stop ${index} value`, this.input('number', {step: 'any', value: String(value)})).dataset.role = 'value';
      this.field(row, `Stop ${index} colour`, this.input('color', {value: color})).dataset.role = 'color';
      const remove = this.button('Remove', () => {if (this.stops.children.length > 2) row.remove();}); remove.setAttribute('aria-label', `Remove colour stop ${index}`); row.append(remove);
      this.stops.append(row);
    }
    categoryRows() {
      let count; try { count = numberList(this.catThresholds.value, 'Category thresholds').length + 1; } catch (_) { return; }
      const palette = ['#4f6d8f', '#d98e32', '#5aa469', '#b04a5a', '#7b5ea7', '#3aa0a8'];
      const previous = [...this.catRows.children].map(row => ({id: row.querySelector('[data-role=id]').value, label: row.querySelector('[data-role=label]').value, color: row.querySelector('[data-role=color]').value}));
      this.catRows.replaceChildren();
      for (let index = 0; index < count; index++) {
        const row = make('div', '', 'output-row');
        this.field(row, `Category ${index + 1} id`, this.input('text', {value: previous[index]?.id || `category_${index + 1}`, maxLength: 80})).dataset.role = 'id';
        this.field(row, `Category ${index + 1} label`, this.input('text', {value: previous[index]?.label || `Category ${index + 1}`, maxLength: 160})).dataset.role = 'label';
        this.field(row, `Category ${index + 1} colour`, this.input('color', {value: previous[index]?.color || palette[index % palette.length]})).dataset.role = 'color';
        this.catRows.append(row);
      }
    }
    sync() {
      const kind = this.source.value; this.epoch.root.hidden = kind !== 'current_optimization_state';
      if (!this.epoch.root.hidden) this.epoch.sync();
      this.artifact.closest('label').hidden = kind !== 'result_artifact';
      const boundary = this.surfaceField.value === 'model_boundary';
      this.iso.closest('label').hidden = boundary; this.region.closest('label').hidden = !boundary;
      const fieldOption = [...this.colouring.options].find(option => option.value === 'field');
      if (fieldOption) {fieldOption.disabled = !this.colorField.options.length; if (fieldOption.disabled && this.colouring.value === 'field') this.colouring.value = 'default';}
      const mode = this.colouring.value, field = mode === 'field', mapping = this.mapping.value;
      this.surfaceColor.closest('label').hidden = mode !== 'uniform';
      for (const node of [this.colorField, this.mapping]) node.closest('label').hidden = !field;
      this.palette.closest('label').hidden = !field || mapping !== 'palette';
      this.rangeOn.closest('label').hidden = !field || mapping !== 'palette'; this.range.hidden = !field || mapping !== 'palette' || !this.rangeOn.checked;
      this.stops.hidden = this.addStop.hidden = !field || mapping !== 'stops';
      this.catThresholds.closest('label').hidden = this.catRows.hidden = !field || mapping !== 'categories';
      const clip = this.clipMode.value; const viewportPlane = viewportClip();
      const viewportOption = [...this.clipMode.options].find(option => option.value === 'viewport');
      if (viewportOption) viewportOption.disabled = !viewportPlane;
      if (clip === 'viewport' && !viewportPlane) this.clipMode.value = 'off';
      this.clipCustom.hidden = this.clipMode.value !== 'custom';
      for (const node of [this.keep, this.capInside, this.capColor]) node.closest('label').hidden = this.clipMode.value === 'off';
      this.capOn.closest('label').hidden = this.capColorOn.closest('label').hidden = this.clipMode.value === 'off';
      this.capInside.disabled = !this.capOn.checked; this.capColorOn.disabled = !this.capOn.checked; this.capColor.disabled = !this.capOn.checked || !this.capColorOn.checked;
      const viewportCameraOption = [...this.camera.options].find(option => option.value === 'viewport');
      if (viewportCameraOption) viewportCameraOption.disabled = !viewportCamera();
      this.runButton.disabled = this.busy || !modelLoaded();
    }
    payload() {
      const payload = {}; const kind = this.source.value;
      if (kind === 'current_optimization_state') payload.source = {kind, ...this.epoch.value()};
      else if (kind === 'result_artifact') {
        const id = this.artifact.value.trim(); if (!/^result-[0-9a-f]{32}$/.test(id)) throw new Error('Enter a result artifact id (result- followed by 32 hex characters).');
        payload.source = {kind, artifact_id: id};
      } else payload.source = {kind: 'current_model'};
      payload.surface_field = this.surfaceField.value;
      if (payload.surface_field === 'model_boundary') payload.region = this.region.value;
      else payload.iso_value = finite(this.iso.value, 'Iso value');
      if (this.colouring.value === 'uniform') payload.surface_color_rgb = rgb255(this.surfaceColor.value);
      else if (this.colouring.value === 'field') {
        if (!this.colorField.value) throw new Error('The current model registers no field to colour by.');
        payload.color_field = this.colorField.value;
        if (this.mapping.value === 'palette') {
          payload.palette = this.palette.value;
          if (this.rangeOn.checked) {
            const range = [finite(this.rangeLow.value, 'Range minimum'), finite(this.rangeHigh.value, 'Range maximum')];
            if (!(range[1] > range[0])) throw new Error('The value range must increase.');
            payload.value_range = range;
          }
        } else if (this.mapping.value === 'stops') {
          payload.color_stops = [...this.stops.children].map((row, index) => ({value: finite(row.querySelector('[data-role=value]').value, `Stop ${index + 1} value`), color_rgb: rgb255(row.querySelector('[data-role=color]').value)}));
          for (let index = 1; index < payload.color_stops.length; index++)
            if (!(payload.color_stops[index].value > payload.color_stops[index - 1].value)) throw new Error('Colour stop values must be strictly increasing.');
        } else {
          const thresholds = numberList(this.catThresholds.value, 'Category thresholds');
          if (this.catRows.children.length !== thresholds.length + 1) this.categoryRows();
          payload.color_categories = {thresholds, categories: [...this.catRows.children].map(row => ({
            id: row.querySelector('[data-role=id]').value.trim(), label: row.querySelector('[data-role=label]').value.trim(), color_rgb: rgb255(row.querySelector('[data-role=color]').value)}))};
          if (payload.color_categories.categories.some(row => !row.id || !row.label)) throw new Error('Every category needs an id and a label.');
        }
      }
      if (this.clipMode.value !== 'off') {
        let point, normal;
        if (this.clipMode.value === 'viewport') {
          const plane = viewportClip(); if (!plane) throw new Error('Choose a clip plane in the viewport toolbar first.');
          normal = plane.normal; const [lo, hi] = plane.bbox_mm; point = lo.map((value, axis) => (value + hi[axis]) / 2);
          const length = Math.hypot(...normal); const along = plane.offset_mm / length;
          const unit = normal.map(value => value / length); const shift = along - point.reduce((sum, value, axis) => sum + value * unit[axis], 0);
          point = point.map((value, axis) => value + shift * unit[axis]);
        } else {
          point = this.clipPoint.map((input, axis) => finite(input.value, `Clip point ${'xyz'[axis]}`));
          normal = this.clipNormal.map((input, axis) => finite(input.value, `Clip normal ${'xyz'[axis]}`));
          if (Math.hypot(...normal) < 1e-12) throw new Error('The clip normal must be nonzero.');
        }
        payload.clip = {point_mm: point, normal, keep: this.keep.value};
        if (this.capOn.checked) {
          payload.clip.cap = {inside: this.capInside.value};
          if (this.capColorOn.checked) payload.clip.cap.complement_color_rgb = rgb255(this.capColor.value);
        }
      }
      const fov = finite(this.fov.value, 'Field of view'); if (fov < 15 || fov > 90) throw new Error('Field of view must lie between 15° and 90°.');
      if (this.camera.value === 'viewport') {
        const camera = viewportCamera(); if (!camera) throw new Error('The viewport camera is not available.');
        payload.camera = camera;
      } else payload.camera = {preset: this.camera.value, fov_deg: fov};
      payload.quality = this.quality.value; payload.antialias = Number(this.antialias.value);
      payload.width_px = Number(this.width.value); payload.height_px = Number(this.height.value);
      for (const [key, input] of [['width_px', this.width], ['height_px', this.height]])
        if (!Number.isSafeInteger(payload[key]) || payload[key] < Number(input.min) || payload[key] > Number(input.max)) throw new Error(`${human(key)} must be a whole number from ${input.min} to ${input.max}.`);
      payload.background = this.background.value;
      return payload;
    }
    async run() {
      const payload = this.payload();
      const result = await this.perform('Rendering on the service…', () => runAction('render_3d', payload));
      const image = global.ImplexityAdvancedCommandCore.resultImages(result)[0];
      this.result.replaceChildren();
      if (!image) {this.message('The service returned no image.', 'error'); return;}
      const figure = make('figure', '', 'advanced-command-image'); const img = make('img'); img.src = image.src; img.alt = 'Rendered image of the requested source';
      figure.append(img, make('figcaption', `${image.width_px || ''}×${image.height_px || ''} px · ${result.truth_status || ''}`)); this.result.append(figure);
      const stem = fileSafe(`implexity_render_${payload.surface_field}`);
      const saves = make('div', '', 'advanced-command-actions');
      saves.append(this.button('Save image (PNG)', () => { const link = make('a'); link.href = image.src; link.download = `${stem}.${image.mime_type.split('/')[1]}`; document.body.append(link); link.click(); link.remove(); }),
        this.button('Save render record (JSON)', () => { const record = structuredClone(result); saveJSON({request: payload, record}, `${stem}_record.json`); }));
      this.result.append(saves);
      this.message('Rendered. Review the record before publishing the image.', 'success'); this.reveal();
    }
  }

   
  class SectionImageDialog extends OutputDialog {
    constructor() {
      super('outputSectionImage', 'Section image', 'A bounded PNG section of the current model or an applied optimisation iterate, rasterised by the same signed field evaluator the service uses.');
      const source = this.section('Source');
      this.source = this.field(source, 'Geometry source', this.select([]));
      this.epoch = new EpochPicker(this, {allowCurrent: true}); source.append(this.epoch.root);
      const plane = this.section('Section');
      this.plane = this.field(plane, 'Plane normal', this.select([]));
      this.position = this.field(plane, 'Position (0–1 of the extent)', this.input('number', {min: '0', max: '1', step: '0.01', value: '0.5'}));
      this.field_ = this.field(plane, 'Field', this.select([['model_boundary', 'Model boundary (signed geometry)']]));
      this.scalar = this.field(plane, 'Vector component', this.select([['', 'Scalar field'], ['x', 'x component'], ['y', 'y component'], ['z', 'z component'], ['magnitude', 'Magnitude']]));
      this.palette = this.field(plane, 'Palette', this.select([]));
      this.rangeOn = this.check(plane, 'Explicit value range');
      this.range = make('div', '', 'output-vector-grid'); plane.append(this.range);
      this.rangeLow = this.field(this.range, 'Range minimum', this.input('number', {step: 'any', value: '0'}));
      this.rangeHigh = this.field(this.range, 'Range maximum', this.input('number', {step: 'any', value: '1'}));
      this.overlay = this.field(plane, 'Overlay regions', this.input('text', {placeholder: 'region ids, comma-separated'}), 'Named regions drawn over the section.');
      const image = this.section('Image');
      this.fit = this.field(image, 'Size', this.select([['physical_aspect', 'Equal physical scale'], ['fixed', 'Fixed pixels']]));
      this.maxDimension = this.field(image, 'Longest side (px)', this.input('number', {min: '64', max: '384', step: '1', value: '384'}));
      this.width = this.field(image, 'Width (px)', this.input('number', {min: '64', max: '384', step: '1', value: '320'}));
      this.height = this.field(image, 'Height (px)', this.input('number', {min: '64', max: '384', step: '1', value: '320'}));
      this.annotations = this.check(image, 'Axes, units and margins');
      this.background = this.field(image, 'Background', this.select([]));
      this.runButton = this.button('Render section', () => this.run(), 'primary'); this.runButton.id = 'outputSectionImageRun';
      this.actions.append(this.button('Check request', () => this.checkRequest('render_section')), this.runButton);
      this.form.addEventListener('change', () => this.sync());
    }
    async prepare(preset) {
      const schema = this.schema = (await service.tool('render_section')).input_schema;
      refill(this.source, sourceChoices(schema, ['source', 'kind'], {current_model: 'Current model', current_optimization_state: 'Live optimisation state (applied iterate)'}), preset.source?.kind || this.source.value || 'current_model');
      this.epoch.refresh(preset.source || {});
      refill(this.plane, enumOf(schema, ['plane'], ['x', 'y', 'z']).map(value => [value, `⟂ ${value.toUpperCase()}`]), this.plane.value || 'z');
      refill(this.palette, enumOf(schema, ['palette'], ['auto']).map(value => [value, human(value)]), this.palette.value || 'auto');
      refill(this.background, enumOf(schema, ['background'], ['dark', 'white']).map(value => [value, human(value)]), this.background.value || 'white');
      const catalogue = await service.renderables();
      refill(this.field_, fieldOptions((catalogue?.section_fields || []).filter(row => row.field !== 'model_boundary'), {includeBoundary: true}));
      if (!modelLoaded()) this.message('Load a model first; sections read the stored model.', 'warning');
    }
    sync() {
      this.epoch.root.hidden = this.source.value !== 'current_optimization_state'; if (!this.epoch.root.hidden) this.epoch.sync();
      const fixed = this.fit.value === 'fixed';
      this.maxDimension.closest('label').hidden = fixed; this.width.closest('label').hidden = this.height.closest('label').hidden = !fixed;
      this.range.hidden = !this.rangeOn.checked;
      this.runButton.disabled = this.busy || !modelLoaded();
    }
    payload() {
      const payload = {};
      payload.source = this.source.value === 'current_optimization_state' ? {kind: 'current_optimization_state', ...this.epoch.value()} : {kind: 'current_model'};
      payload.plane = this.plane.value;
      const position = finite(this.position.value, 'Position'); if (position < 0 || position > 1) throw new Error('Position must lie between 0 and 1.');
      payload.position = position; payload.field = this.field_.value; payload.palette = this.palette.value;
      if (this.scalar.value === 'magnitude') payload.scalarization = {magnitude: true};
      else if (this.scalar.value) payload.scalarization = {component: this.scalar.value};
      if (this.rangeOn.checked) {
        payload.value_range = [finite(this.rangeLow.value, 'Range minimum'), finite(this.rangeHigh.value, 'Range maximum')];
        if (!(payload.value_range[1] > payload.value_range[0])) throw new Error('The value range must increase.');
      }
      const regions = this.overlay.value.split(',').map(item => item.trim()).filter(Boolean);
      if (regions.length) payload.overlay_regions = [...new Set(regions)];
      const pixels = (input, label) => { const value = Number(input.value); if (!Number.isSafeInteger(value) || value < 64 || value > 384) throw new Error(`${label} must be a whole number from 64 to 384.`); return value; };
      if (this.fit.value === 'fixed') {payload.width_px = pixels(this.width, 'Width'); payload.height_px = pixels(this.height, 'Height');}
      else {payload.fit = 'physical_aspect'; payload.max_dimension_px = pixels(this.maxDimension, 'Longest side');}
      if (this.annotations.checked) payload.annotations = true;
      payload.background = this.background.value;
      return payload;
    }
    async run() {
      const payload = this.payload();
      const result = await this.perform('Rendering the section…', () => runAction('render_section', payload));
      const image = global.ImplexityAdvancedCommandCore.resultImages(result)[0];
      this.result.replaceChildren();
      if (!image) {this.message('The service returned no image.', 'error'); return;}
      const figure = make('figure', '', 'advanced-command-image'); const img = make('img'); img.src = image.src; img.alt = `Section ⟂ ${payload.plane} at ${payload.position}`;
      figure.append(img, make('figcaption', `${payload.field} · plane ⟂ ${payload.plane} at ${payload.position}`)); this.result.append(figure);
      const stem = fileSafe(`implexity_section_${payload.plane}_${payload.position}`);
      const saves = make('div', '', 'advanced-command-actions');
      saves.append(this.button('Save image (PNG)', () => { const link = make('a'); link.href = image.src; link.download = `${stem}.png`; document.body.append(link); link.click(); link.remove(); }),
        this.button('Save section record (JSON)', () => saveJSON({request: payload, record: result}, `${stem}_record.json`)));
      this.result.append(saves); this.message('Section rendered.', 'success'); this.reveal();
    }
  }

   
  class EpochModelDialog extends OutputDialog {
    constructor() {
      super('outputEpochModel', 'Export optimisation update as model', 'Writes one published, digest-verified update as a self-contained implicit-model document. Read-only: the update is not applied and the live model is not changed.');
      const source = this.section('Update');
      this.epoch = new EpochPicker(this, {allowCurrent: false}); source.append(this.epoch.root);
      this.runButton = this.button('Export model document', () => this.run(), 'primary'); this.runButton.id = 'outputEpochModelRun';
      this.actions.append(this.button('Check request', () => this.checkRequest('export_optimization_epoch')), this.runButton);
    }
    async prepare(preset) { await service.tool('export_optimization_epoch'); this.epoch.refresh(preset.source || preset); }
    sync() { this.epoch.sync(); }
    payload() { return this.epoch.value(); }
    async run() {
      const payload = this.payload();
      const result = await this.perform('Reconstructing the saved epoch…', () => runAction('export_optimization_epoch', payload));
      this.result.replaceChildren();
      const facts = make('dl', '', 'application-case-facts');
      for (const key of ['schema', 'job_id', 'epoch', 'document_sha256', 'document_bytes'])
        if (result?.[key] !== undefined) facts.append(make('dt', human(key)), make('dd', String(result[key])));
      const stem = `implexity_run_${payload.job_id}_update_${payload.epoch}`;
      const saves = make('div', '', 'advanced-command-actions');
      if (object(result?.document)) saves.append(this.button('Save model document (JSON)', () => saveJSON(result.document, `${stem}_model.json`)));
      saves.append(this.button('Save export record (JSON)', () => saveJSON(result, `${stem}_record.json`)));
      this.result.append(facts, saves);
      this.message('Epoch exported. The saved model document can be imported as a new model.', 'success'); this.reveal();
    }
  }

   
  class BodyExportDialog extends OutputDialog {
    constructor() {
      super('outputBodyExport', 'Export solid body', 'The accepted design as a closed body: a STEP boundary-representation solid of planar faces at a declared chord tolerance, or STL/3MF/PLY meshes, capped against the domain boundary. A provenance record travels with every file. Not a re-engineered analytic model.');
      const body = this.section('Body');
      this.format = this.field(body, 'Formats', this.select([['step', 'STEP (solid)'], ['stl', 'STL (mesh)'], ['3mf', '3MF (mesh)'], ['ply', 'PLY (mesh)'],
        ['step+stl', 'STEP + STL'], ['mesh', 'STL + PLY + 3MF'], ['all', 'All four']], 'step'));
      this.tolerance = this.field(body, 'Chord tolerance', this.select([['0.05', '0.05 mm'], ['0.02', '0.02 mm'], ['0.01', '0.01 mm'], ['0.005', '0.005 mm']], '0.02'));
      this.cap = this.field(body, 'Cap', this.select([['1', 'Cap on the part'], ['0', 'Cap on the box']], '1'));
      this.schema = this.field(body, 'STEP schema', this.select([['AP214', 'AP214'], ['AP203', 'AP203'], ['AP242', 'AP242']], 'AP214'));
      this.merge = this.check(body, 'Merge coplanar faces (STEP)', true);
      this.estimate = make('p', 'Estimate: -', 'advanced-help'); this.estimate.id = 'outputBodyEstimate'; body.append(this.estimate);
      this.progress = make('progress', '', 'output-progress'); this.progress.max = 1; this.progress.value = 0; this.progress.setAttribute('aria-label', 'Body export progress');
      this.form.append(this.progress);
      this.estimateButton = this.button('Estimate', () => this.estimateBody());
      this.runButton = this.button('Export body', () => this.run(), 'primary'); this.runButton.id = 'outputBodyExportRun';
      this.cancelButton = this.button('Cancel export', () => this.cancel()); this.cancelButton.hidden = true;
      this.resumeButton = this.button('Refresh export status', () => this.poll()); this.resumeButton.hidden = true;
      this.actions.append(this.estimateButton, this.runButton, this.cancelButton, this.resumeButton);
      this.form.addEventListener('change', () => this.renderEstimate());
    }
    formatsOf(value) {
      return value === 'all' ? ['step', 'stl', 'ply', '3mf'] : value === 'mesh' ? ['stl', 'ply', '3mf'] : value === 'step+stl' ? ['step', 'stl'] : [value];
    }
    formats() { return this.formatsOf(this.format.value); }
     
     
    async prepare() {
      const info = await request('GET', '/v1/model');
      this.available = Boolean(info?.body?.endpoint) && info.backend !== 'synthetic';
      const catalogue = new Map((info?.body?.format_catalogue || []).map(row => [row.format, row]));
      for (const option of this.format.options) {
        const formats = this.formatsOf(option.value);
        const missing = formats.filter(format => catalogue.has(format) && catalogue.get(format).available === false);
        option.disabled = missing.length > 0;
        option.title = missing.length ? missing.map(format => `${format}: ${catalogue.get(format).detail || 'unavailable'}`).join('; ') : '';
      }
      if (this.format.selectedOptions[0]?.disabled) this.format.value = [...this.format.options].find(option => !option.disabled)?.value || this.format.value;
      if (!this.available) this.message('This service runs the empty native workspace, which has no study design for the solid body export. Start the service with a study design (--bundle/--design or --backend real) to export bodies; use Export STL for the implicit model.', 'warning');
      this.renderEstimate();
    }
    sync() {
      this.runButton.disabled = this.busy || Boolean(this.job) || !this.available;
      this.estimateButton.disabled = this.busy || !this.available;
      this.cancelButton.hidden = !this.job; this.cancelButton.disabled = false;
      this.resumeButton.hidden = !this.job; this.resumeButton.disabled = Boolean(this.polling);
    }
    async estimateBody() {
      this.est = await this.perform('Estimating from one coarse extraction of this design…', () =>
        request('POST', '/v1/body/estimate', {domain_aware: this.cap.value === '1', tolerances: [0.05, 0.02, 0.01, 0.005]}));
      this.renderEstimate(); this.message('Estimate measured on this design.', 'success');
    }
    renderEstimate() {
      this.schema.closest('label').hidden = this.merge.closest('label').hidden = !this.formats().includes('step');
      const est = this.est; if (!est) {this.estimate.textContent = 'Estimate: -'; return;}
      const row = est.at_tolerance?.[Number(this.tolerance.value).toFixed(4)];
      if (!row) {this.estimate.textContent = 'Estimate: -'; return;}
      let text = `${row.triangles.toLocaleString()} triangles at ${row.spacing_mm.toFixed(4)} mm sampling ≈ ${(row.stl_bytes_estimate / 1048576).toFixed(1)} MB STL`;
      if (row.over_budget) text += ': over the sample budget; it will be refused';
      const step = est.step || {};
      if (this.formats().includes('step')) {
        if (!step.available) text += `. STEP unavailable on this service: ${step.detail || 'no OpenCASCADE binding'}`;
        else if (row.step_faces_merged_estimate !== undefined) text += `. STEP ≈ ${row.step_faces_merged_estimate.toLocaleString()} planar faces (${row.step_faces_unmerged.toLocaleString()} unmerged)${row.step_over_face_budget ? ': over the STEP budget; it will be refused' : ''}`;
      }
      this.estimate.textContent = text + '.';
    }
    async run() {
      const body = {format: this.formats(), tolerance_mm: Number(this.tolerance.value), domain_aware: this.cap.value === '1', components: 'all',
        step_merge: this.merge.checked, step_schema: this.schema.value, channel: 'body', seq: Date.now() % 2147483647, name: 'implexity_body'};
      const reply = await this.perform('Submitting the export job…', () => request('POST', '/v1/body', body));
      this.job = reply.job_id; this.result.replaceChildren(); this.progress.value = 0; this.sync();
      this.message(`Job ${this.job} queued.`);
      await this.poll();
    }
    async poll() {
      if(this.polling || !this.job) return;
      this.polling = true; this.sync();
      let failures = 0;
      try{
        while(this.job){
          const id = this.job;
          let job;
          try{job = await request('GET', `/v1/body/jobs/${encodeURIComponent(id)}`);}
          catch(error){
            if(this.job !== id) return;
            if(++failures >= 3){
              this.message('Export status unavailable. Use Refresh export status to reconnect.', 'warning');
              return;
            }
            this.message('Connection interrupted. Retrying export status…', 'warning');
            await new Promise(resolve => setTimeout(resolve, 1200));
            continue;
          }
          if(this.job !== id) return;
          failures = 0;
          this.progress.value = Number(job.progress) || 0; this.message(`${job.status}: ${job.message || ''}`);
          if(job.status === 'completed' || job.status === 'failed'){
            this.job = null;
            if(job.status === 'completed') this.show(job);
            else this.message(`Export failed: ${job.error || job.message || 'unknown error'}`, 'error');
            return;
          }
          await new Promise(resolve => setTimeout(resolve, 1200));
        }
      }finally{this.polling = false; this.sync();}
    }
    async cancel() {
      if (!this.job) return; const job = this.job;
      await request('POST', `/v1/body/jobs/${encodeURIComponent(job)}`, {op: 'cancel'});
      this.message(`Cancel requested for job ${job}.`, 'warning');
    }
    show(job) {
      this.result.replaceChildren();
      const accepted = job.accepted || {}; const extraction = job.report?.extraction || {};
      const facts = make('dl', '', 'application-case-facts');
      const fact = (key, value) => facts.append(make('dt', key), make('dd', String(value)));
      fact('Watertight', accepted.watertight ? 'yes' : 'NO'); fact('Boundary edges', accepted.boundary_edges); fact('Non-manifold edges', accepted.nonmanifold_edges);
      fact('Triangles', (extraction.triangles ?? 'N/A').toLocaleString()); fact('Components', accepted.components); fact('Genus', accepted.genus);
      if (extraction.volume) fact('Body volume (mm³)', Number(extraction.volume.mesh_mm3).toFixed(2));
      const step = job.report?.step?.accepted;
      if (step) {fact('STEP valid solid', step.valid_solid ? 'yes' : 'NO'); fact('STEP planar faces', step.faces); fact('STEP schema', step.schema);}
      const links = make('div', '', 'advanced-command-actions');
      for (const [name, file] of Object.entries(job.files || {})) {
        const link = make('a', `Download ${name} · ${Math.round((file.bytes || 0) / 1024)} kB`, 'output-download'); link.href = file.download; link.download = '';
        links.append(link);
      }
      this.result.append(facts, links); this.message('Body exported. Download the files below.', 'success'); this.reveal();
    }
  }

   
  const DIALOGS = {export_stl: ExportSTLDialog, render_3d: RenderImageDialog, render_section: SectionImageDialog,
    export_optimization_epoch: EpochModelDialog, body: BodyExportDialog};
  const MENU = [
    ['export_stl', 'Export STL…', 'Watertight STL files of the model, the live iterate or a published update'],
    ['body', 'Export solid body (STEP/3MF/PLY)…', 'A STEP solid or meshes of the accepted design with provenance'],
    ['export_optimization_epoch', 'Export epoch as model…', 'A saved optimization epoch as an importable model document'],
    ['render_3d', 'Render image…', 'A service-rendered PNG with camera, clip, colouring and quality'],
    ['render_section', 'Section image…', 'A PNG section through the model or a registered field'],
  ];
  const instances = new Map();
  async function open(kind, preset = {}) {
    const Dialog = DIALOGS[kind]; if (!Dialog) throw new Error(`Unknown output dialog ${kind}.`);
    if (!instances.has(kind)) instances.set(kind, new Dialog());
    await instances.get(kind).open(preset);
    return instances.get(kind);
  }
  function buildMenu() {
    const host = document.querySelector('#implexityTopbarCommands') || document.querySelector('#topbar'); if (!host) return;
    const menu = make('details', '', 'implexity-output-menu'); menu.id = 'implexityOutputMenu';
    const summary = make('summary', 'Output'); summary.title = 'Export geometry, render images and sections'; menu.append(summary);
    const list = make('div', '', 'implexity-output-menu-list'); list.setAttribute('role', 'group'); list.setAttribute('aria-label', 'Output');
    for (const [kind, label, title] of MENU) {
      const item = make('button', label); item.type = 'button'; item.title = title; item.dataset.output = kind;
      item.addEventListener('click', () => { menu.open = false; open(kind).catch(error => console.warn(error)); });
      list.append(item);
    }
    menu.append(list); host.prepend(menu);
    document.addEventListener('click', event => { if (menu.open && !menu.contains(event.target)) menu.open = false; });
    menu.addEventListener('keydown', event => { if (event.key === 'Escape' && menu.open) {menu.open = false; summary.focus();} });
  }
  global.ImplexityOutputDialogs = Object.freeze({open, dialogs: instances});
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', buildMenu, {once: true}); else buildMenu();
})(globalThis);
