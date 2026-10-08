// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

const SCHEMA = "implexity-grid-registration/1";
const describe = (identifier, metadata = {}) => window.ImplexityText?.present?.(identifier, metadata) || ({
  id: String(identifier ?? ""),
  label: String(metadata.label || metadata.display_name || metadata.title || identifier || ""),
  unit: String(metadata.unit || metadata.units || ""),
  description: String(metadata.description || metadata.help || ""),
});
const human = (identifier, metadata = {}) =>
  window.ImplexityText?.humanize?.(identifier, metadata) || describe(identifier, metadata).label;
const ACTION_LABELS = Object.freeze({
  designable: "Release",
  fixed_solid: "Keep solid",
  fixed_void: "Keep void",
  preserve_current: "Preserve",
  refine: "Refine",
  control_proposal: "Propose update",
});
const renderStatusMessage = (host, summary, technical, error = false) => {
  const presenter = window.ImplexityUIPresentation;
  if (presenter?.render) return presenter.render(host, {summary, technical, severity:error ? "error" : "info"});
  host.replaceChildren();
  const lead = document.createElement("span"); lead.textContent = summary; host.append(lead);
  host.setAttribute("role", error ? "alert" : "status");
  return summary;
};

class ExactGridRegistration {
  constructor(wire) {
    if (!wire || (wire.schema && wire.schema !== SCHEMA)) throw new Error("Missing exact field registration");
    if (!Array.isArray(wire.shape) || !Array.isArray(wire.origin) || !Array.isArray(wire.basis)) {
      throw new Error("Invalid field registration");
    }
    this.shape = wire.shape.map(Number);
    this.origin = wire.origin.map(Number);
    this.basis = wire.basis.map(row => Array.isArray(row) ? row.map(Number) : []);
    this.centering = wire.centering || "cell";
    if (this.shape.length !== 3 || this.origin.length !== 3 || this.basis.length !== 3
        || this.basis.some(row => row.length !== 3)
        || this.shape.some(value => !Number.isInteger(value) || value <= 0)
        || this.origin.some(value => !Number.isFinite(value))
        || this.basis.flat().some(value => !Number.isFinite(value))) {
      throw new Error("Invalid field registration");
    }
    this.M = [
      [this.basis[0][0], this.basis[1][0], this.basis[2][0]],
      [this.basis[0][1], this.basis[1][1], this.basis[2][1]],
      [this.basis[0][2], this.basis[1][2], this.basis[2][2]],
    ];
    this.inv = ExactGridRegistration.inverse3(this.M);
    const s = this.centering === "cell" ? 0.5 : 0.0;
    const shift = this.mul(this.M, [s, s, s]);
    this.offset = this.origin.map((v, i) => v + shift[i]);
  }
  static inverse3(m) {
    const d = m[0][0]*(m[1][1]*m[2][2]-m[1][2]*m[2][1]) - m[0][1]*(m[1][0]*m[2][2]-m[1][2]*m[2][0]) + m[0][2]*(m[1][0]*m[2][1]-m[1][1]*m[2][0]);
    if (!Number.isFinite(d) || Math.abs(d) < 1e-15) throw new Error("Non-invertible field registration");
    return [
      [(m[1][1]*m[2][2]-m[1][2]*m[2][1])/d, (m[0][2]*m[2][1]-m[0][1]*m[2][2])/d, (m[0][1]*m[1][2]-m[0][2]*m[1][1])/d],
      [(m[1][2]*m[2][0]-m[1][0]*m[2][2])/d, (m[0][0]*m[2][2]-m[0][2]*m[2][0])/d, (m[0][2]*m[1][0]-m[0][0]*m[1][2])/d],
      [(m[1][0]*m[2][1]-m[1][1]*m[2][0])/d, (m[0][1]*m[2][0]-m[0][0]*m[2][1])/d, (m[0][0]*m[1][1]-m[0][1]*m[1][0])/d],
    ];
  }
  mul(m, v) { return m.map(row => row[0]*v[0] + row[1]*v[1] + row[2]*v[2]); }
  worldToIndex(point) {
    if (!Array.isArray(point) || point.length !== 3 || point.some(value => !Number.isFinite(Number(value)))) {
      throw new Error("Picked point is not a finite three-dimensional coordinate");
    }
    const q = point.map((v, i) => Number(v) - this.offset[i]);
    return this.mul(this.inv, q);
  }
  nearest(point) {
    const q = this.worldToIndex(point).map(Math.round);
    if (q.some((v, i) => v < 0 || v >= this.shape[i])) throw new Error("Cursor lies outside registered analysis field");
    return q;
  }
  flat(index) { return (index[0]*this.shape[1] + index[1])*this.shape[2] + index[2]; }
}

function percentile(values, p) {
  if (!values.length) return Infinity;
  const sorted = values.slice().sort((a,b) => a-b);
  const q = Math.max(0, Math.min(sorted.length-1, (p/100)*(sorted.length-1)));
  const lo = Math.floor(q), hi = Math.ceil(q), t = q-lo;
  return sorted[lo]*(1-t)+sorted[hi]*t;
}

