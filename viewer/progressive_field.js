// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(() => {
  "use strict";

  const VERSION = "22.0.0";
  const IDLE_MS = 150;
  const QUALITY_KEY = "implexity.fieldStream.quality";
  const MAX_ATOMIC_DELTA_FRACTION = 0.25;
  const MAX_ATOMIC_DELTA_BYTES = 8 * 1024 * 1024;
  const describe = (identifier, metadata = {}) => window.ImplexityText?.present?.(identifier, metadata) || ({
    id: String(identifier ?? ""),
    label: String(metadata.label || metadata.display_name || metadata.title || identifier || ""),
    unit: String(metadata.unit || metadata.units || ""),
    description: String(metadata.description || metadata.help || ""),
  });
  const human = (identifier, metadata = {}) =>
    window.ImplexityText?.humanize?.(identifier, metadata) || describe(identifier, metadata).label;
  const state = {
    generation: 0,
    active: false,
    idleTimer: 0,
    worker: null,
    requestSerial: 0,
    requests: new Map(),
    aborters: new Set(),
    current: null,
    stagingName: "implexity_field_a",
    alternateName: "implexity_field_b",
    focus: null,
    available: [],
    selectedFieldName: null,
    selectedViews: new Map(),
    quality: "auto",
    intercepted: new WeakSet(),
    metrics: {
      requested: 0, decoded: 0, uploaded: 0, bytes: 0,
      stale: 0, cancelled: 0, cacheHits: 0, deltaFallbacks: 0,
      atomicDeltas: 0, fullReloads: 0, clearedUploads: 0,
      stagedDrains: 0, rangeExpansions: 0,
      failed: 0, viewChanges: 0, qualityChanges: 0,
    },
  };

  function viewportElement() {
    const found = document.querySelector("[data-implexity-viewport], #viewport, .viewport, #glview, canvas");


    const isCanvas = typeof HTMLCanvasElement !== "undefined" && found instanceof HTMLCanvasElement;
    return isCanvas ? (found.parentElement || document.body) : (found || document.body);
  }

  function adapter() {
    const value = window.ImplexityViewerAdapter || window.ImplexityViewer || window.viewer;
    if (!value) return null;
    const required = ["allocateTexture", "queueTextureRegion", "setColorBy"];
    return required.every(name => typeof value[name] === "function") ? value : null;
  }

  function statusElement() {
    let element = document.getElementById("implexity-field-stream-status");
    if (!element) {
      element = document.createElement("div");
      element.id = "implexity-field-stream-status";
      element.className = "implexity-field-stream-status";
      element.setAttribute("role", "status");
      element.setAttribute("aria-live", "polite");
      element.setAttribute("aria-atomic", "true");
      element.hidden = true;
      viewportElement().appendChild(element);
    }
    return element;
  }

  function setStatus(text, kind = "working", technical = null) {
    const element = statusElement();
    if (!text) {
      element.replaceChildren();
      element.setAttribute("role", "status");
    } else if (kind === "error" && technical && window.ImplexityUIPresentation?.render) {
      window.ImplexityUIPresentation.render(element, { summary: text, technical, severity: "error" });
    } else {
      element.textContent = text;
      element.setAttribute("role", kind === "error" ? "alert" : "status");
    }
    element.dataset.kind = kind;
    element.setAttribute("aria-live", kind === "error" ? "assertive" : "polite");
    element.setAttribute("aria-atomic", "true");
    element.hidden = !text;
  }

  function viewsFor(stream) {
    const declared = Array.isArray(stream?.views)
      ? stream.views.filter(item => item && item.id != null)
      : [];
    if (declared.length) return declared;
    return [{
      id: "value", label: "Value",
      component: stream?.component == null ? 0 : stream.component,
      range: stream?.visual_range,
    }];
  }

  function fieldPresentation(stream) {
    const identifier = String(stream?.fieldName || stream?.field_name || stream?.response_id || "field");
    return describe(identifier, {
      label: stream?.field_label || stream?.display_name || stream?.label,
      unit: stream?.units || stream?.unit,
      description: stream?.description || stream?.help,
    });
  }

  function viewPresentation(view) {
    return describe(view?.id || "value", {
      label: view?.label || view?.display_name,
      unit: view?.units || view?.unit,
      description: view?.description || view?.help,
    });
  }

  function truthPresentation(ref, manifest = null) {
    const declared = ref?.truth_status ?? ref?.truthStatus ?? manifest?.truth_status ?? manifest?.truthStatus;
    const raw = declared && typeof declared === "object"
      ? (declared.status ?? declared.kind ?? declared.value ?? declared.id)
      : declared;
    const token = String(raw ?? "").trim().toLowerCase().replace(/[\s-]+/g, "_");
    const suppliedLabel = declared && typeof declared === "object" ? String(declared.label || "").trim() : "";
    if (["exact", "exact_solver", "authoritative_exact"].includes(token))
      return { code: token, kind: "authoritative", label: suppliedLabel || "Exact-solver result" };
    if (["authoritative", "verified", "accepted"].includes(token))
      return { code: token, kind: "authoritative", label: suppliedLabel || "Authoritative result" };
    if (["approximate", "preview", "surrogate", "interpolated"].includes(token))
      return { code: token, kind: "approximate", label: suppliedLabel || "Approximate preview" };
    if (token) return { code: token, kind: "unknown", label: suppliedLabel || human(token) };
    return { code: "undeclared", kind: "unknown", label: "Truth status not declared" };
  }

  function selectedView(stream) {
    const views = viewsFor(stream);
    const remembered = state.selectedViews.get(stream.fieldName);
    const wanted = remembered || stream.default_view || views[0].id;
    return views.find(item => String(item.id) === String(wanted)) || views[0];
  }

  function withSelectedView(stream) {
    const view = selectedView(stream);
    const field = fieldPresentation(stream);
    const displayedView = viewPresentation(view);
    return {
      ...stream,
      viewId: String(view.id),
      fieldLabel: field.label,
      fieldDescription: field.description,
      viewLabel: displayedView.label,
      viewDescription: displayedView.description,
      unit: displayedView.unit || field.unit || "",
      component: view.component == null ? 0 : view.component,
      visual_range: Array.isArray(view.range) ? view.range : stream.visual_range,
      truth_status: view.truth_status ?? stream.truth_status,
    };
  }

  function streamKey(ref) {
    return `${String(ref?.fieldName || ref?.field_name || "field")}::${String(ref?.viewId || ref?.default_view || "value")}`;
  }

  function populateViewSelect(panel, stream) {
    const select = panel.querySelector('[data-role="view"]');
    const views = viewsFor(stream);
    select.replaceChildren(...views.map(item => {
      const option = document.createElement("option");
      option.value = String(item.id);
      const displayed = viewPresentation(item);
      option.textContent = displayed.label;
      option.title = displayed.description || `Technical view identifier: ${String(item.id)}`;
      return option;
    }));
    const view = selectedView(stream);
    select.value = String(view.id);
    select.disabled = views.length < 2;
    select.setAttribute("aria-disabled", String(views.length < 2));
    select.setAttribute("aria-description", views.length < 2 ? "This field provides one display view." : `${views.length} display views are available.`);
    const help=panel.querySelector("#implexity-field-view-help");if(help)help.textContent=views.length < 2 ? "This field has one display view." : `${views.length} display views are available.`;
  }

  function controls() {
    let panel = document.getElementById("implexity-field-stream-controls");
    if (panel) return panel;
    panel = document.createElement("div");
    panel.id = "implexity-field-stream-controls";
    panel.className = "implexity-field-stream-controls";
    panel.setAttribute("role","region");
    panel.setAttribute("aria-label","Engineering field display controls");
    panel.setAttribute("aria-describedby","implexity-field-view-help");
    panel.hidden = true;
    panel.innerHTML = `
      <label>Field
        <select data-role="field" aria-label="Displayed engineering field"></select>
      </label>
      <label>View
        <select data-role="view" aria-label="Displayed field component"></select>
      </label>
      <label>Quality
        <select data-role="quality" aria-label="Field visualisation quality">
          <option value="auto">Auto</option>
          <option value="responsive">Responsive</option>
          <option value="exact">Exact when idle</option>
        </select>
      </label>
      <button type="button" data-action="hide" title="Hide field colours">Hide</button>
      <p id="implexity-field-view-help" class="implexity-field-view-help" role="status" aria-live="polite" aria-atomic="true">Choose an engineering field to inspect.</p>`;
    const fieldSelect = panel.querySelector('[data-role="field"]');
    const viewSelect = panel.querySelector('[data-role="view"]');
    const qualitySelect = panel.querySelector('[data-role="quality"]');
    qualitySelect.value = state.quality;
    fieldSelect.addEventListener("change", () => {
      const stream = state.available.find(item => item.fieldName === fieldSelect.value);
      if (stream) {
        state.selectedFieldName = stream.fieldName;
        populateViewSelect(panel, stream);
        present(withSelectedView(stream), { fieldName: stream.fieldName });
      }
    });
    viewSelect.addEventListener("change", () => {
      const stream = state.available.find(item => item.fieldName === fieldSelect.value);
      if (!stream) return;
      state.selectedViews.set(stream.fieldName, viewSelect.value);
      state.metrics.viewChanges += 1;
      present(withSelectedView(stream), { fieldName: stream.fieldName });
    });
    qualitySelect.addEventListener("change", () => {
      const value = ["auto", "responsive", "exact"].includes(qualitySelect.value)
        ? qualitySelect.value : "auto";
      state.quality = value;
      state.metrics.qualityChanges += 1;
      try { localStorage.setItem(QUALITY_KEY, value); } catch (_) {                }
      const current = state.current;
      if (current) present(current.ref, { fieldName: current.fieldName });
    });
    panel.querySelector('[data-action="hide"]').addEventListener("click", () => {
      adapter()?.setColorBy?.(null);
      setStatus("");
      const fieldLegend = document.getElementById("implexity-field-legend");
      if (fieldLegend) fieldLegend.hidden = true;
    });
    viewportElement().appendChild(panel);
    return panel;
  }

  function legend() {
    let element = document.getElementById("implexity-field-legend");
    if (element) return element;
    element = document.createElement("aside");
    element.id = "implexity-field-legend";
    element.className = "implexity-field-legend";
    element.hidden = true;
    element.setAttribute("role", "region");
    element.setAttribute("tabindex", "0");
    element.setAttribute("data-viewport-shortcuts", "suspend");
    element.setAttribute("aria-labelledby", "implexity-field-legend-title");
    element.setAttribute("aria-describedby", "implexity-field-legend-truth implexity-field-legend-polarity");
    element.innerHTML = `
      <div class="implexity-field-legend-head"><strong data-legend="title" id="implexity-field-legend-title">Field</strong><span data-legend="truth" id="implexity-field-legend-truth" role="status" aria-live="polite" aria-atomic="true"></span></div>
      <div class="implexity-field-colourbar" data-legend="bar" aria-hidden="true"><i data-legend="zero"></i></div>
      <div class="implexity-field-ticks"><span data-legend="min"></span><span data-legend="zero-label">0</span><span data-legend="max"></span></div>
      <dl><div><dt>Range</dt><dd data-legend="policy"></dd></div><div><dt>Display detail</dt><dd data-legend="lod"></dd></div><div class="implexity-field-polarity"><dt>Colour meaning</dt><dd data-legend="polarity" id="implexity-field-legend-polarity"></dd></div></dl>`;


    if (typeof element.addEventListener === "function") {
      for (const type of ["pointerdown", "pointermove", "pointerup", "pointercancel", "contextmenu", "wheel"])
        element.addEventListener(type, event => event.stopPropagation(), { passive: true });
    }
    viewportElement().appendChild(element);
    return element;
  }

  function formatLegendValue(value) {
    const number = Number(value);
    if (!Number.isFinite(number)) return "N/A";
    if (number === 0) return "0";
    const magnitude = Math.abs(number);
    if (magnitude >= 1e5 || magnitude < 1e-3) return number.toExponential(3);
    return new Intl.NumberFormat(undefined, { maximumSignificantDigits: 5 }).format(number);
  }

  function rangePolicyLabel(policy) {
    return ({expand:"Stable, expands as needed",locked:"Locked",per_revision:"Per result"})[policy] || human(policy);
  }

  function updateLegend(current) {
    const element = legend();
    if (!current) { element.hidden = true; return; }


    if (typeof element.querySelector !== "function") return;
    const range = current.displayRange || [0, 1];
    const unit = current.unit ? ` ${current.unit}` : "";
    const title = current.viewLabel ? `${current.fieldLabel} · ${current.viewLabel}` : current.fieldLabel;
    element.querySelector('[data-legend="title"]').textContent = title;
    const truth = element.querySelector('[data-legend="truth"]');
    truth.textContent = current.truthStatus.label;
    truth.dataset.kind = current.truthStatus.kind;
    element.querySelector('[data-legend="min"]').textContent = `${formatLegendValue(range[0])}${unit}`;
    element.querySelector('[data-legend="max"]').textContent = `${formatLegendValue(range[1])}${unit}`;
    element.querySelector('[data-legend="policy"]').textContent = rangePolicyLabel(current.rangePolicy);
    element.querySelector('[data-legend="lod"]').textContent = current.renderExact ? "Full-resolution field" : `Render LOD ${current.level}`;
    const polarity=current.palette==="diverging"?"Signed values: blue is negative, neutral is zero, and orange is positive.":"Sequential values: the scale runs from the numeric minimum to maximum shown above.";
    element.querySelector('[data-legend="polarity"]').textContent=polarity;
    const zero = element.querySelector('[data-legend="zero"]');
    const zeroLabel = element.querySelector('[data-legend="zero-label"]');
    const containsZero = range[0] < 0 && range[1] > 0;
    zero.hidden = !containsZero;zeroLabel.hidden = !containsZero;
    if (containsZero) {
      const position = 100 * (0 - range[0]) / (range[1] - range[0]);
      zero.style.left = `${Math.max(0, Math.min(100, position))}%`;
      zeroLabel.style.left = zero.style.left;
    }
    element.dataset.palette = current.palette;
    element.dataset.truthStatus = current.truthStatus.code;
    element.setAttribute("aria-label", `${title}. Displayed range ${formatLegendValue(range[0])} to ${formatLegendValue(range[1])}${unit}. ${polarity} ${current.truthStatus.label}. ${current.renderExact ? "Full-resolution field" : `Render level ${current.level}`}.`);
    element.hidden = false;
  }

  function updateControls(streams) {
    state.available = streams.slice();
    const panel = controls();
    const select = panel.querySelector('[data-role="field"]');
    const remembered = state.selectedFieldName;
    select.replaceChildren(...streams.map(stream => {
      const option = document.createElement("option");
      option.value = stream.fieldName;
      const displayedField = fieldPresentation(stream);
      const displayedResponse = stream.response_id && stream.response_id !== stream.fieldName
        ? describe(stream.response_id, {
          label: stream.response_label,
          unit: stream.response_unit,
          description: stream.response_description,
        }) : null;
      const response = displayedResponse ? ` · ${displayedResponse.label}` : "";
      option.textContent = `${displayedField.label}${response}${displayedField.unit ? ` [${displayedField.unit}]` : ""}`;
      option.title = displayedField.description || `Technical field identifier: ${stream.fieldName}`;
      return option;
    }));
    const selected = streams.find(item => item.fieldName === remembered) || streams[0];
    if (selected) {
      select.value = selected.fieldName;
      state.selectedFieldName = selected.fieldName;
      populateViewSelect(panel, selected);
    }
    panel.querySelector('[data-role="quality"]').value = state.quality;
    panel.hidden = streams.length === 0;
    if (!streams.length) updateLegend(null);
    return selected ? withSelectedView(selected) : null;
  }

  function scriptURL(name) {
    const source = document.currentScript?.src || document.querySelector('script[src*="progressive_field"]')?.src;
    return source ? new URL(name, source).href : name;
  }

  function ensureVisibility() {
    if (globalThis.ImplexityFieldVisibility) return Promise.resolve(globalThis.ImplexityFieldVisibility);
    return new Promise((resolve, reject) => {
      const existing = document.querySelector('script[data-implexity-field-visibility]');
      if (existing) {
        existing.addEventListener("load", () => resolve(globalThis.ImplexityFieldVisibility), { once: true });
        existing.addEventListener("error", reject, { once: true });
        return;
      }
      const script = document.createElement("script");
      script.src = scriptURL("field_visibility.js");
      script.defer = true;
      script.dataset.implexityFieldVisibility = "";
      script.onload = () => resolve(globalThis.ImplexityFieldVisibility);
      script.onerror = () => reject(new Error("field visibility helper failed to load"));
      document.head.appendChild(script);
    });
  }

  function ensureWorker() {
    if (state.worker) return state.worker;
    state.worker = new Worker(scriptURL("field_tile_worker.js"));
    state.worker.onmessage = event => {
      const message = event.data || {};
      const pending = state.requests.get(message.requestId);
      if (!pending) return;
      state.requests.delete(message.requestId);
      if (message.generation !== pending.generation) {
        state.metrics.stale += 1;
        pending.reject(new DOMException("stale field tile", "AbortError"));
        return;
      }
      if (message.type === "error") {
        const error = new Error(message.error || "field tile decoding failed");
        error.retryRaw = Boolean(message.retryRaw);
        pending.reject(error);
        return;
      }
      state.metrics.decoded += 1;
      pending.resolve({ header: message.header, values: new Float32Array(message.values) });
    };
    state.worker.onerror = event => setStatus(
      "The field decoder stopped unexpectedly. Hide the field or reload the result before trying again.",
      "error",
      event?.error || event?.message || "The field worker reported an unknown error.",
    );
    return state.worker;
  }

  function decodeTile(buffer, header, encoding, generation, component) {
    const worker = ensureWorker();
    const requestId = ++state.requestSerial;
    return new Promise((resolve, reject) => {
      state.requests.set(requestId, { generation, resolve, reject });
      worker.postMessage({ type: "decode", generation, requestId, buffer, header, encoding, component }, [buffer]);
    });
  }

  async function fetchJSON(url, signal) {


    const response = await fetch(url, { signal, cache: "force-cache" });
    if (!response.ok) throw new Error(`${response.status} ${response.statusText}: ${url}`);
    return response.json();
  }

  function abortGeneration() {
    for (const controller of state.aborters) controller.abort();
    state.metrics.cancelled += state.aborters.size;
    state.aborters.clear();
    for (const [id, pending] of state.requests) {
      pending.reject(new DOMException("field generation superseded", "AbortError"));
      state.requests.delete(id);
    }
    const view = adapter();
    if (view?.clearTextureUpdates) {
      state.metrics.clearedUploads += Number(view.clearTextureUpdates(state.stagingName) || 0);
      state.metrics.clearedUploads += Number(view.clearTextureUpdates(state.alternateName) || 0);
    }
  }

  function deltaBytes(changes) {
    return (changes || []).reduce((sum, change) => {
      const declared = Number(change.raw_bytes);
      if (Number.isFinite(declared) && declared >= 0) return sum + declared;
      const valueShape = Array.isArray(change.value_shape) && change.value_shape.length >= 3
        ? change.value_shape : change.shape;
      if (!Array.isArray(valueShape) || valueShape.length < 3) return Number.POSITIVE_INFINITY;
      return sum + valueShape.reduce((count, value) => count * Number(value), 1) * 4;
    }, 0);
  }

  function canCommitDeltaAtomically(delta) {
    if (!delta?.compatible) return false;
    return Number(delta.changed_fraction || 0) <= MAX_ATOMIC_DELTA_FRACTION
      && deltaBytes(delta.changed) <= MAX_ATOMIC_DELTA_BYTES;
  }

  async function fetchTile(ref, level, descriptor, tileShape, generation, component, encoding = "gzip") {
    const controller = new AbortController();
    state.aborters.add(controller);
    const query = new URLSearchParams({
      level: String(level), index: descriptor.index.join(","),
      tile_shape: tileShape.join(","), encoding,
    });
    try {
      state.metrics.requested += 1;
      const response = await fetch(`${ref.tile}?${query}`, {
        signal: controller.signal,
        cache: "force-cache",
      });
      if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
      const headerText = response.headers.get("X-Implexity-Tile");
      if (!headerText) throw new Error("field tile response has no X-Implexity-Tile header");
      const header = JSON.parse(headerText);
      const buffer = await response.arrayBuffer();
      state.metrics.bytes += buffer.byteLength;
      try {
        return await decodeTile(buffer, header, encoding, generation, component);
      } catch (error) {
        if (error.retryRaw && encoding === "gzip") {
          return fetchTile(ref, level, descriptor, tileShape, generation, component, "raw");
        }
        throw error;
      }
    } finally {
      state.aborters.delete(controller);
    }
  }

  function cameraFrustum() {
    const value = adapter();
    try { return value?.camera?.frustum?.() || null; }
    catch (_) { return null; }
  }

  function levelForBudget(manifest, budget) {
    let selected = 0;
    for (const level of manifest.levels || []) {
      if (Number(level.voxel_count) <= budget) selected = Number(level.level);
    }
    return selected;
  }

  function qualityBudget() {
    if (state.quality === "responsive") return state.active ? 40_000 : 180_000;
    if (state.quality === "exact") return state.active ? 70_000 : Number.POSITIVE_INFINITY;
    return state.active ? 70_000 : 350_000;
  }

  function targetLevel(manifest) {
    if (!state.active && state.quality !== "responsive") return Number(manifest.exact_level);
    return levelForBudget(manifest, qualityBudget());
  }

  function concurrency() {
    if (state.active) return 2;
    if (state.quality === "responsive") return 3;
    return state.quality === "exact" ? 6 : 5;
  }

  function normaliseRange(value, signed = false) {
    const source = Array.isArray(value) && value.length === 2 ? value : [0, 1];
    let lo = Number(source[0]);
    let hi = Number(source[1]);
    if (!Number.isFinite(lo) || !Number.isFinite(hi)) [lo, hi] = signed ? [-1, 1] : [0, 1];
    if (lo > hi) [lo, hi] = [hi, lo];
    if (lo === hi) {
      const pad = Math.max(Math.abs(lo) * 1e-6, 1e-9);
      lo -= pad;
      hi += pad;
    }
    if (signed) {
      const extent = Math.max(Math.abs(lo), Math.abs(hi), 1e-12);
      return [-extent, extent];
    }
    return [lo, hi];
  }

  function rangeIsSymmetric(value) {
    if (!Array.isArray(value) || value.length !== 2) return false;
    const lo = Number(value[0]);
    const hi = Number(value[1]);
    if (!Number.isFinite(lo) || !Number.isFinite(hi) || !(lo < 0 && hi > 0)) return false;
    return Math.abs(lo + hi) <= Math.max(Math.abs(lo), Math.abs(hi), 1) * 1e-9;
  }

  function resolveDisplayRange(previousRange, nextRange, {
    sameField = false, signed = false, policy = "expand",
  } = {}) {
    const next = normaliseRange(nextRange, signed);
    if (!sameField || !previousRange || policy === "per_revision") return next;
    const previous = normaliseRange(previousRange, signed);
    if (policy === "locked") return previous;
    return normaliseRange([
      Math.min(previous[0], next[0]),
      Math.max(previous[1], next[1]),
    ], signed);
  }

  function displayRange(ref, manifest, previous, key) {
    const candidate = Array.isArray(ref?.visual_range) && ref.visual_range.length === 2
      ? ref.visual_range : manifest.visual_range;
    const signed = Boolean(ref?.signed || manifest?.symmetric_range || rangeIsSymmetric(candidate));
    const policy = String(ref?.rangePolicy || ref?.range_policy || "expand");
    const sameField = Boolean(previous && previous.streamKey === key);
    const resolved = resolveDisplayRange(previous?.displayRange, candidate, { sameField, signed, policy });
    const normal = normaliseRange(candidate, signed);
    if (sameField && policy === "expand" && (resolved[0] !== normal[0] || resolved[1] !== normal[1])) {
      state.metrics.rangeExpansions += 1;
    }
    return { range: resolved, signed, policy };
  }

  function textureRevision(ref, rawDigest) {
    return `${String(rawDigest || ref.field_id)}|view=${String(ref.viewId || "value")}`;
  }

  async function mapConcurrent(items, limit, operation, generation) {
    let next = 0;
    async function lane() {
      while (generation === state.generation) {
        const index = next++;
        if (index >= items.length) return;
        await operation(items[index], index);
      }
    }
    await Promise.all(Array.from({ length: Math.max(1, Math.min(limit, items.length || 1)) }, lane));
  }

  function tileMap(descriptors) {
    const out = new Map();
    for (const item of descriptors) out.set(item.index.join(","), item);
    return out;
  }

  async function loadLevel(ref, manifest, levelNumber, generation, { permitDelta = true } = {}) {
    if (generation !== state.generation) return false;
    const view = adapter();
    if (!view) throw new Error("the WebGL viewport is not ready for registered field tiles");
    const visibility = await ensureVisibility();
    const level = manifest.levels.find(item => Number(item.level) === Number(levelNumber));
    if (!level?.registration) throw new Error("field level has no exact registration");
    const tileShape = (manifest.tile_shape || [32, 32, 32]).map(Number);
    const key = streamKey(ref);
    const previous = state.current;
    const display = displayRange(ref, manifest, previous, key);
    const range = display.range;
    let descriptors = visibility.descriptors(level, tileShape, cameraFrustum(), state.focus);
    let incremental = false;
    let deltaFallback = false;

    if (permitDelta && previous && previous.streamKey === key
        && previous.level === levelNumber && previous.textureName) {
      const controller = new AbortController();
      state.aborters.add(controller);
      try {
        const query = new URLSearchParams({ from: previous.fieldId, level: String(levelNumber), tile_shape: tileShape.join(",") });
        const delta = await fetchJSON(`${ref.delta}?${query}`, controller.signal);
        if (canCommitDeltaAtomically(delta) && typeof view.applyTextureRegionsAtomic === "function") {
          incremental = true;
          const all = tileMap(descriptors);
          descriptors = (delta.changed || []).map(change => ({
            ...(all.get(change.index.join(",")) || change), ...change,
            priority: all.get(change.index.join(","))?.priority || 0,
          })).sort((a, b) => a.priority - b.priority);
        } else if (delta.compatible) {


          deltaFallback = true;
          state.metrics.fullReloads += 1;
        }
      } catch (error) {
        if (error?.name === "AbortError") throw error;

        state.metrics.deltaFallbacks += 1;
        incremental = false;
      } finally {
        state.aborters.delete(controller);
      }
    }

    const textureName = incremental
      ? previous.textureName
      : (previous?.textureName === state.stagingName ? state.alternateName : state.stagingName);
    const allocated = !incremental;
    if (allocated) {
      view.allocateTexture(textureName, {
        shape: level.spatial_shape,
        registration: level.registration,
        range,
        revision: textureRevision(ref, ref.field_id),
      });
    }

    try {
      const total = descriptors.length;
      let completed = 0;
      const atomicUpdates = [];
      const fieldLabel = ref.fieldLabel || human(ref.fieldName);
      const label = ref.viewLabel ? `${fieldLabel} · ${ref.viewLabel}` : fieldLabel;
      setStatus(`${label} · LOD ${levelNumber} · ${total} tile${total === 1 ? "" : "s"}`);
      await mapConcurrent(descriptors, concurrency(), async descriptor => {
        if (generation !== state.generation) throw new DOMException("stale field generation", "AbortError");
        const decoded = await fetchTile(ref, levelNumber, descriptor, tileShape, generation, ref.component);
        if (generation !== state.generation) return;
        const update = {
          data: decoded.values,
          offset: decoded.header.offset,
          shape: decoded.header.spatial_shape || decoded.header.shape.slice(0, 3),
          revision: textureRevision(ref, decoded.header.raw_sha256),
          range,
          priority: descriptor.priority,
          budgetMs: state.active ? 1.5 : 4.0,
        };
        if (incremental) atomicUpdates.push(update);
        else view.queueTextureRegion(textureName, update);
        completed += 1;
        if (!incremental) state.metrics.uploaded += 1;
        if (completed === total || completed % Math.max(1, Math.ceil(total / 8)) === 0) {
          setStatus(`${label} · LOD ${levelNumber} · ${completed}/${total}`);
        }
      }, generation);

      if (generation !== state.generation) throw new DOMException("stale field generation", "AbortError");
      if (incremental && atomicUpdates.length) {
        atomicUpdates.sort((a, b) => (a.priority || 0) - (b.priority || 0));
        const applied = Number(view.applyTextureRegionsAtomic(textureName, atomicUpdates) || 0);
        if (applied !== atomicUpdates.length) {
          throw new Error(`atomic field commit applied ${applied}/${atomicUpdates.length} tiles`);
        }
        state.metrics.uploaded += applied;
        state.metrics.atomicDeltas += 1;
      }


      if (allocated && typeof view.drainTextureUpdates === "function") {
        state.metrics.stagedDrains += 1;
        await view.drainTextureUpdates(state.active ? 1.5 : 4.0);
      } else if (allocated) {


        view.flushTextureUpdates?.(Number.POSITIVE_INFINITY);
      }
      if (generation !== state.generation) {
        throw new DOMException("stale field generation", "AbortError");
      }
      const palette=display.signed ? "diverging" : "sequential";
      view.setColorBy(textureName, range, {signed:display.signed,palette});


      const truthStatus=truthPresentation(ref,manifest);
      state.current = {
        fieldId: ref.field_id, fieldName: ref.fieldName, textureName,
        level: levelNumber, manifest, ref, streamKey: key,
        fieldLabel:ref.fieldLabel||human(ref.fieldName),viewId: ref.viewId, viewLabel: ref.viewLabel, component: ref.component,
        unit:ref.unit||ref.units||manifest.unit||manifest.units||"",
        displayRange: range.slice(), rangePolicy: display.policy, signed: display.signed,palette,
        truthStatus,renderExact:Boolean(level.exact),
      };
      updateLegend(state.current);
      window.dispatchEvent(new CustomEvent("implexity:field-stream-level", { detail: {
        field_id: ref.field_id, field_name: ref.fieldName, view_id: ref.viewId,
        level: levelNumber, exact: Boolean(level.exact), changed_tiles: total,
        render_exact:Boolean(level.exact),truth_status:truthStatus.code,
        incremental, delta_fallback: deltaFallback,
      }}));
      setStatus(level.exact ? `${label} · exact` : `${label} · LOD ${levelNumber}`, level.exact ? "exact" : "working");
      return true;
    } catch (error) {
      if (allocated) {
        state.metrics.clearedUploads += Number(view.clearTextureUpdates?.(textureName) || 0);
        view.deleteTexture?.(textureName);
      }
      throw error;
    }
  }

  async function present(rawRef, options = {}) {
    if (!rawRef || typeof rawRef !== "object" || !rawRef.field_id || !rawRef.manifest) return false;
    const generation = ++state.generation;
    abortGeneration();
    const named = {
      ...rawRef,
      fieldName: String(options.fieldName || rawRef.field_name || rawRef.response_id || "field"),
      rangePolicy: String(options.rangePolicy || rawRef.range_policy || rawRef.rangePolicy || "expand"),
    };
    const displayedField = fieldPresentation(named);
    const ref = rawRef.viewId ? {
      ...named,
      fieldLabel: displayedField.label,
      fieldDescription: displayedField.description,
      viewLabel: viewPresentation({
        id: rawRef.viewId,
        label: rawRef.viewLabel,
        description: rawRef.viewDescription,
      }).label,
    } : withSelectedView(named);
    const label = ref.viewLabel ? `${ref.fieldLabel} · ${ref.viewLabel}` : ref.fieldLabel;
    setStatus(`Opening ${label}…`);
    try {
      const controller = new AbortController();
      state.aborters.add(controller);
      let manifest;
      try { manifest = await fetchJSON(ref.manifest, controller.signal); }
      finally { state.aborters.delete(controller); }
      if (generation !== state.generation) return false;
      const target = targetLevel(manifest);


      const sameStream = state.current && state.current.streamKey === streamKey(ref);
      const initial = sameStream
        ? Math.min(Number(state.current.level), target)
        : levelForBudget(manifest, qualityBudget());
      await loadLevel(ref, manifest, initial, generation);
      if (generation !== state.generation || state.active) return true;
      for (let level = initial + 1; level <= target; level += 1) {
        if (generation !== state.generation || state.active) break;
        await loadLevel(ref, manifest, level, generation, { permitDelta: false });
      }
      return true;
    } catch (error) {
      if (error?.name !== "AbortError") {
        state.metrics.failed += 1;
        setStatus(
          "The selected result field could not be displayed. Try a lower quality or reload the result.",
          "error",
          error,
        );
      }
      return false;
    }
  }

  function availableStreams(payload) {
    const groups = [payload?.field_streams, payload?.sensitivity_streams]
      .filter(value => value && typeof value === "object");
    return groups.flatMap(streams =>
      Object.entries(streams).map(([fieldName, ref]) => ({ ...ref, fieldName }))
    );
  }

  async function fetchExactArray(url,signal,ref) {
    if(!/^[<>=|]?[iu]8$/.test(ref.dtype||''))return fetchJSON(url,signal);
    const response=await fetch(url.replace('encoding=json','encoding=raw'),{signal,cache:'force-cache'});
    if(!response.ok)throw new Error('Exact integer array could not be loaded');
    const header=JSON.parse(response.headers.get('X-Implexity-Array')||'null');
    const buffer=await response.arrayBuffer();
    if(!header || header.dtype!==ref.dtype || !Number.isSafeInteger(header.count) || header.count<1 || header.count>128 || buffer.byteLength!==header.count*8)throw new Error('Invalid exact integer array');
    const little=ref.dtype[0]==='<' || (ref.dtype[0]!=='>' && new Uint8Array(new Uint16Array([1]).buffer)[0]===1);
    const view=new DataView(buffer),unsigned=ref.dtype.includes('u');
    return {...header,values:Array.from({length:header.count},(_,index)=>unsigned?view.getBigUint64(index*8,little):view.getBigInt64(index*8,little))};
  }
  let arrayRequest=0, arrayAbort=null;
  function updateArrayInspector(payload) {
    if(!payload || !Object.prototype.hasOwnProperty.call(payload,'field_arrays'))return false;
    ++arrayRequest;arrayAbort?.abort();
    const entries=Object.entries(payload.field_arrays||{}).filter(([name,ref])=>
      ref?.schema==='implexity-result-array-ref/1' && /^result-[0-9a-f]{32}$/.test(ref.artifact_id||'') &&
      ref.field_name===name && Array.isArray(ref.shape) && ref.shape.every(n=>Number.isSafeInteger(n)&&n>0) &&
      Number.isSafeInteger(ref.shape.reduce((a,b)=>a*b,1)));
    const panel=controls();let box=panel.querySelector('[data-role="exact-arrays"]');
    if(!box){
      box=document.createElement('details');box.dataset.role='exact-arrays';box.className='implexity-exact-arrays';
      box.innerHTML='<summary>Exact result data</summary><label>Array<select aria-label="Exact result array"></select></label><p data-role="description"></p><label>Start element (zero-based)<input data-role="array-offset" type="number" min="0" step="1" value="0"></label><div><button type="button" data-action="previous">Previous values</button> <button type="button" data-action="next">Next values</button></div><pre role="status" aria-live="polite"></pre>';
      panel.appendChild(box);
    }
    box.hidden=!entries.length;if(!entries.length)return false;
    panel.hidden=false;
    const select=box.querySelector('select'),description=box.querySelector('[data-role="description"]'),output=box.querySelector('pre');
    const previous=box.querySelector('[data-action="previous"]'),next=box.querySelector('[data-action="next"]');
    const offsetInput=box.querySelector('[data-role="array-offset"]');
    offsetInput.value='0';offsetInput.setCustomValidity('');
    const previousSelection=select.value;
    select.replaceChildren();
    for(const [name,ref] of entries){const option=document.createElement('option');option.value=name;option.textContent=human(name,ref.metadata);select.appendChild(option);}
    if(entries.some(([name])=>name===previousSelection))select.value=previousSelection;
    description.textContent='';previous.disabled=next.disabled=true;
    let offset=0;
    async function load(){
      const ref=entries.find(([name])=>name===select.value)?.[1];if(!ref)return;
      const total=ref.shape.reduce((a,b)=>a*b,1),count=Math.min(128,total-offset),ticket=++arrayRequest;
      offsetInput.max=String(total-1);offsetInput.value=String(offset);offsetInput.setCustomValidity('');
      arrayAbort?.abort();arrayAbort=new AbortController();previous.disabled=next.disabled=true;
      const meta=ref.metadata||{};
      const axes=Array.isArray(meta.axes)?meta.axes:[];
      const components=Array.isArray(meta.state_metadata)?meta.state_metadata.map((item,index)=>`${index}: ${human(item.name||'State')} [${item.units||'unit not declared'}]`):Array.isArray(meta.components)?meta.components.map((name,index)=>`${index}: ${human(name)}`):[];
      description.textContent=`${human(meta.association||'Array')} · ${ref.shape.join(' × ')||'scalar'} · ${meta.units||'unit not declared'}${axes.length ? ' · '+axes.map(name=>human(name)).join(' / ') : ''}. Exact stored values; not a spatial rendering. Artifact ${ref.artifact_id}.${components.length?' Components: '+components.join('; '):''}`;
      output.textContent='Loading values…';
      try{
        const url=`/v1/implicit/result-artifact/${ref.artifact_id}/array?field=${encodeURIComponent(ref.field_name)}&offset=${offset}&count=${count}&encoding=json`;
        const data=await fetchExactArray(url,arrayAbort.signal,ref);
        if(ticket!==arrayRequest)return;
        const complex=data.value_encoding==='complex_real_imaginary_pairs';
        const validValue=value=>complex?Array.isArray(value)&&value.length===2&&value.every(Number.isFinite):typeof value==='bigint'||Number.isFinite(value);
        if(data.artifact_id!==ref.artifact_id || data.field!==ref.field_name || data.offset!==offset || data.count!==count ||
           JSON.stringify(data.shape)!==JSON.stringify(ref.shape) || !Array.isArray(data.values) || data.values.length!==count || !data.values.every(validValue))throw new Error('Array response does not match selection');
        output.textContent=`Values ${offset+1}–${offset+count} of ${total} (zero-based indices, C order)\n`+data.values.map((value,index)=>{
          let flat=offset+index;const indices=ref.shape.map(()=>0);
          for(let axis=indices.length-1;axis>=0;axis--){indices[axis]=flat%ref.shape[axis];flat=Math.floor(flat/ref.shape[axis]);}
          const displayed=complex?`${value[0]} ${value[1]<0?'-':'+'} ${Math.abs(value[1])}i`:value;
          return `[${indices.join(', ')}]  ${displayed}`;
        }).join('\n');
        previous.disabled=offset===0;next.disabled=offset+count>=total;
      }catch(error){if(ticket===arrayRequest && error?.name!=='AbortError')output.textContent='Exact values could not be loaded. Select the array again to retry.';}
    }
    select.onchange=()=>{offset=0;load();};
    offsetInput.onchange=()=>{
      const ref=entries.find(([name])=>name===select.value)?.[1];
      const value=Number(offsetInput.value),total=ref?.shape.reduce((a,b)=>a*b,1)||0;
      if(!offsetInput.value.trim() || !Number.isSafeInteger(value) || value<0 || value>=total){
        offsetInput.setCustomValidity(`Enter an integer from 0 to ${Math.max(0,total-1)}.`);offsetInput.reportValidity();return;
      }
      offset=value;load();
    };
    previous.onclick=()=>{offset=Math.max(0,offset-128);load();};
    next.onclick=()=>{offset+=128;load();};
    box.ontoggle=()=>{if(box.open)load();else{++arrayRequest;arrayAbort?.abort();}};
    output.textContent='Open this section to inspect stored values.';
    if(box.open)load();
    return true;
  }
  function ingestResponse(payload) {
    const streams = availableStreams(payload);
    const hasArrays=updateArrayInspector(streams.length && !Object.prototype.hasOwnProperty.call(payload,'field_arrays') ? {...payload,field_arrays:{}} : payload);
    if(hasArrays || streams.length){
      const panel=controls();
      for(const role of ['field','view','quality']){const input=panel.querySelector(`[data-role="${role}"]`);if(input?.parentElement)input.parentElement.hidden=!streams.length;}
      const hide=panel.querySelector('[data-action="hide"]');if(hide)hide.hidden=!streams.length;
      const help=panel.querySelector('#implexity-field-view-help');if(help)help.hidden=!streams.length;
    }
    if (!streams.length) {
      if(hasArrays){
        ++state.generation;abortGeneration();state.available=[];state.current=null;
        adapter()?.setColorBy?.(null);updateLegend(null);setStatus('');
      }
      return hasArrays;
    }
    window.dispatchEvent(new CustomEvent("implexity:field-streams-available", { detail: { streams, response: payload } }));
    const selected = updateControls(streams);
    const preferredRaw = streams.find(item => item.fieldName === payload?.display_field);
    const preferred = preferredRaw ? withSelectedView(preferredRaw) : selected;
    if (preferred) queueMicrotask(() => present(preferred, { fieldName: preferred.fieldName }));
    return true;
  }

  function installResponseHook() {
    if (Response.prototype.__implexityFieldStreams) return;
    const original = Response.prototype.json;
    Object.defineProperty(Response.prototype, "__implexityFieldStreams", { value: true });
    Response.prototype.json = async function implexityFieldJSON() {
      const data = await original.call(this);
      try {
        if (!state.intercepted.has(this) && /\/v1\/implicit\/(results|sensitivity|derivatives|optimi[sz])/.test(this.url || "")) {
          state.intercepted.add(this);
          ingestResponse(data);
        }
      } catch (_) {                                                      }
      return data;
    };
  }

  function setInteraction(active) {
    const value = Boolean(active);
    if (state.active === value) return;
    state.active = value;
    clearTimeout(state.idleTimer);
    if (value) {
      ++state.generation;
      abortGeneration();
      window.dispatchEvent(new CustomEvent("implexity:visual-quality", { detail: { mode: "interactive" } }));
    } else {
      state.idleTimer = window.setTimeout(() => {
        const current = state.current;
        const target = current?.manifest ? targetLevel(current.manifest) : -1;
        if (current?.ref && current.level < target) {
          present(current.ref, { fieldName: current.fieldName });
        }
        window.dispatchEvent(new CustomEvent("implexity:visual-quality", { detail: { mode: "refine" } }));
      }, IDLE_MS);
    }
  }

  function installHooks() {
    const target = viewportElement();
    target.addEventListener("pointerdown", () => setInteraction(true), { capture: true, passive: true });
    window.addEventListener("pointerup", () => setInteraction(false), { capture: true, passive: true });
    window.addEventListener("pointercancel", () => setInteraction(false), { capture: true, passive: true });
    window.addEventListener("implexity:interaction-begin", () => setInteraction(true));
    window.addEventListener("implexity:interaction-end", () => setInteraction(false));
    window.addEventListener("implexity:field-stream", event => present(event.detail?.ref || event.detail, event.detail || {}));
    window.addEventListener("implexity:field-stream-response", event => ingestResponse(event.detail));
    window.addEventListener("implexity:field-focus", event => { state.focus = event.detail?.point || null; });
  }

   
   
   
  async function presentArray(spec = {}) {
    const view = adapter();
    if (!view) throw new Error("the WebGL viewport is not ready for field display");
    const shape = Array.isArray(spec.shape) ? spec.shape.map(Number) : [];
    if (shape.length !== 3 || shape.some(n => !Number.isSafeInteger(n) || n < 1))
      throw new Error("only three-dimensional cell fields can be shown in the viewport");
    const registration = spec.registration;
    if (!registration || !Array.isArray(registration.basis) || !Array.isArray(registration.origin) ||
        !Array.isArray(registration.shape) || registration.shape.join(",") !== shape.join(","))
      throw new Error("the field has no spatial registration matching its shape");
    const values = spec.values instanceof Float32Array ? spec.values : Float32Array.from(spec.values || []);
    if (values.length !== shape[0] * shape[1] * shape[2]) throw new Error("field value count does not match its shape");
    let lo = Infinity, hi = -Infinity;
    for (const value of values) if (Number.isFinite(value)) { if (value < lo) lo = value; if (value > hi) hi = value; }
    if (!Number.isFinite(lo)) throw new Error("the field has no finite values");
    const signed = lo < 0 && hi > 0;
    const range = normaliseRange([lo, hi], signed);
    const generation = ++state.generation;
    abortGeneration();
    const textureName = state.current?.textureName === state.stagingName ? state.alternateName : state.stagingName;
    const revision = String(spec.revision || `${spec.fieldName}:${Date.now()}`);
    view.allocateTexture(textureName, { shape, registration, range, revision, reset: true });
    view.queueTextureRegion(textureName, { data: values, offset: [0, 0, 0], shape, revision, range, priority: 0, budgetMs: 4.0 });
    if (typeof view.drainTextureUpdates === "function") await view.drainTextureUpdates(4.0);
    else view.flushTextureUpdates?.(Number.POSITIVE_INFINITY);
    if (generation !== state.generation) return false;
    const palette = signed ? "diverging" : "sequential";
    view.setColorBy(textureName, range, { signed, palette });
    const fieldLabel = String(spec.label || human(spec.fieldName));
    state.current = {
      fieldId: revision, fieldName: String(spec.fieldName || ""), textureName, level: 0, manifest: null,
      ref: null, streamKey: `array:${revision}`, fieldLabel, viewId: null, viewLabel: spec.viewLabel || "",
      component: null, unit: String(spec.unit || ""), displayRange: range.slice(), rangePolicy: "per_revision",
      signed, palette, truthStatus: truthPresentation({ truth_status: spec.truthStatus || "recorded" }),
      renderExact: true,
    };
    updateLegend(state.current);
    setStatus(`${fieldLabel} · exact`, "exact");
    window.dispatchEvent(new CustomEvent("implexity:field-stream-level", { detail: {
      field_id: revision, field_name: state.current.fieldName, view_id: null, level: 0, exact: true,
      changed_tiles: 1, render_exact: true, truth_status: state.current.truthStatus.code,
      incremental: false, delta_fallback: false,
    }}));
    return true;
  }

  const fieldStreamApi = Object.freeze({
    version: VERSION,
    present,
    presentArray,
    ingestResponse,
    setInteractionActive: setInteraction,
    normaliseRange,
    resolveDisplayRange,
    state: () => ({ generation: state.generation, active: state.active, current: state.current, metrics: { ...state.metrics } }),
  });
  window.ImplexityFieldStream=fieldStreamApi;

  function start() {
    try {
      const saved = localStorage.getItem(QUALITY_KEY);
      if (["auto", "responsive", "exact"].includes(saved)) state.quality = saved;
    } catch (_) {                                                     }
    installResponseHook();
    installHooks();
    window.dispatchEvent(new CustomEvent("implexity:field-stream-ready", { detail: { version: VERSION } }));
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start, { once: true });
  else start();
})();
