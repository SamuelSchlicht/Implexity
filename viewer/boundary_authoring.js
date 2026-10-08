// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

(global => {
  "use strict";

  const BOUNDARY_KINDS = Object.freeze({
    dirichlet_T: Object.freeze({
      label: "Prescribed temperature",
      summary: "Hold the selected region at an absolute temperature.",
      unit: "K",
      field: "value_K",
      defaultValue: 293.15,
    }),
    robin: Object.freeze({
      label: "Resolved convection",
      summary: "Exchange heat with the coolant resolved by the active physics add-in.",
      unit: "W/(m²·K)",
    }),
    clamp: Object.freeze({
      label: "Fixed support",
      summary: "Set selected displacement components to zero.",
      unit: "m",
    }),
  });
  const IDENTIFIER = /^[A-Za-z][A-Za-z0-9_.-]*$/;
  const clone = value => value == null ? value : JSON.parse(JSON.stringify(value));
  const text = value => typeof value === "string" ? value.trim() : "";
  const element = (tag, className = "", content = null) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (content != null) node.textContent = String(content);
    return node;
  };

  function publicBoundary(value) {
    if (!value || typeof value !== "object" || Array.isArray(value)) return value;
    if (!value.spec || typeof value.spec !== "object" || Array.isArray(value.spec)) return clone(value);
    const outer = clone(value);
    const spec = clone(outer.spec);
    delete outer.spec;
    return {
      ...outer,
      ...spec,
      id: outer.id,
      name: outer.name,
      enabled: outer.enabled !== false,
    };
  }

  function publicBoundaries(value) {
    if (value == null) return [];
    const requireEntry = (item, label) => {
      if (!item || typeof item !== "object" || Array.isArray(item)) {
        throw new TypeError(`${label} must be a boundary-condition object. Correct the expert JSON and try again.`);
      }
      return item;
    };
    const source = Array.isArray(value)
      ? value.map((item, index) => requireEntry(item, `Boundary ${index + 1}`))
      : (typeof value === "object" ? Object.entries(value).map(([id, item]) => ({id, ...requireEntry(item, `Boundary “${id}”`)})) : null);
    if (!source) throw new TypeError("Boundary conditions must be a list or named object.");
    return source.map(publicBoundary);
  }

  function namedRegions(value) {
    if (value == null) return [];
    const source = Array.isArray(value)
      ? value
      : (typeof value === "object" ? Object.entries(value).map(([id, item]) => {
        if (!item || typeof item !== "object" || Array.isArray(item)) throw new TypeError(`Region “${id}” must be an object.`);
        return {id, ...item};
      }) : null);
    if (!source) throw new TypeError("Named regions must be a list or named object.");
    const seen = new Set();
    return source.map((item, index) => {
      if (!item || typeof item !== "object" || Array.isArray(item)) throw new TypeError(`Region ${index + 1} must be an object.`);
      const id = text(item.id);
      if (!id || id !== item.id) throw new TypeError(`Region ${index + 1} needs an explicit, nonempty ID without surrounding whitespace.`);
      if (seen.has(id)) throw new TypeError(`Region ID “${id}” is repeated. Give each region a unique ID.`);
      seen.add(id);
      return {id, name: text(item.name) || id};
    });
  }

  function regionReference(value) {
    if (typeof value === "string") return {kind: "named", id: value};
    if (value && typeof value === "object" && !Array.isArray(value)) {
      if ((value.type === "ref" || value.kind === "ref") && text(value.id)) return {kind: "named", id: text(value.id)};
      return {kind: "embedded", value: clone(value)};
    }
    return {kind: "missing", id: ""};
  }

  function cleanKind(value) {
    const kind = text(value);
    if (["temperature", "prescribed_temperature", "dirichlet_t"].includes(kind.toLowerCase())) return "dirichlet_T";
    return kind;
  }

  function identifierStem(value, fallback = "boundary") {
    let stem = text(value).toLowerCase().replace(/[^a-z0-9_.-]+/g, "_").replace(/^[^a-z]+|[_.-]+$/g, "");
    if (!stem) stem = fallback;
    if (!/^[a-z]/.test(stem)) stem = `boundary_${stem}`;
    return stem;
  }

  function uniqueIdentifier(preferred, boundaries, excludedIndex = -1) {
    const occupied = new Set(publicBoundaries(boundaries).map((item, index) => index === excludedIndex ? "" : text(item?.id)).filter(Boolean));
    const stem = identifierStem(preferred);
    let candidate = stem;let suffix = 2;
    while (occupied.has(candidate)) candidate = `${stem}_${suffix++}`;
    return candidate;
  }

  function regenerateBoundaryId(boundary, boundaries, index = -1) {
    const ref = regionReference(boundary?.region);
    const preferred = text(boundary?.name) || (ref.kind === "named" ? ref.id : "") || cleanKind(boundary?.kind || boundary?.type) || "boundary";
    return uniqueIdentifier(preferred, boundaries, index);
  }

  function selectorValue(value) {
    return String(value).replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  }

  function providerIssueRows(value) {
    if (!value || typeof value !== "object") return [];
    const candidates = [value.issues, value.errors, value.problems, value.result?.issues, value.result?.errors];
    const source = candidates.find(Array.isArray) || [];
    return source.filter(item => item && typeof item === "object" && text(item.path) && text(item.message));
  }

  function mapProviderIssue(issue) {
    const path = text(issue?.path);const message = text(issue?.message);
    if (!path || !message) return null;
    const mapped = {path, message, step: "qualification", selectors: []};
    const boundary = path.match(/^(?:boundaries|boundary_conditions)(?:\[(\d+)\]|\.(\d+))(?:\.|\[)?([A-Za-z0-9_-]+)?/);
    if (boundary) {
      const index = Number(boundary[1] ?? boundary[2]);
      const rawField = boundary[3] || "";
      const field = ({value_K: "value", temperature: "value", type: "kind"})[rawField] || rawField || "role";
      mapped.step = "boundaries";mapped.index = index;mapped.field = field;
      mapped.selectors.push(`[data-boundary-index="${index}"] [data-boundary-field="${selectorValue(field)}"]`);
      if (field === "selector") mapped.selectors.push(`[data-pw-boundary-selector="${index}"]`);
      else mapped.selectors.push(`[data-pw-boundary="${index}"] [data-pw-boundary-field="${selectorValue(field)}"]`);
      return mapped;
    }
    const point = path.match(/^mission\.operatingPoints(?:\[(\d+)\]|\.(\d+))\.([A-Za-z0-9_-]+)/);
    if (point) {
      const index = Number(point[1] ?? point[2]);const field = point[3];
      mapped.step = "mission";mapped.index = index;mapped.field = field;
      mapped.selectors.push(`[data-pw-op-index="${index}"][data-pw-op-field="${selectorValue(field)}"]`);
      return mapped;
    }
    const firstMatch = /^[A-Za-z0-9_-]+/.exec(path);
    const first = firstMatch ? firstMatch[0] : "";
    mapped.step = ({
      engine: "mission", domain: "mission", mission: "mission",
      streams: "streams", chemistry: "chemistry", flow: "flow",
      thermal: "thermal", material: "life", life: "life",
      design: "design", advancedPhysics: "advanced",
      screeningPhysics: "advanced", responses: "qualification",
      couplingSelection: "advanced", boundaries: "boundaries",
      boundary_conditions: "boundaries",
    })[first] || "qualification";
    const dotted = path.replace(/\[(\d+)\]/g, ".$1");
    mapped.selectors.push(`[data-pw-path="${selectorValue(dotted)}"]`);
    return mapped;
  }

  function mapProviderIssues(value) {
    return providerIssueRows(value).map(mapProviderIssue).filter(Boolean);
  }

  function validationIssues(boundaries, regions) {
    const knownRegions = new Set((regions || []).map(region => region.id));
    const ids = new Set();
    const issues = [];
    publicBoundaries(boundaries).forEach((boundary, index) => {
      const path = `Boundary ${index + 1}`;
      const id = text(boundary?.id);
      if (!IDENTIFIER.test(id)) issues.push({index, field: "id", message: `${path} needs a stable ID beginning with a letter.`});
      else if (ids.has(id)) issues.push({index, field: "id", message: `${path} repeats the ID “${id}”.`});
      ids.add(id);
      if (!text(boundary?.name)) issues.push({index, field: "name", message: `${path} needs a descriptive name.`});
      const ref = regionReference(boundary?.region);
      if (ref.kind === "missing") issues.push({index, field: "region", message: `${path} needs a target region.`});
      if (ref.kind === "named" && !knownRegions.has(ref.id)) issues.push({index, field: "region", message: `${path} refers to unavailable region “${ref.id}”.`});
      const kind = cleanKind(boundary?.kind || boundary?.type);
      if (kind === "dirichlet_T") {
        const value = boundary.value_K ?? boundary.value ?? boundary.temperature;
        if (!Number.isFinite(value) || value < 1) issues.push({index, field: "value", message: `${path} temperature must be a finite absolute value of at least 1 K.`});
      } else if (kind === "robin") {
        if ((boundary.alpha ?? "h_conv") !== "h_conv" || (boundary.reference ?? "T_f") !== "T_f") {
          issues.push({index, field: "kind", message: `${path} must use the provider-resolved h_conv and T_f fields.`});
        }
      } else if (kind === "clamp") {
        const components = Array.isArray(boundary.components) ? boundary.components.join("") : text(boundary.components ?? "xyz");
        const invalidArray = Array.isArray(boundary.components) && boundary.components.some(axis => typeof axis !== "string" || axis.length !== 1);
        if (invalidArray || !components || [...components].some(axis => !"xyz".includes(axis)) || new Set(components).size !== components.length) {
          issues.push({index, field: "components", message: `${path} must constrain at least one unique x, y or z component.`});
        }
      }
    });
    return issues;
  }

  function nextIdentifier(boundaries, kind) {
    const occupied = new Set(publicBoundaries(boundaries).map(item => text(item?.id)));
    const stem = kind === "dirichlet_T" ? "temperature" : kind === "robin" ? "convection" : "support";
    let index = 1;
    while (occupied.has(`${stem}_${index}`)) index += 1;
    return `${stem}_${index}`;
  }

  function newBoundary(kind, regionId, boundaries = []) {
    if (!BOUNDARY_KINDS[kind]) throw new RangeError(`Unsupported guided boundary kind: ${kind}`);
    const id = nextIdentifier(boundaries, kind);
    const base = {id, name: BOUNDARY_KINDS[kind].label, enabled: true, kind, region: regionId || ""};
    if (kind === "dirichlet_T") return {...base, value_K: BOUNDARY_KINDS[kind].defaultValue, units: "K"};
    if (kind === "robin") return {...base, alpha: "h_conv", reference: "T_f", units: "W/(m^2 K)", coupling: {mode: "coarea"}};
    return {...base, components: "xyz", units: "-"};
  }

  class GuidedBoundaryEditor {
    constructor({host, textarea, regionsTextarea, onDirty = () => {}, onPickRegion = () => {}} = {}) {
      if (!host || !textarea) throw new Error("Guided boundary editor needs a host and authoritative JSON field.");
      this.host = host;
      this.textarea = textarea;
      this.regionsTextarea = regionsTextarea || null;
      this.onDirty = onDirty;
      this.onPickRegion = onPickRegion;
      this.boundaries = [];
      this.regions = [];
      this.busy = false;
      this.history = [[]];
      this.historyIndex = 0;
      this.pendingBoundary = null;
      this.render();
    }

    setProblem(problem = {}) {
      const boundaries = publicBoundaries(problem.boundary_conditions);
      const regions = namedRegions(problem.regions);
      this.pendingBoundary = null;
      this.boundaries = boundaries;
      this.regions = regions;
      this.history = [clone(this.boundaries)];
      this.historyIndex = 0;
      this._write();
      this.render();
    }

    syncRegionsFromTextarea() {
      if (!this.regionsTextarea) return;
      const raw = this.regionsTextarea.value.trim();
      if (!raw) this.regions = [];
      else this.regions = namedRegions(JSON.parse(raw));
      this.render();
    }

    syncFromTextarea() {
      const raw = this.textarea.value.trim();
      this._commit(publicBoundaries(raw ? JSON.parse(raw) : []));
    }

    setBusy(value) {
      this.busy = Boolean(value);
      this.host.setAttribute("aria-busy", String(this.busy));
      this.host.querySelectorAll("button,input,select").forEach(control => {
        control.disabled = this.busy || control.dataset.boundaryDisabled === "true" || (control.dataset.boundaryRequiresRegion === "true" && !this.regions.length);
      });
    }

    flush({report = false} = {}) {
      const issues = validationIssues(this.boundaries, this.regions);
      if (issues.length) {
        this.render();
        if (report) {
          const first = this.host.querySelector(`[data-boundary-index="${issues[0].index}"] [data-boundary-field="${issues[0].field}"]`);
          first?.focus?.({preventScroll: false});
        }
        const error = new RangeError(issues[0].message);
        error.handledByBoundaryEditor = true;
        error.issues = issues;
        throw error;
      }
      this._write();
      return clone(this.boundaries);
    }

    _write() {
      this.textarea.value = JSON.stringify(this.boundaries, null, 2);
      this.textarea.dispatchEvent?.(new Event("input", {bubbles: true}));
    }

    _commit(next) {
      this.boundaries = publicBoundaries(next);
      this.history = this.history.slice(0, this.historyIndex + 1);
      this.history.push(clone(this.boundaries));
      this.historyIndex = this.history.length - 1;
      this._write();
      this.render();
      this.onDirty(clone(this.boundaries));
    }

    _replace(index, next) {
      const boundaries = clone(this.boundaries);
      boundaries[index] = next;
      this._commit(boundaries);
    }

    _add(kind, regionId) {
      this._commit([...this.boundaries, newBoundary(kind, regionId, this.boundaries)]);
      requestAnimationFrame(() => this.host.querySelector(`[data-boundary-index="${this.boundaries.length - 1}"] input[data-boundary-field="name"]`)?.focus?.());
    }

    _renderCreator() {
      const creator = element("section", "implexity-boundary-creator");
      creator.setAttribute("aria-label", "Create a boundary condition");
      const target = element("select");target.id = "implexityBoundaryTargetRegion";
      const choose = element("option", "", "Choose a named area…");choose.value = "";target.append(choose);
      this.regions.forEach(region => {const option = element("option", "", region.name);option.value = region.id;target.append(option);});
      target.value = this.pendingBoundary?.region || "";
      creator.append(this._labelledControl("1 · Select the area", target, "Use a named surface region, or select a new region on the model below."));
      target.addEventListener("change", () => {this.pendingBoundary = target.value ? newBoundary(this.pendingBoundary?.kind || "dirichlet_T", target.value, this.boundaries) : null;this.render();});
      const pick = element("button", "implexity-boundary-pick", "Select a new region on the model");pick.type = "button";pick.addEventListener("click", () => this.onPickRegion());creator.append(pick);
      if (!this.regions.length) creator.append(element("p", "implexity-boundary-empty", "No named areas yet. Select a region on the model, then return here to assign its condition."));
      if (this.pendingBoundary && this.regions.some(r => r.id === this.pendingBoundary.region)) {
        const draft = this.pendingBoundary;
        const role = element("select");role.setAttribute("aria-label", "Physical role for the selected area");
        Object.entries(BOUNDARY_KINDS).forEach(([kind, meta]) => {const option=element("option","",meta.label);option.value=kind;role.append(option);});role.value=draft.kind;
        role.addEventListener("change", () => {this.pendingBoundary=newBoundary(role.value,draft.region,this.boundaries);this.render();});
        creator.append(this._labelledControl("2 · Choose its role",role,BOUNDARY_KINDS[draft.kind].summary));
        const name=element("input");name.value=draft.name;name.required=true;name.setAttribute("aria-label","New condition name");
        name.addEventListener("input",()=>{draft.name=name.value.trim();});
        creator.append(this._labelledControl("3 · Name and values",name,"Give the condition a name you will recognize in the exported problem."));
        if(draft.kind==="dirichlet_T") {
          const value=element("input");value.type="number";value.min="1";value.step="any";value.required=true;value.value=draft.value_K;value.setAttribute("aria-label","New absolute temperature in kelvin");
          value.addEventListener("input",()=>{draft.value_K=value.value.trim()===""?null:Number(value.value);});
          creator.append(this._labelledControl("Absolute temperature [K]",value,"Use kelvin, not degrees Celsius. The provider checks the material temperature range."));
        } else if(draft.kind==="clamp") {
          const axes=element("fieldset");axes.append(element("legend","","Fixed displacement [0 m]"));
          for(const axis of "xyz") {const label=element("label"),box=element("input");box.type="checkbox";box.checked=draft.components.includes(axis);box.value=axis;box.addEventListener("change",()=>{draft.components=[...axes.querySelectorAll("input:checked")].map(c=>c.value).join("");});label.append(box,document.createTextNode(axis.toUpperCase()));axes.append(label);}creator.append(axes);
        } else creator.append(element("p","implexity-boundary-resolved","Heat-transfer coefficient h_conv [W/(m²·K)] and coolant temperature T_f [K] come from the active provider. No constant coefficient is assumed."));
        const review=element("button","","4 · Review condition");review.type="button";
        const preview=element("div","");preview.setAttribute("aria-live","polite");
        review.addEventListener("click",()=>{
          preview.replaceChildren();
          const problems=validationIssues([...this.boundaries,draft],this.regions).filter(i=>i.index===this.boundaries.length);
          if(problems.length){preview.append(element("p","implexity-boundary-errors",problems.map(i=>i.message).join(" ")));return;}
          const region=this.regions.find(r=>r.id===draft.region);
          const value=draft.kind==="dirichlet_T"?`${draft.value_K} K`:draft.kind==="clamp"?`${draft.components.toUpperCase()} fixed at 0 m`:"provider-resolved convection";
          preview.append(element("p","",`${region.name} → ${draft.name}: ${value}.`));
          const add=element("button","","Add to problem draft");add.type="button";const reviewed=clone(draft);
          add.addEventListener("click",()=>{if(JSON.stringify(draft)!==JSON.stringify(reviewed)){preview.replaceChildren(element("p","implexity-boundary-errors","Values changed. Review the condition again before adding it."));return;}this.pendingBoundary=null;this._commit([...this.boundaries,reviewed]);});preview.append(add,element("small","","Then use Validate and apply to save the problem. No solver is started."));
        });creator.append(review,preview);
      }
      return creator;
    }

    _remove(index) {
      this._commit(this.boundaries.filter((_, row) => row !== index));
    }

    undo() {
      if (this.historyIndex <= 0) return false;
      this.historyIndex -= 1;
      this.boundaries = clone(this.history[this.historyIndex]);
      this._write();this.render();this.onDirty(clone(this.boundaries));return true;
    }

    redo() {
      if (this.historyIndex >= this.history.length - 1) return false;
      this.historyIndex += 1;
      this.boundaries = clone(this.history[this.historyIndex]);
      this._write();this.render();this.onDirty(clone(this.boundaries));return true;
    }

    _labelledControl(labelText, control, helpText = "") {
      const label = element("label", "implexity-boundary-field");
      label.append(element("span", "implexity-boundary-label", labelText), control);
      if (helpText) label.append(element("small", "", helpText));
      return label;
    }

    _selectRegion(boundary, index) {
      const select = element("select");
      select.dataset.boundaryField = "region";
      select.setAttribute("aria-label", `Target region for ${text(boundary.name) || `boundary ${index + 1}`}`);
      const ref = regionReference(boundary.region);
      const placeholder = element("option", "", "Choose a named region…");placeholder.value = "";select.append(placeholder);
      if (ref.kind === "embedded") {
        const embedded = element("option", "", "Embedded region selector (preserved)");embedded.value = "__embedded__";select.append(embedded);select.value = "__embedded__";
      }
      for (const region of this.regions) {
        const option = element("option", "", region.name === region.id ? region.name : `${region.name} · ${region.id}`);
        option.value = region.id;select.append(option);
      }
      if (ref.kind === "named" && !this.regions.some(region => region.id === ref.id)) {
        const missing = element("option", "", `Unavailable region · ${ref.id}`);missing.value = ref.id;select.append(missing);
      }
      if (ref.kind === "named") select.value = ref.id;
      select.addEventListener("change", () => {
        if (select.value === "__embedded__") return;
        this._replace(index, {...boundary, region: select.value});
      });
      return select;
    }

    _renderKnownBody(card, boundary, index, kind) {
      const fields = element("div", "implexity-boundary-grid");
      const name = element("input");name.type = "text";name.value = text(boundary.name);name.required = true;name.dataset.boundaryField = "name";
      name.setAttribute("autocomplete", "off");name.addEventListener("change", () => this._replace(index, {...boundary, name: name.value.trim()}));
      fields.append(this._labelledControl("Condition name", name, "Shown in histories, summaries and exported provenance."));

      const region = this._selectRegion(boundary, index);
      fields.append(this._labelledControl("Target region", region, "Named regions stay attached to the authoritative implicit geometry."));

      const type = element("select");type.dataset.boundaryField = "kind";
      for (const [id, meta] of Object.entries(BOUNDARY_KINDS)) { const option = element("option", "", meta.label);option.value = id;type.append(option); }
      type.value = kind;
      type.addEventListener("change", () => {
        const next = newBoundary(type.value, regionReference(boundary.region).id || "", this.boundaries);
        next.id = boundary.id;next.name = boundary.name;next.enabled = boundary.enabled !== false;
        if (regionReference(boundary.region).kind === "embedded") next.region = clone(boundary.region);
        this._replace(index, next);
      });
      fields.append(this._labelledControl("Condition type", type, BOUNDARY_KINDS[kind].summary));

      if (kind === "dirichlet_T") {
        const wrap = element("span", "implexity-boundary-number");
        const value = element("input");value.type = "number";value.min = "1";value.step = "any";value.inputMode = "decimal";value.required = true;value.dataset.boundaryField = "value";
        value.value = String(boundary.value_K ?? boundary.value ?? boundary.temperature ?? 293.15);
        value.addEventListener("change", () => {
          const next = {...boundary, kind: "dirichlet_T", value_K: value.value.trim() === "" ? null : Number(value.value), units: "K"};
          delete next.value;delete next.temperature;this._replace(index, next);
        });
        wrap.append(value, element("span", "", "K"));
        fields.append(this._labelledControl("Absolute temperature", wrap, "Finite and ≥ 1 K. The active add-in validates its applicable material range."));
      } else if (kind === "robin") {
        const resolved = element("div", "implexity-boundary-resolved");
        resolved.append(element("strong", "", "Resolved by the active add-in"), element("span", "", "h_conv [W/(m²·K)] ↔ coolant T_f [K]"));
        fields.append(this._labelledControl("Exchange law", resolved, "Coefficient and reference temperature follow the solved coolant state; no inert numeric override is stored."));
      } else {
        const group = element("fieldset", "implexity-boundary-components");group.dataset.boundaryField = "components";
        const legend = element("legend", "", "Constrained displacement components");group.append(legend);
        const active = new Set(Array.isArray(boundary.components) ? boundary.components : [...text(boundary.components ?? "xyz")]);
        for (const axis of "xyz") {
          const label = element("label");const checkbox = element("input");checkbox.type = "checkbox";checkbox.checked = active.has(axis);checkbox.value = axis;
          checkbox.addEventListener("change", () => {
            const checked = [...group.querySelectorAll("input:checked")].map(input => input.value).join("");
            this._replace(index, {...boundary, kind: "clamp", components: checked, units: "-"});
          });
          label.append(checkbox, document.createTextNode(axis.toUpperCase()));group.append(label);
        }
        const holder = element("div", "implexity-boundary-field implexity-boundary-component-field");holder.append(group, element("small", "", "Selected components are fixed at 0 m; choose at least one axis."));fields.append(holder);
      }
      card.append(fields);
    }

    _renderCard(boundary, index, issues) {
      const kind = cleanKind(boundary?.kind || boundary?.type);
      const known = Boolean(BOUNDARY_KINDS[kind]);
      const card = element("article", "implexity-boundary-card");card.dataset.boundaryIndex = String(index);card.dataset.state = boundary.enabled === false ? "disabled" : issues.length ? "invalid" : "ready";
      const head = element("div", "implexity-boundary-card-head");
      const enabledLabel = element("label", "implexity-boundary-enabled");const enabled = element("input");enabled.type = "checkbox";enabled.checked = boundary.enabled !== false;
      enabled.addEventListener("change", () => this._replace(index, {...boundary, enabled: enabled.checked}));
      enabledLabel.append(enabled, element("span", "", enabled.checked ? "Active" : "Excluded"));
      const identity = element("div", "");identity.append(element("strong", "", text(boundary.name) || `Boundary ${index + 1}`), element("code", "", text(boundary.id) || "missing-id"));
      const remove = element("button", "implexity-boundary-remove", "Remove");remove.type = "button";remove.setAttribute("aria-label", `Remove ${text(boundary.name) || `boundary ${index + 1}`}`);
      remove.addEventListener("click", () => this._remove(index));
      head.append(enabledLabel, identity, remove);card.append(head);
      const idRow = element("div", "implexity-boundary-grid");
      const id = element("input");id.type = "text";id.value = text(boundary.id);id.required = true;id.pattern = "[A-Za-z][A-Za-z0-9_.-]*";id.dataset.boundaryField = "id";
      id.setAttribute("autocomplete", "off");id.setAttribute("aria-label", `Stable ID for ${text(boundary.name) || `boundary ${index + 1}`}`);
      id.addEventListener("change", () => this._replace(index, {...boundary, id: id.value.trim()}));
      const idField = this._labelledControl("Stable ID", id, "Used by preflight reports, history and external control; editing does not change the physical role.");
      const regenerate = element("button", "", "Regenerate ID");regenerate.type = "button";regenerate.dataset.boundaryField = "regenerate-id";
      regenerate.addEventListener("click", () => this._replace(index, {...boundary, id: regenerateBoundaryId(boundary, this.boundaries, index)}));
      idRow.append(idField, regenerate);card.append(idRow);
      if (known) this._renderKnownBody(card, boundary, index, kind);
      else {
        const expert = element("div", "implexity-boundary-expert-note");expert.append(element("strong", "", `Provider-specific condition · ${kind || "kind not declared"}`), element("span", "", "This declaration is preserved exactly. Use the expert JSON below to edit fields not advertised by the guided boundary editor."));card.append(expert);
      }
      if (issues.length) {
        const list = element("ul", "implexity-boundary-errors");list.setAttribute("role", "alert");
        issues.forEach(issue => list.append(element("li", "", issue.message)));card.append(list);
      }
      return card;
    }

    render() {
      const issues = validationIssues(this.boundaries, this.regions);
      const byIndex = new Map();
      issues.forEach(issue => {const list = byIndex.get(issue.index) || [];list.push(issue);byIndex.set(issue.index, list);});
      this.host.replaceChildren();

      const heading = element("div", "implexity-boundary-heading");
      const title = element("div", "");title.append(element("h4", "", "Boundary conditions"), element("p", "", "Choose a region, then add a physical condition. Changes remain a draft until Validate and apply."));
      const active = this.boundaries.filter(item => item?.enabled !== false).length;
      const summary = element("span", "implexity-boundary-summary", `${active} active · ${this.boundaries.length - active} excluded · ${issues.length} ${issues.length === 1 ? "issue" : "issues"}`);
      summary.setAttribute("role", "status");summary.setAttribute("aria-live", "polite");
      const history = element("div", "implexity-boundary-history");
      const undo = element("button", "", "Undo");undo.type = "button";undo.disabled = this.historyIndex <= 0;undo.dataset.boundaryDisabled = String(undo.disabled);undo.setAttribute("aria-label", "Undo last boundary-condition draft edit");undo.addEventListener("click", () => this.undo());
      const redo = element("button", "", "Redo");redo.type = "button";redo.disabled = this.historyIndex >= this.history.length - 1;redo.dataset.boundaryDisabled = String(redo.disabled);redo.setAttribute("aria-label", "Redo boundary-condition draft edit");redo.addEventListener("click", () => this.redo());
      history.append(undo, redo);heading.append(title, summary, history);this.host.append(heading);

      this.host.append(this._renderCreator());
      const assigned=new Set(this.boundaries.filter(b=>b.enabled!==false).map(b=>regionReference(b.region).id));
      const unused=this.regions.filter(r=>!assigned.has(r.id));
      if(unused.length)this.host.append(element("p","implexity-boundary-empty",`No active boundary condition on: ${unused.map(r=>r.name).join(", ")}. A region may instead be used by a load or design rule; this is not a provider validation error.`));

      const list = element("div", "implexity-boundary-list");list.setAttribute("role", "list");list.setAttribute("aria-label", "Authored boundary conditions");
      if (!this.boundaries.length) {
        const empty = element("div", "implexity-boundary-empty");empty.append(element("strong", "", "No boundary conditions declared"), element("span", "", this.regions.length ? "Choose a target region and add the first condition above." : "Create a region on the model or declare one in the expert region editor."));list.append(empty);
      } else this.boundaries.forEach((boundary, index) => list.append(this._renderCard(boundary, index, byIndex.get(index) || [])));
      this.host.append(list);
      this.setBusy(this.busy);
    }
  }

  global.ImplexityBoundaryAuthoring = Object.freeze({
    kinds: BOUNDARY_KINDS,
    publicBoundary,
    publicBoundaries,
    namedRegions,
    regionReference,
    validationIssues,
    newBoundary,
    regenerateBoundaryId,
    providerIssueRows,
    mapProviderIssue,
    mapProviderIssues,
    bind(options) { return new GuidedBoundaryEditor(options); },
  });
})(window);