class SensitivityAuthoring {
  constructor() {
    this.field = null;
    this.registration = null;
    this.selection = null;
    this._commitPromise = null;
    this._commitBusy = false;
    this.installPanel();
    window.addEventListener("implexity:sensitivity-field", e => this.setField(e.detail));
    window.addEventListener("implexity:surface-picked", e => this.pick(e.detail));
  }
  installPanel() {
    let host = document.querySelector("#implexity-sensitivity-authoring");
    if (!host) {
      host = document.createElement("section");
      host.id = "implexity-sensitivity-authoring";
      host.className = "implexity-panel";
      host.innerHTML = `
        <header>
          <div><strong>Sensitivity-guided design domain</strong><span>Turn the exact topology gradient into auditable editable regions.</span></div>
          <button data="collapse" type="button" title="Collapse sensitivity authoring" aria-label="Collapse sensitivity authoring" aria-expanded="true" aria-controls="implexity-authoring-body">−</button>
        </header>
        <div class="implexity-body" id="implexity-authoring-body">
          <div class="implexity-summary"><span>Response</span><b data="response">No coupled sensitivity loaded</b></div>
          <div class="implexity-controls">
            <label>Selection meaning<select data="sign"><option value="favourable_add">Favourable for material addition</option><option value="favourable_remove">Favourable for material removal</option><option value="magnitude">Largest influence</option></select></label>
            <label>Threshold <span class="implexity-range"><input data="percentile" type="range" min="50" max="99.5" step="0.5" value="90" aria-label="Sensitivity selection percentile"><output data="percentileOut">90%</output></span></label>
          </div>
          <label class="implexity-connected"><input data="connected" type="checkbox" checked> Keep only the connected region around the picked surface point</label>
          <div class="implexity-actions" aria-label="Topology-domain actions">
            <button data-action="designable" type="button"><span aria-hidden="true">◇</span>Release</button>
            <button data-action="fixed_solid" type="button"><span aria-hidden="true">■</span>Keep solid</button>
            <button data-action="fixed_void" type="button"><span aria-hidden="true">□</span>Keep void</button>
            <button data-action="preserve_current" type="button"><span aria-hidden="true">◆</span>Preserve</button>
            <button data-action="refine" type="button"><span aria-hidden="true">⊕</span>Refine</button>
            <button data-action="control_proposal" type="button"><span aria-hidden="true">∇</span>Propose update</button>
          </div>
          <div data="status" role="status" aria-live="polite" aria-atomic="true">Evaluate a differentiable response to activate sensitivity-guided editing.</div>
        </div>`;
      const embedded = document.querySelector("#s_sens .sect-b");
      const target = embedded || document.querySelector("#s_opt .sect-b, aside, .right-panel, #right-panel, #inspector") || document.body;
      if (embedded) host.classList.add("implexity-panel--embedded");
      target.appendChild(host);
    }
    this.host = host;
    const range = host.querySelector('[data="percentile"]');
    range.addEventListener("input", () => {
      host.querySelector('[data="percentileOut"]').value = `${range.value}%`;
      range.setAttribute("aria-valuetext", `${range.value}th percentile`);
      this.recompute();
    });
    host.querySelector('[data="sign"]').addEventListener("change", () => this.recompute());
    host.querySelector('[data="collapse"]').addEventListener("click", event => {
      host.classList.toggle("collapsed");
      const collapsed = host.classList.contains("collapsed");
      event.currentTarget.textContent = collapsed ? "+" : "−";
      event.currentTarget.setAttribute("aria-expanded", String(!collapsed));
      event.currentTarget.setAttribute("aria-label", collapsed ? "Expand sensitivity authoring" : "Collapse sensitivity authoring");
      event.currentTarget.title = collapsed ? "Expand sensitivity authoring" : "Collapse sensitivity authoring";
    });
    host.querySelectorAll("[data-action]").forEach(button => button.addEventListener("click", () => this.apply(button.dataset.action)));
    this.updateActionAvailability();
  }
  updateActionAvailability() {
    const unavailable = !this.field || !this.selection || this._commitBusy || Boolean(this._commitPromise);
    this.host.querySelectorAll("[data-action]").forEach(button => { button.disabled = unavailable; });
  }
  setCommitBusy(busy) {
    this._commitBusy = Boolean(busy);
    if (busy) this.host.setAttribute("aria-busy", "true");
    else this.host.removeAttribute("aria-busy");
    this.updateActionAvailability();
  }
  setField(detail) {
    const registration = detail?.registration || detail?.field_registration;
    if (!registration) {
      this.field = null; this.registration = null; this.selection = null; this.updateActionAvailability();
      this.status("Exact analysis-grid registration is required; bounding-box fallback is intentionally disabled.", true);
      return;
    }
    try { this.registration = new ExactGridRegistration(registration); }
    catch (error) { this.field = null; this.registration = null; this.selection = null; this.updateActionAvailability(); this.status("The sensitivity field registration is invalid. Re-evaluate the response before editing the design domain.", true, error); return; }
    const raw = detail.values || detail.gradient || detail.data;
    this.field = (ArrayBuffer.isView(raw) ? Array.from(raw) : raw?.flat?.(Infinity) || []).map(Number);
    const expected = this.registration.shape.reduce((a,b)=>a*b,1);
    if (this.field.length !== expected || this.field.some(value => !Number.isFinite(value))) {
      const reason = this.field.length !== expected
        ? `Sensitivity size ${this.field.length} does not match registered grid ${expected}.`
        : "Sensitivity data contains a non-finite value.";
      this.status(reason, true);
      this.field = null;
      this.selection = null;
      this.updateActionAvailability();
      return;
    }
    this.response = detail.response || detail.response_id || "response";
    this.objectiveDirection = detail.objective_direction || "minimize";
    const displayedResponse = describe(this.response, {
      label: detail.response_label || detail.label,
      unit: detail.response_unit || detail.units || detail.unit,
      description: detail.response_description || detail.description,
    });
    const responseNode = this.host.querySelector('[data="response"]');
    responseNode.textContent = displayedResponse.unit ? `${displayedResponse.label} [${displayedResponse.unit}]` : displayedResponse.label;
    responseNode.dataset.responseId = this.response;
    responseNode.title = displayedResponse.description || `Technical response identifier: ${this.response}`;
    this.status("Sensitivity registered exactly. Click the model or adjust the threshold.");
    this.recompute();
  }
  pick(detail) {
    if (!this.field || !detail?.world) return;
    try { this.seed = this.registration.nearest(detail.world); this.recompute(); }
    catch (error) { this.status("The selected point is outside the registered sensitivity field.", true, error); }
  }
  recompute() {
    if (!this.field) { this.selection = null; this.updateActionAvailability(); return; }
    const sign = this.host.querySelector('[data="sign"]').value;
    const percentileControl = this.host.querySelector('[data="percentile"]');
    const p = Number(percentileControl.value);
    if (!Number.isFinite(p) || p < 50 || p > 99.5) {
      percentileControl.setAttribute("aria-invalid", "true");
      this.selection = null;
      this.updateActionAvailability();
      this.status("Enter a finite sensitivity percentile from 50 through 99.5.", true);
      return;
    }
    percentileControl.removeAttribute("aria-invalid");
    const direction = this.objectiveDirection === "maximize" ? -1 : 1;
    const score = this.field.map(g => {
      const s = direction*g;
      if (sign === "favourable_add") return Math.max(-s,0);
      if (sign === "favourable_remove") return Math.max(s,0);
      return Math.abs(s);
    });
    const threshold = percentile(score.filter(v=>v>0), p);
    this.selection = score.map(v => v >= threshold);
    this.updateActionAvailability();

    const payload = {schema:"implexity-sensitivity-selection/1", response:this.response, sign, percentile:p, threshold, seed_index:this.seed || null, connected:this.host.querySelector('[data="connected"]').checked, registration:this.registration ? {shape:this.registration.shape} : null};
    window.dispatchEvent(new CustomEvent("implexity:sensitivity-selection-preview", {detail:payload}));
    this.status(`${this.selection.filter(Boolean).length.toLocaleString()} cells selected at ${p}th percentile.`);
  }
  async apply(action) {
    if (this._commitPromise) return this._commitPromise;
    if (!this.field || !this.selection) return this.status("No registered sensitivity selection is available.", true);
    const detail = {schema:"implexity-sensitivity-authoring-request/1", action, response:this.response, objective_direction:this.objectiveDirection, selection:this.selection, seed_index:this.seed || null, registration:this.registration, gradient:this.field};
    const commit = (async () => {
      this.setCommitBusy(true);
      this.status(`Committing ${human(action, {label: ACTION_LABELS[action]})} as one topology transaction…`);
      try {
        const event = new CustomEvent("implexity:topology-mask-edit-request", {detail, cancelable:true});
        window.dispatchEvent(event);
        if (!event.defaultPrevented) {
          const bridge = window.ImplexitySensitivityAuthoringBridge || window.implexitySensitivityAuthoring;
          if (!bridge?.apply) throw new Error("The topology authoring bridge is not connected to the active project.");
          await bridge.apply(detail);
        }
        this.status(`${human(action, {label: ACTION_LABELS[action]})} committed as one topology transaction.`);
        return true;
      } catch (error) { this.status("The topology edit was not committed. Check the active project connection and try again.", true, error); return false; }
    })();
    this._commitPromise = commit;
    try {
      return await commit;
    } finally {
      if (this._commitPromise === commit) this._commitPromise = null;
      this.setCommitBusy(false);
    }
  }
  status(message, error=false, technical=undefined) {
    const node = this.host.querySelector('[data="status"]');
    renderStatusMessage(node, String(message ?? ""), technical, error); node.classList.toggle("error", error);
  }
}

window.ImplexitySensitivityAuthoringClass = SensitivityAuthoring;
function boot() {
  if (!window.implexitySensitivityAuthoring) window.implexitySensitivityAuthoring = new SensitivityAuthoring();
}
if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot, {once:true}); else boot();
