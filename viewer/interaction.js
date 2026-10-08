// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(function (global) {
  "use strict";

  const MODES = Object.freeze({
    SELECT: "select",
    SELECT_BRUSH: "selection_brush",
    SELECT_BOX: "selection_box",
    SELECT_LASSO: "selection_lasso",
    SELECT_FLOOD: "selection_flood",
    MOVE: "move",
    SIZE: "size",
    SURFACE: "surface",
    REGION: "surface_patch",
    PRESSURE: "pressure",
    TRACTION: "traction",
    HEAT: "heat_flux",
    TEMPERATURE: "temperature",
    CLAMP: "clamp",
    SCULPT: "geometry_sculpt",
    BRUSH: "field_brush",
    CONTROL: "control_lattice",
    CAGE: "deformation_cage"
  });

  const MODE_KEYS = Object.freeze({
    q: MODES.SELECT,
    v: MODES.SELECT_BRUSH,
    x: MODES.SELECT_BOX,
    l: MODES.SELECT_LASSO,
    f: MODES.SELECT_FLOOD,
    w: MODES.MOVE,
    e: MODES.SIZE,
    r: MODES.SURFACE,
    g: MODES.REGION,
    p: MODES.PRESSURE,
    t: MODES.TRACTION,
    h: MODES.HEAT,
    k: MODES.TEMPERATURE,
    c: MODES.CLAMP,
    b: MODES.BRUSH,
    s: MODES.SCULPT
  });

  const TOOL_DEFS = [
    [MODES.SELECT, "Select", "Q", "↖"],
    [MODES.SELECT_BRUSH, "Selection brush", "V", "◉"],
    [MODES.SELECT_BOX, "Box selection", "X", "□"],
    [MODES.SELECT_LASSO, "Lasso selection", "L", "⌁"],
    [MODES.SELECT_FLOOD, "Connected selection", "F", "⬡"],
    [MODES.MOVE, "Move geometry", "W", "↔"],
    [MODES.SIZE, "Change size or thickness", "E", "◫"],
    [MODES.SURFACE, "Differentiable surface drag", "R", "∇"],
    [MODES.REGION, "Surface region", "G", "◎"],
    [MODES.PRESSURE, "Pressure", "P", "⇥"],
    [MODES.TRACTION, "Traction", "T", "→"],
    [MODES.HEAT, "Heat flux", "H", "♨"],
    [MODES.TEMPERATURE, "Prescribed temperature", "K", "K"],
    [MODES.CLAMP, "Clamp", "C", "⊥"],
    [MODES.SCULPT, "Shape and sculpt", "S", "≈"],
    [MODES.BRUSH, "Field brush", "B", "●"],
    [MODES.CONTROL, "Control points", "", "⌘"],
    [MODES.CAGE, "Deformation cage", "", "◇"]
  ];

  const TOOL_BY_MODE = new Map(TOOL_DEFS.map(def => [def[0], def]));
  const SELECTION_MODES = new Set([
    MODES.SELECT, MODES.SELECT_BRUSH, MODES.SELECT_BOX,
    MODES.SELECT_LASSO, MODES.SELECT_FLOOD
  ]);
  const SELECTION_KIND = Object.freeze({
    [MODES.SELECT]: "click",
    [MODES.SELECT_BRUSH]: "brush",
    [MODES.SELECT_BOX]: "box",
    [MODES.SELECT_LASSO]: "lasso",
    [MODES.SELECT_FLOOD]: "flood"
  });
  const TOOL_HELP = Object.freeze({
    [MODES.SCULPT]: "Drag to shape the existing implicit design. Choose direction, influence depth and protected regions. Release commits one edit; Escape cancels. Physics must be preflighted again.",
    [MODES.SELECT]: "Select a surface cell in an editable field. For a parametric model, use Surface region after configuring its physics problem.",
    [MODES.SELECT_BRUSH]: "Paint exact surface cells. Radius is measured in model millimetres.",
    [MODES.SELECT_BOX]: "Drag a rectangle. Visible-front and through selection use the exact registered field.",
    [MODES.SELECT_LASSO]: "Draw a freehand outline around cells; release to apply it.",
    [MODES.SELECT_FLOOD]: "Click one cell to select its six-connected visible surface component.",
    [MODES.MOVE]: "Drag the implicit surface to move the strongest authored parameter.",
    [MODES.SIZE]: "Drag the implicit surface to change a size or thickness parameter.",
    [MODES.SURFACE]: "Drag the surface; release to save one model edit.",
    [MODES.REGION]: "Click or drag over the surface; release to save the region. A region-capable physics problem is required; assigning a load is a separate step.",
    [MODES.PRESSURE]: "Set pressure in Pa, then place it on the surface. Review its area and direction before analysis.",
    [MODES.TRACTION]: "Set surface traction in Pa, then place it on the surface and review its direction.",
    [MODES.HEAT]: "Set heat flux in W/m², then select the heated surface. Review the provider's sign convention before analysis.",
    [MODES.TEMPERATURE]: "Set absolute temperature in K, then select the surface to hold at that temperature.",
    [MODES.CLAMP]: "Select the supported surface. Review the constrained displacement components in the condition setup."
  });
  const SVG_NS = "http://www.w3.org/2000/svg";

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

  function displayIdentifier(identifier, metadata = {}) {
    const tool = TOOL_BY_MODE.get(identifier);
    const fallback = String(metadata.label || metadata.display_name || metadata.title ||
      (tool && tool[1]) || localLabel(identifier) || "Engineering object");
    const descriptor = global.ImplexityText?.present?.(identifier, Object.assign({}, metadata, {
      label: metadata.label || metadata.display_name || metadata.title || (tool && tool[1]) || undefined
    }));
    return localLabel(descriptor?.label || fallback);
  }

  function modeLabel(mode) {
    return displayIdentifier(mode, { label: TOOL_BY_MODE.get(mode)?.[1] });
  }

  function isEditableTarget(target) {
    if (!target) return false;
    if (target.isContentEditable) return true;
    if (/^(input|textarea|select)$/i.test(String(target.tagName || ""))) return true;
    return typeof target.closest === "function" &&
      Boolean(target.closest('input,textarea,select,[contenteditable="true"],[contenteditable=""],[role="textbox"]'));
  }

  const OPTION_PROFILES = Object.freeze({
    [MODES.SELECT_BRUSH]: {
      title: "Selection brush settings", radius: ["Selection radius", "mm"]
    },
    [MODES.REGION]: {
      title: "Surface region settings",
      radius: ["Patch radius", "mm"], falloff: ["Edge falloff", ""]
    },
    [MODES.PRESSURE]: {
      title: "Pressure settings",
      radius: ["Patch radius", "mm"], magnitude: ["Pressure", "Pa"],
      falloff: ["Edge falloff", ""], snap: ["Movement snap", "mm"]
    },
    [MODES.TRACTION]: {
      title: "Traction settings",
      radius: ["Patch radius", "mm"], magnitude: ["Traction", "Pa"],
      falloff: ["Edge falloff", ""], snap: ["Movement snap", "mm"]
    },
    [MODES.HEAT]: {
      title: "Heat flux settings",
      radius: ["Patch radius", "mm"], magnitude: ["Heat flux", "W/m²"],
      falloff: ["Edge falloff", ""], snap: ["Movement snap", "mm"]
    },
    [MODES.TEMPERATURE]: {
      title: "Temperature settings",
      radius: ["Patch radius", "mm"], magnitude: ["Temperature", "K"],
      falloff: ["Edge falloff", ""], snap: ["Movement snap", "mm"]
    },
    [MODES.CLAMP]: {
      title: "Clamp settings",
      radius: ["Patch radius", "mm"], falloff: ["Edge falloff", ""],
      snap: ["Movement snap", "mm"]
    },
    [MODES.SCULPT]: {title:"Shape and sculpt",radius:["Influence radius","mm"]},
    [MODES.BRUSH]: {
      title: "Field brush settings",
      radius: ["Brush radius", "mm"], strength: ["Brush increment", "field units"],
      brushMode: ["Brush operation", ""], falloff: ["Brush falloff", ""]
    },
    [MODES.CONTROL]: {
      title: "Control point settings",
      falloff: ["Control falloff", ""], snap: ["Movement snap", "mm"]
    },
    [MODES.CAGE]: {
      title: "Deformation cage settings",
      falloff: ["Control falloff", ""], snap: ["Movement snap", "mm"]
    }
  });

  function optionProfile(mode) {
    const source = OPTION_PROFILES[mode];
    if (!source) return null;
    return Object.fromEntries(Object.entries(source).map(([key, value]) =>
      [key, Array.isArray(value) ? value.slice() : value]));
  }

  function friendlyErrorMessage(error, fallback = "The interaction could not be completed.") {
    const problem = error && Array.isArray(error.problems) && error.problems.length
      ? error.problems[0] : error?.userMessage || error?.message || error;
    const clean = String(problem == null ? "" : problem).replace(/^\s*Error:\s*/i, "").trim();
    if (!clean) return fallback;
    if (/\r|\n|https?:\/\/|\/v\d+(?:\/|\b)|\/(?:Users|home|private|tmp|var)\/|[A-Za-z]:\\|Traceback|\bat\s+\S+\s*\(|\b[A-Za-z][A-Za-z0-9]*_[A-Za-z0-9_]+\b|[{}\[\]<>]/i.test(clean)) {
      return fallback;
    }
    return clean.length <= 220 ? clean : fallback;
  }

  function createSvgElement(tag, className, attributes = {}, text) {
    const element = document.createElementNS(SVG_NS, tag);
    if (className) element.classList.add(className);
    for (const [name, value] of Object.entries(attributes)) {
      if (value !== undefined && value !== null) element.setAttribute(name, String(value));
    }
    if (text !== undefined) element.textContent = String(text);
    return element;
  }

  const DEFAULTS = Object.freeze({
    radiusMm: 3.0,
    strength: 0.08,
    magnitude: 1.0,
    falloff: "smoothstep",
    brushMode: "add",
    snap: 0.0,
    previewIntervalMs: 28,
    screenRadiusPx: 42,
    cageShape: [3, 3, 3]
  });

  function deepClone(value) {
    if (global.structuredClone) return global.structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  }

  function finiteNumber(value, fallback) {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : fallback;
  }

  function vec3(value, fallback = [0, 0, 0]) {
    if (!value) return fallback.slice();
    if (Array.isArray(value) && value.length >= 3) {
      const out = value.slice(0, 3).map(Number);
      return out.every(Number.isFinite) ? out : fallback.slice();
    }
    if (typeof value === "object") {
      const out = [Number(value.x), Number(value.y), Number(value.z)];
      return out.every(Number.isFinite) ? out : fallback.slice();
    }
    return fallback.slice();
  }

  function add3(a, b) { return [a[0] + b[0], a[1] + b[1], a[2] + b[2]]; }
  function sub3(a, b) { return [a[0] - b[0], a[1] - b[1], a[2] - b[2]]; }
  function scale3(a, s) { return [a[0] * s, a[1] * s, a[2] * s]; }
  function dot3(a, b) { return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]; }
  function len3(a) { return Math.sqrt(dot3(a, a)); }
  function unit3(a, fallback = [0, 0, 1]) {
    const length = len3(a);
    return length > 1e-12 ? scale3(a, 1 / length) : fallback.slice();
  }

  function decodeRuns(runs, shape) {
    const size=shape.reduce((total,value)=>total*Number(value),1),indices=[];
    for(const run of runs||[]){const start=Number(run[0]),count=Number(run[1]);for(let offset=0;offset<count;offset++){const flat=start+offset;if(flat>=0&&flat<size)indices.push(flat);}}
    return indices;
  }

  function registeredCellWorld(grid, shape, flat) {
    const ny=shape[1],nz=shape[2],i=Math.floor(flat/(ny*nz)),remainder=flat-i*ny*nz,j=Math.floor(remainder/nz),k=remainder-j*nz;
    const index=[i+.5,j+.5,k+.5],origin=grid.origin.map(Number),basis=grid.basis.map(row=>row.map(Number));
    return [0,1,2].map(component=>origin[component]+basis[0][component]*index[0]+basis[1][component]*index[1]+basis[2][component]*index[2]);
  }

  function createElement(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  }

  function dispatch(name, detail, target = global) {
    const event = new CustomEvent(name, { detail, bubbles: false, cancelable: true });
    target.dispatchEvent(event);
    return event;
  }

  const pointerCoordinator = global.ImplexityPointerCoordinator || (() => {
    let owner = null;
    let pointerId = null;
    return Object.freeze({
      claim(candidate, id) {
        if (owner !== null && owner !== candidate) return false;
        owner = candidate; pointerId = id;
        return true;
      },
      release(candidate, id = pointerId) {
        if (owner !== candidate || (pointerId !== null && id !== pointerId)) return false;
        owner = null; pointerId = null;
        return true;
      },
      owns(candidate, id = pointerId) {
        return owner === candidate && (pointerId === null || id === pointerId);
      },
      current() { return { owner, pointerId }; }
    });
  })();
  global.ImplexityPointerCoordinator = pointerCoordinator;

  const manualHistory = global.ImplexityManualHistory;
  if (!manualHistory) throw new Error("The shared manual history module was not loaded.");
  dispatch("implexity:manual-history-ready", {history: manualHistory});

  function validateSpatialField(field) {
    const value = deepClone(field);
    const shape = (value.shape || []).map(Number);
    const grid = value.grid || value.registration;
    if (shape.length !== 3 || shape.some(n => !Number.isInteger(n) || n < 2)) throw new Error("Spatial field shape must contain three integers of at least two");
    if (!grid || !["cell", "node"].includes(grid.centering) || !Array.isArray(grid.origin) || grid.origin.length !== 3 || !Array.isArray(grid.basis) || grid.basis.length !== 3) throw new Error("Spatial fields require an exact cell- or node-centred registration");
    if (grid.shape && grid.shape.some((n, i) => Number(n) !== shape[i])) throw new Error("Spatial field and registration shapes differ");
    if ((grid.frame || "model") !== "model" || [...(grid.axis_order || "xyz")].sort().join("") !== "xyz") throw new Error("Spatial field registration must use the model frame and xyz axes");
    const b = grid.basis.map(row => row.map(Number));
    const determinant = b[0][0]*(b[1][1]*b[2][2]-b[1][2]*b[2][1])-b[0][1]*(b[1][0]*b[2][2]-b[1][2]*b[2][0])+b[0][2]*(b[1][0]*b[2][1]-b[1][1]*b[2][0]);
    if (!Number.isFinite(determinant) || Math.abs(determinant) < 1e-15) throw new Error("Spatial field basis must be finite and invertible");
    const size = shape.reduce((a, n) => a*n, 1);
    value.values = Array.from(value.values || []);
    if (value.values.length !== size || value.values.some(v => !Number.isFinite(Number(v)))) throw new Error("Spatial field payload does not match its registered grid");
    for (const [name, raw] of Object.entries(value.protected_masks || {})) {
      const mask = Array.from((Array.isArray(raw) || ArrayBuffer.isView(raw)) ? raw : raw && raw.values || []);
      if (mask.length !== size) throw new Error(`Protected mask '${name}' does not match the spatial field`);
      value.protected_masks[name] = mask.map(Boolean);
    }
    value.shape = shape;
    value.grid = Object.assign({}, grid, { shape });
    return value;
  }

  const spatialFields = global.ImplexitySpatialFields || (() => {
    const fields = new Map();
    let active = null;
    let generation = 0;
    return Object.freeze({
      register(fieldId, field) {
        const id=String(fieldId), next=validateSpatialField(field), previous=fields.get(id);
        if (field.authoritative === true && (!next.identity || !next.identity.registration_id || !next.identity.payload_sha256)) {
          throw new Error("Authoritative spatial fields require registration and payload identities");
        }
        if(field.authoritative===true&&next.grid.registration_id&&String(next.identity.registration_id)!==String(next.grid.registration_id)){
          throw new Error("Authoritative spatial field registration identity does not match its grid");
        }
        fields.set(id,next);
        if(next.authoritative===true){for(const candidate of Array.from(fields.keys()))if(candidate!==id&&fields.get(candidate)?.authoritative===true)fields.delete(candidate);active=id;}
        else if(active===null)active=id;
        generation+=1;
        dispatch("implexity:spatial-field-registered",{fieldId:id,previousIdentity:previous?.identity||null,identity:next.identity||null,generation});
        return id;
      },
      remove(fieldId) { const id=String(fieldId); fields.delete(id); if(active===id)active=fields.keys().next().value || null; },
      replaceAuthoritative(response) {
        if (!response || !response.field_id) throw new Error("The authoritative field response is incomplete");
        const id=String(response.field_id);
        const field=Object.assign({},response,{authoritative:true});
        this.register(id,field);active=id;
        for(const candidate of Array.from(fields.keys()))if(candidate!==id&&fields.get(candidate)?.authoritative===true)fields.delete(candidate);
        return id;
      },
      setActive(fieldId) { const id=String(fieldId); if(!fields.has(id))throw new Error(`Unknown spatial field '${id}'`); active=id; },
      activeFieldId() { return active; },
      activeFieldPayload() { return active===null ? null : deepClone(fields.get(active)); },
      field(fieldId) { const value=fields.get(String(fieldId)); return value ? deepClone(value) : null; },
      ids() { return Array.from(fields.keys()); },
      generation() { return generation; }
    });
  })();
  global.ImplexitySpatialFields = spatialFields;

  const spatialSelections = global.ImplexitySpatialSelections || (() => {
    const selections = new Map();
    let active = null;
    function accept(value) {
      if (!value || value.kind !== "cell_selection" || !value.id || !Array.isArray(value.selected_runs)) throw new Error("Exact cell selection is malformed");
      if (!value.field_identity?.registration_id || !value.definition_id) throw new Error("Exact cell selection lacks identity evidence");
      const field=spatialFields.field(value.field_id);
      if(field?.authoritative===true&&(String(value.field_identity.registration_id)!==String(field.identity.registration_id)||String(value.field_identity.payload_sha256)!==String(field.identity.payload_sha256))){
        throw new Error("Exact cell selection belongs to a stale authoritative field");
      }
      return deepClone(value);
    }
    return Object.freeze({
      upsert(value){const selection=accept(value);selections.set(String(selection.id),selection);active=String(selection.id);dispatch("implexity:selection-updated",{selection:deepClone(selection)});return active;},
      hydrate(values,activeId=null){selections.clear();for(const value of values||[]){const selection=accept(value);selections.set(String(selection.id),selection);}active=activeId&&selections.has(String(activeId))?String(activeId):(selections.keys().next().value||null);dispatch("implexity:selections-hydrated",{count:selections.size,active});},
      active(){return active===null?null:deepClone(selections.get(active));},
      activeId(){return active;},
      setActive(id){id=String(id);if(!selections.has(id))throw new Error(`Unknown selection '${id}'`);active=id;dispatch("implexity:selection-updated",{selection:deepClone(selections.get(id))});},
      all(){return Array.from(selections.values(),deepClone);},
      clear(){selections.clear();active=null;dispatch("implexity:selections-hydrated",{count:0,active:null});}
    });
  })();
  global.ImplexitySpatialSelections=spatialSelections;

  class SerialPreviewQueue {
    constructor(execute) {
      this.execute = execute;
      this.sequence = 0;
      this.applied = -1;
      this.pending = null;
      this.running = false;
      this.abortController = null;
      this.completed = -1;
      this.failure = null;
      this.waiters = [];
    }

    submit(payload) {
      const explicit = Number(payload && payload.sequence);
      const sequence = Number.isInteger(explicit) && explicit >= 0 ? explicit : this.sequence + 1;
      if (sequence <= this.sequence) throw new Error("Preview sequences must increase monotonically");
      const request = { sequence, payload };
      this.sequence = sequence;
      this.pending = request;
      if (this.abortController) this.abortController.abort();
      this._run();
      return request.sequence;
    }

    async _run() {
      if (this.running) return;
      this.running = true;
      try {
        while (this.pending) {
          const request = this.pending;
          this.pending = null;
          const controller = new AbortController();
          this.abortController = controller;
          try {
            const response = await this.execute(request, controller.signal);
            const acknowledged=response?.sequence ?? response?.latest_applied;
            const sequences=[acknowledged,response?.latest_applied,response?.latest_requested].filter(value=>value!==undefined);
            if (!response || response.accepted !== true || !sequences.length || sequences.some(value=>!Number.isInteger(value)||value!==request.sequence)) {
              throw new Error(`Preview ${request.sequence} was not acknowledged exactly`);
            }
            if (request.sequence >= this.applied && request.sequence === this.sequence) {
              this.applied = request.sequence;
              dispatch("implexity:interaction-preview-applied", { request, response });
            }
          } catch (error) {
            if (error && error.name !== "AbortError") {
              this.failure = { sequence: request.sequence, error };
              dispatch("implexity:interaction-preview-error", { request, error });
            }
          } finally {
            this.completed = Math.max(this.completed, request.sequence);
            if (this.abortController === controller) this.abortController = null;
            this._settleWaiters();
          }
        }
      } finally {
        this.running = false;
        if (this.pending) this._run();
        else this._settleWaiters();
      }
    }

    drain(sequence = this.sequence) {
      const target = Number(sequence);
      return new Promise((resolve, reject) => {
        this.waiters.push({ target, resolve, reject });
        this._settleWaiters();
      });
    }

    async flush(payload) {
      const sequence = this.submit(payload);
      await this.drain(sequence);
      return sequence;
    }

    _settleWaiters() {
      const remaining = [];
      for (const waiter of this.waiters) {
        if (this.failure && this.failure.sequence >= waiter.target) waiter.reject(this.failure.error);
        else if (!this.running && !this.pending && this.completed >= waiter.target && this.applied >= waiter.target) waiter.resolve(waiter.target);
        else remaining.push(waiter);
      }
      this.waiters = remaining;
    }

    clear() {
      this.pending = null;
      if (this.abortController) this.abortController.abort();
      this.abortController = null;
      this._settleWaiters();
    }

    reset() {
      if (this.running || this.pending) throw new Error("Cannot reset an active preview queue");
      this.sequence = 0; this.applied = -1; this.completed = -1; this.failure = null; this.waiters = [];
    }
  }

  class InteractionBackend {
    constructor(controller) {
      this.controller = controller;
      this.endpoints = Object.assign({
        capabilities: "/v1/implicit/interactions",
        field: "/v1/implicit/interactions/field",
        begin: "/v1/implicit/interactions/begin",
        preview: "/v1/implicit/interactions/preview",
        refine: "/v1/implicit/interactions/refine",
        commit: "/v1/implicit/interactions/commit",
        cancel: "/v1/implicit/interactions/cancel",
        undo: "/v1/implicit/interactions/undo",
        redo: "/v1/implicit/interactions/redo",
        promoteCage: "/v1/implicit/interactions/cage/promote"
      }, global.IMPLEXITY_INTERACTION_ENDPOINTS || {});
      this.active = null;
    }

    async promoteCage(cageId) {
      const response = await fetch(this.endpoints.promoteCage,{method:"POST",credentials:"same-origin",
        headers:{"Content-Type":"application/json"},body:JSON.stringify({cage_id:cageId})});
      const data=await response.json();if(!response.ok)throw new Error(data.error||"Could not release the deformation cage for shape optimisation");
      return data;
    }

    async request(action, payload, signal) {
      const adapters = [
        global.ImplexityInteractionRuntime,
        global.ImplexityViewportAuthoring,
        action === "refine" ? global.ImplexityDirectInteraction : null,
        global.implexityInteraction
      ].filter(Boolean);
      const names = {
        begin: ["begin", "beginInteraction", "beginTransaction"],
        preview: ["preview", "previewInteraction", "previewTransaction"],
        refine: ["refine", "refineSurfaceHit", "refineSurface"],
        commit: ["commit", "commitInteraction", "commitTransaction"],
        cancel: ["cancel", "cancelInteraction", "cancelTransaction"]
      }[action] || [action];
      for (const adapter of adapters) {
        for (const name of names) {
          if (typeof adapter[name] === "function") {
            return await adapter[name](payload);
          }
        }
      }

      const bridged = await this._requestByEvent(action, payload);
      if (bridged.handled) return bridged.value;

      const endpoint = this.endpoints[action];
      if (!endpoint || typeof fetch !== "function") {
        throw new Error(`No interaction backend handles '${action}'`);
      }
      const response = await fetch(endpoint, {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
        signal
      });
      if (!response.ok) {
        const text = await response.text();
        const error=new Error(`${response.status} ${response.statusText}: ${text || endpoint}`);
        try {const detail=JSON.parse(text);if(Array.isArray(detail.problems))error.problems=detail.problems;}catch (_) {}
        throw error;
      }
      return await response.json();
    }

    _requestByEvent(action, payload) {
      return new Promise(resolve => {
        let settled = false;
        const finish = value => {
          if (!settled) {
            settled = true;
            resolve({ handled: true, value });
          }
        };
        const event = dispatch("implexity:interaction-request", {
          action,
          payload: deepClone(payload),
          respond: finish,
          reject: error => finish(Promise.reject(error))
        });
        if (!event.defaultPrevented) {
          queueMicrotask(() => {
            if (!settled) resolve({ handled: false, value: null });
          });
        }
      });
    }

    async begin(payload) {
      const response = await this.request("begin", payload);
      this.active = response;
      return response;
    }

    async field(payload = {}) { return await this.request("field", payload); }
    async undo(payload = {}) { return await this.request("undo", payload); }
    async redo(payload = {}) { return await this.request("redo", payload); }

    async preview(payload, signal) {
      const tx = this.active && (this.active.transaction_id || this.active.id);
      return await this.request("preview", Object.assign({ transaction_id: tx }, payload), signal);
    }

    async refine(payload, signal) {
      return await this.request("refine", payload, signal);
    }

    async commit(payload = {}) {
      const tx = this.active && (this.active.transaction_id || this.active.id);
      const result = await this.request("commit", Object.assign({ transaction_id: tx }, payload));
      this.active = null;
      return result;
    }

    async cancel(payload = {}) {
      if (!this.active) return null;
      const tx = this.active.transaction_id || this.active.id;
      try {
        return await this.request("cancel", Object.assign({ transaction_id: tx }, payload));
      } finally {
        this.active = null;
      }
    }
  }

  class PickingAdapter {
    constructor(controller) { this.controller = controller; }

    candidates() {
      return Array.from(new Set([
        global.ImplexityViewerAdapter,
        global.ImplexityViewer,
        global.implexityViewer,
        global.viewer,
        global.ImplexityViewport,
        global.ImplexityDirectInteraction
      ].filter(Boolean)));
    }

    coordinates(event) {
      const rect = this.controller.viewport.getBoundingClientRect();
      const canvas = this.controller.viewport.querySelector?.("canvas") || null;
      const css = [event.clientX - rect.left, event.clientY - rect.top];
      const scale = [
        canvas && canvas.width ? canvas.width / Math.max(1, rect.width) : (global.devicePixelRatio || 1),
        canvas && canvas.height ? canvas.height / Math.max(1, rect.height) : (global.devicePixelRatio || 1)
      ];
      return { css, device: [css[0] * scale[0], css[1] * scale[1]], scale, rect: { left: rect.left, top: rect.top, width: rect.width, height: rect.height } };
    }

    async pick(event, options = {}) {
      const coordinates = this.coordinates(event);
      const [x, y] = coordinates.css;
      const candidates = this.candidates();
      const methods = ["pickSurface", "surfaceHitAt", "raycast", "pick", "hitTest"];
      for (const candidate of candidates) {
        for (const method of methods) {
          if (typeof candidate[method] === "function") {
            const result = await candidate[method](x, y, { event, coordinates, coordinateSpace: "viewport-css-pixels" });
            if (result) {
              const hit = this.normalise(result, event, coordinates);
              return options.requireExact ? await this.refine(hit, event, candidate) : hit;
            }
          }
        }
      }

      return await new Promise(resolve => {
        let settled = false;
        const finish = hit => {
          if (!settled) {
            settled = true;
            resolve(hit ? this.normalise(hit, event, coordinates) : null);
          }
        };
        const custom = dispatch("implexity:surface-pick-request", {
          clientX: event.clientX,
          clientY: event.clientY,
          viewportX: x,
          viewportY: y,
          coordinates,
          event,
          resolve: finish
        });
        if (!custom.defaultPrevented) queueMicrotask(() => finish(null));
      }).then(hit => options.requireExact && hit ? this.refine(hit, event) : hit);
    }

    normalise(result, event, coordinates = this.coordinates(event)) {
      const point = vec3(result.point_mm || result.point || result.position || result.worldPoint);
      const normal = unit3(vec3(result.normal || result.surfaceNormal || result.worldNormal, [0, 0, 1]));
      const identity = Object.assign({}, result.model_identity || result.identity || {}, {
        structure_id: result.structure_id || result.structureId || result.model_identity?.structure_id || null,
        content_id: result.content_id || result.contentId || result.model_identity?.content_id || null,
        revision: result.revision || result.model_identity?.revision || null
      });
      const clipEvidence = result.clip_evidence || result.clip || result.evidence?.clip || null;
      const exact = result.exact === true && result.approximate !== true;
      const identityExact = Boolean(identity.structure_id || identity.content_id || identity.revision);
      const clipExact = Boolean(clipEvidence && typeof clipEvidence.active === "boolean" && clipEvidence.hit_on_clip_cap !== true);
      return {
        point_mm: point,
        normal,
        node_id: result.node_id || result.nodeId || result.object_id || result.objectId || null,
        selector: result.selector || result.graph_path || result.path || null,
        structure_id: identity.structure_id,
        content_id: identity.content_id,
        revision: identity.revision,
        client: [event.clientX, event.clientY],
        coordinates,
        clip_evidence: clipEvidence,
        exact,
        approximate: !exact,
        committable: exact && identityExact && clipExact,
        raw: result
      };
    }

    async refine(hit, event, preferred = null) {
      if (hit && hit.committable) return hit;
      const candidates = preferred ? [preferred, ...this.candidates().filter(item => item !== preferred)] : this.candidates();
      const payload = { hit: deepClone(hit), coordinates: this.coordinates(event) };
      for (const candidate of candidates) {
        for (const method of ["refineSurfaceHit", "refineSurface", "exactSurfaceHit"]) {
          if (typeof candidate[method] !== "function") continue;
          const result = await candidate[method](payload);
          if (result) {
            const refined = this.normalise(result, event, payload.coordinates);
            if (refined.committable) return refined;
          }
        }
      }
      try {
        const result = await this.controller.backend.refine(payload);
        const refined = result && this.normalise(result.hit || result, event, payload.coordinates);
        return refined && refined.committable ? refined : null;
      } catch (_) {
        return null;
      }
    }

    async worldAt(event, depthReference) {
      const coordinates = this.coordinates(event);
      const candidates = this.candidates();
      for (const candidate of candidates) {
        for (const name of ["unprojectAtDepth", "worldAtDepth", "screenToWorld", "unproject"] ) {
          if (typeof candidate[name] === "function") {
            const result = await candidate[name](coordinates.css[0], coordinates.css[1], depthReference, { event, coordinates });
            if (result) return vec3(result);
          }
        }
      }
      const hit = await this.pick(event);
      return hit ? hit.point_mm : null;
    }

    project(pointMm) {
      const candidates = this.candidates();
      for (const candidate of candidates) {
        for (const name of ["project", "worldToScreen", "projectPoint"]) {
          if (typeof candidate[name] === "function") {
            const result = candidate[name](pointMm);
            if (result) return Array.isArray(result) ? result : [result.x, result.y, result.z || 0];
          }
        }
      }
      return null;
    }

    cameraEvidence() {
      for (const candidate of this.candidates()) {
        for (const name of ["getCameraEvidence", "cameraEvidence", "selectionCamera"]) {
          const member = candidate && candidate[name];
          const raw = typeof member === "function" ? member.call(candidate) : member;
          if (!raw) continue;
          const eye = vec3(raw.eye_mm || raw.eye, [NaN, NaN, NaN]);
          const target = vec3(raw.target_mm || raw.target, [NaN, NaN, NaN]);
          const up = vec3(raw.up, [NaN, NaN, NaN]);
          const viewport = raw.viewport_px || raw.viewport;
          const projection = String(raw.projection || "perspective").toLowerCase();
          if (![...eye, ...target, ...up].every(Number.isFinite) || !Array.isArray(viewport) || viewport.length !== 2 || viewport.some(value => !Number.isFinite(+value) || +value <= 0)) continue;
          const evidence = {eye_mm:eye,target_mm:target,up,viewport_px:viewport.map(Number),projection};
          if(projection === "orthographic"){
            evidence.ortho_height_mm=Number(raw.ortho_height_mm ?? raw.height_mm);
            if(!Number.isFinite(evidence.ortho_height_mm)||evidence.ortho_height_mm<=0)continue;
          }else{
            evidence.projection="perspective";evidence.fov_deg=Number(raw.fov_deg ?? raw.fov);
            if(!Number.isFinite(evidence.fov_deg)||evidence.fov_deg<=0||evidence.fov_deg>=179)continue;
          }
          return evidence;
        }
      }
      throw new Error("Exact camera evidence is unavailable for registered-cell selection");
    }

    clipEvidence(hit = null) {
      if(hit?.clip_evidence && typeof hit.clip_evidence.active === "boolean") return deepClone(hit.clip_evidence);
      for (const candidate of this.candidates()) {
        for (const name of ["getClipEvidence", "clipEvidence", "selectionClip"]) {
          const member = candidate && candidate[name];
          const raw = typeof member === "function" ? member.call(candidate) : member;
          if(raw && typeof raw.active === "boolean" && raw.hit_on_clip_cap !== true)return deepClone(raw);
        }
      }
      throw new Error("Exact section-plane evidence is unavailable for registered-cell selection");
    }
  }

  function reconcileSculptOverlay(layer, nodes) {
     
     
    const markup = nodes.map(node => node.outerHTML).join("");
    if (layer.innerHTML !== markup) layer.replaceChildren(...nodes);
  }

   
  class SculptEnhancements {
    constructor(owner) {
      this.owner=owner; this.anchor=null; this.pickMode=null; this.records=[]; this.previewSelection=null;
      const panel=owner.sculptPanel;
      panel.querySelector('optgroup[label="Shape"]').insertAdjacentHTML('beforeend', '<option value="scale">Scale about pivot</option><option value="bend">Bend about root plane</option><option value="taper">Taper along direction</option>');
      panel.querySelector('[data-sculpt="tool"]').insertAdjacentHTML('beforeend','<optgroup label="Selection"><option value="select">Paint reusable selection</option></optgroup>');
      const extra=createElement('div','implexity-sculpt-grid');
      extra.innerHTML=`
        <details data-sculpt-selection-settings open><summary>Reusable selection</summary>
          <label>Selection slot<input data-sculpt="selectionId" value="main" maxlength="64" list="implexity-sculpt-slots"></label><datalist id="implexity-sculpt-slots"></datalist>
          <label class="implexity-sculpt-check"><input data-sculpt="useSelection" type="checkbox">Limit manual influence to selection</label>
          <label data-sculpt-selection-action>Paint operation<select data-sculpt="selectionAction"><option value="replace">Replace</option><option value="add">Add</option><option value="subtract">Subtract</option><option value="intersect">Intersect</option></select></label>
          <div class="implexity-sculpt-actions"><button type="button" data-sculpt-selection-command="all">All</button><button type="button" data-sculpt-selection-command="invert">Invert</button><button type="button" data-sculpt-selection-command="clear">Clear</button></div>
          <label class="implexity-sculpt-check"><input data-sculpt="showSelection" type="checkbox" checked>Show selection through model</label>
          <p data-sculpt-selection-status class="implexity-sculpt-note">No selection saved. A selection limits manual edits, not optimization.</p>
        </details>
        <label>Brush core [0–0.95]<input data-sculpt="hardness" type="number" min="0" max="0.95" step="0.05" value="0"></label>
        <details data-sculpt-pivot-settings><summary>Pivot and deformation</summary>
          <label>Pivot<select data-sculpt="pivotMode"><option value="hit">Picked point</option><option value="custom">Entered / separately picked point</option><option value="selection">Selection centre</option></select></label>
          <label data-sculpt-pivot-input>Pivot X, Y, Z [mm]<input data-sculpt="pivot" type="text" value="0, 0, 0"></label>
          <div class="implexity-sculpt-actions"><button type="button" data-sculpt-pick="pivot">Pick pivot</button><button type="button" data-sculpt-pick="direction">Pick axis end</button></div>
          <label data-sculpt-span>Axial length [mm]<input data-sculpt="axialLength" type="number" min="0.001" value="10" step="0.5"></label>
          <label data-sculpt-bend>Bend towards X, Y, Z<input data-sculpt="bendDirection" type="text" value="0, 1, 0"></label>
          <p class="implexity-sculpt-note">Drag an X/Y/Z handle for a model-axis gesture. Bend and taper keep their pivot plane fixed in the coordinate flow. Use Protect for a persistent shape constraint.</p>
        </details>
        <details data-sculpt-exact><summary>Place influence in the volume</summary>
          <label>Influence centre X, Y, Z [mm]<input data-sculpt="center" value="0, 0, 0"></label>
          <label>Displacement X, Y, Z [mm]<input data-sculpt="delta" value="1, 0, 0"></label>
          <label>Scale / stretch / taper factor<input data-sculpt="factor" type="number" min="0.02" max="50" step="0.05" value="1.1"></label>
          <label>Twist angle / bend amount [degrees]<input data-sculpt="angle" type="number" min="-720" max="720" value="10" step="1"></label>
          <label>Drag translation increment [mm, 0 = free]<input data-sculpt="linearSnap" type="number" min="0" value="0" step="0.1"></label>
          <label>Drag angle increment [degrees, 0 = free]<input data-sculpt="angleSnap" type="number" min="0" value="0" step="1"></label>
          <button type="button" data-sculpt-apply-exact>Preview exact values</button>
          <p class="implexity-sculpt-note">Enter an influence centre anywhere in the editable volume, then preview and apply. Add and subtract work without a surface pick. [ and ] change brush radius.</p>
        </details>`;
      panel.querySelector('.implexity-sculpt-grid').append(extra);
      owner.sculptControls=Object.fromEntries(Array.from(panel.querySelectorAll('[data-sculpt]')).map(e=>[e.dataset.sculpt,e]));
      this.c=owner.sculptControls;
      panel.querySelectorAll('[data-sculpt-pick]').forEach(b=>b.addEventListener('click',()=>{
        if(owner.activeGesture||owner.pendingGesture||owner.selectionOperationPending||manualHistory.counts().busy)return;
        this.pickMode=b.dataset.sculptPick;
        owner.toast(this.pickMode==='pivot'?'Click an exact surface point for the pivot. Escape cancels picking.':'Click an axis endpoint measured from the current pivot. Escape cancels picking.','info');
      }));
      panel.querySelectorAll('[data-sculpt-selection-command]').forEach(b=>b.addEventListener('click',()=>this.applyExact(b.dataset.sculptSelectionCommand)));
      panel.querySelector('[data-sculpt-apply-exact]').addEventListener('click',()=>this.previewExact());
      this.layer=document.createElementNS(SVG_NS,'svg');this.layer.classList.add('implexity-sculpt-overlay');this.layer.setAttribute('aria-label','Sculpt selection and axis handles');owner.viewport.append(this.layer);
      this.layer.addEventListener('pointerdown',e=>{
        const handle=e.target.closest?.('[data-sculpt-axis]');if(!handle||!this.anchor||owner.activeGesture||owner.pendingGesture)return;
        this.c.direction.value=handle.dataset.sculptAxis;owner._configureSculptOptions();
        owner._sculptAnchorOverride=deepClone(this.anchor);
         
      });
      global.addEventListener('implexity:interaction-mode',()=>{this.pickMode=null;this.render();});
      let last=0;
      const tick=now=>{if(!owner.viewport.isConnected)return;if(now-last>120){last=now;if(owner.mode===MODES.SCULPT)this.render();}requestAnimationFrame(tick);};requestAnimationFrame(tick);
    }
    vector(text,name){const a=String(text).split(/[,\s]+/).filter(Boolean).map(Number);if(a.length!==3||!a.every(Number.isFinite))throw new Error(`${name} requires three finite numbers.`);return a;}
    record(){return this.records.find(r=>r.selection_id===this.c.selectionId.value.trim());}
    hydrate(field){this.records=field?.sculpt_selections||[];this.previewSelection=null;this.configure();}
    configure(){
      const c=this.c,o=this.owner,tool=c.tool.value;
      o.sculptPanel.querySelector('[data-sculpt-selection-action]').hidden=tool!=='select';
      c.useSelection.disabled=tool==='select';
      o.sculptPanel.querySelector('[data-sculpt-pivot-input]').hidden=c.pivotMode.value!=='custom';
      o.sculptPanel.querySelector('[data-sculpt-span]').hidden=!['bend','taper'].includes(tool);
      o.sculptPanel.querySelector('[data-sculpt-bend]').hidden=tool!=='bend';
      c.direction.disabled=!['grab','stretch','twist','bend','taper','flatten'].includes(tool);
      o.sculptPanel.querySelector('#implexity-sculpt-slots').replaceChildren(...this.records.map(r=>new Option(r.selection_id,r.selection_id)));
      const r=this.record();
      o.sculptPanel.querySelector('[data-sculpt-selection-status]').textContent=r?.valid?`${r.selected_samples} samples in “${r.selection_id}”. ${r.overlay_sampled?'Overlay subsampled. ':''}Selection is fixed in model space and is not an optimizer hold.`:r?`Selection is stale: ${r.reason}`:'No selection in this slot. Paint one or choose All. Enabling an absent selection refuses the edit.';
      const help={select:'Paint a reusable soft selection. Choose Replace, Add, Subtract or Intersect, then switch back to a deformation tool and enable Edit only this selection. Shift adds and Alt subtracts for this stroke.',scale:'Drag vertically to scale about the pivot. The same factor applies in all directions before local influence and protection.',bend:'Direction is the longitudinal axis. Bend towards defines the bending plane. Axial length sets the curvature scale. Bend amount is not an exact tip angle.',taper:'Direction runs from the pivot plane towards the tapered end. Scaling rises smoothly to the requested factor over Axial length. The pivot plane is unchanged in the coordinate flow.'};
      if(help[tool])o.sculptPanel.querySelector('[data-sculpt-help]').textContent=help[tool];
      this.render();
    }
    settings(hit){
      const c=this.c,settings={hardness:Number(c.hardness.value),selection_id:c.selectionId.value.trim(),
        use_selection:c.tool.value!=='select'&&c.useSelection.checked,selection_action:c.selectionAction.value,
        _linearSnap:c.tool.value==='grab'?Number(c.linearSnap.value):0,
        _angleSnap:['twist','bend'].includes(c.tool.value)?Number(c.angleSnap.value):0};
      if(['bend','taper'].includes(c.tool.value))settings.axial_length_mm=Number(c.axialLength.value);
      if(c.tool.value==='scale')settings._directionMode='view';
      if(c.tool.value==='bend')settings.bend_direction=this.vector(c.bendDirection.value,'Bend direction');
      if(['grab','stretch','twist','scale','bend','taper'].includes(c.tool.value)&&c.pivotMode.value==='custom')settings.pivot_mm=this.vector(c.pivot.value,'Pivot');
      if(['grab','stretch','twist','scale','bend','taper'].includes(c.tool.value)&&c.pivotMode.value==='selection'){
        const r=this.record();if(!r?.valid||!r.centroid_mm)throw new Error('Choose a nonempty valid selection before using its centre as pivot.');
        settings.pivot_mm=r.centroid_mm.slice();
      }
      if(!Number.isFinite(settings._linearSnap)||settings._linearSnap<0||!Number.isFinite(settings._angleSnap)||settings._angleSnap<0)throw new Error('Drag increments must be finite and nonnegative.');
      return settings;
    }
    picked(hit){
      const previousAnchor=this.anchor;
      this.anchor=deepClone(hit);this.c.center.value=hit.point_mm.map(v=>Number(v.toPrecision(9))).join(', ');
      if(!this.pickMode){this.render();return false;}
      if(this.pickMode==='pivot'){
        this.c.pivot.value=hit.point_mm.map(v=>Number(v.toPrecision(12))).join(', ');this.c.pivotMode.value='custom';
      }else{
        const p=this.c.pivotMode.value==='selection'?this.record()?.centroid_mm:this.c.pivotMode.value==='hit'?previousAnchor?.point_mm:this.vector(this.c.pivot.value,'Pivot');
        if(!p)throw new Error('Choose a pivot before picking the axis endpoint.');
        const axis=sub3(hit.point_mm,p);if(len3(axis)<1e-9)throw new Error('The axis endpoint must differ from the pivot.');
        this.c.custom.value=axis.map(v=>Number(v.toPrecision(12))).join(', ');this.c.direction.value='custom';
        this.c.pivot.value=p.join(', ');this.c.pivotMode.value='custom';
      }
      this.pickMode=null;this.owner._configureSculptOptions();return true;
    }
    gestureOperation(operation,settings,event){
      const clean={...operation};
      if(clean.tool==='select')clean.selection_action=event.shiftKey?'add':event.altKey?'subtract':settings.selection_action;
      if(settings._linearSnap>0&&clean.tool==='grab'){
        if(settings._directionMode==='view')clean.delta_mm=clean.delta_mm.map(v=>Math.round(v/settings._linearSnap)*settings._linearSnap);
        else {const along=clean.delta_mm.reduce((sum,v,i)=>sum+v*settings.direction[i],0);const snapped=Math.round(along/settings._linearSnap)*settings._linearSnap;clean.delta_mm=settings.direction.map(v=>v*snapped);}
      }
      if(settings._angleSnap>0&&['twist','bend'].includes(clean.tool)){
        const step=settings._angleSnap*Math.PI/180;clean.angle_rad=Math.round(clean.angle_rad/step)*step;
      }
      for(const key of Object.keys(clean))if(key.startsWith('_'))delete clean[key];
      return clean;
    }
     
     
    exactOperation() {
      const o=this.owner,c=this.c;
      const hit={point_mm:this.vector(c.center.value,'Influence centre'),normal:this.anchor?.normal?.slice()||[0,0,1]};
      const operation=o._sculptSettings(hit);
      for(const key of Object.keys(operation))if(key.startsWith('_'))delete operation[key];
      if(operation.tool==='grab')operation.delta_mm=this.vector(c.delta.value,'Displacement');
      if(['scale','stretch','taper'].includes(operation.tool)){
        operation.factor=Number(c.factor.value);
        if(!c.factor.value.trim()||!Number.isFinite(operation.factor)||operation.factor<=0)throw new Error('Enter a positive scale factor.');
      }
      if(['twist','bend'].includes(operation.tool)){
        operation.angle_rad=Number(c.angle.value)*Math.PI/180;
        if(!c.angle.value.trim()||!Number.isFinite(operation.angle_rad))throw new Error('Enter a finite angle.');
      }
      return operation;
    }
    async previewExact() {
      const o=this.owner;
      if(o._editingRecoveryRequired||o.activeGesture||o.pendingGesture||o.selectionOperationPending||manualHistory.counts().busy)return;
      const review={status:'preparing',cancelRequested:false,transaction:null};
      this.exactReview=review;o.selectionOperationPending=true;
      dispatch('implexity:editing-review',{status:'preparing'});
      try{
        const operation=this.exactOperation(),field=o._activeFieldPayload();
        if(!field)throw new Error('Create an editable field or controlled volume before shaping.');
        review.transaction=await o.backend.begin({kind:'geometry_sculpt',field_id:o._activeFieldId(),field_identity:field.identity,preview_geometry:true,preview_count:32});
        if(review.cancelRequested){review.status='ready';await this.cancelExact();return;}
        const reply=await o.backend.preview({sequence:1,operation});
        if(review.cancelRequested){review.status='ready';await this.cancelExact();return;}
        const sequences=[reply.sequence??reply.latest_applied,reply.latest_applied,reply.latest_requested].filter(v=>v!==undefined);
        if(reply.accepted!==true||!sequences.length||sequences.some(v=>v!==1))throw new Error('The exact-value preview was not acknowledged.');
        review.evidence=reply.state?.evidence||{};
        const fieldPreview=reply.state?.preview_field;
        if(fieldPreview)await global.ImplexityViewerAdapter?.applyGeometryPreview?.(fieldPreview);
        if(review.cancelRequested){review.status='ready';await this.cancelExact();return;}
        review.status='ready';
        this.evidence(review.evidence,false);
        o.sculptPanel.querySelector('[data-sculpt-evidence]').textContent=review.evidence.selection
          ? `Selection preview · ${review.evidence.selection.selected_samples} samples. Apply to save the selection, or Cancel. Geometry is unchanged.`
          : `Preview only · ${review.evidence.changed_control_values||0} values changed. Apply to save, or Cancel.`;
        dispatch('implexity:editing-review',{status:'ready',evidence:review.evidence});
      }catch(error){
        try{if(review.transaction)await o.backend.cancel();}catch(_){o._editingRecoveryRequired=true;}
        try{await global.ImplexityViewerAdapter?.clearGeometryPreview?.();}catch(_){o._editingRecoveryRequired=true;}
        this.exactReview=null;o.selectionOperationPending=false;
        o._showError(error,'The numerical preview could not be prepared. No edit was applied.');
        dispatch('implexity:editing-review',{status:'error',message:friendlyErrorMessage(error,'Check the values and try again.')});
      }
    }
    async confirmExact() {
      const review=this.exactReview,o=this.owner;
      if(review?.status!=='ready')return;
      review.status='committing';let saved=false;
      dispatch('implexity:editing-review',{status:'committing'});
      try{
        const result=await o.backend.commit({final_sequence:1});saved=true;
        if(result?.history?.label)manualHistory.record('viewport',result.history.label);
        this.evidence(result.evidence||{},true);
        await o._refreshCommittedGeometry();
        dispatch('implexity:interaction-committed',{mode:MODES.SCULPT,transaction:review.transaction,result});
      }catch(error){
         
         
        if(!saved)try{await o.backend.cancel();}catch(_){}
        try{await o._refreshCommittedGeometry();}
        catch(_){o._editingRecoveryRequired=true;}
        o._showError(error,saved?'The edit was saved, but its view needs to be refreshed.':'Apply was not confirmed. The saved model has been re-read. Check it before continuing.');
        dispatch('implexity:editing-review',{status:'error',message:saved?'Saved edit; refresh required.':'Apply not confirmed. Check the saved model.'});
      }finally{
        this.exactReview=null;o.selectionOperationPending=false;
        this.previewSelection=null;this.render();
        dispatch('implexity:editing-review',{status:o._editingRecoveryRequired?'recovery':'idle'});
      }
    }
    async cancelExact() {
      const review=this.exactReview,o=this.owner;if(!review)return;
      if(review.status==='committing'||review.status==='cancelling')return;
      review.cancelRequested=true;
       
      if(review.status==='preparing')return;
      review.status='cancelling';
      try{await o.backend.cancel();}
      catch(error){o._editingRecoveryRequired=true;o._showError(error,'Cancellation was not confirmed. Refresh before continuing.');}
      finally{
        try{await global.ImplexityViewerAdapter?.clearGeometryPreview?.();}catch(error){o._editingRecoveryRequired=true;o._showError(error,'The saved viewport could not be restored. Refresh before continuing.');}
        this.exactReview=null;this.previewSelection=null;o.selectionOperationPending=false;
        o.sculptPanel.querySelector('[data-sculpt-evidence]').textContent=o._editingRecoveryRequired?'No commit was sent. Refresh the saved model before further editing.':'Preview cancelled. The saved model was not edited.';
        this.render();dispatch('implexity:editing-review',{status:o._editingRecoveryRequired?'recovery':'cancelled'});
      }
    }
    async applyExact(selectionAction=null){
      const o=this.owner,c=this.c;if(o._editingRecoveryRequired||o.activeGesture||o.pendingGesture||o.selectionOperationPending||manualHistory.counts().busy)return;
      let started=false,saved=false; o.selectionOperationPending=true;
      try{
        const hit={point_mm:this.vector(c.center.value,'Influence centre'),normal:this.anchor?.normal?.slice()||[0,0,1]};
        let operation=selectionAction?{tool:'select',center_mm:hit.point_mm,scope:'whole',selection_id:c.selectionId.value.trim(),selection_action:selectionAction,use_selection:false}:o._sculptSettings(hit);
        for(const key of Object.keys(operation))if(key.startsWith('_'))delete operation[key];
        if(!selectionAction){operation.delta_mm=this.vector(c.delta.value,'Displacement');operation.factor=Number(c.factor.value);operation.angle_rad=Number(c.angle.value)*Math.PI/180;}
        const field=o._activeFieldPayload();if(!field)throw new Error('Load an editable geometry first.');
        const tx=await o.backend.begin({kind:'geometry_sculpt',field_id:o._activeFieldId(),field_identity:field.identity,preview_geometry:true,preview_count:24});started=true;
        const preview=await o.backend.preview({sequence:1,operation});
        const acknowledged=[preview.sequence??preview.latest_applied,preview.latest_applied,preview.latest_requested].filter(v=>v!==undefined);
        if(preview.accepted!==true||!acknowledged.length||acknowledged.some(v=>!Number.isInteger(v)||v!==1))throw new Error('The exact-value preview was not acknowledged.');
        const result=await o.backend.commit({final_sequence:1});started=false;saved=true;
        if(result?.history?.label)manualHistory.record('viewport',result.history.label);
        this.evidence(result.evidence||{},true);
        await o._refreshCommittedGeometry();
        dispatch('implexity:interaction-committed',{mode:MODES.SCULPT,transaction:tx,result});
      }catch(error){if(started)try{await o.backend.cancel();}catch(_){}o._showError(error,saved?'The edit was saved, but its viewport refresh failed. Refresh before continuing.':'The exact-value edit was not saved.');}
      finally{o.selectionOperationPending=false;}
    }
    evidence(e,saved=false){
      if(e.selection){this.previewSelection=saved?null:e.selection;this.owner.sculptPanel.querySelector('[data-sculpt-evidence]').textContent=`${saved?'Saved':'Uncommitted'} selection · ${e.selection.selected_samples} samples. Geometry and optimizer freedom unchanged.`;}
      else if(saved)this.owner.sculptPanel.querySelector('[data-sculpt-evidence]').textContent=`Saved · ${e.changed_control_values||0} values changed. Run fresh physics preflight.`;
      this.render();
    }
    render(){
      const o=this.owner;this.layer.style.display=o.mode===MODES.SCULPT?'':'none';if(o.mode!==MODES.SCULPT)return;
      const nodes=[],selection=this.previewSelection||this.record();
      if(this.c.showSelection.checked&&(selection?.valid||this.previewSelection))for(let i=0;i<(selection.points_mm||[]).length;i++){
        const p=o.picking.project(selection.points_mm[i]);if(!p||!p.slice(0,2).every(Number.isFinite))continue;
        nodes.push(createSvgElement('circle','implexity-sculpt-selected-point',{cx:p[0],cy:p[1],r:2.2,opacity:.18+.6*selection.weights[i]}));
      }
      if(this.anchor&&['grab','stretch','twist','scale','bend','taper'].includes(this.c.tool.value)){
        let pivot=this.anchor.point_mm;
        try{if(this.c.pivotMode.value==='custom')pivot=this.vector(this.c.pivot.value,'Pivot');else if(this.c.pivotMode.value==='selection'&&this.record()?.centroid_mm)pivot=this.record().centroid_mm;}catch(_){}
        const a=o.picking.project(pivot),length=Math.max(.001,Number(o.radiusInput.value)||1);
        if(a&&a.slice(0,2).every(Number.isFinite)){
          nodes.push(createSvgElement('circle','implexity-sculpt-pivot',{cx:a[0],cy:a[1],r:4}));
          for(let i=0;i<3;i++){
            const v=pivot.slice();v[i]+=length;const b=o.picking.project(v);if(!b)continue;
            const dx=b[0]-a[0],dy=b[1]-a[1],n=Math.hypot(dx,dy);if(n<3)continue;
            const end=[a[0]+dx/n*65,a[1]+dy/n*65];
            const group=createSvgElement('g','implexity-sculpt-axis');group.dataset.sculptAxis='xyz'[i];group.setAttribute('aria-label',`Drag model ${'XYZ'[i]} direction`);
            group.append(createSvgElement('line','',{x1:a[0],y1:a[1],x2:end[0],y2:end[1]}));
            group.append(createSvgElement('circle','',{cx:end[0],cy:end[1],r:11}));
            const text=createSvgElement('text','',{x:end[0],y:end[1]+4,'text-anchor':'middle'});text.textContent='XYZ'[i];group.append(text);nodes.push(group);
          }
        }
      }
      reconcileSculptOverlay(this.layer,nodes);
    }
  }


  class ViewportInteraction {
    constructor(options = {}) {
      this.options = Object.assign({}, DEFAULTS, options);
      this.viewport = this._findViewport(options.viewport);
      if (!this.viewport) throw new Error("Implexity interaction layer could not locate the viewport");
      this.viewport.classList.add("implexity-viewport-authoring");
      if (!this.viewport.hasAttribute("tabindex")) this.viewport.tabIndex = 0;
      if (getComputedStyle(this.viewport).position === "static") this.viewport.style.position = "relative";
      this.mode = MODES.SELECT;
      this.activeGesture = null;
      this.pendingGesture = null;
      this.hoverHit = null;
      this.glyphs = new Map();
      this.cages = new Map();
      this.selectedControl = null;
      this.selection = null;
      this._fieldRefresh = null;
      this._modelEventRevision = 0;
      this.pointerOwner = this;
      this.backend = new InteractionBackend(this);
      this.picking = new PickingAdapter(this);
      this.previewQueue = new SerialPreviewQueue((request, signal) => this._executePreview(request, signal));
      this._pendingFrame = 0;
      this._lastPointer = null;
      this._buildUi();
      this.sculptEnhancements=new SculptEnhancements(this);
      this._bind();
      this.setMode(MODES.SELECT);
      manualHistory.register("viewport", { undo: payload => this._historyLocal("undo", payload), redo: payload => this._historyLocal("redo", payload), refresh: () => this._refreshCommittedGeometry() });
      this._refreshInitialField().catch(error => {
        this._setInspectorStatus("The editable field could not be loaded. See the warning for technical details.", "warning");
        this._showError(error, "The editable field could not be loaded.");
      });
      global.ImplexityInteraction = this;
      dispatch("implexity:interaction-ready", { controller: this });
    }

    _findViewport(explicit) {
      if (explicit instanceof Element) return explicit;
      if (typeof explicit === "string") return document.querySelector(explicit);
      return document.querySelector("[data-implexity-viewport], #viewport, #viewer, .viewport, .viewer") ||
             (document.querySelector("canvas") && document.querySelector("canvas").parentElement);
    }

    _buildUi() {
      this.toolbar = createElement("div", "implexity-toolbar");
      this.toolbar.setAttribute("role", "toolbar");
      this.toolbar.setAttribute("aria-label", "Viewport authoring tools");
      this.buttons = new Map();
      let row = createElement("div", "implexity-toolbar-row");
      this.toolbar.appendChild(row);
      TOOL_DEFS.forEach((def, index) => {
        if (index === 4 || index === 10) {
          row = createElement("div", "implexity-toolbar-row");
          this.toolbar.appendChild(row);
        }
        const [mode, label, key, icon] = def;
        const button = createElement("button", "implexity-tool", icon);
        button.type = "button";
        button.title = key ? `${label} (${key})` : label;
        button.dataset.mode = mode;
        button.setAttribute("aria-label", label);
        button.setAttribute("aria-pressed", "false");
        const shortLabel=({move:"Move",size:"Resize",surface:"Surface drag",field_brush:"Brush",control_lattice:"Control points",deformation_cage:"Cage",surface_patch:"Region",clamp:"Support"})[mode]||label;
        button.appendChild(createElement("span", "implexity-tool-label", shortLabel));
        if (key) button.appendChild(createElement("span", "implexity-tool-key", key));
        button.addEventListener("click", () => this.setMode(mode));
        row.appendChild(button);
        this.buttons.set(mode, button);
      });

      this.optionsPanel = createElement("section", "implexity-options");
      this.optionsPanel.innerHTML = `
        <div class="implexity-options-title">Interaction settings</div>
        <div class="implexity-option-grid">
          <label for="implexity-interaction-radius" data-option-label="radius">Patch radius [mm]</label><input id="implexity-interaction-radius" data-option-control="radius" type="number" min="0.000001" step="0.1" value="${this.options.radiusMm}">
          <label for="implexity-interaction-strength" data-option-label="strength">Brush increment [field units]</label><input id="implexity-interaction-strength" data-option-control="strength" type="number" step="0.01" value="${this.options.strength}">
          <label for="implexity-interaction-magnitude" data-option-label="magnitude">Magnitude</label><input id="implexity-interaction-magnitude" data-option-control="magnitude" type="number" step="0.1" value="${this.options.magnitude}">
          <label for="implexity-interaction-brush-mode" data-option-label="brushMode">Brush operation</label><select id="implexity-interaction-brush-mode" data-option-control="brushMode"><option value="add">Increase field value</option><option value="subtract">Decrease field value</option><option value="set">Set value</option><option value="smooth">Smooth field</option></select>
          <label for="implexity-interaction-falloff" data-option-label="falloff">Edge falloff</label><select id="implexity-interaction-falloff" data-option-control="falloff"><option value="smoothstep">Smooth</option><option value="linear">Linear</option><option value="gaussian">Gaussian</option><option value="constant">Constant</option></select>
          <label for="implexity-interaction-snap" data-option-label="snap">Movement snap [mm]</label><input id="implexity-interaction-snap" data-option-control="snap" type="number" min="0" step="0.1" value="${this.options.snap}">
        </div>`;
      this.sculptPanel = createElement("div", "implexity-sculpt-panel");
      this.sculptPanel.hidden = true;
      this.sculptPanel.innerHTML = `
        <div class="implexity-sculpt-grid">
          <label>Tool<select data-sculpt="tool">
            <optgroup label="Shape"><option value="grab">Grab / local warp</option><option value="stretch">Stretch along direction</option><option value="twist">Twist around direction</option></optgroup>
            <optgroup label="Sculpt"><option value="inflate">Inflate / thicken</option><option value="deflate">Deflate / thin</option><option value="smooth">Smooth</option><option value="add">Add material</option><option value="subtract">Remove material</option><option value="flatten">Flatten to plane</option></optgroup>
            <optgroup label="Maintain"><option value="protect">Protect region</option><option value="release">Release protected region</option></optgroup></select></label>
          <label>Direction<select data-sculpt="direction"><option value="view">Screen plane</option><option value="normal">Picked surface normal</option><option value="x">Model X</option><option value="y">Model Y</option><option value="z">Model Z</option><option value="custom">Custom vector</option></select></label>
          <label data-sculpt-custom hidden>Vector X, Y, Z<input data-sculpt="custom" type="text" value="1, 0, 0" aria-label="Custom model direction vector"></label>
          <label>Influence<select data-sculpt="scope"><option value="local">Local brush / stroke</option><option value="box">Numeric box</option><option value="whole">Entire editable domain</option></select></label>
          <label data-sculpt-depth>Half-depth along surface normal [mm]<input data-sculpt="depth" type="number" value="3" min="0.001" step="0.1"></label>
          <div data-sculpt-box hidden><label>Box minimum X, Y, Z [mm]<input data-sculpt="boxMin" type="text" value="0, 0, 0"></label><label>Box maximum X, Y, Z [mm]<input data-sculpt="boxMax" type="text" value="10, 10, 10"></label></div>
          <label data-sculpt-amount>Thickness increment [mm]<input data-sculpt="amount" type="number" value="0.25" min="0" step="0.05"></label>
          <label data-sculpt-strength>Stroke strength [0–1]<input data-sculpt="strength" type="number" value="0.35" min="0" max="1" step="0.05"></label>
          <label data-sculpt-protection>Maintain<select data-sculpt="protection"><option value="geometry">Sampled shape</option><option value="controls">Design controls</option></select></label>
          <label class="implexity-sculpt-check" data-sculpt-phase-hold><input data-sculpt="protectPhase" type="checkbox">Hold neutral phase too</label>
          <details><summary>Symmetry and phase transport</summary>
            <label>Mirror across model plane<select data-sculpt="symmetry"><option value="">None</option><option value="x">X = 0</option><option value="y">Y = 0</option><option value="z">Z = 0</option><option value="x,y">X = 0 and Y = 0</option></select></label>
            <label class="implexity-sculpt-check"><input data-sculpt="carryPhase" type="checkbox">Carry neutral phase with shape</label>
          </details>
        </div>
        <label data-sculpt-volume-wrap hidden>Editable native volume<select data-sculpt-volume aria-label="Editable native volume"></select></label>
        <p data-sculpt-target class="implexity-sculpt-note"></p>
        <p data-sculpt-help class="implexity-sculpt-note">Grab a surface point and drag. Small depth isolates the picked side. The screen-plane cursor defines one anchored stroke.</p>
        <div class="implexity-sculpt-actions"><button type="button" data-sculpt-undo>Undo</button><button type="button" data-sculpt-redo>Redo</button></div>
        <output data-sculpt-evidence aria-live="polite">No uncommitted edit.</output>`;
      this.optionsPanel.append(this.sculptPanel);
      this.sculptControls = Object.fromEntries(Array.from(this.sculptPanel.querySelectorAll("[data-sculpt]")).map(e=>[e.dataset.sculpt,e]));
      this.sculptPanel.addEventListener("change",()=>this._configureSculptOptions());
      this.sculptPanel.querySelector("[data-sculpt-volume]").addEventListener("change",async e=>{
        if(this.activeGesture||this.pendingGesture)return;
        this._nativeComponentId=e.target.value;
        try{await this._refreshAuthoritativeField();}catch(error){this._showError(error,"The selected native volume is unavailable.");}
      });
      this.sculptPanel.querySelector("[data-sculpt-undo]").addEventListener("click",()=>this._requestHistory("undo"));
      this.sculptPanel.querySelector("[data-sculpt-redo]").addEventListener("click",()=>this._requestHistory("redo"));
      this.optionsTitle = this.optionsPanel.querySelector(".implexity-options-title");
      this.radiusInput = this.optionsPanel.querySelector("#implexity-interaction-radius");
      this.strengthInput = this.optionsPanel.querySelector("#implexity-interaction-strength");
      this.magnitudeInput = this.optionsPanel.querySelector("#implexity-interaction-magnitude");
      this.brushModeInput = this.optionsPanel.querySelector("#implexity-interaction-brush-mode");
      this.falloffInput = this.optionsPanel.querySelector("#implexity-interaction-falloff");
      this.snapInput = this.optionsPanel.querySelector("#implexity-interaction-snap");
      this.optionControls = Object.fromEntries(["radius", "strength", "magnitude", "brushMode", "falloff", "snap"].map(key => [key, {
        label: this.optionsPanel.querySelector(`[data-option-label="${key}"]`),
        control: this.optionsPanel.querySelector(`[data-option-control="${key}"]`)
      }]));

      this.cursorLayer = createElement("div", "implexity-cursor-layer");
      this.brushRing = createElement("div", "implexity-brush-ring");
      this.cursorLayer.appendChild(this.brushRing);
      this.hud = createElement("div", "implexity-hud");
      this.hud.innerHTML = '<div class="implexity-hud-title"></div><div class="implexity-hud-detail"></div>';

      this.glyphLayer = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      this.glyphLayer.classList.add("implexity-glyph-layer");
      this.glyphLayer.setAttribute("aria-label", "Engineering objects");
      this.cageLayer = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      this.cageLayer.classList.add("implexity-cage-layer");
      this.selectionLayer = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      this.selectionLayer.classList.add("implexity-selection-layer");
      this.selectionLayer.setAttribute("aria-hidden", "true");
      this.inspector = createElement("aside", "implexity-selection-inspector");
      this.inspector.setAttribute("aria-label", "Selection and regions");
      this.inspector.innerHTML = `
        <header><div><strong>Selection &amp; regions</strong><span data-selection-field>No editable field</span></div><button type="button" data-selection-collapse aria-label="Collapse selection inspector" aria-expanded="true">−</button></header>
        <div class="implexity-inspector-body">
          <div class="implexity-active-tool"><span>Active tool</span><strong data-active-tool>Select</strong><p data-tool-help>Click an exact cell surface to create a reusable region.</p></div>
          <div class="implexity-counts" aria-live="polite"><span><b data-count-selected>0</b> selected</span><span><b data-count-affected>0</b> affected</span><span><b data-count-protected>0</b> protected</span></div>
          <label>Selection operation<select data-selection-operation><option value="replace">Replace selection</option><option value="add">Add to selection</option><option value="subtract">Subtract from selection</option><option value="intersect">Intersect selection</option></select></label>
          <div class="implexity-inline-fields"><label>Depth<select data-selection-visibility><option value="front">Visible front only</option><option value="through">Through / X-ray</option></select></label><label>Snap<select data-selection-snap><option value="feature" selected>Nearest surface feature</option><option value="cell">Cell centre</option><option value="grid">Grid vertex</option><option value="none">No display snap</option></select></label></div>
          <label class="implexity-check"><input type="checkbox" data-selection-connected><span>Keep only the connected component</span></label>
          <label class="implexity-check"><input type="checkbox" data-selection-volume><span>Select interior cells too (optimisable volume, not a boundary surface)</span></label>
          <details><summary>Numeric point and bounds</summary>
            <fieldset><legend>World point [mm]</legend><div class="implexity-vector">${["X","Y","Z"].map((axis,i)=>`<label>${axis}<input type="number" step="any" data-world-point="${i}"></label>`).join("")}</div><button type="button" data-apply-point>Select point</button></fieldset>
            <fieldset><legend>World bounds [mm]</legend><div class="implexity-bounds">${["X","Y","Z"].map((axis,i)=>`<label>${axis} min<input type="number" step="any" data-world-min="${i}"></label><label>${axis} max<input type="number" step="any" data-world-max="${i}"></label>`).join("")}</div><button type="button" data-apply-world>Apply world bounds</button></fieldset>
            <fieldset><legend>Cell-index bounds</legend><div class="implexity-bounds">${["I","J","K"].map((axis,i)=>`<label>${axis} min<input type="number" step="1" data-index-min="${i}"></label><label>${axis} max<input type="number" step="1" data-index-max="${i}"></label>`).join("")}</div><button type="button" data-apply-index>Apply index bounds</button></fieldset>
          </details>
          <label>Region name<input type="text" data-region-name value="Current selection" maxlength="80"></label>
          <div class="implexity-mask-panel"><strong>Protected cells</strong><div data-protected-reasons>No protected masks</div></div>
          <label>Saved regions<select data-region-list aria-label="Saved exact regions"><option value="">Current selection</option></select></label>
          <div class="implexity-inspector-actions"><button type="button" data-selection-clear>Clear</button><button type="button" data-selection-undo>Undo</button><button type="button" data-selection-redo>Redo</button></div>
          <div class="implexity-inspector-status" data-inspector-status role="status" aria-live="polite" aria-atomic="true"><span>Ready for an exact selection.</span><button type="button" data-status-dismiss aria-label="Dismiss status">×</button></div>
        </div>`;
      this.selectionOperation=this.inspector.querySelector("[data-selection-operation]");
      this.selectionVisibility=this.inspector.querySelector("[data-selection-visibility]");
      this.selectionSnap=this.inspector.querySelector("[data-selection-snap]");
      this.selectionConnected=this.inspector.querySelector("[data-selection-connected]");
      this.regionName=this.inspector.querySelector("[data-region-name]");
      this.toastStack = createElement("div", "implexity-toast-stack");
      this.contextMenu = createElement("div", "implexity-context-menu");
      this.contextMenu.innerHTML = '<div class="implexity-context-title">Create on surface</div>';
      [
        [MODES.REGION, "◎", "Surface region", "G"],
        [MODES.PRESSURE, "⇥", "Pressure", "P"],
        [MODES.TRACTION, "→", "Traction", "T"],
        [MODES.HEAT, "♨", "Heat flux", "H"],
        [MODES.TEMPERATURE, "K", "Temperature", "K"],
        [MODES.CLAMP, "⊥", "Clamp", "C"],
        [MODES.BRUSH, "●", "Field brush", "B"],
        [MODES.CAGE, "◇", "Deformation cage", ""]
      ].forEach(([mode, icon, label, key]) => {
        const button = createElement("button", "implexity-context-action");
        button.type = "button";
        button.dataset.mode = mode;
        button.innerHTML = `<span>${icon}</span><span>${label}</span><span class="implexity-context-key">${key}</span>`;
        button.addEventListener("click", async () => {
          const hit = this.contextHit;
          this._hideContextMenu();
          this.setMode(mode);
          if (hit && ![MODES.BRUSH, MODES.CAGE].includes(mode)) await this._placeAtHit(mode, hit);
        });
        this.contextMenu.appendChild(button);
      });

      this.viewport.append(this.cursorLayer, this.selectionLayer, this.glyphLayer, this.cageLayer, this.toolbar, this.optionsPanel, this.inspector, this.hud, this.contextMenu, this.toastStack);
      const engineeringPane=document.querySelector('#implexityInspectorPane')||document.querySelector('#s_params')?.parentElement;
      if(engineeringPane)engineeringPane.insertBefore(this.inspector,engineeringPane.firstChild);
      this._setSelectionInspectorCollapsed(true);
      this.inspector.querySelector("[data-selection-collapse]").addEventListener("click",()=>{this.inspectorExpansionExplicit=true;this._setSelectionInspectorCollapsed(this.inspector.dataset.collapsed!=="true");});
      this.inspector.querySelector("[data-status-dismiss]").addEventListener("click",()=>{this.inspector.querySelector("[data-inspector-status]").hidden=true;});
      this.inspector.querySelector("[data-apply-point]").addEventListener("click",()=>this._applyNumericSelection("point"));
      this.inspector.querySelector("[data-apply-world]").addEventListener("click",()=>this._applyNumericSelection("world"));
      this.inspector.querySelector("[data-apply-index]").addEventListener("click",()=>this._applyNumericSelection("index"));
      this.inspector.querySelector("[data-selection-clear]").addEventListener("click",()=>this._applySelectionOperation({kind:"indices",selected_runs:[],visibility:"through",surface_only:false},"replace"));
      this.inspector.querySelector("[data-selection-undo]").addEventListener("click",()=>this._requestHistory("undo"));
      this.inspector.querySelector("[data-selection-redo]").addEventListener("click",()=>this._requestHistory("redo"));
      this.inspector.querySelector("[data-region-list]").addEventListener("change",event=>{if(event.target.value){spatialSelections.setActive(event.target.value);this.selection=spatialSelections.active();this._renderSelectionInspector();this.renderSelection();}});
    }

    _bind() {
      this.viewport.addEventListener("pointermove", event => this._onPointerMove(event));
      this.viewport.addEventListener("pointerdown", event => this._onPointerDown(event));
      this.viewport.addEventListener("pointerup", event => this._onPointerUp(event));
      this.viewport.addEventListener("pointercancel", () => this.cancel());
      this.viewport.addEventListener("contextmenu", event => this._onContextMenu(event));
      this.viewport.addEventListener("pointerdown", event => {
        if (!event.target.closest(".implexity-context-menu") && event.button !== 2) this._hideContextMenu();
      }, true);
      this.viewport.addEventListener("pointerleave", () => {
        if (!this.activeGesture) this._showBrush(false);
      });
      global.addEventListener("keydown", event => this._onKeyDown(event), true);
      global.addEventListener("resize", () => this.renderOverlays());
      global.addEventListener("implexity:model-updated", () => {this._modelEventRevision++;this._refreshAuthoritativeField().catch(error=>this._setInspectorStatus(friendlyErrorMessage(error,"The editable field could not be refreshed."),"error"));this.renderOverlays();});
      global.addEventListener("implexity:engineering-problem-updated", event => {
        if (event.detail && event.detail.glyphs) this.setGlyphs(event.detail.glyphs);
      });
      global.addEventListener("implexity:interaction-preview-applied", event => this._applyPreviewResponse(event.detail.response));
      global.addEventListener("implexity:interaction-preview-error", event =>
        this._showError(event.detail.error, "The live preview could not be updated. Your model has not been changed."));
      global.addEventListener("implexity:selection-updated",event=>{this.selection=event.detail?.selection||null;this._renderSelectionInspector();this.renderSelection();});
      global.addEventListener("implexity:manual-history-changed",()=>this._renderSelectionInspector());
      this.glyphLayer.addEventListener("pointerdown", event => this._onGlyphPointerDown(event));
      this.cageLayer.addEventListener("pointerdown", event => this._onControlPointerDown(event));
      this.cageLayer.addEventListener("dblclick", async event => {
        const control=event.target.closest?.("[data-cage-id]");if(!control)return;
        event.preventDefault();
        try{await this.backend.promoteCage(control.dataset.cageId);this.toast("Deformation cage released for shape optimisation.","success");dispatch("implexity:interaction-committed",{kind:"shape_coordinate_promoted",cageId:control.dataset.cageId});}
        catch(error){this._showError(error,"The deformation cage could not be released for shape optimisation.","warning");}
      });
    }

    _setSelectionInspectorCollapsed(collapsed) {
      if(!this.inspector)return;
      this.inspector.dataset.collapsed=String(collapsed);
      const button=this.inspector.querySelector('[data-selection-collapse]');
      button.textContent=collapsed?'+':'−';button.setAttribute('aria-expanded',String(!collapsed));
      button.setAttribute('aria-label',collapsed?'Expand selection inspector':'Collapse selection inspector');
      this.inspector.querySelector('.implexity-inspector-body').hidden=collapsed;
    }

    _setInspectorStatus(message, level="info") {
      if(!this.inspector)return;
      const status=this.inspector.querySelector("[data-inspector-status]");
      status.hidden=false;status.dataset.level=level;
      status.querySelector("span").textContent=String(message||"");
    }

    async _refreshCommittedGeometry() {
       
       
       
      const viewer=global.ImplexityViewerAdapter;
      await viewer?.clearGeometryPreview?.();
      if(typeof viewer?.refreshAuthoritativeModel!=="function")
        throw new Error("Authoritative viewport refresh is unavailable. Reload the model before editing again.");
      const oldAnchor=this.sculptEnhancements.anchor;
      await viewer.refreshAuthoritativeModel();
       
       
      if(this._fieldRefresh)await this._fieldRefresh;
      await this._refreshAuthoritativeField();
      this.sculptEnhancements.anchor=null;
       
       
      if(oldAnchor&&typeof viewer.project==="function"&&typeof viewer.pickSurface==="function"){
        try{
          const p=viewer.project(oldAnchor.point_mm);
          const raw=p&&await viewer.pickSurface(p[0],p[1]);
          if(raw){const rect=viewer.canvas.getBoundingClientRect(),event={clientX:rect.left+p[0],clientY:rect.top+p[1]};this.sculptEnhancements.anchor=await this.picking.refine(this.picking.normalise(raw,event),event);}
        }catch(_){   }
      }
      this.sculptEnhancements.render();
      manualHistory.synchronized();
    }

    async _refreshInitialField() {
       
       
      const revision=this._modelEventRevision;
      try {
        const response=await fetch("/v1/implicit/model", {credentials:"same-origin"});
        const model=await response.json();
         
         
        if(revision!==this._modelEventRevision)return null;
        if(!response.ok || !model || typeof model.loaded!=="boolean")
          throw new Error("Could not inspect the current model before loading editing controls.");
        if(!model.loaded){
          this._setInspectorStatus("Create or load a model to begin editing.","info");
          return null;
        }
        return await this._refreshAuthoritativeField(model);
      } catch(error) {
        if(revision!==this._modelEventRevision)return null;
        throw error;
      }
    }

     
     
    static _declaresTopologyField(model) {
      const topology=model?.document?.meta?.implexity?.topology;
      return Boolean(topology&&typeof topology==="object"&&[topology.array_key,topology.field_id,topology.ref].some(value=>typeof value==="string"&&value.trim()));
    }

    async _refreshAuthoritativeField(knownModel=null) {
      if(this._fieldRefresh)return await this._fieldRefresh;
      this._fieldRefresh=(async()=>{
        if(!this._nativeComponentId){
          const model=knownModel||await (await fetch("/v1/implicit/model",{credentials:"same-origin",cache:"no-store"})).json();
          if(model?.loaded!==true){this._setInspectorStatus("Create or load a model to begin editing.","info");return null;}
          if(!this.constructor._declaresTopologyField(model)){
            this._setInspectorStatus("This model has no editable topology field yet. Bake or hand off its geometry to edit it spatially.","info");
            return null;
          }
        }
        const response=await this.backend.field(this._nativeComponentId?{field_id:this._nativeComponentId}:{});
        if(response.selection_required){
          if(this.mode===MODES.SCULPT){
            const targets=(response.components||[]).filter(c=>c.field_id.endsWith("occupancy"));
            const targetSelect=this.sculptPanel.querySelector("[data-sculpt-volume]");
            targetSelect.replaceChildren(...targets.map(c=>new Option(c.label.replace(/occupancy/i,"volume"),c.field_id)));
            this.sculptPanel.querySelector("[data-sculpt-volume-wrap]").hidden=targets.length<2;
          }
          let selector=this.inspector.querySelector('[data-native-component]');
          if(!selector){
            selector=document.createElement('select');selector.dataset.nativeComponent='';
            selector.setAttribute('aria-label','Native geometry control component');
            this.inspector.querySelector('.implexity-inspector-body').prepend(selector);
            selector.addEventListener('change',async()=>{
              if(!selector.value||this.activeGesture)return;
              this._nativeComponentId=selector.value;
              try{await this._refreshAuthoritativeField();}
              catch(error){this._setInspectorStatus(String(error),'error');}
            });
          }
          selector.replaceChildren(new Option('Choose geometry control component',''));
          for(const item of response.components||[])selector.add(new Option(`${item.label} (${item.units})`,item.field_id));
          this._setSelectionInspectorCollapsed(false);
          this._setInspectorStatus('Choose a native geometry component before editing. No geometry conversion is needed.');
          if(this.mode===MODES.SCULPT && !this._nativeComponentId && response.components?.length){
            const target=response.components.find(c=>c.field_id.endsWith("occupancy"))||response.components[0];
            this._nativeComponentId=target.field_id;selector.value=target.field_id;
            const selected=await this.backend.field({field_id:target.field_id});
            selected.authoritative=true;spatialFields.register(String(selected.field_id),selected);spatialFields.setActive(String(selected.field_id));
            this._setInspectorStatus('Native spatial controls loaded. The existing representation is ready to edit.','ok');
            this.sculptEnhancements?.hydrate(selected);this._configureSculptOptions();return selected;
          }
          return response;
        }
        response.authoritative=true;


        spatialFields.register(String(response.field_id),response);
        spatialFields.setActive(String(response.field_id));
        if(this.mode===MODES.SCULPT){this.sculptEnhancements?.hydrate(response);this._configureSculptOptions();}
        spatialSelections.hydrate(response.selections||[],response.active_selection_id||null);
        this.selection=spatialSelections.active();
        this._renderSelectionInspector();this.renderSelection();
        const stale=(response.stale_selection_ids||[]).length,invalid=(response.invalid_selection_ids||[]).length,totalExcluded=stale+invalid;
        const sampleKind=response.grid?.centering==='node'?'control nodes':'cells';
        this._setInspectorStatus(totalExcluded
          ? `Authoritative field loaded · ${response.shape.join(" × ")} ${sampleKind} · ${totalExcluded} stale or invalid selection${totalExcluded===1?"":"s"} kept out of use`
          : `Authoritative field loaded · ${response.shape.join(" × ")} ${sampleKind}`,totalExcluded?"warning":"ok");
        return response;
      })();
      try{return await this._fieldRefresh;}finally{this._fieldRefresh=null;}
    }

    _renderSelectionInspector() {
      if(!this.inspector)return;
      const field=spatialFields.activeFieldPayload(),selection=this.selection||spatialSelections.active();
      if(!field&&!this.inspectorExpansionExplicit)this._setSelectionInspectorCollapsed(true);
      this.inspector.querySelector("[data-selection-field]").textContent=field?`${displayIdentifier(field.field_id||spatialFields.activeFieldId())} · ${field.shape.join(" × ")}`:"No editable field";
      const counts=selection?.counts||{};
      this.inspector.querySelector("[data-count-selected]").textContent=String(counts.selected||0);
      this.inspector.querySelector("[data-count-affected]").textContent=String(counts.affected||0);
      this.inspector.querySelector("[data-count-protected]").textContent=String(counts.protected_total??counts.protected??0);
      const reasons=this.inspector.querySelector("[data-protected-reasons]");
      const byReason=selection?.protected_by_reason||field?.protected_masks||{};
      const rows=Object.entries(byReason).map(([name,value])=>({name,count:Number(value?.count??(Array.isArray(value)?value.filter(Boolean).length:0))}));
      reasons.replaceChildren(...(rows.length?rows.map(row=>{const item=createElement("span","implexity-mask-reason");item.append(createElement("b","",displayIdentifier(row.name)),document.createTextNode(` ${row.count} cells`));return item;}):[document.createTextNode("No protected masks")]));
      const list=this.inspector.querySelector("[data-region-list]"),active=spatialSelections.activeId();
      list.replaceChildren();
      const current=document.createElement("option");current.value="";current.textContent="Current selection";list.appendChild(current);
      for(const item of spatialSelections.all()){const option=document.createElement("option");option.value=item.id;option.textContent=`${item.region_name||item.id} · ${item.counts?.selected||0} cells`;option.selected=item.id===active;list.appendChild(option);}
      const history=manualHistory.counts();
      this.inspector.querySelector("[data-selection-undo]").disabled=history.busy||history.undo<=0;
      this.inspector.querySelector("[data-selection-redo]").disabled=history.busy||history.redo<=0;
    }

    renderSelection() {
      if(!this.selectionLayer)return;
      this.selectionLayer.replaceChildren();
      const selection=this.selection||spatialSelections.active();
      if(!selection||!selection.grid||!selection.shape)return;
      const groups=[
        [selection.selected_runs||[],"implexity-selected-cell"],
        [selection.protected_runs||[],"implexity-protected-cell"]
      ];
      for(const [runs,className] of groups){
        const indices=decodeRuns(runs,selection.shape),stride=Math.max(1,Math.ceil(indices.length/1600));
        for(let pos=0;pos<indices.length;pos+=stride){
          const world=registeredCellWorld(selection.grid,selection.shape,indices[pos]);
          const screen=this.picking.project(world);if(!screen)continue;
          this.selectionLayer.appendChild(createSvgElement("circle",className,{cx:screen[0],cy:screen[1],r:className.includes("protected")?3.2:2.5}));
        }
      }
    }

    _renderSelectionMarquee(gesture = this.activeGesture) {
      if(!gesture||gesture.kind!=="spatial_selection"||!["box","lasso"].includes(gesture.selectionKind))return;
      const points=gesture.screenPoints||[];
      if(gesture.selectionKind==="box"&&points.length>=2){
        const lo=[Math.min(points[0][0],points.at(-1)[0]),Math.min(points[0][1],points.at(-1)[1])];
        const hi=[Math.max(points[0][0],points.at(-1)[0]),Math.max(points[0][1],points.at(-1)[1])];
        this.selectionLayer.appendChild(createSvgElement("rect","implexity-selection-marquee",{x:lo[0],y:lo[1],width:hi[0]-lo[0],height:hi[1]-lo[1]}));
      }else if(points.length>=2){
        this.selectionLayer.appendChild(createSvgElement("polyline","implexity-selection-marquee",{points:points.map(point=>point.join(",")).join(" ")}));
      }
    }

    async _requestHistory(action) {
      if(this.activeGesture||this.pendingGesture||this.selectionOperationPending||manualHistory.counts().busy){
        this._setInspectorStatus("Finish or cancel the current edit before undo or redo.","warning");return;
      }
      let restored=false;
      try{const entry=await manualHistory.request(action);restored=true;this._setInspectorStatus(`${displayIdentifier(action)}: ${entry.label}`,"ok");}
      catch(error){this._showError(error,(restored||error?.historyApplied)?"The history state was restored, but its viewport refresh failed. Reload the model before continuing.":`The manual edit could not be ${action === "undo" ? "undone" : "redone"}.`);}
    }

    async _historyLocal(action, payload = {}) {
      const result=action==="undo"?await this.backend.undo(payload):await this.backend.redo(payload);
      if(!result||result.action!==action)throw new Error("The viewport history action was not acknowledged by its owning runtime.");
       
      try{await this._refreshCommittedGeometry();}
      catch(error){error.historyApplied=true;error.history=result.history;throw error;}
      dispatch("implexity:model-updated",{source:`manual-${action}`});
      return result;
    }

    _numericVector(attribute) {
      const raw=[0,1,2].map(axis=>this.inspector.querySelector(`[${attribute}="${axis}"]`).value.trim());
      if(raw.some(value=>value===""))throw new Error("Complete all three numeric coordinates first");
      const values=raw.map(Number);
      if(values.some(value=>!Number.isFinite(value)))throw new Error("Complete all three numeric coordinates first");
      return values;
    }

    async _applyNumericSelection(kind) {
      try{
        let selector;
        if(kind==="point")selector={kind:"click",point_mm:this._numericVector("data-world-point"),surface_only:false,visibility:"through"};
        else if(kind==="world")selector={kind:"bounds",world_bounds_mm:[this._numericVector("data-world-min"),this._numericVector("data-world-max")],surface_only:false,visibility:"through"};
        else selector={kind:"bounds",index_bounds:[this._numericVector("data-index-min"),this._numericVector("data-index-max")],surface_only:false,visibility:"through"};
        await this._applySelectionOperation(selector);
      }catch(error){this._showError(error,"The numeric selection is incomplete or outside the registered field.");}
    }

    async _applySelectionOperation(selector, operation = null) {
      if(this.activeGesture||this.pendingGesture||this.selectionOperationPending||manualHistory.counts().busy)throw new Error("Finish the current edit first");
      const field=spatialFields.activeFieldPayload();
      if(!field)throw new Error("Select or create an editable topology field first");
      const selectionBefore=deepClone(spatialSelections.active());
      const selectedOperation=operation||this.selectionOperation.value;
      const current=selectionBefore?.field_id===spatialFields.activeFieldId()&&["registration_id","payload_sha256"].every(key=>selectionBefore?.field_identity?.[key]===field.identity?.[key]);
      const fresh=selectedOperation==="replace"&&!current;
      const request={kind:"spatial_selection",field_id:spatialFields.activeFieldId(),field_identity:field.identity,selection:fresh?null:selectionBefore,selection_id:fresh?`selection_${global.crypto.randomUUID()}`:spatialSelections.activeId(),save_region:selector.surface_only!==false,region_name:this.regionName.value};
      this.selectionOperationPending=true;
      try{
        this.previewQueue.reset();await this.backend.begin(request);
        const sequence=this.previewQueue.submit({sequence:1,operation:{operation:selectedOperation,selection_revision:fresh?0:selectionBefore?.revision??0,selector}});
        await this.previewQueue.drain(sequence);
        const result=await this.backend.commit({final_sequence:sequence});
        if(!result?.selection)throw new Error("The service returned no authoritative selection");
        const selection=Object.assign({},result.selection,{region_name:result?.entities?.region?.name||result.selection.region_name||this.regionName.value.trim()||"Current selection"});
        spatialSelections.upsert(selection);this.selection=selection;
        manualHistory.record("viewport",result.history?.label||"Selection and region");
        this._renderSelectionInspector();this.renderSelection();this._setInspectorStatus(`${result.selection.counts.selected} cells selected`,"ok");
      }catch(error){try{await this.backend.cancel();}catch(_){ }this.selection=selectionBefore;try{await this._refreshAuthoritativeField();}catch(_){this._renderSelectionInspector();this.renderSelection();}this._setInspectorStatus("Selection was not saved. The last saved selection remains active.","error");throw error;}
      finally{this.selectionOperationPending=false;}
    }

    async _onContextMenu(event) {
      if (event.target.closest(".implexity-toolbar,.implexity-options,.implexity-context-menu")) return;
      event.preventDefault();
      const hit = await this.picking.pick(event, { requireExact: true });
      if (!hit) {
        this._hideContextMenu();
        return;
      }
      this.contextHit = hit;
      const rect = this.viewport.getBoundingClientRect();
      this.contextMenu.style.left = `${Math.min(rect.width - 205, Math.max(4, event.clientX - rect.left))}px`;
      this.contextMenu.style.top = `${Math.min(rect.height - 290, Math.max(4, event.clientY - rect.top))}px`;
      this.contextMenu.dataset.active = "true";
    }

    _hideContextMenu() {
      this.contextMenu.dataset.active = "false";
      this.contextHit = null;
    }

    async _placeAtHit(mode, hit) {
      const patch = this._patchFromHit(hit);
      const glyphModes = [MODES.PRESSURE, MODES.TRACTION, MODES.HEAT, MODES.TEMPERATURE, MODES.CLAMP];
      const request = glyphModes.includes(mode)
        ? { kind: "glyph", glyph: this._glyphFromHit(hit, mode), patch, hit, mode }
        : { kind: "surface_patch", patch, hit, mode };
      try {
        const transaction = await this.backend.begin(request);
        const result = await this.backend.commit({ final_sequence: 0 });
        if(request.kind==="surface_patch")this._reportSavedRegion(result);
        const encoded = result?.entities?.glyph || result?.entities?.load?.glyph || result?.entities?.boundary_condition?.glyph;
        if (encoded && result?.entity_ids?.glyph_id === encoded.id) {
          this.glyphs.set(encoded.id, encoded);
          this.renderGlyphs();
        }
        if(result?.history?.label)manualHistory.record("viewport",result.history.label);
        dispatch("implexity:interaction-committed", { mode, transaction, result, directPlacement: true });
      } catch (error) {
        this._showError(error, "The engineering object could not be created. Your model has not been changed.");
        try { await this.backend.cancel(); } catch (_) {                            }
      }
    }

    async prepareVolumeSelection(fieldId, name = "Optimisable volume") {
      if(this.activeGesture||this.pendingGesture||this.selectionOperationPending||manualHistory.counts().busy)throw new Error("Finish the current edit first.");
      if(this._fieldRefresh)await this._fieldRefresh;
      if(!spatialFields.field(fieldId)){
        const response=await this.backend.field({field_id:fieldId});
        spatialFields.replaceAuthoritative(response);
        spatialSelections.hydrate(response.selections||[],response.active_selection_id||null);
        this.selection=spatialSelections.active();
      }
      const field=spatialFields.field(fieldId);
      if(!field||field.authoritative!==true)throw new Error("This coordinate needs a registered editable topology field. Create or load the topology field before selecting its volume.");
      spatialFields.setActive(fieldId);
      this.selectionVisibility.value="through";
      this.inspector.querySelector("[data-selection-volume]").checked=true;
      this.selectionOperation.value="replace";
      this.regionName.value=name;
      this.setMode(MODES.SELECT_BOX);
      this._renderSelectionInspector();
      this._setInspectorStatus("Drag a box through the model to save interior cells, or use numeric bounds for a precise 3-D volume. Then return to Optimisation scope and choose the saved volume. Protected cells stay fixed.","ok");
      return true;
    }

    setMode(mode) {
      if (!Object.values(MODES).includes(mode)||this.selectionOperationPending) return;
      if (this.activeGesture) this.cancel();
      if (this.pendingGesture) this.cancel();
      this.mode = mode;
      if([MODES.SELECT_BRUSH,MODES.SELECT_BOX,MODES.SELECT_LASSO,MODES.SELECT_FLOOD,MODES.REGION,MODES.SCULPT,MODES.BRUSH,MODES.CONTROL,MODES.CAGE].includes(mode)){
        this.inspectorExpansionExplicit=true;this._setSelectionInspectorCollapsed(false);
      }
      const directEnabled = [MODES.MOVE, MODES.SIZE, MODES.SURFACE].includes(mode);
      const direct = global.ImplexityDirectInteraction;
      if (direct && typeof direct.setEnabled === "function") direct.setEnabled(directEnabled);
      for (const [key, button] of this.buttons) button.setAttribute("aria-pressed", String(key === mode));
      const activeTool=this.inspector?.querySelector("[data-active-tool]");
      const toolHelp=this.inspector?.querySelector("[data-tool-help]");
      if(activeTool)activeTool.textContent=modeLabel(mode);
      if(toolHelp)toolHelp.textContent=TOOL_HELP[mode]||"Use the viewport to author this object; release commits one exact transaction.";
      this._configureOptions(mode);
      this.sculptPanel.hidden = mode !== MODES.SCULPT;
       
      this.inspector.classList.toggle("implexity-sculpt-inspector",mode===MODES.SCULPT);
      if(mode===MODES.SCULPT){this._nativeComponentId=null;this._configureSculptOptions();this._refreshAuthoritativeField().catch(error=>this._showError(error,"Create an editable field or controlled volume before sculpting."));}
      this._showBrush(false);
      dispatch("implexity:interaction-mode", { mode, directEnabled, label: modeLabel(mode), help: TOOL_HELP[mode] || "" });
    }

    _configureSculptOptions() {
      if(!this.sculptControls)return;
      const c=this.sculptControls,tool=c.tool.value,native=String(this._activeFieldId()||"").includes("::component::");
      for(const option of c.tool.options)option.disabled=native&&["add","flatten"].includes(option.value);
      if(c.tool.selectedOptions[0]?.disabled)c.tool.value="grab";
      this.sculptPanel.querySelector("[data-sculpt-custom]").hidden=c.direction.value!=="custom";
      this.sculptPanel.querySelector("[data-sculpt-depth]").hidden=c.scope.value!=="local";
      this.sculptPanel.querySelector("[data-sculpt-box]").hidden=c.scope.value!=="box";
      this.sculptPanel.querySelector("[data-sculpt-amount]").hidden=!["inflate","deflate"].includes(c.tool.value);
      this.sculptPanel.querySelector("[data-sculpt-strength]").hidden=!["smooth","add","subtract","flatten"].includes(c.tool.value);
      this.sculptPanel.querySelector("[data-sculpt-protection]").hidden=!["protect","release"].includes(c.tool.value);
      this.sculptPanel.querySelector("[data-sculpt-phase-hold]").hidden=!native||!["protect","release"].includes(c.tool.value);
      c.direction.disabled=!["grab","stretch","twist","flatten"].includes(c.tool.value);
      this.sculptPanel.querySelector("[data-sculpt-target]").textContent=native?"Native volume: the same twenty control fields. Warp is projected onto the control grid. Phase controls can influence shape outside a selection. Use Protect → Sampled shape where geometry must stay fixed. Add and Flatten require an occupancy field.":"Occupancy field: edits remain live topology variables. The existing domain and grid resolution stay fixed.";
      const help={protect:"Freeze the sampled occupancy in rendering and physics, with a one-cell support halo. Assemblies are represented on the declared geometry grid. Control locks alone do not fix a native surface. Release uses the same Maintain selection.",release:"Paint the region to release. Choose the same Maintain mode used when protecting it.",twist:"Drag horizontally to rotate around the selected direction. The influence boundary remains stationary.",stretch:"Drag along the selected direction to stretch. For a direction facing the camera, drag vertically.",grab:"Drag a picked point. Choose Screen plane, a model axis or the surface normal. A camera-facing direction uses vertical dragging."};
      this.sculptPanel.querySelector("[data-sculpt-help]").textContent=help[c.tool.value]||"One stroke follows the initial screen plane. Depth limits its reach through the model. Release commits; Escape cancels. Slow motion does not multiply brush strength.";
      this.sculptEnhancements?.configure();
    }

    _sculptSettings(hit) {
      const c=this.sculptControls;
      const parse=(text,name)=>{const a=text.split(/[,\s]+/).filter(Boolean).map(Number);if(a.length!==3||!a.every(Number.isFinite))throw new Error(`${name} needs three finite numbers.`);return a;};
      const directional=["grab","stretch","twist","bend","taper","flatten"].includes(c.tool.value);
      let axis=!directional?hit.normal.slice():c.direction.value==="custom"?parse(c.custom.value,"Custom direction"):["x","y","z"].includes(c.direction.value)?[0,1,2].map(i=>i==="xyz".indexOf(c.direction.value)?1:0):hit.normal.slice();
      const length=len3(axis);if(length<1e-12)throw new Error("Choose a nonzero direction vector.");axis=scale3(axis,1/length);
      const settings={tool:c.tool.value,center_mm:hit.point_mm.slice(),normal:hit.normal.slice(),direction:axis,
        radius_mm:finiteNumber(this.radiusInput.value,DEFAULTS.radiusMm),depth_mm:Number(c.depth.value),scope:c.scope.value,
        amount_mm:Number(c.amount.value),strength:Number(c.strength.value),symmetry_axes:c.symmetry.value?c.symmetry.value.split(","):[],
        carry_phase:c.carryPhase.checked,protect_phase:c.protectPhase.checked,protection:c.protection.value,
        _directionMode:c.direction.value};
      if(settings.scope==="box")settings.bounds_mm={min_mm:parse(c.boxMin.value,"Box minimum"),max_mm:parse(c.boxMax.value,"Box maximum")};
      Object.assign(settings,this.sculptEnhancements?.settings(hit)||{});
      return settings;
    }

    _configureOptions(mode) {
      const profile = optionProfile(mode);
      this.optionsPanel.hidden = !profile;
      if (!profile) return;
      this.optionsTitle.textContent = profile.title;
      for (const [key, nodes] of Object.entries(this.optionControls)) {
        const spec = profile[key];
        const hidden = !spec;
        nodes.label.hidden = hidden;
        nodes.control.hidden = hidden;
        nodes.control.disabled = hidden;
        if (!spec) continue;
        const [label, unit] = spec;
        const visible = unit ? `${label} [${unit}]` : label;
        nodes.label.textContent = visible;
        nodes.control.setAttribute("aria-label", visible);
      }
    }

    _onKeyDown(event) {
      if(event.key==="Escape"&&this.sculptEnhancements?.exactReview){event.preventDefault();this.sculptEnhancements.cancelExact();return;}
      if(event.key==="Escape"&&this.sculptEnhancements?.pickMode){event.preventDefault();this.sculptEnhancements.pickMode=null;return;}
      if (event.key === "Escape" && this.contextMenu.dataset.active === "true") {
        event.preventDefault();
        this._hideContextMenu();
        return;
      }
      if (event.key === "Escape" && (this.activeGesture || this.pendingGesture)) {
        event.preventDefault();
        this.cancel();
        return;
      }
      if (isEditableTarget(event.target)) return;
      const targetsViewport = event.target === this.viewport ||
        (event.target && typeof this.viewport.contains === "function" && this.viewport.contains(event.target));
      const key=event.key.toLowerCase();
      if((event.ctrlKey||event.metaKey)&&targetsViewport&&!this.activeGesture&&!this.pendingGesture){
        if(!event.shiftKey&&key==="z"){event.preventDefault();this._requestHistory("undo");return;}
        if(key==="y"||(event.shiftKey&&key==="z")){event.preventDefault();this._requestHistory("redo");return;}
      }
      if(targetsViewport&&this.mode===MODES.SCULPT&&["[","]"].includes(key)&&!this.activeGesture&&!this.pendingGesture){
        event.preventDefault();this.radiusInput.value=String(Math.max(.001,Number(this.radiusInput.value)*(key==="]"?1.2:1/1.2)));this.sculptEnhancements?.render();return;
      }
      if(targetsViewport&&this.mode===MODES.SCULPT&&["x","y","z"].includes(key)&&!event.ctrlKey&&!event.metaKey&&!event.altKey&&!this.activeGesture&&!this.pendingGesture&&!this.selectionOperationPending&&!this.sculptControls.direction.disabled){
        event.preventDefault();event.stopImmediatePropagation();this.sculptControls.direction.value=key;this._configureSculptOptions();return;
      }
      const mode = MODE_KEYS[key];
      if (mode && targetsViewport) {
        event.preventDefault();
        this.setMode(mode);
      }
    }

    async _onPointerMove(event) {
      this._lastPointer = event;
      if (this.pendingGesture && event.pointerId === this.pendingGesture.pointerId) {
        this.pendingGesture.lastEvent = event;
        event.preventDefault();
        return;
      }
      if (this.activeGesture) {
        this._scheduleGesturePreview(event);
        return;
      }
      if(this.selectionOperationPending||event.target.closest?.("[data-sculpt-axis],.implexity-toolbar,.implexity-options,.implexity-selection-inspector,button,input,select,textarea")){this._showBrush(false);return;}
      if (this._pendingFrame) return;
      this._pendingFrame = requestAnimationFrame(async () => {
        this._pendingFrame = 0;
        const latest = this._lastPointer;
        try{
          const hit = await this.picking.pick(latest);
          if(latest!==this._lastPointer||this.activeGesture||this.pendingGesture||this.selectionOperationPending)return;
          this.hoverHit = hit;
          this._updateHover(latest, hit);
        }catch(_){if(latest===this._lastPointer)this._showBrush(false);}
      });
    }

    _updateHover(event, hit) {
      const brushLike = [MODES.SCULPT, MODES.SELECT_BRUSH, MODES.BRUSH, MODES.REGION, MODES.PRESSURE, MODES.TRACTION, MODES.HEAT, MODES.TEMPERATURE, MODES.CLAMP].includes(this.mode);
      this.viewport.dataset.pickQuality = hit ? (hit.committable ? "exact" : "preview") : "none";
      this._showBrush(Boolean(hit && brushLike), event);
      if (hit) {
        dispatch("implexity:surface-hover", { hit, mode: this.mode });
      }
    }

    _worldRadiusToPixels(radiusMm, event) {
      const candidates = [global.ImplexityViewer, global.implexityViewer, global.viewer, global.ImplexityViewport].filter(Boolean);
      for (const candidate of candidates) {
        for (const name of ["worldRadiusToPixels", "radiusToPixels", "worldLengthToPixels"]) {
          if (typeof candidate[name] === "function") {
            const value = Number(candidate[name](radiusMm, this.hoverHit && this.hoverHit.point_mm));
            if (Number.isFinite(value) && value > 0) return Math.min(400, value);
          }
        }
      }
      if(this.hoverHit && this.picking){
        const a=this.picking.project(this.hoverHit.point_mm);
        if(a){
          const lengths=[0,1,2].map(i=>{const p=this.hoverHit.point_mm.slice();p[i]+=radiusMm;const b=this.picking.project(p);return b?Math.hypot(b[0]-a[0],b[1]-a[1]):0;});
          const radius=Math.max(...lengths);if(radius>0)return Math.min(400,radius);
        }
      }
      return finiteNumber(this.options.screenRadiusPx, 42);
    }

    _modelBounds(hit) {
      const raw = hit && hit.raw;
      if (raw && raw.bounds_mm) return raw.bounds_mm;
      const candidates = [global.ImplexityViewer, global.implexityViewer, global.viewer, global.ImplexityViewport].filter(Boolean);
      for (const candidate of candidates) {
        for (const name of ["getModelBounds", "modelBounds", "getBounds", "bounds"]) {
          const member = candidate[name];
          const value = typeof member === "function" ? member.call(candidate) : member;
          if (value) {
            const minimum = value.min_mm || value.minimum || value.min || value[0];
            const maximum = value.max_mm || value.maximum || value.max || value[1];
            if (minimum && maximum) return { min_mm: vec3(minimum), max_mm: vec3(maximum) };
          }
        }
      }
      const center = hit ? hit.point_mm : [0, 0, 0];
      const half = Math.max(1, finiteNumber(this.radiusInput.value, DEFAULTS.radiusMm) * 2.5);
      return { min_mm: sub3(center, [half, half, half]), max_mm: add3(center, [half, half, half]) };
    }

    _showBrush(active, event) {
      this.brushRing.dataset.active = String(Boolean(active));
      if (!active || !event) return;
      const rect = this.viewport.getBoundingClientRect();
      const radiusPx = Math.max(7, this._worldRadiusToPixels(finiteNumber(this.radiusInput.value, DEFAULTS.radiusMm), event));
      this.brushRing.style.left = `${event.clientX - rect.left}px`;
      this.brushRing.style.top = `${event.clientY - rect.top}px`;
      this.brushRing.style.width = `${2 * radiusPx}px`;
      this.brushRing.style.height = `${2 * radiusPx}px`;
      this.brushRing.dataset.sign = (this.mode===MODES.SCULPT?["deflate","subtract","release"].includes(this.sculptControls.tool.value):this.brushModeInput.value==="subtract") ? "negative" : "positive";
    }

    async _onPointerDown(event) {
      if (event.button !== 0 || event.target.closest(".implexity-toolbar,.implexity-options,.implexity-selection-inspector,.implexity-context-menu,.implexity-toast-stack,button,input,select,textarea,a,[role=button]")) return;
      if(this._editingRecoveryRequired||this.selectionOperationPending||this.activeGesture||this.pendingGesture||manualHistory.counts().busy)return;
      try { this.viewport.focus({ preventScroll: true }); } catch (_) {                                             }
      if ([MODES.MOVE, MODES.SIZE, MODES.SURFACE].includes(this.mode)) {

        dispatch("implexity:direct-geometry-pointerdown", { event, mode: this.mode });
        return;
      }
      if (!pointerCoordinator.claim(this.pointerOwner, event.pointerId)) return;
      const pending = this._startPendingGesture(event);
      let hit = null;
      let cameraEvidence = null;
      let clipEvidence = null;
      const selectionMode = SELECTION_MODES.has(this.mode);
      const screenSelection = [MODES.SELECT_BOX, MODES.SELECT_LASSO].includes(this.mode);
      try {
        if (!screenSelection) {
          const override=this._sculptAnchorOverride;this._sculptAnchorOverride=null;
          hit=override?await this.picking.refine(override,event):await this.picking.pick(event,{requireExact:true});
        }
        if (selectionMode) {
          cameraEvidence = this.picking.cameraEvidence();
          clipEvidence = this.picking.clipEvidence(hit);
        }
      } catch (error) {
        this._finishPendingGesture(pending);
        this._showError(error, selectionMode
          ? "Exact field, camera, or section evidence is unavailable, so the selection was not started."
          : "This preview point could not be refined on the exact surface, so nothing was changed.");
        return;
      }
      if (this.pendingGesture !== pending || pending.cancelled) {
        this._finishPendingGesture(pending);
        return;
      }
      if (!hit && !screenSelection) {
        this._finishPendingGesture(pending);
        this.toast("This preview point could not be refined on the exact surface, so nothing was changed.", "warning");
        return;
      }
      if(this.mode===MODES.SCULPT&&hit){
        try{if(this.sculptEnhancements?.picked(hit)){this._finishPendingGesture(pending);return;}}
        catch(error){this._finishPendingGesture(pending);this._showError(error,"The point could not be used.");return;}
      }
      event.preventDefault();
      event.stopPropagation();
      try { this.viewport.setPointerCapture(event.pointerId); } catch (_) {                                 }
      const kind = selectionMode ? "spatial_selection" :
        ([MODES.PRESSURE, MODES.TRACTION, MODES.HEAT, MODES.TEMPERATURE, MODES.CLAMP].includes(this.mode) ? "glyph" : this.mode);
      const patch = hit ? this._patchFromHit(hit) : null;
      const request = { kind, patch, hit, mode: this.mode };
      const selectionAlgebra = event.shiftKey ? "add" : event.altKey ? "subtract" :
        (event.ctrlKey || event.metaKey) ? "intersect" : this.selectionOperation.value;
      const selectionBefore = deepClone(spatialSelections.active());
      let freshSelection = false;
      if (kind === "glyph") request.glyph = this._glyphFromHit(hit, this.mode);
      if (kind === "spatial_selection") {
        const field = this._activeFieldPayload();
        request.field_id = this._activeFieldId();
        request.field_identity = field?.identity;
        const current = selectionBefore?.field_id === request.field_id &&
          ["registration_id", "payload_sha256"].every(key => selectionBefore?.field_identity?.[key] === field?.identity?.[key]);
        freshSelection = selectionAlgebra === "replace" && !current;
        request.selection = freshSelection ? null : selectionBefore;
        request.selection_id = freshSelection ? `selection_${global.crypto.randomUUID()}` : spatialSelections.activeId();
        request.save_region = !this.inspector.querySelector("[data-selection-volume]").checked;
        request.region_name = this.regionName.value.trim() || "Current selection";
        if (!field || !request.field_id || !request.field_identity) {
          this.toast("No authoritative editable topology field is available for selection.", "warning");
          this._finishPendingGesture(pending);
          return;
        }
      }
      if (kind === "field_brush" || kind === "geometry_sculpt") {
        request.preview_geometry = kind === "geometry_sculpt";
        request.field_id = this._activeFieldId();
        const field=this._activeFieldPayload();
        request.field_identity=field?.identity;
        if (!field || !request.field_id) {
          this.toast("Select a spatial design field before using the brush.", "warning");
          this._finishPendingGesture(pending);
          return;
        }
      }
      if (kind === "deformation_cage" || kind === "control_lattice") {
        request.bounds_mm = this._modelBounds(hit);
        request.shape = this.options.cageShape;
        request.field_id = this._activeFieldId();
        const field=this._activeFieldPayload();
        request.field_identity=field?.identity;
        request.target = { field_id: request.field_id, node_id: hit.node_id, selector: hit.selector };
        if (!field || !request.field_id) {
          this.toast("Select a spatial design field before creating control points or a deformation cage.", "warning");
          this._finishPendingGesture(pending);
          return;
        }
      }
      try {
        this.previewQueue.reset();
        pending.backendStarted = true;
        const tx = await this.backend.begin(request);
        const startScreen = this.picking.coordinates(event).css;
        const gesture = {
          pointerId: event.pointerId,
          mode: this.mode,
          kind,
          hit,
          startClient: [event.clientX, event.clientY],
          startScreen,
          startWorld: hit ? hit.point_mm.slice() : [0, 0, 0],
          latestWorld: hit ? hit.point_mm.slice() : [0, 0, 0],
          samples: [],
          selectionKind: selectionMode ? SELECTION_KIND[this.mode] : null,
          selectionAlgebra,
          selectionSettings: selectionMode ? {visibility:this.selectionVisibility.value,
            snap:this.selectionSnap.value,connected:this.selectionConnected.checked,volume:this.inspector.querySelector("[data-selection-volume]").checked} : null,
          selectionRevision: freshSelection ? 0 : selectionBefore?.revision ?? 0,
          selectionBefore,
          screenPoints: selectionMode ? [startScreen.slice()] : [],
          selectionPoints: selectionMode && hit ? [hit.point_mm.slice()] : [],
          cameraEvidence,
          clipEvidence,
          sculpt: kind === "geometry_sculpt" ? this._sculptSettings(hit) : null,
          sculptPoints: kind === "geometry_sculpt" ? [hit.point_mm.slice()] : [],
          regionPatches: kind === "surface_patch" ? [patch] : [],
          transaction: tx,
          sequence: 0,
          buildEpoch: 0,
          finalizing: false
        };
        await this._activatePendingGesture(pending, gesture);
        if (this.activeGesture === gesture) {
          if (kind === "field_brush") this._appendBrushSample(hit.point_mm);
          const detail = kind === "spatial_selection"
            ? (gesture.selectionKind === "click" || gesture.selectionKind === "flood" ? "Release to apply exact cells" : "Drag to define cells; release to commit")
            : "Drag to preview; release to commit";
          this._setHud(pending.lastEvent || event, modeLabel(this.mode), detail);
        }
      } catch (error) {
        if (pending.backendStarted) { try { await this.backend.cancel(); } catch (_) {                          } }
        this._finishPendingGesture(pending);
        this._showError(error, "The interaction could not start. Try again or refresh the model.");
      }
    }

    _startPendingGesture(event) {
      const pending = {
        pointerId: event.pointerId, downEvent: event, lastEvent: event,
        releaseEvent: null, released: false, cancelled: false, backendStarted: false
      };
      this.pendingGesture = pending;
      try { this.viewport.setPointerCapture(event.pointerId); } catch (_) {                                 }
      return pending;
    }

    _finishPendingGesture(pending) {
      if (!pending) return;
      if (this.pendingGesture === pending) this.pendingGesture = null;
      try {
        if (this.viewport.hasPointerCapture?.(pending.pointerId)) this.viewport.releasePointerCapture(pending.pointerId);
      } catch (_) {                                                 }
      pointerCoordinator.release(this.pointerOwner, pending.pointerId);
    }

    async _activatePendingGesture(pending, gesture) {
      if (this.pendingGesture !== pending) {
        try { await this.backend.cancel(); } catch (_) {                                           }
        return false;
      }
      this.pendingGesture = null;
      this.activeGesture = gesture;
      this.viewport.classList.add("implexity-viewport-busy");
      if (pending.cancelled) {
        await this.cancel();
        return false;
      }
      if (pending.released) {
        await this._onPointerUp(pending.releaseEvent || pending.lastEvent || pending.downEvent);
        return false;
      }
      return true;
    }

    _scheduleGesturePreview(event) {
      const gesture = this.activeGesture;
      if (!gesture || gesture.finalizing || gesture.cancelling || event.pointerId !== gesture.pointerId) return;
      this._lastPointer = event;
      if (gesture.frame) return;
      const epoch = ++gesture.buildEpoch;
      gesture.frame = requestAnimationFrame(() => {
        gesture.frame = 0;
        const latest = this._lastPointer;
        this._submitGesturePreview(latest, false, epoch).catch(error =>
          this._showError(error, "The live preview could not be updated. Your model has not been changed."));
      });
    }

    async _submitGesturePreview(event, final = false, epoch = null) {
      const gesture = this.activeGesture;
      if (!gesture || event.pointerId !== gesture.pointerId) return null;
      const token = epoch == null ? ++gesture.buildEpoch : epoch;
      let liveHit = null;
      const selectionNeedsHit = gesture.kind === "spatial_selection" && ["click", "brush", "flood"].includes(gesture.selectionKind);
      if (["field_brush", "glyph", "surface_patch"].includes(gesture.kind) || selectionNeedsHit) {
        liveHit = await this.picking.pick(event, { requireExact: true });
      }
      if (token !== gesture.buildEpoch || this.activeGesture !== gesture) return null;
      const movedPixels = Math.hypot(event.clientX - gesture.startClient[0], event.clientY - gesture.startClient[1]);
      if (!liveHit && (["field_brush", "glyph", "surface_patch"].includes(gesture.kind) || selectionNeedsHit)) {
        if (movedPixels > 0.5) throw new Error("The final cursor point could not be refined on the exact implicit surface");
        liveHit = gesture.hit;
      }
      let world = gesture.latestWorld;
      if (gesture.kind !== "spatial_selection" || selectionNeedsHit) {
        world = liveHit ? liveHit.point_mm : await this.picking.worldAt(event, gesture.startWorld);
        if (!world || token !== gesture.buildEpoch || this.activeGesture !== gesture) return null;
      }
      gesture.latestWorld = world;
      const delta = sub3(world, gesture.startWorld);
      if (gesture.kind === "field_brush") this._appendBrushSample(world);
      if (gesture.kind === "geometry_sculpt" && !["grab","stretch","twist","scale","bend","taper"].includes(gesture.sculpt.tool)) {
        const points=gesture.sculptPoints;
        if(len3(sub3(points.at(-1),world))>Math.max(1e-8,gesture.sculpt.radius_mm*.05)){
          if(points.length>=512)throw new Error("This stroke has reached 512 points. Release and begin another stroke.");
          points.push(world.slice());
        }
      }
      if (gesture.kind === "surface_patch" && liveHit) this._appendRegionPatch(liveHit);
      if (gesture.kind === "spatial_selection") {
        if(final){
          const liveCamera=this.picking.cameraEvidence(),liveClip=this.picking.clipEvidence(liveHit);
          if(JSON.stringify(liveCamera)!==JSON.stringify(gesture.cameraEvidence)||JSON.stringify(liveClip)!==JSON.stringify(gesture.clipEvidence)){
            throw new Error("The camera or section plane changed during selection; start the gesture again");
          }
        }
        this._appendSelectionGesturePoint(event, liveHit);
        if (gesture.selectionKind === "lasso" && gesture.screenPoints.length < 3) {
          if (final) throw new Error("Draw at least three lasso points before release");
          this._setHud(event, modeLabel(this.mode), "Keep drawing the lasso; release to apply");
          return null;
        }
        this._renderSelectionMarquee(gesture);
      }
      const operation = this._operationForGesture(gesture, delta, event, liveHit);
      const sequence = ++gesture.sequence;
      this.previewQueue.submit({ transaction_id: gesture.transaction.transaction_id || gesture.transaction.id, sequence, operation, final });
      this._setHud(event, modeLabel(this.mode), this._hudDetail(gesture, delta));
      return sequence;
    }

    _operationForGesture(gesture, delta, event, liveHit = null) {
      const snap = finiteNumber(this.snapInput.value, 0);
      if (gesture.kind === "spatial_selection") {
        const selector = {
          kind: gesture.selectionKind,
          visibility: gesture.selectionSettings.visibility,
          snap: gesture.selectionSettings.snap,
          connected: gesture.selectionKind === "flood" || gesture.selectionSettings.connected,
          surface_only: !gesture.selectionSettings.volume,
          camera: deepClone(gesture.cameraEvidence),
          clip_evidence: deepClone(gesture.clipEvidence)
        };
        if (["click", "flood"].includes(gesture.selectionKind)) selector.point_mm = (liveHit || gesture.hit).point_mm.slice();
        else if (gesture.selectionKind === "brush") {
          selector.points_mm = gesture.selectionPoints.map(point => point.slice());
          selector.radius_mm = finiteNumber(this.radiusInput.value, DEFAULTS.radiusMm);
        } else if (gesture.selectionKind === "box") {
          selector.screen_bounds_px = [gesture.startScreen.slice(), this.picking.coordinates(event).css];
        } else if (gesture.selectionKind === "lasso") {
          selector.screen_points_px = gesture.screenPoints.map(point => point.slice());
        }
        return {operation:gesture.selectionAlgebra,selection_revision:gesture.selectionRevision,selector};
      }
      if (gesture.kind === "geometry_sculpt") {
        const settings=gesture.sculpt;
        const axis=settings.direction;
        let move=this._axisLock(delta,event),along=0;
        if(settings._directionMode!=="view"){
          const a=this.picking.project(gesture.startWorld),b=this.picking.project(add3(gesture.startWorld,scale3(axis,settings.radius_mm)));
          const pixel=[event.clientX-gesture.startClient[0],event.clientY-gesture.startClient[1]];
          const v=a&&b?[b[0]-a[0],b[1]-a[1]]:[0,0];const den=v[0]*v[0]+v[1]*v[1];
          along=den>4?settings.radius_mm*(pixel[0]*v[0]+pixel[1]*v[1])/den:-pixel[1]*settings.radius_mm/Math.max(12,this._worldRadiusToPixels(settings.radius_mm,event));
          if(event.shiftKey)along*=.2;
          move=scale3(axis,along);
        }else along=delta.reduce((a,v,i)=>a+v*axis[i],0);
        const operation={...settings,delta_mm:move,points_mm:gesture.sculptPoints.map(p=>p.slice())};
        delete operation._directionMode;
        if(["stretch","scale","taper"].includes(settings.tool))operation.factor=Math.max(.05,Math.min(20,Math.exp((settings._directionMode==="view"?- (event.clientY-gesture.startClient[1])*settings.radius_mm/Math.max(12,this._worldRadiusToPixels(settings.radius_mm,event)):along)/settings.radius_mm)));
        if(["twist","bend"].includes(settings.tool))operation.angle_rad=Math.max(-4*Math.PI,Math.min(4*Math.PI,(event.clientX-gesture.startClient[0]-(event.clientY-gesture.startClient[1]))*.0125*(event.shiftKey?.2:1)));
        return this.sculptEnhancements?this.sculptEnhancements.gestureOperation(operation,settings,event):operation;
      }
      if (gesture.kind === "field_brush") {
        return { samples: gesture.samples.slice() };
      }
      if (gesture.kind === "glyph") {
        const handle = gesture.glyphHandle || "anchor";
        if (handle === "magnitude") {
          return {
            handle,
            delta: -(event.clientY - gesture.startClient[1]),
            sensitivity: event.shiftKey ? 0.02 : event.altKey ? 1.0 : 0.1,
            snap: snap > 0 ? snap : null
          };
        }
        if (handle === "direction") {
          const dx = (event.clientX - gesture.startClient[0]) * 0.01;
          const dy = -(event.clientY - gesture.startClient[1]) * 0.01;
          return { handle, delta: [dx, dy, 0], gain: event.shiftKey ? 0.2 : 1.0 };
        }
        if (handle === "radius") {
          return {
            handle,
            delta_mm: len3(delta) * (event.clientX >= gesture.startClient[0] ? 1 : -1),
            snap_mm: snap > 0 ? snap : null
          };
        }
        return {
          handle: "anchor",
          delta_mm: this._axisLock(delta, event),
          anchor_mm: liveHit ? liveHit.point_mm : null,
          normal: liveHit ? liveHit.normal : null,
          snap_mm: snap > 0 ? snap : null
        };
      }
      if (gesture.kind === "surface_patch") {
        return gesture.regionPatches && gesture.regionPatches.length > 1 ? {
          patches: gesture.regionPatches.slice(),
          combination: "union"
        } : {
          delta_mm: this._axisLock(delta, event),
          anchor_mm: liveHit ? liveHit.point_mm : null,
          normal: liveHit ? liveHit.normal : null
        };
      }
      if (gesture.kind === "control_lattice" || gesture.kind === "deformation_cage") {
        return {
          index: gesture.controlIndex || [1, 1, 1],
          delta_mm: this._axisLock(delta, event),
          influence_radius: event.shiftKey ? 1.5 : 0,
          falloff: this.falloffInput.value,
          symmetry_axes: gesture.symmetryAxes || []
        };
      }
      return { delta_mm: this._axisLock(delta, event) };
    }

    _appendSelectionGesturePoint(event, hit) {
      const gesture=this.activeGesture;
      if(!gesture||gesture.kind!=="spatial_selection")return;
      const screen=this.picking.coordinates(event).css;
      if(gesture.selectionKind==="box")gesture.screenPoints=[gesture.startScreen.slice(),screen];
      else if(gesture.selectionKind==="lasso"){
        const previous=gesture.screenPoints.at(-1);
        if(!previous||Math.hypot(screen[0]-previous[0],screen[1]-previous[1])>=2)gesture.screenPoints.push(screen);
      }
      if(gesture.selectionKind==="brush"&&hit){
        const point=hit.point_mm.slice(),previous=gesture.selectionPoints.at(-1);
        const spacing=Math.max(1e-9,finiteNumber(this.radiusInput.value,DEFAULTS.radiusMm)*0.22);
        if(!previous||len3(sub3(previous,point))>=spacing)gesture.selectionPoints.push(point);
      }
    }

    _axisLock(delta, event) {
      const axis = this.activeGesture && this.activeGesture.axisLock;
      const scale = event.shiftKey ? 0.2 : event.altKey ? 5.0 : 1.0;
      if (axis === 0) return [delta[0] * scale, 0, 0];
      if (axis === 1) return [0, delta[1] * scale, 0];
      if (axis === 2) return [0, 0, delta[2] * scale];
      return scale3(delta, scale);
    }

    _appendBrushSample(point) {
      const gesture = this.activeGesture;
      if (!gesture) return;
      const sample = {
        point_mm: point.slice(),
        radius_mm: finiteNumber(this.radiusInput.value, DEFAULTS.radiusMm),
        strength: finiteNumber(this.strengthInput.value, DEFAULTS.strength),
        mode: this.brushModeInput.value,
        falloff: this.falloffInput.value
      };
      const previous = gesture.samples[gesture.samples.length - 1];
      const spacing = sample.radius_mm * 0.22;
      if (!previous || len3(sub3(previous.point_mm, sample.point_mm)) >= spacing) {
        gesture.samples.push(sample);
      }
    }

    _appendRegionPatch(hit) {
      const gesture = this.activeGesture;
      if (!gesture || gesture.kind !== "surface_patch") return;
      const patch = this._patchFromHit(hit);
      const previous = gesture.regionPatches[gesture.regionPatches.length - 1];
      const spacing = patch.radius_mm * 0.36;
      const previousPoint = previous && (previous.anchor_mm || previous.point);
      if (!previousPoint || len3(sub3(previousPoint, patch.anchor_mm)) >= spacing) {
        gesture.regionPatches.push(patch);
      }
    }

    async _executePreview(request, signal) {
      return await this.backend.preview({ sequence: request.sequence, operation: request.payload.operation }, signal);
    }

    _applyPreviewResponse(response) {
      if (!response) return;
      dispatch("implexity:interaction-preview", { response, mode: this.mode });
      if(this.activeGesture?.kind==="geometry_sculpt" && response.accepted){
        const evidence=response.state?.evidence||{};
        const count=evidence.changed_control_values||0, held=evidence.frozen_occupancy_samples??evidence.held_control_values??0;
        this.sculptPanel.querySelector("[data-sculpt-evidence]").textContent=`Uncommitted: ${count} values changed · ${held} protected${evidence.clipped_control_values?` · ${evidence.clipped_control_values} limited by control bounds`:""}`;
        this.sculptEnhancements?.evidence(evidence,false);
        const field=response.state?.preview_field;
        if(field)global.ImplexityViewerAdapter?.applyGeometryPreview?.(field);
      }
      const topology = response.topology || (response.state && response.state.topology);
      if (topology && topology.changed) {
        const changes = Array.isArray(topology.changes) ? topology.changes : [];
        const displayValue = value => {
          if (value === null || value === undefined) return "not available";
          if (typeof value === "number" || typeof value === "boolean") return String(value);
          if (typeof value === "string") return displayIdentifier(value);
          return "updated";
        };
        const summary = changes.length ? changes.map(change =>
          `${displayIdentifier(change.kind, change)}: ${displayValue(change.before)} → ${displayValue(change.after)}`
        ).join("; ") : "The connected geometry changed";
        this.toast(`Topology event in preview: ${summary}`, "warning");
        dispatch("implexity:topology-preview-event", { topology, mode: this.mode });
      }
      if (response.glyph || (response.state && response.state.glyph)) {
        const glyph = response.glyph || response.state.glyph;
        this.glyphs.set(glyph.id, glyph);
        this.renderGlyphs();
      }
      if (response.cage || (response.state && response.state.cage)) {
        const cage = response.cage || response.state.cage;
        this.cages.set(cage.id, cage);
        this.renderCages();
      }
      const selection=response.selection||(response.state&&response.state.selection);
      if(selection){
        this.selection=selection;
        this._renderSelectionInspector();
        this.renderSelection();
        this._renderSelectionMarquee();
        const counts=selection.counts||{};
        this._setInspectorStatus(`${counts.selected||0} selected · ${counts.affected||0} affected · ${counts.protected||0} protected`,counts.protected?"warning":"info");
      }
    }

    async _onPointerUp(event) {
      if (this.pendingGesture && event.pointerId === this.pendingGesture.pointerId) {
        this.pendingGesture.released = true;
        this.pendingGesture.releaseEvent = event;
        this.pendingGesture.lastEvent = event;
        event.preventDefault();
        return;
      }
      const gesture = this.activeGesture;
      if (!gesture || gesture.finalizing || gesture.cancelling || event.pointerId !== gesture.pointerId) return;
      event.preventDefault();
      let committed = false;
      try {
        gesture.finalizing = true;
        if (gesture.frame) { cancelAnimationFrame(gesture.frame); gesture.frame = 0; }
        const finalSequence = await this._submitGesturePreview(event, true);
        if (finalSequence == null) throw new Error("The final preview could not be constructed");
        await this.previewQueue.drain(finalSequence);
        const result = await this.backend.commit({ final_sequence: finalSequence });
        committed = true;
        if(gesture.kind==="surface_patch")this._reportSavedRegion(result);
        if(gesture.kind==="spatial_selection"){
          if(!result?.selection)throw new Error("The service returned no authoritative selection");
          const selection=Object.assign({},result.selection,{region_name:result?.entities?.region?.name||result.selection.region_name||this.regionName.value.trim()||"Current selection"});
          spatialSelections.upsert(selection);this.selection=selection;
          this._renderSelectionInspector();this.renderSelection();
          const counts=selection.counts||{};
          this._setInspectorStatus(`${counts.selected||0} cells saved as ${selection.region_name}${counts.protected?` · ${counts.protected} protected cells unchanged`:""}`,counts.protected?"warning":"ok");
        }
        const glyph = result?.entities?.glyph || result?.entities?.load?.glyph || result?.entities?.boundary_condition?.glyph;
        if (glyph && result?.entity_ids?.glyph_id === glyph.id) this.glyphs.set(glyph.id, glyph);
        if(result?.history?.label)manualHistory.record("viewport",result.history.label);
        if(gesture.kind==="geometry_sculpt"){
          const evidence=result.evidence||{};
          this.sculptPanel.querySelector("[data-sculpt-evidence]").textContent=`Saved · ${evidence.changed_control_values||0} values changed. Run a fresh physics preflight.`;
          this.sculptEnhancements?.evidence(evidence,true);
          await this._refreshCommittedGeometry();
          if(evidence.changed_control_values===0 && !["protect","release","select"].includes(gesture.sculpt.tool))this.toast("No control samples changed. Increase the influence radius or use a finer declared control grid.","warning");
        }
        dispatch("implexity:interaction-committed", { mode: gesture.mode, transaction: gesture.transaction, result });
        committed = true;
      } catch (error) {
        this._showError(error, committed ? "The edit was saved, but its viewport refresh failed. Refresh before continuing." : "The interaction could not be committed. Your previous model remains active.");
        if(!committed)try { await this.backend.cancel(); } catch (_) { }
      } finally {
        if(!committed&&gesture.kind==="spatial_selection"){
          this.selection=gesture.selectionBefore;
          try{await this._refreshAuthoritativeField();}catch(_){this._renderSelectionInspector();this.renderSelection();}
          this._setInspectorStatus("Selection was not saved. The last saved selection remains active.","error");
        }
        this._finishGesture(event.pointerId, committed);
      }
    }

    async cancel() {
      if(this.activeGesture?.finalizing){
        this._setInspectorStatus("The edit is being committed. Wait for completion, then use Undo if needed.","warning");return;
      }
      if (!this.activeGesture && this.pendingGesture) {
        const pending = this.pendingGesture;
        pending.cancelled = true;
        if (!pending.backendStarted) this._finishPendingGesture(pending);
        return;
      }
      if (!this.activeGesture) return;
      if(this.activeGesture.cancelling)return;
      this.activeGesture.cancelling=true;
      const pointerId = this.activeGesture.pointerId;
      const selectionBefore=this.activeGesture.kind==="spatial_selection"?this.activeGesture.selectionBefore:null;
      this.previewQueue.clear();
      try {
        await this.backend.cancel();
        dispatch("implexity:interaction-cancelled", { mode: this.activeGesture.mode });
      } catch (error) {
        this._showError(error, "The interaction could not be cancelled cleanly. Refresh the model before continuing.");
      } finally {
        if(selectionBefore!==null)this.selection=selectionBefore;
        this._finishGesture(pointerId);
      }
    }

    _finishGesture(pointerId, committed=false) {
      if(this.activeGesture?.kind === "geometry_sculpt"){
        global.ImplexityViewerAdapter?.clearGeometryPreview?.();
        if(this.sculptPanel && !committed)this.sculptPanel.querySelector("[data-sculpt-evidence]").textContent="No uncommitted edit. Physics evidence must match the saved model.";
      }
      try {
        if (this.viewport.hasPointerCapture(pointerId)) this.viewport.releasePointerCapture(pointerId);
      } catch (_) {                                                 }
      this.activeGesture = null;
      if(this.sculptEnhancements){this.sculptEnhancements.previewSelection=null;this.sculptEnhancements.render();}
      pointerCoordinator.release(this.pointerOwner, pointerId);
      this.viewport.classList.remove("implexity-viewport-busy");
      this.hud.dataset.active = "false";
      this._showBrush(false);
      this.renderSelection();
    }

    _reportSavedRegion(result) {
      const region=result?.entities?.region;
      const confirmed=region?.id&&result?.entity_ids?.region_id===region.id;
      const message=confirmed?`Region saved: ${region.name||region.id}. Open CAE setup to assign its physical role.`:"The service did not confirm a saved named region. Reload the problem before assigning a condition.";
      this._setInspectorStatus(message,confirmed?"ok":"warning");this.toast(message,confirmed?"success":"warning");
    }

    _patchFromHit(hit) {
      return {
        schema: "implexity-surface-patch/2",
        kind: "surface_patch",
        anchor_mm: hit.point_mm.slice(),
        normal: hit.normal.slice(),
        radius_mm: finiteNumber(this.radiusInput.value, DEFAULTS.radiusMm),
        band_mm: Math.max(1e-6, finiteNumber(this.radiusInput.value, DEFAULTS.radiusMm) * 0.08),
        falloff: { kind: this.falloffInput.value, exponent: 2.0 },
        normal_mode: "front",
        minimum_normal_alignment: 0.05,
        connected: true,
        tracking: {
          mode: "implicit",
          node_id: hit.node_id,
          selector: hit.selector,
          structure_id: hit.structure_id,
          content_id: hit.content_id,
          revision: hit.revision,
          exact_pick: true,
          clip_evidence: deepClone(hit.clip_evidence),
          coordinate_evidence: deepClone(hit.coordinates)
        }
      };
    }

    _glyphFromHit(hit, kind) {
      return {
        schema: "implexity-engineering-glyph/1",
        kind,
        patch: this._patchFromHit(hit),
        magnitude: finiteNumber(this.magnitudeInput.value, DEFAULTS.magnitude),
        direction: hit.normal.slice(),
        units: this._unitsForKind(kind),
        enabled: true
      };
    }

    _unitsForKind(kind) {
      return ({ pressure: "Pa", traction: "Pa", heat_flux: "W/m2", temperature: "K", clamp: "m" })[kind] || null;
    }

    _activeFieldId() {
      const sources = [global.ImplexitySpatialFields, global.implexitySpatialFields].filter(Boolean);
      for (const source of sources) {
        if (typeof source.activeFieldId === "function") return source.activeFieldId();
        if (source.activeFieldId) return source.activeFieldId;
      }
      return null;
    }

    _activeFieldPayload() {
      const sources = [global.ImplexitySpatialFields, global.implexitySpatialFields].filter(Boolean);
      for (const source of sources) {
        if (typeof source.activeFieldPayload === "function") return source.activeFieldPayload();
        if (source.activeField) return deepClone(source.activeField);
      }
      dispatch("implexity:spatial-field-request", { controller: this });
      return null;
    }

    _setHud(event, title, detail) {
      const rect = this.viewport.getBoundingClientRect();
      this.hud.style.left = `${event.clientX - rect.left}px`;
      this.hud.style.top = `${event.clientY - rect.top}px`;
      this.hud.querySelector(".implexity-hud-title").textContent = title;
      this.hud.querySelector(".implexity-hud-detail").textContent = detail;
      this.hud.dataset.active = "true";
    }

    _hudDetail(gesture, delta) {
      const mm = len3(delta).toFixed(3);
      if(gesture.kind==="spatial_selection"){
        if(gesture.selectionKind==="lasso")return `${gesture.screenPoints.length} outline points · ${gesture.selectionAlgebra}`;
        if(gesture.selectionKind==="box")return `${Math.abs((gesture.screenPoints.at(-1)?.[0]||0)-gesture.startScreen[0]).toFixed(0)} × ${Math.abs((gesture.screenPoints.at(-1)?.[1]||0)-gesture.startScreen[1]).toFixed(0)} px · ${gesture.selectionAlgebra}`;
        if(gesture.selectionKind==="brush")return `${gesture.selectionPoints.length} exact samples · ${gesture.selectionAlgebra}`;
        return `${displayIdentifier(gesture.selectionKind)} · ${gesture.selectionAlgebra}`;
      }
      if (gesture.kind === "field_brush") return `${gesture.samples.length} samples · ${mm} mm`;
      if (gesture.kind === "surface_patch") return `${gesture.regionPatches.length} surface samples · ${mm} mm`;
      if (gesture.kind === "glyph") return `${modeLabel(gesture.mode)} · ${mm} mm`;
      if (gesture.kind.includes("cage") || gesture.kind.includes("lattice")) return `Move control point · ${mm} mm`;
      return `${mm} mm`;
    }

    toast(message, level = "info") {
      const existing=Array.from(this.toastStack.children).find(item => item.dataset.level === level && item.querySelector(".implexity-toast-message")?.textContent === String(message));if(existing)return existing;
      const node = createElement("div", "implexity-toast");
      for (const eventName of ["pointerdown","pointerup","mousedown","mouseup","click","dblclick"]) node.addEventListener(eventName, event => event.stopPropagation());
      node.dataset.level = level;
      node.setAttribute("role",["error","warning"].includes(level)?"alert":"status");
      node.setAttribute("aria-live",["error","warning"].includes(level)?"assertive":"polite");
      node.appendChild(createElement("span","implexity-toast-message",message));
      const close=createElement("button","implexity-toast-close","×");
      close.type="button";close.setAttribute("aria-label","Dismiss notification");close.addEventListener("click",()=>node.remove());node.appendChild(close);
      this.toastStack.appendChild(node);
      while (this.toastStack.children.length > 3) this.toastStack.firstElementChild.remove();
      if(!["error","warning"].includes(level))setTimeout(() => node.remove(), 8000);
      return node;
    }

    _showError(error, fallback, level = "error") {
      if (global.console?.warn) global.console.warn("Implexity viewport interaction failed", error);
      const notice=this.toast(friendlyErrorMessage(error, fallback), level);
      if(notice&&!notice.querySelector("details")){const details=createElement("details","");details.appendChild(createElement("summary","","Technical details"));details.appendChild(createElement("pre","",String(error?.message||error||"Unknown error")));notice.appendChild(details);}
    }

    setGlyphs(glyphs) {
      this.glyphs.clear();
      (glyphs || []).forEach(glyph => this.glyphs.set(glyph.id, deepClone(glyph)));
      this.renderGlyphs();
    }

    renderOverlays() {
      this.renderSelection();
      this._renderSelectionMarquee();
      this.renderGlyphs();
      this.renderCages();
    }

    renderGlyphs() {
      this.glyphLayer.replaceChildren();
      for (const glyph of this.glyphs.values()) {
        const point = glyph.patch && (glyph.patch.anchor_mm || glyph.patch.point);
        const screen = point && this.picking.project(point);
        if (!screen) continue;
        const group = createSvgElement("g", "implexity-glyph");
        group.dataset.glyphId = glyph.id;
        group.dataset.kind = glyph.kind;
        group.setAttribute("transform", `translate(${screen[0]} ${screen[1]})`);
        const radius = Math.max(12, finiteNumber(glyph.screen_radius, 24));
        const direction3 = unit3(vec3(glyph.direction || (glyph.patch && glyph.patch.normal), [0, 0, 1]));
        const worldEnd = add3(point, scale3(direction3, Math.max(1, finiteNumber(glyph.world_vector_length_mm, 1))));
        const projectedEnd = this.picking.project(worldEnd);
        let dx = 0, dy = -radius * 1.45;
        if (projectedEnd) {
          const rawDx = projectedEnd[0] - screen[0], rawDy = projectedEnd[1] - screen[1];
          const rawLength = Math.hypot(rawDx, rawDy);
          if (rawLength > 1e-6) { dx = rawDx / rawLength * radius * 1.45; dy = rawDy / rawLength * radius * 1.45; }
        }
        const radiusHandle = createSvgElement("circle", "implexity-glyph-radius", { r: radius, "data-handle": "radius" });
        const anchorHandle = createSvgElement("circle", "implexity-glyph-anchor", { r: 5, "data-handle": "anchor" });
        const vectorHandle = createSvgElement("line", "implexity-glyph-vector", { x1: 0, y1: 0, x2: dx, y2: dy, "data-handle": "direction" });
        const magnitudeHandle = createSvgElement("circle", "implexity-glyph-magnitude", { cx: dx, cy: dy, r: 4, "data-handle": "magnitude" });
        const label = createSvgElement("text", "implexity-glyph-label", { x: 8, y: -8 },
          displayIdentifier(glyph.kind, { label: glyph.label, display_name: glyph.display_name }));
        group.setAttribute("aria-label", label.textContent);
        group.setAttribute("role", "img");
        group.appendChild(createSvgElement("title", "", {}, `${label.textContent}: drag the centre to move the area or the outer ring to resize it. Review condition values in the physics setup.`));
        group.append(radiusHandle, anchorHandle, vectorHandle, magnitudeHandle, label);
        this.glyphLayer.appendChild(group);
      }
    }

    renderCages() {
      this.cageLayer.replaceChildren();
      for (const cage of this.cages.values()) {
        const lattice = cage.lattice || cage;
        const offsets = lattice.offsets_mm;
        const bounds = lattice.bounds_mm;
        if (!offsets || !bounds) continue;
        const shape = lattice.shape || [offsets.length, offsets[0].length, offsets[0][0].length];
        const lo = bounds.min_mm || bounds[0];
        const hi = bounds.max_mm || bounds[1];
        const projected = new Map();
        for (let i = 0; i < shape[0]; i++) for (let j = 0; j < shape[1]; j++) for (let k = 0; k < shape[2]; k++) {
          const base = [0,1,2].map(axis => lo[axis] + (hi[axis] - lo[axis]) * [i,j,k][axis] / (shape[axis] - 1));
          const point = add3(base, offsets[i][j][k]);
          const screen = this.picking.project(point);
          if (screen) projected.set(`${i},${j},${k}`, screen);
        }
        const svg = "http://www.w3.org/2000/svg";
        for (const [key, screen] of projected) {
          const idx = key.split(",").map(Number);
          for (let axis = 0; axis < 3; axis++) {
            const next = idx.slice(); next[axis]++;
            const other = projected.get(next.join(","));
            if (!other) continue;
            const line = document.createElementNS(svg, "line");
            line.classList.add("implexity-cage-edge");
            line.setAttribute("x1", screen[0]); line.setAttribute("y1", screen[1]);
            line.setAttribute("x2", other[0]); line.setAttribute("y2", other[1]);
            this.cageLayer.appendChild(line);
          }
        }
        for (const [key, screen] of projected) {
          const circle = document.createElementNS(svg, "circle");
          circle.classList.add("implexity-control-point");
          circle.dataset.cageId = cage.id;
          circle.dataset.index = key;
          circle.setAttribute("cx", screen[0]); circle.setAttribute("cy", screen[1]); circle.setAttribute("r", "4.5");
          this.cageLayer.appendChild(circle);
        }
      }
    }

    async _onGlyphPointerDown(event) {
      const group = event.target.closest(".implexity-glyph");
      if (!group) return;
      const glyph = this.glyphs.get(group.dataset.glyphId);
      if (!glyph) return;
      if (!pointerCoordinator.claim(this.pointerOwner, event.pointerId)) return;
      event.preventDefault(); event.stopPropagation();
      this.setMode(glyph.kind);
      const pending = this._startPendingGesture(event);
      const handle = event.target.dataset.handle || "anchor";
      const hit = { point_mm: glyph.patch.anchor_mm, normal: glyph.patch.normal, raw: {} };
      try {
        this.previewQueue.reset();
        pending.backendStarted = true;
        const tx = await this.backend.begin({ kind: "glyph", glyph });
        await this._activatePendingGesture(pending, {
          pointerId: event.pointerId,
          mode: glyph.kind,
          kind: "glyph",
          glyphHandle: handle,
          hit,
          startClient: [event.clientX, event.clientY],
          startWorld: hit.point_mm.slice(),
          latestWorld: hit.point_mm.slice(),
          samples: [],
          transaction: tx,
          sequence: 0, buildEpoch: 0, finalizing: false
        });
      } catch (error) {
        if (pending.backendStarted) { try { await this.backend.cancel(); } catch (_) {                          } }
        this._finishPendingGesture(pending);
        this._showError(error, "The engineering object could not be selected for editing.");
      }
    }


    _controlPointWorld(cage, index) {
      const lattice = cage && (cage.lattice || cage);
      if (!lattice || !lattice.bounds_mm || !lattice.offsets_mm) return null;
      const lo = lattice.bounds_mm.min_mm || lattice.bounds_mm[0];
      const hi = lattice.bounds_mm.max_mm || lattice.bounds_mm[1];
      const shape = lattice.shape || [lattice.offsets_mm.length, lattice.offsets_mm[0].length, lattice.offsets_mm[0][0].length];
      const base = [0, 1, 2].map(axis => lo[axis] + (hi[axis] - lo[axis]) * index[axis] / (shape[axis] - 1));
      return add3(base, lattice.offsets_mm[index[0]][index[1]][index[2]]);
    }

    async _onControlPointerDown(event) {
      const control = event.target.closest(".implexity-control-point");
      if (!control) return;
      const cage = this.cages.get(control.dataset.cageId);
      if (!cage) return;
      if (!pointerCoordinator.claim(this.pointerOwner, event.pointerId)) return;
      event.preventDefault(); event.stopPropagation();
      const pending = this._startPendingGesture(event);
      const index = control.dataset.index.split(",").map(Number);
      const point = this._controlPointWorld(cage, index) || [0, 0, 0];
      try {
        this.previewQueue.reset();
        pending.backendStarted = true;
        const tx = await this.backend.begin({ kind: "deformation_cage", cage });
        await this._activatePendingGesture(pending, {
          pointerId: event.pointerId,
          mode: MODES.CAGE,
          kind: "deformation_cage",
          controlIndex: index,
          startClient: [event.clientX, event.clientY],
          startWorld: point,
          latestWorld: point,
          samples: [],
          transaction: tx,
          sequence: 0, buildEpoch: 0, finalizing: false
        });
      } catch (error) {
        if (pending.backendStarted) { try { await this.backend.cancel(); } catch (_) {                          } }
        this._finishPendingGesture(pending);
        this._showError(error, "The deformation control could not be selected for editing.");
      }
    }
  }

  function autoStart() {
    if (global.IMPLEXITY_DISABLE_INTERACTION) return;
    try {
      if (!global.ImplexityInteraction) new ViewportInteraction(global.IMPLEXITY_INTERACTION_OPTIONS || {});
    } catch (error) {
      console.warn("Implexity interaction layer did not start:", error);
    }
  }

  global.ImplexityViewportInteraction = ViewportInteraction;
  global.ImplexitySerialPreviewQueue = SerialPreviewQueue;
  global.ImplexityInteractionBackend = InteractionBackend;
  global.ImplexityPickingAdapter = PickingAdapter;
  global.ImplexityInteractionModes = MODES;
  global.ImplexityInteractionPresentation = Object.freeze({
    displayIdentifier, modeLabel, optionProfile, friendlyErrorMessage
  });
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", autoStart, { once: true });
  else autoStart();
})(window);
